use std::path::{Path, PathBuf};

pub fn path_to_display(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().into_owned()
}

pub fn fallback_current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|error| {
        log::debug!("current_dir failed while resolving a fallback path: {error}");
        std::env::temp_dir()
    })
}
