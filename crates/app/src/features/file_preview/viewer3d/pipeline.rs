use super::camera::ViewId;
#[cfg(test)]
use super::look_to;
use super::{
    AMBIENT, Arc, BLIT_SHADER_SOURCE, CHECKERBOARD_BYTES, CHECKERBOARD_MIP_1X1_BYTES,
    CHECKERBOARD_MIP_2X2_BYTES, CHECKERBOARD_MIP_4X4_BYTES, CHECKERBOARD_MIP_RGBA,
    CHECKERBOARD_SIZE, CHECKERBOARD_SIZE_USIZE, Camera, DETAIL_SHADER_SOURCE,
    DETAIL_VERTEX_ATTRIBUTES, DETAIL_VERTEX_FLOAT_COUNT, DetailSprite, DoorInstance,
    DoorRenderPose, DrawItem, DrawPlan, DrawPlans, FOV_Y, FlyCamera, MODEL_VERTEX_ATTRIBUTES,
    MapFog, MapPreview, MapSkyCamera, MapVisibilityBucket, MaterialSlot, MeshData, ModelPreview,
    ModelVertex, OverlayDrawItem, OverlayPrimitive, PHY_DEBUG_MATERIAL_NAME, PHY_DEBUG_RGBA,
    Rectangle, RenderMode, RenderScene, ResolvedTexture, SHADER_SOURCE, SKY_SHADER_SOURCE,
    SKYBOX_FACE_COUNT, SOURCE_UP, Skybox, SkyboxFace, TextureUploadLevel, Viewport,
    WATER_SHADER_SOURCE, WorldVisibilityPlan, half_extent, initial_door_swing, look_at, mat_mul,
    mid, perspective, shader, skybox_eye, transform_door_vertices, wgpu,
};
use gmpublished_domain::math::Vec3;

mod admission;
mod background_bindings;
mod bindings;
mod bounds_query;
mod clipping;
mod completion;
pub use completion::ShutdownError;

pub fn finish_gpu_shutdown() -> Result<(), ShutdownError> {
    completion::finish_application_shutdown()
}

mod cubemap;
mod draw;
mod frame_selection;
mod geometry;
mod glass;
mod images;
mod intersection;
mod lighting;
mod lightmap_demand;
mod lightmap_ranges;
mod lightmaps;
mod marks;
mod materials;

mod model_lighting;
mod model_lighting_visibility;
mod optical_planning;
mod passes;
mod planar;
mod presentation;
mod reflections;
mod refraction;
mod residency;
mod residency_demand;
mod residency_hash;
mod resources;
mod samplers;
pub(super) mod sampling;
mod screen_refraction;
mod skinning;
mod sky;
mod sun;
mod surface;
mod surface_pipelines;
mod targets;
mod texture_barriers;
pub(super) mod texture_upload;
mod transparency;
mod transparent_indirect;
mod uniforms;
mod upload;
pub(super) mod view_camera;
mod view_preparation;
mod visibility;
mod water_clip;

pub use uniforms::Uniforms;

use super::draw_plan::{DrawPlanRequest, DrawPlanSelection};
pub(super) use geometry::ViewerScene;
use presentation::{OutputKey, Readiness, SubmissionHistory, ViewOutput};
use reflections::{PlanarView, SceneView, ViewUniforms};
use resources::RenderResources;
use std::sync::atomic::Ordering;
use targets::RenderTargets;
use upload::UploadCache;
pub(super) use upload::{UploadedMapRenderable, UploadedMesh, UploadedModel, UploadedReceiver};

#[derive(Clone, Debug)]
pub(super) enum PreviewScene {
    Model(Arc<ModelPreview>),
    Map(Arc<MapPreview>),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SceneKey {
    Model(usize),
    Map(usize),
}

impl PreviewScene {
    fn key(&self) -> SceneKey {
        match self {
            Self::Model(model) => SceneKey::Model(Arc::as_ptr(model) as usize),
            Self::Map(map) => SceneKey::Map(Arc::as_ptr(map) as usize),
        }
    }

    fn model_animations(&self) -> &[crate::media::preview_model::ModelAnimationInstance] {
        match self {
            Self::Model(model) => &model.model_animations,
            Self::Map(map) => &map.model_animations,
        }
    }

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
    pub(super) cpu_executor: Option<gmpublished_backend::CpuExecutor>,
    pub(super) render_feedback: Option<Arc<super::feedback::RenderFeedback>>,
    pub(super) model_lighting_time: f64,
    pub(super) animation_clock: crate::bridge::materials::AnimationClock,
    pub(super) view_id: ViewId,
    pub(super) clear_color: Option<[f32; 3]>,
    pub(super) air_fog: Option<super::MapFog>,
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
    pub(super) view_camera: Option<view_camera::ViewCamera>,
    pub(super) submerged: bool,
    pub(super) map_skybox_uniforms: Option<Uniforms>,
    pub(super) sky_uniforms: Option<Uniforms>,
    pub(super) door_poses: Vec<DoorRenderPose>,
}

#[derive(Debug)]
pub struct PreparedFrame {
    view_id: ViewId,
    scene_key: SceneKey,
    scene: ViewerScene,
    pub lighting: lighting::Frame,
    marks: marks::Frame,
    pub screen_overlay: Option<screen_refraction::Frame>,
    pub main: SceneView,
    lighting_contexts: Vec<bool>,
    water_intersection: Option<intersection::IntersectionView>,
    pub planar_views: Vec<PlanarView>,
    pub content_id: u64,
    pub bounds: Rectangle,
    pub live: bool,
    readiness: Readiness,
    admission_priority: bool,
    presentation: SubmissionHistory<OutputKey, Arc<ViewOutput>>,
    demand_cache: residency_demand::FrameDemandCache,
    binding_request: Option<bindings::FrameRequest>,
    proxy_states: bindings::ProxyStates,
    proxy_bindings: bindings::ProxyBindings,
    lightmap_request: Option<lightmaps::Request>,
    lightmap_demand_cache: lightmap_demand::Cache,
    animation_demand_cache: skinning::DemandCache,
    feedback: Option<Arc<super::feedback::RenderFeedback>>,
    pending_clock: Option<(crate::bridge::materials::AnimationClock, f32)>,
}

impl PreparedFrame {
    fn output_key(&self) -> OutputKey {
        OutputKey {
            view: self.view_id,
            scene: self.scene_key,
            size: self
                .main
                .targets
                .as_ref()
                .map_or((0, 0), |targets| targets.size),
        }
    }
}

impl ModelPrimitive {
    fn retains_output(&self) -> bool {
        true
    }
    pub(super) fn raster_offset(&self) -> [f32; 2] {
        if self.preview.map().is_some() {
            [0.5; 2]
        } else {
            [0.0; 2]
        }
    }
}

fn unclaimed_viewer_frame_slot(
    frames: impl Iterator<Item = (ViewId, bool)>,
    view_id: ViewId,
) -> usize {
    let mut count = 0;
    for (index, (identity, live)) in frames.enumerate() {
        if identity == view_id && !live {
            return index;
        }
        count = index + 1;
    }
    count
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
        if self.render_feedback.as_ref().is_some_and(|feedback| {
            feedback.reject_expired_retirement(std::time::Instant::now())
                || matches!(
                    feedback.snapshot(),
                    super::feedback::RenderStatus::RetirementTimedOut
                )
        }) {
            return;
        }
        let scene = self.preview.render_scene();
        let map = self.preview.map();
        let scene_key = self.preview.key();
        if let Err(error) = pipeline
            .resources
            .texture_uploader
            .lock()
            .expect("texture uploader lock")
            .poll_retirements()
        {
            if let Some(feedback) = &self.render_feedback {
                feedback.set(super::feedback::RenderStatus::Rejected {
                    reason: error.to_string(),
                });
            }
            return;
        }
        if let Err(error) = pipeline.uploads.ensure_upload(
            device,
            queue,
            &pipeline.resources,
            scene_key,
            &self.preview,
        ) {
            if let Some(feedback) = &self.render_feedback {
                feedback.set(super::feedback::RenderStatus::Rejected {
                    reason: error.to_string(),
                });
            }

            let _ = error;
            return;
        }
        pipeline.uploads.touch(scene_key);

        let slot = unclaimed_viewer_frame_slot(
            pipeline
                .frames
                .iter()
                .map(|frame| (frame.view_id, frame.live)),
            self.view_id,
        );
        if slot == pipeline.frames.len() {
            let lighting = lighting::Frame::new(device, map);
            let scene = ViewerScene::new(
                Arc::clone(pipeline.uploads.get(scene_key).expect("uploaded scene")),
                lightmaps::Snapshot::empty(Arc::clone(&pipeline.resources.default_lightmap)),
            );
            let marks = marks::Frame::new(device, queue, &scene);
            pipeline.frames.push(PreparedFrame {
                view_id: self.view_id,
                scene_key,
                scene,
                screen_overlay: None,
                main: SceneView::new(device, &pipeline.resources, &lighting, &marks.buffer),
                marks,
                lighting,
                lighting_contexts: Vec::new(),
                planar_views: Vec::new(),
                water_intersection: None,
                content_id: self.content_id,
                bounds: *bounds,
                live: false,
                readiness: Readiness::Loading,
                admission_priority: false,
                presentation: SubmissionHistory::default(),
                demand_cache: residency_demand::FrameDemandCache::default(),
                binding_request: None,
                proxy_states: Default::default(),
                proxy_bindings: Default::default(),
                lightmap_request: None,
                lightmap_demand_cache: lightmap_demand::Cache::default(),
                animation_demand_cache: skinning::DemandCache::default(),
                feedback: self.render_feedback.clone(),
                pending_clock: None,
            });
        }
        self.prepared_frame.store(slot, Ordering::Relaxed);
        let frame = &mut pipeline.frames[slot];
        if frame.scene_key != scene_key {
            frame.admission_priority = false;
            frame.presentation.clear();
            frame.scene_key = scene_key;
            frame.pending_clock = None;
            frame.scene = ViewerScene::new(
                Arc::clone(pipeline.uploads.get(scene_key).expect("uploaded scene")),
                lightmaps::Snapshot::empty(Arc::clone(&pipeline.resources.default_lightmap)),
            );
            frame.planar_views.clear();
            frame.water_intersection = None;
            frame.screen_overlay = None;
            frame.lighting = lighting::Frame::new(device, map);
            frame.marks = marks::Frame::new(device, queue, &frame.scene);
            frame.main = SceneView::new(
                device,
                &pipeline.resources,
                &frame.lighting,
                &frame.marks.buffer,
            );
        }
        if !Arc::ptr_eq(
            &frame.scene.assets,
            pipeline.uploads.get(scene_key).expect("uploaded scene"),
        ) {
            frame.scene.refresh_assets(Arc::clone(
                pipeline.uploads.get(scene_key).expect("uploaded scene"),
            ));
        }

        frame.content_id = self.content_id;
        frame.feedback.clone_from(&self.render_feedback);
        frame.readiness = Readiness::Loading;
        frame.live = true;
        let scale = viewport.scale_factor();
        frame.bounds = Rectangle {
            x: bounds.x * scale,
            y: bounds.y * scale,
            width: bounds.width * scale,
            height: bounds.height * scale,
        };
        let size = RenderTargets::size_for_device(
            device,
            (
                frame.bounds.width.ceil().max(1.0) as u32,
                frame.bounds.height.ceil().max(1.0) as u32,
            ),
        );
        frame.presentation.validate(&OutputKey {
            view: self.view_id,
            scene: scene_key,
            size,
        });
        if !self.retains_output() {
            frame.presentation.clear();
        }

        if self.phy_debug_visible {
            frame
                .scene
                .assets
                .ensure_phy_debug_meshes(device, queue, scene);
        }
        frame.scene.update_doors(device, queue, &self.door_poses);
        frame.marks.update(queue, &frame.scene, &self.door_poses);
        let upload = &frame.scene;
        if let Some(map) = map {
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
            uniforms.lightmap_mode = self.uniforms.lightmap_mode;
            uniforms.water_time_sky_tint[0] = self.uniforms.water_time_sky_tint[0];
            uniforms.water_time_sky_tint[1..].copy_from_slice(upload.assets.sky_tint.as_array());
            uniforms.with_raster_offset(size, self.raster_offset())
        };
        let mut main_uniforms = with_time(self.uniforms);
        let skybox_uniforms = self.map_skybox_uniforms.map(with_time);
        if let Some(sky) = self.sky_uniforms {
            let mut sky_with_metadata = with_time(sky);
            sky_with_metadata.water_time_sky_tint[0] = sky.water_time_sky_tint[0];
            frame.main.sky.write(queue, &sky_with_metadata);
        }
        if let Some(skybox) = skybox_uniforms {
            frame.main.skybox.write(queue, &skybox);
        }

        let plans = frame.main.plan_cache.prepare(
            upload,
            DrawPlanRequest {
                world_view: Some(&main_uniforms),
                skybox_view: skybox_uniforms.as_ref(),
                content_id: self.content_id,
                selection: DrawPlanSelection {
                    skin_remap: &self.skin_remap,
                    bodygroup_choices: &self.bodygroup_choices,
                },
                camera_position: self.uniforms.camera_position,
                map_skybox_visible: self.map_skybox_visible,
                map_skybox_content_present: upload.assets.has_map_skybox_content(),
                map_skybox_camera_position: self
                    .map_skybox_uniforms
                    .map(|uniforms| uniforms.camera_position),
                visibility_plan: frame.main.visibility.plan.as_ref(),
            },
            frame.main.visibility.revision,
            view_preparation::Entities::All,
        );
        let needs_water = !plans.world.planar.is_empty()
            && pipeline.resources.refractive_water_pipeline.is_some();
        RenderTargets::ensure(
            &mut frame.main.targets,
            device,
            pipeline.resources.for_view(draw::ViewKind::Main),
            size,
        );
        frame.main.sun.prepare(
            device,
            queue,
            &pipeline.resources.sun,
            &sun::gpu::Inputs {
                map,
                world: with_time(self.uniforms),
                skybox: self
                    .map_skybox_uniforms
                    .filter(|_| plans.map_skybox.is_some())
                    .map(with_time),
                targets: frame.main.targets.as_ref().unwrap(),
            },
        );
        let eye = Vec3::from([
            self.uniforms.camera_position[0],
            self.uniforms.camera_position[1],
            self.uniforms.camera_position[2],
        ]);
        let active = upload
            .assets
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
                    && surface.intersects_view(with_time(self.uniforms).view_proj)
                    && plans
                        .world
                        .planar
                        .iter()
                        .any(|item| item.mesh_index == surface.mesh_index)
            })
            .map(|(index, surface)| {
                if let Some(ranges) = plans.world.planar_world_ranges.get(&surface.mesh_index) {
                    let uploaded = &upload.meshes[surface.mesh_index];
                    let selected = scene
                        .meshes
                        .get(uploaded.scene_mesh_index)
                        .and_then(|mesh| {
                            planar::select_indices(
                                &mesh.vertices,
                                &mesh.indices,
                                Some(ranges),
                                &[],
                                transparency::WorldDrawGroups::ALL,
                                None,
                            )
                        });
                    return (
                        index,
                        surface.visible_indices(Some(selected.as_deref().unwrap_or(&[]))),
                    );
                }
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
                let surface = &upload.assets.reflectors[*index];
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
                    reflection: None,
                    refraction: None,
                    refraction_clear: wgpu::Color::BLACK,
                    water_bind_group: None,
                    surface_uniforms: ViewUniforms::new(
                        device,
                        &pipeline.resources,
                        &frame.lighting,
                        &frame.marks.buffer,
                    ),
                });
            }
            let view = frame
                .planar_views
                .iter_mut()
                .find(|view| view.surface_index == surface_index)
                .unwrap();
            if view.surface_indices != indices {
                view.surface_buffer = visibility::upload_visible_indices(device, queue, &indices)
                    .with_lightmaps(
                        &upload.meshes[upload.assets.reflectors[surface_index].mesh_index],
                        &indices,
                    );
                view.surface_indices = indices;
            }
        }

        optical_planning::prepare(
            &mut frame.planar_views,
            device,
            queue,
            &pipeline.resources,
            self,
            upload,
            refraction::OpticalInputs {
                size: reflection_size,
                lighting: &frame.lighting,
                surface_marks: &frame.marks.buffer,
            },
        );
        frame.main.plans = Some(plans);

        let mut water_views = frame.planar_views.iter().filter(|view| {
            scene.materials[upload.assets.reflectors[view.surface_index].material_slot]
                .shader
                .water()
                .is_some()
        });
        let mut water_side = water_views
            .next()
            .filter(|view| view.refraction.is_some())
            .filter(|_| water_views.all(|view| view.refraction.is_some()))
            .map(|_| transparency::WaterSide::from_submerged(self.submerged));
        if let Some(plane) =
            intersection::prepare(frame, self, device, queue, &pipeline.resources, size)
        {
            water_side = Some(transparency::WaterSide::Above);
            main_uniforms.clip_plane = [
                plane.normal[0],
                plane.normal[1],
                plane.normal[2],
                -plane.distance,
            ];
        }

        main_uniforms = main_uniforms.with_clip_camera(self.view_camera);
        frame.main.world.write(queue, &main_uniforms);
        let upload = &frame.scene;
        for surface in &frame.planar_views {
            let mut uniforms = main_uniforms;
            uniforms.water_depth_params[2] = f32::from(surface.reflection.is_some());
            uniforms.water_depth_params[3] = f32::from(surface.refraction.is_some());
            if scene.materials[upload.assets.reflectors[surface.surface_index].material_slot]
                .shader
                .water()
                .is_some()
            {
                let plane = upload.assets.reflectors[surface.surface_index].plane;
                uniforms.medium_plane = [
                    plane.normal[0],
                    plane.normal[1],
                    plane.normal[2],
                    -plane.distance,
                ];
            }
            surface.surface_uniforms.write(queue, &uniforms);
        }

        let mut animation_demand_cache = std::mem::take(&mut frame.animation_demand_cache);
        let animated_instances = animation_demand_cache.collect(frame);
        frame.animation_demand_cache = animation_demand_cache;
        if let Err(error) = animated_instances.and_then(|animated_instances| {
            frame.scene.update_animations(
                device,
                queue,
                &pipeline.resources,
                &animated_instances,
                self.animation_clock.time_seconds,
            )
        }) {
            frame.readiness = Readiness::Rejected;
            frame.binding_request = None;
            frame.lightmap_request = None;
            frame.presentation.clear();
            frame.screen_overlay = None;
            if let Some(feedback) = &frame.feedback {
                feedback.set(super::feedback::RenderStatus::Rejected {
                    reason: error.to_string(),
                });
            }
            return;
        }
        let upload = &frame.scene;
        let lighting_contexts = &mut frame.lighting_contexts;
        lighting_contexts.resize(map.map_or(0, |map| map.model_lighting.len()), false);
        lighting_contexts.fill(false);
        model_lighting_visibility::gather(lighting_contexts, scene, map, upload, &frame.main);
        for planar in &frame.planar_views {
            if let Some(view) = &planar.reflection {
                model_lighting_visibility::gather(lighting_contexts, scene, map, upload, view);
            }
            if let Some(view) = &planar.refraction {
                model_lighting_visibility::gather(lighting_contexts, scene, map, upload, view);
            }
        }
        if let Some(view) = frame
            .water_intersection
            .as_ref()
            .filter(|view| view.selection.is_some())
        {
            model_lighting_visibility::gather(lighting_contexts, scene, map, upload, &view.view);
        }

        frame.lighting.update(
            queue,
            map,
            self.model_lighting_time,
            lighting_contexts,
            &frame.scene.animation.lighting_origin_updates,
        );
        transparency::prepare(
            &mut frame.main,
            upload,
            &main_uniforms,
            skybox_uniforms.as_ref(),
            water_side,
            true,
        );
        planar::prepare(&mut frame.main, upload, scene, device, queue, None);
        transparent_indirect::prepare(&mut frame.main, upload, device, queue);

        let mut demand_cache = std::mem::take(&mut frame.demand_cache);
        let demand = demand_cache.collect(frame, self);
        frame.demand_cache = demand_cache;
        match demand.and_then(|demand| {
            bindings::FrameRequest::new(
                self,
                demand,
                frame
                    .pending_clock
                    .unwrap_or((self.animation_clock, self.uniforms.water_time_sky_tint[0])),
            )
            .and_then(|request| request.evaluate_proxies(&mut frame.proxy_states))
            .map_err(|error| residency_demand::DemandError::Animation(error.to_string()))
        }) {
            Ok(request) => {
                frame.pending_clock = Some((request.clock, request.frame_water_time));
                if frame
                    .binding_request
                    .as_ref()
                    .is_some_and(|previous| previous.immersion != request.immersion)
                {
                    frame.screen_overlay = None;
                }
                frame.binding_request = Some(request);
                let mut cache = std::mem::take(&mut frame.lightmap_demand_cache);
                let pages = cache.collect(frame);
                frame.lightmap_demand_cache = cache;
                match lightmaps::Request::new(
                    self.preview.clone(),
                    pages,
                    self.model_lighting_time as f32,
                ) {
                    Ok(request) => {
                        frame.lightmap_request =
                            Some(request.with_cpu_executor(self.cpu_executor.clone()));
                    }
                    Err(error) => {
                        frame.binding_request = None;
                        frame.lightmap_request = None;
                        frame.readiness = Readiness::Rejected;
                        frame.presentation.clear();
                        if let Some(feedback) = &frame.feedback {
                            feedback.bind_pages(None);
                            feedback.bind_retirement(None);
                            feedback.set(super::feedback::RenderStatus::Rejected {
                                reason: error.to_string(),
                            });
                        }
                    }
                }
            }
            Err(error) => {
                frame.readiness = Readiness::Rejected;
                frame.presentation.clear();
                frame.binding_request = None;
                frame.lightmap_request = None;
                frame.pending_clock = None;
                frame.scene.bindings = Arc::new(bindings::BindingSnapshot::default());
                frame.screen_overlay = None;
                if let Some(feedback) = &frame.feedback {
                    feedback.bind_pages(None);
                    feedback.bind_retirement(None);
                    feedback.set(super::feedback::RenderStatus::Rejected {
                        reason: error.to_string(),
                    });
                }
            }
        }
    }
    fn render(
        &self,
        pipeline: &ModelPipeline,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        let frame = pipeline
            .frames
            .get(self.prepared_frame.load(Ordering::Relaxed))
            .filter(|frame| {
                frame.live && frame.view_id == self.view_id && frame.scene_key == self.preview.key()
            });
        let retained = frame.and_then(|frame| {
            (self.retains_output() && frame.readiness.can_retain())
                .then(|| frame.presentation.get(&frame.output_key()))
                .flatten()
        });
        let Some(frame) = frame else {
            return;
        };
        if !frame.readiness.can_render() {
            if let Some(output) = retained {
                passes::compose(
                    output.composition(),
                    frame.bounds,
                    &pipeline.resources,
                    encoder,
                    target,
                    clip_bounds,
                );
                frame
                    .presentation
                    .stage(frame.output_key(), Arc::clone(output));
            }
            return;
        }
        frame.scene.encode_animations(&pipeline.resources, encoder);
        let upload = &frame.scene;
        let composition = passes::render_frame(
            self,
            frame,
            &pipeline.resources,
            upload,
            encoder,
            target,
            clip_bounds,
        );
        if self.retains_output() {
            let key = frame.output_key();
            let output = frame
                .presentation
                .get(&key)
                .filter(|output| output.matches(composition))
                .cloned()
                .unwrap_or_else(|| Arc::new(ViewOutput::new(composition)));
            frame.presentation.stage(key, output);
        }
    }
}

#[derive(Debug)]
pub struct ModelPipeline {
    resources: RenderResources,
    uploads: UploadCache,
    frames: Vec<PreparedFrame>,
    bindings: bindings::BindingCache,
    lightmaps: lightmaps::Pool,
    submission: Option<residency_demand::SubmissionPins<bindings::BindingSnapshot>>,
    lightmap_submission: Option<residency_demand::SubmissionPins<lightmaps::Snapshot>>,
}
impl shader::Pipeline for ModelPipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let resources = RenderResources::new(device, queue, format, None);
        let lightmaps = lightmaps::Pool::new(
            &resources.texture_uploader,
            Arc::clone(&resources.default_lightmap),
        );
        Self {
            resources,
            uploads: UploadCache::new(None),
            frames: Vec::new(),
            bindings: bindings::BindingCache::new(None),
            submission: None,
            lightmaps,
            lightmap_submission: None,
        }
    }
    fn new_with_capabilities(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        capabilities: &wgpu::DownlevelCapabilities,
    ) -> Self {
        let resources = RenderResources::new(device, queue, format, Some(capabilities));
        let lightmaps = lightmaps::Pool::new(
            &resources.texture_uploader,
            Arc::clone(&resources.default_lightmap),
        );
        Self {
            resources,
            uploads: UploadCache::new(Some(capabilities)),
            frames: Vec::new(),
            bindings: bindings::BindingCache::new(Some(capabilities)),
            submission: None,
            lightmaps,
            lightmap_submission: None,
        }
    }
    fn begin_prepare(&mut self) {
        self.submission = None;
        self.lightmap_submission = None;
        for frame in &mut self.frames {
            frame.presentation.begin();
        }
    }
    fn finish_prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let empty_lightmaps = self.lightmaps.empty_snapshot();
        for frame in &mut self.frames {
            if !frame.live || frame.binding_request.is_none() || frame.lightmap_request.is_none() {
                frame.scene.bindings = Arc::new(bindings::BindingSnapshot::default());
                frame.scene.lightmaps = Arc::clone(&empty_lightmaps);
                frame.screen_overlay = None;
                frame.binding_request = None;
                frame.lightmap_request = None;
                frame.presentation.clear();
            } else if frame
                .lightmap_request
                .as_ref()
                .is_some_and(|request| !frame.scene.lightmaps.matches(request))
            {
                frame.scene.lightmaps = Arc::clone(&empty_lightmaps);
            }
        }
        let (bytes, allocations) = self
            .resources
            .texture_uploader
            .lock()
            .expect("texture uploader lock")
            .sampled_capacity();
        let capacity = lightmaps::MissingCost { bytes, allocations };
        let lightmap_costs: Vec<_> = self
            .frames
            .iter()
            .map(|frame| {
                frame
                    .lightmap_request
                    .as_ref()
                    .map(|request| request.working_set().map_err(|error| error.to_string()))
            })
            .collect();
        let stage_requests: Vec<_> = self
            .frames
            .iter()
            .enumerate()
            .filter(|(_, frame)| frame.live)
            .filter(|(index, _)| {
                lightmap_costs[*index].as_ref().is_some_and(|cost| {
                    cost.as_ref().is_ok_and(|cost| {
                        cost.upper_bound()
                            .is_ok_and(|cost| !admission::exceeds(cost, capacity))
                    })
                })
            })
            .filter_map(|(_, frame)| frame.binding_request.as_ref())
            .collect();
        self.bindings.stage_visible(&stage_requests);
        self.bindings.stage_pages(device);
        let candidates: Vec<_> = self
            .frames
            .iter()
            .enumerate()
            .map(|(index, frame)| {
                let cost = (|| {
                    let mut cost = lightmap_costs[index]
                        .as_ref()
                        .ok_or("preview has no lightmap request")?
                        .clone()?;
                    let lightmap_cost = cost.upper_bound()?;
                    if admission::exceeds(lightmap_cost, capacity) {
                        return Err(admission::rejection(lightmap_cost, capacity));
                    }
                    let request = frame
                        .binding_request
                        .as_ref()
                        .ok_or("preview has no material request")?;
                    cost.extend(&self.bindings.working_set(device, request)?);
                    Ok(cost)
                })();
                admission::Candidate {
                    priority: frame.admission_priority,
                    cost,
                }
            })
            .collect();
        let decisions = admission::select(&candidates, capacity);
        for (index, frame) in self.frames.iter_mut().enumerate() {
            if decisions[index] != admission::Decision::Admitted {
                frame.scene.bindings = Arc::new(bindings::BindingSnapshot::default());
                frame.scene.lightmaps = Arc::clone(&empty_lightmaps);
                frame.screen_overlay = None;
            }
        }

        let lightmap_requests: Vec<_> = self
            .frames
            .iter()
            .enumerate()
            .filter(|(index, frame)| {
                frame.live && decisions[*index] == admission::Decision::Admitted
            })
            .filter_map(|(_, frame)| frame.lightmap_request.as_ref())
            .collect();
        let lightmap_demand = self
            .lightmaps
            .demand(&lightmap_requests)
            .map_err(|error| error.to_string());
        let requests: Vec<_> = self
            .frames
            .iter()
            .enumerate()
            .filter(|(index, frame)| {
                frame.live && decisions[*index] == admission::Decision::Admitted
            })
            .filter_map(|(_, frame)| frame.binding_request.as_ref())
            .collect();
        self.bindings.demand(device, &requests);
        let reserved = lightmap_demand.and_then(|missing| {
            self.bindings.reserve_lightmaps(
                &self.resources.texture_uploader,
                &self.lightmaps,
                missing,
                self.lightmaps.active_cost(),
            )
        });
        let mut lightmap_results = Vec::with_capacity(self.frames.len());
        for (index, frame) in self.frames.iter_mut().enumerate() {
            let result = if let admission::Decision::Rejected(reason) = &decisions[index] {
                Err(reason.clone())
            } else if decisions[index] == admission::Decision::Pending {
                Ok(false)
            } else if let Some(request) = frame.lightmap_request.as_ref().filter(|_| frame.live) {
                if frame.scene.lightmaps.matches(request) {
                    Ok(true)
                } else if let Some(snapshot) = self.lightmaps.cached(request) {
                    frame.scene.lightmaps = snapshot;
                    Ok(true)
                } else {
                    match &reserved {
                        Ok(true) => self
                            .lightmaps
                            .prepare(device, &self.resources.lightmap_layout, request)
                            .map(|snapshot| {
                                frame.scene.lightmaps = snapshot;
                                true
                            })
                            .map_err(|error| error.to_string()),
                        Ok(false) => Ok(false),
                        Err(reason) => Err(reason.clone()),
                    }
                }
            } else {
                Ok(false)
            };
            lightmap_results.push(result);
        }

        let mut pins = Vec::new();
        let mut lightmap_pins = Vec::new();
        for (index, frame) in self
            .frames
            .iter_mut()
            .enumerate()
            .filter(|(_, frame)| frame.live)
        {
            let Some(request) = &frame.binding_request else {
                continue;
            };
            let previous = std::mem::replace(
                &mut frame.scene.bindings,
                Arc::new(bindings::BindingSnapshot::default()),
            );
            let previous = if previous.covers(request) {
                Some(previous)
            } else {
                None
            };
            let lightmap_ready = lightmap_results[index].as_ref().is_ok_and(|ready| *ready);
            let prepared = if let Some(error) = self.bindings.staged_error(request) {
                Err(error)
            } else {
                match &lightmap_results[index] {
                    Ok(true) if reserved.as_ref().is_ok_and(|ready| *ready) => self
                        .bindings
                        .prepare(device, queue, &self.resources, request),
                    Ok(true) => self.bindings.cached(request),
                    Ok(false) => Ok(None),
                    Err(reason) => Err(reason.clone()),
                }
            };
            let prepared = prepared.and_then(|snapshot| {
                snapshot
                    .map(|mut snapshot| {
                        snapshot.apply_proxies(
                            device,
                            queue,
                            &self.resources,
                            &mut self.bindings,
                            request,
                            &mut frame.proxy_bindings,
                        )?;
                        Ok(snapshot)
                    })
                    .transpose()
            });
            match prepared {
                Ok(Some(snapshot)) => {
                    frame.readiness = Readiness::Current;
                    frame.pending_clock = None;
                    frame.scene.bindings = Arc::new(snapshot);
                    if let Some(feedback) = &frame.feedback {
                        feedback.bind_pages(None);
                        feedback.bind_retirement(Some(
                            self.resources
                                .texture_uploader
                                .lock()
                                .expect("texture uploader lock")
                                .retirement_subscription(),
                        ));
                        feedback.set(super::feedback::RenderStatus::Ready {
                            unavailable_images: frame.scene.bindings.issues().count(),
                        });
                    }
                    screen_refraction::prepare(
                        &mut frame.screen_overlay,
                        device,
                        queue,
                        &self.resources,
                        &frame.scene,
                        request.eye,
                        request.water_time,
                    );
                    pins.push(Arc::clone(&frame.scene.bindings));
                    lightmap_pins.push(Arc::clone(&frame.scene.lightmaps));
                }
                Ok(None) => {
                    if lightmap_ready
                        && !self.bindings.capacity_blocked()
                        && let Some(previous) = previous
                    {
                        frame.scene.bindings = previous;
                        frame.readiness = Readiness::Previous;
                        screen_refraction::prepare(
                            &mut frame.screen_overlay,
                            device,
                            queue,
                            &self.resources,
                            &frame.scene,
                            request.eye,
                            request.water_time,
                        );
                        pins.push(Arc::clone(&frame.scene.bindings));
                        lightmap_pins.push(Arc::clone(&frame.scene.lightmaps));
                    } else {
                        frame.readiness = if self.bindings.capacity_blocked() {
                            Readiness::Retiring
                        } else {
                            Readiness::Loading
                        };
                        frame.screen_overlay = None;
                    }
                    let key = frame.output_key();
                    if frame.readiness == Readiness::Retiring
                        && frame
                            .presentation
                            .get(&key)
                            .is_some_and(|output| output.has_material_image())
                    {
                        frame.presentation.clear();
                    }
                    if frame.readiness.can_retain()
                        && let Some(output) = frame.presentation.get(&key)
                    {
                        output.prepare(device, queue, &self.resources);
                    }
                    if let Some(feedback) = &frame.feedback {
                        feedback.bind_pages(self.bindings.subscription());
                        let uploader = self
                            .resources
                            .texture_uploader
                            .lock()
                            .expect("texture uploader lock");
                        feedback.bind_retirement(Some(uploader.retirement_subscription()));
                        drop(uploader);
                        feedback.set(if frame.readiness.can_render() {
                            super::feedback::RenderStatus::Ready {
                                unavailable_images: frame.scene.bindings.issues().count(),
                            }
                        } else if self.bindings.capacity_blocked() {
                            super::feedback::RenderStatus::Retiring {
                                unavailable_images: 0,
                            }
                        } else {
                            super::feedback::RenderStatus::Pending
                        });
                    }
                }
                Err(reason) => {
                    frame.scene.lightmaps = Arc::clone(&empty_lightmaps);
                    frame.readiness = Readiness::Rejected;
                    frame.presentation.clear();
                    frame.screen_overlay = None;
                    if let Some(feedback) = &frame.feedback {
                        feedback.bind_pages(None);
                        feedback.bind_retirement(None);
                        feedback.set(super::feedback::RenderStatus::Rejected { reason });
                    }
                }
            }
        }

        if let Some(pressure) = self.bindings.pressure(&self.resources) {
            for frame in self
                .frames
                .iter_mut()
                .filter(|frame| frame.live && !frame.readiness.can_render())
            {
                frame.readiness = Readiness::Rejected;
                frame.presentation.clear();
                if let Some(feedback) = &frame.feedback {
                    feedback.set(super::feedback::RenderStatus::Rejected {
                        reason: pressure.to_string(),
                    });
                }
            }
        }
        self.submission = Some(residency_demand::SubmissionPins::new(pins));
        self.lightmap_submission = Some(residency_demand::SubmissionPins::new(lightmap_pins));
    }
    fn submitted(&mut self, queue: &wgpu::Queue) {
        let completion = self
            .resources
            .texture_uploader
            .lock()
            .expect("texture uploader lock")
            .completion();
        if let Some(pins) = self.submission.take() {
            pins.submitted(queue, &completion);
        }
        if let Some(pins) = self.lightmap_submission.take() {
            pins.submitted(queue, &completion);
        }
        let mut outputs = Vec::new();
        for frame in &mut self.frames {
            frame.admission_priority |= frame.readiness.can_render();
            frame
                .presentation
                .submitted(|output| outputs.push(Arc::clone(output)));
        }
        if !outputs.is_empty() {
            residency_demand::SubmissionPins::new(outputs).submitted(queue, &completion);
        }
        self.bindings.trim();
    }
    fn trim(&mut self) {
        self.frames.retain(|frame| frame.live);
        for frame in &mut self.frames {
            frame.live = false;
        }
        self.uploads.trim();
        self.bindings.trim();
    }
}

#[cfg(test)]
mod frame_identity_tests {
    use super::*;

    struct History {
        view_id: ViewId,
        live: bool,
        sun_visibility: f32,
    }

    fn claim(frames: &mut Vec<History>, view_id: ViewId) -> usize {
        let slot = unclaimed_viewer_frame_slot(
            frames.iter().map(|frame| (frame.view_id, frame.live)),
            view_id,
        );
        if slot == frames.len() {
            frames.push(History {
                view_id,
                live: false,
                sun_visibility: 0.0,
            });
        }
        assert!(!frames[slot].live);
        frames[slot].live = true;
        slot
    }

    #[test]
    fn reordered_widgets_keep_distinct_temporal_histories() {
        let clear = ViewId::default();
        let occluded = ViewId::default();
        let mut frames = vec![
            History {
                view_id: clear,
                live: false,
                sun_visibility: 1.0,
            },
            History {
                view_id: occluded,
                live: false,
                sun_visibility: 0.25,
            },
        ];
        let second = claim(&mut frames, occluded);
        assert_eq!(second, 1);
        assert_eq!(frames[second].sun_visibility, 0.25);
        frames[second].sun_visibility *= 0.5;
        let first = claim(&mut frames, clear);
        assert_eq!(first, 0);
        assert_eq!(frames[first].sun_visibility, 1.0);
        assert_eq!(frames[second].sun_visibility, 0.125);
        assert_eq!(frames.len(), 2);
    }

    #[test]
    fn inserting_widget_preserves_later_unclaimed_views_and_drops_dead_history() {
        let removed = ViewId::default();
        let retained = ViewId::default();
        let inserted = ViewId::default();
        let mut frames = vec![
            History {
                view_id: removed,
                live: false,
                sun_visibility: 0.8,
            },
            History {
                view_id: retained,
                live: false,
                sun_visibility: 0.6,
            },
        ];
        let new = claim(&mut frames, inserted);
        assert_eq!(new, 2);
        assert_eq!(frames[new].sun_visibility, 0.0);
        let existing = claim(&mut frames, retained);
        assert_eq!(existing, 1);
        assert_eq!(frames[existing].sun_visibility, 0.6);
        frames.retain(|frame| frame.live);
        for frame in &mut frames {
            frame.live = false;
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(claim(&mut frames, inserted), 1);
        assert_eq!(claim(&mut frames, retained), 0);
    }

    #[test]
    fn repeated_prepare_cannot_overwrite_a_live_frame() {
        let view_id = ViewId::default();
        let mut frames = Vec::new();
        let first = claim(&mut frames, view_id);
        frames[first].sun_visibility = 0.7;
        let second = claim(&mut frames, view_id);
        assert_ne!(first, second);
        assert_eq!(frames[first].sun_visibility, 0.7);
        assert_eq!(frames[second].sun_visibility, 0.0);
    }
}
