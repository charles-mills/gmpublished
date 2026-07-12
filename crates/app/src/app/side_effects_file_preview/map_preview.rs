use super::materials::{
    MapMaterialResolution, resolve_detail_sprites, resolve_map_material_slots_parallel,
    resolve_map_overlays, resolve_skybox, sky_log_status,
};
use super::{
    AmbientCube, Arc, BTreeMap, BTreeSet, ContentSourceTier, ConvexHull, ConvexLedge,
    DecodedTextureBudget, DoorInstance, DoorSound, DoorSoundSourceTier, DoorSoundWave, DoorSounds,
    Duration, HashMap, HashSet, InfoReason, Instant, LightmapSlot, MAP_FALLBACK_TEXTURE_DIMENSION,
    MAP_PROP_PLACEMENT_CAP, MAP_PROP_TRIANGLE_CAP, MAP_TEXTURE_DECODE_BUDGET_BYTES,
    MAP_TEXTURE_MAX_DIMENSION, MAP_TOO_LARGE_BYTES, MapAmbientLighting, MapDoor, MapDoorGeometry,
    MapEnvironmentLighting, MapFog, MapMeshClusterRanges, MapMeshIndexRange, MapMeshVisibility,
    MapPropVisibility, MapSkyCamera, MapSpawn, MapStats, MapVisibility, MapWalkCollision,
    MapWalkPropModel, MapWalkPropModelPlacement, MaterialResolver, MaterialSlot, MeshData,
    ModelData, ModelPreview, ModelStats, ModelVertex, PHY_DEBUG_MATERIAL_NAME,
    PHY_DEBUG_TRIANGLE_CAP, PHY_DEBUG_VERTEX_COLOR, PreviewContent, PreviewData, PreviewLoadStage,
    PreviewRequest, ReadStats, RenderMode, ResolvedPrimaryMaterial, ResolvedSoundReference,
    SkipReason, StaticPropPlacement, catch_asset_decode, entry_stem, info_preview_data,
    load_model_catching_panic, load_model_companions, srgb_byte_to_linear,
};

pub(super) fn map_preview_data(
    request: &PreviewRequest,
    bsp_bytes: &[u8],
    gmod_dir: Option<std::path::PathBuf>,
    emit_stage: &mut impl FnMut(PreviewLoadStage),
) -> PreviewData {
    map_preview_data_with_prop_model_loader(
        request,
        bsp_bytes,
        gmod_dir,
        emit_stage,
        &load_prop_model,
    )
}

pub(super) fn map_preview_data_with_prop_model_loader(
    request: &PreviewRequest,
    bsp_bytes: &[u8],
    gmod_dir: Option<std::path::PathBuf>,
    emit_stage: &mut impl FnMut(PreviewLoadStage),
    load_model: &(impl Fn(&str, &MaterialResolver) -> Option<LoadedPropModel> + Sync),
) -> PreviewData {
    if !request.bypass_size_limits && bsp_bytes.len() > MAP_TOO_LARGE_BYTES {
        return info_preview_data(request, InfoReason::TooLarge);
    }

    let bsp_started = Instant::now();
    let map = match gmpublished_backend::scene::map::load_map(bsp_bytes) {
        Ok(map) => map,
        Err(error) => {
            log::debug!("file preview bsp decode failed: {error}");
            return info_preview_data(request, InfoReason::DecodeFailed);
        }
    };
    let bsp_timing = bsp_started.elapsed();

    let gmpublished_backend::scene::map::MapData {
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
        environment_lighting,
        player_start,
        bounds_min,
        bounds_max,
        stats: raw_stats,
        skybox_partition,
        skybox_completion_bounds: _,
        visibility,
        mut walk_collision,
        pakfile,
        lightmap,
        skyname,
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
    let MapMaterialResolution {
        mut materials,
        mut material_indexes,
        mut resolved_material_count,
        mut water_fallback_material_count,
    } = resolve_map_material_slots_parallel(&material_names, &material_resolver);
    let (detail_sprites, map_skybox_detail_sprites) = resolve_detail_sprites(
        &map_detail_sprites,
        &map_skybox_detail_sprites,
        &detail_material_name,
        &material_resolver,
        PropMaterialState {
            materials: &mut materials,
            material_indexes: &mut material_indexes,
            resolved_material_count: &mut resolved_material_count,
            water_fallback_material_count: &mut water_fallback_material_count,
        },
    );
    let overlay_bake = resolve_map_overlays(
        &map_overlays,
        &material_resolver,
        PropMaterialState {
            materials: &mut materials,
            material_indexes: &mut material_indexes,
            resolved_material_count: &mut resolved_material_count,
            water_fallback_material_count: &mut water_fallback_material_count,
        },
    );
    let skybox_overlay_bake = resolve_map_overlays(
        &map_skybox_overlays,
        &material_resolver,
        PropMaterialState {
            materials: &mut materials,
            material_indexes: &mut material_indexes,
            resolved_material_count: &mut resolved_material_count,
            water_fallback_material_count: &mut water_fallback_material_count,
        },
    );
    let materials_timing = materials_started.elapsed();
    let mut mesh_visibility = map_meshes
        .iter()
        .map(|mesh| mesh.visibility.clone())
        .collect::<Vec<_>>();
    let mut meshes = map_meshes
        .iter()
        .map(|mesh| map_mesh_to_model_mesh(mesh, &materials))
        .collect::<Vec<_>>();
    let mut map_skybox_meshes = map_skybox_meshes
        .iter()
        .map(|mesh| map_mesh_to_model_mesh(mesh, &materials))
        .collect::<Vec<_>>();
    emit_stage(PreviewLoadStage::PlacingProps);
    let props_started = Instant::now();
    refresh_entity_prop_aabb_visibility(
        &mut static_props,
        usize::try_from(raw_stats.world_static_prop_count).unwrap_or(usize::MAX),
        usize::try_from(raw_stats.world_entity_prop_count).unwrap_or(0),
        visibility.as_ref(),
        &material_resolver,
        load_model,
    );
    refresh_entity_prop_aabb_visibility(
        &mut skybox_static_props,
        usize::try_from(raw_stats.skybox_static_prop_count).unwrap_or(usize::MAX),
        usize::try_from(raw_stats.skybox_entity_prop_count).unwrap_or(0),
        visibility.as_ref(),
        &material_resolver,
        load_model,
    );
    let prop_load_started = Instant::now();
    let loaded_model_cache =
        load_unique_prop_models_parallel(&static_props, &material_resolver, load_model);
    let (enriched_walk_collision, prop_collision_stats) = enrich_walk_collision_with_prop_collision(
        walk_collision,
        &static_props,
        &loaded_model_cache,
    );
    walk_collision = enriched_walk_collision;
    log::debug!(
        "map preview prop collision: solid placements {}, collidable {}, parsed models {}, hulls {}, memory {} bytes ({} MiB), skipped not-solid {}, model-load {}, missing-phy {}, unparseable-phy {}, phy reasons {:?}",
        prop_collision_stats.solid_placements,
        prop_collision_stats.collidable_placements,
        prop_collision_stats.parsed_models,
        prop_collision_stats.prop_hulls,
        prop_collision_stats.memory_bytes,
        format_mib(prop_collision_stats.memory_bytes),
        prop_collision_stats.skipped_not_solid,
        prop_collision_stats.skipped_model_load,
        prop_collision_stats.skipped_missing_phy,
        prop_collision_stats.skipped_unparseable_phy,
        prop_collision_stats.skip_reasons
    );
    let prop_lighting = StaticPropLightingInputs {
        ambient: &ambient,
        environment_lighting: environment_lighting.as_ref(),
        walk_collision: walk_collision.as_ref(),
    };
    let pre_resolved_prop_materials =
        pre_resolve_prop_materials(&static_props, &loaded_model_cache, &material_resolver);
    let prop_bake = bake_static_props_from_loaded_model_cache(
        &static_props,
        &material_resolver,
        PropMaterialState {
            materials: &mut materials,
            material_indexes: &mut material_indexes,
            resolved_material_count: &mut resolved_material_count,
            water_fallback_material_count: &mut water_fallback_material_count,
        },
        &loaded_model_cache,
        Some(&pre_resolved_prop_materials),
        prop_lighting,
        prop_load_started,
    );
    let skybox_prop_bake = bake_static_props_with_prop_model_loader(
        &skybox_static_props,
        &material_resolver,
        PropMaterialState {
            materials: &mut materials,
            material_indexes: &mut material_indexes,
            resolved_material_count: &mut resolved_material_count,
            water_fallback_material_count: &mut water_fallback_material_count,
        },
        prop_lighting,
        load_model,
    );
    let door_bake = bake_map_doors_with_prop_model_loader(
        &map_doors,
        &material_resolver,
        PropMaterialState {
            materials: &mut materials,
            material_indexes: &mut material_indexes,
            resolved_material_count: &mut resolved_material_count,
            water_fallback_material_count: &mut water_fallback_material_count,
        },
        prop_lighting,
        load_model,
    );
    log_prop_door_material_resolution(&door_bake.prop_material_resolutions);
    let props_timing = props_started.elapsed();
    meshes.extend(prop_bake.meshes);
    mesh_visibility.extend(prop_bake.mesh_visibility);
    map_skybox_meshes.extend(skybox_prop_bake.meshes);
    emit_stage(PreviewLoadStage::BakingLightmap);
    let lightmap_started = Instant::now();
    let lightmap_status = lightmap_status(lightmap.as_ref());
    let lightmap = lightmap.map(|atlas| LightmapSlot {
        rgba: atlas.rgba,
        width: atlas.width,
        height: atlas.height,
    });
    let lightmap_timing = lightmap_started.elapsed();
    let skybox = skyname
        .as_deref()
        .and_then(|skyname| resolve_skybox(skyname, &resolver.with_bc_textures_disabled()));
    let sky_status = sky_log_status(skyname.as_deref());
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
    let material_slot_count = materials.len();
    let material_count = u32::try_from(material_slot_count).unwrap_or(u32::MAX);
    let (bounds_min, bounds_max) =
        bounds_from_model_meshes(&meshes).unwrap_or((bounds_min, bounds_max));
    let phy_debug_meshes =
        phy_debug_mesh_from_loaded_prop_models(&static_props, &loaded_model_cache, materials.len())
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
    log_unresolved_materials(&materials);
    let water_status = water_fallback_log_suffix(water_fallback_material_count);
    let texture_mib = format_mib(texture_budget.decoded_bytes());
    let texture_payloads = texture_payload_log_suffix(&materials);
    let render_mode_status = render_mode_log_suffix(&materials);
    if !phy_debug_meshes.is_empty() {
        materials.push(MaterialSlot {
            name: PHY_DEBUG_MATERIAL_NAME.to_owned(),
            texture: None,
            texture2: None,
            force_opaque: false,
            render_mode: RenderMode::Translucent,
        });
    }
    let skin_table = identity_skin_table(materials.len());
    let scene = Arc::new(ModelPreview {
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
        mesh_visibility,
        map_skybox_meshes,
        materials,
        lightmap,
        skybox,
        detail_sprites,
        map_skybox_detail_sprites,
        overlays: overlay_bake.overlays,
        map_skybox_overlays: skybox_overlay_bake.overlays,
        doors: door_bake.doors,
        phy_debug_meshes,
        skin_tables: vec![skin_table],
        bodygroups: Vec::new(),
        bounds_min,
        bounds_max,
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
        placed_prop_count: prop_bake
            .placed_count
            .saturating_add(skybox_prop_bake.placed_count),
        skipped_prop_count: prop_bake
            .skipped_count
            .saturating_add(skybox_prop_bake.skipped_count),
        detail_sprite_count,
        overlay_count,
        skybox_face_count: skybox_partition.face_count,
        skybox_prop_count: skybox_partition.static_prop_count,
        skybox_detail_sprite_count: map_skybox_detail_sprite_count,
        skybox_overlay_count: map_skybox_overlay_count,
        version: raw_stats.version,
    };
    let prop_skip_stats = prop_bake.skip_stats + skybox_prop_bake.skip_stats;
    let prop_mesh_bytes = prop_bake
        .mesh_bytes
        .saturating_add(skybox_prop_bake.mesh_bytes);
    log::info!(
        "map {}: materials resolved {resolved_material_count}/{material_count}{water_status}{render_mode_status}, textures {texture_mib} MiB{texture_payloads}, {lightmap_status}, {sky_status}, clusters {}, skybox faces {}, props {}, props placed {} (skipped {}: cap {}, triangles {}, load {}, invalid {}, empty {}), prop mesh {} bytes ({} MiB), detail sprites {}, overlays {} (skipped {}), timings: bsp {}ms, materials {}ms, props {}ms, props load {}ms, bake {}ms, lightmap {}ms",
        request.entry_path,
        stats.cluster_count,
        stats.skybox_face_count,
        stats.skybox_prop_count,
        stats.placed_prop_count,
        stats.skipped_prop_count,
        prop_skip_stats.placement_cap,
        prop_skip_stats.triangle_cap,
        prop_skip_stats.load_failure,
        prop_skip_stats.invalid_model_path,
        prop_skip_stats.no_bakeable_mesh,
        prop_mesh_bytes,
        format_mib(prop_mesh_bytes),
        stats.detail_sprite_count,
        stats.overlay_count,
        skipped_overlay_count,
        duration_ms(bsp_timing),
        duration_ms(materials_timing),
        duration_ms(props_timing),
        duration_ms(prop_bake.load_timing + skybox_prop_bake.load_timing),
        duration_ms(prop_bake.bake_timing + skybox_prop_bake.bake_timing),
        duration_ms(lightmap_timing)
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
    T: Sync,
    R: Send,
    F: Fn(usize, &T) -> R + Sync,
{
    if items.is_empty() {
        return Vec::new();
    }

    let worker_count = preview_worker_count(items.len());
    if worker_count == 1 {
        return items
            .iter()
            .enumerate()
            .map(|(index, item)| work(index, item))
            .collect();
    }

    let chunk_size = items.len().div_ceil(worker_count);
    std::thread::scope(|scope| {
        items
            .chunks(chunk_size)
            .enumerate()
            .map(|(chunk_index, chunk)| {
                let base_index = chunk_index * chunk_size;
                let work = &work;
                scope.spawn(move || {
                    chunk
                        .iter()
                        .enumerate()
                        .map(|(offset, item)| work(base_index + offset, item))
                        .collect::<Vec<R>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|handle| handle.join().expect("parallel_collect worker panicked"))
            .collect()
    })
}

pub(super) fn preview_worker_count(item_count: usize) -> usize {
    let parallelism = std::thread::available_parallelism().map_or(1, usize::from);
    item_count.min(parallelism.min(8)).max(1)
}

pub(super) fn duration_ms(duration: Duration) -> u128 {
    duration.as_millis()
}

pub(super) fn map_fog_to_preview(fog: gmpublished_backend::scene::map::MapFog) -> MapFog {
    MapFog {
        color_linear: fog.color_srgb.map(srgb_byte_to_linear),
        start: fog.start,
        end: fog.end,
        max_density: fog.max_density,
    }
}

pub(super) fn map_sky_camera_to_preview(
    camera: gmpublished_backend::scene::map::MapSkyCamera,
) -> MapSkyCamera {
    MapSkyCamera {
        origin: camera.origin,
        scale: camera.scale,
        fog: camera.fog.map(map_fog_to_preview),
    }
}

pub(super) fn map_spawn_to_preview(
    spawn: gmpublished_backend::scene::map::MapPlayerStart,
) -> MapSpawn {
    MapSpawn {
        origin: spawn.origin,
        angles: spawn.angles,
    }
}

#[derive(Debug)]
pub(super) struct PropBakeResult {
    pub(super) meshes: Vec<MeshData>,
    pub(super) mesh_visibility: Vec<MapMeshVisibility>,
    pub(super) placed_count: u32,
    pub(super) skipped_count: u32,
    pub(super) skip_stats: PropBakeSkipStats,
    pub(super) mesh_bytes: usize,
    pub(super) load_timing: Duration,
    pub(super) bake_timing: Duration,
}

#[derive(Debug, Default)]
pub(super) struct DoorBakeResult {
    pub(super) doors: Vec<DoorInstance>,
    pub(super) prop_door_count: u32,
    pub(super) skipped_prop_door_count: u32,
    pub(super) prop_material_resolutions: BTreeMap<String, PropDoorMaterialResolution>,
}

#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub(super) struct PropDoorMaterialResolution {
    pub(super) door_count: u32,
    pub(super) used_material_slots: BTreeSet<usize>,
    pub(super) resolved_used_material_slots: BTreeSet<usize>,
    pub(super) unresolved_used_material_slots: BTreeSet<usize>,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(super) struct PropBakeSkipStats {
    pub(super) placement_cap: usize,
    pub(super) triangle_cap: usize,
    pub(super) invalid_model_path: usize,
    pub(super) load_failure: usize,
    pub(super) no_bakeable_mesh: usize,
}

impl PropBakeSkipStats {
    pub(super) const fn total(self) -> usize {
        self.placement_cap
            + self.triangle_cap
            + self.invalid_model_path
            + self.load_failure
            + self.no_bakeable_mesh
    }

    const fn has_skips(self) -> bool {
        self.total() > 0
    }
}

impl std::ops::Add for PropBakeSkipStats {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            placement_cap: self.placement_cap.saturating_add(rhs.placement_cap),
            triangle_cap: self.triangle_cap.saturating_add(rhs.triangle_cap),
            invalid_model_path: self
                .invalid_model_path
                .saturating_add(rhs.invalid_model_path),
            load_failure: self.load_failure.saturating_add(rhs.load_failure),
            no_bakeable_mesh: self.no_bakeable_mesh.saturating_add(rhs.no_bakeable_mesh),
        }
    }
}

#[derive(Debug)]
pub(super) struct PropModelAsset {
    pub(super) model: Arc<ModelData>,
    pub(super) material_indices: Vec<usize>,
    pub(super) default_triangle_count: usize,
}

#[derive(Debug)]
pub(super) struct LoadedPropModel {
    pub(super) model: Arc<ModelData>,
    pub(super) default_triangle_count: usize,
    pub(super) physics: LoadedPropPhysics,
    pub(super) collision: Option<Arc<MapWalkPropModel>>,
}

#[derive(Debug)]
pub(super) enum LoadedPropPhysics {
    Parsed(Arc<LoadedPhy>),
    Missing,
    Unparseable(ReadStats),
}

#[derive(Debug, Default, Clone)]
pub(super) struct PropCollisionStats {
    pub(super) solid_placements: usize,
    pub(super) collidable_placements: usize,
    pub(super) parsed_models: usize,
    pub(super) prop_hulls: usize,
    pub(super) memory_bytes: usize,
    pub(super) skipped_not_solid: usize,
    pub(super) skipped_model_load: usize,
    pub(super) skipped_missing_phy: usize,
    pub(super) skipped_unparseable_phy: usize,
    pub(super) skip_reasons: BTreeMap<SkipReason, usize>,
}

#[derive(Debug)]
pub(super) struct PropMaterialResolveJob {
    pub(super) key: String,
    pub(super) name: String,
    pub(super) material_dirs: Vec<String>,
}

pub(super) struct PropMaterialState<'a> {
    pub(super) materials: &'a mut Vec<MaterialSlot>,
    pub(super) material_indexes: &'a mut HashMap<String, usize>,
    pub(super) resolved_material_count: &'a mut u32,
    pub(super) water_fallback_material_count: &'a mut u32,
}

#[derive(Debug)]
pub(super) struct PropBuildMesh {
    pub(super) vertices: Vec<ModelVertex>,
    pub(super) indices: Vec<u32>,
    pub(super) material_index: usize,
    pub(super) visibility: PropBuildMeshVisibility,
}

#[derive(Debug, Default)]
pub(super) struct PropBuildMeshVisibility {
    pub(super) always_visible: Vec<MapMeshIndexRange>,
    pub(super) clusters: BTreeMap<u32, Vec<MapMeshIndexRange>>,
    pub(super) next_face: u32,
}

impl PropBuildMeshVisibility {
    fn push(&mut self, visibility: &MapPropVisibility, start: usize, count: usize) {
        let (Ok(start), Ok(count)) = (u32::try_from(start), u32::try_from(count)) else {
            return;
        };
        let range = MapMeshIndexRange {
            face: self.next_face,
            start,
            count,
        };
        self.next_face = self.next_face.saturating_add(1);
        match visibility {
            MapPropVisibility::Always => self.always_visible.push(range),
            MapPropVisibility::Clusters(clusters) if clusters.is_empty() => {
                self.always_visible.push(range);
            }
            MapPropVisibility::Clusters(clusters) => {
                for cluster in clusters {
                    self.clusters.entry(*cluster).or_default().push(range);
                }
            }
        }
    }

    fn append_shifted(&mut self, source: Self, index_base: u32) {
        // Face ids are the rebuild-time dedup key and must stay unique within
        // a merged mesh; each parallel chunk numbers its own faces from zero,
        // so rebase them past the ids already used here or colliding props
        // would be silently dropped from the visible plan.
        let face_base = self.next_face;
        for range in source.always_visible {
            if let Some(range) = shifted_range(range, index_base, face_base) {
                self.always_visible.push(range);
            }
        }
        for (cluster, ranges) in source.clusters {
            let target = self.clusters.entry(cluster).or_default();
            target.extend(
                ranges
                    .into_iter()
                    .filter_map(|range| shifted_range(range, index_base, face_base)),
            );
        }
        self.next_face = self.next_face.saturating_add(source.next_face);
    }

    fn into_map_visibility(self) -> MapMeshVisibility {
        MapMeshVisibility {
            always_visible: self.always_visible,
            clusters: self
                .clusters
                .into_iter()
                .map(|(cluster, ranges)| MapMeshClusterRanges { cluster, ranges })
                .collect(),
        }
    }
}

pub(super) fn shifted_range(
    range: MapMeshIndexRange,
    index_base: u32,
    face_base: u32,
) -> Option<MapMeshIndexRange> {
    Some(MapMeshIndexRange {
        face: face_base.checked_add(range.face)?,
        start: range.start.checked_add(index_base)?,
        count: range.count,
    })
}

#[derive(Debug)]
pub(super) struct SelectedPropPlacement<'a> {
    pub(super) placement: &'a StaticPropPlacement,
    pub(super) model: Arc<PropModelAsset>,
    pub(super) lighting: PropPlacementLighting,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PropPlacementLighting {
    pub(super) ambient_cube: AmbientCube,
    pub(super) sun: Option<PropSunLighting>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PropSunLighting {
    pub(super) direction_to_sun: [f32; 3],
    pub(super) color_linear: [f32; 3],
    pub(super) visible: bool,
}

#[derive(Clone, Copy)]
pub(super) struct StaticPropLightingInputs<'a> {
    pub(super) ambient: &'a MapAmbientLighting,
    pub(super) environment_lighting: Option<&'a MapEnvironmentLighting>,
    pub(super) walk_collision: Option<&'a MapWalkCollision>,
}

const PROP_LIGHT_SAMPLE_OFFSET: [f32; 3] = [0.0, 0.0, 16.0];
const PROP_LIGHT_LINEAR_CLAMP: f32 = 2.0;

impl PropPlacementLighting {
    pub(super) fn evaluate(self, normal: [f32; 3]) -> [f32; 3] {
        let normal = normalize(normal);
        let mut color = self.ambient_cube.evaluate(normal);
        if let Some(sun) = self.sun.filter(|sun| sun.visible) {
            let amount = dot(normal, sun.direction_to_sun).max(0.0);
            for (channel, value) in color.iter_mut().enumerate() {
                *value += sun.color_linear[channel] * amount;
            }
        }
        // No separate skylight term: the ambient cube already integrates sky
        // bounce (the engine likewise skips sky-ambient world lights for
        // entities because ambient cubes cover them).
        color.map(|channel| channel.clamp(0.0, PROP_LIGHT_LINEAR_CLAMP))
    }
}

pub(super) fn prop_placement_lighting(
    placement: &StaticPropPlacement,
    lighting: StaticPropLightingInputs<'_>,
) -> PropPlacementLighting {
    let ambient_cube = lighting.ambient.cube_at(placement.origin);
    let Some(environment_lighting) = lighting.environment_lighting else {
        return PropPlacementLighting {
            ambient_cube,
            sun: None,
        };
    };
    let ray_start = add(placement.origin, PROP_LIGHT_SAMPLE_OFFSET);
    let sun = environment_lighting.sun.map(|sun| PropSunLighting {
        direction_to_sun: sun.direction_to_sun,
        color_linear: sun.color_linear,
        visible: lighting
            .walk_collision
            .is_some_and(|collision| collision.ray_hits_sky(ray_start, sun.direction_to_sun)),
    });
    PropPlacementLighting { ambient_cube, sun }
}

pub(super) fn refresh_entity_prop_aabb_visibility(
    placements: &mut [StaticPropPlacement],
    entity_start: usize,
    entity_count: usize,
    visibility: Option<&MapVisibility>,
    resolver: &MaterialResolver,
    load_model: &(impl Fn(&str, &MaterialResolver) -> Option<LoadedPropModel> + Sync),
) {
    if entity_count == 0 {
        return;
    }
    let Some(visibility) = visibility else {
        return;
    };
    let Some(entity_end) = entity_start.checked_add(entity_count) else {
        log::debug!(
            "map preview entity prop AABB visibility range overflows start={entity_start} count={entity_count}: using existing visibility"
        );
        return;
    };
    let Some(entity_props) = placements.get_mut(entity_start..entity_end) else {
        log::debug!(
            "map preview entity prop AABB visibility range out of bounds start={entity_start} count={entity_count} placements={}: using existing visibility",
            placements.len()
        );
        return;
    };
    let loaded_model_cache = load_unique_prop_models_parallel(entity_props, resolver, load_model);
    for placement in entity_props {
        let Some(model) = loaded_model_cache
            .get(&placement.model_path)
            .and_then(|model| model.as_ref())
        else {
            log::debug!(
                "map preview entity prop {} AABB visibility missing model: using Always",
                placement.model_path
            );
            placement.visibility = MapPropVisibility::Always;
            continue;
        };
        let Some((bounds_min, bounds_max)) = prop_model_world_bounds(placement, &model.model)
        else {
            log::debug!(
                "map preview entity prop {} AABB visibility missing bounds: using Always",
                placement.model_path
            );
            placement.visibility = MapPropVisibility::Always;
            continue;
        };
        placement.visibility = visibility.clusters_for_aabb(bounds_min, bounds_max);
    }
}

pub(super) fn prop_model_world_bounds(
    placement: &StaticPropPlacement,
    model: &ModelData,
) -> Option<([f32; 3], [f32; 3])> {
    let mut positions = model
        .meshes
        .iter()
        .filter(|mesh| mesh.bodygroup_choice == 0)
        .flat_map(|mesh| mesh.vertices.iter())
        .map(|vertex| transform_prop_position(vertex.position, placement));
    let first = positions.next()?;
    let mut min = first;
    let mut max = first;
    for position in positions {
        for axis in 0..3 {
            min[axis] = min[axis].min(position[axis]);
            max[axis] = max[axis].max(position[axis]);
        }
    }
    Some((min, max))
}

#[cfg(test)]
pub(super) fn bake_static_props(
    placements: &[StaticPropPlacement],
    resolver: &MaterialResolver,
    materials: &mut Vec<MaterialSlot>,
    material_indexes: &mut HashMap<String, usize>,
    resolved_material_count: &mut u32,
    water_fallback_material_count: &mut u32,
    ambient: &MapAmbientLighting,
) -> PropBakeResult {
    bake_static_props_with_prop_model_loader(
        placements,
        resolver,
        PropMaterialState {
            materials,
            material_indexes,
            resolved_material_count,
            water_fallback_material_count,
        },
        StaticPropLightingInputs {
            ambient,
            environment_lighting: None,
            walk_collision: None,
        },
        &load_prop_model,
    )
}

pub(super) fn bake_static_props_with_prop_model_loader(
    placements: &[StaticPropPlacement],
    resolver: &MaterialResolver,
    material_state: PropMaterialState<'_>,
    lighting: StaticPropLightingInputs<'_>,
    load_model: &(impl Fn(&str, &MaterialResolver) -> Option<LoadedPropModel> + Sync),
) -> PropBakeResult {
    let load_started = Instant::now();
    let loaded_model_cache = load_unique_prop_models_parallel(placements, resolver, load_model);
    let pre_resolved_prop_materials =
        pre_resolve_prop_materials(placements, &loaded_model_cache, resolver);
    bake_static_props_from_loaded_model_cache(
        placements,
        resolver,
        material_state,
        &loaded_model_cache,
        Some(&pre_resolved_prop_materials),
        lighting,
        load_started,
    )
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "material_state's &mut fields are moved out into a PropMaterialContext, which requires owning the bundle"
)]
pub(super) fn bake_map_doors_with_prop_model_loader(
    doors: &[MapDoor],
    resolver: &MaterialResolver,
    material_state: PropMaterialState<'_>,
    lighting: StaticPropLightingInputs<'_>,
    load_model: &(impl Fn(&str, &MaterialResolver) -> Option<LoadedPropModel> + Sync),
) -> DoorBakeResult {
    if doors.is_empty() {
        return DoorBakeResult::default();
    }
    let prop_placements = doors
        .iter()
        .filter_map(|door| match &door.geometry {
            MapDoorGeometry::Prop { placement } => Some(placement.clone()),
            MapDoorGeometry::Brush { .. } => None,
        })
        .collect::<Vec<_>>();
    let loaded_model_cache =
        load_unique_prop_models_parallel(&prop_placements, resolver, load_model);
    let pre_resolved_prop_materials =
        pre_resolve_prop_materials(&prop_placements, &loaded_model_cache, resolver);
    let mut model_cache = HashMap::<String, Option<Arc<PropModelAsset>>>::new();
    let mut context = PropMaterialContext {
        resolver,
        pre_resolved_prop_materials: Some(&pre_resolved_prop_materials),
        materials: material_state.materials,
        material_indexes: material_state.material_indexes,
        resolved_material_count: material_state.resolved_material_count,
        water_fallback_material_count: material_state.water_fallback_material_count,
    };
    let resolved_door_sounds = resolve_map_door_sounds(doors, resolver);
    let mut baked = DoorBakeResult::default();
    for (door_index, door) in doors.iter().enumerate() {
        let sounds = resolved_door_sounds
            .get(door_index)
            .cloned()
            .unwrap_or_default();
        match &door.geometry {
            MapDoorGeometry::Brush { meshes, .. } => {
                let meshes = meshes
                    .iter()
                    .map(|mesh| map_mesh_to_model_mesh(mesh, context.materials))
                    .collect::<Vec<_>>();
                baked.doors.push(DoorInstance {
                    class: door.class,
                    origin: door.origin,
                    angles: door.angles,
                    local_bounds_min: door.local_bounds_min,
                    local_bounds_max: door.local_bounds_max,
                    visibility: door.visibility,
                    initial_progress: door.initial_progress,
                    motion: door.motion,
                    sounds,
                    meshes,
                });
            }
            MapDoorGeometry::Prop { placement } => {
                baked.prop_door_count = baked.prop_door_count.saturating_add(1);
                let Some(model) = cached_prop_model(
                    placement,
                    &loaded_model_cache,
                    &mut context,
                    &mut model_cache,
                ) else {
                    baked.skipped_prop_door_count = baked.skipped_prop_door_count.saturating_add(1);
                    continue;
                };
                record_prop_door_material_resolution(
                    &mut baked.prop_material_resolutions,
                    placement,
                    &model,
                    context.materials,
                );
                let lighting = prop_placement_lighting(placement, lighting);
                let meshes = bake_prop_door_meshes(placement, &model, lighting);
                let Some((local_bounds_min, local_bounds_max)) = bounds_from_model_meshes(&meshes)
                else {
                    baked.skipped_prop_door_count = baked.skipped_prop_door_count.saturating_add(1);
                    continue;
                };
                baked.doors.push(DoorInstance {
                    class: door.class,
                    origin: door.origin,
                    angles: door.angles,
                    local_bounds_min,
                    local_bounds_max,
                    visibility: door.visibility,
                    initial_progress: door.initial_progress,
                    motion: door.motion,
                    sounds,
                    meshes,
                });
            }
        }
    }
    baked
}

pub(super) fn resolve_map_door_sounds(
    doors: &[MapDoor],
    resolver: &MaterialResolver,
) -> Vec<DoorSounds> {
    let mut cache = HashMap::<String, Option<DoorSound>>::new();
    doors
        .iter()
        .map(|door| DoorSounds {
            move_sound: resolve_door_sound_slot(
                door.sounds.move_sound.as_deref(),
                resolver,
                &mut cache,
            ),
            stop_sound: resolve_door_sound_slot(
                door.sounds.stop_sound.as_deref(),
                resolver,
                &mut cache,
            ),
            open_sound: resolve_door_sound_slot(
                door.sounds.open_sound.as_deref(),
                resolver,
                &mut cache,
            ),
            close_sound: resolve_door_sound_slot(
                door.sounds.close_sound.as_deref(),
                resolver,
                &mut cache,
            ),
        })
        .collect()
}

pub(super) fn resolve_door_sound_slot(
    reference: Option<&str>,
    resolver: &MaterialResolver,
    cache: &mut HashMap<String, Option<DoorSound>>,
) -> Option<DoorSound> {
    let reference = reference?;
    let key = reference.trim().to_ascii_lowercase();
    if key.is_empty() {
        return None;
    }
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }
    let resolved = resolver
        .resolve_sound_reference(reference)
        .map(door_sound_from_resolved);
    cache.insert(key, resolved.clone());
    resolved
}

pub(super) fn door_sound_from_resolved(resolved: ResolvedSoundReference) -> DoorSound {
    DoorSound {
        reference: resolved.reference,
        sound_level: resolved.sound_level,
        volume: resolved.volume,
        waves: resolved
            .waves
            .into_iter()
            .map(|wave| DoorSoundWave {
                path: wave.path,
                source_tier: door_sound_source_tier(wave.source_tier),
                bytes: wave.bytes,
            })
            .collect(),
    }
}

const fn door_sound_source_tier(tier: ContentSourceTier) -> DoorSoundSourceTier {
    match tier {
        ContentSourceTier::Pakfile => DoorSoundSourceTier::Pakfile,
        ContentSourceTier::Addon => DoorSoundSourceTier::Addon,
        ContentSourceTier::Loose => DoorSoundSourceTier::Loose,
        ContentSourceTier::SiblingGma => DoorSoundSourceTier::SiblingGma,
        ContentSourceTier::GameVpk => DoorSoundSourceTier::GameVpk,
        ContentSourceTier::Prepended => DoorSoundSourceTier::Prepended,
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "material_state's &mut fields are moved out into a PropMaterialContext, which requires owning the bundle"
)]
pub(super) fn bake_static_props_from_loaded_model_cache(
    placements: &[StaticPropPlacement],
    resolver: &MaterialResolver,
    material_state: PropMaterialState<'_>,
    loaded_model_cache: &HashMap<String, Option<Arc<LoadedPropModel>>>,
    pre_resolved_prop_materials: Option<&HashMap<String, Option<ResolvedPrimaryMaterial>>>,
    lighting: StaticPropLightingInputs<'_>,
    load_started: Instant,
) -> PropBakeResult {
    let mut model_cache = HashMap::<String, Option<Arc<PropModelAsset>>>::new();
    let mut context = PropMaterialContext {
        resolver,
        pre_resolved_prop_materials,
        materials: material_state.materials,
        material_indexes: material_state.material_indexes,
        resolved_material_count: material_state.resolved_material_count,
        water_fallback_material_count: material_state.water_fallback_material_count,
    };
    let selected = select_static_prop_placements(
        placements,
        MAP_PROP_PLACEMENT_CAP,
        MAP_PROP_TRIANGLE_CAP,
        &mut |placement| {
            cached_prop_model(
                placement,
                loaded_model_cache,
                &mut context,
                &mut model_cache,
            )
        },
        lighting,
    );
    let load_timing = load_started.elapsed();
    bake_selected_static_props(selected, load_timing)
}

#[cfg(test)]
pub(super) fn bake_static_props_with_loaded_model_cache(
    placements: &[StaticPropPlacement],
    resolver: &MaterialResolver,
    material_state: PropMaterialState<'_>,
    loaded_model_cache: &HashMap<String, Option<Arc<LoadedPropModel>>>,
    pre_resolve: bool,
    ambient: &MapAmbientLighting,
) -> PropBakeResult {
    let load_started = Instant::now();
    let pre_resolved_prop_materials =
        pre_resolve.then(|| pre_resolve_prop_materials(placements, loaded_model_cache, resolver));
    bake_static_props_from_loaded_model_cache(
        placements,
        resolver,
        material_state,
        loaded_model_cache,
        pre_resolved_prop_materials.as_ref(),
        StaticPropLightingInputs {
            ambient,
            environment_lighting: None,
            walk_collision: None,
        },
        load_started,
    )
}

#[cfg(test)]
pub(super) fn bake_static_props_with_loader(
    placements: &[StaticPropPlacement],
    placement_cap: usize,
    triangle_cap: usize,
    lighting: StaticPropLightingInputs<'_>,
    mut load_model: impl FnMut(&StaticPropPlacement) -> Option<Arc<PropModelAsset>>,
) -> PropBakeResult {
    let load_started = Instant::now();
    let selected = select_static_prop_placements(
        placements,
        placement_cap,
        triangle_cap,
        &mut load_model,
        lighting,
    );
    let load_timing = load_started.elapsed();
    bake_selected_static_props(selected, load_timing)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "selected.skip_stats is moved out into the returned PropBakeResult, which requires owning it"
)]
pub(super) fn bake_selected_static_props(
    selected: SelectedPropPlacements<'_>,
    load_timing: Duration,
) -> PropBakeResult {
    let bake_started = Instant::now();
    let prop_meshes = bake_selected_prop_placements_parallel(&selected.placements);
    let bake_timing = bake_started.elapsed();
    let (meshes, mesh_visibility) = prop_meshes_to_mesh_data(prop_meshes);
    let mesh_bytes = prop_mesh_buffer_bytes(&meshes);

    PropBakeResult {
        meshes,
        mesh_visibility,
        placed_count: u32::try_from(selected.placements.len()).unwrap_or(u32::MAX),
        skipped_count: u32::try_from(selected.skip_stats.total()).unwrap_or(u32::MAX),
        skip_stats: selected.skip_stats,
        mesh_bytes,
        load_timing,
        bake_timing,
    }
}

#[cfg(test)]
pub(super) fn bake_static_props_with_loader_serial(
    placements: &[StaticPropPlacement],
    placement_cap: usize,
    triangle_cap: usize,
    lighting: StaticPropLightingInputs<'_>,
    mut load_model: impl FnMut(&StaticPropPlacement) -> Option<Arc<PropModelAsset>>,
) -> PropBakeResult {
    let selected = select_static_prop_placements(
        placements,
        placement_cap,
        triangle_cap,
        &mut load_model,
        lighting,
    );
    let prop_meshes = bake_selected_prop_placements_serial(&selected.placements);
    let (meshes, mesh_visibility) = prop_meshes_to_mesh_data(prop_meshes);
    let mesh_bytes = prop_mesh_buffer_bytes(&meshes);

    PropBakeResult {
        meshes,
        mesh_visibility,
        placed_count: u32::try_from(selected.placements.len()).unwrap_or(u32::MAX),
        skipped_count: u32::try_from(selected.skip_stats.total()).unwrap_or(u32::MAX),
        skip_stats: selected.skip_stats,
        mesh_bytes,
        load_timing: Duration::ZERO,
        bake_timing: Duration::ZERO,
    }
}

#[derive(Debug)]
pub(super) struct SelectedPropPlacements<'a> {
    pub(super) placements: Vec<SelectedPropPlacement<'a>>,
    pub(super) skip_stats: PropBakeSkipStats,
}

pub(super) fn select_static_prop_placements<'a>(
    placements: &'a [StaticPropPlacement],
    placement_cap: usize,
    triangle_cap: usize,
    load_model: &mut impl FnMut(&'a StaticPropPlacement) -> Option<Arc<PropModelAsset>>,
    lighting: StaticPropLightingInputs<'_>,
) -> SelectedPropPlacements<'a> {
    let capped_count = placements.len().min(placement_cap);
    let mut skip_stats = PropBakeSkipStats {
        placement_cap: placements.len().saturating_sub(capped_count),
        ..PropBakeSkipStats::default()
    };
    if skip_stats.placement_cap > 0 {
        log::debug!(
            "map preview static props placement cap {placement_cap}: skipped {}",
            skip_stats.placement_cap
        );
    }

    let mut selected = Vec::new();
    let mut baked_triangles = 0_usize;

    for (index, placement) in placements.iter().take(capped_count).enumerate() {
        if !std::path::Path::new(&placement.model_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mdl"))
        {
            skip_stats.invalid_model_path = skip_stats.invalid_model_path.saturating_add(1);
            continue;
        }

        let Some(model) = load_model(placement) else {
            skip_stats.load_failure = skip_stats.load_failure.saturating_add(1);
            continue;
        };

        if baked_triangles.saturating_add(model.default_triangle_count) > triangle_cap {
            let remaining = capped_count.saturating_sub(index);
            skip_stats.triangle_cap = skip_stats.triangle_cap.saturating_add(remaining);
            let placed_count = selected.len();
            log::debug!(
                "map preview static props triangle cap {triangle_cap}: placed {placed_count}, skipped remaining {remaining}"
            );
            break;
        }

        if prop_placement_has_bakeable_mesh(placement, &model) {
            baked_triangles = baked_triangles.saturating_add(model.default_triangle_count);
            selected.push(SelectedPropPlacement {
                placement,
                model,
                lighting: prop_placement_lighting(placement, lighting),
            });
        } else {
            skip_stats.no_bakeable_mesh = skip_stats.no_bakeable_mesh.saturating_add(1);
        }
    }

    if skip_stats.has_skips() {
        log::debug!(
            "map preview static props skipped: cap {}, triangles {}, load {}, invalid {}, empty {}",
            skip_stats.placement_cap,
            skip_stats.triangle_cap,
            skip_stats.load_failure,
            skip_stats.invalid_model_path,
            skip_stats.no_bakeable_mesh
        );
    }

    SelectedPropPlacements {
        placements: selected,
        skip_stats,
    }
}

pub(super) fn prop_mesh_buffer_bytes(meshes: &[MeshData]) -> usize {
    meshes
        .iter()
        .map(|mesh| {
            mesh.vertices
                .len()
                .saturating_mul(std::mem::size_of::<ModelVertex>())
                .saturating_add(
                    mesh.indices
                        .len()
                        .saturating_mul(std::mem::size_of::<u32>()),
                )
        })
        .sum()
}

pub(super) fn prop_meshes_to_mesh_data(
    prop_meshes: BTreeMap<usize, PropBuildMesh>,
) -> (Vec<MeshData>, Vec<MapMeshVisibility>) {
    prop_meshes
        .into_values()
        .map(|mesh| {
            (
                MeshData {
                    vertices: mesh.vertices,
                    indices: mesh.indices,
                    material_index: mesh.material_index,
                    bodygroup: 0,
                    bodygroup_choice: 0,
                },
                mesh.visibility.into_map_visibility(),
            )
        })
        .unzip()
}

pub(super) fn bake_selected_prop_placements_parallel(
    selected: &[SelectedPropPlacement<'_>],
) -> BTreeMap<usize, PropBuildMesh> {
    if selected.is_empty() {
        return BTreeMap::new();
    }

    let worker_count = preview_worker_count(selected.len());
    if worker_count == 1 {
        return bake_selected_prop_placements_serial(selected);
    }

    let chunk_size = selected.len().div_ceil(worker_count);
    let mut prop_meshes = BTreeMap::<usize, PropBuildMesh>::new();
    std::thread::scope(|scope| {
        let handles = selected
            .chunks(chunk_size)
            .map(|chunk| scope.spawn(move || bake_selected_prop_placements_serial(chunk)))
            .collect::<Vec<_>>();
        for handle in handles {
            merge_prop_meshes(
                &mut prop_meshes,
                handle.join().expect("prop bake worker panicked"),
            );
        }
    });
    prop_meshes
}

pub(super) fn bake_selected_prop_placements_serial(
    selected: &[SelectedPropPlacement<'_>],
) -> BTreeMap<usize, PropBuildMesh> {
    let mut prop_meshes = BTreeMap::<usize, PropBuildMesh>::new();
    for selected in selected {
        let baked = bake_prop_placement(
            selected.placement,
            &selected.model,
            selected.lighting,
            &mut prop_meshes,
        );
        debug_assert!(baked, "selected prop placement should bake");
    }
    prop_meshes
}

pub(super) fn merge_prop_meshes(
    target_meshes: &mut BTreeMap<usize, PropBuildMesh>,
    source_meshes: BTreeMap<usize, PropBuildMesh>,
) {
    for (material_index, source) in source_meshes {
        let target = target_meshes
            .entry(material_index)
            .or_insert_with(|| PropBuildMesh {
                vertices: Vec::new(),
                indices: Vec::new(),
                material_index,
                visibility: PropBuildMeshVisibility::default(),
            });
        let Some(base) = u32::try_from(target.vertices.len()).ok() else {
            continue;
        };
        // Visibility range starts are positions in the INDEX buffer, not
        // vertex ids — shift them by the index length, not the vertex base.
        let Some(index_base) = u32::try_from(target.indices.len()).ok() else {
            continue;
        };
        target.vertices.extend(source.vertices);
        target.indices.extend(
            source
                .indices
                .into_iter()
                .filter_map(|index| base.checked_add(index)),
        );
        target
            .visibility
            .append_shifted(source.visibility, index_base);
    }
}

pub(super) fn prop_placement_has_bakeable_mesh(
    placement: &StaticPropPlacement,
    model: &PropModelAsset,
) -> bool {
    let skin_table = prop_skin_table(&model.model, placement.skin);
    model
        .model
        .meshes
        .iter()
        .filter(|mesh| mesh.bodygroup_choice == 0)
        .any(|mesh| {
            let material_index = skin_table
                .and_then(|table| table.get(mesh.material_index))
                .map_or(mesh.material_index, |index| usize::from(*index));
            model.material_indices.get(material_index).is_some()
        })
}

pub(super) fn record_prop_door_material_resolution(
    resolutions: &mut BTreeMap<String, PropDoorMaterialResolution>,
    placement: &StaticPropPlacement,
    model: &PropModelAsset,
    materials: &[MaterialSlot],
) {
    let entry = resolutions.entry(placement.model_path.clone()).or_default();
    entry.door_count = entry.door_count.saturating_add(1);
    for source_slot in prop_model_used_material_slots(placement, model) {
        entry.used_material_slots.insert(source_slot);
        let resolved = model
            .material_indices
            .get(source_slot)
            .and_then(|&slot| materials.get(slot))
            .and_then(|material| material.texture.as_ref())
            .is_some();
        if resolved {
            entry.resolved_used_material_slots.insert(source_slot);
        } else {
            entry.unresolved_used_material_slots.insert(source_slot);
        }
    }
}

pub(super) fn prop_model_used_material_slots(
    placement: &StaticPropPlacement,
    model: &PropModelAsset,
) -> BTreeSet<usize> {
    let skin_table = prop_skin_table(&model.model, placement.skin);
    model
        .model
        .meshes
        .iter()
        .filter(|mesh| mesh.bodygroup_choice == 0)
        .map(|mesh| {
            skin_table
                .and_then(|table| table.get(mesh.material_index))
                .map_or(mesh.material_index, |index| usize::from(*index))
        })
        .collect()
}

pub(super) fn log_prop_door_material_resolution(
    resolutions: &BTreeMap<String, PropDoorMaterialResolution>,
) {
    for (model_path, resolution) in resolutions {
        let used = resolution.used_material_slots.len();
        if used == 0 {
            continue;
        }
        let resolved = resolution.resolved_used_material_slots.len();
        let unresolved = resolution
            .unresolved_used_material_slots
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let unresolved = if unresolved.is_empty() {
            "none".to_owned()
        } else {
            unresolved
        };
        log::debug!(
            "map preview prop doors {model_path}: used materials resolved {resolved}/{used} across {} doors (unresolved slots: {unresolved})",
            resolution.door_count
        );
    }
}

pub(super) fn bake_prop_door_meshes(
    placement: &StaticPropPlacement,
    model: &PropModelAsset,
    lighting: PropPlacementLighting,
) -> Vec<MeshData> {
    let skin_table = prop_skin_table(&model.model, placement.skin);
    model
        .model
        .meshes
        .iter()
        .filter(|mesh| mesh.bodygroup_choice == 0)
        .filter_map(|mesh| {
            let material_index = skin_table
                .and_then(|table| table.get(mesh.material_index))
                .map_or(mesh.material_index, |index| usize::from(*index));
            let material_index = model.material_indices.get(material_index).copied()?;
            let vertices = mesh
                .vertices
                .iter()
                .map(|vertex| {
                    let closed_normal = transform_prop_normal(vertex.normal, placement);
                    ModelVertex {
                        position: vertex.position,
                        normal: vertex.normal,
                        uv: vertex.uv,
                        lightmap_uv: [0.0; 2],
                        color: lighting.evaluate(closed_normal),
                        blend_alpha: 0.0,
                    }
                })
                .collect::<Vec<_>>();
            (!vertices.is_empty() && !mesh.indices.is_empty()).then(|| MeshData {
                vertices,
                indices: mesh.indices.clone(),
                material_index,
                bodygroup: 0,
                bodygroup_choice: 0,
            })
        })
        .collect()
}

pub(super) struct PropMaterialContext<'a> {
    pub(super) resolver: &'a MaterialResolver,
    pub(super) pre_resolved_prop_materials:
        Option<&'a HashMap<String, Option<ResolvedPrimaryMaterial>>>,
    pub(super) materials: &'a mut Vec<MaterialSlot>,
    pub(super) material_indexes: &'a mut HashMap<String, usize>,
    pub(super) resolved_material_count: &'a mut u32,
    pub(super) water_fallback_material_count: &'a mut u32,
}

pub(super) fn cached_prop_model(
    placement: &StaticPropPlacement,
    loaded_model_cache: &HashMap<String, Option<Arc<LoadedPropModel>>>,
    context: &mut PropMaterialContext<'_>,
    model_cache: &mut HashMap<String, Option<Arc<PropModelAsset>>>,
) -> Option<Arc<PropModelAsset>> {
    if let Some(cached) = model_cache.get(&placement.model_path) {
        return cached.clone();
    }

    let loaded = loaded_model_cache
        .get(&placement.model_path)
        .and_then(|model| model.as_ref())
        .map(|model| Arc::new(prop_model_asset_from_loaded(model, context)));
    model_cache.insert(placement.model_path.clone(), loaded.clone());
    loaded
}

pub(super) fn load_unique_prop_models_parallel(
    placements: &[StaticPropPlacement],
    resolver: &MaterialResolver,
    load_model: &(impl Fn(&str, &MaterialResolver) -> Option<LoadedPropModel> + Sync),
) -> HashMap<String, Option<Arc<LoadedPropModel>>> {
    let model_paths = unique_prop_model_paths(placements);
    parallel_collect(&model_paths, |_, model_path| {
        let context = format!("static prop model {model_path}");
        (
            model_path.clone(),
            catch_asset_decode(&context, || load_model(model_path, resolver))
                .flatten()
                .map(Arc::new),
        )
    })
    .into_iter()
    .collect()
}

pub(super) fn enrich_walk_collision_with_prop_collision(
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

pub(super) fn phy_debug_mesh_from_loaded_prop_models(
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
pub(super) struct PhyDebugMeshBuilder {
    pub(super) vertices: Vec<ModelVertex>,
    pub(super) indices: Vec<u32>,
    pub(super) truncated: bool,
}

impl PhyDebugMeshBuilder {
    pub(super) fn push_loaded_phy(
        &mut self,
        physics: &LoadedPhy,
        mut transform: impl FnMut([f32; 3]) -> [f32; 3],
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
        transform: &mut impl FnMut([f32; 3]) -> [f32; 3],
    ) {
        let Some(points) = triangle_points(ledge, triangle) else {
            return;
        };
        let positions = points.map(transform);
        if !positions.iter().flatten().all(|value| value.is_finite()) {
            return;
        }
        let normal = normalize(cross(
            sub(positions[1], positions[0]),
            sub(positions[2], positions[0]),
        ));
        if !normal.iter().all(|value| value.is_finite()) || dot(normal, normal) <= f32::EPSILON {
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

    pub(super) fn finish(self, material_index: usize) -> Option<MeshData> {
        (!self.indices.is_empty()).then_some(MeshData {
            vertices: self.vertices,
            indices: self.indices,
            material_index,
            bodygroup: 0,
            bodygroup_choice: 0,
        })
    }
}

pub(super) fn triangle_points(ledge: &ConvexLedge, triangle: [usize; 3]) -> Option<[[f32; 3]; 3]> {
    Some([
        *ledge.vertices.get(triangle[0])?,
        *ledge.vertices.get(triangle[1])?,
        *ledge.vertices.get(triangle[2])?,
    ])
}

pub(super) fn merge_phy_skip_reasons(
    target: &mut BTreeMap<SkipReason, usize>,
    parse_stats: &ReadStats,
) {
    for (reason, count) in &parse_stats.skip_reasons {
        *target.entry(*reason).or_default() += count;
    }
}

pub(super) fn unique_prop_model_paths(placements: &[StaticPropPlacement]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for placement in placements.iter().take(MAP_PROP_PLACEMENT_CAP) {
        let is_mdl = std::path::Path::new(&placement.model_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mdl"));
        if !is_mdl || !seen.insert(placement.model_path.clone()) {
            continue;
        }
        paths.push(placement.model_path.clone());
    }
    paths
}

pub(super) fn pre_resolve_prop_materials(
    placements: &[StaticPropPlacement],
    loaded_model_cache: &HashMap<String, Option<Arc<LoadedPropModel>>>,
    resolver: &MaterialResolver,
) -> HashMap<String, Option<ResolvedPrimaryMaterial>> {
    let jobs = unique_prop_material_resolve_jobs(placements, loaded_model_cache);
    parallel_collect(&jobs, |_, job| {
        (
            job.key.clone(),
            resolver.resolve_primary(&job.material_dirs, &job.name),
        )
    })
    .into_iter()
    .collect()
}

pub(super) fn unique_prop_material_resolve_jobs(
    placements: &[StaticPropPlacement],
    loaded_model_cache: &HashMap<String, Option<Arc<LoadedPropModel>>>,
) -> Vec<PropMaterialResolveJob> {
    let mut jobs = Vec::new();
    let mut seen = HashSet::new();
    for placement in placements.iter().take(MAP_PROP_PLACEMENT_CAP) {
        if !std::path::Path::new(&placement.model_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mdl"))
        {
            continue;
        }
        let Some(loaded) = loaded_model_cache
            .get(&placement.model_path)
            .and_then(|model| model.as_ref())
        else {
            continue;
        };
        for name in &loaded.model.material_names {
            let key = prop_material_cache_key(&loaded.model.material_dirs, name);
            if seen.insert(key.clone()) {
                jobs.push(PropMaterialResolveJob {
                    key,
                    name: name.clone(),
                    material_dirs: loaded.model.material_dirs.clone(),
                });
            }
        }
    }
    jobs
}

pub(super) fn load_prop_model(
    model_path: &str,
    resolver: &MaterialResolver,
) -> Option<LoadedPropModel> {
    let stem = entry_stem(model_path)?;
    let phy_path = format!("{stem}.phy");

    let Some(mdl_bytes) = resolver.entry_bytes(model_path) else {
        log::debug!("map preview static prop missing model {model_path}");
        return None;
    };
    let (vvd_bytes, vtx_bytes) = load_model_companions(stem, "map preview static prop", |path| {
        resolver.entry_bytes(path)
    })?;

    let model = load_model_catching_panic(
        &format!("static prop model {model_path}"),
        &mdl_bytes,
        &vvd_bytes,
        &vtx_bytes,
    )?;
    let default_triangle_count = model
        .meshes
        .iter()
        .filter(|mesh| mesh.bodygroup_choice == 0)
        .map(|mesh| mesh.indices.len() / 3)
        .sum::<usize>();

    let physics = load_prop_physics(&phy_path, resolver);
    let collision = match &physics {
        LoadedPropPhysics::Parsed(physics) => {
            let hulls = physics
                .ledges
                .iter()
                .map(|ledge| ConvexHull {
                    vertices: ledge.vertices.clone(),
                    triangles: ledge.triangles.clone(),
                })
                .collect::<Vec<_>>();
            let model = MapWalkPropModel::from_hulls(&hulls);
            (!model.is_empty()).then(|| Arc::new(model))
        }
        LoadedPropPhysics::Missing | LoadedPropPhysics::Unparseable(_) => None,
    };

    Some(LoadedPropModel {
        model: Arc::new(model),
        default_triangle_count,
        physics,
        collision,
    })
}

pub(super) fn load_prop_physics(phy_path: &str, resolver: &MaterialResolver) -> LoadedPropPhysics {
    let Some(bytes) = resolver.entry_bytes(phy_path) else {
        return LoadedPropPhysics::Missing;
    };
    match parse_phy_bytes(&bytes, &format!("map preview prop physics {phy_path}")) {
        Ok(model) => LoadedPropPhysics::Parsed(Arc::new(model)),
        Err(stats) => LoadedPropPhysics::Unparseable(stats),
    }
}

/// A parsed `.phy` flattened to the shape the prop-physics preview
/// consumes (solid boundaries do not matter to it).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct LoadedPhy {
    pub(super) ledges: Vec<ConvexLedge>,
    pub(super) stats: ReadStats,
}

pub(super) fn parse_phy_bytes(bytes: &[u8], log_context: &str) -> Result<LoadedPhy, ReadStats> {
    let parsed = match vformats::phy::parse_lossy(bytes, &vformats::Limits::default()) {
        Ok(parsed) => parsed,
        Err(error) => {
            log::debug!("{log_context} container invalid: {error}");
            return Err(ReadStats::default());
        }
    };
    let model = LoadedPhy {
        ledges: parsed
            .solids
            .into_iter()
            .flat_map(|solid| solid.ledges)
            .collect(),
        stats: parsed.stats,
    };
    if model.ledges.is_empty() {
        log::debug!(
            "{log_context} unparseable or empty: counts={:?} skips={:?}",
            model.stats.skip_reasons,
            model.stats.skips
        );
        Err(model.stats)
    } else {
        Ok(model)
    }
}

pub(super) fn prop_model_asset_from_loaded(
    loaded: &LoadedPropModel,
    context: &mut PropMaterialContext<'_>,
) -> PropModelAsset {
    let material_indices = loaded
        .model
        .material_names
        .iter()
        .map(|name| prop_material_index(&loaded.model.material_dirs, name, context))
        .collect::<Vec<_>>();

    PropModelAsset {
        model: Arc::clone(&loaded.model),
        material_indices,
        default_triangle_count: loaded.default_triangle_count,
    }
}

pub(super) fn prop_material_index(
    material_dirs: &[String],
    name: &str,
    context: &mut PropMaterialContext<'_>,
) -> usize {
    let key = prop_material_cache_key(material_dirs, name);
    if let Some(index) = context.material_indexes.get(&key).copied() {
        return index;
    }

    let resolved = match context
        .pre_resolved_prop_materials
        .and_then(|materials| materials.get(&key))
    {
        Some(material) => material.clone(),
        None => context.resolver.resolve_primary(material_dirs, name),
    };
    let texture = resolved
        .as_ref()
        .map(|material| Arc::clone(&material.texture));
    if texture.is_some() {
        *context.resolved_material_count = context.resolved_material_count.saturating_add(1);
    }
    if texture
        .as_ref()
        .is_some_and(|texture| texture.is_water_fallback())
    {
        *context.water_fallback_material_count =
            context.water_fallback_material_count.saturating_add(1);
    }
    let index = context.materials.len();
    context.materials.push(MaterialSlot {
        name: name.to_owned(),
        texture,
        texture2: None,
        force_opaque: resolved
            .as_ref()
            .is_none_or(|material| material.force_opaque),
        render_mode: resolved
            .as_ref()
            .map_or(RenderMode::Opaque, |material| material.render_mode),
    });
    context.material_indexes.insert(key, index);
    index
}

pub(super) fn prop_material_cache_key(material_dirs: &[String], name: &str) -> String {
    let mut key = String::from("prop");
    for dir in material_dirs {
        key.push('\0');
        key.push_str(dir);
    }
    key.push('\0');
    key.push_str(name);
    key
}

pub(super) fn bake_prop_placement(
    placement: &StaticPropPlacement,
    model: &PropModelAsset,
    lighting: PropPlacementLighting,
    prop_meshes: &mut BTreeMap<usize, PropBuildMesh>,
) -> bool {
    let skin_table = prop_skin_table(&model.model, placement.skin);
    let mut baked_any = false;

    for mesh in model
        .model
        .meshes
        .iter()
        .filter(|mesh| mesh.bodygroup_choice == 0)
    {
        let material_index = skin_table
            .and_then(|table| table.get(mesh.material_index))
            .map_or(mesh.material_index, |index| usize::from(*index));
        let Some(material_index) = model.material_indices.get(material_index).copied() else {
            continue;
        };
        let Some(base) = prop_meshes
            .get(&material_index)
            .map_or(0, |target| target.vertices.len())
            .checked_add(mesh.vertices.len())
            .and_then(|count| u32::try_from(count).ok())
        else {
            continue;
        };
        let target = prop_meshes
            .entry(material_index)
            .or_insert_with(|| PropBuildMesh {
                vertices: Vec::new(),
                indices: Vec::new(),
                material_index,
                visibility: PropBuildMeshVisibility::default(),
            });
        let base = base.saturating_sub(u32::try_from(mesh.vertices.len()).unwrap_or(u32::MAX));

        target.vertices.extend(mesh.vertices.iter().map(|vertex| {
            let normal = transform_prop_normal(vertex.normal, placement);
            ModelVertex {
                position: transform_prop_position(vertex.position, placement),
                normal,
                uv: vertex.uv,
                lightmap_uv: [0.0; 2],
                color: lighting.evaluate(normal),
                blend_alpha: 0.0,
            }
        }));
        let index_start = target.indices.len();
        target.indices.extend(
            mesh.indices
                .iter()
                .filter_map(|index| base.checked_add(*index)),
        );
        let index_count = target.indices.len().saturating_sub(index_start);
        target
            .visibility
            .push(&placement.visibility, index_start, index_count);
        baked_any = true;
    }

    baked_any
}

pub(super) fn prop_skin_table(model: &ModelData, skin: i32) -> Option<&[u16]> {
    usize::try_from(skin)
        .ok()
        .and_then(|skin| model.skin_tables.get(skin))
        .or_else(|| model.skin_tables.first())
        .map(Vec::as_slice)
}

pub(super) fn transform_prop_position(
    position: [f32; 3],
    placement: &StaticPropPlacement,
) -> [f32; 3] {
    add(
        rotate_prop_vector(scale_vector(position, placement.scale), placement.angles),
        placement.origin,
    )
}

pub(super) fn transform_prop_normal(normal: [f32; 3], placement: &StaticPropPlacement) -> [f32; 3] {
    normalize(rotate_prop_vector(normal, placement.angles))
}

pub(super) fn rotate_prop_vector(vector: [f32; 3], angles: [f32; 3]) -> [f32; 3] {
    let pitch = angles[0].to_radians();
    let yaw = angles[1].to_radians();
    let roll = angles[2].to_radians();
    // Source AngleMatrix is Rz(yaw)·Ry(pitch)·Rx(roll): roll reaches the
    // vector first, yaw last.
    rotate_z(rotate_y(rotate_x(vector, roll), pitch), yaw)
}

pub(super) fn rotate_x(vector: [f32; 3], radians: f32) -> [f32; 3] {
    let (sin, cos) = radians.sin_cos();
    [
        vector[0],
        vector[1] * cos - vector[2] * sin,
        vector[1] * sin + vector[2] * cos,
    ]
}

pub(super) fn rotate_y(vector: [f32; 3], radians: f32) -> [f32; 3] {
    let (sin, cos) = radians.sin_cos();
    [
        vector[0] * cos + vector[2] * sin,
        vector[1],
        -vector[0] * sin + vector[2] * cos,
    ]
}

pub(super) fn rotate_z(vector: [f32; 3], radians: f32) -> [f32; 3] {
    let (sin, cos) = radians.sin_cos();
    [
        vector[0] * cos - vector[1] * sin,
        vector[0] * sin + vector[1] * cos,
        vector[2],
    ]
}

pub(super) fn add(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

pub(super) fn sub(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

pub(super) fn scale_vector(vector: [f32; 3], scale: f32) -> [f32; 3] {
    [vector[0] * scale, vector[1] * scale, vector[2] * scale]
}

pub(super) fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

pub(super) fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

pub(super) fn normalize(vector: [f32; 3]) -> [f32; 3] {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    if length <= f32::EPSILON {
        [0.0; 3]
    } else {
        [vector[0] / length, vector[1] / length, vector[2] / length]
    }
}

pub(super) fn map_mesh_to_model_mesh(
    mesh: &gmpublished_backend::scene::map::MapMesh,
    materials: &[MaterialSlot],
) -> MeshData {
    let (width, height) = material_dimensions(materials, mesh.material_index);
    MeshData {
        vertices: mesh
            .vertices
            .iter()
            .map(|vertex| ModelVertex {
                position: vertex.position,
                normal: vertex.normal,
                uv: normalize_map_uv(vertex.tex_s, vertex.tex_t, width, height),
                lightmap_uv: vertex.lightmap_uv,
                color: [1.0; 3],
                blend_alpha: vertex.blend_alpha,
            })
            .collect(),
        indices: mesh.indices.clone(),
        material_index: mesh.material_index,
        bodygroup: 0,
        bodygroup_choice: 0,
    }
}

pub(super) fn bounds_from_model_meshes(meshes: &[MeshData]) -> Option<([f32; 3], [f32; 3])> {
    let mut positions = meshes
        .iter()
        .flat_map(|mesh| mesh.vertices.iter())
        .map(|vertex| vertex.position);
    let first = positions.next()?;
    let mut min = first;
    let mut max = first;
    for position in positions {
        for axis in 0..3 {
            min[axis] = min[axis].min(position[axis]);
            max[axis] = max[axis].max(position[axis]);
        }
    }
    Some((min, max))
}

pub(super) fn lightmap_status(
    lightmap: Option<&gmpublished_backend::scene::map::LightmapAtlas>,
) -> String {
    lightmap.map_or_else(
        || "lightmap none".to_owned(),
        |lightmap| {
            let source = match lightmap.source {
                gmpublished_backend::scene::map::LightmapSource::Ldr => "LDR",
                gmpublished_backend::scene::map::LightmapSource::Hdr => "HDR",
            };
            format!("lightmap {}x{} ({source})", lightmap.width, lightmap.height)
        },
    )
}

pub(super) fn water_fallback_log_suffix(count: u32) -> String {
    if count == 0 {
        String::new()
    } else {
        format!(", water {count}")
    }
}

pub(super) fn format_mib(bytes: usize) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

pub(super) fn texture_payload_log_suffix(materials: &[MaterialSlot]) -> String {
    let (bc, rgba) = texture_payload_counts(materials);
    format!(" (BC {bc}, RGBA {rgba})")
}

pub(super) fn texture_payload_counts(materials: &[MaterialSlot]) -> (usize, usize) {
    let mut seen = HashSet::new();
    let mut bc = 0_usize;
    let mut rgba = 0_usize;
    for texture in materials
        .iter()
        .flat_map(|material| [material.texture.as_ref(), material.texture2.as_ref()])
        .flatten()
    {
        if !seen.insert(Arc::as_ptr(texture)) {
            continue;
        }
        if texture.is_bc() {
            bc += 1;
        } else {
            rgba += 1;
        }
    }
    (bc, rgba)
}

pub(super) fn render_mode_log_suffix(materials: &[MaterialSlot]) -> String {
    let translucent = materials
        .iter()
        .filter(|material| material.render_mode == RenderMode::Translucent)
        .count();
    let additive = materials
        .iter()
        .filter(|material| material.render_mode == RenderMode::Additive)
        .count();
    format!(", translucent {translucent}, additive {additive}")
}

pub(super) fn log_unresolved_materials(materials: &[MaterialSlot]) {
    let (names, total) = unresolved_material_names_for_debug(materials);
    if total > 0 {
        log::debug!(
            "map unresolved materials {}/{}: {}",
            names.len(),
            total,
            names.join(", ")
        );
    }
}

pub(super) fn unresolved_material_names_for_debug(
    materials: &[MaterialSlot],
) -> (Vec<String>, usize) {
    let names = materials
        .iter()
        .filter(|material| material.texture.is_none())
        .map(|material| material.name.clone())
        .collect::<BTreeSet<_>>();
    let total = names.len();
    (names.into_iter().take(20).collect(), total)
}

pub(super) fn material_dimensions(materials: &[MaterialSlot], index: usize) -> (u32, u32) {
    materials
        .get(index)
        .and_then(|material| material.texture.as_ref())
        .map_or(
            (
                MAP_FALLBACK_TEXTURE_DIMENSION,
                MAP_FALLBACK_TEXTURE_DIMENSION,
            ),
            |texture| texture.original_dimensions(),
        )
}

pub(super) fn normalize_map_uv(tex_s: f32, tex_t: f32, width: u32, height: u32) -> [f32; 2] {
    [tex_s / width.max(1) as f32, tex_t / height.max(1) as f32]
}

pub(super) fn identity_skin_table(material_count: usize) -> Vec<u16> {
    (0..material_count)
        .map(|index| u16::try_from(index).unwrap_or(u16::MAX))
        .collect()
}
