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

pub const GMOD_APP_ID: steamworks::AppId = steamworks::AppId(4000);

pub(crate) mod util;
pub use util::path;
pub(crate) use util::{stream_bytes, write_nt_string};

pub type ArcBytes = std::sync::Arc<[u8]>;

pub mod transactions;
pub use transactions::Transaction;

pub mod rgba_image;
pub use rgba_image::RgbaImage;

pub mod events;

pub mod addon;
pub use addon::Addon;

pub mod bbcode;

pub mod appdata;
pub use appdata::AppData;

pub mod gma;
pub use gma::{GMAError, GMAFile, GMAHeader, GMAMetadata};

pub mod vpk;

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
