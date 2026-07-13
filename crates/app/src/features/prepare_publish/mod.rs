mod effect;
mod message;
mod model;
mod state;
mod update;
mod view;

use iced::{Subscription, time};

pub use effect::Effect;
pub use message::Message;
pub use model::{
    ContentPathVerificationRequest, IconVerificationRequest, IgnorePatternMutation, IgnoredPattern,
    PublishIconSubmitRequestEnvelope, PublishSubmitContext, PublishSubmitRequestEnvelope,
    PublishSubmitResult, WorkshopContentRequest, apply_ignore_pattern_mutation,
    ignored_patterns_from_settings, inspect_workshop_snapshot, run_publish_icon_submit,
    run_publish_submit, verify_content_path, verify_icon_preview,
};
pub use state::{OpenTarget, State, UpdateTarget};
pub use update::update;
pub use view::view;

const SUBMIT_SPINNER_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);

/// Both clocks are strictly gated: the icon clock runs only while an animated
/// GIF icon is selected and the spinner clock only while a publish is running,
/// keeping the idle app at 0% CPU.
pub fn subscription(state: &State) -> Subscription<Message> {
    let mut clocks = Vec::with_capacity(2);
    if state.has_active_icon_animation() {
        clocks.push(
            time::every(crate::media::thumbnail_animation::ANIMATION_TICK_INTERVAL)
                .map(Message::IconAnimationTick),
        );
    }
    if state.submit_pending() {
        clocks.push(time::every(SUBMIT_SPINNER_TICK_INTERVAL).map(Message::SubmitSpinnerTick));
    }
    Subscription::batch(clocks)
}
