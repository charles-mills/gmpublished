use super::model::DoorAudioEvent;
use super::model::PreviewRequest;
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
        bytes: Arc<Vec<u8>>,
        resume_at: f32,
    },
        AudioPauseRequested,
        AudioStopRequested,
        AudioPositionPollRequested,
        DoorAudioEvent(DoorAudioEvent),
        DoorAudioStopRequested,
}
