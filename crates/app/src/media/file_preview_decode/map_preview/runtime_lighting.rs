use crate::{
    bridge::materials::{SurfaceFamily, SurfaceMaterial},
    media::preview_model::{MaterialSlot, MeshData, ModelLightingContext},
};
use gmpublished_domain::math::Vec3;

fn pixel_lighting(surface: &SurfaceMaterial) -> bool {
    surface.normal.is_some()
        || surface.uses_skin_shader()
        || surface.phong.as_ref().is_some_and(|phong| {
            phong
                .diffuse_warp
                .as_ref()
                .is_some_and(|warp| warp.texture.is_some())
        })
}

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
            let Some(context) = vertex
                .lighting_context
                .checked_sub(1)
                .and_then(|index| contexts.get(index as usize))
            else {
                continue;
            };
            if !matches!(context.cpu_seed, Ok(Some(_)))
                || (!context.entity && context.compiled_lighting && !pixel_lighting(surface))
            {
                continue;
            }
            // Plain compiled VHV remains a vertex stream. Bumped/skin draws
            // use runtime directional ambient and selected local lights.
            vertex.lighting_mode |= 5;
            vertex.color = Vec3::from(std::array::from_fn(|channel| {
                f32::from(context.diffuse_modulation[channel]) / 255.0
            }));
        }
    }
}
