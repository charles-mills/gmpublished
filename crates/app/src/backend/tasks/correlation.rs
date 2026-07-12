use std::collections::VecDeque;
use std::path::PathBuf;

use gmpublished_backend::error_key::keys;

use super::{
    BackendRuntimeAction, BackendRuntimeEventEffects, HashMap,
    MAX_PENDING_PRE_START_EVENTS_PER_TRANSACTION, MAX_PENDING_PRE_START_TRANSACTIONS, Mutex,
    PublishedFileId, TRANSACTION_PROGRESS_SCALE, TaskHandle, TaskId, TransactionPayload,
    TransactionRuntimeEvent, UiError, WorkshopDownloadTaskKind, transactions,
};

#[derive(Debug, Default)]
pub(super) struct BackendTransactionTasks {
    active: Mutex<HashMap<u32, CorrelatedBackendTask>>,
    pub(super) pending_pre_start: Mutex<HashMap<u32, VecDeque<TransactionRuntimeEvent>>>,
}

impl BackendTransactionTasks {
    pub(super) fn correlate(
        &self,
        transaction_id: u32,
        task: TaskHandle,
        source: BackendTaskSource,
    ) -> Vec<BackendRuntimeAction> {
        let task_id = task.id();
        let mut task = CorrelatedBackendTask {
            handle: task,
            source,
        };
        let mut actions = task.take_ready_actions();
        self.active.lock().insert(transaction_id, task);
        for pending_event in self.take_pending_pre_start(transaction_id) {
            actions.extend(self.apply(&pending_event).into_actions());
        }
        debug_assert!(
            actions
                .iter()
                .all(|action| action.task_id() == Some(task_id))
        );
        actions
    }

    pub(super) fn apply(&self, event: &TransactionRuntimeEvent) -> BackendRuntimeEventEffects {
        let transaction_id = event.transaction_id();
        let mut active = self.active.lock();
        let (terminal, actions) = {
            let Some(task) = active.get_mut(&transaction_id) else {
                if event.is_bufferable_pre_start() {
                    drop(active);
                    self.buffer_pre_start(event.clone());
                    return BackendRuntimeEventEffects::handled();
                }
                return BackendRuntimeEventEffects::ignored();
            };
            apply_transaction_event_to_task(task, event)
        };

        if terminal {
            active.remove(&transaction_id);
        }
        drop(active);

        BackendRuntimeEventEffects::handled_with(actions)
    }

    fn buffer_pre_start(&self, event: TransactionRuntimeEvent) {
        let transaction_id = event.transaction_id();
        let mut pending = self.pending_pre_start.lock();
        if pending.len() >= MAX_PENDING_PRE_START_TRANSACTIONS
            && !pending.contains_key(&transaction_id)
            && let Some(stale_transaction_id) = pending.keys().next().copied()
        {
            pending.remove(&stale_transaction_id);
        }
        let events = pending.entry(transaction_id).or_default();
        if events.len() >= MAX_PENDING_PRE_START_EVENTS_PER_TRANSACTION {
            events.pop_front();
        }
        events.push_back(event);
        drop(pending);
    }

    fn take_pending_pre_start(&self, transaction_id: u32) -> VecDeque<TransactionRuntimeEvent> {
        self.pending_pre_start
            .lock()
            .remove(&transaction_id)
            .unwrap_or_default()
    }

    pub(super) fn error(&self, transaction_id: u32, error: UiError) -> bool {
        let Some(task) = self.active.lock().remove(&transaction_id) else {
            return false;
        };
        task.handle.error(error);
        true
    }

    pub(super) fn is_active(&self, transaction_id: u32) -> bool {
        self.active.lock().contains_key(&transaction_id)
    }

    pub(super) fn cancel_task(
        &self,
        task_id: TaskId,
        transactions: &transactions::Transactions,
    ) -> BackendTaskCancelResult {
        let mut active = self.active.lock();
        let Some(transaction_id) = active.iter().find_map(|(transaction_id, task)| {
            (task.task_id() == task_id).then_some(*transaction_id)
        }) else {
            return BackendTaskCancelResult::Uncorrelated;
        };

        if !transactions.cancel_by_id(transaction_id) {
            return BackendTaskCancelResult::NotCancellable;
        }

        if let Some(task) = active.remove(&transaction_id) {
            task.cancelled();
        }
        drop(active);

        BackendTaskCancelResult::Cancelled
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BackendTaskCancelResult {
    Cancelled,
    NotCancellable,
    Uncorrelated,
}

#[derive(Debug)]
pub(super) struct CorrelatedBackendTask {
    handle: TaskHandle,
    source: BackendTaskSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum BackendTaskSource {
    Generic,
    WorkshopDownload {
        item_id: Option<PublishedFileId>,
        start_emitted: bool,
    },
    WorkshopExtraction {
        item_id: Option<PublishedFileId>,
        start_emitted: bool,
        /// The on-disk `.gma` the extraction reads from, when it outlives
        /// the extraction (installed workshop content, not temp payloads).
        source_gma: Option<PathBuf>,
    },
}

impl CorrelatedBackendTask {
    fn task_id(&self) -> TaskId {
        self.handle.id()
    }

    fn cancelled(self) {
        self.handle.error(keys::CANCELLED);
    }

    fn take_ready_actions(&mut self) -> Vec<BackendRuntimeAction> {
        match &mut self.source {
            BackendTaskSource::Generic => Vec::new(),
            BackendTaskSource::WorkshopDownload {
                item_id,
                start_emitted,
            } => take_workshop_start_action(
                WorkshopDownloadTaskKind::Download,
                *item_id,
                start_emitted,
                self.handle.id(),
            ),
            BackendTaskSource::WorkshopExtraction {
                item_id,
                start_emitted,
                ..
            } => take_workshop_start_action(
                WorkshopDownloadTaskKind::Extract,
                *item_id,
                start_emitted,
                self.handle.id(),
            ),
        }
    }

    fn set_workshop_item_id(&mut self, item_id: PublishedFileId) -> Vec<BackendRuntimeAction> {
        match &mut self.source {
            BackendTaskSource::WorkshopDownload {
                item_id: slot,
                start_emitted,
            } => {
                if slot.is_none() {
                    *slot = Some(item_id);
                }
                take_workshop_start_action(
                    WorkshopDownloadTaskKind::Download,
                    *slot,
                    start_emitted,
                    self.handle.id(),
                )
            }
            BackendTaskSource::WorkshopExtraction {
                item_id: slot,
                start_emitted,
                ..
            } => {
                if slot.is_none() {
                    *slot = Some(item_id);
                }
                take_workshop_start_action(
                    WorkshopDownloadTaskKind::Extract,
                    *slot,
                    start_emitted,
                    self.handle.id(),
                )
            }
            BackendTaskSource::Generic => Vec::new(),
        }
    }

    fn finished_actions(&self, payload: &TransactionPayload) -> Vec<BackendRuntimeAction> {
        let Some(item_id) = self.source.item_id() else {
            return Vec::new();
        };
        let TransactionPayload::ExtractedPath(extracted_path) = payload else {
            return Vec::new();
        };
        if self.source.workshop_kind() != Some(WorkshopDownloadTaskKind::Extract) {
            return Vec::new();
        }

        vec![BackendRuntimeAction::WorkshopDownloadFinished {
            item_id,
            installed_path: self.source.source_gma().cloned(),
            extracted_path: extracted_path.clone(),
        }]
    }
}

impl BackendTaskSource {
    const fn item_id(&self) -> Option<PublishedFileId> {
        match self {
            Self::Generic => None,
            Self::WorkshopDownload { item_id, .. } | Self::WorkshopExtraction { item_id, .. } => {
                *item_id
            }
        }
    }

    const fn workshop_kind(&self) -> Option<WorkshopDownloadTaskKind> {
        match self {
            Self::Generic => None,
            Self::WorkshopDownload { .. } => Some(WorkshopDownloadTaskKind::Download),
            Self::WorkshopExtraction { .. } => Some(WorkshopDownloadTaskKind::Extract),
        }
    }

    const fn source_gma(&self) -> Option<&PathBuf> {
        match self {
            Self::Generic | Self::WorkshopDownload { .. } => None,
            Self::WorkshopExtraction { source_gma, .. } => source_gma.as_ref(),
        }
    }
}

impl BackendRuntimeAction {
    const fn task_id(&self) -> Option<TaskId> {
        match self {
            Self::WorkshopDownloadTaskStarted { task_id, .. } => Some(*task_id),
            Self::WorkshopDownloadFinished { .. } => None,
        }
    }
}

pub(super) fn take_workshop_start_action(
    kind: WorkshopDownloadTaskKind,
    item_id: Option<PublishedFileId>,
    start_emitted: &mut bool,
    task_id: TaskId,
) -> Vec<BackendRuntimeAction> {
    if *start_emitted {
        return Vec::new();
    }
    let Some(item_id) = item_id else {
        return Vec::new();
    };

    *start_emitted = true;
    vec![BackendRuntimeAction::WorkshopDownloadTaskStarted {
        kind,
        item_id,
        task_id,
    }]
}

pub(super) fn apply_transaction_event_to_task(
    task: &mut CorrelatedBackendTask,
    event: &TransactionRuntimeEvent,
) -> (bool, Vec<BackendRuntimeAction>) {
    match event {
        TransactionRuntimeEvent::Finished { payload, .. } => {
            let actions = task.finished_actions(payload);
            task.handle.finished();
            (true, actions)
        }
        TransactionRuntimeEvent::Error { error, .. } => {
            task.handle.error(UiError::from(error.clone()));
            (true, Vec::new())
        }
        TransactionRuntimeEvent::Data { payload, .. } => {
            let mut actions = match payload {
                TransactionPayload::WorkshopItem(item_id) => task.set_workshop_item_id(
                    PublishedFileId::new(item_id.0)
                        .expect("Steam never issues a zero published file id"),
                ),
                _ => Vec::new(),
            };
            match payload {
                TransactionPayload::TotalBytes(total_bytes)
                | TransactionPayload::ByteSize {
                    bytes: total_bytes, ..
                } => {
                    task.handle.total(*total_bytes);
                }
                _ => {}
            }
            actions.extend(task.take_ready_actions());
            (false, actions)
        }
        TransactionRuntimeEvent::Status { status, .. } => {
            task.handle.status(status.clone());
            (false, Vec::new())
        }
        TransactionRuntimeEvent::Progress { progress, .. } => {
            task.handle
                .progress(f64::from(*progress) / TRANSACTION_PROGRESS_SCALE);
            (false, Vec::new())
        }
        TransactionRuntimeEvent::IncrProgress { incr, .. } => {
            task.handle
                .progress_incr(f64::from(*incr) / TRANSACTION_PROGRESS_SCALE);
            (false, Vec::new())
        }
        TransactionRuntimeEvent::ResetProgress { .. } => {
            task.handle.progress_reset();
            (false, Vec::new())
        }
    }
}
