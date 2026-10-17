use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

const QUERY_COUNT: u32 = 128;
const RING_SIZE: usize = 8;

#[derive(Debug, Default)]
pub struct Report {
    pub label: String,
    pub frames: Vec<FrameSample>,
    pub dropped: u64,
    pub unsubmitted_inputs: usize,
    pub gpu_query_supported: Option<bool>,
    pub gpu_ring_exhausted: u64,
}

#[derive(Debug, Default)]
pub struct FrameSample {
    pub frame_index: usize,
    pub cpu_prepare_ms: f64,
    pub cpu_encode_ms: f64,
    pub cpu_cleanup_ms: f64,
    pub cpu_submit_ms: f64,
    pub surface_acquire_ms: f64,
    pub surface_present_ms: f64,
    pub interval_ms: Option<f64>,
    pub gpu_ms: Option<f64>,
    pub gpu_scopes: Vec<(&'static str, f64)>,
    pub input_submissions: Vec<(u64, f64)>,
}

#[derive(Default)]
struct State {
    epoch: u64,
    limit: usize,
    report: Report,
    previous: Option<Instant>,
    inputs: Vec<(u64, Instant)>,
    input_sequence: u64,
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static STATE: OnceLock<Mutex<State>> = OnceLock::new();

pub fn start(label: String, limit: usize) {
    let mut state = STATE
        .get_or_init(Default::default)
        .lock()
        .expect("profiler lock");
    state.epoch = state.epoch.wrapping_add(1);
    state.limit = limit;
    state.report = Report {
        label,
        frames: Vec::with_capacity(limit),
        dropped: 0,
        ..Default::default()
    };
    state.previous = None;
    state.inputs.clear();
    ACTIVE.store(true, Ordering::Relaxed);
}

pub fn finish() -> Report {
    ACTIVE.store(false, Ordering::Relaxed);
    let mut state = STATE
        .get_or_init(Default::default)
        .lock()
        .expect("profiler lock");
    state.report.unsubmitted_inputs = state.inputs.len();
    std::mem::take(&mut state.report)
}

pub(crate) fn enabled() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

pub fn mark_input(issued: Instant) -> Option<u64> {
    if !enabled() {
        return None;
    }
    let mut state = STATE
        .get_or_init(Default::default)
        .lock()
        .expect("profiler lock");
    if state.inputs.len() >= state.limit {
        return None;
    }
    state.input_sequence = state.input_sequence.wrapping_add(1);
    let sequence = state.input_sequence;
    state.inputs.push((sequence, issued));
    Some(sequence)
}

#[derive(Clone, Copy)]
pub(crate) struct FrameToken {
    epoch: u64,
    index: usize,
    started: Instant,
}

pub(crate) fn begin() -> Option<FrameToken> {
    if !enabled() {
        return None;
    }
    let now = Instant::now();
    let mut state = STATE
        .get_or_init(Default::default)
        .lock()
        .expect("profiler lock");
    if state.report.frames.len() == state.limit {
        state.report.dropped += 1;
        return None;
    }
    let index = state.report.frames.len();
    let interval_ms = state
        .previous
        .replace(now)
        .map(|previous| (now - previous).as_secs_f64() * 1000.0);
    let input_submissions = state
        .inputs
        .drain(..)
        .map(|(sequence, issued)| {
            (
                sequence,
                now.saturating_duration_since(issued).as_secs_f64() * 1000.0,
            )
        })
        .collect();
    state.report.frames.push(FrameSample {
        frame_index: index,
        interval_ms,
        input_submissions,
        ..Default::default()
    });
    Some(FrameToken {
        epoch: state.epoch,
        index,
        started: now,
    })
}

pub(crate) fn submitted(token: FrameToken) {
    let elapsed = elapsed(token.started);
    update(token, |sample| {
        for (_, delay) in &mut sample.input_submissions {
            *delay += elapsed;
        }
    });
}

pub(crate) fn update(token: FrameToken, update: impl FnOnce(&mut FrameSample)) {
    let mut state = STATE
        .get_or_init(Default::default)
        .lock()
        .expect("profiler lock");
    if state.epoch == token.epoch
        && let Some(sample) = state.report.frames.get_mut(token.index)
    {
        update(sample);
    }
}

pub(crate) fn elapsed(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

struct Slot {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    busy: Arc<AtomicBool>,
}

#[derive(Default)]
pub(crate) struct GpuProfiler {
    slots: Vec<Slot>,
}

struct Recording {
    queries: wgpu::QuerySet,
    names: Vec<(&'static str, u32, u32)>,
    next: u32,
}

thread_local! { static RECORDING: std::cell::RefCell<Option<Recording>> = const { std::cell::RefCell::new(None) }; }

pub struct GpuScope {
    name: &'static str,
    begin: u32,
}

pub fn begin_gpu_scope(encoder: &mut wgpu::CommandEncoder, name: &'static str) -> Option<GpuScope> {
    RECORDING.with_borrow_mut(|recording| {
        let recording = recording.as_mut()?;
        if recording.next + 2 >= QUERY_COUNT {
            return None;
        }
        let begin = recording.next;
        recording.next += 2;
        encoder.write_timestamp(&recording.queries, begin);
        Some(GpuScope { name, begin })
    })
}

pub fn end_gpu_scope(encoder: &mut wgpu::CommandEncoder, scope: Option<GpuScope>) {
    let Some(scope) = scope else {
        return;
    };
    RECORDING.with_borrow_mut(|recording| {
        if let Some(recording) = recording {
            encoder.write_timestamp(&recording.queries, scope.begin + 1);
            recording
                .names
                .push((scope.name, scope.begin, scope.begin + 1));
        }
    });
}

pub(crate) struct Pending {
    slot: usize,
    token: FrameToken,
    names: Vec<(&'static str, u32, u32)>,
    count: u32,
}

impl GpuProfiler {
    pub(crate) fn begin(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        token: Option<FrameToken>,
    ) -> Option<Pending> {
        let token = token?;
        let supported = device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);
        {
            let mut state = STATE
                .get_or_init(Default::default)
                .lock()
                .expect("profiler lock");
            state.report.gpu_query_supported = Some(supported);
        }
        if !supported {
            return None;
        }
        let _ = device.poll(wgpu::PollType::Poll);
        if self.slots.is_empty() {
            self.slots = (0..RING_SIZE)
                .map(|_| Slot {
                    queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                        label: Some("renderer.profile"),
                        ty: wgpu::QueryType::Timestamp,
                        count: QUERY_COUNT,
                    }),
                    resolve: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("renderer.profile.resolve"),
                        size: u64::from(QUERY_COUNT) * 8,
                        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                        mapped_at_creation: false,
                    }),
                    readback: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("renderer.profile.readback"),
                        size: u64::from(QUERY_COUNT) * 8,
                        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    busy: Arc::new(AtomicBool::new(false)),
                })
                .collect();
        }
        let slot = self
            .slots
            .iter()
            .position(|slot| !slot.busy.load(Ordering::Acquire));
        let Some(slot) = slot else {
            STATE
                .get_or_init(Default::default)
                .lock()
                .expect("profiler lock")
                .report
                .gpu_ring_exhausted += 1;
            return None;
        };
        self.slots[slot].busy.store(true, Ordering::Release);
        encoder.write_timestamp(&self.slots[slot].queries, 0);
        RECORDING.with_borrow_mut(|recording| {
            *recording = Some(Recording {
                queries: self.slots[slot].queries.clone(),
                names: Vec::with_capacity(32),
                next: 1,
            })
        });
        Some(Pending {
            slot,
            token,
            names: Vec::new(),
            count: 0,
        })
    }

    pub(crate) fn resolve(&self, encoder: &mut wgpu::CommandEncoder, pending: &mut Pending) {
        let recording = RECORDING
            .with_borrow_mut(Option::take)
            .expect("GPU profile recording");
        let slot = &self.slots[pending.slot];
        encoder.write_timestamp(&slot.queries, recording.next);
        pending.count = recording.next + 1;
        pending.names = recording.names;
        encoder.resolve_query_set(&slot.queries, 0..pending.count, &slot.resolve, 0);
        encoder.copy_buffer_to_buffer(
            &slot.resolve,
            0,
            &slot.readback,
            0,
            u64::from(pending.count) * 8,
        );
    }

    pub(crate) fn submitted(&self, queue: &wgpu::Queue, pending: Pending) {
        let slot = &self.slots[pending.slot];
        let buffer = slot.readback.clone();
        let busy = Arc::clone(&slot.busy);
        let period = f64::from(queue.get_timestamp_period()) / 1_000_000.0;
        slot.readback
            .slice(..u64::from(pending.count) * 8)
            .map_async(wgpu::MapMode::Read, move |result| {
                if result.is_ok() {
                    let data = buffer
                        .slice(..u64::from(pending.count) * 8)
                        .get_mapped_range();
                    let times: Vec<u64> = data
                        .chunks_exact(8)
                        .map(|bytes| u64::from_ne_bytes(bytes.try_into().expect("timestamp width")))
                        .collect();
                    update(pending.token, |sample| {
                        sample.gpu_ms =
                            Some(times[times.len() - 1].saturating_sub(times[0]) as f64 * period);
                        sample.gpu_scopes = pending
                            .names
                            .into_iter()
                            .map(|(name, begin, end)| {
                                (
                                    name,
                                    times[end as usize].saturating_sub(times[begin as usize])
                                        as f64
                                        * period,
                                )
                            })
                            .collect();
                    });
                    drop(data);
                    buffer.unmap();
                }
                busy.store(false, Ordering::Release);
            });
    }
}
