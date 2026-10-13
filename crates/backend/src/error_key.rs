//! Error values are stable localization keys (`ERR_WHITELIST` becomes
//! `err-whitelist`). Detail text is carried separately.

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ErrorKey(&'static str);

impl ErrorKey {
    const fn new(key: &'static str) -> Self {
        assert!(is_valid_error_key(key), "invalid error key");
        Self(key)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub const fn for_test(key: &'static str) -> Self {
        Self::new(key)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// ERR_SCREAMING_SNAKE with no empty segments. Const validation rejects malformed catalog keys
/// at compile time.
const fn is_valid_error_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.len() <= 4
        || bytes[0] != b'E'
        || bytes[1] != b'R'
        || bytes[2] != b'R'
        || bytes[3] != b'_'
    {
        return false;
    }

    let mut index = 4;
    let mut previous_was_underscore = true;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'_' {
            if previous_was_underscore {
                return false;
            }
            previous_was_underscore = true;
        } else if byte.is_ascii_uppercase() || byte.is_ascii_digit() {
            previous_was_underscore = false;
        } else {
            return false;
        }
        index += 1;
    }
    !previous_was_underscore
}

impl fmt::Display for ErrorKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

pub trait HasErrorKey {
    fn error_key(&self) -> ErrorKey;

    /// Contextual payload carried alongside the key (a path, an entry name,
    /// an upstream error message). `None` when the key alone identifies the
    /// failure.
    fn error_detail(&self) -> Option<String> {
        None
    }
}

macro_rules! error_key_catalog {
    ($($name:ident => $value:literal),+ $(,)?) => {
        pub mod keys {
            use super::ErrorKey;

            $(pub const $name: ErrorKey = ErrorKey::new($value);)+

            pub const ALL: &[ErrorKey] = &[$($name),+];
        }
    };
}

error_key_catalog! {
    MODEL_COMPILE => "ERR_MODEL_COMPILE",
    IO_ERROR => "ERR_IO_ERROR",
    PATH_IO_ERROR => "ERR_PATH_IO_ERROR",
    CANCELLED => "ERR_CANCELLED",
    UNKNOWN => "ERR_UNKNOWN",

    GMA_FORMAT_ERROR => "ERR_GMA_FORMAT_ERROR",
    GMA_INVALID_HEADER => "ERR_GMA_INVALID_HEADER",
    GMA_ENTRY_NOT_FOUND => "ERR_GMA_ENTRY_NOT_FOUND",
    LZMA => "ERR_LZMA",
    WHITELIST => "ERR_WHITELIST",
    GMA_EXTRACTION_FAILED => "ERR_GMA_EXTRACTION_FAILED",
    GMA_DESTINATION_UNAVAILABLE => "ERR_GMA_DESTINATION_UNAVAILABLE",

    VPK_FORMAT_ERROR => "ERR_VPK_FORMAT_ERROR",
    VPK_INVALID_HEADER => "ERR_VPK_INVALID_HEADER",
    VPK_ENTRY_NOT_FOUND => "ERR_VPK_ENTRY_NOT_FOUND",
    VPK_UNSAFE_PATH => "ERR_VPK_UNSAFE_PATH",
    VPK_MISSING_ARCHIVE => "ERR_VPK_MISSING_ARCHIVE",

    STEAM_ERROR => "ERR_STEAM_ERROR",
    DOWNLOAD_MISSING => "ERR_DOWNLOAD_MISSING",
    DOWNLOAD_FAILED => "ERR_DOWNLOAD_FAILED",
    ITEM_NOT_FOUND => "ERR_ITEM_NOT_FOUND",

    MULTIPLE_GMAS => "ERR_MULTIPLE_GMAS",
    INVALID_CONTENT_PATH => "ERR_INVALID_CONTENT_PATH",
    NO_ENTRIES => "ERR_NO_ENTRIES",
    DUPLICATE_ENTRIES => "ERR_DUPLICATE_ENTRIES",
    IMAGE_ERROR => "ERR_IMAGE_ERROR",
    DESCRIPTION_TOO_LONG => "ERR_DESCRIPTION_TOO_LONG",
    ICON_TOO_LARGE => "ERR_ICON_TOO_LARGE",
    ICON_TOO_SMALL => "ERR_ICON_TOO_SMALL",
    ICON_INVALID_FORMAT => "ERR_ICON_INVALID_FORMAT",

    NO_ADDONS_FOUND => "ERR_NO_ADDONS_FOUND",
    SEARCH_EVENT_SINK_UNAVAILABLE => "ERR_SEARCH_EVENT_SINK_UNAVAILABLE",
    SEARCH_EVENT_SINK_DISCONNECTED => "ERR_SEARCH_EVENT_SINK_DISCONNECTED",
    SEARCH_DATA_SHAPE => "ERR_SEARCH_DATA_SHAPE",
    GMOD_PATH_MISSING => "ERR_GMOD_PATH_MISSING",

    WORKER_QUEUE_FULL => "ERR_WORKER_QUEUE_FULL",
    WORKER_POOL_STOPPED => "ERR_WORKER_POOL_STOPPED",
    WORKER_DROPPED => "ERR_WORKER_DROPPED",
}

#[cfg(test)]
mod tests {
    use super::{is_valid_error_key, keys};

    #[test]
    fn production_catalog_is_valid_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for key in keys::ALL {
            assert!(is_valid_error_key(key.as_str()), "{key}");
            assert!(seen.insert(key.as_str()), "duplicate error key {key}");
        }
    }

    #[test]
    fn malformed_fixture_keys_are_rejected() {
        for key in [
            "",
            "DOWNLOAD_FAILED",
            "ERR_",
            "ERR_BAD_",
            "ERR_BAD__KEY",
            "ERR_bad",
        ] {
            assert!(!is_valid_error_key(key), "{key}");
        }
    }
}
