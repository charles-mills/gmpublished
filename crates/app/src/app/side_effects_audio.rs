use std::sync::Arc;

use iced::Task;

use super::{App, RootMessage, file_preview};
use crate::media::audio_playback::AudioPlayback;

impl App {
    fn ensure_audio_playback(&mut self) -> Option<&mut AudioPlayback> {
        if self.audio_playback.is_none() {
            match AudioPlayback::new() {
                Ok(playback) => self.audio_playback = Some(playback),
                Err(error) => {
                    log::debug!("audio output unavailable: {error}");
                    return None;
                }
            }
        }
        self.audio_playback.as_mut()
    }

    pub(super) fn file_preview_audio_play_task(
        &mut self,
        bytes: Arc<Vec<u8>>,
        resume_at: f32,
    ) -> Task<RootMessage> {
        let Some(playback) = self.ensure_audio_playback() else {
            return Task::done(RootMessage::FilePreview(
                file_preview::Message::AudioPlaybackEnded,
            ));
        };

        match playback.play(bytes, resume_at) {
            Ok(()) => Task::done(RootMessage::FilePreview(
                file_preview::Message::AudioPlaybackStarted,
            )),
            Err(error) => {
                log::debug!("file preview audio playback failed: {error}");
                self.audio_playback = None;
                Task::done(RootMessage::FilePreview(
                    file_preview::Message::AudioPlaybackEnded,
                ))
            }
        }
    }

    pub(super) fn file_preview_audio_pause_task(&self) -> Task<RootMessage> {
        if let Some(playback) = self.audio_playback.as_ref() {
            playback.pause();
        }
        Task::done(RootMessage::FilePreview(
            file_preview::Message::AudioPlaybackPaused,
        ))
    }

    pub(super) fn file_preview_audio_stop_task(&mut self) -> Task<RootMessage> {
        self.audio_playback = None;
        Task::none()
    }

    pub(super) fn file_preview_door_audio_stop_task(&mut self) -> Task<RootMessage> {
        if let Some(playback) = self.audio_playback.as_mut() {
            playback.stop_door_audio();
        }
        Task::none()
    }

    pub(super) fn file_preview_door_audio_event_task(
        &mut self,
        event: file_preview::DoorAudioEvent,
    ) -> Task<RootMessage> {
        let Some(door) = self.current_preview_door_for_audio(event) else {
            return Task::none();
        };
        let door = door.clone();
        let Some(playback) = self.ensure_audio_playback() else {
            return Task::none();
        };
        playback.handle_door_audio_event(event, &door);
        Task::none()
    }

    pub(super) fn file_preview_audio_position_poll_task(&mut self) -> Task<RootMessage> {
        let Some(playback) = self.audio_playback.as_ref() else {
            return Task::done(RootMessage::FilePreview(
                file_preview::Message::AudioPlaybackEnded,
            ));
        };

        if playback.empty() {
            self.audio_playback = None;
            Task::done(RootMessage::FilePreview(
                file_preview::Message::AudioPlaybackEnded,
            ))
        } else {
            Task::done(RootMessage::FilePreview(
                file_preview::Message::AudioPositionUpdated(playback.position_secs()),
            ))
        }
    }

    fn current_preview_door_for_audio(
        &self,
        event: file_preview::DoorAudioEvent,
    ) -> Option<&file_preview::DoorInstance> {
        let data = self.state.file_preview.current()?;
        let expected_content_id = data.content_id();
        if event.content_id != expected_content_id {
            return None;
        }
        let file_preview::PreviewContent::Map { scene, .. } = &data.content else {
            return None;
        };
        scene.doors.get(event.door_index)
    }
}
