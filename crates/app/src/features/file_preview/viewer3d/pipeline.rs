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
    TextureUploadLevel, Viewport, WATER_SHADER_SOURCE, WorldVisibilityPlan, add, bc_mip_is_valid,
    bc_texture_format, decode_bc_texture, half_extent, initial_door_swing, look_at, mat_mul, mid,
    perspective, prepare_draw_plans, shader, skybox_eye, transform_door_vertices, wgpu,
    write_bc_texture_level, write_texture_level,
};

mod draw;
mod materials;
mod resources;
mod targets;
mod uniforms;
mod upload;
mod visibility;

#[cfg(test)]
pub use materials::checkerboard_mip_levels;
#[cfg(test)]
pub use resources::skybox_face_corners;
pub use uniforms::Uniforms;
#[cfg(test)]
pub use uniforms::average_srgb_rgba;
pub use upload::UploadedModel;
#[cfg(test)]
pub use visibility::{VisibilityClusterState, VisibilityClusterTracker};

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
            uniforms.water_time_sky_tint[1..].copy_from_slice(&sky_tint);
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
