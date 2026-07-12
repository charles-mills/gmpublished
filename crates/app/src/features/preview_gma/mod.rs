mod details;
mod effect;
mod message;
mod model;
mod state;
mod update;
mod view;

pub use effect::Effect;
use iced::{Subscription, time};
pub use message::Message;

pub use model::{
    AuthorRequest, ExtractionIntent, ExtractionRequest, LoadedArchive, MetadataRequest,
    OpenRequest, OpenSeed, OpenTarget, query_steam_user, query_workshop_metadata,
};
pub use state::State;
pub use update::{nav_path_scrollable_id, update};
pub use view::view;

/// Animation clock for an open Preview GMA modal: runs while the thumbnail
/// animates or the loading spinner is visible, never while idle.
pub fn subscription(state: &State) -> Subscription<Message> {
    if state.has_active_animation() || state.spinner_visible() {
        time::every(crate::media::thumbnail_animation::ANIMATION_TICK_INTERVAL)
            .map(Message::AnimationTick)
    } else {
        Subscription::none()
    }
}
