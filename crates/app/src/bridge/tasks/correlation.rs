use std::{collections::HashMap, path::PathBuf};

use gmpublished_backend::{FinalizeOutcome, TransactionId, TransactionPayload};
use parking_lot::Mutex;

use super::{
    BackendRuntimeAction, BackendRuntimeEventEffects, ExtractionOutput, PublishedFileId,
    TRANSACTION_PROGRESS_SCALE, TaskHandle, TaskId, TransactionRuntimeEvent, UiError,
    WorkshopSnapshotId,
};

#[derive(Debug, Default)]
pub(super) struct BackendTransactionTasks {
    active: Mutex<HashMap<TransactionId, CorrelatedBackendTask>>,
}

impl BackendTransactionTasks {
    pub(super) fn correlate(
        &self,
        transaction_id: TransactionId,
        task: TaskHandle,
        source: BackendTaskSource,
    ) {
        let mut active = self.active.lock();
        debug_assert!(
            !active.contains_key(&transaction_id),
            "transaction correlated twice"
        );
        active.insert(
            transaction_id,
            CorrelatedBackendTask {
                handle: task,
                source,
            },
        );
    }

    pub(super) fn apply(&self, event: &TransactionRuntimeEvent) -> BackendRuntimeEventEffects {
        let id = event.transaction_id();
        let mut active = self.active.lock();
        let Some(task) = active.get_mut(&id) else {
            // Start precedes every event for an automatically discovered job;
            // directly initiated transactions are registered before work starts.
            return BackendRuntimeEventEffects::ignored();
        };
        let actions = task.apply(event);
        if event.is_terminal() {
            active.remove(&id);
        }
        drop(active);
        BackendRuntimeEventEffects::handled_with(actions)
    }

    pub(super) fn error(&self, transaction_id: TransactionId, error: UiError) -> bool {
        let task = self.active.lock().remove(&transaction_id);
        if let Some(task) = task {
            task.handle.error(error);
            true
        } else {
            false
        }
    }

    pub(super) fn is_active(&self, transaction_id: TransactionId) -> bool {
        self.active.lock().contains_key(&transaction_id)
    }

    pub(super) fn cancel_task(
        &self,
        task_id: TaskId,
        cancel_transaction: impl FnOnce(TransactionId) -> Option<FinalizeOutcome>,
    ) -> BackendTaskCancelResult {
        let id = self
            .active
            .lock()
            .iter()
            .find_map(|(id, task)| (task.handle.id() == task_id).then_some(*id));
        let Some(id) = id else {
            return BackendTaskCancelResult::Uncorrelated;
        };
        if cancel_transaction(id) != Some(FinalizeOutcome::Finalized) {
            return BackendTaskCancelResult::NotCancellable;
        }
        if let Some(task) = self.active.lock().remove(&id) {
            task.handle
                .error(gmpublished_backend::error_keys::CANCELLED);
        }
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
struct CorrelatedBackendTask {
    handle: TaskHandle,
    source: BackendTaskSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum BackendTaskSource {
    Generic,
    WorkshopDownload {
        request_id: Option<WorkshopSnapshotId>,
    },
    WorkshopExtraction {
        item_id: Option<PublishedFileId>,
        source_gma: Option<PathBuf>,
        request_id: Option<WorkshopSnapshotId>,
    },
}

impl BackendTaskSource {
    const fn request_id(&self) -> Option<WorkshopSnapshotId> {
        match self {
            Self::Generic => None,
            Self::WorkshopDownload { request_id, .. }
            | Self::WorkshopExtraction { request_id, .. } => *request_id,
        }
    }
}

impl CorrelatedBackendTask {
    fn finish(&self, payload: &TransactionPayload) -> Vec<BackendRuntimeAction> {
        let TransactionPayload::ExtractedPath(extracted_path) = payload else {
            return vec![];
        };
        let source_path = match &self.source {
            BackendTaskSource::WorkshopExtraction { source_gma, .. } => source_gma.clone(),
            _ => None,
        };
        if let BackendTaskSource::WorkshopExtraction {
            item_id: Some(item_id),
            request_id: Some(request_id),
            ..
        } = &self.source
        {
            vec![BackendRuntimeAction::SnapshotDownloaded {
                request_id: *request_id,
                item_id: *item_id,
                installed_path: source_path,
                extracted_path: extracted_path.clone(),
            }]
        } else {
            self.handle.extracted(ExtractionOutput {
                source_path,
                extracted_path: extracted_path.clone(),
            });
            vec![]
        }
    }

    fn apply(&mut self, event: &TransactionRuntimeEvent) -> Vec<BackendRuntimeAction> {
        match event {
            TransactionRuntimeEvent::Finished { payload, .. } => {
                let actions = self.finish(payload);
                self.handle.finished();
                actions
            }
            TransactionRuntimeEvent::Error { error, .. } => {
                self.handle.error(UiError::from(error.clone()));
                self.source
                    .request_id()
                    .map(|request_id| BackendRuntimeAction::SnapshotFailed {
                        request_id,
                        error: UiError::from(error.clone()),
                    })
                    .into_iter()
                    .collect()
            }
            TransactionRuntimeEvent::Data { payload, .. } => {
                match payload {
                    TransactionPayload::TotalBytes(bytes)
                    | TransactionPayload::ByteSize { bytes, .. } => self.handle.total(*bytes),
                    TransactionPayload::SourcePath(path) => {
                        if let BackendTaskSource::WorkshopExtraction { source_gma, .. } =
                            &mut self.source
                        {
                            *source_gma = Some(path.clone());
                        }
                    }
                    _ => {}
                }
                vec![]
            }
            TransactionRuntimeEvent::Status { status, .. } => {
                self.handle.status(*status);
                vec![]
            }
            TransactionRuntimeEvent::Progress { progress, .. } => {
                self.handle
                    .progress(f64::from(*progress) / TRANSACTION_PROGRESS_SCALE);
                vec![]
            }
            TransactionRuntimeEvent::IncrProgress { incr, .. } => {
                self.handle
                    .progress_incr(f64::from(*incr) / TRANSACTION_PROGRESS_SCALE);
                vec![]
            }
            TransactionRuntimeEvent::ResetProgress { .. } => {
                self.handle.progress_reset();
                vec![]
            }
        }
    }
}
