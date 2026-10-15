use super::draw::{self, DrawView, ViewKind};
use super::reflections::SceneView;
use super::surface_pipelines::SurfaceBlend;
use super::targets::{ColorTransfer, RenderTargets};
use super::{ModelPrimitive, PreparedFrame, Rectangle, RenderResources, ViewerScene, wgpu};

fn pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    targets: &RenderTargets,
    label: &'static str,
    load: wgpu::LoadOp<wgpu::Color>,
    depth: Option<wgpu::Operations<f32>>,
    resolve: bool,
) -> wgpu::RenderPass<'a> {
    pass_transfer(
        encoder,
        targets,
        label,
        load,
        depth,
        resolve,
        ColorTransfer::Linear,
    )
}

fn pass_transfer<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    targets: &RenderTargets,
    label: &'static str,
    load: wgpu::LoadOp<wgpu::Color>,
    depth: Option<wgpu::Operations<f32>>,
    resolve: bool,
    transfer: ColorTransfer,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: targets.color_attachment(transfer),
            resolve_target: if resolve {
                targets.resolve_attachment(transfer)
            } else {
                None
            },
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
    upload: &ViewerScene,
    encoder: &mut wgpu::CommandEncoder,
    kind: ViewKind,
    clear: wgpu::Color,
    sky: bool,
) {
    let sun = match kind {
        ViewKind::Main => Some(&resources.sun),
        ViewKind::Reflection => Some(&resources.reflection_sun),
        ViewKind::Refraction => None,
    };
    let resources = resources.for_view(kind);
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
        draw::draw_scene_plan_opaque(
            &mut pass,
            resources,
            upload,
            plan,
            DrawView {
                uniforms: &view.skybox.bind_group,
                visibility: &view.skybox_visibility,
                skybox: true,
                transparency: &view.skybox_transparency,
            },
        );
        drop(pass);
        let draw_view = DrawView {
            uniforms: &view.skybox.bind_group,
            visibility: &view.skybox_visibility,
            skybox: true,
            transparency: &view.skybox_transparency,
        };
        render_marks(encoder, targets, resources, upload, plan, draw_view);
        render_transparent(encoder, targets, resources, upload, &plan.planar, draw_view);
        if sky && let Some(sun) = sun {
            view.sun.query(encoder, sun, true);
        }
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
            skybox: false,
            transparency: &view.transparency,
        },
    );
    drop(pass);
    render_marks(
        encoder,
        targets,
        resources,
        upload,
        &plans.world,
        DrawView {
            uniforms: &view.world.bind_group,
            visibility: &view.visibility,
            skybox: false,
            transparency: &view.transparency,
        },
    );
}

fn render_marks(
    encoder: &mut wgpu::CommandEncoder,
    targets: &RenderTargets,
    resources: &super::resources::ViewPipelines,
    upload: &ViewerScene,
    plan: &super::DrawPlan,
    view: DrawView<'_>,
) {
    let mut start = 0;
    while start < plan.surface_marks.len() {
        let transfer = SurfaceBlend::from(plan.surface_marks[start].blend).color_transfer();
        let end = (start + 1..plan.surface_marks.len())
            .find(|&index| {
                SurfaceBlend::from(plan.surface_marks[index].blend).color_transfer() != transfer
            })
            .unwrap_or(plan.surface_marks.len());
        let mut pass = pass_transfer(
            encoder,
            targets,
            "file_preview.surface_marks",
            wgpu::LoadOp::Load,
            Some(depth_load()),
            true,
            transfer,
        );
        draw::configure_scene_pass(&mut pass, &local_bounds(targets));
        pass.set_bind_group(0, view.uniforms, &[]);
        for &item in &plan.surface_marks[start..end] {
            draw::draw_surface_mark(&mut pass, resources, upload, item);
        }
        drop(pass);
        start = end;
    }
}

fn render_transparent(
    encoder: &mut wgpu::CommandEncoder,
    targets: &RenderTargets,
    resources: &super::resources::ViewPipelines,
    upload: &ViewerScene,
    planar: &[super::DrawItem],
    view: DrawView<'_>,
) {
    if !planar.is_empty() {
        let mut pass = pass(
            encoder,
            targets,
            "file_preview.planar_fallback",
            wgpu::LoadOp::Load,
            Some(depth_load()),
            true,
        );
        draw::configure_scene_pass(&mut pass, &local_bounds(targets));
        draw::draw_planar_items(
            &mut pass,
            resources,
            upload,
            planar,
            DrawView {
                transparency: &[],
                ..view
            },
        );
    }
    let mut start = 0;
    while start < view.transparency.len() {
        let transfer = SurfaceBlend::from(view.transparency[start].blend).color_transfer();
        let end = (start + 1..view.transparency.len())
            .find(|&index| {
                SurfaceBlend::from(view.transparency[index].blend).color_transfer() != transfer
            })
            .unwrap_or(view.transparency.len());
        let mut pass = pass_transfer(
            encoder,
            targets,
            "file_preview.transparent",
            wgpu::LoadOp::Load,
            Some(depth_load()),
            true,
            transfer,
        );
        draw::configure_scene_pass(&mut pass, &local_bounds(targets));
        draw::draw_operations(
            &mut pass,
            resources,
            upload,
            DrawView {
                transparency: &view.transparency[start..end],
                ..view
            },
        );
        drop(pass);
        start = end;
    }
}

fn render_scene(
    view: &SceneView,
    resources: &RenderResources,
    upload: &ViewerScene,
    encoder: &mut wgpu::CommandEncoder,
    kind: ViewKind,
    clear: wgpu::Color,
    sky: bool,
) {
    render_opaque(view, resources, upload, encoder, kind, clear, sky);
    let sun = &resources.reflection_sun;
    let resources = resources.for_view(kind);
    let targets = view.targets.as_ref().expect("prepared optical targets");
    render_transparent(
        encoder,
        targets,
        resources,
        upload,
        &view.plans.as_ref().unwrap().world.planar,
        DrawView {
            uniforms: &view.world.bind_group,
            visibility: &view.visibility,
            skybox: false,
            transparency: &view.transparency,
        },
    );
    if matches!(kind, ViewKind::Reflection) && sky {
        view.sun.query(encoder, sun, false);
        view.sun.draw(encoder, sun, upload, targets);
    }
}

pub fn render_frame(
    primitive: &ModelPrimitive,
    frame: &PreparedFrame,
    resources: &RenderResources,
    upload: &ViewerScene,
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    clip_bounds: &Rectangle<u32>,
) {
    for surface_view in &frame.planar_views {
        if let Some(view) = surface_view.reflection.as_ref() {
            render_scene(
                view,
                resources,
                upload,
                encoder,
                ViewKind::Reflection,
                wgpu::Color::BLACK,
                true,
            );
        }
        if let Some(view) = surface_view.refraction.as_ref() {
            let material = upload.assets.reflectors[surface_view.surface_index].material_slot;
            let sky = primitive.preview.render_scene().materials[material]
                .shader
                .water()
                .is_none_or(|water| !water.above_water);
            render_scene(
                view,
                resources,
                upload,
                encoder,
                ViewKind::Refraction,
                surface_view.refraction_clear,
                sky,
            );
        }
    }
    let clear = if primitive.submerged {
        let rgb = primitive.clear_color.unwrap_or([
            primitive.uniforms.fog_color[0],
            primitive.uniforms.fog_color[1],
            primitive.uniforms.fog_color[2],
        ]);
        wgpu::Color {
            r: f64::from(rgb[0]),
            g: f64::from(rgb[1]),
            b: f64::from(rgb[2]),
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
        ViewKind::Main,
        clear,
        !primitive.submerged,
    );
    let view = &frame.main;
    let targets = view.targets.as_ref().unwrap();
    let plans = view.plans.as_ref().unwrap();
    assert_eq!(plans.content_id, primitive.content_id);
    {
        let mut pass = pass(
            encoder,
            targets,
            "file_preview.main.surfaces_transparent",
            wgpu::LoadOp::Load,
            Some(depth_load()),
            true,
        );
        draw::configure_scene_pass(&mut pass, &local_bounds(targets));
        if let Some(pipeline) = resources.refractive_water_pipeline.as_ref() {
            // Near-plane water composes the opposite medium at near depth.
            for optical in &frame.planar_views {
                let surface = &upload.assets.reflectors[optical.surface_index];
                if optical.refraction.is_none()
                    || !surface.plane.intersects_near_plane(primitive.uniforms)
                    || primitive.preview.render_scene().materials[surface.material_slot]
                        .shader
                        .water()
                        .is_none()
                {
                    continue;
                }
                pass.set_pipeline(&resources.water_near_intersection_pipeline);
                pass.set_bind_group(
                    0,
                    &optical.water_bind_group.as_ref().unwrap().bind_group,
                    &[],
                );
                pass.set_bind_group(
                    1,
                    &upload.assets.material_bind_groups[surface.material_slot],
                    &[],
                );
                pass.set_vertex_buffer(0, upload.meshes[surface.mesh_index].vertices().slice(..));
                if let Some(buffer) = optical.surface_buffer.buffer.as_ref() {
                    pass.set_index_buffer(buffer.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..optical.surface_buffer.index_count, 0, 0..1);
                }
            }
            for reflection in &frame.planar_views {
                let surface = &upload.assets.reflectors[reflection.surface_index];
                pass.set_pipeline(pipeline);
                pass.set_bind_group(
                    0,
                    &reflection
                        .water_bind_group
                        .as_ref()
                        .expect("prepared plane inputs")
                        .bind_group,
                    &[],
                );
                pass.set_bind_group(
                    1,
                    &upload.assets.material_bind_groups[surface.material_slot],
                    &[],
                );
                pass.set_vertex_buffer(0, upload.meshes[surface.mesh_index].vertices().slice(..));
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
            }
        }
        pass.set_bind_group(0, &view.world.bind_group, &[]);
        pass.set_pipeline(&resources.for_view(ViewKind::Main).planar_pipeline);
        for item in &plans.world.planar {
            let rendered = resources.refractive_water_pipeline.is_some()
                && frame.planar_views.iter().any(|view| {
                    upload.assets.reflectors[view.surface_index].mesh_index == item.mesh_index
                });
            if !rendered {
                draw::draw_model_item(&mut pass, upload, *item, &view.visibility);
            }
        }
    }
    render_transparent(
        encoder,
        targets,
        resources.for_view(ViewKind::Main),
        upload,
        &[],
        DrawView {
            uniforms: &view.world.bind_group,
            visibility: &view.visibility,
            skybox: false,
            transparency: &view.transparency,
        },
    );
    if !primitive.submerged {
        view.sun.query(encoder, &resources.sun, false);
        view.sun.draw(encoder, &resources.sun, upload, targets);
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
    let targets = if primitive.refraction_target {
        frame
            .planar_views
            .iter()
            .find_map(|surface| surface.refraction.as_ref())
            .and_then(|view| view.targets.as_ref())
            .expect("refraction diagnostic requires a visible refractive surface")
    } else if primitive.reflection_target {
        frame
            .planar_views
            .iter()
            .find_map(|surface| surface.reflection.as_ref())
            .and_then(|view| view.targets.as_ref())
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
    let overlay = frame.screen_overlay.as_ref();
    #[cfg(feature = "map-capture")]
    let overlay = if primitive.reflection_target || primitive.refraction_target {
        None
    } else {
        overlay
    };
    if let Some(overlay) = overlay {
        pass.set_pipeline(&resources.screen_refraction.pipeline);
        pass.set_bind_group(1, &overlay.bind_group, &[]);
    } else {
        pass.set_pipeline(&resources.blit_pipeline);
    }
    pass.set_bind_group(0, &targets.blit_bind_group, &[]);
    pass.draw(0..6, 0..1);
}
