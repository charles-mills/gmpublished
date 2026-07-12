use gmpublished_backend::events::BackendEventSink;

use super::{
    Arc, BackendAppDataSnapshot, BackendDownloadStartedEvent, BackendExtractionStartedEvent,
    BackendSinkEvent, BackendTransactionEvent, DOWNLOAD_STATUS_LOCATING, Duration, PathBuf,
    PublishedFileId, SyncSender, TaskId, TransactionError, TransactionPayload, TrySendError,
    WorkshopDownloadTaskKind, fmt, mpsc,
};

#[cfg(not(test))]
pub(super) const fn install_backend_event_sink_by_default() -> bool {
    true
}

#[cfg(test)]
pub(super) const fn install_backend_event_sink_by_default() -> bool {
    false
}

#[cfg(not(test))]
pub(super) const fn persist_appdata_settings_by_default() -> bool {
    true
}

#[cfg(test)]
pub(super) const fn persist_appdata_settings_by_default() -> bool {
    false
}

#[cfg(not(test))]
pub(super) const fn persist_ui_settings_by_default() -> bool {
    true
}

#[cfg(test)]
pub(super) const fn persist_ui_settings_by_default() -> bool {
    false
}

/// Forwards `Backend`-emitted events into the app's task/event pipeline:
/// this is itself the `Arc<dyn BackendEventSink>` handed to `BackendConfig`
/// at `Backend::init` time — no process-global sink involved.
pub(super) struct BackendEventSinkRegistration {
    hub: Arc<BackendEventHub>,
}

impl BackendEventSinkRegistration {
    pub(super) fn new(sender: SyncSender<BackendRuntimeEvent>) -> Self {
        Self {
            hub: Arc::new(BackendEventHub::new(sender)),
        }
    }

    /// The sink to hand to `BackendConfig::event_sink`.
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
        if event.is_terminal() {
            // Terminals (Finished/Error) must never be silently dropped: the
            // consumer is the dedicated forwarder thread, which always
            // drains, so a full queue here is a transient backlog, not a
            // stall worth blocking indefinitely to avoid.
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
    // Boxed: every `BackendRuntimeEvent`, whatever its variant, is sized by
    // the largest one and moves through `BackendEventHub`'s bounded channels
    // (the root `SyncSender` plus one `mpsc::Sender` per subscriber, cloned
    // per recipient in `emit`). An unboxed snapshot here would tax every
    // channel slot and every fieldless variant (e.g. `SteamConnected`) with
    // the ~500-byte `AppDataSnapshot` payload, not just appdata updates.
    AppDataUpdated(Box<BackendAppDataSnapshot>),
    InstalledAddonsRefreshed,
    DownloadStarted {
        transaction_id: u32,
    },
    ExtractionStarted {
        transaction_id: u32,
        source_path: Option<PathBuf>,
        file_name: Option<String>,
        workshop_id: Option<PublishedFileId>,
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

    /// Whether this event is a transaction's terminal delivery
    /// (Finished/Error - cancellation rides the `Error` variant with a
    /// `CANCELLED` error key). Terminal events must reach their subscriber;
    /// everything else may be dropped under backpressure.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Transaction(
                TransactionRuntimeEvent::Finished { .. } | TransactionRuntimeEvent::Error { .. }
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
    WorkshopDownloadTaskStarted {
        kind: WorkshopDownloadTaskKind,
        item_id: PublishedFileId,
        task_id: TaskId,
    },
    WorkshopDownloadFinished {
        item_id: PublishedFileId,
        installed_path: Option<PathBuf>,
        extracted_path: PathBuf,
    },
}

/// These values update the task overlay only when a live-service boundary has
/// explicitly correlated the backend transaction id with an app `TaskId`.
/// Uncorrelated transaction events remain data-only no-ops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransactionRuntimeEvent {
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
            // `BackendSinkEvent::AppDataUpdated` already carries a
            // `Box<AppDataSnapshot>` (boxed at the source in
            // crates/backend/src/events.rs), which is the same boxed type
            // this variant expects, so this is a plain move, not a
            // second allocation.
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
        }
    }
}

impl From<BackendExtractionStartedEvent> for BackendRuntimeEvent {
    fn from(event: BackendExtractionStartedEvent) -> Self {
        Self::ExtractionStarted {
            transaction_id: event.transaction_id,
            source_path: event.source_path,
            file_name: event.file_name,
            workshop_id: event.workshop_id.map(|id| {
                PublishedFileId::new(id.0).expect("backend never stores a zero workshop id")
            }),
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
    pub(super) const fn transaction_id(&self) -> u32 {
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
            Self::Status { status, .. } if status == DOWNLOAD_STATUS_LOCATING
        )
    }
}
