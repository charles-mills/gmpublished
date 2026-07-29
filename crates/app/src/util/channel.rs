//! Channel bridging between worker threads and async channels: blocking
//! sends belong here, never in per-call-site retry loops.
//!
//! Everything comes through `iced::futures` rather than the `futures` crate
//! directly. They are the same crate today, but only by coincidence of
//! version: mixing the two spellings means an iced bump silently turns a
//! `Sender` and the `SinkExt` used on it into different types.

use iced::futures::SinkExt;
use iced::futures::channel::mpsc;
use iced::futures::executor::block_on;

/// Blocks the calling (non-async) thread until the item is accepted,
/// applying real backpressure instead of polling. Returns `false` when the
/// receiver has disconnected.
pub fn send_blocking<T>(sender: &mut mpsc::Sender<T>, item: T) -> bool {
    block_on(sender.send(item)).is_ok()
}
