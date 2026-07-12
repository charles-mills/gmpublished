use super::{
    Action, Arc, DOOR_PROGRESS_EPSILON, DOOR_USE_REACH, DoorAudioEvent, DoorAudioEventKind,
    DoorInstance, DoorMoveLoopRuntime, DoorRenderPose, DoorRuntime, DoorTarget, Event, FlyPose,
    MAX_PITCH, MIN_PITCH, MapFog, MapSkyCamera, MapSpawn, MapTrace, MapVisibilityBucket,
    MapWalkCollision, Message, ModelPreview, ModelPrimitive, MovementMode, ORBIT_SENSITIVITY,
    OrbitPose, Point, Rectangle, Uniforms, ZOOM_STEP, add, bounds_intersect, choose_door_open_sign,
    cross, door_audio_event, door_progress_step, door_sound_gain, door_uses_move_loop,
    door_world_bounds, dot, endpoint_sound, expand_bounds, half_extent, initial_door_open_sign,
    length_squared, mid, mouse, mul, normalize, normalize_or_zero, ray_aabb_distance, shader, sub,
    trace_aabb_against_aabb,
};

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
    pub(super) yaw: f32,
    pub(super) pitch: f32,
    /// Multiplier over the model's auto-framed distance.
    pub(super) distance: f32,
    pub(super) drag_from: Option<Point>,
}

impl Default for Camera {
    fn default() -> Self {
        let pose = OrbitPose::default();
        Self {
            content_id: None,
            yaw: pose.yaw,
            pitch: pose.pitch,
            distance: pose.distance,
            drag_from: None,
        }
    }
}

impl Camera {
    pub(super) fn ensure_spawn(&mut self, content_id: u64, pose: Option<OrbitPose>) {
        if self.content_id == Some(content_id) {
            return;
        }
        let pose = pose.unwrap_or_default();
        self.content_id = Some(content_id);
        self.yaw = pose.yaw;
        self.pitch = pose.pitch;
        self.distance = pose.distance;
        self.drag_from = None;
    }

    pub(super) fn pose(&self) -> OrbitPose {
        OrbitPose {
            yaw: self.yaw,
            pitch: self.pitch,
            distance: self.distance,
        }
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
                camera.yaw += (to.x - from.x) * ORBIT_SENSITIVITY;
                camera.pitch = (camera.pitch + (to.y - from.y) * ORBIT_SENSITIVITY)
                    .clamp(MIN_PITCH, MAX_PITCH);
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
                camera.distance = (camera.distance * ZOOM_STEP.powf(steps)).clamp(0.2, 8.0);
                Some(Action::publish(Message::OrbitPoseChanged(camera.pose())).and_capture())
            }
            _ => None,
        }
    }

    fn draw(&self, camera: &Camera, _cursor: mouse::Cursor, bounds: Rectangle) -> ModelPrimitive {
        ModelPrimitive {
            model: Arc::clone(&self.model),
            content_id: self.content_id,
            skin_remap: self.skin_remap.clone(),
            bodygroup_choices: self.bodygroup_choices.clone(),
            map_skybox_visible: true,
            visibility_culling: false,
            phy_debug_visible: self.phy_debug_visible,
            uniforms: Uniforms::for_model(&self.model, camera, bounds),
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
    pub scene: Arc<ModelPreview>,
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
pub(super) const WALK_HULL_HALF_EXTENTS: [f32; 3] = [16.0, 16.0, 36.0];
pub(super) const WALK_EYE_TO_HULL_CENTER: [f32; 3] = [0.0, 0.0, -28.0];
pub(super) const WALK_HULL_CENTER_TO_EYE: [f32; 3] = [0.0, 0.0, 28.0];
pub(super) const WALK_DUCK_HULL_HALF_EXTENTS: [f32; 3] = [16.0, 16.0, 18.0];
pub(super) const WALK_DUCK_EYE_HEIGHT: f32 = 28.0;
pub(super) const WALK_DUCK_EYE_TO_HULL_CENTER: [f32; 3] = [0.0, 0.0, -10.0];
pub(super) const WALK_DUCK_HULL_CENTER_TO_EYE: [f32; 3] = [0.0, 0.0, 10.0];
pub(super) const WALK_SPEED: f32 = 190.0;
pub(super) const WALK_DUCK_SPEED: f32 = WALK_SPEED / 3.0;
// HL2 sprint speed; keeps the Source-defaults convention of the rest.
pub(super) const WALK_SPRINT_SPEED: f32 = 320.0;
pub(super) const WALK_SWIM_SPEED: f32 = 150.0;
pub(super) const WALK_WATER_FRICTION: f32 = 4.0;
pub(super) const WALK_WATER_EXIT_BOOST: f32 = 256.0;
pub(super) const WALK_SWIM_STOP_SPEED: f32 = 0.1;
pub(super) const WALK_GRAVITY: f32 = 800.0;
pub(super) const WALK_JUMP_SPEED: f32 = 268.328_16;
pub(super) const WALK_STEP_HEIGHT: f32 = 18.0;
pub(super) const WALK_GROUND_SNAP: f32 = 4.0;
pub(super) const WALK_GROUND_NORMAL_Z: f32 = 0.7;
pub(super) const WALK_SUBSTEP_SECONDS: f32 = 1.0 / 60.0;
pub(super) const WALK_MAX_SUBSTEPS: usize = 8;
pub(super) const WALK_UNSTICK_STEPS: usize = 16;
pub(super) const WALK_BOB_AMPLITUDE: f32 = 1.1;
pub(super) const WALK_BOB_FREQUENCY_HZ: f32 = 1.8;
pub(super) const WALK_BOB_RETURN_SPEED: f32 = 10.0;
pub(super) const LAND_BOB_DURATION: f32 = 0.22;
pub(super) const LAND_BOB_AMPLITUDE: f32 = 3.0;
pub(super) const LAND_BOB_MIN_FALL_SPEED: f32 = 120.0;
pub(super) const WALK_DUCK_VIEW_DURATION: f32 = 0.2;
pub(super) const WALK_VOID_EXIT_MARGIN: f32 = 512.0;

#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "ground, jump, water, duck, and exit-assist states change independently"
)]
pub struct FlyCamera {
    pub(super) content_id: Option<u64>,
    pub(super) position: Option<[f32; 3]>,
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
    pub(super) walk_velocity: [f32; 3],
    pub(super) grounded: bool,
    pub(super) jump_requested: bool,
    pub(super) walk_bob_phase: f32,
    pub(super) walk_bob_offset: f32,
    pub(super) land_bob_elapsed: f32,
    pub(super) land_bob_amplitude: f32,
    pub(super) swimming: bool,
    pub(super) submerged: bool,
    pub(super) water_exit_assist: bool,
    pub(super) walk_hull: WalkHull,
    pub(super) duck_view_animation: Option<DuckViewAnimation>,
    pub(super) duck_reconcile_requested: bool,
    pub(super) doors: Vec<DoorRuntime>,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub(super) enum WalkHull {
    #[default]
    Standing,
    Ducked,
}

impl WalkHull {
    pub(super) const fn half_extents(self) -> [f32; 3] {
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

    pub(super) const fn eye_to_hull_center(self) -> [f32; 3] {
        match self {
            Self::Standing => WALK_EYE_TO_HULL_CENTER,
            Self::Ducked => WALK_DUCK_EYE_TO_HULL_CENTER,
        }
    }

    pub(super) const fn hull_center_to_eye(self) -> [f32; 3] {
        match self {
            Self::Standing => WALK_HULL_CENTER_TO_EYE,
            Self::Ducked => WALK_DUCK_HULL_CENTER_TO_EYE,
        }
    }

    pub(super) const fn is_ducked(self) -> bool {
        matches!(self, Self::Ducked)
    }
}

#[derive(Debug, Clone, Copy)]
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

impl FlyCamera {
    pub(super) fn ensure_spawn(
        &mut self,
        scene: &ModelPreview,
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
        self.move_factor = 0.0;
        self.mode = MovementMode::Fly;
        self.walk_velocity = [0.0; 3];
        self.grounded = false;
        self.jump_requested = false;
        self.walk_bob_phase = 0.0;
        self.walk_bob_offset = 0.0;
        self.land_bob_elapsed = LAND_BOB_DURATION;
        self.land_bob_amplitude = 0.0;
        self.swimming = false;
        self.submerged = false;
        self.water_exit_assist = false;
        self.reset_duck_state();
        self.doors = scene
            .doors
            .iter()
            .map(|door| {
                let progress = door.initial_progress.clamp(0.0, 1.0);
                let open_sign = initial_door_open_sign(door.motion);
                let (bounds_min, bounds_max) = door_world_bounds(door, progress, open_sign);
                DoorRuntime {
                    progress,
                    target: if progress >= 1.0 - DOOR_PROGRESS_EPSILON {
                        DoorTarget::Open
                    } else {
                        DoorTarget::Closed
                    },
                    moving: false,
                    blocked_closing: false,
                    move_loop: None,
                    open_sign,
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
        self.position = Some([
            spawn.origin[0],
            spawn.origin[1],
            spawn.origin[2] + PLAYER_START_EYE_NUDGE,
        ]);
        self.yaw = spawn.angles[1].to_radians();
        self.pitch = (-spawn.angles[0].to_radians()).clamp(MIN_PITCH, MAX_PITCH);
    }

    pub(super) fn seed_from_bounds(&mut self, scene: &ModelPreview) {
        let center = mid(scene.bounds_min, scene.bounds_max);
        let radius = half_extent(scene.bounds_min, scene.bounds_max).max(1.0);
        self.position = Some([
            center[0] - radius * 0.6,
            center[1] - radius * 0.6,
            center[2] + radius * 0.35,
        ]);
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

    pub const fn submerged(&self) -> bool {
        self.submerged
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

    pub(super) fn forward(&self) -> [f32; 3] {
        [
            self.pitch.cos() * self.yaw.cos(),
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
        ]
    }

    pub(super) fn integrate(
        &mut self,
        scene: &ModelPreview,
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

    pub(super) fn integrate_fly(&mut self, scene: &ModelPreview, dt: f32) {
        if self.held.any_movement() {
            self.move_factor = (self.move_factor + dt / FLY_ACCEL_SECONDS).clamp(0.0, 1.0);
        } else {
            self.move_factor = 0.0;
            return;
        }
        let Some(position) = self.position.as_mut() else {
            return;
        };
        let radius = half_extent(scene.bounds_min, scene.bounds_max).max(1.0);
        let mut speed = radius * 0.4 * self.speed * self.move_factor;
        if self.held.fast {
            speed *= 3.0;
        }

        let forward = [
            self.pitch.cos() * self.yaw.cos(),
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
        ];
        let right = normalize(cross(forward, [0.0, 0.0, 1.0]));

        let mut delta = [0.0_f32; 3];
        let mut add = |direction: [f32; 3], sign: f32| {
            for (axis, value) in direction.iter().enumerate() {
                delta[axis] += value * sign;
            }
        };
        if self.held.forward {
            add(forward, 1.0);
        }
        if self.held.back {
            add(forward, -1.0);
        }
        if self.held.right {
            add(right, 1.0);
        }
        if self.held.left {
            add(right, -1.0);
        }
        if self.held.up {
            add([0.0, 0.0, 1.0], 1.0);
        }
        if self.held.down {
            add([0.0, 0.0, 1.0], -1.0);
        }

        let length = dot(delta, delta).sqrt();
        if length > f32::EPSILON {
            for axis in 0..3 {
                position[axis] += delta[axis] / length * speed * dt;
            }
        }
    }

    pub(super) fn exit_walk(&mut self) {
        self.mode = MovementMode::Fly;
        self.walk_velocity = [0.0; 3];
        self.grounded = false;
        self.jump_requested = false;
        self.walk_bob_offset = 0.0;
        self.land_bob_elapsed = LAND_BOB_DURATION;
        self.land_bob_amplitude = 0.0;
        self.swimming = false;
        self.submerged = false;
        self.water_exit_assist = false;
        self.reset_duck_state();
    }

    pub(super) fn toggle_walk(&mut self, scene: &ModelPreview) -> bool {
        let target = if self.mode == MovementMode::Walk {
            MovementMode::Fly
        } else {
            MovementMode::Walk
        };
        self.select_mode(scene, target)
    }

    pub(super) fn select_mode(&mut self, scene: &ModelPreview, target: MovementMode) -> bool {
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

    pub(super) fn enter_walk(&mut self, scene: &ModelPreview) -> bool {
        let Some(collision) = scene
            .walk_collision
            .as_ref()
            .filter(|collision| !collision.is_empty())
        else {
            return false;
        };
        let Some(position) = self.position else {
            return false;
        };

        self.reset_duck_state();
        // GMod noclip-off semantics: enter walk mode right where the camera
        // is and let gravity bring you down — no teleport-to-ground. The
        // first landing plays the head-bob; the void failsafe covers
        // toggling over nothing.
        let start = self.unstick_eye(collision, position, WalkHull::Standing);
        if self.aabb_trace_solid(
            collision,
            add(start, WalkHull::Standing.eye_to_hull_center()),
            WalkHull::Standing.half_extents(),
        ) {
            return false;
        }
        self.position = Some(start);
        self.mode = MovementMode::Walk;
        self.walk_velocity = [0.0; 3];
        self.grounded = false;
        self.jump_requested = false;
        self.walk_bob_phase = 0.0;
        self.walk_bob_offset = 0.0;
        self.land_bob_elapsed = LAND_BOB_DURATION;
        self.land_bob_amplitude = 0.0;
        self.swimming = false;
        self.submerged = false;
        self.water_exit_assist = false;
        self.move_factor = 0.0;
        true
    }

    pub(super) fn reset_duck_state(&mut self) {
        self.walk_hull = WalkHull::Standing;
        self.duck_view_animation = None;
        self.duck_reconcile_requested = false;
    }

    pub(super) fn request_jump(&mut self) {
        if self.mode == MovementMode::Walk {
            self.jump_requested = true;
        }
    }

    pub(super) fn needs_movement_tick(&self) -> bool {
        let door_moving = self.doors.iter().any(|door| door.moving);
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
                    || (self.swimming
                        && (self.held.any_movement()
                            || self.held.duck
                            || length_squared(self.walk_velocity)
                                > WALK_SWIM_STOP_SPEED * WALK_SWIM_STOP_SPEED))
                    || (!self.swimming && !self.grounded)
                    || self.land_bob_active()
                    || self.walk_bob_offset.abs() > 0.01
                    || self.duck_reconcile_requested
                    || self.duck_view_transition_active()
                    || door_moving
            }
        }
    }

    pub(super) fn land_bob_active(&self) -> bool {
        self.land_bob_amplitude > 0.0 && self.land_bob_elapsed < LAND_BOB_DURATION
    }

    pub(super) fn view_bob_offset(&self) -> f32 {
        if self.mode != MovementMode::Walk {
            return 0.0;
        }
        let landing = if self.land_bob_active() {
            let t = (self.land_bob_elapsed / LAND_BOB_DURATION).clamp(0.0, 1.0);
            -self.land_bob_amplitude * (std::f32::consts::PI * t).sin()
        } else {
            0.0
        };
        self.walk_bob_offset + landing
    }

    pub(super) fn duck_view_offset(&self) -> f32 {
        if self.mode != MovementMode::Walk {
            return 0.0;
        }
        self.duck_visual_eye_height() - self.walk_hull.eye_height()
    }

    pub(super) fn integrate_walk(&mut self, scene: &ModelPreview, dt: f32) {
        let Some(collision) = scene
            .walk_collision
            .as_ref()
            .filter(|collision| !collision.is_empty())
        else {
            self.exit_walk();
            return;
        };
        if self.position.is_none() {
            return;
        }

        let mut remaining = dt.min(0.1);
        for _ in 0..WALK_MAX_SUBSTEPS {
            if remaining <= f32::EPSILON {
                break;
            }
            let step = remaining.min(WALK_SUBSTEP_SECONDS);
            self.integrate_walk_step(collision, step);
            remaining -= step;
        }
        self.jump_requested = false;

        // Failsafe: a fall that never lands (off the map edge, out of the
        // world through a leak) has nothing left to collide with — without
        // this, `!grounded` keeps the redraw loop alive forever and
        // velocity grows without bound. idle-0% is a hard rule, so hand
        // the camera back to fly once we're clearly below all geometry.
        if let Some(position) = self.position
            && position[2] - self.walk_hull.eye_height()
                < scene.bounds_min[2] - WALK_VOID_EXIT_MARGIN
        {
            self.exit_walk();
        }
    }

    pub(super) fn integrate_walk_step(&mut self, collision: &MapWalkCollision, dt: f32) {
        self.reconcile_duck_state(collision);
        if !self.held.forward {
            self.water_exit_assist = false;
        }
        let was_swimming = self.swimming;
        let (water_level, surface_z) = self.water_level(collision);
        self.swimming = water_level >= 2;
        self.submerged = water_level == 3;
        if self.swimming {
            self.integrate_swim_step(collision, dt, was_swimming, surface_z);
            return;
        }

        let wish_direction = self.walk_wish_direction();
        let moving = length_squared(wish_direction) > f32::EPSILON;
        let was_grounded = self.grounded;
        let jumped = self.grounded && self.jump_requested;

        // Shift sprints — same mental model as the fly-mode speed boost.
        let speed = if self.walk_hull.is_ducked() {
            WALK_DUCK_SPEED
        } else if self.held.fast {
            WALK_SPRINT_SPEED
        } else {
            WALK_SPEED
        };
        self.walk_velocity[0] = wish_direction[0] * speed;
        self.walk_velocity[1] = wish_direction[1] * speed;
        if jumped {
            self.walk_velocity[2] = WALK_JUMP_SPEED;
            self.grounded = false;
        } else if self.grounded {
            self.walk_velocity[2] = 0.0;
        } else {
            self.walk_velocity[2] -= WALK_GRAVITY * dt;
        }

        let fall_speed = (-self.walk_velocity[2]).max(0.0);
        if !jumped {
            self.grounded = false;
        }
        self.move_walk_delta(
            collision,
            mul(self.walk_velocity, dt),
            (was_grounded && !jumped) || self.water_exit_assist,
        );
        if !jumped {
            self.snap_to_ground(collision, WALK_GROUND_SNAP);
        }
        if self.grounded {
            self.water_exit_assist = false;
        }
        if !was_grounded && self.grounded && fall_speed >= LAND_BOB_MIN_FALL_SPEED {
            self.land_bob_elapsed = 0.0;
            self.land_bob_amplitude = LAND_BOB_AMPLITUDE;
        }
        self.update_walk_bob(dt, moving && self.grounded);
        self.update_duck_view_animation(dt);
    }

    fn integrate_swim_step(
        &mut self,
        collision: &MapWalkCollision,
        dt: f32,
        was_swimming: bool,
        surface_z: Option<f32>,
    ) {
        if !was_swimming {
            self.walk_velocity = mul(self.walk_velocity, 0.25);
            self.land_bob_elapsed = LAND_BOB_DURATION;
            self.land_bob_amplitude = 0.0;
        }

        let wish_direction = self.swim_wish_direction();
        let moving = length_squared(wish_direction) > f32::EPSILON;
        let friction = (1.0 - WALK_WATER_FRICTION * dt).max(0.0);
        self.walk_velocity = mul(self.walk_velocity, friction);
        let wish_velocity = mul(wish_direction, WALK_SWIM_SPEED);
        self.walk_velocity = add(self.walk_velocity, mul(wish_velocity, 1.0 - friction));
        if self.held.forward
            && !self.submerged
            && surface_z.is_some_and(|surface_z| self.water_exit_ahead(collision, surface_z))
        {
            self.walk_velocity[2] = self.walk_velocity[2].max(WALK_WATER_EXIT_BOOST);
            self.water_exit_assist = true;
        }
        if !moving
            && length_squared(self.walk_velocity) <= WALK_SWIM_STOP_SPEED * WALK_SWIM_STOP_SPEED
        {
            self.walk_velocity = [0.0; 3];
        }

        let was_grounded = self.grounded;
        self.grounded = false;
        self.move_walk_delta(
            collision,
            mul(self.walk_velocity, dt),
            was_grounded || self.water_exit_assist,
        );
        self.snap_to_ground(collision, WALK_GROUND_SNAP);
        if self.grounded {
            self.water_exit_assist = false;
        }
        let (water_level, _) = self.water_level(collision);
        self.swimming = water_level >= 2;
        self.submerged = water_level == 3;
        self.update_walk_bob(dt, false);
        self.update_duck_view_animation(dt);
    }

    fn water_level(&self, collision: &MapWalkCollision) -> (u8, Option<f32>) {
        let Some(eye) = self.position else {
            return (0, None);
        };
        let center = add(eye, self.walk_hull.eye_to_hull_center());
        let feet = sub(center, [0.0, 0.0, self.walk_hull.half_extents()[2] - 2.0]);
        let feet_water = collision.water_at(feet);
        let waist_water = collision.water_at(center);
        let eye_water = collision.water_at(eye);
        let level = if eye_water.is_some() {
            3
        } else if waist_water.is_some() {
            2
        } else if feet_water.is_some() {
            1
        } else {
            0
        };
        let surface_z = [feet_water, waist_water, eye_water]
            .into_iter()
            .flatten()
            .map(|water| water.surface_z)
            .max_by(f32::total_cmp);
        (level, surface_z)
    }

    fn swim_wish_direction(&self) -> [f32; 3] {
        let forward = self.forward();
        let right = normalize(cross(forward, [0.0, 0.0, 1.0]));
        let mut direction = [0.0; 3];
        if self.held.forward {
            direction = add(direction, forward);
        }
        if self.held.back {
            direction = sub(direction, forward);
        }
        if self.held.right {
            direction = add(direction, right);
        }
        if self.held.left {
            direction = sub(direction, right);
        }
        if self.held.up {
            direction[2] += 1.0;
        }
        if self.held.down || self.held.duck {
            direction[2] -= 1.0;
        }
        normalize_or_zero(direction)
    }

    fn water_exit_ahead(&self, collision: &MapWalkCollision, surface_z: f32) -> bool {
        let Some(position) = self.position else {
            return false;
        };
        let forward = [self.yaw.cos(), self.yaw.sin(), 0.0];
        let distance = WALK_STEP_HEIGHT * 2.0;
        let blocked = self.trace_eye(
            collision,
            self.walk_hull,
            position,
            add(position, mul(forward, distance)),
        );
        if blocked.start_solid || blocked.fraction >= 1.0 {
            return false;
        }

        let probe = [position[0], position[1], surface_z + WALK_STEP_HEIGHT * 3.0];
        let over_ledge = collision.trace_aabb(probe, add(probe, mul(forward, distance)), [0.0; 3]);
        !over_ledge.start_solid && over_ledge.fraction >= 1.0
    }

    pub(super) fn walk_wish_direction(&self) -> [f32; 3] {
        let forward = [self.yaw.cos(), self.yaw.sin(), 0.0];
        let right = normalize(cross(forward, [0.0, 0.0, 1.0]));
        let mut direction = [0.0; 3];
        if self.held.forward {
            direction = add(direction, forward);
        }
        if self.held.back {
            direction = sub(direction, forward);
        }
        if self.held.right {
            direction = add(direction, right);
        }
        if self.held.left {
            direction = sub(direction, right);
        }
        normalize_or_zero(direction)
    }

    pub(super) fn move_walk_delta(
        &mut self,
        collision: &MapWalkCollision,
        delta: [f32; 3],
        allow_step: bool,
    ) {
        let Some(mut position) = self.position else {
            return;
        };
        let mut remaining = delta;
        for _ in 0..4 {
            if length_squared(remaining) <= 1.0e-6 {
                break;
            }
            let move_start = position;
            let trace = self.trace_eye(
                collision,
                self.walk_hull,
                move_start,
                add(move_start, remaining),
            );
            if trace.start_solid {
                self.walk_velocity = [0.0; 3];
                break;
            }
            position = trace.end_position;
            self.position = Some(position);
            if trace.fraction >= 1.0 {
                break;
            }

            if trace.normal[2] >= WALK_GROUND_NORMAL_Z && self.walk_velocity[2] <= 0.0 {
                self.grounded = true;
                self.walk_velocity[2] = 0.0;
            } else if allow_step
                && trace.normal[2].abs() < WALK_GROUND_NORMAL_Z
                && horizontal_length_squared(remaining) > 1.0e-4
                && self.try_step(collision, move_start, remaining)
            {
                return;
            }

            let leftover = mul(remaining, 1.0 - trace.fraction);
            remaining = clip_along_plane(leftover, trace.normal);
            self.walk_velocity = clip_along_plane(self.walk_velocity, trace.normal);
        }
    }

    pub(super) fn try_step(
        &mut self,
        collision: &MapWalkCollision,
        start: [f32; 3],
        delta: [f32; 3],
    ) -> bool {
        let up = self.trace_eye(
            collision,
            self.walk_hull,
            start,
            add(start, [0.0, 0.0, WALK_STEP_HEIGHT]),
        );
        if up.start_solid || up.fraction < 1.0 {
            return false;
        }

        let horizontal_delta = [delta[0], delta[1], 0.0];
        let forward = self.trace_eye(
            collision,
            self.walk_hull,
            up.end_position,
            add(up.end_position, horizontal_delta),
        );
        if forward.start_solid {
            return false;
        }
        if horizontal_length_squared(sub(forward.end_position, up.end_position)) <= 1.0e-4 {
            return false;
        }

        let down = self.trace_eye(
            collision,
            self.walk_hull,
            forward.end_position,
            sub(
                forward.end_position,
                [0.0, 0.0, WALK_STEP_HEIGHT + WALK_GROUND_SNAP],
            ),
        );
        if down.start_solid || down.fraction >= 1.0 || down.normal[2] < WALK_GROUND_NORMAL_Z {
            return false;
        }

        self.position = Some(down.end_position);
        self.grounded = true;
        self.walk_velocity[2] = 0.0;
        true
    }

    pub(super) fn snap_to_ground(&mut self, collision: &MapWalkCollision, distance: f32) {
        let Some(position) = self.position else {
            return;
        };
        let down = self.trace_eye(
            collision,
            self.walk_hull,
            position,
            sub(position, [0.0, 0.0, distance]),
        );
        if !down.start_solid && down.fraction < 1.0 && down.normal[2] >= WALK_GROUND_NORMAL_Z {
            self.position = Some(down.end_position);
            self.grounded = true;
            self.walk_velocity[2] = 0.0;
        }
    }

    pub(super) fn update_walk_bob(&mut self, dt: f32, moving: bool) {
        if moving {
            self.walk_bob_phase = (self.walk_bob_phase
                + dt * WALK_BOB_FREQUENCY_HZ * std::f32::consts::TAU)
                % std::f32::consts::TAU;
            self.walk_bob_offset = self.walk_bob_phase.sin() * WALK_BOB_AMPLITUDE;
        } else {
            let decay = (WALK_BOB_RETURN_SPEED * dt).clamp(0.0, 1.0);
            self.walk_bob_offset += (0.0 - self.walk_bob_offset) * decay;
            if self.walk_bob_offset.abs() <= 0.01 {
                self.walk_bob_offset = 0.0;
            }
        }

        if self.land_bob_active() {
            self.land_bob_elapsed = (self.land_bob_elapsed + dt).min(LAND_BOB_DURATION);
        }
    }

    pub(super) fn reconcile_duck_state(&mut self, collision: &MapWalkCollision) {
        if self.held.duck {
            self.duck();
        } else {
            self.try_unduck(collision);
        }
        self.duck_reconcile_requested = false;
    }

    pub(super) fn duck(&mut self) {
        if self.walk_hull.is_ducked() {
            return;
        }
        let Some(mut position) = self.position else {
            return;
        };
        let visual_height = self.duck_visual_eye_height();
        if self.grounded {
            position[2] -= PLAYER_START_EYE_NUDGE - WALK_DUCK_EYE_HEIGHT;
            self.position = Some(position);
        }
        self.set_walk_hull(WalkHull::Ducked, visual_height);
    }

    pub(super) fn try_unduck(&mut self, collision: &MapWalkCollision) {
        if !self.walk_hull.is_ducked() {
            return;
        }
        let Some(position) = self.position else {
            return;
        };
        let candidate = if self.grounded {
            add(
                position,
                [0.0, 0.0, PLAYER_START_EYE_NUDGE - WALK_DUCK_EYE_HEIGHT],
            )
        } else {
            // Airborne unduck expands the standing hull downward from the
            // current eye if it fits; this is the inverse of crouch-jump's
            // feet-pull-up shrink and avoids an eye teleport in mid-air.
            position
        };
        if self.aabb_trace_solid(
            collision,
            add(candidate, WalkHull::Standing.eye_to_hull_center()),
            WalkHull::Standing.half_extents(),
        ) {
            return;
        }
        let visual_height = self.duck_visual_eye_height();
        self.position = Some(candidate);
        self.set_walk_hull(WalkHull::Standing, visual_height);
    }

    pub(super) fn set_walk_hull(&mut self, hull: WalkHull, visual_height: f32) {
        self.walk_hull = hull;
        let target = self.walk_hull.eye_height();
        if (visual_height - target).abs() <= 0.01 {
            self.duck_view_animation = None;
        } else {
            self.duck_view_animation = Some(DuckViewAnimation {
                from_height: visual_height,
                elapsed: 0.0,
            });
        }
    }

    pub(super) fn update_duck_view_animation(&mut self, dt: f32) {
        if let Some(animation) = self.duck_view_animation.as_mut() {
            animation.elapsed = (animation.elapsed + dt).min(WALK_DUCK_VIEW_DURATION);
            if animation.elapsed >= WALK_DUCK_VIEW_DURATION {
                self.duck_view_animation = None;
            }
        }
    }

    pub(super) fn duck_view_transition_active(&self) -> bool {
        self.duck_view_animation
            .is_some_and(|animation| animation.elapsed < WALK_DUCK_VIEW_DURATION)
    }

    pub(super) fn duck_visual_eye_height(&self) -> f32 {
        let target = self.walk_hull.eye_height();
        self.duck_view_animation.map_or(target, |animation| {
            let t = (animation.elapsed / WALK_DUCK_VIEW_DURATION).clamp(0.0, 1.0);
            animation.from_height + (target - animation.from_height) * simple_spline(t)
        })
    }

    pub(super) fn integrate_doors(
        &mut self,
        scene: &ModelPreview,
        content_id: u64,
        dt: f32,
    ) -> Vec<DoorAudioEvent> {
        let player_hull = self.player_hull_bounds();
        let listener = self.position;
        let mut audio_events = Vec::new();
        for (index, runtime) in self.doors.iter_mut().enumerate() {
            if !runtime.moving {
                continue;
            }
            let Some(door) = scene.doors.get(index) else {
                runtime.moving = false;
                runtime.move_loop = None;
                continue;
            };
            let step = door_progress_step(door.motion, dt);
            let next_progress = match runtime.target {
                DoorTarget::Open => (runtime.progress + step).min(1.0),
                DoorTarget::Closed => (runtime.progress - step).max(0.0),
            };
            if runtime.target == DoorTarget::Closed
                && player_hull.is_some_and(|hull| {
                    let bounds = door_world_bounds(door, next_progress, runtime.open_sign);
                    bounds_intersect(bounds, hull)
                })
            {
                runtime.moving = false;
                runtime.blocked_closing = true;
                runtime.move_loop = None;
                audio_events.push(door_audio_event(
                    content_id,
                    index,
                    DoorAudioEventKind::Parked,
                    0.0,
                ));
                continue;
            }
            runtime.progress = next_progress;
            (runtime.bounds_min, runtime.bounds_max) =
                door_world_bounds(door, runtime.progress, runtime.open_sign);
            if (runtime.target == DoorTarget::Open
                && runtime.progress >= 1.0 - DOOR_PROGRESS_EPSILON)
                || (runtime.target == DoorTarget::Closed
                    && runtime.progress <= DOOR_PROGRESS_EPSILON)
            {
                runtime.progress = match runtime.target {
                    DoorTarget::Open => 1.0,
                    DoorTarget::Closed => 0.0,
                };
                (runtime.bounds_min, runtime.bounds_max) =
                    door_world_bounds(door, runtime.progress, runtime.open_sign);
                runtime.moving = false;
                runtime.blocked_closing = false;
                runtime.move_loop = None;
                let open = runtime.target == DoorTarget::Open;
                let sound = endpoint_sound(door, open);
                let gain =
                    door_sound_gain(listener, (runtime.bounds_min, runtime.bounds_max), sound);
                audio_events.push(door_audio_event(
                    content_id,
                    index,
                    DoorAudioEventKind::MotionEnded { open },
                    gain,
                ));
            } else if door_uses_move_loop(door.class) {
                let gain = door_sound_gain(
                    listener,
                    (runtime.bounds_min, runtime.bounds_max),
                    door.sounds.move_sound.as_ref(),
                );
                audio_events.push(door_audio_event(
                    content_id,
                    index,
                    DoorAudioEventKind::MoveLoopVolumeChanged,
                    gain,
                ));
            }
        }
        audio_events
    }

    pub(super) fn resume_blocked_doors_if_clear(
        &mut self,
        scene: &ModelPreview,
        content_id: u64,
    ) -> Vec<DoorAudioEvent> {
        let player_hull = self.player_hull_bounds();
        let listener = self.position;
        let mut audio_events = Vec::new();
        for (index, runtime) in self.doors.iter_mut().enumerate() {
            if !runtime.blocked_closing {
                continue;
            }
            let Some(door) = scene.doors.get(index) else {
                continue;
            };
            let next_progress = (runtime.progress - DOOR_PROGRESS_EPSILON).max(0.0);
            let bounds = door_world_bounds(door, next_progress, runtime.open_sign);
            if player_hull.is_none_or(|hull| !bounds_intersect(bounds, hull)) {
                runtime.blocked_closing = false;
                runtime.moving = true;
                runtime.target = DoorTarget::Closed;
                runtime.move_loop = door_uses_move_loop(door.class).then_some(DoorMoveLoopRuntime);
                let gain = door_sound_gain(
                    listener,
                    (runtime.bounds_min, runtime.bounds_max),
                    door.sounds.move_sound.as_ref(),
                );
                audio_events.push(door_audio_event(
                    content_id,
                    index,
                    DoorAudioEventKind::MoveStarted,
                    gain,
                ));
            }
        }
        audio_events
    }

    pub(super) fn toggle_nearest_door(
        &mut self,
        scene: &ModelPreview,
        content_id: u64,
    ) -> Option<DoorAudioEvent> {
        if self.mode != MovementMode::Walk {
            return None;
        }
        let start = self.position?;
        let direction = self.forward();
        let mut best: Option<(usize, f32)> = None;
        for (index, (door, runtime)) in scene.doors.iter().zip(&self.doors).enumerate() {
            if !self.door_visible_from_current_cluster(scene, door) {
                continue;
            }
            let Some(distance) =
                ray_aabb_distance(start, direction, (runtime.bounds_min, runtime.bounds_max))
            else {
                continue;
            };
            if distance <= DOOR_USE_REACH && best.is_none_or(|(_, best)| distance < best) {
                best = Some((index, distance));
            }
        }
        let (index, _) = best?;
        let door = scene.doors.get(index)?;
        let runtime = &mut self.doors[index];
        if runtime.target == DoorTarget::Open {
            runtime.target = DoorTarget::Closed;
        } else {
            runtime.target = DoorTarget::Open;
            runtime.open_sign = choose_door_open_sign(door, start, direction);
        }
        runtime.moving = true;
        runtime.blocked_closing = false;
        runtime.move_loop = door_uses_move_loop(door.class).then_some(DoorMoveLoopRuntime);
        let gain = door_sound_gain(
            self.position,
            (runtime.bounds_min, runtime.bounds_max),
            door.sounds.move_sound.as_ref(),
        );
        Some(door_audio_event(
            content_id,
            index,
            DoorAudioEventKind::MoveStarted,
            gain,
        ))
    }

    pub(super) fn door_visible_from_current_cluster(
        &self,
        scene: &ModelPreview,
        door: &DoorInstance,
    ) -> bool {
        let Some(visibility) = scene.visibility.as_ref() else {
            return true;
        };
        let Some(position) = self.position else {
            return true;
        };
        let Some(cluster) = visibility.cluster_at(position) else {
            return true;
        };
        let Some(visible) = visibility.visible_clusters(cluster) else {
            return true;
        };
        match door.visibility {
            MapVisibilityBucket::Always => true,
            MapVisibilityBucket::Cluster(cluster) => {
                visible.get(cluster as usize).copied().unwrap_or(false)
            }
        }
    }

    pub(super) fn player_hull_bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        (self.mode == MovementMode::Walk).then_some(())?;
        let position = self.position?;
        let center = add(position, self.walk_hull.eye_to_hull_center());
        Some(expand_bounds(
            (center, center),
            self.walk_hull.half_extents(),
        ))
    }

    pub(super) fn trace_eye(
        &self,
        collision: &MapWalkCollision,
        hull: WalkHull,
        start_eye: [f32; 3],
        end_eye: [f32; 3],
    ) -> MapTrace {
        let trace = self.trace_aabb(
            collision,
            add(start_eye, hull.eye_to_hull_center()),
            add(end_eye, hull.eye_to_hull_center()),
            hull.half_extents(),
        );
        MapTrace {
            end_position: add(trace.end_position, hull.hull_center_to_eye()),
            ..trace
        }
    }

    pub(super) fn trace_aabb(
        &self,
        collision: &MapWalkCollision,
        start: [f32; 3],
        end: [f32; 3],
        half_extents: [f32; 3],
    ) -> MapTrace {
        let mut best = collision.trace_aabb(start, end, half_extents);
        for door in &self.doors {
            if let Some(hit) = trace_aabb_against_aabb(
                start,
                end,
                half_extents,
                (door.bounds_min, door.bounds_max),
            ) && (hit.start_solid && !best.start_solid || hit.fraction < best.fraction)
            {
                best = hit;
            }
        }
        best
    }

    pub(super) fn aabb_trace_solid(
        &self,
        collision: &MapWalkCollision,
        center: [f32; 3],
        half_extents: [f32; 3],
    ) -> bool {
        collision.aabb_trace_solid(center, half_extents)
            || self.doors.iter().any(|door| {
                bounds_intersect(
                    expand_bounds((center, center), half_extents),
                    (door.bounds_min, door.bounds_max),
                )
            })
    }

    pub(super) fn unstick_eye(
        &self,
        collision: &MapWalkCollision,
        mut position: [f32; 3],
        hull: WalkHull,
    ) -> [f32; 3] {
        for _ in 0..WALK_UNSTICK_STEPS {
            if !self.aabb_trace_solid(
                collision,
                add(position, hull.eye_to_hull_center()),
                hull.half_extents(),
            ) {
                return position;
            }
            position[2] += WALK_STEP_HEIGHT;
        }
        position
    }
}

pub(super) fn rotate_source_vector(vector: [f32; 3], angles: [f32; 3]) -> [f32; 3] {
    let pitch = angles[0].to_radians();
    let yaw = angles[1].to_radians();
    let roll = angles[2].to_radians();
    rotate_z(rotate_y(rotate_x(vector, roll), pitch), yaw)
}

pub(super) fn rotate_x(vector: [f32; 3], radians: f32) -> [f32; 3] {
    let (sin, cos) = radians.sin_cos();
    [
        vector[0],
        vector[1] * cos - vector[2] * sin,
        vector[1] * sin + vector[2] * cos,
    ]
}

pub(super) fn rotate_y(vector: [f32; 3], radians: f32) -> [f32; 3] {
    let (sin, cos) = radians.sin_cos();
    [
        vector[0] * cos + vector[2] * sin,
        vector[1],
        -vector[0] * sin + vector[2] * cos,
    ]
}

pub(super) fn rotate_z(vector: [f32; 3], radians: f32) -> [f32; 3] {
    let (sin, cos) = radians.sin_cos();
    [
        vector[0] * cos - vector[1] * sin,
        vector[0] * sin + vector[1] * cos,
        vector[2],
    ]
}

pub(super) fn simple_spline(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

pub(super) fn clip_along_plane(vector: [f32; 3], normal: [f32; 3]) -> [f32; 3] {
    let into_plane = dot(vector, normal);
    if into_plane < 0.0 {
        sub(vector, mul(normal, into_plane))
    } else {
        vector
    }
}

pub(super) fn horizontal_length_squared(vector: [f32; 3]) -> f32 {
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
            model: Arc::clone(&self.scene),
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
                    open_sign: door.open_sign,
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
