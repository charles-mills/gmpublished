//! App-owned task, scheduler, and Iced worker boundary.
//!
//! Backend transactions drive the Steam and GMA operations (download,
//! extract, publish); this module is the Iced-facing boundary that
//! schedules app workers and projects typed task events for the UI.

use super::{
    AppPaths, Settings,
    domain::{
        PublishedFileId, SearchFullBatch, SearchFullRequest, SearchHit, SearchItem,
        SearchItemSource, SearchQuickBatch, SearchQuickRequest, SteamUser, WorkshopItem,
    },
    library::{self, LibraryRefresh, LibraryRefreshReason, LibrarySnapshot, LibraryStore},
    metadata_snapshot::{self, CachedWorkshopMetadata},
    native::{self, NativeOpenTarget},
    publish::{
        PublishSelectedPreview, PublishSubmitMode, PublishSubmitPreview, PublishSubmitRequest,
    },
    ui_error::UiError,
};

#[cfg(test)]
use super::domain::{SearchMode, WorkshopMetadata};
#[cfg(test)]
use projections::{
    publish_submission_from_app_request, search_full_batch_from_transaction_payload,
    search_quick_batch_from_backend, subscription_counts_from_items,
};

#[cfg(test)]
use super::UiSettings;

mod context;
mod correlation;
mod projections;
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
pub use services::{
    ArchiveService, BackendServices, ConfigService, PublishService, WorkshopService,
};
#[cfg(test)]
use task_events::task_event_stream;
use task_events::{BackendEventStreamFactory, TaskEventStreamFactory};
/// Surface no production caller needs, but which tests construct or observe
/// directly. Gated so a reader of this module's API is not shown machinery
/// that nothing real reaches for.
#[cfg(test)]
pub use task_events::{CoalescedTaskStart, SharedTaskUpdate};
pub use task_events::{
    CoalescedTaskTerminal, CoalescedTaskUpdate, TaskEvent, TaskHandle, TaskId, TaskKind,
    TaskUpdate, Tasks, WorkshopDownloadTaskKind,
};
use worker_runtime::{AppWorkerRuntime, WorkerPoolSpawner, show_native_open_error_dialog};
pub use worker_runtime::{RunBlockingError, ScheduleError};
const TASK_EVENTS_ID: u64 = 1;
const BACKEND_EVENTS_ID: u64 = 2;
const BACKEND_EVENT_QUEUE_CAPACITY: usize = 256;

/// Every status a task can report, whichever crate raised it.
///
/// Re-exported rather than redeclared: the statuses this crate produces for
/// its own work and the ones it receives from backend transactions land in the
/// same overlay, so they are one closed set with one spelling.
pub use gmpublished_backend::TransactionStatus;
pub use gmpublished_backend::WorkshopSnapshotId;
const TRANSACTION_PROGRESS_SCALE: f64 = 10_000.0;
const MAX_PENDING_PRE_START_TRANSACTIONS: usize = 128;
const MAX_PENDING_PRE_START_EVENTS_PER_TRANSACTION: usize = 8;
/// Recent terminal ids retained to reject delayed running events rather than
/// mistaking them for events that raced their start correlation.
const MAX_COMPLETED_TRANSACTION_TOMBSTONES: usize = 1_024;
