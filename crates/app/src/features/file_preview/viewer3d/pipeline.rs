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
    mid, perspective, scene_sample_count, shader, skybox_eye, transform_door_vertices, wgpu,
};
use gmpublished_domain::math::Vec3;

mod admission;
mod background_bindings;
mod bindings;
mod bounds_query;
mod clipping;
mod completion;
pub use completion::ShutdownError;

// The application scope has dropped all render owners before this boundary.
pub fn finish_gpu_shutdown() -> Result<(), ShutdownError> {
    completion::finish_application_shutdown()
}

mod cubemap;
mod draw;
mod frame_selection;
mod geometry;
mod images;
mod intersection;
mod lighting;
mod lightmap_demand;
mod lightmap_ranges;
mod lightmaps;
mod marks;
mod materials;
#[cfg(feature = "map-capture")]
mod mdi_diagnostics;
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

#[cfg(feature = "map-capture")]
pub(super) use crate::media::preview_model::ShadingMode;
#[cfg(feature = "map-capture")]
pub(super) use materials::capture_bc_upload;
#[cfg(feature = "map-capture")]
pub(super) use texture_upload::configured_policy as capture_texture_upload_policy;
#[cfg(feature = "map-capture")]
pub(super) fn capture_sampler_quality() -> serde_json::Value {
    serde_json::to_value(samplers::quality_profile()).expect("serialize sampler quality")
}
#[cfg(feature = "map-capture")]
pub(super) use skinning::{
    Phase as ModelAnimationPhase, PhaseOverrides as ModelAnimationPhaseOverrides,
};
pub use uniforms::Uniforms;
#[cfg(feature = "map-capture")]
pub(super) use uniforms::{Diagnostic, DiagnosticMode};

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
    #[cfg(feature = "map-capture")]
    pub(super) model_animation_phases: Option<Arc<ModelAnimationPhaseOverrides>>,
    pub(super) animation_clock: crate::bridge::materials::AnimationClock,
    #[cfg(feature = "map-capture")]
    pub(super) exact_texture_frame: bool,
    pub(super) view_id: ViewId,
    pub(super) clear_color: Option<[f32; 3]>,
    pub(super) air_fog: Option<super::MapFog>,
    #[cfg(feature = "map-capture")]
    pub(super) reflection_target: bool,
    #[cfg(feature = "map-capture")]
    pub(super) refraction_target: bool,
    #[cfg(feature = "map-capture")]
    pub(super) capture_diagnostics: Option<super::capture::CaptureDiagnostics>,
    #[cfg(feature = "map-capture")]
    pub(super) capture_lighting_timeline: Option<Arc<super::capture::ModelLightingTimeline>>,
    #[cfg(feature = "map-capture")]
    pub(super) raster_offset: Option<[f32; 2]>,
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

#[cfg(feature = "map-capture")]
fn preparation_diagnostics(view: &SceneView, upload: &ViewerScene) -> serde_json::Value {
    serde_json::json!({
        "opaque_mdi_runs":mdi_diagnostics::collect(view, upload),
        "native_transparent_mdi":view.transparent_indirect.diagnostics(),
        "optical_cpu_planning":view.optical_cpu_planning,
        "opaque_binding_suppression":draw::opaque_binding_suppression_enabled(),
        "transparent_binding_suppression":draw::transparent_binding_suppression_enabled(),
        "opaque_pass_coalescing":passes::opaque_pass_coalescing_enabled(),
        "semantic_world_cache":view.plan_cache.world_cache_diagnostics(),
        "world":view.plans.as_ref().map(|plans|&plans.world.world_opaque.issues),
        "skybox":view.plans.as_ref().and_then(|plans|plans.map_skybox.as_ref()).map(|plan|&plan.world_opaque.issues),
        "transparency":view.transparency_diagnostics,
        "skybox_transparency":view.skybox_transparency_diagnostics,
    })
}

impl ModelPrimitive {
    fn retains_output(&self) -> bool {
        #[cfg(feature = "map-capture")]
        if self.exact_texture_frame
            || self.reflection_target
            || self.refraction_target
            || self.uniforms.diagnostic.mode != DiagnosticMode::Disabled as u32
        {
            return false;
        }
        true
    }
    pub(super) fn raster_offset(&self) -> [f32; 2] {
        #[cfg(feature = "map-capture")]
        if let Some(offset) = self.raster_offset {
            return offset;
        }
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
        #[cfg(feature = "map-capture")]
        let _prepare_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::Prepare));
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
        #[cfg(feature = "map-capture")]
        let upload_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::Upload));
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
            #[cfg(feature = "map-capture")]
            if let Some(diagnostics) = self
                .capture_diagnostics
                .as_ref()
                .filter(|observer| observer.recording())
            {
                *diagnostics.lock().expect("capture diagnostics lock") = serde_json::json!({
                    "upload_error": error.capture_json(),
                });
            }
            #[cfg(not(feature = "map-capture"))]
            let _ = error;
            return;
        }
        pipeline.uploads.touch(scene_key);
        #[cfg(feature = "map-capture")]
        drop(upload_span);

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
        #[cfg(feature = "map-capture")]
        frame
            .lighting
            .apply_capture_timeline(map, self.capture_lighting_timeline.as_ref());
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
        #[cfg(feature = "map-capture")]
        if let Some(observer) = &self.capture_diagnostics {
            observer.observe_extent(size);
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
        #[cfg(feature = "map-capture")]
        let visibility_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::WorldVisibility));
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
        #[cfg(feature = "map-capture")]
        drop(visibility_span);
        #[cfg(feature = "map-capture")]
        if let Some(observer) = &self.capture_diagnostics {
            use super::capture::VisibilityMode;
            observer.observe_visibility(match frame.main.visibility.tracker.state {
                visibility::VisibilityClusterState::Disabled => VisibilityMode::Disabled,
                visibility::VisibilityClusterState::StandDown => VisibilityMode::OutsideCluster,
                visibility::VisibilityClusterState::Cluster(_)
                    if frame.main.visibility.plan.is_some() =>
                {
                    VisibilityMode::ClusterPvs
                }
                visibility::VisibilityClusterState::Cluster(_) => VisibilityMode::ClusterWithoutPvs,
            });
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
        #[cfg(feature = "map-capture")]
        let main_plan_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::MainPlan));
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
        #[cfg(feature = "map-capture")]
        drop(main_plan_span);
        #[cfg(feature = "map-capture")]
        let selection_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::PlanarSelection));
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
        #[cfg(feature = "map-capture")]
        drop(selection_span);
        #[cfg(feature = "map-capture")]
        let optical_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::OpticalPrepare));
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
        #[cfg(feature = "map-capture")]
        drop(optical_span);
        frame.main.plans = Some(plans);
        #[cfg(feature = "map-capture")]
        let intersection_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::Intersection));

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
        #[cfg(feature = "map-capture")]
        drop(intersection_span);
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
        #[cfg(feature = "map-capture")]
        let lighting_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::Lighting));
        let mut animation_demand_cache = std::mem::take(&mut frame.animation_demand_cache);
        let animated_instances = animation_demand_cache.collect(frame);
        frame.animation_demand_cache = animation_demand_cache;
        let animation_phases = {
            #[cfg(feature = "map-capture")]
            {
                self.model_animation_phases.as_deref()
            }
            #[cfg(not(feature = "map-capture"))]
            {
                None
            }
        };
        if let Err(error) = animated_instances.and_then(|animated_instances| {
            frame.scene.update_animations(
                device,
                queue,
                &pipeline.resources,
                &animated_instances,
                self.animation_clock.time_seconds,
                animation_phases,
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
        #[cfg(feature = "map-capture")]
        let lighting_visibility_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::LightingVisibility));
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
        #[cfg(feature = "map-capture")]
        drop(lighting_visibility_span);
        frame.lighting.update(
            queue,
            map,
            self.model_lighting_time,
            lighting_contexts,
            &frame.scene.animation.lighting_origin_updates,
            #[cfg(feature = "map-capture")]
            self.capture_diagnostics.as_ref(),
        );
        #[cfg(feature = "map-capture")]
        drop(lighting_span);
        #[cfg(feature = "map-capture")]
        let transparency_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::Transparency));
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
        #[cfg(feature = "map-capture")]
        drop(transparency_span);

        #[cfg(feature = "map-capture")]
        if let Some(diagnostics) = self
            .capture_diagnostics
            .as_ref()
            .filter(|observer| observer.recording())
        {
            let _observer_span = diagnostics.span(super::capture::Phase::Diagnostics);
            let records = |refracted: bool| {
                frame.planar_views.iter().filter_map(|surface_view| {
                let view=if refracted {surface_view.refraction.as_ref()?} else {
                    surface_view.reflection.as_ref()?
                };
                let surface=&upload.assets.reflectors[surface_view.surface_index];
                let plans=view.plans.as_ref().unwrap();
                Some(serde_json::json!({
                    "material":scene.materials[surface.material_slot].name,
                    "plane_normal":surface.plane.normal.to_array(),"plane_distance":surface.plane.distance,
                    "reflected_eye":surface.plane.reflect_point(eye).to_array(),
                    "camera_position":if refracted {eye.to_array()} else {surface.plane.reflect_point(eye).to_array()},
                    "transparent_primitives":view.transparency.len(),
                    "preparation":preparation_diagnostics(view, upload),
                    "world_opaque_meshes":plans.world.opaque_mesh_indices().len(),
                    "world_surface_marks":plans.world.surface_mark_count(),
                    "transparent_receiver_marks":plans.world.receiver_mark_count(),
                    "world_translucent_meshes":plans.world.translucent.len(),
                    "world_entity_meshes":plans.world.opaque_mesh_indices().iter().filter(|&&index|upload.meshes[index].entity).count(),
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
                            .reflection
                            .as_ref()
                            .and_then(|view| view.targets.as_ref())
                            .map_or(0, RenderTargets::estimated_bytes)
                            + surface
                                .refraction
                                .as_ref()
                                .and_then(|view| view.targets.as_ref())
                                .map_or(0, RenderTargets::estimated_bytes)
                    })
                    .sum::<u64>();
            *diagnostics.lock().expect("capture diagnostics lock") = serde_json::json!({
                "clip_mode": pipeline.resources.clip_mode,
                "device_clip_distances": pipeline.resources.clip_mode == clipping::ClipMode::Primitive,
                "texture_upload":pipeline.resources.texture_uploader.lock().expect("texture uploader lock").stats(),
                "image_cache":pipeline.bindings.stats(), "image_resources": upload.bindings.image_stats(), "image_upload_issues":upload.bindings.issues().collect::<Vec<_>>(),
                "material_samplers":upload.bindings.samplers().map(samplers::SamplerDiagnostic::capture_json).collect::<Vec<_>>(),
                "raster_offset_physical_pixels":self.raster_offset(),"main_target_dimensions":size,"reflection_views":records(false),"refraction_views":records(true),
                "single_bounce":true,"nested_planar_fallback":"average sky tint (cubemap pending)",
                "water_intersection":frame.water_intersection.as_ref().and_then(|intersection|intersection.selection.map(|selection|serde_json::json!({"surface_index":selection.surface_index,"body_index":selection.body_index,"preparation":preparation_diagnostics(&intersection.view, upload)}))),
                "water_body":map.and_then(|map|map.water_volumes.as_ref()).and_then(|volumes|volumes.at(eye)).map(|sample|serde_json::json!({"index":sample.body_index,"surface_z":sample.body.surface_z,"min_z":sample.body.min_z,"surface_material":sample.body.material_name,"contents":sample.contents})),
                "underwater_overlay":map.and_then(|map|map.immersion_material(eye)).and_then(|water|water.underwater_overlay.as_ref()).map(|effect|&effect.name),
                "clear_color_linear":self.clear_color,
                "main_camera_position":self.uniforms.camera_position, "main_fog_color":self.uniforms.fog_color, "main_fog":{"start":self.uniforms.fog.start,"end":self.uniforms.fog.end,"max_density":self.uniforms.fog.max_density,"mode":self.uniforms.fog.mode}, "main_clip_plane":main_uniforms.clip_plane, "main_visibility_state":format!("{:?}",frame.main.visibility.tracker.state),
                "main_preparation":preparation_diagnostics(&frame.main, upload),
                "surface_mark_metadata_bytes":frame.marks.buffer.size(), "world_surface_marks":frame.main.plans.as_ref().unwrap().world.surface_mark_count(), "transparent_receiver_marks":frame.main.plans.as_ref().unwrap().world.receiver_mark_count(),
                "estimated_target_bytes":bytes,"composition":"planar depth occludes main transparency; clipped refraction includes below-plane transparency"
            });
        }
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
        #[cfg(feature = "map-capture")]
        let _encode_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::Encode));
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
        #[cfg(feature = "map-capture")]
        if let Some(observer) = &self.capture_diagnostics {
            use super::capture::Presentation;
            observer.observe_presentation(match frame {
                Some(frame) if frame.readiness == Readiness::Previous => Presentation::Previous,
                Some(frame) if frame.readiness == Readiness::Current => Presentation::Current,
                _ if retained.is_some() => Presentation::Retained,
                _ => Presentation::Pending,
            });
        }
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
        #[cfg(feature = "map-capture")]
        if let Some(observer) = self
            .capture_diagnostics
            .as_ref()
            .filter(|observer| observer.recording())
        {
            let mut diagnostics = observer.lock().expect("capture diagnostics lock");
            diagnostics["animation_state"] = frame
                .scene
                .animation
                .diagnostics(&frame.scene.assets, &frame.scene.meshes);
            diagnostics["page_cache"] = pipeline.bindings.page_diagnostics();
            diagnostics["image_cache"] =
                serde_json::to_value(pipeline.bindings.stats()).expect("image cache diagnostics");
            diagnostics["lightmap_resources"] = frame.scene.lightmaps.stats();
            diagnostics["lightmap_cache"] = pipeline.lightmaps.stats();
            diagnostics["image_resources"] =
                serde_json::to_value(frame.scene.bindings.image_stats())
                    .expect("image residency diagnostics");
            diagnostics["image_upload_issues"] =
                serde_json::to_value(frame.scene.bindings.issues().collect::<Vec<_>>())
                    .expect("image issues diagnostics");
            diagnostics["material_samplers"] = serde_json::Value::Array(
                frame
                    .scene
                    .bindings
                    .samplers()
                    .map(samplers::SamplerDiagnostic::capture_json)
                    .collect(),
            );
            diagnostics["texture_upload"] = serde_json::to_value(
                pipeline
                    .resources
                    .texture_uploader
                    .lock()
                    .expect("texture uploader lock")
                    .stats(),
            )
            .expect("texture upload diagnostics");
        }
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
        #[cfg(feature = "gui-benchmark")]
        let mut scope = iced_wgpu::profiling::begin_cpu_scope("map.residency.stage");
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
        #[cfg(feature = "gui-benchmark")]
        {
            drop(scope.take());
            scope = iced_wgpu::profiling::begin_cpu_scope("map.residency.admission");
        }
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
        #[cfg(feature = "gui-benchmark")]
        {
            drop(scope.take());
            scope = iced_wgpu::profiling::begin_cpu_scope("map.residency.demand");
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
        #[cfg(feature = "gui-benchmark")]
        {
            drop(scope.take());
            scope = iced_wgpu::profiling::begin_cpu_scope("map.residency.lightmaps");
        }
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
        #[cfg(feature = "gui-benchmark")]
        {
            drop(scope.take());
            scope = iced_wgpu::profiling::begin_cpu_scope("map.residency.bindings");
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
            let allow_previous = {
                #[cfg(feature = "map-capture")]
                {
                    frame.feedback.is_some() && !request.exact_frame
                }
                #[cfg(not(feature = "map-capture"))]
                {
                    true
                }
            };
            let previous = if allow_previous && previous.covers(request) {
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
        #[cfg(feature = "gui-benchmark")]
        {
            drop(scope.take());
            scope = iced_wgpu::profiling::begin_cpu_scope("map.residency.pressure");
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
        #[cfg(feature = "gui-benchmark")]
        drop(scope);
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

#[cfg(all(test, feature = "map-capture"))]
mod lifecycle_tests;
