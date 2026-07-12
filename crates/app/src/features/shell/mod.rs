//! Shared shell chrome for the Iced app.

mod effect;
mod message;
mod startup;
mod state;
mod subscription;
mod update;
mod view;

pub const UPSTREAM_REPO_URL: &str = "https://github.com/WilliamVenner/gmpublisher";

pub use effect::Effect;
pub use message::Message;
pub use startup::{UpdateCheckError, fetch_latest_update};
pub use state::{ChromeStrategy, Route, State, UpdateRelease, sidebar_rail_width, sidebar_width};
#[cfg(target_os = "macos")]
pub use state::{traffic_light_center_y, traffic_light_origin_x};
pub use subscription::subscription;
pub use update::update;
pub use view::{account_menu_overlay, sidebar};
