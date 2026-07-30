//! Everything gmpublished does that is not drawing: GMA archives, Steam, the
//! local addon library, and Source asset decoding.
//!
//! [`Backend`] is the composition root. Fallible operations return a typed
//! error implementing [`HasErrorKey`]; the UI maps that key to localized text,
//! so nothing here formats an error for display. Most entry points block and
//! belong off the main thread — `main_thread_forbidden!` asserts that in debug.
// These two disagree about `pub` in a private module; for a library the true
// public surface matters more than the shorter spelling.
#![warn(unreachable_pub)]
#![allow(clippy::redundant_pub_crate)]

mod error_key;
pub use error_key::keys as error_keys;
pub use error_key::{ErrorKey, HasErrorKey};

pub(crate) mod logging;
pub use logging::log_panic;
pub use logging::shutdown as shutdown_logging;

pub const GMOD_APP_ID: steamworks::AppId = steamworks::AppId(4000);

pub(crate) mod util;
pub use util::panic::payload_message as panic_payload_message;
pub use util::path;
pub(crate) use util::{stream_bytes, threads, write_nt_string};

pub type ArcBytes = std::sync::Arc<[u8]>;

mod execution;
pub use execution::{
    CpuExecutor, ExecutionConfig, ExecutionInitError, ExecutionResources, ExecutionScheduleError,
};

mod transactions;
pub use transactions::{
    FinalizeOutcome, Transaction, TransactionError, TransactionId, TransactionPayload,
    TransactionStatus,
};

mod rgba_image;
pub use rgba_image::RgbaImage;

mod workshop_id;
pub use workshop_id::WorkshopId;

mod io_failure;
pub use io_failure::IoFailure;

mod events;
#[cfg(feature = "test-support")]
pub use events::BackendEventCollector;
pub use events::{
    BackendEvent, BackendEventSink, DownloadStartedEvent, ExtractionStartedEvent, NullEventSink,
    TransactionEvent, WorkshopSnapshotId,
};

mod addon;
pub use addon::Addon;

pub mod bbcode;

mod appdata;
pub use appdata::{
    AppDataPathsSnapshot, AppDataSnapshot, Settings, SettingsEnvironment, SettingsError,
    TitlebarPreference, cache_dir, steam_client_installed, validate_gmod,
};

mod gma;
/// The GMA vocabulary, flattened so callers never have to know which
/// submodule a type happens to live in. The operational `gma` module remains
/// private; callers use these deliberate types and their inherent methods.
pub use gma::extract::{
    ExtractDestination, ExtractOptions, ExtractionContext, ExtractionOverwriteMode, Whitelist,
};
pub use gma::read::{GmaIndexBundle, GmaIndexedEntry, GmaMetaBundle, GmaView};
pub use gma::{
    GmaEntry, GmaError, GmaFile, GmaHeader, GmaMetadata, is_unsafe_entry_path, ws_id_from_file_name,
};

/// Stateless whitelist vocabulary used by app-side preflight presentation.
pub mod whitelist {
    pub use crate::gma::whitelist::{
        DEFAULT_IGNORE, is_default_ignored, is_ignored, is_whitelisted_in,
    };
}

pub mod vpk;

mod net;
pub use net::tls_agent_builder;
mod signal;

mod steam;
pub use steam::users::SteamUser;
pub use steam::workshop::{WorkshopItem, WorkshopPage, WorkshopQueryError};
pub use steam::{
    ConnectedSteam, SteamAvatarRgba, SteamId, SteamRuntime, SteamRuntimeError, SteamRuntimeStatus,
    SteamRuntimeUser,
};

/// Deliberate data-conversion API for app-owned publish requests.
pub mod publishing {
    pub use crate::steam::publishing::{
        PublishError, PublishSettingsSnapshot, PublishSubmission, PublishSubmissionMode,
        PublishSubmissionOutcome, WorkshopIcon,
    };
}

/// Connected Workshop queries used by the app's narrow Workshop service.
pub mod workshop {
    pub use crate::steam::workshop::{
        query_workshop_item_details, query_workshop_items, query_workshop_items_streaming,
    };
}

/// Connected Steam-user queries used by the app's narrow Workshop service.
pub mod steam_users {
    pub use crate::steam::users::{SteamUser, fetch_steam_user, fetch_steam_user_streaming};
}

mod search;
pub use search::{
    FileSearchAddon, QuickSearchHit, QuickSearchResult, SearchItem, SearchItemSource, SearchScope,
};

pub mod cli;

mod backend;
pub use backend::{
    Backend, BackendConfig, BackendInitError, BackgroundServices, BackgroundStartError,
    BackgroundStartOutcome,
};

/// Constructors and service handles needed only by integration fixtures.
#[cfg(feature = "test-support")]
pub mod test_support {
    pub use crate::appdata::{AppData, AppDataPaths};
    pub use crate::events::{BackendEventCollector, NullEventSink};
    pub use crate::gma::whitelist::AddonWhitelist;
    pub use crate::gma::whitelist::{is_default_ignored, is_ignored, is_whitelisted_in};
    pub use crate::steam::Steam;
    pub use crate::transactions::Transactions;
}
