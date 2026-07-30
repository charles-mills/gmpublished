use super::{BackendServices, steam_not_connected};
use crate::bridge::{
    domain::PublishedFileId,
    publish::{PublishSubmitOutcome, PublishSubmitRequest},
    tasks::projections::publish_submission_from_app_request,
    ui_error::{ResultExt as _, UiError},
};
use gmpublished_backend::{SteamRuntimeError, Transaction, publishing as steam_publishing};
use std::path::Path;

/// Workshop publishing capability exposed to app features.
#[derive(Clone, Copy)]
pub(crate) struct PublishService<'a> {
    inner: &'a BackendServices,
}

impl<'a> PublishService<'a> {
    pub(super) const fn new(inner: &'a BackendServices) -> Self {
        Self { inner }
    }
    pub(crate) fn submit(
        self,
        request: PublishSubmitRequest,
        transaction: &Transaction,
    ) -> Result<PublishSubmitOutcome, UiError> {
        if !self.inner.steam_runtime.is_connected() {
            transaction.error(&SteamRuntimeError::NotConnected);
            return Err(steam_not_connected());
        }

        let content_source_path = request.content_source_path.clone();
        let submission = publish_submission_from_app_request(request);
        let outcome = self
            .inner
            .backend
            .submit_publish(submission, transaction)
            .ui_err()?;
        let outcome = PublishSubmitOutcome {
            published_file_id: PublishedFileId::from(outcome.published_file_id),
            legal_agreement_required: outcome.legal_agreement_required,
        };
        if let Err(error) = self.inner.config().update_settings_snapshot(|settings| {
            settings
                .backend
                .my_workshop_local_paths
                .insert(outcome.published_file_id.into(), content_source_path);
        }) {
            log::warn!("failed to record workshop item local path: {error}");
        }
        Ok(outcome)
    }
    pub(crate) fn update_icon(
        self,
        icon_source_path: &Path,
        upscale: bool,
        workshop_id: PublishedFileId,
        transaction: &Transaction,
    ) -> Result<bool, UiError> {
        self.inner
            .backend
            .connected_steam()
            .inspect_err(|_| {
                transaction.error(&SteamRuntimeError::NotConnected);
            })
            .ui_err()?;
        let icon = steam_publishing::WorkshopIcon::new(icon_source_path, upscale).ui_err()?;
        self.inner
            .backend
            .update_publish_icon(workshop_id.into(), icon, transaction)
            .ui_err()
    }
}
