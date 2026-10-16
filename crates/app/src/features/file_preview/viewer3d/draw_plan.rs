use super::pipeline::ViewerScene;
use super::{MapVisibilityBucket, RenderMode, WorldVisibilityPlan};
use gmpublished_domain::math::Vec3;
use gmpublished_domain::scene::map::{MapSurfaceMarkSource, SourceLightmapPage};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

mod planar;
mod world;
pub(super) use planar::PlanarSurfaceRange;
pub(super) use world::{WorldBatch, WorldPreparationIssue, validate_rendering};

#[derive(Clone, Debug, Default)]
pub(super) struct DrawPlans {
    pub(super) content_id: u64,
    pub(super) world: Arc<DrawPlan>,
    pub(super) map_skybox: Option<DrawPlan>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct DrawPlan {
    pub(super) transparency_leaves:
        Option<Vec<gmpublished_domain::scene::map::MapWorldSurfaceGroup>>,
    pub(super) world_batches: Vec<WorldBatch>,
    pub(super) world_preparation_issues: Vec<WorldPreparationIssue>,
    pub(super) opaque: Vec<DrawItem>,
    pub(super) brushes: Vec<BrushBatch>,
    pub(super) planar: Vec<DrawItem>,
    pub(super) planar_world_ranges: HashMap<usize, Vec<PlanarSurfaceRange>>,
    pub(super) surface_marks: Vec<OverlayDrawItem>,
    pub(super) receiver_marks: Vec<OverlayDrawItem>,
    pub(super) translucent: Vec<DrawItem>,
    pub(super) additive: Vec<DrawItem>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct SurfaceBatch {
    pub(super) meshes: Vec<IndexedDrawItem>,
    pub(super) surface_marks: Vec<OverlayDrawItem>,
}

#[derive(Clone, Debug)]
pub(super) struct BrushBatch {
    pub(super) origin: Vec3,
    pub(super) local_sort_center: Vec3,
    pub(super) bounds: Option<gmpublished_domain::scene::map::MapBounds>,
    pub(super) opaque: SurfaceBatch,
    pub(super) transparent: Vec<SurfaceBatch>,
}

#[derive(Clone, Debug)]
pub(super) struct IndexedDrawItem {
    pub(super) item: DrawItem,
    pub(super) indices: std::ops::Range<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BrushMaterialKey {
    material_group: usize,
    lightmap_page: SourceLightmapPage,
}

#[derive(Clone, Copy)]
struct BrushSurfaceState {
    material: BrushMaterialKey,
    marked: bool,
}

#[cfg(feature = "map-capture")]
impl DrawPlan {
    pub(super) fn opaque_mesh_indices(&self) -> HashSet<usize> {
        self.opaque
            .iter()
            .map(|item| item.mesh_index)
            .chain(self.world_batches.iter().flat_map(|batch| {
                batch
                    .surfaces
                    .meshes
                    .iter()
                    .map(|mesh| mesh.item.mesh_index)
            }))
            .chain(
                self.brushes
                    .iter()
                    .flat_map(|batch| batch.opaque.meshes.iter().map(|mesh| mesh.item.mesh_index)),
            )
            .collect()
    }

    pub(super) fn surface_mark_count(&self) -> usize {
        self.surface_marks.len()
            + self
                .world_batches
                .iter()
                .map(|batch| batch.surfaces.surface_marks.len())
                .sum::<usize>()
            + self
                .brushes
                .iter()
                .map(|batch| batch.opaque.surface_marks.len())
                .sum::<usize>()
    }

    pub(super) fn receiver_mark_count(&self) -> usize {
        self.receiver_marks.len()
            + self
                .brushes
                .iter()
                .flat_map(|batch| &batch.transparent)
                .map(|group| group.surface_marks.len())
                .sum::<usize>()
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DrawItem {
    pub(super) mesh_index: usize,
    pub(super) material_slot: usize,
    pub(super) distance_squared: f32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OverlayDrawItem {
    pub(super) blend: RenderMode,
    pub(super) receiver_mesh_index: Option<usize>,
    pub(super) receiver_range: gmpublished_domain::scene::map::MapMeshIndexRange,
    pub(super) overlay_index: usize,
    pub(super) material_slot: usize,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MeshPlanSource {
    pub(super) mesh_index: usize,
    pub(super) scene_mesh_index: usize,
    pub(super) material_index: usize,
    pub(super) bodygroup: usize,
    pub(super) bodygroup_choice: usize,
    pub(super) centroid: Vec3,
    pub(super) map_skybox: bool,
    pub(super) door_index: Option<usize>,
    pub(super) door_visibility: Option<MapVisibilityBucket>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OverlayPlanSource {
    pub(super) owner: gmpublished_domain::scene::map::MapSurfaceOwnerId,
    pub(super) receiver_displacement: bool,
    pub(super) receiver_mesh_index: Option<usize>,
    pub(super) receiver_range: gmpublished_domain::scene::map::MapMeshIndexRange,
    pub(super) receiver_material_index: usize,
    pub(super) receiver_material_group: usize,
    pub(super) receiver_lightmap_page: SourceLightmapPage,
    pub(super) source: gmpublished_domain::scene::map::MapSurfaceMarkSource,
    pub(super) decal_sort: Option<crate::media::preview_model::StaticDecalSort>,
    pub(super) render_order: u16,
    pub(super) overlay_index: usize,
    pub(super) material_index: usize,
    pub(super) map_skybox: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ReceiverPlanSource {
    pub(super) receiver_mesh_index: Option<usize>,
    pub(super) face: u32,
    pub(super) material_group: usize,
    pub(super) lightmap_page: SourceLightmapPage,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DrawPlanMaterials<'a> {
    pub(super) render_modes: &'a [RenderMode],
    pub(super) planar_materials: &'a [bool],
    pub(super) material_count: usize,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DrawPlanSelection<'a> {
    pub(super) skin_remap: &'a [u16],
    pub(super) bodygroup_choices: &'a [usize],
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PartitionPlanContext<'a> {
    pub(super) materials: DrawPlanMaterials<'a>,
    pub(super) selection: DrawPlanSelection<'a>,
    pub(super) camera_position: [f32; 4],
    pub(super) partition: PartitionFilter,
    pub(super) visibility_plan: Option<&'a WorldVisibilityPlan>,
    pub(super) receiver_encounters: &'a [ReceiverPlanSource],
    world_visibility: Option<WorldSurfaceVisibility<'a>>,
}

#[derive(Clone, Copy, Debug)]
struct WorldSurfaceVisibility<'a> {
    meshes: &'a HashMap<usize, bool>,
    faces: &'a HashSet<(usize, u32)>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(super) struct DrawPlanSourceSlices<'a> {
    pub(super) meshes: &'a [MeshPlanSource],
    pub(super) overlays: &'a [OverlayPlanSource],
    pub(super) materials: DrawPlanMaterials<'a>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DrawPlanRequest<'a> {
    pub(super) world_view: Option<&'a super::Uniforms>,
    pub(super) skybox_view: Option<&'a super::Uniforms>,
    pub(super) content_id: u64,
    pub(super) selection: DrawPlanSelection<'a>,
    pub(super) camera_position: [f32; 4],
    pub(super) map_skybox_visible: bool,
    pub(super) map_skybox_content_present: bool,
    pub(super) map_skybox_camera_position: Option<[f32; 4]>,
    pub(super) visibility_plan: Option<&'a WorldVisibilityPlan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PartitionFilter {
    World,
    MapSkybox,
}

impl PartitionFilter {
    pub(super) const fn accepts(self, map_skybox: bool) -> bool {
        match self {
            Self::World => !map_skybox,
            Self::MapSkybox => map_skybox,
        }
    }
}

pub(super) fn prepare_draw_plans(upload: &ViewerScene, request: DrawPlanRequest<'_>) -> DrawPlans {
    let world = prepare_partition_draw_plan(
        upload,
        request.selection.skin_remap,
        request.selection.bodygroup_choices,
        request.camera_position,
        PartitionFilter::World,
        request.visibility_plan,
        request.world_view,
    );
    let map_skybox = if request.map_skybox_visible && request.map_skybox_content_present {
        request.map_skybox_camera_position.map(|camera_position| {
            prepare_partition_draw_plan(
                upload,
                request.selection.skin_remap,
                request.selection.bodygroup_choices,
                camera_position,
                PartitionFilter::MapSkybox,
                None,
                request.skybox_view,
            )
        })
    } else {
        None
    };
    DrawPlans {
        content_id: request.content_id,
        world: Arc::new(world),
        map_skybox,
    }
}

pub(super) fn prepare_partition_draw_plan(
    upload: &ViewerScene,
    skin_remap: &[u16],
    bodygroup_choices: &[usize],
    camera_position: [f32; 4],
    partition: PartitionFilter,
    visibility_plan: Option<&WorldVisibilityPlan>,
    view: Option<&super::Uniforms>,
) -> DrawPlan {
    let receivers = receiver_encounters(upload, camera_position, partition, visibility_plan, view);
    let mut plan = prepare_partition_draw_plan_from_sources(
        upload
            .meshes
            .iter()
            .enumerate()
            .map(|(mesh_index, mesh)| MeshPlanSource {
                mesh_index,
                scene_mesh_index: mesh.scene_mesh_index,
                material_index: mesh.material_index,
                bodygroup: mesh.bodygroup,
                bodygroup_choice: mesh.bodygroup_choice,
                centroid: mesh.centroid(),
                map_skybox: mesh.map_skybox,
                door_index: mesh.door_index,
                door_visibility: mesh.door_visibility,
            }),
        upload
            .overlays
            .iter()
            .enumerate()
            .filter(|(_, mark)| {
                receivers.visible.as_ref().is_none_or(|receivers| {
                    mark.receiver_mesh_index
                        .is_some_and(|mesh| receivers.contains(&(mesh, mark.receiver_range.face)))
                })
            })
            .map(|(overlay_index, _)| overlay_source(upload, overlay_index)),
        PartitionPlanContext {
            materials: DrawPlanMaterials {
                render_modes: upload.assets.material_render_modes.as_slice(),
                planar_materials: upload.assets.material_planar_materials.as_slice(),
                material_count: upload.assets.material_bind_groups.len(),
            },
            selection: DrawPlanSelection {
                skin_remap,
                bodygroup_choices,
            },
            camera_position,
            partition,
            visibility_plan,
            receiver_encounters: &receivers.encounters,
            world_visibility: receivers.world_visibility(),
        },
    );
    attach_brushes(
        &mut plan,
        upload,
        receivers.visible.as_ref(),
        partition,
        camera_position,
    );
    world::attach(
        &mut plan,
        upload,
        receivers.visible.as_ref(),
        partition,
        &receivers.encounters,
    );
    planar::attach(&mut plan, upload, receivers.visible.as_ref());
    plan.transparency_leaves = receivers.leaves;
    plan
}

fn overlay_source(upload: &ViewerScene, overlay_index: usize) -> OverlayPlanSource {
    let overlay = &upload.overlays[overlay_index];
    OverlayPlanSource {
        owner: overlay.owner.id(),
        receiver_displacement: upload
            .assets
            .map()
            .and_then(|map| map.visibility.as_ref())
            .and_then(|visibility| visibility.surface_rendering(overlay.receiver_range.face))
            .is_some_and(|surface| surface.displacement.is_some()),
        overlay_index,
        material_index: overlay.material_index,
        receiver_mesh_index: overlay.receiver_mesh_index,
        receiver_range: overlay.receiver_range,
        receiver_material_index: overlay.receiver_material_index,
        receiver_material_group: overlay.receiver_material_group,
        receiver_lightmap_page: overlay.receiver_lightmap_page,
        source: overlay.source,
        decal_sort: overlay.decal_sort,
        render_order: overlay.render_order,
        map_skybox: overlay.map_skybox,
    }
}

pub(super) fn order_batched_marks(
    upload: &ViewerScene,
    marks: &[OverlayDrawItem],
    receivers: &[ReceiverPlanSource],
    order: ReceiverQueueOrder,
) -> Vec<OverlayDrawItem> {
    let sources = marks
        .iter()
        .map(|mark| overlay_source(upload, mark.overlay_index))
        .collect::<Vec<_>>();
    let items = marks
        .iter()
        .map(|mark| (mark.overlay_index, *mark))
        .collect::<HashMap<_, _>>();
    ordered_surface_marks_with_order(&sources, receivers, order)
        .into_iter()
        .map(|source| items[&source.overlay_index])
        .collect()
}

#[derive(Clone, Copy)]
pub(super) enum ReceiverQueueOrder {
    MaterialChains,
    Encounters,
}

fn attach_brushes(
    plan: &mut DrawPlan,
    upload: &ViewerScene,
    visible: Option<&HashSet<(usize, u32)>>,
    partition: PartitionFilter,
    camera: [f32; 4],
) {
    let Some(map) = upload.assets.map() else {
        return;
    };
    let items = plan
        .opaque
        .iter()
        .chain(&plan.translucent)
        .chain(&plan.additive)
        .map(|&item| (item.mesh_index, item))
        .collect::<HashMap<_, _>>();
    let mut grouped_meshes = HashSet::new();
    let mut batches = HashMap::new();
    let marked_faces = plan
        .receiver_marks
        .iter()
        .map(|mark| {
            (
                upload.overlays[mark.overlay_index].owner.id(),
                mark.receiver_range.face,
            )
        })
        .collect::<HashSet<_>>();
    for group in &upload.assets.brush_receiver_groups {
        let Some(&(first_mesh, first_source)) = group.receivers.first() else {
            continue;
        };
        let owner = map.surface_receivers[first_source].owner;
        let mut batch = BrushBatch {
            origin: upload.brush_origin(first_mesh, owner),
            local_sort_center: group.model.as_ref().map_or(Vec3::ZERO, |model| {
                let bounds = model.bounds();
                (bounds.min + bounds.max) * 0.5
            }),
            bounds: group
                .model
                .as_ref()
                .map(|model| upload.brush_bounds(first_mesh, owner, model.bounds())),
            opaque: SurfaceBatch::default(),
            transparent: Vec::new(),
        };
        let mut transparent_faces = HashMap::new();
        for &(mesh_index, source_index) in &group.receivers {
            let source = &map.surface_receivers[source_index];
            if !partition.accepts(upload.meshes[mesh_index].map_skybox) {
                continue;
            }
            let Some(&item) = items.get(&mesh_index) else {
                continue;
            };
            grouped_meshes.insert(mesh_index);
            if visible.is_some_and(|visible| !visible.contains(&(mesh_index, source.range.face))) {
                continue;
            }
            let end = source.range.start + source.range.count;
            let draw = IndexedDrawItem {
                item,
                indices: source.range.start..end,
            };
            if matches!(
                upload.assets.material_render_modes[item.material_slot],
                RenderMode::Opaque | RenderMode::Cutout
            ) {
                batch.opaque.push_mesh(draw);
            } else {
                let allocation = map
                    .surface_lightmaps
                    .as_ref()
                    .and_then(|layout| layout.surfaces.get(source.range.face as usize));
                let material = BrushMaterialKey {
                    material_group: allocation.map_or(source.material_index, |allocation| {
                        allocation.material_group
                    }),
                    lightmap_page: allocation
                        .map_or(SourceLightmapPage::White, |allocation| allocation.page),
                };
                transparent_faces.insert(source.range.face, (draw, material));
            }
        }
        let order = if transparent_faces.is_empty() {
            Vec::new()
        } else {
            group.model.as_ref().map_or_else(
                || {
                    let mut faces = transparent_faces.keys().copied().collect::<Vec<_>>();
                    faces.sort_unstable();
                    vec![gmpublished_domain::scene::map::MapBrushSurfaceGroup { faces }]
                },
                |model| {
                    model.translucent_groups(upload.brush_eye(
                        first_mesh,
                        owner,
                        Vec3::new(camera[0], camera[1], camera[2]),
                    ))
                },
            )
        };
        let mut face_groups = HashMap::new();
        let groups = brush_transparent_groups(order, |face| {
            transparent_faces
                .get(&face)
                .map(|(_, material)| BrushSurfaceState {
                    material: *material,
                    marked: marked_faces.contains(&(group.owner, face)),
                })
        });
        for faces in groups {
            let mut surface = SurfaceBatch::default();
            for face in faces {
                if let Some((draw, _)) = transparent_faces.remove(&face) {
                    face_groups.insert(face, batch.transparent.len());
                    surface.push_mesh(draw);
                }
            }
            if !surface.meshes.is_empty() {
                batch.transparent.push(surface);
            }
        }
        if !batch.opaque.meshes.is_empty() || !batch.transparent.is_empty() {
            batches.insert(group.owner, (batch, face_groups));
        }
    }
    for items in [&mut plan.opaque, &mut plan.translucent, &mut plan.additive] {
        items.retain(|item| !grouped_meshes.contains(&item.mesh_index));
    }
    for (marks, transparent) in [
        (&mut plan.surface_marks, false),
        (&mut plan.receiver_marks, true),
    ] {
        marks.retain(|mark| {
            let owner = upload.overlays[mark.overlay_index].owner.id();
            if owner == gmpublished_domain::scene::map::MapSurfaceOwnerId::World {
                return true;
            }
            if let Some((batch, faces)) = batches.get_mut(&owner) {
                if transparent {
                    if let Some(&index) = faces.get(&mark.receiver_range.face) {
                        batch.transparent[index].surface_marks.push(*mark);
                    }
                } else {
                    batch.opaque.surface_marks.push(*mark);
                }
            }
            false
        });
    }
    plan.brushes = upload
        .assets
        .brush_receiver_groups
        .iter()
        .filter_map(|group| batches.remove(&group.owner).map(|(batch, _)| batch))
        .collect();
}

fn brush_transparent_groups(
    nodes: impl IntoIterator<Item = gmpublished_domain::scene::map::MapBrushSurfaceGroup>,
    surface: impl Fn(u32) -> Option<BrushSurfaceState>,
) -> Vec<Vec<u32>> {
    let mut groups = Vec::<Vec<u32>>::new();
    let mut last = None::<(BrushMaterialKey, usize, bool)>;
    for node in nodes {
        let mut pending = Vec::new();
        let mut pending_last = None;
        let mut pending_marked = false;
        for face in node.faces {
            let Some(state) = surface(face) else {
                continue;
            };
            if let Some((material, index, marked)) = &mut last
                && *material == state.material
            {
                groups[*index].push(face);
                *marked |= state.marked;
            } else {
                pending.push(face);
                pending_last = Some(state.material);
                pending_marked |= state.marked;
            }
        }
        if let Some(material) = pending_last {
            last = Some((material, groups.len(), pending_marked));
            groups.push(pending);
        }
        if last.is_some_and(|(_, _, marked)| marked) {
            last = None;
        }
    }
    groups
}

impl SurfaceBatch {
    fn push_mesh(&mut self, draw: IndexedDrawItem) {
        if let Some(previous) = self.meshes.last_mut()
            && previous.item.mesh_index == draw.item.mesh_index
            && previous.indices.end == draw.indices.start
        {
            previous.indices.end = draw.indices.end;
        } else {
            self.meshes.push(draw);
        }
    }
}

#[derive(Default)]
struct ReceiverEncounters {
    encounters: Vec<ReceiverPlanSource>,
    visible: Option<HashSet<(usize, u32)>>,
    leaves: Option<Vec<gmpublished_domain::scene::map::MapWorldSurfaceGroup>>,
    world_meshes: HashMap<usize, bool>,
}

impl ReceiverEncounters {
    fn world_visibility(&self) -> Option<WorldSurfaceVisibility<'_>> {
        self.visible.as_ref().map(|faces| WorldSurfaceVisibility {
            meshes: &self.world_meshes,
            faces,
        })
    }
}

fn receiver_encounters(
    upload: &ViewerScene,
    camera: [f32; 4],
    partition: PartitionFilter,
    visibility: Option<&WorldVisibilityPlan>,
    view: Option<&super::Uniforms>,
) -> ReceiverEncounters {
    let Some(map) = upload
        .assets
        .map()
        .filter(|map| !map.surface_receivers.is_empty())
    else {
        return ReceiverEncounters::default();
    };
    let mut visible = HashSet::new();
    let eye = Vec3::new(camera[0], camera[1], camera[2]);
    let mut door_eyes = HashMap::new();
    for (&face, receivers) in &upload.assets.receiver_faces {
        for receiver in receivers {
            let source = &map.surface_receivers[receiver.source_index];
            let mesh = &upload.meshes[receiver.mesh_index];
            if !partition.accepts(mesh.map_skybox) {
                continue;
            }
            if source.owner != gmpublished_domain::scene::map::MapSurfaceOwner::World {
                let receiver_eye = mesh.door_index.map_or(eye, |door| {
                    *door_eyes
                        .entry(door)
                        .or_insert_with(|| upload.receiver_eye(receiver.mesh_index, eye))
                });
                if source.plane.signed_distance(receiver_eye) < -0.01 {
                    continue;
                }
            }
            if (map.visibility.is_none()
                || source.owner != gmpublished_domain::scene::map::MapSurfaceOwner::World)
                && visibility.is_some_and(|plan| {
                    !crate::media::preview_model::prop_visible(
                        &source.visibility,
                        &plan.visible_clusters,
                    )
                })
            {
                continue;
            }
            if source.owner != gmpublished_domain::scene::map::MapSurfaceOwner::World
                && mesh.door_index.is_none()
                && view.is_some_and(|view| !view.intersects_bounds(source.bounds))
            {
                continue;
            }
            visible.insert((receiver.mesh_index, face));
        }
    }
    let leaves = map.visibility.as_ref().map(|ordering| {
        ordering.world_surface_groups_in_view(
            eye,
            |face| {
                upload
                    .assets
                    .receiver_faces
                    .get(&face)
                    .is_some_and(|receivers| {
                        receivers
                            .iter()
                            .any(|receiver| visible.contains(&(receiver.mesh_index, face)))
                    })
            },
            |bounds| view.is_none_or(|view| view.intersects_bounds(bounds)),
            |cluster| {
                visibility.is_none_or(|plan| {
                    usize::try_from(cluster)
                        .ok()
                        .and_then(|index| plan.visible_clusters.get(index))
                        .copied()
                        .unwrap_or(false)
                })
            },
        )
    });
    let faces = leaves.as_ref().map_or_else(
        || {
            let mut faces = upload
                .assets
                .receiver_faces
                .keys()
                .copied()
                .collect::<Vec<_>>();
            faces.sort_unstable();
            faces
        },
        |groups| {
            groups
                .iter()
                .flat_map(|group| group.faces.iter().copied())
                .collect()
        },
    );
    let mut encounters = Vec::new();
    let ordered_faces = faces.iter().copied().collect::<HashSet<_>>();
    visible.retain(|&(mesh, face)| {
        ordered_faces.contains(&face)
            || upload
                .assets
                .receiver_faces
                .get(&face)
                .into_iter()
                .flatten()
                .any(|receiver| {
                    receiver.mesh_index == mesh
                        && map.surface_receivers[receiver.source_index].owner
                            != gmpublished_domain::scene::map::MapSurfaceOwner::World
                })
    });
    for face in faces {
        for receiver in upload
            .assets
            .receiver_faces
            .get(&face)
            .into_iter()
            .flatten()
        {
            if visible.contains(&(receiver.mesh_index, face)) {
                encounters.push(ReceiverPlanSource {
                    receiver_mesh_index: Some(receiver.mesh_index),
                    face,
                    material_group: receiver.material_group,
                    lightmap_page: receiver.lightmap_page,
                });
            }
        }
    }
    for group in &upload.assets.brush_receiver_groups {
        for &(mesh_index, source_index) in &group.receivers {
            let receiver = &map.surface_receivers[source_index];
            if visible.contains(&(mesh_index, receiver.range.face)) {
                let allocation = map
                    .surface_lightmaps
                    .as_ref()
                    .and_then(|layout| layout.surfaces.get(receiver.range.face as usize));
                encounters.push(ReceiverPlanSource {
                    receiver_mesh_index: Some(mesh_index),
                    face: receiver.range.face,
                    material_group: allocation.map_or(receiver.material_index, |allocation| {
                        allocation.material_group
                    }),
                    lightmap_page: allocation
                        .map_or(SourceLightmapPage::White, |allocation| allocation.page),
                });
            }
        }
    }
    let mut world_meshes = HashMap::new();
    if map.visibility.is_some() {
        for (&face, receivers) in &upload.assets.receiver_faces {
            for receiver in receivers {
                if map.surface_receivers[receiver.source_index].owner
                    == gmpublished_domain::scene::map::MapSurfaceOwner::World
                    && partition.accepts(upload.meshes[receiver.mesh_index].map_skybox)
                {
                    *world_meshes.entry(receiver.mesh_index).or_insert(false) |=
                        visible.contains(&(receiver.mesh_index, face));
                }
            }
        }
    }
    ReceiverEncounters {
        encounters,
        visible: Some(visible),
        leaves,
        world_meshes,
    }
}

#[cfg(test)]
pub(super) fn prepare_draw_plans_from_sources(
    sources: DrawPlanSourceSlices<'_>,
    request: DrawPlanRequest<'_>,
) -> DrawPlans {
    let world = prepare_partition_draw_plan_from_sources(
        sources.meshes.iter().copied(),
        sources.overlays.iter().copied(),
        PartitionPlanContext {
            materials: sources.materials,
            selection: request.selection,
            camera_position: request.camera_position,
            partition: PartitionFilter::World,
            visibility_plan: request.visibility_plan,
            receiver_encounters: &[],
            world_visibility: None,
        },
    );
    let map_skybox = if request.map_skybox_visible && request.map_skybox_content_present {
        request.map_skybox_camera_position.map(|camera_position| {
            prepare_partition_draw_plan_from_sources(
                sources.meshes.iter().copied(),
                sources.overlays.iter().copied(),
                PartitionPlanContext {
                    materials: sources.materials,
                    selection: request.selection,
                    camera_position,
                    partition: PartitionFilter::MapSkybox,
                    visibility_plan: None,
                    receiver_encounters: &[],
                    world_visibility: None,
                },
            )
        })
    } else {
        None
    };
    DrawPlans {
        content_id: request.content_id,
        world: Arc::new(world),
        map_skybox,
    }
}

pub(super) fn prepare_partition_draw_plan_from_sources(
    meshes: impl IntoIterator<Item = MeshPlanSource>,
    overlays: impl IntoIterator<Item = OverlayPlanSource>,
    context: PartitionPlanContext<'_>,
) -> DrawPlan {
    let mut plan = DrawPlan::default();
    let camera = [
        context.camera_position[0],
        context.camera_position[1],
        context.camera_position[2],
    ];
    for mesh in meshes {
        if !context.partition.accepts(mesh.map_skybox) {
            continue;
        }
        if let Some(&visible) = context
            .world_visibility
            .and_then(|world| world.meshes.get(&mesh.mesh_index))
        {
            if !visible {
                continue;
            }
        } else if context.partition == PartitionFilter::World
            && let Some(plan) = context.visibility_plan
        {
            let visible = if mesh.door_index.is_some() {
                mesh.door_visibility
                    .is_none_or(|bucket| plan.bucket_visible(bucket))
            } else {
                plan.mesh_visible(mesh.scene_mesh_index)
            };
            if !visible {
                continue;
            }
        }
        let selected = context
            .selection
            .bodygroup_choices
            .get(mesh.bodygroup)
            .copied()
            .unwrap_or(0);
        if mesh.bodygroup_choice != selected {
            continue;
        }
        let material_slot = remapped_material_slot(
            mesh.material_index,
            context.selection.skin_remap,
            context.materials.material_count,
        );
        let render_mode = context
            .materials
            .render_modes
            .get(material_slot)
            .copied()
            .unwrap_or(RenderMode::Opaque);
        let item = DrawItem {
            mesh_index: mesh.mesh_index,
            material_slot,
            distance_squared: mesh.centroid.distance_squared(Vec3::from(camera)),
        };
        if context
            .materials
            .planar_materials
            .get(material_slot)
            .copied()
            .unwrap_or(false)
        {
            plan.planar.push(item);
        } else {
            match render_mode {
                RenderMode::Opaque | RenderMode::Cutout => plan.opaque.push(item),
                RenderMode::Translucent | RenderMode::Modulate => plan.translucent.push(item),
                RenderMode::Additive => plan.additive.push(item),
            }
        }
    }
    let overlays = overlays
        .into_iter()
        .filter(|overlay| {
            context.partition.accepts(overlay.map_skybox)
                && if overlay.owner == gmpublished_domain::scene::map::MapSurfaceOwnerId::World
                    && let Some(mesh) = overlay.receiver_mesh_index
                    && let Some(world) = context.world_visibility
                    && world.meshes.contains_key(&mesh)
                {
                    world.faces.contains(&(mesh, overlay.receiver_range.face))
                } else {
                    context.partition != PartitionFilter::World
                        || context
                            .visibility_plan
                            .is_none_or(|plan| plan.overlay_visible(overlay.overlay_index))
                }
        })
        .collect::<Vec<_>>();
    for overlay in ordered_view_marks(
        &overlays,
        context.receiver_encounters,
        context.materials.render_modes,
    ) {
        let material_slot = overlay
            .material_index
            .min(context.materials.material_count.saturating_sub(1));
        let render_mode = context
            .materials
            .render_modes
            .get(material_slot)
            .copied()
            .unwrap_or(RenderMode::Opaque);
        let item = OverlayDrawItem {
            blend: render_mode,
            receiver_mesh_index: overlay.receiver_mesh_index,
            receiver_range: overlay.receiver_range,
            overlay_index: overlay.overlay_index,
            material_slot,
        };
        let receiver_mode = context
            .materials
            .render_modes
            .get(overlay.receiver_material_index)
            .copied()
            .unwrap_or(RenderMode::Opaque);
        if matches!(receiver_mode, RenderMode::Opaque | RenderMode::Cutout) {
            plan.surface_marks.push(item);
        } else {
            plan.receiver_marks.push(item);
        }
    }

    plan.translucent.sort_by(|left, right| {
        right
            .distance_squared
            .total_cmp(&left.distance_squared)
            .then_with(|| left.mesh_index.cmp(&right.mesh_index))
    });
    plan.planar.sort_by(|left, right| {
        right
            .distance_squared
            .total_cmp(&left.distance_squared)
            .then_with(|| left.mesh_index.cmp(&right.mesh_index))
    });
    plan
}

fn ordered_view_marks<'a>(
    marks: &'a [OverlayPlanSource],
    receivers: &[ReceiverPlanSource],
    modes: &[RenderMode],
) -> Vec<&'a OverlayPlanSource> {
    let mut batched = Vec::new();
    let mut immediate = Vec::<Vec<&OverlayPlanSource>>::new();
    let mut faces = HashMap::new();
    for mark in marks {
        let transparent_world = mark.owner
            == gmpublished_domain::scene::map::MapSurfaceOwnerId::World
            && !mark.receiver_displacement
            && !matches!(
                modes.get(mark.receiver_material_index),
                Some(RenderMode::Opaque | RenderMode::Cutout) | None
            );
        if transparent_world {
            let index = *faces
                .entry((mark.receiver_mesh_index, mark.receiver_range.face))
                .or_insert_with(|| {
                    immediate.push(Vec::new());
                    immediate.len() - 1
                });
            immediate[index].push(mark);
        } else {
            batched.push(mark);
        }
    }
    let mut ordered = ordered_surface_marks(batched, receivers);
    for face in immediate {
        let mut compiled = Vec::new();
        let mut decals = Vec::new();
        for mark in face {
            match mark.source {
                MapSurfaceMarkSource::Overlay { .. } => compiled.push(mark),
                MapSurfaceMarkSource::Decal { .. } => decals.push(mark),
            }
        }
        ordered.extend(ordered_surface_marks(compiled, &[]));
        decals.sort_by_key(|mark| match mark.source {
            MapSurfaceMarkSource::Decal { entity_index, .. } => entity_index,
            MapSurfaceMarkSource::Overlay { .. } => unreachable!(),
        });
        ordered.extend(decals);
    }
    ordered
}

fn ordered_surface_marks<'a>(
    marks: impl IntoIterator<Item = &'a OverlayPlanSource>,
    receiver_encounters: &[ReceiverPlanSource],
) -> Vec<&'a OverlayPlanSource> {
    ordered_surface_marks_with_order(
        marks,
        receiver_encounters,
        ReceiverQueueOrder::MaterialChains,
    )
}

fn ordered_surface_marks_with_order<'a>(
    marks: impl IntoIterator<Item = &'a OverlayPlanSource>,
    receiver_encounters: &[ReceiverPlanSource],
    order: ReceiverQueueOrder,
) -> Vec<&'a OverlayPlanSource> {
    let marks = marks.into_iter().collect::<Vec<_>>();
    let mut receiver_chains = Vec::new();
    let mut receiver_chain_indices = HashMap::new();
    let mut encountered_faces = HashSet::new();
    let mut receiver_sequence = Vec::new();
    for receiver in receiver_encounters
        .iter()
        .copied()
        .chain(marks.iter().map(|mark| ReceiverPlanSource {
            receiver_mesh_index: mark.receiver_mesh_index,
            face: mark.receiver_range.face,
            material_group: mark.receiver_material_group,
            lightmap_page: mark.receiver_lightmap_page,
        }))
    {
        let face = (receiver.receiver_mesh_index, receiver.face);
        if !encountered_faces.insert(face) {
            continue;
        }
        receiver_sequence.push(face);
        let next_index = receiver_chains.len();
        let index = *receiver_chain_indices
            .entry((receiver.material_group, receiver.lightmap_page))
            .or_insert_with(|| {
                receiver_chains.push(Vec::new());
                next_index
            });
        receiver_chains[index].push(face);
    }

    let mut face_fragments = HashMap::<_, Vec<&OverlayPlanSource>>::new();
    let mut decals = Vec::new();
    for &mark in &marks {
        match mark.source {
            MapSurfaceMarkSource::Overlay { .. } => {
                face_fragments
                    .entry((mark.receiver_mesh_index, mark.receiver_range.face))
                    .or_default()
                    .push(mark);
            }
            MapSurfaceMarkSource::Decal { .. } => decals.push(mark),
        }
    }
    for fragments in face_fragments.values_mut() {
        fragments.sort_by_key(|mark| {
            let MapSurfaceMarkSource::Overlay { index, .. } = mark.source else {
                unreachable!();
            };
            std::cmp::Reverse((index, mark.overlay_index))
        });
    }

    let mut material_buckets = Vec::<Vec<&OverlayPlanSource>>::new();
    let mut material_bucket_indices = HashMap::new();
    let receiver_sequence = match order {
        ReceiverQueueOrder::MaterialChains => receiver_chains.into_iter().flatten().collect(),
        ReceiverQueueOrder::Encounters => receiver_sequence,
    };
    for face in &receiver_sequence {
        for mark in face_fragments.get(face).into_iter().flatten() {
            let next_index = material_buckets.len();
            let index = *material_bucket_indices
                .entry((mark.material_index, mark.receiver_lightmap_page))
                .or_insert_with(|| {
                    material_buckets.push(Vec::new());
                    next_index
                });
            material_buckets[index].push(mark);
        }
    }

    // Source prepends both material buckets and their encountered fragments.
    let mut ordered = Vec::with_capacity(marks.len());
    for render_order in 0..=3 {
        for bucket in material_buckets.iter().rev() {
            ordered.extend(
                bucket
                    .iter()
                    .rev()
                    .copied()
                    .filter(|mark| mark.render_order == render_order),
            );
        }
    }
    ordered.extend(ordered_static_decals(&decals, receiver_encounters));
    ordered
}

fn ordered_static_decals<'a>(
    decals: &[&'a OverlayPlanSource],
    receiver_encounters: &[ReceiverPlanSource],
) -> Vec<&'a OverlayPlanSource> {
    let mut faces = HashMap::<_, Vec<&OverlayPlanSource>>::new();
    for &mark in decals {
        faces
            .entry((mark.receiver_mesh_index, mark.receiver_range.face))
            .or_default()
            .push(mark);
    }
    for fragments in faces.values_mut() {
        fragments.sort_by_key(|mark| match mark.source {
            MapSurfaceMarkSource::Decal { entity_index, .. } => {
                if mark.receiver_displacement {
                    usize::MAX - entity_index
                } else {
                    entity_index
                }
            }
            MapSurfaceMarkSource::Overlay { .. } => unreachable!(),
        });
    }
    let mut encountered = HashSet::new();
    let mut buckets = std::collections::BTreeMap::<_, Vec<&OverlayPlanSource>>::new();
    for face in receiver_encounters
        .iter()
        .map(|receiver| (receiver.receiver_mesh_index, receiver.face))
        .chain(
            decals
                .iter()
                .map(|mark| (mark.receiver_mesh_index, mark.receiver_range.face)),
        )
    {
        if !encountered.insert(face) {
            continue;
        }
        for &mark in faces.get(&face).into_iter().flatten() {
            let sort = mark
                .decal_sort
                .expect("static decal registration precedes view preparation");
            buckets.entry(sort).or_default().push(mark);
        }
    }
    buckets
        .into_values()
        .flat_map(|fragments| fragments.into_iter().rev())
        .collect()
}

pub(super) fn remapped_material_slot(
    material_index: usize,
    skin_remap: &[u16],
    material_count: usize,
) -> usize {
    let slot = skin_remap
        .get(material_index)
        .map_or(material_index, |&remapped| usize::from(remapped));
    slot.min(material_count.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brush_groups(
        nodes: &[&[u32]],
        materials: &[(usize, SourceLightmapPage)],
        marked: &[u32],
    ) -> Vec<Vec<u32>> {
        brush_transparent_groups(
            nodes.iter().map(
                |faces| gmpublished_domain::scene::map::MapBrushSurfaceGroup {
                    faces: faces.to_vec(),
                },
            ),
            |face| {
                materials
                    .get(face as usize)
                    .map(|&(material_group, lightmap_page)| BrushSurfaceState {
                        material: BrushMaterialKey {
                            material_group,
                            lightmap_page,
                        },
                        marked: marked.contains(&face),
                    })
            },
        )
    }

    #[test]
    fn transparent_brush_runs_absorb_all_equal_faces_before_pending_node_materials() {
        let a = (7, SourceLightmapPage::Physical(2));
        let b = (8, SourceLightmapPage::Physical(2));
        assert_eq!(
            brush_groups(&[&[0], &[1, 2, 3, 4]], &[a, b, a, b, a], &[]),
            [vec![0, 2, 4], vec![1, 3]]
        );
    }

    #[test]
    fn transparent_brush_marks_seal_the_current_flush_group_at_node_end() {
        let a = (7, SourceLightmapPage::Physical(2));
        assert_eq!(
            brush_groups(&[&[0], &[1], &[2], &[3]], &[a; 4], &[1]),
            [vec![0, 1], vec![2, 3]]
        );
        assert_eq!(
            brush_groups(&[&[0], &[1], &[2]], &[a; 3], &[0, 1, 2]),
            [vec![0], vec![1], vec![2]]
        );
    }

    #[test]
    fn transparent_brush_merged_marks_do_not_seal_a_later_unmarked_group() {
        let a = (7, SourceLightmapPage::Physical(2));
        let b = (8, SourceLightmapPage::Physical(2));
        assert_eq!(
            brush_groups(&[&[0], &[1, 2], &[3]], &[a, b, a, b], &[2]),
            [vec![0, 2], vec![1, 3]]
        );
    }

    #[test]
    fn transparent_brush_run_equality_uses_material_identity_and_signed_source_page() {
        let white = (7, SourceLightmapPage::White);
        let white_bump = (7, SourceLightmapPage::WhiteBump);
        let physical = (7, SourceLightmapPage::Physical(0));
        assert_eq!(
            brush_groups(
                &[&[0], &[1], &[2], &[3], &[4]],
                &[white, white, white_bump, physical, physical],
                &[],
            ),
            [vec![0, 1], vec![2], vec![3, 4]]
        );
    }

    #[test]
    fn transparent_brush_hidden_faces_do_not_create_groups_or_mark_barriers() {
        let a = (7, SourceLightmapPage::White);
        assert_eq!(
            brush_groups(&[&[0], &[99], &[], &[1]], &[a; 2], &[99]),
            [vec![0, 1]]
        );
    }

    fn mesh_plan_source(mesh_index: usize, map_skybox: bool, centroid: Vec3) -> MeshPlanSource {
        MeshPlanSource {
            mesh_index,
            scene_mesh_index: mesh_index,
            material_index: 0,
            bodygroup: 0,
            bodygroup_choice: 0,
            centroid,
            map_skybox,
            door_index: None,
            door_visibility: None,
        }
    }

    fn overlay_plan_source(
        overlay_index: usize,
        map_skybox: bool,
        _centroid: Vec3,
    ) -> OverlayPlanSource {
        OverlayPlanSource {
            owner: gmpublished_domain::scene::map::MapSurfaceOwnerId::World,
            receiver_displacement: false,
            receiver_mesh_index: None,
            receiver_range: gmpublished_domain::scene::map::MapMeshIndexRange {
                face: 0,
                start: 0,
                count: 3,
            },
            receiver_material_index: 0,
            receiver_material_group: 0,
            receiver_lightmap_page: SourceLightmapPage::Physical(0),
            source: gmpublished_domain::scene::map::MapSurfaceMarkSource::Overlay {
                index: overlay_index,
                id: overlay_index as i32,
            },
            decal_sort: None,
            render_order: 0,
            overlay_index,
            material_index: 0,
            map_skybox,
        }
    }

    #[test]
    fn canonical_world_faces_override_parent_pvs_for_geometry_and_marks_only() {
        let raw_visibility = WorldVisibilityPlan {
            mesh_indices: vec![Vec::new(); 3],
            overlay_visible: vec![false; 3],
            detail_sprite_visible: Vec::new(),
            visible_clusters: vec![false, true],
            visible_cluster_count: 1,
        };
        let world_meshes = HashMap::from([(0, true), (1, false)]);
        let selected_faces = HashSet::from([(0, 3)]);
        let base = OverlayPlanSource {
            receiver_mesh_index: Some(0),
            receiver_displacement: true,
            receiver_range: gmpublished_domain::scene::map::MapMeshIndexRange {
                face: 3,
                start: 0,
                count: 3,
            },
            ..overlay_plan_source(0, false, Vec3::ZERO)
        };
        let plan = prepare_partition_draw_plan_from_sources(
            (0..3).map(|index| mesh_plan_source(index, false, Vec3::ZERO)),
            [
                base,
                OverlayPlanSource {
                    overlay_index: 1,
                    receiver_range: gmpublished_domain::scene::map::MapMeshIndexRange {
                        face: 4,
                        ..base.receiver_range
                    },
                    ..base
                },
                OverlayPlanSource {
                    overlay_index: 2,
                    owner: gmpublished_domain::scene::map::MapSurfaceOwnerId::Brush(7),
                    receiver_mesh_index: Some(2),
                    ..base
                },
            ],
            PartitionPlanContext {
                materials: DrawPlanMaterials {
                    render_modes: &[RenderMode::Opaque],
                    planar_materials: &[],
                    material_count: 1,
                },
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0; 4],
                partition: PartitionFilter::World,
                visibility_plan: Some(&raw_visibility),
                receiver_encounters: &[],
                world_visibility: Some(WorldSurfaceVisibility {
                    meshes: &world_meshes,
                    faces: &selected_faces,
                }),
            },
        );
        assert_eq!(
            plan.opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            [0]
        );
        assert_eq!(
            plan.surface_marks
                .iter()
                .map(|mark| mark.overlay_index)
                .collect::<Vec<_>>(),
            [0]
        );
    }

    #[test]
    fn marks_follow_receiver_phase_and_authored_layer_order() {
        use gmpublished_domain::scene::map::MapSurfaceMarkSource;
        let base = overlay_plan_source(0, false, Vec3::ZERO);
        let overlays = [
            OverlayPlanSource {
                material_index: 1,
                render_order: 2,
                ..base
            },
            OverlayPlanSource {
                overlay_index: 1,
                material_index: 2,
                source: MapSurfaceMarkSource::Decal {
                    entity_index: 7,
                    permanence: Default::default(),
                },
                decal_sort: Some(decal_sort(0, 0)),
                ..base
            },
            OverlayPlanSource {
                overlay_index: 2,
                material_index: 1,
                receiver_material_index: 1,
                receiver_mesh_index: Some(3),
                ..base
            },
            OverlayPlanSource {
                overlay_index: 3,
                material_index: 1,
                render_order: 0,
                ..base
            },
        ];
        let plan = prepare_partition_draw_plan_from_sources(
            [],
            overlays,
            PartitionPlanContext {
                materials: DrawPlanMaterials {
                    render_modes: &[
                        RenderMode::Opaque,
                        RenderMode::Translucent,
                        RenderMode::Modulate,
                    ],
                    planar_materials: &[],
                    material_count: 3,
                },
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0; 4],
                partition: PartitionFilter::World,
                visibility_plan: None,
                receiver_encounters: &[],
                world_visibility: None,
            },
        );
        assert_eq!(
            plan.surface_marks
                .iter()
                .map(|item| item.overlay_index)
                .collect::<Vec<_>>(),
            [3, 0, 1]
        );
        assert_eq!(plan.surface_marks[2].blend, RenderMode::Modulate);
        assert_eq!(plan.receiver_marks.len(), 1);
        assert_eq!(plan.receiver_marks[0].receiver_mesh_index, Some(3));
    }

    fn mark(index: usize, material: usize, face: u32) -> OverlayPlanSource {
        let mut mark = overlay_plan_source(index, false, Vec3::ZERO);
        mark.material_index = material;
        mark.receiver_range.face = face;
        mark
    }

    fn ordered_indices(
        marks: &[OverlayPlanSource],
        receivers: &[ReceiverPlanSource],
    ) -> Vec<usize> {
        ordered_surface_marks(marks, receivers)
            .iter()
            .map(|mark| mark.overlay_index)
            .collect()
    }

    fn decal_sort(
        format_group: usize,
        bucket: usize,
    ) -> crate::media::preview_model::StaticDecalSort {
        crate::media::preview_model::StaticDecalSort {
            pass: crate::media::preview_model::StaticDecalPass::PermanentLightmapped,
            format_group,
            material_page_bucket: bucket,
        }
    }

    fn static_mark(index: usize, material: usize, face: u32, bucket: usize) -> OverlayPlanSource {
        let mut mark = mark(index, material, face);
        mark.source = MapSurfaceMarkSource::Decal {
            entity_index: index,
            permanence: Default::default(),
        };
        mark.decal_sort = Some(decal_sort(0, bucket));
        mark
    }

    #[test]
    fn transparent_world_decals_use_creation_order_while_brushes_keep_buckets() {
        let mut marks = [
            static_mark(2, 4, 7, 0),
            static_mark(0, 4, 7, 0),
            static_mark(1, 2, 7, 1),
        ];
        let indices = |marks: &[OverlayPlanSource]| {
            ordered_view_marks(marks, &[], &[RenderMode::Translucent])
                .iter()
                .map(|mark| mark.overlay_index)
                .collect::<Vec<_>>()
        };
        assert_eq!(indices(&marks), [0, 1, 2]);
        for mark in &mut marks {
            mark.owner = gmpublished_domain::scene::map::MapSurfaceOwnerId::Brush(6);
        }
        assert_eq!(indices(&marks), [2, 0, 1]);
    }

    #[test]
    fn transparent_world_compiled_buckets_restart_for_each_receiver_face() {
        let marks = [mark(2, 4, 9), mark(0, 4, 7), mark(1, 2, 7)];
        let receivers = [9, 7].map(|face| ReceiverPlanSource {
            receiver_mesh_index: None,
            face,
            material_group: 0,
            lightmap_page: SourceLightmapPage::Physical(0),
        });
        let indices = ordered_view_marks(&marks, &receivers, &[RenderMode::Translucent])
            .into_iter()
            .filter(|mark| mark.receiver_range.face == 7)
            .map(|mark| mark.overlay_index)
            .collect::<Vec<_>>();
        assert_eq!(indices, [0, 1]);
        assert_eq!(ordered_indices(&marks, &receivers), [1, 0, 2]);
    }

    #[test]
    fn displacement_static_chain_prepend_cancels_bucket_prepend_per_face() {
        let mut marks = [static_mark(0, 4, 7, 0), static_mark(1, 4, 7, 0)];
        assert_eq!(ordered_indices(&marks, &[]), [1, 0]);
        for mark in &mut marks {
            mark.receiver_displacement = true;
        }
        assert_eq!(ordered_indices(&marks, &[]), [0, 1]);
    }

    #[test]
    fn raw_translucent_displacement_receivers_preserve_interleaved_materials() {
        let mut marks = [mark(0, 4, 7), mark(1, 4, 8), mark(2, 4, 9)];
        marks[0].receiver_material_group = 0;
        marks[1].receiver_material_group = 1;
        marks[2].receiver_material_group = 0;
        let receivers = marks.map(|mark| ReceiverPlanSource {
            receiver_mesh_index: mark.receiver_mesh_index,
            face: mark.receiver_range.face,
            material_group: mark.receiver_material_group,
            lightmap_page: mark.receiver_lightmap_page,
        });
        let indices = |order| {
            ordered_surface_marks_with_order(&marks, &receivers, order)
                .into_iter()
                .map(|mark| mark.overlay_index)
                .collect::<Vec<_>>()
        };
        assert_eq!(indices(ReceiverQueueOrder::Encounters), [2, 1, 0]);
        assert_eq!(indices(ReceiverQueueOrder::MaterialChains), [1, 2, 0]);
    }

    #[test]
    fn static_decals_group_materials_before_reversing_receiver_encounters() {
        let marks = [
            static_mark(0, 4, 7, 0),
            static_mark(1, 2, 7, 1),
            static_mark(2, 4, 7, 0),
        ];
        assert_eq!(ordered_indices(&marks, &[]), [2, 0, 1]);
        let mut reversed = marks;
        reversed.reverse();
        assert_eq!(ordered_indices(&reversed, &[]), [2, 0, 1]);
    }

    #[test]
    fn static_decals_use_raw_face_encounters_instead_of_receiver_material_chains() {
        let mut marks = [
            static_mark(0, 4, 7, 0),
            static_mark(1, 4, 9, 0),
            static_mark(2, 4, 8, 0),
        ];
        marks[0].receiver_material_group = 1;
        marks[2].receiver_material_group = 1;
        let receivers = [7, 9, 8].map(|face| ReceiverPlanSource {
            receiver_mesh_index: None,
            face,
            material_group: usize::from(face != 9),
            lightmap_page: SourceLightmapPage::Physical(0),
        });
        assert_eq!(ordered_indices(&marks, &receivers), [2, 1, 0]);
    }

    #[test]
    fn static_decal_pass_precedes_global_format_and_material_page_registration() {
        use crate::media::preview_model::StaticDecalPass;
        let mut marks = [
            static_mark(0, 4, 7, 0),
            static_mark(1, 2, 7, 1),
            static_mark(2, 4, 7, 0),
        ];
        marks[0].decal_sort.as_mut().unwrap().pass = StaticDecalPass::Unlit;
        marks[1].decal_sort.as_mut().unwrap().pass = StaticDecalPass::NonPermanentLightmapped;
        marks[2].decal_sort.as_mut().unwrap().format_group = 1;
        assert_eq!(ordered_indices(&marks, &[]), [2, 1, 0]);
    }

    #[test]
    fn shared_material_fragments_keep_lump_order_inside_reverse_material_buckets() {
        let mut marks = [mark(0, 4, 7), mark(1, 2, 7), mark(2, 4, 7)];
        for (mark, id) in marks.iter_mut().zip([91, -10, 0]) {
            let MapSurfaceMarkSource::Overlay { index, .. } = mark.source else {
                unreachable!();
            };
            mark.source = MapSurfaceMarkSource::Overlay { index, id };
        }
        assert_eq!(ordered_indices(&marks, &[]), [1, 0, 2]);
        marks.reverse();
        assert_eq!(ordered_indices(&marks, &[]), [1, 0, 2]);
    }

    #[test]
    fn unmarked_receivers_prime_material_chains_before_marked_faces() {
        let mut marks = [mark(0, 7, 30), mark(1, 8, 20)];
        marks[0].receiver_material_index = 99;
        marks[1].receiver_material_index = 88;
        marks[0].receiver_material_group = 4;
        marks[1].receiver_material_group = 2;
        let receiver = |face, material_group| ReceiverPlanSource {
            receiver_mesh_index: None,
            face,
            material_group,
            lightmap_page: SourceLightmapPage::Physical(0),
        };
        assert_eq!(
            ordered_indices(&marks, &[receiver(20, 2), receiver(30, 4)]),
            [0, 1]
        );
        assert_eq!(
            ordered_indices(&marks, &[receiver(10, 4), receiver(20, 2), receiver(30, 4)]),
            [1, 0]
        );
    }

    #[test]
    fn lightmap_pages_separate_receiver_and_mark_material_buckets() {
        let mut marks = [mark(0, 4, 0), mark(1, 2, 1), mark(2, 4, 2)];
        assert_eq!(ordered_indices(&marks, &[]), [1, 2, 0]);
        marks[2].receiver_lightmap_page = SourceLightmapPage::Physical(1);
        assert_eq!(ordered_indices(&marks, &[]), [2, 1, 0]);
    }

    #[test]
    fn placed_brush_receivers_with_one_bsp_face_keep_separate_encounters() {
        let mut marks = [mark(0, 4, 7), mark(1, 4, 7)];
        marks[0].receiver_mesh_index = Some(0);
        marks[1].receiver_mesh_index = Some(1);
        assert_eq!(ordered_indices(&marks, &[]), [1, 0]);
    }

    #[test]
    fn render_order_precedes_material_buckets_and_static_decals_follow_overlays() {
        let mut marks = [mark(0, 4, 7), mark(1, 2, 7), mark(2, 4, 7), mark(3, 8, 7)];
        marks[0].render_order = 2;
        marks[3].source = MapSurfaceMarkSource::Decal {
            entity_index: 3,
            permanence: Default::default(),
        };
        marks[3].decal_sort = Some(decal_sort(0, 0));
        assert_eq!(ordered_indices(&marks, &[]), [1, 2, 0, 3]);
    }

    #[test]
    fn water_meshes_are_partitioned_and_sorted_back_to_front() {
        let meshes = [
            mesh_plan_source(0, false, Vec3::new(1.0, 0.0, 0.0)),
            MeshPlanSource {
                mesh_index: 1,
                material_index: 1,
                centroid: Vec3::new(2.0, 0.0, 0.0),
                ..mesh_plan_source(1, false, Vec3::new(2.0, 0.0, 0.0))
            },
            MeshPlanSource {
                mesh_index: 2,
                material_index: 1,
                centroid: Vec3::new(4.0, 0.0, 0.0),
                ..mesh_plan_source(2, false, Vec3::new(4.0, 0.0, 0.0))
            },
            MeshPlanSource {
                mesh_index: 3,
                material_index: 2,
                centroid: Vec3::new(3.0, 0.0, 0.0),
                ..mesh_plan_source(3, false, Vec3::new(3.0, 0.0, 0.0))
            },
        ];
        let render_modes = [
            RenderMode::Opaque,
            RenderMode::Translucent,
            RenderMode::Translucent,
        ];
        let planar_materials = [false, true, false];

        let plans = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &planar_materials,
                    material_count: 3,
                },
            },
            DrawPlanRequest {
                world_view: None,
                skybox_view: None,
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0; 4],
                map_skybox_visible: false,
                map_skybox_content_present: false,
                map_skybox_camera_position: None,
                visibility_plan: None,
            },
        );

        assert_eq!(
            plans
                .world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            plans
                .world
                .planar
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(
            plans
                .world
                .translucent
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[test]
    fn map_skybox_disabled_omits_skybox_sets_from_draw_plans() {
        let meshes = [
            mesh_plan_source(0, false, Vec3::new(0.0, 0.0, 0.0)),
            mesh_plan_source(1, true, Vec3::new(10.0, 0.0, 0.0)),
        ];
        let overlays = [overlay_plan_source(0, true, Vec3::new(12.0, 0.0, 0.0))];
        let render_modes = [RenderMode::Opaque];
        let plans = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &overlays,
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                world_view: None,
                skybox_view: None,
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: false,
                map_skybox_content_present: true,
                map_skybox_camera_position: Some([1.0, 0.0, 0.0, 0.0]),
                visibility_plan: None,
            },
        );

        assert_eq!(plans.content_id, 7);
        assert_eq!(
            plans
                .world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert!(plans.world.surface_marks.is_empty());
        assert!(plans.map_skybox.is_none());
    }

    #[test]
    fn missing_sky_camera_omits_composite_plan() {
        let meshes = [mesh_plan_source(0, true, Vec3::new(10.0, 0.0, 0.0))];
        let render_modes = [RenderMode::Opaque];
        let plans = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                world_view: None,
                skybox_view: None,
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: true,
                map_skybox_content_present: true,
                map_skybox_camera_position: None,
                visibility_plan: None,
            },
        );

        assert!(plans.world.opaque.is_empty());
        assert!(plans.map_skybox.is_none());
    }

    #[test]
    fn empty_skybox_partition_omits_composite_plan() {
        let meshes = [mesh_plan_source(0, false, Vec3::new(0.0, 0.0, 0.0))];
        let render_modes = [RenderMode::Opaque];
        let plans = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                world_view: None,
                skybox_view: None,
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: true,
                map_skybox_content_present: false,
                map_skybox_camera_position: Some([1.0, 0.0, 0.0, 0.0]),
                visibility_plan: None,
            },
        );

        assert_eq!(plans.world.opaque.len(), 1);
        assert!(plans.map_skybox.is_none());
    }

    #[test]
    fn visibility_plan_filters_world_meshes_but_off_path_draws_everything() {
        let meshes = [
            mesh_plan_source(0, false, Vec3::new(0.0, 0.0, 0.0)),
            mesh_plan_source(1, false, Vec3::new(10.0, 0.0, 0.0)),
        ];
        let render_modes = [RenderMode::Opaque];
        let visibility = WorldVisibilityPlan {
            mesh_indices: vec![vec![0, 1, 2], Vec::new()],
            overlay_visible: Vec::new(),
            detail_sprite_visible: Vec::new(),
            visible_clusters: vec![true],
            visible_cluster_count: 1,
        };

        let off = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                world_view: None,
                skybox_view: None,
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: false,
                map_skybox_content_present: false,
                map_skybox_camera_position: None,
                visibility_plan: None,
            },
        );
        let on = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                world_view: None,
                skybox_view: None,
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: false,
                map_skybox_content_present: false,
                map_skybox_camera_position: None,
                visibility_plan: Some(&visibility),
            },
        );

        assert_eq!(
            off.world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(
            on.world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0]
        );
    }

    #[test]
    fn visibility_plan_lookup_uses_scene_index_not_upload_index() {
        let meshes = [
            mesh_plan_source(0, false, Vec3::new(0.0, 0.0, 0.0)),
            MeshPlanSource {
                mesh_index: 1,
                scene_mesh_index: 2,
                material_index: 0,
                bodygroup: 0,
                bodygroup_choice: 0,
                centroid: Vec3::new(10.0, 0.0, 0.0),
                map_skybox: false,
                door_index: None,
                door_visibility: None,
            },
        ];
        let render_modes = [RenderMode::Opaque];
        let visibility = WorldVisibilityPlan {
            mesh_indices: vec![vec![0, 1, 2], Vec::new(), vec![3, 4, 5]],
            overlay_visible: Vec::new(),
            detail_sprite_visible: Vec::new(),
            visible_clusters: vec![true],
            visible_cluster_count: 1,
        };

        let on = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    planar_materials: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                world_view: None,
                skybox_view: None,
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: false,
                map_skybox_content_present: false,
                map_skybox_camera_position: None,
                visibility_plan: Some(&visibility),
            },
        );

        assert_eq!(
            on.world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn visibility_plan_filters_door_meshes_by_bucket_not_scene_index() {
        let meshes = [
            mesh_plan_source(0, false, Vec3::new(0.0, 0.0, 0.0)),
            MeshPlanSource {
                mesh_index: 1,
                scene_mesh_index: usize::MAX,
                material_index: 0,
                bodygroup: 0,
                bodygroup_choice: 0,
                centroid: Vec3::new(10.0, 0.0, 0.0),
                map_skybox: false,
                door_index: Some(0),
                door_visibility: Some(MapVisibilityBucket::Cluster(1)),
            },
        ];
        let render_modes = [RenderMode::Opaque];
        let hidden = WorldVisibilityPlan {
            mesh_indices: vec![vec![0, 1, 2]],
            overlay_visible: Vec::new(),
            detail_sprite_visible: Vec::new(),
            visible_clusters: vec![true, false],
            visible_cluster_count: 1,
        };
        let visible = WorldVisibilityPlan {
            visible_clusters: vec![true, true],
            visible_cluster_count: 2,
            ..hidden.clone()
        };

        let plan = |visibility_plan| {
            prepare_draw_plans_from_sources(
                DrawPlanSourceSlices {
                    meshes: &meshes,
                    overlays: &[],
                    materials: DrawPlanMaterials {
                        render_modes: &render_modes,
                        planar_materials: &[],
                        material_count: 1,
                    },
                },
                DrawPlanRequest {
                    world_view: None,
                    skybox_view: None,
                    content_id: 7,
                    selection: DrawPlanSelection {
                        skin_remap: &[],
                        bodygroup_choices: &[],
                    },
                    camera_position: [0.0, 0.0, 0.0, 0.0],
                    map_skybox_visible: false,
                    map_skybox_content_present: false,
                    map_skybox_camera_position: None,
                    visibility_plan: Some(visibility_plan),
                },
            )
        };

        assert_eq!(
            plan(&hidden)
                .world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            plan(&visible)
                .world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }
}
