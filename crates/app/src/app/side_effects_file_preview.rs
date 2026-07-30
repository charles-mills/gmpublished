use iced::Task;

use super::{App, RootMessage, file_preview, send_root_message, stream};
use crate::media::file_preview_decode::load_preview;
use crate::media::preview_model::PreviewRequest;

impl App {
    pub(super) fn apply_file_preview_message(
        &mut self,
        message: file_preview::Message,
    ) -> Task<RootMessage> {
        let effects = file_preview::update(&mut self.state.file_preview, message);
        self.batch_effects(effects, Self::run_file_preview_effect)
    }

    fn run_file_preview_effect(&mut self, effect: file_preview::Effect) -> Task<RootMessage> {
        match effect {
            file_preview::Effect::ModalCloseRequested => self.file_preview_close_finished_task(),
            file_preview::Effect::LoadRequested(request) => self.file_preview_load_task(request),
            file_preview::Effect::ExtractRequested { entry_path } => self
                .state
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
        let gmod_dir = self.ctx.settings_and_paths_snapshot().1.gmod_dir;
        let ctx = self.ctx.clone();
        Task::stream(stream::channel(100, async move |output| {
            let mut schedule_error_output = output.clone();
            let schedule = ctx.spawn_blocking_detached("file-preview-load", move |_app| {
                // The decoder reports progress as plain stages; turning those
                // into messages is this layer's job, not its.
                let mut stage_output = output.clone();
                let mut output = output;
                let result = load_preview(&request, &tokens, gmod_dir, &mut |stage| {
                    let _ = send_root_message(
                        &mut stage_output,
                        RootMessage::FilePreview(file_preview::Message::LoadStageChanged(
                            request_id, stage,
                        )),
                    );
                });
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
}
