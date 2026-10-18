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
            true,
        );
        draw::configure_scene_pass(&mut pass, &bounds);
        if sky {
            draw::draw_sky_background(&mut pass, resources, upload, &view.sky.bind_group);
        }
        drop(pass);
        let draw_view = DrawView {
            uniforms: &view.skybox.bind_group,
            visibility: &view.skybox_visibility,
            skybox: true,
            transparency: &view.skybox_transparency,
            indirect: view.transparent_indirect.skybox.program(),
        };
        render_world_batches(
            encoder,
            targets,
            resources,
            upload,
            plan,
            draw_view,
            super::transparency::WorldDrawGroups::ALL,
        );
        render_opaque_items(encoder, targets, resources, upload, plan, draw_view);
        render_marks(
            encoder,
            targets,
            resources,
            upload,
            &plan.surface_marks,
            draw_view,
        );
        render_opaque_brushes(encoder, targets, resources, upload, plan, draw_view);
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
    drop(pass);
    render_world_opaque(view, resources, upload, encoder, targets);
}

fn render_world_opaque(
    view: &SceneView,
    resources: &super::resources::ViewPipelines,
    upload: &ViewerScene,
    encoder: &mut wgpu::CommandEncoder,
    targets: &RenderTargets,
) {
    let plans = view.plans.as_ref().expect("prepared view plans");
    let draw_view = DrawView {
        uniforms: &view.world.bind_group,
        visibility: &view.visibility,
        skybox: false,
        transparency: &view.transparency,
        indirect: view.transparent_indirect.world.program(),
    };
    render_world_batches(
        encoder,
        targets,
        resources,
        upload,
        &plans.world,
        draw_view,
        view.world_groups,
    );
    render_opaque_items(encoder, targets, resources, upload, &plans.world, draw_view);
    render_marks(
        encoder,
        targets,
        resources,
        upload,
        &plans.world.surface_marks,
        draw_view,
    );
    render_opaque_brushes(encoder, targets, resources, upload, &plans.world, draw_view);
}

fn render_opaque_items(
    encoder: &mut wgpu::CommandEncoder,
    targets: &RenderTargets,
    resources: &super::resources::ViewPipelines,
    upload: &ViewerScene,
    plan: &super::DrawPlan,
    view: DrawView<'_>,
) {
    if plan.opaque.is_empty() {
        return;
    }
    let mut pass = pass(
        encoder,
        targets,
        "file_preview.view.opaque_items",
        wgpu::LoadOp::Load,
        Some(depth_load()),
        true,
    );
    draw::configure_scene_pass(&mut pass, &local_bounds(targets));
    draw::draw_scene_plan_opaque(&mut pass, resources, upload, plan, view);
}

fn render_marks(
    encoder: &mut wgpu::CommandEncoder,
    targets: &RenderTargets,
    resources: &super::resources::ViewPipelines,
    upload: &ViewerScene,
    marks: &[super::OverlayDrawItem],
    view: DrawView<'_>,
) {
    let mut start = 0;
    while start < marks.len() {
        let transfer = SurfaceBlend::from(marks[start].blend).color_transfer();
        let end = (start + 1..marks.len())
            .find(|&index| SurfaceBlend::from(marks[index].blend).color_transfer() != transfer)
            .unwrap_or(marks.len());
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
        for &item in &marks[start..end] {
            draw::draw_surface_mark(&mut pass, resources, upload, item);
        }
        drop(pass);
        start = end;
    }
}

fn render_opaque_brushes(
    encoder: &mut wgpu::CommandEncoder,
    targets: &RenderTargets,
    resources: &super::resources::ViewPipelines,
    upload: &ViewerScene,
    plan: &super::DrawPlan,
    view: DrawView<'_>,
) {
    render_surface_batches(
        encoder,
        targets,
        resources,
        upload,
        plan.brushes.iter().map(|brush| &brush.opaque),
        view,
    );
}

fn render_world_batches(
    encoder: &mut wgpu::CommandEncoder,
    targets: &RenderTargets,
    resources: &super::resources::ViewPipelines,
    upload: &ViewerScene,
    plan: &super::DrawPlan,
    view: DrawView<'_>,
    groups: super::transparency::WorldDrawGroups,
) {
    render_surface_batches(
        encoder,
        targets,
        resources,
        upload,
        plan.world_opaque
            .batches
            .iter()
            .filter(|batch| groups.allows(batch.material_group))
            .map(|batch| &batch.surfaces),
        view,
    );
}

fn render_surface_batches<'a>(
    encoder: &mut wgpu::CommandEncoder,
    targets: &RenderTargets,
    resources: &super::resources::ViewPipelines,
    upload: &ViewerScene,
    batches: impl IntoIterator<Item = &'a super::super::draw_plan::SurfaceBatch>,
    view: DrawView<'_>,
) {
    let linear_marks = |batch: &super::super::draw_plan::SurfaceBatch| {
        batch
            .surface_marks
            .iter()
            .all(|mark| SurfaceBlend::from(mark.blend).color_transfer() == ColorTransfer::Linear)
    };
    let mut batches = batches
        .into_iter()
        .filter(|batch| !batch.meshes.is_empty() || !batch.surface_marks.is_empty())
        .peekable();
    while let Some(mut batch) = batches.next() {
        if !linear_marks(batch) {
            render_opaque_surfaces(encoder, targets, resources, upload, batch, view);
            continue;
        }
        // Material and lightmap inputs are immutable across this ordered MSAA run.
        let mut pass = pass(
            encoder,
            targets,
            "file_preview.surface.opaque_run",
            wgpu::LoadOp::Load,
            Some(depth_load()),
            true,
        );
        draw::configure_scene_pass(&mut pass, &local_bounds(targets));
        loop {
            if !batch.meshes.is_empty() {
                draw::draw_opaque_surfaces(&mut pass, resources, upload, batch, view);
            }
            if !batch.surface_marks.is_empty() {
                pass.set_bind_group(0, view.uniforms, &[]);
                for &item in &batch.surface_marks {
                    draw::draw_surface_mark(&mut pass, resources, upload, item);
                }
            }
            if !batches.peek().is_some_and(|next| linear_marks(next)) {
                break;
            }
            batch = batches.next().expect("peeked consecutive linear batch");
        }
    }
}

fn render_opaque_surfaces(
    encoder: &mut wgpu::CommandEncoder,
    targets: &RenderTargets,
    resources: &super::resources::ViewPipelines,
    upload: &ViewerScene,
    batch: &super::super::draw_plan::SurfaceBatch,
    view: DrawView<'_>,
) {
    if !batch.meshes.is_empty() {
        let mut pass = pass(
            encoder,
            targets,
            "file_preview.surface.opaque",
            wgpu::LoadOp::Load,
            Some(depth_load()),
            true,
        );
        draw::configure_scene_pass(&mut pass, &local_bounds(targets));
        draw::draw_opaque_surfaces(&mut pass, resources, upload, batch, view);
    }
    render_marks(
        encoder,
        targets,
        resources,
        upload,
        &batch.surface_marks,
        view,
    );
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
        let glass_material = |draw: &super::transparency::Draw| match &draw.primitive {
            super::transparency::Primitive::Mesh(item, _) => {
                upload.assets.source.render_scene().materials[item.material_slot]
                    .shader
                    .surface()
                    .filter(|s| s.is_refractive())
                    .map(|_| item.material_slot)
            }
            _ => None,
        };
        let glass = glass_material(&view.transparency[start]);
        let transfer = SurfaceBlend::from(view.transparency[start].blend).color_transfer();
        let end = (start + 1..view.transparency.len())
            .find(|&index| {
                SurfaceBlend::from(view.transparency[index].blend).color_transfer() != transfer
                    || glass_material(&view.transparency[index]) != glass
            })
            .unwrap_or(view.transparency.len());
        let snapshot = glass.map(|_| {
            let snapshot = targets
                .glass
                .get_or_init(|| super::glass::Snapshot::new(resources, targets));
            snapshot.copy(encoder, targets);
            snapshot
        });
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
                indirect: view
                    .indirect
                    .and_then(|program| program.segment(start..end)),
                ..view
            },
            snapshot.map(|s| &s.bind_group),
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
            indirect: view.transparent_indirect.world.program(),
        },
    );
    if matches!(kind, ViewKind::Reflection) && sky {
        view.sun.query(encoder, sun, false);
        view.sun.draw(encoder, sun, upload, targets);
    }
}

#[derive(Clone, Copy)]
pub(super) struct Composition<'a> {
    pub blit: &'a wgpu::BindGroup,
    pub size: (u32, u32),
    pub overlay: Option<&'a super::screen_refraction::Frame>,
}

pub fn render_frame<'a>(
    primitive: &ModelPrimitive,
    frame: &'a PreparedFrame,
    stage: Option<&PreparedFrame>,
    resources: &RenderResources,
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    clip_bounds: &Rectangle<u32>,
) -> Composition<'a> {
    let upload = &frame.scene;
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
    } else if primitive.studio_grid.is_some() {
        super::studio_grid::BACKGROUND
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
    if let Some(grid) = &frame.studio_grid {
        let mut pass = pass(
            encoder,
            targets,
            "file_preview.studio_grid",
            wgpu::LoadOp::Load,
            Some(depth_load()),
            true,
        );
        draw::configure_scene_pass(&mut pass, &local_bounds(targets));
        grid.draw(&mut pass);
    }
    if let Some(stage) = stage {
        render_world_opaque(
            &stage.main,
            resources.for_view(ViewKind::Main),
            &stage.scene,
            encoder,
            targets,
        );
    }
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
            for optical in &frame.planar_views {
                let surface = &upload.assets.reflectors[optical.surface_index];
                if !primitive.submerged
                    || optical.refraction.is_none()
                    || !surface.plane.intersects_near_plane(&primitive.uniforms)
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
                    upload.bindings.materials.water(surface.material_slot),
                    &[],
                );
                pass.set_vertex_buffer(0, upload.meshes[surface.mesh_index].vertices().slice(..));
                if let Some(buffer) = optical.surface_buffer.buffer.as_ref() {
                    pass.set_index_buffer(buffer.slice(..), wgpu::IndexFormat::Uint32);
                    draw::draw_lightmap_ranges(
                        &mut pass,
                        &upload.lightmaps,
                        &optical.surface_buffer.lightmaps,
                    );
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
                    upload.bindings.materials.water(surface.material_slot),
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
                draw::draw_lightmap_ranges(
                    &mut pass,
                    &upload.lightmaps,
                    &reflection.surface_buffer.lightmaps,
                );
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
            indirect: view.transparent_indirect.world.program(),
        },
    );
    if let Some(stage) = stage {
        render_transparent(
            encoder,
            targets,
            resources.for_view(ViewKind::Main),
            &stage.scene,
            &stage.main.plans.as_ref().unwrap().world.planar,
            DrawView {
                uniforms: &stage.main.world.bind_group,
                visibility: &stage.main.visibility,
                skybox: false,
                transparency: &stage.main.transparency,
                indirect: stage.main.transparent_indirect.world.program(),
            },
        );
        if primitive
            .stage
            .as_ref()
            .is_some_and(|stage| stage.phy_debug_visible)
        {
            let mut pass = pass(
                encoder,
                targets,
                "file_preview.stage.physics_debug",
                wgpu::LoadOp::Load,
                Some(depth_load()),
                true,
            );
            draw::configure_scene_pass(&mut pass, &local_bounds(targets));
            pass.set_bind_group(0, &stage.main.world.bind_group, &[]);
            draw::draw_phy_debug_meshes(&mut pass, resources, &stage.scene);
        }
    }
    if let Some(intersection) = frame
        .water_intersection
        .as_ref()
        .filter(|view| view.selection.is_some())
    {
        let intersection = &intersection.view;
        let resources = resources.for_view(ViewKind::Main);
        render_world_opaque(intersection, resources, upload, encoder, targets);
        render_transparent(
            encoder,
            targets,
            resources,
            upload,
            &intersection.plans.as_ref().unwrap().world.planar,
            DrawView {
                uniforms: &intersection.world.bind_group,
                visibility: &intersection.visibility,
                skybox: false,
                transparency: &intersection.transparency,
                indirect: intersection.transparent_indirect.world.program(),
            },
        );
    }
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

    let overlay = frame.screen_overlay.as_ref();
    let composition = Composition {
        blit: &targets.blit_bind_group,
        size: targets.size,
        overlay,
    };
    compose(
        composition,
        frame.bounds,
        resources,
        encoder,
        target,
        clip_bounds,
    );
    composition
}

pub(super) fn compose(
    composition: Composition<'_>,
    bounds: Rectangle,
    resources: &RenderResources,
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    clip_bounds: &Rectangle<u32>,
) {
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
    pass.set_viewport(bounds.x, bounds.y, bounds.width, bounds.height, 0.0, 1.0);
    pass.set_scissor_rect(
        clip_bounds.x,
        clip_bounds.y,
        clip_bounds.width,
        clip_bounds.height,
    );
    if let Some(overlay) = composition.overlay {
        pass.set_pipeline(&resources.screen_refraction.pipeline);
        pass.set_bind_group(1, &overlay.bind_group, &[]);
    } else {
        pass.set_pipeline(&resources.blit_pipeline);
    }
    pass.set_bind_group(0, composition.blit, &[]);
    pass.draw(0..6, 0..1);
}
