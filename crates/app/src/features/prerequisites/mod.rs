//! The two things gmpublished needs from the machine it runs on: a live
//! Steam session, and a Garry's Mod installation.
//!
//! They fail independently — Steam can be running with no game installed,
//! and the game can be installed with Steam closed — so each route reports
//! only the prerequisite it actually depends on, and never mentions the
//! other. My Workshop lives on Steam's servers and doesn't care whether the
//! game is installed; Size Analyzer reads bytes off disk and doesn't care
//! whether Steam is awake.

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

/// Whether this route's content comes from Steam.
#[must_use]
pub const fn requires_steam(route: Route) -> bool {
    matches!(Requirement::of(route), Requirement::Steam)
}

/// Reconnects in the background while a Steam-dependent route is showing the
/// offline panel. Nothing narrates it: the panel simply stops being there.
///
/// Deliberately scoped to the visible route. A background poll running while
/// the user is measuring addon sizes spends threads to answer a question
/// nobody is asking, and the route they switch to re-evaluates on arrival.
pub fn subscription(route: Route, steam: ConnectionStatus) -> Subscription<Message> {
    if steam != ConnectionStatus::Unavailable || Requirement::of(route) != Requirement::Steam {
        return Subscription::none();
    }

    time::every(STEAM_RETRY_INTERVAL).map(|_| Message::Activated(Action::RetrySteam))
}
