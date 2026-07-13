use rayon::{
    ThreadPool,
    iter::{IntoParallelRefIterator, ParallelIterator},
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufWriter, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::LazyLock,
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use walkdir::WalkDir;

use crate::{GMAFile, transactions::Transaction, write_nt_string};

use super::{GMAError, GMAMetadata, whitelist, whitelist::AddonWhitelist};

use super::GMA_HEADER;

static THREAD_POOL: LazyLock<ThreadPool> = LazyLock::new(|| thread_pool!());

/// Small files are read in parallel batches of at most this many bytes, so
/// peak memory stays bounded no matter how large the addon is.
const BATCH_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// Files larger than this never enter a batch; they stream straight through
/// a fixed-size chunk buffer.
const BATCH_FILE_MAX: u64 = 8 * 1024 * 1024;
const STREAM_CHUNK: usize = 1024 * 1024;

/// A unique path in `final_path`'s own directory, so a pack that never
/// finishes leaves nothing at `final_path` and two packs to the same
/// destination never share a file.
fn unique_temp_path(final_path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gmpublisher");

    final_path.with_file_name(format!(".{file_name}.{nanos}.{counter}.tmp"))
}

/// Deletes the temp file it guards unless [`Self::commit`] was called. Covers
/// every early return in [`GMAFile::create`] (an error, a panic unwind, a
/// cancelled transaction) with one mechanism instead of a cleanup call at
/// each exit point.
struct TempFileGuard {
    path: PathBuf,
    committed: bool,
}
impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn commit(mut self) {
        self.committed = true;
    }
}
impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Converts a path already relative to the source root into the
/// forward-slash entry name a GMA stores. `None` for a non-UTF8 relative
/// path: a GMA entry name must be a valid string, so this is treated as an
/// error by the caller, never silently mangled.
fn relative_entry_name(relative: &Path) -> Option<String> {
    relative
        .to_str()
        .map(|path| path.replace('\\', "/").trim_matches('/').to_lowercase())
}

impl GMAFile {
    pub fn create<P: AsRef<Path>>(
        &self,
        src_path: P,
        transaction: &Transaction,
        whitelist: &AddonWhitelist,
    ) -> Result<(), GMAError> {
        let temp_path = unique_temp_path(&self.path);
        let guard = TempFileGuard::new(temp_path.clone());
        let mut f = BufWriter::new(File::create(&temp_path)?);

        let src_path = src_path.as_ref();

        let metadata = &self.metadata;
        let ignore = metadata
            .ignore()
            .map(|ignore| ignore.clone().into_boxed_slice());

        let (title, addon_json) = match metadata {
            GMAMetadata::Legacy { title, .. } => (title.as_str(), None),
            GMAMetadata::Standard { title, .. } => (title.as_str(), Some(metadata)),
        };

        f.write_all(GMA_HEADER)?;

        f.write_all(&[3])?; // gma version

        // steamid [unused]
        f.write_all(&0u64.to_le_bytes())?;

        // timestamp [unused]
        f.write_all(
            &SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |unix| unix.as_secs())
                .to_le_bytes(),
        )?;

        // required content [unused]
        f.write_all(&[0])?;

        // addon name
        write_nt_string(&mut f, title)?;

        // addon description
        match addon_json {
            Some(addon_json) => {
                write_nt_string(
                    &mut f,
                    serde_json::ser::to_string(addon_json).as_deref().unwrap(),
                )?;
            }
            None => write_nt_string(&mut f, "Description")?,
        };

        // addon author [unused]
        write_nt_string(&mut f, "Author Name")?;

        // addon version [unused]
        f.write_all(&1i32.to_le_bytes())?;

        // Contents are streamed later in bounded batches, never held all at once.
        struct Pending {
            path: PathBuf,
            size: u64,
            crc_offset: u64,
        }

        let path_io_error = |transaction: &Transaction, path: PathBuf| {
            transaction.error(crate::transactions::TransactionError::detailed(
                crate::error_key::keys::PATH_IO_ERROR,
                crate::transactions::detail_from_serialize(path),
            ));
            GMAError::IOError(None)
        };

        let mut file_list: BTreeMap<String, Pending> = BTreeMap::new();
        {
            let whitelist_snapshot = whitelist.snapshot();

            for entry in WalkDir::new(src_path).follow_links(false) {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        let path = err
                            .path()
                            .map_or_else(|| src_path.to_path_buf(), Path::to_path_buf);
                        return Err(path_io_error(transaction, path));
                    }
                };
                if !entry.file_type().is_file() {
                    continue;
                }

                let Some(relative_path) = entry
                    .path()
                    .strip_prefix(src_path)
                    .ok()
                    .and_then(relative_entry_name)
                else {
                    return Err(path_io_error(transaction, entry.into_path()));
                };

                if whitelist::is_whitelisted_in(&whitelist_snapshot, &relative_path) {
                    if let Some(ignore) = ignore.as_ref()
                        && whitelist::is_ignored(&relative_path, ignore)
                    {
                        continue;
                    }
                    // Stat only files that will actually be packed: a broken
                    // ignored/non-whitelisted file must not fail the create.
                    let size = match entry.metadata() {
                        Ok(metadata) => metadata.len(),
                        Err(_) => return Err(path_io_error(transaction, entry.into_path())),
                    };
                    file_list.insert(
                        relative_path,
                        Pending {
                            path: entry.into_path(),
                            size,
                            crc_offset: 0,
                        },
                    );
                } else {
                    transaction.data(
                        crate::transactions::TransactionPayload::WhitelistViolation {
                            path: relative_path,
                        },
                    );
                }
            }
        }

        // Write every file-list record up front (sizes come from the walk);
        // per-entry CRCs are patched in after their contents have streamed.
        let mut file_list: Vec<(String, Pending)> = file_list.into_iter().collect();
        let mut cursor = f.stream_position()?;
        for (i, (relative_path, pending)) in file_list.iter_mut().enumerate() {
            pending.crc_offset = cursor + 4 + relative_path.len() as u64 + 1 + 8;
            cursor += 4 + relative_path.len() as u64 + 1 + 8 + 4;

            f.write_all(&((i + 1) as u32).to_le_bytes())?;
            f.write_all(relative_path.as_bytes())?;
            f.write_all(&[0])?;
            f.write_all(&(pending.size as i64).to_le_bytes())?;
            f.write_all(&0u32.to_le_bytes())?;
        }
        f.write_all(&0u32.to_le_bytes())?;

        // Payload: entries stream in list order. Small files are read in
        // parallel batches capped at BATCH_MAX_BYTES; large files go through
        // a reused chunk buffer. A size that no longer matches the walk means
        // the source changed mid-create — fail rather than corrupt offsets.
        let total = file_list.len() as f64;
        let mut written_files: f64 = 0.;
        let mut crcs: Vec<u32> = Vec::with_capacity(file_list.len());
        let mut chunk = vec![0u8; STREAM_CHUNK];

        let mut i = 0;
        while i < file_list.len() {
            let (_, pending) = &file_list[i];

            if pending.size > BATCH_FILE_MAX {
                let Ok(mut reader) = File::open(&pending.path) else {
                    return Err(path_io_error(transaction, pending.path.clone()));
                };

                let mut crc32 = crc32fast::Hasher::new();
                let mut written: u64 = 0;
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            crc32.update(&chunk[..n]);
                            f.write_all(&chunk[..n])?;
                            written += n as u64;
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => return Err(path_io_error(transaction, pending.path.clone())),
                    }
                }
                if written != pending.size {
                    return Err(path_io_error(transaction, pending.path.clone()));
                }

                crcs.push(crc32.finalize());
                written_files += 1.;
                transaction.progress(written_files / total);
                i += 1;
            } else {
                let mut batch_end = i;
                let mut batch_bytes: u64 = 0;
                while batch_end < file_list.len()
                    && file_list[batch_end].1.size <= BATCH_FILE_MAX
                    && (batch_end == i
                        || batch_bytes + file_list[batch_end].1.size <= BATCH_MAX_BYTES)
                {
                    batch_bytes += file_list[batch_end].1.size;
                    batch_end += 1;
                }

                let batch = &file_list[i..batch_end];
                let results: Vec<Result<(Vec<u8>, u32), PathBuf>> = THREAD_POOL.install(|| {
                    batch
                        .par_iter()
                        .map(|(_, pending)| {
                            let contents =
                                fs::read(&pending.path).map_err(|_| pending.path.clone())?;
                            if contents.len() as u64 != pending.size {
                                return Err(pending.path.clone());
                            }

                            let mut crc32 = crc32fast::Hasher::new();
                            crc32.update(&contents);
                            Ok((contents, crc32.finalize()))
                        })
                        .collect()
                });

                for result in results {
                    match result {
                        Ok((contents, crc32)) => {
                            f.write_all(&contents)?;
                            crcs.push(crc32);
                            written_files += 1.;
                            transaction.progress(written_files / total);
                        }
                        Err(path) => return Err(path_io_error(transaction, path)),
                    }
                }
                i = batch_end;
            }
        }

        for ((_, pending), crc32) in file_list.iter().zip(&crcs) {
            f.seek(std::io::SeekFrom::Start(pending.crc_offset))?;
            f.write_all(&crc32.to_le_bytes())?;
        }

        // Trailer CRC: GMod never reads it and fastgmad writes 0.
        f.seek(std::io::SeekFrom::End(0))?;
        f.write_all(&0u32.to_le_bytes())?;

        f.flush()?;
        f.get_ref().sync_all()?;
        drop(f);

        fs::rename(&temp_path, &self.path)?;
        guard.commit();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_entry_name_lowercases_and_uses_forward_slashes() {
        assert_eq!(
            relative_entry_name(Path::new("Lua/AutoRun/Init.lua")).as_deref(),
            Some("lua/autorun/init.lua")
        );
    }

    #[test]
    #[cfg(unix)]
    fn relative_entry_name_rejects_non_utf8_components() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let bad = OsStr::from_bytes(b"lua/invalid-\x80-name.lua");
        assert_eq!(relative_entry_name(Path::new(bad)), None);
    }

    #[test]
    fn unique_temp_path_is_unique_and_stays_in_the_final_path_s_directory() {
        let final_path = Path::new("/tmp/example/gmpublisher.gma");
        let a = unique_temp_path(final_path);
        let b = unique_temp_path(final_path);

        assert_ne!(a, b);
        assert_eq!(a.parent(), final_path.parent());
        assert_eq!(b.parent(), final_path.parent());
    }
}
