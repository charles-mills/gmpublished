// Only reachable through materials.rs's asset-studio-gated call paths (the
// 3D model viewer); building with --no-default-features compiles this file
// but never calls into it.
#![allow(dead_code)]

use std::path::Path;

use gmpublished_backend::vpk::{VpkEntry, VpkFile};

pub use gmpublished_backend::vpk::VpkError;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct VpkArchiveEntry {
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) crc32: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpkArchive {
    file: VpkFile,
    entries: Vec<VpkArchiveEntry>,
}

impl VpkArchive {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self, VpkError> {
        let file = VpkFile::open(path)?;
        let entries = archive_entries_from_backend(file.entries());
        Ok(Self { file, entries })
    }

    pub(crate) fn entries(&self) -> &[VpkArchiveEntry] {
        &self.entries
    }

    pub(crate) fn entry_bytes(&self, path: &str) -> Result<Vec<u8>, VpkError> {
        self.file.read_entry_bytes(path)
    }
}

fn archive_entries_from_backend(
    entries: &std::collections::HashMap<String, VpkEntry>,
) -> Vec<VpkArchiveEntry> {
    let mut entries = entries
        .values()
        .map(|entry| VpkArchiveEntry {
            path: entry.path.clone(),
            size: entry.size,
            crc32: entry.crc,
        })
        .collect::<Vec<_>>();
    entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    entries
}
