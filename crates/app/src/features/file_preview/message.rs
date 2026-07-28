use std::time::Instant;

use iced::widget::pane_grid;

use super::model::DoorAudioEvent;
use super::model::{PreviewData, PreviewLoadStage, PreviewRequest};
use super::state::{FlyPose, MovementMode, OrbitPose};
use crate::bridge::archive::PreviewArchiveSourceError;
use crate::bridge::tasks::ScheduleError;
use crate::generation::Generation;
use gmpublished_backend::particles::ControlPointIndex;

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
    LoadStageChanged(Generation, PreviewLoadStage),
    /// Boxed: the inline `PreviewData` (a `PreviewContent::Map` carries ~170
    /// bytes of scene metadata) otherwise sets the size of this enum, and
    /// through it `RootMessage` — which every message pays, including the
    /// per-frame animation ticks.
    Loaded(Generation, Box<Result<PreviewData, PreviewLoadError>>),
    AnimationTick(Instant),
    AudioToggleRequested,
    AudioPlaybackStarted,
    AudioPlaybackPaused,
    AudioPlaybackEnded,
    AudioPositionUpdated(f32),
    SkinSelected(usize),
    BodygroupChoiceSelected {
        group: usize,
        choice: usize,
    },
    MapFogToggled(bool),
    MapSkyboxToggled(bool),
    MapVisibilityToggled(bool),
    PhyDebugToggled(bool),
    FlyCameraChanged {
        pose: FlyPose,
        mode: MovementMode,
    },
    FlyCameraAndDoorAudioChanged {
        pose: FlyPose,
        mode: MovementMode,
        door_audio_events: Vec<DoorAudioEvent>,
    },
    FlySpeedChanged {
        pose: FlyPose,
        mode: MovementMode,
    },
    MovementModeSelected(MovementMode),
    DoorAudioEvents(Vec<DoorAudioEvent>),
    OrbitPoseChanged(OrbitPose),
    ParticleSystemSelected(usize),
    ParticlePlayToggled,
    ParticleRestartRequested,
    ParticleSpeedSelected(f32),
    ParticleControlPointChanged {
        index: ControlPointIndex,
        position: [f32; 3],
    },
    InspectorResized {
        split: pane_grid::Split,
        ratio: f32,
    },
    InspectorLayoutChanged(f32),
    InspectorReset(f32),
    BackRequested,
    ExpandToggled,
    CloseFinished,
    RelatedPreviewRequested(String),
    LoadAnywayRequested,
    ExtractRequested,
}
