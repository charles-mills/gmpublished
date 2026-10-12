use crate::bridge::ui_error::UiError;

use super::{ConnectionAttempt, ConnectionEvent, PendingRetry, SteamIdentity};
use crate::generation::Generation;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    ConnectionEvent(ConnectionEvent),
    ConnectionAttemptCompleted(ConnectionAttempt),
    PendingRetrySet(PendingRetry),
    IdentityFetched(Generation, Result<SteamIdentity, UiError>),
}
