use super::{Effect, Message, State, state::ConnectionChange};

/// Applies a Steam session message and returns any follow-up effects.
pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    match message {
        Message::ConnectionEvent(event) => {
            let change = state.apply_connection_event(event);
            connection_follow_up_effects(state, change)
        }
        Message::ConnectionAttemptCompleted(attempt) => {
            let mut effects = Vec::new();
            for event in attempt.events() {
                let change = state.apply_connection_event(*event);
                if matches!(change, ConnectionChange::BecameConnected) {
                    let generation = state.start_identity_fetch();
                    effects.push(Effect::IdentityFetchRequested(generation));
                }
            }
            effects
        }
        Message::PendingRetrySet(retry) => {
            state.push_pending_retry(retry);
            Vec::new()
        }
        Message::IdentityFetched(generation, result) => {
            state.apply_identity_result(generation, result);
            Vec::new()
        }
    }
}

fn connection_follow_up_effects(state: &mut State, change: ConnectionChange) -> Vec<Effect> {
    if matches!(change, ConnectionChange::BecameConnected) {
        let generation = state.start_identity_fetch();
        return vec![Effect::IdentityFetchRequested(generation)];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::{Message, State, update};
    use crate::bridge::domain::PublishedFileId;
    use crate::features::steam_session::{ConnectionEvent, ConnectionStatus, PendingRetry};
    use crate::generation::Generation;

    #[test]
    fn connection_event_updates_status() {
        let mut state = State::default();

        let _effects = update(
            &mut state,
            Message::ConnectionEvent(ConnectionEvent::Connecting),
        );

        assert_eq!(state.status(), ConnectionStatus::Connecting);
    }

    #[test]
    fn connected_event_starts_identity_generation() {
        let mut state = State::default();

        let _effects = update(
            &mut state,
            Message::ConnectionEvent(ConnectionEvent::Connected),
        );

        assert_eq!(state.status(), ConnectionStatus::Connected);
        assert_eq!(state.identity_generation(), Generation::from_raw(1));
    }

    #[test]
    fn pending_retry_message_queues_the_retry() {
        let mut state = State::default();
        let retry = PendingRetry::InstalledMetadata {
            generation: Generation::from_raw(3),
            item_ids: vec![PublishedFileId::fixture(10), PublishedFileId::fixture(20)],
        };

        let _effects = update(&mut state, Message::PendingRetrySet(retry.clone()));

        assert_eq!(state.pending_retries(), [retry]);
    }

    #[test]
    fn completed_attempt_applies_all_connection_events() {
        let mut state = State::default();
        let attempt = crate::features::steam_session::ConnectionAttempt::unavailable(
            crate::bridge::ui_error::UiError::detailed(
                gmpublished_backend::error_key::keys::STEAM_ERROR,
                Some("down".to_owned()),
            ),
        );

        let _effects = update(&mut state, Message::ConnectionAttemptCompleted(attempt));

        assert_eq!(
            state.status(),
            crate::features::steam_session::ConnectionStatus::Unavailable
        );
    }
}
