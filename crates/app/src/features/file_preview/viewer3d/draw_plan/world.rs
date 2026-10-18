use super::{
    DrawPlan, IndexedDrawItem, PartitionFilter, ReceiverPlanSource, ReceiverQueueOrder,
    ReceiverVisibility, SurfaceBatch, ViewerScene, order_batched_marks,
};
use crate::media::preview_model::MapDisplacementBatch;
use std::sync::Arc;
mod cache;
pub(in super::super) use cache::Cache;
use gmpublished_domain::scene::map::{
    MapSurfaceOwnerId, MapWorldMaterialGroup, MapWorldSurfaceRendering,
};

#[derive(Clone, Debug)]
pub(in super::super) struct WorldBatch {
    pub(in super::super) material_group: MapWorldMaterialGroup,
    pub(in super::super) surfaces: SurfaceBatch,
}

struct WorldSurface {
    receiver: ReceiverPlanSource,
    encounter_rank: usize,
    rendering: MapWorldSurfaceRendering,
    displacement_batch: Option<MapDisplacementBatch>,
    draw: IndexedDrawItem,
}

struct Epoch {
    material_group: MapWorldMaterialGroup,
    displacement: bool,
    surfaces: Vec<usize>,
    receivers: Vec<ReceiverPlanSource>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum WorldPreparationIssue {
    MissingSurfaceRendering { face: u32 },
    MissingDisplacementBatch { face: u32 },
}

pub(in super::super) fn validate_rendering(
    face: u32,
    rendering: Option<MapWorldSurfaceRendering>,
    has_displacement_batch: bool,
) -> Result<MapWorldSurfaceRendering, WorldPreparationIssue> {
    let rendering = rendering.ok_or(WorldPreparationIssue::MissingSurfaceRendering { face })?;
    if rendering.displacement.is_some() && !has_displacement_batch {
        return Err(WorldPreparationIssue::MissingDisplacementBatch { face });
    }
    Ok(rendering)
}

#[derive(Clone, Debug, Default)]
pub(in super::super) struct WorldOpaquePlan {
    pub(in super::super) batches: Vec<WorldBatch>,
    pub(in super::super) issues: Vec<WorldPreparationIssue>,
    claims: Claims,
}

#[derive(Clone, Debug, Default)]
struct Claims {
    meshes: Vec<bool>,
    marks: Vec<bool>,
}

fn apply(output: Arc<WorldOpaquePlan>, plan: &mut DrawPlan) {
    plan.opaque.retain(|item| {
        !output
            .claims
            .meshes
            .get(item.mesh_index)
            .copied()
            .unwrap_or(false)
    });
    plan.surface_marks.retain(|mark| {
        !output
            .claims
            .marks
            .get(mark.overlay_index)
            .copied()
            .unwrap_or(false)
    });
    plan.world_opaque = output;
}

pub(super) fn attach(
    plan: &mut DrawPlan,
    upload: &ViewerScene,
    visible: Option<&ReceiverVisibility>,
    partition: PartitionFilter,
    cache: &mut Cache,
    world_only: bool,
) {
    cache.attach(plan, upload, visible, partition, world_only);
}

fn build(upload: &ViewerScene, input: &cache::Input) -> WorldOpaquePlan {
    let map = upload.assets.map().expect("prepared world input has map");
    let topology = map
        .visibility
        .as_ref()
        .expect("prepared world input has topology");
    let index = &upload.assets.receiver_faces;
    let mut items = vec![None; upload.meshes.len()];
    for &item in &input.meshes {
        items[item.mesh_index] = Some(item);
    }
    let world_meshes = &input.mesh_claims;
    let mut output = WorldOpaquePlan {
        claims: Claims {
            meshes: world_meshes.clone(),
            marks: vec![false; upload.overlays.len()],
        },
        ..WorldOpaquePlan::default()
    };
    for mark in &input.marks {
        output.claims.marks[mark.overlay_index] = true;
    }
    for &(mesh, issue) in index.issues() {
        if world_meshes[mesh] {
            output.issues.push(issue);
        }
    }
    let ranks = input.encounter_ranks(index.source_count());
    let mut surfaces = Vec::new();
    for selected in &input.sources {
        let source_index = selected.source;
        let source = &map.surface_receivers[source_index];
        let receiver = index
            .for_source(source_index)
            .expect("selected world receiver");
        let item = items[receiver.mesh_index].unwrap();
        let displacement_batch = map.displacement_batches.get(&source.range.face).copied();
        let Ok(rendering) = validate_rendering(
            source.range.face,
            topology.surface_rendering(source.range.face),
            displacement_batch.is_some(),
        ) else {
            continue;
        };
        surfaces.push(WorldSurface {
            receiver: ReceiverPlanSource {
                receiver_mesh_index: Some(item.mesh_index),
                face: source.range.face,
                material_group: receiver.material_group,
                lightmap_page: receiver.lightmap_page,
            },
            encounter_rank: ranks[selected.first_source],
            rendering,
            displacement_batch,
            draw: IndexedDrawItem {
                item,
                indices: source.range.start..source.range.start + source.range.count,
            },
        });
    }
    let mut batches = Vec::new();
    for epoch in epochs(&surfaces) {
        let marks = if input.marks.is_empty() {
            Vec::new()
        } else {
            input
                .marks
                .iter()
                .copied()
                .filter(|mark| {
                    upload.overlays[mark.overlay_index].owner.id() == MapSurfaceOwnerId::World
                        && mark
                            .receiver_mesh_index
                            .is_some_and(|mesh| world_meshes.get(mesh).copied().unwrap_or(false))
                        && validate_rendering(
                            mark.receiver_range.face,
                            topology.surface_rendering(mark.receiver_range.face),
                            map.displacement_batches
                                .contains_key(&mark.receiver_range.face),
                        )
                        .is_ok_and(|rendering| {
                            rendering.material_group == epoch.material_group
                                && rendering.displacement.is_some() == epoch.displacement
                        })
                })
                .collect::<Vec<_>>()
        };
        let mut batch = SurfaceBatch::default();
        for index in epoch.surfaces {
            batch.push_mesh(surfaces[index].draw.clone());
        }
        batch.surface_marks = order_batched_marks(
            upload,
            &marks,
            &epoch.receivers,
            ReceiverQueueOrder::MaterialChains,
        );
        batches.push(WorldBatch {
            material_group: epoch.material_group,
            surfaces: batch,
        });
    }
    output.batches = batches;
    output
}

fn epochs(surfaces: &[WorldSurface]) -> Vec<Epoch> {
    let mut buckets: [[Vec<usize>; 2]; 4] =
        std::array::from_fn(|_| std::array::from_fn(|_| Vec::new()));
    for (index, surface) in surfaces.iter().enumerate() {
        buckets[surface.rendering.material_group.index()]
            [usize::from(surface.rendering.displacement.is_none())]
        .push(index);
    }
    let mut epochs = Vec::new();
    for material_group in [
        MapWorldMaterialGroup::Water,
        MapWorldMaterialGroup::IntersectsWater,
        MapWorldMaterialGroup::Underwater,
        MapWorldMaterialGroup::AboveWater,
    ] {
        for (mut indices, displacement) in std::mem::take(&mut buckets[material_group.index()])
            .into_iter()
            .zip([true, false])
        {
            if indices.is_empty() {
                continue;
            }
            if !indices.is_sorted_by_key(|&index| surfaces[index].encounter_rank) {
                indices.sort_by_key(|&index| surfaces[index].encounter_rank);
            }
            if displacement {
                let mut groups = std::collections::HashMap::new();
                let mut buckets = Vec::<Vec<usize>>::new();
                for index in indices {
                    let receiver = surfaces[index].receiver;
                    let bucket = *groups
                        .entry((receiver.material_group, receiver.lightmap_page))
                        .or_insert_with(|| {
                            buckets.push(Vec::new());
                            buckets.len() - 1
                        });
                    buckets[bucket].push(index);
                }
                indices = buckets.into_iter().flatten().collect();
            }
            let receivers = indices
                .iter()
                .map(|&index| surfaces[index].receiver)
                .collect();
            if displacement {
                indices.sort_by_key(|&index| {
                    let batch = surfaces[index]
                        .displacement_batch
                        .expect("validated displacement batch");
                    (batch.material_bucket, batch.mesh_batch)
                });
            } else {
                indices.sort_by_key(|&index| {
                    let draw = &surfaces[index].draw;
                    (draw.item.mesh_index, draw.indices.start)
                });
            }
            epochs.push(Epoch {
                material_group,
                displacement,
                surfaces: indices,
                receivers,
            });
        }
    }
    epochs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::file_preview::viewer3d::draw_plan::DrawItem;
    use gmpublished_domain::scene::map::{MapDisplacementRendering, SourceLightmapPage};

    fn surface(
        face: u32,
        group: MapWorldMaterialGroup,
        material: usize,
        page: u32,
        batch: Option<(usize, usize)>,
    ) -> WorldSurface {
        WorldSurface {
            receiver: ReceiverPlanSource {
                receiver_mesh_index: Some(face as usize),
                face,
                material_group: material,
                lightmap_page: SourceLightmapPage::Physical(page),
            },
            encounter_rank: face as usize,
            rendering: MapWorldSurfaceRendering {
                material_group: group,
                displacement: batch.map(|_| MapDisplacementRendering {
                    ordinal: face,
                    full_grid_vertices: 25,
                    full_grid_indices: 96,
                }),
            },
            displacement_batch: batch.map(|(material_bucket, mesh_batch)| MapDisplacementBatch {
                material_bucket,
                mesh_batch,
            }),
            draw: IndexedDrawItem {
                item: DrawItem {
                    mesh_index: face as usize,
                    material_slot: material,
                },
                indices: face * 3..face * 3 + 3,
            },
        }
    }

    #[test]
    fn invalid_geometry_still_applies_world_claims_and_retains_other_owners() {
        let mut plan = DrawPlan {
            opaque: vec![
                super::super::DrawItem {
                    mesh_index: 0,
                    material_slot: 2,
                },
                super::super::DrawItem {
                    mesh_index: 1,
                    material_slot: 2,
                },
            ],
            ..DrawPlan::default()
        };
        let mark = |overlay_index| super::super::OverlayDrawItem {
            blend: crate::bridge::materials::RenderMode::Opaque,
            receiver_mesh_index: Some(0),
            receiver_range: gmpublished_domain::scene::map::MapMeshIndexRange {
                face: 7,
                start: 0,
                count: 3,
            },
            overlay_index,
            material_slot: 2,
        };
        plan.surface_marks = vec![mark(0), mark(1)];
        let output = Arc::new(WorldOpaquePlan {
            batches: Vec::new(),
            issues: vec![WorldPreparationIssue::MissingSurfaceRendering { face: 7 }],
            claims: Claims {
                meshes: vec![true, false],
                marks: vec![true, false],
            },
        });
        apply(Arc::clone(&output), &mut plan);
        assert_eq!(
            plan.opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(
            plan.surface_marks
                .iter()
                .map(|mark| mark.overlay_index)
                .collect::<Vec<_>>(),
            [1]
        );
        assert!(Arc::ptr_eq(&output, &plan.world_opaque));
        assert_eq!(
            plan.world_opaque.issues,
            [WorldPreparationIssue::MissingSurfaceRendering { face: 7 }]
        );
    }

    #[test]
    fn missing_batch_rejects_displacement_but_not_planar_without_batch() {
        let planar = surface(1, MapWorldMaterialGroup::AboveWater, 0, 0, None).rendering;
        let displacement =
            surface(2, MapWorldMaterialGroup::AboveWater, 0, 0, Some((0, 0))).rendering;
        assert_eq!(validate_rendering(1, Some(planar), false), Ok(planar));
        assert_eq!(
            validate_rendering(2, Some(displacement), false),
            Err(WorldPreparationIssue::MissingDisplacementBatch { face: 2 })
        );
        assert_eq!(
            validate_rendering(2, Some(displacement), true),
            Ok(displacement)
        );
        assert_eq!(
            validate_rendering(3, None, true),
            Err(WorldPreparationIssue::MissingSurfaceRendering { face: 3 })
        );
    }

    #[test]
    fn opaque_world_groups_descend_and_finish_displacements_before_planar_surfaces() {
        use MapWorldMaterialGroup::*;
        let surfaces = [
            surface(0, AboveWater, 0, 0, None),
            surface(1, Water, 0, 0, None),
            surface(2, Water, 0, 0, Some((0, 0))),
            surface(3, Underwater, 0, 0, Some((0, 0))),
            surface(4, IntersectsWater, 0, 0, None),
            surface(5, AboveWater, 0, 0, Some((0, 0))),
        ];
        let phases = epochs(&surfaces);
        assert_eq!(
            phases
                .iter()
                .map(|phase| (phase.material_group, phase.receivers[0].face))
                .collect::<Vec<_>>(),
            [
                (Water, 2),
                (Water, 1),
                (IntersectsWater, 4),
                (Underwater, 3),
                (AboveWater, 5),
                (AboveWater, 0)
            ],
        );
        assert_eq!(
            phases.iter().flat_map(|phase| &phase.surfaces).count(),
            surfaces.len()
        );
    }

    #[test]
    fn opaque_displacement_enqueue_follows_first_touch_keys_while_geometry_uses_static_batches() {
        use MapWorldMaterialGroup::AboveWater;
        let surfaces = [
            surface(4, AboveWater, 1, 0, Some((0, 1))),
            surface(2, AboveWater, 0, 0, Some((2, 0))),
            surface(0, AboveWater, 0, 0, Some((2, 1))),
            surface(3, AboveWater, 2, 0, Some((1, 0))),
            surface(1, AboveWater, 1, 0, Some((0, 0))),
        ];
        let phase = epochs(&surfaces).pop().unwrap();
        assert_eq!(
            phase
                .receivers
                .iter()
                .map(|receiver| receiver.face)
                .collect::<Vec<_>>(),
            [0, 2, 1, 4, 3]
        );
        assert_eq!(
            phase
                .surfaces
                .iter()
                .map(|&index| surfaces[index].receiver.face)
                .collect::<Vec<_>>(),
            [1, 4, 3, 2, 0]
        );
    }

    #[test]
    fn opaque_displacement_live_encounters_are_independent_of_face_ordinals() {
        use MapWorldMaterialGroup::AboveWater;
        let mut surfaces = [
            surface(0, AboveWater, 0, 0, Some((0, 0))),
            surface(1, AboveWater, 1, 0, Some((1, 0))),
            surface(2, AboveWater, 0, 0, Some((0, 0))),
        ];
        surfaces[0].encounter_rank = 2;
        surfaces[1].encounter_rank = 0;
        surfaces[2].encounter_rank = 1;
        let phase = epochs(&surfaces).pop().unwrap();
        assert_eq!(
            phase
                .receivers
                .iter()
                .map(|receiver| receiver.face)
                .collect::<Vec<_>>(),
            [1, 2, 0]
        );
        assert_eq!(
            phase
                .surfaces
                .iter()
                .map(|&index| surfaces[index].receiver.face)
                .collect::<Vec<_>>(),
            [2, 0, 1]
        );
    }

    #[test]
    fn opaque_displacement_first_touch_buckets_include_the_logical_lightmap_page() {
        use MapWorldMaterialGroup::AboveWater;
        let surfaces = [
            surface(0, AboveWater, 0, 0, Some((0, 0))),
            surface(1, AboveWater, 1, 0, Some((1, 0))),
            surface(2, AboveWater, 0, 1, Some((2, 0))),
            surface(3, AboveWater, 0, 0, Some((0, 0))),
        ];
        let phase = epochs(&surfaces).pop().unwrap();
        assert_eq!(
            phase
                .receivers
                .iter()
                .map(|receiver| receiver.face)
                .collect::<Vec<_>>(),
            [0, 3, 1, 2]
        );
    }
}
