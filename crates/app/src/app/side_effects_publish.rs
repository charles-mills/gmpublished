use super::{
    App, PathBuf, PublishedFileId, RootMessage, Task, TaskKind, UiError, UpdateContext,
    WORKSHOP_LEGAL_URL, connect_steam_for_operation, flatten_blocking_ui_result, modal_stack,
    prepare_publish, sounds, workshop_url,
};
use crate::bridge::tasks::TransactionStatus;
use crate::bridge::tasks::{
    BackendContext, BackendRuntimeEvent, BackendServices, TaskHandle, TransactionRuntimeEvent,
    WorkshopSnapshotId,
};
use crate::bridge::ui_error::ResultExt as _;
use crate::features::file_preview;
use crate::util::paths::fallback_current_dir;
use gmpublished_backend::error_keys as keys;

impl App {
    pub(super) fn apply_prepare_publish_message(
        &mut self,
        message: prepare_publish::Message,
        update: UpdateContext,
    ) -> Task<RootMessage> {
        let opening = matches!(message, prepare_publish::Message::OpenRequested { .. });
        let effects = prepare_publish::update_at(
            &mut self.state.features.prepare_publish,
            message,
            update.now,
        );
        if opening {
            self.state.features.prepare_publish.set_initial_language(
                gmpublished_backend::WorkshopLanguage::from_locale(self.state.i18n.locale_id()),
            );
        }
        let localized = if opening && self.state.features.prepare_publish.update_mode() {
            let state = &self.state.features.prepare_publish;
            let id = state.description_edit_target();
            let generation = state.text_generation();
            let language = state.language();
            id.map_or_else(Task::none, |id| {
                self.environment
                    .ctx
                    .run_blocking("publish-localized-text", move |app| {
                        app.localized_text(id, language)
                    })
                    .map(move |result| {
                        RootMessage::PreparePublish(prepare_publish::Message::LocalizedTextFetched(
                            generation,
                            flatten_blocking_ui_result(result),
                        ))
                    })
            })
        } else {
            Task::none()
        };
        Task::batch([
            self.batch_effects(effects, Self::run_prepare_publish_effect),
            localized,
        ])
    }

    /// Reset state and clean up the icon only after the close animation finishes.
    pub(super) fn prepare_publish_message_task(
        &mut self,
        message: prepare_publish::Message,
        update: UpdateContext,
    ) -> Task<RootMessage> {
        if matches!(message, prepare_publish::Message::CloseRequested) {
            return self.close_modal_stack_task();
        }
        self.apply_prepare_publish_message(message, update)
    }

    pub(super) fn run_prepare_publish_effect(
        &mut self,
        effect: prepare_publish::Effect,
    ) -> Task<RootMessage> {
        match effect {
            prepare_publish::Effect::FilePreview(message) => {
                self.apply_file_preview_message(message)
            }
            prepare_publish::Effect::ModalOpenRequested => {
                self.open_modal_stack_task(modal_stack::ActiveModal::PreparePublish)
            }
            prepare_publish::Effect::BrowserScrollResetRequested => {
                iced::widget::operation::scroll_to(
                    prepare_publish::browser_rows_scrollable_id(),
                    iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: 0.0 },
                )
            }
            prepare_publish::Effect::ThumbnailDemandsChanged => {
                self.prepare_publish_thumbnail_demands()
            }
            prepare_publish::Effect::ContentPickerRequested => {
                self.prepare_publish_content_picker_task()
            }
            prepare_publish::Effect::IconPickerRequested => self.prepare_publish_icon_picker_task(),
            prepare_publish::Effect::OpenUrlRequested(url) => self.open_url_task(url),
            prepare_publish::Effect::DescriptionEditRequested(request) => {
                let message = match request.workshop_id {
                    Some(workshop_id) => {
                        super::description_editor::Message::OpenLocalizedRequested {
                            language: request.language,
                            workshop_id,
                            title: Some(request.title),
                        }
                    }
                    None => super::description_editor::Message::OpenLocalizedDraftRequested {
                        language: request.language,
                        drafts: request.drafts,
                    },
                };
                self.apply_description_editor_message(message, self.update_context)
            }
            prepare_publish::Effect::WorkshopContentRequested(request) => {
                self.prepare_publish_workshop_content_task(request)
            }
            prepare_publish::Effect::WorkshopSnapshotInspectionRequested(request) => {
                let generation = request.generation;
                self.environment
                    .ctx
                    .run_blocking("prepare-publish-workshop-inspect", move |_app| {
                        crate::bridge::prepare_publish::inspect_workshop_snapshot(request)
                    })
                    .map(move |result| {
                        RootMessage::PreparePublish(
                            prepare_publish::Message::WorkshopSnapshotInspected(
                                generation,
                                flatten_blocking_ui_result(result),
                            ),
                        )
                    })
            }
            prepare_publish::Effect::CleanupPathRequested(path) => {
                self.prepare_publish_cleanup_path_task(path)
            }
            prepare_publish::Effect::PathVerificationRequested(request) => {
                self.prepare_publish_content_verification_task(request)
            }
            prepare_publish::Effect::EntryPreviewRequested(request) => {
                self.apply_file_preview_message(file_preview::Message::OpenRequested(request))
            }
            prepare_publish::Effect::IconVerificationRequested(request) => {
                self.prepare_publish_icon_verification_task(request)
            }
            prepare_publish::Effect::IgnorePatternMutationRequested(mutation) => {
                self.prepare_publish_ignore_mutation_task(mutation)
            }
            prepare_publish::Effect::SubmitContextRequested => {
                self.prepare_publish_submit_context_task()
            }
            prepare_publish::Effect::PublishSubmitRequested(request) => {
                self.prepare_publish_submit_task(request)
            }
            prepare_publish::Effect::PublishIconSubmitRequested(request) => {
                self.prepare_publish_icon_submit_task(request)
            }
            prepare_publish::Effect::PublishSuccessUrlsRequested(result) => {
                let recovery = if result.pending_translations.is_empty() {
                    Task::none()
                } else {
                    self.apply_description_editor_message(
                        super::description_editor::Message::OpenPendingTranslationsRequested {
                            workshop_id: result.published_file_id,
                            drafts: result.pending_translations.clone(),
                        },
                        self.update_context,
                    )
                };
                Task::batch([self.prepare_publish_success_urls_task(&result), recovery])
            }
            prepare_publish::Effect::SoundRequested(sound) => {
                sounds::play(sound, self.environment.ctx.sounds_enabled());
                Task::none()
            }
        }
    }

    pub(super) fn prepare_publish_success_urls_task(
        &self,
        result: &prepare_publish::PublishSubmitResult,
    ) -> Task<RootMessage> {
        let workshop_url = workshop_url::workshop_item_url(result.published_file_id.get());
        let mut tasks = Vec::with_capacity(2);
        if result.legal_agreement_required {
            tasks.push(self.open_url_task(WORKSHOP_LEGAL_URL.to_owned()));
        }
        tasks.push(self.open_url_task(workshop_url));
        Task::batch(tasks)
    }
}

pub(super) fn prepare_publish_connect_steam(
    workshop: &crate::bridge::tasks::BackendServices,
) -> Result<(), UiError> {
    let attempt = connect_steam_for_operation(workshop);
    if attempt.connected() {
        Ok(())
    } else {
        Err(attempt
            .error()
            .cloned()
            .unwrap_or_else(|| UiError::new(keys::STEAM_ERROR)))
    }
}

fn run_publish_submit(
    backend_ctx: &BackendContext,
    publish: &BackendServices,
    task: TaskHandle,
    request: crate::bridge::publish::PublishSubmitRequest,
) -> Result<prepare_publish::PublishSubmitResult, UiError> {
    task.status(TransactionStatus::PublishStarting);
    if let Err(error) = prepare_publish_connect_steam(publish) {
        task.error(error.clone());
        return Err(error);
    }

    let transaction = backend_ctx.begin_transaction();
    let transaction_id = transaction.id();
    backend_ctx.correlate_backend_transaction(transaction_id, task);

    match publish.submit_publish(request, &transaction) {
        Ok(outcome) => {
            let _effects = backend_ctx.handle_backend_runtime_event(
                &BackendRuntimeEvent::Transaction(TransactionRuntimeEvent::Finished {
                    id: transaction_id,
                    payload: gmpublished_backend::TransactionPayload::None,
                }),
            );
            Ok(outcome.into())
        }
        Err(error) => {
            let _handled =
                backend_ctx.error_backend_transaction_task(transaction_id, error.clone());
            Err(error)
        }
    }
}

fn run_publish_icon_submit(
    backend_ctx: &BackendContext,
    publish: &BackendServices,
    task: TaskHandle,
    request: &prepare_publish::PublishIconSubmitRequestEnvelope,
) -> Result<prepare_publish::PublishIconSubmitResult, UiError> {
    task.status(TransactionStatus::PublishProcessingIcon);
    if let Err(error) = prepare_publish_connect_steam(publish) {
        task.error(error.clone());
        return Err(error);
    }

    let transaction = backend_ctx.begin_transaction();
    let transaction_id = transaction.id();
    backend_ctx.correlate_backend_transaction(transaction_id, task);

    match publish.update_publish_icon(
        &request.icon_source_path,
        request.upscale,
        request.workshop_id,
        &transaction,
    ) {
        Ok(legal_agreement_required) => {
            let _effects = backend_ctx.handle_backend_runtime_event(
                &BackendRuntimeEvent::Transaction(TransactionRuntimeEvent::Finished {
                    id: transaction_id,
                    payload: gmpublished_backend::TransactionPayload::None,
                }),
            );
            Ok(prepare_publish::PublishIconSubmitResult {
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

pub(super) fn prepare_publish_initial_content_directory(input: &str) -> PathBuf {
    normalized_picker_path(input).unwrap_or_else(fallback_current_dir)
}

pub(super) fn prepare_publish_initial_icon_directory(input: Option<&str>) -> PathBuf {
    input
        .and_then(normalized_picker_path)
        .and_then(|path| path.parent().map(ToOwned::to_owned))
        .unwrap_or_else(fallback_current_dir)
}

fn normalized_picker_path(input: &str) -> Option<PathBuf> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

impl App {
    pub(super) fn prepare_publish_open_task(
        &self,
        target: prepare_publish::OpenTarget,
    ) -> Task<RootMessage> {
        let (settings, _) = self.environment.ctx.settings_and_paths_snapshot();
        Task::done(RootMessage::PreparePublish(
            prepare_publish::Message::OpenRequested {
                target,
                ignored_patterns: crate::bridge::prepare_publish::ignored_patterns_from_settings(
                    &settings,
                ),
                upscale_icon_default: settings.backend.upscale_addon_icon,
            },
        ))
    }

    pub(super) fn prepare_publish_workshop_snapshot(
        &self,
        workshop_id: PublishedFileId,
    ) -> (WorkshopSnapshotId, PathBuf) {
        let (_settings, paths) = self.environment.ctx.settings_and_paths_snapshot();
        let sequence = self.environment.workshop_snapshot_sequence.get();
        self.environment
            .workshop_snapshot_sequence
            .set(sequence.wrapping_add(1));
        let request_id = WorkshopSnapshotId::new(sequence);
        let path = paths
            .temp_dir
            .join("prepare-publish-workshop")
            .join(format!("{}-{request_id}", workshop_id.get()));
        (request_id, path)
    }

    pub(super) fn prepare_publish_workshop_content_task(
        &self,
        request: prepare_publish::WorkshopContentRequest,
    ) -> Task<RootMessage> {
        let request_id = request.request_id;
        self.environment
            .ctx
            .run_blocking("prepare-publish-workshop-content", move |app| {
                if request.destination.exists() {
                    std::fs::remove_dir_all(&request.destination).map_err(|error| {
                        UiError::detailed(keys::IO_ERROR, Some(error.to_string()))
                    })?;
                }
                prepare_publish_connect_steam(app)?;
                app.submit_snapshot(
                    request.workshop_id,
                    crate::bridge::gma::ExtractDestination::Directory(request.destination),
                    request_id,
                )
            })
            .map(move |result| {
                RootMessage::PreparePublish(
                    prepare_publish::Message::WorkshopContentSubmissionCompleted(
                        request_id,
                        flatten_blocking_ui_result(result),
                    ),
                )
            })
    }

    pub(super) fn prepare_publish_cleanup_path_task(&self, path: PathBuf) -> Task<RootMessage> {
        self.environment
            .ctx
            .run_blocking("prepare-publish-workshop-cleanup", move |_app| {
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        log::warn!(
                            "failed to clean Prepare Publish Workshop snapshot {}: {error}",
                            path.display()
                        );
                    }
                }
            })
            .discard()
    }

    pub(super) fn prepare_publish_content_verification_task(
        &self,
        request: prepare_publish::ContentPathVerificationRequest,
    ) -> Task<RootMessage> {
        let generation = request.generation;
        self.environment
            .ctx
            .run_blocking("prepare-publish-verify", move |app| {
                let settings = app.settings_snapshot();
                let whitelist = app.whitelist_snapshot();
                crate::bridge::prepare_publish::verify_content_path(
                    request,
                    &settings.backend.ignore_globs,
                    &whitelist,
                )
            })
            .map(move |result| {
                RootMessage::PreparePublish(prepare_publish::Message::PathVerificationCompleted(
                    generation,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn prepare_publish_icon_verification_task(
        &self,
        request: prepare_publish::IconVerificationRequest,
    ) -> Task<RootMessage> {
        let generation = request.generation;
        self.environment
            .ctx
            .run_blocking("prepare-publish-icon", move |_app| {
                crate::bridge::prepare_publish::verify_icon_preview(request)
            })
            .map(move |result| {
                RootMessage::PreparePublish(prepare_publish::Message::IconVerificationCompleted(
                    generation,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn prepare_publish_ignore_mutation_task(
        &self,
        mutation: prepare_publish::IgnorePatternMutation,
    ) -> Task<RootMessage> {
        let worker_name = mutation.worker_name();
        self.environment
            .ctx
            .run_blocking(worker_name, move |app| {
                crate::bridge::prepare_publish::apply_ignore_pattern_mutation(app, mutation)
            })
            .map(move |result| {
                RootMessage::PreparePublish(
                    prepare_publish::Message::IgnorePatternMutationCompleted(result.ui_err()),
                )
            })
    }

    pub(super) fn prepare_publish_submit_context_task(&self) -> Task<RootMessage> {
        let (settings, paths) = self.environment.ctx.settings_and_paths_snapshot();
        let context = prepare_publish::PublishSubmitContext {
            ignore_globs: settings.backend.ignore_globs,
            temp_dir: paths.temp_dir,
        };

        Task::done(RootMessage::PreparePublish(
            prepare_publish::Message::SubmitContextLoaded(Ok(context)),
        ))
    }

    pub(super) fn prepare_publish_submit_task(
        &self,
        envelope: prepare_publish::PublishSubmitRequestEnvelope,
    ) -> Task<RootMessage> {
        let generation = envelope.generation;
        let initial_status = envelope.initial_status();
        let task = self
            .environment
            .ctx
            .create_task(TaskKind::Publish, initial_status);
        let ctx = self.environment.ctx.clone();
        self.environment
            .ctx
            .run_blocking("prepare-publish-submit", move |app| {
                run_publish_submit(&ctx, app, task, envelope.request)
            })
            .map(move |result| {
                RootMessage::PreparePublish(prepare_publish::Message::PublishSubmitCompleted(
                    generation,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn prepare_publish_icon_submit_task(
        &self,
        request: prepare_publish::PublishIconSubmitRequestEnvelope,
    ) -> Task<RootMessage> {
        let generation = request.generation;
        let task = self
            .environment
            .ctx
            .create_task(TaskKind::Publish, TransactionStatus::PublishProcessingIcon);
        let ctx = self.environment.ctx.clone();
        self.environment
            .ctx
            .run_blocking("prepare-publish-icon-submit", move |app| {
                run_publish_icon_submit(&ctx, app, task, &request)
            })
            .map(move |result| {
                RootMessage::PreparePublish(prepare_publish::Message::PublishIconSubmitCompleted(
                    generation,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn prepare_publish_content_picker_task(&self) -> Task<RootMessage> {
        let directory = prepare_publish_initial_content_directory(
            self.state.features.prepare_publish.addon_path(),
        );
        let title = self
            .state
            .i18n
            .tr("native-dialog-select-addon-content-folder");

        Task::future(async move {
            let selected = rfd::AsyncFileDialog::new()
                .set_title(title)
                .set_directory(directory)
                .set_can_create_directories(false)
                .pick_folder()
                .await
                .map(|folder| folder.path().to_path_buf());
            RootMessage::PreparePublish(prepare_publish::Message::AddonPathBrowseCompleted(
                selected,
            ))
        })
    }

    pub(super) fn prepare_publish_icon_picker_task(&self) -> Task<RootMessage> {
        let directory = prepare_publish_initial_icon_directory(
            self.state.features.prepare_publish.icon_display_path(),
        );
        let title = self.state.i18n.tr("native-dialog-select-workshop-icon");
        let filter = self.state.i18n.tr("native-dialog-workshop-icon-filter");
        let well = self.state.tokens.colors.surface_sunken;
        let well_rgb = [well.r, well.g, well.b];
        Task::future(async move {
            let selected = rfd::AsyncFileDialog::new()
                .set_title(title)
                .set_directory(directory)
                .add_filter(filter, &["jpg", "jpeg", "png", "gif"])
                .pick_file()
                .await
                .map(|file| file.path().to_path_buf());
            RootMessage::PreparePublish(prepare_publish::Message::IconBrowseCompleted {
                path: selected,
                well_rgb,
            })
        })
    }
}
