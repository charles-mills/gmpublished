use super::{
    App, BackendServices, Path, PathBuf, PublishedFileId, RootMessage, Task, TaskKind, UiError,
    WORKSHOP_LEGAL_URL, flatten_blocking_ui_result, modal_stack, prepare_publish, sounds,
    steam_session, workshop_url,
};
#[cfg(feature = "asset-studio")]
use crate::features::file_preview;
use gmpublished_backend::error_key::keys;

impl App {
    pub(super) fn apply_prepare_publish_message(
        &mut self,
        message: prepare_publish::Message,
    ) -> Task<RootMessage> {
        let effects = prepare_publish::update(&mut self.state.prepare_publish, message);
        self.batch_effects(effects, Self::run_prepare_publish_effect)
    }

    /// Direct close requests (e.g. the modal's Cancel button) only start the
    /// close animation; the actual state reset and icon cleanup are deferred
    /// to `finish_modal_close_task`, which runs once that animation settles.
    pub(super) fn prepare_publish_message_task(
        &mut self,
        message: &prepare_publish::Message,
    ) -> Task<RootMessage> {
        if matches!(message, prepare_publish::Message::CloseRequested) {
            return self.close_modal_stack_task();
        }

        let was_valid = self.state.prepare_publish.can_submit();
        let was_pending = self.state.prepare_publish.submit_pending();
        let announce_path_success = self.state.prepare_publish.announce_path_success();
        let task = self.apply_prepare_publish_message(message.clone());
        self.prepare_publish_sounds(message, was_valid, was_pending, announce_path_success);
        task
    }

    pub(super) fn run_prepare_publish_effect(
        &mut self,
        effect: prepare_publish::Effect,
    ) -> Task<RootMessage> {
        match effect {
            prepare_publish::Effect::ModalOpenRequested => {
                self.open_modal_stack_task(modal_stack::ActiveModal::PreparePublish)
            }
            prepare_publish::Effect::ThumbnailDemandsChanged => {
                self.prepare_publish_thumbnail_demands()
            }
            prepare_publish::Effect::ContentPickerRequested => {
                self.prepare_publish_content_picker_task()
            }
            prepare_publish::Effect::IconPickerRequested => self.prepare_publish_icon_picker_task(),
            prepare_publish::Effect::OpenUrlRequested(url) => self.open_url_task(url),
            prepare_publish::Effect::PathVerificationRequested(request) => {
                self.prepare_publish_content_verification_task(request)
            }
            #[cfg(feature = "asset-studio")]
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
                self.prepare_publish_publish_icon_task(request)
            }
            prepare_publish::Effect::PublishSuccessUrlsRequested(result) => {
                self.prepare_publish_success_urls_task(result)
            }
        }
    }

    pub(super) fn prepare_publish_ignored_patterns(&self) -> Vec<prepare_publish::IgnoredPattern> {
        let (settings, _paths) = self.ctx.settings_and_paths_snapshot();
        prepare_publish::ignored_patterns_from_settings(&settings)
    }

    pub(super) fn prepare_publish_upscale_default(&self) -> bool {
        let (settings, _paths) = self.ctx.settings_and_paths_snapshot();
        settings.upscale_addon_icon
    }

    /// Plays the modal's UI sounds in response to a `prepare_publish::Message`.
    ///
    /// `was_valid`/`was_pending` are sampled before the message is applied and
    /// compared against the post-update state; the valid-transition blip is
    /// suppressed for programmatic open/close resets.
    pub(super) fn prepare_publish_sounds(
        &self,
        message: &prepare_publish::Message,
        was_valid: bool,
        was_pending: bool,
        announce_path_success: bool,
    ) {
        let enabled = self.ctx.sounds_enabled();
        if !enabled {
            return;
        }

        if let prepare_publish::Message::PathVerificationCompleted(generation, result) = message
            && self
                .state
                .prepare_publish
                .is_current_path_generation(*generation)
        {
            match result {
                Ok(_) if announce_path_success => sounds::play(sounds::Sound::Success, enabled),
                Err(_) => sounds::play(sounds::Sound::Error, enabled),
                Ok(_) => {}
            }
        }

        // Publish outcomes blip on completion, mirroring the toast the
        // overlay shows; stale-generation completions change nothing and
        // stay silent.
        if was_pending && !self.state.prepare_publish.submit_pending() {
            let completion = match message {
                prepare_publish::Message::PublishSubmitCompleted(_, result) => Some(result.is_ok()),
                prepare_publish::Message::PublishIconSubmitCompleted(_, result) => {
                    Some(result.is_ok())
                }
                _ => None,
            };
            if let Some(succeeded) = completion {
                let blip = if succeeded {
                    sounds::Sound::Success
                } else {
                    sounds::Sound::Error
                };
                sounds::play(blip, enabled);
                return;
            }
        }

        // Only form-field edits and path checks blip; icon verification
        // doesn't.
        if matches!(
            message,
            prepare_publish::Message::OpenRequested { .. }
                | prepare_publish::Message::CloseRequested
                | prepare_publish::Message::IconBrowseRequested
                | prepare_publish::Message::IconBrowseCompleted { .. }
                | prepare_publish::Message::IconVerificationCompleted(_, _)
                | prepare_publish::Message::IconRemoveRequested
                | prepare_publish::Message::PublishSubmitCompleted(_, _)
                | prepare_publish::Message::PublishIconSubmitCompleted(_, _)
        ) {
            return;
        }
        let is_valid = self.state.prepare_publish.can_submit();
        if is_valid != was_valid {
            let blip = if is_valid {
                sounds::Sound::BtnOn
            } else {
                sounds::Sound::BtnOff
            };
            sounds::play(blip, enabled);
        }
    }

    pub(super) fn prepare_publish_content_picker_task(&self) -> Task<RootMessage> {
        let directory =
            prepare_publish_initial_content_directory(self.state.prepare_publish.addon_path());
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
        let directory =
            prepare_publish_initial_icon_directory(self.state.prepare_publish.icon_display_path());
        let (_settings, paths) = self.ctx.settings_and_paths_snapshot();
        let temp_dir = paths.temp_dir;
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
                temp_dir,
                well_rgb,
            })
        })
    }

    pub(super) fn prepare_publish_content_verification_task(
        &self,
        request: prepare_publish::ContentPathVerificationRequest,
    ) -> Task<RootMessage> {
        let generation = request.generation;
        self.ctx
            .run_blocking("prepare-publish-verify", move |app| {
                prepare_publish::verify_content_path(app, request)
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
        self.ctx
            .run_blocking("prepare-publish-icon", move |_app| {
                prepare_publish::verify_icon_preview(request)
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
        self.ctx
            .run_blocking(worker_name, move |app| {
                prepare_publish::apply_ignore_pattern_mutation(app, mutation)
            })
            .map(move |result| {
                RootMessage::PreparePublish(
                    prepare_publish::Message::IgnorePatternMutationCompleted(
                        result.map_err(|error| UiError::from(&error)),
                    ),
                )
            })
    }

    pub(super) fn prepare_publish_submit_context_task(&self) -> Task<RootMessage> {
        let (settings, paths) = self.ctx.settings_and_paths_snapshot();
        let context = prepare_publish::PublishSubmitContext {
            ignore_globs: settings.ignore_globs,
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
        let task = self.ctx.create_task(TaskKind::Publish, initial_status);
        let ctx = self.ctx.clone();
        self.ctx
            .run_blocking("prepare-publish-submit", move |app| {
                prepare_publish::run_publish_submit(
                    &ctx,
                    app,
                    prepare_publish_connect_steam,
                    task,
                    envelope.request,
                )
            })
            .map(move |result| {
                RootMessage::PreparePublish(prepare_publish::Message::PublishSubmitCompleted(
                    generation,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn prepare_publish_publish_icon_task(
        &self,
        request: prepare_publish::PublishIconSubmitRequestEnvelope,
    ) -> Task<RootMessage> {
        let generation = request.generation;
        let task = self
            .ctx
            .create_task(TaskKind::Publish, "PUBLISH_PROCESSING_ICON");
        let ctx = self.ctx.clone();
        self.ctx
            .run_blocking("prepare-publish-icon-submit", move |app| {
                prepare_publish::run_publish_icon_submit(
                    &ctx,
                    app,
                    prepare_publish_connect_steam,
                    task,
                    &request,
                )
            })
            .map(move |result| {
                RootMessage::PreparePublish(prepare_publish::Message::PublishIconSubmitCompleted(
                    generation,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn prepare_publish_success_urls_task(
        &self,
        result: prepare_publish::PublishSubmitResult,
    ) -> Task<RootMessage> {
        let workshop_url = workshop_url::workshop_item_url(result.published_file_id.get());
        let mut tasks = Vec::with_capacity(2);
        if result.legal_agreement_required {
            tasks.push(self.open_url_task(WORKSHOP_LEGAL_URL.to_owned()));
        }
        tasks.push(self.open_url_task(workshop_url));
        Task::batch(tasks)
    }

    pub(super) fn prepare_publish_saved_path(
        &self,
        workshop_id: PublishedFileId,
    ) -> Option<PathBuf> {
        let (settings, _paths) = self.ctx.settings_and_paths_snapshot();
        settings.my_workshop_local_paths.get(&workshop_id).cloned()
    }
}

pub(super) fn prepare_publish_connect_steam(app: &BackendServices) -> Result<(), UiError> {
    let attempt = steam_session::connect_context_for_operation(app);
    if attempt.connected() {
        Ok(())
    } else {
        Err(attempt
            .error()
            .cloned()
            .unwrap_or_else(|| UiError::new(keys::STEAM_ERROR)))
    }
}

pub(super) fn prepare_publish_initial_content_directory(input: &str) -> PathBuf {
    initial_content_directory(input).unwrap_or_else(fallback_current_dir)
}

pub(super) fn prepare_publish_initial_icon_directory(input: Option<&str>) -> PathBuf {
    input
        .and_then(initial_content_directory)
        .unwrap_or_else(fallback_current_dir)
}

pub(super) fn initial_content_directory(input: &str) -> Option<PathBuf> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let path = PathBuf::from(trimmed);
    if path.is_dir() {
        return Some(path);
    }

    path.parent()
        .filter(|parent| parent.is_dir())
        .map(Path::to_path_buf)
        .or(Some(path))
}

pub(super) fn fallback_current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|error| {
        log::debug!("current_dir failed while opening Prepare Publish picker: {error}");
        std::env::temp_dir()
    })
}
