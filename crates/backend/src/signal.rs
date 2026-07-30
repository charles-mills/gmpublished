//! A flag that threads can block on.

use std::time::Duration;

use parking_lot::{Condvar, Mutex};

/// A boolean with waiters woken whenever it changes.
///
/// Most users only ever set it — a shutdown latch — but [`Self::store`] also
/// clears, for state that genuinely goes back, like a Steam connection
/// dropping. The pairing of the flag and its condvar is the point: an
/// anonymous `(Mutex<bool>, Condvar)` leaves every reader to remember that the
/// notify must follow the store and must happen with the lock released, and to
/// spell the wait-while predicate correctly each time.
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

    /// Sets the flag and wakes every waiter. Idempotent.
    pub(crate) fn set(&self) {
        self.store(true);
    }

    /// Sets or clears the flag, waking every waiter either way.
    ///
    /// Clearing exists for state that can genuinely go back — a Steam
    /// connection dropping — rather than for a latch.
    pub(crate) fn store(&self, value: bool) {
        {
            let mut state = self.state.lock();
            *state = value;
        }
        // Notified after the guard above is dropped, so a woken thread does
        // not immediately block re-acquiring the lock.
        self.changed.notify_all();
    }

    #[must_use]
    pub(crate) fn is_set(&self) -> bool {
        *self.state.lock()
    }

    /// Blocks until the flag is set or `timeout` elapses, reporting whether it
    /// is set. Returns immediately if it already was.
    // The guard is handed to `wait_while_for`, so it cannot be tightened.
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
        let setter = Arc::clone(&signal);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            setter.set();
        });

        assert!(signal.wait_until_set(Duration::from_secs(5)));
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
