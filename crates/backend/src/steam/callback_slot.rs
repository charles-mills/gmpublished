use parking_lot::{Mutex, MutexGuard};

/// Serialises use of steamworks' single callback slot for one event type.
///
/// steamworks keeps one registration per event type: a later
/// `register_callback` replaces the current one, and dropping any handle
/// clears the slot. Overlapping waits clobber each other and both ride out
/// their full timeout.
#[derive(Debug, Default)]
pub(crate) struct CallbackSlot {
    occupied: Mutex<()>,
}

impl CallbackSlot {
    /// Waits until the slot is free, then holds it for the guard's lifetime.
    pub(crate) fn claim(&self) -> MutexGuard<'_, ()> {
        self.occupied.lock()
    }
}
