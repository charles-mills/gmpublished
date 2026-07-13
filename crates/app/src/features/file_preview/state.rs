use std::time::{Duration, Instant};

use iced::widget::pane_grid;

use super::message::PreviewLoadError;
use super::model::{PreviewContent, PreviewData, PreviewLoadStage, PreviewRequest};
use crate::widgets::split_pane;

const FLY_SPEED_READOUT_VISIBLE_FOR: Duration = Duration::from_millis(800);
const DEFAULT_VIEWER_RATIO: f32 = (704.0 - 236.0) / 704.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Pane {
    Viewer,
    Inspector,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlyPose {
    pub(crate) position: [f32; 3],
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
        self.position.iter().all(|value| value.is_finite())
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

#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independently toggleable preview UI/setting, not a mode enum"
)]
#[derive(Clone, Debug, PartialEq)]
pub struct State {
    open: bool,
    expanded: bool,
    loading: bool,
    loading_stage: Option<PreviewLoadStage>,
    error: Option<PreviewLoadError>,
    request_id: u64,
    request: Option<PreviewRequest>,
    current: Option<PreviewData>,
    spinner_started_at: Option<Instant>,
    spinner_now: Option<Instant>,
    audio_playing: bool,
    audio_position_secs: f32,
    audio_duration_secs: Option<f32>,
    selected_skin: usize,
    bodygroup_choices: Vec<usize>,
    map_fog_enabled: bool,
    map_skybox_enabled: bool,
    map_visibility_enabled: bool,
    phy_debug_enabled: bool,
    fly_speed_readout: Option<FlySpeedReadout>,
    fly_pose: Option<FlyPose>,
    fly_movement_mode: Option<MovementMode>,
    requested_movement_mode: Option<MovementMode>,
    orbit_pose: Option<OrbitPose>,
    particle_system: usize,
    particle_playing: bool,
    particle_speed: f32,
    particle_restart_epoch: u64,
    particle_control_points: [[f32; 3]; PARTICLE_CONTROL_POINTS],
    inspector_panes: split_pane::State<Pane>,
}

pub(super) const PARTICLE_CONTROL_POINTS: usize = 8;

/// CP0 is the effect origin, pinned at the viewport centre; the rest fan out
/// along +X so two-point effects (beams, tracers) are visible immediately.
const fn default_particle_control_points() -> [[f32; 3]; PARTICLE_CONTROL_POINTS] {
    let mut points = [[0.0; 3]; PARTICLE_CONTROL_POINTS];
    let mut index = 1;
    while index < PARTICLE_CONTROL_POINTS {
        points[index] = [96.0 * index as f32, 0.0, 0.0];
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

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            expanded: false,
            loading: false,
            loading_stage: None,
            error: None,
            request_id: 0,
            request: None,
            current: None,
            spinner_started_at: None,
            spinner_now: None,
            audio_playing: false,
            audio_position_secs: 0.0,
            audio_duration_secs: None,
            selected_skin: 0,
            bodygroup_choices: Vec::new(),
            map_fog_enabled: true,
            map_skybox_enabled: true,
            map_visibility_enabled: true,
            phy_debug_enabled: false,
            fly_speed_readout: None,
            fly_pose: None,
            fly_movement_mode: None,
            requested_movement_mode: None,
            orbit_pose: None,
            particle_system: 0,
            particle_playing: true,
            particle_speed: 1.0,
            particle_restart_epoch: 0,
            particle_control_points: default_particle_control_points(),
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
        self.audio_playing
    }

    pub(crate) const fn audio_position_secs(&self) -> f32 {
        self.audio_position_secs
    }

    pub(crate) const fn audio_duration_secs(&self) -> Option<f32> {
        self.audio_duration_secs
    }

    #[cfg(feature = "asset-studio")]
    pub(crate) fn current_audio_bytes(&self) -> Option<std::sync::Arc<Vec<u8>>> {
        match self.current.as_ref().map(|data| &data.content) {
            Some(PreviewContent::Audio { bytes, .. }) => Some(std::sync::Arc::clone(bytes)),
            _ => None,
        }
    }

    pub(crate) const fn request(&self) -> Option<&PreviewRequest> {
        self.request.as_ref()
    }

    pub(crate) fn related_preview(&self) -> Option<&super::model::RelatedPreviewTarget> {
        self.current.as_ref()?.related_preview.as_ref()
    }

    pub(crate) fn related_preview_request(&self, entry_path: &str) -> Option<PreviewRequest> {
        let archive = &self.request.as_ref()?.archive;
        let entry = archive.entry(entry_path).ok()?;
        Some(PreviewRequest {
            request_id: 0,
            archive: std::sync::Arc::clone(archive),
            entry_path: entry.path.clone(),
            display_name: entry
                .path
                .rsplit_once('/')
                .map_or(entry.path.as_str(), |(_, name)| name)
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
        self.request_id = self.request_id.saturating_add(1);
        request.request_id = self.request_id;
        self.open = true;
        self.expanded = false;
        self.loading = true;
        self.loading_stage = None;
        self.error = None;
        self.current = None;
        self.request = Some(request.clone());
        self.spinner_started_at = None;
        self.spinner_now = None;
        self.clear_audio();
        self.clear_model_selections();
        self.map_fog_enabled = true;
        self.map_skybox_enabled = true;
        self.map_visibility_enabled = true;
        self.phy_debug_enabled = false;
        self.clear_fly_speed_readout();
        self.clear_camera_poses();
        request
    }

    pub(crate) const fn spinner_visible(&self) -> bool {
        self.open && self.loading
    }

    pub(crate) fn spinner_elapsed(&self) -> f32 {
        match (self.spinner_started_at, self.spinner_now) {
            (Some(started), Some(now)) => now.saturating_duration_since(started).as_secs_f32(),
            _ => 0.0,
        }
    }

    pub(crate) fn fly_speed_readout(&self) -> Option<f32> {
        self.fly_speed_readout.as_ref().map(|readout| readout.speed)
    }

    #[cfg(feature = "asset-studio")]
    pub(crate) const fn fly_pose(&self) -> Option<FlyPose> {
        self.fly_pose
    }

    #[cfg(feature = "asset-studio")]
    pub(crate) const fn fly_movement_mode(&self) -> Option<MovementMode> {
        self.fly_movement_mode
    }

    #[cfg(feature = "asset-studio")]
    pub(crate) const fn requested_movement_mode(&self) -> Option<MovementMode> {
        self.requested_movement_mode
    }

    #[cfg(feature = "asset-studio")]
    pub(crate) const fn orbit_pose(&self) -> Option<OrbitPose> {
        self.orbit_pose
    }

    pub(crate) const fn fly_speed_readout_visible(&self) -> bool {
        self.open && self.fly_speed_readout.is_some()
    }

    pub(super) fn tick_animation(&mut self, now: Instant) {
        if self.spinner_visible() {
            if self.spinner_started_at.is_none() {
                self.spinner_started_at = Some(now);
            }
            self.spinner_now = Some(now);
        }

        let Some(readout) = self.fly_speed_readout.as_mut() else {
            return;
        };
        if readout.started_at.is_none() {
            readout.started_at = Some(now);
        }
        readout.now = Some(now);
        if readout.started_at.is_some_and(|started| {
            now.saturating_duration_since(started) >= FLY_SPEED_READOUT_VISIBLE_FOR
        }) {
            self.clear_fly_speed_readout();
        }
    }

    pub(crate) fn apply_load_stage(&mut self, request_id: u64, stage: PreviewLoadStage) -> bool {
        if !self.open || !self.loading || self.request_id != request_id {
            return false;
        }
        self.loading_stage = Some(stage);
        true
    }

    pub(crate) fn apply_loaded(
        &mut self,
        request_id: u64,
        result: Result<PreviewData, PreviewLoadError>,
    ) -> bool {
        if !self.open || self.request_id != request_id {
            return false;
        }

        self.loading = false;
        self.loading_stage = None;
        match result {
            Ok(data) => {
                self.audio_duration_secs = audio_duration_secs(&data.content);
                self.init_model_selections(&data.content);
                self.error = None;
                self.current = Some(data);
            }
            Err(error) => {
                self.clear_audio();
                self.clear_model_selections();
                self.error = Some(error);
                self.current = None;
            }
        }
        true
    }

    #[cfg(feature = "asset-studio")]
    pub(super) fn show_fly_speed_readout(&mut self, speed: f32) {
        if !self.open || !speed.is_finite() {
            return;
        }
        self.fly_speed_readout = Some(FlySpeedReadout {
            speed,
            started_at: None,
            now: None,
        });
    }

    #[cfg(all(feature = "asset-studio", test))]
    pub(super) fn set_fly_pose(&mut self, pose: FlyPose) {
        if !self.open || !pose.is_finite() {
            return;
        }
        self.fly_pose = Some(pose);
        self.fly_movement_mode = None;
        self.requested_movement_mode = None;
    }

    #[cfg(feature = "asset-studio")]
    pub(super) fn set_fly_camera(&mut self, pose: FlyPose, mode: MovementMode) {
        if !self.open || !pose.is_finite() {
            return;
        }
        self.fly_pose = Some(pose);
        self.fly_movement_mode = Some(mode);
        self.requested_movement_mode = None;
    }

    #[cfg(feature = "asset-studio")]
    pub(super) fn request_movement_mode(&mut self, mode: MovementMode) {
        if !self.open {
            return;
        }
        if self.fly_movement_mode == Some(mode) {
            self.requested_movement_mode = None;
            return;
        }
        self.requested_movement_mode = Some(mode);
    }

    #[cfg(feature = "asset-studio")]
    pub(super) fn set_orbit_pose(&mut self, pose: OrbitPose) {
        if self.open && pose.is_finite() {
            self.orbit_pose = Some(pose);
        }
    }

    pub(crate) const fn selected_skin(&self) -> usize {
        self.selected_skin
    }

    pub(crate) fn bodygroup_choices(&self) -> &[usize] {
        &self.bodygroup_choices
    }

    pub(crate) const fn map_fog_enabled(&self) -> bool {
        self.map_fog_enabled
    }

    pub(crate) const fn map_skybox_enabled(&self) -> bool {
        self.map_skybox_enabled
    }

    pub(crate) const fn map_visibility_enabled(&self) -> bool {
        self.map_visibility_enabled
    }

    pub(crate) const fn phy_debug_enabled(&self) -> bool {
        self.phy_debug_enabled
    }

    #[cfg(feature = "asset-studio")]
    pub(crate) fn map_fog_control_visible(&self) -> bool {
        matches!(
            self.current.as_ref().map(|data| &data.content),
            Some(PreviewContent::Map { fog: Some(_), .. })
        )
    }

    #[cfg(not(feature = "asset-studio"))]
    pub(crate) const fn map_fog_control_visible(&self) -> bool {
        false
    }

    #[cfg(feature = "asset-studio")]
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

    #[cfg(not(feature = "asset-studio"))]
    pub(crate) const fn map_skybox_control_visible(&self) -> bool {
        false
    }

    #[cfg(feature = "asset-studio")]
    pub(crate) fn map_visibility_control_visible(&self) -> bool {
        matches!(
            self.current.as_ref().map(|data| &data.content),
            Some(PreviewContent::Map { scene, .. }) if scene.visibility.is_some()
        )
    }

    #[cfg(not(feature = "asset-studio"))]
    pub(crate) const fn map_visibility_control_visible(&self) -> bool {
        false
    }

    #[cfg(feature = "asset-studio")]
    pub(crate) fn phy_debug_control_visible(&self) -> bool {
        match self.current.as_ref().map(|data| &data.content) {
            Some(PreviewContent::Model(model)) => !model.phy_debug_meshes.is_empty(),
            Some(PreviewContent::Map { scene, .. }) => !scene.phy_debug_meshes.is_empty(),
            _ => false,
        }
    }

    #[cfg(not(feature = "asset-studio"))]
    pub(crate) const fn phy_debug_control_visible(&self) -> bool {
        false
    }

    #[cfg(feature = "asset-studio")]
    pub(super) const fn set_map_fog_enabled(&mut self, enabled: bool) {
        self.map_fog_enabled = enabled;
    }

    #[cfg(feature = "asset-studio")]
    pub(super) const fn set_map_skybox_enabled(&mut self, enabled: bool) {
        self.map_skybox_enabled = enabled;
    }

    #[cfg(feature = "asset-studio")]
    pub(super) const fn set_map_visibility_enabled(&mut self, enabled: bool) {
        self.map_visibility_enabled = enabled;
    }

    #[cfg(feature = "asset-studio")]
    pub(super) const fn set_phy_debug_enabled(&mut self, enabled: bool) {
        self.phy_debug_enabled = enabled;
    }

    #[cfg(feature = "asset-studio")]
    pub(super) fn select_skin(&mut self, skin: usize) {
        let skin_count = self
            .current_model()
            .map_or(0, |model| model.skin_tables.len());
        if skin < skin_count {
            self.selected_skin = skin;
        }
    }

    #[cfg(feature = "asset-studio")]
    pub(super) fn select_bodygroup_choice(&mut self, group: usize, choice: usize) {
        let Some(choices) = self
            .current_model()
            .and_then(|model| model.bodygroups.get(group).copied())
        else {
            return;
        };
        if choice < choices
            && let Some(slot) = self.bodygroup_choices.get_mut(group)
        {
            *slot = choice;
        }
    }

    #[cfg(feature = "asset-studio")]
    pub(crate) fn current_model(&self) -> Option<&std::sync::Arc<super::model::ModelPreview>> {
        match self.current.as_ref().map(|data| &data.content) {
            Some(PreviewContent::Model(model)) => Some(model),
            _ => None,
        }
    }

    fn init_model_selections(&mut self, content: &PreviewContent) {
        self.clear_model_selections();
        #[cfg(feature = "asset-studio")]
        if let PreviewContent::Model(model) = content {
            self.bodygroup_choices = vec![0; model.bodygroups.len()];
        }
        #[cfg(not(feature = "asset-studio"))]
        let _ = content;
    }

    fn clear_model_selections(&mut self) {
        self.selected_skin = 0;
        self.bodygroup_choices.clear();
        self.particle_system = 0;
        self.particle_playing = true;
        self.particle_speed = 1.0;
        self.particle_control_points = default_particle_control_points();
    }

    pub(crate) const fn particle_system(&self) -> usize {
        self.particle_system
    }

    pub(crate) const fn particle_playing(&self) -> bool {
        self.particle_playing
    }

    pub(crate) const fn particle_speed(&self) -> f32 {
        self.particle_speed
    }

    pub(crate) const fn particle_restart_epoch(&self) -> u64 {
        self.particle_restart_epoch
    }

    pub(crate) const fn particle_control_points(&self) -> [[f32; 3]; PARTICLE_CONTROL_POINTS] {
        self.particle_control_points
    }

    pub(super) fn select_particle_system(&mut self, index: usize) {
        if self.particle_system == index {
            return;
        }
        self.particle_system = index;
        // A different system is a different effect; replay from t=0 with a
        // clean stage.
        self.particle_playing = true;
        self.particle_restart_epoch = self.particle_restart_epoch.wrapping_add(1);
    }

    pub(super) fn toggle_particle_playing(&mut self) {
        self.particle_playing = !self.particle_playing;
    }

    pub(super) fn request_particle_restart(&mut self) {
        self.particle_restart_epoch = self.particle_restart_epoch.wrapping_add(1);
        self.particle_playing = true;
    }

    pub(super) fn set_particle_speed(&mut self, speed: f32) {
        if speed.is_finite() {
            self.particle_speed = speed.clamp(0.05, 10.0);
        }
    }

    pub(super) fn set_particle_control_point(&mut self, index: usize, position: [f32; 3]) {
        if index < PARTICLE_CONTROL_POINTS && position.iter().all(|component| component.is_finite())
        {
            self.particle_control_points[index] = position;
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

    pub(super) fn start_audio(&mut self) {
        if self.current_audio_available() {
            self.audio_playing = true;
        }
    }

    pub(super) const fn pause_audio(&mut self) {
        self.audio_playing = false;
    }

    pub(super) fn finish_audio(&mut self) {
        self.audio_playing = false;
        self.audio_position_secs = 0.0;
    }

    pub(super) fn update_audio_position(&mut self, position_secs: f32) {
        if position_secs.is_finite() {
            self.audio_position_secs = position_secs.max(0.0);
        }
    }

    pub(crate) fn close(&mut self) {
        if !self.open && !self.loading && self.current.is_none() && self.request.is_none() {
            return;
        }
        self.request_id = self.request_id.saturating_add(1);
        self.open = false;
        self.expanded = false;
        self.loading = false;
        self.loading_stage = None;
        self.error = None;
        self.request = None;
        self.current = None;
        self.spinner_started_at = None;
        self.spinner_now = None;
        self.clear_audio();
        self.clear_model_selections();
        self.map_fog_enabled = true;
        self.map_skybox_enabled = true;
        self.map_visibility_enabled = true;
        self.phy_debug_enabled = false;
        self.clear_fly_speed_readout();
        self.clear_camera_poses();
    }

    fn clear_audio(&mut self) {
        self.audio_playing = false;
        self.audio_position_secs = 0.0;
        self.audio_duration_secs = None;
    }

    fn current_audio_available(&self) -> bool {
        #[cfg(feature = "asset-studio")]
        {
            matches!(
                self.current.as_ref().map(|data| &data.content),
                Some(PreviewContent::Audio { .. })
            )
        }
        #[cfg(not(feature = "asset-studio"))]
        {
            false
        }
    }

    fn clear_fly_speed_readout(&mut self) {
        self.fly_speed_readout = None;
    }

    fn clear_camera_poses(&mut self) {
        self.fly_pose = None;
        self.fly_movement_mode = None;
        self.requested_movement_mode = None;
        self.orbit_pose = None;
    }
}

fn audio_duration_secs(content: &PreviewContent) -> Option<f32> {
    #[cfg(feature = "asset-studio")]
    {
        if let PreviewContent::Audio { duration_secs, .. } = content {
            return *duration_secs;
        }
    }
    let _ = content;
    None
}

#[cfg(test)]
mod tests;
