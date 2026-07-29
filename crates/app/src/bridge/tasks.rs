//! App-owned task, scheduler, and Iced worker boundary.
//!
//! Backend transactions drive the Steam and GMA operations (download,
//! extract, publish); this module is the Iced-facing boundary that
//! schedules app workers and projects typed task events for the UI.

use std::time::Duration;

use super::{
    AppPaths, Settings, SettingsPersistError, UiSettings, appdata_snapshot_from_backend,
    domain::{
        PublishedFileId, SearchFullBatch, SearchFullRequest, SearchHit, SearchItem,
        SearchItemSource, SearchMode, SearchQuickBatch, SearchQuickRequest, SteamUser,
        WorkshopItem, WorkshopMetadata, WorkshopPage,
    },
    library::{self, LibraryRefresh, LibraryRefreshReason, LibrarySnapshot, LibraryStore},
    metadata_snapshot::{self, CachedWorkshopMetadata},
    native::{self, NativeOpenTarget},
    publish::{
        PublishSelectedPreview, PublishSubmitMode, PublishSubmitOutcome, PublishSubmitPreview,
        PublishSubmitRequest,
    },
    ui_error::UiError,
    ui_settings_file_for,
};

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
use projections::{
    clear_directory_contents, publish_submission_from_app_request,
    search_full_batch_from_transaction_payload, search_quick_batch_from_backend,
    steam_user_from_backend, steam_user_from_workshop_backend, subscription_counts_from_items,
    workshop_item_from_backend,
};
use runtime_events::{BackendEventSinkRegistration, install_backend_event_sink_by_default};
pub use runtime_events::{
    BackendRuntimeAction, BackendRuntimeEvent, BackendRuntimeEventEffects,
    BackendRuntimeEventSubscription, TransactionRuntimeEvent,
};
pub use services::BackendServices;
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
#[cfg(test)]
use worker_runtime::{RuntimeConfig, blocking_worker_count};

const BLOCKING_MIN_THREADS: usize = 2;
const BLOCKING_MAX_THREADS: usize = 8;
const BLOCKING_FALLBACK_THREADS: usize = 4;
const MEDIA_THREADS: usize = 16;
const BLOCKING_QUEUE_CAPACITY: usize = 256;
/// Bound on a worker pool's shutdown join, shared across its threads rather
/// than applied per-thread. Matched to `steam::SHUTDOWN_JOIN_TIMEOUT`: both are
/// paid on the same process-exit path, so they add up, and neither is worth
/// making the user wait on.
const WORKER_SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_millis(500);
const TASK_EVENTS_ID: u64 = 1;
const BACKEND_EVENTS_ID: u64 = 2;
const BACKEND_EVENT_QUEUE_CAPACITY: usize = 256;

/// Every status a task can report, whichever crate raised it.
///
/// Re-exported rather than redeclared: the statuses this crate produces for
/// its own work and the ones it receives from backend transactions land in the
/// same overlay, so they are one closed set with one spelling.
pub use gmpublished_backend::transactions::TransactionStatus;
const TRANSACTION_PROGRESS_SCALE: f64 = 10_000.0;
const MAX_PENDING_PRE_START_TRANSACTIONS: usize = 128;
const MAX_PENDING_PRE_START_EVENTS_PER_TRANSACTION: usize = 8;
