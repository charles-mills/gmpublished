use crate::preparation::{Cancelled, PreparationCancellation};
use parking_lot::{Condvar, Mutex};
use std::sync::Arc;

#[derive(Debug, Default)]
struct State {
    paused: bool,
    entered: usize,
}

#[derive(Debug, Default)]
pub struct PageReadGate {
    state: Mutex<State>,
    changed: Condvar,
}
impl PageReadGate {
    pub(super) fn pause(self: &Arc<Self>) -> PageReadPause {
        let mut state = self.state.lock();
        assert!(!state.paused, "page reads already paused");
        state.paused = true;
        let entered = state.entered;
        drop(state);
        PageReadPause {
            gate: Arc::clone(self),
            entered,
        }
    }
    pub(super) fn wait(
        self: &Arc<Self>,
        cancellation: &PreparationCancellation,
    ) -> Result<(), Cancelled> {
        let gate = Arc::clone(self);
        let registration = cancellation.register(move || {
            let state = gate.state.lock();
            gate.changed.notify_all();
            drop(state);
        });
        let mut state = self.state.lock();
        if state.paused {
            state.entered += 1;
            self.changed.notify_all();
        }
        while state.paused {
            cancellation.check()?;
            self.changed.wait(&mut state);
        }
        drop(state);
        drop(registration);
        cancellation.check()
    }
}

#[derive(Debug)]
pub struct PageReadPause {
    gate: Arc<PageReadGate>,
    entered: usize,
}
impl PageReadPause {
    pub(crate) fn has_waiter(&self) -> bool {
        self.gate.state.lock().entered > self.entered
    }
}
impl Drop for PageReadPause {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock();
        state.paused = false;
        drop(state);
        self.gate.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn await_waiter(pause: &PageReadPause) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pause.has_waiter() {
            assert!(Instant::now() < deadline, "worker never entered gate");
            std::thread::yield_now();
        }
    }

    #[test]
    fn dropping_pause_releases_waiter() {
        let gate = Arc::new(PageReadGate::default());
        let pause = gate.pause();
        let worker = std::thread::spawn(move || gate.wait(&PreparationCancellation::default()));
        await_waiter(&pause);
        drop(pause);
        assert_eq!(worker.join().unwrap(), Ok(()));
    }

    #[test]
    fn cancellation_releases_waiter_without_releasing_pause() {
        let gate = Arc::new(PageReadGate::default());
        let pause = gate.pause();
        let cancellation = PreparationCancellation::default();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker =
            std::thread::spawn(move || sender.send(gate.wait(&worker_cancellation)).unwrap());
        await_waiter(&pause);
        cancellation.cancel();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            Err(Cancelled)
        );
        worker.join().unwrap();
        drop(pause);
    }
}
