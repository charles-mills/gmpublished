use gmpublished_domain::math::Vec3;
use std::time::Instant;

use iced::widget::pane_grid;

use super::state::{FlyPose, MovementMode, OrbitPose, StageTransform};
use crate::generation::Generation;
use crate::media::preview_model::{
    DoorAudioEvent, PreviewData, PreviewLoadError, PreviewLoadStage, PreviewRequest,
};
use gmpublished_domain::particles::ControlPointIndex;

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    OpenRequested(PreviewRequest),
    WorkspaceOpenRequested(PreviewRequest),
    WorkspaceLoadCancelled,
    WorkspaceStageRequested {
        model: Box<PreviewRequest>,
        map: Box<PreviewRequest>,
    },
    StageLoaded(
        Generation,
        Box<
            Result<
                (
                    PreviewData,
                    std::sync::Arc<crate::media::preview_model::ModelPreview>,
                ),
                PreviewLoadError,
            >,
        >,
    ),
    StageTransformChanged(StageTransform),
    StageCameraChanged {
        pose: FlyPose,
        mode: MovementMode,
        transform: StageTransform,
        door_audio_events: Vec<DoorAudioEvent>,
    },
    StageMoveStarted,
    StageMoveFinished,
    StagePositionEdited {
        axis: usize,
        value: String,
    },
    StageRotationEdited {
        axis: usize,
        value: String,
    },
    StageResetRequested,
    StageDropRequested,
    StageUndoRequested,
    StagePlaceAheadRequested,
    LoadStageChanged(Generation, PreviewLoadStage),
    Loaded(Generation, Box<Result<PreviewData, PreviewLoadError>>),
    AnimationTick(Instant),
    RenderFeedbackChanged(Generation),
    AudioToggleRequested,
    AudioPlaybackStarted(Generation),
    AudioPlaybackPaused(Generation),
    AudioPlaybackEnded(Generation),
    AudioPositionUpdated(Generation, f32),
    ModelSequenceSelected(usize),
    ModelPlaybackToggled,
    ModelAnimationSeeked(f32),
    ModelPoseParameterChanged {
        parameter: usize,
        value: f32,
    },
    LodSelected(usize),
    SkinSelected(usize),
    BodygroupChoiceSelected {
        group: usize,
        choice: usize,
    },
    MouseCaptureRequested(bool),
    MouseCaptureFailed(Generation),
    MapFogToggled(bool),
    MapSkyboxToggled(bool),
    MapVisibilityToggled(bool),
    MapReducedTextureDetailToggled(bool),
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
    ModelCameraResetRequested,
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
