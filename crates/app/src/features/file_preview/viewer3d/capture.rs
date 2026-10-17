#[path = "capture_isolation.rs"]
mod isolation;
#[path = "capture/performance.rs"]
mod performance;
use super::{FlyCamera, FlyPose, FlyViewer, Message, ModelPrimitive, MovementMode, Vec3, shader};
use crate::bridge::archive::PreviewArchiveSource;
use crate::generation::Generation;
use crate::media::file_preview_decode::{
    MapTextureResidency, TextureResourcePolicy, load_map_for_capture_with_model_options,
};
use crate::media::preview_model::{MaterialSlot, PreviewContent, PreviewRequest};
use iced::advanced::renderer::Headless as _;
use iced::{Color, Length, Rectangle, Size, mouse};
use iced_runtime::{UserInterface, user_interface};
pub use performance::CaptureDiagnostics;
#[cfg(feature = "gui-benchmark")]
pub use performance::{Pause, Profile};
pub(super) use performance::{Phase, Presentation, VisibilityMode};
use serde::Deserialize;
use serde_json::{Value, json};
use shader::Program as _;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

struct CaptureLogger;

fn benchmark_render(
    renderer: &mut iced::Renderer,
    size: Size<u32>,
    samples: usize,
    observers: &[CaptureDiagnostics],
) -> Option<Value> {
    if samples == 0 {
        return None;
    }
    let iced::Renderer::Primary(renderer) = renderer else {
        panic!("render benchmark requires the GPU renderer");
    };
    let format = renderer.target_format();
    let target = renderer
        .device()
        .create_texture(&super::wgpu::TextureDescriptor {
            label: Some("map_capture.benchmark"),
            size: super::wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: super::wgpu::TextureDimension::D2,
            format,
            usage: super::wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
    let view = target.create_view(&Default::default());
    let viewport = iced::advanced::graphics::Viewport::with_physical_size(size, 1.0);
    let mut render = || {
        let present_started = Instant::now();
        let submission = renderer.present(Some(Color::BLACK), format, &view, &viewport);
        let present_ms = present_started.elapsed().as_secs_f64() * 1000.0;
        let completion_started = Instant::now();
        renderer
            .device()
            .poll(super::wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(std::time::Duration::from_secs(30)),
            })
            .expect("benchmark GPU completion");
        (
            present_ms,
            completion_started.elapsed().as_secs_f64() * 1000.0,
        )
    };
    let warmup_profiles = observers
        .iter()
        .map(|observer| observer.profile(1))
        .collect::<Vec<_>>();
    render();
    let observer_warmup_phases = warmup_profiles
        .into_iter()
        .map(performance::Profile::finish)
        .collect::<Vec<_>>();
    let pauses = observers
        .iter()
        .map(CaptureDiagnostics::pause)
        .collect::<Vec<_>>();
    let profiles = observers
        .iter()
        .map(|observer| observer.profile(samples))
        .collect::<Vec<_>>();
    let mut present_times = Vec::with_capacity(samples);
    let mut completion_times = Vec::with_capacity(samples);
    let mut times = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        let (present_ms, completion_ms) = render();
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        present_times.push(present_ms);
        completion_times.push(completion_ms);
    }
    let phases = profiles
        .into_iter()
        .map(performance::Profile::finish)
        .collect::<Vec<_>>();
    drop(pauses);
    render();
    times.sort_by(f64::total_cmp);
    Some(json!({
        "samples": samples,
        "warmup_frames": 1,
        "untimed_observer_refresh_frames": 1,
        "capture_json_observer_in_timed_samples": false,
        "cpu_present_submit_wall": performance::distribution(present_times),
        "residual_submission_completion_wall": performance::distribution(completion_times),
        "per_view_cpu_phases": phases,
        "untimed_observer_warmup_cpu_phases": observer_warmup_phases,
        "phase_timings_overlap": true,
        "isolated_gpu_duration_measured": false,
        "physical_size": [size.width, size.height],
        "metric": "serialized production render plus GPU completion wall time",
        "median_ms": times[samples / 2],
        "p95_ms": times[(samples * 95).div_ceil(100).saturating_sub(1)],
        "min_ms": times[0],
        "max_ms": times[samples - 1],
        "includes_cpu_preparation_and_gpu_completion": true,
        "includes_readback": false,
        "includes_window_presentation_or_widget_update": false,
    }))
}

fn texture_readiness(statuses: &[super::feedback::RenderStatus]) -> Result<bool, String> {
    use super::feedback::RenderStatus;
    if statuses.is_empty() {
        return Err("capture has no render feedback".into());
    }
    for status in statuses {
        match status {
            RenderStatus::Rejected { reason } => return Err(reason.clone()),
            RenderStatus::RetirementTimedOut => {
                return Err("GPU resource retirement timed out".into());
            }
            RenderStatus::Pending | RenderStatus::Retiring { .. } | RenderStatus::Ready { .. } => {}
        }
    }
    Ok(statuses
        .iter()
        .all(|status| matches!(status, RenderStatus::Ready { .. })))
}

fn capture_when_ready(
    renderer: &mut iced::Renderer,
    size: Size<u32>,
    feedbacks: &[Arc<super::feedback::RenderFeedback>],
    observers: &[CaptureDiagnostics],
) -> Vec<u8> {
    let _pauses = observers
        .iter()
        .map(CaptureDiagnostics::pause)
        .collect::<Vec<_>>();
    let started = Instant::now();
    loop {
        // screenshot submits and waits for GPU completion, including page-upload retirements.
        let pixels = renderer.screenshot(size, 1.0, Color::BLACK);
        let statuses = feedbacks
            .iter()
            .map(|feedback| feedback.snapshot())
            .collect::<Vec<_>>();
        match texture_readiness(&statuses) {
            Ok(true) => return pixels,
            Ok(false) => {}
            Err(error) => panic!("capture texture preparation failed: {error}"),
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "capture texture preparation timed out: {statuses:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

static CAPTURE_LOGGER: CaptureLogger = CaptureLogger;
static CAPTURE_LOG_FILE: OnceLock<Mutex<fs::File>> = OnceLock::new();

impl log::Log for CaptureLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
            || metadata.target().starts_with("gmpublished")
            || (metadata.level() <= log::Level::Info && metadata.target().starts_with("iced_wgpu"))
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            if let Some(file) = CAPTURE_LOG_FILE.get() {
                writeln!(
                    file.lock().expect("capture log lock"),
                    "{} {}: {}",
                    record.level(),
                    record.target(),
                    record.args()
                )
                .expect("write capture log");
            }
            if record.level() <= log::Level::Warn {
                eprintln!("{}: {}", record.level(), record.args());
            }
        }
    }

    fn flush(&self) {
        if let Some(file) = CAPTURE_LOG_FILE.get() {
            file.lock()
                .expect("capture log lock")
                .flush()
                .expect("flush capture log");
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureSuite {
    width: u32,
    height: u32,
    animation_time: f32,
    #[serde(default)]
    benchmark_frames: usize,
    #[serde(default)]
    verify_view_isolation: bool,
    #[serde(default)]
    model_vertex_compression: CaptureModelVertexCompression,
    #[serde(default)]
    texture_residency: CaptureTextureResidency,
    maps: Vec<CaptureMap>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureTextureResidency {
    max_dimension: Option<u32>,
    resident_bytes: usize,
    #[serde(default = "CaptureTextureResidency::default_resident_images")]
    resident_images: usize,
    #[serde(default = "CaptureTextureResidency::default_source_decoder_bytes")]
    source_decoder_bytes: usize,
    #[serde(default = "CaptureTextureResidency::default_source_working_bytes")]
    source_working_bytes: usize,
    #[serde(default = "CaptureTextureResidency::default_source_encoded_bytes")]
    source_encoded_bytes: usize,
    #[serde(default = "CaptureTextureResidency::default_resource_resident_bytes")]
    resource_resident_bytes: usize,
    #[serde(default = "CaptureTextureResidency::default_payload_working_bytes")]
    payload_working_bytes: usize,
}

impl CaptureTextureResidency {
    fn default_resident_images() -> usize {
        TextureResourcePolicy::default().resident_images
    }
    fn default_source_decoder_bytes() -> usize {
        TextureResourcePolicy::default().source_decoder_bytes
    }
    fn default_source_working_bytes() -> usize {
        TextureResourcePolicy::default()
            .source_working_budget
            .capacity()
    }

    fn default_source_encoded_bytes() -> usize {
        TextureResourcePolicy::default().source_encoded_bytes
    }
    fn default_resource_resident_bytes() -> usize {
        TextureResourcePolicy::default().resource_resident_bytes
    }
    fn default_payload_working_bytes() -> usize {
        TextureResourcePolicy::default().working_budget.capacity()
    }
}

impl Default for CaptureTextureResidency {
    fn default() -> Self {
        let defaults = MapTextureResidency::default();
        Self {
            max_dimension: defaults.max_dimension,
            resident_bytes: defaults.resources.resident_bytes,
            resident_images: defaults.resources.resident_images,
            source_decoder_bytes: defaults.resources.source_decoder_bytes,
            source_working_bytes: defaults.resources.source_working_budget.capacity(),
            source_encoded_bytes: defaults.resources.source_encoded_bytes,
            resource_resident_bytes: defaults.resources.resource_resident_bytes,
            payload_working_bytes: defaults.resources.working_budget.capacity(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum CaptureModelVertexCompression {
    None,
    #[default]
    SourceUbyte4,
}

impl CaptureModelVertexCompression {
    fn options(self) -> vformats::mdl::ModelAssemblyOptions {
        vformats::mdl::ModelAssemblyOptions {
            vertex_compression: match self {
                Self::None => vformats::mdl::VertexCompression::None,
                Self::SourceUbyte4 => vformats::mdl::VertexCompression::SourceUbyte4,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureMap {
    name: String,
    #[serde(default)]
    archive: Option<PathBuf>,
    cameras: Vec<CaptureCamera>,
    #[serde(default)]
    verify_temporal_view_isolation: bool,
    #[serde(default)]
    verify_door_view_isolation: bool,
    #[serde(default)]
    traces: Vec<CaptureTrace>,
    #[serde(default)]
    shader_lighting: Option<CaptureModelLighting>,
    #[serde(default)]
    bone_pose_control: Option<Value>,
}

struct CaptureMapInput {
    source: Arc<PreviewArchiveSource>,
    entry_path: String,
    bsp_path: Option<PathBuf>,
    archive_path: Option<PathBuf>,
    bytes: Vec<u8>,
}

impl CaptureMap {
    fn validate_source(&self, maps_directory: &std::path::Path) -> Result<PathBuf, String> {
        let path = self
            .archive
            .clone()
            .unwrap_or_else(|| maps_directory.join(format!("{}.bsp", self.name)));
        if self.archive.is_some()
            && !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("gma"))
        {
            return Err(format!("capture archive must be a GMA: {}", path.display()));
        }
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("capture source {}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("capture source is not a file: {}", path.display()));
        }
        Ok(path)
    }

    fn read_source(&self, maps_directory: &std::path::Path) -> Result<CaptureMapInput, String> {
        let path = self.validate_source(maps_directory)?;
        let entry_path = format!("maps/{}.bsp", self.name);
        let (source, bsp_path, archive_path) = if self.archive.is_some() {
            let archive = crate::bridge::gma::PreviewArchive::open(&path)
                .map_err(|error| format!("capture archive {}: {error}", path.display()))?;
            (
                PreviewArchiveSource::from_gma(Arc::new(archive)),
                None,
                Some(path),
            )
        } else {
            let size = fs::metadata(&path)
                .map_err(|error| error.to_string())?
                .len();
            (
                PreviewArchiveSource::from_folder([(entry_path.clone(), size, path.clone())]),
                Some(path),
                None,
            )
        };
        let bytes = source
            .entry_bytes(&entry_path)
            .map_err(|error| format!("capture entry {entry_path}: {error}"))?;
        Ok(CaptureMapInput {
            source,
            entry_path,
            bsp_path,
            archive_path,
            bytes,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureModelLighting {
    targetname: String,
    model: String,
    model_path: String,
    origin: [f32; 3],
    #[serde(default)]
    lighting_origin: Option<[f32; 3]>,
    ambient_cube: [[f32; 3]; 6],
    ambient_cube_order: [String; 6],
    lights: Vec<CaptureModelLight>,
    suppress_engine_lighting: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CaptureModelLight {
    #[serde(rename = "type")]
    kind: String,
    color: [f32; 3],
    pos: [f32; 3],
    dir: [f32; 3],
    constant_falloff: f32,
    linear_falloff: f32,
    quadratic_falloff: f32,
    range: f32,
    #[serde(default)]
    angular_falloff: f32,
    #[serde(default)]
    inner_angle: f32,
    #[serde(default)]
    outer_angle: f32,
}

fn override_model_lighting(
    scene: &mut Arc<crate::media::preview_model::MapPreview>,
    control: &CaptureModelLighting,
) -> Value {
    use crate::media::preview_model::{ModelLightKind, ModelLocalLight};
    assert!(control.suppress_engine_lighting && control.model == control.model_path);
    assert_eq!(
        control.ambient_cube_order,
        ["+X", "-X", "+Y", "-Y", "+Z", "-Z"]
    );
    let scene = Arc::make_mut(scene);
    let indices = scene
        .model_lighting
        .iter()
        .enumerate()
        .filter(|(_, context)| {
            context.model_path == control.model_path && context.origin == Vec3::from(control.origin)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(
        indices.len(),
        1,
        "lighting override must identify exactly one placement"
    );
    let index = indices[0];
    let context = &mut scene.model_lighting[index];
    context.lighting_origin = control.lighting_origin.map_or(context.origin, Vec3::from);
    let lights: Vec<ModelLocalLight> = control
        .lights
        .iter()
        .map(|light| ModelLocalLight {
            kind: match light.kind.as_str() {
                "point" => ModelLightKind::Point,
                "spot" => ModelLightKind::Spot,
                "directional" => ModelLightKind::Directional,
                other => panic!("unsupported controlled light type {other}"),
            },
            position: Vec3::from(light.pos),
            direction: Vec3::from(light.dir),
            radiance: Vec3::from(light.color),
            attenuation: [
                light.constant_falloff,
                light.linear_falloff,
                light.quadratic_falloff,
            ],
            range: light.range,
            spot_exponent: light.angular_falloff,
            inner_cosine: (light.inner_angle.to_radians() * 0.5).cos(),
            outer_cosine: (light.outer_angle.to_radians() * 0.5).cos(),
        })
        .collect();
    assert!(lights.len() <= crate::media::preview_model::MODEL_LOCAL_LIGHT_LIMIT);
    context.source = crate::media::preview_model::ModelLightingSource::Explicit(
        crate::media::preview_model::ModelLightingSnapshot {
            ambient_cube: gmpublished_domain::scene::map::AmbientCube {
                colors: control.ambient_cube.map(Vec3::from),
            },
            lights,
        },
    );
    let id = crate::media::preview_model::ModelLightingContextId::from_index(index)
        .expect("bounded context index")
        .gpu();
    let mut changed = 0;
    for mesh in Arc::make_mut(&mut scene.scene)
        .meshes
        .iter_mut()
        .chain(&mut scene.map_skybox_meshes)
    {
        for vertex in &mut mesh.vertices {
            if vertex.lighting_context == id {
                vertex.lighting_mode &= !(crate::media::preview_model::vertex_lighting::PREVIEW
                    | crate::media::preview_model::vertex_lighting::LEGACY_VERTEX_COLOR);
                vertex.lighting_mode |=
                    crate::media::preview_model::vertex_lighting::RUNTIME_CONTEXT
                        | crate::media::preview_model::vertex_lighting::LINEAR_ILLUMINATION;
                vertex.color = Vec3::new(
                    f32::from(context.diffuse_modulation[0]) / 255.0,
                    f32::from(context.diffuse_modulation[1]) / 255.0,
                    f32::from(context.diffuse_modulation[2]) / 255.0,
                );
                changed += 1;
            }
        }
    }
    assert!(changed > 0, "controlled model has no rendered vertices");
    json!({"targetname":control.targetname,"model_path":control.model_path,"origin":control.origin,
        "lighting_origin":context.lighting_origin.to_array(),"native_lighting_origin_controlled":control.lighting_origin.is_some(),
        "diffuse_modulation_rgba":context.diffuse_modulation,"entity_render_mode":context.render_mode,
        "context_index":id,"vertices":changed,"ambient_cube_linear":control.ambient_cube,
        "lights":format!("{:?}",context.source.fallback_snapshot().lights),"scope":"capture-only explicit shader inputs; does not certify BSP lighting selection"})
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureTrace {
    name: String,
    start: [f32; 3],
    end: [f32; 3],
    half_extents: [f32; 3],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent capture-layer toggles and optional live event replay"
)]
struct CaptureCamera {
    name: String,
    eye: [f32; 3],
    /// Source degrees: positive pitch looks downward; yaw zero faces +X.
    angles: [f32; 2],
    #[serde(default)]
    clip_near: Option<f32>,
    #[serde(default)]
    clip_far: Option<f32>,
    #[serde(default)]
    horizontal_fov_degrees: Option<f32>,
    #[serde(default)]
    animation_time: Option<f32>,
    #[serde(default)]
    model_lighting_timeline: Option<ModelLightingTimeline>,
    #[serde(default)]
    sky_animation_time: Option<f32>,
    #[serde(default)]
    raster_offset: Option<[f32; 2]>,
    door_progress_override: Option<f32>,
    #[serde(default)]
    frame_visualization: Option<FrameVisualization>,
    #[serde(default)]
    frame_visualization_linear_output: bool,
    #[serde(default)]
    frame_probe: Option<FrameProbe>,
    #[serde(default)]
    live_animation: bool,
    #[serde(default)]
    fullbright: bool,
    #[serde(default)]
    phy_debug: bool,
    #[serde(default)]
    reflection_target: bool,
    #[serde(default)]
    refraction_target: bool,
    #[serde(default = "enabled")]
    skybox: bool,
    #[serde(default = "enabled")]
    fog: bool,
    #[serde(default = "enabled")]
    visibility: bool,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ModelLightingTimeline {
    pub model: String,
    pub origin: [f32; 3],
    pub draw_times_before_capture: Vec<f64>,
    pub capture_time: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum FrameVisualization {
    Normal,
    TangentS,
    TangentT,
    Position,
    UnitNormal,
    UnitTangentS,
    UnitEye,
    SurfacePosition,
    SurfaceUv,
    SurfaceAlpha,
    SurfaceBaseColor,
}

impl FrameVisualization {
    fn diagnostic_mode(self, probe: bool, linear_output: bool) -> super::pipeline::DiagnosticMode {
        use super::pipeline::DiagnosticMode;
        match (self, probe, linear_output) {
            (Self::Normal, true, false) => DiagnosticMode::RawNormal,
            (Self::TangentS, true, false) => DiagnosticMode::RawTangentS,
            (Self::TangentT, true, false) => DiagnosticMode::RawTangentT,
            (Self::Position, true, false) => DiagnosticMode::Position,
            (Self::UnitNormal, true, false) => DiagnosticMode::UnitNormal,
            (Self::UnitTangentS, true, false) => DiagnosticMode::UnitTangentS,
            (Self::UnitEye, true, false) => DiagnosticMode::UnitEye,
            (Self::SurfacePosition, true, false) => DiagnosticMode::SurfacePosition,
            (Self::SurfaceUv, true, false) => DiagnosticMode::SurfaceUv,
            (Self::SurfaceAlpha, true, false) => DiagnosticMode::SurfaceAlpha,
            (Self::SurfaceBaseColor, true, false) => DiagnosticMode::SurfaceBaseColor,
            (Self::Normal, false, false) => DiagnosticMode::VertexNormalColor,
            (Self::TangentS, false, false) => DiagnosticMode::VertexTangentSColor,
            (Self::TangentT, false, false) => DiagnosticMode::VertexTangentTColor,
            (Self::Normal, false, true) => DiagnosticMode::VertexNormalLinear,
            (Self::TangentS, false, true) => DiagnosticMode::VertexTangentSLinear,
            (Self::TangentT, false, true) => DiagnosticMode::VertexTangentTLinear,
            _ => panic!("raw frame diagnostics require a probe and disable linear-output mode"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct FrameProbe {
    gain: f32,
    bias: [f32; 3],
}

const fn enabled() -> bool {
    true
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureTarget {
    Main,
    Reflection,
    Refraction,
}
impl CaptureCamera {
    fn horizontal_fov_degrees(&self) -> Option<f32> {
        self.horizontal_fov_degrees.inspect(|&fov| {
            assert!(
                fov.is_finite() && fov > 0.0 && fov < 180.0,
                "capture horizontal FOV requires finite 0 < horizontal_fov_degrees < 180"
            );
        })
    }

    fn clip_distances(&self) -> Option<super::camera::CaptureClipDistances> {
        match (self.clip_near, self.clip_far) {
            (None, None) => None,
            (Some(near), Some(far)) => Some(
                super::camera::CaptureClipDistances::new(near, far)
                    .expect("capture clipping requires finite 0 < clip_near < clip_far"),
            ),
            _ => panic!("capture clipping requires both clip_near and clip_far"),
        }
    }

    fn lighting_timeline(&self) -> Option<Arc<ModelLightingTimeline>> {
        self.model_lighting_timeline.as_ref().map(|timeline| {
            assert!(
                !self.live_animation,
                "native timeline and live event replay are separate controls"
            );
            assert!(timeline.capture_time.is_finite() && timeline.capture_time >= 0.0);
            assert!(timeline.origin.iter().all(|value| value.is_finite()));
            let mut previous = 0.0;
            for &time in &timeline.draw_times_before_capture {
                assert!(time.is_finite() && time >= previous && time <= timeline.capture_time);
                previous = time;
            }
            Arc::new(timeline.clone())
        })
    }

    fn target(&self) -> CaptureTarget {
        assert!(
            !(self.reflection_target && self.refraction_target),
            "choose one optical diagnostic target"
        );
        if self.reflection_target {
            CaptureTarget::Reflection
        } else if self.refraction_target {
            CaptureTarget::Refraction
        } else {
            CaptureTarget::Main
        }
    }
}

struct FixedView {
    model_lighting_timeline: Option<Arc<ModelLightingTimeline>>,
    diagnostics: CaptureDiagnostics,
    viewer: FlyViewer,
    camera: FlyCamera,
    live_animation: bool,
    fullbright: bool,
    sky_animation_time: Option<f32>,
    raster_offset: Option<[f32; 2]>,
    door_progress_override: Option<f32>,
    frame_visualization: Option<FrameVisualization>,
    frame_visualization_linear_output: bool,
    frame_probe: Option<FrameProbe>,
    target: CaptureTarget,
}

impl shader::Program<Message> for FixedView {
    type State = FlyCamera;
    type Primitive = ModelPrimitive;

    fn update(
        &self,
        state: &mut FlyCamera,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<shader::Action<Message>> {
        state.capture_clip_distances = self.camera.capture_clip_distances;
        state.capture_horizontal_fov_degrees = self.camera.capture_horizontal_fov_degrees;
        self.live_animation
            .then(|| self.viewer.update(state, event, bounds, cursor))
            .flatten()
    }

    fn draw(&self, state: &FlyCamera, cursor: mouse::Cursor, bounds: Rectangle) -> ModelPrimitive {
        let mut primitive = self.viewer.draw(
            if self.live_animation {
                state
            } else {
                &self.camera
            },
            cursor,
            bounds,
        );
        primitive.exact_texture_frame = true;
        primitive.reflection_target = self.target == CaptureTarget::Reflection;
        primitive.refraction_target = self.target == CaptureTarget::Refraction;
        primitive.capture_diagnostics = Some(self.diagnostics.clone());
        primitive
            .capture_lighting_timeline
            .clone_from(&self.model_lighting_timeline);
        if let Some(timeline) = &self.model_lighting_timeline {
            primitive.model_lighting_time = timeline.capture_time;
        }
        primitive.raster_offset = self.raster_offset;
        if let Some(progress) = self.door_progress_override {
            assert!(progress.is_finite() && (0.0..=1.0).contains(&progress));
            for pose in &mut primitive.door_poses {
                pose.progress = progress;
            }
        }
        if let Some(probe) = self.frame_probe {
            assert!(
                self.frame_visualization.is_some()
                    && !self.fullbright
                    && !self.frame_visualization_linear_output
            );
            assert!(
                probe.gain.is_finite()
                    && probe.gain >= 0.0
                    && probe.bias.into_iter().all(f32::is_finite)
            );
        }
        if let Some(mode) = self.frame_visualization {
            let mode = mode.diagnostic_mode(
                self.frame_probe.is_some(),
                self.frame_visualization_linear_output,
            );
            let transform = self.frame_probe.map_or([0.0, 0.0, 0.0, 1.0], |probe| {
                [probe.bias[0], probe.bias[1], probe.bias[2], probe.gain]
            });
            let diagnostic = super::pipeline::Diagnostic::new(mode, transform);
            primitive.uniforms.diagnostic = diagnostic;
            if let Some(skybox) = primitive.map_skybox_uniforms.as_mut() {
                skybox.diagnostic = diagnostic;
            }
        }
        if let (Some(time), Some(sky)) = (self.sky_animation_time, primitive.sky_uniforms.as_mut())
        {
            sky.water_time_sky_tint[0] = time;
        }
        if self.fullbright {
            primitive.uniforms.shading_mode = super::pipeline::ShadingMode::Fullbright as u32;
            if let Some(skybox) = primitive.map_skybox_uniforms.as_mut() {
                skybox.shading_mode = super::pipeline::ShadingMode::Fullbright as u32;
            }
        }
        primitive
    }
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn executable_identity(output: &std::path::Path) -> Value {
    let executable = std::env::current_exe().expect("capture executable path");
    let sha256 = Command::new("sha256sum")
        .arg(&executable)
        .output()
        .ok()
        .filter(|result| result.status.success())
        .and_then(|result| String::from_utf8(result.stdout).ok())
        .and_then(|text| text.split_whitespace().next().map(str::to_owned));
    let provenance_path = executable.with_file_name("provenance.json");
    let provenance = fs::read(&provenance_path).ok().map(|bytes| {
        serde_json::from_slice::<Value>(&bytes).expect("frozen build provenance JSON")
    });
    if let Some(provenance) = &provenance {
        assert!(
            sha256.as_ref().is_some_and(
                |hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            ),
            "frozen executable requires a SHA-256 fingerprint"
        );
        assert_eq!(
            sha256.as_deref(),
            provenance["sha256"].as_str(),
            "frozen executable fingerprint mismatch"
        );
        fs::write(
            output.join("executable-provenance.json"),
            serde_json::to_vec_pretty(provenance).unwrap(),
        )
        .expect("copy executable provenance");
        if provenance["stable_inputs_during_build"] == true {
            fs::copy(
                executable.with_file_name("source.tar.gz"),
                output.join("executable-source.tar.gz"),
            )
            .expect("copy frozen build source archive");
        }
    }
    json!({"path":executable,"sha256":sha256,"provenance":provenance,
        "workspace_snapshot_scope":"capture-time workspace; source.patch and untracked-source do not identify a frozen executable's build inputs"})
}

fn texture_metadata_info(metadata: &crate::bridge::materials::TextureMetadata) -> Value {
    json!({"flags":metadata.flags, "dimensions":metadata.original_dimensions,
        "frames":metadata.frame_count, "faces":metadata.face_count, "mips":metadata.mip_count,
        "format":metadata.format.map(|format|format!("{format:?}")), "first_frame":metadata.first_frame,
        "version_minor":metadata.version_minor,"reflectivity_bits":metadata.reflectivity_bits,
        "bumpmap_scale_bits":metadata.bumpmap_scale_bits})
}

fn texture_storage_name(storage: crate::bridge::materials::TextureStorage) -> String {
    match storage {
        crate::bridge::materials::TextureStorage::Rgba8 => "RGBA8".into(),
        crate::bridge::materials::TextureStorage::Bc(format) => format!("{format:?}"),
    }
}

fn texture_recipe_info(recipe: &crate::bridge::materials::TextureFrameRequest) -> Value {
    json!({"process_source_id":format!("{:?}",recipe.source_id), "frame":recipe.frame,
        "faces":[recipe.faces.start,recipe.faces.end],"first_source_mip":recipe.first_source_mip,
        "mip_count":recipe.mip_count,"storage":texture_storage_name(recipe.storage)})
}

fn texture_info(texture: &crate::bridge::materials::ResolvedTexture) -> Value {
    let on_demand = texture.on_demand();
    let resident = texture.resident();
    let format = on_demand
        .map(|(_, recipe)| texture_storage_name(recipe.storage))
        .or_else(|| resident.map(|resident| texture_storage_name(resident.id().storage)));
    let mip_levels = on_demand
        .map(|(_, recipe)| recipe.mip_count)
        .or_else(|| resident.map(|resident| resident.id().mip_count));
    let payload_crc32 = resident.map(|_| {
        let mut hash = crc32fast::Hasher::new();
        if let Some((_, mips)) = texture.bc_payload() {
            for mip in mips {
                hash.update(mip.bytes.as_slice());
            }
        } else {
            for mip in texture.mip_chain() {
                hash.update(mip.rgba);
            }
        }
        format!("{:08x}", hash.finalize())
    });
    json!({
        "state": if on_demand.is_some() { "on_demand" } else if resident.is_some() { "resident" } else { "unmaterialized" },
        "recipe": on_demand.map(|(_, recipe)| texture_recipe_info(recipe)),
        "dimensions": [texture.width(), texture.height()],
        "original_dimensions": texture.original_dimensions(),
        "mapping_dimensions": texture.mapping_dimensions(),
        "effective_base_texture": texture.has_effective_base_texture(),
        "mip_levels": mip_levels,
        "format": format,
        "payload_crc32": payload_crc32,
        "water_fallback": texture.is_water_fallback(),
        "vtf_flags": texture.flags(),
        "load_outcome": format!("{:?}",texture.load_outcome()),
        "effective_reflectivity_bits":texture.reflectivity_bits(),
        "source_metadata":texture.source_metadata().map(texture_metadata_info),
        "resident": resident.map(|resident|json!({"frames":[resident.id().frames.start,resident.id().frames.end],
            "faces":[resident.id().faces.start,resident.id().faces.end],
            "first_source_mip":resident.id().first_source_mip,
            "mip_count":resident.id().mip_count,"bytes":resident.byte_len()})),
    })
}

fn material_info(slot: &MaterialSlot) -> Value {
    let water = slot.shader.water().map(|water| {
        let frames = water.normal_map.as_ref().and_then(|normal| {
            normal.resident()?;
            Some((0..normal.frame_count())
                .filter_map(|index| normal.frame(index))
                .map(|mips| {
                    mips.iter()
                        .map(|mip| {
                            json!({
                                "dimensions": mip.dimensions,
                                "payload_crc32": format!("{:08x}", crc32fast::hash(mip.bytes.as_slice())),
                                "bytes": mip.bytes.as_slice().len(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>())
        });
        json!({
            "shader": water.shader,
            "parameters": water.parameters,
            "normal_map_path": water.normal_map_path,
            "normal_frames_linear_rgba": frames,
            "normal_state": water.normal_map.as_ref().map(|normal| if normal.on_demand().is_some() { "on_demand" } else if normal.resident().is_some() { "resident" } else { "unmaterialized" }),
            "normal_page_recipe": water.normal_map.as_ref().and_then(|normal| normal.on_demand().map(|(_, recipe)| texture_recipe_info(recipe))),
            "normal_source_metadata":water.normal_map.as_ref().and_then(|normal| normal.source_metadata().map(texture_metadata_info)),
            "bump_transform": water.bump_transform,
            "proxy_scroll": water.proxy_scroll,
            "scroll1": water.scroll1, "scroll2": water.scroll2,
            "bump_frame": water.bump_frame, "frame_rate": water.frame_rate,
        "animation_wrap": water.animation_wrap,
            "has_base_texture": water.has_base_texture,
            "above_water": water.above_water,
            "force_cheap":water.force_cheap,"reflect_entities":water.reflect_entities,"reflect_3d_skybox":water.reflect_3d_skybox,
            "reflect_enabled": water.reflect_enabled, "refract_enabled": water.refract_enabled,
            "reflect_amount": water.reflect_amount, "refract_amount": water.refract_amount,
            "reflect_tint_linear": water.reflect_tint_linear,
            "refract_tint_linear": water.refract_tint_linear,
            "fog_color_linear": water.fog_color_linear, "scene_fog_color_linear":water.scene_fog_color_linear, "underwater_overlay":water.underwater_overlay.as_ref().map(|effect|serde_json::json!({"name":effect.name,"refract_amount":effect.refract_amount,"blur":effect.blur,"bump_transform":effect.bump_transform,"proxy_scroll":effect.proxy_scroll})),
            "fog_start": water.fog_start, "fog_end": water.fog_end,
            "fog_enabled": water.fog_enabled,
            "below_material": water.below_material,
            "unsupported_proxies": water.unsupported_proxies,
            "unsupported_features": water.unsupported_features,
        })
    });
    json!({
        "name": slot.name,
        "resolution":format!("{:?}",slot.resolution),
        "canonical_name":slot.canonical_name,
        "shader": slot.shader.shader_name(),
        "mapping_dimensions": slot.shader.mapping_dimensions(slot.texture.as_deref()),
        "water": water,
        "surface": slot.shader.surface().map(|surface|json!({
            "family":format!("{:?}",surface.family),
            "parameters":surface.parameters,
            "color":surface.color,"color2":surface.color2,"alpha":surface.alpha,
            "base_frame":surface.base_frame,"base2_frame":surface.base2_frame,
            "base_transform":surface.base_transform,"base2_transform":surface.base2_transform,
            "base_blend": format!("{:?}",surface.base_blend),
            "detail":surface.detail.as_ref().map(|detail|json!({"path":detail.path,"frame":detail.frame,"scale":detail.scale,"mode":detail.mode,"effective_mode":surface.effective_detail_mode(),"factor":detail.factor,"tint":detail.tint,"transform":detail.transform,"texture_available":detail.texture.is_some()})),
            "emission":surface.emission.as_ref().map(|emission|json!({"self_illumination":emission.self_illumination,"envmap_mask_alpha":emission.envmap_mask_alpha,"tint":emission.tint,"mask":emission.mask_path,"fresnel":emission.fresnel})),
            "envmap":surface.envmap.as_ref().map(|envmap|json!({"path":envmap.path,"frame":envmap.frame,"mask":format!("{:?}",envmap.mask),"tint":envmap.tint})),
            "phong":surface.phong.as_ref().map(|phong|json!({
                "enabled":phong.enabled,"exponent":phong.exponent,"exponent_factor":phong.exponent_factor,
                "boost":phong.boost,"fresnel_ranges":phong.fresnel_ranges,"tint":phong.tint,
                "albedo_tint":phong.albedo_tint,"base_alpha_mask":phong.base_alpha_mask,
                "invert_mask":phong.invert_mask,"half_lambert":phong.half_lambert,
                "exponent_texture":phong.exponent_texture.as_ref().map(|texture|json!({"path":texture.path,"texture_available":texture.texture.is_some()})),
                "specular_warp":phong.specular_warp.as_ref().map(|texture|json!({"path":texture.path,"texture_available":texture.texture.is_some()})),
                "diffuse_warp":phong.diffuse_warp.as_ref().map(|texture|json!({"path":texture.path,"texture_available":texture.texture.is_some()})),
                "rim":{"enabled":phong.rim.enabled,"exponent":phong.rim.exponent,"boost":phong.rim.boost,"exponent_alpha_mask":phong.rim.exponent_alpha_mask}
            })),
            "unsupported_proxies":surface.unsupported_proxies,
            "unsupported_features":surface.unsupported_features(),
            "unavailable_textures":surface.unavailable_textures().iter().map(|texture|json!({"parameter":texture.parameter,"path":texture.path})).collect::<Vec<_>>(),
        })),
        "unlit": slot.shader.unlit().map(|material|json!({"vertex_color":material.vertex_color,"vertex_alpha":material.vertex_alpha})),
        "mirror": slot.shader.mirror().map(|mirror| json!({"shader":mirror.shader,"parameters":mirror.parameters,"fresnel_power":mirror.fresnel_power,"min_reflectivity":mirror.min_reflectivity,"max_reflectivity":mirror.max_reflectivity,"normal_map_path":mirror.normal_map_path,"unsupported_features":mirror.unsupported_features})),
        "render_mode": format!("{:?}", slot.render_mode),
        "force_opaque": slot.alpha_policy.force_opaque(),
        "texture": slot.texture.as_deref().map(texture_info),
        "texture2": slot.texture2.as_deref().map(texture_info),
    })
}

fn sky_info(sky: &crate::media::preview_model::Skybox) -> Value {
    json!({
        "faces": sky.materials.iter().map(|material| material.as_ref().map(|material|json!({
            "shader": material.shader,
            "color_linear": material.color.to_array(),
            "texture_transform": material.transform,
            "texture": material.texture.as_deref().map(texture_info),
            "paint_proxy": material.paint_proxy,
            "painted": material.painted.as_ref().map(|paint|json!({
                "top_color_linear": paint.top_color.to_array(),
                "bottom_color_linear": paint.bottom_color.to_array(),
                "fade_bias": paint.fade_bias,
                "sun_normal": paint.sun_normal.to_array(),
                "sun_color_linear": paint.sun_color.to_array(),
                "sun_size": paint.sun_size,
                "dusk_color_linear": paint.dusk_color.to_array(),
                "dusk_scale": paint.dusk_scale,
                "dusk_intensity": paint.dusk_intensity,
                "hdr_scale": paint.hdr_scale,
                "star_texture": paint.star_texture,
                "star_layers": paint.star_layers,
                "star_scale": paint.star_scale,
                "star_fade": paint.star_fade,
                "star_position": paint.star_position,
                "star_speed": paint.star_speed,
                "frames": paint.texture.as_ref().map(|texture| (0..texture.frame_count()).filter_map(|index| texture.frame(index)).map(|mips|mips.iter().map(|mip|json!({
                    "dimensions": mip.dimensions,
                    "rgba_crc32": format!("{:08x}",crc32fast::hash(mip.bytes.as_slice())),
                })).collect::<Vec<_>>()).collect::<Vec<_>>()),
            })),
        }))).collect::<Vec<_>>(),
        "paint": sky.paint.as_ref().map(|paint|json!({
            "top_color_linear": paint.top_color.to_array(),
            "bottom_color_linear": paint.bottom_color.to_array(),
            "fade_bias": paint.fade_bias,
            "sun_normal": paint.sun_normal.to_array(),
            "authored_sun_normal": paint.authored_sun_normal.to_array(),
            "sun_position_method": paint.sun_position_method,
            "sun_color_linear": paint.sun_color.to_array(),
            "sun_size": paint.sun_size,
            "dusk_color_linear": paint.dusk_color.to_array(),
            "dusk_scale": paint.dusk_scale,
            "dusk_intensity": paint.dusk_intensity,
            "draw_stars": paint.draw_stars,
            "star_texture": paint.star_texture,
            "star_layers": paint.star_layers,
            "star_scale": paint.star_scale,
            "star_fade": paint.star_fade,
            "star_speed": paint.star_speed,
            "client_star_scale": paint.client_star_scale(),
            "client_star_fade": paint.client_star_fade(),
            "client_star_speed": paint.client_star_speed(),
            "hdr_scale": paint.hdr_scale,
            "effective_ldr_hdr_scale": 1.0,
        })),
    })
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned()
    })
}

pub fn capture_map_reference_frames() {
    let texture_upload_policy = super::pipeline::capture_texture_upload_policy();
    let resource_defaults = TextureResourcePolicy::default();
    let suite_path = std::env::var_os("GMPUBLISHED_MAP_CAPTURE_SUITE").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/map-capture/cameras.json"),
        PathBuf::from,
    );
    let suite_bytes = fs::read(&suite_path).expect("read camera suite");
    let suite: CaptureSuite = serde_json::from_slice(&suite_bytes).expect("parse camera suite");
    assert!((1..=4096).contains(&suite.width) && (1..=4096).contains(&suite.height));
    assert!(
        suite
            .texture_residency
            .max_dimension
            .is_none_or(|value| value > 0)
            && suite.texture_residency.resident_bytes > 0
    );
    assert!(suite.animation_time.is_finite() && suite.animation_time >= 0.0);
    assert!(suite.benchmark_frames <= 1000);
    assert!(!suite.maps.is_empty());
    let mut names = std::collections::HashSet::new();
    for map in &suite.maps {
        assert!(valid_name(&map.name) && names.insert(map.name.clone()));
        assert!(!map.cameras.is_empty());
        if suite.verify_view_isolation {
            assert!(
                map.cameras
                    .iter()
                    .filter(|camera| {
                        !camera.reflection_target
                            && !camera.refraction_target
                            && !camera.live_animation
                    })
                    .take(2)
                    .count()
                    == 2,
                "isolation gate needs two fixed main cameras for {}",
                map.name
            );
        }
        let mut camera_names = std::collections::HashSet::new();
        let mut trace_names = std::collections::HashSet::new();
        for trace in &map.traces {
            assert!(valid_name(&trace.name) && trace_names.insert(trace.name.clone()));
            assert!(
                trace
                    .start
                    .iter()
                    .chain(&trace.end)
                    .chain(&trace.half_extents)
                    .all(|v| v.is_finite())
            );
            assert!(trace.half_extents.iter().all(|v| *v >= 0.0));
        }
        for camera in &map.cameras {
            let _ = camera.clip_distances();
            assert!(valid_name(&camera.name) && camera_names.insert(camera.name.clone()));
            assert!(
                camera
                    .eye
                    .iter()
                    .chain(&camera.angles)
                    .all(|v| v.is_finite())
            );
            assert!(camera.angles[0].abs() < 89.0, "avoid a singular view basis");
        }
    }
    let game = PathBuf::from(
        std::env::var_os("GMPUBLISHED_MAP_CAPTURE_GAME")
            .expect("set GMPUBLISHED_MAP_CAPTURE_GAME to the GarrysMod installation directory"),
    );
    assert!(
        game.join("garrysmod/garrysmod_dir.vpk").is_file(),
        "use the installation directory containing garrysmod and sourceengine"
    );
    let maps_directory = std::env::var_os("GMPUBLISHED_MAP_CAPTURE_MAPS")
        .map_or_else(|| game.join("garrysmod/maps"), PathBuf::from);
    let game_vpks = gmpublished_backend::vpk::discover_game_vpks(&game).expect("source discovery");
    assert!(
        game_vpks.contains(&game.join("garrysmod/garrysmod_dir.vpk")),
        "the game asset resolver must discover the primary Garry's Mod VPK"
    );
    for map in &suite.maps {
        map.validate_source(&maps_directory)
            .expect("capture input source");
    }
    let output = PathBuf::from(
        std::env::var_os("GMPUBLISHED_MAP_CAPTURE_DIR")
            .expect("set GMPUBLISHED_MAP_CAPTURE_DIR to a new output directory"),
    );
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).expect("capture parent directory");
    }
    fs::create_dir(&output).expect("use a fresh directory to preserve previous captures");
    CAPTURE_LOG_FILE
        .set(Mutex::new(
            fs::File::create(output.join("renderer.log")).expect("create capture log"),
        ))
        .expect("initialize capture log once");
    log::set_logger(&CAPTURE_LOGGER).expect("install capture logger");
    log::set_max_level(log::LevelFilter::Debug);
    fs::write(output.join("cameras.json"), &suite_bytes).expect("copy exact camera suite");
    fs::write(
        output.join("capture-harness.rs"),
        include_str!("capture.rs"),
    )
    .expect("record harness source including untracked changes");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let untracked_sources = git_output(&[
        "ls-files",
        "--others",
        "--exclude-standard",
        "--",
        "crates/app/src",
        "crates/domain/src",
        "crates/vendor/vformats",
        "tools/map-capture",
    ])
    .unwrap_or_default();
    for relative in untracked_sources.lines() {
        let relative = std::path::Path::new(relative);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            continue;
        }
        if !relative.starts_with("crates/vendor/vformats")
            && !relative
                .extension()
                .is_some_and(|ext| matches!(ext.to_str(), Some("rs" | "wgsl" | "json" | "py")))
        {
            continue;
        }
        let destination = output.join("untracked-source").join(relative);
        fs::create_dir_all(destination.parent().expect("source parent"))
            .expect("create source snapshot directory");
        fs::copy(workspace.join(relative), destination)
            .expect("snapshot new renderer and harness source");
    }
    let mut manifest = json!({
        "status": "in_progress",
        "app_version": env!("CARGO_PKG_VERSION"),
        "git_revision": git_output(&["rev-parse", "HEAD"]),
        "git_status": git_output(&["status", "--short"]),
        "game_installation_directory": game,
        "discovered_game_vpks": game_vpks,
        "maps_directory": maps_directory,
        "viewport": [suite.width, suite.height],
        "scene_msaa_samples": super::scene_sample_count(),
        "optical_msaa_samples": 1,
        "requested_bc_upload": super::pipeline::capture_bc_upload(),
        "sampler_quality": {
            "profile_override": std::env::var("GMPUBLISHED_MAP_CAPTURE_SAMPLER_PROFILE").ok(),
            "anisotropy_override": std::env::var("GMPUBLISHED_MAP_CAPTURE_ANISOTROPY").ok(),
            "effective": super::pipeline::capture_sampler_quality(),
        },
        "texture_upload_policy": texture_upload_policy,
        "texture_residency": {
            "source_discovery_limits": resource_defaults.source_discovery_limits,
            "archive_index_limits": {
                "directory_bytes": resource_defaults.archive_index_limits.directory_bytes,
                "entries": resource_defaults.archive_index_limits.entries,
                "retained_bytes": resource_defaults.archive_index_limits.retained_bytes,
            },
            "archive_index_working_bytes": resource_defaults.archive_index_working_budget.capacity(),
            "archive_index_working_scope": "process",
            "max_dimension": suite.texture_residency.max_dimension,
            "resident_bytes": suite.texture_residency.resident_bytes,
            "resident_images": suite.texture_residency.resident_images,
            "source_decoder_bytes": suite.texture_residency.source_decoder_bytes,
            "source_working_bytes": suite.texture_residency.source_working_bytes,
            "source_working_scope": "map",
            "source_encoded_bytes": suite.texture_residency.source_encoded_bytes,
            "resource_resident_bytes": suite.texture_residency.resource_resident_bytes,
            "payload_working_bytes": suite.texture_residency.payload_working_bytes,
            "payload_working_scope": "map",
            "host_limits": crate::bridge::materials::MaterialResolver::texture_host_limits(),
        },
        "scale_factor": 1.0,
        "vertical_fov_degrees": super::FOV_Y.to_degrees(),
        "animation_time_seconds": suite.animation_time,
        "benchmark_frames_per_view": suite.benchmark_frames,
        "model_vertex_compression": suite.model_vertex_compression,
        "backend": "wgpu headless using the production map pipeline",
        "adapter_and_shader_diagnostics": "renderer.log",
        "test_build": cfg!(test),
        "executable": executable_identity(&output),
        "wgpu_backend_override": std::env::var("WGPU_BACKEND").ok(),
        "game_reference_captures": false,
        "maps": [],
    });
    fs::write(
        output.join("source.patch"),
        format!(
            "{}\n",
            git_output(&["diff", "--no-ext-diff", "HEAD"]).unwrap_or_default()
        ),
    )
    .expect("record tracked source changes");
    write_manifest(&output, &manifest);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(std::thread::available_parallelism().map_or(1, |n| n.get().min(8)))
        .build()
        .expect("capture decode pool");
    let mut renderer = futures::executor::block_on(iced::Renderer::new(
        iced::Font::default(),
        iced::Pixels(16.0),
        Some("wgpu"),
    ))
    .expect("a wgpu adapter is required; software UI rendering cannot capture map shaders");
    let size = Size::new(suite.width as f32, suite.height as f32);
    let physical_size = Size::new(suite.width, suite.height);
    let theme = iced::Theme::Dark;
    for map in &suite.maps {
        println!("map capture: loading {}", map.name);
        let input = map
            .read_source(&maps_directory)
            .expect("read capture map source");
        let bytes = &input.bytes;
        let request = PreviewRequest {
            cancellation: Default::default(),
            request_id: Generation::INITIAL.next(),
            archive: Arc::clone(&input.source),
            entry_path: input.entry_path.clone(),
            display_name: format!("{}.bsp", map.name),
            size_bytes: bytes.len() as u64,
            crc32: crc32fast::hash(bytes),
            bypass_size_limits: true,
            map_texture_quality: Default::default(),
        };
        let load_started = Instant::now();
        let preview = load_map_for_capture_with_model_options(
            &request,
            game.clone(),
            &pool,
            suite.model_vertex_compression.options(),
            &MapTextureResidency {
                max_dimension: suite.texture_residency.max_dimension,
                resources: TextureResourcePolicy {
                    resident_bytes: suite.texture_residency.resident_bytes,
                    resident_images: suite.texture_residency.resident_images,
                    source_decoder_bytes: suite.texture_residency.source_decoder_bytes,
                    source_encoded_bytes: suite.texture_residency.source_encoded_bytes,
                    resource_resident_bytes: suite.texture_residency.resource_resident_bytes,
                    working_budget: Arc::new(crate::bridge::materials::TextureWorkingBudget::new(
                        suite.texture_residency.payload_working_bytes,
                    )),
                    source_working_budget: Arc::new(
                        crate::bridge::materials::TextureWorkingBudget::new(
                            suite.texture_residency.source_working_bytes,
                        ),
                    ),
                    ..resource_defaults.clone()
                },
            },
        )
        .expect("load map preview");
        let load_ms = load_started.elapsed().as_secs_f64() * 1000.0;
        let content_id = preview.content_id();
        let PreviewContent::Map {
            mut scene,
            stats,
            fog,
            sky_camera,
            spawn,
        } = preview.content
        else {
            panic!("{} did not decode as a map", map.name);
        };
        let shader_lighting = map
            .shader_lighting
            .as_ref()
            .map(|control| override_model_lighting(&mut scene, control));
        let directory = output.join(&map.name);
        fs::create_dir(&directory).expect("map capture directory");
        fs::write(
            directory.join("materials.json"),
            serde_json::to_vec_pretty(
                &scene
                    .scene
                    .materials
                    .iter()
                    .map(material_info)
                    .collect::<Vec<_>>(),
            )
            .expect("serialize material diagnostics"),
        )
        .expect("write material diagnostics");
        let mut runtime_vertex_counts = vec![0_usize; scene.model_lighting.len()];
        for vertex in scene
            .scene
            .meshes
            .iter()
            .chain(&scene.map_skybox_meshes)
            .flat_map(|mesh| &mesh.vertices)
        {
            if vertex.lighting_mode & crate::media::preview_model::vertex_lighting::RUNTIME_CONTEXT
                != 0
                && let Some(count) = vertex
                    .lighting_context
                    .checked_sub(1)
                    .and_then(|index| runtime_vertex_counts.get_mut(index as usize))
            {
                *count += 1;
            }
        }
        let mut map_record = json!({
            "name": map.name,
            "shader_lighting":shader_lighting,
            "native_bone_pose_control":map.bone_pose_control,
            "bsp_path": input.bsp_path,
            "archive_path": input.archive_path,
            "entry_path": input.entry_path,
            "entry_source": if map.archive.is_some() { "gma" } else { "folder" },
            "bsp_bytes": bytes.len(),
            "bsp_crc32": format!("{:08x}", request.crc32),
            "load_wall_time_ms": load_ms,
            "stats": format!("{stats:?}"),
            "prop_geometry": scene.prop_geometry,
            "texture_preparation": scene.texture_preparation,
            "invalid_detail_leaves": stats.invalid_detail_leaves,
            "prop_geometry_totals": scene.prop_geometry.admitted_totals(),
            "lightmap_diagnostics": {
                "valid_payloads":stats.lightmap.valid_payloads,
                "missing_payloads":stats.lightmap.missing_payloads,
                "invalid_payloads":stats.lightmap.invalid_payloads,
                "atlas_dropped_blocks":stats.lightmap.atlas_dropped_blocks,
                "unlightmapped_surfaces":stats.lightmap.unlightmapped_surfaces,
            },
            "sky_camera": format!("{sky_camera:?}"),
            "fog": format!("{fog:?}"),
            "world_meshes": scene.scene.meshes.len(),
            "skybox_meshes": scene.map_skybox_meshes.len(),
            "model_lighting_contexts": scene.model_lighting.iter().enumerate().map(|(index, context)| json!({
                "id":index+1,"model":context.model_path,"entity":context.entity,
                "origin":context.origin.to_array(),"lighting_origin":context.lighting_origin.to_array(),
                "mdl_flags":context.mdl_flags,"compiled_vhv":context.compiled_lighting,
                "lighting_source":match &context.source {
                    crate::media::preview_model::ModelLightingSource::Runtime {..}=>"runtime",
                    crate::media::preview_model::ModelLightingSource::Explicit(_)=>"explicit",
                    crate::media::preview_model::ModelLightingSource::DiagnosedFallback {..}=>"diagnosed_fallback",
                },
                "seed":match &context.source {
                    crate::media::preview_model::ModelLightingSource::Runtime {seed,..}=>json!({"sample_point":seed.sample_point.to_array(),"leaf":seed.leaf,
                        "ambient":seed.ambient_cube.colors.map(Vec3::to_array),"diagnostics":format!("{:?}",seed.diagnostics)}),
                    crate::media::preview_model::ModelLightingSource::Explicit(_)=>Value::Null,
                    crate::media::preview_model::ModelLightingSource::DiagnosedFallback {reason,..}=>json!({"error":format!("{reason:?}")}),
                },
                "compiled_supplement":match &context.compiled_supplement {
                    None=>Value::Null,
                    Some(Ok(seed))=>json!({"sample_point":seed.sample_point.to_array(),"leaf":seed.leaf,
                        "ambient":seed.ambient_cube.colors.map(Vec3::to_array),"diagnostics":format!("{:?}",seed.diagnostics)}),
                    Some(Err(error))=>json!({"error":format!("{error:?}")}),
                },
                "runtime_vertices":runtime_vertex_counts[index],
            })).collect::<Vec<_>>(),
            "sky": scene.skybox.as_ref().map(sky_info),
            "suns": scene.suns.iter().map(|sun|json!({
                "direction_to_sun": sun.direction_to_sun.to_array(),
                "color": sun.color_srgb,
                "overlay_color": sun.overlay_color_srgb,
                "size": sun.size,
                "overlay_size": sun.overlay_size,
                "material": sun.material,
                "overlay_material": sun.overlay_material,
                "hdr_color_scale": sun.hdr_color_scale,
                "enabled": sun.enabled,
                "start_disabled": sun.start_disabled,
                "spawn_flags": sun.spawn_flags,
            })).collect::<Vec<_>>(),
            "baked_lighting": scene.lightmap.as_ref().map(|set| serde_json::json!({"source":format!("{:?}",set.source),"storage":"virtual_pages","dimensions":null,"pages":set.pages().iter().enumerate().map(|(id,page)|json!({"id":id,"dimensions":[page.width,page.height],"active_styles":page.active_styles()})).collect::<Vec<_>>(),"animated":set.is_animated(),"active_styles":set.active_styles(),"initial_weights":set.active_styles().into_iter().map(|index|(index.to_string(),set.style_values(0.0)[usize::from(index)])).collect::<std::collections::BTreeMap<_,_>>()})),
            "lightmap_crc32": null,
            "source_lightmap_layout": scene.surface_lightmaps.as_ref().map(|layout| json!({
                "profile": [layout.profile.width, layout.profile.height],
                "physical_pages": layout.physical_page_count,
                "surfaces": layout.surfaces.iter().enumerate().map(|(face, allocation)| json!({
                    "face": face, "lightmap_page": allocation.page.native_id(),
                    "offset": allocation.offset, "material_group": allocation.material_group,
                })).collect::<Vec<_>>(),
            })),
            "surface_mark_lighting": scene.overlays.iter().chain(&scene.map_skybox_overlays).map(|mark| json!({
                "source":format!("{:?}",mark.source), "receiver_face":mark.receiver_range.face,
                "material":scene.scene.materials[mark.material_index].name,
                "lighting_mode":mark.vertices.first().map(|vertex|vertex.lighting_mode),
                "directional_step":mark.vertices.first().map(|vertex|vertex.lightmap_bump_step),
                "decal_sort":mark.decal_sort.map(|sort|json!({"pass":format!("{:?}",sort.pass),"format_group":sort.format_group,"bucket":sort.material_page_bucket})),
            })).collect::<Vec<_>>(),
            "captures": [],
        });
        let mut trace_camera = FlyCamera::default();
        trace_camera.ensure_spawn(&scene, spawn, content_id, None, Some(MovementMode::Fly));
        let collision = scene
            .walk_collision
            .as_ref()
            .cloned()
            .unwrap_or_else(gmpublished_domain::scene::map::MapWalkCollision::empty);
        map_record["traces"] = map.traces.iter().map(|probe| {
            let trace = trace_camera.trace_aabb(&collision, Vec3::from(probe.start), Vec3::from(probe.end), Vec3::from(probe.half_extents));
            json!({"name": probe.name, "start": probe.start, "end": probe.end, "half_extents": probe.half_extents, "fraction": trace.fraction, "end_position": trace.end_position.to_array(), "normal": trace.normal.to_array(), "start_solid": trace.start_solid})
        }).collect::<Vec<_>>().into();
        for shot in &map.cameras {
            let pose = FlyPose {
                position: Vec3::from(shot.eye),
                yaw: shot.angles[1].to_radians(),
                pitch: -shot.angles[0].to_radians(),
                speed: 1.0,
            };
            let mut camera = FlyCamera {
                capture_clip_distances: shot.clip_distances(),
                capture_horizontal_fov_degrees: shot.horizontal_fov_degrees(),
                ..FlyCamera::default()
            };
            camera.ensure_spawn(
                &scene,
                spawn,
                content_id,
                Some(pose),
                Some(MovementMode::Fly),
            );
            let animation_time = shot.animation_time.unwrap_or(suite.animation_time);
            assert!(animation_time.is_finite() && animation_time >= 0.0);
            camera.water_time = animation_time;
            camera.model_lighting_time = f64::from(animation_time);
            let submerged = scene
                .walk_collision
                .as_ref()
                .is_some_and(|collision| collision.water_at(pose.position).is_some());
            let eye_in_solid = scene
                .walk_collision
                .as_ref()
                .is_some_and(|collision| collision.aabb_trace_solid(pose.position, Vec3::ZERO));
            assert!(
                !eye_in_solid,
                "{} / {} eye is inside solid collision",
                map.name, shot.name
            );
            camera.water = if submerged {
                super::camera::WaterLevel::Eyes
            } else {
                super::camera::WaterLevel::Dry
            };
            let diagnostics = CaptureDiagnostics::default();
            let view = FixedView {
                model_lighting_timeline: shot.lighting_timeline(),
                diagnostics: diagnostics.clone(),
                live_animation: shot.live_animation,
                fullbright: shot.fullbright,
                sky_animation_time: shot.sky_animation_time,
                raster_offset: shot.raster_offset,
                door_progress_override: shot.door_progress_override,
                frame_visualization: shot.frame_visualization,
                frame_visualization_linear_output: shot.frame_visualization_linear_output,
                frame_probe: shot.frame_probe,
                target: shot.target(),
                viewer: FlyViewer {
                    #[cfg(feature = "gui-benchmark")]
                    benchmark: None,
                    scene: Arc::clone(&scene),
                    content_id,
                    fog,
                    fog_enabled: shot.fog,
                    sky_camera,
                    map_skybox_visible: shot.skybox,
                    visibility_culling: shot.visibility,
                    phy_debug_visible: shot.phy_debug,
                    spawn,
                    pose: Some(pose),
                    movement_mode: Some(MovementMode::Fly),
                    requested_movement_mode: None,
                    mouse_captured: false,
                    render_feedback: Default::default(),
                },
                camera,
            };
            let clip_distances = view
                .viewer
                .draw(
                    &view.camera,
                    mouse::Cursor::Unavailable,
                    Rectangle::with_size(size),
                )
                .uniforms
                .water_depth_params;
            let render_feedback = Arc::clone(&view.viewer.render_feedback);
            let element: iced::Element<'_, Message> = iced::widget::shader(view)
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
            let mut ui = UserInterface::build(
                element,
                size,
                user_interface::Cache::default(),
                &mut renderer,
            );
            let mut live_animation_samples: Vec<Value> = Vec::new();
            if shot.live_animation {
                assert_eq!(animation_time, 0.0, "live replay starts at time zero");
                let started = Instant::now();
                for frame in 0..=120 {
                    let now = started + std::time::Duration::from_secs_f64(f64::from(frame) / 60.0);
                    let mut messages = Vec::new();
                    let (state, _) = ui.update(
                        &[iced::Event::Window(iced::window::Event::RedrawRequested(
                            now,
                        ))],
                        mouse::Cursor::Unavailable,
                        &mut renderer,
                        &mut iced::advanced::clipboard::Null,
                        &mut messages,
                    );
                    if frame > 0 {
                        assert!(
                            messages.is_empty(),
                            "stationary animation must not publish camera messages"
                        );
                        assert!(
                            matches!(
                                state,
                                user_interface::State::Updated {
                                    redraw_request: iced::window::RedrawRequest::NextFrame,
                                    ..
                                }
                            ),
                            "live animated water must schedule the next frame"
                        );
                    }
                    ui.draw(
                        &mut renderer,
                        &theme,
                        &iced::advanced::renderer::Style {
                            text_color: Color::WHITE,
                        },
                        mouse::Cursor::Unavailable,
                    );
                    let rgba = capture_when_ready(
                        &mut renderer,
                        physical_size,
                        std::slice::from_ref(&render_feedback),
                        std::slice::from_ref(&diagnostics),
                    );
                    if frame % 60 == 0 {
                        let hash = format!("{:08x}", crc32fast::hash(&rgba));
                        if let Some(previous) = live_animation_samples.last() {
                            assert_ne!(
                                previous["pixel_crc32"], hash,
                                "water must visibly animate without input"
                            );
                        }
                        let file = format!("{}-live-{}.png", shot.name, frame / 60);
                        image::save_buffer(
                            directory.join(&file),
                            &rgba,
                            suite.width,
                            suite.height,
                            image::ColorType::Rgba8,
                        )
                        .expect("save live water frame");
                        live_animation_samples.push(json!({"animation_time": frame as f32 / 60.0, "file": file, "pixel_crc32": hash}));
                    }
                }
            }
            ui.draw(
                &mut renderer,
                &theme,
                &iced::advanced::renderer::Style {
                    text_color: Color::WHITE,
                },
                mouse::Cursor::Unavailable,
            );
            let _ = capture_when_ready(
                &mut renderer,
                physical_size,
                std::slice::from_ref(&render_feedback),
                std::slice::from_ref(&diagnostics),
            );
            let started = Instant::now();
            let rgba = renderer.screenshot(physical_size, 1.0, Color::BLACK);
            let capture_ms = started.elapsed().as_secs_f64() * 1000.0;
            let repeat = renderer.screenshot(physical_size, 1.0, Color::BLACK);
            assert!(
                rgba == repeat,
                "{} / {} is not deterministic",
                map.name,
                shot.name
            );
            let mut frame_times = Vec::with_capacity(suite.benchmark_frames);
            for _ in 0..suite.benchmark_frames {
                let frame_started = Instant::now();
                let _ = renderer.screenshot(physical_size, 1.0, Color::BLACK);
                frame_times.push(frame_started.elapsed().as_secs_f64() * 1000.0);
            }
            frame_times.sort_by(f64::total_cmp);
            let capture_latency = (!frame_times.is_empty()).then(|| {
                json!({
                    "samples": frame_times.len(),
                    "median_ms": frame_times[frame_times.len() / 2],
                    "p95_ms": frame_times[(frame_times.len() * 95).div_ceil(100).saturating_sub(1)],
                    "min_ms": frame_times[0],
                    "max_ms": frame_times[frame_times.len() - 1],
                    "includes_gpu_completion_and_cpu_readback": true,
                })
            });
            let render_latency = benchmark_render(
                &mut renderer,
                physical_size,
                suite.benchmark_frames,
                std::slice::from_ref(&diagnostics),
            );
            let filename = format!("{}.png", shot.name);
            assert!(
                diagnostics
                    .lock()
                    .expect("capture diagnostics lock")
                    .get("upload_error")
                    .is_none(),
                "scene GPU residency preparation failed"
            );
            image::save_buffer(
                directory.join(&filename),
                &rgba,
                suite.width,
                suite.height,
                image::ColorType::Rgba8,
            )
            .expect("save reference PNG");
            let player_origin = [
                shot.eye[0],
                shot.eye[1],
                shot.eye[2] - super::camera::PLAYER_START_EYE_NUDGE,
            ];
            let capture_lighting_time = shot.model_lighting_timeline.as_ref().map_or_else(
                || {
                    if shot.live_animation {
                        2.0
                    } else {
                        f64::from(animation_time)
                    }
                },
                |timeline| timeline.capture_time,
            );
            map_record["captures"].as_array_mut().unwrap().push(json!({
                "file": filename,
                "render_diagnostics": diagnostics.lock().unwrap().clone(),
                "render_latency": render_latency,
                "door_progress_override":shot.door_progress_override,"raster_offset": shot.raster_offset.unwrap_or([0.5; 2]), "authored_fullbright": shot.fullbright,
                "fullbright": shot.fullbright || scene.shading_mode == crate::media::preview_model::ShadingMode::Fullbright,
                "phy_debug": shot.phy_debug, "sky_animation_time": shot.sky_animation_time.unwrap_or(if shot.live_animation { 2.0 } else { animation_time }), "reflection_target": shot.reflection_target,"refraction_target":shot.refraction_target,
                "frame_visualization":shot.frame_visualization,"frame_visualization_linear_output":shot.frame_visualization_linear_output,
                "frame_probe":shot.frame_probe,
                "model_lighting_timeline":shot.model_lighting_timeline,
                "model_lighting_time":capture_lighting_time,
                "model_light_styles":scene.model_lighting_inputs.as_ref().map(|inputs|inputs.style_values(capture_lighting_time).to_vec()),
                "animation_time": if shot.live_animation { 2.0 } else { animation_time },
                "live_animation": shot.live_animation.then(|| json!({
                    "redraw_events_without_input": 121,
                    "rendered_frames":121,
                    "all_followup_frames_requested": true,
                    "unchanged_camera_messages_published": 0,
                    "samples": live_animation_samples,
                })),
                "eye": shot.eye,
                "source_angles_degrees": [shot.angles[0], shot.angles[1], 0.0],
                "capture_horizontal_fov_degrees": shot.horizontal_fov_degrees(),
                "clip_near": clip_distances[0],
                "clip_far": clip_distances[1],
                "capture_clip_override": shot.clip_distances().map(|clip| json!({"near":clip.near,"far":clip.far,"scope":"base renderer camera; native sky/optical projections require separate observations"})),
                "source_camera_commands": format!("setpos {} {} {}; setang {} {} 0", player_origin[0], player_origin[1], player_origin[2], shot.angles[0], shot.angles[1]),
                "skybox": shot.skybox,
                "fog": shot.fog,
                "visibility_culling": shot.visibility,
                "submerged": submerged,
                "eye_in_solid_collision": eye_in_solid,
                "pixel_crc32": format!("{:08x}", crc32fast::hash(&rgba)),
                "repeat_pixels_identical": true,
                "capture_wall_time_ms_including_readback": capture_ms,
                "steady_capture_latency": capture_latency,
            }));
            println!("map capture: {}", directory.join(filename).display());
        }
        let make_isolation_view = |shot: &CaptureCamera| {
            let pose = FlyPose {
                position: Vec3::from(shot.eye),
                yaw: shot.angles[1].to_radians(),
                pitch: -shot.angles[0].to_radians(),
                speed: 1.0,
            };
            let mut camera = FlyCamera {
                capture_clip_distances: shot.clip_distances(),
                capture_horizontal_fov_degrees: shot.horizontal_fov_degrees(),
                ..FlyCamera::default()
            };
            camera.ensure_spawn(
                &scene,
                spawn,
                content_id,
                Some(pose),
                Some(MovementMode::Fly),
            );
            camera.water_time = shot.animation_time.unwrap_or(suite.animation_time);
            camera.model_lighting_time = f64::from(camera.water_time);
            camera.water = if scene
                .walk_collision
                .as_ref()
                .is_some_and(|collision| collision.water_at(pose.position).is_some())
            {
                super::camera::WaterLevel::Eyes
            } else {
                super::camera::WaterLevel::Dry
            };
            FixedView {
                model_lighting_timeline: shot.lighting_timeline(),
                diagnostics: CaptureDiagnostics::default(),
                camera,
                live_animation: false,
                fullbright: shot.fullbright,
                sky_animation_time: shot.sky_animation_time,
                raster_offset: shot.raster_offset,
                door_progress_override: shot.door_progress_override,
                frame_visualization: shot.frame_visualization,
                frame_visualization_linear_output: shot.frame_visualization_linear_output,
                frame_probe: shot.frame_probe,
                target: CaptureTarget::Main,
                viewer: FlyViewer {
                    #[cfg(feature = "gui-benchmark")]
                    benchmark: None,
                    scene: Arc::clone(&scene),
                    content_id,
                    fog,
                    fog_enabled: shot.fog,
                    sky_camera,
                    map_skybox_visible: shot.skybox,
                    visibility_culling: shot.visibility,
                    phy_debug_visible: shot.phy_debug,
                    spawn,
                    pose: Some(pose),
                    movement_mode: Some(MovementMode::Fly),
                    requested_movement_mode: None,
                    mouse_captured: false,
                    render_feedback: Default::default(),
                },
            }
        };
        if suite.verify_view_isolation {
            map_record["view_isolation"] = isolation::verify(
                &mut renderer,
                make_isolation_view,
                &map.cameras,
                &directory,
                suite.benchmark_frames,
            );
        }
        if map.verify_temporal_view_isolation {
            map_record["temporal_view_isolation"] = isolation::verify_temporal(
                &mut renderer,
                make_isolation_view,
                &map.cameras,
                &directory,
            );
        }
        if map.verify_door_view_isolation {
            map_record["door_view_isolation"] = isolation::verify_doors(
                &mut renderer,
                make_isolation_view,
                &map.cameras,
                &directory,
            );
        }
        manifest["maps"].as_array_mut().unwrap().push(map_record);
        write_manifest(&output, &manifest);
    }
    manifest["status"] = json!("complete");
    manifest["process_peak_rss_kib"] = json!(
        fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status.lines().find_map(|line| {
                    line.strip_prefix("VmHWM:")
                        .and_then(|value| value.split_whitespace().next()?.parse::<u64>().ok())
                })
            })
    );
    write_manifest(&output, &manifest);
    println!("map capture: complete: {}", output.display());
}

fn write_manifest(output: &std::path::Path, manifest: &Value) {
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(manifest).expect("serialize capture manifest"),
    )
    .expect("write capture manifest");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_render_benchmark_needs_neither_gpu_nor_valid_target() {
        let mut renderer = futures::executor::block_on(iced::Renderer::new(
            iced::Font::default(),
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        assert!(benchmark_render(&mut renderer, Size::new(u32::MAX, 0), 0, &[]).is_none());
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                benchmark_render(&mut renderer, Size::new(1280, 720), 1, &[])
            }))
            .is_err()
        );
    }

    fn capture_map(archive: Option<&std::path::Path>) -> CaptureMap {
        serde_json::from_value(json!({"name":"control", "archive":archive, "cameras":[]})).unwrap()
    }

    #[test]
    fn primary_gma_capture_reads_selected_bsp_and_preserves_bundled_materials() {
        use crate::test_support::{GmaFixtureBuilder, write_gma_fixture};
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("primary.gma");
        let fixture = GmaFixtureBuilder::new("Primary capture source")
            .entry("maps/control.bsp", b"primary BSP".to_vec())
            .entry("materials/control.vmt", b"primary material".to_vec())
            .build();
        write_gma_fixture(&archive, &fixture);
        fs::write(directory.path().join("control.bsp"), b"different loose BSP").unwrap();
        let input = capture_map(Some(&archive))
            .read_source(directory.path())
            .unwrap();
        assert_eq!(input.bytes, b"primary BSP");
        assert_eq!(input.entry_path, "maps/control.bsp");
        assert_eq!(input.archive_path.as_ref(), Some(&archive));
        assert!(input.bsp_path.is_none());
        assert!(matches!(
            input.source.as_ref(),
            PreviewArchiveSource::Gma(_)
        ));
        assert_eq!(
            input.source.entry_bytes("materials/control.vmt").unwrap(),
            b"primary material"
        );
    }

    #[test]
    fn capture_archive_failure_never_falls_back_to_existing_loose_map() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("control.bsp"), b"loose BSP").unwrap();
        let missing = directory.path().join("missing.gma");
        assert!(
            capture_map(Some(&missing))
                .read_source(directory.path())
                .is_err()
        );
        let invalid = directory.path().join("invalid.gma");
        fs::write(&invalid, b"not an archive").unwrap();
        assert!(
            capture_map(Some(&invalid))
                .read_source(directory.path())
                .is_err()
        );
        let wrong_extension = directory.path().join("archive.txt");
        fs::write(&wrong_extension, b"not an archive").unwrap();
        assert!(
            capture_map(Some(&wrong_extension))
                .validate_source(directory.path())
                .unwrap_err()
                .contains("must be a GMA")
        );
        assert!(
            serde_json::from_value::<CaptureMap>(
                json!({"name":"control", "archive":false, "cameras":[]})
            )
            .is_err()
        );
    }

    #[test]
    fn absent_capture_archive_preserves_folder_source_default() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.bsp");
        fs::write(&path, b"loose BSP").unwrap();
        let map: CaptureMap =
            serde_json::from_value(json!({"name":"control", "cameras":[]})).unwrap();
        assert!(map.archive.is_none());
        let input = map.read_source(directory.path()).unwrap();
        assert_eq!(input.bytes, b"loose BSP");
        assert_eq!(input.bsp_path.as_ref(), Some(&path));
        assert!(input.archive_path.is_none());
        assert!(matches!(
            input.source.as_ref(),
            PreviewArchiveSource::Folder(_)
        ));
    }

    #[test]
    fn on_demand_texture_diagnostics_report_recipe_storage_without_inventing_payload_hash() {
        use crate::bridge::{gma::PreviewArchive, materials::MaterialResolver};
        use crate::test_support::{GmaFixtureBuilder, fixture_vtf_bytes};
        let bc = [0_u8; 8];
        let archive = PreviewArchive::from_gma(
            GmaFixtureBuilder::new("BC diagnostics")
                .entry(
                    "materials/control.vmt",
                    br#"LightmappedGeneric { "$basetexture" "base" }"#.to_vec(),
                )
                .entry(
                    "materials/base.vtf",
                    fixture_vtf_bytes(4, 4, vformats::vtf::VtfFormat::Dxt1, &[&bc]),
                )
                .build(),
        )
        .unwrap();
        let material = MaterialResolver::new(Arc::new(archive), None)
            .with_on_demand_textures()
            .resolve_primary(&[], "control")
            .into_material()
            .unwrap();
        let texture = material
            .texture
            .as_ref()
            .expect("capture fixture has base texture");
        assert!(texture.resident().is_none());
        let info = texture_info(texture);
        assert_eq!(info["state"], "on_demand");
        assert_eq!(info["format"], "Bc1");
        assert_eq!(info["mip_levels"], 1);
        assert_eq!(info["recipe"]["storage"], "Bc1");
        assert!(info["payload_crc32"].is_null());
        assert!(info["resident"].is_null());
        assert_eq!(info["source_metadata"]["dimensions"], json!([4, 4]));
    }

    #[test]
    fn on_demand_water_diagnostics_distinguish_unmaterialized_frames_from_empty_texture() {
        use crate::bridge::{gma::PreviewArchive, materials::MaterialResolver};
        use crate::test_support::{GmaFixtureBuilder, fixture_vtf_bytes};
        let rgba = [128, 128, 255, 255].repeat(2);
        let mut bytes = fixture_vtf_bytes(1, 1, vformats::vtf::VtfFormat::Rgba8888, &[&rgba]);
        bytes[24..26].copy_from_slice(&2_u16.to_le_bytes());
        let archive = PreviewArchive::from_gma(
            GmaFixtureBuilder::new("Water diagnostics")
                .entry(
                    "materials/control.vmt",
                    br#"Water { "$normalmap" "normal" }"#.to_vec(),
                )
                .entry("materials/normal.vtf", bytes)
                .build(),
        )
        .unwrap();
        let resolved = MaterialResolver::new(Arc::new(archive), None)
            .with_on_demand_textures()
            .resolve_primary(&[], "control");
        let resolution = resolved.outcome();
        let material = resolved.into_material().unwrap();
        let slot = MaterialSlot {
            resolution,
            canonical_name: Some("control".into()),
            name: "control".into(),
            shader: material.shader,
            texture: material.texture,
            texture2: None,
            render_mode: material.render_mode,
            alpha_policy: material.render_mode.texture_alpha_policy(),
        };
        let info = material_info(&slot);
        assert!(info["water"]["normal_frames_linear_rgba"].is_null());
        assert_eq!(info["water"]["normal_state"], "on_demand");
        assert_eq!(info["water"]["normal_source_metadata"]["frames"], 2);
        assert_eq!(info["water"]["normal_page_recipe"]["mip_count"], 1);
    }

    fn camera(extra: &Value) -> CaptureCamera {
        let mut value = json!({"name":"projection","eye":[0,0,0],"angles":[0,0]});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn capture_horizontal_fov_is_optional_and_validated() {
        assert_eq!(camera(&json!({})).horizontal_fov_degrees(), None);
        assert_eq!(
            camera(&json!({"horizontal_fov_degrees":72.73435974121094})).horizontal_fov_degrees(),
            Some(72.734_36)
        );
        for invalid in [0.0, -1.0, 180.0, f32::NAN, f32::INFINITY] {
            let mut camera = camera(&json!({}));
            camera.horizontal_fov_degrees = Some(invalid);
            assert!(std::panic::catch_unwind(|| camera.horizontal_fov_degrees()).is_err());
        }
    }

    #[test]
    fn explicit_capture_pair_survives_camera_decoding() {
        let camera = camera(&json!({"clip_near":4,"clip_far":32768}));
        let clip = camera.clip_distances().unwrap();
        assert_eq!((clip.near, clip.far), (4.0, 32768.0));
        assert_eq!(self::camera(&json!({})).clip_distances(), None);
    }

    #[test]
    #[should_panic(expected = "both clip_near and clip_far")]
    fn partial_capture_pair_is_rejected() {
        let _ = camera(&json!({"clip_near":4})).clip_distances();
    }

    #[test]
    #[should_panic(expected = "finite 0 < clip_near < clip_far")]
    fn unordered_capture_pair_is_rejected() {
        let _ = camera(&json!({"clip_near":4,"clip_far":2})).clip_distances();
    }
}

#[cfg(test)]
mod texture_readiness_tests {
    use super::texture_readiness;
    use crate::features::file_preview::viewer3d::feedback::RenderStatus;

    #[test]
    fn every_simultaneous_view_must_be_ready_including_policy_placeholders() {
        let ready = RenderStatus::Ready {
            unavailable_images: 3,
        };
        assert_eq!(
            texture_readiness(&[ready.clone(), RenderStatus::Pending]),
            Ok(false)
        );
        assert_eq!(
            texture_readiness(&[
                ready.clone(),
                RenderStatus::Retiring {
                    unavailable_images: 0
                }
            ]),
            Ok(false)
        );
        assert_eq!(texture_readiness(&[ready.clone(), ready]), Ok(true));
        assert!(texture_readiness(&[]).is_err());
    }

    #[test]
    fn terminal_preparation_errors_cannot_become_matching_blank_captures() {
        assert_eq!(
            texture_readiness(&[
                RenderStatus::Ready {
                    unavailable_images: 0
                },
                RenderStatus::Rejected {
                    reason: "intrinsic source failure".into()
                }
            ]),
            Err("intrinsic source failure".into())
        );
        assert_eq!(
            texture_readiness(&[RenderStatus::RetirementTimedOut]),
            Err("GPU resource retirement timed out".into())
        );
    }
}
