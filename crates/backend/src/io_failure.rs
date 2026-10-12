use std::{fmt, io, sync::Arc};

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
}
