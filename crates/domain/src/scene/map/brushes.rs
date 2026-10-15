//! Compiled brush models are local to their owning entity. VBSP subtracts the
//! entity origin from planes and adjusts texinfo before emitting the model;
//! even a zero-origin model therefore follows this same convention.
use super::{MapBounds, MapBsp, ModelIndex, parse_entity_i32, parse_entity_vec3};
use crate::math::Vec3;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct BrushTransform {
    pub(super) origin: Vec3,
    pub(super) angles: Vec3,
}

impl BrushTransform {
    pub(super) fn point(self, point: Vec3) -> Vec3 {
        self.vector(point) + self.origin
    }

    pub(super) fn vector(self, vector: Vec3) -> Vec3 {
        vector.rotate_source(self.angles)
    }

    pub(super) fn bounds(self, bounds: MapBounds) -> Option<MapBounds> {
        super::bounds_from_points_iter(
            super::walk::prop_bounds_corners(bounds)
                .into_iter()
                .map(|p| self.point(p)),
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct BrushInstance {
    pub(super) entity_index: Option<usize>,
    pub(super) model_index: ModelIndex,
    pub(super) transform: BrushTransform,
    pub(super) visible: bool,
    pub(super) solid: bool,
    pub(super) use_vphysics: bool,
}

impl BrushInstance {
    pub(super) fn attribution(
        self,
        bsp: &MapBsp,
        locator: &super::MapLeafLocator,
        sky: &super::SkyboxPartition,
    ) -> (super::GeometryPartition, super::MapFaceVisibility) {
        let model = &bsp.models[self.model_index.get()];
        let bounds = self.transform.bounds(MapBounds {
            min: Vec3::from(model.mins),
            max: Vec3::from(model.maxs),
        });
        let Some(bounds) = bounds else {
            return (
                super::GeometryPartition::Visible,
                super::MapFaceVisibility::always(),
            );
        };
        let visibility = locator.clusters_for_aabb(bounds.min, bounds.max, bsp.cluster_count());
        let partition = match &visibility {
            super::MapPropVisibility::Clusters(clusters) if !clusters.is_empty() => {
                if clusters.iter().all(|cluster| {
                    i16::try_from(*cluster).ok().is_some_and(|cluster| {
                        sky.cluster_partition(cluster) == super::GeometryPartition::Skybox
                    })
                }) {
                    super::GeometryPartition::Skybox
                } else {
                    super::GeometryPartition::Visible
                }
            }
            _ => sky.point_partition(bsp, (bounds.min + bounds.max) * 0.5),
        };
        let visibility = match visibility {
            super::MapPropVisibility::Always => super::MapFaceVisibility::always(),
            super::MapPropVisibility::Clusters(clusters) => super::MapFaceVisibility {
                always_visible: false,
                clusters,
            },
        };
        (partition, visibility)
    }
}

pub(super) fn brush_instances(bsp: &MapBsp) -> Vec<BrushInstance> {
    let mut instances = Vec::new();
    if !bsp.models.is_empty() {
        instances.push(BrushInstance {
            entity_index: None,
            model_index: ModelIndex::new(0),
            transform: BrushTransform::default(),
            visible: true,
            solid: true,
            use_vphysics: false,
        });
    }
    for (entity_index, entity) in bsp.entities.iter().enumerate() {
        let Some(index) = entity.prop("model").and_then(parse_bmodel_index) else {
            continue;
        };
        let Some(model) = bsp.models.get(index.get()) else {
            log::warn!("bsp brush entity references missing model {index}");
            continue;
        };
        let origin = entity
            .prop("origin")
            .and_then(parse_entity_vec3)
            .unwrap_or_else(|| Vec3::from(model.origin));
        let mut angles = entity
            .prop("angles")
            .and_then(parse_entity_vec3)
            .unwrap_or(Vec3::ZERO);
        let class = entity.prop("classname").unwrap_or("");
        // Legacy wall classes discard map angles in their native Spawn path.
        if matches!(
            class,
            "func_illusionary" | "func_wall" | "func_wall_toggle" | "func_conveyor"
        ) {
            angles = Vec3::ZERO;
        }
        let flags = entity
            .prop("spawnflags")
            .and_then(parse_entity_i32)
            .unwrap_or(0);
        let disabled = entity
            .prop("startdisabled")
            .and_then(parse_entity_i32)
            .unwrap_or(0)
            != 0;
        let utility = class.starts_with("trigger_")
            || matches!(
                class,
                "func_areaportal"
                    | "func_areaportalwindow"
                    | "func_occluder"
                    | "func_clip_vphysics"
                    | "func_vehicleclip"
                    | "func_precipitation"
                    | "func_viscluster"
                    | "func_ladder"
                    | "func_useableladder"
            );
        let (visible, solid) = match class {
            "func_brush" => {
                let solidity = entity
                    .prop("solidity")
                    .and_then(parse_entity_i32)
                    .unwrap_or(0);
                (!disabled, solidity != 1 && (!disabled || solidity == 2))
            }
            "func_illusionary" => (true, false),
            "func_door_rotating" if flags & 8 != 0 => (true, false),
            "func_wall_toggle" => (flags & 1 == 0, flags & 1 == 0),
            _ if utility => (false, false),
            _ => (true, true),
        };
        // SetParent computes a parent-relative pose from the child's world
        // pose. A map's parentname does NOT make its origin parent-relative.
        instances.push(BrushInstance {
            entity_index: Some(entity_index),
            model_index: index,
            transform: BrushTransform { origin, angles },
            visible,
            solid,
            use_vphysics: entity.prop("solidbsp").and_then(parse_entity_i32) != Some(1)
                && matches!(
                    class,
                    "func_brush"
                        | "func_reflective_glass"
                        | "func_door"
                        | "func_door_rotating"
                        | "func_movelinear"
                        | "func_rotating"
                        | "func_physbox"
                        | "func_physbox_multiplayer"
                ),
        });
    }
    for index in 1..bsp.models.len() {
        if !instances
            .iter()
            .any(|instance| instance.model_index.get() == index)
        {
            log::debug!("bsp unowned brush model *{index} omitted");
        }
    }
    instances
}

pub(super) fn parse_bmodel_index(model: &str) -> Option<ModelIndex> {
    let index = model.strip_prefix('*')?.parse::<usize>().ok()?;
    (index > 0).then(|| ModelIndex::new(index))
}
