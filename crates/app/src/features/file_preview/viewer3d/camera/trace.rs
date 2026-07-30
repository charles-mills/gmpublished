//! Hull queries against the map: the player's swept AABB traces and the
//! unstick search used when a spawn lands inside geometry.

use super::super::super::state::MovementMode;
use super::super::{MapTrace, bounds_intersect, expand_bounds, trace_aabb_against_aabb};
use super::{FlyCamera, WALK_STEP_HEIGHT, WalkHull};
use gmpublished_domain::math::Vec3;
use gmpublished_domain::scene::map::MapWalkCollision;

const WALK_UNSTICK_STEPS: usize = 16;

impl FlyCamera {
    pub(in super::super) fn player_hull_bounds(&self) -> Option<(Vec3, Vec3)> {
        (self.mode == MovementMode::Walk).then_some(())?;
        let position = self.position?;
        let center = position + self.walk_hull.eye_to_hull_center();
        Some(expand_bounds(
            (center, center),
            self.walk_hull.half_extents(),
        ))
    }

    pub(super) fn trace_eye(
        &self,
        collision: &MapWalkCollision,
        hull: WalkHull,
        start_eye: Vec3,
        end_eye: Vec3,
    ) -> MapTrace {
        let trace = self.trace_aabb(
            collision,
            start_eye + hull.eye_to_hull_center(),
            end_eye + hull.eye_to_hull_center(),
            hull.half_extents(),
        );
        MapTrace {
            end_position: (trace.end_position + hull.hull_center_to_eye()),
            ..trace
        }
    }

    pub(in super::super) fn trace_aabb(
        &self,
        collision: &MapWalkCollision,
        start: Vec3,
        end: Vec3,
        half_extents: Vec3,
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
        center: Vec3,
        half_extents: Vec3,
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
        mut position: Vec3,
        hull: WalkHull,
    ) -> Vec3 {
        for _ in 0..WALK_UNSTICK_STEPS {
            if !self.aabb_trace_solid(
                collision,
                position + hull.eye_to_hull_center(),
                hull.half_extents(),
            ) {
                return position;
            }
            position[2] += WALK_STEP_HEIGHT;
        }
        position
    }
}
