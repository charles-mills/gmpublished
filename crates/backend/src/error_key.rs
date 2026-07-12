//! Stable error codes: the wire- and i18n-addressable identity of an error.
//! The UI resolves keys to localized text (`ERR_WHITELIST` → `err-whitelist`
//! in the .ftl files). Values are frozen — renaming a constant is fine,
//! changing its value silently breaks localization. Contextual payload
//! travels separately as [`crate::transactions::TransactionError::detail`].

use std::fmt;

/// Stable, i18n-addressable error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ErrorKey(pub &'static str);

impl ErrorKey {
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

    pub const IO_ERROR: ErrorKey = ErrorKey("ERR_IO_ERROR");
    pub const PATH_IO_ERROR: ErrorKey = ErrorKey("ERR_PATH_IO_ERROR");
    pub const CANCELLED: ErrorKey = ErrorKey("ERR_CANCELLED");
    pub const UNKNOWN: ErrorKey = ErrorKey("ERR_UNKNOWN");

    pub const GMA_FORMAT_ERROR: ErrorKey = ErrorKey("ERR_GMA_FORMAT_ERROR");
    pub const GMA_INVALID_HEADER: ErrorKey = ErrorKey("ERR_GMA_INVALID_HEADER");
    pub const GMA_ENTRY_NOT_FOUND: ErrorKey = ErrorKey("ERR_GMA_ENTRY_NOT_FOUND");
    pub const LZMA: ErrorKey = ErrorKey("ERR_LZMA");
    pub const WHITELIST: ErrorKey = ErrorKey("ERR_WHITELIST");
    pub const GMA_EXTRACTION_FAILED: ErrorKey = ErrorKey("ERR_GMA_EXTRACTION_FAILED");
    pub const GMA_DESTINATION_UNAVAILABLE: ErrorKey = ErrorKey("ERR_GMA_DESTINATION_UNAVAILABLE");
    // Not ERR_-shaped; the frozen wire value.
    pub const UNSAFE_ENTRY_PATH: ErrorKey = ErrorKey("Illegal path");

    pub const VPK_FORMAT_ERROR: ErrorKey = ErrorKey("ERR_VPK_FORMAT_ERROR");
    pub const VPK_INVALID_HEADER: ErrorKey = ErrorKey("ERR_VPK_INVALID_HEADER");
    pub const VPK_ENTRY_NOT_FOUND: ErrorKey = ErrorKey("ERR_VPK_ENTRY_NOT_FOUND");
    pub const VPK_UNSAFE_PATH: ErrorKey = ErrorKey("ERR_VPK_UNSAFE_PATH");
    pub const VPK_MISSING_ARCHIVE: ErrorKey = ErrorKey("ERR_VPK_MISSING_ARCHIVE");

    pub const STEAM_ERROR: ErrorKey = ErrorKey("ERR_STEAM_ERROR");
    pub const DOWNLOAD_MISSING: ErrorKey = ErrorKey("ERR_DOWNLOAD_MISSING");
    pub const DOWNLOAD_FAILED: ErrorKey = ErrorKey("ERR_DOWNLOAD_FAILED");
    pub const ITEM_NOT_FOUND: ErrorKey = ErrorKey("ERR_ITEM_NOT_FOUND");

    pub const MULTIPLE_GMAS: ErrorKey = ErrorKey("ERR_MULTIPLE_GMAS");
    pub const INVALID_CONTENT_PATH: ErrorKey = ErrorKey("ERR_INVALID_CONTENT_PATH");
    pub const NO_ENTRIES: ErrorKey = ErrorKey("ERR_NO_ENTRIES");
    pub const DUPLICATE_ENTRIES: ErrorKey = ErrorKey("ERR_DUPLICATE_ENTRIES");
    pub const IMAGE_ERROR: ErrorKey = ErrorKey("ERR_IMAGE_ERROR");
    pub const ICON_TOO_LARGE: ErrorKey = ErrorKey("ERR_ICON_TOO_LARGE");
    pub const ICON_TOO_SMALL: ErrorKey = ErrorKey("ERR_ICON_TOO_SMALL");
    pub const ICON_INVALID_FORMAT: ErrorKey = ErrorKey("ERR_ICON_INVALID_FORMAT");

    pub const VTF_DECODE_FAILED: ErrorKey = ErrorKey("ERR_VTF_DECODE_FAILED");
    pub const BSP_DECODE_FAILED: ErrorKey = ErrorKey("ERR_BSP_DECODE_FAILED");
    pub const BSP_UNSUPPORTED_VERSION: ErrorKey = ErrorKey("ERR_BSP_UNSUPPORTED_VERSION");
    pub const BSP_TOO_LARGE: ErrorKey = ErrorKey("ERR_BSP_TOO_LARGE");
    pub const MDL_DECODE_FAILED: ErrorKey = ErrorKey("ERR_MDL_DECODE_FAILED");
    pub const MDL_VERTEX_INDEX_OUT_OF_RANGE: ErrorKey =
        ErrorKey("ERR_MDL_VERTEX_INDEX_OUT_OF_RANGE");
    pub const MDL_TOO_MANY_VERTICES: ErrorKey = ErrorKey("ERR_MDL_TOO_MANY_VERTICES");
    pub const MDL_INVALID_MATERIAL_INDEX: ErrorKey = ErrorKey("ERR_MDL_INVALID_MATERIAL_INDEX");

    pub const NO_ADDONS_FOUND: ErrorKey = ErrorKey("ERR_NO_ADDONS_FOUND");
    pub const SEARCH_EVENT_SINK_UNAVAILABLE: ErrorKey =
        ErrorKey("ERR_SEARCH_EVENT_SINK_UNAVAILABLE");
    pub const SEARCH_EVENT_SINK_DISCONNECTED: ErrorKey =
        ErrorKey("ERR_SEARCH_EVENT_SINK_DISCONNECTED");
    pub const SEARCH_DATA_SHAPE: ErrorKey = ErrorKey("ERR_SEARCH_DATA_SHAPE");
    // Not ERR_-shaped; the frozen wire value, interpolated verbatim by its templates.
    pub const GMOD_PATH_MISSING: ErrorKey = ErrorKey("Garry's Mod path is not configured");
}

#[cfg(test)]
mod tests {
    use super::keys;

    #[test]
    fn key_values_are_frozen() {
        assert_eq!(keys::IO_ERROR.as_str(), "ERR_IO_ERROR");
        assert_eq!(keys::WHITELIST.as_str(), "ERR_WHITELIST");
        assert_eq!(keys::UNSAFE_ENTRY_PATH.as_str(), "Illegal path");
        assert_eq!(
            keys::GMOD_PATH_MISSING.as_str(),
            "Garry's Mod path is not configured"
        );
    }
}
