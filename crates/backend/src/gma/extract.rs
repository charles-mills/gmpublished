use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufWriter, Read, Write},
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
    GmaError, GmaFile, GmaMetadata, is_unsafe_entry_path,
    read::{GmaIndexedEntry, GmaView},
    whitelist::{self, AddonWhitelist},
};

use crate::util::{main_thread_forbidden, thread_pool};
use parking_lot::Mutex;
use rayon::{
    ThreadPool,
    iter::{IntoParallelRefIterator, ParallelIterator},
};
use serde::{Deserialize, Serialize};

static THREAD_POOL: LazyLock<ThreadPool> = LazyLock::new(|| thread_pool!());

/// What to do when a GMA's extraction directory already exists.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ExtractionOverwriteMode {
    /// Extracts over the existing directory, replacing files the GMA
    /// contains and leaving everything else in place.
    Overwrite,
    /// Moves the existing directory to the trash first.
    #[default]
    Recycle,
    /// Removes the existing directory permanently first.
    Delete,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
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
    fn resolve_with_context<S: AsRef<str>>(
        self,
        extracted_name: S,
        context: &ExtractionAppDataContext,
    ) -> Result<(PathBuf, bool), GmaError> {
        use ExtractDestination::{Addons, Directory, Downloads, NamedDirectory, Temp};

        let push_extracted_name = |mut path: PathBuf| {
            path.push(extracted_name.as_ref());
            path
        };

        let recycle_existing = !matches!(self, Directory(_));

        let path = match self {
            Temp => push_extracted_name(context.temp_dir.clone()),
            Directory(path) => path,
            Addons => {
                let mut path = context.gmod_dir.clone().ok_or(GmaError::GmodPathMissing)?;
                path.push("GarrysMod");
                path.push("addons");
                push_extracted_name(path)
            }
            Downloads => push_extracted_name(
                context
                    .downloads_dir
                    .clone()
                    .unwrap_or_else(|| context.temp_dir.clone()),
            ),
            NamedDirectory(path) => push_extracted_name(path),
        };

        Ok((path, recycle_existing))
    }
}

#[derive(Clone, Debug)]
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

/// Everything an extraction needs after service-backed settings, paths and
/// whitelist policy have been resolved. Consumed by [`GmaView::extract`], so
/// the extraction core cannot discover paths or observe a different settings
/// snapshot halfway through the operation.
#[derive(Clone, Debug)]
pub struct ExtractionContext {
    destination: PathBuf,
    recycle_existing: bool,
    overwrite_mode: ExtractionOverwriteMode,
    open_after: bool,
    whitelist: Option<Arc<Vec<String>>>,
}

impl ExtractionContext {
    pub fn resolve(
        handle: &GmaFile,
        destination: ExtractDestination,
        options: ExtractOptions,
        whitelist: &AddonWhitelist,
        app_data: &AppData,
        steam: &Steam,
    ) -> Result<Self, GmaError> {
        let paths = ExtractionAppDataContext::for_destination(&destination, app_data, steam);
        let (destination, recycle_existing) =
            destination.resolve_with_context(&handle.extracted_name, &paths)?;
        let whitelist =
            matches!(options.whitelist, Whitelist::Enforce).then(|| whitelist.snapshot());

        Ok(Self {
            destination,
            recycle_existing,
            overwrite_mode: paths.overwrite_mode,
            open_after: options.open_after,
            whitelist,
        })
    }

    fn prepare_destination(self) -> Result<Self, GmaError> {
        self.prepare_destination_with(cleanup_existing_destination)
    }

    fn prepare_destination_with(
        mut self,
        cleanup_existing: impl Fn(&Path, &ExtractionOverwriteMode) -> bool,
    ) -> Result<Self, GmaError> {
        if self.recycle_existing
            && self.destination.exists()
            && !cleanup_existing(&self.destination, &self.overwrite_mode)
        {
            use_suffixed_fallback_destination(&mut self.destination)?;
        }
        Ok(self)
    }
}

/// Clears an existing destination so extraction can use it. `false` means the
/// path could not be made usable and a suffixed sibling should be used instead.
fn cleanup_existing_destination(path: &Path, overwrite_mode: &ExtractionOverwriteMode) -> bool {
    match overwrite_mode {
        // Nothing to clear: the point of this mode is that files the GMA does
        // not contain survive.
        ExtractionOverwriteMode::Overwrite => true,
        ExtractionOverwriteMode::Delete => fs::remove_dir_all(path).is_ok(),
        ExtractionOverwriteMode::Recycle => trash::delete(path).is_ok(),
    }
}

/// Tries suffixed sibling names (`name (1)`, `name (2)`, ...) until an
/// unused one turns up. Errors once every suffix up to `(255)` is taken
/// rather than silently handing back the popped parent directory.
fn use_suffixed_fallback_destination(path: &mut PathBuf) -> Result<(), GmaError> {
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

    Err(GmaError::DestinationUnavailable)
}

/// Tracks compressed bytes handed to the LZMA decoder so decompression
/// progress can be reported against the on-disk payload size.
struct CountingReader<R> {
    inner: R,
    bytes_read: u64,
    failure: Arc<Mutex<Option<crate::IoFailure>>>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.inner.read(buf) {
            Ok(n) => {
                self.bytes_read += n as u64;
                Ok(n)
            }
            Err(error) => {
                if error.kind() != std::io::ErrorKind::Interrupted {
                    *self.failure.lock() = Some((&error).into());
                }
                Err(error)
            }
        }
    }
}

enum DecompressionSink {
    Memory(Vec<u8>),
    Spill {
        writer: BufWriter<File>,
        temp_path: tempfile::TempPath,
        written: u64,
    },
}

impl DecompressionSink {
    fn new(known_size: Option<u64>, temp_dir: &Path) -> Result<Self, GmaError> {
        if let Some(size) = known_size
            && size <= GmaFile::DECOMPRESS_MEMBUFFER_MAX
        {
            return Ok(Self::Memory(Vec::with_capacity(size as usize)));
        }

        let configured_temp = fs::create_dir_all(temp_dir).and_then(|()| {
            tempfile::Builder::new()
                .prefix("gmpublisher_decompress")
                .suffix(".gma")
                .tempfile_in(temp_dir)
        });
        let temp_file = configured_temp.or_else(|configured_error| {
            tempfile::Builder::new()
                .prefix("gmpublisher_decompress")
                .suffix(".gma")
                .tempfile()
                .map_err(|fallback_error| {
                    std::io::Error::new(
                        fallback_error.kind(),
                        format!(
                            "configured temp output failed: {configured_error}; system temp output failed: {fallback_error}"
                        ),
                    )
                })
        });
        let (file, temp_path) = temp_file
            .map_err(|error| GmaError::DecompressionOutput(error.into()))?
            .into_parts();

        Ok(Self::Spill {
            writer: BufWriter::new(file),
            temp_path,
            written: 0,
        })
    }

    fn len(&self) -> u64 {
        match self {
            Self::Memory(output) => output.len() as u64,
            Self::Spill { written, .. } => *written,
        }
    }

    fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), GmaError> {
        match self {
            Self::Memory(output) => {
                output.extend_from_slice(chunk);
                Ok(())
            }
            Self::Spill {
                writer, written, ..
            } => {
                writer
                    .write_all(chunk)
                    .map_err(|error| GmaError::DecompressionOutput(error.into()))?;
                *written += chunk.len() as u64;
                Ok(())
            }
        }
    }

    fn flush(&mut self) -> Result<(), GmaError> {
        if let Self::Spill { writer, .. } = self {
            writer
                .flush()
                .map_err(|error| GmaError::DecompressionOutput(error.into()))?;
        }
        Ok(())
    }
}

fn classify_decoder_error(
    error: std::io::Error,
    input_failure: &Mutex<Option<crate::IoFailure>>,
) -> GmaError {
    input_failure.lock().take().map_or_else(
        || GmaError::Lzma(error.into()),
        GmaError::DecompressionInput,
    )
}

impl GmaFile {
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
    ) -> Result<(Self, GmaView), GmaError> {
        main_thread_forbidden!();

        let mut input = File::open(path.as_ref())?;

        let bytes_total = input.metadata().map(|metadata| metadata.len()).ok();

        // Legacy Workshop payloads are LZMA-alone (.lzma) streams: a 13-byte
        // header (props byte, u32 LE dictionary size, u64 LE unpacked size)
        // followed by the raw LZMA stream. Parse the header here so the exact
        // unpacked size can size the output buffer; u64::MAX means unknown.
        let mut header = [0u8; 13];
        input
            .read_exact(&mut header)
            .map_err(|error| GmaError::DecompressionInput(error.into()))?;
        let props = header[0];
        let dict_size = u32::from_le_bytes(header[1..5].try_into().unwrap());
        let unpacked_size = u64::from_le_bytes(header[5..13].try_into().unwrap());
        let known_size = (unpacked_size != u64::MAX).then_some(unpacked_size);

        let input_failure = Arc::new(Mutex::new(None));
        let counting = CountingReader {
            inner: std::io::BufReader::new(input),
            bytes_read: header.len() as u64,
            failure: Arc::clone(&input_failure),
        };
        let mut decoder =
            lzma_rust2::LzmaReader::new_with_props(counting, unpacked_size, props, dict_size, None)
                .map_err(|error| classify_decoder_error(error, &input_failure))?;

        let temp_dir = app_data.extraction_context(steam, false).temp_dir;
        let mut sink = DecompressionSink::new(known_size, &temp_dir)?;

        if let Some(bytes_total) = bytes_total {
            transaction.data(crate::transactions::TransactionPayload::ByteSize {
                source: None,
                bytes: bytes_total,
            });
        }

        const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
        let mut buf = vec![0u8; 64 * 1024];
        let mut last_report = std::time::Instant::now();

        loop {
            if transaction.aborted() {
                return Err(GmaError::Cancelled);
            }

            match decoder.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => sink.write_chunk(&buf[..n])?,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(classify_decoder_error(error, &input_failure)),
            }

            if let Some(bytes_total) = bytes_total
                && last_report.elapsed() >= PROGRESS_INTERVAL
            {
                last_report = std::time::Instant::now();
                transaction.progress(decoder.inner().bytes_read as f64 / bytes_total as f64);

                let decompressed_bytes = sink.len();
                if decompressed_bytes > bytes_total {
                    transaction.data(crate::transactions::TransactionPayload::ByteSize {
                        source: None,
                        bytes: decompressed_bytes,
                    });
                }
            }
        }

        sink.flush()?;

        match sink {
            DecompressionSink::Memory(mut output) => {
                // No-op when the header's unpacked size was exact; only a
                // truncated-but-valid stream leaves spare capacity behind.
                output.shrink_to_fit();

                let view = GmaView::from_membuffer(output.into());
                let handle = view.handle(path)?;
                Ok((handle, view))
            }
            DecompressionSink::Spill {
                writer, temp_path, ..
            } => {
                drop(writer);

                let view = GmaView::from_temp_backing(temp_path)?;
                let handle = view.handle(path)?;
                Ok((handle, view))
            }
        }
    }
}

fn write_entry(
    view: &GmaView,
    entry: &GmaIndexedEntry,
    entry_path: &Path,
    transaction: Option<&Transaction>,
) -> Result<(), GmaError> {
    use std::io::Write;

    let parent = entry_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::Builder::new()
        // A constant prefix keeps the temporary component safely below the
        // filesystem limit even when the destination itself is near it.
        // `tempfile_in(parent)` still gives us the same-filesystem atomic
        // persist guarantee; including the destination name adds no safety.
        .prefix(".gmpublished-entry.")
        .suffix(".tmp")
        .tempfile_in(parent)?;

    let mut w = BufWriter::new(temp.as_file_mut());
    let mut payload = view.payload_reader(entry)?;
    crate::stream_bytes(&mut payload, &mut w, entry.size, transaction)?;
    w.flush()?;
    drop(w);
    if transaction.is_some_and(Transaction::aborted) {
        return Err(GmaError::Cancelled);
    }
    temp.persist(entry_path).map_err(|error| error.error)?;

    Ok(())
}

/// Writes `addon.json` for `GmaMetadata::Standard` addons; a no-op for
/// `Legacy` metadata, which has nothing to serialize. Runs straight-line,
/// exactly once, after the parallel entry loop has fully joined and before
/// the transaction is reported finished — a half-extracted addon should
/// never look "done" while its manifest is still missing.
fn write_addon_json(handle: &GmaFile, dest_path: &Path) -> std::io::Result<()> {
    let GmaMetadata::Standard { .. } = &handle.metadata else {
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Whitelist {
    Enforce,
    Ignore,
}

#[derive(Clone, Copy, Debug)]
pub struct ExtractOptions {
    pub open_after: bool,
    pub whitelist: Whitelist,
}

impl GmaView {
    pub fn extract(
        &self,
        handle: &GmaFile,
        transaction: &Transaction,
        context: ExtractionContext,
    ) -> Result<PathBuf, GmaError> {
        let context = match context.prepare_destination() {
            Ok(context) => context,
            Err(error) => {
                if !transaction.aborted() {
                    transaction.error(&error);
                }
                return Err(error);
            }
        };
        let ExtractionContext {
            destination: dest_path,
            recycle_existing: _,
            overwrite_mode: _,
            open_after,
            whitelist: whitelist_snapshot,
        } = context;

        let result = THREAD_POOL.install(|| -> Result<PathBuf, GmaError> {
            // Only a destination that survived cleanup (or was never
            // touched, e.g. an explicit `Directory`) can carry out-of-band
            // symlinks; a freshly allocated one has nothing planted in it.
            let dest_existed = dest_path.exists();

            let entries = self.extraction_entries()?;
            let entries_len_f = entries.len() as f64;

            let i = AtomicUsize::new(0);
            let extracted = AtomicUsize::new(0);
            let failed = AtomicUsize::new(0);
            let rejected = AtomicUsize::new(0);
            let first_error: OnceLock<Arc<str>> = OnceLock::new();
            let verified_dirs: Mutex<HashSet<PathBuf>> = Mutex::new(HashSet::new());
            let record_first_error = |message: String| {
                let _ = first_error.set(message.into());
            };

            entries
                .par_iter()
                .try_for_each(|entry| -> Result<(), GmaError> {
                    let entry_path = &entry.path;
                    if whitelist_snapshot
                        .as_ref()
                        .is_none_or(|snapshot| whitelist::is_whitelisted_in(snapshot, entry_path))
                    {
                        if transaction.aborted() {
                            return Err(GmaError::Cancelled);
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
                            match write_entry(self, entry, &final_path, None) {
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
                return Err(GmaError::ExtractionFailed {
                    extracted,
                    failed: 1,
                    rejected,
                    first_error: Some(format!("failed to write addon.json: {err}").into()),
                });
            }

            if failed > 0 || extracted == 0 {
                return Err(GmaError::ExtractionFailed {
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
        handle: &GmaFile,
        entry_path: String,
        transaction: &Transaction,
        options: ExtractOptions,
        app_data: &AppData,
        steam: &Steam,
    ) -> Result<PathBuf, GmaError> {
        // A single entry always lands in the temp dir, so the destination half
        // of `ExtractOptions` has nothing to choose; only `open_after` applies.
        let ExtractOptions {
            open_after: open_after_extract,
            whitelist: _,
        } = options;
        let context = ExtractionAppDataContext::for_temp_entry(app_data, steam);
        let mut base = context.temp_dir;
        base.push("gmpublisher");
        base.push(&handle.extracted_name);

        let mut path = base.clone();
        path.push(&entry_path);

        if !path.starts_with(&base) {
            return Err(GmaError::FormatError);
        }

        // Unsafe entry paths must stay invisible here, exactly as `entries`
        // filters them out of its projection; the `starts_with` check above
        // does not resolve `..` components.
        let entries = self.extraction_entries()?;
        let entry = (!is_unsafe_entry_path(&entry_path))
            .then(|| entries.iter().find(|entry| entry.path == entry_path))
            .flatten()
            .ok_or(GmaError::EntryNotFound)?;
        let result = write_entry(self, entry, &path, Some(transaction)).map(|_| path.clone());

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
