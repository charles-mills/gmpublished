//! Archive entry-path hardening shared by the container formats
//! (VPK, GMA): archives from untrusted sources must never name a path
//! that escapes an extraction root or aliases OS-special locations.

/// Whether an archive entry path is unsafe to extract or resolve:
/// empty, absolute (Unix or Windows), traversing (`..`), containing
/// NUL, or using backslash separators (never produced by valid Source
/// archives; always suspicious).
#[must_use]
pub fn is_unsafe_entry_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') || path.contains('\\') {
        return true;
    }
    if path.starts_with('/') {
        return true;
    }
    // Windows absolute or drive-relative: "C:", "c:anything".
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return true;
    }
    path.split('/').any(|segment| segment == "..")
}

#[cfg(test)]
mod tests {
    use super::is_unsafe_entry_path;

    #[test]
    fn rejects_escapes_and_accepts_normal_paths() {
        for unsafe_path in [
            "",
            "/etc/passwd",
            "..",
            "../evil.txt",
            "materials/../../evil.txt",
            "a/..",
            "C:evil.txt",
            "c:/evil.txt",
            "materials\\wall.vmt",
            "nul\0byte",
        ] {
            assert!(is_unsafe_entry_path(unsafe_path), "{unsafe_path:?}");
        }
        for safe_path in [
            "addon.json",
            "LICENSE",
            "materials/wall.vmt",
            "sound/doors/door1..move.wav", // dots inside a segment are fine
            "lua/autorun/..init.lua",
        ] {
            assert!(!is_unsafe_entry_path(safe_path), "{safe_path:?}");
        }
    }
}
