use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

pub trait Versioned: DeserializeOwned {
    /// Bump when the schema changes. Incompatible cache files are discarded.
    const VERSION: u32;
    fn version(&self) -> u32;
    const NOUN: &'static str;
}

#[derive(Debug, Error)]
pub enum SnapshotWriteError {
    #[error("failed to serialize {noun} snapshot: {source}")]
    Serialize {
        noun: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to persist {noun} snapshot {}: {source}", path.display())]
    Write {
        noun: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Treat unreadable or incompatible snapshots as cache misses.
pub fn load<T: Versioned>(path: &Path) -> Option<T> {
    let noun = T::NOUN;
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotFound {
                log::debug!("{noun} snapshot {} is missing", path.display());
            } else {
                log::debug!(
                    "ignoring unreadable {noun} snapshot {}: {error}",
                    path.display()
                );
            }
            return None;
        }
    };

    let snapshot = match serde_json::from_str::<T>(&contents) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log::debug!(
                "discarding unparseable {noun} snapshot {}: {error}",
                path.display()
            );
            return None;
        }
    };

    if snapshot.version() != T::VERSION {
        log::debug!(
            "discarding {noun} snapshot {} with unsupported version {}",
            path.display(),
            snapshot.version()
        );
        return None;
    }
    Some(snapshot)
}

/// Use a tempfile beside the target so rename is atomic.
pub fn write<T: Versioned + Serialize>(
    path: &Path,
    snapshot: &T,
) -> Result<(), SnapshotWriteError> {
    let noun = T::NOUN;
    let write_error = |source| SnapshotWriteError::Write {
        noun,
        path: path.to_path_buf(),
        source,
    };
    let mut temp = crate::util::fs::atomic_tempfile(path).map_err(write_error)?;
    {
        let mut writer = BufWriter::with_capacity(
            crate::util::fs::ATOMIC_WRITE_BUFFER_SIZE,
            temp.as_file_mut(),
        );
        serde_json::to_writer(&mut writer, snapshot)
            .map_err(|source| SnapshotWriteError::Serialize { noun, source })?;
        writer.flush().map_err(write_error)?;
    }
    crate::util::fs::persist_atomic(temp, path).map_err(write_error)
}
