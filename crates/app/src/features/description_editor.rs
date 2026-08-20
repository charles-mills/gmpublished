//! Full-window BBCode description editor for Workshop items: raw source on
//! the left, a pixel-honest Steam Workshop preview on the right, saved
//! through a metadata-only UGC revision.

mod effect;
mod markup;
mod message;
mod state;
mod update;
mod view;

pub use effect::{Effect, SaveRequest, SourceRequest};
use iced::{Subscription, time};
pub use message::{FetchedSource, Message, SaveOutcome};
pub use state::State;
pub use update::update_at;
pub use view::view;

/// Animation clock for GIFs playing in the live preview: runs only while the
/// editor is open, the window is focused, and something actually animates.
pub fn subscription(state: &State) -> Subscription<Message> {
    if state.has_active_animation() {
        time::every(crate::media::thumbnail_animation::ANIMATION_TICK_INTERVAL)
            .map(|_| Message::AnimationTick)
    } else {
        Subscription::none()
    }
}
