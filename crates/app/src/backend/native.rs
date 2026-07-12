use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeOpenTarget {
    Url(String),
    Path(PathBuf),
    Reveal(PathBuf),
}

impl NativeOpenTarget {
    pub(crate) fn url(url: impl Into<String>) -> Self {
        Self::Url(url.into())
    }

    pub(crate) fn path(path: impl Into<PathBuf>) -> Self {
        Self::Path(path.into())
    }

    pub(crate) fn reveal(path: impl Into<PathBuf>) -> Self {
        Self::Reveal(path.into())
    }
}

/// The OS shell refused to open or reveal a target; the Display string is
/// shown verbatim in a native dialog.
#[derive(Debug, thiserror::Error)]
#[error("Failed to open {subject}: {source}")]
pub struct NativeOpenError {
    subject: String,
    #[source]
    source: std::io::Error,
}

pub fn open_target(target: NativeOpenTarget) -> Result<(), NativeOpenError> {
    match target {
        NativeOpenTarget::Url(url) => {
            gmpublished_backend::path::open(&url).map_err(|source| NativeOpenError {
                subject: url,
                source,
            })
        }
        NativeOpenTarget::Path(path) => {
            gmpublished_backend::path::open(&path).map_err(|source| NativeOpenError {
                subject: path.display().to_string(),
                source,
            })
        }
        NativeOpenTarget::Reveal(path) => gmpublished_backend::path::open_file_location(&path)
            .map_err(|source| NativeOpenError {
                subject: path.display().to_string(),
                source,
            }),
    }
}
