use iced::Task;

use super::{App, RootMessage, file_preview, send_root_message, stream};
use crate::generation::Generation;
use crate::media::file_preview_decode::load_preview;
use crate::media::preview_model::{PreviewContent, PreviewLoadError, PreviewRequest};

impl App {
    pub(super) fn apply_file_preview_message(
        &mut self,
        message: file_preview::Message,
    ) -> Task<RootMessage> {
        let effects = file_preview::update(&mut self.state.features.file_preview, message);
        self.batch_effects(effects, Self::run_file_preview_effect)
    }

    pub(super) fn apply_model_preview_message(
        &mut self,
        message: file_preview::Message,
    ) -> Task<RootMessage> {
        let completed = match &message {
            file_preview::Message::Loaded(generation, result)
                if self.state.features.model_preview_loading == Some(*generation) =>
            {
                Some((
                    false,
                    result
                        .as_ref()
                        .as_ref()
                        .map(|_| ())
                        .map_err(ToString::to_string),
                ))
            }
            file_preview::Message::StageLoaded(generation, result)
                if self.state.features.model_preview_loading == Some(*generation) =>
            {
                Some((
                    true,
                    result
                        .as_ref()
                        .as_ref()
                        .map(|_| ())
                        .map_err(ToString::to_string),
                ))
            }
            _ => None,
        };
        if completed.is_some() {
            self.state.features.model_preview_loading = None;
        }
        if completed.is_some()
            && !self
                .state
                .features
                .model_manager
                .preview_accepts_presentation()
        {
            file_preview::update(
                &mut self.state.features.model_preview,
                file_preview::Message::WorkspaceLoadCancelled,
            );
            return self.apply_model_manager_message(
                crate::features::model_manager::Message::PreviewPresented,
            );
        }
        if matches!(message, file_preview::Message::BackRequested) {
            self.capture_model_stage();
        }
        let effects = file_preview::update(&mut self.state.features.model_preview, message);
        if let Some(generation) = effects.iter().find_map(|effect| match effect {
            file_preview::Effect::LoadRequested(request) => Some(request.request_id),
            file_preview::Effect::LoadStage { generation, .. } => Some(*generation),
            _ => None,
        }) {
            self.state.features.model_preview_loading = Some(generation);
        }
        let mut tasks = Vec::new();
        for effect in effects {
            if matches!(effect, file_preview::Effect::ModalCloseRequested) {
                file_preview::update(
                    &mut self.state.features.model_preview,
                    file_preview::Message::CloseFinished,
                );
            } else if !matches!(effect, file_preview::Effect::AudioStopRequested) {
                let lease = self.state.features.model_manager.preview_directory();
                tasks.push(self.run_file_preview_effect(effect).map(move |message| {
                    let _lease = &lease;
                    match message {
                        RootMessage::FilePreview(message) => RootMessage::ModelPreview(message),
                        other => other,
                    }
                }));
            }
        }
        if let Some((staged, result)) = completed {
            match result {
                Ok(()) => {
                    tasks.push(self.apply_model_manager_message(
                        crate::features::model_manager::Message::PreviewPresented,
                    ));
                    if staged
                        && let Some((position, angles)) =
                            self.state.features.model_manager.saved_stage_placement()
                    {
                        tasks.push(self.apply_model_preview_message(
                            file_preview::Message::StagePlacementRestored { position, angles },
                        ));
                    }
                    let (skin, bodygroups) = self.state.features.model_manager.preview_selection();
                    tasks.push(
                        self.apply_model_preview_message(file_preview::Message::SkinSelected(skin)),
                    );
                    for (group, choice) in bodygroups.into_iter().enumerate() {
                        tasks.push(self.apply_model_preview_message(
                            file_preview::Message::BodygroupChoiceSelected { group, choice },
                        ));
                    }
                }
                Err(error) => tasks.push(self.apply_model_manager_message(
                    crate::features::model_manager::Message::PreviewFailed(error),
                )),
            }
        }
        self.sync_model_workspace();
        Task::batch(tasks)
    }

    fn run_file_preview_effect(&mut self, effect: file_preview::Effect) -> Task<RootMessage> {
        match effect {
            file_preview::Effect::ModalCloseRequested => self.file_preview_close_finished_task(),
            file_preview::Effect::LoadRequested(request) => self.file_preview_load_task(request),
            file_preview::Effect::LoadStage {
                generation,
                model,
                map,
            } => self.file_preview_stage_task(generation, model, map),
            file_preview::Effect::ExtractRequested { entry_path } => self
                .state
                .features
                .preview_gma
                .entry_extraction_request(&entry_path)
                .map_or_else(Task::none, |request| {
                    self.preview_gma_entry_extraction_task(request)
                }),
            file_preview::Effect::AudioPlayRequested {
                request_id,
                bytes,
                resume_at,
            } => self.file_preview_audio_play_task(request_id, bytes, resume_at),
            file_preview::Effect::AudioPauseRequested(request_id) => {
                self.file_preview_audio_pause_task(request_id)
            }
            file_preview::Effect::AudioStopRequested => self.file_preview_audio_stop_task(),
            file_preview::Effect::AudioPositionPollRequested(request_id) => {
                self.file_preview_audio_position_poll_task(request_id)
            }
            file_preview::Effect::DoorAudioEvent(event) => {
                self.file_preview_door_audio_event_task(event)
            }
            file_preview::Effect::DoorAudioStopRequested => {
                self.file_preview_door_audio_stop_task()
            }
        }
    }

    pub(super) fn file_preview_close_finished_task(&mut self) -> Task<RootMessage> {
        self.apply_file_preview_message(file_preview::Message::CloseFinished)
    }

    fn file_preview_load_task(&self, request: PreviewRequest) -> Task<RootMessage> {
        let request_id = request.request_id;
        let tokens = self.state.tokens;
        let gmod_dir = self
            .environment
            .ctx
            .settings_and_paths_snapshot()
            .1
            .gmod_dir;
        let ctx = self.environment.ctx.clone();
        Task::stream(stream::channel(100, async move |output| {
            let mut schedule_error_output = output.clone();
            let schedule = ctx.spawn_blocking_detached("file-preview-load", move |app| {
                let mut stage_output = output.clone();
                let mut output = output;
                let result = load_preview(
                    &request,
                    &tokens,
                    gmod_dir,
                    app.cpu_executor(),
                    &mut |stage| {
                        let _ = send_root_message(
                            &mut stage_output,
                            RootMessage::FilePreview(file_preview::Message::LoadStageChanged(
                                request_id, stage,
                            )),
                        );
                    },
                );
                let _ = send_root_message(
                    &mut output,
                    RootMessage::FilePreview(file_preview::Message::Loaded(
                        request_id,
                        Box::new(result),
                    )),
                );
            });
            if let Err(error) = schedule {
                log::warn!("failed to schedule file-preview worker: {error}");
                let _ = send_root_message(
                    &mut schedule_error_output,
                    RootMessage::FilePreview(file_preview::Message::Loaded(
                        request_id,
                        Box::new(Err(error.into())),
                    )),
                );
            }
        }))
    }

    fn file_preview_stage_task(
        &self,
        generation: Generation,
        model: PreviewRequest,
        map: PreviewRequest,
    ) -> Task<RootMessage> {
        let tokens = self.state.tokens;
        let gmod_dir = self
            .environment
            .ctx
            .settings_and_paths_snapshot()
            .1
            .gmod_dir;
        let cached_map = self
            .state
            .features
            .model_preview
            .request()
            .filter(|request| {
                request.crc32 == map.crc32
                    && request.entry_path == map.entry_path
                    && request.archive == map.archive
            })
            .and_then(|_| self.state.features.model_preview.current())
            .filter(|data| matches!(data.content, PreviewContent::Map { .. }))
            .cloned();
        let ctx = self.environment.ctx.clone();
        Task::stream(stream::channel(100, async move |output| {
            let mut schedule_error_output = output.clone();
            let schedule = ctx.spawn_blocking_detached("model-stage-load", move |app| {
                let mut stage_output = output.clone();
                let mut output = output;
                let mut on_stage = |stage| {
                    let _ = send_root_message(
                        &mut stage_output,
                        RootMessage::FilePreview(file_preview::Message::LoadStageChanged(
                            generation, stage,
                        )),
                    );
                };
                let result = (|| {
                    let loaded_model = load_preview(
                        &model,
                        &tokens,
                        gmod_dir.clone(),
                        app.cpu_executor(),
                        &mut on_stage,
                    )?;
                    let PreviewContent::Model(model_preview) = loaded_model.content else {
                        return Err(PreviewLoadError::Decode(format!(
                            "Cannot decode model {}",
                            model.display_name
                        )));
                    };
                    let loaded_map = match cached_map {
                        Some(data) => crate::media::preview_model::PreviewData::from_request(
                            &map,
                            data.content,
                        ),
                        None => load_preview(
                            &map,
                            &tokens,
                            gmod_dir,
                            app.cpu_executor(),
                            &mut on_stage,
                        )?,
                    };
                    if !matches!(loaded_map.content, PreviewContent::Map { .. }) {
                        return Err(PreviewLoadError::Decode(format!(
                            "Cannot decode map {}",
                            map.display_name
                        )));
                    }
                    Ok((loaded_map, model_preview))
                })();
                let _ = send_root_message(
                    &mut output,
                    RootMessage::FilePreview(file_preview::Message::StageLoaded(
                        generation,
                        Box::new(result),
                    )),
                );
            });
            if let Err(error) = schedule {
                log::warn!("failed to schedule model-stage worker: {error}");
                let _ = send_root_message(
                    &mut schedule_error_output,
                    RootMessage::FilePreview(file_preview::Message::StageLoaded(
                        generation,
                        Box::new(Err(error.into())),
                    )),
                );
            }
        }))
    }
}
