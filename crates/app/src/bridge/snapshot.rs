//! Versioned JSON snapshots written beside the app's caches.
//!
//! Both the Workshop metadata cache and the library header cache persist the
//! same way: a version-tagged document, discarded wholesale if it is missing,
//! unreadable, unparseable or from another version, and rewritten through a
//! tempfile so a crash mid-write cannot truncate the previous one.

use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

/// A document that records the schema it was written against.
pub trait Versioned: DeserializeOwned {
    /// Bumped whenever the shape changes. Older files are discarded rather
    /// than migrated: a snapshot is a cache, so losing one costs a refetch.
    const VERSION: u32;
    /// What the file on disk claims to be.
    fn version(&self) -> u32;
    /// Names this snapshot in the log lines below.
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

/// Reads a snapshot, or `None` if there is nothing usable at `path`.
///
/// Every failure is a miss rather than an error: the caller's fallback is to
/// rebuild the cache, which is always available and never wrong.
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

/// Writes a snapshot through a tempfile in the same directory, so the rename
/// onto `path` is atomic and a crash cannot leave a half-written cache.
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
