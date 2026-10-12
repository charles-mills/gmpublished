mod message;
mod state;
mod view;

use std::time::Duration;

use iced::{Subscription, time};

use crate::features::shell::Route;
use crate::features::steam_session::ConnectionStatus;

pub use message::{Action, Message};
pub use state::{Blocker, GameStatus, Requirement, State, blocker_for};
pub use view::view;

/// How often a visible Steam panel reconnects on its own. `connect()` carries
/// its own 2s timeout, so this is the gap between attempts rather than a
/// second timeout stacked on top.
const STEAM_RETRY_INTERVAL: Duration = Duration::from_secs(5);

#[must_use]
pub const fn requires_steam(route: Route) -> bool {
    matches!(Requirement::of(route), Requirement::Steam)
}

pub fn subscription(route: Route, steam: ConnectionStatus) -> Subscription<Message> {
    if steam != ConnectionStatus::Unavailable || Requirement::of(route) != Requirement::Steam {
        return Subscription::none();
    }

    time::every(STEAM_RETRY_INTERVAL).map(|_| Message::Activated(Action::RetrySteam))
}
