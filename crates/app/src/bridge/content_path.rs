//! The canonical form every content lookup and every content index uses.

use std::fmt;

/// A content path in the canonical form every tier source indexes by:
/// lowercase, forward slashes, no empty or `.` segments, no `..`.
///
/// A lookup fans out across every content tier, so the path is normalized
/// once here and the sources index by it directly.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ContentPath(String);

impl ContentPath {
    /// `None` when `path` has no canonical form: empty, or containing a `..`
    /// segment. `..` is rejected outright rather than resolved, so a path is
    /// never silently rewritten into one naming a different entry.
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

/// Trims separators, drops empty and `.` segments, rejects `..`, and
/// lowercases. Both sides of a lookup must agree on this or an index built
/// from one form is unreachable from the other.
pub fn normalize_archive_path(path: &str) -> Option<String> {
    let path = path
        .trim()
        .trim_matches(|character| matches!(character, '/' | '\\'));
    let mut normalized = String::with_capacity(path.len());
    for segment in path.split(['/', '\\']) {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return None;
        }
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(segment);
    }
    if normalized.is_empty() {
        return None;
    }
    normalized.make_ascii_lowercase();
    Some(normalized)
}
