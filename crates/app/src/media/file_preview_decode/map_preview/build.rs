use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bridge::materials::MaterialResolver;
use gmpublished_domain::scene::map::{
    MapAmbientLighting, MapDoor, MapEnvironmentLighting, MapStatsRaw, MapVisibility,
    MapWalkCollision, StaticPropPlacement,
};

use super::props::{
    bake_map_doors_from_loaded_model_cache, load_prop_model_paths_parallel,
    unique_prop_model_paths_from,
};
use super::{
    DoorBakeResult, LoadedPropModel, MaterialTable, PropBakeResult, StaticPropLightingInputs,
    bake_static_props_from_loaded_model_cache, enrich_walk_collision_with_prop_collision,
    format_mib, log_prop_door_material_resolution, refresh_entity_prop_aabb_visibility,
};

#[cfg(test)]
mod tests;

type PropModelCache = HashMap<String, Option<Arc<LoadedPropModel>>>;

fn load_role_models(
    roles: [Vec<String>; 3],
    resolver: &MaterialResolver,
    load_model: &(impl Fn(&str, &MaterialResolver) -> Option<LoadedPropModel> + Sync),
) -> [PropModelCache; 3] {
    let mut seen = std::collections::HashSet::new();
    let paths = roles
        .iter()
        .flatten()
        .filter(|path| seen.insert(path.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let models = load_prop_model_paths_parallel(&paths, resolver, load_model);
    roles.map(|paths| {
        paths
            .into_iter()
            .map(|path| {
                let model = models.get(&path).expect("discovered model path").clone();
                (path, model)
            })
            .collect()
    })
}

pub(super) struct PropBuildPhase {
    pub(super) geometry: crate::media::preview_model::PropGeometrySummary,
    pub(super) loaded_models: HashMap<String, Option<Arc<LoadedPropModel>>>,
    pub(super) world: PropBakeResult,
    pub(super) skybox: PropBakeResult,
    pub(super) doors: DoorBakeResult,
    pub(super) walk_collision: Option<MapWalkCollision>,
    pub(super) elapsed: Duration,
}

#[expect(
    clippy::too_many_arguments,
    reason = "this phase boundary mirrors the independent BSP inputs it consumes"
)]
pub(super) fn build_props(
    static_props: &mut [StaticPropPlacement],
    skybox_static_props: &mut [StaticPropPlacement],
    map_doors: &[MapDoor],
    raw_stats: &MapStatsRaw,
    visibility: Option<&MapVisibility>,
    walk_collision: Option<MapWalkCollision>,
    ambient: &MapAmbientLighting,
    environment_maps: super::cubemaps::PropEnvironmentMapInputs<'_>,
    environment_lighting: Option<&MapEnvironmentLighting>,
    lightmap_source: Option<gmpublished_domain::scene::map::LightmapSource>,
    resolver: &MaterialResolver,
    table: &mut MaterialTable,
    load_model: &(impl Fn(&str, &MaterialResolver) -> Option<LoadedPropModel> + Sync),
) -> PropBuildPhase {
    let started = Instant::now();
    let load_started = Instant::now();
    let roles = [
        unique_prop_model_paths_from(static_props.iter()),
        unique_prop_model_paths_from(skybox_static_props.iter()),
        unique_prop_model_paths_from(map_doors.iter().filter_map(|door| match &door.geometry {
            gmpublished_domain::scene::map::MapDoorGeometry::Prop { placement } => Some(placement),
            gmpublished_domain::scene::map::MapDoorGeometry::Brush { .. } => None,
        })),
    ];
    let [loaded_models, skybox_loaded_models, door_loaded_models] =
        load_role_models(roles, resolver, load_model);
    refresh_entity_prop_aabb_visibility(
        static_props,
        usize::try_from(raw_stats.world_static_prop_count)
            .expect("u32 map prop counts fit usize on supported targets"),
        usize::try_from(raw_stats.world_entity_prop_count)
            .expect("u32 map prop counts fit usize on supported targets"),
        visibility,
        &loaded_models,
    );
    refresh_entity_prop_aabb_visibility(
        skybox_static_props,
        usize::try_from(raw_stats.skybox_static_prop_count)
            .expect("u32 map prop counts fit usize on supported targets"),
        usize::try_from(raw_stats.skybox_entity_prop_count)
            .expect("u32 map prop counts fit usize on supported targets"),
        visibility,
        &skybox_loaded_models,
    );
    let (walk_collision, collision_stats) =
        enrich_walk_collision_with_prop_collision(walk_collision, static_props, &loaded_models);
    log::debug!(
        "map preview prop collision: solid placements {}, collidable {}, parsed models {}, hulls {}, memory {} bytes ({} MiB), skipped not-solid {}, model-load {}, missing-phy {}, unparseable-phy {}, phy reasons {:?}",
        collision_stats.solid_placements,
        collision_stats.collidable_placements,
        collision_stats.parsed_models,
        collision_stats.prop_hulls,
        collision_stats.memory_bytes,
        format_mib(collision_stats.memory_bytes),
        collision_stats.skipped_not_solid,
        collision_stats.skipped_model_load,
        collision_stats.skipped_missing_phy,
        collision_stats.skipped_unparseable_phy,
        collision_stats.skip_reasons
    );
    let lighting = StaticPropLightingInputs {
        ambient,
        environment_maps: Some(environment_maps),
        environment_lighting,
        lightmap_source,
        walk_collision: walk_collision.as_ref(),
    };
    let world = bake_static_props_from_loaded_model_cache(
        static_props,
        resolver,
        table,
        &loaded_models,
        lighting,
        load_started,
    );
    let skybox = bake_static_props_from_loaded_model_cache(
        skybox_static_props,
        resolver,
        table,
        &skybox_loaded_models,
        lighting,
        load_started,
    );
    let doors = bake_map_doors_from_loaded_model_cache(
        map_doors,
        resolver,
        table,
        lighting,
        &door_loaded_models,
    );
    log_prop_door_material_resolution(&doors.prop_material_resolutions);
    log::debug!(
        "map compiled lighting: world {:?}, skybox {:?}",
        world.lighting_stats,
        skybox.lighting_stats
    );

    PropBuildPhase {
        geometry: crate::media::preview_model::PropGeometrySummary {
            world: world.geometry.clone(),
            skybox: skybox.geometry.clone(),
            doors: doors.geometry.clone(),
        },
        loaded_models,
        world,
        skybox,
        doors,
        walk_collision,
        elapsed: started.elapsed(),
    }
}
