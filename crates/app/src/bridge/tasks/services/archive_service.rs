use super::BackendServices;
use gmpublished_backend::Transaction;
use std::{path::PathBuf, sync::Arc};

impl BackendServices {
    pub(crate) fn extract_preview_archive(
        &self,
        archive: &super::super::super::gma::PreviewArchive,
        destination: super::super::super::gma::ExtractDestination,
        transaction: &Transaction,
    ) -> Result<PathBuf, super::super::super::gma::GmaError> {
        archive.extract_all_with_transaction(destination, transaction, &self.backend)
    }
    pub(crate) fn extract_preview_archive_entry(
        &self,
        archive: &super::super::super::gma::PreviewArchive,
        entry_path: &str,
        transaction: &Transaction,
    ) -> Result<PathBuf, super::super::super::gma::GmaError> {
        archive.extract_entry_with_transaction(entry_path, transaction, &self.backend)
    }
    pub(crate) fn whitelist_snapshot(&self) -> Arc<[String]> {
        self.backend.whitelist_snapshot()
    }
}
