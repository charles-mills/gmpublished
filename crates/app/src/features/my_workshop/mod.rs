mod effect;
mod message;
mod model;
mod state;
mod update;
mod view;

use iced::{Subscription, time};

pub use effect::Effect;
pub use message::Message;
#[cfg(all(test, feature = "debug"))]
pub use model::Row;
pub use model::{
    ContextMenuRequest, PreparePublishTarget, browse_page, refresh_subscription_counts,
};
pub use state::State;
pub use update::update;
pub use view::{GRID_KEY, view};

pub fn subscription(state: &State) -> Subscription<Message> {
    if !state.is_route_visible() {
        return Subscription::none();
    }

    let mut subscriptions =
        vec![time::every(model::STATS_REFRESH_INTERVAL).map(|_| Message::StatsRefreshTick)];
    if state.has_active_count_rolls() {
        subscriptions
            .push(time::every(model::COUNT_ROLL_TICK_INTERVAL).map(Message::CountRollTick));
    }
    if state.needs_card_motion_ticks() {
        subscriptions
            .push(crate::theme::motion::redraw_subscription(true).map(Message::AnimationTick));
    } else if state.has_active_animations() {
        // Only GIF playback is live: tick at the GIF cadence, not 60Hz.
        subscriptions
            .push(crate::theme::motion::gif_redraw_subscription().map(Message::AnimationTick));
    }

    Subscription::batch(subscriptions)
}
