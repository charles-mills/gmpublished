use super::{DrawPlan, ReceiverVisibility, ViewerScene};
use gmpublished_domain::scene::map::{MapSurfaceOwnerId, MapWorldMaterialGroup};
use std::collections::HashMap;
use std::ops::Range;

#[derive(Clone, Debug)]
pub(in super::super) struct PlanarSurfaceRange {
    pub indices: Range<u32>,
    pub material_group: MapWorldMaterialGroup,
}

pub(super) fn attach(
    plan: &mut DrawPlan,
    upload: &ViewerScene,
    visible: Option<&ReceiverVisibility>,
) {
    let Some(map) = upload.assets.map() else {
        return;
    };
    let Some(topology) = map.visibility.as_ref() else {
        return;
    };
    let index = &upload.assets.receiver_faces;
    let mut meshes = vec![false; upload.meshes.len()];
    let mut ranges = HashMap::<usize, Vec<PlanarSurfaceRange>>::new();
    for item in &plan.planar {
        if index.world_mesh_partitions()[item.mesh_index].is_some() {
            meshes[item.mesh_index] = true;
            ranges.insert(item.mesh_index, Vec::new());
        }
    }
    for &(mesh, issue) in index.issues() {
        if meshes[mesh] {
            log::error!("planar world surface preparation: {issue:?}");
        }
    }
    for source_index in index.selected_sources(visible) {
        let Some(receiver) = index.for_source(source_index) else {
            continue;
        };
        if !meshes[receiver.mesh_index] {
            continue;
        }
        let source = &map.surface_receivers[source_index];
        if source.owner.id() != MapSurfaceOwnerId::World {
            continue;
        }
        let Some(rendering) = topology.surface_rendering(source.range.face) else {
            continue;
        };
        ranges
            .get_mut(&receiver.mesh_index)
            .unwrap()
            .push(PlanarSurfaceRange {
                indices: source.range.start..source.range.start + source.range.count,
                material_group: rendering.material_group,
            });
    }
    for ranges in ranges.values_mut() {
        ranges.sort_by_key(|range| range.indices.start);
    }
    plan.planar_world_ranges = ranges;
}
