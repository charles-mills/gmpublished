#[path = "capture_isolation.rs"]
mod isolation;
use super::{FlyCamera, FlyPose, FlyViewer, Message, ModelPrimitive, MovementMode, Vec3, shader};
use crate::bridge::archive::PreviewArchiveSource;
use crate::generation::Generation;
use crate::media::file_preview_decode::load_map_for_capture_with_model_options;
use crate::media::preview_model::{MaterialSlot, PreviewContent, PreviewRequest};
use iced::advanced::renderer::Headless as _;
use iced::{Color, Length, Rectangle, Size, mouse};
use iced_runtime::{UserInterface, user_interface};
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
    maps: Vec<CaptureMap>,
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
    /// Replay the production widget update path for two seconds without input.
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
    diagnostics: Arc<Mutex<Value>>,
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
        primitive.reflection_target = self.target == CaptureTarget::Reflection;
        primitive.refraction_target = self.target == CaptureTarget::Refraction;
        primitive.capture_diagnostics = Some(Arc::clone(&self.diagnostics));
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

fn texture_info(texture: &crate::bridge::materials::ResolvedTexture) -> Value {
    let mut hash = crc32fast::Hasher::new();
    let format = if let Some((format, mips)) = texture.bc_payload() {
        for mip in mips {
            hash.update(&mip.data);
        }
        format!("{format:?}")
    } else {
        for mip in texture.mip_chain() {
            hash.update(mip.rgba);
        }
        "RGBA8".to_owned()
    };
    json!({
        "dimensions": [texture.width, texture.height],
        "original_dimensions": texture.original_dimensions(),
        "mip_levels": texture.bc_payload().map_or_else(|| texture.mip_chain().count(), |(_, mips)| mips.len()),
        "format": format,
        "payload_crc32": format!("{:08x}", hash.finalize()),
        "water_fallback": texture.is_water_fallback(),
        "vtf_flags": texture.flags,
    })
}

fn material_info(slot: &MaterialSlot) -> Value {
    let water = slot.shader.water().map(|water| {
        let frames = water.normal_map.as_ref().map(|normal| {
            normal
                .frames
                .iter()
                .map(|mips| {
                    mips.iter()
                        .map(|mip| {
                            json!({
                                "dimensions": [mip.width, mip.height],
                                "payload_crc32": format!("{:08x}", crc32fast::hash(&mip.rgba)),
                                "bytes": mip.rgba.len(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        });
        json!({
            "shader": water.shader,
            "parameters": water.parameters,
            "normal_map_path": water.normal_map_path,
            "normal_frames_linear_rgba": frames,
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
        "shader": slot.shader.shader_name(),
        "water": water,
        "surface": slot.shader.surface().map(|surface|json!({
            "family":format!("{:?}",surface.family),
            "parameters":surface.parameters,
            "color":surface.color,"color2":surface.color2,"alpha":surface.alpha,
            "base_transform":surface.base_transform,"base2_transform":surface.base2_transform,
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
                "frames": paint.texture.as_ref().map(|texture| texture.frames.iter().map(|mips|mips.iter().map(|mip|json!({
                    "dimensions": [mip.width,mip.height],
                    "rgba_crc32": format!("{:08x}",crc32fast::hash(&mip.rgba)),
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
    let suite_path = std::env::var_os("GMPUBLISHED_MAP_CAPTURE_SUITE").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/map-capture/cameras.json"),
        PathBuf::from,
    );
    let suite_bytes = fs::read(&suite_path).expect("read camera suite");
    let suite: CaptureSuite = serde_json::from_slice(&suite_bytes).expect("parse camera suite");
    assert!((1..=4096).contains(&suite.width) && (1..=4096).contains(&suite.height));
    assert!(suite.animation_time.is_finite() && suite.animation_time >= 0.0);
    assert!(suite.benchmark_frames <= 1000);
    assert!(!suite.maps.is_empty());
    let mut names = std::collections::HashSet::new();
    for map in &suite.maps {
        assert!(valid_name(&map.name) && names.insert(map.name.clone()));
        assert!(!map.cameras.is_empty());
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
        "maps_directory": maps_directory,
        "viewport": [suite.width, suite.height],
        "scene_msaa_samples": super::scene_sample_count(),
        "optical_msaa_samples": 1,
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
        let map_path = maps_directory.join(format!("{}.bsp", map.name));
        let bytes = fs::read(&map_path).expect("read reference BSP");
        let request = PreviewRequest {
            request_id: Generation::INITIAL.next(),
            archive: PreviewArchiveSource::from_folder([(
                format!("maps/{}.bsp", map.name),
                bytes.len() as u64,
                map_path.clone(),
            )]),
            entry_path: format!("maps/{}.bsp", map.name),
            display_name: format!("{}.bsp", map.name),
            size_bytes: bytes.len() as u64,
            crc32: crc32fast::hash(&bytes),
            bypass_size_limits: true,
        };
        let load_started = Instant::now();
        let preview = load_map_for_capture_with_model_options(
            &request,
            game.clone(),
            &pool,
            suite.model_vertex_compression.options(),
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
        let mut map_record = json!({
            "name": map.name,
            "shader_lighting":shader_lighting,
            "native_bone_pose_control":map.bone_pose_control,
            "bsp_path": map_path,
            "bsp_bytes": bytes.len(),
            "bsp_crc32": format!("{:08x}", request.crc32),
            "load_wall_time_ms": load_ms,
            "stats": format!("{stats:?}"),
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
                "runtime_vertices":scene.scene.meshes.iter().chain(&scene.map_skybox_meshes)
                    .flat_map(|mesh|&mesh.vertices).filter(|vertex|vertex.lighting_context as usize==index+1 && vertex.lighting_mode & crate::media::preview_model::vertex_lighting::RUNTIME_CONTEXT != 0).count(),
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
            "baked_lighting": scene.lightmap.as_ref().map(|atlas| serde_json::json!({"source":format!("{:?}",atlas.source),"dimensions":[atlas.width,atlas.height],"animated":atlas.is_animated(),"active_styles":atlas.active_styles(),"initial_weights":atlas.active_styles().into_iter().map(|index|(index.to_string(),atlas.style_values(0.0)[usize::from(index)])).collect::<std::collections::BTreeMap<_,_>>()})),
            "lightmap_crc32": scene.lightmap.as_ref().map(|atlas| format!("{:08x}", crc32fast::hash(&atlas.rgba))),
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
            let mut camera = FlyCamera::default();
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
            // Fixed fly poses need immersion before their first movement.
            camera.water = if submerged {
                super::camera::WaterLevel::Eyes
            } else {
                super::camera::WaterLevel::Dry
            };
            let diagnostics = Arc::new(Mutex::new(Value::Null));
            let view = FixedView {
                model_lighting_timeline: shot.lighting_timeline(),
                diagnostics: Arc::clone(&diagnostics),
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
                    let rgba = renderer.screenshot(physical_size, 1.0, Color::BLACK);
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
            let _ = renderer.screenshot(physical_size, 1.0, Color::BLACK);
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
            let filename = format!("{}.png", shot.name);
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
                "door_progress_override":shot.door_progress_override,"raster_offset": shot.raster_offset.unwrap_or([0.5; 2]), "fullbright": shot.fullbright, "phy_debug": shot.phy_debug, "sky_animation_time": shot.sky_animation_time.unwrap_or(if shot.live_animation { 2.0 } else { animation_time }), "reflection_target": shot.reflection_target,"refraction_target":shot.refraction_target,
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
                "clip_near": clip_distances[0],
                "clip_far": clip_distances[1],
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
            let mut camera = FlyCamera::default();
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
                diagnostics: Arc::new(Mutex::new(Value::Null)),
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
                },
            }
        };
        if suite.verify_view_isolation {
            map_record["view_isolation"] =
                isolation::verify(&mut renderer, make_isolation_view, &map.cameras, &directory);
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
