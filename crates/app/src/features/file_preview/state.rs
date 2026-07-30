use gmpublished_domain::math::Vec3;
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
    loading: bool,
    loading_stage: Option<PreviewLoadStage>,
    error: Option<PreviewLoadError>,
    request_id: Generation,
    request: Option<PreviewRequest>,
    current: Option<PreviewData>,
    spinner: SpinnerClock,
    content_ui: ContentUiState,
    inspector_panes: split_pane::State<Pane>,
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

#[derive(Debug)]
struct ModelUiState {
    selected_skin: usize,
    bodygroup_choices: Vec<usize>,
    orbit_pose: Option<OrbitPose>,
    phy_debug_enabled: bool,
}

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

impl Default for ModelUiState {
    fn default() -> Self {
        Self {
            selected_skin: 0,
            bodygroup_choices: Vec::new(),
            orbit_pose: None,
            phy_debug_enabled: false,
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
        let extension = request
            .entry_path
            .rsplit_once('.')
            .map(|(_, extension)| extension)
            .unwrap_or_default();
        if extension.eq_ignore_ascii_case("bsp") {
            Self::Map(MapUiState::default())
        } else if ["mdl", "vvd", "vtx", "phy"]
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        {
            Self::Model(ModelUiState::default())
        } else if extension.eq_ignore_ascii_case("pcf") {
            Self::Particle(ParticleUiState::default())
        } else if ["wav", "mp3", "ogg", "flac"]
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        {
            Self::Audio(AudioUiState::default())
        } else {
            Self::None
        }
    }

    fn from_content(content: &PreviewContent) -> Self {
        match content {
            PreviewContent::Audio { duration_secs, .. } => Self::Audio(AudioUiState {
                duration_secs: *duration_secs,
                ..AudioUiState::default()
            }),
            PreviewContent::Model(model) => Self::Model(ModelUiState {
                bodygroup_choices: vec![0; model.bodygroups.len()],
                ..ModelUiState::default()
            }),
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
                model_ui.selected_skin = 0;
                model_ui.bodygroup_choices = vec![0; model.bodygroups.len()];
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
            loading: false,
            loading_stage: None,
            error: None,
            request_id: Generation::INITIAL,
            request: None,
            current: None,
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
        self.loading
    }

    pub(crate) const fn loading_stage(&self) -> Option<PreviewLoadStage> {
        self.loading_stage
    }

    pub(crate) fn error(&self) -> Option<&PreviewLoadError> {
        self.error.as_ref()
    }

    pub(crate) const fn current(&self) -> Option<&PreviewData> {
        self.current.as_ref()
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

    pub(crate) fn current_audio_bytes(&self) -> Option<std::sync::Arc<Vec<u8>>> {
        match self.current.as_ref().map(|data| &data.content) {
            Some(PreviewContent::Audio { bytes, .. }) => Some(std::sync::Arc::clone(bytes)),
            _ => None,
        }
    }

    /// Identifies the loaded audio whose playback callbacks may mutate this
    /// state. A request that is merely loading (even if its extension is an
    /// audio type) deliberately has no playback identity yet.
    pub(crate) fn current_audio_request_id(&self) -> Option<Generation> {
        matches!(
            self.current.as_ref().map(|data| &data.content),
            Some(PreviewContent::Audio { .. })
        )
        .then_some(self.request_id)
    }

    pub(crate) const fn request(&self) -> Option<&PreviewRequest> {
        self.request.as_ref()
    }

    pub(crate) fn related_preview(
        &self,
    ) -> Option<&crate::media::preview_model::RelatedPreviewTarget> {
        self.current.as_ref()?.related_preview.as_ref()
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

    /// The current request re-armed to skip size gates, for the
    /// "Load anyway" action on the very-large-file warning.
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
        self.loading = true;
        self.loading_stage = None;
        self.error = None;
        self.current = None;
        self.request = Some(request.clone());
        self.spinner.stop();
        self.content_ui = ContentUiState::from_request(&request);
        request
    }

    pub(crate) const fn spinner_visible(&self) -> bool {
        self.open && self.loading
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

    pub(crate) const fn fly_speed_readout_visible(&self) -> bool {
        self.open
            && matches!(&self.content_ui, ContentUiState::Map(map) if map.speed_readout.is_some())
    }

    pub(super) fn tick_animation(&mut self, now: Instant) {
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
        if !self.open || !self.loading || self.request_id != request_id {
            return false;
        }
        self.loading_stage = Some(stage);
        true
    }

    pub(crate) fn apply_loaded(
        &mut self,
        request_id: Generation,
        result: Result<PreviewData, PreviewLoadError>,
    ) -> bool {
        if !self.open || self.request_id != request_id {
            return false;
        }

        self.loading = false;
        self.loading_stage = None;
        match result {
            Ok(data) => {
                self.content_ui.reconcile_with_content(&data.content);
                self.error = None;
                self.current = Some(data);
            }
            Err(error) => {
                self.content_ui = ContentUiState::None;
                self.error = Some(error);
                self.current = None;
            }
        }
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
        match &self.content_ui {
            ContentUiState::Model(model) => model.selected_skin,
            _ => 0,
        }
    }

    pub(crate) fn bodygroup_choices(&self) -> &[usize] {
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
        match &self.content_ui {
            ContentUiState::Model(model) => model.phy_debug_enabled,
            ContentUiState::Map(map) => map.phy_debug_enabled,
            _ => false,
        }
    }

    pub(crate) fn map_fog_control_visible(&self) -> bool {
        matches!(
            self.current.as_ref().map(|data| &data.content),
            Some(PreviewContent::Map { fog: Some(_), .. })
        )
    }

    pub(crate) fn map_skybox_control_visible(&self) -> bool {
        matches!(
            self.current.as_ref().map(|data| &data.content),
            Some(PreviewContent::Map { stats, .. })
                if stats.skybox_face_count > 0
                    || stats.skybox_prop_count > 0
                    || stats.skybox_detail_sprite_count > 0
                    || stats.skybox_overlay_count > 0
        )
    }

    pub(crate) fn map_visibility_control_visible(&self) -> bool {
        matches!(
            self.current.as_ref().map(|data| &data.content),
            Some(PreviewContent::Map { scene, .. }) if scene.visibility.is_some()
        )
    }

    pub(crate) fn phy_debug_control_visible(&self) -> bool {
        match self.current.as_ref().map(|data| &data.content) {
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
        match &mut self.content_ui {
            ContentUiState::Model(model) => model.phy_debug_enabled = enabled,
            ContentUiState::Map(map) => map.phy_debug_enabled = enabled,
            _ => {}
        }
    }

    pub(super) fn select_skin(&mut self, skin: usize) {
        let skin_count = self
            .current_model()
            .map_or(0, |model| model.skin_tables.len());
        if skin < skin_count
            && let ContentUiState::Model(model) = &mut self.content_ui
        {
            model.selected_skin = skin;
        }
    }

    pub(super) fn select_bodygroup_choice(&mut self, group: usize, choice: usize) {
        let Some(choices) = self
            .current_model()
            .and_then(|model| model.bodygroups.get(group).copied())
        else {
            return;
        };
        if choice < choices
            && let ContentUiState::Model(model) = &mut self.content_ui
            && let Some(slot) = model.bodygroup_choices.get_mut(group)
        {
            *slot = choice;
        }
    }

    pub(crate) fn current_model(
        &self,
    ) -> Option<&std::sync::Arc<crate::media::preview_model::ModelPreview>> {
        match self.current.as_ref().map(|data| &data.content) {
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
        // A different system is a different effect; replay from t=0 with a
        // clean stage.
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
        self.current.as_ref().map(|data| data.entry_path.clone())
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
        if !self.open && !self.loading && self.current.is_none() && self.request.is_none() {
            return;
        }
        self.request_id.bump();
        self.open = false;
        self.expanded = false;
        self.loading = false;
        self.loading_stage = None;
        self.error = None;
        self.request = None;
        self.current = None;
        self.spinner.stop();
        self.content_ui = ContentUiState::None;
    }
}

#[cfg(test)]
mod tests;
