use iced::Task;

use super::{App, RootMessage, file_preview, stream};
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
        let (effects, manager_effects) = crate::features::model_manager::update_viewer(
            &mut self.state.features.model_manager,
            &mut self.state.features.model_preview,
            message,
        );
        let tasks = effects
            .into_iter()
            .map(|effect| self.run_model_preview_effect(effect))
            .collect::<Vec<_>>();
        let manager = self.batch_effects(manager_effects, Self::run_model_manager_effect);
        self.sync_model_workspace();
        Task::batch(tasks.into_iter().chain([manager]))
    }

    fn run_model_preview_effect(&mut self, effect: file_preview::Effect) -> Task<RootMessage> {
        use file_preview::Effect;
        match effect {
            Effect::LoadRequested(request) => self
                .file_preview_load_task(
                    request,
                    self.state.features.model_manager.preview_directory(),
                )
                .map(RootMessage::ModelPreview),
            Effect::LoadStage {
                generation,
                model,
                map,
            } => self
                .file_preview_stage_task(
                    generation,
                    model,
                    map,
                    self.state.features.model_manager.preview_directory(),
                )
                .map(RootMessage::ModelPreview),
            Effect::MouseCaptureChanged { captured, epoch } => self
                .preview_mouse_capture_task(captured, epoch)
                .map(RootMessage::ModelPreview),
            Effect::ModalCloseRequested => {
                self.apply_model_preview_message(file_preview::Message::CloseFinished)
            }
            Effect::DoorAudioEvent(event) => self.file_preview_door_audio_event_task(event),
            Effect::DoorAudioStopRequested => self.file_preview_door_audio_stop_task(),
            Effect::AudioPlayRequested { .. }
            | Effect::AudioPauseRequested(_)
            | Effect::AudioStopRequested
            | Effect::AudioPositionPollRequested(_)
            | Effect::ExtractRequested { .. } => Task::none(),
        }
    }

    fn preview_mouse_capture_task(
        &self,
        captured: bool,
        epoch: Generation,
    ) -> Task<file_preview::Message> {
        let Some(id) = self.window_id else {
            return Task::done(file_preview::Message::MouseCaptureFailed(epoch));
        };
        iced::window::capture_cursor(id, captured).then(move |result| {
            if let Err(error) = result {
                log::warn!("Failed to change map mouse capture: {error}");
                if captured {
                    return Task::done(file_preview::Message::MouseCaptureFailed(epoch));
                }
            }
            Task::none()
        })
    }

    fn run_file_preview_effect(&mut self, effect: file_preview::Effect) -> Task<RootMessage> {
        match effect {
            file_preview::Effect::MouseCaptureChanged { captured, epoch } => self
                .preview_mouse_capture_task(captured, epoch)
                .map(RootMessage::FilePreview),
            file_preview::Effect::ModalCloseRequested => self.file_preview_close_finished_task(),
            file_preview::Effect::LoadRequested(request) => self
                .file_preview_load_task(request, None)
                .map(RootMessage::FilePreview),
            file_preview::Effect::LoadStage {
                generation,
                model,
                map,
            } => self
                .file_preview_stage_task(generation, model, map, None)
                .map(RootMessage::FilePreview),
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

    fn file_preview_load_task(
        &self,
        request: PreviewRequest,
        lease: Option<std::sync::Arc<tempfile::TempDir>>,
    ) -> Task<file_preview::Message> {
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
                let _lease = lease;
                let mut stage_output = output.clone();
                let mut output = output;
                let result = load_preview(
                    &request,
                    &tokens,
                    gmod_dir,
                    app.cpu_executor(),
                    &mut |stage| {
                        if request.cancellation.is_cancelled() {
                            return;
                        }
                        let _ = crate::util::channel::send_blocking(
                            &mut stage_output,
                            file_preview::Message::LoadStageChanged(request_id, stage),
                        );
                    },
                );
                if request.cancellation.is_cancelled() {
                    return;
                }
                let _ = crate::util::channel::send_blocking(
                    &mut output,
                    file_preview::Message::Loaded(request_id, Box::new(result)),
                );
            });
            if let Err(error) = schedule {
                log::warn!("failed to schedule file-preview worker: {error}");
                let _ = crate::util::channel::send_blocking(
                    &mut schedule_error_output,
                    file_preview::Message::Loaded(request_id, Box::new(Err(error.into()))),
                );
            }
        }))
    }

    fn file_preview_stage_task(
        &self,
        generation: Generation,
        model: PreviewRequest,
        map: PreviewRequest,
        lease: Option<std::sync::Arc<tempfile::TempDir>>,
    ) -> Task<file_preview::Message> {
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
                    && request.map_texture_quality == map.map_texture_quality
            })
            .and_then(|_| self.state.features.model_preview.current())
            .filter(|data| matches!(data.content, PreviewContent::Map { .. }))
            .cloned();
        let ctx = self.environment.ctx.clone();
        Task::stream(stream::channel(100, async move |output| {
            let mut schedule_error_output = output.clone();
            let schedule = ctx.spawn_blocking_detached("model-stage-load", move |app| {
                let _lease = lease;
                let mut stage_output = output.clone();
                let mut output = output;
                let mut on_stage = |stage| {
                    if model.cancellation.is_cancelled() || map.cancellation.is_cancelled() {
                        return;
                    }
                    let _ = crate::util::channel::send_blocking(
                        &mut stage_output,
                        file_preview::Message::LoadStageChanged(generation, stage),
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
                    map.cancellation.check()?;
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
                if model.cancellation.is_cancelled() || map.cancellation.is_cancelled() {
                    return;
                }
                let _ = crate::util::channel::send_blocking(
                    &mut output,
                    file_preview::Message::StageLoaded(generation, Box::new(result)),
                );
            });
            if let Err(error) = schedule {
                log::warn!("failed to schedule model-stage worker: {error}");
                let _ = crate::util::channel::send_blocking(
                    &mut schedule_error_output,
                    file_preview::Message::StageLoaded(generation, Box::new(Err(error.into()))),
                );
            }
        }))
    }
}
