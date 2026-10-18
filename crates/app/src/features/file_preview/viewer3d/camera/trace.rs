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
        let mut trace = self.trace_aabb(
            collision,
            start_eye + hull.eye_to_hull_center(),
            end_eye + hull.eye_to_hull_center(),
            hull.half_extents(),
        );
        if let Some(collision) = self.stage_walk_collision() {
            let staged = collision.trace_aabb(
                start_eye + hull.eye_to_hull_center(),
                end_eye + hull.eye_to_hull_center(),
                hull.half_extents(),
            );
            if staged.start_solid && !trace.start_solid || staged.fraction < trace.fraction {
                trace = staged;
            }
        }
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
            let hit = door.brush_collision.as_ref().map_or_else(
                || {
                    trace_aabb_against_aabb(
                        start,
                        end,
                        half_extents,
                        (door.bounds_min, door.bounds_max),
                    )
                },
                |collision| Some(collision.trace_aabb(start, end, half_extents)),
            );
            if let Some(hit) = hit
                && (hit.start_solid && !best.start_solid || hit.fraction < best.fraction)
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
            || self
                .stage_walk_collision()
                .is_some_and(|collision| collision.aabb_trace_solid(center, half_extents))
            || self.doors.iter().any(|door| {
                door.brush_collision.as_ref().map_or_else(
                    || {
                        bounds_intersect(
                            expand_bounds((center, center), half_extents),
                            (door.bounds_min, door.bounds_max),
                        )
                    },
                    |collision| collision.aabb_trace_solid(center, half_extents),
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
