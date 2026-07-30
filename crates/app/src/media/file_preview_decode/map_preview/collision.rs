//! Prop collision enrichment and PHY diagnostic geometry.

use super::{
    Arc, BTreeMap, ConvexLedge, HashMap, LoadedPhy, LoadedPropModel, LoadedPropPhysics,
    MAP_PROP_PLACEMENT_CAP, MapWalkCollision, MapWalkPropModelPlacement, MeshData, ModelVertex,
    PHY_DEBUG_TRIANGLE_CAP, PHY_DEBUG_VERTEX_COLOR, PropCollisionStats, ReadStats, SkipReason,
    StaticPropPlacement, Vec3, transform_prop_position,
};

pub(in super::super) fn enrich_walk_collision_with_prop_collision(
    walk_collision: Option<MapWalkCollision>,
    placements: &[StaticPropPlacement],
    loaded_model_cache: &HashMap<String, Option<Arc<LoadedPropModel>>>,
) -> (Option<MapWalkCollision>, PropCollisionStats) {
    let mut stats = PropCollisionStats::default();
    for model in loaded_model_cache.values().flatten() {
        match &model.physics {
            LoadedPropPhysics::Parsed(physics) => {
                stats.parsed_models = stats.parsed_models.saturating_add(1);
                merge_phy_skip_reasons(&mut stats.skip_reasons, &physics.stats);
            }
            LoadedPropPhysics::Unparseable(parse_stats) => {
                merge_phy_skip_reasons(&mut stats.skip_reasons, parse_stats);
            }
            LoadedPropPhysics::Missing => {}
        }
    }

    let mut sources = Vec::<MapWalkPropModelPlacement<'_>>::new();
    for placement in placements.iter().take(MAP_PROP_PLACEMENT_CAP) {
        if !placement.solid.is_physics() {
            stats.skipped_not_solid = stats.skipped_not_solid.saturating_add(1);
            continue;
        }
        stats.solid_placements = stats.solid_placements.saturating_add(1);
        let Some(Some(model)) = loaded_model_cache.get(&placement.model_path) else {
            stats.skipped_model_load = stats.skipped_model_load.saturating_add(1);
            continue;
        };
        let physics = match &model.physics {
            LoadedPropPhysics::Parsed(physics) => physics,
            LoadedPropPhysics::Missing => {
                stats.skipped_missing_phy = stats.skipped_missing_phy.saturating_add(1);
                continue;
            }
            LoadedPropPhysics::Unparseable(_) => {
                stats.skipped_unparseable_phy = stats.skipped_unparseable_phy.saturating_add(1);
                continue;
            }
        };
        if physics.ledges.is_empty() {
            stats.skipped_unparseable_phy = stats.skipped_unparseable_phy.saturating_add(1);
            continue;
        }
        let Some(collision) = model.collision.as_deref() else {
            stats.skipped_unparseable_phy = stats.skipped_unparseable_phy.saturating_add(1);
            continue;
        };
        sources.push(MapWalkPropModelPlacement {
            model: collision,
            origin: placement.origin,
            angles: placement.angles,
            scale: placement.scale,
        });
    }
    stats.collidable_placements = sources.len();

    if sources.is_empty() {
        return (walk_collision, stats);
    }
    let collision = walk_collision
        .unwrap_or_else(MapWalkCollision::empty)
        .with_prop_collision_models(sources);
    stats.prop_hulls = collision.prop_hull_count();
    stats.memory_bytes = collision.prop_collision_memory_bytes();
    (Some(collision), stats)
}

pub(in super::super) fn phy_debug_mesh_from_loaded_prop_models(
    placements: &[StaticPropPlacement],
    loaded_model_cache: &HashMap<String, Option<Arc<LoadedPropModel>>>,
    material_index: usize,
) -> Option<MeshData> {
    let mut builder = PhyDebugMeshBuilder::default();

    'placements: for placement in placements.iter().take(MAP_PROP_PLACEMENT_CAP) {
        let Some(Some(model)) = loaded_model_cache.get(&placement.model_path) else {
            continue;
        };
        let LoadedPropPhysics::Parsed(physics) = &model.physics else {
            continue;
        };
        if physics.ledges.is_empty() || !placement.scale.is_finite() || placement.scale <= 0.0 {
            continue;
        }
        if !builder.push_loaded_phy(physics, |position| {
            transform_prop_position(position, placement)
        }) {
            break 'placements;
        }
    }

    if builder.truncated {
        log::debug!("map preview .phy debug mesh truncated at {PHY_DEBUG_TRIANGLE_CAP} triangles");
    }
    builder.finish(material_index)
}

#[derive(Default)]
pub(in super::super) struct PhyDebugMeshBuilder {
    pub(in super::super) vertices: Vec<ModelVertex>,
    pub(in super::super) indices: Vec<u32>,
    pub(in super::super) truncated: bool,
}

impl PhyDebugMeshBuilder {
    pub(in super::super) fn push_loaded_phy(
        &mut self,
        physics: &LoadedPhy,
        mut transform: impl FnMut(Vec3) -> Vec3,
    ) -> bool {
        for ledge in &physics.ledges {
            for &triangle in &ledge.triangles {
                if self.indices.len() / 3 >= PHY_DEBUG_TRIANGLE_CAP {
                    self.truncated = true;
                    return false;
                }
                self.push_triangle(ledge, triangle, &mut transform);
            }
        }
        true
    }

    fn push_triangle(
        &mut self,
        ledge: &ConvexLedge,
        triangle: [usize; 3],
        transform: &mut impl FnMut(Vec3) -> Vec3,
    ) {
        let Some(points) = triangle_points(ledge, triangle) else {
            return;
        };
        let positions = points.map(transform);
        if !positions.iter().all(|position| position.is_finite()) {
            return;
        }
        let normal = (positions[1] - positions[0])
            .cross(positions[2] - positions[0])
            .normalize_or_zero();
        if !normal.is_finite() || normal.dot(normal) <= f32::EPSILON {
            return;
        }
        let Some(base) = u32::try_from(self.vertices.len()).ok() else {
            return;
        };
        self.vertices
            .extend(positions.into_iter().map(|position| ModelVertex {
                position,
                normal,
                uv: [0.0; 2],
                lightmap_uv: [0.0; 2],
                color: PHY_DEBUG_VERTEX_COLOR,
                blend_alpha: 0.0,
            }));
        self.indices
            .extend_from_slice(&[base, base.saturating_add(1), base.saturating_add(2)]);
    }

    pub(in super::super) fn finish(self, material_index: usize) -> Option<MeshData> {
        (!self.indices.is_empty()).then_some(MeshData {
            vertices: self.vertices,
            indices: self.indices,
            material_index,
            bodygroup: 0,
            bodygroup_choice: 0,
        })
    }
}

pub(in super::super) fn triangle_points(
    ledge: &ConvexLedge,
    triangle: [usize; 3],
) -> Option<[Vec3; 3]> {
    Some([
        Vec3::from(*ledge.vertices.get(triangle[0])?),
        Vec3::from(*ledge.vertices.get(triangle[1])?),
        Vec3::from(*ledge.vertices.get(triangle[2])?),
    ])
}

pub(in super::super) fn merge_phy_skip_reasons(
    target: &mut BTreeMap<SkipReason, usize>,
    parse_stats: &ReadStats,
) {
    for (reason, count) in &parse_stats.skip_reasons {
        *target.entry(*reason).or_default() += count;
    }
}
