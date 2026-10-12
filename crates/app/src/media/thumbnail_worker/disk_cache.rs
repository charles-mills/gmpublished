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
const CACHE_FORMAT_GIF: u8 = 2;

/// Clones share an index; separate processes do not. The byte limit is a
/// soft target, and `contains_source` can be stale until a read checks it.
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
                index: Mutex::new(None),
            }),
        }
    }

    fn max_bytes(&self) -> u64 {
        self.state.max_bytes.load(Ordering::Relaxed)
    }

    pub(crate) fn set_max_bytes(&self, max_bytes: u64) {
        self.state.max_bytes.store(max_bytes, Ordering::Relaxed);
    }

    /// Primes the lazy index off-thread before UI demand processing.
    /// The first access runs `read_dir` and a `stat` per file; later calls only lock.
    pub(crate) fn prime_index(&self) {
        let mut slot = self.state.index.lock();
        if let Err(error) = ensure_disk_cache_index(self, &mut slot) {
            log::debug!(
                "failed to pre-build thumbnail disk index {}: {error}",
                self.dir.display()
            );
        }
    }

    pub(crate) fn contains_source(&self, url: &str) -> bool {
        let path = source_cache_path(&self.dir, url);
        let mut slot = self.state.index.lock();
        let Ok(index) = ensure_disk_cache_index(self, &mut slot) else {
            return false;
        };
        index.by_path.contains_key(path.as_path())
    }
}

#[derive(Debug)]
struct DiskCacheState {
    max_bytes: AtomicU64,
    /// `None` until the first operation that needs it builds it from disk.
    index: Mutex<Option<DiskCacheIndex>>,
}

#[derive(Debug, Default)]
struct DiskCacheIndex {
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
    tier: CacheTier,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheTier {
    /// URL-keyed source bytes. Evict after derived entries, which can be rebuilt from them.
    Source,
    Derived,
}

impl CacheTier {
    fn from_path(path: &Path) -> Option<Self> {
        match path.extension().and_then(|value| value.to_str()) {
            Some(crate::media::thumbnail_worker::THUMBNAIL_SOURCE_FILE_EXTENSION) => {
                Some(Self::Source)
            }
            Some(CACHE_FILE_EXTENSION) => Some(Self::Derived),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct CacheFile {
    path: PathBuf,
    tier: CacheTier,
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
            None
        }
    }
}

pub fn source_cache_path(cache_dir: impl AsRef<Path>, url: &str) -> PathBuf {
    cache_dir
        .as_ref()
        .join(super::thumbnail_key::source_file_name(url))
}

pub fn read_source_bytes(cache: &WorkerDiskCache, url: &str) -> Option<Vec<u8>> {
    let path = source_cache_path(&cache.dir, url);
    match fs::read(&path) {
        Ok(bytes) => {
            let _ = refresh_source_age(cache, &path);
            Some(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Repair the index when another process has removed the source file.
            remove_indexed_cache_file(cache, &path);
            None
        }
        Err(error) => {
            // Unreadable files still count toward the byte budget until eviction removes them.
            log::debug!(
                "failed to read thumbnail source {}: {error}",
                path.display()
            );
            None
        }
    }
}

const SOURCE_AGE_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

fn refresh_source_age(cache: &WorkerDiskCache, path: &Path) -> Option<()> {
    let now = SystemTime::now();
    // Read out and drop the guard before the `set_times` syscall: holding the
    // index lock across I/O would stall every other worker's cache write.
    let existing = {
        let slot = cache.state.index.lock();
        let existing = *slot.as_ref()?.by_path.get(path)?;
        drop(slot);
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
        log::debug!(
            "failed to refresh thumbnail source age {}: {error}",
            path.display()
        );
        return None;
    }

    let mut slot = cache.state.index.lock();
    // Eviction may unlink this path while the lock is released; only refresh
    // an entry that still exists in the index.
    let index = slot.as_mut()?;
    if !index.by_path.contains_key(path) {
        return None;
    }
    insert_indexed_cache_file(
        index,
        CacheFile {
            tier: CacheTier::Source,
            path: path.to_owned(),
            len,
            modified: now,
        },
    );
    drop(slot);
    Some(())
}

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

pub fn write_source_bytes(cache: &WorkerDiskCache, url: &str, bytes: &[u8]) {
    let path = source_cache_path(&cache.dir, url);
    match crate::util::fs::atomic_write(&path, bytes) {
        Ok(()) => maybe_evict_disk_cache(cache, path, CacheTier::Source, bytes.len() as u64),
        Err(error) => {
            log::debug!(
                "failed to write thumbnail source {}: {error}",
                path.display()
            );
        }
    }
}

/// Persist stills only. GIF source bytes are already cached; retain reads of old GIF entries.
pub fn write_disk_cache(cache: &WorkerDiskCache, key: &ThumbnailKey, thumbnail: &Thumbnail) {
    if thumbnail.animation().is_some() {
        return;
    }
    match write_disk_cache_inner(cache, key, thumbnail) {
        Ok((path, written_bytes)) => {
            maybe_evict_disk_cache(cache, path, CacheTier::Derived, written_bytes);
        }
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

fn maybe_evict_disk_cache(
    cache: &WorkerDiskCache,
    path: PathBuf,
    tier: CacheTier,
    written_bytes: u64,
) {
    let mut slot = cache.state.index.lock();
    let index = match ensure_disk_cache_index(cache, &mut slot) {
        Ok(index) => index,
        Err(error) => {
            log::warn!(
                "failed to index thumbnail disk cache {}: {error}",
                cache.dir.display()
            );
            return;
        }
    };

    let modified = fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or_else(|_| SystemTime::now());
    insert_indexed_cache_file(
        index,
        CacheFile {
            tier,
            path,
            len: written_bytes,
            modified,
        },
    );
    evict_indexed_disk_cache(cache, index);
    drop(slot);
}

fn ensure_disk_cache_index<'a>(
    cache: &WorkerDiskCache,
    slot: &'a mut Option<DiskCacheIndex>,
) -> std::io::Result<&'a mut DiskCacheIndex> {
    if let Some(index) = slot {
        return Ok(index);
    }

    let files = thumbnail_cache_files(&cache.dir)?;
    let mut index = DiskCacheIndex::default();
    for file in files {
        insert_indexed_cache_file(&mut index, file);
    }
    Ok(slot.insert(index))
}

fn insert_indexed_cache_file(index: &mut DiskCacheIndex, file: CacheFile) {
    let path = CachePath::new(file.path);
    if let Some(previous) = index.by_path.insert(
        path.clone(),
        CacheFileMetadata {
            len: file.len,
            modified: file.modified,
            tier: file.tier,
        },
    ) {
        index.by_age.remove(&(previous.modified, path.clone()));
        index.total_bytes = index.total_bytes.saturating_sub(previous.len);
    }
    index.by_age.insert((file.modified, path), file.len);
    index.total_bytes = index.total_bytes.saturating_add(file.len);
}

fn remove_indexed_cache_file(cache: &WorkerDiskCache, path: &Path) {
    let mut slot = cache.state.index.lock();
    let Some(index) = slot.as_mut() else {
        return;
    };
    if let Some((path, previous)) = index.by_path.remove_entry(path) {
        index.by_age.remove(&(previous.modified, path));
        index.total_bytes = index.total_bytes.saturating_sub(previous.len);
    }
    drop(slot);
}

/// Bound the search so eviction cannot scan every entry when only sources remain.
const EVICTION_PREFERENCE_SCAN: usize = 64;

fn evict_indexed_disk_cache(cache: &WorkerDiskCache, index: &mut DiskCacheIndex) {
    while index.total_bytes > cache.max_bytes() {
        let victim = index
            .by_age
            .iter()
            .take(EVICTION_PREFERENCE_SCAN)
            .find(|((_, path), _)| {
                index
                    .by_path
                    .get(path.as_path())
                    .is_some_and(|metadata| metadata.tier == CacheTier::Derived)
            })
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

fn serialize_cached_thumbnail(thumbnail: &Thumbnail) -> image::ImageResult<Vec<u8>> {
    let metadata = thumbnail.metadata();
    let pixels = thumbnail.rgba_bytes();
    // Old GIF cache entries contain the raw stream; still entries contain PNG frames.
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

    // Drop unexpectedly long ThumbHashes rather than widening the length field.
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

#[derive(Debug, Error)]
#[error("corrupt thumbnail cache entry: {0}")]
struct CacheDecodeError(&'static str);

fn deserialize_cached_thumbnail(
    bytes: &[u8],
    requested_max_edge: u32,
) -> Result<Thumbnail, CacheDecodeError> {
    let mut offset = 0;
    let Some(magic) = bytes.get(..CACHE_PAYLOAD_MAGIC.len()) else {
        return Err(CacheDecodeError("missing magic"));
    };
    if magic != CACHE_PAYLOAD_MAGIC {
        return Err(CacheDecodeError("invalid magic"));
    }
    offset += CACHE_PAYLOAD_MAGIC.len();

    let version = read_u32_le(bytes, &mut offset)?;
    if version != CACHE_PAYLOAD_VERSION {
        return Err(CacheDecodeError("unsupported version"));
    }

    let width = read_u32_le(bytes, &mut offset)?;
    let height = read_u32_le(bytes, &mut offset)?;
    let source_width = read_u32_le(bytes, &mut offset)?;
    let source_height = read_u32_le(bytes, &mut offset)?;
    let max_edge = read_u32_le(bytes, &mut offset)?;
    let thumbhash = {
        let len = usize::from(take_byte(bytes, &mut offset)?);
        if len > 0 {
            Some(Arc::<[u8]>::from(take_bytes(bytes, &mut offset, len)?))
        } else {
            None
        }
    };
    let format = take_byte(bytes, &mut offset)?;
    let encoded_len = read_u64_le(bytes, &mut offset)?;

    if width == 0 || height == 0 || source_width == 0 || source_height == 0 {
        return Err(CacheDecodeError("invalid dimensions"));
    }
    if max_edge == 0 || max_edge != requested_max_edge || width.max(height) > max_edge {
        return Err(CacheDecodeError("stale max edge"));
    }

    let expected_raw_len = crate::media::pixel::checked_rgba_len(width, height)
        .ok_or(CacheDecodeError("byte length overflow"))?;
    let encoded_len_usize =
        usize::try_from(encoded_len).map_err(|_| CacheDecodeError("byte length too large"))?;
    let expected_total = offset
        .checked_add(encoded_len_usize)
        .ok_or(CacheDecodeError("cache length overflow"))?;
    if bytes.len() != expected_total {
        return Err(CacheDecodeError("cache length mismatch"));
    }

    let payload = bytes
        .get(offset..expected_total)
        .ok_or(CacheDecodeError("truncated cache"))?;
    let pixel_bytes = match format {
        CACHE_FORMAT_RAW_RGBA => {
            if payload.len() != expected_raw_len {
                return Err(CacheDecodeError("raw byte length mismatch"));
            }
            payload.to_vec()
        }
        CACHE_FORMAT_PNG_RGBA => {
            let mut reader =
                ImageReader::with_format(std::io::Cursor::new(payload), ImageFormat::Png);
            reader.limits(thumbnail_decode_limits());
            let decoded = reader
                .decode()
                .map_err(|_| CacheDecodeError("failed to decode payload"))?;
            if decoded.width() != width || decoded.height() != height {
                return Err(CacheDecodeError("decoded dimensions mismatch"));
            }
            let pixel_bytes = decoded.into_rgba8().into_raw();
            if pixel_bytes.len() != expected_raw_len {
                return Err(CacheDecodeError("decoded byte length mismatch"));
            }
            pixel_bytes
        }
        CACHE_FORMAT_GIF => {
            let preview = decode_lazy_gif_preview(Arc::<[u8]>::from(payload), max_edge)
                .map_err(|_| CacheDecodeError("failed to decode cached GIF"))?;
            if preview.frame_count() <= 1 {
                return Err(CacheDecodeError("cached GIF is not animated"));
            }
            let mut thumbnail = Thumbnail::from_gif_preview(preview, max_edge);
            thumbnail.set_thumbhash(thumbhash);
            return Ok(thumbnail);
        }
        _ => return Err(CacheDecodeError("unsupported cache payload format")),
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
    .map_err(|_| CacheDecodeError("invalid decoded RGBA payload"))?;
    thumbnail.set_thumbhash(thumbhash);
    Ok(thumbnail)
}

fn take_byte(bytes: &[u8], offset: &mut usize) -> Result<u8, CacheDecodeError> {
    let byte = bytes
        .get(*offset)
        .copied()
        .ok_or(CacheDecodeError("truncated cache"))?;
    *offset += 1;
    Ok(byte)
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
        .ok_or(CacheDecodeError("cache offset overflow"))?;
    let slice = bytes
        .get(*offset..end)
        .ok_or(CacheDecodeError("truncated cache"))?;
    *offset = end;
    Ok(slice)
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
        // Index both tiers so source files count toward eviction.
        let Some(tier) = CacheTier::from_path(&path) else {
            continue;
        };

        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }

        files.push(CacheFile {
            path,
            tier,
            len: metadata.len(),
            modified: metadata.modified().unwrap_or(UNIX_EPOCH),
        });
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    #[test]
    fn eviction_sacrifices_derived_entries_before_sources() {
        let root = crate::test_support::TestDir::new("disk-cache-eviction-order");
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

    #[test]
    fn reading_a_source_saves_it_from_eviction_ahead_of_an_unread_one() {
        let root = TestDir::new("disk-cache-source-lru");
        let read_often = "https://example.invalid/browsed.jpg";
        let never_read = "https://example.invalid/ignored.jpg";

        // Reopen a backdated cache from a previous session so eviction must use the files' mtimes.
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

    #[test]
    fn eviction_still_converges_when_only_sources_remain() {
        let root = TestDir::new("disk-cache-eviction-all-sources");
        let budget = 4_096_u64;
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), budget);

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

    #[test]
    fn priming_the_index_answers_the_same_as_building_it_lazily() {
        let root = TestDir::new("disk-cache-prime-index");
        let present = "https://example.invalid/primed-present.png";
        let absent = "https://example.invalid/primed-absent.png";

        {
            let writer = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
            write_source_bytes(&writer, present, &[1, 2, 3, 4]);
        }

        let primed = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        primed.prime_index();
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

    #[test]
    fn a_source_deleted_out_of_band_stops_reading_as_banked() {
        let root = TestDir::new("disk-cache-external-eviction");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        let url = "https://example.invalid/evicted-elsewhere.png";

        write_source_bytes(&cache, url, &[1, 2, 3, 4]);
        assert!(cache.contains_source(url));

        std::fs::remove_file(source_cache_path(root.path(), url))
            .expect("the banked source should exist on disk");

        assert!(read_source_bytes(&cache, url).is_none());
        assert!(
            !cache.contains_source(url),
            "the read should have repaired the index it found wrong"
        );
    }

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

    #[test]
    fn an_animation_is_not_written_to_the_derived_tier() {
        let root = TestDir::new("disk-cache-animation-not-derived");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        let gif = multi_frame_gif_bytes();
        let mut decoder = super::super::ThumbnailDecoder::new();
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

    #[test]
    fn a_still_thumbnail_is_still_written_to_the_derived_tier() {
        let root = TestDir::new("disk-cache-still-derived");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        let key = ThumbnailKey::for_url("https://example.invalid/still.png", 4);

        write_disk_cache(&cache, &key, &solid_thumbnail(4, 4, 7));

        assert!(read_disk_cache(&cache, &key, 4).is_some());
    }

    #[test]
    fn thumbnails_cached_round_trips_animated_gif() {
        let gif = multi_frame_gif_bytes();
        let mut decoder = super::super::ThumbnailDecoder::new();
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
    fn eviction_stays_under_budget_and_leaves_foreign_files_alone() {
        const MAX_BYTES: u64 = 4_096;

        let root = TestDir::new("disk-cache-eviction");
        std::fs::create_dir_all(root.path()).expect("cache dir should be created");
        std::fs::write(root.path().join("keep.txt"), b"not cache")
            .expect("non-cache file should be written");

        let cache = WorkerDiskCache::new(root.path().to_path_buf(), MAX_BYTES);
        for index in 0..8 {
            let key = ThumbnailKey::for_bytes(format!("item-{index}"), 16);
            write_disk_cache(&cache, &key, &solid_thumbnail(32, 32, index as u8));
        }

        assert!(
            total_cache_bytes(root.path()) <= MAX_BYTES,
            "eviction left the cache over its byte budget"
        );
        assert!(
            root.path().join("keep.txt").is_file(),
            "eviction deleted a file it does not own"
        );
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
