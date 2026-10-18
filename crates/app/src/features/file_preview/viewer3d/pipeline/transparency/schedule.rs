use super::super::super::draw_plan::{
    ReceiverPlanSource, ReceiverQueueOrder, WorldPreparationIssue, order_batched_marks,
    validate_rendering,
};
use super::super::{DrawItem, DrawPlan, Uniforms, ViewerScene};
use super::details::{DetailDraw, DetailProfile};
use super::{Draw, DrawGroup, Primitive, WorldDrawGroups, depth};
use gmpublished_domain::math::Vec3;
use gmpublished_domain::scene::map::{
    MapBounds, MapPropLeafMembership, MapSurfaceOwnerId, MapWorldSurfaceGroupRef,
    MapWorldSurfaceGroups, MapWorldSurfaceRendering,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::OnceLock;

enum MeshItems<'a> {
    Dense(Vec<Option<&'a DrawItem>>),
    #[cfg(test)]
    Standard(HashMap<usize, DrawItem>),
}

impl<'a> MeshItems<'a> {
    fn new(mesh_count: usize, items: impl Iterator<Item = &'a DrawItem>) -> Self {
        Self::dense(mesh_count, items)
    }

    fn dense(mesh_count: usize, items: impl Iterator<Item = &'a DrawItem>) -> Self {
        let mut items = items.peekable();
        if items.peek().is_none() {
            return Self::Dense(Vec::new());
        }
        let mut selected = vec![None; mesh_count];
        for item in items {
            if let Some(slot) = selected.get_mut(item.mesh_index) {
                *slot = Some(item);
            }
        }
        Self::Dense(selected)
    }

    #[cfg(test)]
    fn standard(items: impl Iterator<Item = &'a DrawItem>) -> Self {
        Self::Standard(items.map(|&item| (item.mesh_index, item)).collect())
    }

    fn get(&self, mesh: usize) -> Option<DrawItem> {
        match self {
            Self::Dense(selected) => selected.get(mesh).copied().flatten().copied(),
            #[cfg(test)]
            Self::Standard(selected) => selected.get(&mesh).copied(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum MeshCoverage<'a> {
    World,
    Ranges(&'a [Range<u32>]),
}

impl MeshCoverage<'_> {
    pub(super) fn covers(self, indices: Option<&Range<u32>>) -> bool {
        match self {
            Self::World => true,
            Self::Ranges(ranges) => indices.is_some_and(|indices| {
                ranges
                    .iter()
                    .any(|range| range.start <= indices.start && range.end >= indices.end)
            }),
        }
    }
}

struct WorldFace {
    draw: DrawGroup,
    rendering: MapWorldSurfaceRendering,
    receiver: ReceiverPlanSource,
}

struct Owner {
    draw: DrawGroup,
    origin: Vec3,
}

impl Owner {
    fn from_operations(
        depth: f32,
        origin: Vec3,
        operations: Range<usize>,
        emit_geometry: bool,
    ) -> Option<Self> {
        if emit_geometry && operations.is_empty() {
            return None;
        }
        // A geometrically culled owner still drains details and acts as a
        // depth barrier when compose merges the remaining draw queue.
        Some(Self {
            draw: DrawGroup { depth, operations },
            origin,
        })
    }
}

struct Sprite {
    draw: DrawGroup,
    distance_squared: f32,
}

#[derive(Default)]
struct LeafDetails {
    fast: Vec<Sprite>,
    ordinary: Vec<Sprite>,
    fast_cursor: usize,
    ordinary_cursor: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum ScheduleUnavailable {
    NotMap,
    MissingTopology,
    MissingReceiverPreparation,
    UnsupportedTraversalContext,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum RejectedResource {
    WorldSurface(WorldPreparationIssue),
    LeafWaterMetadata { leaf: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum FallbackResource {
    BrushBounds { brush: usize },
    BrushLeafLookup { brush: usize },
    RenderableBounds { source: usize },
    RenderableLeafLookup { source: usize },
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Diagnostics {
    pub unavailable: Option<ScheduleUnavailable>,
    pub rejected: Vec<RejectedResource>,
    pub fallback: Vec<FallbackResource>,
}

fn prepared_leaves(
    leaves: Option<&MapWorldSurfaceGroups>,
    has_receivers: bool,
) -> Result<&MapWorldSurfaceGroups, ScheduleUnavailable> {
    leaves.ok_or(if has_receivers {
        ScheduleUnavailable::UnsupportedTraversalContext
    } else {
        ScheduleUnavailable::MissingReceiverPreparation
    })
}

#[derive(Default)]
pub(super) struct Schedule {
    operations: Vec<Draw>,
    groups: Vec<DrawGroup>,
    world_meshes: HashSet<usize>,
    ranges: HashMap<usize, Vec<Range<u32>>>,
    brushes: HashSet<usize>,
    pub(super) diagnostics: Diagnostics,
}

impl Schedule {
    pub(super) fn prepare(
        upload: &ViewerScene,
        plan: &DrawPlan,
        uniforms: &Uniforms,
        skybox: bool,
        groups: WorldDrawGroups,
        pvs_limited: bool,
        details: &[DetailDraw],
    ) -> Result<Self, ScheduleUnavailable> {
        let map = upload.assets.map().ok_or(ScheduleUnavailable::NotMap)?;
        let topology = map
            .visibility
            .as_ref()
            .ok_or(ScheduleUnavailable::MissingTopology)?;
        let mut diagnostics = Diagnostics::default();
        let leaves = pruned_leaves(
            prepared_leaves(
                plan.transparency_leaves.as_ref(),
                !map.surface_receivers.is_empty(),
            )?,
            groups.water_side,
            |leaf| topology.leaf_is_underwater(leaf),
            &mut diagnostics.rejected,
        );
        let indices = leaves
            .iter()
            .enumerate()
            .map(|(index, leaf)| (leaf.leaf_index, index))
            .collect::<HashMap<_, _>>();
        let items = MeshItems::new(
            upload.meshes.len(),
            plan.translucent.iter().chain(&plan.additive),
        );
        let mut schedule = Self {
            operations: Vec::new(),
            groups: Vec::new(),
            world_meshes: HashSet::new(),
            ranges: HashMap::new(),
            brushes: HashSet::new(),
            diagnostics,
        };
        let mut world = HashMap::<u32, WorldFace>::new();
        let mut marks = HashMap::<u32, Vec<_>>::new();
        for mark in &plan.receiver_marks {
            marks
                .entry(mark.receiver_range.face)
                .or_default()
                .push(mark);
        }
        for face in upload.assets.receiver_faces.world_faces_for_meshes(
            plan.translucent
                .iter()
                .chain(&plan.additive)
                .map(|item| item.mesh_index),
        ) {
            let receivers = upload.assets.receiver_faces.get(&face).unwrap();
            let receivers = receivers.iter().filter(|receiver| {
                map.surface_receivers[receiver.source_index].owner.id() == MapSurfaceOwnerId::World
                    && items.get(receiver.mesh_index).is_some()
            });
            if receivers.clone().next().is_none() {
                continue;
            }
            let Some(rendering) = schedule.admit_world_face(
                face,
                receivers.clone().map(|receiver| receiver.mesh_index),
                topology.surface_rendering(face),
                map.displacement_batches.contains_key(&face),
            ) else {
                continue;
            };
            let first = schedule.operations.len();
            let mut distance = 0.0;
            let mut encounter = None;
            for receiver in receivers {
                let source = &map.surface_receivers[receiver.source_index];
                let item = items
                    .get(receiver.mesh_index)
                    .expect("admitted receiver mesh");
                encounter = Some(ReceiverPlanSource {
                    receiver_mesh_index: Some(item.mesh_index),
                    face,
                    material_group: receiver.material_group,
                    lightmap_page: receiver.lightmap_page,
                });
                distance = depth(uniforms, (source.bounds.min + source.bounds.max) * 0.5);
                schedule.operations.push(mesh_draw(
                    upload,
                    item,
                    source.range.start..source.range.start + source.range.count,
                    distance,
                ));
            }
            if first == schedule.operations.len() {
                continue;
            }
            if rendering.displacement.is_none() {
                schedule
                    .operations
                    .extend(marks.get(&face).into_iter().flatten().map(|&&mark| Draw {
                        primitive: Primitive::Overlay(mark),
                        depth: distance,
                        blend: mark.blend,
                    }));
            }
            world.insert(
                face,
                WorldFace {
                    draw: DrawGroup {
                        depth: distance,
                        operations: first..schedule.operations.len(),
                    },
                    rendering,
                    receiver: encounter.unwrap(),
                },
            );
        }
        let mut owners = (0..leaves.len())
            .map(|_| Vec::<Owner>::new())
            .collect::<Vec<_>>();
        for (index, brush) in plan.brushes.iter().enumerate() {
            if brush.transparent.is_empty() {
                continue;
            }
            let Some(bounds) = brush.bounds else {
                schedule
                    .diagnostics
                    .fallback
                    .push(FallbackResource::BrushBounds { brush: index });
                continue;
            };
            let Some(leaf) = bounds_leaf(topology, bounds, &indices) else {
                schedule
                    .diagnostics
                    .fallback
                    .push(FallbackResource::BrushLeafLookup { brush: index });
                continue;
            };
            schedule.brushes.insert(index);
            let Some(leaf) = leaf else {
                continue;
            };
            let first = schedule.operations.len();
            let distance = super::owner_depth(uniforms, brush.origin, brush.local_sort_center);
            for surface in &brush.transparent {
                for mesh in &surface.meshes {
                    schedule.operations.push(mesh_draw(
                        upload,
                        mesh.item,
                        mesh.indices.clone(),
                        distance,
                    ));
                }
                schedule
                    .operations
                    .extend(surface.surface_marks.iter().map(|&mark| Draw {
                        primitive: Primitive::Overlay(mark),
                        depth: distance,
                        blend: mark.blend,
                    }));
            }
            owners[leaf].push(Owner {
                draw: DrawGroup {
                    depth: distance,
                    operations: first..schedule.operations.len(),
                },
                origin: brush.origin,
            });
        }
        // The extra owner query pays off when PVS has stood down; ordinary
        // cluster-limited queues already reject most of these placements.
        let bounds_query = (!pvs_limited).then(|| uniforms.bounds_query());
        for (owner_index, renderable) in upload.assets.renderables.iter().enumerate() {
            if renderable.map_skybox != skybox {
                continue;
            }
            let mut ranges = admitted_ranges(&renderable.ranges, &items).peekable();
            if ranges.peek().is_none() {
                continue;
            }
            let source = if skybox {
                &map.map_skybox_renderables
            } else {
                &map.renderables
            };
            let source = &source[renderable.source_index];
            let Ok(bounds) = source.bounds else {
                schedule
                    .diagnostics
                    .fallback
                    .push(FallbackResource::RenderableBounds {
                        source: renderable.source_index,
                    });
                continue;
            };
            let leaf = match &source.leaf_membership {
                MapPropLeafMembership::Compiled(leaves) => Some(nearest_visible_leaf(
                    leaves.iter().map(|leaf| u32::from(*leaf)),
                    &indices,
                )),
                MapPropLeafMembership::SpatialBounds => cached_bounds_leaf(
                    &renderable.spatial_leaves,
                    || topology.leaves_for_aabb(bounds.world),
                    &indices,
                ),
            };
            let Some(leaf) = leaf else {
                schedule
                    .diagnostics
                    .fallback
                    .push(FallbackResource::RenderableLeafLookup {
                        source: renderable.source_index,
                    });
                continue;
            };
            let emit_geometry = bounds_query.as_ref().is_none_or(|query| {
                upload
                    .assets
                    .opaque_placements
                    .static_prop_frustum_bounds(owner_index)
                    .is_none_or(|bounds| query.intersects(bounds))
            });
            let first = schedule.operations.len();
            let distance = super::owner_depth(
                uniforms,
                source.origin,
                (bounds.local.min + bounds.local.max) * 0.5,
            );
            for (range, item) in ranges {
                schedule
                    .ranges
                    .entry(item.mesh_index)
                    .or_default()
                    .push(range.indices.clone());
                if leaf.is_some() && emit_geometry {
                    schedule.operations.push(mesh_draw(
                        upload,
                        item,
                        range.indices.clone(),
                        distance,
                    ));
                }
            }
            if let Some(leaf) = leaf
                && let Some(owner) = Owner::from_operations(
                    distance,
                    source.origin,
                    first..schedule.operations.len(),
                    emit_geometry,
                )
            {
                owners[leaf].push(owner);
            }
        }
        let profile = DetailProfile::SOURCE_LDR;
        let mut detail_leaves = HashMap::<usize, LeafDetails>::new();
        for detail in details {
            let Some(&leaf) = indices.get(&u32::from(detail.leaf_index)) else {
                continue;
            };
            let first = schedule.operations.len();
            schedule.operations.push(detail.draw.clone());
            let sprite = Sprite {
                draw: DrawGroup {
                    depth: detail.draw.depth,
                    operations: first..first + 1,
                },
                distance_squared: detail.distance_squared,
            };
            let leaf = detail_leaves.entry(leaf).or_default();
            if profile.fast_sprites && detail.fast_eligible {
                leaf.fast.push(sprite);
            } else {
                leaf.ordinary.push(sprite);
            }
        }
        for details in detail_leaves.values_mut() {
            super::order::sprites(&mut details.fast, |sprite| sprite.distance_squared);
            super::order::sprites(&mut details.ordinary, |sprite| sprite.distance_squared);
        }
        let eye = Vec3::new(
            uniforms.camera_position[0],
            uniforms.camera_position[1],
            uniforms.camera_position[2],
        );
        let mut pending_details = Vec::new();
        for (index, leaf) in leaves.iter().enumerate().rev() {
            let has_world_surfaces = leaf.faces.iter().any(|face| {
                world
                    .get(face)
                    .is_some_and(|face| groups.allows(face.rendering.material_group))
            });
            if has_world_surfaces {
                schedule.flush_details(&mut detail_leaves, &mut pending_details, profile);
            }
            // With no admitted surface, both face scans for every material group
            // are empty. Keep owner/detail processing below independent of this.
            let material_groups = if has_world_surfaces { 0..4 } else { 0..0 };
            for material_group in material_groups {
                let selected = |face: &u32| {
                    world.get(face).is_some_and(|face| {
                        face.rendering.material_group.index() == material_group
                            && groups.allows(face.rendering.material_group)
                    })
                };
                for face in leaf.faces.iter().rev().filter(|face| selected(face)) {
                    let face = &world[face];
                    if face.rendering.displacement.is_none() {
                        schedule.groups.push(DrawGroup {
                            depth: face.draw.depth,
                            operations: face.draw.operations.clone(),
                        });
                    }
                }
                let displacement_faces = leaf
                    .faces
                    .iter()
                    .copied()
                    .filter(|face| selected(face) && world[face].rendering.displacement.is_some())
                    .collect::<Vec<_>>();
                let mut geometry = displacement_faces.clone();
                geometry.sort_by_key(|face| {
                    let batch = map.displacement_batches[face];
                    (batch.material_bucket, batch.mesh_batch)
                });
                for face in geometry {
                    let face = &world[&face];
                    schedule.groups.push(DrawGroup {
                        depth: face.draw.depth,
                        operations: face.draw.operations.clone(),
                    });
                }
                let batch_marks = displacement_faces
                    .iter()
                    .flat_map(|face| marks.get(face).into_iter().flatten().map(|&&mark| mark))
                    .collect::<Vec<_>>();
                if !batch_marks.is_empty() {
                    let receivers = displacement_faces
                        .iter()
                        .map(|face| world[face].receiver)
                        .collect::<Vec<_>>();
                    let first = schedule.operations.len();
                    let distance = world[&displacement_faces[0]].draw.depth;
                    schedule.operations.extend(
                        order_batched_marks(
                            upload,
                            &batch_marks,
                            &receivers,
                            ReceiverQueueOrder::Encounters,
                        )
                        .into_iter()
                        .map(|mark| Draw {
                            primitive: Primitive::Overlay(mark),
                            depth: distance,
                            blend: mark.blend,
                        }),
                    );
                    schedule.groups.push(DrawGroup {
                        depth: distance,
                        operations: first..schedule.operations.len(),
                    });
                }
            }
            if detail_leaves.contains_key(&index) {
                pending_details.push(index);
            }
            if !owners[index].is_empty() {
                if pending_details.last() == Some(&index) {
                    pending_details.pop();
                }
                schedule.flush_details(&mut detail_leaves, &mut pending_details, profile);
                super::order::owners(&mut owners[index], |owner| owner.draw.depth);
                for owner in owners[index].drain(..) {
                    if let Some(details) = detail_leaves.get_mut(&index) {
                        details.drain_before(
                            &mut schedule.groups,
                            super::details::radial_distance_squared(owner.origin - eye, false),
                            profile,
                        );
                    }
                    schedule.groups.push(owner.draw);
                }
                if let Some(details) = detail_leaves.get_mut(&index) {
                    details.drain_before(&mut schedule.groups, 0.0, profile);
                }
            }
        }
        schedule.flush_details(&mut detail_leaves, &mut pending_details, profile);
        Ok(schedule)
    }

    fn flush_details(
        &mut self,
        leaves: &mut HashMap<usize, LeafDetails>,
        pending: &mut Vec<usize>,
        profile: DetailProfile,
    ) {
        for leaf in pending.iter() {
            if let Some(details) = leaves.get_mut(leaf) {
                details.drain_fast(&mut self.groups, 0.0, profile);
            }
        }
        for leaf in pending.drain(..) {
            if let Some(details) = leaves.get_mut(&leaf) {
                details.drain_ordinary(&mut self.groups, 0.0);
            }
        }
    }

    fn admit_world_face(
        &mut self,
        face: u32,
        meshes: impl IntoIterator<Item = usize>,
        rendering: Option<MapWorldSurfaceRendering>,
        has_displacement_batch: bool,
    ) -> Option<MapWorldSurfaceRendering> {
        self.world_meshes.extend(meshes);
        match validate_rendering(face, rendering, has_displacement_batch) {
            Ok(rendering) => Some(rendering),
            Err(issue) => {
                self.diagnostics
                    .rejected
                    .push(RejectedResource::WorldSurface(issue));
                None
            }
        }
    }

    pub(super) fn coverage(&self, mesh: usize) -> MeshCoverage<'_> {
        if self.world_meshes.contains(&mesh) {
            MeshCoverage::World
        } else {
            MeshCoverage::Ranges(self.ranges.get(&mesh).map_or(&[][..], Vec::as_slice))
        }
    }

    #[cfg(test)]
    fn covers(&self, mesh: usize, indices: Option<&Range<u32>>) -> bool {
        self.coverage(mesh).covers(indices)
    }

    pub(super) fn covers_brush(&self, index: usize) -> bool {
        self.brushes.contains(&index)
    }

    pub(super) fn compose(self, remaining: Vec<Draw>) -> Vec<Draw> {
        let mut ordered = Vec::with_capacity(self.operations.len() + remaining.len());
        let mut remaining = remaining.into_iter().peekable();
        for group in self.groups {
            while remaining
                .peek()
                .is_some_and(|draw| draw.depth > group.depth)
            {
                ordered.push(remaining.next().unwrap());
            }
            super::append_group(&mut ordered, &self.operations, &group);
        }
        ordered.extend(remaining);
        ordered
    }
}

impl LeafDetails {
    fn drain_before(
        &mut self,
        groups: &mut Vec<DrawGroup>,
        threshold: f32,
        profile: DetailProfile,
    ) {
        self.drain_fast(groups, threshold, profile);
        self.drain_ordinary(groups, threshold);
    }

    fn drain_fast(&mut self, groups: &mut Vec<DrawGroup>, threshold: f32, profile: DetailProfile) {
        while self
            .fast
            .get(self.fast_cursor)
            .is_some_and(|sprite| sprite.distance_squared >= threshold)
        {
            let end = (self.fast_cursor + profile.fast_chunk_quads).min(self.fast.len());
            for sprite in &self.fast[self.fast_cursor..end] {
                groups.push(DrawGroup {
                    depth: sprite.draw.depth,
                    operations: sprite.draw.operations.clone(),
                });
            }
            self.fast_cursor = end;
        }
    }

    fn drain_ordinary(&mut self, groups: &mut Vec<DrawGroup>, threshold: f32) {
        while let Some(sprite) = self.ordinary.get(self.ordinary_cursor)
            && sprite.distance_squared >= threshold
        {
            groups.push(DrawGroup {
                depth: sprite.draw.depth,
                operations: sprite.draw.operations.clone(),
            });
            self.ordinary_cursor += 1;
        }
    }
}

fn bounds_leaf(
    topology: &gmpublished_domain::scene::map::MapVisibility,
    bounds: MapBounds,
    visible: &HashMap<u32, usize>,
) -> Option<Option<usize>> {
    bounds_leaf_query(topology.leaves_for_aabb(bounds), visible)
}

fn admitted_ranges<'a>(
    ranges: &'a [super::super::upload::UploadedRenderableRange],
    items: &'a MeshItems<'_>,
) -> impl Iterator<Item = (&'a super::super::upload::UploadedRenderableRange, DrawItem)> {
    ranges
        .iter()
        .filter_map(|range| items.get(range.mesh_index).map(|item| (range, item)))
}

fn cached_bounds_leaf(
    cache: &OnceLock<Option<Vec<u32>>>,
    query: impl FnOnce() -> Option<Vec<u32>>,
    visible: &HashMap<u32, usize>,
) -> Option<Option<usize>> {
    Some(nearest_visible_leaf(
        cache.get_or_init(query).as_deref()?.iter().copied(),
        visible,
    ))
}

fn bounds_leaf_query(
    leaves: Option<Vec<u32>>,
    visible: &HashMap<u32, usize>,
) -> Option<Option<usize>> {
    Some(nearest_visible_leaf(leaves?, visible))
}

fn nearest_visible_leaf(
    leaves: impl IntoIterator<Item = u32>,
    visible: &HashMap<u32, usize>,
) -> Option<usize> {
    leaves
        .into_iter()
        .filter_map(|leaf| visible.get(&leaf).copied())
        .min()
}

fn pruned_leaves<'a>(
    leaves: &'a MapWorldSurfaceGroups,
    side: Option<super::WaterSide>,
    underwater: impl Fn(u32) -> Option<bool>,
    rejected: &mut Vec<RejectedResource>,
) -> Vec<MapWorldSurfaceGroupRef<'a>> {
    let mut selected = Vec::with_capacity(leaves.len());
    for leaf in leaves.iter() {
        if let Some(side) = side {
            let Some(submerged) = underwater(leaf.leaf_index) else {
                rejected.push(RejectedResource::LeafWaterMetadata {
                    leaf: leaf.leaf_index,
                });
                continue;
            };
            if submerged != (side == super::WaterSide::Under) {
                continue;
            }
        }
        selected.push(leaf);
    }
    selected
}

fn mesh_draw(upload: &ViewerScene, item: DrawItem, indices: Range<u32>, depth: f32) -> Draw {
    Draw {
        primitive: Primitive::Mesh(item, Some(indices)),
        depth,
        blend: upload.assets.material_render_modes[item.material_slot],
    }
}

#[cfg(test)]
mod tests {
    use gmpublished_domain::scene::map::MapWorldSurfaceGroup;
    #[test]
    fn absent_receiver_preparation_and_empty_visible_world_are_distinct() {
        assert_eq!(
            prepared_leaves(None, false),
            Err(ScheduleUnavailable::MissingReceiverPreparation)
        );
        assert_eq!(
            prepared_leaves(None, true),
            Err(ScheduleUnavailable::UnsupportedTraversalContext)
        );
        let empty = MapWorldSurfaceGroups::default();
        assert_eq!(prepared_leaves(Some(&empty), false), Ok(&empty));
        assert_eq!(prepared_leaves(Some(&empty), true), Ok(&empty));
    }

    use super::*;
    use gmpublished_domain::scene::map::{MapDisplacementRendering, MapWorldMaterialGroup};

    #[test]
    fn rejected_displacement_is_claimed_without_disabling_other_world_faces() {
        let mut schedule = Schedule::default();
        let planar = MapWorldSurfaceRendering {
            material_group: MapWorldMaterialGroup::AboveWater,
            displacement: None,
        };
        let displacement = MapWorldSurfaceRendering {
            displacement: Some(MapDisplacementRendering {
                ordinal: 0,
                full_grid_vertices: 25,
                full_grid_indices: 96,
            }),
            ..planar
        };
        assert!(
            schedule
                .admit_world_face(10, [0], Some(displacement), false)
                .is_none()
        );
        assert_eq!(
            schedule.admit_world_face(11, [1], Some(planar), false),
            Some(planar)
        );
        assert!(schedule.covers(0, Some(&(0..3))));
        assert!(schedule.covers(1, None));
        assert!(!schedule.covers(2, None));
        assert_eq!(
            schedule.diagnostics.rejected,
            [RejectedResource::WorldSurface(
                WorldPreparationIssue::MissingDisplacementBatch { face: 10 }
            )]
        );
        assert!(schedule.diagnostics.unavailable.is_none());
    }

    #[test]
    fn water_side_omission_does_not_release_world_meshes_to_fallback() {
        let mut schedule = Schedule::default();
        let rendering = MapWorldSurfaceRendering {
            material_group: MapWorldMaterialGroup::AboveWater,
            displacement: None,
        };
        let leaves: MapWorldSurfaceGroups = [
            MapWorldSurfaceGroup {
                leaf_index: 1,
                faces: vec![10],
            },
            MapWorldSurfaceGroup {
                leaf_index: 2,
                faces: vec![11],
            },
        ]
        .into_iter()
        .collect();
        for (face, mesh) in [(10, 0), (11, 1)] {
            assert_eq!(
                schedule.admit_world_face(face, [mesh], Some(rendering), false),
                Some(rendering)
            );
        }
        let selected = pruned_leaves(
            &leaves,
            Some(super::super::WaterSide::Above),
            |leaf| Some(leaf == 2),
            &mut schedule.diagnostics.rejected,
        );
        assert_eq!(
            selected
                .iter()
                .flat_map(|leaf| leaf.faces.iter().copied())
                .collect::<Vec<_>>(),
            [10]
        );
        assert!(schedule.covers(0, None));
        assert!(schedule.covers(1, None));
        assert!(schedule.operations.is_empty());
        assert!(schedule.diagnostics.rejected.is_empty());
    }

    #[test]
    fn bad_optical_leaf_is_omitted_without_reassigning_unaffected_faces() {
        let leaves: MapWorldSurfaceGroups = [
            MapWorldSurfaceGroup {
                leaf_index: 1,
                faces: vec![10],
            },
            MapWorldSurfaceGroup {
                leaf_index: 2,
                faces: vec![11],
            },
            MapWorldSurfaceGroup {
                leaf_index: 3,
                faces: vec![12],
            },
        ]
        .into_iter()
        .collect();
        let mut rejected = Vec::new();
        let selected = pruned_leaves(
            &leaves,
            Some(super::super::WaterSide::Under),
            |leaf| (leaf != 2).then_some(true),
            &mut rejected,
        );
        assert_eq!(
            selected
                .iter()
                .flat_map(|leaf| leaf.faces)
                .copied()
                .collect::<Vec<_>>(),
            [10, 12]
        );
        assert!(std::ptr::eq(
            selected[0].faces,
            leaves.get(0).unwrap().faces
        ));
        assert!(std::ptr::eq(
            selected[1].faces,
            leaves.get(2).unwrap().faces
        ));
        assert_eq!(rejected, [RejectedResource::LeafWaterMetadata { leaf: 2 }]);
        let visible = HashMap::from([(1, 0), (3, 1)]);
        assert_eq!(nearest_visible_leaf([2], &visible), None);
        assert_eq!(nearest_visible_leaf([3, 2], &visible), Some(1));
    }

    #[test]
    fn unsupported_bounds_lookup_and_known_invisible_owner_remain_distinct() {
        let visible = HashMap::from([(2, 0), (7, 1)]);
        assert_eq!(bounds_leaf_query(None, &visible), None);
        assert_eq!(bounds_leaf_query(Some(vec![]), &visible), Some(None));
        assert_eq!(bounds_leaf_query(Some(vec![3, 4]), &visible), Some(None));
        assert_eq!(bounds_leaf_query(Some(vec![7, 2]), &visible), Some(Some(0)));
    }

    #[test]
    fn admitted_placement_ranges_preserve_order_and_last_admission_against_hash_map() {
        use super::super::super::upload::UploadedRenderableRange;
        let ranges = [
            UploadedRenderableRange {
                mesh_index: 9,
                indices: 0..3,
            },
            UploadedRenderableRange {
                mesh_index: 4,
                indices: 3..6,
            },
            UploadedRenderableRange {
                mesh_index: 9,
                indices: 6..9,
            },
            UploadedRenderableRange {
                mesh_index: 2,
                indices: 0..3,
            },
            UploadedRenderableRange {
                mesh_index: 10,
                indices: 12..15,
            },
            UploadedRenderableRange {
                mesh_index: usize::MAX,
                indices: 15..18,
            },
        ];
        let actual = {
            let admissions = [
                DrawItem {
                    mesh_index: 2,
                    material_slot: 20,
                },
                DrawItem {
                    mesh_index: 9,
                    material_slot: 90,
                },
                DrawItem {
                    mesh_index: 9,
                    material_slot: 99,
                },
            ];
            let oracle: HashMap<_, _> = admissions
                .iter()
                .map(|&item| (item.mesh_index, item))
                .collect();
            let expected = ranges
                .iter()
                .filter_map(|range| {
                    oracle
                        .get(&range.mesh_index)
                        .map(|&item| (range.mesh_index, range.indices.clone(), item))
                })
                .collect::<Vec<_>>();
            let dense = MeshItems::dense(10, admissions.iter());
            let standard = MeshItems::standard(admissions.iter());
            for selected in [&dense, &standard] {
                assert_eq!(selected.get(9).unwrap().material_slot, 99);
                for missing in [4, 10, usize::MAX] {
                    assert_eq!(selected.get(missing), None);
                }
                assert_eq!(
                    admitted_ranges(&ranges, selected)
                        .map(|(range, item)| (range.mesh_index, range.indices.clone(), item))
                        .collect::<Vec<_>>(),
                    expected
                );
            }
            admitted_ranges(&ranges, &dense)
                .map(|(range, item)| (range.indices.clone(), item))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            actual,
            [
                (
                    0..3,
                    DrawItem {
                        mesh_index: 9,
                        material_slot: 99
                    }
                ),
                (
                    6..9,
                    DrawItem {
                        mesh_index: 9,
                        material_slot: 99
                    }
                ),
                (
                    0..3,
                    DrawItem {
                        mesh_index: 2,
                        material_slot: 20
                    }
                ),
            ]
        );
        let empty = MeshItems::dense(3848, std::iter::empty());
        assert!(matches!(&empty, MeshItems::Dense(storage) if storage.capacity() == 0));
        assert!(admitted_ranges(&ranges, &empty).next().is_none());
        let standard_empty = MeshItems::standard(std::iter::empty());
        assert!(admitted_ranges(&ranges, &standard_empty).next().is_none());
    }

    #[test]
    fn immutable_spatial_membership_reuses_lookup_but_selects_each_views_nearest_leaf() {
        let cache = OnceLock::new();
        let queries = std::cell::Cell::new(0);
        let query = || {
            queries.set(queries.get() + 1);
            Some(vec![7, 2, 9])
        };
        assert_eq!(
            cached_bounds_leaf(&cache, query, &HashMap::from([(7, 1), (2, 0)])),
            Some(Some(0))
        );
        assert_eq!(
            cached_bounds_leaf(&cache, query, &HashMap::from([(9, 0), (7, 2)])),
            Some(Some(0))
        );
        assert_eq!(
            cached_bounds_leaf(&cache, query, &HashMap::from([(5, 0)])),
            Some(None)
        );
        assert_eq!(queries.get(), 1);
    }

    #[test]
    fn cached_failed_lookup_keeps_fallback_distinct_from_known_empty_membership() {
        for (members, expected) in [(None, None), (Some(Vec::new()), Some(None))] {
            let cache = OnceLock::new();
            let visible = HashMap::from([(1, 0)]);
            assert_eq!(cached_bounds_leaf(&cache, || members, &visible), expected);
            assert_eq!(
                cached_bounds_leaf(
                    &cache,
                    || panic!("membership was already prepared"),
                    &visible
                ),
                expected
            );
        }
    }

    #[test]
    fn partial_placement_claim_keeps_unadmitted_and_straddling_triangles_in_fallback() {
        let mut schedule = Schedule::default();
        schedule.ranges.insert(3, std::iter::once(3..9).collect());
        assert!(!schedule.covers(3, None));
        assert!(!schedule.covers(3, Some(&(0..3))));
        assert!(schedule.covers(3, Some(&(3..6))));
        assert!(!schedule.covers(3, Some(&(8..11))));
        assert!(!schedule.covers(4, None));
        assert!(schedule.diagnostics.fallback.is_empty());
        assert!(schedule.diagnostics.rejected.is_empty());
    }

    fn sprite(index: usize, distance_squared: f32) -> Sprite {
        Sprite {
            draw: DrawGroup {
                depth: 0.0,
                operations: index..index + 1,
            },
            distance_squared,
        }
    }

    fn ids(groups: &[DrawGroup]) -> Vec<usize> {
        groups.iter().map(|group| group.operations.start).collect()
    }

    #[test]
    fn optical_leaf_pruning_assigns_spanning_owners_without_moving_world_faces() {
        let leaves: MapWorldSurfaceGroups = [
            MapWorldSurfaceGroup {
                leaf_index: 7,
                faces: vec![10, 11],
            },
            MapWorldSurfaceGroup {
                leaf_index: 3,
                faces: vec![12],
            },
            MapWorldSurfaceGroup {
                leaf_index: 9,
                faces: vec![13],
            },
        ]
        .into_iter()
        .collect();
        let submerged = |leaf| match leaf {
            7 | 9 => Some(false),
            3 => Some(true),
            _ => None,
        };
        for (side, expected_leaves, expected_faces, owner_leaf) in [
            (
                super::super::WaterSide::Above,
                vec![7, 9],
                vec![10, 11, 13],
                7,
            ),
            (super::super::WaterSide::Under, vec![3], vec![12], 3),
        ] {
            let selected = pruned_leaves(&leaves, Some(side), submerged, &mut Vec::new());
            assert_eq!(
                selected
                    .iter()
                    .map(|leaf| leaf.leaf_index)
                    .collect::<Vec<_>>(),
                expected_leaves
            );
            assert_eq!(
                selected
                    .iter()
                    .flat_map(|leaf| leaf.faces.iter().copied())
                    .collect::<Vec<_>>(),
                expected_faces
            );
            let indices = selected
                .iter()
                .enumerate()
                .map(|(index, leaf)| (leaf.leaf_index, index))
                .collect::<HashMap<_, _>>();
            for membership in [[7, 3, 9], [9, 3, 7]] {
                let index = nearest_visible_leaf(membership, &indices).unwrap();
                assert_eq!(selected[index].leaf_index, owner_leaf);
            }
            assert!(selected.iter().all(|leaf| {
                leaves.iter().any(|original| {
                    leaf.leaf_index == original.leaf_index
                        && std::ptr::eq(leaf.faces, original.faces)
                })
            }));
        }
        let main = pruned_leaves(&leaves, None, |_| None, &mut Vec::new());
        assert_eq!(main.len(), 3);
        let mut rejected = Vec::new();
        assert!(
            pruned_leaves(
                &leaves,
                Some(super::super::WaterSide::Above),
                |_| None,
                &mut rejected
            )
            .is_empty()
        );
        assert_eq!(rejected.len(), 3);
    }

    #[test]
    fn ordinary_threshold_checks_each_sprite_while_fast_checks_each_chunk() {
        let mut details = LeafDetails {
            fast: vec![sprite(0, 9.0), sprite(1, 1.0)],
            ordinary: vec![sprite(2, 9.0), sprite(3, 1.0)],
            ..Default::default()
        };
        let mut groups = Vec::new();
        details.drain_before(
            &mut groups,
            4.0,
            DetailProfile {
                fast_sprites: true,
                fast_chunk_quads: 2,
            },
        );
        assert_eq!(ids(&groups), [0, 1, 2]);
        details.drain_before(
            &mut groups,
            0.0,
            DetailProfile {
                fast_sprites: true,
                fast_chunk_quads: 2,
            },
        );
        assert_eq!(ids(&groups), [0, 1, 2, 3]);
    }

    #[test]
    fn fast_chunk_boundary_rechecks_the_next_chunk_and_keeps_pending_state() {
        let mut details = LeafDetails {
            fast: vec![sprite(0, 9.0), sprite(1, 4.0), sprite(2, 1.0)],
            ..Default::default()
        };
        let profile = DetailProfile {
            fast_sprites: true,
            fast_chunk_quads: 1,
        };
        let mut groups = Vec::new();
        details.drain_fast(&mut groups, 4.0, profile);
        assert_eq!(ids(&groups), [0, 1]);
        details.drain_fast(&mut groups, 0.0, profile);
        assert_eq!(ids(&groups), [0, 1, 2]);
    }

    #[test]
    fn queued_detail_leaves_flush_all_fast_epochs_before_ordinary_epochs() {
        let mut schedule = Schedule {
            operations: Vec::new(),
            groups: Vec::new(),
            world_meshes: HashSet::new(),
            ranges: HashMap::new(),
            brushes: HashSet::new(),
            diagnostics: Diagnostics::default(),
        };
        let mut leaves = HashMap::from([
            (
                7,
                LeafDetails {
                    fast: vec![sprite(0, 1.0)],
                    ordinary: vec![sprite(1, 1.0)],
                    ..Default::default()
                },
            ),
            (
                2,
                LeafDetails {
                    fast: vec![sprite(2, 1.0)],
                    ordinary: vec![sprite(3, 1.0)],
                    ..Default::default()
                },
            ),
        ]);
        let mut pending = vec![7, 2];
        schedule.flush_details(&mut leaves, &mut pending, DetailProfile::SOURCE_LDR);
        assert_eq!(ids(&schedule.groups), [0, 2, 1, 3]);
        assert!(pending.is_empty());
    }

    #[test]
    fn source_world_owner_order_survives_contradictory_center_depths() {
        let draw = |mesh_index, depth| Draw {
            primitive: Primitive::Mesh(
                DrawItem {
                    mesh_index,
                    material_slot: 0,
                },
                None,
            ),
            depth,
            blend: super::super::super::RenderMode::Translucent,
        };
        let schedule = Schedule {
            operations: vec![draw(0, 10.0), draw(1, 100.0)],
            groups: vec![
                DrawGroup {
                    depth: 10.0,
                    operations: 0..1,
                },
                DrawGroup {
                    depth: 100.0,
                    operations: 1..2,
                },
            ],
            world_meshes: HashSet::new(),
            ranges: HashMap::new(),
            brushes: HashSet::new(),
            diagnostics: Diagnostics::default(),
        };
        let ordered = schedule.compose(Vec::new());
        assert!(matches!(ordered[0].primitive, Primitive::Mesh(item, _) if item.mesh_index == 0));
        assert!(matches!(ordered[1].primitive, Primitive::Mesh(item, _) if item.mesh_index == 1));
    }

    fn mesh_draw_fixture(mesh_index: usize, depth: f32) -> Draw {
        Draw {
            primitive: Primitive::Mesh(
                DrawItem {
                    mesh_index,
                    material_slot: 0,
                },
                None,
            ),
            depth,
            blend: super::super::super::RenderMode::Translucent,
        }
    }

    fn mesh_ids(draws: &[Draw]) -> Vec<usize> {
        draws
            .iter()
            .map(|draw| match draw.primitive {
                Primitive::Mesh(item, _) => item.mesh_index,
                _ => panic!("mesh-only fixture"),
            })
            .collect()
    }

    #[test]
    fn culled_owner_retains_fallback_depth_barrier_and_all_coverage_claims() {
        let prepare = |emit_geometry| {
            let mut schedule = Schedule::default();
            schedule.operations.push(mesh_draw_fixture(0, 10.0));
            schedule.groups.push(DrawGroup {
                depth: 10.0,
                operations: 0..1,
            });
            let first = schedule.operations.len();
            if emit_geometry {
                schedule.operations.push(mesh_draw_fixture(1, 1.0));
            }
            let owner = Owner::from_operations(
                1.0,
                Vec3::new(2.0, 0.0, 0.0),
                first..schedule.operations.len(),
                emit_geometry,
            )
            .unwrap();
            assert_eq!(owner.origin, Vec3::new(2.0, 0.0, 0.0));
            assert_eq!(owner.draw.depth, 1.0);
            schedule.groups.push(owner.draw);
            let next = schedule.operations.len();
            schedule.operations.push(mesh_draw_fixture(2, 100.0));
            schedule.groups.push(DrawGroup {
                depth: 100.0,
                operations: next..next + 1,
            });
            // The owner loop must claim its admitted ranges even when it emits
            // no geometry, so remaining_queue cannot resurrect that geometry.
            schedule
                .ranges
                .insert(1, [3..6, 9..12].into_iter().collect());
            assert!(schedule.covers(1, Some(&(3..6))));
            assert!(schedule.covers(1, Some(&(9..12))));
            assert!(!schedule.covers(1, Some(&(3..12))));
            schedule
        };
        let remaining = || vec![mesh_draw_fixture(3, 5.0)];
        let original = mesh_ids(&prepare(true).compose(remaining()))
            .into_iter()
            .filter(|&mesh| mesh != 1)
            .collect::<Vec<_>>();
        let culled = mesh_ids(&prepare(false).compose(remaining()));
        assert_eq!(culled, original);
        assert_eq!(culled, [0, 3, 2]);
        let mut missing_barrier = prepare(false);
        missing_barrier.groups.remove(1);
        assert_eq!(mesh_ids(&missing_barrier.compose(remaining())), [0, 2, 3]);
        assert!(Owner::from_operations(1.0, Vec3::ZERO, 1..1, true).is_none());
    }

    #[test]
    fn culled_owner_retains_detail_drain_position_before_later_world_group() {
        let prepare = |emit_geometry| {
            let mut schedule = Schedule {
                operations: vec![
                    mesh_draw_fixture(0, 10.0),
                    mesh_draw_fixture(2, 100.0),
                    mesh_draw_fixture(3, 5.0),
                    mesh_draw_fixture(4, 0.5),
                ],
                ..Schedule::default()
            };
            schedule.groups.push(DrawGroup {
                depth: 10.0,
                operations: 0..1,
            });
            let first = schedule.operations.len();
            if emit_geometry {
                schedule.operations.push(mesh_draw_fixture(1, 1.0));
            }
            let owner = Owner::from_operations(
                1.0,
                Vec3::new(2.0, 0.0, 0.0),
                first..schedule.operations.len(),
                emit_geometry,
            )
            .unwrap();
            let mut details = LeafDetails {
                ordinary: vec![sprite(2, 9.0), sprite(3, 1.0)],
                ..Default::default()
            };
            details.drain_before(
                &mut schedule.groups,
                super::super::details::radial_distance_squared(owner.origin, false),
                DetailProfile::SOURCE_LDR,
            );
            schedule.groups.push(owner.draw);
            details.drain_before(&mut schedule.groups, 0.0, DetailProfile::SOURCE_LDR);
            schedule.groups.push(DrawGroup {
                depth: 100.0,
                operations: 1..2,
            });
            schedule
        };
        let original = mesh_ids(&prepare(true).compose(Vec::new()))
            .into_iter()
            .filter(|&mesh| mesh != 1)
            .collect::<Vec<_>>();
        let culled = mesh_ids(&prepare(false).compose(Vec::new()));
        assert_eq!(culled, original);
        assert_eq!(culled, [0, 3, 4, 2]);
    }

    #[test]
    fn borrowed_mesh_coverage_matches_original_predicate_exhaustively() {
        let endpoints = [0, 1, 2, 3, u32::MAX - 1, u32::MAX];
        let intervals: Vec<_> = endpoints
            .iter()
            .flat_map(|&start| endpoints.iter().map(move |&end| start..end))
            .collect();
        for first in &intervals {
            for second in &intervals {
                let mut schedule = Schedule::default();
                let original = vec![first.clone(), second.clone()];
                schedule.ranges.insert(2, original.clone());
                schedule.ranges.insert(3, Vec::new());
                schedule.ranges.insert(4, original.clone());
                schedule.world_meshes.extend([1, 4]);
                for mesh in [0, 1, 2, 3, 4, usize::MAX] {
                    let coverage = schedule.coverage(mesh);
                    let original_query = |query: Option<&Range<u32>>| {
                        schedule.world_meshes.contains(&mesh)
                            || query.is_some_and(|query| {
                                schedule.ranges.get(&mesh).is_some_and(|ranges| {
                                    ranges.iter().any(|range| {
                                        range.start <= query.start && range.end >= query.end
                                    })
                                })
                            })
                    };
                    assert_eq!(coverage.covers(None), original_query(None));
                    for query in &intervals {
                        assert_eq!(
                            coverage.covers(Some(query)),
                            original_query(Some(query)),
                            "mesh {mesh}, ranges {original:?}, query {query:?}"
                        );
                    }
                }
                assert_eq!(schedule.ranges[&2], original);
                assert_eq!(schedule.ranges[&4], original);
            }
        }
    }

    #[test]
    fn borrowed_mesh_coverage_retains_original_slice_and_iteration_order() {
        let mut schedule = Schedule::default();
        let ranges = vec![6..9, 0..3, 6..9, 3..6, 9..9];
        schedule.ranges.insert(7, ranges.clone());
        let MeshCoverage::Ranges(borrowed) = schedule.coverage(7) else {
            panic!("range coverage expected");
        };
        assert!(std::ptr::eq(borrowed, schedule.ranges[&7].as_slice()));
        assert_eq!(borrowed, ranges);
        let coverage = schedule.coverage(7);
        for query in [0..3, 0..9, 8..9, 9..9, Range { start: 8, end: 7 }] {
            assert_eq!(
                coverage.covers(Some(&query)),
                ranges
                    .iter()
                    .any(|range| range.start <= query.start && range.end >= query.end)
            );
        }
        assert!(!coverage.covers(Some(&(0..9))));
        assert_eq!(schedule.ranges[&7], ranges);
    }
}
