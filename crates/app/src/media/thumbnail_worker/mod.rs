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
pub use thumbnail_key::{ThumbnailKey, normalize_url};
pub use types::{PreparedAnimation, PreparedAnimationFrame, PreparedThumbnail};
pub use types::{Thumbnail, ThumbnailError, ThumbnailInput, ThumbnailMetadata, ThumbnailResult};

static THUMBNAIL_AGENT: LazyLock<ureq::Agent> = LazyLock::new(decode::http_agent);

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
