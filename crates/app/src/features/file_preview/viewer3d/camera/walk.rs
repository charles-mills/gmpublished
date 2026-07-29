//! Ground movement: the walk state machine, per-substep integration,
//! wish-direction, step-up and ground snapping.

use super::super::super::state::MovementMode;
use super::super::{ModelPreview, SOURCE_UP, normalize_or_up};
use super::{
    FlyCamera, LAND_BOB_AMPLITUDE, LAND_BOB_DURATION, WALK_GROUND_SNAP, WALK_SPEED,
    WALK_STEP_HEIGHT, WalkHull, WaterLevel, clip_along_plane, horizontal_length_squared,
};
use gmpublished_backend::math::Vec3;
use gmpublished_backend::scene::map::MapWalkCollision;

const WALK_DUCK_SPEED: f32 = WALK_SPEED / 3.0;

const WALK_SPRINT_SPEED: f32 = 320.0;

const WALK_GRAVITY: f32 = 800.0;

const WALK_JUMP_SPEED: f32 = 268.328_16;

const WALK_GROUND_NORMAL_Z: f32 = 0.7;

const WALK_SUBSTEP_SECONDS: f32 = 1.0 / 60.0;

const WALK_MAX_SUBSTEPS: usize = 8;

const LAND_BOB_MIN_FALL_SPEED: f32 = 120.0;

const WALK_VOID_EXIT_MARGIN: f32 = 512.0;

impl FlyCamera {
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
            start + WalkHull::Standing.eye_to_hull_center(),
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
    pub(super) fn reset_walk_state(&mut self) {
        self.walk_velocity = Vec3::splat(0.0);
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

    pub(super) fn request_jump(&mut self) {
        if self.mode == MovementMode::Walk {
            self.jump_requested = true;
        }
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
        let moving = wish_direction.length_squared() > f32::EPSILON;
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
            self.walk_velocity * dt,
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

    pub(super) fn walk_wish_direction(&self) -> Vec3 {
        let forward = Vec3::new(self.yaw.cos(), self.yaw.sin(), 0.0);
        let right = normalize_or_up(forward.cross(SOURCE_UP));
        let mut direction = Vec3::ZERO;
        if self.held.forward {
            direction += forward;
        }
        if self.held.back {
            direction -= forward;
        }
        if self.held.right {
            direction += right;
        }
        if self.held.left {
            direction -= right;
        }
        direction.normalize_or_zero()
    }

    pub(super) fn move_walk_delta(
        &mut self,
        collision: &MapWalkCollision,
        delta: Vec3,
        allow_step: bool,
    ) {
        let Some(mut position) = self.position else {
            return;
        };
        let mut remaining = delta;
        for _ in 0..4 {
            if remaining.length_squared() <= 1.0e-6 {
                break;
            }
            let move_start = position;
            let trace = self.trace_eye(
                collision,
                self.walk_hull,
                move_start,
                move_start + remaining,
            );
            if trace.start_solid {
                self.walk_velocity = Vec3::splat(0.0);
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

            let leftover = remaining * (1.0 - trace.fraction);
            remaining = clip_along_plane(leftover, trace.normal);
            self.walk_velocity = clip_along_plane(self.walk_velocity, trace.normal);
        }
    }

    pub(super) fn try_step(
        &mut self,
        collision: &MapWalkCollision,
        start: Vec3,
        delta: Vec3,
    ) -> bool {
        let up = self.trace_eye(
            collision,
            self.walk_hull,
            start,
            start + Vec3::from([0.0, 0.0, WALK_STEP_HEIGHT]),
        );
        if up.start_solid || up.fraction < 1.0 {
            return false;
        }

        let horizontal_delta = [delta[0], delta[1], 0.0];
        let forward = self.trace_eye(
            collision,
            self.walk_hull,
            up.end_position,
            up.end_position + Vec3::from(horizontal_delta),
        );
        if forward.start_solid {
            return false;
        }
        if horizontal_length_squared(forward.end_position - up.end_position) <= 1.0e-4 {
            return false;
        }

        let down = self.trace_eye(
            collision,
            self.walk_hull,
            forward.end_position,
            forward.end_position - Vec3::from([0.0, 0.0, WALK_STEP_HEIGHT + WALK_GROUND_SNAP]),
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
            position - Vec3::from([0.0, 0.0, distance]),
        );
        if !down.start_solid && down.fraction < 1.0 && down.normal[2] >= WALK_GROUND_NORMAL_Z {
            self.position = Some(down.end_position);
            self.grounded = true;
            self.walk_velocity[2] = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::super::state::FlyPose;
    use super::super::super::test_support::{
        empty_preview, floor_scene, horizontal_distance_from, walk_camera,
    };
    use super::super::{PLAYER_START_EYE_NUDGE, WALK_HULL_HALF_EXTENTS};
    use super::*;
    use crate::media::preview_model::MapSpawn;

    /// Three sites leave walk mode, and a field missed by one of them is
    /// silent: dropping `walk_bob_phase` carries the previous walk's head-bob
    /// into the next one, dropping `move_factor` carries its speed ramp. This
    /// fails if any single field escapes the shared reset.
    #[test]
    fn leaving_walk_clears_every_walk_only_field() {
        let mut camera = FlyCamera {
            mode: MovementMode::Walk,
            walk_velocity: Vec3::new(1.0, 2.0, 3.0),
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
        assert_eq!(camera.walk_velocity, Vec3::splat(0.0));
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

    #[test]
    fn walk_standing_on_the_floor_can_move_and_jump() {
        let scene = floor_scene();

        // Resting contact: hull bottom a hair above the floor plane — the
        // state every landing converges to (hit traces back the mover off
        // by the plane epsilon, so a grounded player rests at that
        // separation, never at mathematically exact contact).
        let mut camera = walk_camera(Vec3::new(512.0, 512.0, 64.1), true);

        camera.held.forward = true;
        for _ in 0..30 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
        }
        let after_walk = camera.position.expect("position retained");
        let walked = horizontal_distance_from(after_walk, Vec3::new(512.0, 512.0, 64.1));
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
    fn walk_sprint_covers_more_ground_than_walking() {
        let scene = floor_scene();
        let run = |sprint: bool| {
            let mut camera = walk_camera(Vec3::new(512.0, 512.0, 64.1), true);
            camera.held.forward = true;
            camera.held.fast = sprint;
            for _ in 0..60 {
                let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            }
            let position = camera.position.expect("position retained");
            horizontal_distance_from(position, Vec3::new(512.0, 512.0, 64.1))
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
            position: Some(Vec3::new(512.0, 512.0, 64.0)),
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
        let walked = horizontal_distance_from(position, Vec3::new(512.0, 512.0, 64.0));
        assert!(
            walked > 30.0,
            "held-forward from an exact-contact spawn must move, moved {walked}"
        );
    }

    #[test]
    fn walk_step_rejects_zero_horizontal_progress_at_backed_off_wall_contact() {
        let collision = MapWalkCollision::solid_box_for_tests(
            Vec3::new(80.0, -64.0, 0.0),
            Vec3::new(120.0, 64.0, 128.0),
        );
        let mut camera = walk_camera(
            Vec3::new(
                80.0 - WALK_HULL_HALF_EXTENTS[0] - 0.03125,
                0.0,
                PLAYER_START_EYE_NUDGE,
            ),
            true,
        );
        let start = camera.position.expect("walk position");

        assert!(
            !camera.try_step(&collision, start, Vec3::new(120.0, 0.0, 0.0)),
            "a step attempt that cannot move forward must fall back to slide/clip handling"
        );
        assert_eq!(camera.position, Some(start));
    }

    #[test]
    fn default_walk_entry_settles_grounded_and_goes_idle() {
        let scene = floor_scene();
        let spawn = MapSpawn {
            origin: Vec3::new(512.0, 512.0, 0.0),
            angles: Vec3::new(0.0, 90.0, 0.0),
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
            position: Vec3::new(512.0, 512.0, 128.0),
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
    fn default_walk_entry_falls_back_without_spawn_or_collision() {
        let scene = floor_scene();
        let mut camera = FlyCamera::default();
        camera.ensure_spawn(&scene, None, 7, None, None);
        assert_eq!(camera.mode, MovementMode::Fly);

        let no_collision = empty_preview(Vec3::splat(0.0), Vec3::splat(1024.0));
        let spawn = MapSpawn {
            origin: Vec3::new(512.0, 512.0, 0.0),
            angles: Vec3::splat(0.0),
        };
        let mut camera = FlyCamera::default();
        camera.ensure_spawn(&no_collision, Some(spawn), 7, None, None);
        assert_eq!(camera.mode, MovementMode::Fly);
    }

    #[test]
    fn default_walk_entry_falls_back_when_spawn_remains_solid() {
        let mut scene = empty_preview(Vec3::splat(-1024.0), Vec3::splat(1024.0));
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            Vec3::new(-1024.0, -1024.0, -1024.0),
            Vec3::new(1024.0, 1024.0, 1024.0),
        ));
        let spawn = MapSpawn {
            origin: Vec3::splat(0.0),
            angles: Vec3::splat(0.0),
        };
        let mut camera = FlyCamera::default();

        camera.ensure_spawn(&scene, Some(spawn), 7, None, None);

        assert_eq!(camera.mode, MovementMode::Fly);
        assert_eq!(
            camera.position.expect("fly fallback position"),
            Vec3::new(0.0, 0.0, PLAYER_START_EYE_NUDGE)
        );
    }

    #[test]
    fn walk_falling_into_the_void_reverts_to_fly_and_goes_idle() {
        let mut scene = empty_preview(Vec3::splat(0.0), Vec3::splat(1024.0));
        // Non-empty collision (walk mode refuses to engage otherwise), but
        // nothing anywhere near the camera — an endless fall.
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            Vec3::new(4000.0, 4000.0, 0.0),
            Vec3::new(4100.0, 4100.0, 100.0),
        ));

        let mut camera = FlyCamera {
            content_id: Some(1),
            position: Some(Vec3::new(512.0, 512.0, 2048.0)),
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
}
