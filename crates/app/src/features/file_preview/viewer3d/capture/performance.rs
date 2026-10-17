use serde_json::{Value, json};
use std::sync::{
    Arc, LockResult, Mutex, MutexGuard,
    atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};
use std::time::Instant;

#[derive(Clone, Copy)]
pub(in crate::features::file_preview::viewer3d) enum Phase {
    Prepare,
    Upload,
    WorldVisibility,
    MainPlan,
    PlanarSelection,
    OpticalPrepare,
    Intersection,
    Lighting,
    LightingVisibility,
    LightmapUpdate,
    ModelLightingUpdate,
    Transparency,
    Diagnostics,
    Encode,
}

impl Phase {
    const ALL: [Self; 14] = [
        Self::Prepare,
        Self::Upload,
        Self::WorldVisibility,
        Self::MainPlan,
        Self::PlanarSelection,
        Self::OpticalPrepare,
        Self::Intersection,
        Self::Lighting,
        Self::LightingVisibility,
        Self::LightmapUpdate,
        Self::ModelLightingUpdate,
        Self::Transparency,
        Self::Diagnostics,
        Self::Encode,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Prepare => "prepare_total",
            Self::Upload => "upload_lookup",
            Self::WorldVisibility => "world_visibility",
            Self::MainPlan => "main_draw_plans",
            Self::PlanarSelection => "planar_selection",
            Self::OpticalPrepare => "optical_preparation",
            Self::Intersection => "intersection",
            Self::Lighting => "lighting",
            Self::LightingVisibility => "lighting_visibility",
            Self::LightmapUpdate => "lightmap_update",
            Self::ModelLightingUpdate => "model_lighting_update",
            Self::Transparency => "main_transparency_and_planars",
            Self::Diagnostics => "capture_json_observer",
            Self::Encode => "encode_total",
        }
    }
}

#[derive(Clone, Default)]
pub struct CaptureDiagnostics(Arc<State>);

pub(in crate::features::file_preview::viewer3d) enum VisibilityMode {
    Disabled,
    OutsideCluster,
    ClusterPvs,
    ClusterWithoutPvs,
}

impl std::fmt::Debug for CaptureDiagnostics {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureDiagnostics")
            .field("recording", &self.recording())
            .field("profiling", &self.0.profiling.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct State {
    record: Mutex<Value>,
    paused: AtomicUsize,
    profiling: AtomicBool,
    samples: Mutex<Option<Samples>>,
    extent: [AtomicU32; 2],
    visibility: [AtomicU64; 4],
}

struct Samples {
    phases: [Vec<f64>; Phase::ALL.len()],
    limit: usize,
    omitted: [u64; Phase::ALL.len()],
}

impl CaptureDiagnostics {
    #[cfg(feature = "gui-benchmark")]
    pub fn observed_visibility(&self) -> Value {
        let counts = self
            .0
            .visibility
            .each_ref()
            .map(|value| value.load(Ordering::Relaxed));
        json!({"disabled":counts[0],"outside_cluster":counts[1],"cluster_pvs":counts[2],"cluster_without_pvs":counts[3]})
    }
    pub(in crate::features::file_preview::viewer3d) fn observe_visibility(
        &self,
        mode: VisibilityMode,
    ) {
        self.0.visibility[mode as usize].fetch_add(1, Ordering::Relaxed);
    }
    #[cfg(feature = "gui-benchmark")]
    pub fn observed_extent(&self) -> [u32; 2] {
        self.0
            .extent
            .each_ref()
            .map(|value| value.load(Ordering::Relaxed))
    }
    pub(in crate::features::file_preview::viewer3d) fn observe_extent(&self, size: (u32, u32)) {
        self.0.extent[0].store(size.0, Ordering::Relaxed);
        self.0.extent[1].store(size.1, Ordering::Relaxed);
    }
    pub fn lock(&self) -> LockResult<MutexGuard<'_, Value>> {
        self.0.record.lock()
    }
    pub fn recording(&self) -> bool {
        self.0.paused.load(Ordering::Relaxed) == 0
    }
    pub fn pause(&self) -> Pause {
        self.0.paused.fetch_add(1, Ordering::Relaxed);
        Pause(self.clone())
    }
    pub fn profile(&self, samples: usize) -> Profile {
        assert!(!self.0.profiling.swap(true, Ordering::Relaxed));
        *self.0.samples.lock().expect("profile samples lock") = Some(Samples {
            phases: std::array::from_fn(|_| Vec::with_capacity(samples)),
            limit: samples,
            omitted: [0; Phase::ALL.len()],
        });
        Profile(self.clone())
    }
    pub(in crate::features::file_preview::viewer3d) fn span(&self, phase: Phase) -> Option<Span> {
        self.0.profiling.load(Ordering::Relaxed).then(|| Span {
            diagnostics: self.clone(),
            phase,
            started: Instant::now(),
        })
    }
}

pub struct Pause(CaptureDiagnostics);
impl Drop for Pause {
    fn drop(&mut self) {
        self.0.0.paused.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct Profile(CaptureDiagnostics);
impl Profile {
    pub fn finish(self) -> Value {
        self.0.0.profiling.store(false, Ordering::Relaxed);
        let samples = self
            .0
            .0
            .samples
            .lock()
            .expect("profile samples lock")
            .take()
            .expect("active profile");
        Value::Object(
            Phase::ALL
                .into_iter()
                .zip(samples.phases)
                .enumerate()
                .map(|(index, (phase, times))| {
                    let mut summary = distribution(times);
                    if let Some(object) = summary.as_object_mut() {
                        object.insert("omitted_samples".to_owned(), samples.omitted[index].into());
                    }
                    (phase.name().to_owned(), summary)
                })
                .collect(),
        )
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        self.0.0.profiling.store(false, Ordering::Relaxed);
    }
}

pub(in crate::features::file_preview::viewer3d) struct Span {
    diagnostics: CaptureDiagnostics,
    phase: Phase,
    started: Instant,
}
impl Drop for Span {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed().as_secs_f64() * 1000.0;
        if let Some(samples) = self
            .diagnostics
            .0
            .samples
            .lock()
            .expect("profile samples lock")
            .as_mut()
        {
            let index = self.phase as usize;
            if samples.phases[index].len() < samples.limit {
                samples.phases[index].push(elapsed);
            } else {
                samples.omitted[index] += 1;
            }
        }
    }
}

pub(super) fn distribution(mut times: Vec<f64>) -> Value {
    if times.is_empty() {
        return Value::Null;
    }
    times.sort_by(f64::total_cmp);
    let count = times.len();
    json!({"samples":count,"mean_ms":times.iter().sum::<f64>()/count as f64,"median_ms":times[count/2],"p95_ms":times[(count*95).div_ceil(100).saturating_sub(1)],"p99_ms":times[(count*99).div_ceil(100).saturating_sub(1)],"min_ms":times[0],"max_ms":times[count-1]})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observer_pauses_are_nested_and_view_specific() {
        let first = CaptureDiagnostics::default();
        let second = CaptureDiagnostics::default();
        *first.lock().unwrap() = json!({"preserved":true});
        let outer = first.pause();
        let alias = first.clone();
        let inner = alias.pause();
        assert!(!alias.recording());
        assert!(!first.recording());
        assert!(second.recording());
        drop(outer);
        assert!(!first.recording());
        drop(inner);
        assert!(first.recording());
        assert_eq!(*first.lock().unwrap(), json!({"preserved":true}));
    }
    #[test]
    fn profiling_is_explicit_and_does_not_require_observer_recording() {
        let diagnostics = CaptureDiagnostics::default();
        assert!(diagnostics.span(Phase::Prepare).is_none());
        let pause = diagnostics.pause();
        let profile = diagnostics.profile(1);
        drop(diagnostics.span(Phase::Prepare));
        let summary = profile.finish();
        assert_eq!(summary["prepare_total"]["samples"], 1);
        assert!(summary["capture_json_observer"].is_null());
        assert!(diagnostics.span(Phase::Prepare).is_none());
        drop(pause);
        assert!(diagnostics.recording());
    }
    #[test]
    fn phase_storage_is_bounded_and_overflow_is_reported() {
        let diagnostics = CaptureDiagnostics::default();
        let profile = diagnostics.profile(1);
        drop(diagnostics.span(Phase::Encode));
        drop(diagnostics.span(Phase::Encode));
        let summary = profile.finish();
        assert_eq!(summary["encode_total"]["samples"], 1);
        assert_eq!(summary["encode_total"]["omitted_samples"], 1);
    }
}
