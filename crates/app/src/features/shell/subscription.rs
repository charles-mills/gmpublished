use iced::{Event, Subscription, event, keyboard};

use super::{Message, State};

/// Returns shell event streams, gated so idle shell chrome does not poll.
pub fn subscription(state: &State) -> Subscription<Message> {
    let mut streams = Vec::new();

    if state.account_menu_open() {
        streams.push(event::listen_with(account_menu_keyboard_event));
    }

    Subscription::batch(streams)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "must match iced's fn(Event, Status, window::Id) callback signature required by event::listen_with"
)]
fn account_menu_keyboard_event(
    event: Event,
    _status: event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        }) => Some(Message::AccountMenuDismissed),
        _ => None,
    }
}
