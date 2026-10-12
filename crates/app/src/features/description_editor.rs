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

/// Tick only while the editor is open, focused, and playing an animation.
pub fn subscription(state: &State) -> Subscription<Message> {
    if state.has_active_animation() {
        time::every(crate::media::thumbnail_animation::ANIMATION_TICK_INTERVAL)
            .map(|_| Message::AnimationTick)
    } else {
        Subscription::none()
    }
}
