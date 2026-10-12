use std::time::Duration;

use parking_lot::{Condvar, Mutex};

#[derive(Debug, Default)]
pub(crate) struct Signal {
    state: Mutex<bool>,
    changed: Condvar,
}

impl Signal {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set(&self) {
        self.store(true);
    }

    pub(crate) fn store(&self, value: bool) {
        {
            let mut state = self.state.lock();
            *state = value;
        }
        self.changed.notify_all();
    }

    #[must_use]
    pub(crate) fn is_set(&self) -> bool {
        *self.state.lock()
    }

    /// Wait until set or the timeout expires. Pass the guard to wait_while_for to avoid
    /// lost wakeups.
    #[expect(clippy::significant_drop_tightening)]
    pub(crate) fn wait_until_set(&self, timeout: Duration) -> bool {
        let mut state = self.state.lock();
        if *state {
            return true;
        }
        !self
            .changed
            .wait_while_for(&mut state, |state| !*state, timeout)
            .timed_out()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn an_already_set_signal_does_not_wait() {
        let signal = Signal::new();
        signal.set();

        assert!(signal.wait_until_set(Duration::from_millis(1)));
    }

    #[test]
    fn a_waiter_wakes_when_the_signal_is_set() {
        let signal = Arc::new(Signal::new());
        let waiter_signal = Arc::clone(&signal);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let mut state = waiter_signal.state.lock();
            ready_tx.send(()).expect("waiter holds the signal lock");
            !waiter_signal
                .changed
                .wait_while_for(&mut state, |state| !*state, Duration::from_secs(5))
                .timed_out()
        });
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("waiter ready");
        signal.set();
        assert!(waiter.join().expect("waiter thread"));
    }

    #[test]
    fn waiting_on_a_signal_that_is_never_set_times_out() {
        let signal = Signal::new();

        assert!(!signal.wait_until_set(Duration::from_millis(20)));
        assert!(!signal.is_set());
    }

    #[test]
    fn storing_false_clears_a_set_signal() {
        let signal = Signal::new();
        signal.set();
        signal.store(false);

        assert!(!signal.is_set());
    }
}
