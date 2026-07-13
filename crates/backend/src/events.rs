use std::path::PathBuf;

use steamworks::PublishedFileId;

use crate::appdata::AppDataSnapshot;
pub use crate::transactions::{TransactionError, TransactionPayload};

#[derive(Clone, Debug, PartialEq)]
pub enum BackendEvent {
    SteamConnected,
    SteamDisconnected,
    // Boxed: `BackendEvent` moves through a `dyn BackendEventSink` vtable call
    // on every emit, so an unboxed ~500-byte `AppDataSnapshot` here would tax
    // every variant (including fieldless ones fired in hot loops, e.g.
    // `TransactionEvent::IncrProgress`) with its size. The downstream
    // `BackendRuntimeEvent` (crates/app/src/backend/tasks/runtime_events.rs)
    // already boxes the same payload for the same reason; boxing here lets
    // that conversion become a plain move instead of allocating again.
    AppDataUpdated(Box<AppDataSnapshot>),
    InstalledAddonsRefreshed,
    Transaction(TransactionEvent),
    DownloadStarted(DownloadStartedEvent),
    ExtractionStarted(ExtractionStartedEvent),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadStartedEvent {
    pub transaction_id: u32,
    pub request_id: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractionStartedEvent {
    pub transaction_id: u32,
    pub source_path: Option<PathBuf>,
    pub file_name: Option<String>,
    pub workshop_id: Option<PublishedFileId>,
    pub request_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransactionEvent {
    Finished {
        id: u32,
        payload: TransactionPayload,
    },
    Error {
        id: u32,
        error: TransactionError,
    },
    Data {
        id: u32,
        payload: TransactionPayload,
    },
    Status {
        id: u32,
        status: String,
    },
    Progress {
        id: u32,
        progress: u16,
    },
    IncrProgress {
        id: u32,
        incr: u16,
    },
    ResetProgress {
        id: u32,
    },
}

/// Delivery boundary for [`BackendEvent`]s. `Backend` holds exactly one
/// `Arc<dyn BackendEventSink>` (a no-op [`NullEventSink`] when the caller
/// supplies none), shared by every service that emits events. There is no
/// process-global sink: each `Backend` owns its own.
pub trait BackendEventSink: Send + Sync + 'static {
    fn emit(&self, event: BackendEvent);
}

impl<F> BackendEventSink for F
where
    F: Fn(BackendEvent) + Send + Sync + 'static,
{
    fn emit(&self, event: BackendEvent) {
        self(event);
    }
}

/// Default sink for a `Backend` built without an explicit one (tests, and
/// the CLI-only extraction path, which delivers no events to any UI).
#[derive(Debug, Default)]
pub struct NullEventSink;

impl BackendEventSink for NullEventSink {
    fn emit(&self, _event: BackendEvent) {}
}

/// A `BackendEventSink` that records every event it receives, in order.
/// Not test-gated: downstream crates (the app's own test suite) use it as a
/// generic testing utility too, not just this crate's unit tests.
#[derive(Clone, Default)]
pub struct BackendEventCollector {
    events: std::sync::Arc<parking_lot::Mutex<Vec<BackendEvent>>>,
}

impl BackendEventCollector {
    #[must_use]
    pub fn snapshot(&self) -> Vec<BackendEvent> {
        self.events.lock().clone()
    }

    pub fn drain(&self) -> Vec<BackendEvent> {
        std::mem::take(&mut *self.events.lock())
    }
}

impl BackendEventSink for BackendEventCollector {
    fn emit(&self, event: BackendEvent) {
        self.events.lock().push(event);
    }
}

#[cfg(test)]
mod tests {
    use crate::appdata::{AppDataPathsSnapshot, AppDataSnapshot, Settings};

    use super::{BackendEvent, BackendEventCollector, BackendEventSink};

    fn appdata_snapshot_event_payload_for_test() -> AppDataSnapshot {
        let root = std::env::temp_dir().join("gmpublished-backend-event-test");
        AppDataSnapshot {
            settings: Settings::default(),
            version: "test",
            open_count: 0,
            paths: AppDataPathsSnapshot {
                settings_file: root.join("settings.json"),
                default_user_data_dir: root.join("default-user-data"),
                default_temp_dir: root.join("default-temp"),
                default_downloads_dir: Some(root.join("default-downloads")),
                temp_dir: root.join("temp"),
                user_data_dir: root.join("user-data"),
                downloads_dir: Some(root.join("downloads")),
                gmod_dir: None,
            },
        }
    }

    #[test]
    fn collector_records_events() {
        let collector = BackendEventCollector::default();
        let snapshot = appdata_snapshot_event_payload_for_test();

        collector.emit(BackendEvent::SteamConnected);
        collector.emit(BackendEvent::AppDataUpdated(Box::new(snapshot.clone())));

        assert_eq!(
            collector.drain(),
            vec![
                BackendEvent::SteamConnected,
                BackendEvent::AppDataUpdated(Box::new(snapshot))
            ]
        );
        assert!(collector.snapshot().is_empty());
    }
}
