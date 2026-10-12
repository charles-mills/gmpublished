use parking_lot::{Mutex, MutexGuard};

/// Steam keeps one callback per event type. Registering replaces it;
/// dropping any handle clears it, so overlapping waits would lose callbacks.
#[derive(Debug, Default)]
pub(crate) struct CallbackSlot {
    occupied: Mutex<()>,
}

impl CallbackSlot {
    pub(crate) fn claim(&self) -> MutexGuard<'_, ()> {
        self.occupied.lock()
    }
}
