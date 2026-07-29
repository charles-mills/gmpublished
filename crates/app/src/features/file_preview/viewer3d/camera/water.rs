//! Swimming: buoyant integration, how deep the hull is in a water brush,
//! and the boost that carries a player out over a ledge.

use super::super::{SOURCE_UP, normalize_or_up};
use super::{
    FlyCamera, LAND_BOB_DURATION, WALK_GROUND_SNAP, WALK_STEP_HEIGHT, WALK_SWIM_STOP_SPEED,
    WaterLevel,
};
use gmpublished_backend::math::Vec3;
use gmpublished_backend::scene::map::MapWalkCollision;

const WALK_SWIM_SPEED: f32 = 150.0;

const WALK_WATER_FRICTION: f32 = 4.0;

const WALK_WATER_EXIT_BOOST: f32 = 256.0;

impl FlyCamera {
    pub(super) fn integrate_swim_step(
        &mut self,
        collision: &MapWalkCollision,
        dt: f32,
        was_swimming: bool,
        surface_z: Option<f32>,
    ) {
        if !was_swimming {
            self.walk_velocity *= 0.25;
            self.land_bob_elapsed = LAND_BOB_DURATION;
            self.land_bob_amplitude = 0.0;
        }

        let wish_direction = self.swim_wish_direction();
        let moving = wish_direction.length_squared() > f32::EPSILON;
        let friction = (1.0 - WALK_WATER_FRICTION * dt).max(0.0);
        self.walk_velocity *= friction;
        let wish_velocity = wish_direction * WALK_SWIM_SPEED;
        self.walk_velocity += wish_velocity * (1.0 - friction);
        if self.held.forward
            && !self.water.is_submerged()
            && surface_z.is_some_and(|surface_z| self.water_exit_ahead(collision, surface_z))
        {
            self.walk_velocity[2] = self.walk_velocity[2].max(WALK_WATER_EXIT_BOOST);
            self.water_exit_assist = true;
        }
        if !moving
            && self.walk_velocity.length_squared() <= WALK_SWIM_STOP_SPEED * WALK_SWIM_STOP_SPEED
        {
            self.walk_velocity = Vec3::splat(0.0);
        }

        let was_grounded = self.grounded;
        self.grounded = false;
        self.move_walk_delta(
            collision,
            self.walk_velocity * dt,
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

    pub(super) fn water_level(&self, collision: &MapWalkCollision) -> (WaterLevel, Option<f32>) {
        let Some(eye) = self.position else {
            return (WaterLevel::Dry, None);
        };
        let center = eye + self.walk_hull.eye_to_hull_center();
        let feet = center - Vec3::from([0.0, 0.0, self.walk_hull.half_extents()[2] - 2.0]);
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

    fn swim_wish_direction(&self) -> Vec3 {
        let forward = self.forward();
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
        if self.held.up {
            direction[2] += 1.0;
        }
        if self.held.down || self.held.duck {
            direction[2] -= 1.0;
        }
        direction.normalize_or_zero()
    }

    fn water_exit_ahead(&self, collision: &MapWalkCollision, surface_z: f32) -> bool {
        let Some(position) = self.position else {
            return false;
        };
        let forward = Vec3::new(self.yaw.cos(), self.yaw.sin(), 0.0);
        let distance = WALK_STEP_HEIGHT * 2.0;
        let blocked = self.trace_eye(
            collision,
            self.walk_hull,
            position,
            position + (forward * distance),
        );
        if blocked.start_solid || blocked.fraction >= 1.0 {
            return false;
        }

        let probe = Vec3::new(position[0], position[1], surface_z + WALK_STEP_HEIGHT * 3.0);
        let over_ledge =
            collision.trace_aabb(probe, probe + (forward * distance), Vec3::splat(0.0));
        !over_ledge.start_solid && over_ledge.fraction >= 1.0
    }

    pub fn submerged(&self) -> bool {
        self.water.is_submerged()
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::{deep_water_scene, empty_preview, walk_camera};
    use super::super::LAND_BOB_AMPLITUDE;
    use super::*;

    #[test]
    fn walk_falling_into_deep_water_stops_falling() {
        let scene = deep_water_scene();
        let mut camera = walk_camera(Vec3::new(0.0, 0.0, 180.0), false);

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
        let mut camera = walk_camera(Vec3::new(0.0, 0.0, 64.0), false);
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
        let mut camera = walk_camera(Vec3::new(0.0, 0.0, 64.0), false);
        camera.walk_velocity = Vec3::new(120.0, 0.0, -30.0);

        for _ in 0..120 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            if !camera.needs_movement_tick() {
                break;
            }
        }

        assert!(camera.water.is_swimming());
        assert_eq!(camera.walk_velocity, Vec3::splat(0.0));
        assert!(!camera.needs_movement_tick());
    }

    #[test]
    fn walk_swimming_exit_assist_climbs_pool_ledge() {
        let mut scene = empty_preview(
            Vec3::new(-256.0, -256.0, -128.0),
            Vec3::new(256.0, 256.0, 160.0),
        );
        scene.walk_collision = Some(
            MapWalkCollision::solid_box_for_tests(
                Vec3::new(-4096.0, -4096.0, -128.0),
                Vec3::new(4096.0, 4096.0, -64.0),
            )
            .with_solid_box_for_tests(
                Vec3::new(48.0, -128.0, -64.0),
                Vec3::new(256.0, 128.0, 82.0),
            )
            .with_water_box_for_tests(
                Vec3::new(-256.0, -128.0, -64.0),
                Vec3::new(48.0, 128.0, 64.0),
            ),
        );
        let mut camera = walk_camera(Vec3::new(0.0, 0.0, 68.0), false);
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
        let mut camera = walk_camera(Vec3::new(0.0, 0.0, 64.0), false);
        camera.walk_velocity[2] = -240.0;
        camera.land_bob_elapsed = 0.05;
        camera.land_bob_amplitude = LAND_BOB_AMPLITUDE;

        let _ = camera.integrate(&scene, 1, 1.0 / 60.0);

        assert!(camera.water.is_swimming());
        assert_eq!(camera.land_bob_amplitude, 0.0);
        assert_eq!(camera.view_bob_offset(), 0.0);
    }
}
