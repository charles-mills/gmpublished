use super::{
    AMBIENT, Arc, BLIT_SHADER_SOURCE, BcFormat, CHECKERBOARD_BYTES, CHECKERBOARD_MIP_1X1_BYTES,
    CHECKERBOARD_MIP_2X2_BYTES, CHECKERBOARD_MIP_4X4_BYTES, CHECKERBOARD_MIP_RGBA,
    CHECKERBOARD_SIZE, CHECKERBOARD_SIZE_USIZE, Camera, DETAIL_SHADER_SOURCE,
    DETAIL_VERTEX_ATTRIBUTES, DETAIL_VERTEX_FLOAT_COUNT, DetailSprite, DoorInstance,
    DoorRenderPose, DrawItem, DrawPlan, DrawPlans, FOV_Y, FlyCamera, MATERIAL_ANISOTROPY_CLAMP,
    MODEL_VERTEX_ATTRIBUTES, MSAA_SAMPLE_COUNT, MapFog, MapPreview, MapSkyCamera,
    MapVisibilityBucket, MaterialSlot, MeshData, ModelPreview, ModelVertex, OverlayDrawItem,
    OverlayPrimitive, PHY_DEBUG_MATERIAL_NAME, PHY_DEBUG_RGBA, Rectangle, RenderMode, RenderScene,
    ResolvedBcMip, ResolvedTexture, SHADER_SOURCE, SKY_SHADER_SOURCE, SKYBOX_FACE_COUNT, SOURCE_UP,
    Skybox, SkyboxFace, TextureUploadLevel, Viewport, WATER_SHADER_SOURCE, WorldVisibilityPlan,
    bc_mip_is_valid, bc_texture_format, decode_bc_texture, half_extent, initial_door_swing,
    look_at, mat_mul, mid, perspective, prepare_draw_plans, shader, skybox_eye,
    transform_door_vertices, wgpu, write_bc_texture_level, write_texture_level,
};
use gmpublished_backend::model_decompile::PreviewPose;
use gmpublished_domain::math::Vec3;

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

#[derive(Debug)]
pub(super) enum PreviewScene {
    Model(Arc<ModelPreview>),
    Map(Arc<MapPreview>),
}

impl PreviewScene {
    fn render_scene(&self) -> &RenderScene {
        match self {
            Self::Model(model) => &model.scene,
            Self::Map(map) => &map.scene,
        }
    }

    fn map(&self) -> Option<&MapPreview> {
        match self {
            Self::Model(_) => None,
            Self::Map(map) => Some(map),
        }
    }
}

#[derive(Debug)]
pub(in super::super) struct StagePrimitive {
    pub local_bounds: Option<crate::media::preview_model::ModelBounds>,
    pub model: Arc<ModelPreview>,
    pub animation_pose: Option<PreviewPose>,
    pub content_id: u64,
    pub transform: super::super::state::StageTransform,
    pub skin_remap: Vec<u16>,
    pub bodygroup_choices: Vec<usize>,
    pub phy_debug_visible: bool,
}

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent render layers and camera water state"
)]
pub struct ModelPrimitive {
    pub(super) stage: Option<StagePrimitive>,
    pub(super) preview: PreviewScene,
    pub(super) animation_pose: Option<PreviewPose>,
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

impl ModelPrimitive {
    fn draw_stage<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        pipeline: &'a ModelPipeline,
        transparent: bool,
    ) {
        let Some(stage) = &self.stage else {
            return;
        };
        let Some(upload) = pipeline.stage_uploads.get(stage.content_id) else {
            return;
        };
        let Some(plans) = pipeline
            .stage_plans
            .as_ref()
            .filter(|plans| plans.content_id == stage.content_id)
        else {
            return;
        };
        let resources = &pipeline.resources;
        if transparent {
            pass.set_bind_group(0, &resources.stage_uniform_bind_group, &[]);
            draw_scene_plan_transparent(
                pass,
                resources,
                upload,
                &plans.world,
                &resources.stage_uniform_bind_group,
                None,
            );
            if stage.phy_debug_visible {
                draw_phy_debug_meshes(pass, resources, upload);
            }
        } else {
            draw_scene_plan_opaque(
                pass,
                resources,
                upload,
                &plans.world,
                &resources.stage_uniform_bind_group,
                None,
            );
        }
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
        if let Some(stage) = &self.stage {
            pipeline.stage_uploads.ensure_upload(
                device,
                queue,
                &pipeline.resources,
                stage.content_id,
                &stage.model.scene,
                None,
            );
            pipeline.stage_uploads.touch(stage.content_id);
            if let Some(upload) = pipeline.stage_uploads.get_mut(stage.content_id) {
                upload.update_animation_vertices(
                    queue,
                    &stage.model,
                    stage.animation_pose.as_ref(),
                );
                if stage.phy_debug_visible {
                    upload.ensure_phy_debug_meshes(device, queue, &stage.model.scene);
                }
                let mut uniforms = self.uniforms;
                // Rigid transforms let geometry stay uploaded: express the camera and light
                // in model space so lighting and fog use the same frame as the vertices.
                uniforms.view_proj = mat_mul(uniforms.view_proj, stage.transform.matrix());
                let eye = stage.transform.local_point(Vec3::new(
                    uniforms.camera_position[0],
                    uniforms.camera_position[1],
                    uniforms.camera_position[2],
                ));
                uniforms.camera_position = [eye[0], eye[1], eye[2], 0.0];
                let light = stage.transform.inverse_vector(Vec3::new(
                    uniforms.light[0],
                    uniforms.light[1],
                    uniforms.light[2],
                ));
                uniforms.light[..3].copy_from_slice(light.as_array());
                queue.write_buffer(
                    &pipeline.resources.stage_uniform_buffer,
                    0,
                    bytemuck::bytes_of(&uniforms),
                );
                pipeline.stage_plans = Some(prepare_draw_plans(
                    stage.content_id,
                    upload,
                    &stage.skin_remap,
                    &stage.bodygroup_choices,
                    uniforms.camera_position,
                    false,
                    None,
                ));
            }
        } else {
            pipeline.stage_plans = None;
        }
        let size = viewport.physical_size();
        let scene = self.preview.render_scene();
        let map = self.preview.map();
        pipeline.uploads.ensure_upload(
            device,
            queue,
            &pipeline.resources,
            self.content_id,
            scene,
            map,
        );
        pipeline.uploads.touch(self.content_id);
        if let Some(upload) = pipeline.uploads.get_mut(self.content_id) {
            if let PreviewScene::Model(model) = &self.preview {
                upload.update_animation_vertices(queue, model, self.animation_pose.as_ref());
            }
            if self.phy_debug_visible {
                upload.ensure_phy_debug_meshes(device, queue, scene);
            }
            if let Some(map) = map {
                upload.update_door_vertices(queue, map, self.door_poses.as_slice());
                upload.ensure_world_visibility(
                    device,
                    queue,
                    map,
                    self.visibility_culling,
                    Vec3::new(
                        self.uniforms.camera_position[0],
                        self.uniforms.camera_position[1],
                        self.uniforms.camera_position[2],
                    ),
                );
            }
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
                draw_scene_plan_opaque(
                    &mut pass,
                    &pipeline.resources,
                    upload,
                    plan,
                    &pipeline.resources.uniform_bind_group,
                    upload.detail_sprites.as_ref(),
                );
                self.draw_stage(&mut pass, pipeline, false);
                draw_scene_plan_transparent(
                    &mut pass,
                    &pipeline.resources,
                    upload,
                    plan,
                    &pipeline.resources.uniform_bind_group,
                    None,
                );
                self.draw_stage(&mut pass, pipeline, true);
            }
            if self.phy_debug_visible {
                pass.set_bind_group(0, &pipeline.resources.uniform_bind_group, &[]);
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
            self.draw_stage(&mut pass, pipeline, false);
            if self.phy_debug_visible {
                pass.set_bind_group(0, &pipeline.resources.uniform_bind_group, &[]);
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
                    &pipeline.resources.uniform_bind_group,
                    Some(&refraction.bind_group),
                );
            }
            self.draw_stage(&mut pass, pipeline, true);
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

#[derive(Debug)]
pub struct ModelPipeline {
    resources: RenderResources,
    targets: Option<RenderTargets>,
    uploads: UploadCache,
    stage_uploads: UploadCache,
    stage_plans: Option<DrawPlans>,
    draw_plans: Option<DrawPlans>,
}

impl shader::Pipeline for ModelPipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        Self {
            resources: RenderResources::new(device, queue, format),
            targets: None,
            uploads: UploadCache::default(),
            stage_uploads: UploadCache::default(),
            stage_plans: None,
            draw_plans: None,
        }
    }

    fn trim(&mut self) {
        self.uploads.trim();
        self.stage_uploads.trim();
    }
}

#[cfg(test)]
mod tests {
    use iced::Point;

    use super::super::test_support::empty_model_preview;
    use super::{
        Arc, Camera, DrawItem, DrawPlan, FrameLayout, MaterialSlot, MeshData, ModelPipeline,
        ModelPrimitive, ModelVertex, PreviewPose, PreviewScene, Rectangle, RenderMode, Uniforms,
        Vec3, Viewport, frame_layout, shader, wgpu,
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
        assert_eq!(frame_layout(Some(&plan), false), FrameLayout::SinglePass);
    }
    #[test]
    fn staged_model_renders_and_reuses_gpu_geometry_after_transform() {
        render_uniform_quad(true, false, false);
    }

    #[test]
    fn skeletal_animation_reuses_buffers_and_restores_bind_pose() {
        render_uniform_quad(false, true, false);
        render_uniform_quad(true, true, false);
    }

    #[test]
    fn lod_switch_replaces_geometry_and_keeps_selected_animation_in_both_viewers() {
        render_uniform_quad(false, true, true);
        render_uniform_quad(true, true, true);
    }

    fn render_uniform_quad(staged: bool, animated: bool, lods: bool) {
        const WIDTH: u32 = 512;
        const HEIGHT: u32 = 384;

        // Naga's GLSL backend cannot translate depth textureLoad in the water shader.
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
                // Match iced_wgpu's shared-device limits.
                required_limits: wgpu::Limits {
                    max_bind_groups: 2,
                    ..wgpu::Limits::default()
                },
                ..wgpu::DeviceDescriptor::default()
            }))
            .expect("device");

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
            // Keep the fixture visible from either side of the default orbit camera.
            indices: vec![0, 1, 2, 0, 2, 3, 2, 1, 0, 3, 2, 0],
            material_index: 0,
            bodygroup: 0,
            bodygroup_choice: 0,
        };
        let mut preview =
            empty_model_preview(Vec3::new(-48.0, -24.0, 0.0), Vec3::new(48.0, 24.0, 38.0));
        let scene = Arc::make_mut(&mut preview.scene);
        scene.meshes = vec![mesh];
        scene.materials = vec![MaterialSlot {
            name: "test".to_owned(),
            texture: None,
            texture2: None,
            force_opaque: true,
            render_mode: RenderMode::Opaque,
        }];
        let preview = if lods {
            crate::model_animation_test_support::lod_blended_model()
        } else if animated {
            crate::model_animation_test_support::blended_model()
        } else {
            Arc::new(preview)
        };

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
        if animated {
            camera.orbit.set_distance(1.6);
        }
        let mut primitive = ModelPrimitive {
            animation_pose: None,
            stage: staged.then(|| super::StagePrimitive {
                local_bounds: preview.visible_bounds(None, &[]),
                animation_pose: None,
                model: Arc::clone(&preview),
                content_id: 2,
                transform: crate::features::file_preview::state::StageTransform::default(),
                skin_remap: vec![0],
                bodygroup_choices: Vec::new(),
                phy_debug_visible: false,
            }),
            skin_remap: vec![0],
            bodygroup_choices: Vec::new(),
            map_skybox_visible: false,
            visibility_culling: false,
            phy_debug_visible: false,
            uniforms: Uniforms::for_model(&preview, &camera, bounds, None),
            submerged: false,
            map_skybox_uniforms: None,
            sky_uniforms: None,
            door_poses: Vec::new(),
            preview: PreviewScene::Model(if staged {
                Arc::new(empty_model_preview(
                    preview.scene.bounds_min,
                    preview.scene.bounds_max,
                ))
            } else {
                Arc::clone(&preview)
            }),
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

        if animated {
            let cache = if staged {
                &pipeline_state.stage_uploads
            } else {
                &pipeline_state.uploads
            };
            let id = if staged { 2 } else { 1 };
            let buffer = cache.get(id).unwrap().meshes[0].vertices.clone();
            let centroid = cache.get(id).unwrap().meshes[0].centroid;
            let mut animated_centroid: Option<Vec3> = None;
            for pose in [Some((1.0, 1.0)), Some((1.0, 0.0)), None, Some((0.5, 1.0))].map(|pose| {
                pose.map(|(cycle, parameter)| PreviewPose {
                    sequence: 0,
                    cycle,
                    parameters: vec![parameter],
                })
            }) {
                if let Some(stage) = primitive.stage.as_mut() {
                    stage.animation_pose = pose.clone();
                } else {
                    primitive.animation_pose = pose.clone();
                }
                shader::Primitive::prepare(
                    &primitive,
                    &mut pipeline_state,
                    &device,
                    &queue,
                    &bounds,
                    &viewport,
                );
                let cache = if staged {
                    &pipeline_state.stage_uploads
                } else {
                    &pipeline_state.uploads
                };
                let mesh = &cache.get(id).unwrap().meshes[0];
                assert_eq!(mesh.vertices, buffer);
                if let Some(pose) = &pose {
                    assert!((mesh.centroid - centroid).length() > 1.0);
                    if pose.parameters == [0.0] {
                        let first = animated_centroid.unwrap();
                        assert!((mesh.centroid - first).length() > 1.0);
                        assert!(
                            (mesh.centroid - centroid - (first - centroid) * 0.5).length() < 0.025
                        );
                    } else {
                        animated_centroid = Some(mesh.centroid);
                    }
                } else {
                    assert!((mesh.centroid - centroid).length() < 0.001);
                }
            }
        }

        if lods {
            let id = if staged { 2 } else { 1 };
            let cache = if staged {
                &pipeline_state.stage_uploads
            } else {
                &pipeline_state.uploads
            };
            let original_buffer = cache.get(id).unwrap().meshes[0].vertices.clone();
            let lower = preview.at_lod(1);
            assert_eq!(lower.scene.meshes[0].indices.len(), 3);
            if let Some(stage) = primitive.stage.as_mut() {
                stage.model = Arc::clone(&lower);
            } else {
                primitive.preview = PreviewScene::Model(Arc::clone(&lower));
            }
            shader::Primitive::prepare(
                &primitive,
                &mut pipeline_state,
                &device,
                &queue,
                &bounds,
                &viewport,
            );
            let cache = if staged {
                &pipeline_state.stage_uploads
            } else {
                &pipeline_state.uploads
            };
            let mesh = &cache.get(id).unwrap().meshes[0];
            assert_ne!(mesh.vertices, original_buffer);
            assert_eq!(mesh.index_count, 3);
            let expected = lower
                .animation
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .evaluate_lod(0, 0.5, &[1.0], 1)
                .unwrap();
            let centroid = expected[0].iter().fold(Vec3::default(), |sum, vertex| {
                sum + Vec3::from(vertex.position)
            }) / expected[0].len() as f32;
            assert!((mesh.centroid - centroid).length() < 0.001);
            let lower_buffer = mesh.vertices.clone();
            shader::Primitive::prepare(
                &primitive,
                &mut pipeline_state,
                &device,
                &queue,
                &bounds,
                &viewport,
            );
            let cache = if staged {
                &pipeline_state.stage_uploads
            } else {
                &pipeline_state.uploads
            };
            assert_eq!(cache.get(id).unwrap().meshes[0].vertices, lower_buffer);
            if let Some(stage) = primitive.stage.as_mut() {
                stage.model = Arc::clone(&preview);
            } else {
                primitive.preview = PreviewScene::Model(Arc::clone(&preview));
            }
            shader::Primitive::prepare(
                &primitive,
                &mut pipeline_state,
                &device,
                &queue,
                &bounds,
                &viewport,
            );
        }

        if staged {
            let map_vertices = pipeline_state.uploads.get(1).unwrap().meshes.len();
            let stage_vertices = pipeline_state.stage_uploads.get(2).unwrap().meshes[0]
                .vertices
                .clone();
            primitive.stage.as_mut().unwrap().transform.position = Vec3::new(4.0, 6.0, 2.0);
            primitive.stage.as_mut().unwrap().transform.angles = Vec3::new(0.0, 15.0, 0.0);
            shader::Primitive::prepare(
                &primitive,
                &mut pipeline_state,
                &device,
                &queue,
                &bounds,
                &viewport,
            );
            assert_eq!(
                pipeline_state.uploads.get(1).unwrap().meshes.len(),
                map_vertices
            );
            assert_eq!(
                pipeline_state.stage_uploads.get(2).unwrap().meshes[0].vertices,
                stage_vertices
            );
        }

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

        // Exclude the blue background and its antialiased silhouette pixels.
        let is_quad = |x: u32, y: u32| {
            let offset = ((y * WIDTH + x) * 4) as usize;
            let pixel = &pixels[offset..offset + 4];
            !(pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 255)
        };
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
            }
        }
        assert!(
            interior > if animated { 100 } else { 1000 },
            "model geometry was not rendered: {interior} interior pixels"
        );
    }
}
