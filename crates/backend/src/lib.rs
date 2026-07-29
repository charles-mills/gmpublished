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

pub mod error_key;
pub use error_key::{ErrorKey, HasErrorKey};

pub(crate) mod logging;
pub use logging::log_panic;
pub use logging::shutdown as shutdown_logging;

pub const GMOD_APP_ID: steamworks::AppId = steamworks::AppId(4000);

pub(crate) mod util;
pub use util::panic::payload_message as panic_payload_message;
pub use util::path;
pub use util::threads;
pub(crate) use util::{stream_bytes, write_nt_string};

pub type ArcBytes = std::sync::Arc<[u8]>;

pub mod transactions;
pub use transactions::Transaction;

pub mod rgba_image;
pub use rgba_image::RgbaImage;

pub mod workshop_id;
pub use workshop_id::WorkshopId;

pub mod io_failure;
pub use io_failure::IoFailure;

pub mod events;

pub mod addon;
pub use addon::Addon;

pub mod bbcode;

pub mod appdata;
pub use appdata::AppData;

pub mod gma;
/// The GMA vocabulary, flattened so callers never have to know which
/// submodule a type happens to live in. Everything the app crate reaches for
/// is here; `gma::` itself stays public for the operations, not the types.
pub use gma::read::{GmaIndexedEntry, GmaView};
pub use gma::{GmaEntry, GmaError, GmaFile, GmaHeader, GmaMetadata};

pub mod vpk;

/// Vector maths shared by the scene, particle and viewer code.
pub mod math;
pub mod net;
pub mod signal;

#[cfg(feature = "scene")]
pub mod scene;

#[cfg(feature = "scene")]
pub mod particles;

pub mod steam;
pub use steam::workshop::WorkshopItem;

pub mod search;

pub mod cli;

pub mod backend;
pub use backend::{Backend, BackendConfig, BackendInitError, BackgroundServices};
