use std::path::Path;

use gmpublished_backend::vpk::VpkFile;

pub use gmpublished_backend::vpk::VpkError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VpkArchive {
    file: VpkFile,
    paths: Vec<String>,
}

impl VpkArchive {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self, VpkError> {
        let file = VpkFile::open(path)?;
        let mut paths: Vec<_> = file.entries().keys().cloned().collect();
        paths.sort_unstable();
        Ok(Self { file, paths })
    }

    pub(crate) fn paths(&self) -> &[String] {
        &self.paths
    }

    pub(crate) fn entry_size(&self, path: &str) -> Option<u64> {
        self.file.entries().get(path).map(|entry| entry.size)
    }

    pub(crate) fn entry_prefix(&self, path: &str, max_bytes: usize) -> Result<Vec<u8>, VpkError> {
        self.file.read_entry_prefix(path, max_bytes)
    }

    pub(crate) fn entry_bytes(&self, path: &str) -> Result<Vec<u8>, VpkError> {
        self.file.read_entry_bytes(path)
    }
}
