use crate::backend::domain::{AvatarRgba, PublishedFileId, SteamUser};
use crate::backend::tasks::BackendServices;
use crate::backend::ui_error::UiError;

/// Startup connection policy retained for the Iced shell.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StartupConnectPolicy {
    /// Allow Steam-backed work to request a lazy connection.
    #[default]
    Lazy,
    /// Ignore lazy connection requests, used by measurement modes.
    #[cfg(test)]
    Suppressed,
}

impl StartupConnectPolicy {
    pub(crate) const fn allows_lazy_requests(self) -> bool {
        match self {
            Self::Lazy => true,
            #[cfg(test)]
            Self::Suppressed => false,
        }
    }
}

/// Connection status shown by the shell Steam indicator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Unavailable,
}

impl ConnectionStatus {
    pub(crate) const fn connected(self) -> bool {
        matches!(self, Self::Connected)
    }

    #[cfg(test)]
    pub(crate) const fn kind(self) -> i32 {
        match self {
            Self::Disconnected => 0,
            Self::Connecting => 1,
            Self::Connected => 2,
            Self::Unavailable => 3,
        }
    }

    #[cfg(test)]
    pub(crate) const fn translation_key(self) -> &'static str {
        match self {
            Self::Disconnected => "steam_disconnected",
            Self::Connecting => "steam_connecting",
            Self::Connected => "steam_connected",
            Self::Unavailable => "steam_unavailable",
        }
    }
}

/// Lifecycle events emitted by a lazy connection attempt or the backend runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionEvent {
    Connecting,
    Connected,
    Disconnected,
    Unavailable,
}

/// Edge classification produced when a connection status changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionChange {
    Unchanged,
    StatusChanged,
    BecameConnected,
    BecameDisconnected,
}

/// A Steam-backed operation to retry after a successful lazy connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PendingRetry {
    MyWorkshopPage {
        generation: u64,
        page: u32,
    },
    MyWorkshopStats {
        generation: u64,
        pages: u32,
    },
    InstalledMetadata {
        generation: u64,
        item_ids: Vec<PublishedFileId>,
    },
    InstalledMetadataRefresh {
        generation: u64,
        item_ids: Vec<PublishedFileId>,
    },
    SearchMetadataRefresh {
        generation: u64,
        item_ids: Vec<PublishedFileId>,
    },
}

/// Persona information fetched after a Steam connection is established.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamIdentity {
    name: String,
    avatar: Option<AvatarRgba>,
}

impl SteamIdentity {
    pub(crate) fn from_user(user: SteamUser) -> Self {
        Self {
            name: user.name,
            avatar: user.avatar,
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) const fn avatar(&self) -> Option<&AvatarRgba> {
        self.avatar.as_ref()
    }
}

/// State owned by the Steam session feature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct State {
    status: ConnectionStatus,
    startup_policy: StartupConnectPolicy,
    identity: Option<SteamIdentity>,
    identity_generation: u64,
    pending_retry: Option<PendingRetry>,
    warm_connect_attempted: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            status: ConnectionStatus::Disconnected,
            startup_policy: StartupConnectPolicy::Lazy,
            identity: None,
            identity_generation: 0,
            pending_retry: None,
            warm_connect_attempted: false,
        }
    }
}

impl State {
    pub(crate) const fn status(&self) -> ConnectionStatus {
        self.status
    }

    #[cfg(test)]
    pub(crate) const fn identity_generation(&self) -> u64 {
        self.identity_generation
    }

    pub(crate) const fn identity(&self) -> Option<&SteamIdentity> {
        self.identity.as_ref()
    }

    #[cfg(test)]
    pub(crate) const fn pending_retry(&self) -> Option<&PendingRetry> {
        self.pending_retry.as_ref()
    }

    #[cfg(test)]
    pub(super) fn apply_core_connected(&mut self, connected: bool) -> ConnectionChange {
        let change = if connected {
            self.set_status(ConnectionStatus::Connected)
        } else if self.status == ConnectionStatus::Connected {
            self.set_status(ConnectionStatus::Disconnected)
        } else {
            ConnectionChange::Unchanged
        };
        self.apply_transition_cleanup(change);
        change
    }

    pub(super) fn apply_connection_event(&mut self, event: ConnectionEvent) -> ConnectionChange {
        if !self.startup_policy.allows_lazy_requests() {
            return ConnectionChange::Unchanged;
        }

        let change = match event {
            ConnectionEvent::Connecting => {
                if self.status.connected() {
                    ConnectionChange::Unchanged
                } else {
                    self.set_status(ConnectionStatus::Connecting)
                }
            }
            ConnectionEvent::Connected => self.set_status(ConnectionStatus::Connected),
            ConnectionEvent::Disconnected => self.set_status(ConnectionStatus::Disconnected),
            ConnectionEvent::Unavailable => {
                if self.status.connected() {
                    ConnectionChange::Unchanged
                } else {
                    self.set_status(ConnectionStatus::Unavailable)
                }
            }
        };
        self.apply_transition_cleanup(change);
        change
    }

    /// True exactly once per session, the first time the launch-critical
    /// path finishes: the cue to warm the Steam connection in the
    /// background so the session's first Steam-backed click skips the
    /// connect stall. Policies that forbid lazy connects never cue.
    pub(crate) fn take_warm_connect_cue(&mut self) -> bool {
        if self.warm_connect_attempted {
            return false;
        }
        self.warm_connect_attempted = true;
        self.startup_policy.allows_lazy_requests()
    }

    pub(super) fn set_pending_retry(&mut self, retry: PendingRetry) {
        self.pending_retry = Some(retry);
    }

    pub(crate) fn take_pending_retry(&mut self) -> Option<PendingRetry> {
        self.pending_retry.take()
    }

    pub(super) fn start_identity_fetch(&mut self) -> u64 {
        self.next_identity_generation()
    }

    pub(super) fn apply_identity_result(
        &mut self,
        generation: u64,
        result: Result<SteamIdentity, UiError>,
    ) -> bool {
        if generation != self.identity_generation {
            return false;
        }

        self.identity = result.ok();
        true
    }

    fn set_status(&mut self, status: ConnectionStatus) -> ConnectionChange {
        let previous = self.status;
        self.status = status;

        match (previous.connected(), status.connected()) {
            (false, true) => ConnectionChange::BecameConnected,
            (true, false) => ConnectionChange::BecameDisconnected,
            _ if previous == status => ConnectionChange::Unchanged,
            _ => ConnectionChange::StatusChanged,
        }
    }

    fn apply_transition_cleanup(&mut self, change: ConnectionChange) {
        if matches!(change, ConnectionChange::BecameDisconnected) {
            self.identity = None;
            self.next_identity_generation();
        }
    }

    fn next_identity_generation(&mut self) -> u64 {
        self.identity_generation = self.identity_generation.wrapping_add(1).max(1);
        self.identity_generation
    }

    #[cfg(test)]
    pub(crate) fn with_startup_policy(mut self, startup_policy: StartupConnectPolicy) -> Self {
        self.startup_policy = startup_policy;
        self
    }
}

/// Captured lifecycle events and terminal error from a lazy connection attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionAttempt {
    events: Vec<ConnectionEvent>,
    error: Option<UiError>,
}

impl ConnectionAttempt {
    pub(crate) fn unavailable(error: impl Into<UiError>) -> Self {
        Self {
            events: vec![ConnectionEvent::Connecting, ConnectionEvent::Unavailable],
            error: Some(error.into()),
        }
    }

    pub(crate) fn events(&self) -> &[ConnectionEvent] {
        &self.events
    }

    pub(crate) fn connected(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event, ConnectionEvent::Connected))
            && self.error.is_none()
    }

    pub(crate) fn error(&self) -> Option<&UiError> {
        self.error.as_ref()
    }
}

/// Connects the core Steam service for a Steam-backed operation.
pub fn connect_context_for_operation(ctx: &BackendServices) -> ConnectionAttempt {
    connect_for_operation_with(|| ctx.steam_connected(), || ctx.connect_steam())
}

/// Runs the connection check/connect seam without binding it to a concrete UI runtime.
pub fn connect_for_operation_with<E>(
    connected: impl FnOnce() -> bool,
    connect: impl FnOnce() -> Result<(), E>,
) -> ConnectionAttempt
where
    E: Into<UiError>,
{
    if connected() {
        return ConnectionAttempt {
            events: vec![ConnectionEvent::Connected],
            error: None,
        };
    }

    let mut events = vec![ConnectionEvent::Connecting];
    match connect() {
        Ok(()) => {
            events.push(ConnectionEvent::Connected);
            ConnectionAttempt {
                events,
                error: None,
            }
        }
        Err(error) => {
            events.push(ConnectionEvent::Unavailable);
            ConnectionAttempt {
                events,
                error: Some(error.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gmpublished_backend::error_key::keys;
    use std::{cell::Cell, sync::Arc};

    use crate::backend::domain::{AvatarRgba, SteamUser};

    use super::{
        ConnectionChange, ConnectionEvent, ConnectionStatus, PendingRetry, StartupConnectPolicy,
        State, SteamIdentity, UiError, connect_for_operation_with,
    };

    #[test]
    fn connection_attempt_reports_operation_edges() {
        let attempt = connect_for_operation_with(|| false, || Ok::<_, UiError>(()));

        assert_eq!(
            attempt.events(),
            &[ConnectionEvent::Connecting, ConnectionEvent::Connected]
        );
        assert_eq!(attempt.error(), None);
        assert!(attempt.connected());
    }

    #[test]
    fn connection_attempt_skips_connect_when_already_connected() {
        let connect_called = Cell::new(false);

        let attempt = connect_for_operation_with(
            || true,
            || {
                connect_called.set(true);
                Ok::<_, UiError>(())
            },
        );

        assert!(!connect_called.get());
        assert_eq!(attempt.events(), &[ConnectionEvent::Connected]);
    }

    #[test]
    fn connection_attempt_reports_unavailable_on_connect_error() {
        let attempt = connect_for_operation_with(
            || false,
            || {
                Err(UiError::detailed(
                    keys::STEAM_ERROR,
                    Some("steam unavailable".to_owned()),
                ))
            },
        );

        assert_eq!(
            attempt.events(),
            &[ConnectionEvent::Connecting, ConnectionEvent::Unavailable]
        );
        assert_eq!(
            attempt.error().and_then(|error| error.detail.as_deref()),
            Some("steam unavailable")
        );
        assert!(!attempt.connected());
    }

    #[test]
    fn unavailable_attempt_builds_connecting_then_unavailable_events() {
        let attempt = super::ConnectionAttempt::unavailable(UiError::detailed(
            keys::STEAM_ERROR,
            Some("worker dropped".to_owned()),
        ));

        assert_eq!(
            attempt.events(),
            &[ConnectionEvent::Connecting, ConnectionEvent::Unavailable]
        );
        assert_eq!(
            attempt.error().and_then(|error| error.detail.as_deref()),
            Some("worker dropped")
        );
    }

    #[test]
    fn connection_state_reports_only_edges() {
        let mut state = State::default();

        assert_eq!(
            state.apply_core_connected(false),
            ConnectionChange::Unchanged
        );
        assert_eq!(
            state.apply_core_connected(true),
            ConnectionChange::BecameConnected
        );
        assert_eq!(
            state.apply_core_connected(true),
            ConnectionChange::Unchanged
        );
        assert_eq!(
            state.apply_core_connected(false),
            ConnectionChange::BecameDisconnected
        );
        assert_eq!(
            state.apply_core_connected(false),
            ConnectionChange::Unchanged
        );
    }

    #[test]
    fn lazy_connection_events_preserve_connecting_and_unavailable_states() {
        let mut state = State::default();

        assert_eq!(
            state.apply_connection_event(ConnectionEvent::Connecting),
            ConnectionChange::StatusChanged
        );
        assert_eq!(state.status(), ConnectionStatus::Connecting);
        assert_eq!(
            state.apply_connection_event(ConnectionEvent::Unavailable),
            ConnectionChange::StatusChanged
        );
        assert_eq!(state.status(), ConnectionStatus::Unavailable);
        assert_eq!(
            state.apply_connection_event(ConnectionEvent::Connected),
            ConnectionChange::BecameConnected
        );
        assert_eq!(state.status(), ConnectionStatus::Connected);
        assert_eq!(
            state.apply_connection_event(ConnectionEvent::Unavailable),
            ConnectionChange::Unchanged
        );
        assert_eq!(state.status(), ConnectionStatus::Connected);
    }

    #[test]
    fn suppressed_policy_ignores_lazy_connection_events() {
        let mut state = State::default().with_startup_policy(StartupConnectPolicy::Suppressed);

        assert_eq!(
            state.apply_connection_event(ConnectionEvent::Connecting),
            ConnectionChange::Unchanged
        );
        assert_eq!(state.status(), ConnectionStatus::Disconnected);
    }

    #[test]
    fn warm_connect_cues_exactly_once() {
        let mut state = State::default();

        assert!(state.take_warm_connect_cue());
        assert!(!state.take_warm_connect_cue());
    }

    #[test]
    fn suppressed_policy_never_cues_a_warm_connect() {
        let mut state = State::default().with_startup_policy(StartupConnectPolicy::Suppressed);

        assert!(!state.take_warm_connect_cue());
        assert!(!state.take_warm_connect_cue());
    }

    #[test]
    fn pending_retry_is_single_slot_and_take_clears_it() {
        let mut state = State::default();
        let retry = PendingRetry::MyWorkshopPage {
            generation: 7,
            page: 2,
        };

        state.set_pending_retry(retry.clone());

        assert_eq!(state.pending_retry(), Some(&retry));
        assert_eq!(state.take_pending_retry(), Some(retry));
        assert_eq!(state.pending_retry(), None);
    }

    #[test]
    fn identity_generation_invalidates_stale_results() {
        let mut state = State::default();
        let first = state.start_identity_fetch();
        let second = state.start_identity_fetch();

        assert!(!state.apply_identity_result(first, Ok(identity("Ada", None))));
        assert_eq!(state.identity(), None);
        assert!(state.apply_identity_result(second, Ok(identity("Grace", None))));
        assert_eq!(state.identity().map(SteamIdentity::name), Some("Grace"));
    }

    #[test]
    fn disconnect_clears_identity_and_advances_generation() {
        let mut state = State::default();
        state.apply_connection_event(ConnectionEvent::Connected);
        let generation = state.start_identity_fetch();
        assert!(state.apply_identity_result(generation, Ok(identity("Ada", None))));

        assert_eq!(
            state.apply_core_connected(false),
            ConnectionChange::BecameDisconnected
        );

        assert_eq!(state.identity(), None);
        assert!(state.identity_generation() > generation);
    }

    #[test]
    fn disconnected_event_clears_identity_and_advances_generation() {
        let mut state = State::default();
        state.apply_connection_event(ConnectionEvent::Connected);
        let generation = state.start_identity_fetch();
        assert!(state.apply_identity_result(generation, Ok(identity("Ada", None))));

        assert_eq!(
            state.apply_connection_event(ConnectionEvent::Disconnected),
            ConnectionChange::BecameDisconnected
        );

        assert_eq!(state.identity(), None);
        assert!(state.identity_generation() > generation);
    }

    #[test]
    fn steam_identity_keeps_name_and_optional_avatar() {
        let avatar = AvatarRgba::new(1, 1, vec![1, 2, 3, 4]).expect("avatar should be valid");
        let avatar_bytes = Arc::clone(&avatar.rgba);
        let identity = identity("Ada", Some(avatar));

        assert_eq!(identity.name(), "Ada");
        assert!(Arc::ptr_eq(
            &avatar_bytes,
            &identity.avatar().expect("avatar should be present").rgba
        ));
    }

    fn identity(name: &str, avatar: Option<AvatarRgba>) -> SteamIdentity {
        SteamIdentity::from_user(SteamUser {
            steamid: 76561198000000001,
            name: name.to_owned(),
            avatar,
            dead: false,
        })
    }
}
