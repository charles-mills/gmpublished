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
use gmpublished_domain::math::Vec3;

mod draw;
mod materials;
mod passes;
mod reflections;
mod resources;
mod targets;
mod uniforms;
mod upload;
mod visibility;

pub use uniforms::Uniforms;
pub use upload::UploadedModel;

use super::draw_plan::{DrawPlanRequest, DrawPlanSelection};
use reflections::{ReflectionView, SceneView, ViewUniforms};
use resources::RenderResources;
use std::sync::atomic::Ordering;
use targets::{RenderTargets, water_bind_group};
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "three independent render toggles plus the camera's water state"
)]
pub struct ModelPrimitive {
    #[cfg(feature = "map-capture")]
    pub(super) reflection_target: bool,
    #[cfg(feature = "map-capture")]
    pub(super) capture_diagnostics: Option<Arc<std::sync::Mutex<serde_json::Value>>>,
    pub(super) prepared_frame: std::sync::atomic::AtomicUsize,
    pub(super) skybox_transform: Option<(Vec3, f32)>,
    pub(super) preview: PreviewScene,
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

#[derive(Debug)]
pub struct PreparedFrame {
    pub main: SceneView,
    pub reflections: Vec<ReflectionView>,
    pub content_id: u64,
    pub bounds: Rectangle,
    pub live: bool,
}

impl shader::Primitive for ModelPrimitive {
    type Pipeline = ModelPipeline;
    fn prepare(
        &self,
        pipeline: &mut ModelPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
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
        let slot = pipeline
            .frames
            .iter()
            .position(|frame| !frame.live)
            .unwrap_or(pipeline.frames.len());
        if slot == pipeline.frames.len() {
            pipeline.frames.push(PreparedFrame {
                main: SceneView::new(device, &pipeline.resources),
                reflections: Vec::new(),
                content_id: self.content_id,
                bounds: *bounds,
                live: false,
            });
        }
        self.prepared_frame.store(slot, Ordering::Relaxed);
        let frame = &mut pipeline.frames[slot];
        if frame.content_id != self.content_id {
            frame.reflections.clear();
            frame.main.visibility = Default::default();
        }
        frame.content_id = self.content_id;
        frame.live = true;
        let scale = viewport.scale_factor();
        frame.bounds = Rectangle {
            x: bounds.x * scale,
            y: bounds.y * scale,
            width: bounds.width * scale,
            height: bounds.height * scale,
        };
        let size = (
            frame.bounds.width.ceil().max(1.0) as u32,
            frame.bounds.height.ceil().max(1.0) as u32,
        );
        let upload = pipeline
            .uploads
            .get_mut(self.content_id)
            .expect("uploaded scene");
        if self.phy_debug_visible {
            upload.ensure_phy_debug_meshes(device, queue, scene);
        }
        if let Some(map) = map {
            upload.update_door_vertices(queue, map, &self.door_poses);
            frame.main.visibility.ensure_world_visibility(
                device,
                queue,
                map,
                upload,
                self.visibility_culling,
                Vec3::from([
                    self.uniforms.camera_position[0],
                    self.uniforms.camera_position[1],
                    self.uniforms.camera_position[2],
                ]),
            );
        }
        let with_time = |mut uniforms: Uniforms| {
            uniforms.water_time_sky_tint[0] = self.uniforms.water_time_sky_tint[0];
            uniforms.water_time_sky_tint[1..].copy_from_slice(upload.sky_tint.as_array());
            if self.submerged {
                uniforms.fog_color = self.uniforms.fog_color;
                uniforms.fog_params = self.uniforms.fog_params;
            }
            uniforms
        };
        frame.main.world.write(queue, &with_time(self.uniforms));
        if let Some(sky) = self.sky_uniforms {
            frame.main.sky.write(queue, &with_time(sky));
        }
        if let Some(skybox) = self.map_skybox_uniforms {
            frame.main.skybox.write(queue, &with_time(skybox));
        }
        let plans = prepare_draw_plans(
            upload,
            DrawPlanRequest {
                content_id: self.content_id,
                selection: DrawPlanSelection {
                    skin_remap: &self.skin_remap,
                    bodygroup_choices: &self.bodygroup_choices,
                },
                camera_position: self.uniforms.camera_position,
                map_skybox_visible: self.map_skybox_visible,
                map_skybox_content_present: upload.has_map_skybox_content(),
                map_skybox_camera_position: self
                    .map_skybox_uniforms
                    .map(|uniforms| uniforms.camera_position),
                visibility_plan: frame.main.visibility.plan.as_ref(),
            },
        );
        let needs_water = !plans.world.planar.is_empty()
            && pipeline.resources.refractive_water_pipeline.is_some();
        RenderTargets::ensure(
            &mut frame.main.targets,
            device,
            &pipeline.resources,
            size.0,
            size.1,
            needs_water,
            &frame.main.world.buffer,
        );
        let eye = Vec3::from([
            self.uniforms.camera_position[0],
            self.uniforms.camera_position[1],
            self.uniforms.camera_position[2],
        ]);
        let active = upload
            .reflectors
            .iter()
            .enumerate()
            .filter(|(_, surface)| {
                needs_water
                    && surface.plane.signed_distance(eye) > 0.01
                    && surface.intersects_view(self.uniforms.view_proj)
                    && plans
                        .world
                        .planar
                        .iter()
                        .any(|item| item.mesh_index == surface.mesh_index)
            })
            .map(|(index, surface)| {
                let visible = frame.main.visibility.plan.as_ref().map(|plan| {
                    plan.mesh_indices
                        .get(upload.meshes[surface.mesh_index].scene_mesh_index)
                        .map_or(&[][..], Vec::as_slice)
                });
                (index, surface.visible_indices(visible))
            })
            .filter(|(_, indices)| !indices.is_empty())
            .collect::<Vec<_>>();
        let reflection_size =
            reflections::target_size(size, active.len(), pipeline.resources.target_format);
        frame
            .reflections
            .retain(|view| active.iter().any(|(index, _)| *index == view.surface_index));
        for (surface_index, indices) in active {
            if !frame
                .reflections
                .iter()
                .any(|view| view.surface_index == surface_index)
            {
                frame.reflections.push(ReflectionView {
                    surface_index,
                    surface_indices: Vec::new(),
                    surface_buffer: visibility::upload_visible_indices(device, queue, &[]),
                    view: SceneView::new(device, &pipeline.resources),
                    water_bind_group: None,
                    surface_uniforms: ViewUniforms::new(device, &pipeline.resources),
                });
            }
            let view = frame
                .reflections
                .iter_mut()
                .find(|view| view.surface_index == surface_index)
                .unwrap();
            if view.surface_indices != indices {
                view.surface_buffer = visibility::upload_visible_indices(device, queue, &indices);
                view.surface_indices = indices;
            }
        }
        for reflection in &mut frame.reflections {
            reflection.prepare(
                device,
                queue,
                &pipeline.resources,
                self,
                upload,
                reflection_size,
            );
            let mut uniforms = with_time(self.uniforms);
            uniforms.water_depth_params[2] = 1.0;
            reflection.surface_uniforms.write(queue, &uniforms);
            let main = frame.main.targets.as_ref().expect("main targets");
            let refraction = main.refraction.as_ref().expect("water refraction target");
            let refract_view = refraction.texture.create_view(&Default::default());
            let reflected = reflection
                .view
                .targets
                .as_ref()
                .expect("reflection targets");
            reflection.water_bind_group = Some(water_bind_group(
                device,
                &pipeline.resources,
                &reflection.surface_uniforms.buffer,
                &refract_view,
                &main.depth,
                &reflected.resolve_view,
            ));
        }
        frame.main.plans = Some(plans);
        #[cfg(feature = "map-capture")]
        if let Some(diagnostics) = &self.capture_diagnostics {
            let reflections=frame.reflections.iter().map(|view| {
                let surface=&upload.reflectors[view.surface_index];
                let plans=view.view.plans.as_ref().unwrap();
                serde_json::json!({"material":scene.materials[surface.material_slot].name,
                    "plane_normal":surface.plane.normal.to_array(),"plane_distance":surface.plane.distance,
                    "reflected_eye":surface.plane.reflect_point(eye).to_array(),
                    "world_opaque_meshes":plans.world.opaque.len(),
                    "world_entity_meshes":plans.world.opaque.iter().filter(|item|upload.meshes[item.mesh_index].entity).count(),
                    "skybox_present":plans.map_skybox.is_some(),
                    "visibility_state":format!("{:?}",view.view.visibility.tracker.state),
                    "target_dimensions":view.view.targets.as_ref().unwrap().size})
            }).collect::<Vec<_>>();
            *diagnostics.lock().expect("capture diagnostics lock") = serde_json::json!({"main_target_dimensions":size,"reflection_views":reflections,"single_bounce":true,"nested_planar_fallback":"average sky tint (cubemap pending)","estimated_target_bytes":frame.main.targets.as_ref().unwrap().estimated_bytes()+frame.reflections.iter().map(|view| view.view.targets.as_ref().unwrap().estimated_bytes()).sum::<u64>()});
        }
    }
    fn render(
        &self,
        pipeline: &ModelPipeline,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        let Some(frame) = pipeline
            .frames
            .get(self.prepared_frame.load(Ordering::Relaxed))
            .filter(|frame| frame.content_id == self.content_id)
        else {
            return;
        };
        let Some(upload) = pipeline.uploads.get(self.content_id) else {
            return;
        };
        passes::render_frame(
            self,
            frame,
            &pipeline.resources,
            upload,
            encoder,
            target,
            clip_bounds,
        );
    }
}

#[derive(Debug)]
pub struct ModelPipeline {
    resources: RenderResources,
    uploads: UploadCache,
    frames: Vec<PreparedFrame>,
}
impl shader::Pipeline for ModelPipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        Self {
            resources: RenderResources::new(device, queue, format),
            uploads: UploadCache::default(),
            frames: Vec::new(),
        }
    }
    fn trim(&mut self) {
        self.uploads.trim();
        self.frames.retain(|frame| frame.live);
        for frame in &mut self.frames {
            frame.live = false;
        }
    }
}
