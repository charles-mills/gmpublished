mod model_animation;
mod staging;
mod workspace;
use gmpublished_domain::math::Vec3;
use staging::StageState;
pub use staging::StageTransform;
use std::time::{Duration, Instant};

use iced::widget::pane_grid;

use crate::generation::Generation;
use crate::media::preview_model::{
    PreviewContent, PreviewData, PreviewLoadError, PreviewLoadStage, PreviewRequest,
};
use crate::spinner_clock::SpinnerClock;
use crate::widgets::split_pane;
use gmpublished_domain::particles::{ControlPointIndex, MAX_CONTROL_POINTS};

const FLY_SPEED_READOUT_VISIBLE_FOR: Duration = Duration::from_millis(800);
const DEFAULT_VIEWER_RATIO: f32 = (704.0 - 236.0) / 704.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Pane {
    Viewer,
    Inspector,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlyPose {
    pub(crate) position: Vec3,
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    pub(crate) speed: f32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MovementMode {
    #[default]
    Fly,
    Walk,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbitPose {
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    pub(crate) distance: f32,
}

impl Default for OrbitPose {
    fn default() -> Self {
        Self {
            yaw: std::f32::consts::FRAC_PI_4,
            pitch: 0.35,
            distance: 1.0,
        }
    }
}

impl FlyPose {
    fn is_finite(self) -> bool {
        self.position.is_finite()
            && self.yaw.is_finite()
            && self.pitch.is_finite()
            && self.speed.is_finite()
    }
}

impl OrbitPose {
    fn is_finite(self) -> bool {
        self.yaw.is_finite() && self.pitch.is_finite() && self.distance.is_finite()
    }
}

#[derive(Debug)]
pub struct State {
    open: bool,
    expanded: bool,
    load: LoadState,
    request_id: Generation,
    camera_epoch: Generation,
    request: Option<PreviewRequest>,
    stage: Option<StageState>,
    stage_request: Option<PreviewRequest>,
    workspace_retained: Option<WorkspaceRetained>,
    workspace_pending: bool,
    workspace_error: Option<PreviewLoadError>,
    workspace_transform: [[f32; 4]; 4],
    workspace_camera_bounds: Option<crate::media::preview_model::ModelBounds>,
    workspace_material_tints: Vec<(usize, [u8; 3])>,
    workspace_skin_remap: Option<Vec<u16>>,
    spinner: SpinnerClock,
    content_ui: ContentUiState,
    inspector_panes: split_pane::State<Pane>,
}

#[derive(Debug, Default)]
enum LoadState {
    #[default]
    Idle,
    Loading {
        stage: Option<PreviewLoadStage>,
    },
    Ready(Box<PreviewData>),
    Failed(PreviewLoadError),
}

#[derive(Debug)]
struct WorkspaceRetained {
    data: Box<PreviewData>,
    request: Option<PreviewRequest>,
}

/// CP0 is the effect origin, pinned at the viewport centre; the rest fan out
/// along +X so two-point effects (beams, tracers) are visible immediately.
const fn default_particle_control_points() -> [Vec3; MAX_CONTROL_POINTS] {
    let mut points = [Vec3::splat(0.0); MAX_CONTROL_POINTS];
    let mut index = 1;
    while index < MAX_CONTROL_POINTS {
        points[index] = Vec3::new(96.0 * index as f32, 0.0, 0.0);
        index += 1;
    }
    points
}

#[derive(Clone, Debug, PartialEq)]
struct FlySpeedReadout {
    speed: f32,
    started_at: Option<Instant>,
    now: Option<Instant>,
}

#[derive(Debug, Default)]
enum ContentUiState {
    #[default]
    None,
    Audio(AudioUiState),
    Model(ModelUiState),
    Map(MapUiState),
    Particle(ParticleUiState),
}

#[derive(Debug)]
struct AudioUiState {
    playing: bool,
    position_secs: f32,
    duration_secs: Option<f32>,
}

#[derive(Debug, Default)]
struct ModelUiState {
    framing_bounds: Option<crate::media::preview_model::ModelBounds>,
    animation: model_animation::Playback,
    selected_skin: usize,
    bodygroup_choices: Vec<usize>,
    orbit_pose: Option<OrbitPose>,
    phy_debug_enabled: bool,
}

impl ModelUiState {
    fn for_model(model: &crate::media::preview_model::ModelPreview) -> Self {
        let animation = model_animation::Playback::for_model(model);
        let bodygroup_choices = vec![0; model.bodygroups.len()];
        let pose =
            animation.sequence.map(
                |sequence| gmpublished_backend::model_decompile::PreviewPose {
                    sequence,
                    cycle: 0.0,
                    parameters: animation.parameters.clone(),
                },
            );
        let framing_bounds = model.visible_bounds(pose.as_ref(), &bodygroup_choices);
        Self {
            animation,
            bodygroup_choices,
            framing_bounds,
            ..Self::default()
        }
    }
}

#[expect(clippy::struct_excessive_bools)]
#[derive(Debug)]
struct MapUiState {
    fog_enabled: bool,
    skybox_enabled: bool,
    visibility_enabled: bool,
    phy_debug_enabled: bool,
    speed_readout: Option<FlySpeedReadout>,
    fly_pose: Option<FlyPose>,
    movement_mode: Option<MovementMode>,
    requested_movement_mode: Option<MovementMode>,
}

#[derive(Debug)]
struct ParticleUiState {
    system: usize,
    playing: bool,
    speed: f32,
    restart_epoch: Generation,
    control_points: [Vec3; MAX_CONTROL_POINTS],
    orbit_pose: Option<OrbitPose>,
}

impl Default for AudioUiState {
    fn default() -> Self {
        Self {
            playing: false,
            position_secs: 0.0,
            duration_secs: None,
        }
    }
}

impl Default for MapUiState {
    fn default() -> Self {
        Self {
            fog_enabled: true,
            skybox_enabled: true,
            visibility_enabled: true,
            phy_debug_enabled: false,
            speed_readout: None,
            fly_pose: None,
            movement_mode: None,
            requested_movement_mode: None,
        }
    }
}

impl Default for ParticleUiState {
    fn default() -> Self {
        Self {
            system: 0,
            playing: true,
            speed: 1.0,
            restart_epoch: Generation::INITIAL,
            control_points: default_particle_control_points(),
            orbit_pose: None,
        }
    }
}

impl ContentUiState {
    fn from_request(request: &PreviewRequest) -> Self {
        use crate::media::file_kind::{EntryClass, PreviewClass, classify_entry_path};
        match classify_entry_path(&request.entry_path) {
            EntryClass::ModelCompanion | EntryClass::Previewable(PreviewClass::Model) => {
                Self::Model(ModelUiState::default())
            }
            EntryClass::Previewable(PreviewClass::Map) => Self::Map(MapUiState::default()),
            EntryClass::Previewable(PreviewClass::Particle) => {
                Self::Particle(ParticleUiState::default())
            }
            EntryClass::Previewable(PreviewClass::Audio) => Self::Audio(AudioUiState::default()),
            EntryClass::Previewable(_) => Self::None,
        }
    }

    fn from_content(content: &PreviewContent) -> Self {
        match content {
            PreviewContent::Audio { duration_secs, .. } => Self::Audio(AudioUiState {
                duration_secs: *duration_secs,
                ..AudioUiState::default()
            }),
            PreviewContent::Model(model) => Self::Model(ModelUiState::for_model(model)),
            PreviewContent::Map { .. } => Self::Map(MapUiState::default()),
            PreviewContent::Particle(_) => Self::Particle(ParticleUiState::default()),
            PreviewContent::Code { .. }
            | PreviewContent::Image { .. }
            | PreviewContent::Font { .. }
            | PreviewContent::Info { .. } => Self::None,
        }
    }

    fn reconcile_with_content(&mut self, content: &PreviewContent) {
        match (&mut *self, content) {
            (Self::Audio(audio), PreviewContent::Audio { duration_secs, .. }) => {
                audio.duration_secs = *duration_secs;
            }
            (Self::Model(model_ui), PreviewContent::Model(model)) => {
                let fresh = ModelUiState::for_model(model);
                model_ui.animation = fresh.animation;
                model_ui.framing_bounds = fresh.framing_bounds;
                model_ui.selected_skin = 0;
                model_ui.bodygroup_choices = fresh.bodygroup_choices;
            }
            (Self::Map(_), PreviewContent::Map { .. })
            | (Self::Particle(_), PreviewContent::Particle(_)) => {}
            (state, content) => *state = Self::from_content(content),
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            expanded: false,
            load: LoadState::Idle,
            request_id: Generation::INITIAL,
            camera_epoch: Generation::INITIAL,
            request: None,
            stage: None,
            stage_request: None,
            workspace_retained: None,
            workspace_pending: false,
            workspace_error: None,
            workspace_transform: workspace::IDENTITY,
            workspace_camera_bounds: None,
            workspace_material_tints: vec![],
            workspace_skin_remap: None,
            spinner: SpinnerClock::Idle,
            content_ui: ContentUiState::None,
            inspector_panes: split_pane::State::vertical(
                Pane::Viewer,
                Pane::Inspector,
                DEFAULT_VIEWER_RATIO,
            ),
        }
    }
}

impl State {
    pub(crate) const fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) const fn expanded(&self) -> bool {
        self.expanded
    }

    pub(crate) const fn loading(&self) -> bool {
        matches!(self.load, LoadState::Loading { .. })
    }

    pub(crate) const fn loading_stage(&self) -> Option<PreviewLoadStage> {
        match self.load {
            LoadState::Loading { stage } => stage,
            _ => None,
        }
    }

    pub(crate) fn error(&self) -> Option<&PreviewLoadError> {
        match &self.load {
            LoadState::Failed(error) => Some(error),
            _ => self.workspace_error.as_ref(),
        }
    }

    pub(crate) fn current(&self) -> Option<&PreviewData> {
        match &self.load {
            LoadState::Ready(data) => Some(data.as_ref()),
            _ => self
                .workspace_retained
                .as_ref()
                .map(|retained| retained.data.as_ref()),
        }
    }

    fn current_mut(&mut self) -> Option<&mut PreviewData> {
        match &mut self.load {
            LoadState::Ready(data) => Some(data.as_mut()),
            _ => self
                .workspace_retained
                .as_mut()
                .map(|retained| retained.data.as_mut()),
        }
    }

    pub(super) const fn inspector_panes(&self) -> &pane_grid::State<Pane> {
        self.inspector_panes.grid()
    }

    pub(super) const fn inspector_ratio(&self) -> f32 {
        self.inspector_panes.ratio()
    }

    pub(super) fn resize_inspector(&mut self, split: pane_grid::Split, ratio: f32) {
        self.inspector_panes.resize(split, ratio);
    }

    pub(super) fn set_inspector_ratio(&mut self, ratio: f32) {
        self.inspector_panes.set_ratio(ratio);
    }

    pub(super) fn reset_inspector(&mut self) {
        self.inspector_panes.reset();
    }

    pub(crate) const fn audio_playing(&self) -> bool {
        match &self.content_ui {
            ContentUiState::Audio(audio) => audio.playing,
            _ => false,
        }
    }

    pub(crate) const fn audio_position_secs(&self) -> f32 {
        match &self.content_ui {
            ContentUiState::Audio(audio) => audio.position_secs,
            _ => 0.0,
        }
    }

    pub(crate) const fn audio_duration_secs(&self) -> Option<f32> {
        match &self.content_ui {
            ContentUiState::Audio(audio) => audio.duration_secs,
            _ => None,
        }
    }

    pub(crate) fn current_audio_bytes(&self) -> Option<std::sync::Arc<[u8]>> {
        match self.current().map(|data| &data.content) {
            Some(PreviewContent::Audio { bytes, .. }) => Some(std::sync::Arc::clone(bytes)),
            _ => None,
        }
    }

    /// Only loaded audio has a playback identity; ignore callbacks while a request is loading.
    pub(crate) fn current_audio_request_id(&self) -> Option<Generation> {
        matches!(
            self.current().map(|data| &data.content),
            Some(PreviewContent::Audio { .. })
        )
        .then_some(self.request_id)
    }

    pub(crate) const fn request(&self) -> Option<&PreviewRequest> {
        match &self.workspace_retained {
            Some(retained) => retained.request.as_ref(),
            None => self.request.as_ref(),
        }
    }

    pub(crate) fn related_preview(
        &self,
    ) -> Option<&crate::media::preview_model::RelatedPreviewTarget> {
        self.current()?.related_preview.as_ref()
    }

    pub(crate) fn related_preview_request(&self, entry_path: &str) -> Option<PreviewRequest> {
        let archive = &self.request.as_ref()?.archive;
        let entry = archive.entry(entry_path).ok()?;
        Some(PreviewRequest {
            request_id: Generation::INITIAL,
            archive: std::sync::Arc::clone(archive),
            entry_path: entry.path.to_owned(),
            display_name: entry
                .path
                .rsplit_once('/')
                .map_or(entry.path, |(_, name)| name)
                .to_owned(),
            size_bytes: entry.size,
            crc32: entry.crc32,
            bypass_size_limits: false,
        })
    }

    pub(crate) fn load_anyway_request(&self) -> Option<PreviewRequest> {
        let mut request = self.request.as_ref()?.clone();
        request.bypass_size_limits = true;
        Some(request)
    }

    pub(crate) fn begin_open(&mut self, mut request: PreviewRequest) -> PreviewRequest {
        self.request_id.bump();
        request.request_id = self.request_id;
        self.open = true;
        self.expanded = false;
        self.load = LoadState::Loading { stage: None };
        self.stage = None;
        self.stage_request = None;
        self.workspace_retained = None;
        self.workspace_pending = false;
        self.workspace_error = None;
        self.workspace_transform = workspace::IDENTITY;
        self.workspace_camera_bounds = None;
        self.workspace_material_tints.clear();
        self.workspace_skin_remap = None;
        self.request = Some(request.clone());
        self.spinner.stop();
        self.content_ui = ContentUiState::from_request(&request);
        request
    }

    pub(crate) fn begin_workspace_open(&mut self, mut request: PreviewRequest) -> PreviewRequest {
        if self.workspace_retained.is_none()
            && matches!(&self.load, LoadState::Ready(data) if matches!(data.content, PreviewContent::Model(_) | PreviewContent::Map { .. }))
            && let LoadState::Ready(data) = std::mem::take(&mut self.load)
        {
            self.workspace_retained = Some(WorkspaceRetained {
                data,
                request: self.request.take(),
            });
        }
        if self.workspace_retained.is_none() {
            self.content_ui = ContentUiState::from_request(&request);
            self.stage = None;
        }
        self.request_id.bump();
        request.request_id = self.request_id;
        self.open = true;
        self.load = LoadState::Loading { stage: None };
        self.request = Some(request.clone());
        self.stage_request = None;
        self.workspace_pending = true;
        self.workspace_error = None;
        self.spinner.stop();
        request
    }

    pub(crate) const fn spinner_visible(&self) -> bool {
        self.open && self.loading()
    }

    pub(crate) fn spinner_elapsed(&self) -> f32 {
        self.spinner.elapsed()
    }

    pub(crate) fn fly_speed_readout(&self) -> Option<f32> {
        match &self.content_ui {
            ContentUiState::Map(map) => map.speed_readout.as_ref().map(|readout| readout.speed),
            _ => None,
        }
    }

    pub(crate) const fn fly_pose(&self) -> Option<FlyPose> {
        match &self.content_ui {
            ContentUiState::Map(map) => map.fly_pose,
            _ => None,
        }
    }

    pub(crate) const fn fly_movement_mode(&self) -> Option<MovementMode> {
        match &self.content_ui {
            ContentUiState::Map(map) => map.movement_mode,
            _ => None,
        }
    }

    pub(crate) const fn requested_movement_mode(&self) -> Option<MovementMode> {
        match &self.content_ui {
            ContentUiState::Map(map) => map.requested_movement_mode,
            _ => None,
        }
    }

    pub(crate) const fn orbit_pose(&self) -> Option<OrbitPose> {
        match &self.content_ui {
            ContentUiState::Model(model) => model.orbit_pose,
            ContentUiState::Particle(particle) => particle.orbit_pose,
            _ => None,
        }
    }

    pub(super) const fn camera_epoch(&self) -> Generation {
        self.camera_epoch
    }

    pub(super) fn reset_model_camera(&mut self) {
        if self.open && matches!(self.content_ui, ContentUiState::Model(_)) {
            if self.workspace_camera_bounds.is_some() {
                self.workspace_camera_bounds = self.model_bounds();
            }
            self.set_orbit_pose(OrbitPose::default());
            self.camera_epoch.bump();
        }
    }

    pub(crate) const fn fly_speed_readout_visible(&self) -> bool {
        self.open
            && matches!(&self.content_ui, ContentUiState::Map(map) if map.speed_readout.is_some())
    }

    pub(super) fn tick_animation(&mut self, now: Instant) {
        self.tick_stage(now);
        self.tick_model_animation(now);
        if self.spinner_visible() {
            self.spinner.advance_or_start(now);
        }

        let ContentUiState::Map(map) = &mut self.content_ui else {
            return;
        };
        let Some(readout) = map.speed_readout.as_mut() else {
            return;
        };
        if readout.started_at.is_none() {
            readout.started_at = Some(now);
        }
        readout.now = Some(now);
        if readout.started_at.is_some_and(|started| {
            now.saturating_duration_since(started) >= FLY_SPEED_READOUT_VISIBLE_FOR
        }) {
            map.speed_readout = None;
        }
    }

    pub(crate) fn apply_load_stage(
        &mut self,
        request_id: Generation,
        stage: PreviewLoadStage,
    ) -> bool {
        if !self.open || !self.loading() || self.request_id != request_id {
            return false;
        }
        self.load = LoadState::Loading { stage: Some(stage) };
        true
    }

    pub(crate) fn apply_loaded(
        &mut self,
        request_id: Generation,
        result: Result<PreviewData, PreviewLoadError>,
    ) -> bool {
        if !self.open || self.request_id != request_id || !self.loading() {
            return false;
        }

        match result {
            Ok(data) => {
                let mut selection = None;
                if self.workspace_pending {
                    let same_source = self.workspace_retained.as_ref().is_some_and(|retained| {
                        retained.request.as_ref().map(|request| &request.entry_path)
                            == self.request.as_ref().map(|request| &request.entry_path)
                    });
                    if !same_source {
                        self.content_ui = ContentUiState::from_content(&data.content);
                        self.camera_epoch.bump();
                    } else if let ContentUiState::Model(model) = &self.content_ui {
                        selection = Some((model.selected_skin, model.bodygroup_choices.clone()));
                    }
                    self.stage = None;
                }
                self.content_ui.reconcile_with_content(&data.content);
                if let (
                    Some((skin, choices)),
                    ContentUiState::Model(ui),
                    PreviewContent::Model(model),
                ) = (selection, &mut self.content_ui, &data.content)
                {
                    ui.selected_skin = skin.min(model.skin_tables.len().saturating_sub(1));
                    for (index, choice) in ui.bodygroup_choices.iter_mut().enumerate() {
                        *choice = choices
                            .get(index)
                            .copied()
                            .unwrap_or(0)
                            .min(model.bodygroups[index].saturating_sub(1));
                    }
                }
                self.load = LoadState::Ready(Box::new(data));
                self.workspace_retained = None;
                if self.workspace_pending {
                    self.refresh_model_framing();
                    if self.workspace_camera_bounds.is_none() {
                        self.workspace_camera_bounds = self
                            .model_selection()
                            .and_then(|model| model.framing_bounds);
                    }
                }
            }
            Err(error) => {
                if let Some(retained) = self.workspace_retained.take() {
                    self.load = LoadState::Ready(retained.data);
                    self.request = retained.request;
                    self.workspace_error = Some(error);
                } else {
                    self.content_ui = ContentUiState::None;
                    self.load = LoadState::Failed(error);
                }
            }
        }
        self.workspace_pending = false;
        self.stage_request = None;
        true
    }

    pub(super) fn show_fly_speed_readout(&mut self, speed: f32) {
        if !self.open || !speed.is_finite() {
            return;
        }
        let ContentUiState::Map(map) = &mut self.content_ui else {
            return;
        };
        map.speed_readout = Some(FlySpeedReadout {
            speed,
            started_at: None,
            now: None,
        });
    }

    #[cfg(test)]
    pub(super) fn set_fly_pose(&mut self, pose: FlyPose) {
        if !self.open || !pose.is_finite() {
            return;
        }
        if let ContentUiState::Map(map) = &mut self.content_ui {
            map.fly_pose = Some(pose);
            map.movement_mode = None;
            map.requested_movement_mode = None;
        }
    }

    pub(super) fn set_fly_camera(&mut self, pose: FlyPose, mode: MovementMode) {
        if !self.open || !pose.is_finite() {
            return;
        }
        if let ContentUiState::Map(map) = &mut self.content_ui {
            map.fly_pose = Some(pose);
            map.movement_mode = Some(mode);
            map.requested_movement_mode = None;
        }
    }

    pub(super) fn request_movement_mode(&mut self, mode: MovementMode) {
        if !self.open {
            return;
        }
        let ContentUiState::Map(map) = &mut self.content_ui else {
            return;
        };
        if map.movement_mode == Some(mode) {
            map.requested_movement_mode = None;
            return;
        }
        map.requested_movement_mode = Some(mode);
    }

    pub(super) fn set_orbit_pose(&mut self, pose: OrbitPose) {
        if !self.open || !pose.is_finite() {
            return;
        }
        match &mut self.content_ui {
            ContentUiState::Model(model) => model.orbit_pose = Some(pose),
            ContentUiState::Particle(particle) => particle.orbit_pose = Some(pose),
            _ => {}
        }
    }

    pub(crate) const fn selected_skin(&self) -> usize {
        if let Some(stage) = &self.stage {
            return stage.selection.selected_skin;
        }
        match &self.content_ui {
            ContentUiState::Model(model) => model.selected_skin,
            _ => 0,
        }
    }

    pub(crate) fn bodygroup_choices(&self) -> &[usize] {
        if let Some(stage) = &self.stage {
            return &stage.selection.bodygroup_choices;
        }
        match &self.content_ui {
            ContentUiState::Model(model) => &model.bodygroup_choices,
            _ => &[],
        }
    }

    pub(crate) const fn map_fog_enabled(&self) -> bool {
        match &self.content_ui {
            ContentUiState::Map(map) => map.fog_enabled,
            _ => false,
        }
    }

    pub(crate) const fn map_skybox_enabled(&self) -> bool {
        match &self.content_ui {
            ContentUiState::Map(map) => map.skybox_enabled,
            _ => false,
        }
    }

    pub(crate) const fn map_visibility_enabled(&self) -> bool {
        match &self.content_ui {
            ContentUiState::Map(map) => map.visibility_enabled,
            _ => false,
        }
    }

    pub(crate) const fn phy_debug_enabled(&self) -> bool {
        if let Some(stage) = &self.stage {
            return stage.selection.phy_debug_enabled;
        }
        match &self.content_ui {
            ContentUiState::Model(model) => model.phy_debug_enabled,
            ContentUiState::Map(map) => map.phy_debug_enabled,
            _ => false,
        }
    }

    pub(crate) fn map_fog_control_visible(&self) -> bool {
        matches!(
            self.current().map(|data| &data.content),
            Some(PreviewContent::Map { fog: Some(_), .. })
        )
    }

    pub(crate) fn map_skybox_control_visible(&self) -> bool {
        matches!(
            self.current().map(|data| &data.content),
            Some(PreviewContent::Map { stats, .. })
                if stats.skybox_face_count > 0
                    || stats.skybox_prop_count > 0
                    || stats.skybox_detail_sprite_count > 0
                    || stats.skybox_overlay_count > 0
        )
    }

    pub(crate) fn map_visibility_control_visible(&self) -> bool {
        matches!(
            self.current().map(|data| &data.content),
            Some(PreviewContent::Map { scene, .. }) if scene.visibility.is_some()
        )
    }

    pub(crate) fn phy_debug_control_visible(&self) -> bool {
        if let Some(stage) = &self.stage {
            return !stage.model.scene.phy_debug_meshes.is_empty();
        }
        match self.current().map(|data| &data.content) {
            Some(PreviewContent::Model(model)) => !model.scene.phy_debug_meshes.is_empty(),
            Some(PreviewContent::Map { scene, .. }) => !scene.scene.phy_debug_meshes.is_empty(),
            _ => false,
        }
    }

    pub(super) fn set_map_fog_enabled(&mut self, enabled: bool) {
        if let ContentUiState::Map(map) = &mut self.content_ui {
            map.fog_enabled = enabled;
        }
    }

    pub(super) fn set_map_skybox_enabled(&mut self, enabled: bool) {
        if let ContentUiState::Map(map) = &mut self.content_ui {
            map.skybox_enabled = enabled;
        }
    }

    pub(super) fn set_map_visibility_enabled(&mut self, enabled: bool) {
        if let ContentUiState::Map(map) = &mut self.content_ui {
            map.visibility_enabled = enabled;
        }
    }

    pub(super) fn set_phy_debug_enabled(&mut self, enabled: bool) {
        if let Some(stage) = &mut self.stage {
            stage.selection.phy_debug_enabled = enabled;
            return;
        }
        match &mut self.content_ui {
            ContentUiState::Model(model) => model.phy_debug_enabled = enabled,
            ContentUiState::Map(map) => map.phy_debug_enabled = enabled,
            _ => {}
        }
    }

    pub(crate) fn selected_lod(&self) -> usize {
        self.current_model().map_or(0, |model| model.selected_lod)
    }

    pub(super) fn select_lod(&mut self, lod: usize) {
        if let Some(stage) = &mut self.stage {
            stage.model = stage.model.at_lod(lod);
        } else if let Some(data) = self.current_mut()
            && let PreviewContent::Model(model) = &mut data.content
        {
            *model = model.at_lod(lod);
        }
        self.refresh_model_framing();
    }

    pub(super) fn select_skin(&mut self, skin: usize) {
        let skin_count = self
            .current_model()
            .map_or(0, |model| model.skin_tables.len());
        if skin < skin_count {
            if let Some(stage) = &mut self.stage {
                stage.selection.selected_skin = skin;
            } else if let ContentUiState::Model(model) = &mut self.content_ui {
                model.selected_skin = skin;
            }
        }
    }

    pub(super) fn select_bodygroup_choice(&mut self, group: usize, choice: usize) {
        let Some(choices) = self
            .current_model()
            .and_then(|model| model.bodygroups.get(group).copied())
        else {
            return;
        };
        let model = if let Some(stage) = &mut self.stage {
            Some(&mut stage.selection)
        } else if let ContentUiState::Model(model) = &mut self.content_ui {
            Some(model)
        } else {
            None
        };
        if choice < choices
            && let Some(slot) = model.and_then(|model| model.bodygroup_choices.get_mut(group))
        {
            *slot = choice;
            self.refresh_model_framing();
        }
    }

    pub(crate) fn current_model(
        &self,
    ) -> Option<&std::sync::Arc<crate::media::preview_model::ModelPreview>> {
        if let Some(stage) = &self.stage {
            return Some(&stage.model);
        }
        match self.current().map(|data| &data.content) {
            Some(PreviewContent::Model(model)) => Some(model),
            _ => None,
        }
    }

    pub(crate) const fn particle_system(&self) -> usize {
        match &self.content_ui {
            ContentUiState::Particle(particle) => particle.system,
            _ => 0,
        }
    }

    pub(crate) const fn particle_playing(&self) -> bool {
        match &self.content_ui {
            ContentUiState::Particle(particle) => particle.playing,
            _ => false,
        }
    }

    pub(crate) const fn particle_speed(&self) -> f32 {
        match &self.content_ui {
            ContentUiState::Particle(particle) => particle.speed,
            _ => 1.0,
        }
    }

    pub(crate) const fn particle_restart_epoch(&self) -> Generation {
        match &self.content_ui {
            ContentUiState::Particle(particle) => particle.restart_epoch,
            _ => Generation::INITIAL,
        }
    }

    pub(crate) const fn particle_control_points(&self) -> [Vec3; MAX_CONTROL_POINTS] {
        match &self.content_ui {
            ContentUiState::Particle(particle) => particle.control_points,
            _ => default_particle_control_points(),
        }
    }

    pub(super) fn select_particle_system(&mut self, index: usize) {
        let ContentUiState::Particle(particle) = &mut self.content_ui else {
            return;
        };
        if particle.system == index {
            return;
        }
        particle.system = index;
        particle.playing = true;
        particle.restart_epoch.bump();
    }

    pub(super) fn toggle_particle_playing(&mut self) {
        if let ContentUiState::Particle(particle) = &mut self.content_ui {
            particle.playing = !particle.playing;
        }
    }

    pub(super) fn request_particle_restart(&mut self) {
        if let ContentUiState::Particle(particle) = &mut self.content_ui {
            particle.restart_epoch.bump();
            particle.playing = true;
        }
    }

    pub(super) fn set_particle_speed(&mut self, speed: f32) {
        if speed.is_finite()
            && let ContentUiState::Particle(particle) = &mut self.content_ui
        {
            particle.speed = speed.clamp(0.05, 10.0);
        }
    }

    pub(super) fn set_particle_control_point(&mut self, index: ControlPointIndex, position: Vec3) {
        if position.is_finite()
            && let ContentUiState::Particle(particle) = &mut self.content_ui
        {
            particle.control_points[index.get()] = position;
        }
    }

    pub(crate) fn extract_entry_path(&self) -> Option<String> {
        let request = self.request.as_ref()?;
        if !request.archive.supports_entry_extraction() {
            return None;
        }
        self.current().map(|data| data.entry_path.clone())
    }

    pub(super) fn toggle_expanded(&mut self) {
        if self.open {
            self.expanded = !self.expanded;
        }
    }

    pub(super) fn start_audio(&mut self, request_id: Generation) {
        if self.current_audio_request_id() == Some(request_id)
            && let ContentUiState::Audio(audio) = &mut self.content_ui
        {
            audio.playing = true;
        }
    }

    pub(super) fn pause_audio(&mut self, request_id: Generation) {
        if self.current_audio_request_id() == Some(request_id)
            && let ContentUiState::Audio(audio) = &mut self.content_ui
        {
            audio.playing = false;
        }
    }

    pub(super) fn finish_audio(&mut self, request_id: Generation) {
        if self.current_audio_request_id() == Some(request_id)
            && let ContentUiState::Audio(audio) = &mut self.content_ui
        {
            audio.playing = false;
            audio.position_secs = 0.0;
        }
    }

    pub(super) fn update_audio_position(&mut self, request_id: Generation, position_secs: f32) {
        if position_secs.is_finite()
            && self.current_audio_request_id() == Some(request_id)
            && let ContentUiState::Audio(audio) = &mut self.content_ui
        {
            audio.position_secs = position_secs.max(0.0);
        }
    }

    pub(crate) fn close(&mut self) {
        if !self.open && !self.loading() && self.current().is_none() && self.request.is_none() {
            return;
        }
        self.request_id.bump();
        self.open = false;
        self.expanded = false;
        self.request = None;
        self.load = LoadState::Idle;
        self.stage = None;
        self.stage_request = None;
        self.workspace_retained = None;
        self.workspace_pending = false;
        self.workspace_error = None;
        self.workspace_transform = workspace::IDENTITY;
        self.workspace_camera_bounds = None;
        self.workspace_material_tints.clear();
        self.workspace_skin_remap = None;
        self.spinner.stop();
        self.content_ui = ContentUiState::None;
    }
}

#[cfg(test)]
mod tests;
