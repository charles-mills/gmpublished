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
    OpenRequest, OpenSeed, OpenTarget, author_info_from_user, cached_workshop_metadata,
    workshop_metadata_from_details,
};
pub use state::State;
pub use update::{browser_rows_scrollable_id, nav_path_scrollable_id, update_at};
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
