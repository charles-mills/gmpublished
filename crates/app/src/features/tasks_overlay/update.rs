use std::time::Instant;

use super::{Effect, Message, State};

pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    match message {
        Message::TaskEventsReceived(events) => {
            let _changed = state.apply_task_events(events, Instant::now());
            Vec::new()
        }
        Message::CancelPressed(task_id) => vec![Effect::CancelRequested(task_id)],
    }
}
