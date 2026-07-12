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
