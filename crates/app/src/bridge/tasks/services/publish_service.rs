use super::{BackendServices, steam_not_connected};
use crate::bridge::{
    domain::PublishedFileId,
    publish::{PublishSubmitOutcome, PublishSubmitRequest},
    tasks::projections::publish_submission_from_app_request,
    ui_error::{ResultExt as _, UiError},
};
use gmpublished_backend::{SteamRuntimeError, Transaction, publishing as steam_publishing};
use std::path::Path;

#[derive(Clone, Copy)]
pub struct PublishService<'a> {
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

        let language = request.language;
        let mut translations = request.localized_drafts.clone();
        let primary = translations.remove(&language);
        let creating = matches!(request.mode, crate::bridge::publish::PublishSubmitMode::New);
        let content_source_path = request.content_source_path.clone();
        let submission = publish_submission_from_app_request(request);
        let outcome = self
            .inner
            .backend
            .submit_publish(submission, transaction)
            .ui_err()?;
        let mut outcome = PublishSubmitOutcome {
            pending_translations: Default::default(),
            published_file_id: PublishedFileId::from(outcome.published_file_id),
            legal_agreement_required: outcome.legal_agreement_required,
        };
        if creating {
            use crate::bridge::workshop_text::{FieldEdit, MetadataEdit};
            if let (Some(primary), Ok(user)) = (primary, self.inner.workshop().current_user()) {
                self.inner.text_journal.lock().record(
                    user.steamid,
                    &MetadataEdit {
                        workshop_id: outcome.published_file_id,
                        language,
                        title: Some(FieldEdit {
                            previous: Some(String::new()),
                            value: primary.title,
                        }),
                        description: (!primary.description.is_empty()).then_some(FieldEdit {
                            previous: Some(String::new()),
                            value: primary.description,
                        }),
                    },
                );
            }
            for (language, text) in translations {
                if text.title.trim().is_empty() && text.description.trim().is_empty() {
                    continue;
                }
                let edit = MetadataEdit {
                    workshop_id: outcome.published_file_id,
                    language,
                    title: (!text.title.is_empty()).then(|| FieldEdit {
                        previous: None,
                        value: text.title.clone(),
                    }),
                    description: (!text.description.is_empty()).then(|| FieldEdit {
                        previous: None,
                        value: text.description.clone(),
                    }),
                };
                let revision = self.inner.begin_transaction();
                match self.update_metadata(&edit, &revision) {
                    Ok(legal) => {
                        outcome.legal_agreement_required |= legal;
                        revision.finished(gmpublished_backend::TransactionPayload::None);
                    }
                    Err(error) => {
                        revision.error(gmpublished_backend::TransactionError {
                            key: error.key,
                            detail: error.detail.clone(),
                        });
                        log::warn!("could not publish {language} translation: {error}");
                        outcome.pending_translations.insert(language, text);
                    }
                }
            }
        }
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

    pub(crate) fn update_metadata(
        self,
        edit: &crate::bridge::workshop_text::MetadataEdit,
        transaction: &Transaction,
    ) -> Result<bool, UiError> {
        let account = self.inner.workshop().current_user()?.steamid;
        let mut edit = edit.clone();
        if edit.needs_baseline() {
            let remote = self
                .inner
                .workshop()
                .localized_text(edit.workshop_id, edit.language)?;
            edit.resolve_baseline(remote);
        }
        edit.remove_unchanged();
        if edit.title.is_none() && edit.description.is_none() {
            return Ok(false);
        }
        let result = self
            .inner
            .backend
            .update_publish_metadata(
                edit.workshop_id.into(),
                edit.language,
                edit.title.as_ref().map(|field| field.value.as_str()),
                edit.description.as_ref().map(|field| field.value.as_str()),
                transaction,
            )
            .ui_err()?;
        self.inner.text_journal.lock().record(account, &edit);
        Ok(result)
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
