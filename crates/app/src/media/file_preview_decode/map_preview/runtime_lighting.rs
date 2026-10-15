use crate::{
    bridge::materials::SurfaceFamily,
    media::preview_model::{MaterialSlot, MeshData, ModelLightingContext},
};
use gmpublished_domain::math::Vec3;

pub(super) fn activate(
    meshes: &mut [MeshData],
    contexts: &[ModelLightingContext],
    materials: &[MaterialSlot],
) {
    for mesh in meshes {
        let Some(surface) = materials
            .get(mesh.material_index)
            .and_then(|material| material.shader.surface())
        else {
            continue;
        };
        if surface.family != SurfaceFamily::VertexLit {
            continue;
        }
        for vertex in &mut mesh.vertices {
            let Some(context) = crate::media::preview_model::ModelLightingContextId::from_gpu(
                vertex.lighting_context,
            )
            .and_then(|id| contexts.get(id.index())) else {
                continue;
            };
            if context.source.runtime_seed().is_none()
                || (!context.entity
                    && context.compiled_lighting
                    && !surface.model_shading_profile().requires_runtime_lighting())
            {
                continue;
            }
            vertex.lighting_mode &= !(crate::media::preview_model::vertex_lighting::PREVIEW
                | crate::media::preview_model::vertex_lighting::LEGACY_VERTEX_COLOR);
            vertex.lighting_mode |= crate::media::preview_model::vertex_lighting::RUNTIME_CONTEXT
                | crate::media::preview_model::vertex_lighting::LINEAR_ILLUMINATION;
            vertex.color = Vec3::from(std::array::from_fn(|channel| {
                f32::from(context.diffuse_modulation[channel]) / 255.0
            }));
        }
    }
}
