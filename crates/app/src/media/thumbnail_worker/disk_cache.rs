use image::{
    ColorType, ImageEncoder, ImageFormat, ImageReader,
    codecs::png::{CompressionType as PngCompressionType, FilterType as PngFilterType, PngEncoder},
};
use std::{
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

    /// Whether an entry for `key` is on disk, by index lookup — no stat.
    pub(crate) fn contains(&self, key: &ThumbnailKey) -> bool {
        let path = disk_cache_path(&self.dir, key);
        let mut index = self.state.index.lock();
        if ensure_disk_cache_index(self, &mut index).is_err() {
            return false;
        }
        index.by_path.contains_key(&path)
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
    by_path: HashMap<PathBuf, CacheFileMetadata>,
    by_age: BTreeMap<(SystemTime, PathBuf), u64>,
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

pub fn write_disk_cache(cache: &WorkerDiskCache, key: &ThumbnailKey, thumbnail: &Thumbnail) {
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

fn insert_indexed_cache_file(index: &mut DiskCacheIndex, file: CacheFile) {
    if let Some(previous) = index.by_path.insert(
        file.path.clone(),
        CacheFileMetadata {
            len: file.len,
            modified: file.modified,
        },
    ) {
        index.by_age.remove(&(previous.modified, file.path.clone()));
        index.total_bytes = index.total_bytes.saturating_sub(previous.len);
    }
    index.by_age.insert((file.modified, file.path), file.len);
    index.total_bytes = index.total_bytes.saturating_add(file.len);
}

fn remove_indexed_cache_file(cache: &WorkerDiskCache, path: &Path) {
    let mut index = cache.state.index.lock();
    if !index.initialized {
        return;
    }
    if let Some(previous) = index.by_path.remove(path) {
        index.by_age.remove(&(previous.modified, path.to_owned()));
        index.total_bytes = index.total_bytes.saturating_sub(previous.len);
    }
}

fn evict_indexed_disk_cache(cache: &WorkerDiskCache, index: &mut DiskCacheIndex) {
    while index.total_bytes > cache.max_bytes() {
        let Some(((modified, path), len)) = index
            .by_age
            .iter()
            .next()
            .map(|((modified, path), len)| ((*modified, path.clone()), *len))
        else {
            break;
        };
        index.by_age.remove(&(modified, path.clone()));
        index.by_path.remove(&path);

        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                log::warn!(
                    "failed to remove thumbnail cache file {}: {error}",
                    path.display()
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
        if path.extension().and_then(|value| value.to_str()) != Some(CACHE_FILE_EXTENSION) {
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
    fn contains_tracks_writes_without_stat_calls() {
        let root = TestDir::new("disk-cache-contains");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        let present = ThumbnailKey::for_file("/tmp/present.png", 128);
        let absent = ThumbnailKey::for_file("/tmp/absent.png", 128);

        assert!(!cache.contains(&present));
        write_disk_cache(&cache, &present, &solid_thumbnail(4, 4, 7));
        assert!(cache.contains(&present));
        assert!(!cache.contains(&absent));
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
