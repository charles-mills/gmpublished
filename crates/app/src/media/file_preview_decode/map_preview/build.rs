//! Cohesive build phases used by the map-preview orchestrator.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bridge::materials::MaterialResolver;
use gmpublished_backend::scene::map::{
    MapAmbientLighting, MapDoor, MapEnvironmentLighting, MapStatsRaw, MapVisibility,
    MapWalkCollision, StaticPropPlacement,
};

use super::{
    DoorBakeResult, LoadedPropModel, MaterialTable, PropBakeResult, StaticPropLightingInputs,
    bake_map_doors_with_prop_model_loader, bake_static_props_from_loaded_model_cache,
    enrich_walk_collision_with_prop_collision, format_mib, load_unique_prop_models_parallel,
    log_prop_door_material_resolution, pre_resolve_prop_materials,
    refresh_entity_prop_aabb_visibility,
};

pub(super) struct PropBuildPhase {
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
    environment_lighting: Option<&MapEnvironmentLighting>,
    resolver: &MaterialResolver,
    table: &mut MaterialTable,
    load_model: &(impl Fn(&str, &MaterialResolver) -> Option<LoadedPropModel> + Sync),
) -> PropBuildPhase {
    let started = Instant::now();
    let load_started = Instant::now();
    let loaded_models = load_unique_prop_models_parallel(static_props, resolver, load_model);
    let skybox_loaded_models =
        load_unique_prop_models_parallel(skybox_static_props, resolver, load_model);
    refresh_entity_prop_aabb_visibility(
        static_props,
        usize::try_from(raw_stats.world_static_prop_count).unwrap_or(usize::MAX),
        usize::try_from(raw_stats.world_entity_prop_count).unwrap_or(0),
        visibility,
        &loaded_models,
    );
    refresh_entity_prop_aabb_visibility(
        skybox_static_props,
        usize::try_from(raw_stats.skybox_static_prop_count).unwrap_or(usize::MAX),
        usize::try_from(raw_stats.skybox_entity_prop_count).unwrap_or(0),
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
        environment_lighting,
        walk_collision: walk_collision.as_ref(),
    };
    let resolved = pre_resolve_prop_materials(static_props, &loaded_models, resolver);
    let world = bake_static_props_from_loaded_model_cache(
        static_props,
        resolver,
        table,
        &loaded_models,
        Some(&resolved),
        lighting,
        load_started,
    );
    let skybox_resolved =
        pre_resolve_prop_materials(skybox_static_props, &skybox_loaded_models, resolver);
    let skybox = bake_static_props_from_loaded_model_cache(
        skybox_static_props,
        resolver,
        table,
        &skybox_loaded_models,
        Some(&skybox_resolved),
        lighting,
        load_started,
    );
    let doors =
        bake_map_doors_with_prop_model_loader(map_doors, resolver, table, lighting, load_model);
    log_prop_door_material_resolution(&doors.prop_material_resolutions);

    PropBuildPhase {
        loaded_models,
        world,
        skybox,
        doors,
        walk_collision,
        elapsed: started.elapsed(),
    }
}
