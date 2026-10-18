use super::{
    AppPaths, Settings,
    domain::{
        PublishedFileId, SearchFullBatch, SearchFullRequest, SearchHit, SearchItem,
        SearchItemSource, SearchQuickBatch, SearchQuickRequest, SteamUser, WorkshopItem,
    },
    library::{LibraryRefresh, LibraryRefreshReason, LibrarySnapshot, LibraryStore},
    metadata_snapshot::CachedWorkshopMetadata,
    native::{self, NativeOpenTarget},
    publish::{
        PublishSelectedPreview, PublishSubmitMode, PublishSubmitPreview, PublishSubmitRequest,
    },
    ui_error::UiError,
};

#[cfg(test)]
use super::{
    domain::{SearchMode, WorkshopMetadata},
    metadata_snapshot,
};
#[cfg(test)]
use projections::{
    publish_submission_from_app_request, search_quick_batch_from_backend,
    subscription_counts_from_items,
};

#[cfg(test)]
use super::UiSettings;

mod context;
mod correlation;
mod projections;
pub use projections::search_full_batch_from_transaction_payload;
mod runtime_events;
mod services;
mod task_events;
mod worker_runtime;

#[cfg(test)]
mod tests;

pub use context::{BackendContext, fallback_paths};
use correlation::{BackendTaskCancelResult, BackendTaskSource, BackendTransactionTasks};
use runtime_events::{BackendEventSinkRegistration, install_backend_event_sink_by_default};
pub use runtime_events::{
    BackendRuntimeAction, BackendRuntimeEvent, BackendRuntimeEventEffects,
    BackendRuntimeEventSubscription, TransactionRuntimeEvent,
};
pub use services::BackendServices;
#[cfg(test)]
pub use task_events::SharedTaskUpdate;
#[cfg(test)]
use task_events::task_event_stream;
use task_events::{BackendEventStreamFactory, TaskEventStreamFactory};
pub use task_events::{
    ExtractionOutput, TaskEvent, TaskHandle, TaskId, TaskIdentity, TaskKind, TaskUpdate, Tasks,
    WorkshopDownloadTaskKind,
};
use worker_runtime::show_native_open_error_dialog;
pub use worker_runtime::{RunBlockingError, ScheduleError};
const TASK_EVENTS_ID: u64 = 1;
const BACKEND_EVENTS_ID: u64 = 2;
const BACKEND_EVENT_QUEUE_CAPACITY: usize = 256;

pub use gmpublished_backend::TransactionStatus;
pub use gmpublished_backend::WorkshopSnapshotId;
const TRANSACTION_PROGRESS_SCALE: f64 = 10_000.0;
