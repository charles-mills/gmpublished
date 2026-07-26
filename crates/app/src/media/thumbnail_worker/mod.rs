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
pub use disk_cache::{
    WorkerDiskCache, read_disk_cache, read_source_bytes, remove_source_bytes, write_disk_cache,
    write_source_bytes,
};
pub use thumbnail_key::{
    THUMBNAIL_SOURCE_FILE_EXTENSION, ThumbnailKey, ThumbnailMode, normalize_url,
};
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
    /// A background-warm job that put the source bytes on disk and stopped
    /// there. It carries no thumbnail because it decoded nothing: warming
    /// exists to make the *next* request local, and nothing paints for it.
    SourceBanked,
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
    ///
    /// Stops at bytes. It banks the source and returns
    /// [`ThumbnailWorkerOutcome::SourceBanked`] without decoding — see
    /// [`run_warm_source_request`].
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

    if profile == FetchProfile::BackgroundWarm {
        return run_warm_source_request(disk_cache, input, cancellation);
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

    // Source tier: the bytes exactly as fetched, keyed by URL alone. A derived
    // entry is size-specific, so a DPI change misses every one of them; with the
    // source on disk the new size is re-derived locally instead of re-fetching
    // the whole library.
    if let Some(cache) = disk_cache
        && let Some(bytes) = read_source_bytes(cache, &url)
    {
        match decoder.decode_and_resize_bytes(&bytes, max_edge) {
            Ok(thumbnail) => {
                let thumbnail = finish_fresh_decode(thumbnail, key);
                write_disk_cache(cache, key, &thumbnail);
                return Ok(ThumbnailWorkerOutcome::Completed(thumbnail));
            }
            Err(error) => {
                // A source that will not decode is worse than no source: it
                // would fail identically on every future request. Drop it and
                // fall through to a real fetch.
                //
                // Through `remove_source_bytes`, never a bare `remove_file`:
                // the index is what `contains_source` answers from, so an
                // unindexed delete would leave the warm filter believing this
                // URL is banked and never repairing it.
                log::debug!("cached thumbnail source for {url} failed to decode: {error}");
                remove_source_bytes(cache, &url);
            }
        }
    }

    let mut result = decoder.fetch_decode_and_resize_url_with_agent(
        &THUMBNAIL_AGENT,
        &url,
        max_edge,
        cancellation,
        disk_cache,
    );

    if let Ok(ThumbnailWorkerOutcome::Completed(thumbnail)) = result {
        result = Ok(ThumbnailWorkerOutcome::Completed(finish_fresh_decode(
            thumbnail, key,
        )));
    }

    // Still thumbnails persist their single resized frame. Animations do not
    // persist here at all — their payload would be the source stream verbatim,
    // which the fetch already banked; see `write_disk_cache`.
    if let (Some(cache), Ok(ThumbnailWorkerOutcome::Completed(thumbnail))) = (disk_cache, &result) {
        write_disk_cache(cache, key, thumbnail);
    }

    result
}

/// The whole of a background-warm job: get this URL's bytes into the source
/// tier, and stop.
///
/// Warm used to run the full interactive pipeline — fetch, decode, resize,
/// write a derived entry, hand back a `Thumbnail` — for a tile nothing was
/// going to paint. Two things were wrong with that. The decode and resize were
/// spent on a size chosen by the warm pass rather than by whatever eventually
/// asks (a DPI change, or a different card size, invalidates every one of
/// them), and the derived entry it wrote sat in the same disk budget as the
/// source it was derived from, evicting real sources to store a guess.
///
/// What it costs: the first interactive request for a warmed URL now pays a
/// local decode (measured at 2.7 ms for a 512x512 JPEG, 11.5 ms for
/// 1920x1080) instead of a disk read. What it buys: no network, which is the
/// term that actually dominates.
fn run_warm_source_request(
    disk_cache: Option<&WorkerDiskCache>,
    input: ThumbnailInput,
    cancellation: &ThumbnailCancellation,
) -> ThumbnailResult<ThumbnailWorkerOutcome<Thumbnail>> {
    let ThumbnailInput::Url { url } = input;

    // Without a disk cache there is nothing to warm into, so a fetch would be
    // pure network cost for bytes that go straight in the bin.
    let Some(cache) = disk_cache else {
        return Ok(ThumbnailWorkerOutcome::SourceBanked);
    };

    // The scheduler already skipped keys whose source is on disk; this catches
    // the ones banked between that check and now.
    if cache.contains_source(&url) {
        return Ok(ThumbnailWorkerOutcome::SourceBanked);
    }

    match decode::fetch_source_bytes_with_agent(&THUMBNAIL_AGENT, &url, cancellation)? {
        ThumbnailWorkerOutcome::Completed(bytes) => {
            write_source_bytes(cache, &url, &bytes);
            Ok(ThumbnailWorkerOutcome::SourceBanked)
        }
        ThumbnailWorkerOutcome::Cancelled => Ok(ThumbnailWorkerOutcome::Cancelled),
        ThumbnailWorkerOutcome::SourceBanked => Ok(ThumbnailWorkerOutcome::SourceBanked),
    }
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
            ThumbnailWorkerOutcome::SourceBanked => ThumbnailWorkerOutcome::SourceBanked,
            ThumbnailWorkerOutcome::Cancelled => ThumbnailWorkerOutcome::Cancelled,
        },
    )
}

/// Post-processing every freshly decoded thumbnail needs, regardless of whether
/// the bytes came off the network or out of the source tier.
///
/// A fresh decode computes the ThumbHash once so it is persisted with the pixels
/// and travels back to the placeholder cache; derived-tier hits already carry it.
fn finish_fresh_decode(mut thumbnail: Thumbnail, key: &ThumbnailKey) -> Thumbnail {
    if key.mode() == ThumbnailMode::Static {
        thumbnail.make_static();
    }
    let metadata = thumbnail.metadata();
    let thumbhash =
        crate::media::thumbhash::encode(metadata.width, metadata.height, thumbnail.rgba_bytes());
    thumbnail.set_thumbhash(thumbhash.map(Arc::from));
    thumbnail
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
    fn a_new_size_derives_from_the_stored_source_instead_of_refetching() {
        let root = crate::test_support::TestDir::new("thumbnail-source-tier");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 8 * 1024 * 1024);
        let url = "https://example.invalid/preview.png";

        // Encode a real image and store it as the fetched source.
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            64,
            64,
            image::Rgba([10, 20, 30, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("fixture encodes");
        write_source_bytes(&cache, url, &png.into_inner());

        let input = ThumbnailInput::from_url(url);
        let key = input.cache_key(32);

        // No network is reachable from a test, so completing at all proves the
        // bytes came from disk.
        let outcome = run_thumbnail_request(
            Some(&cache),
            &key,
            input,
            32,
            FetchProfile::Interactive,
            &ThumbnailCancellation::default(),
        )
        .expect("source-derived decode should succeed");

        let ThumbnailWorkerOutcome::Completed(thumbnail) = outcome else {
            panic!("expected a completed thumbnail, got {outcome:?}");
        };
        assert!(thumbnail.metadata().width <= 32 && thumbnail.metadata().height <= 32);
        // Derived output is persisted too, so the next request at this size
        // skips the decode entirely.
        assert!(read_disk_cache(&cache, &key, 32).is_some());
    }

    /// Warming's product is the source tier, so a URL already banked is
    /// finished work — no fetch, no decode, and above all no derived entry
    /// competing with real sources for the same disk budget.
    #[test]
    fn a_warm_request_for_a_banked_source_does_no_work_at_all() {
        let root = crate::test_support::TestDir::new("warm-source-present");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        let url = "https://example.invalid/warm-present.png";

        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            64,
            64,
            image::Rgba([10, 20, 30, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("fixture encodes");
        write_source_bytes(&cache, url, &png.into_inner());

        let input = ThumbnailInput::from_url(url);
        let key = input.cache_key(256);
        let outcome = run_thumbnail_request(
            Some(&cache),
            &key,
            input,
            256,
            FetchProfile::BackgroundWarm,
            &ThumbnailCancellation::default(),
        )
        .expect("a banked source needs no work");

        assert!(
            matches!(outcome, ThumbnailWorkerOutcome::SourceBanked),
            "warm hands back no thumbnail, got {outcome:?}"
        );
        assert!(
            read_disk_cache(&cache, &key, 256).is_none(),
            "warm must not write a derived entry; the size it would guess at is \
             not necessarily the size anything paints"
        );
    }

    /// The interactive path is unchanged by (A): it still derives and persists.
    /// Asserted next to the warm case so the asymmetry is deliberate rather
    /// than something a later edit can quietly flatten.
    #[test]
    fn an_interactive_request_still_writes_the_derived_entry_a_warm_one_skips() {
        let root = crate::test_support::TestDir::new("warm-vs-interactive-derive");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        let url = "https://example.invalid/interactive-derives.png";

        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            64,
            64,
            image::Rgba([10, 20, 30, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("fixture encodes");
        write_source_bytes(&cache, url, &png.into_inner());

        let input = ThumbnailInput::from_url(url);
        let key = input.cache_key(32);
        let outcome = run_thumbnail_request(
            Some(&cache),
            &key,
            input,
            32,
            FetchProfile::Interactive,
            &ThumbnailCancellation::default(),
        )
        .expect("source-derived decode should succeed");

        assert!(matches!(outcome, ThumbnailWorkerOutcome::Completed(_)));
        assert!(read_disk_cache(&cache, &key, 32).is_some());
    }

    /// Animations no longer get a derived entry, so the source tier is the only
    /// thing keeping them off the network. This is the test that says dropping
    /// the duplicate lost nothing: the animation still replays, still carries a
    /// ThumbHash, and still costs no fetch.
    #[test]
    fn an_animation_replays_from_the_source_tier_with_no_derived_entry() {
        let root = crate::test_support::TestDir::new("animation-source-only");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 8 * 1024 * 1024);
        let url = "https://example.invalid/animated.gif";
        write_source_bytes(&cache, url, &multi_frame_gif_bytes());

        let input = ThumbnailInput::from_url(url);
        let key = input.cache_key(256);

        // No network is reachable from a test, so completing at all proves the
        // bytes came from the source tier.
        for attempt in 1..=2 {
            let outcome = run_thumbnail_request(
                Some(&cache),
                &key,
                input.clone(),
                256,
                FetchProfile::Interactive,
                &ThumbnailCancellation::default(),
            )
            .unwrap_or_else(|error| panic!("attempt {attempt} should decode locally: {error}"));

            let ThumbnailWorkerOutcome::Completed(thumbnail) = outcome else {
                panic!("attempt {attempt}: expected a thumbnail, got {outcome:?}");
            };
            assert!(
                thumbnail
                    .animation()
                    .is_some_and(|animation| animation.frame_count() > 1),
                "attempt {attempt} must replay as an animation"
            );
            assert!(
                thumbnail.thumbhash().is_some(),
                "attempt {attempt} must still carry a ThumbHash, which the \
                 derived entry used to persist"
            );
        }

        assert!(
            read_disk_cache(&cache, &key, 256).is_none(),
            "and none of that wrote a second copy of the GIF"
        );
    }

    /// A source that cannot decode would fail identically forever, so it is
    /// discarded rather than retried on every request.
    #[test]
    fn an_undecodable_source_is_dropped_rather_than_poisoning_every_request() {
        let root = crate::test_support::TestDir::new("thumbnail-source-poison");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 1024 * 1024);
        let url = "https://example.invalid/broken.png";
        write_source_bytes(&cache, url, b"not an image");

        let input = ThumbnailInput::from_url(url);
        let key = input.cache_key(32);
        let _ = run_thumbnail_request(
            Some(&cache),
            &key,
            input,
            32,
            FetchProfile::Interactive,
            &ThumbnailCancellation::default(),
        );

        assert!(
            read_source_bytes(&cache, url).is_none(),
            "a source that will not decode must not survive to be retried"
        );
        // The index is what `contains_source` answers from. An unindexed delete
        // leaves a phantom: the whole-library warm filter keeps reporting this
        // URL as banked, so warm never re-fetches it and the card stays broken
        // for the rest of the session.
        assert!(
            !cache.contains_source(url),
            "deleting a poisoned source must unindex it, not just unlink it"
        );
    }

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

    /// Smallest thing that decodes as a multi-frame animation.
    fn multi_frame_gif_bytes() -> Vec<u8> {
        use gif::{DisposalMethod, Encoder, Frame, Repeat};

        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, 8, 8, &[]).expect("GIF fixture encodes");
            encoder.set_repeat(Repeat::Infinite).expect("repeat is set");
            for index in 0..3_u8 {
                let palette = vec![index.wrapping_mul(60), 64, 128];
                let mut frame = Frame::from_palette_pixels(8, 8, vec![0; 8 * 8], palette, None);
                frame.delay = 6;
                frame.dispose = DisposalMethod::Background;
                encoder.write_frame(&frame).expect("frame is written");
            }
        }
        bytes
    }
}
