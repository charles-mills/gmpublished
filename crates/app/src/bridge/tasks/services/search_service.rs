use super::BackendServices;
use crate::bridge::{
    domain::{SearchFullRequest, SearchMode, SearchQuickBatch, SearchQuickRequest},
    tasks::projections::search_quick_batch_from_backend,
};
use gmpublished_backend::{Transaction, TransactionId};

impl BackendServices {
    pub(crate) fn sync_installed_search(
        &self,
        addons: Vec<gmpublished_backend::SearchItem>,
        files: Vec<gmpublished_backend::SearchItem>,
    ) {
        self.backend.sync_installed_search(addons, files);
    }
    pub(crate) fn quick_search(&self, request: &SearchQuickRequest) -> SearchQuickBatch {
        let result = self
            .backend
            .quick_search(request.query(), backend_search_scope(request.mode()));
        search_quick_batch_from_backend(request, &result)
    }
    pub(crate) fn start_full_search(
        &self,
        request: &SearchFullRequest,
        transaction: Transaction,
    ) -> TransactionId {
        self.backend.full_search(
            request.query().to_owned(),
            backend_search_scope(request.mode()),
            transaction,
        )
    }
}

fn backend_search_scope(mode: SearchMode) -> gmpublished_backend::SearchScope {
    match mode {
        SearchMode::Addons => gmpublished_backend::SearchScope::Addons,
        SearchMode::Files => gmpublished_backend::SearchScope::Files,
    }
}
