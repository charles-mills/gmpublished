use super::super::super::draw_plan::{
    ReceiverPlanSource, ReceiverQueueOrder, WorldPreparationIssue, order_batched_marks,
    validate_rendering,
};
use super::super::{DrawItem, DrawPlan, Uniforms, ViewerScene};
use super::details::{DetailDraw, DetailProfile};
use super::{Draw, DrawGroup, Primitive, WorldDrawGroups, depth};
use gmpublished_domain::math::Vec3;
use gmpublished_domain::scene::map::{
    MapBounds, MapPropLeafMembership, MapSurfaceOwnerId, MapWorldSurfaceGroup,
    MapWorldSurfaceRendering,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

struct WorldFace {
    draw: DrawGroup,
    rendering: MapWorldSurfaceRendering,
    receiver: ReceiverPlanSource,
}

struct Owner {
    draw: DrawGroup,
    origin: Vec3,
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
        details: &[DetailDraw],
    ) -> Result<Self, ScheduleUnavailable> {
        let map = upload.assets.map().ok_or(ScheduleUnavailable::NotMap)?;
        let topology = map
            .visibility
            .as_ref()
            .ok_or(ScheduleUnavailable::MissingTopology)?;
        let mut diagnostics = Diagnostics::default();
        let leaves = pruned_leaves(
            plan.transparency_leaves
                .as_ref()
                .ok_or(ScheduleUnavailable::UnsupportedTraversalContext)?,
            groups.water_side,
            |leaf| topology.leaf_is_underwater(leaf),
            &mut diagnostics.rejected,
        );
        let indices = leaves
            .iter()
            .enumerate()
            .map(|(index, leaf)| (leaf.leaf_index, index))
            .collect::<HashMap<_, _>>();
        let items = plan
            .translucent
            .iter()
            .chain(&plan.additive)
            .map(|&item| (item.mesh_index, item))
            .collect::<HashMap<_, _>>();
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
        for (&face, receivers) in &upload.assets.receiver_faces {
            let receivers = receivers.iter().filter(|receiver| {
                map.surface_receivers[receiver.source_index].owner.id() == MapSurfaceOwnerId::World
                    && items.contains_key(&receiver.mesh_index)
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
                let item = items[&receiver.mesh_index];
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
        for renderable in &upload.assets.renderables {
            if renderable.map_skybox != skybox {
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
                MapPropLeafMembership::SpatialBounds => {
                    bounds_leaf(topology, bounds.world, &indices)
                }
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
            let first = schedule.operations.len();
            let distance = super::owner_depth(
                uniforms,
                source.origin,
                (bounds.local.min + bounds.local.max) * 0.5,
            );
            for range in &renderable.ranges {
                let Some(&item) = items.get(&range.mesh_index) else {
                    continue;
                };
                schedule
                    .ranges
                    .entry(item.mesh_index)
                    .or_default()
                    .push(range.indices.clone());
                if leaf.is_some() {
                    schedule.operations.push(mesh_draw(
                        upload,
                        item,
                        range.indices.clone(),
                        distance,
                    ));
                }
            }
            if let Some(leaf) = leaf
                && first != schedule.operations.len()
            {
                owners[leaf].push(Owner {
                    draw: DrawGroup {
                        depth: distance,
                        operations: first..schedule.operations.len(),
                    },
                    origin: source.origin,
                });
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
            if leaf.faces.iter().any(|face| {
                world
                    .get(face)
                    .is_some_and(|face| groups.allows(face.rendering.material_group))
            }) {
                schedule.flush_details(&mut detail_leaves, &mut pending_details, profile);
            }
            for material_group in 0..4 {
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

    pub(super) fn covers(&self, mesh: usize, indices: Option<&Range<u32>>) -> bool {
        self.world_meshes.contains(&mesh)
            || indices.is_some_and(|indices| {
                self.ranges.get(&mesh).is_some_and(|ranges| {
                    ranges
                        .iter()
                        .any(|range| range.start <= indices.start && range.end >= indices.end)
                })
            })
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
            ordered.extend_from_slice(&self.operations[group.operations]);
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
    leaves: &'a [MapWorldSurfaceGroup],
    side: Option<super::WaterSide>,
    underwater: impl Fn(u32) -> Option<bool>,
    rejected: &mut Vec<RejectedResource>,
) -> Vec<&'a MapWorldSurfaceGroup> {
    let mut selected = Vec::with_capacity(leaves.len());
    for leaf in leaves {
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
    fn bad_optical_leaf_is_omitted_without_reassigning_unaffected_faces() {
        let leaves = [
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
        ];
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
                .flat_map(|leaf| &leaf.faces)
                .copied()
                .collect::<Vec<_>>(),
            [10, 12]
        );
        assert!(std::ptr::eq(selected[0], &leaves[0]));
        assert!(std::ptr::eq(selected[1], &leaves[2]));
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
        let leaves = [
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
        ];
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
            assert!(
                selected
                    .iter()
                    .all(|leaf| leaves.iter().any(|original| std::ptr::eq(*leaf, original)))
            );
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
                    distance_squared: 0.0,
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
}
