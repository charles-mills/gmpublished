use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use gmpublished_backend::error_keys as keys;

use super::{
    BackendRuntimeAction, BackendRuntimeEventEffects, MAX_COMPLETED_TRANSACTION_TOMBSTONES,
    MAX_PENDING_PRE_START_EVENTS_PER_TRANSACTION, MAX_PENDING_PRE_START_TRANSACTIONS,
    PublishedFileId, TRANSACTION_PROGRESS_SCALE, TaskHandle, TaskId, TransactionRuntimeEvent,
    UiError, WorkshopDownloadTaskKind, WorkshopSnapshotId,
};
use gmpublished_backend::TransactionPayload;
use gmpublished_backend::{FinalizeOutcome, TransactionId};
use parking_lot::Mutex;

#[derive(Debug, Default)]
pub(super) struct BackendTransactionTasks {
    state: Mutex<BackendTransactionTaskState>,
}

#[derive(Debug, Default)]
struct BackendTransactionTaskState {
    active: HashMap<TransactionId, CorrelatedBackendTask>,
    pending_pre_start: HashMap<TransactionId, VecDeque<TransactionRuntimeEvent>>,
    completed: CompletedTransactionTombstones,
}

#[derive(Debug, Default)]
struct CompletedTransactionTombstones {
    ids: HashSet<TransactionId>,
    insertion_order: VecDeque<TransactionId>,
}

impl CompletedTransactionTombstones {
    fn contains(&self, transaction_id: TransactionId) -> bool {
        self.ids.contains(&transaction_id)
    }

    fn insert(&mut self, transaction_id: TransactionId) {
        if !self.ids.insert(transaction_id) {
            return;
        }
        self.insertion_order.push_back(transaction_id);

        while self.ids.len() > MAX_COMPLETED_TRANSACTION_TOMBSTONES {
            let Some(stale_id) = self.insertion_order.pop_front() else {
                break;
            };
            self.ids.remove(&stale_id);
        }
    }
}

impl BackendTransactionTasks {
    pub(super) fn correlate(
        &self,
        transaction_id: TransactionId,
        task: TaskHandle,
        source: BackendTaskSource,
    ) -> Vec<BackendRuntimeAction> {
        let task_id = task.id();
        let mut task = CorrelatedBackendTask {
            handle: task,
            source,
        };
        let mut state = self.state.lock();

        if state.completed.contains(transaction_id) {
            drop(state);
            log::warn!("Ignoring a late start for completed transaction {transaction_id}");
            task.handle.error(keys::UNKNOWN);
            return Vec::new();
        }

        let mut actions = task.take_ready_actions();
        for pending_event in state
            .pending_pre_start
            .remove(&transaction_id)
            .unwrap_or_default()
        {
            let applied = apply_transaction_event_to_task(&mut task, &pending_event);
            debug_assert!(!applied.terminal, "only ongoing events are buffered");
            actions.extend(applied.actions);
        }
        let replaced = state.active.insert(transaction_id, task);
        debug_assert!(replaced.is_none(), "transaction correlated twice");
        drop(state);

        debug_assert!(
            actions
                .iter()
                .all(|action| action.task_id() == Some(task_id))
        );
        actions
    }

    pub(super) fn apply(&self, event: &TransactionRuntimeEvent) -> BackendRuntimeEventEffects {
        let transaction_id = event.transaction_id();
        let mut state = self.state.lock();
        if state.completed.contains(transaction_id) {
            return BackendRuntimeEventEffects::ignored();
        }

        let AppliedEvent { terminal, actions } = {
            let Some(task) = state.active.get_mut(&transaction_id) else {
                if event.is_terminal() {
                    state.pending_pre_start.remove(&transaction_id);
                    state.completed.insert(transaction_id);
                    return BackendRuntimeEventEffects::ignored();
                }
                if event.is_bufferable_pre_start() {
                    state.buffer_pre_start(event.clone());
                    return BackendRuntimeEventEffects::handled();
                }
                return BackendRuntimeEventEffects::ignored();
            };
            apply_transaction_event_to_task(task, event)
        };

        if terminal {
            state.active.remove(&transaction_id);
            state.pending_pre_start.remove(&transaction_id);
            state.completed.insert(transaction_id);
        }
        drop(state);

        BackendRuntimeEventEffects::handled_with(actions)
    }

    pub(super) fn error(&self, transaction_id: TransactionId, error: UiError) -> bool {
        let mut state = self.state.lock();
        let Some(task) = state.active.remove(&transaction_id) else {
            return false;
        };
        state.pending_pre_start.remove(&transaction_id);
        state.completed.insert(transaction_id);
        drop(state);

        task.handle.error(error);
        true
    }

    pub(super) fn is_active(&self, transaction_id: TransactionId) -> bool {
        self.state.lock().active.contains_key(&transaction_id)
    }

    pub(super) fn cancel_task(
        &self,
        task_id: TaskId,
        cancel_transaction: impl FnOnce(TransactionId) -> Option<FinalizeOutcome>,
    ) -> BackendTaskCancelResult {
        let correlated = self
            .state
            .lock()
            .active
            .iter()
            .find_map(|(transaction_id, task)| {
                (task.task_id() == task_id).then_some(*transaction_id)
            });
        let Some(transaction_id) = correlated else {
            return BackendTaskCancelResult::Uncorrelated;
        };

        if cancel_transaction(transaction_id) != Some(FinalizeOutcome::Finalized) {
            return BackendTaskCancelResult::NotCancellable;
        }

        let mut state = self.state.lock();
        let task = state.active.remove(&transaction_id);
        state.pending_pre_start.remove(&transaction_id);
        state.completed.insert(transaction_id);
        drop(state);

        if let Some(task) = task {
            task.cancelled();
        }

        BackendTaskCancelResult::Cancelled
    }

    #[cfg(test)]
    pub(super) fn pending_pre_start_snapshot(
        &self,
    ) -> HashMap<TransactionId, VecDeque<TransactionRuntimeEvent>> {
        self.state.lock().pending_pre_start.clone()
    }

    #[cfg(test)]
    pub(super) fn completed_count(&self) -> usize {
        self.state.lock().completed.ids.len()
    }

    #[cfg(test)]
    pub(super) fn is_completed(&self, transaction_id: TransactionId) -> bool {
        self.state.lock().completed.contains(transaction_id)
    }
}

impl BackendTransactionTaskState {
    fn buffer_pre_start(&mut self, event: TransactionRuntimeEvent) {
        let transaction_id = event.transaction_id();
        if self.pending_pre_start.len() >= MAX_PENDING_PRE_START_TRANSACTIONS
            && !self.pending_pre_start.contains_key(&transaction_id)
            && let Some(stale_transaction_id) = self.pending_pre_start.keys().next().copied()
        {
            self.pending_pre_start.remove(&stale_transaction_id);
        }
        let events = self.pending_pre_start.entry(transaction_id).or_default();
        if events.len() >= MAX_PENDING_PRE_START_EVENTS_PER_TRANSACTION {
            events.pop_front();
        }
        events.push_back(event);
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
        request_id: Option<WorkshopSnapshotId>,
    },
    WorkshopExtraction {
        item_id: Option<PublishedFileId>,
        start_emitted: bool,
        /// The on-disk `.gma` the extraction reads from, when it outlives
        /// the extraction (installed workshop content, not temp payloads).
        source_gma: Option<PathBuf>,
        request_id: Option<WorkshopSnapshotId>,
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
                request_id,
            } => take_workshop_start_action(
                WorkshopDownloadTaskKind::Download,
                *item_id,
                start_emitted,
                self.handle.id(),
                *request_id,
            ),
            BackendTaskSource::WorkshopExtraction {
                item_id,
                start_emitted,
                request_id,
                ..
            } => take_workshop_start_action(
                WorkshopDownloadTaskKind::Extract,
                *item_id,
                start_emitted,
                self.handle.id(),
                *request_id,
            ),
        }
    }

    fn set_workshop_item_id(&mut self, item_id: PublishedFileId) -> Vec<BackendRuntimeAction> {
        match &mut self.source {
            BackendTaskSource::WorkshopDownload {
                item_id: slot,
                start_emitted,
                request_id,
            } => {
                if slot.is_none() {
                    *slot = Some(item_id);
                }
                take_workshop_start_action(
                    WorkshopDownloadTaskKind::Download,
                    *slot,
                    start_emitted,
                    self.handle.id(),
                    *request_id,
                )
            }
            BackendTaskSource::WorkshopExtraction {
                item_id: slot,
                start_emitted,
                request_id,
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
                    *request_id,
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

        vec![BackendRuntimeAction::DownloadFinished {
            request_id: self.source.request_id(),
            item_id,
            installed_path: self.source.source_gma().map(Path::to_path_buf),
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

    fn source_gma(&self) -> Option<&Path> {
        match self {
            Self::Generic | Self::WorkshopDownload { .. } => None,
            Self::WorkshopExtraction { source_gma, .. } => source_gma.as_deref(),
        }
    }

    const fn request_id(&self) -> Option<WorkshopSnapshotId> {
        match self {
            Self::Generic => None,
            Self::WorkshopDownload { request_id, .. }
            | Self::WorkshopExtraction { request_id, .. } => *request_id,
        }
    }
}

impl BackendRuntimeAction {
    const fn task_id(&self) -> Option<TaskId> {
        match self {
            Self::DownloadTaskStarted { task_id, .. } => Some(*task_id),
            Self::DownloadFinished { .. } | Self::SnapshotFailed { .. } => None,
        }
    }
}

pub(super) fn take_workshop_start_action(
    kind: WorkshopDownloadTaskKind,
    item_id: Option<PublishedFileId>,
    start_emitted: &mut bool,
    task_id: TaskId,
    request_id: Option<WorkshopSnapshotId>,
) -> Vec<BackendRuntimeAction> {
    if request_id.is_some() {
        return Vec::new();
    }
    if *start_emitted {
        return Vec::new();
    }
    let Some(item_id) = item_id else {
        return Vec::new();
    };

    *start_emitted = true;
    vec![BackendRuntimeAction::DownloadTaskStarted {
        kind,
        item_id,
        task_id,
    }]
}

/// What applying one transaction event to its task produced.
struct AppliedEvent {
    /// Whether the transaction reached a terminal state and its correlation
    /// entry should be dropped.
    pub(super) terminal: bool,
    pub(super) actions: Vec<BackendRuntimeAction>,
}

impl AppliedEvent {
    fn ongoing(actions: Vec<BackendRuntimeAction>) -> Self {
        Self {
            terminal: false,
            actions,
        }
    }

    fn terminal(actions: Vec<BackendRuntimeAction>) -> Self {
        Self {
            terminal: true,
            actions,
        }
    }
}

fn apply_transaction_event_to_task(
    task: &mut CorrelatedBackendTask,
    event: &TransactionRuntimeEvent,
) -> AppliedEvent {
    match event {
        TransactionRuntimeEvent::Finished { payload, .. } => {
            let actions = task.finished_actions(payload);
            task.handle.finished();
            AppliedEvent::terminal(actions)
        }
        TransactionRuntimeEvent::Error { error, .. } => {
            let actions = task
                .source
                .request_id()
                .map_or_else(Vec::new, |request_id| {
                    vec![BackendRuntimeAction::SnapshotFailed {
                        request_id,
                        error: UiError::from(error.clone()),
                    }]
                });
            task.handle.error(UiError::from(error.clone()));
            AppliedEvent::terminal(actions)
        }
        TransactionRuntimeEvent::Data { payload, .. } => {
            let mut actions = match payload {
                TransactionPayload::WorkshopItem(item_id) => {
                    task.set_workshop_item_id(PublishedFileId::from(*item_id))
                }
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
            AppliedEvent::ongoing(actions)
        }
        TransactionRuntimeEvent::Status { status, .. } => {
            task.handle.status(*status);
            AppliedEvent::ongoing(Vec::new())
        }
        TransactionRuntimeEvent::Progress { progress, .. } => {
            task.handle
                .progress(f64::from(*progress) / TRANSACTION_PROGRESS_SCALE);
            AppliedEvent::ongoing(Vec::new())
        }
        TransactionRuntimeEvent::IncrProgress { incr, .. } => {
            task.handle
                .progress_incr(f64::from(*incr) / TRANSACTION_PROGRESS_SCALE);
            AppliedEvent::ongoing(Vec::new())
        }
        TransactionRuntimeEvent::ResetProgress { .. } => {
            task.handle.progress_reset();
            AppliedEvent::ongoing(Vec::new())
        }
    }
}
