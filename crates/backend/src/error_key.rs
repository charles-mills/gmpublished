//! Stable error codes: the wire- and i18n-addressable identity of an error.
//! The UI resolves keys to localized text (`ERR_WHITELIST` → `err-whitelist`
//! in the .ftl files). Values are frozen — renaming a constant is fine,
//! changing its value silently breaks localization. Contextual payload
//! travels separately as [`crate::transactions::TransactionError::detail`].

use std::fmt;

/// Stable, i18n-addressable error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ErrorKey(&'static str);

impl ErrorKey {
    /// Values must be `ERR_SCREAMING_SNAKE`: the UI derives its Fluent lookup
    /// from them (`ERR_FOO_BAR` -> `err-foo-bar`).
    #[must_use]
    pub const fn new(key: &'static str) -> Self {
        Self(key)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ErrorKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Implemented by every error type whose failures are user-addressable.
pub trait HasErrorKey {
    fn error_key(&self) -> ErrorKey;

    /// Contextual payload carried alongside the key (a path, an entry name,
    /// an upstream error message). `None` when the key alone identifies the
    /// failure.
    fn error_detail(&self) -> Option<String> {
        None
    }
}

pub mod keys {
    use super::ErrorKey;

    pub const IO_ERROR: ErrorKey = ErrorKey::new("ERR_IO_ERROR");
    pub const PATH_IO_ERROR: ErrorKey = ErrorKey::new("ERR_PATH_IO_ERROR");
    pub const CANCELLED: ErrorKey = ErrorKey::new("ERR_CANCELLED");
    pub const UNKNOWN: ErrorKey = ErrorKey::new("ERR_UNKNOWN");

    pub const GMA_FORMAT_ERROR: ErrorKey = ErrorKey::new("ERR_GMA_FORMAT_ERROR");
    pub const GMA_INVALID_HEADER: ErrorKey = ErrorKey::new("ERR_GMA_INVALID_HEADER");
    pub const GMA_ENTRY_NOT_FOUND: ErrorKey = ErrorKey::new("ERR_GMA_ENTRY_NOT_FOUND");
    pub const LZMA: ErrorKey = ErrorKey::new("ERR_LZMA");
    pub const WHITELIST: ErrorKey = ErrorKey::new("ERR_WHITELIST");
    pub const GMA_EXTRACTION_FAILED: ErrorKey = ErrorKey::new("ERR_GMA_EXTRACTION_FAILED");
    pub const GMA_DESTINATION_UNAVAILABLE: ErrorKey =
        ErrorKey::new("ERR_GMA_DESTINATION_UNAVAILABLE");

    pub const VPK_FORMAT_ERROR: ErrorKey = ErrorKey::new("ERR_VPK_FORMAT_ERROR");
    pub const VPK_INVALID_HEADER: ErrorKey = ErrorKey::new("ERR_VPK_INVALID_HEADER");
    pub const VPK_ENTRY_NOT_FOUND: ErrorKey = ErrorKey::new("ERR_VPK_ENTRY_NOT_FOUND");
    pub const VPK_UNSAFE_PATH: ErrorKey = ErrorKey::new("ERR_VPK_UNSAFE_PATH");
    pub const VPK_MISSING_ARCHIVE: ErrorKey = ErrorKey::new("ERR_VPK_MISSING_ARCHIVE");

    pub const STEAM_ERROR: ErrorKey = ErrorKey::new("ERR_STEAM_ERROR");
    pub const DOWNLOAD_MISSING: ErrorKey = ErrorKey::new("ERR_DOWNLOAD_MISSING");
    pub const DOWNLOAD_FAILED: ErrorKey = ErrorKey::new("ERR_DOWNLOAD_FAILED");
    pub const ITEM_NOT_FOUND: ErrorKey = ErrorKey::new("ERR_ITEM_NOT_FOUND");

    pub const MULTIPLE_GMAS: ErrorKey = ErrorKey::new("ERR_MULTIPLE_GMAS");
    pub const INVALID_CONTENT_PATH: ErrorKey = ErrorKey::new("ERR_INVALID_CONTENT_PATH");
    pub const NO_ENTRIES: ErrorKey = ErrorKey::new("ERR_NO_ENTRIES");
    pub const DUPLICATE_ENTRIES: ErrorKey = ErrorKey::new("ERR_DUPLICATE_ENTRIES");
    pub const IMAGE_ERROR: ErrorKey = ErrorKey::new("ERR_IMAGE_ERROR");
    pub const ICON_TOO_LARGE: ErrorKey = ErrorKey::new("ERR_ICON_TOO_LARGE");
    pub const ICON_TOO_SMALL: ErrorKey = ErrorKey::new("ERR_ICON_TOO_SMALL");
    pub const ICON_INVALID_FORMAT: ErrorKey = ErrorKey::new("ERR_ICON_INVALID_FORMAT");

    pub const NO_ADDONS_FOUND: ErrorKey = ErrorKey::new("ERR_NO_ADDONS_FOUND");
    pub const SEARCH_EVENT_SINK_UNAVAILABLE: ErrorKey =
        ErrorKey::new("ERR_SEARCH_EVENT_SINK_UNAVAILABLE");
    pub const SEARCH_EVENT_SINK_DISCONNECTED: ErrorKey =
        ErrorKey::new("ERR_SEARCH_EVENT_SINK_DISCONNECTED");
    pub const SEARCH_DATA_SHAPE: ErrorKey = ErrorKey::new("ERR_SEARCH_DATA_SHAPE");
    pub const GMOD_PATH_MISSING: ErrorKey = ErrorKey::new("ERR_GMOD_PATH_MISSING");
}

#[cfg(test)]
mod tests {
    use super::keys;

    #[test]
    fn key_values_are_frozen() {
        assert_eq!(keys::IO_ERROR.as_str(), "ERR_IO_ERROR");
        assert_eq!(keys::WHITELIST.as_str(), "ERR_WHITELIST");
        assert_eq!(keys::GMOD_PATH_MISSING.as_str(), "ERR_GMOD_PATH_MISSING");
    }

    /// `Display` is a developer message and `HasErrorKey` is the wire code.
    /// A `#[error("ERR_…")]` attribute means an error type is carrying the code
    /// twice, and the copy in `Display` is the one nothing checks.
    #[test]
    fn display_messages_do_not_restate_error_keys() {
        fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && let Ok(source) = std::fs::read_to_string(&path)
                {
                    for (number, line) in source.lines().enumerate() {
                        if line.trim_start().starts_with("#[error(\"ERR_") {
                            out.push(format!("{}:{}", path.display(), number + 1));
                        }
                    }
                }
            }
        }

        // Both crates: `HasErrorKey` impls live on either side of the bridge,
        // and the app's are as able to restate a key as the backend's.
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the backend crate sits inside the workspace");
        let mut offenders = Vec::new();
        for crate_name in ["backend", "app"] {
            walk(&workspace.join(crate_name).join("src"), &mut offenders);
        }
        assert!(
            workspace.join("app/src").is_dir(),
            "the app crate must be walked, not silently skipped"
        );

        assert!(
            offenders.is_empty(),
            "error keys restated in Display: {offenders:#?}"
        );
    }
}
