//! Blocking thumbnail payload decode, fetch, resize, and disk-cache facade.

use std::sync::{
    Arc, LazyLock,
    atomic::{AtomicBool, Ordering},
};

mod decode;
mod disk_cache;
mod gif_preview;
mod thumbnail;
mod thumbnail_key;
mod types;

pub use decode::ThumbnailDecoder;
#[cfg(test)]
pub use disk_cache::disk_cache_path;
pub use disk_cache::{WorkerDiskCache, read_disk_cache, write_disk_cache};
pub use thumbnail_key::{ThumbnailKey, ThumbnailMode, normalize_url};
pub use types::{PreparedAnimation, PreparedAnimationFrame, PreparedThumbnail};
pub use types::{Thumbnail, ThumbnailError, ThumbnailInput, ThumbnailMetadata, ThumbnailResult};

static THUMBNAIL_AGENT: LazyLock<ureq::Agent> = LazyLock::new(decode::http_agent);
const THUMBNAIL_SOURCE_EDGES: [u32; 3] = [512, 384, 256];

#[derive(Clone, Debug, Default)]
pub struct ThumbnailCancellation {
    cancelled: Arc<AtomicBool>,
}

impl ThumbnailCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug)]
pub enum ThumbnailWorkerOutcome<T> {
    Completed(T),
    Cancelled,
}

/// How a thumbnail job sources bytes for a URL.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchProfile {
    /// Latency-first: the bare URL, exactly as the author uploaded it.
    /// The CDN computes resized renditions on FIRST request (~1s each,
    /// measured), and the local disk cache absorbs repeats — so every
    /// interactive fetch is a first fetch and a variant would put that
    /// rendition latency on screen.
    Interactive,
    /// Bandwidth-first background warming: request a tile-sized CDN
    /// rendition (10-100x fewer bytes), falling back to the bare URL for
    /// GIF re-encodes (byte-unpredictable, 0.6x-2.1x measured) and on any
    /// variant failure.
    BackgroundWarm,
}

pub fn run_thumbnail_request(
    disk_cache: Option<&WorkerDiskCache>,
    key: &ThumbnailKey,
    input: ThumbnailInput,
    max_edge: u32,
    profile: FetchProfile,
    cancellation: &ThumbnailCancellation,
) -> ThumbnailResult<ThumbnailWorkerOutcome<Thumbnail>> {
    // Cancellation may only fire before I/O is paid for: here (nothing
    // spent yet) and just before the network request. Bytes in hand always
    // decode and reach the caches — during a fast scroll the passed-by
    // fetches bank into the disk cache instead of being thrown away.
    if cancellation.is_cancelled() {
        return Ok(ThumbnailWorkerOutcome::Cancelled);
    }

    let mut decoder = ThumbnailDecoder::new();

    if let Some(cache) = disk_cache
        && let Some(thumbnail) = read_disk_cache(cache, key, max_edge)
    {
        return Ok(ThumbnailWorkerOutcome::Completed(thumbnail));
    }

    if key.mode() == ThumbnailMode::Static
        && let Some(cache) = disk_cache
    {
        for source_edge in THUMBNAIL_SOURCE_EDGES {
            let source_key = key.with_max_edge_and_mode(source_edge, ThumbnailMode::Animated);
            let Some(source) = read_disk_cache(cache, &source_key, source_edge) else {
                continue;
            };
            let thumbnail = decoder.resize_static_thumbnail(source, max_edge)?;
            write_disk_cache(cache, key, &thumbnail);
            return Ok(ThumbnailWorkerOutcome::Completed(thumbnail));
        }
    }

    let ThumbnailInput::Url { url } = input;
    let mut result = decoder.fetch_decode_and_resize_url_with_agent(
        &THUMBNAIL_AGENT,
        &url,
        max_edge,
        profile,
        cancellation,
    );

    // A fresh decode computes the ThumbHash once, so it is persisted with the
    // pixels and travels back to the placeholder cache. Disk-cache hits already
    // carry it.
    if let Ok(ThumbnailWorkerOutcome::Completed(thumbnail)) = &mut result {
        if key.mode() == ThumbnailMode::Static {
            thumbnail.make_static();
        }
        let metadata = thumbnail.metadata();
        let thumbhash = crate::media::thumbhash::encode(
            metadata.width,
            metadata.height,
            thumbnail.rgba_bytes(),
        );
        thumbnail.set_thumbhash(thumbhash.map(Arc::from));
    }

    // Still thumbnails persist a single frame; animated GIFs persist their raw
    // encoded stream so both survive restarts.
    if let (Some(cache), Ok(ThumbnailWorkerOutcome::Completed(thumbnail))) = (disk_cache, &result) {
        write_disk_cache(cache, key, thumbnail);
    }

    result
}

pub fn run_prepared_thumbnail_request(
    disk_cache: Option<&WorkerDiskCache>,
    key: &ThumbnailKey,
    input: ThumbnailInput,
    max_edge: u32,
    profile: FetchProfile,
    cancellation: &ThumbnailCancellation,
) -> ThumbnailResult<ThumbnailWorkerOutcome<PreparedThumbnail>> {
    Ok(
        match run_thumbnail_request(disk_cache, key, input, max_edge, profile, cancellation)? {
            ThumbnailWorkerOutcome::Completed(thumbnail) => {
                ThumbnailWorkerOutcome::Completed(PreparedThumbnail::from_thumbnail(thumbnail))
            }
            ThumbnailWorkerOutcome::Cancelled => ThumbnailWorkerOutcome::Cancelled,
        },
    )
}

pub fn validate_max_edge(max_edge: u32) -> ThumbnailResult<()> {
    if max_edge == 0 {
        return Err(ThumbnailError::InvalidMaxEdge);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_request_derives_and_persists_from_the_existing_animated_source() {
        let root = crate::test_support::TestDir::new("static-thumbnail-source-cache");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        let input = ThumbnailInput::from_url("not-a-network-url");
        let source_key = input.cache_key_with_mode(256, ThumbnailMode::Animated);
        let static_key = input.cache_key_with_mode(64, ThumbnailMode::Static);
        let source = Thumbnail::new(
            vec![80; 128 * 64 * 4],
            ThumbnailMetadata {
                width: 128,
                height: 64,
                source_width: 128,
                source_height: 64,
                max_edge: 256,
            },
        )
        .expect("source thumbnail is valid");
        write_disk_cache(&cache, &source_key, &source);

        let outcome = run_thumbnail_request(
            Some(&cache),
            &static_key,
            input,
            64,
            FetchProfile::Interactive,
            &ThumbnailCancellation::default(),
        )
        .expect("static request should use disk, not the invalid URL");
        let ThumbnailWorkerOutcome::Completed(thumbnail) = outcome else {
            panic!("disk-backed request should complete");
        };

        assert_eq!(
            (thumbnail.metadata().width, thumbnail.metadata().height),
            (64, 32)
        );
        assert!(thumbnail.animation().is_none());
        assert!(read_disk_cache(&cache, &static_key, 64).is_some());
    }
}
