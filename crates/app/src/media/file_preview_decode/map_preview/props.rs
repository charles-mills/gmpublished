#[cfg(test)]
use super::MapAmbientLighting;
use super::compiled_lighting::{
    CompiledPropLighting, PropLightingStats, load_compiled_lighting, prepare_compiler_layout,
};
use super::{
    Arc, BTreeMap, BTreeSet, ContentSourceTier, ConvexHull, ConvexLedge, DoorInstance, DoorSound,
    DoorSoundSourceTier, DoorSoundWave, DoorSounds, Duration, HashMap, HashSet, Instant,
    MAP_PROP_PLACEMENT_CAP, MAP_PROP_TRIANGLE_CAP, MapDoor, MapDoorGeometry, MapMeshClusterRanges,
    MapMeshIndexRange, MapMeshVisibility, MapPropVisibility, MapVisibility, MapWalkPropModel,
    MaterialResolver, MaterialSlot, MaterialTable, MeshData, ModelData, ModelVertex,
    PropMaterialResolveJob, PropPlacementLighting, ReadStats, RenderMode, ResolvedPrimaryMaterial,
    ResolvedSoundReference, SelectedPropPlacement, SkipReason, StaticPropLightingInputs,
    StaticPropPlacement, Vec3, bounds_from_model_meshes, catch_asset_decode, entry_stem,
    load_model_companions, map_mesh_to_model_mesh, parallel_collect, prop_placement_lighting,
};
use rayon::prelude::*;

#[derive(Debug)]
pub(in super::super) struct PropBakeResult {
    pub(in super::super) meshes: Vec<MeshData>,
    pub(in super::super) entity_meshes: Vec<bool>,
    pub(in super::super) mesh_visibility: Vec<MapMeshVisibility>,
    pub(in super::super) placed_count: u32,
    pub(in super::super) skip_stats: PropBakeSkipStats,
    pub(in super::super) timing: PropBakeTiming,
    pub(in super::super) lighting_stats: PropLightingStats,
    pub(in super::super) model_lighting: Vec<crate::media::preview_model::ModelLightingContext>,
}

impl PropBakeResult {
    pub(in super::super) fn skipped_count(&self) -> u32 {
        u32::try_from(self.skip_stats.total()).unwrap_or(u32::MAX)
    }

    pub(in super::super) fn mesh_bytes(&self) -> usize {
        prop_mesh_buffer_bytes(&self.meshes)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(in super::super) struct PropBakeTiming {
    pub(in super::super) load: Duration,
    pub(in super::super) bake: Duration,
}

impl std::ops::Add for PropBakeTiming {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            load: self.load.saturating_add(rhs.load),
            bake: self.bake.saturating_add(rhs.bake),
        }
    }
}

#[derive(Debug, Default)]
pub(in super::super) struct DoorBakeResult {
    pub(in super::super) doors: Vec<DoorInstance>,
    pub(in super::super) prop_door_count: u32,
    pub(in super::super) skipped_prop_door_count: u32,
    pub(in super::super) prop_material_resolutions: BTreeMap<String, PropDoorMaterialResolution>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(in super::super) struct PropDoorMaterialResolution {
    pub(in super::super) door_count: u32,
    pub(in super::super) used_material_slots: BTreeSet<usize>,
    pub(in super::super) resolved_used_material_slots: BTreeSet<usize>,
    pub(in super::super) unresolved_used_material_slots: BTreeSet<usize>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in super::super) struct PropBakeSkipStats {
    pub(in super::super) hidden_render_mode: usize,
    pub(in super::super) placement_cap: usize,
    pub(in super::super) triangle_cap: usize,
    pub(in super::super) invalid_model_path: usize,
    pub(in super::super) load_failure: usize,
    pub(in super::super) no_bakeable_mesh: usize,
}

impl PropBakeSkipStats {
    pub(in super::super) const fn total(self) -> usize {
        self.placement_cap
            + self.hidden_render_mode
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
            hidden_render_mode: self
                .hidden_render_mode
                .saturating_add(rhs.hidden_render_mode),
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
pub(in super::super) struct PropModelAsset {
    pub(in super::super) model: Arc<ModelData>,
    pub(in super::super) material_indices: Vec<usize>,
    pub(in super::super) default_triangle_count: usize,
}

#[derive(Debug)]
pub(in super::super) struct LoadedPropModel {
    pub(in super::super) model: Arc<ModelData>,
    pub(in super::super) default_triangle_count: usize,
    pub(in super::super) physics: LoadedPropPhysics,
    pub(in super::super) collision: Option<Arc<MapWalkPropModel>>,
}

#[derive(Debug)]
pub(in super::super) enum LoadedPropPhysics {
    Parsed(Arc<LoadedPhy>),
    Missing,
    Unparseable(ReadStats),
}

#[derive(Clone, Debug, Default)]
pub(in super::super) struct PropCollisionStats {
    pub(in super::super) solid_placements: usize,
    pub(in super::super) collidable_placements: usize,
    pub(in super::super) parsed_models: usize,
    pub(in super::super) prop_hulls: usize,
    pub(in super::super) memory_bytes: usize,
    pub(in super::super) skipped_not_solid: usize,
    pub(in super::super) skipped_model_load: usize,
    pub(in super::super) skipped_missing_phy: usize,
    pub(in super::super) skipped_unparseable_phy: usize,
    pub(in super::super) skip_reasons: BTreeMap<SkipReason, usize>,
}

#[derive(Debug)]
pub(in super::super) struct PropBuildMesh {
    pub(in super::super) vertices: Vec<ModelVertex>,
    pub(in super::super) indices: Vec<u32>,
    pub(in super::super) material_index: usize,
    pub(in super::super) visibility: PropBuildMeshVisibility,
}

#[derive(Debug, Default)]
pub(in super::super) struct PropBuildMeshVisibility {
    pub(in super::super) always_visible: Vec<MapMeshIndexRange>,
    pub(in super::super) clusters: BTreeMap<u32, Vec<MapMeshIndexRange>>,
    pub(in super::super) next_face: u32,
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

pub(in super::super) fn shifted_range(
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

pub(in super::super) fn refresh_entity_prop_aabb_visibility(
    placements: &mut [StaticPropPlacement],
    entity_start: usize,
    entity_count: usize,
    visibility: Option<&MapVisibility>,
    loaded_model_cache: &HashMap<String, Option<Arc<LoadedPropModel>>>,
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

pub(in super::super) fn prop_model_world_bounds(
    placement: &StaticPropPlacement,
    model: &ModelData,
) -> Option<(Vec3, Vec3)> {
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
pub(in super::super) fn bake_static_props(
    placements: &[StaticPropPlacement],
    resolver: &MaterialResolver,
    table: &mut MaterialTable,
    ambient: &MapAmbientLighting,
) -> PropBakeResult {
    bake_static_props_with_prop_model_loader(
        placements,
        resolver,
        table,
        StaticPropLightingInputs {
            ambient,
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
        &load_prop_model,
    )
}

#[cfg(test)]
pub(in super::super) fn bake_static_props_with_prop_model_loader(
    placements: &[StaticPropPlacement],
    resolver: &MaterialResolver,
    table: &mut MaterialTable,
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
        table,
        &loaded_model_cache,
        Some(&pre_resolved_prop_materials),
        lighting,
        load_started,
    )
}

pub(in super::super) fn bake_map_doors_with_prop_model_loader(
    doors: &[MapDoor],
    resolver: &MaterialResolver,
    table: &mut MaterialTable,
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
        table,
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
                    .map(|mesh| map_mesh_to_model_mesh(mesh, context.table.slots()))
                    .collect::<Vec<_>>();
                baked.doors.push(DoorInstance {
                    brush_collision: door.brush_collision.clone(),
                    class: door.class,
                    origin: door.origin,
                    angles: door.angles,
                    local_bounds_min: door.local_bounds_min,
                    local_bounds_max: door.local_bounds_max,
                    visibility: door.visibility,
                    initial_progress: door.initial_progress,
                    auto_close_after: door.auto_close_after,
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
                    context.table.slots(),
                );
                let model = if let Some(inputs) = lighting.environment_maps {
                    Arc::new(PropModelAsset {
                        material_indices: super::cubemaps::bind_prop_materials(
                            &model,
                            placement,
                            inputs,
                            resolver,
                            context.table,
                        ),
                        model: Arc::clone(&model.model),
                        default_triangle_count: model.default_triangle_count,
                    })
                } else {
                    model
                };
                let model = Arc::new(PropModelAsset {
                    material_indices: prop_draw_materials(
                        &model.material_indices,
                        prop_opacity(placement),
                        context.table,
                    ),
                    model: Arc::clone(&model.model),
                    default_triangle_count: model.default_triangle_count,
                });
                let lighting = prop_placement_lighting(placement, lighting);
                let mut meshes = bake_prop_door_meshes(placement, &model, lighting);
                let Some((local_bounds_min, local_bounds_max)) = bounds_from_model_meshes(&meshes)
                else {
                    baked.skipped_prop_door_count = baked.skipped_prop_door_count.saturating_add(1);
                    continue;
                };
                if matches!(placement.render_mode, 6 | 10) {
                    meshes.clear();
                }
                baked.doors.push(DoorInstance {
                    brush_collision: None,
                    class: door.class,
                    origin: door.origin,
                    angles: door.angles,
                    local_bounds_min,
                    local_bounds_max,
                    visibility: door.visibility,
                    initial_progress: door.initial_progress,
                    auto_close_after: door.auto_close_after,
                    motion: door.motion,
                    sounds,
                    meshes,
                });
            }
        }
    }
    baked
}

pub(in super::super) fn resolve_map_door_sounds(
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

pub(in super::super) fn resolve_door_sound_slot(
    reference: Option<&str>,
    resolver: &MaterialResolver,
    cache: &mut HashMap<String, Option<DoorSound>>,
) -> Option<DoorSound> {
    let reference = reference?;
    let key = MaterialResolver::sound_reference_key(reference)?;
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }
    let resolved = resolver
        .resolve_sound_reference(reference)
        .map(door_sound_from_resolved);
    cache.insert(key, resolved.clone());
    resolved
}

pub(in super::super) fn door_sound_from_resolved(resolved: ResolvedSoundReference) -> DoorSound {
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
    }
}

pub(in super::super) fn bake_static_props_from_loaded_model_cache(
    placements: &[StaticPropPlacement],
    resolver: &MaterialResolver,
    table: &mut MaterialTable,
    loaded_model_cache: &HashMap<String, Option<Arc<LoadedPropModel>>>,
    pre_resolved_prop_materials: Option<&HashMap<String, Option<ResolvedPrimaryMaterial>>>,
    lighting: StaticPropLightingInputs<'_>,
    load_started: Instant,
) -> PropBakeResult {
    let mut model_cache = HashMap::<String, Option<Arc<PropModelAsset>>>::new();
    let mut context = PropMaterialContext {
        resolver,
        pre_resolved_prop_materials,
        table,
    };
    let mut selected = select_static_prop_placements(
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
    for prop in &mut selected.placements {
        if prop.placement.entity
            || prop.placement.static_prop_index.is_none()
            || prop.placement.flags & 0x40 != 0
        {
            selected.lighting_stats.disabled += 1;
            continue;
        }
        match load_compiled_lighting(
            prop.placement,
            &prop.model.model,
            resolver,
            lighting.lightmap_source == Some(gmpublished_domain::scene::map::LightmapSource::Hdr),
        ) {
            Ok(Some(compiled)) => {
                prop.compiled_lighting = Some(Arc::new(compiled));
                selected.lighting_stats.compiled += 1;
            }
            Ok(None) => selected.lighting_stats.missing += 1,
            Err(error) => {
                selected.lighting_stats.incompatible += 1;
                log::warn!(
                    "compiled prop lighting {:?} for {} rejected: {error}",
                    prop.placement.static_prop_index,
                    prop.placement.model_path
                );
            }
        }
    }
    log::debug!(
        "compiled prop lighting: applied {}, missing {}, disabled {}, incompatible {}",
        selected.lighting_stats.compiled,
        selected.lighting_stats.missing,
        selected.lighting_stats.disabled,
        selected.lighting_stats.incompatible
    );
    for prop in &mut selected.placements {
        if let Some(inputs) = lighting.environment_maps {
            let indices = super::cubemaps::bind_prop_materials(
                &prop.model,
                prop.placement,
                inputs,
                resolver,
                context.table,
            );
            if indices != prop.model.material_indices {
                prop.model = Arc::new(PropModelAsset {
                    model: Arc::clone(&prop.model.model),
                    material_indices: indices,
                    default_triangle_count: prop.model.default_triangle_count,
                });
            }
        }
        let material_indices = prop_draw_materials(
            &prop.model.material_indices,
            prop_opacity(prop.placement),
            context.table,
        );
        if material_indices != prop.model.material_indices {
            prop.model = Arc::new(PropModelAsset {
                model: Arc::clone(&prop.model.model),
                material_indices,
                default_triangle_count: prop.model.default_triangle_count,
            });
        }
    }
    let load_timing = load_started.elapsed();
    bake_selected_static_props(selected, load_timing)
}

fn prop_draw_materials(indices: &[usize], opacity: f32, table: &mut MaterialTable) -> Vec<usize> {
    indices
        .iter()
        .map(|&index| {
            let key = format!("prop_draw_alpha\0{index}\0{}", opacity < 1.0);
            if let Some(existing) = table.index_of(&key) {
                return existing;
            }
            let mut slot = table.slots()[index].clone();
            if let Some(surface) = slot.shader.surface() {
                // Model draw modulation replaces material alpha. Keep the
                // authored surface intact and select the resulting depth queue.
                slot.render_mode = surface.draw_render_mode(
                    slot.texture.as_ref().and_then(|texture| texture.flags),
                    opacity,
                );
                slot.force_opaque = slot.render_mode.force_opaque();
            } else if opacity < 1.0
                && matches!(slot.render_mode, RenderMode::Opaque | RenderMode::Cutout)
            {
                slot.render_mode = RenderMode::Translucent;
            }
            if slot == table.slots()[index] {
                return index;
            }
            table.push(&key, slot)
        })
        .collect()
}

#[cfg(test)]
pub(in super::super) fn bake_static_props_with_loaded_model_cache(
    placements: &[StaticPropPlacement],
    resolver: &MaterialResolver,
    table: &mut MaterialTable,
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
        table,
        loaded_model_cache,
        pre_resolved_prop_materials.as_ref(),
        StaticPropLightingInputs {
            ambient,
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
        load_started,
    )
}

#[cfg(test)]
pub(in super::super) fn bake_static_props_with_loader(
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

pub(in super::super) fn bake_selected_static_props(
    selected: SelectedPropPlacements<'_>,
    load_timing: Duration,
) -> PropBakeResult {
    bake_selected_with(
        selected,
        load_timing,
        bake_selected_prop_placements_parallel,
    )
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "selected.skip_stats is moved out into the returned PropBakeResult, which requires owning it"
)]
fn bake_selected_with(
    selected: SelectedPropPlacements<'_>,
    load_timing: Duration,
    bake: impl FnOnce(&[SelectedPropPlacement<'_>]) -> BTreeMap<(usize, bool), PropBuildMesh>,
) -> PropBakeResult {
    let bake_started = Instant::now();
    let prop_meshes = bake(&selected.placements);
    let bake_timing = bake_started.elapsed();
    let entity_meshes = prop_meshes.keys().map(|(_, entity)| *entity).collect();
    let (meshes, mesh_visibility) = prop_meshes_to_mesh_data(prop_meshes);

    PropBakeResult {
        model_lighting: selected
            .placements
            .iter()
            .map(|selected| {
                let sample_point = crate::media::preview_model::placement_sample_point(
                    &selected.model.model,
                    selected.placement,
                );
                crate::media::preview_model::ModelLightingContext {
                    mdl_flags: selected.model.model.mdl_flags,
                    compiled_lighting: selected.compiled_lighting.is_some(),
                    model_path: selected.placement.model_path.clone(),
                    origin: selected.placement.origin,
                    lighting_origin: sample_point
                        .as_ref()
                        .copied()
                        .unwrap_or(selected.placement.origin),
                    entity: selected.placement.entity,
                    sample_point,
                    static_world_bounds: crate::media::preview_model::placement_static_bounds(
                        &selected.model.model,
                        selected.placement,
                    ),
                    lighting_origin_reference: selected.placement.lighting_origin_reference.clone(),
                    cpu_seed: Ok(None),
                    diffuse_modulation: selected.placement.diffuse_modulation,
                    render_mode: selected.placement.render_mode,
                    ambient_cube: selected.lighting.ambient_cube,
                    lights: Vec::new(),
                }
            })
            .collect(),
        entity_meshes,
        meshes,
        mesh_visibility,
        placed_count: u32::try_from(selected.placements.len()).unwrap_or(u32::MAX),
        skip_stats: selected.skip_stats,
        lighting_stats: selected.lighting_stats,
        timing: PropBakeTiming {
            load: load_timing,
            bake: bake_timing,
        },
    }
}

#[cfg(test)]
pub(in super::super) fn bake_static_props_with_loader_serial(
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
    bake_selected_with(
        selected,
        load_started.elapsed(),
        bake_selected_prop_placements_serial,
    )
}

#[derive(Debug)]
pub(in super::super) struct SelectedPropPlacements<'a> {
    pub(in super::super) placements: Vec<SelectedPropPlacement<'a>>,
    pub(in super::super) skip_stats: PropBakeSkipStats,
    pub(in super::super) lighting_stats: PropLightingStats,
}

pub(in super::super) fn select_static_prop_placements<'a>(
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
        if placement.entity && matches!(placement.render_mode, 6 | 10) {
            skip_stats.hidden_render_mode += 1;
            continue;
        }
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
                lighting_context: u32::try_from(selected.len() + 1)
                    .expect("bounded placement context"),
                placement,
                model,
                lighting: prop_placement_lighting(placement, lighting),
                compiled_lighting: None,
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
        lighting_stats: PropLightingStats::default(),
    }
}

pub(in super::super) fn prop_mesh_buffer_bytes(meshes: &[MeshData]) -> usize {
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

pub(in super::super) fn prop_meshes_to_mesh_data(
    prop_meshes: BTreeMap<(usize, bool), PropBuildMesh>,
) -> (Vec<MeshData>, Vec<MapMeshVisibility>) {
    prop_meshes
        .into_values()
        .map(|mesh| {
            (
                MeshData {
                    geometry: crate::media::preview_model::MeshGeometry::Model,
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

pub(in super::super) fn bake_selected_prop_placements_parallel(
    selected: &[SelectedPropPlacement<'_>],
) -> BTreeMap<(usize, bool), PropBuildMesh> {
    if selected.is_empty() {
        return BTreeMap::new();
    }

    let worker_count = rayon::current_num_threads().min(selected.len()).max(1);
    if worker_count == 1 {
        return bake_selected_prop_placements_serial(selected);
    }

    let chunk_size = selected.len().div_ceil(worker_count);
    selected
        .par_chunks(chunk_size)
        .map(bake_selected_prop_placements_serial)
        .reduce(BTreeMap::new, |mut merged, chunk| {
            merge_prop_meshes(&mut merged, chunk);
            merged
        })
}

pub(in super::super) fn bake_selected_prop_placements_serial(
    selected: &[SelectedPropPlacement<'_>],
) -> BTreeMap<(usize, bool), PropBuildMesh> {
    let mut prop_meshes = BTreeMap::<(usize, bool), PropBuildMesh>::new();
    for selected in selected {
        let baked = bake_prop_placement_with_compiled(
            selected.placement,
            &selected.model,
            selected.lighting,
            selected.compiled_lighting.as_deref(),
            selected.lighting_context,
            &mut prop_meshes,
        );
        debug_assert!(baked, "selected prop placement should bake");
    }
    prop_meshes
}

pub(in super::super) fn merge_prop_meshes(
    target_meshes: &mut BTreeMap<(usize, bool), PropBuildMesh>,
    source_meshes: BTreeMap<(usize, bool), PropBuildMesh>,
) {
    for ((material_index, entity), source) in source_meshes {
        let target = target_meshes
            .entry((material_index, entity))
            .or_insert_with(|| PropBuildMesh {
                vertices: Vec::new(),
                indices: Vec::new(),
                material_index,
                visibility: PropBuildMeshVisibility::default(),
            });
        let Some(base) = u32::try_from(target.vertices.len()).ok() else {
            continue;
        };
        // Visibility ranges index the index buffer. Shift by its length, not the vertex count.
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

pub(in super::super) fn prop_placement_has_bakeable_mesh(
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

pub(in super::super) fn record_prop_door_material_resolution(
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

pub(in super::super) fn prop_model_used_material_slots(
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

pub(in super::super) fn log_prop_door_material_resolution(
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

pub(in super::super) fn bake_prop_door_meshes(
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
                    let authored_modulation = placement.diffuse_modulation != [255; 4];
                    ModelVertex {
                        position: vertex.position,
                        normal: vertex.normal,
                        tangent_s: vertex.tangent_s,
                        tangent_t: vertex.tangent_t,
                        tangent_sign: vertex.tangent_sign,
                        lighting_context: 0,
                        uv: vertex.uv,
                        lightmap_uv: [0.0; 2],
                        lightmap_bump_step: 0.0,
                        color: if authored_modulation {
                            prop_diffuse_tint(placement)
                        } else {
                            lighting.evaluate(closed_normal)
                        },
                        blend_alpha: 0.0,
                        opacity: prop_opacity(placement),
                        illumination: if authored_modulation {
                            lighting.evaluate(closed_normal) * 2.0
                        } else {
                            Vec3::splat(1.0)
                        },
                        lighting_mode: 8 | u32::from(authored_modulation),
                    }
                })
                .collect::<Vec<_>>();
            (!vertices.is_empty() && !mesh.indices.is_empty()).then(|| MeshData {
                geometry: crate::media::preview_model::MeshGeometry::Model,
                vertices,
                indices: mesh.indices.clone(),
                material_index,
                bodygroup: 0,
                bodygroup_choice: 0,
            })
        })
        .collect()
}

pub(in super::super) struct PropMaterialContext<'a> {
    pub(in super::super) resolver: &'a MaterialResolver,
    pub(in super::super) pre_resolved_prop_materials:
        Option<&'a HashMap<String, Option<ResolvedPrimaryMaterial>>>,
    pub(in super::super) table: &'a mut MaterialTable,
}

pub(in super::super) fn cached_prop_model(
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

pub(in super::super) fn load_unique_prop_models_parallel(
    placements: &[StaticPropPlacement],
    resolver: &MaterialResolver,
    load_model: &(impl Fn(&str, &MaterialResolver) -> Option<LoadedPropModel> + Sync),
) -> HashMap<String, Option<Arc<LoadedPropModel>>> {
    let model_paths = unique_prop_model_paths(placements);
    parallel_collect(&model_paths, |model_path| {
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

pub(in super::super) fn unique_prop_model_paths(placements: &[StaticPropPlacement]) -> Vec<String> {
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

pub(in super::super) fn pre_resolve_prop_materials(
    placements: &[StaticPropPlacement],
    loaded_model_cache: &HashMap<String, Option<Arc<LoadedPropModel>>>,
    resolver: &MaterialResolver,
) -> HashMap<String, Option<ResolvedPrimaryMaterial>> {
    let jobs = unique_prop_material_resolve_jobs(placements, loaded_model_cache);
    parallel_collect(&jobs, |job| {
        (
            job.key.clone(),
            resolver.resolve_primary(&job.material_dirs, &job.name),
        )
    })
    .into_iter()
    .collect()
}

pub(in super::super) fn unique_prop_material_resolve_jobs(
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

pub(in super::super) fn load_prop_model(
    model_path: &str,
    resolver: &MaterialResolver,
) -> Option<LoadedPropModel> {
    load_prop_model_with_options(model_path, resolver, super::super::SOURCE_MODEL_ASSEMBLY)
}

pub(in super::super) fn load_prop_model_with_options(
    model_path: &str,
    resolver: &MaterialResolver,
    options: vformats::mdl::ModelAssemblyOptions,
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

    let mut model = super::super::load_model_catching_panic_with_options(
        &format!("static prop model {model_path}"),
        &mdl_bytes,
        &vvd_bytes,
        &vtx_bytes,
        options,
    )?;
    let compiler_bytes = resolver.entry_bytes(&format!("{stem}.dx80.vtx"));
    if let Err(reason) = prepare_compiler_layout(&mut model, &vtx_bytes, compiler_bytes.as_deref())
    {
        log::debug!("compiled prop lighting layout unavailable for {model_path}: {reason}");
    }
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
                    vertices: ledge.vertices.iter().copied().map(Vec3::from).collect(),
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

pub(in super::super) fn load_prop_physics(
    phy_path: &str,
    resolver: &MaterialResolver,
) -> LoadedPropPhysics {
    let Some(bytes) = resolver.entry_bytes(phy_path) else {
        return LoadedPropPhysics::Missing;
    };
    match parse_phy_bytes(&bytes, &format!("map preview prop physics {phy_path}")) {
        Ok(model) => LoadedPropPhysics::Parsed(Arc::new(model)),
        Err(stats) => LoadedPropPhysics::Unparseable(stats),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(in super::super) struct LoadedPhy {
    pub(in super::super) ledges: Vec<ConvexLedge>,
    pub(in super::super) stats: ReadStats,
}

pub(in super::super) fn parse_phy_bytes(
    bytes: &[u8],
    log_context: &str,
) -> Result<LoadedPhy, ReadStats> {
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

pub(in super::super) fn prop_model_asset_from_loaded(
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

pub(in super::super) fn prop_material_index(
    material_dirs: &[String],
    name: &str,
    context: &mut PropMaterialContext<'_>,
) -> usize {
    let key = prop_material_cache_key(material_dirs, name);
    if let Some(index) = context.table.index_of(&key) {
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
    context.table.push(
        &key,
        MaterialSlot {
            shader: resolved
                .as_ref()
                .map_or_else(Default::default, |material| material.shader.clone()),
            name: name.to_owned(),
            texture,
            texture2: None,
            force_opaque: resolved
                .as_ref()
                .is_none_or(|material| material.force_opaque),
            render_mode: resolved
                .as_ref()
                .map_or(RenderMode::Opaque, |material| material.render_mode),
        },
    )
}

pub(in super::super) fn prop_material_cache_key(material_dirs: &[String], name: &str) -> String {
    let mut key = String::from("prop");
    for dir in material_dirs {
        key.push('\0');
        key.push_str(dir);
    }
    key.push('\0');
    key.push_str(name);
    key
}

#[cfg(test)]
pub(in super::super) fn bake_prop_placement(
    placement: &StaticPropPlacement,
    model: &PropModelAsset,
    lighting: PropPlacementLighting,
    prop_meshes: &mut BTreeMap<(usize, bool), PropBuildMesh>,
) -> bool {
    bake_prop_placement_with_compiled(placement, model, lighting, None, 0, prop_meshes)
}

fn bake_prop_placement_with_compiled(
    placement: &StaticPropPlacement,
    model: &PropModelAsset,
    lighting: PropPlacementLighting,
    compiled: Option<&CompiledPropLighting>,
    lighting_context: u32,
    prop_meshes: &mut BTreeMap<(usize, bool), PropBuildMesh>,
) -> bool {
    let skin_table = prop_skin_table(&model.model, placement.skin);
    let mut baked_any = false;

    for (mesh_index, mesh) in model
        .model
        .meshes
        .iter()
        .enumerate()
        .filter(|(_, mesh)| mesh.bodygroup_choice == 0)
    {
        let material_index = skin_table
            .and_then(|table| table.get(mesh.material_index))
            .map_or(mesh.material_index, |index| usize::from(*index));
        let Some(material_index) = model.material_indices.get(material_index).copied() else {
            continue;
        };
        let Some(base) = prop_meshes
            .get(&(material_index, placement.entity))
            .map_or(0, |target| target.vertices.len())
            .checked_add(mesh.vertices.len())
            .and_then(|count| u32::try_from(count).ok())
        else {
            continue;
        };
        let target = prop_meshes
            .entry((material_index, placement.entity))
            .or_insert_with(|| PropBuildMesh {
                vertices: Vec::new(),
                indices: Vec::new(),
                material_index,
                visibility: PropBuildMeshVisibility::default(),
            });
        let base = base.saturating_sub(u32::try_from(mesh.vertices.len()).unwrap_or(u32::MAX));

        target.vertices.extend(
            mesh.vertices
                .iter()
                .enumerate()
                .map(|(vertex_index, vertex)| {
                    let normal = transform_prop_normal(vertex.normal, placement);
                    let compiled_illumination = compiled
                        .and_then(|compiled| compiled.illumination.get(mesh_index))
                        .and_then(|mesh| mesh.get(vertex_index))
                        .copied();
                    let authored_modulation = placement.diffuse_modulation != [255; 4];
                    let illumination = compiled_illumination
                        .or_else(|| authored_modulation.then(|| lighting.evaluate(normal) * 2.0));
                    ModelVertex {
                        position: transform_prop_position(vertex.position, placement),
                        normal,
                        tangent_s: vertex.tangent_s.rotate_source(placement.angles),
                        tangent_t: vertex.tangent_t.rotate_source(placement.angles),
                        tangent_sign: vertex.tangent_sign,
                        lighting_context,
                        uv: vertex.uv,
                        lightmap_uv: [0.0; 2],
                        lightmap_bump_step: 0.0,
                        color: illumination.map_or_else(
                            || lighting.evaluate(normal),
                            |_| prop_diffuse_tint(placement),
                        ),
                        blend_alpha: 0.0,
                        opacity: prop_opacity(placement),
                        illumination: illumination.unwrap_or_else(|| Vec3::splat(1.0)),
                        lighting_mode: u32::from(illumination.is_some())
                            | (u32::from(!placement.entity) << 1)
                            | 8,
                    }
                }),
        );
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

fn prop_diffuse_tint(placement: &StaticPropPlacement) -> Vec3 {
    Vec3::new(
        f32::from(placement.diffuse_modulation[0]),
        f32::from(placement.diffuse_modulation[1]),
        f32::from(placement.diffuse_modulation[2]),
    )
    .map(|channel| channel / 255.0)
}

fn prop_opacity(placement: &StaticPropPlacement) -> f32 {
    if placement.entity && placement.render_mode != 0 {
        f32::from(placement.diffuse_modulation[3]) / 255.0
    } else {
        1.0
    }
}

pub(in super::super) fn prop_skin_table(model: &ModelData, skin: i32) -> Option<&[u16]> {
    usize::try_from(skin)
        .ok()
        .and_then(|skin| model.skin_tables.get(skin))
        .or_else(|| model.skin_tables.first())
        .map(Vec::as_slice)
}

pub(in super::super) fn transform_prop_position(
    position: Vec3,
    placement: &StaticPropPlacement,
) -> Vec3 {
    (position * placement.scale).rotate_source(placement.angles) + placement.origin
}

pub(in super::super) fn transform_prop_normal(
    normal: Vec3,
    placement: &StaticPropPlacement,
) -> Vec3 {
    normal.rotate_source(placement.angles)
}

#[cfg(test)]
mod compiled_bake_tests {
    use super::*;
    use crate::media::file_preview_decode::tests::{prop_placement, test_prop_asset};
    use gmpublished_domain::scene::map::AmbientCube;

    fn fallback() -> PropPlacementLighting {
        PropPlacementLighting {
            ambient_cube: AmbientCube {
                colors: [Vec3::splat(0.25); 6],
            },
            sun: None,
        }
    }

    #[test]
    fn compiled_black_illumination_keeps_raw_instance_rgb_and_static_alpha_override() {
        let mut placement = prop_placement(
            "models/test/chair.mdl",
            Vec3::new(10.0, 20.0, 30.0),
            Vec3::ZERO,
            0,
        );
        placement.scale = 2.5;
        placement.diffuse_modulation = [128, 64, 255, 96];
        let model = test_prop_asset(1, vec![0, 1]);
        let compiled = CompiledPropLighting {
            illumination: vec![vec![Vec3::ZERO; 3]],
        };
        let mut baked = BTreeMap::new();
        assert!(bake_prop_placement_with_compiled(
            &placement,
            &model,
            fallback(),
            Some(&compiled),
            0,
            &mut baked
        ));
        let vertex = baked[&(0, false)].vertices[0];
        assert_eq!(vertex.position, Vec3::new(12.5, 20.0, 30.0));
        assert_eq!(vertex.illumination, Vec3::ZERO);
        assert_eq!(vertex.lighting_mode, 11);
        assert_eq!(vertex.color.x(), 128.0 / 255.0);
        assert_eq!(vertex.color.y(), 64.0 / 255.0);
        assert_eq!(vertex.color.z(), 1.0);
        assert_eq!(vertex.opacity, 1.0);
    }

    #[test]
    fn compiled_instances_keep_independent_lighting_in_parallel_bakes() {
        let placements = [
            prop_placement("models/test/chair.mdl", Vec3::ZERO, Vec3::ZERO, 0),
            prop_placement(
                "models/test/chair.mdl",
                Vec3::new(10.0, 0.0, 0.0),
                Vec3::ZERO,
                0,
            ),
        ];
        let model = Arc::new(test_prop_asset(1, vec![0, 1]));
        let selected = placements
            .iter()
            .enumerate()
            .map(|(index, placement)| SelectedPropPlacement {
                placement,
                model: Arc::clone(&model),
                lighting: fallback(),
                lighting_context: index as u32 + 1,
                compiled_lighting: Some(Arc::new(CompiledPropLighting {
                    illumination: vec![vec![Vec3::splat(index as f32); 3]],
                })),
            })
            .collect::<Vec<_>>();
        let serial = bake_selected_prop_placements_serial(&selected);
        let parallel = bake_selected_prop_placements_parallel(&selected);
        assert_eq!(serial[&(0, false)].vertices, parallel[&(0, false)].vertices);
        assert_eq!(serial[&(0, false)].indices, parallel[&(0, false)].indices);
        assert_eq!(parallel[&(0, false)].vertices[0].illumination, Vec3::ZERO);
        assert_eq!(
            parallel[&(0, false)].vertices[3].illumination,
            Vec3::splat(1.0)
        );
        assert_eq!(parallel[&(0, false)].vertices[0].color, Vec3::splat(1.0));
        assert_eq!(parallel[&(0, false)].vertices[0].lighting_context, 1);
        assert_eq!(parallel[&(0, false)].vertices[3].lighting_context, 2);
    }

    #[test]
    fn ordinary_fallback_keeps_its_previous_vertex_contract() {
        let placement = prop_placement("models/test/chair.mdl", Vec3::ZERO, Vec3::ZERO, 0);
        let model = test_prop_asset(1, vec![0, 1]);
        let mut baked = BTreeMap::new();
        assert!(bake_prop_placement_with_compiled(
            &placement,
            &model,
            fallback(),
            None,
            0,
            &mut baked
        ));
        let vertex = baked[&(0, false)].vertices[0];
        assert_eq!(vertex.color, Vec3::splat(0.25));
        assert_eq!(vertex.illumination, Vec3::splat(1.0));
        assert_eq!(vertex.lighting_mode, 10);
    }

    #[test]
    fn translucent_prop_variants_preserve_original_materials_and_share_slots() {
        let mut table = MaterialTable::default();
        let original = table.push(
            "fixture",
            MaterialSlot {
                shader: Default::default(),
                name: "fixture".to_owned(),
                texture: None,
                texture2: None,
                force_opaque: true,
                render_mode: RenderMode::Opaque,
            },
        );
        let translucent = prop_draw_materials(&[original], 0.5, &mut table)[0];
        assert_ne!(original, translucent);
        assert_eq!(table.slots()[original].render_mode, RenderMode::Opaque);
        assert_eq!(
            table.slots()[translucent].render_mode,
            RenderMode::Translucent
        );
        assert!(table.slots()[translucent].force_opaque);
        assert_eq!(
            prop_draw_materials(&[original], 0.5, &mut table),
            [translucent]
        );
    }

    #[test]
    fn model_draw_alpha_replaces_material_alpha_without_changing_shared_world_state() {
        use crate::bridge::materials::{MaterialShader, SurfaceMaterial};
        let mut table = MaterialTable::default();
        let original = table.push(
            "scalar_alpha",
            MaterialSlot {
                shader: MaterialShader::VertexLit(Arc::new(SurfaceMaterial {
                    alpha: 0.5,
                    ..SurfaceMaterial::default()
                })),
                name: "scalar_alpha".to_owned(),
                texture: None,
                texture2: None,
                force_opaque: false,
                render_mode: RenderMode::Translucent,
            },
        );
        let opaque = prop_draw_materials(&[original], 1.0, &mut table)[0];
        let blended = prop_draw_materials(&[original], 0.5, &mut table)[0];
        assert_eq!(table.slots()[original].render_mode, RenderMode::Translucent);
        assert_eq!(table.slots()[opaque].render_mode, RenderMode::Opaque);
        assert_eq!(table.slots()[blended].render_mode, RenderMode::Translucent);
        assert!(table.slots()[opaque].force_opaque);
        assert!(!table.slots()[blended].force_opaque);
        assert_eq!(table.slots()[opaque].shader.surface().unwrap().alpha, 0.5);
        assert_eq!(prop_draw_materials(&[original], 1.0, &mut table), [opaque]);
    }
}
