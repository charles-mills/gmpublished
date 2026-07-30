use super::BackendServices;
use crate::bridge::{
    domain::{
        SearchFullBatch, SearchFullRequest, SearchMode, SearchQuickBatch, SearchQuickRequest,
    },
    tasks::projections::{
        search_full_batch_from_transaction_payload, search_quick_batch_from_backend,
    },
    ui_error::UiError,
};
use gmpublished_backend::{Transaction, TransactionId, TransactionPayload};

/// Installed-content search capability exposed to app features.
#[derive(Clone, Copy)]
pub(crate) struct SearchService<'a> {
    inner: &'a BackendServices,
}

impl<'a> SearchService<'a> {
    pub(super) const fn new(inner: &'a BackendServices) -> Self {
        Self { inner }
    }
    pub(crate) fn sync_installed(
        self,
        addons: Vec<gmpublished_backend::SearchItem>,
        files: Vec<gmpublished_backend::SearchItem>,
    ) {
        self.inner.backend.sync_installed_search(addons, files);
    }
    pub(crate) fn quick(self, request: &SearchQuickRequest) -> SearchQuickBatch {
        let result = match request.mode() {
            SearchMode::Addons => self.inner.backend.quick_search(
                request.query().to_owned(),
                gmpublished_backend::SearchScope::Addons,
            ),
            SearchMode::Files => self.inner.backend.quick_search(
                request.query().to_owned(),
                gmpublished_backend::SearchScope::Files,
            ),
        };
        search_quick_batch_from_backend(request, &result)
    }
    pub(crate) fn start_full(
        self,
        request: &SearchFullRequest,
        transaction: Transaction,
    ) -> TransactionId {
        match request.mode() {
            SearchMode::Addons => self.inner.backend.full_search(
                request.query().to_owned(),
                gmpublished_backend::SearchScope::Addons,
                transaction,
            ),
            SearchMode::Files => self.inner.backend.full_search(
                request.query().to_owned(),
                gmpublished_backend::SearchScope::Files,
                transaction,
            ),
        }
    }
    pub(crate) fn full_batch_from_transaction_payload(
        self,
        request: &SearchFullRequest,
        sequence: u64,
        payload: &TransactionPayload,
    ) -> Result<SearchFullBatch, UiError> {
        search_full_batch_from_transaction_payload(request, sequence, payload)
    }
}
