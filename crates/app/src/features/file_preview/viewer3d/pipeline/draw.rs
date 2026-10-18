use super::resources::{RenderResources, ViewPipelines};
use super::surface_pipelines::SurfaceBlend;
use super::visibility::{UploadedVisibility, UploadedVisibleIndices};

mod transparent_bindings;

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
    pub indirect: Option<super::transparent_indirect::Program<'a>>,
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

/// Feature-only counterfactual; ordinary builds always suppress redundant bindings.
#[inline]
pub(super) fn opaque_binding_suppression_enabled() -> bool {
    #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
    {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ENABLED.get_or_init(|| {
            !matches!(
                std::env::var("GMPUBLISHED_OPAQUE_BINDING_SUPPRESSION").as_deref(),
                Ok("0")
            )
        })
    }
    #[cfg(not(any(feature = "gui-benchmark", feature = "map-capture")))]
    {
        true
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
    let suppress_bindings = opaque_binding_suppression_enabled();
    for item in &plan.opaque {
        bind_opaque_mesh_pipeline(pass, resources, upload, *item);
        if let Some(ranges) = plan.opaque_ranges.get(&item.mesh_index) {
            if suppress_bindings {
                if !ranges.indices.is_empty() {
                    bind_model_mesh(pass, upload, *item);
                }
                for range in &ranges.indices {
                    draw_bound_model_range(pass, upload, *item, range.clone());
                }
            } else {
                for range in &ranges.indices {
                    draw_model_range(pass, upload, *item, range.clone());
                }
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
    if !opaque_binding_suppression_enabled() {
        for mesh in &batch.meshes {
            let item = mesh.item;
            bind_opaque_mesh_pipeline(pass, resources, upload, item);
            draw_model_range(pass, upload, item, mesh.indices.clone());
        }
        return;
    }
    let mut bound_mesh = None;
    for mesh in &batch.meshes {
        let item = mesh.item;
        if bound_mesh != Some(item) {
            bind_opaque_mesh_pipeline(pass, resources, upload, item);
            bind_model_mesh(pass, upload, item);
            bound_mesh = Some(item);
        }
        draw_bound_model_range(pass, upload, item, mesh.indices.clone());
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

/// Feature-only counterfactual; ordinary builds suppress redundant transparent bindings.
#[inline]
pub(super) fn transparent_binding_suppression_enabled() -> bool {
    #[cfg(any(feature = "gui-benchmark", feature = "map-capture"))]
    {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ENABLED.get_or_init(|| {
            std::env::var_os("GMPUBLISHED_TRANSPARENT_BINDING_SUPPRESSION").as_deref()
                != Some(std::ffi::OsStr::new("0"))
        })
    }
    #[cfg(not(any(feature = "gui-benchmark", feature = "map-capture")))]
    {
        true
    }
}

pub fn draw_operations<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    resources: &'a ViewPipelines,
    upload: &'a ViewerScene,
    view: DrawView<'a>,
    framebuffer: Option<&'a wgpu::BindGroup>,
) {
    let suppress_bindings = transparent_binding_suppression_enabled();
    use transparent_bindings::{IndexSource, Key, State};
    pass.set_bind_group(0, view.uniforms, &[]);
    // One invocation is one pass/target/transfer segment. Never retain this state.
    let mut bindings = State::default();
    let mut operations = view.transparency.iter().enumerate();
    let mut run_index = 0;
    while let Some((operation_index, operation)) = operations.next() {
        use super::transparency::Primitive;
        match &operation.primitive {
            Primitive::Mesh(item, range) => {
                let mesh = &upload.meshes[item.mesh_index];
                let visible = if range.is_none() {
                    view.visibility.mesh_indices.get(&item.mesh_index)
                } else {
                    None
                };
                let source = IndexSource::select(
                    range.is_some(),
                    visible.map(|visible| visible.buffer.is_some()),
                )
                .unwrap_or(IndexSource::Visible);
                if !suppress_bindings
                    || bindings.mesh(Key {
                        item: *item,
                        blend: operation.blend,
                        source,
                    })
                {
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
                                mesh.geometry,
                            )
                            .unwrap_or(ordinary),
                    );
                    let material = if range.is_some() {
                        &upload.bindings.materials[item.material_slot]
                    } else {
                        let Some(material) = upload.bindings.materials.get(item.material_slot)
                        else {
                            bindings.reset();
                            continue;
                        };
                        material
                    };
                    pass.set_bind_group(1, material, &[]);
                    pass.set_vertex_buffer(0, mesh.vertices().slice(..));
                    let indices = match source {
                        IndexSource::Original => Some(&mesh.indices),
                        IndexSource::Visible => visible.and_then(|visible| visible.buffer.as_ref()),
                    };
                    let Some(indices) = indices else {
                        // Preserve the original pipeline/material/vertex binds above, but
                        // never fall back to original indices for an empty visible entry.
                        bindings.reset();
                        continue;
                    };
                    pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                }
                if let Some(program) = view.indirect
                    && let Some(run) = program.runs.get(run_index)
                    && run.operations.start == operation_index + program.operation_offset
                {
                    if !suppress_bindings || bindings.page(run.page) {
                        pass.set_bind_group(
                            2,
                            framebuffer.unwrap_or_else(|| upload.lightmaps.binding(run.page)),
                            &[],
                        );
                    }
                    pass.multi_draw_indexed_indirect(program.buffer, run.offset, run.count());
                    // The first operation used the ordinary binding path above.
                    // Every remaining operation is a whole, same-key one-part draw.
                    operations.nth(run.operations.len() - 2);
                    run_index += 1;
                    continue;
                }
                let mut draw_part = |part: super::lightmap_ranges::PageRange| {
                    if !suppress_bindings || bindings.page(part.page) {
                        pass.set_bind_group(
                            2,
                            framebuffer.unwrap_or_else(|| upload.lightmaps.binding(part.page)),
                            &[],
                        );
                    }
                    pass.draw_indexed(part.indices, 0, 0..1);
                };
                if let Some(range) = range {
                    for part in mesh.lightmaps.intersect(range.clone()) {
                        draw_part(part);
                    }
                } else {
                    let ranges = match source {
                        IndexSource::Original => &mesh.lightmaps.indexed,
                        IndexSource::Visible => {
                            &visible.expect("selected visible indices").lightmaps
                        }
                    };
                    for part in ranges {
                        draw_part(part.clone());
                    }
                }
            }
            Primitive::Overlay(item) => {
                bindings.reset();
                draw_surface_mark(pass, resources, upload, *item);
            }
            Primitive::Sprite(range) => {
                bindings.reset();
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
    bind_model_mesh(pass, upload, item);
    draw_bound_model_range(pass, upload, item, range);
}

fn bind_model_mesh<'a>(pass: &mut wgpu::RenderPass<'a>, upload: &'a ViewerScene, item: DrawItem) {
    let mesh = &upload.meshes[item.mesh_index];
    pass.set_bind_group(1, &upload.bindings.materials[item.material_slot], &[]);
    pass.set_vertex_buffer(0, mesh.vertices().slice(..));
    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
}

fn draw_bound_model_range<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    upload: &'a ViewerScene,
    item: DrawItem,
    range: std::ops::Range<u32>,
) {
    let mesh = &upload.meshes[item.mesh_index];
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
