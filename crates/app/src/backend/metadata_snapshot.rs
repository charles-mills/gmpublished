//! Disk snapshot of the Workshop metadata RAM cache.
//!
//! Persists `services.workshop_metadata` to `<cache_dir>/metadata.snap` so
//! restarts (including offline ones) render titles, tags, and scores
//! instantly, then revalidate in the background. The file is a disposable
//! cache: missing, corrupt, or version-mismatched snapshots are silently
//! discarded and rebuilt.

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

use super::domain::{PublishedFileId, WorkshopMetadata};

/// Cached entries older than this are re-queued for the existing background
/// refresh loop; they keep rendering while the refresh runs.
pub const METADATA_TTL: Duration = Duration::from_secs(24 * 60 * 60);

const SNAPSHOT_VERSION: u32 = 1;
const SNAPSHOT_FILE_NAME: &str = "metadata.snap";

/// Workshop metadata plus the wall-clock unix second it was fetched from
/// Steam (or persisted by a previous run).
#[derive(Clone, Debug, PartialEq)]
pub struct CachedWorkshopMetadata {
    pub(crate) metadata: WorkshopMetadata,
    pub(crate) fetched_at: u64,
}

impl CachedWorkshopMetadata {
    pub(crate) fn is_fresh_at(&self, now_unix_seconds: u64) -> bool {
        // saturating_sub also treats future timestamps (clock skew during a
        // run) as age zero, i.e. fresh now.
        now_unix_seconds.saturating_sub(self.fetched_at) <= METADATA_TTL.as_secs()
    }
}

/// Wall-clock unix seconds; pre-epoch clocks degrade to zero (always stale).
pub fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

pub fn snapshot_path() -> Option<PathBuf> {
    gmpublished_backend::appdata::cache_dir().map(|dir| dir.join(SNAPSHOT_FILE_NAME))
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
    id: u64,
    title: String,
    time_created: u32,
    time_updated: u32,
    score: f32,
    tags: Vec<String>,
    preview_url: Option<String>,
    subscriptions: u64,
    fetched_at: u64,
    // Added after v1 shipped; `default` keeps pre-ThumbHash snapshots loadable
    // without a version bump.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thumbhash: Option<Vec<u8>>,
}

pub fn load(path: &Path) -> HashMap<PublishedFileId, CachedWorkshopMetadata> {
    load_at(path, now_unix_seconds())
}

fn load_at(path: &Path, now_unix_seconds: u64) -> HashMap<PublishedFileId, CachedWorkshopMetadata> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                log::debug!(
                    "ignoring unreadable Workshop metadata snapshot {}: {error}",
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
                "discarding unparseable Workshop metadata snapshot {}: {error}",
                path.display()
            );
            return HashMap::new();
        }
    };
    if snapshot.version != SNAPSHOT_VERSION {
        log::debug!(
            "discarding Workshop metadata snapshot {} with unsupported version {}",
            path.display(),
            snapshot.version
        );
        return HashMap::new();
    }

    snapshot
        .entries
        .into_iter()
        .filter_map(|entry| {
            // A zero id can't happen from a snapshot this process wrote,
            // but the file is user-editable on disk; drop the entry rather
            // than fail the whole load.
            let id = PublishedFileId::new(entry.id)?;
            let cached = CachedWorkshopMetadata {
                metadata: WorkshopMetadata {
                    id,
                    title: entry.title,
                    time_created: entry.time_created,
                    time_updated: entry.time_updated,
                    score: entry.score,
                    tags: entry.tags,
                    preview_url: entry.preview_url,
                    subscriptions: entry.subscriptions,
                    thumbhash: entry.thumbhash.map(Arc::from),
                },
                // Clock-skew guard: never trust timestamps from the future.
                fetched_at: entry.fetched_at.min(now_unix_seconds),
            };
            Some((id, cached))
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum MetadataSnapshotWriteError {
    #[error("failed to serialize snapshot: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to persist snapshot {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub fn write(
    path: &Path,
    entries: &HashMap<PublishedFileId, CachedWorkshopMetadata>,
) -> Result<(), MetadataSnapshotWriteError> {
    let mut snapshot_entries = entries
        .values()
        .map(|cached| SnapshotEntry {
            id: cached.metadata.id.get(),
            title: cached.metadata.title.clone(),
            time_created: cached.metadata.time_created,
            time_updated: cached.metadata.time_updated,
            score: cached.metadata.score,
            tags: cached.metadata.tags.clone(),
            preview_url: cached.metadata.preview_url.clone(),
            subscriptions: cached.metadata.subscriptions,
            fetched_at: cached.fetched_at,
            thumbhash: cached.metadata.thumbhash.as_deref().map(<[u8]>::to_vec),
        })
        .collect::<Vec<_>>();
    // Stable on-disk order keeps rewrites auditable (the map iterates in
    // arbitrary order).
    snapshot_entries.sort_unstable_by_key(|entry| entry.id);
    let snapshot = SnapshotFile {
        version: SNAPSHOT_VERSION,
        entries: snapshot_entries,
    };

    let bytes = serde_json::to_vec(&snapshot).map_err(MetadataSnapshotWriteError::Serialize)?;
    crate::util::fs::atomic_write(path, &bytes).map_err(|source| {
        MetadataSnapshotWriteError::Write {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn sample_cached(id: u64, fetched_at: u64) -> CachedWorkshopMetadata {
        CachedWorkshopMetadata {
            metadata: WorkshopMetadata {
                id: PublishedFileId::new(id).expect("test fixture ids are always nonzero"),
                title: format!("Addon {id}"),
                time_created: 10,
                time_updated: 20,
                score: 0.75,
                tags: vec!["addon".to_owned(), "fun".to_owned()],
                preview_url: Some(format!("https://example.test/{id}.jpg")),
                subscriptions: 42,
                thumbhash: Some(Arc::from(vec![id as u8, 7, 9].as_slice())),
            },
            fetched_at,
        }
    }

    fn sample_map(fetched_at: u64) -> HashMap<PublishedFileId, CachedWorkshopMetadata> {
        [
            sample_cached(123, fetched_at),
            sample_cached(456, fetched_at),
        ]
        .into_iter()
        .map(|cached| (cached.metadata.id, cached))
        .collect()
    }

    #[test]
    fn round_trip_preserves_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("metadata.snap");
        let entries = sample_map(NOW - 100);

        write(&path, &entries).expect("write snapshot");

        assert_eq!(load_at(&path, NOW), entries);
    }

    #[test]
    fn write_creates_parent_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("nested/cache/metadata.snap");
        let entries = sample_map(NOW - 100);

        write(&path, &entries).expect("write snapshot");

        assert_eq!(load_at(&path, NOW), entries);
    }

    #[test]
    fn missing_file_loads_empty() {
        let temp = tempfile::tempdir().expect("tempdir");

        assert!(load_at(&temp.path().join("metadata.snap"), NOW).is_empty());
    }

    #[test]
    fn corrupt_file_loads_empty() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("metadata.snap");
        std::fs::write(&path, b"{ not json").expect("write corrupt snapshot");

        assert!(load_at(&path, NOW).is_empty());
    }

    #[test]
    fn version_mismatch_loads_empty() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("metadata.snap");
        std::fs::write(&path, br#"{"version":999,"entries":[]}"#).expect("write snapshot");

        assert!(load_at(&path, NOW).is_empty());

        let versioned = serde_json::json!({
            "version": SNAPSHOT_VERSION + 1,
            "entries": [{
                "id": 123,
                "title": "Addon",
                "time_created": 10,
                "time_updated": 20,
                "score": 0.5,
                "tags": [],
                "preview_url": null,
                "subscriptions": 1,
                "fetched_at": NOW,
            }],
        });
        std::fs::write(&path, versioned.to_string()).expect("write snapshot");

        assert!(load_at(&path, NOW).is_empty());
    }

    #[test]
    fn pre_thumbhash_snapshot_entries_load_without_a_hash() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("metadata.snap");
        let legacy = serde_json::json!({
            "version": SNAPSHOT_VERSION,
            "entries": [{
                "id": 123,
                "title": "Addon",
                "time_created": 10,
                "time_updated": 20,
                "score": 0.5,
                "tags": [],
                "preview_url": "https://example.test/123.jpg",
                "subscriptions": 1,
                "fetched_at": NOW,
            }],
        });
        std::fs::write(&path, legacy.to_string()).expect("write legacy snapshot");

        let loaded = load_at(&path, NOW);
        let id = PublishedFileId::new(123).expect("nonzero");
        assert!(
            loaded
                .get(&id)
                .expect("entry loads")
                .metadata
                .thumbhash
                .is_none()
        );
    }

    #[test]
    fn future_fetched_at_clamps_to_now_on_load() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("metadata.snap");
        let entries = sample_map(NOW + 10_000);

        write(&path, &entries).expect("write snapshot");

        let loaded = load_at(&path, NOW);
        assert_eq!(loaded.len(), 2);
        assert!(loaded.values().all(|cached| cached.fetched_at == NOW));
    }

    #[test]
    fn freshness_follows_the_24h_ttl() {
        let ttl = METADATA_TTL.as_secs();
        assert!(sample_cached(123, NOW).is_fresh_at(NOW));
        assert!(sample_cached(123, NOW - ttl).is_fresh_at(NOW));
        assert!(!sample_cached(123, NOW - ttl - 1).is_fresh_at(NOW));
        // Future timestamps (clock skew) count as fresh now.
        assert!(sample_cached(123, NOW + 10_000).is_fresh_at(NOW));
    }
}
