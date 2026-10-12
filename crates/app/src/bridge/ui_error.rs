use std::{fmt, sync::Arc};

use gmpublished_backend::TransactionError;
use gmpublished_backend::{ErrorKey, HasErrorKey};

/// Log the source error when converting. Messages carry only the localization key and detail.
#[derive(Clone, Debug, Eq, PartialEq)]
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

/// A blanket `From<E>` would conflict with `From<T> for T`. Borrow the error
/// here so `?` can propagate it as `UiError`.
pub trait ResultExt<T> {
    fn ui_err(self) -> Result<T, UiError>;
}

impl<T, E: HasErrorKey> ResultExt<T> for Result<T, E> {
    fn ui_err(self) -> Result<T, UiError> {
        self.map_err(|error| UiError::from(&error))
    }
}
