use super::{texture_upload::RetirementSignal, wgpu};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

mod threads;

const POLL_INTERVAL: Duration = Duration::from_millis(2);
const FAILED_POLL_INTERVAL: Duration = Duration::from_millis(250);
const PROGRESS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, thiserror::Error)]
pub(in crate::features::file_preview::viewer3d) enum CompletionFailure {
    #[error("GPU completion made no progress for 30 seconds")]
    Timeout,
    #[error(
        "GPU completion poll used submission {requested}, last successful submission {last_successful}"
    )]
    WrongSubmission {
        requested: u64,
        last_successful: u64,
    },
}
impl From<wgpu::PollError> for CompletionFailure {
    fn from(error: wgpu::PollError) -> Self {
        match error {
            wgpu::PollError::Timeout => Self::Timeout,
            wgpu::PollError::WrongSubmissionIndex(requested, last_successful) => {
                Self::WrongSubmission {
                    requested,
                    last_successful,
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct Control {
    pending: u64,
    completed: u64,
    last_progress: Option<Instant>,
    closed: bool,
    failure: Option<CompletionFailure>,
}
impl Control {
    fn start(&mut self, now: Instant) {
        if self.pending == 0 {
            self.last_progress = Some(now);
        }
        self.pending = self
            .pending
            .checked_add(1)
            .expect("GPU completion count exhausted");
    }
    fn complete(&mut self, now: Instant) {
        self.pending = self
            .pending
            .checked_sub(1)
            .expect("registered GPU completion");
        self.completed = self
            .completed
            .checked_add(1)
            .expect("GPU completion epoch exhausted");
        self.last_progress = (self.pending != 0).then_some(now);
    }
    fn stalled(&self, now: Instant) -> bool {
        self.last_progress
            .is_some_and(|last| now.saturating_duration_since(last) >= PROGRESS_TIMEOUT)
    }
}

#[derive(Debug, Default)]
struct Signal {
    control: Mutex<Control>,
    changed: Condvar,
    retirements: Mutex<Vec<Weak<RetirementSignal>>>,
}
impl Signal {
    fn complete(&self) {
        self.control
            .lock()
            .expect("GPU completion control")
            .complete(Instant::now());
        self.changed.notify_all();
    }
    fn fail(&self, failure: CompletionFailure) {
        {
            let mut control = self.control.lock().expect("GPU completion control");
            if control.failure.is_some() {
                return;
            }
            control.failure = Some(failure);
        }
        let signals: Vec<_> = self
            .retirements
            .lock()
            .expect("GPU retirement observers")
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for signal in signals {
            signal.fail(failure);
        }
        self.changed.notify_all();
    }
}

#[cfg(any(test, feature = "map-capture"))]
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub(super) struct CompletionStats {
    pending: u64,
    completed: u64,
    failure: Option<CompletionFailure>,
}

#[derive(Debug)]
pub(super) struct CompletionDriver {
    device: wgpu::Device,
    signal: Arc<Signal>,
}
impl CompletionDriver {
    pub fn shared(device: &wgpu::Device) -> Result<Arc<Self>, std::io::Error> {
        let registry = registry();
        let mut state = registry.lock().expect("GPU completion drivers");
        let finished = state.take_finished();
        state.drivers.retain(|driver| driver.strong_count() != 0);
        // Upgrades can become final owners; drop them after unlocking on all
        // paths, including a spawn failure.
        let keep_alive: Vec<_> = state.drivers.iter().filter_map(Weak::upgrade).collect();
        let result = (|| {
            if state.sealed {
                return Err(std::io::Error::other("GPU completion is shutting down"));
            }
            if let Some(driver) = keep_alive.iter().find(|driver| driver.device == *device) {
                return Ok(Arc::clone(driver));
            }
            let signal = Arc::new(Signal::default());
            let worker_signal = Arc::clone(&signal);
            let worker_device = device.clone();
            let join = std::thread::Builder::new()
                .name("gpu-completion".to_owned())
                .spawn(move || {
                    run(&worker_signal, || {
                        worker_device
                            .poll(wgpu::PollType::Poll)
                            .map(|_| ())
                            .map_err(Into::into)
                    });
                })?;
            let driver = Arc::new(Self {
                device: device.clone(),
                signal: Arc::clone(&signal),
            });
            state.drivers.push(Arc::downgrade(&driver));
            state.workers.push(threads::Worker::new(
                device.clone(),
                Arc::downgrade(&signal),
                join,
            ));
            Ok(driver)
        })();
        drop(state);
        drop(keep_alive);
        threads::join(finished).map_err(std::io::Error::other)?;
        result
    }
    pub fn register_retirement(&self, retirement: &Arc<RetirementSignal>) {
        let failure = {
            let control = self.signal.control.lock().expect("GPU completion control");
            let mut signals = self
                .signal
                .retirements
                .lock()
                .expect("GPU retirement observers");
            signals.retain(|signal| signal.strong_count() != 0);
            signals.push(Arc::downgrade(retirement));
            drop(signals);
            control.failure
        };
        if let Some(failure) = failure {
            retirement.fail(failure);
        }
    }
    pub fn check(&self) -> Result<(), CompletionFailure> {
        self.signal
            .control
            .lock()
            .expect("GPU completion control")
            .failure
            .map_or(Ok(()), Err)
    }
    pub fn on_done(
        self: &Arc<Self>,
        queue: &wgpu::Queue,
        completed: impl FnOnce() + Send + 'static,
    ) {
        self.signal
            .control
            .lock()
            .expect("GPU completion control")
            .start(Instant::now());
        let signal = Arc::clone(&self.signal);
        queue.on_submitted_work_done(move || {
            completed();
            signal.complete();
        });
        self.signal.changed.notify_all();
    }
    #[cfg(any(test, feature = "map-capture"))]
    pub fn stats(&self) -> CompletionStats {
        let control = self.signal.control.lock().expect("GPU completion control");
        CompletionStats {
            pending: control.pending,
            completed: control.completed,
            failure: control.failure,
        }
    }
}
impl Drop for CompletionDriver {
    fn drop(&mut self) {
        self.signal
            .control
            .lock()
            .expect("GPU completion control")
            .closed = true;
        self.signal.changed.notify_all();
    }
}

fn run(signal: &Signal, mut poll: impl FnMut() -> Result<(), CompletionFailure>) {
    loop {
        let mut control = signal.control.lock().expect("GPU completion control");
        while control.pending == 0 && !control.closed {
            control = signal.changed.wait(control).expect("GPU completion wake");
        }
        if control.pending == 0 {
            return;
        }
        drop(control);
        if let Err(error) = poll() {
            signal.fail(error);
        }
        let control = signal.control.lock().expect("GPU completion control");
        if control.failure.is_none() && control.stalled(Instant::now()) {
            drop(control);
            signal.fail(CompletionFailure::Timeout);
            continue;
        }
        if control.pending != 0 {
            let interval = if control.failure.is_some() {
                FAILED_POLL_INTERVAL
            } else {
                POLL_INTERVAL
            };
            drop(
                signal
                    .changed
                    .wait_timeout(control, interval)
                    .expect("GPU completion wake"),
            );
        }
    }
}

type ThreadRegistry = threads::Registry<wgpu::Device>;
pub use threads::ShutdownError;

fn registry() -> &'static Mutex<ThreadRegistry> {
    static DRIVERS: OnceLock<Mutex<ThreadRegistry>> = OnceLock::new();
    DRIVERS.get_or_init(Default::default)
}

/// Call after the application scope has dropped all render owners and stopped
/// registering workers. Joining must finish before native process finalizers.
pub fn finish_application_shutdown() -> Result<(), ShutdownError> {
    threads::shutdown(registry(), None)
}

#[cfg(test)]
pub(in crate::features::file_preview::viewer3d) struct TestDeviceScope {
    device: wgpu::Device,
}
#[cfg(test)]
impl TestDeviceScope {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            device: device.clone(),
        }
    }
}
#[cfg(test)]
impl Drop for TestDeviceScope {
    fn drop(&mut self) {
        if let Err(error) = threads::shutdown(registry(), Some(&self.device)) {
            if std::thread::panicking() {
                eprintln!("GPU fixture shutdown also failed: {error}");
            } else {
                panic!("GPU fixture shutdown failed: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_completion_progress_extends_a_busy_timeout() {
        let start = Instant::now();
        let mut control = Control::default();
        control.start(start);
        control.start(start + Duration::from_secs(29));
        assert!(control.stalled(start + Duration::from_secs(30)));
        control.complete(start + Duration::from_secs(29));
        assert!(!control.stalled(start + Duration::from_secs(30)));
        assert!(control.stalled(start + Duration::from_secs(59)));
        control.complete(start + Duration::from_secs(60));
        assert!(!control.stalled(start + Duration::from_secs(100)));
    }
    #[test]
    fn timeout_keeps_draining_nested_releases_before_closed_worker_exits() {
        let signal = Signal::default();
        signal.control.lock().unwrap().start(Instant::now());
        let mut polls = 0;
        run(&signal, || {
            polls += 1;
            match polls {
                1 => {
                    let mut control = signal.control.lock().unwrap();
                    control.last_progress = Some(Instant::now() - PROGRESS_TIMEOUT);
                    control.closed = true;
                }
                2 => {
                    assert_eq!(
                        signal.control.lock().unwrap().failure,
                        Some(CompletionFailure::Timeout)
                    );
                    signal.control.lock().unwrap().start(Instant::now());
                    signal.complete();
                    signal.fail(CompletionFailure::WrongSubmission {
                        requested: 2,
                        last_successful: 1,
                    });
                }
                3 => signal.complete(),
                _ => panic!("completion worker did not finish"),
            }
            Ok(())
        });
        let control = signal.control.lock().unwrap();
        assert_eq!(polls, 3);
        assert_eq!(control.pending, 0);
        assert_eq!(control.completed, 2);
        assert_eq!(control.failure, Some(CompletionFailure::Timeout));
        drop(control);
    }

    #[test]
    fn worker_drains_nested_completion_and_closes_without_another_render() {
        let signal = Arc::new(Signal::default());
        signal.control.lock().unwrap().start(Instant::now());
        let worker_signal = Arc::clone(&signal);
        let (finished, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut polls = 0;
            run(&worker_signal, || {
                polls += 1;
                if polls == 1 {
                    worker_signal.control.lock().unwrap().start(Instant::now());
                }
                worker_signal.complete();
                Ok(())
            });
            finished.send(polls).unwrap();
        });
        let mut control = signal.control.lock().unwrap();
        while control.pending != 0 {
            let (next, timeout) = signal
                .changed
                .wait_timeout(control, Duration::from_secs(5))
                .unwrap();
            assert!(!timeout.timed_out());
            control = next;
        }
        assert_eq!(control.completed, 2);
        control.closed = true;
        drop(control);
        signal.changed.notify_all();
        assert_eq!(receiver.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
        worker.join().unwrap();
    }
}
