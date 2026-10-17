use super::resources::{RenderResources, ViewPipelines};
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
    pub skybox: bool,
    pub transparency: &'a [super::transparency::Draw],
}
use super::geometry::ViewerScene;
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
    resources: &'a ViewPipelines,
    upload: &'a ViewerScene,
    uniforms: &'a wgpu::BindGroup,
) {
    let Some(skybox) = upload.bindings.background.skybox.as_ref() else {
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

pub fn draw_scene_plan_opaque<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a ViewPipelines,
    upload: &'a ViewerScene,
    plan: &DrawPlan,
    view: DrawView<'a>,
) {
    if plan.opaque_draw_count() == 0 {
        return;
    }
    pass.set_bind_group(0, view.uniforms, &[]);
    for item in &plan.opaque {
        bind_opaque_mesh_pipeline(pass, resources, upload, *item);
        if let Some(ranges) = plan.opaque_ranges.get(&item.mesh_index) {
            for range in &ranges.indices {
                draw_model_range(pass, upload, *item, range.clone());
            }
        } else {
            draw_model_item(pass, upload, *item, view.visibility);
        }
    }
}

pub fn draw_opaque_surfaces<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a ViewPipelines,
    upload: &'a ViewerScene,
    batch: &super::super::draw_plan::SurfaceBatch,
    view: DrawView<'a>,
) {
    pass.set_bind_group(0, view.uniforms, &[]);
    for mesh in &batch.meshes {
        let item = mesh.item;
        bind_opaque_mesh_pipeline(pass, resources, upload, item);
        draw_model_range(pass, upload, item, mesh.indices.clone());
    }
}

fn bind_opaque_mesh_pipeline<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a ViewPipelines,
    upload: &'a ViewerScene,
    item: DrawItem,
) {
    pass.set_pipeline(
        upload
            .assets
            .surface_pipelines
            .get(
                item.material_slot,
                resources.kind(),
                false,
                SurfaceBlend::Opaque,
                upload.meshes[item.mesh_index].geometry,
            )
            .unwrap_or(&resources.opaque_pipeline),
    );
}

pub fn draw_planar_items<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a ViewPipelines,
    upload: &'a ViewerScene,
    items: &[DrawItem],
    view: DrawView<'a>,
) {
    pass.set_bind_group(0, view.uniforms, &[]);
    pass.set_pipeline(&resources.planar_pipeline);
    for item in items {
        draw_model_item(pass, upload, *item, view.visibility);
    }
}

pub fn draw_operations<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a ViewPipelines,
    upload: &'a ViewerScene,
    view: DrawView<'a>,
) {
    pass.set_bind_group(0, view.uniforms, &[]);
    for operation in view.transparency {
        use super::transparency::Primitive;
        match &operation.primitive {
            Primitive::Mesh(item, range) => {
                let ordinary = if operation.blend == super::RenderMode::Modulate {
                    &resources.modulate_pipeline
                } else {
                    transparent_pipeline(
                        resources,
                        false,
                        operation.blend == super::RenderMode::Additive,
                    )
                };
                pass.set_pipeline(
                    upload
                        .assets
                        .surface_pipelines
                        .get(
                            item.material_slot,
                            resources.kind(),
                            false,
                            SurfaceBlend::from(operation.blend),
                            upload.meshes[item.mesh_index].geometry,
                        )
                        .unwrap_or(ordinary),
                );
                if let Some(range) = range {
                    draw_model_range(pass, upload, *item, range.clone());
                } else {
                    draw_model_item(pass, upload, *item, view.visibility);
                }
            }
            Primitive::Overlay(item) => draw_surface_mark(pass, resources, upload, *item),
            Primitive::Sprite(range) => {
                let sprites = if view.skybox {
                    upload.assets.map_skybox_detail_sprites.as_ref()
                } else {
                    upload.assets.detail_sprites.as_ref()
                };
                let Some(sprites) = sprites else {
                    continue;
                };
                pass.set_pipeline(&resources.detail_pipeline);
                pass.set_bind_group(2, upload.lightmaps.binding(None), &[]);
                pass.set_bind_group(1, &upload.bindings.materials[sprites.material_index], &[]);
                pass.set_vertex_buffer(0, sprites.vertices.slice(..));
                pass.draw(range.clone(), 0..1);
            }
        }
    }
}

pub fn draw_surface_mark<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a ViewPipelines,
    upload: &'a ViewerScene,
    item: OverlayDrawItem,
) {
    let blend = SurfaceBlend::from(item.blend);
    let fallback = match item.blend {
        super::RenderMode::NoDraw => return,
        super::RenderMode::Opaque | super::RenderMode::Cutout => &resources.overlay_opaque_pipeline,
        super::RenderMode::Additive => &resources.overlay_additive_pipeline,
        super::RenderMode::Modulate => &resources.overlay_modulate_pipeline,
        super::RenderMode::Translucent => &resources.overlay_translucent_pipeline,
    };
    pass.set_pipeline(
        upload
            .assets
            .surface_pipelines
            .get(
                item.material_slot,
                resources.kind(),
                true,
                blend,
                crate::media::preview_model::MeshGeometry::Brush,
            )
            .unwrap_or(fallback),
    );
    draw_overlay_item(pass, upload, item);
}

fn transparent_pipeline(
    resources: &ViewPipelines,
    overlay: bool,
    additive: bool,
) -> &wgpu::RenderPipeline {
    match (overlay, additive) {
        (true, true) => &resources.overlay_additive_pipeline,
        (true, false) => &resources.overlay_translucent_pipeline,
        (false, true) => &resources.additive_pipeline,
        (false, false) => &resources.translucent_pipeline,
    }
}

pub fn draw_model_item<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    upload: &'a ViewerScene,
    item: DrawItem,
    visibility: &'a UploadedVisibility,
) {
    let Some(mesh) = upload.meshes.get(item.mesh_index) else {
        return;
    };
    let Some(bind_group) = upload.bindings.materials.get(item.material_slot) else {
        return;
    };
    pass.set_bind_group(1, bind_group, &[]);
    pass.set_vertex_buffer(0, mesh.vertices().slice(..));
    if let Some(visible) = visibility.mesh_indices.get(&item.mesh_index) {
        if let Some(buffer) = &visible.buffer {
            pass.set_index_buffer(buffer.slice(..), wgpu::IndexFormat::Uint32);
            draw_lightmap_ranges(pass, &upload.lightmaps, &visible.lightmaps);
        }
    } else {
        pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        draw_lightmap_ranges(pass, &upload.lightmaps, &mesh.lightmaps.indexed);
    }
}

fn draw_model_range<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    upload: &'a ViewerScene,
    item: DrawItem,
    range: std::ops::Range<u32>,
) {
    let mesh = &upload.meshes[item.mesh_index];
    pass.set_bind_group(1, &upload.bindings.materials[item.material_slot], &[]);
    pass.set_vertex_buffer(0, mesh.vertices().slice(..));
    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
    for part in mesh.lightmaps.intersect(range) {
        pass.set_bind_group(2, upload.lightmaps.binding(part.page), &[]);
        pass.draw_indexed(part.indices, 0, 0..1);
    }
}

pub(super) fn draw_lightmap_ranges<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    lightmaps: &'a super::lightmaps::Snapshot,
    ranges: &[super::lightmap_ranges::PageRange],
) {
    for range in ranges {
        pass.set_bind_group(2, lightmaps.binding(range.page), &[]);
        pass.draw_indexed(range.indices.clone(), 0, 0..1);
    }
}

pub fn draw_phy_debug_meshes<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a RenderResources,
    upload: &'a ViewerScene,
) {
    let Some(meshes) = upload.assets.phy_debug_meshes.get() else {
        return;
    };
    pass.set_pipeline(&resources.phy_debug_pipeline);
    pass.set_bind_group(2, upload.lightmaps.binding(None), &[]);
    for mesh in meshes {
        let Some(bind_group) = upload.bindings.materials.get(mesh.material_index) else {
            continue;
        };
        draw_uploaded_mesh(
            pass,
            mesh.vertices(),
            &mesh.indices,
            mesh.index_count,
            bind_group,
            None,
        );
    }
}

pub fn draw_uploaded_mesh<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    vertices: &'a wgpu::Buffer,
    indices: &'a wgpu::Buffer,
    index_count: u32,
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
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_index_buffer(buffer.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..visible.index_count, 0, 0..1);
        return;
    }
    pass.set_bind_group(1, bind_group, &[]);
    pass.set_vertex_buffer(0, vertices.slice(..));
    pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
    pass.draw_indexed(0..index_count, 0, 0..1);
}

pub fn draw_overlay_item<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    upload: &'a ViewerScene,
    item: OverlayDrawItem,
) {
    let Some(overlay) = upload.overlays.get(item.overlay_index) else {
        return;
    };
    let Some(bind_group) = upload.bindings.materials.get(item.material_slot) else {
        return;
    };
    pass.set_bind_group(1, bind_group, &[]);
    pass.set_vertex_buffer(0, overlay.vertices().slice(..));
    for range in &overlay.lightmaps {
        pass.set_bind_group(2, upload.lightmaps.binding(range.page), &[]);
        pass.draw(range.indices.clone(), 0..1);
    }
}
