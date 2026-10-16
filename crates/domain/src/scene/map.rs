//! BSP preview geometry, props, lighting, visibility, and walk collision over
//! [`vformats::bsp`]. Leaf traversal, invalid-vertex tolerance, and displacement
//! corner rotation are ported from vbsp (MIT, © icewind1991).

use crate::math::Vec3;
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
};

use thiserror::Error;
use vformats::{
    Limits,
    bsp::{
        self, Brush, BrushSide, BspModel, ColorRgbExp, DetailProp, DetailProps, DispInfo, DispVert,
        Face, Leaf, LeafAmbientIndex, LeafAmbientSample, Node, Overlay, Plane, StaticProp,
        StaticProps, TexData, TexInfo, Visibility, ZipReader, contents_flags, texture_flags,
    },
    keyvalues::KvValue,
};

use crate::scene::QAngle;

mod asset;
pub use asset::{MapAsset, decode_map, decode_map_with_pool};
mod brushes;
mod cubemaps;
mod data;
mod decals;
pub use decals::{
    MapDecal, MapDecalDimensions, MapDecalPermanence, MapDecalPosition, MapSurfaceProjector,
};
mod doors;
mod entities;
mod frames;
mod lighting;
mod lightmap_layout;
pub use lightmap_layout::{
    SourceLightmapAllocation, SourceLightmapLayout, SourceLightmapLayoutError,
    SourceLightmapMaterial, SourceLightmapPage, SourceLightmapPageProfile,
    SourceLightmapRequirement, SourceLightmapSurface,
};
mod lighting_trace;
mod lumps;
mod mesh;
mod overlays;
mod pakfile;
mod paths;
mod physics;
mod placements;
mod projection;
mod radiosity;
mod sky;
mod spatial;
mod visibility;
mod walk;
mod water;
mod world_lights;
pub use water::{MapWaterBody, MapWaterSample, MapWaterVolumes};
use world_lights::map_world_lighting;
pub use world_lights::{MapLightStyleEntity, MapWorldLight, MapWorldLightSet, MapWorldLighting};

pub use cubemaps::MapCubemapProbe;
pub use data::{
    MapBounds, MapData, MapDisplacementProjection, MapLightmapStats, MapMesh, MapMeshClusterRanges,
    MapMeshIndexRange, MapMeshSurfaceOwner, MapMeshVisibility, MapPropLeafMembership, MapPropSolid,
    MapPropVisibility, MapReceiverMesh, MapSkyboxPartitionStats, MapStatsRaw, MapSurfaceOwner,
    MapSurfaceOwnerId, MapSurfaceTextureProjection, MapVertex, MapVisibilityBucket,
    StaticPropPlacement,
};
#[cfg(test)]
use doors::linear_door_distance;
pub use doors::{
    MapDoor, MapDoorClass, MapDoorGeometry, MapDoorMotion, MapDoorOpenDirection, MapDoorSounds,
};
use doors::{PendingDoorBuild, PendingMapDoor, extract_pending_door_meshes, pending_map_doors};
pub use entities::{MapFog, MapPlayerStart, MapSkyCamera};
use entities::{
    info_player_start, map_fog, map_sky_camera, parse_entity_bool, parse_entity_float,
    parse_entity_i32, parse_entity_vec3, worldspawn_detail_material_name, worldspawn_skyname,
};
pub use lighting::{
    AmbientCube, AmbientLightSource, LightmapAtlas, LightmapSource, LightmapUpdate,
    MapAmbientLighting, MapAmbientSample, MapEnvironmentLighting, MapSunLighting,
};
#[cfg(test)]
use lighting::{
    AmbientSampleRange, brush_lightmap_uv_from_transforms, decode_ambient_sample_linear,
    decode_light_sample, decode_light_sample_linear, pack_lightmap_blocks,
};
use lighting::{
    PendingLightmapBlock, bake_lightmap_atlas, brush_lightmap_uv, displacement_lightmap_uv,
    extract_face_lightmap, map_ambient_lighting, map_environment_lighting,
    selected_lightmap_samples,
};
pub use lighting_trace::{MASK_OPAQUE, MapLightingCollision, MapLightingTrace};
pub use lumps::{BrushIndex, ModelIndex};
use lumps::{
    MapBsp, MapEntity, MapLeaf, MapNode, MapPlane, NodeChild, brush_indices_for_model, bsp_version,
    walk_to_leaf,
};
use mesh::{
    BuildMesh, BuildMeshes, FaceAppendContext, GeometryPartition, append_face,
    displacement_vertices, texture_coord,
};
#[cfg(test)]
use mesh::{
    DisplacementGridVertex, displacement_blend_alpha, fan_indices, tessellate_displacement_grid,
};
use overlays::partitioned_map_overlays;
pub use overlays::{MapOverlay, MapSurfaceMarkSource};
#[cfg(test)]
use overlays::{OverlayBasis, overlay_quad_positions};
use pakfile::pakfile_error;
pub use pakfile::{MAX_PAKFILE_ENTRY_BYTES, MapPakFile, MapPakFileEntry};
#[cfg(test)]
use pakfile::{is_pakfile_retained_entry, normalize_pakfile_path};
use paths::{
    is_preview_material_visible, normalize_entity_prop_model_path, normalize_material_name,
    normalize_skyname, normalize_source_path, normalize_static_prop_model_path,
};
#[cfg(test)]
use placements::parse_entity_prop_model_scale;
pub use placements::{MapDetailSprite, MapLightingOriginReference};
use placements::{
    partitioned_detail_sprite_placements, partitioned_entity_prop_placements,
    partitioned_static_prop_placements,
};
pub use projection::{MapSurfacePlane, MapSurfaceReceiver, MapSurfaceVertex};
pub use radiosity::{
    MapRadiosityDisplacement, MapRadiosityHit, MapRadiosityMaterialFlags, MapRadiosityStyles,
    MapRadiositySurfaces, MapRadiosityTraceError,
};
pub use sky::{MapSkyPaint, MapSun};
use sky::{map_sky_paint, map_suns};
pub use vformats::bsp::OverlayFade as MapOverlayFade;
use visibility::{
    FaceAttributions, MapFaceVisibility, PROP_AABB_MAX_DEPTH, PROP_AABB_MAX_EXTENT,
    PROP_AABB_MAX_LEAVES, SkyboxPartition, cluster_in_range, model_face_range,
    point_visibility_bucket,
};
pub use visibility::{
    MapBrushModelRendering, MapBrushSurfaceGroup, MapDecalProjection, MapDisplacementRendering,
    MapVisibility, MapWorldMaterialGroup, MapWorldSurfaceGroup, MapWorldSurfaceRendering,
};
use walk::{
    BoundsBuilder, MapLeafLocator, SKYBOX_COMPLETION_AABB_EXPANSION,
    SKYBOX_COMPLETION_MAX_WORLD_VOLUME_FRACTION, bounds_contains_point, bounds_from_points_iter,
    bounds_volume, bsp_world_bounds, expand_bounds,
};
pub use walk::{
    ConvexHull, MapBrushCollision, MapTrace, MapWalkCollision, MapWalkPropCollisionSource,
    MapWalkPropModel, MapWalkPropModelPlacement, WaterVolume,
};
#[cfg(test)]
use walk::{
    MapWalkBrush, MapWalkBrushPlane, MapWalkDisplacement, MapWalkPropCollision, MapWalkTriangle,
    TRACE_PLANE_EPSILON, brush_side_sky_from_texture_flags, local_prop_brush_from_hull,
    prop_brush_from_local, trace_brush_aabb, walk_brush_from_brush_planes, walk_brush_from_planes,
};

/// UI warning threshold, not a limit enforced by load_map.
pub const MAX_BSP_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum BspError {
    #[error("BSP decode failed: {0}")]
    Decode(#[from] bsp::BspError),
    #[error("BSP decode failed: {message}")]
    Malformed { message: &'static str },
    #[error("BSP pakfile decode failed: {message}")]
    Pakfile { message: String },
    #[error("unsupported BSP version {version}")]
    UnsupportedVersion { version: u32 },
    #[error("BSP {item} exceeds the supported limit")]
    TooLarge { item: &'static str },
    #[error("map material requirements have {actual} entries; expected {expected}")]
    MaterialRequirementCount { expected: usize, actual: usize },
}

pub fn load_map(bytes: &[u8]) -> Result<MapData, BspError> {
    load_map_with_skybox_partition(bytes, true)
}

pub fn load_map_with_pool(bytes: &[u8], pool: &rayon::ThreadPool) -> Result<MapData, BspError> {
    load_map_impl(bytes, true, Some(pool))
}

fn load_map_with_skybox_partition(
    bytes: &[u8],
    partition_skybox: bool,
) -> Result<MapData, BspError> {
    load_map_impl(bytes, partition_skybox, None)
}

fn load_map_impl(
    bytes: &[u8],
    partition_skybox: bool,
    pool: Option<&rayon::ThreadPool>,
) -> Result<MapData, BspError> {
    let asset = match pool {
        Some(pool) => decode_map_with_pool(bytes, pool)?,
        None => decode_map(bytes)?,
    };
    match pool {
        Some(pool) => pool.install(|| prepare_map(asset, partition_skybox, None)),
        None => prepare_map(asset, partition_skybox, None),
    }
}

fn prepare_map(
    asset: MapAsset,
    partition_skybox: bool,
    lightmap_requirements: Option<&[SourceLightmapRequirement]>,
) -> Result<MapData, BspError> {
    let MapAsset {
        bsp,
        version,
        pakfile,
    } = asset;
    let lighting_source = selected_lightmap_samples(&bsp).1;
    let mut stats = MapStatsRaw {
        face_count: count_to_u32(bsp.faces.len()),
        displacement_count: count_to_u32(bsp.displacements.len()),
        entity_count: count_to_u32(bsp.entities.len()),
        model_count: count_to_u32(bsp.models.len()),
        static_prop_count: count_to_u32(bsp.static_props_iter().count()),
        world_static_prop_count: 0,
        skybox_static_prop_count: 0,
        entity_prop_count: 0,
        world_entity_prop_count: 0,
        skybox_entity_prop_count: 0,
        cluster_count: bsp.cluster_count(),
        version,
        lightmap: MapLightmapStats::default(),
    };
    let player_start = info_player_start(&bsp.entities);
    let sky_camera = map_sky_camera(&bsp.entities);
    let suns = map_suns(&bsp.entities);
    let sky_paint = map_sky_paint(&bsp.entities, &suns);
    let mut skybox_partition = partition_skybox
        .then(|| SkyboxPartition::from_bsp(&bsp, player_start.as_ref(), sky_camera))
        .flatten()
        .unwrap_or_else(|| SkyboxPartition::inactive(sky_camera.is_some()));
    if partition_skybox {
        let initial_face_attributions = FaceAttributions::from_bsp(&bsp, &skybox_partition);
        skybox_partition.apply_completion_bounds(
            &bsp,
            &initial_face_attributions,
            player_start.as_ref(),
        );
    }
    let mut face_attributions = FaceAttributions::from_bsp(&bsp, &skybox_partition);
    let brush_instances = brushes::brush_instances(&bsp);
    let brush_locator = MapLeafLocator::from_bsp(&bsp);
    let pending_doors = pending_map_doors(&bsp);
    let door_entity_indices = pending_doors
        .iter()
        .filter_map(PendingMapDoor::brush_entity_index)
        .collect::<BTreeSet<_>>();

    let mut material_names = Vec::<String>::new();
    let mut material_indexes = HashMap::<String, usize>::new();
    let mut meshes = BuildMeshes::default();
    let mut door_builds = Vec::<PendingDoorBuild>::new();
    let mut lightmap_blocks = Vec::<PendingLightmapBlock>::new();
    let (lightmap_samples, lightmap_source) = selected_lightmap_samples(&bsp);
    let lightmap_sample_ends = lighting::lightmap_sample_ends(
        &bsp.faces,
        lightmap_samples.map_or(0, <[ColorRgbExp]>::len),
    );

    {
        let mut face_context = FaceAppendContext {
            entity: false,
            receiver_owner: MapSurfaceOwner::World,
            receiver_solid: true,
            transform: brushes::BrushTransform::default(),
            meshes: &mut meshes,
            material_names: &mut material_names,
            material_indexes: &mut material_indexes,
            lightmap_samples,
            lightmap_requirements,
            lightmap_sample_ends: &lightmap_sample_ends,
            lightmap_blocks: &mut lightmap_blocks,
            lightmap_stats: &mut stats.lightmap,
        };
        for instance in &brush_instances {
            if !instance.visible
                || instance
                    .entity_index
                    .is_some_and(|index| door_entity_indices.contains(&index))
            {
                continue;
            }
            let model = &bsp.models[instance.model_index.get()];
            face_context.transform = instance.transform;
            face_context.entity = instance.model_index.get() != 0;
            face_context.receiver_owner =
                instance
                    .entity_index
                    .map_or(MapSurfaceOwner::World, |entity_index| {
                        MapSurfaceOwner::Brush {
                            entity_index,
                            model_index: instance.model_index,
                            origin: instance.transform.origin,
                            angles: instance.transform.angles,
                        }
                    });
            face_context.receiver_solid = instance.solid;
            let attribution = (instance.model_index.get() != 0)
                .then(|| instance.attribution(&bsp, &brush_locator, &skybox_partition));
            let Some(face_range) = model_face_range(model, bsp.faces.len()) else {
                continue;
            };
            for face_index in face_range {
                let Some(face) = bsp.face(face_index) else {
                    continue;
                };
                let (partition, visibility) = attribution.clone().unwrap_or_else(|| {
                    (
                        face_attributions.partition(face_index),
                        face_attributions.visibility(face_index),
                    )
                });
                if attribution.is_some() {
                    face_attributions.partitions[face_index] = partition;
                    face_attributions.visibility[face_index] = visibility.clone();
                }
                append_face(
                    &bsp,
                    face,
                    face_index,
                    partition,
                    &visibility,
                    &mut face_context,
                )?;
            }
        }
        for pending in pending_doors {
            let instance = brush_instances
                .iter()
                .find(|instance| instance.entity_index == Some(pending.entity_index))
                .copied();
            door_builds.push(extract_pending_door_meshes(
                pending,
                &bsp,
                &face_attributions,
                instance,
                &mut face_context,
            )?);
        }
    }

    let (mut static_props, mut skybox_static_props) =
        partitioned_static_prop_placements(&bsp, &skybox_partition);
    stats.world_static_prop_count = count_to_u32(static_props.len());
    stats.skybox_static_prop_count = count_to_u32(skybox_static_props.len());
    let (mut entity_props, mut skybox_entity_props) =
        partitioned_entity_prop_placements(&bsp, &skybox_partition);
    stats.world_entity_prop_count = count_to_u32(entity_props.len());
    stats.skybox_entity_prop_count = count_to_u32(skybox_entity_props.len());
    stats.entity_prop_count =
        count_to_u32(entity_props.len().saturating_add(skybox_entity_props.len()));
    static_props.append(&mut entity_props);
    skybox_static_props.append(&mut skybox_entity_props);
    let detail_material_name = worldspawn_detail_material_name(&bsp.entities);
    let (detail_sprites, skybox_detail_sprites) =
        partitioned_detail_sprite_placements(&bsp, &skybox_partition);
    let ambient = map_ambient_lighting(&bsp);
    let world_lighting = map_world_lighting(&bsp, lightmap_source, ambient.source());
    let environment_lighting = map_environment_lighting(&bsp.entities);
    let skyname = worldspawn_skyname(&bsp.entities);
    let fog = map_fog(&bsp.entities);
    let lightmap = bake_lightmap_atlas(
        meshes.iter_mut().chain(
            door_builds
                .iter_mut()
                .flat_map(|door| door.meshes.iter_mut()),
        ),
        &lightmap_blocks,
        lightmap_source,
        &bsp.entities,
        &mut stats.lightmap,
    );
    let doors = door_builds
        .into_iter()
        .filter_map(PendingDoorBuild::into_map_door)
        .collect::<Vec<_>>();
    let (meshes, skybox_meshes) = split_build_meshes(meshes.into_inner());
    let meshes = meshes
        .into_iter()
        .map(|mesh| MapMesh {
            entity: mesh.entity,
            vertices: mesh
                .vertices
                .into_iter()
                .map(|vertex| vertex.vertex)
                .collect(),
            indices: mesh.indices,
            material_index: mesh.material_index,
            visibility: mesh.visibility.into_map_visibility(),
            surface_owners: mesh.surface_owners,
        })
        .collect::<Vec<_>>();
    let skybox_meshes = skybox_meshes
        .into_iter()
        .map(|mesh| MapMesh {
            entity: mesh.entity,
            vertices: mesh
                .vertices
                .into_iter()
                .map(|vertex| vertex.vertex)
                .collect(),
            indices: mesh.indices,
            material_index: mesh.material_index,
            visibility: mesh.visibility.into_map_visibility(),
            surface_owners: mesh.surface_owners,
        })
        .collect::<Vec<_>>();
    let projector = MapSurfaceProjector::new(&meshes, &skybox_meshes, &doors);
    let (overlays, skybox_overlays) = partitioned_map_overlays(&bsp, &projector)?;
    let visibility = MapVisibility::from_bsp(&bsp, projector.displacement_bounds());
    let decals = decals::map_decals(&bsp, &projector, &doors, visibility.as_ref());
    let MapBounds {
        min: bounds_min,
        max: bounds_max,
    } = bounds_from_meshes(&meshes);
    let walk_collision =
        MapWalkCollision::from_bsp_instances(&bsp, &brush_instances, &door_entity_indices);
    let water_volumes = MapWaterVolumes::from_bsp(&bsp);
    let lighting_collision = MapLightingCollision::from_bsp(&bsp);
    let radiosity_surfaces = MapRadiositySurfaces::from_bsp(&bsp);
    if let Some(volumes) = &water_volumes {
        for body in volumes.bodies.iter().flatten() {
            if !material_names.contains(&body.material_name) {
                material_names.push(body.material_name.clone());
            }
        }
    }
    let skybox_completion_bounds = skybox_partition.completion_bounds();
    let skybox_partition = MapSkyboxPartitionStats {
        sky_camera_present: skybox_partition.sky_camera_present(),
        face_count: count_to_u32(face_attributions.skybox_face_count()),
        completion_reattributed_face_count: count_to_u32(
            face_attributions.completion_reattributed_face_count(),
        ),
        static_prop_count: count_to_u32(skybox_static_props.len()),
        detail_sprite_count: count_to_u32(skybox_detail_sprites.len()),
        overlay_count: count_to_u32(
            skybox_overlays
                .iter()
                .map(|overlay| overlay.source)
                .collect::<std::collections::HashSet<_>>()
                .len(),
        ),
    };

    Ok(MapData {
        lighting_source,
        meshes,
        skybox_meshes,
        material_names,
        static_props,
        skybox_static_props,
        doors,
        detail_material_name,
        detail_sprites,
        skybox_detail_sprites,
        overlays,
        decals,
        skybox_overlays,
        ambient,
        world_lighting,
        environment_lighting,
        player_start,
        skyname,
        sky_paint,
        suns,
        cubemap_probes: bsp
            .cubemap_samples
            .iter()
            .map(MapCubemapProbe::from)
            .collect(),
        fog,
        sky_camera,
        skybox_completion_bounds,
        lightmap,
        bounds_min,
        bounds_max,
        stats,
        skybox_partition,
        visibility,
        walk_collision,
        water_volumes,
        lighting_collision,
        radiosity_surfaces,
        pakfile,
    })
}

fn split_build_meshes(meshes: Vec<BuildMesh>) -> (Vec<BuildMesh>, Vec<BuildMesh>) {
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    for mesh in meshes {
        match mesh.partition {
            GeometryPartition::Visible => visible.push(mesh),
            GeometryPartition::Skybox => skybox.push(mesh),
        }
    }
    (visible, skybox)
}

fn vector_is_finite_nonzero(vector: Vec3) -> bool {
    vector.is_finite() && vector.length_squared() > f32::EPSILON
}

/// Skip non-finite vertices; use a zero box if none remain. Door bounds
/// use [`bounds_from_map_meshes`], which rejects the whole set instead.
fn bounds_from_meshes(meshes: &[MapMesh]) -> MapBounds {
    let mut bounds = BoundsBuilder::default();
    for position in meshes
        .iter()
        .flat_map(|mesh| mesh.vertices.iter().map(|vertex| vertex.position))
    {
        bounds.push(position);
    }
    bounds.finish().unwrap_or(MapBounds {
        min: Vec3::splat(0.0),
        max: Vec3::splat(0.0),
    })
}

fn count_to_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;
