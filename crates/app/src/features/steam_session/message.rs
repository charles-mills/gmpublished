use crate::bridge::ui_error::UiError;

use super::{ConnectionAttempt, ConnectionEvent, PendingRetry, SteamIdentity};

/// Facts emitted by the Steam session feature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    ConnectionEvent(ConnectionEvent),
    /// A blocking lazy connection attempt completed.
    ConnectionAttemptCompleted(ConnectionAttempt),
    /// A Steam-backed operation should be retried once the session connects.
    PendingRetrySet(PendingRetry),
    /// The current-user worker completed for this generation.
    IdentityFetched(u64, Result<SteamIdentity, UiError>),
}
