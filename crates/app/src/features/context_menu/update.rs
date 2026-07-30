use std::time::Instant;

use iced::{Event, Subscription, event, keyboard};

use super::{Effect, Message, State};

pub fn update_at(state: &mut State, message: Message, now: Instant) -> Vec<Effect> {
    match message {
        Message::OpenRequested(request) => {
            state.open_request(request, now);
            Vec::new()
        }
        Message::ActionSelected(action) => {
            state.dismiss(now);
            vec![Effect::ActionSelected(action)]
        }
        Message::DismissRequested | Message::EscapePressed => {
            state.dismiss(now);
            vec![Effect::Dismissed]
        }
    }
}

#[cfg(test)]
fn update(state: &mut State, message: Message) -> Vec<Effect> {
    update_at(state, message, Instant::now())
}

pub fn subscription(state: &State) -> Subscription<Message> {
    if state.open() {
        event::listen_with(keyboard_event)
    } else {
        Subscription::none()
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "must match iced's fn(Event, Status, window::Id) callback signature required by event::listen_with"
)]
fn keyboard_event(
    event: Event,
    _status: event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        }) => Some(Message::EscapePressed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use iced::Point;

    use super::*;
    use crate::features::context_menu::{
        ContextMenuAction, ContextMenuTarget, Entry, LocalMenuTarget, OpenRequest,
        accepts_pointer_input,
    };
    use std::path::PathBuf;

    fn target() -> ContextMenuTarget {
        ContextMenuTarget::Local(LocalMenuTarget {
            path: PathBuf::from("/tmp/addon.gma"),
            path_text: "/tmp/addon.gma".to_owned(),
            workshop_id: None,
            workshop_url: None,
            preview_url: None,
        })
    }

    /// Requested entries plus the debug-only toast simulators appended to
    /// every menu (a separator and three actions).
    fn expected_len(requested: usize) -> usize {
        if cfg!(feature = "debug") {
            requested + 4
        } else {
            requested
        }
    }

    #[test]
    fn open_request_replaces_current_entries() {
        let mut state = State::default();

        let _effects = update(
            &mut state,
            Message::OpenRequested(OpenRequest::new(
                Point::new(14.0, 28.0),
                vec![Entry::copy_path()],
                target(),
            )),
        );

        assert!(state.open());
        assert!(state.visible());
        assert_eq!(state.position(), Point::new(14.0, 28.0));
        assert_eq!(state.entries().len(), expected_len(1));
    }

    #[test]
    fn selecting_action_closes_menu() {
        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::OpenRequested(OpenRequest::new(
                Point::ORIGIN,
                vec![Entry::copy_path()],
                target(),
            )),
        );

        let _effects = update(
            &mut state,
            Message::ActionSelected(ContextMenuAction::CopyPath),
        );

        assert!(!state.open());
        assert!(state.visible());
        // The entries remain mounted for the fade, but the action target is
        // one-shot so a queued second click cannot dispatch it twice.
        assert!(state.take_target().is_some());
        assert!(state.take_target().is_none());
        assert!(!accepts_pointer_input(&state));
        assert_eq!(state.entries().len(), expected_len(1));
    }

    #[test]
    fn tick_clears_entries_after_dismiss_animation() {
        let mut state = State::default();
        let now = std::time::Instant::now();
        state.open_request(
            OpenRequest::new(Point::ORIGIN, vec![Entry::copy_path()], target()),
            now,
        );

        state.dismiss(now + std::time::Duration::from_millis(1));
        assert!(state.tick(now + std::time::Duration::from_millis(300)));

        assert!(!state.visible());
        assert!(state.target().is_none());
        assert!(state.entries().is_empty());
    }

    #[test]
    fn close_keeps_tick_gate_alive_until_finalizing_tick() {
        let mut state = State::default();
        let now = std::time::Instant::now();
        state.open_request(
            OpenRequest::new(Point::ORIGIN, vec![Entry::copy_path()], target()),
            now,
        );
        assert!(state.needs_ticks());
        assert!(!state.tick(now + std::time::Duration::from_millis(300)));
        assert!(!state.needs_ticks());

        state.dismiss(now + std::time::Duration::from_millis(400));
        let after_animation = now + std::time::Duration::from_millis(700);

        assert!(!state.open());
        assert!(state.visible());
        assert!(!accepts_pointer_input(&state));
        assert!(state.needs_ticks());
        assert!(state.tick(after_animation));
        assert!(!state.needs_ticks());
        assert!(!state.visible());
    }
}
