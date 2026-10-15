use super::materials::{
    resolve_detail_sprites, resolve_map_material_slots_parallel, resolve_map_overlays,
    resolve_skybox, resolve_sun_materials, sky_log_status,
};
use super::{
    MAP_FALLBACK_TEXTURE_DIMENSION, MAP_PROP_PLACEMENT_CAP, MAP_PROP_TRIANGLE_CAP,
    MAP_TEXTURE_DECODE_BUDGET_BYTES, MAP_TEXTURE_MAX_DIMENSION, MAP_TOO_LARGE_BYTES,
    PHY_DEBUG_TRIANGLE_CAP, PHY_DEBUG_VERTEX_COLOR, catch_asset_decode, entry_stem,
    info_preview_data, load_model_companions,
};
use crate::bridge::materials::{
    ContentSourceTier, DecodedTextureBudget, MaterialResolver, RenderMode, ResolvedPrimaryMaterial,
    ResolvedSoundReference, srgb_byte_to_linear,
};
use crate::media::preview_model::{
    DoorInstance, DoorSound, DoorSoundSourceTier, DoorSoundWave, DoorSounds, InfoReason, MapFog,
    MapPreview, MapSkyCamera, MapSpawn, MapStats, MaterialSlot, MeshData, ModelData, ModelStats,
    ModelVertex, PHY_DEBUG_MATERIAL_NAME, PreviewContent, PreviewData, PreviewLoadStage,
    PreviewRequest, RenderScene,
};
use gmpublished_domain::math::Vec3;
use gmpublished_domain::scene::map::{
    AmbientCube, ConvexHull, MapAmbientLighting, MapDoor, MapDoorGeometry, MapEnvironmentLighting,
    MapMeshClusterRanges, MapMeshIndexRange, MapMeshVisibility, MapPropVisibility, MapVisibility,
    MapWalkCollision, MapWalkPropModel, MapWalkPropModelPlacement, StaticPropPlacement,
};
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vformats::phy::{ConvexLedge, ReadStats, SkipReason};

mod build;
mod collision;
mod compiled_lighting;
mod cubemaps;
mod diagnostics;
mod lighting;
mod materials;
mod props;
mod runtime_lighting;
mod world_geometry;

use build::{PropBuildPhase, build_props};
pub(super) use collision::{
    PhyDebugMeshBuilder, enrich_walk_collision_with_prop_collision,
    phy_debug_mesh_from_loaded_prop_models,
};
use diagnostics::{
    MapPreviewStatuses, MapPreviewTimings, lightmap_status, log_map_preview_summary,
};
#[cfg(test)]
pub(super) use lighting::PropSunLighting;
pub(super) use lighting::{
    PropPlacementLighting, SelectedPropPlacement, StaticPropLightingInputs, prop_placement_lighting,
};
#[cfg(test)]
pub(super) use materials::unresolved_material_names_for_debug;
pub(super) use materials::{
    MaterialTable, PropMaterialResolveJob, format_mib, log_unresolved_materials,
    material_dimensions, normalize_map_uv, render_mode_log_suffix, texture_payload_log_suffix,
    water_fallback_log_suffix,
};
#[cfg(feature = "map-capture")]
pub(super) use props::load_prop_model_with_options;
pub(super) use props::{
    DoorBakeResult, LoadedPhy, LoadedPropModel, LoadedPropPhysics, PropBakeResult,
    PropBakeSkipStats, PropCollisionStats, PropModelAsset, bake_map_doors_with_prop_model_loader,
    bake_static_props_from_loaded_model_cache, load_prop_model, load_unique_prop_models_parallel,
    log_prop_door_material_resolution, parse_phy_bytes, pre_resolve_prop_materials,
    refresh_entity_prop_aabb_visibility, transform_prop_position,
};
#[cfg(test)]
pub(super) use props::{
    bake_prop_placement, bake_static_props, bake_static_props_with_loaded_model_cache,
    bake_static_props_with_loader, bake_static_props_with_loader_serial, transform_prop_normal,
};
pub(super) use world_geometry::{
    bounds_from_model_meshes, map_fog_to_preview, map_mesh_to_model_mesh,
    map_sky_camera_to_preview, map_spawn_to_preview,
};

pub(super) fn map_preview_data(
    request: &PreviewRequest,
    bsp_bytes: &[u8],
    gmod_dir: Option<std::path::PathBuf>,
    map_pool: Option<&rayon::ThreadPool>,
    emit_stage: &mut dyn FnMut(PreviewLoadStage),
) -> PreviewData {
    map_preview_data_with_prop_model_loader(
        request,
        bsp_bytes,
        gmod_dir,
        map_pool,
        emit_stage,
        &load_prop_model,
    )
}

pub(super) fn map_preview_data_with_prop_model_loader(
    request: &PreviewRequest,
    bsp_bytes: &[u8],
    gmod_dir: Option<std::path::PathBuf>,
    map_pool: Option<&rayon::ThreadPool>,
    emit_stage: &mut dyn FnMut(PreviewLoadStage),
    load_model: &(impl Fn(&str, &MaterialResolver) -> Option<LoadedPropModel> + Sync),
) -> PreviewData {
    if !request.bypass_size_limits && bsp_bytes.len() > MAP_TOO_LARGE_BYTES {
        return info_preview_data(request, InfoReason::TooLarge);
    }

    let bsp_started = Instant::now();
    let map = match map_pool.map_or_else(
        || gmpublished_domain::scene::map::load_map(bsp_bytes),
        |pool| gmpublished_domain::scene::map::load_map_with_pool(bsp_bytes, pool),
    ) {
        Ok(map) => map,
        Err(error) => {
            log::debug!("file preview bsp decode failed: {error}");
            return info_preview_data(request, InfoReason::DecodeFailed);
        }
    };
    let bsp_timing = bsp_started.elapsed();

    let gmpublished_domain::scene::map::MapData {
        meshes: map_meshes,
        skybox_meshes: map_skybox_meshes,
        material_names,
        mut static_props,
        mut skybox_static_props,
        doors: map_doors,
        detail_material_name,
        detail_sprites: map_detail_sprites,
        skybox_detail_sprites: map_skybox_detail_sprites,
        overlays: map_overlays,
        skybox_overlays: map_skybox_overlays,
        ambient,
        world_lighting,
        lighting_collision,
        radiosity_surfaces,
        environment_lighting,
        player_start,
        bounds_min,
        bounds_max,
        stats: raw_stats,
        skybox_partition,
        skybox_completion_bounds: _,
        visibility,
        walk_collision,
        pakfile,
        water_volumes,
        lightmap,
        skyname,
        sky_paint,
        suns,
        cubemap_probes,
        fog,
        sky_camera,
    } = map;
    let fog = fog.map(map_fog_to_preview);
    let sky_camera = sky_camera.map(map_sky_camera_to_preview);
    let resolver =
        MaterialResolver::with_pakfile_source(request.archive.clone(), gmod_dir, pakfile);
    let texture_budget = Arc::new(DecodedTextureBudget::new(MAP_TEXTURE_DECODE_BUDGET_BYTES));
    let material_resolver = resolver
        .with_decoded_texture_max_dimension(MAP_TEXTURE_MAX_DIMENSION)
        .with_decoded_texture_budget(Arc::clone(&texture_budget));
    emit_stage(PreviewLoadStage::ResolvingMaterials);
    let materials_started = Instant::now();
    let mut table = resolve_map_material_slots_parallel(&material_names, &material_resolver);
    let surface_materials = radiosity_surfaces
        .as_ref()
        .map_or_else(Vec::new, |surfaces| {
            parallel_collect(
                &(0..surfaces.texdatas.len()).collect::<Vec<_>>(),
                |&index| material_resolver.resolve_surface_lighting(surfaces.material_name(index)?),
            )
        });
    let model_lighting_inputs = Arc::new(crate::media::preview_model::ModelLightingSceneInputs {
        ambient,
        world_lighting,
        visibility: visibility.clone(),
        collision: lighting_collision,
        radiosity_surfaces,
        surface_materials,
    });
    // A compiled fog volume can name an underside material even when no
    // visible face references it. Resolve that dependency independently.
    let below_names = table
        .slots()
        .iter()
        .filter_map(|slot| {
            slot.shader
                .water()
                .and_then(|water| water.below_material.clone())
        })
        .filter(|name| !table.contains(name))
        .collect::<BTreeSet<_>>();
    let below = resolve_map_material_slots_parallel(
        &below_names.into_iter().collect::<Vec<_>>(),
        &material_resolver,
    );
    for slot in below.into_slots() {
        table.push(&slot.name.clone(), slot);
    }
    let (detail_sprites, map_skybox_detail_sprites) = resolve_detail_sprites(
        &map_detail_sprites,
        &map_skybox_detail_sprites,
        &detail_material_name,
        &material_resolver,
        &mut table,
    );
    let overlay_bake = resolve_map_overlays(&map_overlays, &material_resolver, &mut table);
    let skybox_overlay_bake =
        resolve_map_overlays(&map_skybox_overlays, &material_resolver, &mut table);
    let materials_timing = materials_started.elapsed();
    let mut entity_meshes = map_meshes
        .iter()
        .map(|mesh| mesh.entity)
        .collect::<Vec<_>>();
    let mut skybox_entity_meshes = map_skybox_meshes
        .iter()
        .map(|mesh| mesh.entity)
        .collect::<Vec<_>>();
    let mut mesh_visibility = map_meshes
        .iter()
        .map(|mesh| mesh.visibility.clone())
        .collect::<Vec<_>>();
    let mut meshes = map_meshes
        .iter()
        .map(|mesh| map_mesh_to_model_mesh(mesh, table.slots()))
        .collect::<Vec<_>>();
    let mut map_skybox_meshes = map_skybox_meshes
        .iter()
        .map(|mesh| map_mesh_to_model_mesh(mesh, table.slots()))
        .collect::<Vec<_>>();
    emit_stage(PreviewLoadStage::PlacingProps);
    let PropBuildPhase {
        loaded_models: loaded_model_cache,
        world: prop_bake,
        skybox: skybox_prop_bake,
        doors: door_bake,
        walk_collision,
        elapsed: props_timing,
    } = build_props(
        &mut static_props,
        &mut skybox_static_props,
        &map_doors,
        &raw_stats,
        visibility.as_ref(),
        walk_collision,
        &model_lighting_inputs.ambient,
        cubemaps::PropEnvironmentMapInputs {
            map_name: request
                .entry_path
                .rsplit(['/', '\\'])
                .next()
                .and_then(entry_stem)
                .unwrap_or_default(),
            probes: &cubemap_probes,
        },
        environment_lighting.as_ref(),
        lightmap.as_ref().map(|atlas| atlas.source),
        &material_resolver,
        &mut table,
        load_model,
    );
    let placed_prop_count = prop_bake
        .placed_count
        .saturating_add(skybox_prop_bake.placed_count);
    let skipped_prop_count = prop_bake
        .skipped_count()
        .saturating_add(skybox_prop_bake.skipped_count());
    let prop_skip_stats = prop_bake.skip_stats + skybox_prop_bake.skip_stats;
    let prop_bake_timing = prop_bake.timing + skybox_prop_bake.timing;
    let prop_mesh_bytes = prop_bake
        .mesh_bytes()
        .saturating_add(skybox_prop_bake.mesh_bytes());
    let sky_context_offset =
        u32::try_from(prop_bake.model_lighting.len()).expect("bounded model contexts");
    let mut skybox_prop_bake = skybox_prop_bake;
    for mesh in &mut skybox_prop_bake.meshes {
        for vertex in &mut mesh.vertices {
            if vertex.lighting_context != 0 {
                vertex.lighting_context += sky_context_offset;
            }
        }
    }
    let mut model_lighting = prop_bake.model_lighting;
    model_lighting.extend(skybox_prop_bake.model_lighting);
    model_lighting_inputs.seed_entities(&mut model_lighting);
    model_lighting_inputs.seed_static(&mut model_lighting);
    entity_meshes.extend(prop_bake.entity_meshes);
    skybox_entity_meshes.extend(skybox_prop_bake.entity_meshes);
    meshes.extend(prop_bake.meshes);
    mesh_visibility.extend(prop_bake.mesh_visibility);
    map_skybox_meshes.extend(skybox_prop_bake.meshes);
    runtime_lighting::activate(&mut meshes, &model_lighting, table.slots());
    runtime_lighting::activate(&mut map_skybox_meshes, &model_lighting, table.slots());
    emit_stage(PreviewLoadStage::BakingLightmap);
    let lightmap_started = Instant::now();
    let lightmap_status = lightmap_status(lightmap.as_ref());
    let lightmap_timing = lightmap_started.elapsed();
    let skybox = skyname.as_deref().and_then(|skyname| {
        resolve_skybox(
            skyname,
            &resolver.with_bc_textures_disabled(),
            sky_paint.as_ref(),
        )
    });
    let sky_status = sky_log_status(skyname.as_deref());
    let sun_material_slots = resolve_sun_materials(&suns, &material_resolver, &mut table);
    let vertex_count = meshes
        .iter()
        .chain(&map_skybox_meshes)
        .map(|mesh| mesh.vertices.len())
        .sum::<usize>();
    let triangle_count = meshes
        .iter()
        .chain(&map_skybox_meshes)
        .map(|mesh| mesh.indices.len() / 3)
        .sum::<usize>();
    let material_slot_count = table.len();
    let resolved_material_count = table.resolved_count();
    let water_material_count = table.water_material_count();
    let material_count = u32::try_from(material_slot_count).unwrap_or(u32::MAX);
    let (bounds_min, bounds_max) =
        bounds_from_model_meshes(&meshes).unwrap_or((bounds_min, bounds_max));
    let phy_debug_meshes =
        phy_debug_mesh_from_loaded_prop_models(&static_props, &loaded_model_cache, table.len())
            .into_iter()
            .collect::<Vec<_>>();
    let detail_sprite_count = u32::try_from(detail_sprites.len()).unwrap_or(u32::MAX);
    let map_skybox_detail_sprite_count =
        u32::try_from(map_skybox_detail_sprites.len()).unwrap_or(u32::MAX);
    let overlay_count = u32::try_from(overlay_bake.overlays.len()).unwrap_or(u32::MAX);
    let map_skybox_overlay_count =
        u32::try_from(skybox_overlay_bake.overlays.len()).unwrap_or(u32::MAX);
    let skipped_overlay_count = overlay_bake
        .skipped_count
        .saturating_add(skybox_overlay_bake.skipped_count);
    if texture_budget.rejected_textures() > 0 {
        log::debug!(
            "map preview texture budget {} MiB exhausted: dropped {} texture decodes",
            format_mib(MAP_TEXTURE_DECODE_BUDGET_BYTES),
            texture_budget.rejected_textures()
        );
    }
    log_unresolved_materials(table.slots());
    let water_status = water_fallback_log_suffix(water_material_count);
    let texture_mib = format_mib(texture_budget.decoded_bytes());
    let texture_payloads = texture_payload_log_suffix(table.slots());
    let render_mode_status = render_mode_log_suffix(table.slots());
    if !phy_debug_meshes.is_empty() {
        table.push_unindexed(MaterialSlot {
            shader: Default::default(),
            name: PHY_DEBUG_MATERIAL_NAME.to_owned(),
            texture: None,
            texture2: None,
            force_opaque: false,
            render_mode: RenderMode::Translucent,
        });
    }
    let scene = Arc::new(MapPreview {
        model_lighting,
        model_lighting_inputs: Some(model_lighting_inputs),
        water_volumes,
        scene: Arc::new(RenderScene {
            stats: ModelStats {
                bone_count: 0,
                sequence_count: 0,
                vertex_count: u32::try_from(vertex_count).unwrap_or(u32::MAX),
                triangle_count: u32::try_from(triangle_count).unwrap_or(u32::MAX),
                mesh_count: u32::try_from(meshes.len().saturating_add(map_skybox_meshes.len()))
                    .unwrap_or(u32::MAX),
                material_count,
                resolved_material_count,
            },
            meshes,
            materials: table.into_slots(),
            phy_debug_meshes,
            bounds_min,
            bounds_max,
        }),
        entity_meshes,
        skybox_entity_meshes,
        mesh_visibility,
        map_skybox_meshes,
        lightmap,
        skybox,
        suns,
        cubemap_probes,
        sun_material_slots,
        detail_sprites,
        map_skybox_detail_sprites,
        overlays: overlay_bake.overlays,
        map_skybox_overlays: skybox_overlay_bake.overlays,
        doors: door_bake.doors,
        visibility,
        walk_collision,
    });
    let stats = MapStats {
        face_count: raw_stats.face_count,
        displacement_count: raw_stats.displacement_count,
        entity_count: raw_stats.entity_count,
        material_count,
        resolved_material_count,
        static_prop_count: raw_stats.static_prop_count,
        cluster_count: raw_stats.cluster_count,
        placed_prop_count,
        skipped_prop_count,
        detail_sprite_count,
        overlay_count,
        skybox_face_count: skybox_partition.face_count,
        skybox_prop_count: skybox_partition.static_prop_count,
        skybox_detail_sprite_count: map_skybox_detail_sprite_count,
        skybox_overlay_count: map_skybox_overlay_count,
        version: raw_stats.version,
    };
    log_map_preview_summary(
        &request.entry_path,
        &stats,
        &MapPreviewStatuses {
            water: &water_status,
            render_mode: &render_mode_status,
            texture_mib: &texture_mib,
            texture_payloads: &texture_payloads,
            lightmap: &lightmap_status,
            sky: &sky_status,
        },
        &prop_skip_stats,
        prop_mesh_bytes,
        skipped_overlay_count,
        &MapPreviewTimings {
            bsp: bsp_timing,
            materials: materials_timing,
            props: props_timing,
            prop_load: prop_bake_timing.load,
            prop_bake: prop_bake_timing.bake,
            lightmap: lightmap_timing,
        },
    );

    PreviewData::from_request(
        request,
        PreviewContent::Map {
            scene,
            stats,
            fog,
            sky_camera,
            spawn: player_start.map(map_spawn_to_preview),
        },
    )
}

pub(super) fn parallel_collect<T, R, F>(items: &[T], work: F) -> Vec<R>
where
    T: Sync + Send,
    R: Send,
    F: Fn(&T) -> R + Sync + Send,
{
    if items.len() <= 1 {
        return items.iter().map(work).collect();
    }

    items.par_iter().map(&work).collect()
}

pub(super) fn duration_ms(duration: Duration) -> u128 {
    duration.as_millis()
}
