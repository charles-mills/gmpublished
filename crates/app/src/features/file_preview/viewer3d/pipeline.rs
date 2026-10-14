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
mod lightmaps;
mod materials;
mod passes;
mod reflections;
mod refraction;
mod resources;
mod screen_refraction;
mod targets;
mod transparency;
mod uniforms;
mod upload;
mod visibility;

pub use uniforms::Uniforms;
pub use upload::UploadedModel;

use super::draw_plan::{DrawPlanRequest, DrawPlanSelection};
use reflections::{PlanarView, SceneView, ViewUniforms};
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
    pub(super) clear_color: Option<[f32; 3]>,
    pub(super) air_fog: Option<super::MapFog>,
    #[cfg(feature = "map-capture")]
    pub(super) reflection_target: bool,
    #[cfg(feature = "map-capture")]
    pub(super) refraction_target: bool,
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
    pub lightmap: lightmaps::Frame,
    pub screen_overlay: Option<screen_refraction::Frame>,
    pub main: SceneView,
    pub planar_views: Vec<PlanarView>,
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
            let lightmap = lightmaps::Frame::new(
                device,
                queue,
                &pipeline.resources,
                map.and_then(|map| map.lightmap.as_ref()),
            );
            pipeline.frames.push(PreparedFrame {
                screen_overlay: None,
                main: SceneView::new(device, &pipeline.resources, &lightmap),
                lightmap,
                planar_views: Vec::new(),
                content_id: self.content_id,
                bounds: *bounds,
                live: false,
            });
        }
        self.prepared_frame.store(slot, Ordering::Relaxed);
        let frame = &mut pipeline.frames[slot];
        if frame.content_id != self.content_id {
            frame.planar_views.clear();
            frame.screen_overlay = None;
            frame.lightmap = lightmaps::Frame::new(
                device,
                queue,
                &pipeline.resources,
                map.and_then(|map| map.lightmap.as_ref()),
            );
            frame.main = SceneView::new(device, &pipeline.resources, &frame.lightmap);
        }
        frame.lightmap.update(
            queue,
            map.and_then(|map| map.lightmap.as_ref()),
            self.uniforms.water_time_sky_tint[0],
        );
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
            uniforms.viewport = [size.0 as f32, size.1 as f32, self.uniforms.viewport[2], 0.0];
            uniforms.water_time_sky_tint[0] = self.uniforms.water_time_sky_tint[0];
            uniforms.water_time_sky_tint[1..].copy_from_slice(upload.sky_tint.as_array());
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
        RenderTargets::ensure(&mut frame.main.targets, device, &pipeline.resources, size);
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
                    && {
                        let distance = surface.plane.signed_distance(eye);
                        let water = scene.materials[surface.material_slot].shader.water();
                        if distance.abs() <= 0.01
                            && let Some(water) = water
                        {
                            water.above_water != self.submerged
                        } else {
                            distance > 0.01
                        }
                    }
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
        let pass_count = active
            .iter()
            .map(|(index, _)| {
                let surface = &upload.reflectors[*index];
                let (reflect, refract) =
                    reflections::optical_passes(&scene.materials[surface.material_slot].shader);
                usize::from(reflect) + usize::from(refract)
            })
            .sum();
        let reflection_size =
            reflections::target_size(size, pass_count, pipeline.resources.target_format);
        frame
            .planar_views
            .retain(|view| active.iter().any(|(index, _)| *index == view.surface_index));
        for (surface_index, indices) in active {
            if !frame
                .planar_views
                .iter()
                .any(|view| view.surface_index == surface_index)
            {
                frame.planar_views.push(PlanarView {
                    surface_index,
                    surface_indices: Vec::new(),
                    surface_buffer: visibility::upload_visible_indices(device, queue, &[]),
                    view: SceneView::new(device, &pipeline.resources, &frame.lightmap),
                    reflect_enabled: false,
                    refraction: None,
                    refraction_clear: wgpu::Color::BLACK,
                    water_bind_group: None,
                    surface_uniforms: ViewUniforms::new(
                        device,
                        &pipeline.resources,
                        &frame.lightmap,
                    ),
                });
            }
            let view = frame
                .planar_views
                .iter_mut()
                .find(|view| view.surface_index == surface_index)
                .unwrap();
            if view.surface_indices != indices {
                view.surface_buffer = visibility::upload_visible_indices(device, queue, &indices);
                view.surface_indices = indices;
            }
        }
        for surface in &mut frame.planar_views {
            surface.prepare_optics(
                device,
                queue,
                &pipeline.resources,
                self,
                upload,
                refraction::OpticalInputs {
                    size: reflection_size,
                    lightmap: &frame.lightmap,
                },
            );
            let mut uniforms = with_time(self.uniforms);
            uniforms.water_depth_params[2] = f32::from(surface.reflect_enabled);
            uniforms.water_depth_params[3] = f32::from(surface.refraction.is_some());
            if scene.materials[upload.reflectors[surface.surface_index].material_slot]
                .shader
                .water()
                .is_some()
            {
                let plane = upload.reflectors[surface.surface_index].plane;
                uniforms.medium_plane = [
                    plane.normal[0],
                    plane.normal[1],
                    plane.normal[2],
                    -plane.distance,
                ];
            }
            surface.surface_uniforms.write(queue, &uniforms);
            let refracted = surface
                .refraction
                .as_ref()
                .or_else(|| surface.reflect_enabled.then_some(&surface.view))
                .and_then(|view| view.targets.as_ref())
                .expect("active planar input");
            let reflected = surface.view.targets.as_ref().unwrap_or(refracted);
            surface.water_bind_group = Some(water_bind_group(
                device,
                &pipeline.resources,
                &surface.surface_uniforms.buffer,
                &refracted.resolve_view,
                &refracted.depth,
                &reflected.resolve_view,
                &frame.lightmap,
            ));
        }
        screen_refraction::prepare(
            &mut frame.screen_overlay,
            device,
            queue,
            &pipeline.resources,
            upload,
            self,
        );
        frame.main.plans = Some(plans);
        let detail_water_side = frame
            .planar_views
            .iter()
            .any(|view| {
                scene.materials[upload.reflectors[view.surface_index].material_slot]
                    .shader
                    .water()
                    .is_some()
            })
            .then_some(self.submerged);
        transparency::prepare(
            &mut frame.main,
            upload,
            with_time(self.uniforms),
            self.map_skybox_uniforms,
            detail_water_side,
        );
        #[cfg(feature = "map-capture")]
        if let Some(diagnostics) = &self.capture_diagnostics {
            let records = |refracted: bool| {
                frame.planar_views.iter().filter_map(|surface_view| {
                let view=if refracted {surface_view.refraction.as_ref()?} else {
                    if !surface_view.reflect_enabled {return None;}
                    &surface_view.view
                };
                let surface=&upload.reflectors[surface_view.surface_index];
                let plans=view.plans.as_ref().unwrap();
                Some(serde_json::json!({
                    "material":scene.materials[surface.material_slot].name,
                    "plane_normal":surface.plane.normal.to_array(),"plane_distance":surface.plane.distance,
                    "reflected_eye":surface.plane.reflect_point(eye).to_array(),
                    "camera_position":if refracted {eye.to_array()} else {surface.plane.reflect_point(eye).to_array()},
                    "transparent_primitives":view.transparency.len(),
                    "world_opaque_meshes":plans.world.opaque.len(),
                    "world_translucent_meshes":plans.world.translucent.len(),
                    "world_entity_meshes":plans.world.opaque.iter().filter(|item|upload.meshes[item.mesh_index].entity).count(),
                    "skybox_present":plans.map_skybox.is_some(),
                    "visibility_state":format!("{:?}",view.visibility.tracker.state),
                    "target_dimensions":view.targets.as_ref().unwrap().size
                }))
            }).collect::<Vec<_>>()
            };
            let bytes = frame.main.targets.as_ref().unwrap().estimated_bytes()
                + frame
                    .planar_views
                    .iter()
                    .map(|surface| {
                        surface
                            .view
                            .targets
                            .as_ref()
                            .map_or(0, RenderTargets::estimated_bytes)
                            + surface
                                .refraction
                                .as_ref()
                                .and_then(|view| view.targets.as_ref())
                                .map_or(0, RenderTargets::estimated_bytes)
                    })
                    .sum::<u64>();
            *diagnostics.lock().expect("capture diagnostics lock") = serde_json::json!({
                "main_target_dimensions":size,"reflection_views":records(false),"refraction_views":records(true),
                "single_bounce":true,"nested_planar_fallback":"average sky tint (cubemap pending)",
                "water_body":map.and_then(|map|map.water_volumes.as_ref()).and_then(|volumes|volumes.at(eye)).map(|sample|serde_json::json!({"index":sample.body_index,"surface_z":sample.body.surface_z,"min_z":sample.body.min_z,"surface_material":sample.body.material_name,"contents":sample.contents})),
                "underwater_overlay":map.and_then(|map|map.immersion_material(eye)).and_then(|water|water.underwater_overlay.as_ref()).map(|effect|&effect.name),
                "clear_color_linear":self.clear_color,
                "main_camera_position":self.uniforms.camera_position, "main_fog_color":self.uniforms.fog_color, "main_fog_params":self.uniforms.fog_params, "main_clip_plane":self.uniforms.clip_plane, "main_visibility_state":format!("{:?}",frame.main.visibility.tracker.state),
                "estimated_target_bytes":bytes,"composition":"planar depth occludes main transparency; clipped refraction includes below-plane transparency"
            });
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
