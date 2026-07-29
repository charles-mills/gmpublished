use std::{fmt, sync::Arc};

use gmpublished_backend::error_key::{ErrorKey, HasErrorKey};
use gmpublished_backend::transactions::TransactionError;

/// Value-semantic error carried through Iced messages and feature state:
/// a stable [`ErrorKey`] plus optional contextual payload. Rich errors (with
/// sources) are logged where they convert into this type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiError {
    pub(crate) key: ErrorKey,
    pub(crate) detail: Option<Arc<str>>,
}

impl UiError {
    pub(crate) fn new(key: ErrorKey) -> Self {
        Self { key, detail: None }
    }

    pub(crate) fn detailed(key: ErrorKey, detail: Option<String>) -> Self {
        Self {
            key,
            detail: detail.map(Into::into),
        }
    }
}

/// Renders the wire composite: `KEY` or `KEY:detail`.
impl fmt::Display for UiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            None => f.write_str(self.key.as_str()),
            Some(detail) => write!(f, "{}:{detail}", self.key),
        }
    }
}

impl From<ErrorKey> for UiError {
    fn from(key: ErrorKey) -> Self {
        Self::new(key)
    }
}

impl From<TransactionError> for UiError {
    fn from(error: TransactionError) -> Self {
        Self {
            key: error.key,
            detail: error.detail,
        }
    }
}

impl<E: HasErrorKey> From<&E> for UiError {
    fn from(error: &E) -> Self {
        Self::detailed(error.error_key(), error.error_detail())
    }
}

/// Converts a fallible result's error into a [`UiError`], so `?` works.
///
/// The `From` impl above has to take `&E` — a blanket `From<E>` would collide
/// with the identity impl — which is exactly the form `?` cannot use. Without
/// this, every fallible call at the boundary has to spell out
/// `.map_err(|error| UiError::from(&error))`.
pub trait ResultExt<T> {
    /// # Errors
    /// Forwards the receiver's error, translated to a [`UiError`].
    fn ui_err(self) -> Result<T, UiError>;
}

impl<T, E: HasErrorKey> ResultExt<T> for Result<T, E> {
    fn ui_err(self) -> Result<T, UiError> {
        self.map_err(|error| UiError::from(&error))
    }
}
