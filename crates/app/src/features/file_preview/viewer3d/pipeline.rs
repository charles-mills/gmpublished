//! The wgpu renderer behind the 3D model preview, organised by state
//! lifecycle: `resources` owns objects created once per device, `targets`
//! follows the viewport, `upload` and `visibility` own per-preview GPU
//! data, and `uniforms` and `draw` serve a single frame. [`ModelPipeline`]
//! holds one of each; [`ModelPrimitive`] is one frame's draw against it.

use super::{
    AMBIENT, Arc, BLIT_SHADER_SOURCE, BcFormat, CHECKERBOARD_BYTES, CHECKERBOARD_MIP_1X1_BYTES,
    CHECKERBOARD_MIP_2X2_BYTES, CHECKERBOARD_MIP_4X4_BYTES, CHECKERBOARD_MIP_RGBA,
    CHECKERBOARD_SIZE, CHECKERBOARD_SIZE_USIZE, Camera, DETAIL_SHADER_SOURCE,
    DETAIL_VERTEX_ATTRIBUTES, DETAIL_VERTEX_FLOAT_COUNT, DetailSprite, DoorInstance,
    DoorRenderPose, DrawItem, DrawPlan, DrawPlans, FOV_Y, FlyCamera, MATERIAL_ANISOTROPY_CLAMP,
    MODEL_VERTEX_ATTRIBUTES, MSAA_SAMPLE_COUNT, MapFog, MapSkyCamera, MapVisibilityBucket,
    MaterialSlot, MeshData, ModelPreview, ModelVertex, OverlayDrawItem, OverlayPrimitive,
    PHY_DEBUG_MATERIAL_NAME, PHY_DEBUG_RGBA, Rectangle, RenderMode, ResolvedBcMip, ResolvedTexture,
    SHADER_SOURCE, SKY_SHADER_SOURCE, SKYBOX_FACE_COUNT, SOURCE_UP, Skybox, SkyboxFace,
    TextureUploadLevel, Viewport, WATER_SHADER_SOURCE, WorldVisibilityPlan, bc_mip_is_valid,
    bc_texture_format, decode_bc_texture, half_extent, initial_door_swing, look_at, mat_mul, mid,
    perspective, prepare_draw_plans, shader, skybox_eye, transform_door_vertices, wgpu,
    write_bc_texture_level, write_texture_level,
};
use gmpublished_backend::math::Vec3;

mod draw;
mod materials;
mod resources;
mod targets;
mod uniforms;
mod upload;
mod visibility;

pub use uniforms::Uniforms;
pub use upload::UploadedModel;

use draw::{
    configure_scene_pass, draw_phy_debug_meshes, draw_scene_plan, draw_scene_plan_opaque,
    draw_scene_plan_transparent, draw_sky_background,
};
use resources::RenderResources;
use targets::RenderTargets;
use uniforms::DEFAULT_SKY_TINT;
use upload::UploadCache;

/// One frame's draw of a loaded model; heavy data is uploaded once per
/// `content_id` and cached in the shared [`ModelPipeline`].
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "three independent render toggles plus the camera's water state"
)]
pub struct ModelPrimitive {
    pub(super) model: Arc<ModelPreview>,
    pub(super) content_id: u64,
    pub(super) skin_remap: Vec<u16>,
    pub(super) bodygroup_choices: Vec<usize>,
    pub(super) map_skybox_visible: bool,
    pub(super) visibility_culling: bool,
    pub(super) phy_debug_visible: bool,
    pub(super) uniforms: Uniforms,
    /// Whether the camera eye is under water. Drives the clear colour and
    /// which passes run; no shader reads it.
    pub(super) submerged: bool,
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
        pipeline.uploads.ensure_upload(
            device,
            queue,
            &pipeline.resources,
            self.content_id,
            &self.model,
        );
        pipeline.uploads.touch(self.content_id);
        if let Some(upload) = pipeline.uploads.get_mut(self.content_id) {
            if self.phy_debug_visible {
                upload.ensure_phy_debug_meshes(device, queue, &self.model);
            }
            upload.update_door_vertices(queue, &self.model, self.door_poses.as_slice());
            upload.ensure_world_visibility(
                device,
                queue,
                &self.model,
                self.visibility_culling,
                Vec3::new(
                    self.uniforms.camera_position[0],
                    self.uniforms.camera_position[1],
                    self.uniforms.camera_position[2],
                ),
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
                pipeline.resources.refractive_water_pipeline.is_some(),
            )
            .uses_refraction();
            pipeline.draw_plans = Some(draw_plans);
            RenderTargets::ensure(
                &mut pipeline.targets,
                device,
                &pipeline.resources,
                size.width,
                size.height,
                needs_refraction,
            );
        }
        let sky_tint = pipeline
            .uploads
            .get(self.content_id)
            .map_or(DEFAULT_SKY_TINT, |upload| upload.sky_tint);
        let with_sky_tint = |mut uniforms: Uniforms| {
            uniforms.water_time_sky_tint[0] = self.uniforms.water_time_sky_tint[0];
            uniforms.water_time_sky_tint[1..].copy_from_slice(sky_tint.as_array());
            if self.submerged {
                uniforms.fog_color = self.uniforms.fog_color;
                uniforms.fog_params = self.uniforms.fog_params;
            }
            uniforms
        };
        let uniforms = with_sky_tint(self.uniforms);
        queue.write_buffer(
            &pipeline.resources.uniform_buffer,
            0,
            bytemuck::bytes_of(&uniforms),
        );
        if let Some(sky_uniforms) = self.sky_uniforms.as_ref() {
            let sky_uniforms = with_sky_tint(*sky_uniforms);
            queue.write_buffer(
                &pipeline.resources.sky_uniform_buffer,
                0,
                bytemuck::bytes_of(&sky_uniforms),
            );
        }
        if let Some(map_skybox_uniforms) = self.map_skybox_uniforms.as_ref() {
            let map_skybox_uniforms = with_sky_tint(*map_skybox_uniforms);
            queue.write_buffer(
                &pipeline.resources.map_skybox_uniform_buffer,
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
        let Some(upload) = pipeline.uploads.get(self.content_id) else {
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
        let submerged = self.submerged;
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
                draw_sky_background(&mut pass, &pipeline.resources, upload);
            }
            if let Some(plan) = plans.and_then(|plans| plans.map_skybox.as_ref()) {
                draw_scene_plan(
                    &mut pass,
                    &pipeline.resources,
                    upload,
                    plan,
                    &pipeline.resources.map_skybox_uniform_bind_group,
                    upload.map_skybox_detail_sprites.as_ref(),
                );
            }
            drop(pass);
        }

        let world_plan = plans.map(|plans| &plans.world);
        let layout = frame_layout(
            world_plan,
            pipeline.resources.refractive_water_pipeline.is_some(),
        );
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
                draw_sky_background(&mut pass, &pipeline.resources, upload);
            }
            if let Some(plan) = world_plan {
                draw_scene_plan(
                    &mut pass,
                    &pipeline.resources,
                    upload,
                    plan,
                    &pipeline.resources.uniform_bind_group,
                    upload.detail_sprites.as_ref(),
                );
            }
            if self.phy_debug_visible {
                draw_phy_debug_meshes(&mut pass, &pipeline.resources, upload);
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
                draw_sky_background(&mut pass, &pipeline.resources, upload);
            }
            if let Some(plan) = world_plan {
                draw_scene_plan_opaque(
                    &mut pass,
                    &pipeline.resources,
                    upload,
                    plan,
                    &pipeline.resources.uniform_bind_group,
                    upload.detail_sprites.as_ref(),
                );
            }
            if self.phy_debug_visible {
                draw_phy_debug_meshes(&mut pass, &pipeline.resources, upload);
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
            pass.set_bind_group(0, &pipeline.resources.uniform_bind_group, &[]);
            if let Some(plan) = world_plan {
                draw_scene_plan_transparent(
                    &mut pass,
                    &pipeline.resources,
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
        pass.set_pipeline(&pipeline.resources.blit_pipeline);
        pass.set_bind_group(0, &targets.blit_bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

/// The viewer's GPU state, one field per lifecycle: resources live as long
/// as the device, targets as long as the viewport keeps its size, uploads
/// as long as their preview draws, and draw plans for the frame being
/// encoded.
#[derive(Debug)]
pub struct ModelPipeline {
    resources: RenderResources,
    targets: Option<RenderTargets>,
    uploads: UploadCache,
    draw_plans: Option<DrawPlans>,
}

impl shader::Pipeline for ModelPipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        Self {
            resources: RenderResources::new(device, queue, format),
            targets: None,
            uploads: UploadCache::default(),
            draw_plans: None,
        }
    }

    fn trim(&mut self) {
        self.uploads.trim();
    }
}

#[cfg(test)]
mod tests {
    use iced::Point;

    use super::super::test_support::empty_preview;
    use super::{
        Arc, Camera, DrawItem, DrawPlan, FrameLayout, MaterialSlot, MeshData, ModelPipeline,
        ModelPrimitive, ModelVertex, Rectangle, RenderMode, Uniforms, Vec3, Viewport, frame_layout,
        shader, wgpu,
    };

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
    /// Fragment shading must not flicker per pixel on meshes with a constant
    /// white vertex color. Regression test for the `all(input.color ==
    /// vec3(1.0))` exact float compare in `model_viewer.wgsl`: attribute
    /// interpolation is not required to reproduce 1.0 exactly per fragment,
    /// and on NVIDIA hardware the compare flickered pixel-by-pixel between
    /// the ambient/diffuse and vertex-color-modulate branches, rendering
    /// every textured surface as salt-and-pepper noise. Renders an angled
    /// quad with constant color/UV through the real ModelPipeline and
    /// asserts the interior shades uniformly. (Only catches the regression
    /// on GPUs with inexact constant interpolation; exact GPUs pass either
    /// way.)
    #[test]
    fn constant_white_vertex_color_shades_uniformly() {
        const WIDTH: u32 = 512;
        const HEIGHT: u32 = 384;

        // GL only: naga's GLSL backend cannot translate the refractive water
        // shader's `textureLoad` on a depth texture, so ModelPipeline::new
        // panics on driverless machines (CI) that fall back to GL. Restrict
        // to the primary backends and take the skip path instead.
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::default()
        });
        let Ok(adapter) = futures::executor::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        ) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let (device, queue) =
            futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("test.uniform_shading"),
                ..wgpu::DeviceDescriptor::default()
            }))
            .expect("device");

        // Desk-top-like angled quad: constant white color, constant UV, so
        // every fragment must shade identically.
        let vertex = |x: f32, y: f32| ModelVertex {
            position: Vec3::new(x, y, 20.0),
            normal: Vec3::new(0.0, 0.0, 1.0),
            uv: [0.25, 0.25],
            lightmap_uv: [0.0; 2],
            color: Vec3::splat(1.0),
            blend_alpha: 0.0,
        };
        let mesh = MeshData {
            vertices: vec![
                vertex(-48.0, -24.0),
                vertex(48.0, -24.0),
                vertex(48.0, 24.0),
                vertex(-48.0, 24.0),
            ],
            // Both windings so the quad is visible regardless of which side
            // the default orbit camera ends up on.
            indices: vec![0, 1, 2, 0, 2, 3, 2, 1, 0, 3, 2, 0],
            material_index: 0,
            bodygroup: 0,
            bodygroup_choice: 0,
        };
        let mut preview = empty_preview(Vec3::new(-48.0, -24.0, 0.0), Vec3::new(48.0, 24.0, 38.0));
        preview.meshes = vec![mesh];
        preview.materials = vec![MaterialSlot {
            name: "test".to_owned(),
            texture: None,
            texture2: None,
            force_opaque: true,
            render_mode: RenderMode::Opaque,
        }];
        let preview = Arc::new(preview);

        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(WIDTH as f32, HEIGHT as f32));
        let clip_bounds = Rectangle::<u32> {
            x: 0,
            y: 0,
            width: WIDTH,
            height: HEIGHT,
        };
        let viewport = Viewport::with_physical_size(iced::Size::new(WIDTH, HEIGHT), 1.0);
        let mut camera = Camera::default();
        camera.ensure_spawn(1, None);
        let primitive = ModelPrimitive {
            skin_remap: vec![0],
            bodygroup_choices: Vec::new(),
            map_skybox_visible: false,
            visibility_culling: false,
            phy_debug_visible: false,
            uniforms: Uniforms::for_model(&preview, &camera, bounds),
            submerged: false,
            map_skybox_uniforms: None,
            sky_uniforms: None,
            door_poses: Vec::new(),
            model: preview,
            content_id: 1,
        };
        let mut pipeline_state = <ModelPipeline as shader::Pipeline>::new(&device, &queue, format);
        shader::Primitive::prepare(
            &primitive,
            &mut pipeline_state,
            &device,
            &queue,
            &bounds,
            &viewport,
        );

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test.uniform_shading.target"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("test.uniform_shading.clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLUE),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        shader::Primitive::render(
            &primitive,
            &pipeline_state,
            &mut encoder,
            &target_view,
            &clip_bounds,
        );
        let padded_row = (WIDTH * 4).div_ceil(256) * 256;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test.uniform_shading.readback"),
            size: u64::from(padded_row) * u64::from(HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        receiver
            .recv()
            .expect("map_async callback")
            .expect("map readback");
        let mapped = readback.slice(..).get_mapped_range();
        let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
        for y in 0..HEIGHT {
            let start = (y * padded_row) as usize;
            pixels.extend_from_slice(&mapped[start..start + (WIDTH * 4) as usize]);
        }
        drop(mapped);
        readback.unmap();

        // Quad pixels are whatever isn't the blue clear color; erode by
        // requiring the 4-neighborhood to also be quad so edge antialiasing
        // and silhouette pixels don't count.
        let is_quad = |x: u32, y: u32| {
            let offset = ((y * WIDTH + x) * 4) as usize;
            let pixel = &pixels[offset..offset + 4];
            !(pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 255)
        };
        let mut min_rgb = [u8::MAX; 3];
        let mut max_rgb = [u8::MIN; 3];
        let mut interior = 0_usize;
        for y in 1..HEIGHT - 1 {
            for x in 1..WIDTH - 1 {
                if !(is_quad(x, y)
                    && is_quad(x - 1, y)
                    && is_quad(x + 1, y)
                    && is_quad(x, y - 1)
                    && is_quad(x, y + 1))
                {
                    continue;
                }
                interior += 1;
                let offset = ((y * WIDTH + x) * 4) as usize;
                for channel in 0..3 {
                    let value = pixels[offset + channel];
                    min_rgb[channel] = min_rgb[channel].min(value);
                    max_rgb[channel] = max_rgb[channel].max(value);
                }
            }
        }
        assert!(
            interior > 1000,
            "quad did not render (interior={interior}); harness is broken"
        );
        let spread: Vec<u8> = (0..3)
            .map(|channel| max_rgb[channel].saturating_sub(min_rgb[channel]))
            .collect();
        assert!(
            spread.iter().all(|&value| value <= 2),
            "shading is not uniform across the quad: rgb spread {spread:?} over {interior} pixels \
             (min {min_rgb:?}, max {max_rgb:?}) — fragment branches are flickering per pixel"
        );
    }
}
