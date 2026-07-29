//! Ducking: the hull swap, the unduck headroom check, and the eased view
//! offset that keeps the camera from snapping between eye heights.

use super::super::super::state::MovementMode;
use super::{DuckViewAnimation, FlyCamera, PLAYER_START_EYE_NUDGE, WALK_DUCK_EYE_HEIGHT, WalkHull};
use gmpublished_backend::math::{Vec3, simple_spline};
use gmpublished_backend::scene::map::MapWalkCollision;

const WALK_DUCK_VIEW_DURATION: f32 = 0.2;

impl FlyCamera {
    pub(super) fn reset_duck_state(&mut self) {
        self.walk_hull = WalkHull::Standing;
        self.duck_view_animation = None;
        self.duck_reconcile_requested = false;
    }

    pub(in super::super) fn duck_view_offset(&self) -> f32 {
        if self.mode != MovementMode::Walk {
            return 0.0;
        }
        self.duck_visual_eye_height() - self.walk_hull.eye_height()
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
            position + Vec3::from([0.0, 0.0, PLAYER_START_EYE_NUDGE - WALK_DUCK_EYE_HEIGHT])
        } else {
            // Airborne unduck expands the standing hull downward from the
            // current eye if it fits; this is the inverse of crouch-jump's
            // feet-pull-up shrink and avoids an eye teleport in mid-air.
            position
        };
        if self.aabb_trace_solid(
            collision,
            candidate + WalkHull::Standing.eye_to_hull_center(),
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
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::{
        empty_preview, floor_scene, horizontal_distance_from, walk_camera,
    };
    use super::*;

    #[test]
    fn walk_crouch_enters_low_gap_and_refuses_blocked_unduck() {
        let mut scene = empty_preview(
            Vec3::new(-128.0, -128.0, 0.0),
            Vec3::new(256.0, 128.0, 128.0),
        );
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            Vec3::new(80.0, -64.0, 40.0),
            Vec3::new(160.0, 64.0, 128.0),
        ));
        let collision = scene.walk_collision.as_ref().expect("collision fixture");

        let mut standing = walk_camera(Vec3::new(0.0, 0.0, PLAYER_START_EYE_NUDGE), true);
        standing.move_walk_delta(collision, Vec3::new(140.0, 0.0, 0.0), true);
        assert!(
            standing.position.expect("standing position")[0] < 70.0,
            "standing hull must not enter a 40-unit gap"
        );

        let mut camera = walk_camera(Vec3::new(0.0, 0.0, PLAYER_START_EYE_NUDGE), true);
        camera.held.duck = true;
        camera.reconcile_duck_state(collision);
        assert_eq!(camera.walk_hull, WalkHull::Ducked);
        assert_eq!(
            camera.position.expect("ducked position")[2],
            WALK_DUCK_EYE_HEIGHT
        );

        camera.move_walk_delta(collision, Vec3::new(140.0, 0.0, 0.0), true);
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

        camera.move_walk_delta(collision, Vec3::new(100.0, 0.0, 0.0), true);
        camera.reconcile_duck_state(collision);
        assert_eq!(camera.walk_hull, WalkHull::Standing);
        assert!(
            (camera.position.expect("standing again")[2] - PLAYER_START_EYE_NUDGE).abs() < 1.0e-4,
            "unduck outside the ceiling must restore standing eye height"
        );
    }

    #[test]
    fn walk_crouch_jump_pulls_feet_up_to_clear_obstacle() {
        let mut scene = empty_preview(
            Vec3::new(-128.0, -128.0, 0.0),
            Vec3::new(256.0, 128.0, 128.0),
        );
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            Vec3::new(60.0, -32.0, 0.0),
            Vec3::new(90.0, 32.0, 64.0),
        ));
        let collision = scene.walk_collision.as_ref().expect("collision fixture");

        let mut jumper = walk_camera(Vec3::new(0.0, 0.0, PLAYER_START_EYE_NUDGE), true);
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
        standing.move_walk_delta(collision, Vec3::new(140.0, 0.0, 0.0), false);
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

        ducked.move_walk_delta(collision, Vec3::new(140.0, 0.0, 0.0), false);
        assert!(
            ducked.position.expect("ducked air move")[0] > 120.0,
            "ducked jump must pull feet above the obstacle"
        );
    }

    #[test]
    fn walk_ducked_speed_is_one_third_and_overrides_sprint() {
        let scene = floor_scene();
        let run = |duck: bool, sprint: bool| {
            let mut camera = walk_camera(Vec3::new(512.0, 512.0, 64.1), true);
            camera.held.forward = true;
            camera.held.duck = duck;
            camera.held.fast = sprint;
            for _ in 0..60 {
                let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            }
            horizontal_distance_from(
                camera.position.expect("position retained"),
                Vec3::new(512.0, 512.0, 64.1),
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
        let mut camera = walk_camera(Vec3::new(512.0, 512.0, 64.1), true);
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
}
