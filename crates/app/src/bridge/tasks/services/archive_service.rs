use super::BackendServices;
use gmpublished_backend::Transaction;
use std::{path::PathBuf, sync::Arc};

/// GMA inspection and extraction capability exposed to app features.
#[derive(Clone, Copy)]
pub struct ArchiveService<'a> {
    inner: &'a BackendServices,
}

impl<'a> ArchiveService<'a> {
    pub(super) const fn new(inner: &'a BackendServices) -> Self {
        Self { inner }
    }
    pub(crate) fn extract_preview_archive(
        self,
        archive: &super::super::super::gma::PreviewArchive,
        destination: super::super::super::gma::ExtractDestination,
        options: &super::super::super::gma::PreviewExtractOptions,
        transaction: &Transaction,
    ) -> Result<PathBuf, super::super::super::gma::GmaError> {
        archive.extract_all_with_transaction(destination, options, transaction, &self.inner.backend)
    }
    pub(crate) fn extract_preview_archive_entry(
        self,
        archive: &super::super::super::gma::PreviewArchive,
        entry_path: &str,
        transaction: &Transaction,
    ) -> Result<PathBuf, super::super::super::gma::GmaError> {
        archive.extract_entry_with_transaction(entry_path, transaction, &self.inner.backend)
    }
    pub(crate) fn whitelist_snapshot(self) -> Arc<[String]> {
        self.inner.backend.whitelist_snapshot()
    }
}
