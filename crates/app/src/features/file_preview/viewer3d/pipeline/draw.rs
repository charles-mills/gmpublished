use super::resources::RenderResources;
use super::surface_pipelines::SurfaceBlend;
use super::visibility::{UploadedVisibility, UploadedVisibleIndices};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewKind {
    Main,
    Reflection,
    Refraction,
}

#[derive(Clone, Copy)]
pub struct DrawView<'a> {
    pub uniforms: &'a wgpu::BindGroup,
    pub visibility: &'a UploadedVisibility,
    pub kind: ViewKind,
    pub skybox: bool,
    pub transparency: &'a [super::transparency::Draw],
}
use super::upload::{UploadedMesh, UploadedModel};
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
    uniforms: &'a wgpu::BindGroup,
) {
    let Some(skybox) = upload.skybox.as_ref() else {
        return;
    };
    pass.set_pipeline(&resources.sky_pipeline);
    pass.set_bind_group(0, uniforms, &[]);
    pass.set_vertex_buffer(0, resources.sky_vertices.slice(..));
    for face in SkyboxFace::ALL {
        let Some(bind_group) = skybox.face_bind_groups[face.index()].as_ref() else {
            continue;
        };
        let start =
            u32::try_from(face.index() * 6).expect("the six skybox faces fit a u32 draw range");
        pass.set_bind_group(1, bind_group, &[]);
        pass.draw(start..start + 6, 0..1);
    }
}

pub fn draw_scene_plan<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a RenderResources,
    upload: &'a UploadedModel,
    plan: &DrawPlan,
    view: DrawView<'a>,
) {
    draw_scene_plan_opaque(pass, resources, upload, plan, view);
    draw_scene_plan_transparent(pass, resources, upload, plan, view);
}

pub fn draw_scene_plan_opaque<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a RenderResources,
    upload: &'a UploadedModel,
    plan: &DrawPlan,
    view: DrawView<'a>,
) {
    pass.set_bind_group(0, view.uniforms, &[]);
    let ordinary = if view.kind == ViewKind::Refraction {
        &resources.refracted_opaque_pipeline
    } else if view.kind == ViewKind::Reflection {
        &resources.reflected_opaque_pipeline
    } else {
        &resources.opaque_pipeline
    };
    for item in &plan.opaque {
        pass.set_pipeline(
            upload
                .surface_pipelines
                .get(
                    item.material_slot,
                    view.kind,
                    false,
                    SurfaceBlend::Opaque,
                    upload.meshes[item.mesh_index].geometry,
                )
                .unwrap_or(ordinary),
        );
        draw_model_item(pass, upload, *item, view.visibility);
    }
    let overlay = if view.kind == ViewKind::Refraction {
        &resources.refracted_overlay_opaque_pipeline
    } else {
        &resources.overlay_opaque_pipeline
    };
    for item in &plan.overlay_opaque {
        pass.set_pipeline(
            upload
                .surface_pipelines
                .get(
                    item.material_slot,
                    view.kind,
                    true,
                    SurfaceBlend::Opaque,
                    crate::media::preview_model::MeshGeometry::Brush,
                )
                .unwrap_or(overlay),
        );
        draw_overlay_item(pass, upload, *item);
    }
}

pub fn draw_scene_plan_transparent<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a RenderResources,
    upload: &'a UploadedModel,
    plan: &DrawPlan,
    view: DrawView<'a>,
) {
    pass.set_bind_group(0, view.uniforms, &[]);
    pass.set_pipeline(if view.kind == ViewKind::Refraction {
        &resources.refracted_planar_pipeline
    } else if view.kind == ViewKind::Reflection {
        &resources.reflected_water_pipeline
    } else {
        &resources.water_pipeline
    });
    for item in &plan.planar {
        draw_model_item(pass, upload, *item, view.visibility);
    }
    for operation in view.transparency {
        use super::transparency::Primitive;
        match &operation.primitive {
            Primitive::Mesh(item, range) => {
                let ordinary = transparent_pipeline(resources, view, false, operation.additive);
                pass.set_pipeline(
                    upload
                        .surface_pipelines
                        .get(
                            item.material_slot,
                            view.kind,
                            false,
                            if operation.additive {
                                SurfaceBlend::Additive
                            } else {
                                SurfaceBlend::Alpha
                            },
                            upload.meshes[item.mesh_index].geometry,
                        )
                        .unwrap_or(ordinary),
                );
                if let Some(range) = range {
                    let mesh = &upload.meshes[item.mesh_index];
                    pass.set_bind_group(1, &upload.material_bind_groups[item.material_slot], &[]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(range.clone(), 0, 0..1);
                } else {
                    draw_model_item(pass, upload, *item, view.visibility);
                }
            }
            Primitive::Overlay(item) => {
                let ordinary = transparent_pipeline(resources, view, true, operation.additive);
                pass.set_pipeline(
                    upload
                        .surface_pipelines
                        .get(
                            item.material_slot,
                            view.kind,
                            true,
                            if operation.additive {
                                SurfaceBlend::Additive
                            } else {
                                SurfaceBlend::Alpha
                            },
                            crate::media::preview_model::MeshGeometry::Brush,
                        )
                        .unwrap_or(ordinary),
                );
                draw_overlay_item(pass, upload, *item);
            }
            Primitive::Sprite(range) => {
                let sprites = if view.skybox {
                    upload.map_skybox_detail_sprites.as_ref()
                } else {
                    upload.detail_sprites.as_ref()
                };
                let Some(sprites) = sprites else {
                    continue;
                };
                pass.set_pipeline(if view.kind == ViewKind::Refraction {
                    &resources.refracted_detail_pipeline
                } else {
                    &resources.detail_pipeline
                });
                pass.set_bind_group(1, &upload.material_bind_groups[sprites.material_index], &[]);
                pass.set_vertex_buffer(0, sprites.vertices.slice(..));
                pass.draw(range.clone(), 0..1);
            }
        }
    }
}

fn transparent_pipeline<'a>(
    resources: &'a RenderResources,
    view: DrawView<'_>,
    overlay: bool,
    additive: bool,
) -> &'a wgpu::RenderPipeline {
    match (view.kind, overlay, additive) {
        (ViewKind::Refraction, true, true) => &resources.refracted_overlay_additive_pipeline,
        (ViewKind::Refraction, true, false) => &resources.refracted_overlay_translucent_pipeline,
        (ViewKind::Refraction, false, true) => &resources.refracted_additive_pipeline,
        (ViewKind::Refraction, false, false) => &resources.refracted_translucent_pipeline,
        (_, true, true) => &resources.overlay_additive_pipeline,
        (_, true, false) => &resources.overlay_translucent_pipeline,
        (ViewKind::Reflection, false, true) => &resources.reflected_additive_pipeline,
        (ViewKind::Reflection, false, false) => &resources.reflected_translucent_pipeline,
        (ViewKind::Main, false, true) => &resources.additive_pipeline,
        (ViewKind::Main, false, false) => &resources.translucent_pipeline,
    }
}

pub fn draw_model_item<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    upload: &'a UploadedModel,
    item: DrawItem,
    visibility: &'a UploadedVisibility,
) {
    let Some(mesh) = upload.meshes.get(item.mesh_index) else {
        return;
    };
    let Some(bind_group) = upload.material_bind_groups.get(item.material_slot) else {
        return;
    };
    draw_uploaded_mesh(
        pass,
        mesh,
        bind_group,
        visibility.mesh_indices.get(&item.mesh_index),
    );
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
        draw_uploaded_mesh(pass, mesh, bind_group, None);
    }
}

pub fn draw_uploaded_mesh<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    mesh: &'a UploadedMesh,
    bind_group: &'a wgpu::BindGroup,
    visible: Option<&'a UploadedVisibleIndices>,
) {
    if let Some(visible) = visible {
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
