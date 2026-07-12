//! Channel bridging between worker threads and async channels: blocking
//! sends belong here, never in per-call-site retry loops.

use iced::futures::SinkExt;
use iced::futures::channel::mpsc;

/// Blocks the calling (non-async) thread until the item is accepted,
/// applying real backpressure instead of polling. Returns `false` when the
/// receiver has disconnected.
pub fn send_blocking<T>(sender: &mut mpsc::Sender<T>, item: T) -> bool {
    futures::executor::block_on(sender.send(item)).is_ok()
}
