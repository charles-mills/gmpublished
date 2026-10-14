//! Explicit view-local pass ordering: reflected scene views, main opaque
//! snapshot, planar surfaces, remaining transparency, then UI composition.
use super::draw::{self, DrawView};
use super::reflections::SceneView;
use super::targets::RenderTargets;
use super::{ModelPrimitive, PreparedFrame, Rectangle, RenderResources, UploadedModel, wgpu};

fn pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    targets: &RenderTargets,
    label: &'static str,
    load: wgpu::LoadOp<wgpu::Color>,
    depth: Option<wgpu::Operations<f32>>,
    resolve: bool,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &targets.color,
            resolve_target: resolve.then_some(&targets.resolve_view),
            ops: wgpu::Operations {
                load,
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &targets.depth,
            depth_ops: depth,
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
    })
}
fn depth_clear() -> wgpu::Operations<f32> {
    wgpu::Operations {
        load: wgpu::LoadOp::Clear(0.0),
        store: wgpu::StoreOp::Store,
    }
}
fn depth_load() -> wgpu::Operations<f32> {
    wgpu::Operations {
        load: wgpu::LoadOp::Load,
        store: wgpu::StoreOp::Store,
    }
}
fn local_bounds(targets: &RenderTargets) -> Rectangle<u32> {
    Rectangle {
        x: 0,
        y: 0,
        width: targets.size.0,
        height: targets.size.1,
    }
}

fn render_opaque(
    view: &SceneView,
    resources: &RenderResources,
    upload: &UploadedModel,
    encoder: &mut wgpu::CommandEncoder,
    reflected: bool,
    clear: wgpu::Color,
    sky: bool,
) {
    let targets = view.targets.as_ref().expect("prepared view targets");
    let plans = view.plans.as_ref().expect("prepared view plans");
    let bounds = local_bounds(targets);
    if let Some(plan) = plans.map_skybox.as_ref() {
        let mut pass = pass(
            encoder,
            targets,
            "file_preview.view.skybox",
            wgpu::LoadOp::Clear(clear),
            Some(depth_clear()),
            false,
        );
        draw::configure_scene_pass(&mut pass, &bounds);
        if sky {
            draw::draw_sky_background(&mut pass, resources, upload, &view.sky.bind_group);
        }
        draw::draw_scene_plan(
            &mut pass,
            resources,
            upload,
            plan,
            DrawView {
                uniforms: &view.skybox.bind_group,
                visibility: &view.skybox_visibility,
                reflected,
            },
            upload.map_skybox_detail_sprites.as_ref(),
        );
    }
    let mut pass = pass(
        encoder,
        targets,
        "file_preview.view.opaque",
        if plans.map_skybox.is_some() {
            wgpu::LoadOp::Load
        } else {
            wgpu::LoadOp::Clear(clear)
        },
        Some(depth_clear()),
        true,
    );
    draw::configure_scene_pass(&mut pass, &bounds);
    if sky && plans.map_skybox.is_none() {
        draw::draw_sky_background(&mut pass, resources, upload, &view.sky.bind_group);
    }
    draw::draw_scene_plan_opaque(
        &mut pass,
        resources,
        upload,
        &plans.world,
        DrawView {
            uniforms: &view.world.bind_group,
            visibility: &view.visibility,
            reflected,
        },
        upload.detail_sprites.as_ref(),
    );
}

pub fn render_frame(
    primitive: &ModelPrimitive,
    frame: &PreparedFrame,
    resources: &RenderResources,
    upload: &UploadedModel,
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    clip_bounds: &Rectangle<u32>,
) {
    for reflection in &frame.reflections {
        render_opaque(
            &reflection.view,
            resources,
            upload,
            encoder,
            true,
            wgpu::Color::BLACK,
            true,
        );
        let view = &reflection.view;
        let targets = view.targets.as_ref().unwrap();
        let mut pass = pass(
            encoder,
            targets,
            "file_preview.reflection.transparent",
            wgpu::LoadOp::Load,
            Some(depth_load()),
            true,
        );
        draw::configure_scene_pass(&mut pass, &local_bounds(targets));
        draw::draw_scene_plan_transparent(
            &mut pass,
            resources,
            upload,
            &view.plans.as_ref().unwrap().world,
            DrawView {
                uniforms: &view.world.bind_group,
                visibility: &view.visibility,
                reflected: true,
            },
            None,
        );
    }
    let clear = if primitive.submerged {
        wgpu::Color {
            r: f64::from(primitive.uniforms.fog_color[0]),
            g: f64::from(primitive.uniforms.fog_color[1]),
            b: f64::from(primitive.uniforms.fog_color[2]),
            a: 1.0,
        }
    } else {
        wgpu::Color::TRANSPARENT
    };
    render_opaque(
        &frame.main,
        resources,
        upload,
        encoder,
        false,
        clear,
        !primitive.submerged,
    );
    let view = &frame.main;
    let targets = view.targets.as_ref().unwrap();
    let plans = view.plans.as_ref().unwrap();
    assert_eq!(plans.content_id, primitive.content_id);
    if let Some(refraction) = targets.refraction.as_ref() {
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &targets.resolve_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &refraction.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: targets.size.0,
                height: targets.size.1,
                depth_or_array_layers: 1,
            },
        );
    }
    {
        let mut pass = pass(
            encoder,
            targets,
            "file_preview.main.surfaces_transparent",
            wgpu::LoadOp::Load,
            if targets.refraction.is_some() {
                None
            } else {
                Some(depth_load())
            },
            true,
        );
        draw::configure_scene_pass(&mut pass, &local_bounds(targets));
        let mut remaining = plans.world.clone();
        if let Some(pipeline) = resources.refractive_water_pipeline.as_ref() {
            for reflection in &frame.reflections {
                let surface = &upload.reflectors[reflection.surface_index];
                pass.set_pipeline(pipeline);
                pass.set_bind_group(
                    0,
                    reflection
                        .water_bind_group
                        .as_ref()
                        .expect("prepared plane inputs"),
                    &[],
                );
                pass.set_bind_group(1, &upload.material_bind_groups[surface.material_slot], &[]);
                pass.set_vertex_buffer(0, upload.meshes[surface.mesh_index].vertices.slice(..));
                pass.set_index_buffer(
                    reflection
                        .surface_buffer
                        .buffer
                        .as_ref()
                        .expect("visible plane indices")
                        .slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                pass.draw_indexed(0..reflection.surface_buffer.index_count, 0, 0..1);
                remaining
                    .planar
                    .retain(|item| item.mesh_index != surface.mesh_index);
            }
        }
        pass.set_bind_group(0, &view.world.bind_group, &[]);
        draw::draw_scene_plan_transparent(
            &mut pass,
            resources,
            upload,
            &remaining,
            DrawView {
                uniforms: &view.world.bind_group,
                visibility: &view.visibility,
                reflected: false,
            },
            targets
                .refraction
                .as_ref()
                .map(|refraction| &refraction.bind_group),
        );
    }
    if primitive.phy_debug_visible {
        let mut pass = pass(
            encoder,
            targets,
            "file_preview.main.physics_debug",
            wgpu::LoadOp::Load,
            Some(depth_load()),
            true,
        );
        draw::configure_scene_pass(&mut pass, &local_bounds(targets));
        pass.set_bind_group(0, &view.world.bind_group, &[]);
        draw::draw_phy_debug_meshes(&mut pass, resources, upload);
    }
    #[cfg(feature = "map-capture")]
    let targets = if primitive.reflection_target {
        frame
            .reflections
            .first()
            .and_then(|view| view.view.targets.as_ref())
            .expect("reflection diagnostic requires a visible reflector")
    } else {
        targets
    };
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("file_preview.view.compose"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    });
    pass.set_viewport(
        frame.bounds.x,
        frame.bounds.y,
        frame.bounds.width,
        frame.bounds.height,
        0.0,
        1.0,
    );
    pass.set_scissor_rect(
        clip_bounds.x,
        clip_bounds.y,
        clip_bounds.width,
        clip_bounds.height,
    );
    pass.set_pipeline(&resources.blit_pipeline);
    pass.set_bind_group(0, &targets.blit_bind_group, &[]);
    pass.draw(0..6, 0..1);
}
