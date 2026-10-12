use std::fmt;

/// Lowercase, forward slashes, no empty or `.` segments, no `..`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ContentPath(String);

impl ContentPath {
    /// Rejects empty paths and `..` segments; parent segments are never resolved.
    pub fn new(path: &str) -> Option<Self> {
        normalize_archive_path(path).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

pub use gmpublished_backend::path::normalize_archive_path;
