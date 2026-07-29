//! An [`std::io::Error`] reduced to what this crate reports and compares.

use std::{fmt, io, sync::Arc};

/// The reportable content of an I/O failure.
///
/// `io::Error` is neither `Clone` nor `PartialEq`, which is why errors holding
/// one end up with hand-written equality — either comparing rendered text or
/// ignoring the payload entirely, both of which make two unrelated failures
/// compare equal. This keeps the two things anything downstream actually uses
/// (the kind, for deciding; the message, for showing) in a form that derives
/// both traits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IoFailure {
    kind: io::ErrorKind,
    message: Arc<str>,
}

impl IoFailure {
    #[must_use]
    pub fn kind(&self) -> io::ErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<io::Error> for IoFailure {
    fn from(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            message: Arc::from(error.to_string()),
        }
    }
}

impl From<&io::Error> for IoFailure {
    fn from(error: &io::Error) -> Self {
        Self {
            kind: error.kind(),
            message: Arc::from(error.to_string()),
        }
    }
}

impl fmt::Display for IoFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for IoFailure {}

#[cfg(test)]
mod tests {
    use super::IoFailure;
    use std::io;

    #[test]
    fn it_keeps_the_kind_and_the_message() {
        let failure = IoFailure::from(io::Error::new(io::ErrorKind::NotFound, "no such addon"));

        assert_eq!(failure.kind(), io::ErrorKind::NotFound);
        assert!(failure.message().contains("no such addon"));
    }

    /// The point of the type: two different failures must not compare equal.
    /// Discriminant-only equality made every I/O error the same error, and
    /// rendered-text equality made two different kinds equal whenever their
    /// messages happened to match.
    #[test]
    fn failures_compare_on_both_kind_and_message() {
        let not_found = IoFailure::from(io::Error::new(io::ErrorKind::NotFound, "gone"));
        let denied = IoFailure::from(io::Error::new(io::ErrorKind::PermissionDenied, "gone"));
        let other_message = IoFailure::from(io::Error::new(io::ErrorKind::NotFound, "elsewhere"));

        assert_ne!(not_found, denied);
        assert_ne!(not_found, other_message);
        assert_eq!(
            not_found,
            IoFailure::from(io::Error::new(io::ErrorKind::NotFound, "gone"))
        );
    }
}
