//! Filesystem helpers: atomic write-then-rename and friends.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

/// Writes `bytes` to `path` atomically: create parent directories, write to a
/// tempfile in the target's directory (so the final rename stays on one
/// filesystem), then persist over `path`. A crash or error mid-write can
/// never leave a torn file at `path`.
///
/// The stages (create dir, create tempfile, write, persist) are collapsed
/// into a single `io::Error`; callers that need a typed error wrap it with
/// their own context.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_with(path, |writer| writer.write_all(bytes))
}

/// Same as [`atomic_write`], but streams into the tempfile via `write`
/// instead of requiring the caller to buffer to a `Vec` first.
pub fn atomic_write_with(
    path: &Path,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path.parent();
    if let Some(parent) = parent {
        fs::create_dir_all(parent)?;
    }

    let mut tmp = parent.map_or_else(tempfile::NamedTempFile::new, |parent| {
        tempfile::NamedTempFile::new_in(parent)
    })?;
    write(&mut tmp)?;
    tmp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_parent_dirs_and_persists_bytes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("nested/dir/file.txt");

        atomic_write(&path, b"hello").expect("atomic_write should succeed");

        assert_eq!(fs::read(&path).expect("file should exist"), b"hello");
    }

    #[test]
    fn atomic_write_with_streams_into_the_tempfile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("file.txt");

        atomic_write_with(&path, |writer| writer.write_all(b"streamed"))
            .expect("atomic_write_with should succeed");

        assert_eq!(fs::read(&path).expect("file should exist"), b"streamed");
    }

    #[test]
    fn atomic_write_overwrites_an_existing_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("file.txt");
        fs::write(&path, b"old").expect("seed file");

        atomic_write(&path, b"new").expect("atomic_write should succeed");

        assert_eq!(fs::read(&path).expect("file should exist"), b"new");
    }
}
