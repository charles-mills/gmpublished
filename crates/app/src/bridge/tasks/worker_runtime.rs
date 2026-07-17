use super::{
    Arc, AssertUnwindSafe, AtomicBool, BLOCKING_FALLBACK_THREADS, BLOCKING_MAX_THREADS,
    BLOCKING_MIN_THREADS, BLOCKING_QUEUE_CAPACITY, Error, JoinHandle, MEDIA_THREADS, Mutex,
    NonZeroUsize, Ordering, SyncSender, TrySendError, catch_unwind, mpsc, thread,
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
    let _ = block_on_worker(
        rfd::AsyncMessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("gmpublished")
            .set_description(description)
            .show(),
    );
}

pub(super) fn block_on_worker<F: std::future::Future>(future: F) -> F::Output {
    futures::executor::block_on(future)
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
        job: impl FnOnce(CancellationToken) + Send + 'static,
    ) -> Result<(), ScheduleError> {
        self.spawn_blocking_job(name.into(), Box::new(job))
    }

    pub(super) fn spawn_blocking_job(
        &self,
        name: Arc<str>,
        job: RuntimeJob,
    ) -> Result<(), ScheduleError> {
        self.blocking.submit(name, &CancellationToken::new(), job)
    }

    pub(super) fn spawn_media_job(
        &self,
        name: Arc<str>,
        job: RuntimeJob,
    ) -> Result<(), ScheduleError> {
        self.media.submit(name, &CancellationToken::new(), job)
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

#[derive(Clone, Debug)]
pub(super) struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    parents: Vec<Arc<AtomicBool>>,
}

impl CancellationToken {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            parents: Vec::new(),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || self
                .parents
                .iter()
                .any(|parent| parent.load(Ordering::Acquire))
    }

    fn with_parent(&self, parent: Arc<AtomicBool>) -> Self {
        let mut parents = self.parents.clone();
        parents.push(parent);
        Self {
            cancelled: Arc::clone(&self.cancelled),
            parents,
        }
    }
}

pub(super) type RuntimeJob = Box<dyn FnOnce(CancellationToken) + Send + 'static>;

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
    token: CancellationToken,
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

    fn submit(
        &self,
        name: impl Into<Arc<str>>,
        token: &CancellationToken,
        job: RuntimeJob,
    ) -> Result<(), ScheduleError> {
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

        pool.submit(job_name, token, job)
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
    sender: Mutex<Option<SyncSender<JobEnvelope>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    shutdown: Arc<AtomicBool>,
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
                    join_started_workers(workers);
                    return Err(WorkerPoolInitError {
                        thread_name,
                        source,
                    });
                }
            }
        }

        Ok(Self {
            name,
            sender: Mutex::new(Some(sender)),
            workers: Mutex::new(workers),
            shutdown,
        })
    }

    fn submit(
        &self,
        name: impl Into<Arc<str>>,
        token: &CancellationToken,
        job: RuntimeJob,
    ) -> Result<(), ScheduleError> {
        let name = name.into();
        let envelope = JobEnvelope {
            name: Arc::clone(&name),
            token: token.with_parent(Arc::clone(&self.shutdown)),
            job,
        };

        let result = {
            let sender = self.sender.lock();
            match sender.as_ref() {
                Some(sender) => sender.try_send(envelope),
                None => return Err(self.reject(name, ScheduleErrorKind::Stopped)),
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
        {
            let mut sender = self.sender.lock();
            sender.take();
        }

        let mut workers = self.workers.lock();
        join_started_workers(std::mem::take(&mut *workers));
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
    let JobEnvelope { name, token, job } = envelope;

    if token.is_cancelled() {
        return;
    }

    if catch_unwind(AssertUnwindSafe(|| job(token))).is_err() {
        log::error!("backend worker job `{name}` panicked");
    }
}

pub(super) fn join_started_workers(workers: Vec<JoinHandle<()>>) {
    for worker in workers {
        if worker.join().is_err() {
            log::error!("backend worker panicked during shutdown");
        }
    }
}
