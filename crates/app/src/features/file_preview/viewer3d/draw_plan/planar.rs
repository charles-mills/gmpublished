use super::{DrawPlan, ViewerScene};
use gmpublished_domain::scene::map::{MapSurfaceOwnerId, MapWorldMaterialGroup};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

#[derive(Clone, Debug)]
pub(in super::super) struct PlanarSurfaceRange {
    pub indices: Range<u32>,
    pub material_group: MapWorldMaterialGroup,
}

pub(super) fn attach(
    plan: &mut DrawPlan,
    upload: &ViewerScene,
    visible: Option<&HashSet<(usize, u32)>>,
) {
    let Some(map) = upload.assets.map() else {
        return;
    };
    let Some(topology) = map.visibility.as_ref() else {
        return;
    };
    let meshes = plan
        .planar
        .iter()
        .map(|item| item.mesh_index)
        .collect::<HashSet<_>>();
    let mut ranges = HashMap::<usize, Vec<PlanarSurfaceRange>>::new();
    for (&face, receivers) in &upload.assets.receiver_faces {
        for receiver in receivers {
            if !meshes.contains(&receiver.mesh_index) {
                continue;
            }
            let source = &map.surface_receivers[receiver.source_index];
            if source.owner.id() != MapSurfaceOwnerId::World {
                continue;
            }
            let Some(rendering) = topology.surface_rendering(face) else {
                log::error!("planar world face {face} lacks rendering metadata");
                continue;
            };
            let selected = ranges.entry(receiver.mesh_index).or_default();
            if visible.is_some_and(|faces| !faces.contains(&(receiver.mesh_index, face))) {
                continue;
            }
            selected.push(PlanarSurfaceRange {
                indices: source.range.start..source.range.start + source.range.count,
                material_group: rendering.material_group,
            });
        }
    }
    for ranges in ranges.values_mut() {
        ranges.sort_by_key(|range| range.indices.start);
    }
    plan.planar_world_ranges = ranges;
}
