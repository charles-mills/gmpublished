use std::fmt;
use std::path::PathBuf;

use crate::WorkshopId;

use crate::appdata::AppDataSnapshot;
pub(crate) use crate::transactions::{
    TransactionError, TransactionId, TransactionPayload, TransactionStatus,
};

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

#[derive(Debug, Default)]
pub struct NullEventSink;

impl BackendEventSink for NullEventSink {
    fn emit(&self, _event: BackendEvent) {}
}

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
