//! Process-owned execution resources shared by backend and app workloads.
//!
//! CPU parallelism uses one Rayon pool. Work that may park on filesystem,
//! Steam, or HTTP I/O uses separately bounded standard-thread executors so it
//! cannot consume the CPU pool's workers.

use parking_lot::Mutex;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

struct Job {
    name: Arc<str>,
    work: Box<dyn FnOnce() + Send + 'static>,
}

const DEFAULT_QUEUE_CAPACITY: usize = 256;
const MAX_CPU_THREADS: usize = 8;
const MAX_BLOCKING_THREADS: usize = 8;
const NETWORK_THREADS: usize = 8;
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionConfig {
    pub cpu_threads: usize,
    pub blocking_threads: usize,
    pub network_threads: usize,
    pub queue_capacity: usize,
}

impl ExecutionConfig {
    #[must_use]
    pub fn for_machine() -> Self {
        let available = std::thread::available_parallelism().map_or(1, usize::from);
        Self {
            cpu_threads: available.saturating_sub(2).clamp(1, MAX_CPU_THREADS),
            blocking_threads: available.clamp(2, MAX_BLOCKING_THREADS),
            network_threads: NETWORK_THREADS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
        }
    }
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self::for_machine()
    }
}

#[derive(Clone)]
pub struct CpuExecutor(Arc<rayon::ThreadPool>);

impl fmt::Debug for CpuExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CpuExecutor")
            .field("threads", &self.0.current_num_threads())
            .finish()
    }
}

impl CpuExecutor {
    pub fn build(threads: usize) -> Result<Self, ExecutionInitError> {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads.max(1))
            .thread_name(|index| format!("gmpublished-cpu-{index}"))
            .build()
            .map(|pool| Self(Arc::new(pool)))
            .map_err(|source| ExecutionInitError::Cpu {
                message: source.to_string(),
            })
    }

    pub fn install<R: Send>(&self, work: impl FnOnce() -> R + Send) -> R {
        self.0.install(work)
    }

    /// The owned Rayon pool for APIs that need scoped parallelism and must
    /// not fall back to Rayon's ambient global pool.
    #[must_use]
    pub fn rayon_pool(&self) -> &rayon::ThreadPool {
        &self.0
    }

    pub fn spawn(&self, work: impl FnOnce() + Send + 'static) {
        self.0.spawn(work);
    }

    #[must_use]
    pub fn thread_count(&self) -> usize {
        self.0.current_num_threads()
    }
}

#[derive(Clone)]
struct TaskExecutor(Arc<TaskExecutorInner>);

struct TaskExecutorInner {
    name: &'static str,
    sender: Mutex<Option<SyncSender<Job>>>,
    shutdown: Arc<AtomicBool>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl fmt::Debug for TaskExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskExecutor")
            .field("name", &self.0.name)
            .field("workers", &self.0.workers.lock().len())
            .finish()
    }
}

impl TaskExecutor {
    fn build(
        name: &'static str,
        thread_prefix: &'static str,
        thread_count: usize,
        queue_capacity: usize,
    ) -> Result<Self, ExecutionInitError> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(queue_capacity.max(1));
        let receiver = Arc::new(Mutex::new(receiver));
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::with_capacity(thread_count.max(1));

        for index in 0..thread_count.max(1) {
            let thread_name = format!("{thread_prefix}-{index}");
            let receiver = Arc::clone(&receiver);
            let shutdown = Arc::clone(&shutdown);
            match std::thread::Builder::new()
                .name(thread_name.clone())
                .spawn(move || worker_loop(name, &receiver, &shutdown))
            {
                Ok(worker) => workers.push(worker),
                Err(source) => {
                    drop(sender);
                    for worker in workers {
                        let _ = worker.join();
                    }
                    return Err(ExecutionInitError::Worker {
                        executor: name,
                        thread_name,
                        source,
                    });
                }
            }
        }

        Ok(Self(Arc::new(TaskExecutorInner {
            name,
            sender: Mutex::new(Some(sender)),
            shutdown,
            workers: Mutex::new(workers),
        })))
    }

    fn spawn(
        &self,
        job_name: Arc<str>,
        job: impl FnOnce() + Send + 'static,
    ) -> Result<(), ExecutionScheduleError> {
        let sender = self.0.sender.lock();
        let Some(sender) = sender.as_ref() else {
            return Err(ExecutionScheduleError::Stopped {
                executor: self.0.name,
                job: Arc::clone(&job_name),
            });
        };
        match sender.try_send(Job {
            name: Arc::clone(&job_name),
            work: Box::new(job),
        }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(ExecutionScheduleError::Full {
                executor: self.0.name,
                job: Arc::clone(&job_name),
            }),
            Err(TrySendError::Disconnected(_)) => Err(ExecutionScheduleError::Stopped {
                executor: self.0.name,
                job: job_name,
            }),
        }
    }
}

impl Drop for TaskExecutorInner {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.sender.get_mut().take();
        let current = std::thread::current().id();
        let mut joinable = Vec::new();
        for worker in self.workers.get_mut().drain(..) {
            // The last capability can be released by one of its own jobs
            // during shutdown. Dropping that worker's handle detaches it;
            // joining oneself would deadlock.
            if worker.thread().id() != current {
                joinable.push(worker);
            }
        }
        crate::threads::join_all_within(joinable, SHUTDOWN_JOIN_TIMEOUT, self.name);
    }
}

fn worker_loop(
    executor_name: &'static str,
    receiver: &Mutex<mpsc::Receiver<Job>>,
    shutdown: &AtomicBool,
) {
    loop {
        let job = receiver.lock().recv();
        let Ok(job) = job else {
            break;
        };
        if shutdown.load(Ordering::Acquire) {
            continue;
        }
        if catch_unwind(AssertUnwindSafe(job.work)).is_err() {
            log::error!("{executor_name} executor job `{}` panicked", job.name);
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExecutionResources {
    cpu: CpuExecutor,
    blocking: TaskExecutor,
    network: TaskExecutor,
}

impl ExecutionResources {
    pub fn build(config: ExecutionConfig) -> Result<Self, ExecutionInitError> {
        Ok(Self {
            cpu: CpuExecutor::build(config.cpu_threads)?,
            blocking: TaskExecutor::build(
                "blocking",
                "gmpublished-blocking",
                config.blocking_threads,
                config.queue_capacity,
            )?,
            network: TaskExecutor::build(
                "network",
                "gmpublished-network",
                config.network_threads,
                config.queue_capacity,
            )?,
        })
    }

    #[must_use]
    pub fn cpu(&self) -> &CpuExecutor {
        &self.cpu
    }

    pub fn spawn_blocking(
        &self,
        name: impl Into<Arc<str>>,
        job: impl FnOnce() + Send + 'static,
    ) -> Result<(), ExecutionScheduleError> {
        self.blocking.spawn(name.into(), job)
    }

    pub fn spawn_network(
        &self,
        name: impl Into<Arc<str>>,
        job: impl FnOnce() + Send + 'static,
    ) -> Result<(), ExecutionScheduleError> {
        self.network.spawn(name.into(), job)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionInitError {
    #[error("failed to build CPU executor: {message}")]
    Cpu { message: String },
    #[error("failed to start {executor} executor thread {thread_name}: {source}")]
    Worker {
        executor: &'static str,
        thread_name: String,
        source: std::io::Error,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExecutionScheduleError {
    #[error("{executor} executor queue is full while scheduling {job}")]
    Full {
        executor: &'static str,
        job: Arc<str>,
    },
    #[error("{executor} executor is stopped while scheduling {job}")]
    Stopped {
        executor: &'static str,
        job: Arc<str>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Barrier, time::Instant};

    #[test]
    fn machine_config_assigns_each_category_an_independent_positive_budget() {
        let config = ExecutionConfig::for_machine();
        assert!((1..=MAX_CPU_THREADS).contains(&config.cpu_threads));
        assert!((2..=MAX_BLOCKING_THREADS).contains(&config.blocking_threads));
        assert_eq!(config.network_threads, NETWORK_THREADS);
        assert!(config.queue_capacity > 0);
    }

    #[test]
    fn cpu_executor_uses_the_named_owned_pool() {
        let resources = ExecutionResources::build(ExecutionConfig {
            cpu_threads: 2,
            blocking_threads: 1,
            network_threads: 1,
            queue_capacity: 1,
        })
        .expect("execution resources");

        let name = resources
            .cpu()
            .install(|| std::thread::current().name().map(str::to_owned));
        assert!(name.is_some_and(|name| name.starts_with("gmpublished-cpu-")));
    }

    #[test]
    fn blocking_executor_applies_its_queue_bound() {
        let resources = ExecutionResources::build(ExecutionConfig {
            cpu_threads: 1,
            blocking_threads: 1,
            network_threads: 1,
            queue_capacity: 1,
        })
        .expect("execution resources");
        let (release_tx, release_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();

        resources
            .spawn_blocking("running", move || {
                started_tx.send(()).expect("started signal");
                release_rx.recv().expect("release signal");
            })
            .expect("running job");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker started");
        resources
            .spawn_blocking("queued", || {})
            .expect("one queued job fits");
        assert!(matches!(
            resources.spawn_blocking("overflow", || {}),
            Err(ExecutionScheduleError::Full { .. })
        ));

        release_tx.send(()).expect("release worker");
    }

    #[test]
    fn dropping_resources_does_not_wait_indefinitely_for_an_active_job() {
        let resources = ExecutionResources::build(ExecutionConfig {
            cpu_threads: 1,
            blocking_threads: 1,
            network_threads: 1,
            queue_capacity: 1,
        })
        .expect("execution resources");
        let running = Arc::new(Barrier::new(2));
        let job_running = Arc::clone(&running);
        resources
            .spawn_blocking("slow job", move || {
                job_running.wait();
                std::thread::sleep(SHUTDOWN_JOIN_TIMEOUT * 6);
            })
            .expect("schedule slow job");
        running.wait();

        let before = Instant::now();
        drop(resources);
        let elapsed = before.elapsed();

        assert!(
            elapsed < SHUTDOWN_JOIN_TIMEOUT * 4,
            "resource drop blocked for {elapsed:?}"
        );
    }
}
