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

mod bounds_query;
mod clipping;
mod cubemap;
mod draw;
mod geometry;
mod images;
mod intersection;
mod lighting;
mod lightmaps;
mod marks;
mod materials;
mod model_lighting;
mod model_lighting_visibility;
mod passes;
mod planar;
mod reflections;
mod refraction;
mod residency;
mod resources;
mod samplers;
pub(super) mod sampling;
mod screen_refraction;
mod sky;
mod sun;
mod surface;
mod surface_pipelines;
mod targets;
pub(super) mod texture_upload;
mod transparency;
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
pub use uniforms::Uniforms;
#[cfg(feature = "map-capture")]
pub(super) use uniforms::{Diagnostic, DiagnosticMode};

use super::draw_plan::{DrawPlanRequest, DrawPlanSelection};
pub(super) use geometry::ViewerScene;
use reflections::{PlanarView, SceneView, ViewUniforms};
use resources::RenderResources;
use std::sync::atomic::Ordering;
use targets::{RenderTargets, WaterBindingInputs, WaterBindings};
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
    pub(super) render_feedback: Option<Arc<super::feedback::RenderFeedback>>,
    pub(super) model_lighting_time: f64,
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
}

#[cfg(feature = "map-capture")]
fn preparation_diagnostics(view: &SceneView) -> serde_json::Value {
    serde_json::json!({
        "semantic_world_cache":view.plan_cache.world_cache_diagnostics(),
        "world":view.plans.as_ref().map(|plans|&plans.world.world_opaque.issues),
        "skybox":view.plans.as_ref().and_then(|plans|plans.map_skybox.as_ref()).map(|plan|&plan.world_opaque.issues),
        "transparency":view.transparency_diagnostics,
        "skybox_transparency":view.skybox_transparency_diagnostics,
    })
}

impl ModelPrimitive {
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

        if let Some(feedback) = &self.render_feedback {
            let upload = pipeline.uploads.get(scene_key).expect("uploaded scene");
            let uploader = pipeline
                .resources
                .texture_uploader
                .lock()
                .expect("texture uploader lock");
            let capacity = upload.has_capacity_issues();
            let subscription = capacity.then(|| uploader.retirement_subscription());
            let retirement = uploader.retirement_state();
            drop(uploader);
            feedback.bind_retirement(subscription);
            let unavailable_images = upload.image_upload_issues.len();
            feedback.set(if capacity && retirement.pending != 0 {
                super::feedback::RenderStatus::Retiring { unavailable_images }
            } else {
                super::feedback::RenderStatus::Ready { unavailable_images }
            });
        }
        let slot = unclaimed_viewer_frame_slot(
            pipeline
                .frames
                .iter()
                .map(|frame| (frame.view_id, frame.live)),
            self.view_id,
        );
        if slot == pipeline.frames.len() {
            let lighting = lighting::Frame::new(device, &pipeline.resources.texture_uploader, map);
            let scene = ViewerScene::new(Arc::clone(
                pipeline.uploads.get(scene_key).expect("uploaded scene"),
            ));
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
            });
        }
        self.prepared_frame.store(slot, Ordering::Relaxed);
        let frame = &mut pipeline.frames[slot];
        if frame.scene_key != scene_key {
            frame.scene_key = scene_key;
            frame.scene = ViewerScene::new(Arc::clone(
                pipeline.uploads.get(scene_key).expect("uploaded scene"),
            ));
            frame.planar_views.clear();
            frame.water_intersection = None;
            frame.screen_overlay = None;
            frame.lighting =
                lighting::Frame::new(device, &pipeline.resources.texture_uploader, map);
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
                view.surface_buffer = visibility::upload_visible_indices(device, queue, &indices);
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
        for surface in &mut frame.planar_views {
            surface.prepare_optics(
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
            let refracted = surface
                .refraction
                .as_ref()
                .or(surface.reflection.as_ref())
                .and_then(|view| view.targets.as_ref())
                .expect("active planar input");
            let reflected = surface
                .reflection
                .as_ref()
                .and_then(|view| view.targets.as_ref())
                .unwrap_or(refracted);
            WaterBindings::ensure(
                &mut surface.water_bind_group,
                device,
                &pipeline.resources,
                WaterBindingInputs {
                    uniforms: &surface.surface_uniforms.buffer,
                    refraction: &refracted.resolve_view,
                    depth: &refracted.depth,
                    reflection: &reflected.resolve_view,
                    lightmap: &frame.lighting.lightmap,
                },
            );
        }
        screen_refraction::prepare(
            &mut frame.screen_overlay,
            device,
            queue,
            &pipeline.resources,
            upload,
            self,
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
        let upload = &frame.scene;
        #[cfg(feature = "map-capture")]
        let lighting_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::Lighting));
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
                    "preparation":preparation_diagnostics(view),
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
                "image_cache":pipeline.uploads.image_stats(), "image_resources": upload.assets.image_stats(), "image_upload_issues":upload.assets.image_upload_issues,
                "material_samplers":upload.assets.sampler_diagnostics.iter().map(samplers::SamplerDiagnostic::capture_json).collect::<Vec<_>>(),
                "raster_offset_physical_pixels":self.raster_offset(),"main_target_dimensions":size,"reflection_views":records(false),"refraction_views":records(true),
                "single_bounce":true,"nested_planar_fallback":"average sky tint (cubemap pending)",
                "water_intersection":frame.water_intersection.as_ref().and_then(|intersection|intersection.selection.map(|selection|serde_json::json!({"surface_index":selection.surface_index,"body_index":selection.body_index,"preparation":preparation_diagnostics(&intersection.view)}))),
                "water_body":map.and_then(|map|map.water_volumes.as_ref()).and_then(|volumes|volumes.at(eye)).map(|sample|serde_json::json!({"index":sample.body_index,"surface_z":sample.body.surface_z,"min_z":sample.body.min_z,"surface_material":sample.body.material_name,"contents":sample.contents})),
                "underwater_overlay":map.and_then(|map|map.immersion_material(eye)).and_then(|water|water.underwater_overlay.as_ref()).map(|effect|&effect.name),
                "clear_color_linear":self.clear_color,
                "main_camera_position":self.uniforms.camera_position, "main_fog_color":self.uniforms.fog_color, "main_fog":{"start":self.uniforms.fog.start,"end":self.uniforms.fog.end,"max_density":self.uniforms.fog.max_density,"mode":self.uniforms.fog.mode}, "main_clip_plane":main_uniforms.clip_plane, "main_visibility_state":format!("{:?}",frame.main.visibility.tracker.state),
                "main_preparation":preparation_diagnostics(&frame.main),
                "surface_mark_metadata_bytes":frame.marks.buffer.size(), "world_surface_marks":frame.main.plans.as_ref().unwrap().world.surface_mark_count(), "transparent_receiver_marks":frame.main.plans.as_ref().unwrap().world.receiver_mark_count(),
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
        #[cfg(feature = "map-capture")]
        let _encode_span = self
            .capture_diagnostics
            .as_ref()
            .and_then(|observer| observer.span(super::capture::Phase::Encode));
        let Some(frame) = pipeline
            .frames
            .get(self.prepared_frame.load(Ordering::Relaxed))
            .filter(|frame| frame.view_id == self.view_id && frame.scene_key == self.preview.key())
        else {
            return;
        };
        let upload = &frame.scene;
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
            resources: RenderResources::new(device, queue, format, None),
            uploads: UploadCache::new(None),
            frames: Vec::new(),
        }
    }
    fn new_with_capabilities(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        capabilities: &wgpu::DownlevelCapabilities,
    ) -> Self {
        Self {
            resources: RenderResources::new(device, queue, format, Some(capabilities)),
            uploads: UploadCache::new(Some(capabilities)),
            frames: Vec::new(),
        }
    }
    fn trim(&mut self) {
        self.frames.retain(|frame| frame.live);
        for frame in &mut self.frames {
            frame.live = false;
        }
        self.uploads.trim();
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
