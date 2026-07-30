//! Bounded joining for background threads at shutdown.

use std::{
    sync::mpsc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// Joins every handle concurrently, giving up once `timeout` has elapsed
/// overall and logging what was left under `context`.
///
/// The bound is shared across every thread rather than applied per-thread:
/// the joins run concurrently, so one thread that refuses to stop costs
/// `timeout` in total, not `timeout` each.
///
/// Threads still running past the bound are detached, not killed — their joins
/// still complete, on the helper threads spawned here. Giving up only stops a
/// thread that ignored its shutdown signal from blocking the caller, which is
/// process exit often enough that an unbounded join is never the right default.
pub(crate) fn join_all_within(
    handles: Vec<JoinHandle<()>>,
    timeout: Duration,
    context: &'static str,
) {
    let expected = handles.len();
    if expected == 0 {
        return;
    }
    let deadline = Instant::now() + timeout;

    let (done_tx, done_rx) = mpsc::channel();
    let mut awaiting = 0;
    for (index, handle) in handles.into_iter().enumerate() {
        let done_tx = done_tx.clone();
        let thread_name = format!("gmpublished-join-{index}");
        match std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                if handle.join().is_err() {
                    log::error!("[{context}] a background thread panicked during shutdown");
                }
                let _ = done_tx.send(());
            }) {
            Ok(_) => awaiting += 1,
            Err(error) => {
                // Dropping the captured join handle detaches the worker. A
                // failed helper must not turn shutdown into an unbounded join.
                log::warn!("[{context}] could not start bounded-join helper: {error}");
            }
        }
    }
    // Otherwise the receiver below never sees a disconnect.
    drop(done_tx);

    let mut exited = 0;
    while exited < awaiting {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        if done_rx.recv_timeout(remaining).is_err() {
            break;
        }
        exited += 1;
    }

    if exited < awaiting {
        log::warn!(
            "[{context}] {} background thread(s) did not exit within {timeout:?} of shutdown; detaching them",
            awaiting - exited
        );
    }
}
