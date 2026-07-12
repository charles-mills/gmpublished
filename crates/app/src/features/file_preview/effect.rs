#[cfg(feature = "asset-studio")]
use super::model::DoorAudioEvent;
use super::model::PreviewRequest;
#[cfg(feature = "asset-studio")]
use std::sync::Arc;

/// Outward consequences of a File Preview state transition.
// Without the asset-studio feature, every remaining variant shares the
// "Requested" suffix.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "asset-studio"), derive(Eq))]
#[allow(clippy::enum_variant_names)]
pub enum Effect {
    ModalCloseRequested,
    LoadRequested(PreviewRequest),
    ExtractRequested {
        entry_path: String,
    },
    #[cfg(feature = "asset-studio")]
    AudioPlayRequested {
        bytes: Arc<Vec<u8>>,
        resume_at: f32,
    },
    #[cfg(feature = "asset-studio")]
    AudioPauseRequested,
    #[cfg(feature = "asset-studio")]
    AudioStopRequested,
    #[cfg(feature = "asset-studio")]
    AudioPositionPollRequested,
    #[cfg(feature = "asset-studio")]
    DoorAudioEvent(DoorAudioEvent),
    #[cfg(feature = "asset-studio")]
    DoorAudioStopRequested,
}
