//! App-owned task, scheduler, and Iced worker boundary.
//!
//! Backend transactions drive the Steam and GMA operations (download,
//! extract, publish); this module is the Iced-facing boundary that
//! schedules app workers and projects typed task events for the UI.

use std::{
    collections::HashMap,
    fmt,
    hash::{Hash, Hasher},
    num::NonZeroUsize,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use iced::futures::channel::{mpsc as iced_mpsc, oneshot};
use iced::{Subscription, Task, stream};
use parking_lot::Mutex;
use thiserror::Error;

use gmpublished_backend::{
    Backend,
    appdata::AppDataSnapshot as BackendAppDataSnapshot,
    events::TransactionPayload,
    events::{
        BackendEvent as BackendSinkEvent, DownloadStartedEvent as BackendDownloadStartedEvent,
        ExtractionStartedEvent as BackendExtractionStartedEvent, TransactionError,
        TransactionEvent as BackendTransactionEvent,
    },
    steam::{
        SteamAvatarRgba, SteamRuntime, SteamRuntimeError, SteamRuntimeUser, downloads,
        publishing as steam_publishing, users as steam_users, workshop as steam_workshop,
    },
    transactions,
};

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

pub use context::*;
use correlation::{BackendTaskCancelResult, BackendTaskSource, BackendTransactionTasks};
use projections::{
    clear_directory_contents, publish_submission_from_app_request,
    search_full_batch_from_transaction_payload, search_quick_batch_from_backend,
    steam_user_from_backend, steam_user_from_workshop_backend, subscription_counts_from_items,
    workshop_item_from_backend,
};
pub use runtime_events::*;
pub use services::*;
pub use task_events::*;
pub use worker_runtime::*;

const BLOCKING_MIN_THREADS: usize = 2;
const BLOCKING_MAX_THREADS: usize = 8;
const BLOCKING_FALLBACK_THREADS: usize = 4;
const MEDIA_THREADS: usize = 16;
const BLOCKING_QUEUE_CAPACITY: usize = 256;
const TASK_EVENTS_ID: u64 = 1;
const BACKEND_EVENTS_ID: u64 = 2;
const BACKEND_EVENT_QUEUE_CAPACITY: usize = 256;

pub const DOWNLOAD_STATUS_DOWNLOADING: &str = "downloading";
pub const DOWNLOAD_STATUS_LOCATING: &str = "locating";
pub const EXTRACT_STATUS: &str = "extracting_progress";
const TRANSACTION_PROGRESS_SCALE: f64 = 10_000.0;
const MAX_PENDING_PRE_START_TRANSACTIONS: usize = 128;
const MAX_PENDING_PRE_START_EVENTS_PER_TRANSACTION: usize = 8;
