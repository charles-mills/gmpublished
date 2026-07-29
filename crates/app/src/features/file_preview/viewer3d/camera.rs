use super::super::orbit::{MAX_PITCH, MIN_PITCH, Orbit, ZoomFloor};
use super::{
    Action, Arc, DOOR_PROGRESS_EPSILON, DOOR_USE_REACH, DoorAudioEvent, DoorAudioEventKind,
    DoorInstance, DoorMotion, DoorRenderPose, DoorRuntime, DoorTarget, Event, FlyPose, MapFog,
    MapSkyCamera, MapSpawn, MapTrace, MapVisibilityBucket, MapWalkCollision, Message, ModelPreview,
    ModelPrimitive, MovementMode, OrbitPose, Point, Rectangle, SOURCE_UP, Uniforms, add,
    bounds_intersect, choose_door_swing, cross, door_audio_event, door_progress_step,
    door_sound_gain, door_uses_move_loop, door_world_bounds, dot, endpoint_sound, expand_bounds,
    half_extent, initial_door_swing, length_squared, mid, mouse, mul, normalize, normalize_or_zero,
    ray_aabb_distance, shader, sub, trace_aabb_against_aabb,
};
pub(super) use gmpublished_backend::math::rotate_source_vector;
use gmpublished_backend::math::{QAngle, simple_spline};

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
            model: Arc::clone(&self.model),
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
    pub(super) water: WaterLevel,
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
        self.position = Some([
            spawn.origin[0],
            spawn.origin[1],
            spawn.origin[2] + PLAYER_START_EYE_NUDGE,
        ]);
        let angles = QAngle::from_source_degrees(spawn.angles);
        self.yaw = angles.yaw;
        self.pitch = (-angles.pitch).clamp(MIN_PITCH, MAX_PITCH);
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

    pub fn submerged(&self) -> bool {
        self.water.is_submerged()
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
        let right = normalize(cross(forward, SOURCE_UP));

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
        self.reset_walk_state();
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
        self.reset_walk_state();
        true
    }

    /// Clears every field that only means something while walking.
    ///
    /// One definition, because three sites need it and a field missed by one
    /// of them is silent: leaving `walk_bob_phase` behind carries the previous
    /// walk's head-bob into the next one, and leaving `move_factor` behind
    /// carries its speed ramp.
    ///
    /// `mode` is deliberately not touched: each caller sets it to the mode it
    /// is entering, and folding that in here would let a reset silently
    /// change which mode the camera is in.
    fn reset_walk_state(&mut self) {
        self.walk_velocity = [0.0; 3];
        self.grounded = false;
        self.jump_requested = false;
        self.walk_bob_phase = 0.0;
        self.walk_bob_offset = 0.0;
        self.land_bob_elapsed = LAND_BOB_DURATION;
        self.land_bob_amplitude = 0.0;
        self.water = WaterLevel::Dry;
        self.water_exit_assist = false;
        self.move_factor = 0.0;
        self.reset_duck_state();
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
                            || length_squared(self.walk_velocity)
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
        let was_swimming = self.water.is_swimming();
        let (water, surface_z) = self.water_level(collision);
        self.water = water;
        if self.water.is_swimming() {
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
            && !self.water.is_submerged()
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
        (self.water, _) = self.water_level(collision);
        self.update_walk_bob(dt, false);
        self.update_duck_view_animation(dt);
    }

    fn water_level(&self, collision: &MapWalkCollision) -> (WaterLevel, Option<f32>) {
        let Some(eye) = self.position else {
            return (WaterLevel::Dry, None);
        };
        let center = add(eye, self.walk_hull.eye_to_hull_center());
        let feet = sub(center, [0.0, 0.0, self.walk_hull.half_extents()[2] - 2.0]);
        let feet_water = collision.water_at(feet);
        let waist_water = collision.water_at(center);
        let eye_water = collision.water_at(eye);
        let level = if eye_water.is_some() {
            WaterLevel::Eyes
        } else if waist_water.is_some() {
            WaterLevel::Waist
        } else if feet_water.is_some() {
            WaterLevel::Feet
        } else {
            WaterLevel::Dry
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
        let right = normalize(cross(forward, SOURCE_UP));
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
        let right = normalize(cross(forward, SOURCE_UP));
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
            if let DoorMotion::HoldingOpen { remaining } = runtime.motion {
                if remaining > dt {
                    runtime.motion = DoorMotion::HoldingOpen {
                        remaining: remaining - dt,
                    };
                    continue;
                }
                // The hold elapsed: close on its own, exactly as a toggle would.
                runtime.motion = DoorMotion::Moving;
                runtime.target = DoorTarget::Closed;
                if let Some(door) = scene.doors.get(index) {
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
            if runtime.motion != DoorMotion::Moving {
                continue;
            }
            let Some(door) = scene.doors.get(index) else {
                runtime.motion = DoorMotion::Idle;
                continue;
            };
            let step = door_progress_step(door.motion, dt);
            let next_progress = match runtime.target {
                DoorTarget::Open => (runtime.progress + step).min(1.0),
                DoorTarget::Closed => (runtime.progress - step).max(0.0),
            };
            if runtime.target == DoorTarget::Closed
                && player_hull.is_some_and(|hull| {
                    let bounds = door_world_bounds(door, next_progress, runtime.swing);
                    bounds_intersect(bounds, hull)
                })
            {
                runtime.motion = DoorMotion::BlockedClosing;
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
                door_world_bounds(door, runtime.progress, runtime.swing);
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
                    door_world_bounds(door, runtime.progress, runtime.swing);
                let open = runtime.target == DoorTarget::Open;
                runtime.motion = door
                    .auto_close_after
                    .filter(|_| open)
                    .map_or(DoorMotion::Idle, |remaining| DoorMotion::HoldingOpen {
                        remaining,
                    });
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
            if runtime.motion != DoorMotion::BlockedClosing {
                continue;
            }
            let Some(door) = scene.doors.get(index) else {
                continue;
            };
            let next_progress = (runtime.progress - DOOR_PROGRESS_EPSILON).max(0.0);
            let bounds = door_world_bounds(door, next_progress, runtime.swing);
            if player_hull.is_none_or(|hull| !bounds_intersect(bounds, hull)) {
                runtime.motion = DoorMotion::Moving;
                runtime.target = DoorTarget::Closed;
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
            runtime.swing = choose_door_swing(door, start, direction);
        }
        runtime.motion = DoorMotion::Moving;
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
    use super::super::test_support::empty_preview;
    use super::*;

    /// Three sites leave walk mode, and a field missed by one of them is
    /// silent: dropping `walk_bob_phase` carries the previous walk's head-bob
    /// into the next one, dropping `move_factor` carries its speed ramp. This
    /// fails if any single field escapes the shared reset.
    #[test]
    fn leaving_walk_clears_every_walk_only_field() {
        let mut camera = FlyCamera {
            mode: MovementMode::Walk,
            walk_velocity: [1.0, 2.0, 3.0],
            grounded: true,
            jump_requested: true,
            walk_bob_phase: 1.5,
            walk_bob_offset: 4.0,
            land_bob_elapsed: 0.0,
            land_bob_amplitude: 9.0,
            water: WaterLevel::Eyes,
            water_exit_assist: true,
            walk_hull: WalkHull::Ducked,
            duck_reconcile_requested: true,
            move_factor: 0.75,
            ..FlyCamera::default()
        };

        camera.exit_walk();

        assert_eq!(camera.mode, MovementMode::Fly);
        assert_eq!(camera.walk_velocity, [0.0; 3]);
        assert!(!camera.grounded);
        assert!(!camera.jump_requested);
        assert_eq!(camera.walk_bob_phase, 0.0);
        assert_eq!(camera.walk_bob_offset, 0.0);
        assert_eq!(camera.land_bob_elapsed, LAND_BOB_DURATION);
        assert_eq!(camera.land_bob_amplitude, 0.0);
        assert_eq!(camera.water, WaterLevel::Dry);
        assert!(!camera.water_exit_assist);
        assert_eq!(camera.walk_hull, WalkHull::Standing);
        assert!(camera.duck_view_animation.is_none());
        assert!(!camera.duck_reconcile_requested);
        assert_eq!(camera.move_factor, 0.0);
    }
    fn floor_scene() -> ModelPreview {
        let mut scene = empty_preview([0.0; 3], [1024.0; 3]);
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            [-4096.0, -4096.0, -64.0],
            [4096.0, 4096.0, 0.0],
        ));
        scene
    }

    fn deep_water_scene() -> ModelPreview {
        let mut scene = empty_preview([-512.0, -512.0, -320.0], [512.0, 512.0, 256.0]);
        scene.walk_collision = Some(
            MapWalkCollision::solid_box_for_tests(
                [-4096.0, -4096.0, -320.0],
                [4096.0, 4096.0, -256.0],
            )
            .with_water_box_for_tests([-4096.0, -4096.0, -256.0], [4096.0, 4096.0, 100.0]),
        );
        scene
    }

    fn walk_camera(position: [f32; 3], grounded: bool) -> FlyCamera {
        FlyCamera {
            content_id: Some(1),
            position: Some(position),
            mode: MovementMode::Walk,
            grounded,
            ..FlyCamera::default()
        }
    }

    fn horizontal_distance_from(position: [f32; 3], origin: [f32; 3]) -> f32 {
        ((position[0] - origin[0]).powi(2) + (position[1] - origin[1]).powi(2)).sqrt()
    }

    #[test]
    fn walk_standing_on_the_floor_can_move_and_jump() {
        let scene = floor_scene();

        // Resting contact: hull bottom a hair above the floor plane — the
        // state every landing converges to (hit traces back the mover off
        // by the plane epsilon, so a grounded player rests at that
        // separation, never at mathematically exact contact).
        let mut camera = walk_camera([512.0, 512.0, 64.1], true);

        camera.held.forward = true;
        for _ in 0..30 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
        }
        let after_walk = camera.position.expect("position retained");
        let walked = horizontal_distance_from(after_walk, [512.0, 512.0, 64.1]);
        assert!(
            walked > 30.0,
            "half a second of held-forward must actually move the player, moved {walked}"
        );
        assert!(camera.grounded, "walking on flat ground must stay grounded");

        camera.held.forward = false;
        camera.request_jump();
        let ground_z = after_walk[2];
        let mut apex = ground_z;
        let mut left_ground = false;
        for _ in 0..120 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            let z = camera.position.expect("position retained")[2];
            apex = apex.max(z);
            left_ground |= !camera.grounded;
        }
        assert!(left_ground, "jump must leave the ground");
        assert!(
            apex > ground_z + 20.0,
            "jump apex should clear ~45 units, got {}",
            apex - ground_z
        );
        assert!(camera.grounded, "jump must land again within two seconds");
    }

    #[test]
    fn walk_falling_into_deep_water_stops_falling() {
        let scene = deep_water_scene();
        let mut camera = walk_camera([0.0, 0.0, 180.0], false);

        for _ in 0..180 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
        }

        assert!(camera.water.is_swimming());
        assert!(!camera.grounded);
        assert!(camera.position.expect("swimmer position")[2] > 40.0);
        assert!(camera.walk_velocity[2].abs() < 0.1);
    }

    #[test]
    fn walk_swimming_forward_uses_view_pitch() {
        let scene = deep_water_scene();
        let mut camera = walk_camera([0.0, 0.0, 64.0], false);
        camera.pitch = -0.6;
        camera.held.forward = true;

        for _ in 0..60 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
        }

        let position = camera.position.expect("swimmer position");
        assert!(
            position[0] > 50.0,
            "forward swim should advance: {position:?}"
        );
        assert!(
            position[2] < 20.0,
            "downward pitch should dive: {position:?}"
        );
        assert!(camera.submerged());
    }

    #[test]
    fn walk_motionless_floating_swimmer_goes_idle_within_two_seconds() {
        let scene = deep_water_scene();
        let mut camera = walk_camera([0.0, 0.0, 64.0], false);
        camera.walk_velocity = [120.0, 0.0, -30.0];

        for _ in 0..120 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            if !camera.needs_movement_tick() {
                break;
            }
        }

        assert!(camera.water.is_swimming());
        assert_eq!(camera.walk_velocity, [0.0; 3]);
        assert!(!camera.needs_movement_tick());
    }

    #[test]
    fn walk_swimming_exit_assist_climbs_pool_ledge() {
        let mut scene = empty_preview([-256.0, -256.0, -128.0], [256.0, 256.0, 160.0]);
        scene.walk_collision = Some(
            MapWalkCollision::solid_box_for_tests(
                [-4096.0, -4096.0, -128.0],
                [4096.0, 4096.0, -64.0],
            )
            .with_solid_box_for_tests([48.0, -128.0, -64.0], [256.0, 128.0, 82.0])
            .with_water_box_for_tests([-256.0, -128.0, -64.0], [48.0, 128.0, 64.0]),
        );
        let mut camera = walk_camera([0.0, 0.0, 68.0], false);
        camera.held.forward = true;

        for _ in 0..240 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            if camera.grounded && camera.position.is_some_and(|position| position[2] > 145.0) {
                break;
            }
        }

        let position = camera.position.expect("walker position");
        assert!(
            camera.grounded,
            "exit assist should finish grounded: {camera:?}"
        );
        assert!(
            position[0] > 32.0,
            "exit assist should clear the ledge: {position:?}"
        );
        assert!(
            position[2] > 145.0,
            "hull should stand on the ledge: {position:?}"
        );
    }

    #[test]
    fn walk_entering_water_suppresses_land_bob() {
        let scene = deep_water_scene();
        let mut camera = walk_camera([0.0, 0.0, 64.0], false);
        camera.walk_velocity[2] = -240.0;
        camera.land_bob_elapsed = 0.05;
        camera.land_bob_amplitude = LAND_BOB_AMPLITUDE;

        let _ = camera.integrate(&scene, 1, 1.0 / 60.0);

        assert!(camera.water.is_swimming());
        assert_eq!(camera.land_bob_amplitude, 0.0);
        assert_eq!(camera.view_bob_offset(), 0.0);
    }

    #[test]
    fn walk_sprint_covers_more_ground_than_walking() {
        let scene = floor_scene();
        let run = |sprint: bool| {
            let mut camera = walk_camera([512.0, 512.0, 64.1], true);
            camera.held.forward = true;
            camera.held.fast = sprint;
            for _ in 0..60 {
                let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            }
            let position = camera.position.expect("position retained");
            horizontal_distance_from(position, [512.0, 512.0, 64.1])
        };
        let walked = run(false);
        let sprinted = run(true);
        assert!(
            sprinted > walked * 1.4,
            "shift must sprint: walked {walked}, sprinted {sprinted}"
        );
    }

    #[test]
    fn walk_toggle_at_exact_floor_contact_unsticks_and_walks() {
        let scene = floor_scene();

        // Mappers place info_player_start exactly on the floor, so the
        // hull starts at mathematically exact contact — the trace calls
        // that solid even though the embed check does not. Toggling walk
        // here must unstick and produce a mover that actually moves.
        let mut camera = FlyCamera {
            content_id: Some(1),
            position: Some([512.0, 512.0, 64.0]),
            ..FlyCamera::default()
        };
        camera.toggle_walk(&scene);
        assert_eq!(camera.mode, MovementMode::Walk, "toggle must engage walk");

        camera.held.forward = true;
        for _ in 0..90 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
        }
        assert!(camera.grounded, "must settle onto the floor");
        let position = camera.position.expect("position retained");
        let walked = horizontal_distance_from(position, [512.0, 512.0, 64.0]);
        assert!(
            walked > 30.0,
            "held-forward from an exact-contact spawn must move, moved {walked}"
        );
    }

    #[test]
    fn walk_crouch_enters_low_gap_and_refuses_blocked_unduck() {
        let mut scene = empty_preview([-128.0, -128.0, 0.0], [256.0, 128.0, 128.0]);
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            [80.0, -64.0, 40.0],
            [160.0, 64.0, 128.0],
        ));
        let collision = scene.walk_collision.as_ref().expect("collision fixture");

        let mut standing = walk_camera([0.0, 0.0, PLAYER_START_EYE_NUDGE], true);
        standing.move_walk_delta(collision, [140.0, 0.0, 0.0], true);
        assert!(
            standing.position.expect("standing position")[0] < 70.0,
            "standing hull must not enter a 40-unit gap"
        );

        let mut camera = walk_camera([0.0, 0.0, PLAYER_START_EYE_NUDGE], true);
        camera.held.duck = true;
        camera.reconcile_duck_state(collision);
        assert_eq!(camera.walk_hull, WalkHull::Ducked);
        assert_eq!(
            camera.position.expect("ducked position")[2],
            WALK_DUCK_EYE_HEIGHT
        );

        camera.move_walk_delta(collision, [140.0, 0.0, 0.0], true);
        let under_ceiling = camera.position.expect("under ceiling");
        assert!(
            under_ceiling[0] > 120.0,
            "ducked hull must pass under the 40-unit ceiling"
        );

        camera.held.duck = false;
        camera.reconcile_duck_state(collision);
        assert_eq!(camera.walk_hull, WalkHull::Ducked);
        assert_eq!(
            camera.position.expect("blocked unduck keeps eye")[2],
            under_ceiling[2],
            "blocked unduck must leave the physics eye low"
        );

        camera.move_walk_delta(collision, [100.0, 0.0, 0.0], true);
        camera.reconcile_duck_state(collision);
        assert_eq!(camera.walk_hull, WalkHull::Standing);
        assert!(
            (camera.position.expect("standing again")[2] - PLAYER_START_EYE_NUDGE).abs() < 1.0e-4,
            "unduck outside the ceiling must restore standing eye height"
        );
    }

    #[test]
    fn walk_step_rejects_zero_horizontal_progress_at_backed_off_wall_contact() {
        let collision =
            MapWalkCollision::solid_box_for_tests([80.0, -64.0, 0.0], [120.0, 64.0, 128.0]);
        let mut camera = walk_camera(
            [
                80.0 - WALK_HULL_HALF_EXTENTS[0] - 0.03125,
                0.0,
                PLAYER_START_EYE_NUDGE,
            ],
            true,
        );
        let start = camera.position.expect("walk position");

        assert!(
            !camera.try_step(&collision, start, [120.0, 0.0, 0.0]),
            "a step attempt that cannot move forward must fall back to slide/clip handling"
        );
        assert_eq!(camera.position, Some(start));
    }

    #[test]
    fn walk_crouch_jump_pulls_feet_up_to_clear_obstacle() {
        let mut scene = empty_preview([-128.0, -128.0, 0.0], [256.0, 128.0, 128.0]);
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            [60.0, -32.0, 0.0],
            [90.0, 32.0, 64.0],
        ));
        let collision = scene.walk_collision.as_ref().expect("collision fixture");

        let mut jumper = walk_camera([0.0, 0.0, PLAYER_START_EYE_NUDGE], true);
        jumper.request_jump();
        for _ in 0..24 {
            jumper.integrate_walk_step(collision, 1.0 / 60.0);
            if jumper.walk_velocity[2] <= 0.0 {
                break;
            }
        }
        let apex = jumper.position.expect("jump apex");
        assert!(apex[2] > 100.0, "jump fixture should reach obstacle height");

        let mut standing = walk_camera(apex, false);
        standing.move_walk_delta(collision, [140.0, 0.0, 0.0], false);
        assert!(
            standing.position.expect("standing air move")[0] < 50.0,
            "standing jump must hit the obstacle"
        );

        let mut ducked = walk_camera(apex, false);
        ducked.held.duck = true;
        ducked.reconcile_duck_state(collision);
        assert_eq!(
            ducked.position.expect("air duck keeps eye"),
            apex,
            "air duck must shrink toward the eye, not lower it"
        );
        assert_eq!(ducked.walk_hull, WalkHull::Ducked);

        ducked.move_walk_delta(collision, [140.0, 0.0, 0.0], false);
        assert!(
            ducked.position.expect("ducked air move")[0] > 120.0,
            "ducked jump must pull feet above the obstacle"
        );
    }

    #[test]
    fn walk_ducked_speed_is_one_third_and_overrides_sprint() {
        let scene = floor_scene();
        let run = |duck: bool, sprint: bool| {
            let mut camera = walk_camera([512.0, 512.0, 64.1], true);
            camera.held.forward = true;
            camera.held.duck = duck;
            camera.held.fast = sprint;
            for _ in 0..60 {
                let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            }
            horizontal_distance_from(
                camera.position.expect("position retained"),
                [512.0, 512.0, 64.1],
            )
        };

        let walked = run(false, false);
        let ducked = run(true, false);
        let duck_sprinted = run(true, true);
        assert!(
            ((ducked / walked) - (1.0 / 3.0)).abs() < 0.03,
            "ducked speed must be one third of walk: walked {walked}, ducked {ducked}"
        );
        assert!(
            (duck_sprinted - ducked).abs() < 0.5,
            "duck must override sprint: ducked {ducked}, duck+sprint {duck_sprinted}"
        );
    }

    #[test]
    fn walk_duck_view_animation_terminates_and_goes_idle() {
        let scene = floor_scene();
        let mut camera = walk_camera([512.0, 512.0, 64.1], true);
        camera.held.duck = true;
        camera.duck_reconcile_requested = true;

        assert!(camera.needs_movement_tick());
        for _ in 0..20 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
        }

        assert_eq!(camera.walk_hull, WalkHull::Ducked);
        assert!(
            !camera.duck_view_transition_active(),
            "duck view interpolation must finish after >0.2s"
        );
        assert!(
            !camera.needs_movement_tick(),
            "settled crouch with no movement must not keep the tick loop alive"
        );
    }

    #[test]
    fn default_walk_entry_settles_grounded_and_goes_idle() {
        let scene = floor_scene();
        let spawn = MapSpawn {
            origin: [512.0, 512.0, 0.0],
            angles: [0.0, 90.0, 0.0],
        };
        let mut camera = FlyCamera::default();

        camera.ensure_spawn(&scene, Some(spawn), 7, None, None);

        assert_eq!(camera.mode, MovementMode::Walk);
        assert!(!camera.grounded, "default walk entry starts airborne");
        for _ in 0..240 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            if !camera.needs_movement_tick() {
                break;
            }
        }
        assert_eq!(camera.mode, MovementMode::Walk);
        assert!(camera.grounded, "spawned walker must settle to ground");
        assert!(
            !camera.needs_movement_tick(),
            "settled default-walk spawn must reach idle"
        );
    }

    #[test]
    fn restored_walk_mode_reenters_walk_from_pose() {
        let scene = floor_scene();
        let pose = FlyPose {
            position: [512.0, 512.0, 128.0],
            yaw: 0.5,
            pitch: -0.25,
            speed: 1.75,
        };
        let mut camera = FlyCamera::default();

        camera.ensure_spawn(&scene, None, 7, Some(pose), Some(MovementMode::Walk));

        assert_eq!(camera.pose(), Some(pose));
        assert_eq!(camera.mode, MovementMode::Walk);
        assert!(
            !camera.grounded,
            "walk restore must resume gravity from pose"
        );
    }

    #[test]
    fn restored_fly_mode_keeps_fly_pose() {
        let scene = floor_scene();
        let pose = FlyPose {
            position: [512.0, 512.0, 128.0],
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
            position: [128.0, 256.0, 384.0],
            yaw: 0.5,
            pitch: -0.25,
            speed: 2.0,
        };
        let spawn = MapSpawn {
            origin: [512.0, 512.0, 0.0],
            angles: [0.0, 90.0, 0.0],
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
            position: [512.0, 512.0, 128.0],
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
    fn default_walk_entry_falls_back_without_spawn_or_collision() {
        let scene = floor_scene();
        let mut camera = FlyCamera::default();
        camera.ensure_spawn(&scene, None, 7, None, None);
        assert_eq!(camera.mode, MovementMode::Fly);

        let no_collision = empty_preview([0.0; 3], [1024.0; 3]);
        let spawn = MapSpawn {
            origin: [512.0, 512.0, 0.0],
            angles: [0.0; 3],
        };
        let mut camera = FlyCamera::default();
        camera.ensure_spawn(&no_collision, Some(spawn), 7, None, None);
        assert_eq!(camera.mode, MovementMode::Fly);
    }

    #[test]
    fn default_walk_entry_falls_back_when_spawn_remains_solid() {
        let mut scene = empty_preview([-1024.0; 3], [1024.0; 3]);
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            [-1024.0, -1024.0, -1024.0],
            [1024.0, 1024.0, 1024.0],
        ));
        let spawn = MapSpawn {
            origin: [0.0; 3],
            angles: [0.0; 3],
        };
        let mut camera = FlyCamera::default();

        camera.ensure_spawn(&scene, Some(spawn), 7, None, None);

        assert_eq!(camera.mode, MovementMode::Fly);
        assert_eq!(
            camera.position.expect("fly fallback position"),
            [0.0, 0.0, PLAYER_START_EYE_NUDGE]
        );
    }

    #[test]
    fn walk_falling_into_the_void_reverts_to_fly_and_goes_idle() {
        let mut scene = empty_preview([0.0; 3], [1024.0; 3]);
        // Non-empty collision (walk mode refuses to engage otherwise), but
        // nothing anywhere near the camera — an endless fall.
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            [4000.0, 4000.0, 0.0],
            [4100.0, 4100.0, 100.0],
        ));

        let mut camera = FlyCamera {
            content_id: Some(1),
            position: Some([512.0, 512.0, 2048.0]),
            mode: MovementMode::Walk,
            ..FlyCamera::default()
        };

        assert!(camera.needs_movement_tick(), "airborne walker must tick");
        for _ in 0..600 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            if camera.mode == MovementMode::Fly {
                break;
            }
        }
        assert_eq!(
            camera.mode,
            MovementMode::Fly,
            "endless fall must hand the camera back to fly"
        );
        assert!(
            !camera.needs_movement_tick(),
            "after the void failsafe the redraw loop must go idle"
        );
        let position = camera.position.expect("position retained");
        assert!(position[2].is_finite());
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
            orbit: crate::features::file_preview::orbit::Orbit::from_pose(
                OrbitPose {
                    yaw: 9.0,
                    pitch: -9.0,
                    distance: 4.0,
                },
                crate::features::file_preview::orbit::ZoomFloor::SolidMesh,
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
        let scene = empty_preview([-10.0, -10.0, -10.0], [10.0, 10.0, 10.0]);
        let mut camera = FlyCamera::default();
        let pose = FlyPose {
            position: [3.0, 4.0, 5.0],
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
        let scene = empty_preview([-10.0, -10.0, -10.0], [10.0, 10.0, 10.0]);
        let mut camera = FlyCamera::default();
        let spawn = MapSpawn {
            origin: [1.0, 2.0, 3.0],
            angles: [10.0, 90.0, 0.0],
        };

        camera.ensure_spawn(&scene, Some(spawn), 7, None, None);

        let pose = camera.pose().expect("spawn should initialize fly pose");
        assert_eq!(camera.content_id, Some(7));
        assert_eq!(pose.position, [1.0, 2.0, 3.0 + PLAYER_START_EYE_NUDGE]);
        assert!((pose.yaw - 90.0_f32.to_radians()).abs() < 1e-6);
        assert!((pose.pitch - -10.0_f32.to_radians()).abs() < 1e-6);
        assert_eq!(pose.speed, 1.0);
    }
}
