use super::{App, RootMessage, preview_gma};
use crate::features::file_preview;
use gmpublished_domain::math::Vec3;
use iced::{Element, Event, Length, Subscription, Task, event, window};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error)]
#[error("GUI benchmark: {0}")]
pub struct Error(String);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent benchmark controls"
)]
pub struct Config {
    pub width: u32,
    pub height: u32,
    pub output: PathBuf,
    pub cases: Vec<Case>,
    #[serde(default = "timeout")]
    pub load_timeout_seconds: f64,
    #[serde(default = "frame_limit")]
    pub max_frames: usize,
    #[serde(default)]
    pub allow_debug_build: bool,
    #[serde(default = "enabled")]
    pub profile_renderer: bool,
    #[serde(default)]
    pub smoke: bool,
    #[serde(default = "enabled")]
    pub screenshots: bool,
    #[serde(default = "enabled")]
    pub expanded: bool,
}

fn enabled() -> bool {
    true
}
fn timeout() -> f64 {
    120.0
}
fn frame_limit() -> usize {
    65536
}
fn measurement() -> f64 {
    10.0
}
fn warmup() -> f64 {
    2.0
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub name: String,
    pub archive: PathBuf,
    pub entry: String,
    pub camera: Pose,
    #[serde(default)]
    pub second_camera: Option<Pose>,
    #[serde(default = "measurement")]
    pub seconds: f64,
    #[serde(default = "warmup")]
    pub warmup_seconds: f64,
    #[serde(default)]
    pub fixed_scene_clock: Option<f64>,
    #[serde(default)]
    pub continuous_redraw: bool,
    #[serde(default)]
    pub out_of_bounds: bool,
    #[serde(default)]
    pub allow_camera_input: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pose {
    pub eye: [f32; 3],
    pub angles: [f32; 2],
    #[serde(default)]
    pub velocity: [f32; 3],
    #[serde(default)]
    pub yaw_degrees_per_second: f32,
}

impl Config {
    pub fn from_args() -> Result<Option<Self>, Error> {
        let args: Vec<_> = std::env::args_os().collect();
        if args.get(1).is_none_or(|arg| arg != "--benchmark-gui") {
            return Ok(None);
        }
        if args.len() != 3 {
            return Err(Error(
                "expected --benchmark-gui /absolute/suite.json".into(),
            ));
        }
        let path = PathBuf::from(&args[2]);
        let bytes =
            fs::read(&path).map_err(|error| Error(format!("{}: {error}", path.display())))?;
        let mut config: Self =
            serde_json::from_slice(&bytes).map_err(|error| Error(error.to_string()))?;
        if !config.smoke {
            for case in &mut config.cases {
                case.warmup_seconds = case.warmup_seconds.max(2.0);
                case.seconds = case
                    .seconds
                    .max(if case.out_of_bounds { 120.0 } else { 10.0 });
            }
        }
        config.validate()?;
        if cfg!(debug_assertions) && !config.allow_debug_build {
            return Err(Error(
                "use an optimized build; allow_debug_build is only for harness smoke tests".into(),
            ));
        }
        fs::create_dir(&config.output)
            .map_err(|error| Error(format!("fresh output {}: {error}", config.output.display())))?;
        fs::write(config.output.join("suite.json"), bytes)
            .map_err(|error| Error(error.to_string()))?;
        Ok(Some(config))
    }

    fn validate(&self) -> Result<(), Error> {
        let valid = self.width >= 640
            && self.height >= 480
            && self.width <= 16384
            && self.height <= 16384
            && self.output.is_absolute()
            && !self.cases.is_empty()
            && self.cases.len() <= 256
            && self.load_timeout_seconds.is_finite()
            && (1.0..=3600.0).contains(&self.load_timeout_seconds)
            && (1..=1_000_000).contains(&self.max_frames);
        if !valid {
            return Err(Error(
                "invalid window, output, case count, timeout or frame limit".into(),
            ));
        }
        let mut names = std::collections::HashSet::new();
        for case in &self.cases {
            if case.name.is_empty()
                || !names.insert(&case.name)
                || !case.archive.is_absolute()
                || !case.archive.is_file()
                || !case.entry.starts_with("maps/")
                || !PathBuf::from(&case.entry)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("bsp"))
                || case.entry.contains("..")
                || !case.seconds.is_finite()
                || !(0.1..=3600.0).contains(&case.seconds)
                || !case.warmup_seconds.is_finite()
                || !(0.0..=300.0).contains(&case.warmup_seconds)
                || case.fixed_scene_clock.is_some_and(|clock| {
                    !clock.is_finite() || !(clock as f32).is_finite() || clock < 0.0
                })
                || !case.camera.valid()
                || case
                    .second_camera
                    .as_ref()
                    .is_some_and(|pose| !pose.valid())
            {
                return Err(Error(format!("invalid benchmark case {}", case.name)));
            }
        }
        Ok(())
    }
}

impl Pose {
    fn valid(&self) -> bool {
        self.eye
            .iter()
            .chain(self.angles.iter())
            .chain(self.velocity.iter())
            .all(|v| v.is_finite())
            && self.yaw_degrees_per_second.is_finite()
            && self.angles[0].abs() < 90.0
    }

    pub fn at(&self, seconds: f32) -> ([f32; 3], [f32; 2]) {
        (
            std::array::from_fn(|i| self.eye[i] + self.velocity[i] * seconds),
            [
                self.angles[0],
                self.angles[1] + self.yaw_degrees_per_second * seconds,
            ],
        )
    }
}

#[derive(Clone, Debug)]
pub struct ViewControl {
    pub pose: Pose,
    pub measurement_started: Arc<std::sync::OnceLock<Instant>>,
    pub fixed_scene_clock: Option<f64>,
    pub continuous_redraw: bool,
    allow_camera_input: bool,
    seeded: Arc<std::sync::atomic::AtomicBool>,
    pub observed: Arc<Mutex<ViewObservation>>,
    pub diagnostics: Option<file_preview::CaptureDiagnostics>,
}

#[derive(Debug, Default, Serialize)]
pub struct ViewObservation {
    pub logical_bounds: [f32; 4],
    pub eye: [f32; 3],
    pub scene_clock: f64,
    pub draws: u64,
    pub steady_draws: u64,
    pub steady_first_elapsed_seconds: Option<f64>,
    pub steady_last_elapsed_seconds: Option<f64>,
    pub requested_eye: Option<[f32; 3]>,
    pub pose_mismatch_draws: u64,
    pub non_fly_draws: u64,
    pub source_angles_degrees: [f32; 2],
}

impl ViewControl {
    pub(crate) fn new(pose: Pose, case: &Case) -> Self {
        Self {
            pose,
            measurement_started: Arc::default(),
            fixed_scene_clock: case.fixed_scene_clock,
            continuous_redraw: case.continuous_redraw,
            allow_camera_input: case.allow_camera_input,
            seeded: Arc::default(),
            observed: Arc::default(),
            diagnostics: None,
        }
    }

    pub fn camera(&self, now: Instant) -> Option<([f32; 3], [f32; 2])> {
        if self.allow_camera_input && self.seeded.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let elapsed = self.measurement_started.get().map_or(0.0, |started| {
            now.saturating_duration_since(*started).as_secs_f32()
        });
        let pose = self.pose.at(elapsed);
        if let Ok(mut observed) = self.observed.lock() {
            observed.requested_eye = Some(pose.0);
        }
        Some(pose)
    }

    pub fn record(
        &self,
        bounds: iced::Rectangle,
        eye: Vec3,
        angles: [f32; 2],
        fly_mode: bool,
        clock: f64,
    ) {
        if let Ok(mut observed) = self.observed.lock() {
            observed.logical_bounds = [bounds.x, bounds.y, bounds.width, bounds.height];
            observed.eye = eye.to_array();
            observed.source_angles_degrees = angles;
            if !self.allow_camera_input && observed.requested_eye != Some(observed.eye) {
                observed.pose_mismatch_draws += 1;
            }
            if !fly_mode {
                observed.non_fly_draws += 1;
            }
            observed.scene_clock = clock;
            observed.draws += 1;
            if let Some(started) = self.measurement_started.get() {
                let elapsed = started.elapsed().as_secs_f64();
                observed.steady_draws += 1;
                observed.steady_first_elapsed_seconds.get_or_insert(elapsed);
                observed.steady_last_elapsed_seconds = Some(elapsed);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Tick(Instant),
    Input {
        issued: Instant,
        sequence: Option<u64>,
        kind: &'static str,
        captured: bool,
    },
    Secondary(file_preview::Message),
    Screenshot {
        case_index: usize,
        screenshot: window::Screenshot,
    },
}

fn input_message(kind: &'static str, status: event::Status) -> Message {
    let issued = Instant::now();
    Message::Input {
        issued,
        sequence: iced_wgpu::profiling::mark_input(issued),
        kind,
        captured: status == event::Status::Captured,
    }
}

fn observed_input(event: &Event, status: event::Status) -> Option<Message> {
    match event {
        Event::Keyboard(_) => Some(input_message("keyboard", status)),
        Event::Mouse(_) => Some(input_message("mouse", status)),
        _ => None,
    }
}

pub fn subscription() -> Subscription<RootMessage> {
    Subscription::batch([
        event::listen_raw(|event, status, _id| {
            observed_input(&event, status).map(RootMessage::GuiBenchmark)
        }),
        iced::time::every(Duration::from_millis(100))
            .map(|now| RootMessage::GuiBenchmark(Message::Tick(now))),
    ])
}

#[derive(Debug)]
enum Phase {
    Start,
    Loading {
        started: Instant,
        controls_installed: bool,
    },
    Warming {
        started: Instant,
        ready_ms: f64,
    },
    TakingScreenshot {
        started: Instant,
        ready_ms: f64,
        initial: Value,
    },
    Measuring {
        started: Instant,
        ready_ms: f64,
        initial: Value,
    },
    Closing {
        started: Instant,
    },
    Finished,
}

struct Observer {
    diagnostics: file_preview::CaptureDiagnostics,
    _pause: file_preview::DiagnosticsPause,
    profile: Option<file_preview::DiagnosticsProfile>,
}

impl std::fmt::Debug for Observer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Observer")
            .field("diagnostics", &self.diagnostics)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct Harness {
    config: Config,
    case_index: usize,
    phase: Phase,
    results: Vec<Value>,
    inputs: Vec<Value>,
    pub secondary: Option<file_preview::State>,
    controls: Vec<ViewControl>,
    observers: Vec<Observer>,
    started: Instant,
    executable: PathBuf,
    pub(super) scale_factor: Option<f32>,
    memory: Vec<Value>,
}

impl Harness {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            case_index: 0,
            phase: Phase::Start,
            results: Vec::new(),
            inputs: Vec::new(),
            secondary: None,
            controls: Vec::new(),
            observers: Vec::new(),
            started: Instant::now(),
            executable: std::env::current_exe().unwrap_or_default(),
            scale_factor: None,
            memory: vec![memory_sample("app-created")],
        }
    }

    fn finish_file(&self, status: &str, error: Option<&str>) {
        let report = json!({"status":status,"error":error,"suite":self.config,"cases":self.results,
            "elapsed_seconds":self.started.elapsed().as_secs_f64(),"executable":self.executable,
            "debug_assertions":cfg!(debug_assertions),"input_dispatch_samples":self.inputs,
            "window_scale_factor":self.scale_factor,"memory_samples":self.memory,
            "profiling_storage_included_in_rss":self.config.profile_renderer,
            "heartbeat_interval_ms":100,"fixed_clock_only_when_requested":true,
            "observer_scheduling":"heartbeat-and-input-only; trajectory changes publish production camera messages on redraw",
            "scope":"Real App startup, PreviewGma archive/entry loading, FilePreview UI/update, production map widgets and native window compositor. Pair mode is two benchmark production panes sharing one loaded scene; ordinary app remains single-pane.",
            "timing_contract":"Renderer samples measure actual compositor stages and frame intervals. Input samples start at Iced raw-event delivery and do not measure kernel-to-photon latency. GPU query results may be absent; no synchronous GPU wait is imposed."});
        let temp = self.config.output.join("report.tmp.json");
        if let Err(error) = serde_json::to_vec_pretty(&report)
            .map_err(std::io::Error::other)
            .and_then(|bytes| fs::write(&temp, bytes))
            .and_then(|()| fs::rename(&temp, self.config.output.join("report.json")))
        {
            log::error!("GUI benchmark report: {error}");
        }
    }

    fn progress(&self, phase: &str) {
        let progress = json!({"phase":phase,"case_index":self.case_index,
            "case":self.config.cases.get(self.case_index).map(|case|&case.name),
            "pid":std::process::id(),"elapsed_seconds":self.started.elapsed().as_secs_f64(),
            "window_scale_factor":self.scale_factor});
        let temp = self.config.output.join("progress.tmp.json");
        if let Err(error) = serde_json::to_vec(&progress)
            .map_err(std::io::Error::other)
            .and_then(|bytes| fs::write(&temp, bytes))
            .and_then(|()| fs::rename(temp, self.config.output.join("progress.json")))
        {
            log::error!("GUI benchmark progress: {error}");
        }
    }

    fn fail(&mut self, error: &str) -> Task<RootMessage> {
        let _report = self.finish_renderer();
        let _phases = self.finish_observers();
        self.phase = Phase::Finished;
        self.finish_file("failed", Some(error));
        self.progress("failed");
        iced::exit()
    }

    fn start_renderer(&self, label: String) {
        if self.config.profile_renderer {
            iced_wgpu::profiling::start(label, self.config.max_frames);
        }
    }

    fn finish_renderer(&self) -> iced_wgpu::profiling::Report {
        if self.config.profile_renderer {
            iced_wgpu::profiling::finish()
        } else {
            iced_wgpu::profiling::Report::default()
        }
    }

    fn make_control(&mut self, pose: Pose, case: &Case) -> ViewControl {
        let mut control = ViewControl::new(pose, case);
        let diagnostics = file_preview::CaptureDiagnostics::default();
        let pause = diagnostics.pause();
        let profile = self
            .config
            .profile_renderer
            .then(|| diagnostics.profile(self.config.max_frames));
        control.diagnostics = Some(diagnostics.clone());
        self.observers.push(Observer {
            diagnostics,
            _pause: pause,
            profile,
        });
        control
    }

    fn finish_observers(&mut self) -> Vec<Value> {
        self.observers
            .iter_mut()
            .filter_map(|observer| observer.profile.take())
            .map(file_preview::DiagnosticsProfile::finish)
            .collect()
    }

    fn start_measurement(&mut self, ready_ms: f64, initial: Value, now: Instant) {
        for control in &self.controls {
            let _set_once = control.measurement_started.set(now);
        }
        if self.config.profile_renderer {
            for observer in &mut self.observers {
                observer.profile = Some(observer.diagnostics.profile(self.config.max_frames));
            }
        }
        let name = self.config.cases[self.case_index].name.clone();
        self.memory
            .push(memory_sample(&format!("{name}:steady-start")));
        self.progress("measuring");
        self.start_renderer(name);
        self.phase = Phase::Measuring {
            started: now,
            ready_ms,
            initial,
        };
    }

    fn screenshot(
        &mut self,
        case_index: usize,
        screenshot: &window::Screenshot,
    ) -> Task<RootMessage> {
        if case_index != self.case_index || !matches!(self.phase, Phase::TakingScreenshot { .. }) {
            return self.fail("unexpected window screenshot delivery");
        }
        let path = self.config.output.join(format!("case-{case_index:03}.png"));
        if let Err(error) = image::save_buffer(
            &path,
            &screenshot.rgba,
            screenshot.size.width,
            screenshot.size.height,
            image::ColorType::Rgba8,
        ) {
            return self.fail(&format!("window screenshot: {error}"));
        }
        self.scale_factor = Some(screenshot.scale_factor);
        let phase = std::mem::replace(&mut self.phase, Phase::Start);
        let Phase::TakingScreenshot {
            ready_ms,
            mut initial,
            ..
        } = phase
        else {
            return Task::none();
        };
        initial["window_screenshot"] = json!({"file":path,"physical_size":[screenshot.size.width,screenshot.size.height],
            "scale_factor":screenshot.scale_factor,"rgba_crc32":crc32fast::hash(&screenshot.rgba),"timed":false});
        self.start_measurement(ready_ms, initial, Instant::now());
        Task::none()
    }

    fn tick(&mut self, app: &mut App, now: Instant) -> Task<RootMessage> {
        if matches!(self.phase, Phase::Finished) {
            return Task::none();
        }
        if self.case_index == self.config.cases.len() {
            if let Phase::Closing { started } = self.phase {
                if now.saturating_duration_since(started).as_secs() >= 5 {
                    self.memory
                        .push(memory_sample("five-seconds-after-preview-close"));
                    self.phase = Phase::Finished;
                    self.finish_file("complete", None);
                    self.progress("complete");
                    return iced::exit();
                }
                return Task::none();
            }
            self.secondary = None;
            self.controls.clear();
            self.observers.clear();
            self.phase = Phase::Closing { started: now };
            let task = app.apply_file_preview_message(file_preview::Message::BackRequested);
            self.memory.push(memory_sample("preview-close-requested"));
            self.progress("closing-and-settling");
            return task;
        }
        let case = self.config.cases[self.case_index].clone();
        match self.phase {
            Phase::Start => {
                if !app.startup_phase.started() {
                    return Task::none();
                }
                self.secondary = None;
                self.memory
                    .push(memory_sample(&format!("{}:before-open", case.name)));
                self.controls.clear();
                self.observers.clear();
                self.start_renderer(format!("{}:loading-and-warmup", case.name));
                self.phase = Phase::Loading {
                    started: now,
                    controls_installed: false,
                };
                self.progress("loading");
                let target = preview_gma::OpenTarget::new(case.archive, case.name, None)
                    .with_initial_entry_preview(case.entry);
                let close_task =
                    app.apply_file_preview_message(file_preview::Message::BackRequested);
                let open_task = app.apply_preview_gma_message(
                    preview_gma::Message::OpenRequested(target),
                    app.update_context,
                );
                return Task::batch([close_task, open_task]);
            }
            Phase::Loading {
                started,
                controls_installed,
            } => {
                if let Some(error) = app.state.features.preview_gma.error() {
                    return self.fail(&format!("{}: archive loading failed: {error:?}", case.name));
                }
                if now.saturating_duration_since(started).as_secs_f64()
                    > self.config.load_timeout_seconds
                {
                    let primary = &app.state.features.file_preview;
                    let draws = self
                        .controls
                        .iter()
                        .map(|control| control.observed.lock().ok().map(|view| view.draws))
                        .collect::<Vec<_>>();
                    return self.fail(&format!(
                        "{}: preview readiness timeout; archive_loading={}, preview_loading={}, stage={:?}, decoded={}, render={:?}, controls_installed={controls_installed}, draws={draws:?}",
                        case.name,
                        app.state.features.preview_gma.loading(),
                        primary.loading(),
                        primary.loading_stage(),
                        primary.current().is_some(),
                        primary.render_status(),
                    ));
                }
                let primary = &mut app.state.features.file_preview;
                if let Some(error) = primary.error() {
                    return self.fail(&format!("{}: {error:?}", case.name));
                }
                if !controls_installed {
                    let Some(data) = primary.current().cloned() else {
                        return Task::none();
                    };
                    if !matches!(
                        data.content,
                        crate::media::preview_model::PreviewContent::Map { .. }
                    ) {
                        return self.fail(&format!("{}: entry is not a map", case.name));
                    }
                    if self.config.expanded && !primary.expanded() {
                        let _effects =
                            file_preview::update(primary, file_preview::Message::ExpandToggled);
                    }
                    let first = self.make_control(case.camera.clone(), &case);
                    primary.set_benchmark_view(Some(first.clone()));
                    self.controls.push(first);
                    if let Some(pose) = &case.second_camera {
                        let Some(request) = primary.request().cloned() else {
                            return self.fail("loaded preview has no request");
                        };
                        let mut secondary = file_preview::State::default();
                        let request = secondary.begin_open(request);
                        file_preview::update(
                            &mut secondary,
                            file_preview::Message::Loaded(request.request_id, Box::new(Ok(data))),
                        );
                        let control = self.make_control(pose.clone(), &case);
                        secondary.set_benchmark_view(Some(control.clone()));
                        self.controls.push(control);
                        self.secondary = Some(secondary);
                    }
                    self.phase = Phase::Loading {
                        started,
                        controls_installed: true,
                    };
                    return Task::none();
                }
                let ready = match primary.benchmark_ready() {
                    Ok(ready) => ready,
                    Err(error) => return self.fail(&error),
                };
                let secondary_ready = match self
                    .secondary
                    .as_ref()
                    .map(file_preview::State::benchmark_ready)
                    .transpose()
                {
                    Ok(ready) => ready.unwrap_or(true),
                    Err(error) => return self.fail(&error),
                };
                let controlled_views_drawn = self.controls.iter().all(|control| {
                    control
                        .observed
                        .lock()
                        .is_ok_and(|observed| observed.draws > 0)
                });
                if ready && secondary_ready && controlled_views_drawn {
                    self.memory
                        .push(memory_sample(&format!("{}:render-ready", case.name)));
                    self.phase = Phase::Warming {
                        started: now,
                        ready_ms: now.saturating_duration_since(started).as_secs_f64() * 1000.0,
                    };
                    self.progress("warming");
                }
            }
            Phase::Warming { started, ready_ms } => {
                if now.saturating_duration_since(started).as_secs_f64() >= case.warmup_seconds {
                    let initial = json!({"renderer":report_value(&self.finish_renderer()), "map_cpu_phases": self.finish_observers()});
                    if self.config.screenshots {
                        let Some(id) = app.window_id else {
                            return self.fail("benchmark has no native window");
                        };
                        self.phase = Phase::TakingScreenshot {
                            started: now,
                            ready_ms,
                            initial,
                        };
                        self.progress("untimed-screenshot");
                        let case_index = self.case_index;
                        return window::screenshot(id).map(move |screenshot| {
                            RootMessage::GuiBenchmark(Message::Screenshot {
                                case_index,
                                screenshot,
                            })
                        });
                    }
                    self.start_measurement(ready_ms, initial, now);
                }
            }
            Phase::TakingScreenshot { started, .. } => {
                if now.saturating_duration_since(started).as_secs() > 30 {
                    return self.fail("window screenshot timeout");
                }
            }
            Phase::Measuring { started, .. } => {
                if now.saturating_duration_since(started).as_secs_f64() >= case.seconds {
                    let Phase::Measuring {
                        ready_ms, initial, ..
                    } = std::mem::replace(&mut self.phase, Phase::Start)
                    else {
                        return Task::none();
                    };
                    let views:Vec<_> = self.controls.iter().map(|control| json!({
                        "logical":control.observed.lock().ok().map(|v| json!(&*v)),
                        "physical_render_extent":control.diagnostics.as_ref().map(file_preview::CaptureDiagnostics::observed_extent),
                        "visibility":control.diagnostics.as_ref().map(file_preview::CaptureDiagnostics::observed_visibility)
                    })).collect();
                    let invalid_pose = self.controls.iter().any(|control| {
                        control.observed.lock().map_or(true, |observed| {
                            observed.pose_mismatch_draws != 0 || observed.non_fly_draws != 0
                        })
                    });
                    let outside_verified = !case.out_of_bounds
                        || self.controls.iter().all(|control| {
                            control.diagnostics.as_ref().is_some_and(|diagnostics| {
                                let counts = diagnostics.observed_visibility();
                                counts["outside_cluster"].as_u64().unwrap_or(0) > 0
                                    && ["disabled", "cluster_pvs", "cluster_without_pvs"]
                                        .iter()
                                        .all(|key| counts[*key].as_u64() == Some(0))
                            })
                        });
                    let renderer_report = self.finish_renderer();
                    let dropped = renderer_report.dropped;
                    let presented_frames = renderer_report.frames.len();
                    let steady = report_value(&renderer_report);
                    let phases = self.finish_observers();
                    self.memory
                        .push(memory_sample(&format!("{}:steady-end", case.name)));
                    let decoded_scene_stats = app.state.features.file_preview.current().and_then(|data| {
                        let crate::media::preview_model::PreviewContent::Map { scene, stats, .. } = &data.content else {
                            return None;
                        };
                        if data.entry_path != case.entry {
                            return None;
                        }
                        Some(json!({"entry":data.entry_path,"crc32":data.crc32,"bytes":data.size_bytes,
                            "bsp_version":stats.version,
                            "material_count":stats.material_count,"resolved_material_count":stats.resolved_material_count,
                            "placed_prop_count":stats.placed_prop_count,"skipped_prop_count":stats.skipped_prop_count,
                            "model_lighting_context_count":scene.model_lighting.len(),
                            "mesh_count":scene.scene.meshes.len(),"sky_mesh_count":scene.map_skybox_meshes.len(),
                            "vertex_count":scene.scene.meshes.iter().map(|mesh|mesh.vertices.len()).sum::<usize>(),
                            "sky_vertex_count":scene.map_skybox_meshes.iter().map(|mesh|mesh.vertices.len()).sum::<usize>(),
                            "lightmap_present":scene.lightmap.is_some(),
                            "lightmap_valid_payloads":stats.lightmap.valid_payloads,
                            "lightmap_missing_payloads":stats.lightmap.missing_payloads,
                            "lightmap_invalid_payloads":stats.lightmap.invalid_payloads,
                            "lightmap_atlas_dropped_blocks":stats.lightmap.atlas_dropped_blocks}))
                    });
                    let preview_current = decoded_scene_stats.is_some();
                    self.results.push(json!({"name":case.name,"ready_ms":ready_ms,"initial":initial,
                        "decoded_scene_stats":decoded_scene_stats,"preview_current":preview_current,
                        "steady":steady,"map_cpu_phases":phases,
                        "view_observations":views,"pair_shared_loaded_scene":self.secondary.is_some()}));
                    if !preview_current {
                        return self.fail("current map disappeared or changed during measurement");
                    }
                    if !outside_verified {
                        return self.fail(&format!(
                            "{}: outside-map route did not stay outside valid BSP clusters",
                            case.name
                        ));
                    }
                    if invalid_pose {
                        return self.fail(&format!(
                            "{}: benchmark camera pose or Fly mode was not preserved",
                            case.name
                        ));
                    }
                    if self.config.profile_renderer && presented_frames == 0 {
                        return self.fail(&format!(
                            "{}: no actual window compositor frames were recorded",
                            case.name
                        ));
                    }
                    if dropped != 0 {
                        return self.fail(&format!(
                            "{}: frame sample bound exceeded ({dropped} dropped)",
                            case.name
                        ));
                    }
                    self.case_index += 1;
                    self.finish_file("running", None);
                    self.progress("between-cases");
                }
            }
            Phase::Closing { .. } | Phase::Finished => {}
        }
        Task::none()
    }
}

fn memory_sample(stage: &str) -> Value {
    let status = fs::read_to_string("/proc/self/status").ok();
    let fields = status.as_deref().map(|status| {
        status
            .lines()
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                matches!(name, "VmRSS" | "VmHWM" | "VmSize" | "Threads")
                    .then(|| (name.to_owned(), value.trim().to_owned()))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    });
    json!({"stage":stage,"process_status":fields})
}

fn distribution(mut values: Vec<f64>) -> Value {
    values.retain(|value| value.is_finite());
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return Value::Null;
    }
    let at = |q: f64| values[(values.len() as f64 * q).ceil() as usize - 1];
    json!({"count":values.len(),"percentile_method":"nearest-rank ceil(n*q)-1","mean":values.iter().sum::<f64>()/values.len() as f64,
           "p50":at(0.5),"p95":at(0.95),"p99":at(0.99),"max":values.last()})
}

fn report_value(report: &iced_wgpu::profiling::Report) -> Value {
    let frames:Vec<_> = report.frames.iter().map(|frame| json!({"frame_index":frame.frame_index,
        "cpu_prepare_ms":frame.cpu_prepare_ms,"cpu_cleanup_ms":frame.cpu_cleanup_ms,"cpu_encode_ms":frame.cpu_encode_ms,"cpu_submit_ms":frame.cpu_submit_ms,
        "surface_acquire_ms":frame.surface_acquire_ms,"surface_present_ms":frame.surface_present_ms,
        "interval_ms":frame.interval_ms,"gpu_ms":frame.gpu_ms,"gpu_scopes":frame.gpu_scopes,
        "input_submissions":frame.input_submissions})).collect();
    json!({"label":report.label,"dropped":report.dropped,"frames":frames,
        "unsubmitted_inputs":report.unsubmitted_inputs,"gpu_query_supported":report.gpu_query_supported,"gpu_ring_exhausted":report.gpu_ring_exhausted,
        "gpu_samples_present":report.frames.iter().filter(|frame|frame.gpu_ms.is_some()).count(),
        "gpu_samples_missing":report.frames.iter().filter(|frame|frame.gpu_ms.is_none()).count(),
        "gpu_queries_asynchronous":true,
        "distributions":{"input_raw_delivery_to_submission_ms":distribution(report.frames.iter()
            .flat_map(|v|v.input_submissions.iter().map(|(_,ms)|*ms)).collect()),"cpu_cleanup_ms":distribution(report.frames.iter().map(|v|v.cpu_cleanup_ms).collect()),"interval_ms":distribution(report.frames.iter().filter_map(|v|v.interval_ms).collect()),
            "cpu_prepare_ms":distribution(report.frames.iter().map(|v|v.cpu_prepare_ms).collect()),
            "cpu_encode_ms":distribution(report.frames.iter().map(|v|v.cpu_encode_ms).collect()),
            "cpu_submit_ms":distribution(report.frames.iter().map(|v|v.cpu_submit_ms).collect()),
            "surface_acquire_ms":distribution(report.frames.iter().map(|v|v.surface_acquire_ms).collect()),
            "surface_present_ms":distribution(report.frames.iter().map(|v|v.surface_present_ms).collect()),
            "gpu_ms":distribution(report.frames.iter().filter_map(|v|v.gpu_ms).collect())}})
}

impl App {
    pub fn with_gui_benchmark(
        mut self,
        config: Option<Config>,
        task: Task<RootMessage>,
    ) -> (Self, Task<RootMessage>) {
        self.gui_benchmark = config.map(Harness::new);
        (self, task)
    }

    pub(super) fn update_gui_benchmark(&mut self, message: Message) -> Task<RootMessage> {
        let Some(mut harness) = self.gui_benchmark.take() else {
            return Task::none();
        };
        let task = match message {
            Message::Tick(now) => harness.tick(self, now),
            Message::Input {
                issued,
                sequence,
                kind,
                captured,
            } => {
                if harness.config.profile_renderer
                    && harness.inputs.len() < harness.config.max_frames
                {
                    harness.inputs.push(json!({"kind":kind,"sequence":sequence,"captured":captured,"dispatch_ms":issued.elapsed().as_secs_f64()*1000.0}));
                }
                Task::none()
            }
            Message::Screenshot {
                case_index,
                screenshot,
            } => harness.screenshot(case_index, &screenshot),
            Message::Secondary(message) => {
                if let Some(state) = &mut harness.secondary {
                    let _effects = file_preview::update(state, message);
                }
                Task::none()
            }
        };
        self.gui_benchmark = Some(harness);
        task
    }

    pub(super) fn gui_benchmark_pair_view(&self) -> Option<Element<'_, RootMessage>> {
        let secondary = self.gui_benchmark.as_ref()?.secondary.as_ref()?;
        let ctx = crate::theme::ViewCtx::new(&self.state.tokens, &self.state.i18n);
        let width = self.state.viewport_size.width * 0.5;
        let panes = iced::widget::row![
            file_preview::pane(&self.state.features.file_preview, ctx, false, width)
                .map(RootMessage::FilePreview),
            file_preview::pane(secondary, ctx, false, width)
                .map(|message| RootMessage::GuiBenchmark(Message::Secondary(message)))
        ]
        .width(Length::Fill)
        .height(Length::Fill);
        Some(
            iced::widget::sensor(panes)
                .on_show(RootMessage::LayoutObserved)
                .on_resize(RootMessage::LayoutObserved)
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_redraws_do_not_publish_root_messages_but_input_is_observed() {
        assert!(
            observed_input(
                &Event::Window(window::Event::RedrawRequested(Instant::now())),
                event::Status::Ignored,
            )
            .is_none()
        );
        assert!(matches!(
            observed_input(
                &Event::Mouse(iced::mouse::Event::CursorLeft),
                event::Status::Captured
            ),
            Some(Message::Input {
                kind: "mouse",
                captured: true,
                ..
            })
        ));
    }

    #[test]
    fn renderer_intervals_are_retained_without_a_second_redraw_series() {
        let report = iced_wgpu::profiling::Report {
            frames: vec![
                iced_wgpu::profiling::FrameSample::default(),
                iced_wgpu::profiling::FrameSample {
                    interval_ms: Some(12.5),
                    ..Default::default()
                },
                iced_wgpu::profiling::FrameSample {
                    interval_ms: Some(25.0),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let value = report_value(&report);
        assert_eq!(value["frames"][0]["interval_ms"], Value::Null);
        assert_eq!(value["frames"][1]["interval_ms"], 12.5);
        assert_eq!(value["distributions"]["interval_ms"]["count"], 2);
        assert_eq!(value["distributions"]["interval_ms"]["mean"], 18.75);
    }

    #[test]
    fn motion_is_derived_from_elapsed_time() {
        let pose = Pose {
            eye: [1.0, 2.0, 3.0],
            angles: [10.0, 20.0],
            velocity: [4.0, 0.0, -1.0],
            yaw_degrees_per_second: 30.0,
        };
        assert_eq!(pose.at(2.0), ([9.0, 2.0, 1.0], [10.0, 80.0]));
    }
    #[test]
    fn missing_gpu_samples_are_not_reported_as_zero() {
        assert_eq!(
            report_value(&iced_wgpu::profiling::Report::default())["distributions"]["gpu_ms"],
            Value::Null
        );
    }
    #[test]
    fn nearest_rank_matches_capture_phase_statistics() {
        assert_eq!(distribution((0..20).map(f64::from).collect())["p95"], 18.0);
    }
    #[test]
    fn invalid_camera_values_are_rejected() {
        let pose = Pose {
            eye: [f32::NAN, 0.0, 0.0],
            angles: [0.0, 0.0],
            velocity: [0.0; 3],
            yaw_degrees_per_second: 0.0,
        };
        assert!(!pose.valid());
    }
    #[test]
    fn percentiles_and_outliers_preserve_raw_values() {
        assert_eq!(distribution(vec![1.0, 2.0, 100.0])["p95"], 100.0);
    }
}
