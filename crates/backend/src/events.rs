use std::fmt;
use std::path::PathBuf;

use crate::WorkshopId;

use crate::appdata::AppDataSnapshot;
pub(crate) use crate::transactions::{
    TransactionError, TransactionId, TransactionPayload, TransactionStatus,
};

/// Correlates the download/extraction pair used to stage one Workshop item
/// for publishing. It is deliberately distinct from transaction ids and
/// Workshop ids even though all three happen to fit in `u64`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkshopSnapshotId(u64);

impl WorkshopSnapshotId {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for WorkshopSnapshotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

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
    pub transaction_id: TransactionId,
    pub request_id: Option<WorkshopSnapshotId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractionStartedEvent {
    pub transaction_id: TransactionId,
    pub source_path: Option<PathBuf>,
    pub file_name: Option<String>,
    pub workshop_id: Option<WorkshopId>,
    pub request_id: Option<WorkshopSnapshotId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionEvent {
    Finished {
        id: TransactionId,
        payload: TransactionPayload,
    },
    Error {
        id: TransactionId,
        error: TransactionError,
    },
    Data {
        id: TransactionId,
        payload: TransactionPayload,
    },
    Status {
        id: TransactionId,
        status: TransactionStatus,
    },
    Progress {
        id: TransactionId,
        progress: u16,
    },
    IncrProgress {
        id: TransactionId,
        incr: u16,
    },
    ResetProgress {
        id: TransactionId,
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
///
/// Behind `test-support` rather than `cfg(test)`, because the app crate's own
/// test suite uses it too — a `cfg(test)` item is invisible to a downstream
/// crate, and this needs to reach one.
#[cfg(feature = "test-support")]
#[derive(Clone, Default)]
pub struct BackendEventCollector {
    events: std::sync::Arc<parking_lot::Mutex<Vec<BackendEvent>>>,
}

#[cfg(feature = "test-support")]
impl BackendEventCollector {
    #[must_use]
    pub fn snapshot(&self) -> Vec<BackendEvent> {
        self.events.lock().clone()
    }

    pub fn drain(&self) -> Vec<BackendEvent> {
        std::mem::take(&mut *self.events.lock())
    }
}

#[cfg(feature = "test-support")]
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
            settings_revision: 0,
            version: "test",
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
