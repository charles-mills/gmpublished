use std::{
    sync::mpsc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// Joins concurrently under one shared timeout, logging unfinished threads.
/// Timed-out threads continue running; helper threads finish their joins later.
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
