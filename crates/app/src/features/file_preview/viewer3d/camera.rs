use super::super::orbit::{MAX_PITCH, MIN_PITCH, Orbit, ZoomFloor};
use super::pipeline::PreviewScene;
use super::{
    Action, Arc, DOOR_PROGRESS_EPSILON, DoorAudioEvent, DoorMotion, DoorRenderPose, DoorRuntime,
    DoorTarget, Event, FlyPose, MapFog, MapPreview, MapSkyCamera, MapSpawn, Message, ModelPreview,
    ModelPrimitive, MovementMode, OrbitPose, Point, Rectangle, SOURCE_UP, Uniforms,
    door_world_bounds, half_extent, initial_door_swing, mid, mouse, shader,
};
use gmpublished_domain::math::QAngle;
use gmpublished_domain::math::Vec3;

/// Shader-widget program: owns nothing but a handle to the loaded model;
/// camera state lives in the widget tree so it survives redraws.
pub struct Viewer3d {
    pub model: Arc<ModelPreview>,
    /// Identifies the upload in the shared pipeline cache; bump per load.
    pub content_id: u64,
    /// Material remap for the selected skin family; empty = identity.
    pub skin_remap: Vec<u16>,
    /// Selected choice per bodygroup; meshes of other choices are skipped.
    pub bodygroup_choices: Vec<usize>,
    pub phy_debug_visible: bool,
    pub pose: Option<OrbitPose>,
}

#[derive(Debug)]
pub struct Camera {
    pub(super) content_id: Option<u64>,
    /// `distance` is a multiplier over the model's auto-framed distance.
    pub(super) orbit: Orbit,
    pub(super) drag_from: Option<Point>,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            content_id: None,
            orbit: Orbit::new(ZoomFloor::SolidMesh),
            drag_from: None,
        }
    }
}

impl Camera {
    pub(super) fn ensure_spawn(&mut self, content_id: u64, pose: Option<OrbitPose>) {
        if self.content_id == Some(content_id) {
            return;
        }
        self.content_id = Some(content_id);
        self.orbit = Orbit::from_pose(pose.unwrap_or_default(), ZoomFloor::SolidMesh);
        self.drag_from = None;
    }

    pub(super) fn pose(&self) -> OrbitPose {
        self.orbit.pose()
    }
}

impl shader::Program<Message> for Viewer3d {
    type State = Camera;
    type Primitive = ModelPrimitive;

    fn update(
        &self,
        camera: &mut Camera,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        camera.ensure_spawn(self.content_id, self.pose);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let position = cursor.position_over(bounds)?;
                camera.drag_from = Some(position);
                Some(Action::capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let from = camera.drag_from?;
                let to = cursor.position()?;
                camera.orbit.drag(to.x - from.x, to.y - from.y);
                camera.drag_from = Some(to);
                Some(Action::publish(Message::OrbitPoseChanged(camera.pose())).and_capture())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                camera.drag_from.take().map(|_| Action::capture())
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                cursor.position_over(bounds)?;
                let steps = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 40.0,
                };
                camera.orbit.zoom(steps);
                Some(Action::publish(Message::OrbitPoseChanged(camera.pose())).and_capture())
            }
            _ => None,
        }
    }

    fn draw(&self, camera: &Camera, _cursor: mouse::Cursor, bounds: Rectangle) -> ModelPrimitive {
        ModelPrimitive {
            preview: PreviewScene::Model(Arc::clone(&self.model)),
            content_id: self.content_id,
            skin_remap: self.skin_remap.clone(),
            bodygroup_choices: self.bodygroup_choices.clone(),
            map_skybox_visible: true,
            visibility_culling: false,
            phy_debug_visible: self.phy_debug_visible,
            uniforms: Uniforms::for_model(&self.model, camera, bounds),
            submerged: false,
            map_skybox_uniforms: None,
            sky_uniforms: None,
            door_poses: Vec::new(),
        }
    }

    fn mouse_interaction(
        &self,
        camera: &Camera,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if camera.drag_from.is_some() {
            mouse::Interaction::Grabbing
        } else if cursor.position_over(bounds).is_some() {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::default()
        }
    }
}

/// Fly-through program for map scenes: WASD + drag-to-look. Movement rides
/// the redraw chain — each held-key frame requests the next — so the loop
/// stops dead (0% idle) the moment all keys are released.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag toggles an independent map render layer, not a mode enum"
)]
pub struct FlyViewer {
    pub scene: Arc<MapPreview>,
    pub content_id: u64,
    pub fog: Option<MapFog>,
    pub fog_enabled: bool,
    pub sky_camera: Option<MapSkyCamera>,
    pub map_skybox_visible: bool,
    pub visibility_culling: bool,
    pub phy_debug_visible: bool,
    pub spawn: Option<MapSpawn>,
    pub pose: Option<FlyPose>,
    pub movement_mode: Option<MovementMode>,
    pub requested_movement_mode: Option<MovementMode>,
}

pub(super) const FLY_LOOK_SENSITIVITY: f32 = 0.006;
pub(super) const FLY_SPEED_WHEEL_STEP: f32 = 1.25;
pub(super) const FLY_ACCEL_SECONDS: f32 = 0.2;
pub(super) const PLAYER_START_EYE_NUDGE: f32 = 64.0;
pub(super) const WALK_HULL_HALF_EXTENTS: Vec3 = Vec3::new(16.0, 16.0, 36.0);
pub(super) const WALK_EYE_TO_HULL_CENTER: Vec3 = Vec3::new(0.0, 0.0, -28.0);
pub(super) const WALK_HULL_CENTER_TO_EYE: Vec3 = Vec3::new(0.0, 0.0, 28.0);
pub(super) const WALK_DUCK_HULL_HALF_EXTENTS: Vec3 = Vec3::new(16.0, 16.0, 18.0);
pub(super) const WALK_DUCK_EYE_HEIGHT: f32 = 28.0;
pub(super) const WALK_DUCK_EYE_TO_HULL_CENTER: Vec3 = Vec3::new(0.0, 0.0, -10.0);
pub(super) const WALK_DUCK_HULL_CENTER_TO_EYE: Vec3 = Vec3::new(0.0, 0.0, 10.0);
pub(super) const WALK_SPEED: f32 = 190.0;
// HL2 sprint speed; keeps the Source-defaults convention of the rest.
pub(super) const WALK_SWIM_STOP_SPEED: f32 = 0.1;
pub(super) const WALK_STEP_HEIGHT: f32 = 18.0;
pub(super) const WALK_GROUND_SNAP: f32 = 4.0;
pub(super) const LAND_BOB_DURATION: f32 = 0.22;
pub(super) const LAND_BOB_AMPLITUDE: f32 = 3.0;

/// How deep the walk hull is in water, sampled at feet, waist and eye.
///
/// Ordered: swimming starts at [`Self::Waist`], and [`Self::Eyes`] is also
/// submerged.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum WaterLevel {
    #[default]
    Dry,
    Feet,
    Waist,
    Eyes,
}

impl WaterLevel {
    pub(super) fn is_swimming(self) -> bool {
        self >= Self::Waist
    }

    pub(super) fn is_submerged(self) -> bool {
        self == Self::Eyes
    }
}

#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "ground, jump, duck-reconcile, and exit-assist states change independently"
)]
pub struct FlyCamera {
    pub(super) content_id: Option<u64>,
    pub(super) position: Option<Vec3>,
    pub(super) yaw: f32,
    pub(super) pitch: f32,
    /// Multiplier over the map-scaled base speed, adjusted by wheel.
    pub(super) speed: f32,
    pub(super) move_factor: f32,
    pub(super) held: HeldKeys,
    pub(super) look_from: Option<Point>,
    pub(super) last_frame: Option<std::time::Instant>,
    pub(super) water_time: f32,
    pub(super) mode: MovementMode,
    pub(super) walk_velocity: Vec3,
    pub(super) grounded: bool,
    pub(super) jump_requested: bool,
    pub(super) walk_bob_phase: f32,
    pub(super) walk_bob_offset: f32,
    pub(super) land_bob_elapsed: f32,
    pub(super) land_bob_amplitude: f32,
    pub(super) water: WaterLevel,
    pub(super) water_exit_assist: bool,
    pub(super) walk_hull: WalkHull,
    pub(super) duck_view_animation: Option<DuckViewAnimation>,
    pub(super) duck_reconcile_requested: bool,
    pub(super) doors: Vec<DoorRuntime>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum WalkHull {
    #[default]
    Standing,
    Ducked,
}

impl WalkHull {
    pub(super) const fn half_extents(self) -> Vec3 {
        match self {
            Self::Standing => WALK_HULL_HALF_EXTENTS,
            Self::Ducked => WALK_DUCK_HULL_HALF_EXTENTS,
        }
    }

    pub(super) const fn eye_height(self) -> f32 {
        match self {
            Self::Standing => PLAYER_START_EYE_NUDGE,
            Self::Ducked => WALK_DUCK_EYE_HEIGHT,
        }
    }

    pub(super) const fn eye_to_hull_center(self) -> Vec3 {
        match self {
            Self::Standing => WALK_EYE_TO_HULL_CENTER,
            Self::Ducked => WALK_DUCK_EYE_TO_HULL_CENTER,
        }
    }

    pub(super) const fn hull_center_to_eye(self) -> Vec3 {
        match self {
            Self::Standing => WALK_HULL_CENTER_TO_EYE,
            Self::Ducked => WALK_DUCK_HULL_CENTER_TO_EYE,
        }
    }

    pub(super) const fn is_ducked(self) -> bool {
        matches!(self, Self::Ducked)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DuckViewAnimation {
    pub(super) from_height: f32,
    pub(super) elapsed: f32,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag tracks one physical key's held state, independent of the others"
)]
#[derive(Debug, Default)]
pub(super) struct HeldKeys {
    pub(super) forward: bool,
    pub(super) back: bool,
    pub(super) left: bool,
    pub(super) right: bool,
    pub(super) up: bool,
    pub(super) down: bool,
    pub(super) fast: bool,
    pub(super) duck: bool,
    pub(super) walk_toggle: bool,
}

impl HeldKeys {
    pub(super) const fn any_movement(&self) -> bool {
        self.forward || self.back || self.left || self.right || self.up || self.down
    }

    pub(super) const fn any_horizontal(&self) -> bool {
        self.forward || self.back || self.left || self.right
    }

    pub(super) const fn is_duck_code(code: iced::keyboard::key::Code) -> bool {
        use iced::keyboard::key::Code;
        matches!(code, Code::ControlLeft | Code::ControlRight | Code::KeyC)
    }

    pub(super) fn set(&mut self, code: iced::keyboard::key::Code, pressed: bool) -> bool {
        use iced::keyboard::key::Code;
        if matches!(code, Code::ControlLeft | Code::ControlRight) {
            self.down = pressed;
            self.duck = pressed;
            return true;
        }
        let slot = match code {
            Code::KeyW | Code::ArrowUp => &mut self.forward,
            Code::KeyS | Code::ArrowDown => &mut self.back,
            Code::KeyA | Code::ArrowLeft => &mut self.left,
            Code::KeyD | Code::ArrowRight => &mut self.right,
            Code::Space => &mut self.up,
            Code::KeyE => &mut self.up,
            Code::KeyQ => &mut self.down,
            Code::ShiftLeft | Code::ShiftRight => &mut self.fast,
            Code::KeyC => &mut self.duck,
            _ => return false,
        };
        *slot = pressed;
        true
    }
}

mod bob;
mod duck;
mod trace;
mod walk;
mod water;

impl FlyCamera {
    pub(super) fn ensure_spawn(
        &mut self,
        scene: &MapPreview,
        spawn: Option<MapSpawn>,
        content_id: u64,
        pose: Option<FlyPose>,
        movement_mode: Option<MovementMode>,
    ) -> bool {
        if self.content_id == Some(content_id) && self.position.is_some() {
            return false;
        }
        self.content_id = Some(content_id);
        self.look_from = None;
        self.held = HeldKeys::default();
        self.last_frame = None;
        self.mode = MovementMode::Fly;
        self.reset_walk_state();
        self.doors = scene
            .doors
            .iter()
            .map(|door| {
                let progress = door.initial_progress.clamp(0.0, 1.0);
                let swing = initial_door_swing(door.motion);
                let (bounds_min, bounds_max) = door_world_bounds(door, progress, swing);
                DoorRuntime {
                    progress,
                    target: if progress >= 1.0 - DOOR_PROGRESS_EPSILON {
                        DoorTarget::Open
                    } else {
                        DoorTarget::Closed
                    },
                    motion: DoorMotion::Idle,
                    swing,
                    bounds_min,
                    bounds_max,
                }
            })
            .collect();

        let default_walk_from_spawn = movement_mode.is_none() && spawn.is_some();
        if let (true, Some(spawn)) = (default_walk_from_spawn, spawn) {
            self.seed_from_spawn(spawn);
        } else if let Some(pose) = pose {
            self.position = Some(pose.position);
            self.yaw = pose.yaw;
            self.pitch = pose.pitch;
            self.speed = pose.speed;
        } else if let Some(spawn) = spawn {
            self.seed_from_spawn(spawn);
        } else {
            self.seed_from_bounds(scene);
        }
        if self.speed == 0.0 {
            self.speed = 1.0;
        }

        match movement_mode {
            Some(MovementMode::Fly) => {}
            Some(MovementMode::Walk) => {
                self.enter_walk(scene);
            }
            None if default_walk_from_spawn => {
                self.enter_walk(scene);
            }
            None => {}
        }
        true
    }

    pub(super) fn seed_from_spawn(&mut self, spawn: MapSpawn) {
        self.position = Some(Vec3::new(
            spawn.origin[0],
            spawn.origin[1],
            spawn.origin[2] + PLAYER_START_EYE_NUDGE,
        ));
        let angles = QAngle::from_source_degrees(spawn.angles);
        self.yaw = angles.yaw;
        self.pitch = (-angles.pitch).clamp(MIN_PITCH, MAX_PITCH);
    }

    pub(super) fn seed_from_bounds(&mut self, scene: &MapPreview) {
        let center = mid(scene.scene.bounds_min, scene.scene.bounds_max);
        let radius = half_extent(scene.scene.bounds_min, scene.scene.bounds_max).max(1.0);
        self.position = Some(Vec3::new(
            center[0] - radius * 0.6,
            center[1] - radius * 0.6,
            center[2] + radius * 0.35,
        ));
        // Face the map center from the spawn corner.
        self.yaw = std::f32::consts::FRAC_PI_4;
        self.pitch = -0.25;
    }

    pub(super) fn pose(&self) -> Option<FlyPose> {
        Some(FlyPose {
            position: self.position?,
            yaw: self.yaw,
            pitch: self.pitch,
            speed: self.speed,
        })
    }

    pub(super) const fn mode(&self) -> MovementMode {
        self.mode
    }

    pub(super) fn camera_update_message(&self) -> Option<Message> {
        self.pose().map(|pose| Message::FlyCameraChanged {
            pose,
            mode: self.mode(),
        })
    }

    pub(super) fn speed_update_message(&self) -> Option<Message> {
        self.pose().map(|pose| Message::FlySpeedChanged {
            pose,
            mode: self.mode(),
        })
    }

    pub(super) fn forward(&self) -> Vec3 {
        Vec3::new(
            self.pitch.cos() * self.yaw.cos(),
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
        )
    }

    pub(super) fn integrate(
        &mut self,
        scene: &MapPreview,
        content_id: u64,
        dt: f32,
    ) -> Vec<DoorAudioEvent> {
        self.water_time += dt;
        let mut audio_events = self.integrate_doors(scene, content_id, dt);
        match self.mode {
            MovementMode::Fly => self.integrate_fly(scene, dt),
            MovementMode::Walk => self.integrate_walk(scene, dt),
        }
        if self.mode == MovementMode::Walk && self.held.any_horizontal() {
            audio_events.extend(self.resume_blocked_doors_if_clear(scene, content_id));
        }
        audio_events
    }

    pub(super) fn integrate_fly(&mut self, scene: &MapPreview, dt: f32) {
        if self.held.any_movement() {
            self.move_factor = (self.move_factor + dt / FLY_ACCEL_SECONDS).clamp(0.0, 1.0);
        } else {
            self.move_factor = 0.0;
            return;
        }
        let Some(position) = self.position.as_mut() else {
            return;
        };
        let radius = half_extent(scene.scene.bounds_min, scene.scene.bounds_max).max(1.0);
        let mut speed = radius * 0.4 * self.speed * self.move_factor;
        if self.held.fast {
            speed *= 3.0;
        }

        let forward = Vec3::new(
            self.pitch.cos() * self.yaw.cos(),
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
        );
        let right = forward.cross(SOURCE_UP).normalize_or_zero();

        let mut delta = Vec3::ZERO;
        if self.held.forward {
            delta += forward;
        }
        if self.held.back {
            delta -= forward;
        }
        if self.held.right {
            delta += right;
        }
        if self.held.left {
            delta -= right;
        }
        if self.held.up {
            delta += SOURCE_UP;
        }
        if self.held.down {
            delta -= SOURCE_UP;
        }

        if let Some(direction) = delta.normalize() {
            *position += direction * (speed * dt);
        }
    }

    pub(super) fn select_mode(&mut self, scene: &MapPreview, target: MovementMode) -> bool {
        if self.mode == target {
            return false;
        }
        match target {
            MovementMode::Fly => {
                self.exit_walk();
                true
            }
            MovementMode::Walk => self.enter_walk(scene),
        }
    }

    pub(super) fn needs_movement_tick(&self) -> bool {
        let door_moving = self.doors.iter().any(|door| door.motion.needs_tick());
        match self.mode {
            MovementMode::Fly => self.held.any_movement() || door_moving,
            MovementMode::Walk => {
                // `duck_reconcile_requested` is one-shot and is cleared by
                // the next walk step after a single hull-fit attempt.
                // `duck_view_transition_active` is clamped to Source's
                // 0.2s duck-view spline, so it terminates by construction.
                // Door transitions clamp to a finite 0..1 progress range;
                // a blocked close parks without ticking and only resumes on
                // later input/movement checks, preserving idle-0%.
                self.held.any_horizontal()
                    || self.jump_requested
                    || (self.water.is_swimming()
                        && (self.held.any_movement()
                            || self.held.duck
                            || self.walk_velocity.length_squared()
                                > WALK_SWIM_STOP_SPEED * WALK_SWIM_STOP_SPEED))
                    || (!self.water.is_swimming() && !self.grounded)
                    || self.land_bob_active()
                    || self.walk_bob_offset.abs() > 0.01
                    || self.duck_reconcile_requested
                    || self.duck_view_transition_active()
                    || door_moving
            }
        }
    }
}

pub(super) fn clip_along_plane(vector: Vec3, normal: Vec3) -> Vec3 {
    let into_plane = vector.dot(normal);
    if into_plane < 0.0 {
        vector - (normal * into_plane)
    } else {
        vector
    }
}

pub(super) fn horizontal_length_squared(vector: Vec3) -> f32 {
    vector[0] * vector[0] + vector[1] * vector[1]
}

impl shader::Program<Message> for FlyViewer {
    type State = FlyCamera;
    type Primitive = ModelPrimitive;

    fn update(
        &self,
        camera: &mut FlyCamera,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let initialized = camera.ensure_spawn(
            &self.scene,
            self.spawn,
            self.content_id,
            self.pose,
            self.movement_mode,
        );
        let mode_request_seen = self.requested_movement_mode.is_some();
        let mode_request_changed = self.requested_movement_mode.is_some_and(|mode| {
            let changed = camera.select_mode(&self.scene, mode);
            if changed {
                camera.last_frame = None;
            }
            changed
        });
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let position = cursor.position_over(bounds)?;
                camera.look_from = Some(position);
                Some(Action::capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let from = camera.look_from?;
                let to = cursor.position()?;
                camera.yaw += (to.x - from.x) * FLY_LOOK_SENSITIVITY;
                camera.pitch = (camera.pitch - (to.y - from.y) * FLY_LOOK_SENSITIVITY)
                    .clamp(MIN_PITCH, MAX_PITCH);
                camera.look_from = Some(to);
                camera
                    .camera_update_message()
                    .map(|message| Action::publish(message).and_capture())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                camera.look_from.take().map(|_| Action::capture())
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                cursor.position_over(bounds)?;
                let steps = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 40.0,
                };
                camera.speed = (camera.speed * FLY_SPEED_WHEEL_STEP.powf(steps)).clamp(0.05, 20.0);
                camera
                    .speed_update_message()
                    .map(|message| Action::publish(message).and_capture())
            }
            Event::Keyboard(iced::keyboard::Event::KeyPressed { physical_key, .. }) => {
                let iced::keyboard::key::Physical::Code(code) = physical_key else {
                    return None;
                };
                if *code == iced::keyboard::key::Code::KeyV {
                    if !camera.held.walk_toggle {
                        camera.held.walk_toggle = true;
                        camera.toggle_walk(&self.scene);
                        camera.last_frame = None;
                        return camera
                            .camera_update_message()
                            .map(|message| Action::publish(message).and_capture());
                    }
                    return Some(Action::capture());
                }
                if *code == iced::keyboard::key::Code::KeyE && camera.mode == MovementMode::Walk {
                    if let Some(event) = camera.toggle_nearest_door(&self.scene, self.content_id) {
                        camera.last_frame = None;
                        let action = Action::publish(Message::DoorAudioEvents(vec![event]));
                        return Some(action.and_capture());
                    }
                    return Some(Action::capture());
                }

                if *code == iced::keyboard::key::Code::Space {
                    camera.request_jump();
                }
                let was_moving = camera.needs_movement_tick();
                if !camera.held.set(*code, true) {
                    return None;
                }
                if camera.mode == MovementMode::Walk && HeldKeys::is_duck_code(*code) {
                    camera.duck_reconcile_requested = true;
                }
                if !was_moving {
                    camera.last_frame = None;
                }
                Some(Action::request_redraw().and_capture())
            }
            Event::Keyboard(iced::keyboard::Event::KeyReleased { physical_key, .. }) => {
                let iced::keyboard::key::Physical::Code(code) = physical_key else {
                    return None;
                };
                if *code == iced::keyboard::key::Code::KeyV {
                    camera.held.walk_toggle = false;
                    return Some(Action::capture());
                }
                if *code == iced::keyboard::key::Code::KeyE && camera.mode == MovementMode::Walk {
                    return Some(Action::capture());
                }
                let was_moving = camera.needs_movement_tick();
                let duck_key = HeldKeys::is_duck_code(*code);
                camera.held.set(*code, false).then(|| {
                    if camera.mode == MovementMode::Walk && duck_key {
                        camera.duck_reconcile_requested = true;
                    }
                    if !was_moving && camera.needs_movement_tick() {
                        camera.last_frame = None;
                        return Action::request_redraw().and_capture();
                    }
                    camera.camera_update_message().map_or_else(
                        || Action::request_redraw().and_capture(),
                        |message| Action::publish(message).and_capture(),
                    )
                })
            }
            Event::Window(iced::window::Event::RedrawRequested(now)) => {
                if !camera.needs_movement_tick() {
                    camera.last_frame = None;
                    camera.move_factor = 0.0;
                    return (initialized || mode_request_seen || mode_request_changed)
                        .then(|| camera.camera_update_message().map(Action::publish))
                        .flatten();
                }
                if let Some(last) = camera.last_frame {
                    let dt = now.saturating_duration_since(last).as_secs_f32().min(0.1);
                    let audio_events = camera.integrate(&self.scene, self.content_id, dt);
                    if !audio_events.is_empty()
                        && let Some(pose) = camera.pose()
                    {
                        camera.last_frame = Some(*now);
                        return Some(Action::publish(Message::FlyCameraAndDoorAudioChanged {
                            pose,
                            mode: camera.mode(),
                            door_audio_events: audio_events,
                        }));
                    }
                }
                camera.last_frame = Some(*now);
                camera.camera_update_message().map_or_else(
                    || Some(Action::request_redraw()),
                    |message| Some(Action::publish(message)),
                )
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        camera: &FlyCamera,
        _cursor: mouse::Cursor,
        bounds: Rectangle,
    ) -> ModelPrimitive {
        ModelPrimitive {
            preview: PreviewScene::Map(Arc::clone(&self.scene)),
            content_id: self.content_id,
            skin_remap: Vec::new(),
            bodygroup_choices: Vec::new(),
            map_skybox_visible: self.map_skybox_visible,
            visibility_culling: self.visibility_culling,
            phy_debug_visible: self.phy_debug_visible,
            uniforms: Uniforms::for_fly(
                &self.scene,
                camera,
                bounds,
                self.fog.filter(|_| self.fog_enabled),
                camera.water_time,
                camera.submerged(),
            ),
            submerged: camera.submerged(),
            map_skybox_uniforms: self.sky_camera.map(|sky_camera| {
                Uniforms::for_fly_skybox_composite(
                    &self.scene,
                    camera,
                    bounds,
                    sky_camera,
                    sky_camera.fog.filter(|_| self.fog_enabled),
                )
            }),
            sky_uniforms: self
                .scene
                .skybox
                .as_ref()
                .map(|_| Uniforms::for_fly_sky(&self.scene, camera, bounds)),
            door_poses: camera
                .doors
                .iter()
                .map(|door| DoorRenderPose {
                    progress: door.progress,
                    swing: door.swing,
                })
                .collect(),
        }
    }

    fn mouse_interaction(
        &self,
        camera: &FlyCamera,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if camera.look_from.is_some() {
            mouse::Interaction::Grabbing
        } else if cursor.position_over(bounds).is_some() {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{empty_preview, floor_scene};
    use super::*;

    #[test]
    fn restored_fly_mode_keeps_fly_pose() {
        let scene = floor_scene();
        let pose = FlyPose {
            position: Vec3::new(512.0, 512.0, 128.0),
            yaw: 0.5,
            pitch: -0.25,
            speed: 1.75,
        };
        let mut camera = FlyCamera::default();

        camera.ensure_spawn(&scene, None, 7, Some(pose), Some(MovementMode::Fly));

        assert_eq!(camera.pose(), Some(pose));
        assert_eq!(camera.mode, MovementMode::Fly);
    }

    #[test]
    fn absent_mode_with_legacy_pose_defaults_to_walk_at_spawn() {
        let scene = floor_scene();
        let legacy_pose = FlyPose {
            position: Vec3::new(128.0, 256.0, 384.0),
            yaw: 0.5,
            pitch: -0.25,
            speed: 2.0,
        };
        let spawn = MapSpawn {
            origin: Vec3::new(512.0, 512.0, 0.0),
            angles: Vec3::new(0.0, 90.0, 0.0),
        };
        let mut camera = FlyCamera::default();

        camera.ensure_spawn(&scene, Some(spawn), 7, Some(legacy_pose), None);

        let pose = camera.pose().expect("spawn should initialize camera");
        assert_eq!(camera.mode, MovementMode::Walk);
        assert_eq!(
            [pose.position[0], pose.position[1]],
            [512.0, 512.0],
            "legacy pose-only state must not suppress spawn walk default"
        );
        assert_ne!(pose.position, legacy_pose.position);
        assert!((pose.yaw - 90.0_f32.to_radians()).abs() < 1.0e-6);
    }

    #[test]
    fn direct_mode_selection_matches_v_toggle_selector() {
        let scene = floor_scene();
        let pose = FlyPose {
            position: Vec3::new(512.0, 512.0, 128.0),
            yaw: 0.5,
            pitch: -0.25,
            speed: 1.75,
        };

        let mut via_toggle = FlyCamera::default();
        via_toggle.ensure_spawn(&scene, None, 7, Some(pose), Some(MovementMode::Fly));
        assert!(via_toggle.toggle_walk(&scene));

        let mut via_select = FlyCamera::default();
        via_select.ensure_spawn(&scene, None, 7, Some(pose), Some(MovementMode::Fly));
        assert!(via_select.select_mode(&scene, MovementMode::Walk));

        assert_eq!(via_toggle.mode, via_select.mode);
        assert_eq!(via_toggle.pose(), via_select.pose());
        assert_eq!(via_toggle.grounded, via_select.grounded);
        assert_eq!(
            via_toggle.needs_movement_tick(),
            via_select.needs_movement_tick()
        );
    }

    #[test]
    fn orbit_camera_fresh_state_seeds_from_pose() {
        let mut camera = Camera::default();
        let pose = OrbitPose {
            yaw: 1.25,
            pitch: -0.75,
            distance: 3.5,
        };

        camera.ensure_spawn(7, Some(pose));

        assert_eq!(camera.content_id, Some(7));
        assert_eq!(camera.pose(), pose);
    }

    #[test]
    fn orbit_camera_without_pose_uses_default_framing() {
        let mut camera = Camera {
            content_id: None,
            orbit: super::super::super::orbit::Orbit::from_pose(
                OrbitPose {
                    yaw: 9.0,
                    pitch: -9.0,
                    distance: 4.0,
                },
                super::super::super::orbit::ZoomFloor::SolidMesh,
            ),
            drag_from: Some(Point::new(1.0, 2.0)),
        };

        camera.ensure_spawn(7, None);

        assert_eq!(camera.content_id, Some(7));
        assert_eq!(camera.pose(), OrbitPose::default());
        assert_eq!(camera.drag_from, None);
    }

    #[test]
    fn fly_camera_fresh_state_seeds_from_pose() {
        let scene = empty_preview(Vec3::new(-10.0, -10.0, -10.0), Vec3::new(10.0, 10.0, 10.0));
        let mut camera = FlyCamera::default();
        let pose = FlyPose {
            position: Vec3::new(3.0, 4.0, 5.0),
            yaw: 1.25,
            pitch: -0.75,
            speed: 3.5,
        };

        camera.ensure_spawn(&scene, None, 7, Some(pose), Some(MovementMode::Fly));

        assert_eq!(camera.content_id, Some(7));
        assert_eq!(camera.pose(), Some(pose));
    }

    #[test]
    fn fly_camera_without_pose_uses_map_spawn() {
        let scene = empty_preview(Vec3::new(-10.0, -10.0, -10.0), Vec3::new(10.0, 10.0, 10.0));
        let mut camera = FlyCamera::default();
        let spawn = MapSpawn {
            origin: Vec3::new(1.0, 2.0, 3.0),
            angles: Vec3::new(10.0, 90.0, 0.0),
        };

        camera.ensure_spawn(&scene, Some(spawn), 7, None, None);

        let pose = camera.pose().expect("spawn should initialize fly pose");
        assert_eq!(camera.content_id, Some(7));
        assert_eq!(
            pose.position,
            Vec3::new(1.0, 2.0, 3.0 + PLAYER_START_EYE_NUDGE)
        );
        assert!((pose.yaw - 90.0_f32.to_radians()).abs() < 1e-6);
        assert!((pose.pitch - -10.0_f32.to_radians()).abs() < 1e-6);
        assert_eq!(pose.speed, 1.0);
    }
}
