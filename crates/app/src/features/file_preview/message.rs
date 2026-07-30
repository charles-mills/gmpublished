use gmpublished_backend::math::Vec3;
use std::time::Instant;

use iced::widget::pane_grid;

use super::state::{FlyPose, MovementMode, OrbitPose};
use crate::generation::Generation;
use crate::media::preview_model::{
    DoorAudioEvent, PreviewData, PreviewLoadError, PreviewLoadStage, PreviewRequest,
};
use gmpublished_backend::particles::ControlPointIndex;

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
    AudioPlaybackStarted(Generation),
    AudioPlaybackPaused(Generation),
    AudioPlaybackEnded(Generation),
    AudioPositionUpdated(Generation, f32),
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
        position: Vec3,
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
