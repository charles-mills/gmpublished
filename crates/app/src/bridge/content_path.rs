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
