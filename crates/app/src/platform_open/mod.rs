//! macOS `.gma` document-open bridge.
//!
//! Packaging registers the `.gma` file association (`CFBundleDocumentTypes` in
//! `packaging/macos/Info.extra.plist`), but macOS delivers double-clicked
//! documents through Apple Events, not `argv`. This module installs the two
//! native handlers (see [`macos`]) and forwards accepted `.gma` paths through
//! a process-global queue into the Iced runtime as a [`Subscription`].
//!
//! Paths that arrive before the subscription is live (i.e. documents that
//! launched the app) are buffered inside the queue's std `mpsc` channel and
//! drained once the subscription's forwarder thread starts.

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        LazyLock,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use iced::{Subscription, futures::channel::mpsc as iced_mpsc, stream};
use parking_lot::Mutex;

use crate::backend::gma;

pub mod macos;

struct DocumentOpenQueue {
    sender: Sender<Vec<PathBuf>>,
    receiver: Mutex<Option<Receiver<Vec<PathBuf>>>>,
}

static DOCUMENT_OPEN_QUEUE: LazyLock<DocumentOpenQueue> = LazyLock::new(|| {
    let (sender, receiver) = mpsc::channel();
    DocumentOpenQueue {
        sender,
        receiver: Mutex::new(Some(receiver)),
    }
});

/// Must be called on the main thread before `iced::application(...).run()`;
/// see [`macos::install`] for the mechanism details.
pub fn install() {
    macos::install();
}

pub fn subscription() -> Subscription<Vec<PathBuf>> {
    Subscription::run(document_open_stream)
}

fn document_open_stream() -> impl iced::futures::Stream<Item = Vec<PathBuf>> + use<> {
    stream::channel(16, async move |output| {
        let Some(receiver) = take_receiver() else {
            log::warn!("macOS document-open receiver was already taken; stream is inert");
            return;
        };

        let spawned = thread::Builder::new()
            .name("macos-document-open-drain".to_owned())
            .spawn(move || forward_document_opens(&receiver, output));
        if let Err(error) = spawned {
            log::warn!("failed to spawn macOS document-open forwarder: {error}");
        }
    })
}

fn take_receiver() -> Option<Receiver<Vec<PathBuf>>> {
    DOCUMENT_OPEN_QUEUE.receiver.lock().take()
}

fn forward_document_opens(
    receiver: &Receiver<Vec<PathBuf>>,
    mut output: iced_mpsc::Sender<Vec<PathBuf>>,
) {
    while let Ok(paths) = receiver.recv() {
        if !crate::util::channel::send_blocking(&mut output, paths) {
            return;
        }
    }
}

/// Callable from any thread, including native Apple Event callbacks. Paths
/// queued before the subscription drains are buffered, not lost.
pub fn accept_paths(paths: Vec<PathBuf>) {
    let paths = filter_open_gma_paths(paths);
    if paths.is_empty() {
        log::debug!("macOS document-open event had no valid GMA paths");
        return;
    }

    log::info!(
        "accepted macOS document-open event with {} GMA path(s)",
        paths.len()
    );
    let _send_result = DOCUMENT_OPEN_QUEUE.sender.send(paths);
}

/// Keeps unique, existing `.gma` files (case-insensitive extension match).
pub fn filter_open_gma_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| accept_open_gma_path(path, &mut seen))
        .collect()
}

fn accept_open_gma_path(path: &PathBuf, seen: &mut HashSet<PathBuf>) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        log::debug!("ignoring missing document-open path {}", path.display());
        return false;
    };
    if !metadata.is_file() {
        log::debug!("ignoring non-file document-open path {}", path.display());
        return false;
    }
    if !gma::is_gma_path(path) {
        log::debug!("ignoring non-GMA document-open path {}", path.display());
        return false;
    }
    if !seen.insert(path.clone()) {
        log::debug!("ignoring duplicate document-open path {}", path.display());
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn open_gma_filter_keeps_unique_existing_gma_files_case_insensitively() {
        let temp = tempfile::tempdir().expect("temp dir");
        let gma = temp.path().join("addon.GMA");
        let duplicate = gma.clone();
        let txt = temp.path().join("notes.txt");
        let dir = temp.path().join("folder.gma");
        let missing = temp.path().join("missing.gma");
        fs::write(&gma, b"gma").expect("gma file");
        fs::write(&txt, b"text").expect("txt file");
        fs::create_dir(&dir).expect("gma dir");

        let filtered = filter_open_gma_paths([gma.clone(), txt, dir, missing, duplicate]);

        assert_eq!(filtered, vec![gma]);
    }

    #[test]
    fn queued_paths_buffer_until_receiver_drains() {
        let temp = tempfile::tempdir().expect("temp dir");
        let gma = temp.path().join("buffered.gma");
        fs::write(&gma, b"gma").expect("gma file");

        accept_paths(vec![gma.clone()]);

        let receiver = take_receiver().expect("receiver still available");
        assert_eq!(
            receiver.recv_timeout(std::time::Duration::from_secs(1)),
            Ok(vec![gma])
        );
    }
}
