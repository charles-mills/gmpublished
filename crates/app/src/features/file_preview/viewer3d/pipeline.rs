use super::{
    AMBIENT, Arc, BLIT_SHADER_SOURCE, BcFormat, CHECKERBOARD_BYTES, CHECKERBOARD_MIP_1X1_BYTES,
    CHECKERBOARD_MIP_2X2_BYTES, CHECKERBOARD_MIP_4X4_BYTES, CHECKERBOARD_MIP_RGBA,
    CHECKERBOARD_SIZE, CHECKERBOARD_SIZE_USIZE, Camera, DETAIL_SHADER_SOURCE,
    DETAIL_VERTEX_ATTRIBUTES, DETAIL_VERTEX_FLOAT_COUNT, DetailSprite, DoorInstance,
    DoorRenderPose, DrawItem, DrawPlan, DrawPlans, FOV_Y, FlyCamera, MATERIAL_ANISOTROPY_CLAMP,
    MODEL_VERTEX_ATTRIBUTES, MODEL_VERTEX_FLOAT_COUNT, MSAA_SAMPLE_COUNT, MapFog, MapSkyCamera,
    MaterialSlot, MeshData, ModelPreview, ModelVertex, OverlayDrawItem, OverlayPrimitive,
    PHY_DEBUG_MATERIAL_NAME, PHY_DEBUG_RGBA, Rectangle, RenderMode, ResolvedBcMip, ResolvedTexture,
    SHADER_SOURCE, SKY_SHADER_SOURCE, SKYBOX_FACE_COUNT, Skybox, SkyboxFace, TextureUploadLevel,
    Viewport, WATER_SHADER_SOURCE, WorldVisibilityPlan, add, bc_mip_is_valid, bc_texture_format,
    decode_bc_texture, half_extent, initial_door_open_sign, look_at, mat_mul, mid, perspective,
    prepare_draw_plans, shader, skybox_eye, transform_door_vertices, wgpu, write_bc_texture_level,
    write_texture_level,
};
use std::collections::HashMap;

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct Uniforms {
    pub(super) view_proj: [[f32; 4]; 4],
    pub(super) light: [f32; 4],
    pub(super) camera_position: [f32; 4],
    pub(super) fog_color: [f32; 4],
    pub(super) fog_params: [f32; 4],
    pub(super) water_time_sky_tint: [f32; 4],
    pub(super) water_depth_params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MaterialUniform {
    pub(super) flags: [u32; 4],
}

impl MaterialUniform {
    pub(super) const fn new(force_opaque: bool, render_mode: RenderMode) -> Self {
        Self {
            flags: [
                (force_opaque && matches!(render_mode, RenderMode::Opaque)) as u32,
                matches!(render_mode, RenderMode::Cutout) as u32,
                0,
                0,
            ],
        }
    }
}

impl Uniforms {
    pub(super) fn for_fly(
        scene: &ModelPreview,
        camera: &FlyCamera,
        bounds: Rectangle,
        fog: Option<MapFog>,
        water_time: f32,
        submerged: bool,
    ) -> Self {
        let frame = FlyCameraFrame::new(scene, camera, bounds);
        let target = add(frame.eye, frame.forward);
        let view = look_at(frame.eye, target, [0.0, 0.0, 1.0]);
        let (fog_color, fog_params) = if submerged {
            let color = scene_water_fog_color(scene);
            ([color[0], color[1], color[2], 1.0], [0.0, 2048.0, 1.0, 1.0])
        } else {
            (
                fog.map_or([0.0; 4], |fog| {
                    [
                        fog.color_linear[0],
                        fog.color_linear[1],
                        fog.color_linear[2],
                        0.0,
                    ]
                }),
                fog.map_or([0.0, 1.0, 0.0, 0.0], |fog| {
                    [fog.start, fog.end, fog.max_density, 1.0]
                }),
            )
        };

        Self {
            view_proj: mat_mul(frame.proj, view),
            light: [0.4, 0.6, 0.8, AMBIENT],
            camera_position: [frame.eye[0], frame.eye[1], frame.eye[2], 0.0],
            fog_color,
            fog_params,
            water_time_sky_tint: [water_time, 0.0, 0.0, 0.0],
            water_depth_params: [frame.near, frame.far, 0.0, 0.0],
        }
    }

    pub(super) fn for_fly_sky(scene: &ModelPreview, camera: &FlyCamera, bounds: Rectangle) -> Self {
        let frame = FlyCameraFrame::new(scene, camera, bounds);
        let target = add(frame.eye, frame.forward);
        let mut view = look_at(frame.eye, target, [0.0, 0.0, 1.0]);
        view[3][0] = 0.0;
        view[3][1] = 0.0;
        view[3][2] = 0.0;

        Self {
            view_proj: mat_mul(frame.proj, view),
            light: [0.0; 4],
            camera_position: [frame.eye[0], frame.eye[1], frame.eye[2], 0.0],
            fog_color: [0.0; 4],
            fog_params: [0.0, 1.0, 0.0, 0.0],
            water_time_sky_tint: [0.0; 4],
            water_depth_params: [frame.near, frame.far, 0.0, 0.0],
        }
    }

    pub(super) fn for_fly_skybox_composite(
        scene: &ModelPreview,
        camera: &FlyCamera,
        bounds: Rectangle,
        sky_camera: MapSkyCamera,
        fog: Option<MapFog>,
    ) -> Self {
        let frame = FlyCameraFrame::new(scene, camera, bounds);
        let eye = skybox_eye(frame.eye, sky_camera.origin, sky_camera.scale);
        let view = look_at(eye, add(eye, frame.forward), [0.0, 0.0, 1.0]);

        Self {
            view_proj: mat_mul(frame.proj, view),
            light: [0.4, 0.6, 0.8, AMBIENT],
            camera_position: [eye[0], eye[1], eye[2], 0.0],
            fog_color: fog.map_or([0.0; 4], |fog| {
                [
                    fog.color_linear[0],
                    fog.color_linear[1],
                    fog.color_linear[2],
                    0.0,
                ]
            }),
            fog_params: fog.map_or([0.0, 1.0, 0.0, 0.0], |fog| {
                [fog.start, fog.end, fog.max_density, 1.0]
            }),
            water_time_sky_tint: [0.0; 4],
            water_depth_params: [frame.near, frame.far, 0.0, 0.0],
        }
    }

    pub(super) fn for_model(model: &ModelPreview, camera: &Camera, bounds: Rectangle) -> Self {
        let center = mid(model.bounds_min, model.bounds_max);
        let radius = half_extent(model.bounds_min, model.bounds_max).max(1.0);
        let distance = radius * 2.2 * camera.distance;

        let eye = [
            center[0] + distance * camera.pitch.cos() * camera.yaw.sin(),
            center[1] + distance * camera.pitch.cos() * camera.yaw.cos(),
            center[2] + distance * camera.pitch.sin(),
        ];
        // Source models are Z-up.
        let view = look_at(eye, center, [0.0, 0.0, 1.0]);
        let aspect = (bounds.width / bounds.height.max(1.0)).max(0.1);
        let proj = perspective(FOV_Y, aspect, radius * 0.01, radius * 20.0 + distance);

        Self {
            view_proj: mat_mul(proj, view),
            light: [0.4, 0.6, 0.8, AMBIENT],
            camera_position: [eye[0], eye[1], eye[2], 0.0],
            fog_color: [0.0; 4],
            fog_params: [0.0, 1.0, 0.0, 0.0],
            water_time_sky_tint: [0.0; 4],
            water_depth_params: [0.0; 4],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct FlyCameraFrame {
    pub(super) eye: [f32; 3],
    pub(super) forward: [f32; 3],
    pub(super) proj: [[f32; 4]; 4],
    pub(super) near: f32,
    pub(super) far: f32,
}

impl FlyCameraFrame {
    pub(super) fn new(scene: &ModelPreview, camera: &FlyCamera, bounds: Rectangle) -> Self {
        let radius = half_extent(scene.bounds_min, scene.bounds_max).max(1.0);
        let eye = camera.position.map_or_else(
            || mid(scene.bounds_min, scene.bounds_max),
            |position| {
                add(
                    position,
                    [
                        0.0,
                        0.0,
                        camera.view_bob_offset() + camera.duck_view_offset(),
                    ],
                )
            },
        );
        let aspect = (bounds.width / bounds.height.max(1.0)).max(0.1);
        let near = 4.0;
        let far = (radius * 6.0).max(30_000.0);
        Self {
            eye,
            forward: camera.forward(),
            proj: perspective(FOV_Y, aspect, near, far),
            near,
            far,
        }
    }
}

const DEFAULT_WATER_FOG_COLOR: [f32; 3] = [0.03, 0.10, 0.10];
const DEFAULT_SKY_TINT: [f32; 3] = [0.12, 0.18, 0.24];

fn scene_water_fog_color(scene: &ModelPreview) -> [f32; 3] {
    scene
        .materials
        .iter()
        .filter_map(|slot| slot.texture.as_deref())
        .find(|texture| texture.is_water_fallback())
        .and_then(texture_smallest_mip_average)
        .unwrap_or(DEFAULT_WATER_FOG_COLOR)
}

fn scene_sky_tint(skybox: Option<&Skybox>) -> [f32; 3] {
    let Some(skybox) = skybox else {
        return DEFAULT_SKY_TINT;
    };
    let mut sum = [0.0; 3];
    let mut count = 0_u32;
    for color in skybox
        .faces
        .iter()
        .filter_map(Option::as_deref)
        .filter_map(texture_smallest_mip_average)
    {
        for channel in 0..3 {
            sum[channel] += color[channel];
        }
        count += 1;
    }
    if count == 0 {
        DEFAULT_SKY_TINT
    } else {
        let scale = 1.0 / count as f32;
        sum.map(|channel| channel * scale)
    }
}

fn texture_smallest_mip_average(texture: &ResolvedTexture) -> Option<[f32; 3]> {
    if let Some((format, mips)) = texture.bc_payload() {
        let mip = mips.last()?;
        let rgba = decode_bc_texture(format, mip.width, mip.height, &mip.data)?;
        return average_srgb_rgba(&rgba, mip.width, mip.height);
    }
    let mip = texture.mip_chain().last()?;
    average_srgb_rgba(mip.rgba, mip.width, mip.height)
}

pub(super) fn average_srgb_rgba(rgba: &[u8], width: u32, height: u32) -> Option<[f32; 3]> {
    let pixel_count = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?;
    if pixel_count == 0 || rgba.len() < pixel_count.checked_mul(4)? {
        return None;
    }
    let mut sum = [0.0; 3];
    for pixel in rgba.chunks_exact(4).take(pixel_count) {
        for channel in 0..3 {
            sum[channel] += srgb_channel_to_linear(pixel[channel]);
        }
    }
    let scale = 1.0 / pixel_count as f32;
    Some(sum.map(|channel| channel * scale))
}

fn srgb_channel_to_linear(channel: u8) -> f32 {
    let value = f32::from(channel) / 255.0;
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// One frame's draw of a loaded model; heavy data is uploaded once per
/// `content_id` and cached in the shared [`ModelPipeline`].
#[derive(Debug)]
pub struct ModelPrimitive {
    pub(super) model: Arc<ModelPreview>,
    pub(super) content_id: u64,
    pub(super) skin_remap: Vec<u16>,
    pub(super) bodygroup_choices: Vec<usize>,
    pub(super) map_skybox_visible: bool,
    pub(super) visibility_culling: bool,
    pub(super) phy_debug_visible: bool,
    pub(super) uniforms: Uniforms,
    pub(super) map_skybox_uniforms: Option<Uniforms>,
    pub(super) sky_uniforms: Option<Uniforms>,
    pub(super) door_poses: Vec<DoorRenderPose>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrameLayout {
    SinglePass,
    RefractiveWater,
}

impl FrameLayout {
    const fn uses_refraction(self) -> bool {
        matches!(self, Self::RefractiveWater)
    }
}

fn frame_layout(plan: Option<&DrawPlan>, refraction_supported: bool) -> FrameLayout {
    if refraction_supported && plan.is_some_and(|plan| !plan.water.is_empty()) {
        FrameLayout::RefractiveWater
    } else {
        FrameLayout::SinglePass
    }
}

/// Pops the top error scope off `device`. Native wgpu records scoped errors
/// synchronously, so the returned future is already resolved; a single poll
/// retrieves it without blocking.
fn take_scoped_error(device: &wgpu::Device) -> Option<wgpu::Error> {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    let future = std::pin::pin!(device.pop_error_scope());
    match future.poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(error) => error,
        Poll::Pending => None,
    }
}

impl shader::Primitive for ModelPrimitive {
    type Pipeline = ModelPipeline;

    fn prepare(
        &self,
        pipeline: &mut ModelPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let size = viewport.physical_size();
        pipeline.ensure_upload(device, queue, self.content_id, &self.model);
        pipeline.touch(self.content_id);
        if let Some(upload) = pipeline.uploads.get_mut(&self.content_id) {
            if self.phy_debug_visible {
                upload.ensure_phy_debug_meshes(device, queue, &self.model);
            }
            upload.update_door_vertices(queue, &self.model, self.door_poses.as_slice());
            upload.ensure_world_visibility(
                device,
                queue,
                &self.model,
                self.visibility_culling,
                [
                    self.uniforms.camera_position[0],
                    self.uniforms.camera_position[1],
                    self.uniforms.camera_position[2],
                ],
            );
            let draw_plans = prepare_draw_plans(
                self.content_id,
                upload,
                &self.skin_remap,
                &self.bodygroup_choices,
                self.uniforms.camera_position,
                self.map_skybox_visible,
                self.map_skybox_uniforms
                    .map(|uniforms| uniforms.camera_position),
            );
            let needs_refraction = frame_layout(
                Some(&draw_plans.world),
                pipeline.refractive_water_pipeline.is_some(),
            )
            .uses_refraction();
            pipeline.draw_plans = Some(draw_plans);
            pipeline.ensure_targets(device, size.width, size.height, needs_refraction);
        }
        let sky_tint = pipeline
            .uploads
            .get(&self.content_id)
            .map_or(DEFAULT_SKY_TINT, |upload| upload.sky_tint);
        let with_sky_tint = |mut uniforms: Uniforms| {
            uniforms.water_time_sky_tint[0] = self.uniforms.water_time_sky_tint[0];
            uniforms.water_time_sky_tint[1..].copy_from_slice(&sky_tint);
            if self.uniforms.fog_color[3] > 0.5 {
                uniforms.fog_color = self.uniforms.fog_color;
                uniforms.fog_params = self.uniforms.fog_params;
            }
            uniforms
        };
        let uniforms = with_sky_tint(self.uniforms);
        queue.write_buffer(&pipeline.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
        if let Some(sky_uniforms) = self.sky_uniforms.as_ref() {
            let sky_uniforms = with_sky_tint(*sky_uniforms);
            queue.write_buffer(
                &pipeline.sky_uniform_buffer,
                0,
                bytemuck::bytes_of(&sky_uniforms),
            );
        }
        if let Some(map_skybox_uniforms) = self.map_skybox_uniforms.as_ref() {
            let map_skybox_uniforms = with_sky_tint(*map_skybox_uniforms);
            queue.write_buffer(
                &pipeline.map_skybox_uniform_buffer,
                0,
                bytemuck::bytes_of(&map_skybox_uniforms),
            );
        }
    }

    fn render(
        &self,
        pipeline: &ModelPipeline,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        let Some(upload) = pipeline.uploads.get(&self.content_id) else {
            return;
        };
        let Some(targets) = pipeline.targets.as_ref() else {
            return;
        };

        let plans = pipeline
            .draw_plans
            .as_ref()
            .filter(|plans| plans.content_id == self.content_id);
        let has_skybox_composite = plans.and_then(|plans| plans.map_skybox.as_ref()).is_some();
        let submerged = self.uniforms.fog_color[3] > 0.5;
        let background_color = if submerged {
            wgpu::Color {
                r: f64::from(self.uniforms.fog_color[0]),
                g: f64::from(self.uniforms.fog_color[1]),
                b: f64::from(self.uniforms.fog_color[2]),
                a: 1.0,
            }
        } else {
            wgpu::Color::TRANSPARENT
        };

        if has_skybox_composite {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("file_preview.model_viewer.skybox_composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.color,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(background_color),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            configure_scene_pass(&mut pass, clip_bounds);
            if !submerged {
                draw_sky_background(&mut pass, pipeline, upload);
            }
            if let Some(plan) = plans.and_then(|plans| plans.map_skybox.as_ref()) {
                draw_scene_plan(
                    &mut pass,
                    pipeline,
                    upload,
                    plan,
                    &pipeline.map_skybox_uniform_bind_group,
                    upload.map_skybox_detail_sprites.as_ref(),
                );
            }
            drop(pass);
        }

        let world_plan = plans.map(|plans| &plans.world);
        let layout = frame_layout(world_plan, pipeline.refractive_water_pipeline.is_some());
        let world_load = if has_skybox_composite {
            wgpu::LoadOp::Load
        } else {
            wgpu::LoadOp::Clear(background_color)
        };

        if layout == FrameLayout::SinglePass {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("file_preview.model_viewer"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.color,
                    resolve_target: Some(&targets.resolve_view),
                    ops: wgpu::Operations {
                        load: world_load,
                        store: wgpu::StoreOp::Discard,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            configure_scene_pass(&mut pass, clip_bounds);
            if !has_skybox_composite && !submerged {
                draw_sky_background(&mut pass, pipeline, upload);
            }
            if let Some(plan) = world_plan {
                draw_scene_plan(
                    &mut pass,
                    pipeline,
                    upload,
                    plan,
                    &pipeline.uniform_bind_group,
                    upload.detail_sprites.as_ref(),
                );
            }
            if self.phy_debug_visible {
                draw_phy_debug_meshes(&mut pass, pipeline, upload);
            }
            drop(pass);
        } else {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("file_preview.model_viewer.opaque"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.color,
                    resolve_target: Some(&targets.resolve_view),
                    ops: wgpu::Operations {
                        load: world_load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            configure_scene_pass(&mut pass, clip_bounds);
            if !has_skybox_composite && !submerged {
                draw_sky_background(&mut pass, pipeline, upload);
            }
            if let Some(plan) = world_plan {
                draw_scene_plan_opaque(
                    &mut pass,
                    pipeline,
                    upload,
                    plan,
                    &pipeline.uniform_bind_group,
                    upload.detail_sprites.as_ref(),
                );
            }
            if self.phy_debug_visible {
                draw_phy_debug_meshes(&mut pass, pipeline, upload);
            }
            drop(pass);

            let refraction = targets
                .refraction
                .as_ref()
                .expect("refraction targets exist for water frames");
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

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("file_preview.model_viewer.water_transparent"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.color,
                    resolve_target: Some(&targets.resolve_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Discard,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: None,
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            configure_scene_pass(&mut pass, clip_bounds);
            pass.set_bind_group(0, &pipeline.uniform_bind_group, &[]);
            if let Some(plan) = world_plan {
                draw_scene_plan_transparent(
                    &mut pass,
                    pipeline,
                    upload,
                    plan,
                    Some(&refraction.bind_group),
                );
            }
            drop(pass);
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("file_preview.model_viewer.blit"),
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
            0.0,
            0.0,
            targets.size.0 as f32,
            targets.size.1 as f32,
            0.0,
            1.0,
        );
        pass.set_scissor_rect(
            clip_bounds.x,
            clip_bounds.y,
            clip_bounds.width,
            clip_bounds.height,
        );
        pass.set_pipeline(&pipeline.blit_pipeline);
        pass.set_bind_group(0, &targets.blit_bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

#[derive(Debug)]
pub struct ModelPipeline {
    opaque_pipeline: wgpu::RenderPipeline,
    water_pipeline: wgpu::RenderPipeline,
    /// `None` on backends whose shader translation rejects the refractive
    /// water shader (naga's GLSL backend cannot translate `textureLoad` on a
    /// depth texture); water then renders through `water_pipeline` instead.
    refractive_water_pipeline: Option<wgpu::RenderPipeline>,
    translucent_pipeline: wgpu::RenderPipeline,
    additive_pipeline: wgpu::RenderPipeline,
    detail_pipeline: wgpu::RenderPipeline,
    overlay_opaque_pipeline: wgpu::RenderPipeline,
    overlay_translucent_pipeline: wgpu::RenderPipeline,
    overlay_additive_pipeline: wgpu::RenderPipeline,
    phy_debug_pipeline: wgpu::RenderPipeline,
    sky_pipeline: wgpu::RenderPipeline,
    blit_pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    sky_uniform_buffer: wgpu::Buffer,
    sky_uniform_bind_group: wgpu::BindGroup,
    map_skybox_uniform_buffer: wgpu::Buffer,
    map_skybox_uniform_bind_group: wgpu::BindGroup,
    material_layout: wgpu::BindGroupLayout,
    water_refraction_layout: wgpu::BindGroupLayout,
    sky_layout: wgpu::BindGroupLayout,
    blit_layout: wgpu::BindGroupLayout,
    material_sampler: wgpu::Sampler,
    simple_sampler: wgpu::Sampler,
    blit_sampler: wgpu::Sampler,
    water_refraction_sampler: wgpu::Sampler,
    sky_vertices: wgpu::Buffer,
    target_format: wgpu::TextureFormat,
    targets: Option<RenderTargets>,
    uploads: HashMap<u64, UploadedModel>,
    live: Vec<u64>,
    draw_plans: Option<DrawPlans>,
}

#[derive(Debug)]
pub(super) struct RenderTargets {
    pub(super) color: wgpu::TextureView,
    pub(super) resolve_texture: wgpu::Texture,
    pub(super) resolve_view: wgpu::TextureView,
    pub(super) depth: wgpu::TextureView,
    pub(super) refraction: Option<RefractionTarget>,
    pub(super) blit_bind_group: wgpu::BindGroup,
    pub(super) size: (u32, u32),
    pub(super) format: wgpu::TextureFormat,
}

#[derive(Debug)]
pub(super) struct RefractionTarget {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

impl RenderTargets {
    fn ensure_refraction(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
    ) {
        if self.refraction.is_some() {
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.model_viewer.refraction"),
            size: wgpu::Extent3d {
                width: self.size.0,
                height: self.size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.model_viewer.water_refraction_bind_group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&self.depth),
                },
            ],
        });
        self.refraction = Some(RefractionTarget {
            texture,
            bind_group,
        });
    }
}

#[derive(Debug)]
pub(super) struct UploadedModel {
    pub(super) meshes: Vec<UploadedMesh>,
    pub(super) detail_sprites: Option<UploadedDetailSprites>,
    pub(super) map_skybox_detail_sprites: Option<UploadedDetailSprites>,
    pub(super) overlays: Vec<UploadedOverlay>,
    pub(super) phy_debug_meshes: Option<Vec<UploadedMesh>>,
    pub(super) material_bind_groups: Vec<wgpu::BindGroup>,
    pub(super) material_render_modes: Vec<RenderMode>,
    pub(super) material_water_fallbacks: Vec<bool>,
    pub(super) _material_uniforms: Vec<wgpu::Buffer>,
    pub(super) skybox: Option<UploadedSkybox>,
    pub(super) sky_tint: [f32; 3],
    pub(super) visibility: UploadedVisibility,
}

impl UploadedModel {
    pub(super) fn has_map_skybox_content(&self) -> bool {
        self.meshes.iter().any(|mesh| mesh.map_skybox)
            || self.map_skybox_detail_sprites.is_some()
            || self.overlays.iter().any(|overlay| overlay.map_skybox)
    }

    pub(super) fn ensure_phy_debug_meshes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &ModelPreview,
    ) {
        if self.phy_debug_meshes.is_some() {
            return;
        }
        self.phy_debug_meshes = Some(upload_meshes(
            device,
            queue,
            scene.phy_debug_meshes.as_slice(),
            false,
        ));
    }

    pub(super) fn ensure_world_visibility(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &ModelPreview,
        enabled: bool,
        camera_position: [f32; 3],
    ) {
        let Some(visibility) = scene.visibility.as_ref() else {
            if self
                .visibility
                .tracker
                .set_state(VisibilityClusterState::Disabled)
                .is_some()
            {
                self.apply_world_visibility_plan(device, queue, None);
            }
            return;
        };
        let Some(state) = self
            .visibility
            .tracker
            .update(enabled, camera_position, |point| {
                visibility.cluster_at(point)
            })
        else {
            return;
        };

        match state {
            VisibilityClusterState::Disabled | VisibilityClusterState::StandDown => {
                self.apply_world_visibility_plan(device, queue, None);
            }
            VisibilityClusterState::Cluster(cluster) => {
                if let Some(visible_clusters) = visibility.visible_clusters(cluster) {
                    let plan = WorldVisibilityPlan::from_visible_clusters(scene, &visible_clusters);
                    log::debug!(
                        "map preview visibility rebuild cluster {cluster}: {} visible clusters",
                        plan.visible_cluster_count
                    );
                    self.apply_world_visibility_plan(device, queue, Some(plan));
                } else {
                    self.apply_world_visibility_plan(device, queue, None);
                }
            }
        }
    }

    pub(super) fn apply_world_visibility_plan(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: Option<WorldVisibilityPlan>,
    ) {
        for mesh in self.meshes.iter_mut() {
            if mesh.map_skybox || mesh.door_index.is_some() {
                continue;
            }
            mesh.visible_indices = plan.as_ref().map(|plan| {
                let visible = plan
                    .mesh_indices
                    .get(mesh.scene_mesh_index)
                    .map_or(&[][..], Vec::as_slice);
                upload_visible_indices(device, queue, visible)
            });
        }

        if let Some(detail_sprites) = self.detail_sprites.as_mut() {
            detail_sprites.visible_vertices = plan
                .as_ref()
                .map(|plan| upload_visible_detail_sprites(device, queue, detail_sprites, plan));
        }

        self.visibility.plan = plan;
    }

    pub(super) fn update_door_vertices(
        &mut self,
        queue: &wgpu::Queue,
        scene: &ModelPreview,
        poses: &[DoorRenderPose],
    ) {
        for mesh in self.meshes.iter_mut() {
            let Some(door_index) = mesh.door_index else {
                continue;
            };
            let Some(door) = scene.doors.get(door_index) else {
                continue;
            };
            let pose = poses
                .get(door_index)
                .copied()
                .unwrap_or_else(|| DoorRenderPose {
                    progress: door.initial_progress.clamp(0.0, 1.0),
                    open_sign: initial_door_open_sign(door.motion),
                });
            if mesh.last_door_pose == Some(pose) {
                continue;
            }
            let Some(local_vertices) = mesh.local_vertices.as_ref() else {
                continue;
            };
            let transformed = transform_door_vertices(door, local_vertices.as_slice(), pose);
            let bytes = model_vertex_bytes(transformed.as_slice());
            queue.write_buffer(&mesh.vertices, 0, &bytes);
            mesh.centroid = mesh_centroid(transformed.as_slice());
            mesh.last_door_pose = Some(pose);
        }
    }
}

#[derive(Debug)]
pub(super) struct UploadedSkybox {
    pub(super) face_bind_groups: [Option<wgpu::BindGroup>; SKYBOX_FACE_COUNT],
}

#[derive(Debug)]
pub(super) struct UploadedMesh {
    pub(super) vertices: wgpu::Buffer,
    pub(super) indices: wgpu::Buffer,
    pub(super) index_count: u32,
    pub(super) visible_indices: Option<UploadedVisibleIndices>,
    // Position in the source scene's mesh list, NOT this upload list:
    // empty-index meshes are dropped at upload, and WorldVisibilityPlan is
    // keyed by the unfiltered scene order.
    pub(super) scene_mesh_index: usize,
    pub(super) centroid: [f32; 3],
    pub(super) material_index: usize,
    pub(super) bodygroup: usize,
    pub(super) bodygroup_choice: usize,
    pub(super) map_skybox: bool,
    pub(super) door_index: Option<usize>,
    pub(super) door_visibility: Option<super::super::model::MapVisibilityBucket>,
    pub(super) local_vertices: Option<Vec<ModelVertex>>,
    pub(super) last_door_pose: Option<DoorRenderPose>,
}

#[derive(Debug)]
pub(super) struct UploadedDetailSprites {
    pub(super) vertices: wgpu::Buffer,
    pub(super) vertex_count: u32,
    pub(super) all_vertices: Vec<u8>,
    pub(super) sprite_count: usize,
    pub(super) visible_vertices: Option<UploadedVisibleVertices>,
    pub(super) material_index: usize,
}

#[derive(Debug)]
pub(super) struct UploadedOverlay {
    pub(super) vertices: wgpu::Buffer,
    pub(super) vertex_count: u32,
    pub(super) centroid: [f32; 3],
    pub(super) material_index: usize,
    pub(super) map_skybox: bool,
}

#[derive(Debug)]
pub(super) struct UploadedVisibleIndices {
    pub(super) buffer: Option<wgpu::Buffer>,
    pub(super) index_count: u32,
}

#[derive(Debug)]
pub(super) struct UploadedVisibleVertices {
    pub(super) buffer: Option<wgpu::Buffer>,
    pub(super) vertex_count: u32,
}

#[derive(Debug, Default)]
pub(super) struct UploadedVisibility {
    pub(super) tracker: VisibilityClusterTracker,
    pub(super) plan: Option<WorldVisibilityPlan>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum VisibilityClusterState {
    #[default]
    Disabled,
    StandDown,
    Cluster(i16),
}

#[derive(Debug, Default)]
pub(super) struct VisibilityClusterTracker {
    pub(super) last_camera_position: Option<[u32; 3]>,
    pub(super) state: VisibilityClusterState,
    pub(super) rebuild_count: u64,
}

impl VisibilityClusterTracker {
    pub(super) fn update(
        &mut self,
        enabled: bool,
        camera_position: [f32; 3],
        mut cluster_at: impl FnMut([f32; 3]) -> Option<i16>,
    ) -> Option<VisibilityClusterState> {
        if !enabled {
            self.last_camera_position = None;
            return self.set_state(VisibilityClusterState::Disabled);
        }

        let position_key = camera_position.map(f32::to_bits);
        if self.last_camera_position == Some(position_key) {
            return None;
        }
        self.last_camera_position = Some(position_key);
        let state = cluster_at(camera_position).map_or(
            VisibilityClusterState::StandDown,
            VisibilityClusterState::Cluster,
        );
        self.set_state(state)
    }

    pub(super) fn set_state(
        &mut self,
        state: VisibilityClusterState,
    ) -> Option<VisibilityClusterState> {
        if self.state == state {
            return None;
        }
        self.state = state;
        self.rebuild_count = self.rebuild_count.saturating_add(1);
        Some(state)
    }
}

#[derive(Clone, Copy)]
pub(super) struct MaterialTextureViews<'a> {
    pub(super) base: &'a wgpu::TextureView,
    pub(super) base2: &'a wgpu::TextureView,
    pub(super) lightmap: &'a wgpu::TextureView,
}

#[derive(Clone, Copy)]
pub(super) struct MaterialUploadMode {
    pub(super) force_opaque: bool,
    pub(super) render_mode: RenderMode,
}

#[derive(Clone, Copy)]
pub(super) struct PipelineRasterMode {
    pub(super) write_enabled: bool,
    pub(super) bias: wgpu::DepthBiasState,
    pub(super) cull_mode: Option<wgpu::Face>,
}

#[derive(Clone, Copy)]
struct PipelineShaderEntry {
    fragment: &'static str,
    label: &'static str,
}

pub(super) fn configure_scene_pass(pass: &mut wgpu::RenderPass<'_>, clip_bounds: &Rectangle<u32>) {
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

pub(super) fn draw_sky_background<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    pipeline: &'a ModelPipeline,
    upload: &'a UploadedModel,
) {
    let Some(skybox) = upload.skybox.as_ref() else {
        return;
    };
    pass.set_pipeline(&pipeline.sky_pipeline);
    pass.set_bind_group(0, &pipeline.sky_uniform_bind_group, &[]);
    pass.set_vertex_buffer(0, pipeline.sky_vertices.slice(..));
    for face in SkyboxFace::ALL {
        let Some(bind_group) = skybox.face_bind_groups[face.index()].as_ref() else {
            continue;
        };
        let start = u32::try_from(face.index() * 6).unwrap_or(0);
        pass.set_bind_group(1, bind_group, &[]);
        pass.draw(start..start + 6, 0..1);
    }
}

pub(super) fn draw_scene_plan<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    pipeline: &'a ModelPipeline,
    upload: &'a UploadedModel,
    plan: &'a DrawPlan,
    uniform_bind_group: &'a wgpu::BindGroup,
    detail_sprites: Option<&'a UploadedDetailSprites>,
) {
    draw_scene_plan_opaque(
        pass,
        pipeline,
        upload,
        plan,
        uniform_bind_group,
        detail_sprites,
    );
    draw_scene_plan_transparent(pass, pipeline, upload, plan, None);
}

fn draw_scene_plan_opaque<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    pipeline: &'a ModelPipeline,
    upload: &'a UploadedModel,
    plan: &'a DrawPlan,
    uniform_bind_group: &'a wgpu::BindGroup,
    detail_sprites: Option<&'a UploadedDetailSprites>,
) {
    pass.set_bind_group(0, uniform_bind_group, &[]);
    pass.set_pipeline(&pipeline.opaque_pipeline);
    for item in &plan.opaque {
        draw_model_item(pass, upload, *item);
    }
    if let Some(detail_sprites) = detail_sprites {
        pass.set_pipeline(&pipeline.detail_pipeline);
        draw_detail_sprites(pass, upload, detail_sprites);
    }
    pass.set_pipeline(&pipeline.overlay_opaque_pipeline);
    for item in &plan.overlay_opaque {
        draw_overlay_item(pass, upload, *item);
    }
}

fn draw_scene_plan_transparent<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    pipeline: &'a ModelPipeline,
    upload: &'a UploadedModel,
    plan: &'a DrawPlan,
    refraction_bind_group: Option<&'a wgpu::BindGroup>,
) {
    if let (Some(refraction_bind_group), Some(refractive_water_pipeline)) = (
        refraction_bind_group,
        pipeline.refractive_water_pipeline.as_ref(),
    ) {
        pass.set_pipeline(refractive_water_pipeline);
        pass.set_bind_group(2, refraction_bind_group, &[]);
    } else {
        pass.set_pipeline(&pipeline.water_pipeline);
    }
    for item in &plan.water {
        draw_model_item(pass, upload, *item);
    }
    pass.set_pipeline(&pipeline.overlay_translucent_pipeline);
    for item in &plan.overlay_translucent {
        draw_overlay_item(pass, upload, *item);
    }
    pass.set_pipeline(&pipeline.overlay_additive_pipeline);
    for item in &plan.overlay_additive {
        draw_overlay_item(pass, upload, *item);
    }
    pass.set_pipeline(&pipeline.translucent_pipeline);
    for item in &plan.translucent {
        draw_model_item(pass, upload, *item);
    }
    pass.set_pipeline(&pipeline.additive_pipeline);
    for item in &plan.additive {
        draw_model_item(pass, upload, *item);
    }
}

pub(super) fn draw_model_item<'a>(
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

pub(super) fn draw_phy_debug_meshes<'a>(
    pass: &mut wgpu::RenderPass<'a>,
    pipeline: &'a ModelPipeline,
    upload: &'a UploadedModel,
) {
    let Some(meshes) = upload.phy_debug_meshes.as_ref() else {
        return;
    };
    pass.set_pipeline(&pipeline.phy_debug_pipeline);
    for mesh in meshes {
        let Some(bind_group) = upload.material_bind_groups.get(mesh.material_index) else {
            continue;
        };
        draw_uploaded_mesh(pass, mesh, bind_group);
    }
}

pub(super) fn draw_uploaded_mesh<'a>(
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

pub(super) fn draw_detail_sprites<'a>(
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

pub(super) fn draw_overlay_item<'a>(
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

impl shader::Pipeline for ModelPipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        // These shaders run on iced_wgpu's own device, so they must fit the
        // limits it requests (max_bind_groups: 4, max_non_sampler_bindings:
        // 2048). Exceeding them panics here at preview time, not at build time.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SOURCE.into()),
        });
        let detail_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.detail_shader"),
            source: wgpu::ShaderSource::Wgsl(DETAIL_SHADER_SOURCE.into()),
        });
        let water_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.water_shader"),
            source: wgpu::ShaderSource::Wgsl(WATER_SHADER_SOURCE.into()),
        });
        let sky_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.sky_shader"),
            source: wgpu::ShaderSource::Wgsl(SKY_SHADER_SOURCE.into()),
        });
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.blit_shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER_SOURCE.into()),
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.model_viewer.uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.model_viewer.material"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let water_refraction_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("file_preview.model_viewer.water_refraction"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: true,
                        },
                        count: None,
                    },
                ],
            });
        let sky_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.model_viewer.sky_material"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.model_viewer.blit_texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("file_preview.model_viewer.layout"),
            bind_group_layouts: &[&uniform_layout, &material_layout],
            push_constant_ranges: &[],
        });
        let refractive_water_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("file_preview.model_viewer.refractive_water_layout"),
                bind_group_layouts: &[&uniform_layout, &material_layout, &water_refraction_layout],
                push_constant_ranges: &[],
            });
        let sky_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("file_preview.model_viewer.sky_layout"),
            bind_group_layouts: &[&uniform_layout, &sky_layout],
            push_constant_ranges: &[],
        });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("file_preview.model_viewer.blit_layout"),
            bind_group_layouts: &[&blit_layout],
            push_constant_ranges: &[],
        });

        let opaque_pipeline = create_model_pipeline(
            device,
            &layout,
            &shader,
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            true,
            "file_preview.model_viewer.opaque_pipeline",
        );
        let translucent_pipeline = create_model_pipeline(
            device,
            &layout,
            &shader,
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            false,
            "file_preview.model_viewer.translucent_pipeline",
        );
        let water_pipeline = create_model_pipeline_with_fragment_entry(
            device,
            &layout,
            &water_shader,
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                write_enabled: false,
                bias: wgpu::DepthBiasState::default(),
                cull_mode: Some(wgpu::Face::Back),
            },
            PipelineShaderEntry {
                fragment: "fs_skybox",
                label: "file_preview.model_viewer.water_pipeline",
            },
        );
        // Some backends fail shader translation for this pipeline (naga's
        // GLSL backend rejects `textureLoad` on a depth texture); capture
        // the error instead of hitting wgpu's panicking uncaptured-error
        // handler, and fall back to non-refractive water.
        device.push_error_scope(wgpu::ErrorFilter::Internal);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let refractive_water_pipeline = create_model_pipeline_with_fragment_entry(
            device,
            &refractive_water_layout,
            &water_shader,
            format,
            None,
            PipelineRasterMode {
                write_enabled: false,
                bias: wgpu::DepthBiasState::default(),
                cull_mode: Some(wgpu::Face::Back),
            },
            PipelineShaderEntry {
                fragment: "fs_main",
                label: "file_preview.model_viewer.refractive_water_pipeline",
            },
        );
        let validation_error = take_scoped_error(device);
        let internal_error = take_scoped_error(device);
        let translation_error = validation_error.or(internal_error);
        if let Some(error) = &translation_error {
            log::warn!(
                "refractive water pipeline unavailable, using non-refractive water: {error}"
            );
        }
        let refractive_water_pipeline = translation_error
            .is_none()
            .then_some(refractive_water_pipeline);
        let additive_pipeline = create_model_pipeline(
            device,
            &layout,
            &shader,
            format,
            Some(additive_blend_state()),
            false,
            "file_preview.model_viewer.additive_pipeline",
        );
        let detail_pipeline = create_detail_pipeline(
            device,
            &layout,
            &detail_shader,
            format,
            "file_preview.model_viewer.detail_pipeline",
        );
        let overlay_opaque_pipeline = create_model_pipeline_with_depth_bias(
            device,
            &layout,
            &shader,
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                write_enabled: false,
                bias: overlay_depth_bias_state(),
                // Overlays stay two-sided: their quad winding comes from
                // the packed overlay basis, not face winding.
                cull_mode: None,
            },
            "file_preview.model_viewer.overlay_opaque_pipeline",
        );
        let overlay_translucent_pipeline = create_model_pipeline_with_depth_bias(
            device,
            &layout,
            &shader,
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                write_enabled: false,
                bias: overlay_depth_bias_state(),
                cull_mode: None,
            },
            "file_preview.model_viewer.overlay_translucent_pipeline",
        );
        let overlay_additive_pipeline = create_model_pipeline_with_depth_bias(
            device,
            &layout,
            &shader,
            format,
            Some(additive_blend_state()),
            PipelineRasterMode {
                write_enabled: false,
                bias: overlay_depth_bias_state(),
                cull_mode: None,
            },
            "file_preview.model_viewer.overlay_additive_pipeline",
        );
        let phy_debug_pipeline = create_model_pipeline_with_depth_bias(
            device,
            &layout,
            &shader,
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                write_enabled: false,
                bias: overlay_depth_bias_state(),
                cull_mode: None,
            },
            "file_preview.model_viewer.phy_debug_pipeline",
        );
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("file_preview.model_viewer.sky_pipeline"),
            layout: Some(&sky_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &sky_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: (3 * std::mem::size_of::<f32>()) as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &sky_shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: multisample_state(),
            multiview: None,
            cache: None,
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("file_preview.model_viewer.blit_pipeline"),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.model_viewer.uniform_buffer"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sky_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.model_viewer.sky_uniform_buffer"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let map_skybox_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.model_viewer.map_skybox_uniform_buffer"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.model_viewer.uniform_bind_group"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let sky_uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.model_viewer.sky_uniform_bind_group"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: sky_uniform_buffer.as_entire_binding(),
            }],
        });
        let map_skybox_uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.model_viewer.map_skybox_uniform_bind_group"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: map_skybox_uniform_buffer.as_entire_binding(),
            }],
        });

        let material_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("file_preview.model_viewer.material_sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            anisotropy_clamp: MATERIAL_ANISOTROPY_CLAMP,
            ..wgpu::SamplerDescriptor::default()
        });
        let simple_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("file_preview.model_viewer.simple_sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..wgpu::SamplerDescriptor::default()
        });
        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("file_preview.model_viewer.blit_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..wgpu::SamplerDescriptor::default()
        });
        let water_refraction_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("file_preview.model_viewer.water_refraction_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..wgpu::SamplerDescriptor::default()
        });
        let sky_vertex_bytes = skybox_vertex_bytes();
        let sky_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.model_viewer.sky_vertices"),
            size: sky_vertex_bytes.len() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&sky_vertices, 0, &sky_vertex_bytes);

        Self {
            opaque_pipeline,
            water_pipeline,
            refractive_water_pipeline,
            translucent_pipeline,
            additive_pipeline,
            detail_pipeline,
            overlay_opaque_pipeline,
            overlay_translucent_pipeline,
            overlay_additive_pipeline,
            phy_debug_pipeline,
            sky_pipeline,
            blit_pipeline,
            uniform_buffer,
            uniform_bind_group,
            sky_uniform_buffer,
            sky_uniform_bind_group,
            map_skybox_uniform_buffer,
            map_skybox_uniform_bind_group,
            material_layout,
            water_refraction_layout,
            sky_layout,
            blit_layout,
            material_sampler,
            simple_sampler,
            blit_sampler,
            water_refraction_sampler,
            sky_vertices,
            target_format: format,
            targets: None,
            uploads: HashMap::new(),
            live: Vec::new(),
            draw_plans: None,
        }
    }

    fn trim(&mut self) {
        // Keep only uploads drawn since the last trim; a closed/replaced
        // preview drops its GPU buffers on the next frame.
        let live = std::mem::take(&mut self.live);
        self.uploads.retain(|id, _| live.contains(id));
    }
}

pub(super) fn multisample_state() -> wgpu::MultisampleState {
    wgpu::MultisampleState {
        count: MSAA_SAMPLE_COUNT,
        mask: !0,
        alpha_to_coverage_enabled: false,
    }
}

pub(super) fn create_model_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    depth_write_enabled: bool,
    label: &'static str,
) -> wgpu::RenderPipeline {
    create_model_pipeline_with_depth_bias(
        device,
        layout,
        shader,
        format,
        blend,
        PipelineRasterMode {
            write_enabled: depth_write_enabled,
            bias: wgpu::DepthBiasState::default(),
            cull_mode: Some(wgpu::Face::Back),
        },
        label,
    )
}

pub(super) fn create_model_pipeline_with_depth_bias(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    raster: PipelineRasterMode,
    label: &'static str,
) -> wgpu::RenderPipeline {
    create_model_pipeline_with_fragment_entry(
        device,
        layout,
        shader,
        format,
        blend,
        raster,
        PipelineShaderEntry {
            fragment: "fs_main",
            label,
        },
    )
}

fn create_model_pipeline_with_fragment_entry(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    raster: PipelineRasterMode,
    entry: PipelineShaderEntry,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(entry.label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[model_vertex_buffer_layout()],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(entry.fragment),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // Source renders brush/model geometry one-sided with clockwise
            // front faces; without this, up-facing water planes above the
            // camera paint their undersides across the sky.
            front_face: wgpu::FrontFace::Cw,
            cull_mode: raster.cull_mode,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: raster.write_enabled,
            depth_compare: wgpu::CompareFunction::GreaterEqual,
            stencil: wgpu::StencilState::default(),
            bias: raster.bias,
        }),
        multisample: multisample_state(),
        multiview: None,
        cache: None,
    })
}

pub(super) fn create_detail_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[detail_vertex_buffer_layout()],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::GreaterEqual,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: multisample_state(),
        multiview: None,
        cache: None,
    })
}

pub(super) fn overlay_depth_bias_state() -> wgpu::DepthBiasState {
    // Depth bias ADDS to the fragment depth. This viewer is reverse-Z
    // (clear 0.0, GreaterEqual, closer = LARGER depth), so pulling a
    // coplanar decal toward the viewer needs a POSITIVE constant; a
    // negative one pushes it behind its wall and loses the depth test.
    //
    // The constant alone is a couple of float ULPs — nowhere near the
    // interpolation divergence between a decal quad and its wall's
    // triangles at oblique angles, so decals flickered in and out with
    // camera movement. Slope-scaled bias grows with the polygon's depth
    // gradient; polygon offset exists precisely for coplanar decals.
    wgpu::DepthBiasState {
        constant: 2,
        slope_scale: 2.0,
        clamp: 0.0,
    }
}

pub(super) fn model_vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: MODEL_VERTEX_FLOAT_COUNT * std::mem::size_of::<f32>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &MODEL_VERTEX_ATTRIBUTES,
    }
}

pub(super) fn detail_vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: DETAIL_VERTEX_FLOAT_COUNT * std::mem::size_of::<f32>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &DETAIL_VERTEX_ATTRIBUTES,
    }
}

pub(super) fn additive_blend_state() -> wgpu::BlendState {
    let component = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    wgpu::BlendState {
        color: component,
        alpha: component,
    }
}

impl ModelPipeline {
    pub(super) fn ensure_targets(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        needs_refraction: bool,
    ) {
        let size = (width.max(1), height.max(1));
        if self
            .targets
            .as_ref()
            .is_some_and(|targets| targets.size == size && targets.format == self.target_format)
        {
            if needs_refraction && let Some(targets) = self.targets.as_mut() {
                targets.ensure_refraction(
                    device,
                    &self.water_refraction_layout,
                    &self.water_refraction_sampler,
                );
            }
            return;
        }
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.model_viewer.msaa_color"),
            size: extent,
            mip_level_count: 1,
            sample_count: MSAA_SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: self.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let resolve = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.model_viewer.msaa_resolve"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.model_viewer.depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: MSAA_SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let color = color.create_view(&wgpu::TextureViewDescriptor::default());
        let resolve_view = resolve.create_view(&wgpu::TextureViewDescriptor::default());
        let depth = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let blit_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.model_viewer.blit_bind_group"),
            layout: &self.blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&resolve_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                },
            ],
        });
        let mut targets = RenderTargets {
            color,
            resolve_texture: resolve,
            resolve_view,
            depth,
            refraction: None,
            blit_bind_group,
            size,
            format: self.target_format,
        };
        if needs_refraction {
            targets.ensure_refraction(
                device,
                &self.water_refraction_layout,
                &self.water_refraction_sampler,
            );
        }
        self.targets = Some(targets);
    }

    pub(super) fn touch(&mut self, content_id: u64) {
        if !self.live.contains(&content_id) {
            self.live.push(content_id);
        }
    }

    pub(super) fn ensure_upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        content_id: u64,
        model: &ModelPreview,
    ) {
        if self.uploads.contains_key(&content_id) {
            return;
        }

        let (lightmap_rgba, lightmap_width, lightmap_height) =
            model
                .lightmap
                .as_ref()
                .map_or((WHITE_RGBA.as_slice(), 1, 1), |lightmap| {
                    (
                        lightmap.rgba.as_slice(),
                        lightmap.width.max(1),
                        lightmap.height.max(1),
                    )
                });
        let lightmap_view = self.create_texture_view(
            device,
            queue,
            "file_preview.model_viewer.lightmap",
            lightmap_rgba,
            lightmap_width,
            lightmap_height,
        );

        let material_uploads = model
            .materials
            .iter()
            .map(|slot| {
                let base_view = self.create_slot_texture_view(
                    device,
                    queue,
                    "file_preview.model_viewer.texture",
                    slot,
                    slot.texture.as_deref(),
                );
                let base2_view = self.create_slot_texture_view(
                    device,
                    queue,
                    "file_preview.model_viewer.texture2",
                    slot,
                    slot.texture2.as_deref().or(slot.texture.as_deref()),
                );
                self.create_material(
                    device,
                    queue,
                    MaterialTextureViews {
                        base: &base_view,
                        base2: &base2_view,
                        lightmap: &lightmap_view,
                    },
                    MaterialUploadMode {
                        force_opaque: slot.force_opaque,
                        render_mode: slot.render_mode,
                    },
                )
            })
            .collect::<Vec<_>>();
        let material_render_modes = if model.materials.is_empty() {
            vec![RenderMode::Opaque]
        } else {
            model
                .materials
                .iter()
                .map(|slot| slot.render_mode)
                .collect::<Vec<_>>()
        };
        let material_water_fallbacks = if model.materials.is_empty() {
            vec![false]
        } else {
            model
                .materials
                .iter()
                .map(|slot| {
                    slot.texture
                        .as_deref()
                        .is_some_and(ResolvedTexture::is_water_fallback)
                })
                .collect::<Vec<_>>()
        };
        let (material_bind_groups, material_uniforms): (Vec<_>, Vec<_>) =
            if material_uploads.is_empty() {
                let base_view = self.create_material_texture_view(
                    device,
                    queue,
                    "file_preview.model_viewer.texture",
                    None,
                );
                let (bind_group, uniform) = self.create_material(
                    device,
                    queue,
                    MaterialTextureViews {
                        base: &base_view,
                        base2: &base_view,
                        lightmap: &lightmap_view,
                    },
                    MaterialUploadMode {
                        force_opaque: true,
                        render_mode: RenderMode::Opaque,
                    },
                );
                (vec![bind_group], vec![uniform])
            } else {
                material_uploads.into_iter().unzip()
            };

        let mut meshes = upload_meshes(device, queue, model.meshes.as_slice(), false);
        meshes.extend(upload_meshes(
            device,
            queue,
            model.map_skybox_meshes.as_slice(),
            true,
        ));
        meshes.extend(upload_door_meshes(device, queue, model.doors.as_slice()));
        let detail_sprites = upload_detail_sprites(device, queue, model.detail_sprites.as_slice());
        let map_skybox_detail_sprites =
            upload_detail_sprites(device, queue, model.map_skybox_detail_sprites.as_slice());
        let mut overlays = upload_overlays(device, queue, model.overlays.as_slice(), false);
        overlays.extend(upload_overlays(
            device,
            queue,
            model.map_skybox_overlays.as_slice(),
            true,
        ));
        let sky_tint = scene_sky_tint(model.skybox.as_ref());
        let skybox = model.skybox.as_ref().map(|skybox| {
            let face_bind_groups = std::array::from_fn(|index| {
                skybox.faces[index].as_ref().map(|texture| {
                    let view = self.create_texture_view(
                        device,
                        queue,
                        "file_preview.model_viewer.sky_texture",
                        texture.rgba_bytes().unwrap_or(WHITE_RGBA.as_slice()),
                        texture.width.max(1),
                        texture.height.max(1),
                    );
                    self.create_sky_material(device, &view)
                })
            });
            UploadedSkybox { face_bind_groups }
        });

        self.uploads.insert(
            content_id,
            UploadedModel {
                meshes,
                detail_sprites,
                map_skybox_detail_sprites,
                overlays,
                phy_debug_meshes: None,
                material_bind_groups,
                material_render_modes,
                material_water_fallbacks,
                _material_uniforms: material_uniforms,
                skybox,
                sky_tint,
                visibility: UploadedVisibility::default(),
            },
        );
    }

    pub(super) fn create_texture_view(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> wgpu::TextureView {
        let level = TextureUploadLevel {
            rgba,
            width: width.max(1),
            height: height.max(1),
        };
        self.create_texture_view_from_levels(device, queue, label, &[level])
    }

    pub(super) fn create_material_texture_view(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        texture: Option<&ResolvedTexture>,
    ) -> wgpu::TextureView {
        let Some(texture) = texture else {
            let levels = checkerboard_mip_levels();
            return self.create_texture_view_from_levels(device, queue, label, &levels);
        };
        if texture.is_water_fallback() {
            return self.create_texture_view(
                device,
                queue,
                label,
                texture.rgba_bytes().unwrap_or(WHITE_RGBA.as_slice()),
                texture.width.max(1),
                texture.height.max(1),
            );
        }
        if let Some((format, mips)) = texture.bc_payload() {
            return self.create_bc_texture_view(device, queue, label, format, mips);
        }

        let levels = texture
            .mip_chain()
            .map(|mip| TextureUploadLevel {
                rgba: mip.rgba,
                width: mip.width,
                height: mip.height,
            })
            .collect::<Vec<_>>();
        self.create_texture_view_from_levels(device, queue, label, &levels)
    }

    pub(super) fn create_slot_texture_view(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        slot: &MaterialSlot,
        texture: Option<&ResolvedTexture>,
    ) -> wgpu::TextureView {
        if slot.name == PHY_DEBUG_MATERIAL_NAME && texture.is_none() {
            return self.create_texture_view(device, queue, label, PHY_DEBUG_RGBA.as_slice(), 1, 1);
        }
        self.create_material_texture_view(device, queue, label, texture)
    }

    pub(super) fn create_texture_view_from_levels(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        levels: &[TextureUploadLevel<'_>],
    ) -> wgpu::TextureView {
        let fallback_level = TextureUploadLevel {
            rgba: WHITE_RGBA.as_slice(),
            width: 1,
            height: 1,
        };
        let supplied_base = levels.first().copied();
        let use_supplied_chain = supplied_base.is_some_and(TextureUploadLevel::is_valid)
            && levels.iter().all(|level| level.is_valid());
        let base = if use_supplied_chain {
            supplied_base.unwrap_or(fallback_level)
        } else {
            supplied_base
                .filter(|level| level.is_valid())
                .unwrap_or(fallback_level)
        };
        let mip_level_count = if use_supplied_chain {
            u32::try_from(levels.len()).unwrap_or(1).max(1)
        } else {
            1
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: base.width.max(1),
                height: base.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        if use_supplied_chain {
            for (mip_level, level) in levels.iter().take(mip_level_count as usize).enumerate() {
                write_texture_level(
                    queue,
                    &texture,
                    u32::try_from(mip_level).unwrap_or(0),
                    *level,
                );
            }
        } else {
            write_texture_level(queue, &texture, 0, base);
        }
        texture.create_view(&wgpu::TextureViewDescriptor::default())
    }

    pub(super) fn create_bc_texture_view(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        format: BcFormat,
        mips: &[ResolvedBcMip],
    ) -> wgpu::TextureView {
        let Some(base) = mips.first() else {
            return self.create_texture_view(device, queue, label, WHITE_RGBA.as_slice(), 1, 1);
        };
        if !device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            if let Some(rgba) = decode_bc_texture(format, base.width, base.height, &base.data) {
                return self.create_texture_view(
                    device,
                    queue,
                    label,
                    &rgba,
                    base.width,
                    base.height,
                );
            }
            return self.create_texture_view(device, queue, label, WHITE_RGBA.as_slice(), 1, 1);
        }
        if !mips.iter().all(|mip| bc_mip_is_valid(format, mip)) {
            if let Some(rgba) = decode_bc_texture(format, base.width, base.height, &base.data) {
                return self.create_texture_view(
                    device,
                    queue,
                    label,
                    &rgba,
                    base.width,
                    base.height,
                );
            }
            return self.create_texture_view(device, queue, label, WHITE_RGBA.as_slice(), 1, 1);
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: base.width.max(1),
                height: base.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: u32::try_from(mips.len()).unwrap_or(1).max(1),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: bc_texture_format(format),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (mip_level, mip) in mips.iter().enumerate() {
            write_bc_texture_level(
                queue,
                &texture,
                u32::try_from(mip_level).unwrap_or(0),
                format,
                mip,
            );
        }
        texture.create_view(&wgpu::TextureViewDescriptor::default())
    }

    pub(super) fn create_material(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        views: MaterialTextureViews<'_>,
        mode: MaterialUploadMode,
    ) -> (wgpu::BindGroup, wgpu::Buffer) {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.model_viewer.material_uniform"),
            size: std::mem::size_of::<MaterialUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let material_uniform = MaterialUniform::new(mode.force_opaque, mode.render_mode);
        // Material uploads are immutable for this content_id; write once while
        // creating the bind group.
        queue.write_buffer(&uniform, 0, bytemuck::bytes_of(&material_uniform));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.model_viewer.material_bind_group"),
            layout: &self.material_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(views.base),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(views.base2),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(views.lightmap),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.material_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.simple_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        (bind_group, uniform)
    }

    pub(super) fn create_sky_material(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.model_viewer.sky_bind_group"),
            layout: &self.sky_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.simple_sampler),
                },
            ],
        })
    }
}

pub(super) fn upload_detail_sprites(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    sprites: &[DetailSprite],
) -> Option<UploadedDetailSprites> {
    let first = sprites.first()?;
    let mut vertex_bytes = Vec::with_capacity(
        sprites.len()
            * 6
            * usize::try_from(DETAIL_VERTEX_FLOAT_COUNT).unwrap_or(7)
            * std::mem::size_of::<f32>(),
    );
    for sprite in sprites {
        push_detail_sprite_vertices(&mut vertex_bytes, sprite);
    }
    let vertices = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("file_preview.model_viewer.detail_vertices"),
        size: vertex_bytes.len() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&vertices, 0, &vertex_bytes);

    Some(UploadedDetailSprites {
        vertices,
        vertex_count: u32::try_from(sprites.len().saturating_mul(6)).unwrap_or(u32::MAX),
        all_vertices: vertex_bytes,
        sprite_count: sprites.len(),
        visible_vertices: None,
        material_index: first.material_index,
    })
}

pub(super) fn upload_visible_indices(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    indices: &[u32],
) -> UploadedVisibleIndices {
    let index_count = u32::try_from(indices.len()).unwrap_or(u32::MAX);
    if indices.is_empty() {
        return UploadedVisibleIndices {
            buffer: None,
            index_count,
        };
    }
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("file_preview.model_viewer.visible_indices"),
        size: std::mem::size_of_val(indices) as u64,
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, bytemuck::cast_slice(indices));
    UploadedVisibleIndices {
        buffer: Some(buffer),
        index_count,
    }
}

pub(super) fn upload_visible_detail_sprites(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    sprites: &UploadedDetailSprites,
    plan: &WorldVisibilityPlan,
) -> UploadedVisibleVertices {
    let sprite_bytes = sprites.all_vertices.len() / sprites.sprite_count.max(1);
    let mut bytes = Vec::with_capacity(sprites.all_vertices.len());
    for (sprite_index, visible) in plan.detail_sprite_visible.iter().copied().enumerate() {
        if !visible {
            continue;
        }
        let start = sprite_index.saturating_mul(sprite_bytes);
        let end = start.saturating_add(sprite_bytes);
        if let Some(slice) = sprites.all_vertices.get(start..end) {
            bytes.extend_from_slice(slice);
        }
    }
    let vertex_count = u32::try_from(
        bytes.len() / (DETAIL_VERTEX_FLOAT_COUNT as usize * std::mem::size_of::<f32>()),
    )
    .unwrap_or(u32::MAX);
    if bytes.is_empty() {
        return UploadedVisibleVertices {
            buffer: None,
            vertex_count,
        };
    }
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("file_preview.model_viewer.visible_detail_vertices"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, &bytes);
    UploadedVisibleVertices {
        buffer: Some(buffer),
        vertex_count,
    }
}

pub(super) fn upload_meshes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    meshes: &[MeshData],
    map_skybox: bool,
) -> Vec<UploadedMesh> {
    meshes
        .iter()
        .enumerate()
        .filter(|(_, mesh)| !mesh.indices.is_empty())
        .map(|(scene_mesh_index, mesh)| {
            let mut vertex_bytes = Vec::with_capacity(
                mesh.vertices.len()
                    * usize::try_from(MODEL_VERTEX_FLOAT_COUNT).unwrap_or(14)
                    * std::mem::size_of::<f32>(),
            );
            for vertex in &mesh.vertices {
                for component in vertex
                    .position
                    .iter()
                    .chain(vertex.normal.iter())
                    .chain(vertex.uv.iter())
                    .chain(vertex.lightmap_uv.iter())
                    .chain(vertex.color.iter())
                {
                    vertex_bytes.extend_from_slice(&component.to_le_bytes());
                }
                vertex_bytes.extend_from_slice(&vertex.blend_alpha.to_le_bytes());
            }
            let centroid = mesh_centroid(&mesh.vertices);

            let vertices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.model_viewer.vertices"),
                size: vertex_bytes.len() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&vertices, 0, &vertex_bytes);

            let indices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.model_viewer.indices"),
                size: (mesh.indices.len() * std::mem::size_of::<u32>()) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&indices, 0, bytemuck::cast_slice(&mesh.indices));

            UploadedMesh {
                vertices,
                indices,
                index_count: mesh.indices.len() as u32,
                visible_indices: None,
                scene_mesh_index,
                centroid,
                material_index: mesh.material_index,
                bodygroup: mesh.bodygroup,
                bodygroup_choice: mesh.bodygroup_choice,
                map_skybox,
                door_index: None,
                door_visibility: None,
                local_vertices: None,
                last_door_pose: None,
            }
        })
        .collect()
}

pub(super) fn upload_door_meshes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    doors: &[DoorInstance],
) -> Vec<UploadedMesh> {
    let mut uploaded = Vec::new();
    for (door_index, door) in doors.iter().enumerate() {
        let pose = DoorRenderPose {
            progress: door.initial_progress.clamp(0.0, 1.0),
            open_sign: initial_door_open_sign(door.motion),
        };
        for mesh in &door.meshes {
            if mesh.vertices.is_empty() || mesh.indices.is_empty() {
                continue;
            }
            let transformed_vertices =
                transform_door_vertices(door, mesh.vertices.as_slice(), pose);
            let vertex_bytes = model_vertex_bytes(transformed_vertices.as_slice());
            let vertices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.model_viewer.door_vertices"),
                size: vertex_bytes.len() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&vertices, 0, &vertex_bytes);

            let indices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.model_viewer.door_indices"),
                size: (mesh.indices.len() * std::mem::size_of::<u32>()) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&indices, 0, bytemuck::cast_slice(&mesh.indices));

            uploaded.push(UploadedMesh {
                vertices,
                indices,
                index_count: mesh.indices.len() as u32,
                visible_indices: None,
                scene_mesh_index: usize::MAX,
                centroid: mesh_centroid(&transformed_vertices),
                material_index: mesh.material_index,
                bodygroup: 0,
                bodygroup_choice: 0,
                map_skybox: false,
                door_index: Some(door_index),
                door_visibility: Some(door.visibility),
                local_vertices: Some(mesh.vertices.clone()),
                last_door_pose: Some(pose),
            });
        }
    }
    uploaded
}

pub(super) fn model_vertex_bytes(vertices: &[ModelVertex]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(
        vertices.len()
            * usize::try_from(MODEL_VERTEX_FLOAT_COUNT).unwrap_or(14)
            * std::mem::size_of::<f32>(),
    );
    for vertex in vertices {
        push_f32s(&mut bytes, &vertex.position);
        push_f32s(&mut bytes, &vertex.normal);
        push_f32s(&mut bytes, &vertex.uv);
        push_f32s(&mut bytes, &vertex.lightmap_uv);
        push_f32s(&mut bytes, &vertex.color);
        bytes.extend_from_slice(&vertex.blend_alpha.to_le_bytes());
    }
    bytes
}

pub(super) fn push_detail_sprite_vertices(bytes: &mut Vec<u8>, sprite: &DetailSprite) {
    let left = sprite.upper_left[0];
    let right = sprite.lower_right[0];
    let top = sprite.upper_left[1];
    let bottom = sprite.lower_right[1];
    let tex_left = sprite.tex_upper_left[0];
    let tex_top = sprite.tex_upper_left[1];
    let tex_right = sprite.tex_lower_right[0];
    let tex_bottom = sprite.tex_lower_right[1];
    for (corner, uv) in [
        ([left, top], [tex_left, tex_top]),
        ([right, top], [tex_right, tex_top]),
        ([right, bottom], [tex_right, tex_bottom]),
        ([left, top], [tex_left, tex_top]),
        ([right, bottom], [tex_right, tex_bottom]),
        ([left, bottom], [tex_left, tex_bottom]),
    ] {
        push_f32s(bytes, &sprite.origin);
        push_f32s(bytes, &corner);
        push_f32s(bytes, &uv);
    }
}

pub(super) fn upload_overlays(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    overlays: &[OverlayPrimitive],
    map_skybox: bool,
) -> Vec<UploadedOverlay> {
    overlays
        .iter()
        .filter_map(|overlay| {
            let vertex_bytes = overlay_vertex_bytes(overlay);
            if vertex_bytes.is_empty() {
                return None;
            }
            let vertices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.model_viewer.overlay_vertices"),
                size: vertex_bytes.len() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&vertices, 0, &vertex_bytes);
            Some(UploadedOverlay {
                vertices,
                vertex_count: 6,
                centroid: overlay_centroid(overlay),
                material_index: overlay.material_index,
                map_skybox,
            })
        })
        .collect()
}

pub(super) fn overlay_vertex_bytes(overlay: &OverlayPrimitive) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(
        6 * usize::try_from(MODEL_VERTEX_FLOAT_COUNT).unwrap_or(14) * std::mem::size_of::<f32>(),
    );
    for index in [0_usize, 1, 2, 0, 2, 3] {
        let vertex = overlay.vertices[index];
        push_f32s(&mut bytes, &vertex.position);
        push_f32s(&mut bytes, &vertex.normal);
        push_f32s(&mut bytes, &vertex.uv);
        push_f32s(&mut bytes, &[0.0, 0.0]);
        push_f32s(&mut bytes, &[1.0, 1.0, 1.0]);
        push_f32s(&mut bytes, &[0.0]);
    }
    bytes
}

pub(super) fn overlay_centroid(overlay: &OverlayPrimitive) -> [f32; 3] {
    let mut centroid = [0.0_f32; 3];
    for vertex in overlay.vertices {
        for (axis, component) in vertex.position.into_iter().enumerate() {
            centroid[axis] += component;
        }
    }
    centroid.map(|component| component / overlay.vertices.len() as f32)
}

pub(super) fn push_f32s(bytes: &mut Vec<u8>, values: &[f32]) {
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

pub(super) fn mesh_centroid(vertices: &[ModelVertex]) -> [f32; 3] {
    if vertices.is_empty() {
        return [0.0; 3];
    }
    let mut sum = [0.0_f32; 3];
    for vertex in vertices {
        sum[0] += vertex.position[0];
        sum[1] += vertex.position[1];
        sum[2] += vertex.position[2];
    }
    let scale = 1.0 / vertices.len() as f32;
    [sum[0] * scale, sum[1] * scale, sum[2] * scale]
}

pub(super) fn skybox_vertex_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SKYBOX_FACE_COUNT * 6 * 3 * std::mem::size_of::<f32>());
    for face in SkyboxFace::ALL {
        let corners = skybox_face_corners(face);
        for position in [
            corners[0], corners[1], corners[2], corners[0], corners[2], corners[3],
        ] {
            for component in position {
                bytes.extend_from_slice(&component.to_le_bytes());
            }
        }
    }
    bytes
}

// Valve's 2D skybox suffixes are documented on the Valve Developer Community
// "Skybox (2D)" page; the Source-space corner data below follows
// noclip.website's SourceEngine SkyboxRenderer vertex table:
// https://github.com/magcius/noclip.website/blob/main/src/SourceEngine/Main.ts
pub(super) fn skybox_face_corners(face: SkyboxFace) -> [[f32; 3]; 4] {
    match face {
        SkyboxFace::Rt => [
            [1.0, 1.0, -1.0],
            [1.0, 1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, -1.0, -1.0],
        ],
        SkyboxFace::Lf => [
            [-1.0, -1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [-1.0, 1.0, 1.0],
            [-1.0, 1.0, -1.0],
        ],
        SkyboxFace::Bk => [
            [-1.0, 1.0, -1.0],
            [-1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, -1.0],
        ],
        SkyboxFace::Ft => [
            [1.0, -1.0, -1.0],
            [1.0, -1.0, 1.0],
            [-1.0, -1.0, 1.0],
            [-1.0, -1.0, -1.0],
        ],
        SkyboxFace::Up => [
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
        ],
        SkyboxFace::Dn => [
            [-1.0, 1.0, -1.0],
            [1.0, 1.0, -1.0],
            [1.0, -1.0, -1.0],
            [-1.0, -1.0, -1.0],
        ],
    }
}

pub(super) static WHITE_RGBA: [u8; 4] = [255, 255, 255, 255];

// 8x8 magenta/black checkerboard for unresolved materials.
pub(super) static CHECKERBOARD_RGBA: [u8; CHECKERBOARD_BYTES] = checkerboard_rgba();
pub(super) static CHECKERBOARD_MIP_4X4_RGBA: [u8; CHECKERBOARD_MIP_4X4_BYTES] =
    solid_rgba(CHECKERBOARD_MIP_RGBA);
pub(super) static CHECKERBOARD_MIP_2X2_RGBA: [u8; CHECKERBOARD_MIP_2X2_BYTES] =
    solid_rgba(CHECKERBOARD_MIP_RGBA);
pub(super) static CHECKERBOARD_MIP_1X1_RGBA: [u8; CHECKERBOARD_MIP_1X1_BYTES] =
    solid_rgba(CHECKERBOARD_MIP_RGBA);

pub(super) fn checkerboard_mip_levels() -> [TextureUploadLevel<'static>; 4] {
    [
        TextureUploadLevel {
            rgba: CHECKERBOARD_RGBA.as_slice(),
            width: CHECKERBOARD_SIZE,
            height: CHECKERBOARD_SIZE,
        },
        TextureUploadLevel {
            rgba: CHECKERBOARD_MIP_4X4_RGBA.as_slice(),
            width: 4,
            height: 4,
        },
        TextureUploadLevel {
            rgba: CHECKERBOARD_MIP_2X2_RGBA.as_slice(),
            width: 2,
            height: 2,
        },
        TextureUploadLevel {
            rgba: CHECKERBOARD_MIP_1X1_RGBA.as_slice(),
            width: 1,
            height: 1,
        },
    ]
}

pub(super) const fn checkerboard_rgba() -> [u8; CHECKERBOARD_BYTES] {
    let mut rgba = [0_u8; CHECKERBOARD_BYTES];
    let mut y = 0;
    while y < CHECKERBOARD_SIZE_USIZE {
        let mut x = 0;
        while x < CHECKERBOARD_SIZE_USIZE {
            let offset = (y * CHECKERBOARD_SIZE_USIZE + x) * 4;
            if (x + y) % 2 == 0 {
                rgba[offset] = 255;
                rgba[offset + 1] = 0;
                rgba[offset + 2] = 255;
                rgba[offset + 3] = 255;
            } else {
                rgba[offset] = 20;
                rgba[offset + 1] = 20;
                rgba[offset + 2] = 20;
                rgba[offset + 3] = 255;
            }
            x += 1;
        }
        y += 1;
    }
    rgba
}

pub(super) const fn solid_rgba<const N: usize>(color: [u8; 4]) -> [u8; N] {
    let mut rgba = [0_u8; N];
    let mut offset = 0;
    while offset < N {
        rgba[offset] = color[0];
        rgba[offset + 1] = color[1];
        rgba[offset + 2] = color[2];
        rgba[offset + 3] = color[3];
        offset += 4;
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::{DrawItem, DrawPlan, FrameLayout, frame_layout};

    #[test]
    fn frame_layout_splits_only_for_world_water() {
        assert_eq!(frame_layout(None, true), FrameLayout::SinglePass);
        assert_eq!(
            frame_layout(Some(&DrawPlan::default()), true),
            FrameLayout::SinglePass
        );

        let mut plan = DrawPlan::default();
        plan.water.push(DrawItem {
            mesh_index: 0,
            material_slot: 0,
            distance_squared: 0.0,
        });
        assert_eq!(
            frame_layout(Some(&plan), true),
            FrameLayout::RefractiveWater
        );
        // Without the refractive pipeline, water stays on the single-pass path.
        assert_eq!(frame_layout(Some(&plan), false), FrameLayout::SinglePass);
    }
}
