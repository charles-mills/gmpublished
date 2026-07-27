use image::{
    ColorType, ImageEncoder, ImageFormat, ImageReader,
    codecs::png::{CompressionType as PngCompressionType, FilterType as PngFilterType, PngEncoder},
};
use std::{
    borrow::Borrow,
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

use super::gif_preview::decode_lazy_gif_preview;
use super::thumbnail::thumbnail_decode_limits;
use super::thumbnail_key::{THUMBNAIL_CACHE_FILE_EXTENSION as CACHE_FILE_EXTENSION, ThumbnailKey};
use super::{Thumbnail, ThumbnailMetadata};

const CACHE_PAYLOAD_MAGIC: &[u8; 8] = b"GMPTB001";
const CACHE_PAYLOAD_VERSION: u32 = 1;
const CACHE_FORMAT_RAW_RGBA: u8 = 0;
const CACHE_FORMAT_PNG_RGBA: u8 = 1;
// Animated GIF: the payload is the raw encoded GIF stream, replayed on read.
const CACHE_FORMAT_GIF: u8 = 2;

#[derive(Clone, Debug)]
pub struct WorkerDiskCache {
    dir: PathBuf,
    state: Arc<DiskCacheState>,
}

impl WorkerDiskCache {
    pub(crate) fn new(dir: PathBuf, max_bytes: u64) -> Self {
        Self {
            dir,
            state: Arc::new(DiskCacheState {
                max_bytes: AtomicU64::new(max_bytes),
                index: Mutex::new(DiskCacheIndex::default()),
            }),
        }
    }

    fn max_bytes(&self) -> u64 {
        self.state.max_bytes.load(Ordering::Relaxed)
    }

    /// Resizes the eviction budget; clones share it. A shrink takes effect
    /// on the next write's eviction pass rather than immediately.
    pub(crate) fn set_max_bytes(&self, max_bytes: u64) {
        self.state.max_bytes.store(max_bytes, Ordering::Relaxed);
    }

    /// Builds the on-disk index now, if it is not built already.
    ///
    /// Exists so the build can be *placed*. It is a `read_dir` plus one `stat`
    /// per cached file, and it happens lazily inside whichever call touches the
    /// index first. Left to itself that call is the whole-library warm skip,
    /// which runs on the main thread inside a single `update` — so a warm start
    /// with a full cache paid a directory scan mid-frame. Calling this from a
    /// blocking task beforehand moves the syscalls off the main thread without
    /// changing what any caller sees.
    ///
    /// Idempotent and racy-safe: a worker that got there first leaves this a
    /// mutex acquire.
    pub(crate) fn prime_index(&self) {
        let mut index = self.state.index.lock();
        if let Err(error) = ensure_disk_cache_index(self, &mut index) {
            log::debug!(
                "failed to pre-build thumbnail disk index {}: {error}",
                self.dir.display()
            );
        }
    }

    /// Whether a URL's fetched bytes are in the source tier, by index lookup —
    /// no stat.
    ///
    /// Deliberately asks about the source rather than a derived entry. The
    /// derived tier is size-keyed, so it answers "can this exact request be
    /// served without decoding?"; the source tier is URL-keyed and answers "is
    /// this URL local at all?". Background warming wants the second, because
    /// the source is what it produces.
    pub(crate) fn contains_source(&self, url: &str) -> bool {
        let path = source_cache_path(&self.dir, url);
        let mut index = self.state.index.lock();
        if ensure_disk_cache_index(self, &mut index).is_err() {
            return false;
        }
        index.by_path.contains_key(path.as_path())
    }
}

#[derive(Debug)]
struct DiskCacheState {
    /// Shared across clones so a capacity change reaches the workers.
    max_bytes: AtomicU64,
    index: Mutex<DiskCacheIndex>,
}

#[derive(Debug, Default)]
struct DiskCacheIndex {
    initialized: bool,
    total_bytes: u64,
    by_path: HashMap<CachePath, CacheFileMetadata>,
    by_age: BTreeMap<(SystemTime, CachePath), u64>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct CachePath(Arc<PathBuf>);

impl CachePath {
    fn new(path: PathBuf) -> Self {
        Self(Arc::new(path))
    }

    fn as_path(&self) -> &Path {
        self.0.as_path()
    }
}

impl Borrow<Path> for CachePath {
    fn borrow(&self) -> &Path {
        self.as_path()
    }
}

#[derive(Clone, Copy, Debug)]
struct CacheFileMetadata {
    len: u64,
    modified: SystemTime,
}

#[derive(Clone, Debug)]
struct CacheFile {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

#[must_use]
pub fn disk_cache_path(cache_dir: impl AsRef<Path>, key: &ThumbnailKey) -> PathBuf {
    cache_dir.as_ref().join(key.disk_file_name())
}

pub fn read_disk_cache(
    cache: &WorkerDiskCache,
    key: &ThumbnailKey,
    max_edge: u32,
) -> Option<Thumbnail> {
    let path = disk_cache_path(&cache.dir, key);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            log::debug!(
                "failed to read thumbnail disk cache {}: {error}",
                path.display()
            );
            return None;
        }
    };

    match deserialize_cached_thumbnail(&bytes, max_edge) {
        Ok(thumbnail) => Some(thumbnail),
        Err(error) => {
            log::debug!(
                "ignoring invalid thumbnail disk cache {}: {error}",
                path.display()
            );
            if let Err(remove_error) = fs::remove_file(&path)
                && remove_error.kind() != std::io::ErrorKind::NotFound
            {
                log::debug!(
                    "failed to remove invalid thumbnail disk cache {}: {remove_error}",
                    path.display()
                );
            }
            remove_indexed_cache_file(cache, &path);
            None
        }
    }
}

/// Path for a URL's fetched bytes in the source tier.
pub fn source_cache_path(cache_dir: impl AsRef<Path>, url: &str) -> PathBuf {
    cache_dir
        .as_ref()
        .join(super::thumbnail_key::source_file_name(url))
}

/// Reads a URL's fetched bytes, if they are still on disk.
pub fn read_source_bytes(cache: &WorkerDiskCache, url: &str) -> Option<Vec<u8>> {
    let path = source_cache_path(&cache.dir, url);
    match fs::read(&path) {
        Ok(bytes) => {
            let _ = refresh_source_age(cache, &path);
            Some(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            log::debug!(
                "failed to read thumbnail source {}: {error}",
                path.display()
            );
            None
        }
    }
}

/// How stale an entry's recorded age must be before a read refreshes it.
///
/// Without a floor this is one `set_times` syscall per thumbnail load. An hour
/// is far below the interval eviction ordering actually turns on — sessions and
/// days — and far above the rate at which one thumbnail is re-read.
const SOURCE_AGE_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Marks a source as recently used, so eviction orders the tier by use rather
/// than by when it happened to be banked.
///
/// Sources are write-once and read-forever, so without this the whole tier is
/// FIFO: a library warm banks 20k sources in ID order, the budget forces
/// eviction, and the ones dropped are simply the ones banked first — not the
/// ones nobody opens. A user who browses the same 200 addons would re-fetch
/// them from the network every session while sources they have never viewed
/// survive.
///
/// The mtime is written, not just the in-memory index, because the index is
/// rebuilt from mtimes on every start. An in-memory-only bump would be
/// forgotten exactly when it matters — the next session's warm sweep.
fn refresh_source_age(cache: &WorkerDiskCache, path: &Path) -> Option<()> {
    let now = SystemTime::now();
    // Read out and drop the guard before the `set_times` syscall: holding the
    // index lock across I/O would stall every other worker's cache write.
    let existing = {
        let index = cache.state.index.lock();
        // Before the first build there is nothing to reorder, and the build
        // itself will read this file's real mtime anyway.
        let existing = *index.by_path.get(path)?;
        drop(index);
        existing
    };
    if now
        .duration_since(existing.modified)
        .is_ok_and(|age| age < SOURCE_AGE_REFRESH_INTERVAL)
    {
        return None;
    }
    let len = existing.len;

    let touched = fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_times(fs::FileTimes::new().set_modified(now)));
    if let Err(error) = touched {
        // Not worth failing a read over: the entry keeps its old age and is
        // simply evicted sooner than ideal.
        log::debug!(
            "failed to refresh thumbnail source age {}: {error}",
            path.display()
        );
        return None;
    }

    let mut index = cache.state.index.lock();
    // The lock was dropped across the syscall, so another worker's write may
    // have run eviction and unlinked this very path in the meantime. Inserting
    // blindly would resurrect an index entry for a file that no longer exists —
    // `contains_source` would then report it as banked and the warm filter
    // would never re-fetch it.
    if !index.by_path.contains_key(path) {
        return None;
    }
    insert_indexed_cache_file(
        &mut index,
        CacheFile {
            path: path.to_owned(),
            len,
            modified: now,
        },
    );
    drop(index);
    Some(())
}

/// Deletes a URL's banked source, keeping the index in step.
///
/// The index is authoritative for `contains_source` and for the eviction
/// budget, so a bare `remove_file` leaves a phantom: the warm filter keeps
/// reporting the URL as banked and never repairs it, and `total_bytes`
/// overcounts until eviction happens to trip over the missing file.
pub fn remove_source_bytes(cache: &WorkerDiskCache, url: &str) {
    let path = source_cache_path(&cache.dir, url);
    if let Err(error) = fs::remove_file(&path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::debug!(
            "failed to remove thumbnail source {}: {error}",
            path.display()
        );
    }
    remove_indexed_cache_file(cache, &path);
}

/// Stores a URL's fetched bytes.
///
/// Shares the derived tier's directory, index, and budget, so the two compete
/// under one ceiling rather than needing a second one to tune.
///
/// Note that they compete by **write age, not use**: `by_age` is keyed on
/// mtime and no read path touches it. For the derived tier that is close
/// enough, since entries are rewritten as they are re-derived. Sources are
/// write-once and read-forever, so a hot source banked during the first warm
/// ages toward the front of the eviction queue and can be dropped in favour of
/// a newer source nothing has ever looked at. Correct but not ideal; see the
/// plan's note on refreshing source age on read.
pub fn write_source_bytes(cache: &WorkerDiskCache, url: &str, bytes: &[u8]) {
    let path = source_cache_path(&cache.dir, url);
    match crate::util::fs::atomic_write(&path, bytes) {
        Ok(()) => maybe_evict_disk_cache(cache, path, bytes.len() as u64),
        Err(error) => {
            log::debug!(
                "failed to write thumbnail source {}: {error}",
                path.display()
            );
        }
    }
}

/// Persists a derived entry — **except for animations, which are not stored
/// here at all**.
///
/// An animated entry's payload is `LazyGifPreview::encoded_bytes`, which is the
/// `Arc<[u8]>` the preview was decoded from: byte-for-byte the stream already
/// sitting in the source tier. Writing it again stored the same GIF twice under
/// one disk budget, so an animated addon cost double and evicted twice as much
/// of everything else.
///
/// Almost nothing is lost by skipping it. A derived GIF entry is not a
/// *derivation* — unlike a resized still it is size-independent, so its read
/// path did exactly the work the source path does: `decode_lazy_gif_preview`
/// over the same bytes.
///
/// The one real cost is the ThumbHash, which the derived entry persisted and
/// which `finish_fresh_decode` now recomputes per cold load. Measured at
/// **327 µs** for a 256² first frame (`probe_thumbhash_encode_cost`) — an order
/// of magnitude more than it looks like it should be, so it is worth naming
/// rather than waving at. It is paid on a media-pool thread, never the main
/// one, next to a GIF decode already costing milliseconds. Halving the disk
/// footprint of every animated addon is worth 0.3 ms of background work.
///
/// The reader still understands `CACHE_FORMAT_GIF`, so caches written before
/// this change keep loading rather than being discarded on upgrade.
pub fn write_disk_cache(cache: &WorkerDiskCache, key: &ThumbnailKey, thumbnail: &Thumbnail) {
    if thumbnail.animation().is_some() {
        return;
    }
    match write_disk_cache_inner(cache, key, thumbnail) {
        Ok((path, written_bytes)) => maybe_evict_disk_cache(cache, path, written_bytes),
        Err(error) => {
            log::warn!(
                "failed to write thumbnail disk cache {}: {error}",
                disk_cache_path(&cache.dir, key).display()
            );
        }
    }
}

fn write_disk_cache_inner(
    cache: &WorkerDiskCache,
    key: &ThumbnailKey,
    thumbnail: &Thumbnail,
) -> std::io::Result<(PathBuf, u64)> {
    let path = disk_cache_path(&cache.dir, key);
    let bytes = serialize_cached_thumbnail(thumbnail).map_err(std::io::Error::other)?;
    let written_bytes = bytes.len() as u64;
    crate::util::fs::atomic_write(&path, &bytes)?;
    Ok((path, written_bytes))
}

fn maybe_evict_disk_cache(cache: &WorkerDiskCache, path: PathBuf, written_bytes: u64) {
    let mut index = cache.state.index.lock();
    if let Err(error) = ensure_disk_cache_index(cache, &mut index) {
        log::warn!(
            "failed to index thumbnail disk cache {}: {error}",
            cache.dir.display()
        );
        return;
    }

    let modified = fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or_else(|_| SystemTime::now());
    insert_indexed_cache_file(
        &mut index,
        CacheFile {
            path,
            len: written_bytes,
            modified,
        },
    );
    evict_indexed_disk_cache(cache, &mut index);
}

fn ensure_disk_cache_index(
    cache: &WorkerDiskCache,
    index: &mut DiskCacheIndex,
) -> std::io::Result<()> {
    if index.initialized {
        return Ok(());
    }

    let files = thumbnail_cache_files(&cache.dir)?;
    index.clear();
    for file in files {
        insert_indexed_cache_file(index, file);
    }
    index.initialized = true;
    Ok(())
}

fn is_source_file(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str())
        == Some(crate::media::thumbnail_worker::THUMBNAIL_SOURCE_FILE_EXTENSION)
}

fn insert_indexed_cache_file(index: &mut DiskCacheIndex, file: CacheFile) {
    let path = CachePath::new(file.path);
    if let Some(previous) = index.by_path.insert(
        path.clone(),
        CacheFileMetadata {
            len: file.len,
            modified: file.modified,
        },
    ) {
        index.by_age.remove(&(previous.modified, path.clone()));
        index.total_bytes = index.total_bytes.saturating_sub(previous.len);
    }
    index.by_age.insert((file.modified, path), file.len);
    index.total_bytes = index.total_bytes.saturating_add(file.len);
}

fn remove_indexed_cache_file(cache: &WorkerDiskCache, path: &Path) {
    let mut index = cache.state.index.lock();
    if !index.initialized {
        return;
    }
    if let Some((path, previous)) = index.by_path.remove_entry(path) {
        index.by_age.remove(&(previous.modified, path));
        index.total_bytes = index.total_bytes.saturating_sub(previous.len);
    }
}

/// How far into the age order to look for a derived entry before giving up and
/// evicting whatever is oldest.
///
/// Bounds the cost of the preference below: without a cap this would be a full
/// scan per eviction once only sources remain.
const EVICTION_PREFERENCE_SCAN: usize = 64;

/// Evicts oldest-first, but prefers derived entries over sources.
///
/// The two tiers share one budget, and a source is necessarily written just
/// *before* the derived entry decoded from it — so plain age order evicts every
/// source first, which is exactly backwards: losing a derived entry costs a
/// local decode, losing a source costs a network fetch. Keeping a source is
/// also usually the cheaper of the two in bytes — measured at 31x smaller for a
/// warm-banked 512px variant, but only 2.3x for a bare 1920x1080 original, so
/// "cheap" holds strongly for warm-banked sources and weakly for
/// interactively-banked ones.
fn evict_indexed_disk_cache(cache: &WorkerDiskCache, index: &mut DiskCacheIndex) {
    while index.total_bytes > cache.max_bytes() {
        let victim = index
            .by_age
            .iter()
            .take(EVICTION_PREFERENCE_SCAN)
            .find(|((_, path), _)| !is_source_file(path.as_path()))
            .or_else(|| index.by_age.iter().next())
            .map(|((modified, path), len)| ((*modified, path.clone()), *len));
        let Some(((modified, path), len)) = victim else {
            break;
        };
        index.by_age.remove(&(modified, path.clone()));
        index.by_path.remove(path.as_path());

        match fs::remove_file(path.as_path()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                log::warn!(
                    "failed to remove thumbnail cache file {}: {error}",
                    path.as_path().display()
                );
            }
        }
        index.total_bytes = index.total_bytes.saturating_sub(len);
    }
}

impl DiskCacheIndex {
    fn clear(&mut self) {
        self.total_bytes = 0;
        self.by_path.clear();
        self.by_age.clear();
    }
}

fn serialize_cached_thumbnail(thumbnail: &Thumbnail) -> image::ImageResult<Vec<u8>> {
    let metadata = thumbnail.metadata();
    let pixels = thumbnail.rgba_bytes();
    // Animated GIFs persist as their raw encoded stream so they replay after a
    // restart; only still thumbnails are worth PNG-compressing a frame.
    let compressed = if thumbnail.animation().is_none() && should_try_png_cache_payload(pixels) {
        let mut png = Vec::new();
        PngEncoder::new_with_quality(&mut png, PngCompressionType::Fast, PngFilterType::Adaptive)
            .write_image(
            pixels,
            metadata.width,
            metadata.height,
            ColorType::Rgba8.into(),
        )?;
        if png.len() < pixels.len() {
            Some(png)
        } else {
            None
        }
    } else {
        None
    };

    let (format, payload) = thumbnail.animation().map_or_else(
        || {
            compressed
                .as_deref()
                .map_or((CACHE_FORMAT_RAW_RGBA, pixels), |png| {
                    (CACHE_FORMAT_PNG_RGBA, png)
                })
        },
        |animation| (CACHE_FORMAT_GIF, animation.encoded_bytes()),
    );

    // ThumbHashes are tens of bytes; anything longer is a bug, so drop it
    // rather than widen the length field.
    let thumbhash = thumbnail
        .thumbhash()
        .filter(|hash| u8::try_from(hash.len()).is_ok());

    let mut encoded = Vec::with_capacity(42 + thumbhash.map_or(0, <[u8]>::len) + payload.len());
    encoded.extend_from_slice(CACHE_PAYLOAD_MAGIC);
    encoded.extend_from_slice(&CACHE_PAYLOAD_VERSION.to_le_bytes());
    encoded.extend_from_slice(&metadata.width.to_le_bytes());
    encoded.extend_from_slice(&metadata.height.to_le_bytes());
    encoded.extend_from_slice(&metadata.source_width.to_le_bytes());
    encoded.extend_from_slice(&metadata.source_height.to_le_bytes());
    encoded.extend_from_slice(&metadata.max_edge.to_le_bytes());
    encoded.push(thumbhash.map_or(0, <[u8]>::len) as u8);
    if let Some(thumbhash) = thumbhash {
        encoded.extend_from_slice(thumbhash);
    }
    encoded.push(format);
    encoded.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    encoded.extend_from_slice(payload);
    Ok(encoded)
}

fn should_try_png_cache_payload(pixels: &[u8]) -> bool {
    let mut sampled = 0_usize;
    let mut same_as_previous = 0_usize;
    let mut transparent = false;
    let mut previous = None::<[u8; 4]>;

    for pixel in pixels.chunks_exact(4).step_by(8) {
        let current = [pixel[0], pixel[1], pixel[2], pixel[3]];
        transparent |= current[3] != 255;
        if previous == Some(current) {
            same_as_previous += 1;
        }
        previous = Some(current);
        sampled += 1;
    }

    transparent || sampled == 0 || same_as_previous.saturating_mul(8) >= sampled
}

/// Why a cached thumbnail payload was rejected.
#[derive(Debug, Error)]
enum CacheDecodeError {
    #[error("missing magic")]
    MissingMagic,
    #[error("invalid magic")]
    InvalidMagic,
    #[error("unsupported version")]
    UnsupportedVersion,
    #[error("missing format")]
    MissingFormat,
    #[error("invalid dimensions")]
    InvalidDimensions,
    #[error("stale max edge")]
    StaleMaxEdge,
    #[error("byte length overflow")]
    ByteLengthOverflow,
    #[error("byte length too large")]
    ByteLengthTooLarge,
    #[error("cache length overflow")]
    CacheLengthOverflow,
    #[error("cache length mismatch")]
    CacheLengthMismatch,
    #[error("missing pixels")]
    MissingPixels,
    #[error("raw byte length mismatch")]
    RawByteLengthMismatch,
    #[error("failed to decode payload")]
    DecodePayloadFailed,
    #[error("decoded dimensions mismatch")]
    DecodedDimensionsMismatch,
    #[error("decoded byte length mismatch")]
    DecodedByteLengthMismatch,
    #[error("failed to decode cached GIF")]
    DecodeGifFailed,
    #[error("cached GIF is not animated")]
    GifNotAnimated,
    #[error("unsupported cache payload format")]
    UnsupportedFormat,
    #[error("invalid decoded RGBA payload")]
    InvalidRgbaPayload,
    #[error("cache offset overflow")]
    OffsetOverflow,
    #[error("truncated cache")]
    Truncated,
}

fn deserialize_cached_thumbnail(
    bytes: &[u8],
    requested_max_edge: u32,
) -> Result<Thumbnail, CacheDecodeError> {
    let mut offset = 0;
    let Some(magic) = bytes.get(..CACHE_PAYLOAD_MAGIC.len()) else {
        return Err(CacheDecodeError::MissingMagic);
    };
    if magic != CACHE_PAYLOAD_MAGIC {
        return Err(CacheDecodeError::InvalidMagic);
    }
    offset += CACHE_PAYLOAD_MAGIC.len();

    let version = read_u32_le(bytes, &mut offset)?;
    if version != CACHE_PAYLOAD_VERSION {
        return Err(CacheDecodeError::UnsupportedVersion);
    }

    let width = read_u32_le(bytes, &mut offset)?;
    let height = read_u32_le(bytes, &mut offset)?;
    let source_width = read_u32_le(bytes, &mut offset)?;
    let source_height = read_u32_le(bytes, &mut offset)?;
    let max_edge = read_u32_le(bytes, &mut offset)?;
    let thumbhash = {
        let len = usize::from(
            *take_bytes(bytes, &mut offset, 1)?
                .first()
                .ok_or(CacheDecodeError::MissingFormat)?,
        );
        if len > 0 {
            Some(Arc::<[u8]>::from(take_bytes(bytes, &mut offset, len)?))
        } else {
            None
        }
    };
    let format = *take_bytes(bytes, &mut offset, 1)?
        .first()
        .ok_or(CacheDecodeError::MissingFormat)?;
    let encoded_len = read_u64_le(bytes, &mut offset)?;

    if width == 0 || height == 0 || source_width == 0 || source_height == 0 {
        return Err(CacheDecodeError::InvalidDimensions);
    }
    if max_edge == 0 || max_edge != requested_max_edge || width.max(height) > max_edge {
        return Err(CacheDecodeError::StaleMaxEdge);
    }

    let expected_raw_len = crate::media::pixel::checked_rgba_len(width, height)
        .ok_or(CacheDecodeError::ByteLengthOverflow)?;
    let encoded_len_usize =
        usize::try_from(encoded_len).map_err(|_| CacheDecodeError::ByteLengthTooLarge)?;
    let expected_total = offset
        .checked_add(encoded_len_usize)
        .ok_or(CacheDecodeError::CacheLengthOverflow)?;
    if bytes.len() != expected_total {
        return Err(CacheDecodeError::CacheLengthMismatch);
    }

    let payload = bytes
        .get(offset..expected_total)
        .ok_or(CacheDecodeError::MissingPixels)?;
    let pixel_bytes = match format {
        CACHE_FORMAT_RAW_RGBA => {
            if payload.len() != expected_raw_len {
                return Err(CacheDecodeError::RawByteLengthMismatch);
            }
            payload.to_vec()
        }
        CACHE_FORMAT_PNG_RGBA => {
            let mut reader =
                ImageReader::with_format(std::io::Cursor::new(payload), ImageFormat::Png);
            reader.limits(thumbnail_decode_limits());
            let decoded = reader
                .decode()
                .map_err(|_| CacheDecodeError::DecodePayloadFailed)?;
            if decoded.width() != width || decoded.height() != height {
                return Err(CacheDecodeError::DecodedDimensionsMismatch);
            }
            let pixel_bytes = decoded.into_rgba8().into_raw();
            if pixel_bytes.len() != expected_raw_len {
                return Err(CacheDecodeError::DecodedByteLengthMismatch);
            }
            pixel_bytes
        }
        CACHE_FORMAT_GIF => {
            let preview = decode_lazy_gif_preview(Arc::<[u8]>::from(payload), max_edge)
                .map_err(|_| CacheDecodeError::DecodeGifFailed)?;
            if preview.frame_count() <= 1 {
                return Err(CacheDecodeError::GifNotAnimated);
            }
            let mut thumbnail = Thumbnail::from_gif_preview(preview, max_edge);
            thumbnail.set_thumbhash(thumbhash);
            return Ok(thumbnail);
        }
        _ => return Err(CacheDecodeError::UnsupportedFormat),
    };

    let mut thumbnail = Thumbnail::new(
        pixel_bytes,
        ThumbnailMetadata {
            width,
            height,
            source_width,
            source_height,
            max_edge,
        },
    )
    .map_err(|_| CacheDecodeError::InvalidRgbaPayload)?;
    thumbnail.set_thumbhash(thumbhash);
    Ok(thumbnail)
}

fn read_u32_le(bytes: &[u8], offset: &mut usize) -> Result<u32, CacheDecodeError> {
    let slice = take_bytes(bytes, offset, 4)?;
    let mut raw = [0_u8; 4];
    raw.copy_from_slice(slice);
    Ok(u32::from_le_bytes(raw))
}

fn read_u64_le(bytes: &[u8], offset: &mut usize) -> Result<u64, CacheDecodeError> {
    let slice = take_bytes(bytes, offset, 8)?;
    let mut raw = [0_u8; 8];
    raw.copy_from_slice(slice);
    Ok(u64::from_le_bytes(raw))
}

fn take_bytes<'a>(
    bytes: &'a [u8],
    offset: &mut usize,
    len: usize,
) -> Result<&'a [u8], CacheDecodeError> {
    let end = offset
        .checked_add(len)
        .ok_or(CacheDecodeError::OffsetOverflow)?;
    let slice = bytes.get(*offset..end).ok_or(CacheDecodeError::Truncated)?;
    *offset = end;
    Ok(slice)
}

#[cfg(test)]
fn evict_disk_cache(cache_dir: &Path, max_bytes: u64) -> std::io::Result<u64> {
    let mut files = thumbnail_cache_files(cache_dir)?;
    let mut total = files.iter().map(|file| file.len).sum::<u64>();
    if total <= max_bytes {
        return Ok(total);
    }

    files.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.path.cmp(&right.path))
    });

    for file in files {
        if total <= max_bytes {
            break;
        }

        match fs::remove_file(&file.path) {
            Ok(()) => {
                total = total.saturating_sub(file.len);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                total = total.saturating_sub(file.len);
            }
            Err(error) => {
                total = total.saturating_sub(file.len);
                log::warn!(
                    "failed to remove thumbnail cache file {}: {error}",
                    file.path.display()
                );
            }
        }
    }

    Ok(total)
}

fn thumbnail_cache_files(cache_dir: &Path) -> std::io::Result<Vec<CacheFile>> {
    let entries = match fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();

    for entry_result in entries {
        let entry = entry_result?;
        let path = entry.path();
        // Both tiers live here and share one budget, so both must be indexed
        // or the source files would be invisible to eviction and grow forever.
        let extension = path.extension().and_then(|value| value.to_str());
        if extension != Some(CACHE_FILE_EXTENSION)
            && extension != Some(crate::media::thumbnail_worker::THUMBNAIL_SOURCE_FILE_EXTENSION)
        {
            continue;
        }

        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }

        files.push(CacheFile {
            path,
            len: metadata.len(),
            modified: metadata.modified().unwrap_or(UNIX_EPOCH),
        });
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    /// Under pressure the cheap-to-rebuild tier must go first. A source costs a
    /// network fetch to replace; a derived entry costs a local decode.
    #[test]
    fn eviction_sacrifices_derived_entries_before_sources() {
        let root = crate::test_support::TestDir::new("disk-cache-eviction-order");
        // Room for roughly one derived entry, forcing eviction on the second.
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 4_096);
        let url = "https://example.invalid/preview.jpg";

        write_source_bytes(&cache, url, &vec![7_u8; 512]);
        for index in 0..8 {
            let key = ThumbnailKey::for_url(format!("{url}?v={index}"), 64);
            let thumbnail = Thumbnail::new(
                vec![3_u8; 32 * 32 * 4],
                ThumbnailMetadata {
                    width: 32,
                    height: 32,
                    source_width: 32,
                    source_height: 32,
                    max_edge: 64,
                },
            )
            .expect("fixture length matches dimensions");
            write_disk_cache(&cache, &key, &thumbnail);
        }

        assert!(
            read_source_bytes(&cache, url).is_some(),
            "the source outlived every derived entry written after it"
        );
    }

    use std::path::Path;

    use super::*;
    use crate::test_support::TestDir;

    #[test]
    fn thumbnails_disk_cache_file_names_are_stable_for_bytes_and_files() {
        let bytes_key = ThumbnailKey::for_bytes("avatar:76561198000000000", 96);
        let file_key = ThumbnailKey::for_file("/tmp/source.png", 128);

        assert_eq!(bytes_key.disk_file_name(), "29d36f33527fe33e-96.rgba");
        assert_eq!(file_key.disk_file_name(), "c9249628c039637d-128.rgba");
    }

    /// Reading a source must move it out of the firing line. Without this the
    /// source tier is FIFO on bank order, so the addons a user actually opens
    /// are evicted in favour of ones they have never looked at.
    #[test]
    fn reading_a_source_saves_it_from_eviction_ahead_of_an_unread_one() {
        let root = TestDir::new("disk-cache-source-lru");
        let read_often = "https://example.invalid/browsed.jpg";
        let never_read = "https://example.invalid/ignored.jpg";

        // Banked through a handle that is then dropped, and backdated — this is
        // a *previous session's* cache. The index has to come from the files'
        // own mtimes, which is the state the refresh has to work against.
        {
            let previous_session = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
            write_source_bytes(&previous_session, read_often, &[1_u8; 512]);
            write_source_bytes(&previous_session, never_read, &[2_u8; 512]);
        }
        // `browsed` is the *older* of the two, so FIFO on bank order would evict
        // exactly the one the user keeps opening.
        let long_ago = SystemTime::now() - std::time::Duration::from_secs(48 * 60 * 60);
        backdate(&source_cache_path(root.path(), read_often), long_ago);
        backdate(
            &source_cache_path(root.path(), never_read),
            long_ago + std::time::Duration::from_secs(60 * 60),
        );

        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        cache.prime_index();

        assert!(read_source_bytes(&cache, read_often).is_some());

        // Shrink the budget so exactly one of the two must go, then force an
        // eviction pass by writing something.
        cache.set_max_bytes(1_200);
        write_source_bytes(&cache, "https://example.invalid/newcomer.jpg", &[3_u8; 512]);

        assert!(
            read_source_bytes(&cache, read_often).is_some(),
            "the source that was actually used must survive"
        );
        assert!(
            read_source_bytes(&cache, never_read).is_none(),
            "the one nobody read must be the one evicted"
        );
    }

    fn backdate(path: &Path, when: SystemTime) {
        let file = fs::File::options()
            .write(true)
            .open(path)
            .expect("fixture file opens");
        file.set_times(fs::FileTimes::new().set_modified(when))
            .expect("fixture mtime is settable");
    }

    /// The preference scan is bounded, so once the window is entirely sources
    /// it gives up and evicts the oldest thing it found. That fallback is the
    /// normal state for a warmed large library — every derived entry already
    /// gone — and an unbounded or non-terminating preference there would mean
    /// the budget silently stops converging.
    #[test]
    fn eviction_still_converges_when_only_sources_remain() {
        let root = TestDir::new("disk-cache-eviction-all-sources");
        let budget = 4_096_u64;
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), budget);

        // Well past `EVICTION_PREFERENCE_SCAN`, so the window can never contain
        // a derived entry to prefer.
        for index in 0..(EVICTION_PREFERENCE_SCAN * 2) {
            write_source_bytes(
                &cache,
                &format!("https://example.invalid/source-{index}.jpg"),
                &vec![9_u8; 256],
            );
        }

        assert!(
            total_cache_bytes(root.path()) <= budget,
            "eviction must converge on the budget with no derived entries to sacrifice"
        );
    }

    /// Priming only moves *when* the index is built, never what it answers.
    /// A cache primed up front and one left to build lazily must agree, or the
    /// warm filter would drop work the scheduler still needs — or keep work it
    /// does not.
    #[test]
    fn priming_the_index_answers_the_same_as_building_it_lazily() {
        let root = TestDir::new("disk-cache-prime-index");
        let present = "https://example.invalid/primed-present.png";
        let absent = "https://example.invalid/primed-absent.png";

        // Written through a handle that is then dropped, so the priming cache
        // below has to discover the file from disk rather than from its own
        // in-memory bookkeeping.
        {
            let writer = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
            write_source_bytes(&writer, present, &[1, 2, 3, 4]);
        }

        let primed = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        primed.prime_index();
        // Twice, because priming must be idempotent — the app races a worker
        // that may have built it already.
        primed.prime_index();

        let lazy = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);

        assert!(primed.contains_source(present));
        assert_eq!(
            primed.contains_source(present),
            lazy.contains_source(present)
        );
        assert!(!primed.contains_source(absent));
        assert_eq!(primed.contains_source(absent), lazy.contains_source(absent));
    }

    #[test]
    fn contains_source_tracks_writes_without_stat_calls() {
        let root = TestDir::new("disk-cache-contains");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        let present = "https://example.invalid/present.png";
        let absent = "https://example.invalid/absent.png";

        assert!(!cache.contains_source(present));
        write_source_bytes(&cache, present, &[1, 2, 3, 4]);
        assert!(cache.contains_source(present));
        assert!(!cache.contains_source(absent));
    }

    /// A derived entry is not a source. Warming skips URLs whose *source* is
    /// banked; if a derived write satisfied that check, warming would never
    /// bank the source for anything the user had already scrolled past, and
    /// the next DPI change would re-fetch all of it.
    #[test]
    fn a_derived_entry_does_not_make_a_source_look_present() {
        let root = TestDir::new("disk-cache-derived-is-not-source");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        let url = "https://example.invalid/preview.png";
        let key = ThumbnailKey::for_url(url, 128);

        write_disk_cache(&cache, &key, &solid_thumbnail(4, 4, 7));

        assert!(!cache.contains_source(url));
    }

    #[test]
    fn capacity_changes_reach_clones() {
        let root = TestDir::new("disk-cache-capacity");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024);
        let clone = cache.clone();
        cache.set_max_bytes(4096);
        assert_eq!(clone.max_bytes(), 4096);
    }

    #[test]
    fn thumbnails_disk_cache_path_is_under_supplied_directory() {
        let key = ThumbnailKey::for_file("/tmp/source.png", 128);
        let path = disk_cache_path("/tmp/gmpublished-thumbs", &key);

        assert_eq!(path.parent(), Some(Path::new("/tmp/gmpublished-thumbs")));
        assert_eq!(
            path.file_name().and_then(|value| value.to_str()),
            Some("c9249628c039637d-128.rgba")
        );
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("rgba")
        );
    }

    #[test]
    fn thumbnails_cached_serialization_round_trips_rgba_payload() {
        let thumbnail = solid_thumbnail(11, 7, 42);
        let encoded = serialize_cached_thumbnail(&thumbnail).expect("cache payload should encode");
        let decoded = deserialize_cached_thumbnail(&encoded, thumbnail.metadata().max_edge)
            .expect("serialized thumbnail should decode");

        assert!(encoded.len() < 40 + thumbnail.byte_len());
        assert_eq!(decoded.metadata(), thumbnail.metadata());
        assert_eq!(decoded.rgba_bytes(), thumbnail.rgba_bytes());
    }

    #[test]
    fn thumbnails_cached_round_trips_stored_thumbhash() {
        let mut thumbnail = solid_thumbnail(11, 7, 42);
        assert!(thumbnail.thumbhash().is_none());
        thumbnail.set_thumbhash(Some(Arc::from(vec![1_u8, 2, 3, 4, 5].as_slice())));

        let encoded = serialize_cached_thumbnail(&thumbnail).expect("cache payload should encode");
        let decoded = deserialize_cached_thumbnail(&encoded, thumbnail.metadata().max_edge)
            .expect("serialized thumbnail should decode");

        assert_eq!(decoded.thumbhash(), Some([1, 2, 3, 4, 5].as_slice()));
        assert_eq!(decoded.rgba_bytes(), thumbnail.rgba_bytes());
    }

    /// The derived payload for an animation is the source stream verbatim, so
    /// storing it charged one disk budget twice for the same bytes.
    #[test]
    fn an_animation_is_not_written_to_the_derived_tier() {
        let root = TestDir::new("disk-cache-animation-not-derived");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        let gif = multi_frame_gif_bytes();
        let mut decoder = super::super::decode::ThumbnailDecoder::new();
        let thumbnail = decoder
            .decode_and_resize_bytes(&gif, 256)
            .expect("animated GIF should decode");
        assert!(thumbnail.animation().is_some(), "fixture must be animated");

        let key = ThumbnailKey::for_url("https://example.invalid/anim.gif", 256);
        write_disk_cache(&cache, &key, &thumbnail);

        assert!(
            read_disk_cache(&cache, &key, 256).is_none(),
            "an animation belongs in the source tier only"
        );
        assert!(
            !disk_cache_path(root.path(), &key).exists(),
            "and must not leave a file behind for the budget to account for"
        );
    }

    /// The counterpart: a still thumbnail *is* a derivation — resized to this
    /// key's edge — so it keeps its derived entry.
    #[test]
    fn a_still_thumbnail_is_still_written_to_the_derived_tier() {
        let root = TestDir::new("disk-cache-still-derived");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        // The fixture's `max_edge` is its longest side, and `read_disk_cache`
        // rejects an entry whose edge does not match what is being asked for.
        let key = ThumbnailKey::for_url("https://example.invalid/still.png", 4);

        write_disk_cache(&cache, &key, &solid_thumbnail(4, 4, 7));

        assert!(read_disk_cache(&cache, &key, 4).is_some());
    }

    /// Caches written before animations left the derived tier must keep
    /// loading, so the reader still understands `CACHE_FORMAT_GIF`. This is a
    /// format test, not a round-trip the app performs any more — `write_disk_cache`
    /// no longer produces this payload.
    #[test]
    fn thumbnails_cached_round_trips_animated_gif() {
        let gif = multi_frame_gif_bytes();
        let mut decoder = super::super::decode::ThumbnailDecoder::new();
        let thumbnail = decoder
            .decode_and_resize_bytes(&gif, 256)
            .expect("animated GIF should decode");
        let frames = thumbnail
            .animation()
            .expect("multi-frame GIF should be animated")
            .frame_count();
        assert!(frames > 1);

        let encoded = serialize_cached_thumbnail(&thumbnail).expect("cache payload should encode");
        let decoded =
            deserialize_cached_thumbnail(&encoded, 256).expect("cached GIF payload should decode");

        let replayed = decoded
            .animation()
            .expect("round-trip must preserve animation");
        assert_eq!(replayed.frame_count(), frames);
        assert_eq!(decoded.rgba_bytes(), thumbnail.rgba_bytes());
    }

    #[test]
    fn thumbnails_corrupt_disk_cache_entry_is_removed_and_treated_as_miss() {
        let root = TestDir::new("corrupt-disk-cache");
        let key = ThumbnailKey::for_bytes("avatar", 32);
        let path = disk_cache_path(root.path(), &key);
        std::fs::create_dir_all(root.path()).expect("cache dir should be created");
        std::fs::write(&path, b"not a thumbnail cache").expect("corrupt cache should be written");

        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024);

        assert!(read_disk_cache(&cache, &key, 32).is_none());
        assert!(!path.exists());
    }

    #[test]
    fn thumbnails_disk_cache_eviction_keeps_cache_files_under_byte_limit() {
        let root = TestDir::new("disk-cache-eviction");
        std::fs::create_dir_all(root.path()).expect("cache dir should be created");
        std::fs::write(root.path().join("keep.txt"), b"not cache")
            .expect("non-cache file should be written");

        for index in 0..3 {
            let key = ThumbnailKey::for_bytes(format!("item-{index}"), 16);
            let path = disk_cache_path(root.path(), &key);
            let thumbnail = solid_thumbnail(16, 16, index as u8);
            std::fs::write(
                path,
                serialize_cached_thumbnail(&thumbnail).expect("cache payload should encode"),
            )
            .expect("cache file should be written");
        }

        let max_bytes = thumbnail_cache_files(root.path())
            .expect("cache files")
            .first()
            .map_or(1, |file| file.len.saturating_add(1));

        evict_disk_cache(root.path(), max_bytes).expect("eviction should succeed");

        assert!(total_cache_bytes(root.path()) <= max_bytes);
        assert!(root.path().join("keep.txt").is_file());
    }

    fn multi_frame_gif_bytes() -> Vec<u8> {
        use gif::{DisposalMethod, Encoder, Frame, Repeat};

        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, 8, 8, &[]).unwrap();
            encoder.set_repeat(Repeat::Infinite).unwrap();
            for frame in 0..3_u8 {
                let color = [frame.wrapping_mul(60), 64, 128, 255];
                let pixels = vec![0; 8 * 8];
                let palette = vec![color[0], color[1], color[2]];
                let mut frame = Frame::from_palette_pixels(8, 8, pixels, palette, None);
                frame.delay = 6;
                frame.dispose = DisposalMethod::Background;
                encoder.write_frame(&frame).unwrap();
            }
        }
        bytes
    }

    fn solid_thumbnail(width: u32, height: u32, seed: u8) -> Thumbnail {
        let mut pixels = vec![0; (width * height * 4) as usize];
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.copy_from_slice(&[seed, seed.wrapping_add(1), seed.wrapping_add(2), 255]);
        }

        Thumbnail::new(
            pixels,
            ThumbnailMetadata {
                width,
                height,
                source_width: width,
                source_height: height,
                max_edge: width.max(height),
            },
        )
        .expect("solid thumbnail fixture should be valid")
    }

    fn total_cache_bytes(path: &Path) -> u64 {
        thumbnail_cache_files(path)
            .expect("cache directory should list")
            .into_iter()
            .map(|file| file.len)
            .sum()
    }
}
