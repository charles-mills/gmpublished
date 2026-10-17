use super::{MapPreview, ModelVertex, RenderScene, SceneView, ViewerScene};
use crate::media::preview_model::{MapMeshVisibility, MeshGeometry, vertex_lighting};
use gmpublished_domain::scene::map::MapVisibilityBucket;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct References {
    meshes: Vec<MeshReferences>,
}

#[derive(Debug, Default)]
struct MeshReferences {
    all: Arc<[usize]>,
    faces: Option<Vec<Vec<RangeReferences>>>,
}

#[derive(Debug)]
struct RangeReferences {
    visibility: MapVisibilityBucket,
    contexts: Arc<[usize]>,
}

impl References {
    fn new(scene: &RenderScene, map: Option<&MapPreview>, upload: &ViewerScene) -> Self {
        Self {
            meshes: upload
                .meshes
                .iter()
                .map(|uploaded| {
                    if uploaded.geometry != MeshGeometry::Model {
                        return MeshReferences::default();
                    }
                    let meshes = if uploaded.map_skybox {
                        let Some(map) = map else {
                            return MeshReferences::default();
                        };
                        &map.map_skybox_meshes
                    } else {
                        &scene.meshes
                    };
                    let Some(mesh) = meshes.get(uploaded.scene_mesh_index) else {
                        return MeshReferences::default();
                    };
                    let visibility = (!uploaded.map_skybox && uploaded.door_index.is_none())
                        .then(|| {
                            map.and_then(|map| map.mesh_visibility.get(uploaded.scene_mesh_index))
                        })
                        .flatten();
                    MeshReferences::new(&mesh.vertices, &mesh.indices, visibility)
                })
                .collect(),
        }
    }
}

impl MeshReferences {
    fn new(
        vertices: &[ModelVertex],
        indices: &[u32],
        visibility: Option<&MapMeshVisibility>,
    ) -> Self {
        let all = contexts(vertices, indices);
        if all.is_empty() {
            return Self::default();
        }
        let faces = visibility
            .filter(|visibility| {
                !visibility.always_visible.is_empty() || !visibility.clusters.is_empty()
            })
            .map(|visibility| {
                let mut faces = Vec::<Vec<RangeReferences>>::new();
                let mut face_slots = HashMap::<u32, usize>::new();
                let mut ranges = HashMap::new();
                for (range, bucket) in visibility
                    .always_visible
                    .iter()
                    .map(|range| (range, MapVisibilityBucket::Always))
                    .chain(visibility.clusters.iter().flat_map(|cluster| {
                        cluster
                            .ranges
                            .iter()
                            .map(|range| (range, MapVisibilityBucket::Cluster(cluster.cluster)))
                    }))
                {
                    let start = range.start as usize;
                    let Some(slice) =
                        indices.get(start..start.saturating_add(range.count as usize))
                    else {
                        continue;
                    };
                    let contexts = ranges
                        .entry((range.start, range.count))
                        .or_insert_with(|| contexts(vertices, slice));
                    let slot = *face_slots.entry(range.face).or_insert_with(|| {
                        faces.push(Vec::new());
                        faces.len() - 1
                    });
                    faces[slot].push(RangeReferences {
                        visibility: bucket,
                        contexts: Arc::clone(contexts),
                    });
                }
                faces
            });
        Self { all, faces }
    }

    fn mark(&self, active: &mut [bool], clusters: Option<&[bool]>) {
        if let Some((faces, clusters)) = self.faces.as_ref().zip(clusters) {
            for alternatives in faces {
                // Visibility emits the first eligible range for each face.
                if let Some(range) = alternatives.iter().find(|range| match range.visibility {
                    MapVisibilityBucket::Always => true,
                    MapVisibilityBucket::Cluster(cluster) => {
                        clusters.get(cluster as usize).copied().unwrap_or(false)
                    }
                }) {
                    mark(active, &range.contexts);
                }
            }
        } else {
            mark(active, &self.all);
        }
    }
}

pub(super) fn gather(
    active: &mut [bool],
    scene: &RenderScene,
    map: Option<&MapPreview>,
    upload: &ViewerScene,
    view: &SceneView,
) {
    let Some(plans) = &view.plans else { return };
    let references = upload
        .assets
        .model_lighting_references
        .get_or_init(|| References::new(scene, map, upload));
    for (plan, skybox) in std::iter::once((plans.world.as_ref(), false))
        .chain(plans.map_skybox.as_ref().map(|plan| (plan.as_ref(), true)))
    {
        for item in plan
            .opaque
            .iter()
            .chain(&plan.planar)
            .chain(&plan.translucent)
            .chain(&plan.additive)
        {
            if mark_selected(active, plan, item.mesh_index) {
                continue;
            }
            let uploaded = &upload.meshes[item.mesh_index];
            let visibility = (!skybox && uploaded.door_index.is_none())
                .then_some(view.visibility.plan.as_ref())
                .flatten();
            if visibility
                .is_some_and(|plan| plan.mesh_indices.get(uploaded.scene_mesh_index).is_none())
            {
                continue;
            }
            references.meshes[item.mesh_index].mark(
                active,
                visibility.map(|plan| plan.visible_clusters.as_slice()),
            );
        }
    }
}

fn mark_selected(active: &mut [bool], plan: &super::DrawPlan, mesh_index: usize) -> bool {
    let Some(ranges) = plan.opaque_ranges.get(&mesh_index) else {
        return false;
    };
    mark(active, &ranges.contexts);
    true
}

fn contexts(vertices: &[ModelVertex], indices: &[u32]) -> Arc<[usize]> {
    indices
        .iter()
        .filter_map(|&index| vertices.get(index as usize))
        .filter(|vertex| vertex.lighting_mode & vertex_lighting::RUNTIME_CONTEXT != 0)
        .filter_map(|vertex| {
            crate::media::preview_model::ModelLightingContextId::from_gpu(vertex.lighting_context)
                .map(crate::media::preview_model::ModelLightingContextId::index)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn mark(active: &mut [bool], contexts: &[usize]) {
    for &context in contexts {
        if let Some(active) = active.get_mut(context) {
            *active = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmpublished_domain::scene::map::{MapMeshClusterRanges, MapMeshIndexRange};

    fn vertex(context: u32, runtime: bool) -> ModelVertex {
        let mut vertex: ModelVertex = bytemuck::Zeroable::zeroed();
        vertex.lighting_context = context;
        vertex.lighting_mode = if runtime {
            vertex_lighting::RUNTIME_CONTEXT
        } else {
            0
        };
        vertex
    }

    fn range(face: u32, start: u32, count: u32) -> MapMeshIndexRange {
        MapMeshIndexRange { face, start, count }
    }

    #[test]
    fn merged_mesh_visibility_and_optical_union_retain_only_referenced_runtime_contexts() {
        let vertices = [
            vertex(1, true),
            vertex(2, true),
            vertex(3, false),
            vertex(0, true),
        ];
        let visibility = MapMeshVisibility {
            always_visible: vec![range(2, 2, 2)],
            clusters: vec![
                MapMeshClusterRanges {
                    cluster: 0,
                    ranges: vec![range(0, 0, 1)],
                },
                MapMeshClusterRanges {
                    cluster: 1,
                    ranges: vec![range(1, 1, 1)],
                },
            ],
        };
        let references = MeshReferences::new(&vertices, &[0, 1, 2, 3], Some(&visibility));
        let mut active = [false; 3];
        references.mark(&mut active, Some(&[false, true]));
        assert_eq!(active, [false, true, false]);
        references.mark(&mut active, Some(&[true, false]));
        assert_eq!(active, [true, true, false]);
        active.fill(false);
        references.mark(&mut active, None);
        assert_eq!(active, [true, true, false]);
    }

    #[test]
    fn first_valid_visible_face_range_matches_index_selection_and_invalid_ranges_do_not_claim_faces()
     {
        let visibility = MapMeshVisibility {
            always_visible: vec![range(7, 10, 1)],
            clusters: vec![
                MapMeshClusterRanges {
                    cluster: 0,
                    ranges: vec![range(7, 0, 1)],
                },
                MapMeshClusterRanges {
                    cluster: 1,
                    ranges: vec![range(7, 1, 1)],
                },
            ],
        };
        let references = MeshReferences::new(
            &[vertex(1, true), vertex(2, true)],
            &[0, 1],
            Some(&visibility),
        );
        let mut active = [false; 2];
        references.mark(&mut active, Some(&[true, true]));
        assert_eq!(active, [true, false]);
        active.fill(false);
        references.mark(&mut active, Some(&[false, true]));
        assert_eq!(active, [false, true]);
    }

    #[test]
    fn compiled_always_visible_range_claims_face_before_runtime_cluster_alternative() {
        let visibility = MapMeshVisibility {
            always_visible: vec![range(7, 0, 1)],
            clusters: vec![MapMeshClusterRanges {
                cluster: 0,
                ranges: vec![range(7, 1, 1)],
            }],
        };
        let references = MeshReferences::new(
            &[vertex(1, false), vertex(2, true)],
            &[0, 1],
            Some(&visibility),
        );
        let mut active = [false; 2];
        references.mark(&mut active, Some(&[true]));
        assert_eq!(active, [false, false]);
        references.mark(&mut active, None);
        assert_eq!(active, [false, true]);
    }

    #[test]
    fn selected_opaque_contexts_union_across_views_without_admitting_hidden_siblings() {
        let mut main = super::super::DrawPlan::default();
        main.opaque_ranges.entry(4).or_default().contexts = vec![0];
        let mut optical = super::super::DrawPlan::default();
        optical.opaque_ranges.entry(4).or_default().contexts = vec![2];
        let mut active = [false; 3];
        assert!(mark_selected(&mut active, &main, 4));
        assert_eq!(active, [true, false, false]);
        assert!(mark_selected(&mut active, &optical, 4));
        assert_eq!(active, [true, false, true]);
        assert!(!mark_selected(&mut active, &main, 5));
    }

    #[test]
    fn empty_visibility_uses_full_mesh_and_repeated_indices_have_one_reference() {
        let references = MeshReferences::new(
            &[vertex(1, true)],
            &[0, 0, 0, 9],
            Some(&MapMeshVisibility::default()),
        );
        assert_eq!(references.all.as_ref(), &[0]);
        let mut active = [false];
        references.mark(&mut active, Some(&[]));
        assert_eq!(active, [true]);
    }
}
