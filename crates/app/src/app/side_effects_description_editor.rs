use super::{
    App, RootMessage, Task, TaskKind, UiError, UpdateContext, description_editor,
    flatten_blocking_ui_result, modal_stack, prepare_publish,
};
use crate::bridge::tasks::TransactionStatus;
use crate::bridge::tasks::{
    BackendContext, BackendRuntimeEvent, BackendServices, TaskHandle, TransactionRuntimeEvent,
};

impl App {
    pub(super) fn apply_description_editor_message(
        &mut self,
        message: description_editor::Message,
        update: UpdateContext,
    ) -> Task<RootMessage> {
        if matches!(message, description_editor::Message::OpenRequested { .. }) {
            self.state.features.description_editor.set_initial_language(
                gmpublished_backend::WorkshopLanguage::from_locale(self.state.i18n.locale_id()),
            );
        }
        let effects = description_editor::update_at(
            &mut self.state.features.description_editor,
            message,
            update.now,
        );
        self.batch_effects(effects, Self::run_description_editor_effect)
    }

    fn run_description_editor_effect(
        &mut self,
        effect: description_editor::Effect,
    ) -> Task<RootMessage> {
        match effect {
            description_editor::Effect::ModalOpenRequested => {
                self.open_modal_stack_task(modal_stack::ActiveModal::DescriptionEditor)
            }
            description_editor::Effect::ModalCloseRequested => self.close_modal_stack_task(),
            description_editor::Effect::SourceFetchRequested(request) => {
                self.description_source_fetch_task(request)
            }
            description_editor::Effect::SaveRequested(request) => {
                self.description_save_task(request)
            }
            description_editor::Effect::DraftStaged { language, drafts } => self
                .apply_prepare_publish_message(
                    prepare_publish::Message::LocalizedDraftsStaged { language, drafts },
                    self.update_context,
                ),
            description_editor::Effect::MetadataSaved {
                workshop_id,
                language,
                text,
            } => {
                self.state.features.prepare_publish.apply_saved_metadata(
                    workshop_id,
                    language,
                    &text,
                );
                self.state
                    .features
                    .preview_gma
                    .apply_saved_metadata(workshop_id, language, text);
                Task::none()
            }
            description_editor::Effect::OpenUrlRequested(url) => self.open_url_task(url),
            description_editor::Effect::ThumbnailDemandsChanged => {
                self.description_editor_thumbnail_demands()
            }
        }
    }

    fn description_source_fetch_task(
        &self,
        request: description_editor::SourceRequest,
    ) -> Task<RootMessage> {
        let generation = request.generation;
        self.environment
            .ctx
            .run_blocking("description-editor-fetch", move |app| {
                let item = app.localized_text(request.workshop_id, request.language)?;
                Ok::<_, UiError>(description_editor::FetchedSource {
                    title: item.title,
                    description: item.description,
                })
            })
            .map(move |result| {
                RootMessage::DescriptionEditor(description_editor::Message::SourceFetched {
                    generation,
                    result: flatten_blocking_ui_result(result),
                })
            })
    }

    fn description_save_task(&self, request: description_editor::SaveRequest) -> Task<RootMessage> {
        let generation = request.generation;
        let task = self
            .environment
            .ctx
            .create_task(TaskKind::Publish, TransactionStatus::PublishStarting);
        let ctx = self.environment.ctx.clone();
        self.environment
            .ctx
            .run_blocking("description-editor-save", move |app| {
                run_metadata_update(&ctx, app, task, &request)
            })
            .map(move |result| {
                RootMessage::DescriptionEditor(description_editor::Message::SaveCompleted {
                    generation,
                    result: flatten_blocking_ui_result(result),
                })
            })
    }
}

fn run_metadata_update(
    backend_ctx: &BackendContext,
    publish: &BackendServices,
    task: TaskHandle,
    request: &description_editor::SaveRequest,
) -> Result<description_editor::SaveOutcome, UiError> {
    let transaction = backend_ctx.begin_correlated_transaction(task);
    let transaction_id = transaction.id();

    match publish.update_publish_metadata(&request.edit, &transaction) {
        Ok(legal_agreement_required) => {
            let _effects = backend_ctx.handle_backend_runtime_event(
                &BackendRuntimeEvent::Transaction(TransactionRuntimeEvent::Finished {
                    id: transaction_id,
                    payload: gmpublished_backend::TransactionPayload::None,
                }),
            );
            Ok(description_editor::SaveOutcome {
                legal_agreement_required,
            })
        }
        Err(error) => {
            let _handled =
                backend_ctx.error_backend_transaction_task(transaction_id, error.clone());
            Err(error)
        }
    }
}
