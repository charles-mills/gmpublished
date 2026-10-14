use super::{FlyCamera, FlyPose, FlyViewer, Message, ModelPrimitive, MovementMode, Vec3, shader};
use crate::bridge::archive::PreviewArchiveSource;
use crate::generation::Generation;
use crate::media::file_preview_decode::load_map_for_capture;
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
    maps: Vec<CaptureMap>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureMap {
    name: String,
    cameras: Vec<CaptureCamera>,
    #[serde(default)]
    traces: Vec<CaptureTrace>,
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
    /// Replay the production widget update path for two seconds without input.
    #[serde(default)]
    live_animation: bool,
    #[serde(default = "enabled")]
    skybox: bool,
    #[serde(default = "enabled")]
    fog: bool,
    #[serde(default = "enabled")]
    visibility: bool,
}

const fn enabled() -> bool {
    true
}

struct FixedView {
    viewer: FlyViewer,
    camera: FlyCamera,
    live_animation: bool,
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
        self.viewer.draw(
            if self.live_animation {
                state
            } else {
                &self.camera
            },
            cursor,
            bounds,
        )
    }
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
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
    })
}

fn material_info(slot: &MaterialSlot) -> Value {
    let water =
        slot.shader.water().map(|water| {
            let frames =
                water.normal_map.as_ref().map(|normal| {
                    normal
                        .frames
                        .iter()
                        .map(|mips| {
                            mips.iter().map(|mip| json!({
                "dimensions": [mip.width, mip.height],
                "payload_crc32": format!("{:08x}", crc32fast::hash(&mip.rgba)),
                "bytes": mip.rgba.len(),
            })).collect::<Vec<_>>()
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
                "reflect_enabled": water.reflect_enabled, "refract_enabled": water.refract_enabled,
                "reflect_amount": water.reflect_amount, "refract_amount": water.refract_amount,
                "reflect_tint_linear": water.reflect_tint_linear,
                "refract_tint_linear": water.refract_tint_linear,
                "fog_color_linear": water.fog_color_linear,
                "fog_start": water.fog_start, "fog_end": water.fog_end,
                "fog_enabled": water.fog_enabled,
                "below_material": water.below_material,
                "unsupported_proxies": water.unsupported_proxies,
                "unsupported_features": water.unsupported_features,
            })
        });
    json!({
        "name": slot.name,
        "shader": match &slot.shader {
            crate::bridge::materials::MaterialShader::Unknown => "unknown",
            crate::bridge::materials::MaterialShader::Surface(name) => name,
            crate::bridge::materials::MaterialShader::Water(material) => &material.shader,
        },
        "water": water,
        "render_mode": format!("{:?}", slot.render_mode),
        "force_opaque": slot.force_opaque,
        "texture": slot.texture.as_deref().map(texture_info),
        "texture2": slot.texture2.as_deref().map(texture_info),
    })
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
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
        if !relative
            .extension()
            .is_some_and(|ext| matches!(ext.to_str(), Some("rs" | "json" | "py")))
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
        "scale_factor": 1.0,
        "vertical_fov_degrees": super::FOV_Y.to_degrees(),
        "animation_time_seconds": suite.animation_time,
        "benchmark_frames_per_view": suite.benchmark_frames,
        "backend": "wgpu headless using the production map pipeline",
        "adapter_and_shader_diagnostics": "renderer.log",
        "test_build": cfg!(test),
        "wgpu_backend_override": std::env::var("WGPU_BACKEND").ok(),
        "game_reference_captures": false,
        "maps": [],
    });
    fs::write(
        output.join("source.patch"),
        git_output(&["diff", "--no-ext-diff", "HEAD"]).unwrap_or_default(),
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
        let preview =
            load_map_for_capture(&request, game.clone(), &pool).expect("load map preview");
        let load_ms = load_started.elapsed().as_secs_f64() * 1000.0;
        let content_id = preview.content_id();
        let PreviewContent::Map {
            scene,
            stats,
            fog,
            sky_camera,
            spawn,
        } = preview.content
        else {
            panic!("{} did not decode as a map", map.name);
        };
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
            "bsp_path": map_path,
            "bsp_bytes": bytes.len(),
            "bsp_crc32": format!("{:08x}", request.crc32),
            "load_wall_time_ms": load_ms,
            "stats": format!("{stats:?}"),
            "sky_camera": format!("{sky_camera:?}"),
            "fog": format!("{fog:?}"),
            "world_meshes": scene.scene.meshes.len(),
            "skybox_meshes": scene.map_skybox_meshes.len(),
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
            // Fly movement does not sample immersion until it moves. Fixed poses
            // need the same eye-in-volume state without advancing simulation.
            camera.water = if submerged {
                super::camera::WaterLevel::Eyes
            } else {
                super::camera::WaterLevel::Dry
            };
            let view = FixedView {
                live_animation: shot.live_animation,
                viewer: FlyViewer {
                    scene: Arc::clone(&scene),
                    content_id,
                    fog,
                    fog_enabled: shot.fog,
                    sky_camera,
                    map_skybox_visible: shot.skybox,
                    visibility_culling: shot.visibility,
                    phy_debug_visible: false,
                    spawn,
                    pose: Some(pose),
                    movement_mode: Some(MovementMode::Fly),
                    requested_movement_mode: None,
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
                    if frame % 60 == 0 {
                        ui.draw(
                            &mut renderer,
                            &theme,
                            &iced::advanced::renderer::Style {
                                text_color: Color::WHITE,
                            },
                            mouse::Cursor::Unavailable,
                        );
                        let rgba = renderer.screenshot(physical_size, 1.0, Color::BLACK);
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
            // Warm upload and render once, then verify byte-for-byte stability
            // at the frozen pose and animation time before accepting the PNG.
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
            map_record["captures"].as_array_mut().unwrap().push(json!({
                "file": filename,
                "animation_time": if shot.live_animation { 2.0 } else { animation_time },
                "live_animation": shot.live_animation.then(|| json!({
                    "redraw_events_without_input": 121,
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
