use gmpublished_backend::threads::join_all_within;

use super::{
    Arc, AssertUnwindSafe, AtomicBool, BLOCKING_FALLBACK_THREADS, BLOCKING_MAX_THREADS,
    BLOCKING_MIN_THREADS, BLOCKING_QUEUE_CAPACITY, Error, JoinHandle, MEDIA_THREADS, Mutex,
    NonZeroUsize, Ordering, SyncSender, TrySendError, WORKER_SHUTDOWN_JOIN_TIMEOUT, catch_unwind,
    mpsc, thread,
};

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum RunBlockingError {
    #[error(transparent)]
    Schedule(#[from] ScheduleError),
    #[error("worker dropped before returning a result")]
    WorkerDropped,
}

impl gmpublished_backend::error_key::HasErrorKey for RunBlockingError {
    fn error_key(&self) -> gmpublished_backend::error_key::ErrorKey {
        gmpublished_backend::error_key::keys::UNKNOWN
    }

    fn error_detail(&self) -> Option<String> {
        Some(self.to_string())
    }
}

pub(super) fn show_native_open_error_dialog(description: String) {
    let _ = futures::executor::block_on(
        rfd::AsyncMessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("gmpublished")
            .set_description(description)
            .show(),
    );
}

pub(super) type WorkerPoolSpawner =
    fn(&AppWorkerRuntime, Arc<str>, RuntimeJob) -> Result<(), ScheduleError>;

#[derive(Debug)]
pub(super) struct AppWorkerRuntime {
    pub(super) blocking: LazyWorkerPool,
    pub(super) media: LazyWorkerPool,
}

impl AppWorkerRuntime {
    pub(super) fn new() -> Self {
        let available = std::thread::available_parallelism().ok();
        Self::with_config(RuntimeConfig {
            blocking_threads: blocking_worker_count(available),
            blocking_queue_capacity: BLOCKING_QUEUE_CAPACITY,
            media_threads: media_worker_count(),
            media_queue_capacity: BLOCKING_QUEUE_CAPACITY,
        })
    }

    pub(super) fn with_config(config: RuntimeConfig) -> Self {
        let blocking = LazyWorkerPool::new(
            "blocking",
            "gmpublished-blocking",
            config.blocking_threads,
            config.blocking_queue_capacity,
        );
        let media = LazyWorkerPool::new(
            "media",
            "gmpublished-media",
            config.media_threads,
            config.media_queue_capacity,
        );

        Self { blocking, media }
    }

    pub(super) fn spawn_blocking(
        &self,
        name: impl Into<Arc<str>>,
        job: impl FnOnce() + Send + 'static,
    ) -> Result<(), ScheduleError> {
        self.spawn_blocking_job(name.into(), Box::new(job))
    }

    pub(super) fn spawn_blocking_job(
        &self,
        name: Arc<str>,
        job: RuntimeJob,
    ) -> Result<(), ScheduleError> {
        self.blocking.submit(name, job)
    }

    pub(super) fn spawn_media_job(
        &self,
        name: Arc<str>,
        job: RuntimeJob,
    ) -> Result<(), ScheduleError> {
        self.media.submit(name, job)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RuntimeConfig {
    pub(super) blocking_threads: usize,
    pub(super) blocking_queue_capacity: usize,
    pub(super) media_threads: usize,
    pub(super) media_queue_capacity: usize,
}

pub(super) fn blocking_worker_count(available: Option<NonZeroUsize>) -> usize {
    available.map_or(BLOCKING_FALLBACK_THREADS, |available| {
        available
            .get()
            .clamp(BLOCKING_MIN_THREADS, BLOCKING_MAX_THREADS)
    })
}

/// Media jobs are network-latency-bound: a synchronous ureq fetch of a small
/// CDN image dominates, and decode/resize is a millisecond-scale tail. Size
/// this pool for concurrent CDN fetches rather than core count; parked threads
/// cost no CPU. `thumbnail_demand::DEFAULT_MAX_IN_FLIGHT` is 2x this width and
/// `thumbnail_worker::decode::HTTP_MAX_IDLE_CONNECTIONS_PER_HOST` is 1x; move
/// all three together.
pub(super) const fn media_worker_count() -> usize {
    MEDIA_THREADS
}

pub(super) type RuntimeJob = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ScheduleError {
    #[error("{pool} worker queue is full while scheduling `{job}`")]
    QueueFull { pool: &'static str, job: Arc<str> },
    #[error("{pool} worker queue is stopped while scheduling `{job}`")]
    PoolStopped { pool: &'static str, job: Arc<str> },
    #[error("failed to start {pool} worker `{thread_name}` while scheduling `{job}`: {message}")]
    PoolStart {
        pool: &'static str,
        thread_name: Arc<str>,
        job: Arc<str>,
        message: Arc<str>,
    },
}

#[derive(Debug, Error)]
#[error("failed to start worker `{thread_name}`: {source}")]
pub(super) struct WorkerPoolInitError {
    thread_name: String,
    source: std::io::Error,
}

pub(super) struct JobEnvelope {
    name: Arc<str>,
    /// The owning pool's shutdown flag, checked once before the job runs so a
    /// queue drained after `shutdown` does no work.
    shutdown: Arc<AtomicBool>,
    job: RuntimeJob,
}

#[derive(Debug)]
pub(super) struct LazyWorkerPool {
    name: &'static str,
    config: WorkerPoolConfig,
    pool: Mutex<Option<WorkerPool>>,
}

impl LazyWorkerPool {
    fn new(
        name: &'static str,
        thread_prefix: &'static str,
        thread_count: usize,
        queue_capacity: usize,
    ) -> Self {
        Self {
            name,
            config: WorkerPoolConfig {
                thread_prefix,
                thread_count,
                queue_capacity,
            },
            pool: Mutex::new(None),
        }
    }

    fn submit(&self, name: impl Into<Arc<str>>, job: RuntimeJob) -> Result<(), ScheduleError> {
        let job_name = name.into();
        let mut pool = self.pool.lock();
        let pool = if let Some(pool) = pool.as_mut() {
            pool
        } else {
            pool.insert(
                WorkerPool::start(
                    self.name,
                    self.config.thread_prefix,
                    self.config.thread_count,
                    self.config.queue_capacity,
                )
                .map_err(|source| ScheduleError::PoolStart {
                    pool: self.name,
                    thread_name: Arc::from(source.thread_name),
                    job: Arc::clone(&job_name),
                    message: Arc::from(source.source.to_string()),
                })?,
            )
        };

        pool.submit(job_name, job)
    }

    #[cfg(test)]
    pub(super) fn started(&self) -> bool {
        self.pool.lock().is_some()
    }
}

#[derive(Debug)]
pub(super) struct WorkerPoolConfig {
    thread_prefix: &'static str,
    thread_count: usize,
    queue_capacity: usize,
}

#[derive(Debug)]
pub(super) struct WorkerPool {
    name: &'static str,
    state: Mutex<PoolState>,
    shutdown: Arc<AtomicBool>,
}

/// One lock over the sender and the threads it feeds, because they are one
/// fact: a pool is either accepting work through a live channel with workers
/// draining it, or it is not.
///
/// Splitting them across two mutexes makes "sender present, workers gone"
/// representable — a state nothing can check for and nothing could act on.
#[derive(Debug)]
enum PoolState {
    Running {
        sender: SyncSender<JobEnvelope>,
        workers: Vec<JoinHandle<()>>,
    },
    Stopped,
}

impl WorkerPool {
    fn start(
        name: &'static str,
        thread_prefix: &'static str,
        thread_count: usize,
        queue_capacity: usize,
    ) -> Result<Self, WorkerPoolInitError> {
        let (sender, receiver) = mpsc::sync_channel(queue_capacity.max(1));
        let receiver = Arc::new(Mutex::new(receiver));
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::with_capacity(thread_count.max(1));

        for index in 0..thread_count.max(1) {
            let thread_name = format!("{thread_prefix}-{index}");
            let worker_receiver = Arc::clone(&receiver);
            match thread::Builder::new()
                .name(thread_name.clone())
                .spawn(move || worker_loop(&worker_receiver))
            {
                Ok(worker) => workers.push(worker),
                Err(source) => {
                    drop(sender);
                    join_workers_within_bound(name, workers);
                    return Err(WorkerPoolInitError {
                        thread_name,
                        source,
                    });
                }
            }
        }

        Ok(Self {
            name,
            state: Mutex::new(PoolState::Running { sender, workers }),
            shutdown,
        })
    }

    fn submit(&self, name: impl Into<Arc<str>>, job: RuntimeJob) -> Result<(), ScheduleError> {
        let name = name.into();
        let envelope = JobEnvelope {
            name: Arc::clone(&name),
            shutdown: Arc::clone(&self.shutdown),
            job,
        };

        let result = {
            let state = self.state.lock();
            match &*state {
                PoolState::Running { sender, .. } => sender.try_send(envelope),
                PoolState::Stopped => return Err(self.reject(name, ScheduleErrorKind::Stopped)),
            }
        };

        match result {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(self.reject(name, ScheduleErrorKind::Full)),
            Err(TrySendError::Disconnected(_)) => {
                Err(self.reject(name, ScheduleErrorKind::Stopped))
            }
        }
    }

    fn reject(&self, job: Arc<str>, kind: ScheduleErrorKind) -> ScheduleError {
        match kind {
            ScheduleErrorKind::Full => ScheduleError::QueueFull {
                pool: self.name,
                job,
            },
            ScheduleErrorKind::Stopped => ScheduleError::PoolStopped {
                pool: self.name,
                job,
            },
        }
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let mut state = self.state.lock();
        let PoolState::Running { sender, workers } =
            std::mem::replace(&mut *state, PoolState::Stopped)
        else {
            return;
        };
        // The lock goes before the join, not after: holding it across a join
        // would block a concurrent `submit` for as long as the slowest worker
        // takes to notice the shutdown.
        drop(state);
        // Dropped before the join, because a worker only sees the channel
        // disconnect once the last sender is gone.
        drop(sender);
        join_workers_within_bound(self.name, workers);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScheduleErrorKind {
    Full,
    Stopped,
}

pub(super) fn worker_loop(receiver: &Arc<Mutex<mpsc::Receiver<JobEnvelope>>>) {
    loop {
        let envelope = {
            let receiver = receiver.lock();
            receiver.recv()
        };

        let Ok(envelope) = envelope else {
            break;
        };

        run_envelope(envelope);
    }
}

pub(super) fn run_envelope(envelope: JobEnvelope) {
    let JobEnvelope {
        name,
        shutdown,
        job,
    } = envelope;

    if shutdown.load(Ordering::Acquire) {
        return;
    }

    if catch_unwind(AssertUnwindSafe(job)).is_err() {
        log::error!("backend worker job `{name}` panicked");
    }
}

/// Joins `workers` within [`WORKER_SHUTDOWN_JOIN_TIMEOUT`].
///
/// A job already running never observes the pool's shutdown flag — that is
/// checked once, before the job starts ([`run_envelope`]) — so a pack, bake or
/// extraction in flight would otherwise hold process exit for as long as it
/// takes to finish. The bound trades a worker's remaining work for a window
/// that closes.
fn join_workers_within_bound(pool: &'static str, workers: Vec<JoinHandle<()>>) {
    join_all_within(workers, WORKER_SHUTDOWN_JOIN_TIMEOUT, pool);
}

#[cfg(test)]
mod tests {
    use std::{sync::Barrier, time::Instant};

    use super::{Arc, WORKER_SHUTDOWN_JOIN_TIMEOUT, WorkerPool, thread};

    /// A job already running never observes the shutdown flag, so dropping the
    /// pool has to give up on it. Without a bound this waits out the job, which
    /// on a large pack or bake means the window is gone and the process is
    /// still alive.
    #[test]
    fn dropping_a_pool_mid_job_returns_within_the_join_bound() {
        let pool =
            WorkerPool::start("test", "gmpublished-test-join", 1, 4).expect("pool should start");

        let running = Arc::new(Barrier::new(2));
        let job_running = Arc::clone(&running);
        pool.submit(
            "blocking-job",
            Box::new(move || {
                job_running.wait();
                thread::sleep(WORKER_SHUTDOWN_JOIN_TIMEOUT * 6);
            }),
        )
        .expect("submit should be accepted");

        // Only meaningful once the job is actually executing: a job still in
        // the queue is skipped by the shutdown check and joins immediately.
        running.wait();

        let before = Instant::now();
        drop(pool);
        let elapsed = before.elapsed();

        assert!(
            elapsed < WORKER_SHUTDOWN_JOIN_TIMEOUT * 4,
            "drop blocked for {elapsed:?}, which is not bounded by {WORKER_SHUTDOWN_JOIN_TIMEOUT:?}"
        );
    }

    /// The bound is shared, not per-thread: N stuck workers must still cost one
    /// timeout in total.
    #[test]
    fn the_join_bound_is_shared_across_workers_not_paid_per_worker() {
        const WORKERS: usize = 4;

        let pool = WorkerPool::start("test", "gmpublished-test-join-shared", WORKERS, WORKERS)
            .expect("pool should start");

        let running = Arc::new(Barrier::new(WORKERS + 1));
        for _ in 0..WORKERS {
            let job_running = Arc::clone(&running);
            pool.submit(
                "blocking-job",
                Box::new(move || {
                    job_running.wait();
                    thread::sleep(WORKER_SHUTDOWN_JOIN_TIMEOUT * 6);
                }),
            )
            .expect("submit should be accepted");
        }
        running.wait();

        let before = Instant::now();
        drop(pool);
        let elapsed = before.elapsed();

        // One bound plus scheduler slack — not `* WORKERS`, which is exactly
        // the regression this test exists to catch.
        assert!(
            elapsed < WORKER_SHUTDOWN_JOIN_TIMEOUT * 2,
            "drop blocked for {elapsed:?}; {WORKERS} stuck workers should still cost one bound"
        );
    }
}
