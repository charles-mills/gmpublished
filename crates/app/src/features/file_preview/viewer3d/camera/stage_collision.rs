use std::sync::Arc;

use gmpublished_domain::scene::map::{ConvexHull, MapWalkCollision, MapWalkPropCollisionSource};

use super::super::super::state::MovementMode;
use super::super::pipeline::StagePrimitive;
use super::{FlyCamera, Vec3};

#[derive(Debug)]
pub(in super::super) struct StageCollision {
    hulls: Arc<[ConvexHull]>,
    transform: [[f32; 4]; 4],
    collision: MapWalkCollision,
    pass_through: bool,
}

impl FlyCamera {
    pub(super) fn sync_stage_collision(&mut self, stage: Option<&StagePrimitive>) -> bool {
        if stage.is_none() {
            self.stage_grab = None;
        }
        let stage = stage.filter(|stage| !stage.model.physics_hulls.is_empty());
        let Some(stage) = stage else {
            return self.stage_collision.take().is_some();
        };
        // Held geometry follows the camera. Rebuild on release instead of making
        // the player collide with the object they are moving on every input tick.
        if self.stage_grab.is_some() {
            if let Some(collision) = &mut self.stage_collision {
                collision.pass_through = true;
            }
            return false;
        }
        let transform = super::super::mat_mul(stage.transform.matrix(), stage.live_transform);
        let changed = self.stage_collision.as_ref().is_none_or(|cached| {
            !Arc::ptr_eq(&cached.hulls, &stage.model.physics_hulls) || cached.transform != transform
        });
        if changed {
            let recovering = self
                .stage_collision
                .as_ref()
                .is_some_and(|cached| cached.pass_through);
            let hulls = stage
                .model
                .physics_hulls
                .iter()
                .map(|hull| ConvexHull {
                    vertices: hull
                        .vertices
                        .iter()
                        .map(|point| {
                            Vec3::from(std::array::from_fn(|row| {
                                transform[3][row]
                                    + (0..3)
                                        .map(|column| transform[column][row] * point[column])
                                        .sum::<f32>()
                            }))
                        })
                        .collect(),
                    triangles: hull.triangles.clone(),
                })
                .collect::<Vec<_>>();
            // Bevel planes must be regenerated after the full placement transform:
            // the swept player AABB remains aligned to the world's axes.
            let collision =
                MapWalkCollision::empty().with_prop_collisions([MapWalkPropCollisionSource {
                    ledges: &hulls,
                    origin: Vec3::ZERO,
                    angles: Vec3::ZERO,
                    scale: 1.0,
                }]);
            // New placement contact alone is not penetration: retain solid
            // support beneath a resting player, but allow escape from overlap.
            let overlaps_player = self.mode == MovementMode::Walk
                && self.position.is_some_and(|position| {
                    collision.aabb_trace_solid(
                        position + self.walk_hull.eye_to_hull_center(),
                        self.walk_hull.half_extents() - Vec3::splat(0.25),
                    )
                });
            self.stage_collision = Some(StageCollision {
                hulls: Arc::clone(&stage.model.physics_hulls),
                transform,
                collision,
                pass_through: recovering || overlaps_player,
            });
        }
        if let Some(collision) = &mut self.stage_collision
            && collision.pass_through
        {
            // Keep escape enabled until the entire player hull has cleared,
            // rather than reenabling collision while its edge still penetrates.
            collision.pass_through = self.mode == MovementMode::Walk
                && self.position.is_some_and(|position| {
                    collision.collision.aabb_trace_solid(
                        position + self.walk_hull.eye_to_hull_center(),
                        self.walk_hull.half_extents(),
                    )
                });
        }
        changed
    }

    pub(super) fn stage_walk_collision(&self) -> Option<&MapWalkCollision> {
        if self.stage_grab.is_some() {
            return None;
        }
        self.stage_collision
            .as_ref()
            .filter(|collision| !collision.pass_through)
            .map(|collision| &collision.collision)
    }
}
