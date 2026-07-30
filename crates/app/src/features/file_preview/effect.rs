use crate::generation::Generation;
use crate::media::preview_model::DoorAudioEvent;
use crate::media::preview_model::PreviewRequest;
use std::sync::Arc;

/// Outward consequences of a File Preview state transition.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::enum_variant_names)]
pub enum Effect {
    ModalCloseRequested,
    LoadRequested(PreviewRequest),
    ExtractRequested {
        entry_path: String,
    },
    AudioPlayRequested {
        request_id: Generation,
        bytes: Arc<[u8]>,
        resume_at: f32,
    },
    AudioPauseRequested(Generation),
    AudioStopRequested,
    AudioPositionPollRequested(Generation),
    DoorAudioEvent(DoorAudioEvent),
    DoorAudioStopRequested,
}
