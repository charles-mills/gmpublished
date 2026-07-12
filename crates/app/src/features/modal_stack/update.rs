use std::time::Instant;

use iced::{Event, Subscription, event, keyboard};

use super::{ActiveModal, Effect, Message, State};

pub fn update(state: &mut State, message: &Message) -> Vec<Effect> {
    let now = Instant::now();
    match message {
        Message::OpenDestinationSelect => state.open(ActiveModal::DestinationSelect, now),
        Message::OpenPreparePublish => state.open(ActiveModal::PreparePublish, now),
        Message::OpenPreviewGma => state.open(ActiveModal::PreviewGma, now),
        Message::OpenSettings => state.open(ActiveModal::Settings, now),
        Message::CloseRequested => state.close(now),
    }
    Vec::new()
}

/// Overlay modals always close on Escape. For the base layer, Settings is
/// excluded: it owns a layered Escape handler that must unwind pickers and
/// confirmations before closing the modal itself.
pub fn subscription(state: &State) -> Subscription<Message> {
    if listens_for_escape(state) {
        event::listen_with(escape_event)
    } else {
        Subscription::none()
    }
}

fn listens_for_escape(state: &State) -> bool {
    state.overlay_active()
        || state
            .active()
            .is_some_and(|modal| modal != ActiveModal::Settings)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "must match iced's fn(Event, Status, window::Id) callback signature required by event::listen_with"
)]
fn escape_event(
    event: Event,
    _status: event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        }) => Some(Message::CloseRequested),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{ActiveModal, Message, State, update};

    #[test]
    fn open_destination_select_layers_over_the_base_modal() {
        let mut state = State::default();

        let _effects = update(&mut state, &Message::OpenPreviewGma);
        let _effects = update(&mut state, &Message::OpenDestinationSelect);

        assert_eq!(state.active(), Some(ActiveModal::PreviewGma));
        assert_eq!(state.overlay_modal(), Some(ActiveModal::DestinationSelect));
        assert!(state.overlay_active());
        assert!(state.has_active_modal());
    }

    #[test]
    fn open_destination_select_works_without_a_base_modal() {
        let mut state = State::default();

        let _effects = update(&mut state, &Message::OpenDestinationSelect);

        assert_eq!(state.active(), None);
        assert!(state.overlay_active());
        assert!(state.has_active_modal());
    }

    #[test]
    fn base_modals_swap_within_the_base_layer() {
        let mut state = State::default();

        let _effects = update(&mut state, &Message::OpenPreviewGma);
        let _effects = update(&mut state, &Message::OpenPreparePublish);
        assert_eq!(state.active(), Some(ActiveModal::PreparePublish));

        let _effects = update(&mut state, &Message::OpenSettings);
        assert_eq!(state.active(), Some(ActiveModal::Settings));
        assert!(!state.overlay_active());
    }

    #[test]
    fn close_requested_closes_the_overlay_before_the_base() {
        let mut state = State::default();
        let now = Instant::now();

        state.open(ActiveModal::PreviewGma, now);
        state.open(ActiveModal::DestinationSelect, now);

        state.close(now + Duration::from_millis(1));
        assert_eq!(
            state.tick(now + Duration::from_millis(300)),
            vec![ActiveModal::DestinationSelect]
        );
        assert!(!state.overlay_active());
        assert_eq!(state.active(), Some(ActiveModal::PreviewGma));

        state.close(now + Duration::from_millis(260));
        assert_eq!(
            state.tick(now + Duration::from_millis(600)),
            vec![ActiveModal::PreviewGma]
        );
        assert_eq!(state.active(), None);
        assert!(!state.has_active_modal());
    }

    #[test]
    fn overlay_alone_closes_cleanly() {
        let mut state = State::default();
        let now = Instant::now();

        state.open(ActiveModal::DestinationSelect, now);
        state.close(now + Duration::from_millis(1));

        assert!(state.overlay_active());
        assert!(!state.overlay_interactive());
        assert_eq!(
            state.tick(now + Duration::from_millis(300)),
            vec![ActiveModal::DestinationSelect]
        );
        assert!(!state.has_active_modal());
    }

    #[test]
    fn both_layers_finish_closing_in_one_tick() {
        let mut state = State::default();
        let now = Instant::now();

        state.open(ActiveModal::PreviewGma, now);
        state.open(ActiveModal::DestinationSelect, now);
        state.close(now + Duration::from_millis(1));
        state.close(now + Duration::from_millis(2));

        assert_eq!(
            state.tick(now + Duration::from_millis(400)),
            vec![ActiveModal::DestinationSelect, ActiveModal::PreviewGma]
        );
        assert!(!state.has_active_modal());
    }

    #[test]
    fn base_close_leaves_an_open_overlay_untouched() {
        let mut state = State::default();
        let now = Instant::now();

        state.open(ActiveModal::PreviewGma, now);
        state.open(ActiveModal::DestinationSelect, now);
        state.close(now + Duration::from_millis(1));
        state.open(
            ActiveModal::DestinationSelect,
            now + Duration::from_millis(2),
        );

        assert_eq!(state.tick(now + Duration::from_millis(400)), vec![]);
        assert!(state.overlay_active());
        assert_eq!(state.active(), Some(ActiveModal::PreviewGma));
    }

    #[test]
    fn open_reverses_a_pending_close_animation() {
        let mut state = State::default();
        let now = Instant::now();

        state.open(ActiveModal::PreviewGma, now);
        state.close(now + Duration::from_millis(20));
        state.open(ActiveModal::PreparePublish, now + Duration::from_millis(40));

        assert_eq!(state.active(), Some(ActiveModal::PreparePublish));
        assert_eq!(state.tick(now + Duration::from_millis(250)), vec![]);
        assert_eq!(state.active(), Some(ActiveModal::PreparePublish));
    }

    #[test]
    fn close_keeps_tick_gate_alive_until_finalizing_tick() {
        let mut state = State::default();
        let now = Instant::now();

        state.open(ActiveModal::PreviewGma, now);
        assert!(state.needs_ticks());
        assert_eq!(state.tick(now + Duration::from_millis(300)), vec![]);
        assert!(!state.needs_ticks());

        state.close(now + Duration::from_millis(400));
        let after_animation = now + Duration::from_millis(700);

        assert!(state.active().is_some());
        assert!(!state.interactive());
        assert!(state.needs_ticks());
        assert_eq!(state.tick(after_animation), vec![ActiveModal::PreviewGma]);
        assert!(!state.needs_ticks());
        assert_eq!(
            state.tick(after_animation + Duration::from_millis(1)),
            vec![]
        );
        assert_eq!(state.active(), None);
    }

    #[test]
    fn each_layer_scrim_tracks_its_own_presence() {
        let mut state = State::default();
        let now = Instant::now();

        state.open(ActiveModal::Settings, now);

        assert!(state.is_animating(now + Duration::from_millis(80)));
        let opacity = state.opacity(now + Duration::from_millis(80));
        assert!(opacity > 0.0 && opacity < 1.0);
        assert_eq!(state.opacity(now + Duration::from_millis(300)), 1.0);

        // A standalone overlay drives its own scrim; the base dim stays off.
        let mut overlay_only = State::default();
        overlay_only.open(ActiveModal::DestinationSelect, now);
        assert_eq!(overlay_only.opacity(now + Duration::from_millis(300)), 0.0);
        assert_eq!(
            overlay_only.overlay_opacity(now + Duration::from_millis(300)),
            1.0
        );
        let mid = overlay_only.overlay_opacity(now + Duration::from_millis(80));
        assert!(mid > 0.0 && mid < 1.0);
    }

    #[test]
    fn escape_subscription_follows_the_topmost_layer() {
        let now = Instant::now();

        let closed = State::default();
        assert!(!super::listens_for_escape(&closed));

        let mut settings = State::default();
        settings.open(ActiveModal::Settings, now);
        assert!(!super::listens_for_escape(&settings));

        let mut publish = State::default();
        publish.open(ActiveModal::PreparePublish, now);
        assert!(super::listens_for_escape(&publish));

        // The overlay always listens, even over Settings.
        let mut overlay_over_settings = State::default();
        overlay_over_settings.open(ActiveModal::Settings, now);
        overlay_over_settings.open(ActiveModal::DestinationSelect, now);
        assert!(super::listens_for_escape(&overlay_over_settings));

        let mut overlay_only = State::default();
        overlay_only.open(ActiveModal::DestinationSelect, now);
        assert!(super::listens_for_escape(&overlay_only));
    }
}
