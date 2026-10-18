use super::{CompletionDriver, Signal};
use std::sync::{Mutex, Weak};
use std::thread::JoinHandle;

pub(super) struct Worker<D> {
    pub device: D,
    signal: Weak<Signal>,
    join: JoinHandle<()>,
}
impl<D> Worker<D> {
    pub fn new(device: D, signal: Weak<Signal>, join: JoinHandle<()>) -> Self {
        Self {
            device,
            signal,
            join,
        }
    }
}

pub(super) struct Registry<D> {
    pub drivers: Vec<Weak<CompletionDriver>>,
    pub workers: Vec<Worker<D>>,
    pub sealed: bool,
}
impl<D> Default for Registry<D> {
    fn default() -> Self {
        Self {
            drivers: Vec::new(),
            workers: Vec::new(),
            sealed: false,
        }
    }
}
impl<D> Registry<D> {
    pub fn take_finished(&mut self) -> Vec<Worker<D>> {
        let mut finished = Vec::new();
        let mut index = 0;
        while index < self.workers.len() {
            if self.workers[index].join.is_finished() {
                finished.push(self.workers.swap_remove(index));
            } else {
                index += 1;
            }
        }
        finished
    }
}
impl<D: Eq> Registry<D> {
    fn take(&mut self, device: Option<&D>) -> Vec<Worker<D>> {
        if device.is_none() {
            self.sealed = true;
        }
        let mut workers = Vec::new();
        let mut index = 0;
        while index < self.workers.len() {
            if device.is_none_or(|device| self.workers[index].device == *device) {
                workers.push(self.workers.swap_remove(index));
            } else {
                index += 1;
            }
        }
        workers
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{panicked} GPU completion worker(s) panicked")]
pub struct ShutdownError {
    panicked: usize,
}

pub(super) fn join<D>(workers: Vec<Worker<D>>) -> Result<(), ShutdownError> {
    let mut panicked = 0;
    for Worker { device, join, .. } in workers {
        panicked += usize::from(join.join().is_err());
        // Keep the exact device identity alive through its worker's join.
        drop(device);
    }
    if panicked == 0 {
        Ok(())
    } else {
        Err(ShutdownError { panicked })
    }
}

/// Call only after the selected device's render owners and registration callers
/// have stopped. No registry, completion-control, or uploader lock spans a join.
/// Device scopes leave other fixtures active; None seals application shutdown.
pub(super) fn shutdown<D: Eq>(
    registry: &Mutex<Registry<D>>,
    device: Option<&D>,
) -> Result<(), ShutdownError> {
    let workers = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take(device);
    // A GUI unwind can leave ManuallyDrop render owners alive. Registration and
    // submission have stopped, so idle workers may close without waiting for the
    // last driver Arc; run() still drains pending and nested callbacks.
    for worker in &workers {
        if let Some(signal) = worker.signal.upgrade() {
            signal
                .control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .closed = true;
            signal.changed.notify_all();
        }
    }
    join(workers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    type Task = Box<dyn FnOnce() + Send>;
    struct Native {
        tasks: Mutex<VecDeque<Task>>,
        destroyed: mpsc::Sender<std::thread::ThreadId>,
    }
    impl Drop for Native {
        fn drop(&mut self) {
            let _ = self.destroyed.send(std::thread::current().id());
        }
    }
    #[derive(Clone)]
    struct FakeDevice(Arc<Native>);
    impl PartialEq for FakeDevice {
        fn eq(&self, other: &Self) -> bool {
            Arc::ptr_eq(&self.0, &other.0)
        }
    }
    impl Eq for FakeDevice {}
    impl FakeDevice {
        fn new() -> (Self, mpsc::Receiver<std::thread::ThreadId>) {
            let (destroyed, receiver) = mpsc::channel();
            (
                Self(Arc::new(Native {
                    tasks: Mutex::default(),
                    destroyed,
                })),
                receiver,
            )
        }
        fn poll(&self) {
            let task = self.0.tasks.lock().unwrap().pop_front();
            if let Some(task) = task {
                task();
            }
        }
        fn on_done(&self, signal: &Arc<Signal>, task: impl FnOnce() + Send + 'static) {
            signal.control.lock().unwrap().start(Instant::now());
            let completed_signal = Arc::clone(signal);
            self.0.tasks.lock().unwrap().push_back(Box::new(move || {
                task();
                completed_signal.complete();
            }));
            signal.changed.notify_all();
        }
    }
    fn add_worker(
        registry: &Arc<Mutex<Registry<FakeDevice>>>,
        device: &FakeDevice,
        signal: &Arc<Signal>,
        poll: impl FnMut() -> Result<(), super::super::CompletionFailure> + Send + 'static,
    ) {
        let worker_signal = Arc::clone(signal);
        let worker_device = device.clone();
        let (registered, ready) = mpsc::channel();
        let join = std::thread::spawn(move || {
            ready.recv().unwrap();
            let _device = worker_device;
            super::super::run(&worker_signal, poll);
        });
        registry.lock().unwrap().workers.push(Worker::new(
            device.clone(),
            Arc::downgrade(signal),
            join,
        ));
        registered.send(()).unwrap();
    }
    fn close(signal: &Signal) {
        signal.control.lock().unwrap().closed = true;
        signal.changed.notify_all();
    }

    #[test]
    fn outer_boundary_joins_idle_worker_before_final_native_owner_release() {
        let registry = Arc::new(Mutex::new(Registry::default()));
        let (device, destroyed) = FakeDevice::new();
        let signal = Arc::new(Signal::default());
        add_worker(&registry, &device, &signal, || panic!("idle poll"));
        close(&signal);
        let workers = registry.lock().unwrap().take(Some(&device));
        drop(device);
        join(workers).unwrap();
        assert_eq!(destroyed.try_recv().unwrap(), std::thread::current().id());
        assert!(registry.lock().unwrap().workers.is_empty());
    }

    #[test]
    fn worker_final_owner_drop_drains_nested_callback_before_boundary_returns() {
        struct LastOwner {
            device: FakeDevice,
            signal: Arc<Signal>,
            done: mpsc::Sender<()>,
        }
        impl Drop for LastOwner {
            fn drop(&mut self) {
                close(&self.signal);
                let done = self.done.clone();
                self.device
                    .on_done(&self.signal, move || done.send(()).unwrap());
            }
        }
        let registry = Arc::new(Mutex::new(Registry::default()));
        let (device, destroyed) = FakeDevice::new();
        let signal = Arc::new(Signal::default());
        let (done, received) = mpsc::channel();
        let owner = Arc::new(LastOwner {
            device: device.clone(),
            signal: Arc::clone(&signal),
            done,
        });
        let weak_owner = Arc::downgrade(&owner);
        let callback_registry = Arc::clone(&registry);
        let callback_signal = Arc::clone(&signal);
        device.on_done(&signal, move || {
            assert!(callback_registry.try_lock().is_ok());
            assert!(callback_signal.control.try_lock().is_ok());
            drop(owner); // Equivalent to the final ImagePermit owning a driver.
        });
        let poll_device = device.clone();
        add_worker(&registry, &device, &signal, move || {
            poll_device.poll();
            Ok(())
        });
        let workers = registry.lock().unwrap().take(Some(&device));
        drop(device);
        join(workers).unwrap();
        received.try_recv().unwrap();
        assert!(weak_owner.upgrade().is_none());
        assert_eq!(signal.control.lock().unwrap().completed, 2);
        assert_eq!(signal.control.lock().unwrap().pending, 0);
        assert_eq!(destroyed.try_recv().unwrap(), std::thread::current().id());
    }

    #[test]
    fn final_boundary_closes_idle_worker_even_when_a_render_owner_survives() {
        let registry = Arc::new(Mutex::new(Registry::default()));
        let (device, _) = FakeDevice::new();
        let signal = Arc::new(Signal::default());
        add_worker(&registry, &device, &signal, || panic!("idle poll"));
        shutdown(&registry, None).unwrap();
        assert!(signal.control.lock().unwrap().closed);
        assert!(registry.lock().unwrap().workers.is_empty());
    }

    #[test]
    fn fixture_scope_joins_during_assertion_unwinding() {
        struct Scope(Arc<Mutex<Registry<FakeDevice>>>, FakeDevice);
        impl Drop for Scope {
            fn drop(&mut self) {
                shutdown(&self.0, Some(&self.1)).unwrap();
            }
        }
        let registry = Arc::new(Mutex::new(Registry::default()));
        let (device, _) = FakeDevice::new();
        let signal = Arc::new(Signal::default());
        add_worker(&registry, &device, &signal, || panic!("idle poll"));
        let result = std::panic::catch_unwind(|| {
            let _scope = Scope(Arc::clone(&registry), device.clone());
            panic!("injected fixture assertion");
        });
        assert!(result.is_err());
        assert!(signal.control.lock().unwrap().closed);
        assert!(registry.lock().unwrap().workers.is_empty());
    }

    #[test]
    fn device_scope_joins_exact_device_and_leaves_other_workers_registered() {
        let registry = Arc::new(Mutex::new(Registry::default()));
        let (first, _) = FakeDevice::new();
        let (second, _) = FakeDevice::new();
        let first_signal = Arc::new(Signal::default());
        let second_signal = Arc::new(Signal::default());
        add_worker(&registry, &first, &first_signal, || panic!("idle poll"));
        add_worker(&registry, &second, &second_signal, || panic!("idle poll"));
        close(&first_signal);
        shutdown(&registry, Some(&first)).unwrap();
        let state = registry.lock().unwrap();
        assert_eq!(state.workers.len(), 1);
        assert!(state.workers[0].device == second);
        assert!(!state.sealed);
        drop(state);
        close(&second_signal);
        shutdown(&registry, None).unwrap();
        assert!(registry.lock().unwrap().sealed);
    }

    #[test]
    fn panic_error_is_reported_only_after_all_workers_join_outside_registry_lock() {
        let registry = Arc::new(Mutex::new(Registry::default()));
        let (device, _) = FakeDevice::new();
        let failed = std::thread::spawn(|| panic!("injected worker panic"));
        let (arrived, waiting) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let callback_registry = Arc::clone(&registry);
        let joined = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_joined = Arc::clone(&joined);
        let healthy = std::thread::spawn(move || {
            arrived.send(()).unwrap();
            gate.recv().unwrap();
            assert!(callback_registry.try_lock().is_ok());
            callback_joined.store(true, std::sync::atomic::Ordering::Release);
        });
        registry.lock().unwrap().workers.extend([
            Worker::new(device.clone(), Weak::new(), healthy),
            Worker::new(device, Weak::new(), failed),
        ]);
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        let shutdown_registry = Arc::clone(&registry);
        let (done, result) = mpsc::channel();
        let shutdown_thread = std::thread::spawn(move || {
            done.send(shutdown(&shutdown_registry, None)).unwrap();
        });
        assert!(result.try_recv().is_err());
        release.send(()).unwrap();
        assert_eq!(
            result.recv_timeout(Duration::from_secs(2)).unwrap(),
            Err(ShutdownError { panicked: 1 })
        );
        shutdown_thread.join().unwrap();
        assert!(joined.load(std::sync::atomic::Ordering::Acquire));
        assert!(registry.lock().unwrap().workers.is_empty());
    }

    #[test]
    fn shutdown_recovers_poisoned_boundary_locks_then_reports_joined_worker_panic() {
        let registry = Arc::new(Mutex::new(Registry::default()));
        let (device, _) = FakeDevice::new();
        let signal = Arc::new(Signal::default());
        let worker_signal = Arc::clone(&signal);
        let join = std::thread::spawn(move || {
            let _control = worker_signal.control.lock().unwrap();
            panic!("injected control-lock poison");
        });
        registry
            .lock()
            .unwrap()
            .workers
            .push(Worker::new(device, Arc::downgrade(&signal), join));
        let poisoned = std::panic::catch_unwind(|| {
            let _state = registry.lock().unwrap();
            panic!("injected registry-lock poison");
        });
        assert!(poisoned.is_err());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .workers[0]
            .join
            .is_finished()
        {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(
            shutdown(&registry, None),
            Err(ShutdownError { panicked: 1 })
        );
        let state = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(state.sealed);
        assert!(state.workers.is_empty());
        drop(state);
        assert!(signal.control.lock().unwrap_err().into_inner().closed);
    }

    #[test]
    fn registration_reaps_finished_records_while_other_workers_remain_active() {
        let registry = Arc::new(Mutex::new(Registry::default()));
        let (device, destroyed) = FakeDevice::new();
        let active = Arc::new(Signal::default());
        add_worker(&registry, &device, &active, || panic!("idle poll"));
        for _ in 0..3 {
            let completed = Arc::new(Signal::default());
            close(&completed);
            add_worker(&registry, &device, &completed, || panic!("idle poll"));
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let finished = registry.lock().unwrap().take_finished();
                if !finished.is_empty() {
                    join(finished).unwrap();
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            assert_eq!(registry.lock().unwrap().workers.len(), 1);
        }
        close(&active);
        let workers = registry.lock().unwrap().take(Some(&device));
        drop(device);
        join(workers).unwrap();
        assert_eq!(destroyed.try_recv().unwrap(), std::thread::current().id());
    }
}
