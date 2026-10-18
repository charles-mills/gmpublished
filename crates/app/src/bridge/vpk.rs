use gmpublished_backend::vpk::VpkFile;

pub use gmpublished_backend::vpk::VpkError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VpkArchive {
    file: Result<VpkFile, VpkError>,
    paths: Vec<String>,
}

impl VpkArchive {
    pub(crate) fn from_file(file: VpkFile) -> Self {
        let mut paths: Vec<_> = file.entries().keys().cloned().collect();
        paths.sort_unstable();
        Self {
            file: Ok(file),
            paths,
        }
    }

    pub(crate) fn unavailable(error: VpkError) -> Self {
        Self {
            file: Err(error),
            paths: Vec::new(),
        }
    }

    pub(crate) fn index_error(&self) -> Option<&VpkError> {
        self.file.as_ref().err()
    }

    pub(crate) fn paths(&self) -> &[String] {
        &self.paths
    }

    pub(crate) fn entry_size(&self, path: &str) -> Option<u64> {
        self.file
            .as_ref()
            .ok()?
            .entries()
            .get(path)
            .map(|entry| entry.size)
    }

    pub(crate) fn entry_storage(
        &self,
        path: &str,
    ) -> Result<gmpublished_backend::vpk::VpkEntryStorage, VpkError> {
        self.file
            .as_ref()
            .map_err(Clone::clone)?
            .entry_storage(path)
    }

    pub(crate) fn entry_bytes(&self, path: &str) -> Result<Vec<u8>, VpkError> {
        self.file
            .as_ref()
            .map_err(Clone::clone)?
            .read_entry_bytes(path)
    }
}
