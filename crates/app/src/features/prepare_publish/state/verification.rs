//! Supersedable verification state.

use super::UiError;

/// Something the user chose that has to be checked before the app can use it.
/// Exactly one of "nothing chosen", "check in flight", "usable" and "rejected"
/// holds at a time, and the checked-out value lives on `Verified` so it cannot
/// outlive the check that produced it.
#[derive(Clone, Debug, Default)]
pub(super) enum Verification<T> {
    #[default]
    Empty,
    Pending,
    Verified(T),
    Failed(UiError),
}

impl<T> Verification<T> {
    pub(super) const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    pub(super) const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    pub(super) const fn error(&self) -> Option<&UiError> {
        match self {
            Self::Failed(error) => Some(error),
            _ => None,
        }
    }

    pub(super) const fn verified(&self) -> Option<&T> {
        match self {
            Self::Verified(value) => Some(value),
            _ => None,
        }
    }

    pub(super) const fn verified_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Verified(value) => Some(value),
            _ => None,
        }
    }
}
