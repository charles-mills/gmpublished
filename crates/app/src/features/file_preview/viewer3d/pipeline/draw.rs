//! Pass encoding: binds the pipelines from [`RenderResources`] and the
//! buffers of an uploaded model, and issues the draws a plan orders. Holds
//! no state of its own.

use super::resources::RenderResources;
use super::upload::{UploadedDetailSprites, UploadedMesh, UploadedModel};
use super::{DrawItem, DrawPlan, OverlayDrawItem, Rectangle, SkyboxFace, wgpu};

pub fn configure_scene_pass(pass: &mut wgpu::RenderPass<'_>, clip_bounds: &Rectangle<u32>) {
    pass.set_scissor_rect(
        clip_bounds.x,
        clip_bounds.y,
        clip_bounds.width,
        clip_bounds.height,
    );
    pass.set_viewport(
        clip_bounds.x as f32,
        clip_bounds.y as f32,
        clip_bounds.width as f32,
        clip_bounds.height as f32,
        0.0,
        1.0,
    );
}

pub fn draw_sky_background<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a RenderResources,
    upload: &'a UploadedModel,
) {
    let Some(skybox) = upload.skybox.as_ref() else {
        return;
    };
    pass.set_pipeline(&resources.sky_pipeline);
    pass.set_bind_group(0, &resources.sky_uniform_bind_group, &[]);
    pass.set_vertex_buffer(0, resources.sky_vertices.slice(..));
    for face in SkyboxFace::ALL {
        let Some(bind_group) = skybox.face_bind_groups[face.index()].as_ref() else {
            continue;
        };
        let start = u32::try_from(face.index() * 6).unwrap_or(0);
        pass.set_bind_group(1, bind_group, &[]);
        pass.draw(start..start + 6, 0..1);
    }
}

pub fn draw_scene_plan<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a RenderResources,
    upload: &'a UploadedModel,
    plan: &'a DrawPlan,
    uniform_bind_group: &'a wgpu::BindGroup,
    detail_sprites: Option<&'a UploadedDetailSprites>,
) {
    draw_scene_plan_opaque(
        pass,
        resources,
        upload,
        plan,
        uniform_bind_group,
        detail_sprites,
    );
    draw_scene_plan_transparent(pass, resources, upload, plan, None);
}

pub fn draw_scene_plan_opaque<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a RenderResources,
    upload: &'a UploadedModel,
    plan: &'a DrawPlan,
    uniform_bind_group: &'a wgpu::BindGroup,
    detail_sprites: Option<&'a UploadedDetailSprites>,
) {
    pass.set_bind_group(0, uniform_bind_group, &[]);
    pass.set_pipeline(&resources.opaque_pipeline);
    for item in &plan.opaque {
        draw_model_item(pass, upload, *item);
    }
    if let Some(detail_sprites) = detail_sprites {
        pass.set_pipeline(&resources.detail_pipeline);
        draw_detail_sprites(pass, upload, detail_sprites);
    }
    pass.set_pipeline(&resources.overlay_opaque_pipeline);
    for item in &plan.overlay_opaque {
        draw_overlay_item(pass, upload, *item);
    }
}

pub fn draw_scene_plan_transparent<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a RenderResources,
    upload: &'a UploadedModel,
    plan: &'a DrawPlan,
    refraction_bind_group: Option<&'a wgpu::BindGroup>,
) {
    if let (Some(refraction_bind_group), Some(refractive_water_pipeline)) = (
        refraction_bind_group,
        resources.refractive_water_pipeline.as_ref(),
    ) {
        pass.set_pipeline(refractive_water_pipeline);
        pass.set_bind_group(2, refraction_bind_group, &[]);
    } else {
        pass.set_pipeline(&resources.water_pipeline);
    }
    for item in &plan.water {
        draw_model_item(pass, upload, *item);
    }
    pass.set_pipeline(&resources.overlay_translucent_pipeline);
    for item in &plan.overlay_translucent {
        draw_overlay_item(pass, upload, *item);
    }
    pass.set_pipeline(&resources.overlay_additive_pipeline);
    for item in &plan.overlay_additive {
        draw_overlay_item(pass, upload, *item);
    }
    pass.set_pipeline(&resources.translucent_pipeline);
    for item in &plan.translucent {
        draw_model_item(pass, upload, *item);
    }
    pass.set_pipeline(&resources.additive_pipeline);
    for item in &plan.additive {
        draw_model_item(pass, upload, *item);
    }
}

pub fn draw_model_item<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    upload: &'a UploadedModel,
    item: DrawItem,
) {
    let Some(mesh) = upload.meshes.get(item.mesh_index) else {
        return;
    };
    let Some(bind_group) = upload.material_bind_groups.get(item.material_slot) else {
        return;
    };
    draw_uploaded_mesh(pass, mesh, bind_group);
}

pub fn draw_phy_debug_meshes<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a RenderResources,
    upload: &'a UploadedModel,
) {
    let Some(meshes) = upload.phy_debug_meshes.as_ref() else {
        return;
    };
    pass.set_pipeline(&resources.phy_debug_pipeline);
    for mesh in meshes {
        let Some(bind_group) = upload.material_bind_groups.get(mesh.material_index) else {
            continue;
        };
        draw_uploaded_mesh(pass, mesh, bind_group);
    }
}

pub fn draw_uploaded_mesh<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    mesh: &'a UploadedMesh,
    bind_group: &'a wgpu::BindGroup,
) {
    if let Some(visible) = mesh.visible_indices.as_ref() {
        if visible.index_count == 0 {
            return;
        }
        let Some(buffer) = visible.buffer.as_ref() else {
            return;
        };
        pass.set_bind_group(1, bind_group, &[]);
        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
        pass.set_index_buffer(buffer.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..visible.index_count, 0, 0..1);
        return;
    }
    pass.set_bind_group(1, bind_group, &[]);
    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
}

pub fn draw_detail_sprites<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    upload: &'a UploadedModel,
    detail_sprites: &'a UploadedDetailSprites,
) {
    let Some(bind_group) = upload
        .material_bind_groups
        .get(detail_sprites.material_index)
    else {
        return;
    };
    pass.set_bind_group(1, bind_group, &[]);
    if let Some(visible) = detail_sprites.visible_vertices.as_ref() {
        if visible.vertex_count == 0 {
            return;
        }
        let Some(buffer) = visible.buffer.as_ref() else {
            return;
        };
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.draw(0..visible.vertex_count, 0..1);
    } else {
        pass.set_vertex_buffer(0, detail_sprites.vertices.slice(..));
        pass.draw(0..detail_sprites.vertex_count, 0..1);
    }
}

pub fn draw_overlay_item<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    upload: &'a UploadedModel,
    item: OverlayDrawItem,
) {
    let Some(overlay) = upload.overlays.get(item.overlay_index) else {
        return;
    };
    let Some(bind_group) = upload.material_bind_groups.get(item.material_slot) else {
        return;
    };
    pass.set_bind_group(1, bind_group, &[]);
    pass.set_vertex_buffer(0, overlay.vertices.slice(..));
    pass.draw(0..overlay.vertex_count, 0..1);
}
