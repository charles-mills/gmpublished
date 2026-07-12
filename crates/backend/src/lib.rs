pub mod error_key;
pub use error_key::{ErrorKey, HasErrorKey};

pub(crate) mod logging;

pub const GMOD_APP_ID: steamworks::AppId = steamworks::AppId(4000);

#[macro_use]
pub(crate) mod util;
pub use util::{ArcBytes, path};
pub(crate) use util::{NTStringWriter, stream_bytes};

#[macro_use]
pub mod transactions;
pub use transactions::Transaction;

pub mod rgba_image;
pub use rgba_image::RgbaImage;

pub mod events;

pub mod addon;
pub use addon::Addon;

pub mod appdata;
pub use appdata::AppData;

// Only exercised by its own `#[cfg(test)]` warm-hydration coverage — no
// production caller remains.
#[cfg(test)]
mod discovery_snapshot;

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
pub use backend::{Backend, BackendConfig, BackendInitError};
