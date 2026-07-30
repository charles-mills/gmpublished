use std::time::Instant;

use super::{Effect, Message, State};

pub fn update_at(state: &mut State, message: Message, now: Instant) -> Vec<Effect> {
    match message {
        Message::TaskEventsReceived(events) => {
            let _changed = state.apply_task_events(events, now);
            Vec::new()
        }
        Message::CancelPressed(task_id) => vec![Effect::CancelRequested(task_id)],
    }
}
