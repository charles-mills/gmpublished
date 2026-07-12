use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufWriter, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use crate::appdata::AppData;
use crate::steam::Steam;
use crate::transactions::Transaction;

use super::{
    GMAError, GMAFile, GMAMetadata,
    read::GmaView,
    whitelist::{self, AddonWhitelist},
};

use parking_lot::Mutex;
use rayon::{
    ThreadPool,
    iter::{IntoParallelRefIterator, ParallelIterator},
};
use serde::{Deserialize, Serialize};

static THREAD_POOL: LazyLock<ThreadPool> = LazyLock::new(|| thread_pool!());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ExtractionOverwriteMode {
    /// Removes the existing destination directory before extracting: a
    /// full replace, not a merge with whatever was there before.
    Overwrite,
    #[default]
    Recycle,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ExtractDestination {
    #[default]
    Temp,
    Downloads,
    Addons,
    /// path/to/addon/*
    Directory(PathBuf),
    /// path/to/addon/addon_name_123456790/*
    NamedDirectory(PathBuf),
}
impl ExtractDestination {
    fn prepare<S: AsRef<str>>(
        self,
        extracted_name: S,
        app_data: &AppData,
        steam: &Steam,
    ) -> Result<PathBuf, GMAError> {
        let context = ExtractionAppDataContext::for_destination(&self, app_data, steam);
        self.prepare_with_context(extracted_name, &context, cleanup_existing_destination)
    }

    fn prepare_with_context<S: AsRef<str>>(
        self,
        extracted_name: S,
        context: &ExtractionAppDataContext,
        cleanup_existing: impl Fn(&Path, &ExtractionOverwriteMode) -> bool,
    ) -> Result<PathBuf, GMAError> {
        use ExtractDestination::{Addons, Directory, Downloads, NamedDirectory, Temp};

        let push_extracted_name = |mut path: PathBuf| {
            path.push(extracted_name.as_ref());
            Some(path)
        };

        let recycle_existing = !matches!(self, Directory(_));

        let mut path = match self {
            Temp => None,

            Directory(path) => Some(path),

            Addons => context.gmod_dir.clone().map(|mut path| {
                path.push("GarrysMod");
                path.push("addons");
                path.push(extracted_name.as_ref());
                path
            }),

            Downloads => context.downloads_dir.clone().and_then(push_extracted_name),

            NamedDirectory(path) => push_extracted_name(path),
        }
        .unwrap_or_else(|| push_extracted_name(context.temp_dir.clone()).unwrap());

        if recycle_existing && path.exists() {
            let success = cleanup_existing(&path, &context.overwrite_mode);
            if !success {
                use_suffixed_fallback_destination(&mut path)?;
            }
        }

        Ok(path)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExtractionAppDataContext {
    pub(crate) temp_dir: PathBuf,
    pub(crate) downloads_dir: Option<PathBuf>,
    pub(crate) gmod_dir: Option<PathBuf>,
    pub(crate) overwrite_mode: ExtractionOverwriteMode,
}

impl ExtractionAppDataContext {
    fn for_destination(
        destination: &ExtractDestination,
        app_data: &AppData,
        steam: &Steam,
    ) -> Self {
        app_data.extraction_context(steam, matches!(destination, ExtractDestination::Addons))
    }

    fn for_temp_entry(app_data: &AppData, steam: &Steam) -> Self {
        app_data.extraction_context(steam, false)
    }
}

fn cleanup_existing_destination(path: &Path, overwrite_mode: &ExtractionOverwriteMode) -> bool {
    match overwrite_mode {
        ExtractionOverwriteMode::Overwrite | ExtractionOverwriteMode::Delete => {
            fs::remove_dir_all(path).is_ok()
        }
        ExtractionOverwriteMode::Recycle => trash::delete(path).is_ok(),
    }
}

/// Tries suffixed sibling names (`name (1)`, `name (2)`, ...) until an
/// unused one turns up. Errors once every suffix up to `(255)` is taken
/// rather than silently handing back the popped parent directory.
fn use_suffixed_fallback_destination(path: &mut PathBuf) -> Result<(), GMAError> {
    // Root/`..`-terminated paths have no file name; fall back to a static one
    // instead of panicking. Normal destinations are unaffected.
    let dir_name = path.file_name().map_or_else(
        || "gma".to_string(),
        |name| name.to_string_lossy().to_string(),
    );
    path.pop();

    for i in 1..=255u8 {
        path.push(format!("{dir_name} ({i})"));
        if !path.exists() {
            return Ok(());
        }
        path.pop();
    }

    Err(GMAError::DestinationUnavailable)
}

/// Tracks compressed bytes handed to the LZMA decoder so decompression
/// progress can be reported against the on-disk payload size.
struct CountingReader<R> {
    inner: R,
    bytes_read: u64,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes_read += n as u64;
        Ok(n)
    }
}

impl GMAFile {
    /// Decompressed payloads at most this large are kept in memory for the
    /// extraction that follows; anything larger (or of unknown size) spills
    /// to a temp .gma so peak RSS stays bounded regardless of addon size.
    const DECOMPRESS_MEMBUFFER_MAX: u64 = 256 * 1024 * 1024;

    /// Decompresses a legacy Workshop `.bin` payload. Returns the parsed
    /// identity handle alongside the [`GmaView`] holding its bytes: there is
    /// no on-disk GMA to re-view later (`path` names the original
    /// compressed payload), so callers keep the view for extraction.
    pub fn decompress<P: AsRef<Path>>(
        path: P,
        transaction: &Transaction,
        app_data: &AppData,
        steam: &Steam,
    ) -> Result<(Self, GmaView), GMAError> {
        main_thread_forbidden!();

        let mut input = File::open(path.as_ref())?;

        let bytes_total = input.metadata().map(|metadata| metadata.len()).ok();

        // Legacy Workshop payloads are LZMA-alone (.lzma) streams: a 13-byte
        // header (props byte, u32 LE dictionary size, u64 LE unpacked size)
        // followed by the raw LZMA stream. Parse the header here so the exact
        // unpacked size can size the output buffer; u64::MAX means unknown.
        let mut header = [0u8; 13];
        input.read_exact(&mut header).map_err(|err| {
            log::error!("LZMA error: {err:?}");
            GMAError::LZMA
        })?;
        let props = header[0];
        let dict_size = u32::from_le_bytes(header[1..5].try_into().unwrap());
        let unpacked_size = u64::from_le_bytes(header[5..13].try_into().unwrap());
        let known_size = (unpacked_size != u64::MAX).then_some(unpacked_size);

        let counting = CountingReader {
            inner: std::io::BufReader::new(input),
            bytes_read: header.len() as u64,
        };
        let mut decoder =
            lzma_rust2::LzmaReader::new_with_props(counting, unpacked_size, props, dict_size, None)
                .map_err(|err| {
                    log::error!("LZMA error: {err:?}");
                    GMAError::LZMA
                })?;

        enum Sink {
            Mem(Vec<u8>),
            Disk {
                writer: BufWriter<File>,
                temp_path: tempfile::TempPath,
                written: u64,
            },
        }

        let mut sink = match known_size {
            Some(size) if size <= Self::DECOMPRESS_MEMBUFFER_MAX => {
                Sink::Mem(Vec::with_capacity(size as usize))
            }
            _ => {
                let temp_dir = app_data.extraction_context(steam, false).temp_dir;
                let temp_file = fs::create_dir_all(&temp_dir)
                    .ok()
                    .and_then(|_| {
                        tempfile::Builder::new()
                            .prefix("gmpublisher_decompress")
                            .suffix(".gma")
                            .tempfile_in(&temp_dir)
                            .ok()
                    })
                    .map_or_else(
                        || {
                            tempfile::Builder::new()
                                .prefix("gmpublisher_decompress")
                                .suffix(".gma")
                                .tempfile()
                        },
                        Ok,
                    )?;
                let (file, temp_path) = temp_file.into_parts();
                Sink::Disk {
                    writer: BufWriter::new(file),
                    temp_path,
                    written: 0,
                }
            }
        };

        let result = {
            use std::io::Write;

            let sink_len = |sink: &Sink| match sink {
                Sink::Mem(output) => output.len() as u64,
                Sink::Disk { written, .. } => *written,
            };

            let write_chunk = |sink: &mut Sink, chunk: &[u8]| -> std::io::Result<()> {
                match sink {
                    Sink::Mem(output) => {
                        output.extend_from_slice(chunk);
                        Ok(())
                    }
                    Sink::Disk {
                        writer, written, ..
                    } => {
                        writer.write_all(chunk)?;
                        *written += chunk.len() as u64;
                        Ok(())
                    }
                }
            };

            if let Some(bytes_total) = bytes_total {
                transaction.data(crate::transactions::TransactionPayload::ByteSize {
                    source: None,
                    bytes: bytes_total,
                });

                let bytes_total_f = bytes_total as f64;

                const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
                let mut buf = vec![0u8; 64 * 1024];
                let mut last_report = std::time::Instant::now();

                loop {
                    if transaction.aborted() {
                        return Err(GMAError::Cancelled);
                    }
                    match decoder.read(&mut buf) {
                        Ok(0) => break Ok(()),
                        Ok(n) => {
                            if let Err(err) = write_chunk(&mut sink, &buf[..n]) {
                                break Err(err);
                            }

                            if last_report.elapsed() >= PROGRESS_INTERVAL {
                                last_report = std::time::Instant::now();

                                transaction
                                    .progress(decoder.inner().bytes_read as f64 / bytes_total_f);

                                let decompressed_bytes = sink_len(&sink);
                                if decompressed_bytes > bytes_total {
                                    transaction.data(
                                        crate::transactions::TransactionPayload::ByteSize {
                                            source: None,
                                            bytes: decompressed_bytes,
                                        },
                                    );
                                }
                            }
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(err) => break Err(err),
                    }
                }
            } else {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    if transaction.aborted() {
                        return Err(GMAError::Cancelled);
                    }
                    match decoder.read(&mut buf) {
                        Ok(0) => break Ok(()),
                        Ok(n) => {
                            if let Err(err) = write_chunk(&mut sink, &buf[..n]) {
                                break Err(err);
                            }
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(err) => break Err(err),
                    }
                }
            }
        };

        if let Err(err) = result {
            log::error!("LZMA error: {err:#?}");
            return Err(GMAError::LZMA);
        }

        match sink {
            Sink::Mem(mut output) => {
                // No-op when the header's unpacked size was exact; only a
                // truncated-but-valid stream leaves spare capacity behind.
                output.shrink_to_fit();

                let view = GmaView::from_membuffer(output.into(), path.as_ref());
                let handle = view.handle(path)?;
                Ok((handle, view))
            }
            Sink::Disk {
                mut writer,
                temp_path,
                ..
            } => {
                use std::io::Write;

                writer.flush()?;
                drop(writer);

                let view = GmaView::from_temp_backing(temp_path, path.as_ref())?;
                let handle = view.handle(path)?;
                Ok((handle, view))
            }
        }
    }
}

fn write_entry_bytes(
    payload: &[u8],
    entry_path: &PathBuf,
    transaction: Option<&Transaction>,
) -> Result<(), GMAError> {
    use std::io::Write;

    fs::create_dir_all(entry_path.with_file_name(""))?;
    let f = File::create(entry_path)?;

    let mut w = BufWriter::new(f);
    crate::stream_bytes(&mut &payload[..], &mut w, payload.len(), transaction)?;

    w.flush()?;

    Ok(())
}

/// Writes `addon.json` for `GMAMetadata::Standard` addons; a no-op for
/// `Legacy` metadata, which has nothing to serialize. Runs straight-line,
/// exactly once, after the parallel entry loop has fully joined and before
/// the transaction is reported finished — a half-extracted addon should
/// never look "done" while its manifest is still missing.
fn write_addon_json(handle: &GMAFile, dest_path: &Path) -> std::io::Result<()> {
    let GMAMetadata::Standard { .. } = &handle.metadata else {
        return Ok(());
    };
    let json = serde_json::ser::to_string_pretty(&handle.metadata)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    fs::create_dir_all(dest_path)?;
    fs::write(dest_path.join("addon.json"), json.as_bytes())
}

/// Confirms no directory from `root` (inclusive) down to `leaf_dir`
/// (inclusive) is a symlink, so extracting into a destination that already
/// existed before this run can't be redirected through a symlink planted by
/// something other than this extraction. `verified` caches directories
/// already cleared so sibling entries sharing an ancestor don't re-walk it.
///
/// This is cheap defense-in-depth, not a hard guarantee: it doesn't close
/// the window between this check and the write that follows it. GMA entries
/// themselves can never carry a symlink entry type, so the gap this guards
/// is purely out-of-band filesystem state (something other than this
/// extraction placing a symlink in a destination directory reused across
/// runs).
fn verify_no_symlink_ancestors(
    root: &Path,
    leaf_dir: &Path,
    verified: &Mutex<HashSet<PathBuf>>,
) -> std::io::Result<()> {
    let mut to_check = Vec::new();
    let mut current = leaf_dir;
    loop {
        if verified.lock().contains(current) {
            break;
        }
        to_check.push(current.to_path_buf());
        if current == root {
            break;
        }
        match current.parent() {
            Some(parent) if parent.starts_with(root) => current = parent,
            _ => break,
        }
    }

    for dir in to_check.iter().rev() {
        if fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(std::io::Error::other(format!(
                "{} is a symlink",
                dir.display()
            )));
        }
    }

    verified.lock().extend(to_check);
    Ok(())
}

/// Whether an extraction bypasses the addon-content whitelist. `Enforce` is
/// the safety-relevant default; `Ignore` is opt-in (previews, CLI extraction,
/// downloads of addons Steam already accepted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whitelist {
    Enforce,
    Ignore,
}

#[derive(Debug, Clone, Copy)]
pub struct ExtractOptions {
    pub open_after: bool,
    pub whitelist: Whitelist,
}

impl GmaView {
    #[expect(clippy::too_many_arguments)]
    pub fn extract(
        &self,
        handle: &GMAFile,
        dest: ExtractDestination,
        transaction: &Transaction,
        options: ExtractOptions,
        whitelist: &AddonWhitelist,
        app_data: &AppData,
        steam: &Steam,
    ) -> Result<PathBuf, GMAError> {
        let ExtractOptions {
            open_after,
            whitelist: whitelist_mode,
        } = options;

        let result = THREAD_POOL.install(|| -> Result<PathBuf, GMAError> {
            let dest_path = dest.prepare(&handle.extracted_name, app_data, steam)?;
            // Only a destination that survived cleanup (or was never
            // touched, e.g. an explicit `Directory`) can carry out-of-band
            // symlinks; a freshly allocated one has nothing planted in it.
            let dest_existed = dest_path.exists();

            let entries = self.entries()?;
            let entries_len_f = entries.len() as f64;

            // Fail before spinning up threads if the file no longer parses.
            let parsed = self.parse()?;

            let i = AtomicUsize::new(0);
            let extracted = AtomicUsize::new(0);
            let failed = AtomicUsize::new(0);
            let rejected = AtomicUsize::new(0);
            let first_error: OnceLock<Arc<str>> = OnceLock::new();
            let verified_dirs: Mutex<HashSet<PathBuf>> = Mutex::new(HashSet::new());
            let whitelist_snapshot = whitelist.snapshot();

            let record_first_error = |message: String| {
                let _ = first_error.set(message.into());
            };

            entries
                .par_iter()
                .try_for_each(|(entry_path, entry)| -> Result<(), GMAError> {
                    if matches!(whitelist_mode, Whitelist::Ignore)
                        || whitelist::is_whitelisted_in(&whitelist_snapshot, entry_path)
                    {
                        if transaction.aborted() {
                            return Err(GMAError::Cancelled);
                        }

                        let final_path = dest_path.join(entry_path);
                        if !final_path.starts_with(&dest_path) {
                            failed.fetch_add(1, Ordering::AcqRel);
                            record_first_error(format!("unsafe entry path: {entry_path}"));
                            log::warn!("Refusing to extract unsafe entry path: {entry_path}");
                        } else if dest_existed
                            && let Err(err) = verify_no_symlink_ancestors(
                                &dest_path,
                                final_path.parent().unwrap_or(&dest_path),
                                &verified_dirs,
                            )
                        {
                            failed.fetch_add(1, Ordering::AcqRel);
                            record_first_error(format!(
                                "refusing to extract {}: {err}",
                                final_path.display()
                            ));
                            log::warn!("Refusing to extract {}: {err}", final_path.display());
                        } else {
                            match parsed.entry_bytes(entry.index as usize) {
                                Ok(payload) => {
                                    match write_entry_bytes(payload, &final_path, None) {
                                        Ok(()) => {
                                            extracted.fetch_add(1, Ordering::AcqRel);
                                        }
                                        Err(err) => {
                                            failed.fetch_add(1, Ordering::AcqRel);
                                            record_first_error(format!(
                                                "failed to extract entry to {}: {err}",
                                                final_path.display()
                                            ));
                                            log::warn!(
                                                "Failed to extract entry to {}: {err}",
                                                final_path.display()
                                            );
                                        }
                                    }
                                }
                                Err(err) => {
                                    failed.fetch_add(1, Ordering::AcqRel);
                                    record_first_error(format!(
                                        "failed to extract entry to {}: {err}",
                                        final_path.display()
                                    ));
                                    log::warn!(
                                        "Failed to extract entry to {}: {err}",
                                        final_path.display()
                                    );
                                }
                            }
                        }
                    } else {
                        rejected.fetch_add(1, Ordering::AcqRel);
                        record_first_error(format!("entry rejected by whitelist: {entry_path}"));
                    }

                    let i = i.fetch_add(1, Ordering::AcqRel) + 1;
                    transaction.progress((i as f64) / entries_len_f);

                    Ok(())
                })?;

            let extracted = extracted.into_inner();
            let failed = failed.into_inner();
            let rejected = rejected.into_inner();
            let mut first_error = first_error.into_inner();

            // A manifest write failure on an otherwise-complete extraction
            // still means the addon didn't fully land; fold it into the
            // same failed-entry accounting rather than a separate outcome.
            if failed == 0
                && extracted > 0
                && let Err(err) = write_addon_json(handle, &dest_path)
            {
                return Err(GMAError::ExtractionFailed {
                    extracted,
                    failed: 1,
                    rejected,
                    first_error: Some(format!("failed to write addon.json: {err}").into()),
                });
            }

            if failed > 0 || extracted == 0 {
                return Err(GMAError::ExtractionFailed {
                    extracted,
                    failed,
                    rejected,
                    first_error: first_error.take(),
                });
            }

            Ok(dest_path)
        });

        match &result {
            Ok(dest_path) => {
                if !transaction.aborted() {
                    transaction.finished(crate::transactions::TransactionPayload::ExtractedPath(
                        dest_path.clone(),
                    ));
                    if open_after {
                        // Failure is already logged; extraction itself succeeded.
                        let _ = crate::path::open(dest_path);
                    }
                }
            }
            Err(error) => {
                if !transaction.aborted() {
                    transaction.error(error);
                }
            }
        }

        result
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "the app-layer caller across the crate boundary already owns this string"
    )]
    pub fn extract_entry(
        &self,
        handle: &GMAFile,
        entry_path: String,
        transaction: &Transaction,
        open_after_extract: bool,
        app_data: &AppData,
        steam: &Steam,
    ) -> Result<PathBuf, GMAError> {
        let context = ExtractionAppDataContext::for_temp_entry(app_data, steam);
        let mut base = context.temp_dir;
        base.push("gmpublisher");
        base.push(&handle.extracted_name);

        let mut path = base.clone();
        path.push(&entry_path);

        if !path.starts_with(&base) {
            return Err(GMAError::FormatError);
        }

        let entries = self.entries()?;
        let entry = entries.get(&entry_path).ok_or(GMAError::EntryNotFound)?;

        let parsed = self.parse()?;
        let result = parsed
            .entry_bytes(entry.index as usize)
            .map_err(|_| GMAError::FormatError)
            .and_then(|payload| write_entry_bytes(payload, &path, Some(transaction)))
            .map(|_| path.clone());

        if let Err(error) = &result {
            if !transaction.aborted() {
                transaction.error(error);
            }
        } else if !transaction.aborted() {
            if open_after_extract {
                transaction.finished(crate::transactions::TransactionPayload::ExtractedPath(
                    path.clone(),
                ));
                // Failure is already logged; extraction itself succeeded.
                let _ = crate::path::open(path);
            } else {
                transaction.finished(crate::transactions::TransactionPayload::ExtractedPath(path));
            }
        }

        result
    }
}

#[cfg(test)]
mod tests;
