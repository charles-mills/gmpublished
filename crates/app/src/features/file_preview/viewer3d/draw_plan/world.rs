use super::{
    DrawPlan, IndexedDrawItem, PartitionFilter, ReceiverPlanSource, ReceiverQueueOrder,
    SurfaceBatch, ViewerScene, order_batched_marks,
};
use crate::media::preview_model::MapDisplacementBatch;
use gmpublished_domain::scene::map::{
    MapSurfaceOwnerId, MapWorldMaterialGroup, MapWorldSurfaceRendering,
};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub(in super::super) struct WorldBatch {
    pub(in super::super) material_group: MapWorldMaterialGroup,
    pub(in super::super) surfaces: SurfaceBatch,
}

struct WorldSurface {
    receiver: ReceiverPlanSource,
    rendering: MapWorldSurfaceRendering,
    displacement_batch: Option<MapDisplacementBatch>,
    draw: IndexedDrawItem,
}

struct Epoch {
    material_group: MapWorldMaterialGroup,
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

pub(super) fn attach(
    plan: &mut DrawPlan,
    upload: &ViewerScene,
    visible: Option<&HashSet<(usize, u32)>>,
    partition: PartitionFilter,
    encounters: &[ReceiverPlanSource],
) {
    let Some(map) = upload.assets.map() else {
        return;
    };
    let Some(topology) = map.visibility.as_ref() else {
        return;
    };
    let items = plan
        .opaque
        .iter()
        .map(|&item| (item.mesh_index, item))
        .collect::<HashMap<_, _>>();
    let mut world_meshes = HashSet::new();
    let mut surfaces = Vec::new();
    for (source_index, source) in map.surface_receivers.iter().enumerate() {
        if source.owner.id() != MapSurfaceOwnerId::World {
            continue;
        }
        let Some(receiver) = upload
            .assets
            .receiver_faces
            .get(&source.range.face)
            .and_then(|receivers| {
                receivers
                    .iter()
                    .find(|receiver| receiver.source_index == source_index)
            })
        else {
            continue;
        };
        let Some(&item) = items.get(&receiver.mesh_index) else {
            continue;
        };
        if !partition.accepts(upload.meshes[item.mesh_index].map_skybox) {
            continue;
        }
        world_meshes.insert(item.mesh_index);
        let displacement_batch = map.displacement_batches.get(&source.range.face).copied();
        let rendering = match validate_rendering(
            source.range.face,
            topology.surface_rendering(source.range.face),
            displacement_batch.is_some(),
        ) {
            Ok(rendering) => rendering,
            Err(issue) => {
                plan.world_preparation_issues.push(issue);
                continue;
            }
        };
        if visible.is_some_and(|visible| !visible.contains(&(item.mesh_index, source.range.face))) {
            continue;
        }
        surfaces.push(WorldSurface {
            receiver: ReceiverPlanSource {
                receiver_mesh_index: Some(item.mesh_index),
                face: source.range.face,
                material_group: receiver.material_group,
                lightmap_page: receiver.lightmap_page,
            },
            rendering,
            displacement_batch,
            draw: IndexedDrawItem {
                item,
                indices: source.range.start..source.range.start + source.range.count,
            },
        });
    }
    let mut batches = Vec::new();
    for epoch in epochs(&surfaces, encounters) {
        let faces = epoch
            .receivers
            .iter()
            .map(receiver_id)
            .collect::<HashSet<_>>();
        let marks = plan
            .surface_marks
            .iter()
            .copied()
            .filter(|mark| {
                upload.overlays[mark.overlay_index].owner.id() == MapSurfaceOwnerId::World
                    && faces.contains(&(mark.receiver_mesh_index, mark.receiver_range.face))
            })
            .collect::<Vec<_>>();
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
    plan.opaque
        .retain(|item| !world_meshes.contains(&item.mesh_index));
    plan.surface_marks.retain(|mark| {
        upload.overlays[mark.overlay_index].owner.id() != MapSurfaceOwnerId::World
            || mark
                .receiver_mesh_index
                .is_none_or(|mesh| !world_meshes.contains(&mesh))
    });
    plan.world_batches = batches;
}

fn receiver_id(receiver: &ReceiverPlanSource) -> (Option<usize>, u32) {
    (receiver.receiver_mesh_index, receiver.face)
}

fn epochs(surfaces: &[WorldSurface], encounters: &[ReceiverPlanSource]) -> Vec<Epoch> {
    let mut ranks = HashMap::new();
    for (index, receiver) in encounters.iter().enumerate() {
        ranks.entry(receiver_id(receiver)).or_insert(index);
    }
    let mut epochs = Vec::new();
    for material_group in [
        MapWorldMaterialGroup::Water,
        MapWorldMaterialGroup::IntersectsWater,
        MapWorldMaterialGroup::Underwater,
        MapWorldMaterialGroup::AboveWater,
    ] {
        for displacement in [true, false] {
            let mut indices = surfaces
                .iter()
                .enumerate()
                .filter_map(|(index, surface)| {
                    (surface.rendering.material_group == material_group
                        && surface.rendering.displacement.is_some() == displacement)
                        .then_some(index)
                })
                .collect::<Vec<_>>();
            if indices.is_empty() {
                continue;
            }
            indices.sort_by_key(|&index| {
                ranks
                    .get(&receiver_id(&surfaces[index].receiver))
                    .copied()
                    .unwrap_or(usize::MAX)
            });
            if displacement {
                let mut groups = HashMap::new();
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
                    distance_squared: 0.0,
                },
                indices: face * 3..face * 3 + 3,
            },
        }
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
        let encounters = surfaces
            .iter()
            .map(|surface| surface.receiver)
            .collect::<Vec<_>>();
        let phases = epochs(&surfaces, &encounters);
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
        let mut encounters = surfaces
            .iter()
            .map(|surface| surface.receiver)
            .collect::<Vec<_>>();
        encounters.sort_by_key(|receiver| receiver.face);
        let phase = epochs(&surfaces, &encounters).pop().unwrap();
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
    fn opaque_displacement_first_touch_buckets_include_the_logical_lightmap_page() {
        use MapWorldMaterialGroup::AboveWater;
        let surfaces = [
            surface(0, AboveWater, 0, 0, Some((0, 0))),
            surface(1, AboveWater, 1, 0, Some((1, 0))),
            surface(2, AboveWater, 0, 1, Some((2, 0))),
            surface(3, AboveWater, 0, 0, Some((0, 0))),
        ];
        let encounters = surfaces
            .iter()
            .map(|surface| surface.receiver)
            .collect::<Vec<_>>();
        let phase = epochs(&surfaces, &encounters).pop().unwrap();
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
