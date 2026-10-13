mod effect;
mod message;
mod orbit;
mod particles3d;
mod state;
mod update;
mod view;
mod viewer3d;

pub use effect::Effect;
use iced::{Subscription, time};
pub use message::Message;
pub use state::State;
pub use update::update;
pub use view::pane;

pub fn embedded_expanded(state: &State) -> bool {
    state.is_open() && state.expanded()
}

pub fn subscription(state: &State) -> Subscription<Message> {
    if state.spinner_visible()
        || state.audio_playing()
        || state.model_playing()
        || state.fly_speed_readout_visible()
        || state.stage_falling()
    {
        time::every(crate::media::thumbnail_animation::ANIMATION_TICK_INTERVAL)
            .map(Message::AnimationTick)
    } else {
        Subscription::none()
    }
}
