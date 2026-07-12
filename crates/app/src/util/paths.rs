//! Path-to-display-string helpers shared by the pickers and settings views.

use std::path::{Path, PathBuf};

use crate::backend::{AppPaths, Settings};

/// Renders `path` for display, lossily substituting any non-UTF-8 bytes.
pub fn path_to_display(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().into_owned()
}

/// Falls back to the temp directory when the current directory can't be read
/// (e.g. it was deleted out from under the process).
pub fn fallback_current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|error| {
        log::debug!("current_dir failed while resolving a fallback path: {error}");
        std::env::temp_dir()
    })
}

/// Best-effort [`AppPaths`] rooted at the temp directory, used when the real
/// paths haven't resolved yet (e.g. before startup finishes).
pub fn fallback_paths(settings: &Settings) -> AppPaths {
    AppPaths::resolve_with_defaults(
        settings,
        AppPaths {
            settings_file: std::env::temp_dir().join("gmpublished-settings.json"),
            default_user_data_dir: std::env::temp_dir(),
            default_temp_dir: std::env::temp_dir(),
            default_downloads_dir: None,
            temp_dir: std::env::temp_dir(),
            user_data_dir: std::env::temp_dir(),
            downloads_dir: None,
            gmod_dir: None,
        },
    )
}
