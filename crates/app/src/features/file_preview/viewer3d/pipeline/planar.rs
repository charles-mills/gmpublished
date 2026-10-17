use super::super::draw_plan::PlanarSurfaceRange;
use super::geometry::ViewerScene;
use super::reflections::{Plane, SceneView};
use super::transparency::WorldDrawGroups;
use super::visibility::{UploadedVisibleIndices, upload_visible_indices};
use super::{ModelVertex, RenderScene, wgpu};
use std::collections::HashMap;

#[derive(Debug, Default)]
pub(super) struct Selections {
    entries: HashMap<usize, Selection>,
}

#[derive(Debug)]
struct Selection {
    indices: Vec<u32>,
    uploaded: UploadedVisibleIndices,
}

impl Selections {
    fn update(
        &mut self,
        mesh: usize,
        indices: Vec<u32>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> UploadedVisibleIndices {
        match self.entries.entry(mesh) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if entry.get().indices != indices {
                    *entry.get_mut() = Selection {
                        uploaded: upload_visible_indices(device, queue, &indices),
                        indices,
                    };
                }
                entry.get().uploaded.clone()
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let uploaded = upload_visible_indices(device, queue, &indices);
                entry
                    .insert(Selection { indices, uploaded })
                    .uploaded
                    .clone()
            }
        }
    }
}

pub(super) fn prepare(
    view: &mut SceneView,
    upload: &ViewerScene,
    scene: &RenderScene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    excluded_plane: Option<Plane>,
) {
    let Some(plans) = view.plans.as_ref() else {
        return;
    };
    for (plan, skybox, groups) in std::iter::once((plans.world.as_ref(), false, view.world_groups))
        .chain(
            plans
                .map_skybox
                .as_ref()
                .map(|plan| (plan.as_ref(), true, WorldDrawGroups::ALL)),
        )
    {
        let visibility = if skybox {
            &mut view.skybox_visibility
        } else {
            &mut view.visibility
        };
        for item in &plan.planar {
            let uploaded = &upload.meshes[item.mesh_index];
            if uploaded.door_index.is_some() {
                continue;
            }
            let mesh = if skybox {
                upload
                    .assets
                    .map()
                    .and_then(|map| map.map_skybox_meshes.get(uploaded.scene_mesh_index))
            } else {
                scene.meshes.get(uploaded.scene_mesh_index)
            };
            let Some(mesh) = mesh else {
                log::error!("planar mesh {} lacks source geometry", item.mesh_index);
                continue;
            };
            let fallback = visibility
                .plan
                .as_ref()
                .map_or(mesh.indices.as_slice(), |plan| {
                    plan.mesh_indices
                        .get(uploaded.scene_mesh_index)
                        .map_or(&[], Vec::as_slice)
                });
            let indices = select_indices(
                &mesh.vertices,
                &mesh.indices,
                plan.planar_world_ranges
                    .get(&item.mesh_index)
                    .map(Vec::as_slice),
                fallback,
                groups,
                if skybox { None } else { excluded_plane },
            )
            .unwrap_or_else(|| {
                log::error!(
                    "planar mesh {} has invalid canonical index ranges",
                    item.mesh_index
                );
                Vec::new()
            });
            let selected = view
                .planar_indices
                .update(item.mesh_index, indices, device, queue);
            visibility.mesh_indices.insert(item.mesh_index, selected);
        }
    }
}

pub(super) fn select_indices(
    vertices: &[ModelVertex],
    indices: &[u32],
    ranges: Option<&[PlanarSurfaceRange]>,
    fallback: &[u32],
    groups: WorldDrawGroups,
    excluded_plane: Option<Plane>,
) -> Option<Vec<u32>> {
    let selected = if let Some(ranges) = ranges {
        let mut selected = Vec::new();
        for range in ranges {
            if groups.allows(range.material_group) {
                selected.extend_from_slice(
                    indices.get(range.indices.start as usize..range.indices.end as usize)?,
                );
            }
        }
        selected
    } else {
        fallback.to_vec()
    };
    Some(match excluded_plane {
        Some(plane) => without_plane(vertices, &selected, plane),
        None => selected,
    })
}

pub(super) fn without_plane(vertices: &[ModelVertex], indices: &[u32], plane: Plane) -> Vec<u32> {
    indices
        .chunks_exact(3)
        .filter(|triangle| {
            Plane::from_triangle(vertices, triangle).is_none_or(|candidate| {
                !candidate.same(plane)
                    && !candidate.same(Plane {
                        normal: -plane.normal,
                        distance: -plane.distance,
                    })
            })
        })
        .flatten()
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::transparency::WaterSide;
    use super::*;
    use bytemuck::Zeroable;
    use gmpublished_domain::scene::map::MapWorldMaterialGroup::*;

    fn geometry() -> (Vec<ModelVertex>, Vec<u32>) {
        let vertices = [0.0, 16.0, 32.0]
            .into_iter()
            .flat_map(|z| {
                [[0.0, 0.0, z], [8.0, 0.0, z], [0.0, 8.0, z]].map(|position| ModelVertex {
                    position: position.into(),
                    ..ModelVertex::zeroed()
                })
            })
            .collect();
        (vertices, (0..9).collect())
    }

    #[test]
    fn mixed_raw_water_groups_filter_by_view_without_discarding_other_planar_faces() {
        let (vertices, indices) = geometry();
        let ranges = [
            PlanarSurfaceRange {
                indices: 0..3,
                material_group: Water,
            },
            PlanarSurfaceRange {
                indices: 3..6,
                material_group: AboveWater,
            },
            PlanarSurfaceRange {
                indices: 6..9,
                material_group: Underwater,
            },
        ];
        for (side, expected) in [
            (WaterSide::Above, vec![3, 4, 5]),
            (WaterSide::Under, vec![6, 7, 8]),
        ] {
            let groups = WorldDrawGroups {
                water_side: Some(side),
                include_water: false,
            };
            assert_eq!(
                select_indices(&vertices, &indices, Some(&ranges), &[], groups, None),
                Some(expected)
            );
        }
        assert_eq!(
            select_indices(
                &vertices,
                &indices,
                Some(&ranges),
                &[],
                WorldDrawGroups::ALL,
                None
            ),
            Some(indices.clone())
        );
        assert_eq!(
            select_indices(
                &vertices,
                &indices,
                None,
                &[3, 4, 5],
                WorldDrawGroups {
                    water_side: Some(WaterSide::Under),
                    include_water: false
                },
                None
            ),
            Some(vec![3, 4, 5])
        );
    }

    #[test]
    fn canonical_visibility_and_own_plane_filter_compose_without_coarse_pvs_reintroduction() {
        let (vertices, indices) = geometry();
        let ranges = [
            PlanarSurfaceRange {
                indices: 0..3,
                material_group: AboveWater,
            },
            PlanarSurfaceRange {
                indices: 6..9,
                material_group: AboveWater,
            },
        ];
        let selected = select_indices(
            &vertices,
            &indices,
            Some(&ranges),
            &indices,
            WorldDrawGroups::ALL,
            Some(Plane {
                normal: [0.0, 0.0, 1.0].into(),
                distance: 0.0,
            }),
        );
        assert_eq!(selected, Some(vec![6, 7, 8]));
        assert_eq!(
            select_indices(
                &vertices,
                &indices,
                Some(&[]),
                &indices,
                WorldDrawGroups::ALL,
                None
            ),
            Some(vec![])
        );
        let invalid = [PlanarSurfaceRange {
            indices: 6..12,
            material_group: AboveWater,
        }];
        assert!(
            select_indices(
                &vertices,
                &indices,
                Some(&invalid),
                &indices,
                WorldDrawGroups::ALL,
                None
            )
            .is_none()
        );
    }
}
