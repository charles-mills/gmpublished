//! Advance temporal lighting only for placements included in prepared views.
use super::{MapPreview, ModelVertex, RenderScene, SceneView, UploadedModel};

pub(super) fn gather(
    active: &mut [bool],
    scene: &RenderScene,
    map: Option<&MapPreview>,
    upload: &UploadedModel,
    view: &SceneView,
) {
    let Some(plans) = &view.plans else { return };
    for (plan, skybox) in std::iter::once((&plans.world, false))
        .chain(plans.map_skybox.as_ref().map(|plan| (plan, true)))
    {
        for item in plan
            .opaque
            .iter()
            .chain(&plan.planar)
            .chain(&plan.translucent)
            .chain(&plan.additive)
        {
            let uploaded = &upload.meshes[item.mesh_index];
            if uploaded.geometry != crate::media::preview_model::MeshGeometry::Model {
                continue;
            }
            let meshes = if skybox {
                let Some(map) = map else { continue };
                &map.map_skybox_meshes
            } else {
                &scene.meshes
            };
            let Some(mesh) = meshes.get(uploaded.scene_mesh_index) else {
                continue;
            };
            let indices = if !skybox && uploaded.door_index.is_none() {
                view.visibility
                    .plan
                    .as_ref()
                    .map_or(mesh.indices.as_slice(), |plan| {
                        plan.mesh_indices
                            .get(uploaded.scene_mesh_index)
                            .map_or(&[], Vec::as_slice)
                    })
            } else {
                &mesh.indices
            };
            mark(active, &mesh.vertices, indices);
        }
    }
}

fn mark(active: &mut [bool], vertices: &[ModelVertex], indices: &[u32]) {
    for &index in indices {
        let Some(vertex) = vertices.get(index as usize) else {
            continue;
        };
        if vertex.lighting_mode & 4 == 0 {
            continue;
        }
        if let Some(context) = vertex
            .lighting_context
            .checked_sub(1)
            .and_then(|id| active.get_mut(id as usize))
        {
            *context = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merged_mesh_visibility_advances_only_referenced_runtime_placements() {
        let mut vertex: ModelVertex = bytemuck::Zeroable::zeroed();
        vertex.lighting_mode = 4;
        vertex.lighting_context = 1;
        let first = vertex;
        vertex.lighting_context = 2;
        let second = vertex;
        vertex.lighting_mode = 0;
        vertex.lighting_context = 3;
        let mut active = [false; 3];
        mark(&mut active, &[first, second, vertex], &[1, 2]);
        assert_eq!(active, [false, true, false]);
        mark(&mut active, &[first], &[0]);
        assert_eq!(active, [true, true, false]);
    }
}
