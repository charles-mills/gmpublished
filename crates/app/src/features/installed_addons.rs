//! Installed Addons route surface for the Iced application.

mod effect;
mod message;
mod model;
mod state;
mod update;
mod view;

use iced::Subscription;

pub use effect::Effect;
pub use message::Message;
pub use model::{
    ContextMenuRequest, PreviewTarget, refresh_metadata_streaming, resolve_metadata,
    rows_from_snapshot,
};
pub use state::State;
pub use update::update;
pub use view::{GRID_KEY, view};

pub fn subscription(state: &State) -> Subscription<Message> {
    if state.needs_card_motion_ticks() {
        crate::theme::motion::redraw_subscription(true).map(Message::AnimationTick)
    } else if state.has_active_animations() {
        // Only GIF playback is live: tick at the GIF cadence, not 60Hz.
        crate::theme::motion::gif_redraw_subscription().map(Message::AnimationTick)
    } else {
        Subscription::none()
    }
}
