use std::time::Instant;

use iced::widget::pane_grid;

#[cfg(feature = "asset-studio")]
use super::model::DoorAudioEvent;
use super::model::{PreviewData, PreviewLoadStage, PreviewRequest};
#[cfg(feature = "asset-studio")]
use super::state::{FlyPose, MovementMode, OrbitPose};
use crate::backend::archive::PreviewArchiveSourceError;
use crate::backend::tasks::ScheduleError;

/// Why loading a preview entry failed. Variants carry the actual producer
/// error so its `Display` reaches the user verbatim; only the wire boundary
/// (this type's `Display`) is rendered.
#[derive(Clone, Debug, thiserror::Error)]
pub enum PreviewLoadError {
    /// The blocking worker pool rejected the load job before it could run.
    #[error(transparent)]
    Schedule(#[from] ScheduleError),
    /// Reading the entry's bytes out of the archive failed.
    #[error(transparent)]
    Archive(#[from] PreviewArchiveSourceError),
}

// `GmaError::IOError` carries an `Option<Arc<io::Error>>`, which has no
// `PartialEq`, so derive isn't available; compare the rendered text instead.
impl PartialEq for PreviewLoadError {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
}

/// Facts emitted by the in-archive file preview modal.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    OpenRequested(PreviewRequest),
    LoadStageChanged(u64, PreviewLoadStage),
    Loaded(u64, Result<PreviewData, PreviewLoadError>),
    AnimationTick(Instant),
    #[cfg(feature = "asset-studio")]
    AudioToggleRequested,
    #[cfg(feature = "asset-studio")]
    AudioPlaybackStarted,
    #[cfg(feature = "asset-studio")]
    AudioPlaybackPaused,
    #[cfg(feature = "asset-studio")]
    AudioPlaybackEnded,
    #[cfg(feature = "asset-studio")]
    AudioPositionUpdated(f32),
    #[cfg(feature = "asset-studio")]
    SkinSelected(usize),
    #[cfg(feature = "asset-studio")]
    BodygroupChoiceSelected {
        group: usize,
        choice: usize,
    },
    #[cfg(feature = "asset-studio")]
    MapFogToggled(bool),
    #[cfg(feature = "asset-studio")]
    MapSkyboxToggled(bool),
    #[cfg(feature = "asset-studio")]
    MapVisibilityToggled(bool),
    #[cfg(feature = "asset-studio")]
    PhyDebugToggled(bool),
    #[cfg(feature = "asset-studio")]
    FlyCameraChanged {
        pose: FlyPose,
        mode: MovementMode,
    },
    #[cfg(feature = "asset-studio")]
    FlyCameraAndDoorAudioChanged {
        pose: FlyPose,
        mode: MovementMode,
        door_audio_events: Vec<DoorAudioEvent>,
    },
    #[cfg(feature = "asset-studio")]
    FlySpeedChanged {
        pose: FlyPose,
        mode: MovementMode,
    },
    #[cfg(feature = "asset-studio")]
    MovementModeSelected(MovementMode),
    #[cfg(feature = "asset-studio")]
    DoorAudioEvents(Vec<DoorAudioEvent>),
    #[cfg(feature = "asset-studio")]
    OrbitPoseChanged(OrbitPose),
    #[cfg(feature = "asset-studio")]
    ParticleSystemSelected(usize),
    #[cfg(feature = "asset-studio")]
    ParticlePlayToggled,
    #[cfg(feature = "asset-studio")]
    ParticleRestartRequested,
    #[cfg(feature = "asset-studio")]
    ParticleSpeedSelected(f32),
    #[cfg(feature = "asset-studio")]
    ParticleControlPointChanged {
        index: usize,
        position: [f32; 3],
    },
    #[cfg(feature = "asset-studio")]
    InspectorResized {
        split: pane_grid::Split,
        ratio: f32,
    },
    #[cfg(feature = "asset-studio")]
    InspectorLayoutChanged(f32),
    #[cfg(feature = "asset-studio")]
    InspectorReset(f32),
    BackRequested,
    ExpandToggled,
    CloseFinished,
    RelatedPreviewRequested(String),
    LoadAnywayRequested,
    ExtractRequested,
}
