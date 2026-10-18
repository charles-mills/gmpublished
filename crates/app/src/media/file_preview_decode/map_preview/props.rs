#[cfg(test)]
use super::MapAmbientLighting;
#[cfg(test)]
use super::compiled_lighting::CompiledPropLighting;
use super::compiled_lighting::{
    PropLightingStats, load_compiled_lighting, prepare_compiler_layout,
};
#[cfg(test)]
use super::prop_geometry::bake_selected_prop_placements_serial;
use super::prop_geometry::{
    self, BakedGeometry, GeometryComponent, GeometryCounts, GeometryError, PropGeometryReport,
    bake_selected_prop_placements_parallel,
};
use super::{
    Arc, BTreeMap, BTreeSet, ContentSourceTier, ConvexHull, ConvexLedge, DoorInstance, DoorSound,
    DoorSoundSourceTier, DoorSoundWave, DoorSounds, Duration, HashMap, HashSet, Instant,
    LegacyPropLightingFallback, MAP_PROP_PLACEMENT_CAP, MAP_PROP_TRIANGLE_CAP, MapDoor,
    MapDoorGeometry, MapMeshClusterRanges, MapMeshIndexRange, MapMeshVisibility, MapPropVisibility,
    MapVisibility, MapWalkPropModel, MaterialResolution, MaterialResolver, MaterialSlot,
    MaterialTable, MeshData, ModelData, ModelVertex, PropMaterialResolveJob, ReadStats, RenderMode,
    ResolvedPrimaryMaterial, ResolvedSoundReference, SkipReason, StaticPropLightingInputs,
    StaticPropPlacement, Vec3, catch_asset_decode, entry_stem, legacy_prop_lighting_fallback,
    load_model_companions, map_mesh_to_model_mesh, parallel_collect,
};
use crate::media::preview_model::{
    MapRenderableBoundsError, MapRenderableRange, MapRenderableSource, PreparedMapRenderable,
    PreparedMapRenderableBounds,
};
use gmpublished_domain::scene::map::MapBounds;

#[derive(Debug)]
pub(in super::super) struct PropBakeResult {
    pub(in super::super) model_animations: Vec<crate::media::preview_model::ModelAnimationInstance>,
    pub(in super::super) model_animation_issues:
        Vec<crate::media::preview_model::ModelAnimationIssue>,
    pub(in super::super) geometry: PropGeometryReport,
    pub(in super::super) renderables: Vec<PreparedMapRenderable>,
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
    pub(in super::super) geometry: PropGeometryReport,
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
    pub(in super::super) hidden_placement: usize,
    pub(in super::super) placement_cap: usize,
    pub(in super::super) triangle_cap: usize,
    pub(in super::super) invalid_model_path: usize,
    pub(in super::super) load_failure: usize,
    pub(in super::super) no_bakeable_mesh: usize,
    pub(in super::super) geometry_rejected: usize,
}

impl PropBakeSkipStats {
    pub(in super::super) const fn total(self) -> usize {
        self.placement_cap
            + self.hidden_placement
            + self.triangle_cap
            + self.invalid_model_path
            + self.load_failure
            + self.no_bakeable_mesh
            + self.geometry_rejected
    }

    const fn has_skips(self) -> bool {
        self.total() > 0
    }
}

impl std::ops::Add for PropBakeSkipStats {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            hidden_placement: self.hidden_placement.saturating_add(rhs.hidden_placement),
            placement_cap: self.placement_cap.saturating_add(rhs.placement_cap),
            triangle_cap: self.triangle_cap.saturating_add(rhs.triangle_cap),
            invalid_model_path: self
                .invalid_model_path
                .saturating_add(rhs.invalid_model_path),
            load_failure: self.load_failure.saturating_add(rhs.load_failure),
            no_bakeable_mesh: self.no_bakeable_mesh.saturating_add(rhs.no_bakeable_mesh),
            geometry_rejected: self.geometry_rejected.saturating_add(rhs.geometry_rejected),
        }
    }
}

#[derive(Debug)]
pub(in super::super) struct PropModelAsset {
    pub(in super::super) model: Arc<ModelData>,
    pub(in super::super) material_indices: BTreeMap<usize, usize>,
}

#[derive(Debug)]
pub(in super::super) struct LoadedPropModel {
    pub(in super::super) model: Arc<ModelData>,
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
    pub(super) renderable_ranges: Vec<PropBuildRenderableRange>,
}

#[derive(Debug)]
pub(super) struct PropBuildRenderableRange {
    pub(super) source_index: usize,
    pub(super) draw_order: usize,
    pub(super) vertex_start: u32,
    pub(super) start: u32,
    pub(super) count: u32,
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct PropBakePlacement {
    source_index: usize,
    lighting_context: u32,
}

#[derive(Debug, Default)]
pub(in super::super) struct PropBuildMeshVisibility {
    pub(in super::super) always_visible: Vec<MapMeshIndexRange>,
    pub(in super::super) clusters: BTreeMap<u32, Vec<MapMeshIndexRange>>,
}

impl PropBuildMeshVisibility {
    pub(super) fn push(&mut self, visibility: &MapPropVisibility, range: MapMeshIndexRange) {
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
    if placement.animation.is_some() && model.animation.is_some() {
        let bounds = prop_renderable_bounds(placement, model).ok()?.world;
        return Some((bounds.min, bounds.max));
    }
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

fn prop_renderable_bounds(
    placement: &StaticPropPlacement,
    model: &ModelData,
) -> Result<PreparedMapRenderableBounds, MapRenderableBoundsError> {
    let (min, max) = if placement.entity {
        animated_local_bounds(placement, model)
            .ok_or(MapRenderableBoundsError::EntitySequenceUnavailable)?
    } else if model.view_min == Vec3::ZERO && model.view_max == Vec3::ZERO {
        (model.hull_min, model.hull_max)
    } else {
        (model.view_min, model.view_max)
    };
    if !min.is_finite()
        || !max.is_finite()
        || !placement.origin.is_finite()
        || !placement.angles.is_finite()
        || !placement.scale.is_finite()
        || placement.scale <= 0.0
        || (0..3).any(|axis| min[axis] > max[axis])
    {
        return Err(MapRenderableBoundsError::InvalidCompiledBounds);
    }
    let local = MapBounds {
        min: min * placement.scale,
        max: max * placement.scale,
    };
    let center = ((local.min + local.max) * 0.5).rotate_source(placement.angles) + placement.origin;
    let extent = (local.max - local.min) * 0.5;
    let axes = [
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(0.0, 0.0, 1.0),
    ]
    .map(|axis| axis.rotate_source(placement.angles));
    let world_extent = Vec3::new(
        axes[0][0].abs() * extent[0] + axes[1][0].abs() * extent[1] + axes[2][0].abs() * extent[2],
        axes[0][1].abs() * extent[0] + axes[1][1].abs() * extent[1] + axes[2][1].abs() * extent[2],
        axes[0][2].abs() * extent[0] + axes[1][2].abs() * extent[1] + axes[2][2].abs() * extent[2],
    );
    let world = MapBounds {
        min: center - world_extent,
        max: center + world_extent,
    };
    if !world.min.is_finite() || !world.max.is_finite() {
        return Err(MapRenderableBoundsError::InvalidCompiledBounds);
    }
    Ok(PreparedMapRenderableBounds { local, world })
}

fn animated_local_bounds(
    placement: &StaticPropPlacement,
    model: &ModelData,
) -> Option<(Vec3, Vec3)> {
    placement.animation.as_ref()?;
    crate::media::preview_model::animation_local_bounds(model)
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
    bake_static_props_from_loaded_model_cache(
        placements,
        resolver,
        table,
        &loaded_model_cache,
        lighting,
        load_started,
    )
}

#[cfg(test)]
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
            MapDoorGeometry::Prop { placement } => Some(placement.as_ref().clone()),
            MapDoorGeometry::Brush { .. } => None,
        })
        .collect::<Vec<_>>();
    let loaded_model_cache =
        load_unique_prop_models_parallel(&prop_placements, resolver, load_model);
    bake_map_doors_from_loaded_model_cache(doors, resolver, table, lighting, &loaded_model_cache)
}

struct PreparedPropDoor {
    model: Arc<LoadedPropModel>,
    geometry: prop_geometry::DoorGeometryPlan,
    hidden: bool,
}

pub(super) fn bake_map_doors_from_loaded_model_cache(
    doors: &[MapDoor],
    resolver: &MaterialResolver,
    table: &mut MaterialTable,
    lighting: StaticPropLightingInputs<'_>,
    loaded_model_cache: &HashMap<String, Option<Arc<LoadedPropModel>>>,
) -> DoorBakeResult {
    let mut planner = prop_geometry::RawGeometryPlanner::default();
    let mut prepared = doors
        .iter()
        .enumerate()
        .filter_map(|(index, door)| {
            let MapDoorGeometry::Prop { placement } = &door.geometry else {
                return None;
            };
            let model = loaded_model_cache
                .get(&placement.model_path)
                .cloned()
                .flatten()?;
            let geometry = planner.door(placement, &model.model, |slot| {
                slot < model.model.material_names.len()
            });
            Some((
                index,
                geometry.map(|geometry| PreparedPropDoor {
                    model,
                    geometry,
                    hidden: !placement.initially_visible || matches!(placement.render_mode, 6 | 10),
                }),
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let pre_resolved_prop_materials = pre_resolve_prop_materials(
        prepared
            .values()
            .filter_map(|prepared| prepared.as_ref().ok())
            .filter(|prepared| !prepared.hidden)
            .map(|prepared| (prepared.model.as_ref(), prepared.geometry.geometry.as_ref())),
        resolver,
    );
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
                    source_index: door_index,
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
                let prepared = match prepared.remove(&door_index) {
                    Some(Ok(prepared)) => prepared,
                    Some(Err(reason)) => {
                        baked.geometry.rejected.push(
                            crate::media::preview_model::GeometryDiagnostic {
                                placement_index: door_index,
                                reason,
                            },
                        );
                        baked.skipped_prop_door_count =
                            baked.skipped_prop_door_count.saturating_add(1);
                        continue;
                    }
                    None => {
                        baked.skipped_prop_door_count =
                            baked.skipped_prop_door_count.saturating_add(1);
                        continue;
                    }
                };
                let PreparedPropDoor {
                    model: loaded,
                    geometry,
                    hidden,
                } = prepared;
                let slots = if hidden {
                    BTreeSet::new()
                } else {
                    geometry.geometry.used_material_slots()
                };
                let model = Arc::new(prop_model_asset_from_loaded(&loaded, slots, &mut context));
                if !hidden {
                    record_prop_door_material_resolution(
                        &mut baked.prop_material_resolutions,
                        placement,
                        &model,
                        &geometry.geometry,
                        context.table.slots(),
                    );
                }
                let model = if let Some(inputs) = lighting.environment_maps.filter(|_| !hidden) {
                    Arc::new(PropModelAsset {
                        material_indices: super::cubemaps::bind_prop_materials(
                            &model,
                            placement,
                            inputs,
                            resolver,
                            context.table,
                        ),
                        model: Arc::clone(&model.model),
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
                });
                let lighting = legacy_prop_lighting_fallback(placement, lighting);
                let prepared = (|| {
                    let counts = if hidden {
                        GeometryCounts::default()
                    } else {
                        geometry.geometry.counts()
                    };
                    baked.geometry.planned = baked.geometry.planned.checked_add(counts)?;
                    let admitted = baked.geometry.admitted.checked_add(counts)?;
                    let meshes = if hidden {
                        Vec::new()
                    } else {
                        let parts =
                            prop_geometry::bind_geometry(placement, &model, &geometry.geometry)?;
                        bake_prop_door_meshes(
                            placement,
                            &geometry.geometry.source,
                            lighting,
                            &parts,
                        )?
                    };
                    baked.geometry.admitted = admitted;
                    Ok((meshes, geometry.bounds.min, geometry.bounds.max))
                })();
                let (meshes, local_bounds_min, local_bounds_max) = match prepared {
                    Ok(prepared) => prepared,
                    Err(reason) => {
                        baked.geometry.rejected.push(
                            crate::media::preview_model::GeometryDiagnostic {
                                placement_index: door_index,
                                reason,
                            },
                        );
                        baked.skipped_prop_door_count =
                            baked.skipped_prop_door_count.saturating_add(1);
                        continue;
                    }
                };
                baked.doors.push(DoorInstance {
                    source_index: door_index,
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
    lighting: StaticPropLightingInputs<'_>,
    load_started: Instant,
) -> PropBakeResult {
    let selection = select_loaded_prop_placements(placements, loaded_model_cache);
    let prepared = pre_resolve_prop_materials(selection.material_inputs(), resolver);
    bake_loaded_prop_selection(
        selection,
        resolver,
        table,
        Some(&prepared),
        lighting,
        load_started,
    )
}

fn bake_loaded_prop_selection(
    selection: PropSelection<'_, LoadedPropModel>,
    resolver: &MaterialResolver,
    table: &mut MaterialTable,
    pre_resolved_prop_materials: Option<
        &HashMap<String, MaterialResolution<ResolvedPrimaryMaterial>>,
    >,
    lighting: StaticPropLightingInputs<'_>,
    load_started: Instant,
) -> PropBakeResult {
    let mut context = PropMaterialContext {
        resolver,
        pre_resolved_prop_materials,
        table,
    };
    let mut selected = SelectedPropPlacements {
        placements: selection
            .placements
            .into_iter()
            .enumerate()
            .map(|(index, selected)| {
                let slots = selected
                    .geometry
                    .as_ref()
                    .map(|geometry| geometry.used_material_slots())
                    .unwrap_or_default();
                let model = Arc::new(prop_model_asset_from_loaded(
                    &selected.model,
                    slots,
                    &mut context,
                ));
                SelectedPropPlacement {
                    source_index: selected.source_index,
                    geometry: selected.geometry,
                    placement: selected.placement,
                    model,
                    lighting_context:
                        crate::media::preview_model::ModelLightingContextId::from_index(index)
                            .expect("bounded placement context")
                            .gpu(),
                    lighting: legacy_prop_lighting_fallback(selected.placement, lighting),
                    compiled_lighting: None,
                }
            })
            .collect(),
        skip_stats: selection.skip_stats,
        lighting_stats: Default::default(),
    };
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
        if prop.geometry.is_err() {
            continue;
        }
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
            });
        }
    }
    let load_timing = load_started.elapsed();
    bake_selected_static_props(&selected, load_timing)
}

fn prop_draw_materials(
    indices: &BTreeMap<usize, usize>,
    opacity: f32,
    table: &mut MaterialTable,
) -> BTreeMap<usize, usize> {
    indices
        .iter()
        .map(|(&raw_slot, &index)| {
            if !table.slots()[index].render_mode.draws() {
                return (raw_slot, index);
            }
            let key = format!("prop_draw_alpha\0{index}\0{}", opacity < 1.0);
            if let Some(existing) = table.index_of(&key) {
                return (raw_slot, existing);
            }
            let mut slot = table.slots()[index].clone();
            if let Some(surface) = slot.shader.surface() {
                slot.render_mode = surface.draw_render_mode(
                    slot.texture.as_ref().and_then(|texture| texture.flags()),
                    opacity,
                );
                slot.alpha_policy = slot.render_mode.texture_alpha_policy();
            } else if opacity < 1.0
                && matches!(slot.render_mode, RenderMode::Opaque | RenderMode::Cutout)
            {
                slot.render_mode = RenderMode::Translucent;
            }
            if slot == table.slots()[index] {
                return (raw_slot, index);
            }
            (raw_slot, table.push(&key, slot))
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
    let selection = select_loaded_prop_placements(placements, loaded_model_cache);
    let pre_resolved_prop_materials =
        pre_resolve.then(|| pre_resolve_prop_materials(selection.material_inputs(), resolver));
    bake_loaded_prop_selection(
        selection,
        resolver,
        table,
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
    bake_selected_static_props(&selected, load_timing)
}

pub(in super::super) fn bake_selected_static_props(
    selected: &SelectedPropPlacements<'_>,
    load_timing: Duration,
) -> PropBakeResult {
    bake_selected_with(
        selected,
        load_timing,
        bake_selected_prop_placements_parallel,
    )
}

fn bake_selected_with(
    selected: &SelectedPropPlacements<'_>,
    load_timing: Duration,
    bake: impl FnOnce(&[SelectedPropPlacement<'_>]) -> BakedGeometry,
) -> PropBakeResult {
    let bake_started = Instant::now();
    let BakedGeometry {
        meshes: prop_meshes,
        geometry,
        admitted,
    } = bake(&selected.placements);
    let bake_timing = bake_started.elapsed();
    let entity_meshes = prop_meshes
        .keys()
        .map(|destination| destination.entity)
        .collect();
    let PreparedPropMeshes {
        meshes,
        visibility: mesh_visibility,
        renderable_ranges: mut ranges,
        animation_bindings,
    } = prop_meshes_to_mesh_data(prop_meshes);

    let mut skip_stats = selected.skip_stats;
    skip_stats.geometry_rejected += geometry.rejected.len();
    let mut model_animations = Vec::new();
    let mut model_animation_issues = Vec::new();
    for &index in &admitted {
        let prop = &selected.placements[index];
        if prop.placement.animation.is_none() {
            continue;
        }
        match prepare_prop_animation(prop, animation_bindings.get(&prop.source_index)) {
            Ok(instance) => model_animations.push(instance),
            Err(error) => {
                log::warn!("prop animation {}: {error}", prop.placement.model_path);
                model_animation_issues.push(crate::media::preview_model::ModelAnimationIssue {
                    model_path: prop.placement.model_path.clone(),
                    entity_index: prop
                        .placement
                        .animation
                        .as_ref()
                        .map(|state| state.entity_index),
                    error,
                });
            }
        }
    }
    PropBakeResult {
        model_animations,
        model_animation_issues,
        renderables: admitted.iter().map(|&index| &selected.placements[index]).map(|selected| {
            let placement = selected.placement;
            let bounds = prop_renderable_bounds(placement, &selected.model.model);
            if let Err(error) = bounds {
                log::warn!("prop render bounds {}: {error}", placement.model_path);
            }
            PreparedMapRenderable {
                leaf_membership: placement.leaf_membership.clone(),
                source: placement.static_prop_index.map_or(
                    MapRenderableSource::EntityProp { placement_index: selected.source_index },
                    |index| MapRenderableSource::StaticProp { index },
                ),
                origin: placement.origin,
                angles: placement.angles,
                scale: placement.scale,
                bounds,
                ranges: ranges.remove(&selected.source_index).unwrap_or_default(),
            }
        }).collect(),
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
                    compiled_supplement: None,
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
                    source: crate::media::preview_model::ModelLightingSource::DiagnosedFallback {
                        reason: crate::media::preview_model::ModelLightingFallbackReason::LegacyVertexLighting,
                        snapshot: crate::media::preview_model::ModelLightingSnapshot {
                            ambient_cube: selected.lighting.ambient_cube, lights: Vec::new(),
                        },
                    },
                    diffuse_modulation: selected.placement.diffuse_modulation,
                    render_mode: selected.placement.render_mode,
                }
            })
            .collect(),
        entity_meshes,
        meshes,
        mesh_visibility,
        placed_count: u32::try_from(admitted.len()).expect("bounded prop placements"),
        geometry,
        skip_stats,
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
        &selected,
        load_started.elapsed(),
        bake_selected_prop_placements_serial,
    )
}

#[derive(Debug)]
pub(in super::super) struct SelectedPropPlacement<'a> {
    pub(in super::super) source_index: usize,
    pub(in super::super) placement: &'a StaticPropPlacement,
    pub(in super::super) model: Arc<PropModelAsset>,
    pub(in super::super) lighting: LegacyPropLightingFallback,
    pub(in super::super) lighting_context: u32,
    pub(in super::super) compiled_lighting:
        Option<Arc<super::compiled_lighting::CompiledPropLighting>>,
    pub(super) geometry: Result<Arc<prop_geometry::RawPropGeometry>, GeometryError>,
}

#[derive(Debug)]
pub(in super::super) struct SelectedPropPlacements<'a> {
    pub(in super::super) placements: Vec<SelectedPropPlacement<'a>>,
    pub(in super::super) skip_stats: PropBakeSkipStats,
    pub(in super::super) lighting_stats: PropLightingStats,
}

trait AdmissionModel {
    fn model(&self) -> &Arc<ModelData>;
    fn has_material(&self, slot: usize) -> bool;
}

impl AdmissionModel for LoadedPropModel {
    fn model(&self) -> &Arc<ModelData> {
        &self.model
    }
    fn has_material(&self, slot: usize) -> bool {
        slot < self.model.material_names.len()
    }
}

#[cfg(test)]
impl AdmissionModel for PropModelAsset {
    fn model(&self) -> &Arc<ModelData> {
        &self.model
    }
    fn has_material(&self, slot: usize) -> bool {
        self.material_indices.contains_key(&slot)
    }
}

struct SelectedModelPlacement<'a, T> {
    source_index: usize,
    placement: &'a StaticPropPlacement,
    model: Arc<T>,
    geometry: Result<Arc<prop_geometry::RawPropGeometry>, GeometryError>,
}

struct PropSelection<'a, T> {
    placements: Vec<SelectedModelPlacement<'a, T>>,
    skip_stats: PropBakeSkipStats,
}

impl PropSelection<'_, LoadedPropModel> {
    fn material_inputs(
        &self,
    ) -> impl Iterator<Item = (&LoadedPropModel, &prop_geometry::RawPropGeometry)> {
        self.placements.iter().filter_map(|selected| {
            selected
                .geometry
                .as_ref()
                .ok()
                .map(|geometry| (selected.model.as_ref(), geometry.as_ref()))
        })
    }
}

fn select_models<'a, T: AdmissionModel>(
    placements: &'a [StaticPropPlacement],
    placement_cap: usize,
    triangle_cap: usize,
    load_model: &mut impl FnMut(&'a StaticPropPlacement) -> Option<Arc<T>>,
) -> PropSelection<'a, T> {
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
    let mut geometry = prop_geometry::RawGeometryPlanner::default();
    let mut baked_triangles = 0_usize;

    for (index, placement) in placements.iter().take(capped_count).enumerate() {
        if !placement.initially_visible
            || (placement.entity && matches!(placement.render_mode, 6 | 10))
        {
            skip_stats.hidden_placement += 1;
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

        if !has_bakeable_mesh(placement, model.model(), |slot| model.has_material(slot)) {
            skip_stats.no_bakeable_mesh = skip_stats.no_bakeable_mesh.saturating_add(1);
            continue;
        }
        let raw = geometry.world(placement, model.model(), |slot| model.has_material(slot));
        if let Ok(raw) = &raw {
            let triangles = raw.counts().indices / 3;
            let Some(total) = baked_triangles
                .checked_add(triangles)
                .filter(|total| *total <= triangle_cap)
            else {
                let remaining = capped_count.saturating_sub(index);
                skip_stats.triangle_cap = skip_stats.triangle_cap.saturating_add(remaining);
                break;
            };
            baked_triangles = total;
        }
        selected.push(SelectedModelPlacement {
            source_index: index,
            placement,
            geometry: raw,
            model,
        });
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

    PropSelection {
        placements: selected,
        skip_stats,
    }
}

fn select_loaded_prop_placements<'a>(
    placements: &'a [StaticPropPlacement],
    loaded: &HashMap<String, Option<Arc<LoadedPropModel>>>,
) -> PropSelection<'a, LoadedPropModel> {
    select_models(
        placements,
        MAP_PROP_PLACEMENT_CAP,
        MAP_PROP_TRIANGLE_CAP,
        &mut |placement| loaded.get(&placement.model_path).cloned().flatten(),
    )
}

#[cfg(test)]
pub(in super::super) fn select_static_prop_placements<'a>(
    placements: &'a [StaticPropPlacement],
    placement_cap: usize,
    triangle_cap: usize,
    load_model: &mut impl FnMut(&'a StaticPropPlacement) -> Option<Arc<PropModelAsset>>,
    lighting: StaticPropLightingInputs<'_>,
) -> SelectedPropPlacements<'a> {
    let selection = select_models(placements, placement_cap, triangle_cap, load_model);
    SelectedPropPlacements {
        placements: selection
            .placements
            .into_iter()
            .enumerate()
            .map(|(index, selected)| SelectedPropPlacement {
                source_index: selected.source_index,
                geometry: selected.geometry,
                placement: selected.placement,
                model: selected.model,
                lighting_context: crate::media::preview_model::ModelLightingContextId::from_index(
                    index,
                )
                .expect("bounded placement context")
                .gpu(),
                lighting: legacy_prop_lighting_fallback(selected.placement, lighting),
                compiled_lighting: None,
            })
            .collect(),
        skip_stats: selection.skip_stats,
        lighting_stats: Default::default(),
    }
}

pub(in super::super) fn prop_mesh_buffer_bytes(meshes: &[MeshData]) -> usize {
    meshes
        .iter()
        .try_fold(GeometryCounts::default(), |total, mesh| {
            total.checked_add(GeometryCounts::checked(
                mesh.vertices.len(),
                mesh.indices.len(),
                0,
            )?)
        })
        .and_then(GeometryCounts::payload_bytes)
        .expect("admitted prop geometry size")
}

type PropRenderableRanges = BTreeMap<usize, Vec<MapRenderableRange>>;

struct PreparedPropMeshes {
    meshes: Vec<MeshData>,
    visibility: Vec<MapMeshVisibility>,
    renderable_ranges: PropRenderableRanges,
    animation_bindings: BTreeMap<usize, Vec<PropAnimationMeshBinding>>,
}

struct PropAnimationMeshBinding {
    receiver_mesh: usize,
    source_mesh: usize,
    vertex_start: u32,
}

fn prepare_prop_animation(
    selected: &SelectedPropPlacement<'_>,
    bindings: Option<&Vec<PropAnimationMeshBinding>>,
) -> Result<
    crate::media::preview_model::ModelAnimationInstance,
    crate::media::preview_model::ModelAnimationPreparationError,
> {
    use crate::media::preview_model::{
        ModelAnimationBinding, ModelAnimationInstance, ModelAnimationPlayback,
        ModelAnimationPlaybackMode, ModelAnimationPreparationError as Error,
        ModelLightingContextId,
    };
    let placement = selected.placement;
    let state = placement
        .animation
        .as_ref()
        .ok_or(Error::MissingAnimation)?;
    if state.random.is_some() {
        return Err(Error::RandomActivity);
    }
    let model = &selected.model.model;
    let animation = model.animation.as_ref().ok_or(Error::MissingAnimation)?;
    let (sequence, playback) = if let Some(name) = &state.default_sequence {
        (
            animation
                .sequence_index(name)
                .ok_or_else(|| Error::MissingSequence { name: name.clone() })?,
            ModelAnimationPlayback {
                initial_cycle: 0.0,
                playback_rate: 1.0,
                mode: ModelAnimationPlaybackMode::Default,
            },
        )
    } else {
        (
            state.sequence_index,
            ModelAnimationPlayback {
                initial_cycle: state.initial_cycle,
                playback_rate: 0.0,
                mode: ModelAnimationPlaybackMode::Sequence,
            },
        )
    };
    let cycle = playback.cycle(animation, sequence, 0.0)?;
    animation.evaluate(sequence, cycle, &mut Default::default())?;
    let bindings =
        bindings
            .into_iter()
            .flatten()
            .map(|binding| {
                let expected = model.meshes[binding.source_mesh].vertices.len();
                let vertices = model.skin_vertices.get(binding.source_mesh).ok_or(
                    Error::InvalidVertexCount {
                        mesh: binding.source_mesh,
                        expected,
                        actual: 0,
                    },
                )?;
                if vertices.len() != expected {
                    return Err(Error::InvalidVertexCount {
                        mesh: binding.source_mesh,
                        expected,
                        actual: vertices.len(),
                    });
                }
                Ok(ModelAnimationBinding {
                    receiver_mesh: gmpublished_domain::scene::map::MapReceiverMesh::World(
                        binding.receiver_mesh,
                    ),
                    vertex_start: binding.vertex_start,
                    vertices: Arc::clone(vertices),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
    Ok(ModelAnimationInstance {
        model_path: placement.model_path.clone(),
        entity_index: Some(state.entity_index),
        hammer_id: state.hammer_id,
        animation: Arc::clone(animation),
        sequence,
        playback,
        origin: placement.origin,
        angles: placement.angles,
        scale: placement.scale,
        bindings,
        lighting_context: ModelLightingContextId::from_gpu(selected.lighting_context),
        illumination_attachment_index: model.illumination_attachment_index,
        illumination_position: model.illumination_position,
        lighting_origin_override: placement.lighting_origin,
    })
}

fn prop_meshes_to_mesh_data(
    prop_meshes: BTreeMap<prop_geometry::Destination, PropBuildMesh>,
) -> PreparedPropMeshes {
    let mut ranges = BTreeMap::<usize, Vec<MapRenderableRange>>::new();
    let mut animation_bindings = BTreeMap::<usize, Vec<PropAnimationMeshBinding>>::new();
    let (meshes, visibility) = prop_meshes
        .into_values()
        .enumerate()
        .map(|(mesh_index, mesh)| {
            for range in mesh.renderable_ranges {
                animation_bindings
                    .entry(range.source_index)
                    .or_default()
                    .push(PropAnimationMeshBinding {
                        receiver_mesh: mesh_index,
                        source_mesh: range.draw_order,
                        vertex_start: range.vertex_start,
                    });
                ranges
                    .entry(range.source_index)
                    .or_default()
                    .push(MapRenderableRange {
                        mesh_index,
                        draw_order: range.draw_order,
                        start: range.start,
                        count: range.count,
                    });
            }
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
        .unzip();
    for parts in ranges.values_mut() {
        parts.sort_by_key(|part| part.draw_order);
    }
    PreparedPropMeshes {
        meshes,
        visibility,
        renderable_ranges: ranges,
        animation_bindings,
    }
}

fn has_bakeable_mesh(
    placement: &StaticPropPlacement,
    model: &ModelData,
    has_material: impl Fn(usize) -> bool,
) -> bool {
    let skin_table = prop_skin_table(model, placement.skin);
    model
        .meshes
        .iter()
        .filter(|mesh| mesh.bodygroup_choice == 0)
        .any(|mesh| {
            let material_index = skin_table
                .and_then(|table| table.get(mesh.material_index))
                .map_or(mesh.material_index, |index| usize::from(*index));
            has_material(material_index)
        })
}

pub(in super::super) fn record_prop_door_material_resolution(
    resolutions: &mut BTreeMap<String, PropDoorMaterialResolution>,
    placement: &StaticPropPlacement,
    model: &PropModelAsset,
    geometry: &prop_geometry::RawPropGeometry,
    materials: &[MaterialSlot],
) {
    let entry = resolutions.entry(placement.model_path.clone()).or_default();
    entry.door_count = entry.door_count.saturating_add(1);
    for source_slot in geometry.used_material_slots() {
        entry.used_material_slots.insert(source_slot);
        let resolved = model
            .material_indices
            .get(&source_slot)
            .and_then(|&slot| materials.get(slot))
            .is_some_and(MaterialSlot::has_resolved_base);
        if resolved {
            entry.resolved_used_material_slots.insert(source_slot);
        } else {
            entry.unresolved_used_material_slots.insert(source_slot);
        }
    }
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

fn bake_prop_door_meshes(
    placement: &StaticPropPlacement,
    model: &ModelData,
    lighting: LegacyPropLightingFallback,
    parts: &[prop_geometry::SourceMesh],
) -> Result<Vec<MeshData>, GeometryError> {
    let mut meshes = Vec::with_capacity(parts.len());
    for part in parts {
        let failure = |component, bytes, error: std::collections::TryReserveError| {
            GeometryError::Allocation {
                material_index: part.destination.material_index,
                entity: part.destination.entity,
                component,
                bytes,
                detail: error.to_string(),
            }
        };
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        vertices
            .try_reserve_exact(part.counts.vertices)
            .map_err(|error| {
                failure(GeometryComponent::Vertices, part.counts.vertex_bytes, error)
            })?;
        indices
            .try_reserve_exact(part.counts.indices)
            .map_err(|error| failure(GeometryComponent::Indices, part.counts.index_bytes, error))?;
        meshes.push(MeshData {
            geometry: crate::media::preview_model::MeshGeometry::Model,
            vertices,
            indices,
            material_index: part.destination.material_index,
            bodygroup: 0,
            bodygroup_choice: 0,
        });
    }
    for (target, part) in meshes.iter_mut().zip(parts) {
        let source = &model.meshes[part.mesh_index];
        target.vertices.extend(source.vertices.iter().map(|vertex| {
            let closed_normal = transform_prop_normal(vertex.normal, placement);
            let fallback = lighting.vertex_lighting(
                closed_normal,
                prop_diffuse_tint(placement),
                placement.diffuse_modulation != [255; 4],
            );
            ModelVertex {
                position: vertex.position,
                normal: vertex.normal,
                tangent_s: vertex.tangent_s,
                tangent_t: vertex.tangent_t,
                tangent_sign: vertex.tangent_sign,
                lighting_context: 0,
                surface_mark: 0,
                uv: vertex.uv,
                lightmap_uv: [0.0; 2],
                lightmap_bump_step: 0.0,
                lightmap_page: 0,
                color: fallback.color,
                blend_alpha: 0.0,
                opacity: prop_opacity(placement),
                illumination: fallback.illumination,
                lighting_mode: crate::media::preview_model::vertex_lighting::MODEL_DRAW
                    | fallback.flags,
            }
        }));
        target.indices.extend_from_slice(&source.indices);
        assert_eq!(target.vertices.len(), part.counts.vertices);
        assert_eq!(target.indices.len(), part.counts.indices);
    }
    Ok(meshes)
}

pub(in super::super) struct PropMaterialContext<'a> {
    pub(in super::super) resolver: &'a MaterialResolver,
    pub(in super::super) pre_resolved_prop_materials:
        Option<&'a HashMap<String, MaterialResolution<ResolvedPrimaryMaterial>>>,
    pub(in super::super) table: &'a mut MaterialTable,
}

#[cfg(test)]
pub(in super::super) fn load_unique_prop_models_parallel(
    placements: &[StaticPropPlacement],
    resolver: &MaterialResolver,
    load_model: &(impl Fn(&str, &MaterialResolver) -> Option<LoadedPropModel> + Sync),
) -> HashMap<String, Option<Arc<LoadedPropModel>>> {
    let model_paths = unique_prop_model_paths(placements);
    load_prop_model_paths_parallel(&model_paths, resolver, load_model)
}

pub(super) fn load_prop_model_paths_parallel(
    model_paths: &[String],
    resolver: &MaterialResolver,
    load_model: &(impl Fn(&str, &MaterialResolver) -> Option<LoadedPropModel> + Sync),
) -> HashMap<String, Option<Arc<LoadedPropModel>>> {
    parallel_collect(model_paths, |model_path| {
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

#[cfg(test)]
pub(in super::super) fn unique_prop_model_paths(placements: &[StaticPropPlacement]) -> Vec<String> {
    unique_prop_model_paths_from(placements.iter())
}

pub(super) fn unique_prop_model_paths_from<'a>(
    placements: impl Iterator<Item = &'a StaticPropPlacement>,
) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for placement in placements.take(MAP_PROP_PLACEMENT_CAP) {
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

fn pre_resolve_prop_materials<'a>(
    inputs: impl IntoIterator<Item = (&'a LoadedPropModel, &'a prop_geometry::RawPropGeometry)>,
    resolver: &MaterialResolver,
) -> HashMap<String, MaterialResolution<ResolvedPrimaryMaterial>> {
    let jobs = unique_prop_material_resolve_jobs(inputs);
    super::super::materials::resolve_texture_jobs(&jobs, resolver, |job, resolver| {
        (
            job.key.clone(),
            resolver.resolve_primary(&job.material_dirs, &job.name),
        )
    })
    .into_iter()
    .collect()
}

fn unique_prop_material_resolve_jobs<'a>(
    inputs: impl IntoIterator<Item = (&'a LoadedPropModel, &'a prop_geometry::RawPropGeometry)>,
) -> Vec<PropMaterialResolveJob> {
    let mut jobs = Vec::new();
    let mut seen = HashSet::new();
    for (loaded, geometry) in inputs {
        for slot in geometry.used_material_slots() {
            let name = &loaded.model.material_names[slot];
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

fn prop_model_asset_from_loaded(
    loaded: &LoadedPropModel,
    slots: impl IntoIterator<Item = usize>,
    context: &mut PropMaterialContext<'_>,
) -> PropModelAsset {
    let material_indices = slots
        .into_iter()
        .map(|slot| {
            (
                slot,
                prop_material_index(
                    &loaded.model.material_dirs,
                    &loaded.model.material_names[slot],
                    context,
                ),
            )
        })
        .collect();
    PropModelAsset {
        model: Arc::clone(&loaded.model),
        material_indices,
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
        .material()
        .and_then(|material| material.texture.as_ref().map(Arc::clone));
    context.table.push(
        &key,
        MaterialSlot {
            resolution: resolved.outcome(),
            canonical_name: resolved.canonical_name().map(str::to_owned),
            shader: resolved
                .material()
                .map_or_else(Default::default, |material| material.shader.clone()),
            name: name.to_owned(),
            texture,
            texture2: resolved
                .material()
                .and_then(|material| material.texture2.as_ref().map(Arc::clone)),
            render_mode: resolved
                .material()
                .map_or(RenderMode::Opaque, |material| material.render_mode),
            alpha_policy: resolved
                .material()
                .map_or(RenderMode::Opaque, |material| material.render_mode)
                .texture_alpha_policy(),
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
    lighting: LegacyPropLightingFallback,
    prop_meshes: &mut BTreeMap<prop_geometry::Destination, PropBuildMesh>,
) -> bool {
    bake_prop_placement_with_compiled(
        placement,
        model,
        lighting,
        None,
        PropBakePlacement {
            source_index: 0,
            lighting_context: 0,
        },
        prop_meshes,
    )
}

#[cfg(test)]
fn bake_prop_placement_with_compiled(
    placement: &StaticPropPlacement,
    model: &PropModelAsset,
    lighting: LegacyPropLightingFallback,
    compiled: Option<&CompiledPropLighting>,
    identity: PropBakePlacement,
    prop_meshes: &mut BTreeMap<prop_geometry::Destination, PropBuildMesh>,
) -> bool {
    assert!(prop_meshes.is_empty(), "single-placement fixture output");
    let selected = SelectedPropPlacement {
        geometry: prop_geometry::world_fixture(placement, model),
        source_index: identity.source_index,
        lighting_context: identity.lighting_context,
        placement,
        model: Arc::new(PropModelAsset {
            model: Arc::clone(&model.model),
            material_indices: model.material_indices.clone(),
        }),
        lighting,
        compiled_lighting: compiled.map(|compiled| {
            Arc::new(CompiledPropLighting {
                illumination: compiled.illumination.clone(),
            })
        }),
    };
    let baked = bake_selected_prop_placements_serial(&[selected]);
    let success = !baked.admitted.is_empty();
    *prop_meshes = baked.meshes;
    success
}

pub(super) fn bake_world_vertex(
    selected: &SelectedPropPlacement<'_>,
    mesh_index: usize,
    vertex_index: usize,
    vertex: &ModelVertex,
) -> ModelVertex {
    let placement = selected.placement;
    let compiled = selected.compiled_lighting.as_deref();
    let lighting = selected.lighting;
    let normal = transform_prop_normal(vertex.normal, placement);
    let compiled_illumination = compiled
        .and_then(|compiled| compiled.illumination.get(mesh_index))
        .and_then(|mesh| mesh.get(vertex_index))
        .copied();
    let fallback = lighting.vertex_lighting(
        normal,
        prop_diffuse_tint(placement),
        placement.diffuse_modulation != [255; 4],
    );
    ModelVertex {
        position: transform_prop_position(vertex.position, placement),
        normal,
        tangent_s: vertex.tangent_s.rotate_source(placement.angles),
        tangent_t: vertex.tangent_t.rotate_source(placement.angles),
        tangent_sign: vertex.tangent_sign,
        lighting_context: selected.lighting_context,
        surface_mark: 0,
        uv: vertex.uv,
        lightmap_uv: [0.0; 2],
        lightmap_bump_step: 0.0,
        lightmap_page: 0,
        color: compiled_illumination.map_or(fallback.color, |_| prop_diffuse_tint(placement)),
        blend_alpha: 0.0,
        opacity: prop_opacity(placement),
        illumination: compiled_illumination.unwrap_or(fallback.illumination),
        lighting_mode: compiled_illumination.map_or(fallback.flags, |_| {
            crate::media::preview_model::vertex_lighting::LINEAR_ILLUMINATION
                | crate::media::preview_model::vertex_lighting::COMPILED_VERTEX
        }) | if placement.entity {
            0
        } else {
            crate::media::preview_model::vertex_lighting::STATIC_PROP
        } | crate::media::preview_model::vertex_lighting::MODEL_DRAW,
    }
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

    fn destination(material_index: usize, entity: bool) -> prop_geometry::Destination {
        prop_geometry::Destination {
            material_index,
            entity,
            animation_instance: None,
        }
    }
    use crate::media::file_preview_decode::tests::{prop_placement, test_prop_asset};
    use gmpublished_domain::scene::map::AmbientCube;

    fn fallback() -> LegacyPropLightingFallback {
        LegacyPropLightingFallback {
            ambient_cube: AmbientCube {
                colors: [Vec3::splat(0.25); 6],
            },
            sun: None,
        }
    }

    #[test]
    fn rejected_geometry_preserves_selected_lighting_rows_and_prefix_admission() {
        let placements = [
            prop_placement("models/bad.mdl", Vec3::ZERO, Vec3::ZERO, 0),
            prop_placement("models/good.mdl", Vec3::splat(10.0), Vec3::ZERO, 0),
            prop_placement("models/capped.mdl", Vec3::splat(20.0), Vec3::ZERO, 0),
            prop_placement("models/suffix.mdl", Vec3::splat(30.0), Vec3::ZERO, 0),
        ];
        let mut bad = test_prop_asset(vec![0, 1]);
        Arc::make_mut(&mut bad.model).meshes[0].indices[2] = 99;
        let bad = Arc::new(bad);
        let good = Arc::new(test_prop_asset(vec![0, 1]));
        let mut calls = Vec::new();
        let selected = select_static_prop_placements(
            &placements,
            4,
            1,
            &mut |placement| {
                calls.push(placement.model_path.clone());
                Some(if placement.model_path == "models/bad.mdl" {
                    Arc::clone(&bad)
                } else {
                    Arc::clone(&good)
                })
            },
            StaticPropLightingInputs {
                ambient: &MapAmbientLighting::neutral(),
                environment_maps: None,
                environment_lighting: None,
                lightmap_source: None,
                walk_collision: None,
            },
        );
        let result = bake_selected_with(
            &selected,
            Duration::ZERO,
            bake_selected_prop_placements_serial,
        );
        assert_eq!(result.placed_count, 1);
        assert_eq!(result.geometry.rejected.len(), 1);
        assert_eq!(result.skip_stats.geometry_rejected, 1);
        assert_eq!(result.model_lighting.len(), 2);
        assert_eq!(result.model_lighting[0].origin, placements[0].origin);
        assert_eq!(result.model_lighting[1].origin, placements[1].origin);
        assert_eq!(
            result.meshes[0].vertices[0].lighting_context,
            selected.placements[1].lighting_context
        );
        assert_eq!(result.renderables.len(), 1);
        assert_eq!(result.renderables[0].origin, placements[1].origin);
        assert_eq!(
            result.geometry.admitted.payload_bytes().unwrap(),
            result.mesh_bytes()
        );
        assert!(!calls.iter().any(|path| path == "models/suffix.mdl"));
    }

    #[test]
    fn door_geometry_keeps_source_local_vertices_and_mesh_boundaries() {
        let placement = prop_placement(
            "models/door.mdl",
            Vec3::splat(100.0),
            Vec3::new(0.0, 90.0, 0.0),
            1,
        );
        let mut asset = test_prop_asset(vec![7, 8]);
        let model = Arc::make_mut(&mut asset.model);
        model.meshes.push(model.meshes[0].clone());
        model.meshes[1].material_index = 1;
        let parts = prop_geometry::plan_door(&placement, &asset).unwrap();
        let output = bake_prop_door_meshes(&placement, &asset.model, fallback(), &parts).unwrap();
        assert_eq!(output.len(), 2);
        for (index, mesh) in output.iter().enumerate() {
            assert_eq!(mesh.indices, asset.model.meshes[index].indices);
            assert_eq!(mesh.vertices.len(), parts[index].counts.vertices);
            assert_eq!(mesh.material_index, parts[index].destination.material_index);
            for (vertex, source) in mesh
                .vertices
                .iter()
                .zip(&asset.model.meshes[index].vertices)
            {
                assert_eq!(vertex.position, source.position);
                assert_eq!(vertex.normal, source.normal);
                assert_eq!(vertex.uv, source.uv);
            }
        }
        assert_eq!(output[0].material_index, 8);
    }

    #[test]
    fn compiled_bounds_use_view_box_then_hull_with_scale_and_rotation() {
        let mut placement = prop_placement(
            "models/test/chair.mdl",
            Vec3::new(100.0, 200.0, 300.0),
            Vec3::new(0.0, 90.0, 0.0),
            0,
        );
        placement.scale = 2.0;
        let mut model = (*test_prop_asset(vec![0, 1]).model).clone();
        model.view_min = Vec3::new(2.0, 3.0, 4.0);
        model.view_max = Vec3::new(8.0, 9.0, 10.0);
        model.bounds_min = Vec3::splat(-999.0);
        model.bounds_max = Vec3::splat(999.0);
        let bounds = prop_renderable_bounds(&placement, &model).unwrap();
        assert_eq!(
            bounds.local,
            MapBounds {
                min: Vec3::new(4.0, 6.0, 8.0),
                max: Vec3::new(16.0, 18.0, 20.0)
            }
        );
        assert!((bounds.world.min - Vec3::new(82.0, 204.0, 308.0)).length() < 0.001);
        assert!((bounds.world.max - Vec3::new(94.0, 216.0, 320.0)).length() < 0.001);
        model.view_min = Vec3::ZERO;
        model.view_max = Vec3::ZERO;
        let bounds = prop_renderable_bounds(&placement, &model).unwrap();
        assert_eq!(
            bounds.local,
            MapBounds {
                min: model.hull_min * 2.0,
                max: model.hull_max * 2.0
            }
        );
        placement.entity = true;
        assert_eq!(
            prop_renderable_bounds(&placement, &model),
            Err(MapRenderableBoundsError::EntitySequenceUnavailable)
        );
        placement.entity = false;
        model.view_max[0] = f32::NAN;
        assert_eq!(
            prop_renderable_bounds(&placement, &model),
            Err(MapRenderableBoundsError::InvalidCompiledBounds)
        );
    }

    #[test]
    fn animated_entity_bounds_include_motion_under_placement_transform() {
        let mut placement = prop_placement(
            "models/turn.mdl",
            Vec3::new(100.0, 200.0, 300.0),
            Vec3::new(0.0, 90.0, 0.0),
            0,
        );
        placement.entity = true;
        placement.scale = 2.0;
        placement.animation = Some(gmpublished_domain::scene::map::MapPropAnimation {
            entity_index: 7,
            hammer_id: None,
            default_sequence: Some("turn".into()),
            sequence_index: 0,
            initial_cycle: 0.0,
            playback_rate: 1.0,
            random: None,
        });
        let mut model = (*test_prop_asset(vec![0, 1]).model).clone();
        model.animation = Some(gmpublished_domain::scene::model::animation_for_tests());
        model.bounds_min = Vec3::new(-40.0, -1.0, -1.0);
        model.bounds_max = Vec3::new(1.0, 40.0, 1.0);
        let bounds = prop_renderable_bounds(&placement, &model).unwrap();
        assert_eq!(
            bounds.local,
            MapBounds {
                min: Vec3::new(-80.0, -64.0, -64.0),
                max: Vec3::new(64.0, 80.0, 64.0)
            }
        );
        assert!((bounds.world.min - Vec3::new(20.0, 120.0, 236.0)).length() < 0.001);
        assert!((bounds.world.max - Vec3::new(164.0, 264.0, 364.0)).length() < 0.001);
        assert_eq!(
            prop_model_world_bounds(&placement, &model),
            Some((bounds.world.min, bounds.world.max))
        );
    }

    #[test]
    fn renderable_identity_and_draw_order_survive_parallel_material_merging() {
        let mut asset = test_prop_asset(vec![1, 0]);
        let mut model = (*asset.model).clone();
        let mut second = model.meshes[0].clone();
        second.material_index = 1;
        model.meshes.push(second);
        asset.model = Arc::new(model);
        let asset = Arc::new(asset);
        let mut placements = [
            prop_placement("models/test/chair.mdl", Vec3::ZERO, Vec3::ZERO, 0),
            prop_placement(
                "models/test/chair.mdl",
                Vec3::new(10.0, 0.0, 0.0),
                Vec3::ZERO,
                0,
            ),
        ];
        placements[0].static_prop_index = Some(31);
        placements[1].static_prop_index = Some(47);
        placements[0].leaf_membership =
            gmpublished_domain::scene::map::MapPropLeafMembership::Compiled(vec![3, 3, 9]);
        let make_selected = || SelectedPropPlacements {
            placements: placements
                .iter()
                .enumerate()
                .map(|(index, placement)| SelectedPropPlacement {
                    geometry: prop_geometry::world_fixture(placement, &asset),
                    source_index: 3 + index * 5,
                    placement,
                    model: Arc::clone(&asset),
                    lighting: fallback(),
                    lighting_context: 99,
                    compiled_lighting: None,
                })
                .collect(),
            skip_stats: PropBakeSkipStats::default(),
            lighting_stats: PropLightingStats::default(),
        };
        let serial = bake_selected_with(
            &make_selected(),
            Duration::ZERO,
            bake_selected_prop_placements_serial,
        );
        let parallel = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap()
            .install(|| {
                bake_selected_with(
                    &make_selected(),
                    Duration::ZERO,
                    bake_selected_prop_placements_parallel,
                )
            });
        assert_eq!(serial.meshes, parallel.meshes);
        assert_eq!(serial.mesh_visibility, parallel.mesh_visibility);
        assert_eq!(serial.renderables, parallel.renderables);
        for (index, owner) in parallel.renderables.iter().enumerate() {
            assert_eq!(owner.leaf_membership, placements[index].leaf_membership);
            assert_eq!(
                owner.source,
                MapRenderableSource::StaticProp {
                    index: [31, 47][index]
                }
            );
            assert_eq!(
                owner
                    .ranges
                    .iter()
                    .map(|range| (range.draw_order, range.mesh_index))
                    .collect::<Vec<_>>(),
                [(0, 1), (1, 0)]
            );
            for range in &owner.ranges {
                assert_eq!(range.start, index as u32 * 3);
                assert_eq!(range.count, 3);
                let mesh = &parallel.meshes[range.mesh_index];
                let vertex = mesh.vertices[mesh.indices[range.start as usize] as usize];
                assert_eq!(vertex.position.x(), index as f32 * 10.0 + 1.0);
                assert_eq!(vertex.lighting_context, 99);
            }
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
        let model = test_prop_asset(vec![0, 1]);
        let compiled = CompiledPropLighting {
            illumination: vec![vec![Vec3::ZERO; 3]],
        };
        let mut baked = BTreeMap::new();
        assert!(bake_prop_placement_with_compiled(
            &placement,
            &model,
            fallback(),
            Some(&compiled),
            PropBakePlacement {
                source_index: 0,
                lighting_context: 0
            },
            &mut baked
        ));
        let vertex = baked[&destination(0, false)].vertices[0];
        assert_eq!(vertex.position, Vec3::new(12.5, 20.0, 30.0));
        assert_eq!(vertex.illumination, Vec3::ZERO);
        assert_eq!(
            vertex.lighting_mode,
            crate::media::preview_model::vertex_lighting::LINEAR_ILLUMINATION
                | crate::media::preview_model::vertex_lighting::COMPILED_VERTEX
                | crate::media::preview_model::vertex_lighting::STATIC_PROP
                | crate::media::preview_model::vertex_lighting::MODEL_DRAW
        );
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
        let model = Arc::new(test_prop_asset(vec![0, 1]));
        let selected = placements
            .iter()
            .enumerate()
            .map(|(index, placement)| SelectedPropPlacement {
                geometry: prop_geometry::world_fixture(placement, &model),
                source_index: index,
                placement,
                model: Arc::clone(&model),
                lighting: fallback(),
                lighting_context: index as u32 + 1,
                compiled_lighting: Some(Arc::new(CompiledPropLighting {
                    illumination: vec![vec![Vec3::splat(index as f32); 3]],
                })),
            })
            .collect::<Vec<_>>();
        let serial = bake_selected_prop_placements_serial(&selected).meshes;
        let parallel = bake_selected_prop_placements_parallel(&selected).meshes;
        assert_eq!(
            serial[&destination(0, false)].vertices,
            parallel[&destination(0, false)].vertices
        );
        assert_eq!(
            serial[&destination(0, false)].indices,
            parallel[&destination(0, false)].indices
        );
        assert_eq!(
            parallel[&destination(0, false)].vertices[0].illumination,
            Vec3::ZERO
        );
        assert_eq!(
            parallel[&destination(0, false)].vertices[3].illumination,
            Vec3::splat(1.0)
        );
        assert_eq!(
            parallel[&destination(0, false)].vertices[0].color,
            Vec3::splat(1.0)
        );
        assert_eq!(
            parallel[&destination(0, false)].vertices[0].lighting_context,
            1
        );
        assert_eq!(
            parallel[&destination(0, false)].vertices[3].lighting_context,
            2
        );
    }

    #[test]
    fn ordinary_fallback_keeps_its_previous_vertex_contract() {
        let placement = prop_placement("models/test/chair.mdl", Vec3::ZERO, Vec3::ZERO, 0);
        let model = test_prop_asset(vec![0, 1]);
        let mut baked = BTreeMap::new();
        assert!(bake_prop_placement_with_compiled(
            &placement,
            &model,
            fallback(),
            None,
            PropBakePlacement {
                source_index: 0,
                lighting_context: 0
            },
            &mut baked
        ));
        let vertex = baked[&destination(0, false)].vertices[0];
        assert_eq!(vertex.color, Vec3::splat(0.25));
        assert_eq!(vertex.illumination, Vec3::splat(1.0));
        assert_eq!(vertex.lighting_mode & 15, 10);
        assert_eq!(
            vertex.lighting_mode & crate::media::preview_model::vertex_lighting::COMPILED_VERTEX,
            0
        );
        assert_ne!(
            vertex.lighting_mode
                & crate::media::preview_model::vertex_lighting::LEGACY_VERTEX_COLOR,
            0
        );
    }

    #[test]
    fn translucent_prop_variants_preserve_original_materials_and_share_slots() {
        let mut table = MaterialTable::default();
        let original = table.push(
            "fixture",
            MaterialSlot {
                resolution: Default::default(),
                canonical_name: None,
                shader: Default::default(),
                name: "fixture".to_owned(),
                texture: None,
                texture2: None,
                render_mode: RenderMode::Opaque,
                alpha_policy: RenderMode::Opaque.texture_alpha_policy(),
            },
        );
        let translucent =
            prop_draw_materials(&BTreeMap::from([(7, original)]), 0.5, &mut table)[&7];
        assert_ne!(original, translucent);
        assert_eq!(table.slots()[original].render_mode, RenderMode::Opaque);
        assert_eq!(
            table.slots()[translucent].render_mode,
            RenderMode::Translucent
        );
        assert!(table.slots()[translucent].alpha_policy.force_opaque());
        assert_eq!(
            prop_draw_materials(&BTreeMap::from([(7, original)]), 0.5, &mut table),
            BTreeMap::from([(7, translucent)])
        );
    }

    #[test]
    fn instance_alpha_does_not_restore_hidden_model_materials() {
        use crate::bridge::materials::{MaterialShader, SurfaceMaterial};
        let mut table = MaterialTable::default();
        let hidden = table.push(
            "hidden",
            MaterialSlot {
                resolution: Default::default(),
                canonical_name: None,
                name: "hidden".into(),
                shader: MaterialShader::Surface(Arc::new(SurfaceMaterial {
                    no_draw: true,
                    ..Default::default()
                })),
                texture: None,
                texture2: None,
                render_mode: RenderMode::NoDraw,
                alpha_policy: RenderMode::NoDraw.texture_alpha_policy(),
            },
        );
        for alpha in [0.0, 0.5, 1.0] {
            assert_eq!(
                prop_draw_materials(&BTreeMap::from([(7, hidden)]), alpha, &mut table),
                BTreeMap::from([(7, hidden)])
            );
        }
        assert_eq!(table.slots().len(), 1);
        assert_eq!(table.slots()[hidden].render_mode, RenderMode::NoDraw);
    }

    #[test]
    fn model_draw_alpha_replaces_material_alpha_without_changing_shared_world_state() {
        use crate::bridge::materials::{MaterialShader, SurfaceMaterial};
        let mut table = MaterialTable::default();
        let original = table.push(
            "scalar_alpha",
            MaterialSlot {
                resolution: Default::default(),
                canonical_name: None,
                shader: MaterialShader::Surface(Arc::new(SurfaceMaterial {
                    alpha: 0.5,
                    ..SurfaceMaterial::default()
                })),
                name: "scalar_alpha".to_owned(),
                texture: None,
                texture2: None,
                render_mode: RenderMode::Translucent,
                alpha_policy: RenderMode::Translucent.texture_alpha_policy(),
            },
        );
        let opaque = prop_draw_materials(&BTreeMap::from([(7, original)]), 1.0, &mut table)[&7];
        let blended = prop_draw_materials(&BTreeMap::from([(7, original)]), 0.5, &mut table)[&7];
        assert_eq!(table.slots()[original].render_mode, RenderMode::Translucent);
        assert_eq!(table.slots()[opaque].render_mode, RenderMode::Opaque);
        assert_eq!(table.slots()[blended].render_mode, RenderMode::Translucent);
        assert!(table.slots()[opaque].alpha_policy.force_opaque());
        assert!(!table.slots()[blended].alpha_policy.force_opaque());
        assert_eq!(table.slots()[opaque].shader.surface().unwrap().alpha, 0.5);
        assert_eq!(
            prop_draw_materials(&BTreeMap::from([(7, original)]), 1.0, &mut table),
            BTreeMap::from([(7, opaque)])
        );
    }
}

#[cfg(test)]
#[path = "props/admission_tests.rs"]
mod admission_tests;
