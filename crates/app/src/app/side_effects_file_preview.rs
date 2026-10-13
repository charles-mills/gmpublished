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
        if matches!(message, file_preview::Message::BackRequested) {
            self.capture_model_stage();
        }
        let effects = file_preview::update(&mut self.state.features.file_preview, message);
        self.batch_effects(effects, Self::run_file_preview_effect)
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
                    let loaded_map =
                        load_preview(&map, &tokens, gmod_dir, app.cpu_executor(), &mut on_stage)?;
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
