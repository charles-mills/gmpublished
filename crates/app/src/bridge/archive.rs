use std::borrow::Borrow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;

use crate::bridge::gma::{GmaError, PreviewArchive};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ArchivePreviewEntry<'a> {
    pub(crate) path: &'a str,
    pub(crate) size: u64,
    pub(crate) crc32: u32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PreviewArchiveSource {
    Gma(Arc<PreviewArchive>),
    Folder(FolderSource),
}

/// Loose addon folder snapshot. Entry paths are the normalized
/// lowercase/forward-slash form the rest of the preview stack expects;
/// `disk_paths` maps each back to the real file so reads still resolve on
/// case-sensitive filesystems.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FolderSource {
    entries: HashMap<FolderPath, FolderEntry>,
    paths: Vec<FolderPath>,
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
struct FolderPath(Arc<String>);

impl FolderPath {
    /// `None` for a path with no canonical form. The index must be keyed the
    /// same way lookups arrive, or an entry is present but unreachable.
    fn new(path: &str) -> Option<Self> {
        crate::bridge::content_path::normalize_archive_path(path).map(|path| Self(Arc::new(path)))
    }

    fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Borrow<str> for FolderPath {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct FolderEntry {
    size: u64,
    disk_path: PathBuf,
}

#[derive(Debug, Clone, Eq, PartialEq, Error)]
pub enum PreviewArchiveSourceError {
    #[error(transparent)]
    Gma(#[from] GmaError),
    #[error("failed to read {path}: {message}")]
    FolderRead { path: String, message: String },
    #[error("entry not found: {0}")]
    EntryNotFound(String),
}

impl PreviewArchiveSource {
    pub(crate) fn from_gma(archive: Arc<PreviewArchive>) -> Arc<Self> {
        Arc::new(Self::Gma(archive))
    }

    pub(crate) fn from_folder(
        files: impl IntoIterator<Item = (String, u64, PathBuf)>,
    ) -> Arc<Self> {
        let files = files.into_iter();
        let (lower, _) = files.size_hint();
        let mut entries = HashMap::with_capacity(lower);
        let mut paths = Vec::with_capacity(lower);
        for (path, size, disk_path) in files {
            let Some(path) = FolderPath::new(&path) else {
                continue;
            };
            if entries
                .insert(path.clone(), FolderEntry { size, disk_path })
                .is_none()
            {
                paths.push(path);
            }
        }
        Arc::new(Self::Folder(FolderSource { entries, paths }))
    }

    pub(crate) fn for_each_path(&self, mut visit: impl FnMut(&str)) {
        match self {
            Self::Gma(archive) => archive
                .entries()
                .iter()
                .for_each(|entry| visit(entry.path.as_str())),
            Self::Folder(folder) => folder.paths.iter().for_each(|path| visit(path.as_str())),
        }
    }

    pub(crate) fn entry(
        &self,
        path: &str,
    ) -> Result<ArchivePreviewEntry<'_>, PreviewArchiveSourceError> {
        match self {
            Self::Gma(archive) => {
                let entry = archive
                    .entry(path)
                    .map_err(PreviewArchiveSourceError::Gma)?;
                Ok(ArchivePreviewEntry {
                    path: entry.path.as_str(),
                    size: entry.size,
                    crc32: entry.crc32,
                })
            }
            Self::Folder(folder) => folder
                .entries
                .get_key_value(path)
                .map(|(entry_path, entry)| ArchivePreviewEntry {
                    path: entry_path.as_str(),
                    size: entry.size,
                    crc32: 0,
                })
                .ok_or_else(|| PreviewArchiveSourceError::EntryNotFound(path.to_owned())),
        }
    }

    pub(crate) fn entry_ignore_ascii_case(
        &self,
        path: &str,
    ) -> Result<ArchivePreviewEntry<'_>, PreviewArchiveSourceError> {
        match self {
            Self::Gma(archive) => archive
                .entries()
                .iter()
                .find(|entry| entry.path.as_str().eq_ignore_ascii_case(path))
                .map(|entry| ArchivePreviewEntry {
                    path: entry.path.as_str(),
                    size: entry.size,
                    crc32: entry.crc32,
                })
                .ok_or_else(|| PreviewArchiveSourceError::EntryNotFound(path.to_owned())),
            Self::Folder(folder) => folder
                .paths
                .iter()
                .find(|entry_path| entry_path.as_str().eq_ignore_ascii_case(path))
                .and_then(|entry_path| {
                    folder
                        .entries
                        .get(entry_path)
                        .map(|entry| ArchivePreviewEntry {
                            path: entry_path.as_str(),
                            size: entry.size,
                            crc32: 0,
                        })
                })
                .ok_or_else(|| PreviewArchiveSourceError::EntryNotFound(path.to_owned())),
        }
    }

    pub(crate) const fn supports_entry_extraction(&self) -> bool {
        matches!(self, Self::Gma(_))
    }

    pub(crate) fn entry_bytes(&self, path: &str) -> Result<Vec<u8>, PreviewArchiveSourceError> {
        match self {
            Self::Gma(archive) => archive
                .entry_bytes(path)
                .map_err(PreviewArchiveSourceError::Gma),
            Self::Folder(folder) => {
                let entry = folder
                    .entries
                    .get(path)
                    .ok_or_else(|| PreviewArchiveSourceError::EntryNotFound(path.to_owned()))?;
                std::fs::read(&entry.disk_path).map_err(|error| {
                    PreviewArchiveSourceError::FolderRead {
                        path: path.to_owned(),
                        message: error.to_string(),
                    }
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder index keyed by the entry path as authored is unreachable from
    /// the normalized paths every content lookup arrives with.
    #[test]
    fn folder_entries_are_indexed_in_the_form_lookups_use() {
        let dir = tempfile::tempdir().expect("tempdir");
        let disk_path = dir.path().join("Thing.VMT");
        std::fs::write(&disk_path, b"vmt").expect("write");

        let source = PreviewArchiveSource::from_folder([(
            r"Materials\Models\Thing.VMT".to_owned(),
            3,
            disk_path,
        )]);

        assert_eq!(
            source
                .entry_bytes("materials/models/thing.vmt")
                .expect("a normalized lookup must reach the entry"),
            b"vmt"
        );
    }

    #[test]
    fn folder_source_reads_entries_through_the_disk_path_map() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Uppercase on disk, normalized lowercase entry path — the map must
        // bridge the two even on case-sensitive filesystems.
        let disk_path = dir.path().join("Init.LUA");
        std::fs::write(&disk_path, b"print(1)").expect("write");

        let source =
            PreviewArchiveSource::from_folder([("lua/autorun/init.lua".to_owned(), 8, disk_path)]);

        assert!(!source.supports_entry_extraction());
        let entry = source.entry("lua/autorun/init.lua").expect("entry");
        assert_eq!((entry.size, entry.crc32), (8, 0));
        assert_eq!(
            source
                .entry_ignore_ascii_case("LUA/AUTORUN/INIT.LUA")
                .expect("case-insensitive entry"),
            entry
        );
        assert_eq!(
            source.entry_bytes("lua/autorun/init.lua").expect("bytes"),
            b"print(1)"
        );
        assert!(matches!(
            source.entry_bytes("lua/missing.lua"),
            Err(PreviewArchiveSourceError::EntryNotFound(_))
        ));
    }
}
