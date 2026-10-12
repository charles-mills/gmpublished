use gmpublished_backend::BackendEventSink;

use super::{
    PublishedFileId, TaskId, TransactionStatus, UiError, WorkshopDownloadTaskKind,
    WorkshopSnapshotId,
};
use gmpublished_backend::AppDataSnapshot as BackendAppDataSnapshot;
use gmpublished_backend::BackendEvent as BackendSinkEvent;
use gmpublished_backend::DownloadStartedEvent as BackendDownloadStartedEvent;
use gmpublished_backend::ExtractionStartedEvent as BackendExtractionStartedEvent;
use gmpublished_backend::TransactionError;
use gmpublished_backend::TransactionEvent as BackendTransactionEvent;
use gmpublished_backend::TransactionId;
use gmpublished_backend::TransactionPayload;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TrySendError;
use std::time::Duration;

#[cfg(not(test))]
pub(super) const fn install_backend_event_sink_by_default() -> bool {
    true
}

#[cfg(test)]
pub(super) const fn install_backend_event_sink_by_default() -> bool {
    false
}

pub(super) struct BackendEventSinkRegistration {
    hub: Arc<BackendEventHub>,
}

impl BackendEventSinkRegistration {
    pub(super) fn new(sender: SyncSender<BackendRuntimeEvent>) -> Self {
        Self {
            hub: Arc::new(BackendEventHub::new(sender)),
        }
    }

    pub(super) fn sink(&self) -> Arc<dyn BackendEventSink> {
        let hub = Arc::clone(&self.hub);
        Arc::new(move |event: BackendSinkEvent| {
            hub.emit(&BackendRuntimeEvent::from(event));
        })
    }

    pub(super) fn subscribe(&self) -> BackendRuntimeEventSubscription {
        self.hub.subscribe()
    }
}

impl fmt::Debug for BackendEventSinkRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackendEventSinkRegistration")
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(super) struct BackendEventHub {
    root: SyncSender<BackendRuntimeEvent>,
    subscribers: parking_lot::Mutex<Vec<mpsc::Sender<BackendRuntimeEvent>>>,
}

impl BackendEventHub {
    fn new(root: SyncSender<BackendRuntimeEvent>) -> Self {
        Self {
            root,
            subscribers: parking_lot::Mutex::new(Vec::new()),
        }
    }

    fn subscribe(&self) -> BackendRuntimeEventSubscription {
        let (sender, receiver) = mpsc::channel();
        self.subscribers.lock().push(sender);
        BackendRuntimeEventSubscription { receiver }
    }

    fn emit(&self, event: &BackendRuntimeEvent) {
        if event.must_not_drop() {
            // The forwarder always drains. Wait through backpressure rather than
            // dropping lifecycle events.
            let _disconnected = self.root.send(event.clone());
        } else {
            match self.root.try_send(event.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    log::warn!(
                        "backend event root queue is full; dropping `{}`",
                        event.event_name()
                    );
                }
                Err(TrySendError::Disconnected(_)) => {}
            }
        }

        self.subscribers
            .lock()
            .retain(|sender| sender.send(event.clone()).is_ok());
    }
}

pub struct BackendRuntimeEventSubscription {
    receiver: mpsc::Receiver<BackendRuntimeEvent>,
}

impl BackendRuntimeEventSubscription {
    pub(crate) fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<BackendRuntimeEvent, mpsc::RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BackendRuntimeEvent {
    SteamConnected,
    SteamDisconnected,
    AppDataUpdated(Box<BackendAppDataSnapshot>),
    InstalledAddonsRefreshed,
    DownloadStarted {
        transaction_id: TransactionId,
        request_id: Option<WorkshopSnapshotId>,
    },
    ExtractionStarted {
        transaction_id: TransactionId,
        source_path: Option<PathBuf>,
        file_name: Option<String>,
        workshop_id: Option<PublishedFileId>,
        request_id: Option<WorkshopSnapshotId>,
    },
    Transaction(TransactionRuntimeEvent),
}

impl BackendRuntimeEvent {
    pub(crate) fn event_name(&self) -> &'static str {
        match self {
            Self::SteamConnected => "SteamConnected",
            Self::SteamDisconnected => "SteamDisconnected",
            Self::AppDataUpdated(_) => "UpdateAppData",
            Self::InstalledAddonsRefreshed => "InstalledAddonsRefreshed",
            Self::DownloadStarted { .. } => "DownloadStarted",
            Self::ExtractionStarted { .. } => "ExtractionStarted",
            Self::Transaction(event) => event.event_name(),
        }
    }

    /// Lifecycle events must survive backpressure. Start events correlate backend
    /// transactions with app tasks; terminal events complete them. Progress may be
    /// dropped because later updates supersede it.
    pub(crate) const fn must_not_drop(&self) -> bool {
        matches!(
            self,
            Self::DownloadStarted { .. }
                | Self::ExtractionStarted { .. }
                | Self::Transaction(
                    TransactionRuntimeEvent::Finished { .. }
                        | TransactionRuntimeEvent::Error { .. }
                )
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackendRuntimeEventEffects {
    handled: bool,
    actions: Vec<BackendRuntimeAction>,
}

impl BackendRuntimeEventEffects {
    pub(super) const fn ignored() -> Self {
        Self {
            handled: false,
            actions: Vec::new(),
        }
    }

    pub(super) const fn handled() -> Self {
        Self {
            handled: true,
            actions: Vec::new(),
        }
    }

    pub(super) fn handled_with(actions: Vec<BackendRuntimeAction>) -> Self {
        Self {
            handled: true,
            actions,
        }
    }

    #[cfg(test)]
    pub(crate) const fn handled_event(&self) -> bool {
        self.handled
    }

    pub(crate) fn into_actions(self) -> Vec<BackendRuntimeAction> {
        self.actions
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendRuntimeAction {
    DownloadTaskStarted {
        kind: WorkshopDownloadTaskKind,
        item_id: PublishedFileId,
        task_id: TaskId,
    },
    DownloadFinished {
        request_id: Option<WorkshopSnapshotId>,
        item_id: PublishedFileId,
        installed_path: Option<PathBuf>,
        extracted_path: PathBuf,
    },
    SnapshotFailed {
        request_id: WorkshopSnapshotId,
        error: UiError,
    },
}

/// Ignore transaction events until a live service correlates them with an app task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionRuntimeEvent {
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

impl TransactionRuntimeEvent {
    pub(crate) const fn event_name(&self) -> &'static str {
        match self {
            Self::Finished { .. } => "TransactionFinished",
            Self::Error { .. } => "TransactionError",
            Self::Data { .. } => "TransactionData",
            Self::Status { .. } => "TransactionStatus",
            Self::Progress { .. } => "TransactionProgress",
            Self::IncrProgress { .. } => "TransactionIncrProgress",
            Self::ResetProgress { .. } => "TransactionResetProgress",
        }
    }
}

impl From<BackendSinkEvent> for BackendRuntimeEvent {
    fn from(event: BackendSinkEvent) -> Self {
        match event {
            BackendSinkEvent::SteamConnected => Self::SteamConnected,
            BackendSinkEvent::SteamDisconnected => Self::SteamDisconnected,
            BackendSinkEvent::AppDataUpdated(snapshot) => Self::AppDataUpdated(snapshot),
            BackendSinkEvent::InstalledAddonsRefreshed => Self::InstalledAddonsRefreshed,
            BackendSinkEvent::DownloadStarted(event) => event.into(),
            BackendSinkEvent::ExtractionStarted(event) => event.into(),
            BackendSinkEvent::Transaction(event) => Self::Transaction(event.into()),
        }
    }
}

impl From<BackendDownloadStartedEvent> for BackendRuntimeEvent {
    fn from(event: BackendDownloadStartedEvent) -> Self {
        Self::DownloadStarted {
            transaction_id: event.transaction_id,
            request_id: event.request_id,
        }
    }
}

impl From<BackendExtractionStartedEvent> for BackendRuntimeEvent {
    fn from(event: BackendExtractionStartedEvent) -> Self {
        Self::ExtractionStarted {
            transaction_id: event.transaction_id,
            source_path: event.source_path,
            file_name: event.file_name,
            workshop_id: event.workshop_id.map(PublishedFileId::from),
            request_id: event.request_id,
        }
    }
}

impl From<BackendTransactionEvent> for TransactionRuntimeEvent {
    fn from(event: BackendTransactionEvent) -> Self {
        match event {
            BackendTransactionEvent::Finished { id, payload } => Self::Finished { id, payload },
            BackendTransactionEvent::Error { id, error } => Self::Error { id, error },
            BackendTransactionEvent::Data { id, payload } => Self::Data { id, payload },
            BackendTransactionEvent::Status { id, status } => Self::Status { id, status },
            BackendTransactionEvent::Progress { id, progress } => Self::Progress { id, progress },
            BackendTransactionEvent::IncrProgress { id, incr } => Self::IncrProgress { id, incr },
            BackendTransactionEvent::ResetProgress { id } => Self::ResetProgress { id },
        }
    }
}

impl TransactionRuntimeEvent {
    pub(super) const fn transaction_id(&self) -> TransactionId {
        match self {
            Self::Finished { id, .. }
            | Self::Error { id, .. }
            | Self::Data { id, .. }
            | Self::Status { id, .. }
            | Self::Progress { id, .. }
            | Self::IncrProgress { id, .. }
            | Self::ResetProgress { id } => *id,
        }
    }

    pub(super) fn is_bufferable_pre_start(&self) -> bool {
        matches!(
            self,
            Self::Status { status, .. } if *status == TransactionStatus::Locating
        )
    }

    pub(super) const fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. } | Self::Error { .. })
    }
}
