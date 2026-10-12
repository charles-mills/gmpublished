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
pub use disk_cache::{
    WorkerDiskCache, read_disk_cache, read_source_bytes, remove_source_bytes, write_disk_cache,
    write_source_bytes,
};
pub use thumbnail::ThumbnailDecodeError;
pub use thumbnail_key::{
    THUMBNAIL_SOURCE_FILE_EXTENSION, ThumbnailKey, ThumbnailMode, normalized_url,
};
pub use types::{PreparedAnimation, PreparedAnimationFrame, PreparedThumbnail, ThumbnailRequest};
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
    SourceBanked,
    Cancelled,
}

impl<T> ThumbnailWorkerOutcome<T> {
    fn map<U>(self, transform: impl FnOnce(T) -> U) -> ThumbnailWorkerOutcome<U> {
        match self {
            Self::Completed(value) => ThumbnailWorkerOutcome::Completed(transform(value)),
            Self::SourceBanked => ThumbnailWorkerOutcome::SourceBanked,
            Self::Cancelled => ThumbnailWorkerOutcome::Cancelled,
        }
    }
}

#[derive(Debug)]
pub enum FetchOutcome<T> {
    Fetched(T),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchProfile {
    Interactive,
    CacheOnly,
}

pub fn run_thumbnail_request(
    disk_cache: Option<&WorkerDiskCache>,
    request: &ThumbnailRequest,
    profile: FetchProfile,
    cancellation: &ThumbnailCancellation,
) -> ThumbnailResult<ThumbnailWorkerOutcome<Thumbnail>> {
    // Cancel only before I/O. Once fetched, decode and cache the bytes for later scrolls.
    if cancellation.is_cancelled() {
        return Ok(ThumbnailWorkerOutcome::Cancelled);
    }

    if profile == FetchProfile::CacheOnly {
        return run_cache_only_source_request(disk_cache, request.input().clone(), cancellation);
    }

    let key = request.key();
    let max_edge = request.physical_max_edge();
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

    let ThumbnailInput::Url { url } = request.input();

    // Source tier: the bytes exactly as fetched, keyed by URL alone. A derived
    // entry is size-specific, so a DPI change misses every one of them; with the
    // source on disk the new size is re-derived locally instead of re-fetching
    // the whole library.
    if let Some(cache) = disk_cache
        && let Some(bytes) = read_source_bytes(cache, url.as_ref())
    {
        match decoder.decode_and_resize_bytes(&bytes, max_edge) {
            Ok(thumbnail) => {
                let thumbnail = finish_fresh_decode(thumbnail, key);
                write_disk_cache(cache, key, &thumbnail);
                return Ok(ThumbnailWorkerOutcome::Completed(thumbnail));
            }
            Err(error) => {
                log::debug!("cached thumbnail source for {url} failed to decode: {error}");
                remove_source_bytes(cache, url.as_ref());
            }
        }
    }

    let mut result = decoder.fetch_decode_and_resize_url_with_agent(
        &THUMBNAIL_AGENT,
        url.as_ref(),
        max_edge,
        cancellation,
        disk_cache,
    );

    if let Ok(ThumbnailWorkerOutcome::Completed(thumbnail)) = result {
        result = Ok(ThumbnailWorkerOutcome::Completed(finish_fresh_decode(
            thumbnail, key,
        )));
    }

    if let (Some(cache), Ok(ThumbnailWorkerOutcome::Completed(thumbnail))) = (disk_cache, &result) {
        write_disk_cache(cache, key, thumbnail);
    }

    result
}

fn run_cache_only_source_request(
    disk_cache: Option<&WorkerDiskCache>,
    input: ThumbnailInput,
    cancellation: &ThumbnailCancellation,
) -> ThumbnailResult<ThumbnailWorkerOutcome<Thumbnail>> {
    let ThumbnailInput::Url { url } = input;

    let Some(cache) = disk_cache else {
        return Ok(ThumbnailWorkerOutcome::SourceBanked);
    };

    if cache.contains_source(&url) {
        return Ok(ThumbnailWorkerOutcome::SourceBanked);
    }

    match decode::fetch_source_bytes_with_agent(&THUMBNAIL_AGENT, &url, cancellation)? {
        FetchOutcome::Fetched(bytes) => {
            write_source_bytes(cache, &url, &bytes);
            Ok(ThumbnailWorkerOutcome::SourceBanked)
        }
        FetchOutcome::Cancelled => Ok(ThumbnailWorkerOutcome::Cancelled),
    }
}

pub fn run_prepared_thumbnail_request(
    disk_cache: Option<&WorkerDiskCache>,
    request: &ThumbnailRequest,
    profile: FetchProfile,
    cancellation: &ThumbnailCancellation,
) -> ThumbnailResult<ThumbnailWorkerOutcome<PreparedThumbnail>> {
    Ok(
        run_thumbnail_request(disk_cache, request, profile, cancellation)?
            .map(PreparedThumbnail::from_thumbnail),
    )
}

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
        return Err(ThumbnailDecodeError::InvalidMaxEdge.into());
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
            &ThumbnailRequest::new(input, 32, key.mode()),
            FetchProfile::Interactive,
            &ThumbnailCancellation::default(),
        )
        .expect("source-derived decode should succeed");

        let ThumbnailWorkerOutcome::Completed(thumbnail) = outcome else {
            panic!("expected a completed thumbnail, got {outcome:?}");
        };
        assert!(thumbnail.metadata().width <= 32 && thumbnail.metadata().height <= 32);
        assert!(read_disk_cache(&cache, &key, 32).is_some());
    }

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
            &ThumbnailRequest::new(input, 256, key.mode()),
            FetchProfile::CacheOnly,
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
            &ThumbnailRequest::new(input, 32, key.mode()),
            FetchProfile::Interactive,
            &ThumbnailCancellation::default(),
        )
        .expect("source-derived decode should succeed");

        assert!(matches!(outcome, ThumbnailWorkerOutcome::Completed(_)));
        assert!(read_disk_cache(&cache, &key, 32).is_some());
    }

    #[test]
    fn an_animation_replays_from_the_source_tier_with_no_derived_entry() {
        let root = crate::test_support::TestDir::new("animation-source-only");
        let cache = WorkerDiskCache::new(root.path().to_path_buf(), 8 * 1024 * 1024);
        let url = "https://example.invalid/animated.gif";
        write_source_bytes(&cache, url, &multi_frame_gif_bytes());

        let input = ThumbnailInput::from_url(url);
        let key = input.cache_key(256);

        for attempt in 1..=2 {
            let outcome = run_thumbnail_request(
                Some(&cache),
                &ThumbnailRequest::new(input.clone(), 256, key.mode()),
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
            &ThumbnailRequest::new(input, 32, key.mode()),
            FetchProfile::Interactive,
            &ThumbnailCancellation::default(),
        );

        assert!(
            read_source_bytes(&cache, url).is_none(),
            "a source that will not decode must not survive to be retried"
        );
        // Delete through the cache API or the index keeps reporting the poisoned URL as banked.
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
            &ThumbnailRequest::new(input, 64, static_key.mode()),
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
