#![cfg_attr(not(feature = "asset-studio"), allow(dead_code))]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;

use crate::backend::gma::{GmaError, PreviewArchive};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ArchivePreviewEntry {
    pub(crate) path: String,
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
    entries: Vec<ArchivePreviewEntry>,
    disk_paths: HashMap<String, PathBuf>,
}

#[derive(Debug, Clone, Error)]
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
        let mut entries = Vec::new();
        let mut disk_paths = HashMap::new();
        for (path, size, disk_path) in files {
            entries.push(ArchivePreviewEntry {
                path: path.clone(),
                size,
                crc32: 0,
            });
            disk_paths.insert(path, disk_path);
        }
        Arc::new(Self::Folder(FolderSource {
            entries,
            disk_paths,
        }))
    }

    pub(crate) fn entries(&self) -> Vec<ArchivePreviewEntry> {
        match self {
            Self::Gma(archive) => archive
                .entries()
                .iter()
                .map(|entry| ArchivePreviewEntry {
                    path: entry.path.as_str().to_owned(),
                    size: entry.size,
                    crc32: entry.crc32,
                })
                .collect(),
            Self::Folder(folder) => folder.entries.clone(),
        }
    }

    pub(crate) fn entry(
        &self,
        path: &str,
    ) -> Result<ArchivePreviewEntry, PreviewArchiveSourceError> {
        match self {
            Self::Gma(archive) => {
                let entry = archive
                    .entry(path)
                    .map_err(PreviewArchiveSourceError::Gma)?;
                Ok(ArchivePreviewEntry {
                    path: entry.path.clone(),
                    size: entry.size,
                    crc32: entry.crc32,
                })
            }
            Self::Folder(folder) => folder
                .entries
                .iter()
                .find(|entry| entry.path == path)
                .cloned()
                .ok_or_else(|| PreviewArchiveSourceError::EntryNotFound(path.to_owned())),
        }
    }

    pub(crate) const fn supports_entry_extraction(&self) -> bool {
        matches!(self, Self::Gma(_))
    }

    #[cfg(feature = "asset-studio")]
    pub(crate) fn entry_bytes(&self, path: &str) -> Result<Vec<u8>, PreviewArchiveSourceError> {
        match self {
            Self::Gma(archive) => archive
                .entry_bytes(path)
                .map_err(PreviewArchiveSourceError::Gma),
            Self::Folder(folder) => {
                let disk_path = folder
                    .disk_paths
                    .get(path)
                    .ok_or_else(|| PreviewArchiveSourceError::EntryNotFound(path.to_owned()))?;
                std::fs::read(disk_path).map_err(|error| PreviewArchiveSourceError::FolderRead {
                    path: path.to_owned(),
                    message: error.to_string(),
                })
            }
        }
    }
}

#[cfg(all(test, feature = "asset-studio"))]
mod tests {
    use super::*;

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
            source.entry_bytes("lua/autorun/init.lua").expect("bytes"),
            b"print(1)"
        );
        assert!(matches!(
            source.entry_bytes("lua/missing.lua"),
            Err(PreviewArchiveSourceError::EntryNotFound(_))
        ));
    }
}
