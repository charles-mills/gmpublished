#![warn(unreachable_pub)]
// Keep pub(crate) in private modules to satisfy unreachable_pub.
#![allow(clippy::redundant_pub_crate)]

mod error_key;
pub use error_key::keys as error_keys;
pub use error_key::{ErrorKey, HasErrorKey};

pub(crate) mod logging;
pub use logging::log_panic;
pub use logging::shutdown as shutdown_logging;

pub(crate) const GMOD_APP_ID: u32 = 4000;
pub(crate) const STEAM_GMOD_APP_ID: steamworks::AppId = steamworks::AppId(GMOD_APP_ID);

pub(crate) mod util;
pub use util::panic::payload_message as panic_payload_message;
pub use util::path;
pub(crate) use util::{stream_bytes, threads, write_nt_string};

pub type ArcBytes = std::sync::Arc<[u8]>;

#[allow(missing_docs)]
pub mod source_discovery;

mod execution;
pub use execution::{
    CpuExecutor, ExecutionConfig, ExecutionInitError, ExecutionResources, ExecutionScheduleError,
};

mod transactions;
pub use transactions::{
    EmitOutcome, FinalizeOutcome, Transaction, TransactionError, TransactionId, TransactionPayload,
    TransactionStatus,
};

mod rgba_image;
pub use rgba_image::RgbaImage;

mod workshop_language;
pub use workshop_language::WorkshopLanguage;

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
    TitlebarPreference, cache_dir, steam_client_installed, steam_library_common_dirs,
    validate_gmod,
};

mod gma;
pub use gma::extract::{
    ExtractDestination, ExtractOptions, ExtractionContext, ExtractionOverwriteMode, Whitelist,
};
pub use gma::read::{GmaIndexBundle, GmaIndexedEntry, GmaMetaBundle, GmaView};
pub use gma::{
    GMA_VERSION, GmaEntry, GmaError, GmaFile, GmaHeader, GmaMetadata, is_unsafe_entry_path,
    ws_id_from_file_name,
};

pub mod whitelist {
    pub use crate::gma::whitelist::{
        DEFAULT_IGNORE, is_default_ignored, is_ignored, is_whitelisted_in,
    };
}

pub mod vpk;

mod net;
pub use net::{HttpAgentConfig, build_http_agent};
mod signal;

mod steam;
pub use steam::users::SteamUser;
pub use steam::workshop::{WorkshopItem, WorkshopPage, WorkshopQueryError};
pub use steam::{
    ConnectedSteam, SteamAvatarRgba, SteamId, SteamRuntime, SteamRuntimeError, SteamRuntimeStatus,
    SteamRuntimeUser,
};

pub mod publishing {
    pub use crate::steam::publishing::{
        PublishError, PublishSettingsSnapshot, PublishSubmission, PublishSubmissionMode,
        PublishSubmissionOutcome, WORKSHOP_DEFAULT_DESCRIPTION, WORKSHOP_DESCRIPTION_MAX_BYTES,
        WORKSHOP_LEGACY_DEFAULT_DESCRIPTION, WorkshopIcon, sanitize_description,
        workshop_icon_can_upscale,
    };
}

pub mod workshop {
    pub use crate::steam::workshop::{
        query_workshop_item_details, query_workshop_items, query_workshop_items_streaming,
    };
}

pub mod steam_users {
    pub use crate::steam::users::{fetch_steam_user, fetch_steam_user_streaming};
}

mod search;
pub use search::{
    FileSearchAddon, QuickSearchHit, QuickSearchResult, SearchItem, SearchItemSource, SearchScope,
};

mod backend;
pub use backend::{
    Backend, BackendConfig, BackendInitError, BackgroundServices, BackgroundStartError,
    BackgroundStartOutcome,
};

#[cfg(feature = "test-support")]
pub mod test_support {
    pub use crate::appdata::{AppData, AppDataPaths};
    pub use crate::events::{BackendEventCollector, NullEventSink};
    pub use crate::gma::whitelist::AddonWhitelist;
    pub use crate::steam::Steam;
    pub use crate::transactions::Transactions;
}
