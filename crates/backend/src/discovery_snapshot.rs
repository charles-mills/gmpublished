//! Disk snapshot of parsed GMA header state, keyed by path, so a warm
//! discovery pass re-parses only files that are new or whose (size, mtime)
//! changed. Persisted to
//! `<cache_dir>/discovery.snap`. The file is a disposable cache: missing,
//! corrupt, or version-mismatched snapshots are silently discarded and
//! rebuilt by a full scan.

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use steamworks::PublishedFileId;

use crate::{Addon, GMAFile, GMAMetadata};

const SNAPSHOT_VERSION: u32 = 1;

/// Mtime as unix seconds, the same derivation the discovery collector stamps
/// onto `GMAFile::modified` (pre-epoch clamps to zero); `None` when the
/// platform reports no mtime.
pub fn modified_unix_seconds(stat: &fs::Metadata) -> Option<u64> {
    stat.modified().ok().map(|modified| {
        modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |dur| dur.as_secs())
    })
}

/// Parsed header state for one previously discovered GMA, keyed by path.
/// While the file's (size, mtime) still match, [`Self::hydrate`] rebuilds a
/// fully functional `GMAFile` with zero content reads.
#[derive(Clone, Debug)]
pub struct SnapshotAddon {
    size: u64,
    modified: u64,
    gma_version: u8,
    metadata: GMAMetadata,
}

impl SnapshotAddon {
    /// Rebuilds the exact state of a fresh parse, or `None` when the file
    /// changed (size/mtime), vanished, or cannot be statted — callers then
    /// fall back to the fresh parse path, which degrades exactly like
    /// today's corrupt-file handling.
    pub(crate) fn hydrate(&self, path: &Path, ws_id: Option<PublishedFileId>) -> Option<GMAFile> {
        let stat = path.metadata().ok()?;
        if stat.len() != self.size || modified_unix_seconds(&stat)? != self.modified {
            return None;
        }

        let mut gma = GMAFile {
            path: path.to_path_buf(),
            size: self.size,
            id: None,
            metadata: self.metadata.clone(),
            version: self.gma_version,
            extracted_name: String::new(),
            // The discovery collector stamps this, as for fresh parses.
            modified: None,
        };

        // Same id/extracted-name derivation as the fresh parse path: the
        // directory-derived id wins, else the filename fallback inside
        // `compute_extracted_name` (`gma::ws_id_from_file_name`).
        match ws_id {
            Some(id) => gma.set_ws_id(id),
            None => gma.compute_extracted_name(),
        }

        Some(gma)
    }
}

/// Versioned snapshot DTOs: a stable schema decoupled from the live structs.
/// Schema changes bump [`SNAPSHOT_VERSION`]; old files are discarded, never
/// migrated.
#[derive(Debug, Deserialize, Serialize)]
struct SnapshotFile {
    version: u32,
    entries: Vec<SnapshotEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SnapshotEntry {
    path: String,
    size: u64,
    modified: u64,
    gma_version: u8,
    metadata: SnapshotMetadata,
}

impl SnapshotEntry {
    /// `None` for entries that cannot round-trip: no mtime to validate
    /// against, or a non-UTF-8 path (JSON cannot carry it).
    fn from_discovered(gma: &GMAFile) -> Option<Self> {
        Some(Self {
            path: gma.path.to_str()?.to_owned(),
            size: gma.size,
            modified: gma.modified?,
            gma_version: gma.version,
            metadata: SnapshotMetadata::from(&gma.metadata),
        })
    }
}

/// Explicitly tagged: the live `GMAMetadata` is `#[serde(untagged)]`, which
/// would make Standard/Legacy ambiguous on load.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind")]
enum SnapshotMetadata {
    Standard {
        title: String,
        addon_type: String,
        tags: Vec<String>,
        ignore: Vec<String>,
    },
    Legacy {
        title: String,
        description: String,
    },
}

impl From<&GMAMetadata> for SnapshotMetadata {
    fn from(metadata: &GMAMetadata) -> Self {
        match metadata {
            GMAMetadata::Standard {
                title,
                addon_type,
                tags,
                ignore,
            } => Self::Standard {
                title: title.clone(),
                addon_type: addon_type.clone(),
                tags: tags.clone(),
                ignore: ignore.clone(),
            },
            GMAMetadata::Legacy { title, description } => Self::Legacy {
                title: title.clone(),
                description: description.clone(),
            },
        }
    }
}

impl From<SnapshotMetadata> for GMAMetadata {
    fn from(metadata: SnapshotMetadata) -> Self {
        match metadata {
            SnapshotMetadata::Standard {
                title,
                addon_type,
                tags,
                ignore,
            } => Self::Standard {
                title,
                addon_type,
                tags,
                ignore,
            },
            SnapshotMetadata::Legacy { title, description } => Self::Legacy { title, description },
        }
    }
}

pub fn load(path: &Path) -> HashMap<PathBuf, SnapshotAddon> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                log::debug!(
                    "ignoring unreadable discovery snapshot {}: {error}",
                    path.display()
                );
            }
            return HashMap::new();
        }
    };

    let snapshot = match serde_json::from_str::<SnapshotFile>(&contents) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log::debug!(
                "discarding unparseable discovery snapshot {}: {error}",
                path.display()
            );
            return HashMap::new();
        }
    };
    if snapshot.version != SNAPSHOT_VERSION {
        log::debug!(
            "discarding discovery snapshot {} with unsupported version {}",
            path.display(),
            snapshot.version
        );
        return HashMap::new();
    }

    snapshot
        .entries
        .into_iter()
        .map(|entry| {
            (
                PathBuf::from(entry.path),
                SnapshotAddon {
                    size: entry.size,
                    modified: entry.modified,
                    gma_version: entry.gma_version,
                    metadata: entry.metadata.into(),
                },
            )
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotWriteError {
    #[error("failed to create snapshot directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to create snapshot tempfile: {0}")]
    CreateTempfile(#[source] std::io::Error),
    #[error("failed to serialize snapshot: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to persist snapshot {path}: {source}")]
    Persist {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub fn write(path: &Path, addons: &HashMap<PathBuf, Arc<Addon>>) -> Result<(), SnapshotWriteError> {
    let parent = path.parent();
    if let Some(parent) = parent {
        fs::create_dir_all(parent).map_err(|source| SnapshotWriteError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let mut snapshot_entries = addons
        .values()
        .filter_map(|addon| {
            // Discovery only ever inserts `Addon::Installed` entries into this map.
            SnapshotEntry::from_discovered(
                addon
                    .installed()
                    .expect("discovery snapshot only stores installed addons"),
            )
        })
        .collect::<Vec<_>>();
    // Stable on-disk order keeps rewrites auditable (the map iterates in
    // arbitrary order).
    snapshot_entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let snapshot = SnapshotFile {
        version: SNAPSHOT_VERSION,
        entries: snapshot_entries,
    };

    // Write-then-rename so a crash mid-write can never corrupt the snapshot.
    // The tempfile lives in the same directory as the target to keep the
    // rename atomic (same filesystem).
    let mut tmp = parent
        .map_or_else(tempfile::NamedTempFile::new, |parent| {
            tempfile::NamedTempFile::new_in(parent)
        })
        .map_err(SnapshotWriteError::CreateTempfile)?;
    serde_json::to_writer(&mut tmp, &snapshot).map_err(SnapshotWriteError::Serialize)?;
    tmp.persist(path)
        .map_err(|error| SnapshotWriteError::Persist {
            path: path.to_path_buf(),
            source: error.error,
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_gma(path: &Path, metadata: GMAMetadata, modified: Option<u64>) -> GMAFile {
        GMAFile {
            path: path.to_path_buf(),
            size: 1234,
            id: None,
            metadata,
            version: 3,
            extracted_name: String::new(),
            modified,
        }
    }

    fn fixture_map(dir: &Path) -> HashMap<PathBuf, Arc<Addon>> {
        let standard = fixture_gma(
            &dir.join("standard_123.gma"),
            GMAMetadata::Standard {
                title: "Standard Fixture".to_owned(),
                addon_type: "servercontent".to_owned(),
                tags: vec!["build".to_owned(), "fun".to_owned()],
                ignore: vec!["*.psd".to_owned()],
            },
            Some(1_700_000_000),
        );
        let legacy = fixture_gma(
            &dir.join("legacy.gma"),
            GMAMetadata::Legacy {
                title: "Legacy Fixture".to_owned(),
                description: "A legacy description".to_owned(),
            },
            Some(1_700_000_001),
        );
        [standard, legacy]
            .into_iter()
            .map(|gma| (gma.path.clone(), Arc::new(Addon::Installed(gma))))
            .collect()
    }

    #[test]
    fn round_trip_preserves_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("discovery.snap");
        let addons = fixture_map(temp.path());

        write(&path, &addons).expect("write snapshot");
        let loaded = load(&path);

        assert_eq!(loaded.len(), 2);

        let standard = &loaded[&temp.path().join("standard_123.gma")];
        assert_eq!(standard.size, 1234);
        assert_eq!(standard.modified, 1_700_000_000);
        assert_eq!(standard.gma_version, 3);
        assert_eq!(standard.metadata.title(), "Standard Fixture");
        assert_eq!(standard.metadata.addon_type(), Some("servercontent"));
        assert_eq!(
            standard.metadata.tags(),
            Some(&vec!["build".to_owned(), "fun".to_owned()])
        );
        assert_eq!(standard.metadata.ignore(), Some(&vec!["*.psd".to_owned()]));

        let legacy = &loaded[&temp.path().join("legacy.gma")];
        assert_eq!(legacy.modified, 1_700_000_001);
        assert_eq!(legacy.metadata.title(), "Legacy Fixture");
        assert!(matches!(
            &legacy.metadata,
            GMAMetadata::Legacy { description, .. } if description == "A legacy description"
        ));
    }

    #[test]
    fn write_skips_entries_that_cannot_round_trip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("discovery.snap");

        // No mtime to validate against next launch (unstattable at
        // discovery time); metadata itself always round-trips now that a
        // `GMAFile` cannot exist without it.
        let unstatted = fixture_gma(
            &temp.path().join("unstatted.gma"),
            GMAMetadata::Legacy {
                title: "No mtime".to_owned(),
                description: String::new(),
            },
            None,
        );
        let addons = [unstatted]
            .into_iter()
            .map(|gma| (gma.path.clone(), Arc::new(Addon::Installed(gma))))
            .collect();

        write(&path, &addons).expect("write snapshot");

        assert!(load(&path).is_empty());
    }

    #[test]
    fn write_creates_parent_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("nested/cache/discovery.snap");

        write(&path, &fixture_map(temp.path())).expect("write snapshot");

        assert_eq!(load(&path).len(), 2);
    }

    #[test]
    fn missing_file_loads_empty() {
        let temp = tempfile::tempdir().expect("tempdir");

        assert!(load(&temp.path().join("discovery.snap")).is_empty());
    }

    #[test]
    fn corrupt_file_loads_empty() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("discovery.snap");
        std::fs::write(&path, b"{ not json").expect("write corrupt snapshot");

        assert!(load(&path).is_empty());
    }

    #[test]
    fn version_mismatch_loads_empty() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("discovery.snap");
        std::fs::write(&path, br#"{"version":999,"entries":[]}"#).expect("write snapshot");

        assert!(load(&path).is_empty());

        let versioned = serde_json::json!({
            "version": SNAPSHOT_VERSION + 1,
            "entries": [{
                "path": "/tmp/addon.gma",
                "size": 1,
                "modified": 2,
                "gma_version": 3,
                "metadata_pointer": 5,
                "entries_list_pointer": 96,
                "metadata": { "kind": "Legacy", "title": "t", "description": "d" },
            }],
        });
        std::fs::write(&path, versioned.to_string()).expect("write snapshot");

        assert!(load(&path).is_empty());
    }

    #[test]
    fn hydrate_rejects_missing_or_changed_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("addon.gma");
        std::fs::write(&path, b"GMAD-content").expect("write file");
        let stat = path.metadata().expect("stat file");
        let modified = modified_unix_seconds(&stat).expect("mtime");

        let cached = SnapshotAddon {
            size: stat.len(),
            modified,
            gma_version: 3,
            metadata: GMAMetadata::Legacy {
                title: "Fixture".to_owned(),
                description: String::new(),
            },
        };

        assert!(cached.hydrate(&path, None).is_some());

        let size_changed = SnapshotAddon {
            size: stat.len() + 1,
            ..cached.clone()
        };
        assert!(size_changed.hydrate(&path, None).is_none());

        let mtime_changed = SnapshotAddon {
            modified: modified + 1,
            ..cached.clone()
        };
        assert!(mtime_changed.hydrate(&path, None).is_none());

        assert!(
            cached
                .hydrate(&temp.path().join("gone.gma"), None)
                .is_none()
        );
    }
}
