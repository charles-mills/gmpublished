//! Channel bridging between worker threads and async channels: blocking
//! sends belong here, never in per-call-site retry loops.
//!
//! Everything comes through `iced::futures` rather than the `futures` crate
//! directly. They are the same crate today, but only by coincidence of
//! version: mixing the two spellings means an iced bump silently turns a
//! `Sender` and the `SinkExt` used on it into different types.

use iced::futures::channel::mpsc;
use iced::futures::executor::block_on;
use iced::futures::future;

/// Blocks the calling (non-async) thread until the item is accepted,
/// applying real backpressure instead of polling. Returns `false` when the
/// receiver has disconnected.
pub fn send_blocking<T>(sender: &mut mpsc::Sender<T>, item: T) -> bool {
    send_blocking_recoverable(sender, item).is_ok()
}

/// Blocks like [`send_blocking`], but hands the item back on disconnect
/// instead of dropping it, so the caller can redeliver it through a
/// replacement channel. `SinkExt::send` consumes the item on failure, which
/// is fine for fire-and-forget sends but would lose one event at every
/// receiver handover.
pub fn send_blocking_recoverable<T>(sender: &mut mpsc::Sender<T>, mut item: T) -> Result<(), T> {
    loop {
        if block_on(future::poll_fn(|context| sender.poll_ready(context))).is_err() {
            return Err(item);
        }

        match sender.try_send(item) {
            Ok(()) => return Ok(()),
            // `poll_ready` reserved capacity for this sender, so a full
            // queue here means the reservation raced away; wait again
            // rather than treating it as fatal.
            Err(error) if error.is_full() => item = error.into_inner(),
            Err(error) => return Err(error.into_inner()),
        }
    }
}
