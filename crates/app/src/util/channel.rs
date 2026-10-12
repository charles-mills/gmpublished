use iced::futures::channel::mpsc;
use iced::futures::executor::block_on;
use iced::futures::future;

/// Blocks the calling (non-async) thread until the item is accepted,
/// applying real backpressure instead of polling. Returns `false` when the
/// receiver has disconnected.
pub fn send_blocking<T>(sender: &mut mpsc::Sender<T>, item: T) -> bool {
    send_blocking_recoverable(sender, item).is_ok()
}

/// Returns the item on disconnect so it can be sent through a replacement
/// channel. `SinkExt::send` consumes it even on failure.
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
