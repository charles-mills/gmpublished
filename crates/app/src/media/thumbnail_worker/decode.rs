#[cfg(test)]
use std::path::Path;
use std::time::Duration;

use super::{
    FetchOutcome, Thumbnail, ThumbnailCancellation, ThumbnailError, ThumbnailResult,
    ThumbnailWorkerOutcome, thumbnail::ThumbnailDecoder as AppThumbnailDecoder,
};
use crate::net::{HttpTimeouts, build_agent_with_max_idle_connections_per_host};

const HTTP_TIMEOUTS: HttpTimeouts = HttpTimeouts::new()
    .global_timeout(Duration::from_secs(20))
    .connect_timeout(Duration::from_secs(8))
    .receive_response_timeout(Duration::from_secs(12))
    .receive_body_timeout(Duration::from_secs(12));
// Matches the media pool width so keep-alive connections cover every worker
// during sustained scrolling and avoid TLS re-handshakes mid-scroll.
const HTTP_MAX_IDLE_CONNECTIONS_PER_HOST: usize = 16;

pub struct ThumbnailDecoder {
    inner: AppThumbnailDecoder,
}

impl ThumbnailDecoder {
    pub(crate) fn new() -> Self {
        Self {
            inner: AppThumbnailDecoder::new(),
        }
    }

    pub(crate) fn decode_and_resize_bytes(
        &mut self,
        bytes: &[u8],
        max_edge: u32,
    ) -> ThumbnailResult<Thumbnail> {
        self.inner
            .decode_and_resize_bytes(bytes, max_edge)
            .map_err(Into::into)
    }

    pub(crate) fn resize_static_thumbnail(
        &mut self,
        thumbnail: Thumbnail,
        max_edge: u32,
    ) -> ThumbnailResult<Thumbnail> {
        self.inner
            .resize_static_thumbnail(thumbnail, max_edge)
            .map_err(Into::into)
    }

    #[cfg(test)]
    pub(crate) fn decode_and_resize_file(
        &mut self,
        path: impl AsRef<Path>,
        max_edge: u32,
    ) -> ThumbnailResult<Thumbnail> {
        self.inner
            .decode_and_resize_file(path, max_edge)
            .map_err(Into::into)
    }

    pub(super) fn fetch_decode_and_resize_url_with_agent(
        &mut self,
        agent: &ureq::Agent,
        url: &str,
        max_edge: u32,
        cancellation: &ThumbnailCancellation,
        disk_cache: Option<&super::WorkerDiskCache>,
    ) -> ThumbnailResult<ThumbnailWorkerOutcome<Thumbnail>> {
        let banked = self.fetch_decode_and_resize_url_with_fetch(
            url,
            max_edge,
            cancellation,
            |fetch_url| fetch_url_bytes(agent, fetch_url),
        )?;

        // Bank only successfully decoded bytes. Banking corrupt fetches would
        // repopulate the source that the next read removes as poisoned.
        if let (Some(cache), Some((bytes, _))) = (disk_cache, banked.as_ref()) {
            super::write_source_bytes(cache, url, bytes);
        }
        Ok(match banked {
            Some((_, thumbnail)) => ThumbnailWorkerOutcome::Completed(thumbnail),
            None => ThumbnailWorkerOutcome::Cancelled,
        })
    }

    fn fetch_decode_and_resize_url_with_fetch(
        &mut self,
        url: &str,
        max_edge: u32,
        cancellation: &ThumbnailCancellation,
        mut fetch: impl FnMut(&str) -> ThumbnailResult<Vec<u8>>,
    ) -> ThumbnailResult<Option<(Vec<u8>, Thumbnail)>> {
        if cancellation.is_cancelled() {
            return Ok(None);
        }

        let bytes = fetch(url)?;

        let decoded = self.decode_and_resize_bytes(&bytes, max_edge);

        decoded.map(|thumbnail| Some((bytes, thumbnail)))
    }
}

pub(super) fn fetch_source_bytes_with_agent(
    agent: &ureq::Agent,
    url: &str,
    cancellation: &ThumbnailCancellation,
) -> ThumbnailResult<FetchOutcome<Vec<u8>>> {
    fetch_source_bytes_with_fetch(
        url,
        cancellation,
        |fetch_url| fetch_url_bytes_rejecting_gif(agent, fetch_url),
        |fetch_url| fetch_url_bytes(agent, fetch_url),
    )
}

/// Only the variant fetch may decline GIF bytes; the bare fetch must accept them.
fn fetch_source_bytes_with_fetch(
    url: &str,
    cancellation: &ThumbnailCancellation,
    mut fetch_variant: impl FnMut(&str) -> ThumbnailResult<Option<Vec<u8>>>,
    mut fetch_bare: impl FnMut(&str) -> ThumbnailResult<Vec<u8>>,
) -> ThumbnailResult<FetchOutcome<Vec<u8>>> {
    if cancellation.is_cancelled() {
        return Ok(FetchOutcome::Cancelled);
    }

    if let Some(variant_url) = steam_cdn_variant_url(url, SOURCE_VARIANT_EDGE)
        && let Ok(Some(bytes)) = fetch_variant(&variant_url)
    {
        return Ok(FetchOutcome::Fetched(bytes));
    }

    if cancellation.is_cancelled() {
        return Ok(FetchOutcome::Cancelled);
    }

    Ok(FetchOutcome::Fetched(fetch_bare(url)?))
}

/// Warm at the largest grid edge. Larger previews can derive from that cached source.
const SOURCE_VARIANT_EDGE: u32 = 512;

fn steam_cdn_variant_url(url: &str, max_edge: u32) -> Option<String> {
    let (_, authority_and_path) = url.split_once("://")?;
    let authority = authority_and_path.split(['/', '?', '#']).next()?;
    if !authority.eq_ignore_ascii_case("images.steamusercontent.com") {
        return None;
    }

    let (base, fragment) = url
        .split_once('#')
        .map_or((url, None), |(base, fragment)| (base, Some(fragment)));
    let separator = if base.contains('?') { '&' } else { '?' };
    let mut variant = format!(
        "{base}{separator}imw={max_edge}&imh={max_edge}&ima=fit&impolicy=Letterbox&letterbox=false"
    );
    if let Some(fragment) = fragment {
        variant.push('#');
        variant.push_str(fragment);
    }
    Some(variant)
}

pub(super) fn http_agent() -> ureq::Agent {
    build_agent_with_max_idle_connections_per_host(
        HTTP_TIMEOUTS,
        HTTP_MAX_IDLE_CONNECTIONS_PER_HOST,
    )
}

fn fetch_url_bytes(agent: &ureq::Agent, url: &str) -> ThumbnailResult<Vec<u8>> {
    let (url, mut response) = start_fetch(agent, url)?;
    read_body(url, &mut response)
}

/// Decline CDN GIF variants; they change byte size unpredictably without adding pixels.
fn fetch_url_bytes_rejecting_gif(
    agent: &ureq::Agent,
    url: &str,
) -> ThumbnailResult<Option<Vec<u8>>> {
    let (url, mut response) = start_fetch(agent, url)?;

    if response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .trim_start()
                .get(..9)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/gif"))
        })
    {
        return Ok(None);
    }

    read_body(url, &mut response).map(Some)
}

fn start_fetch(
    agent: &ureq::Agent,
    url: &str,
) -> ThumbnailResult<(String, ureq::http::Response<ureq::Body>)> {
    let url = super::thumbnail_key::normalize_url(url.to_owned());
    validate_http_url(&url)?;
    let response = agent
        .get(&url)
        .call()
        .map_err(|source| ThumbnailError::UrlFetch {
            url: url.clone(),
            source,
        })?;
    Ok((url, response))
}

fn read_body(
    url: String,
    response: &mut ureq::http::Response<ureq::Body>,
) -> ThumbnailResult<Vec<u8>> {
    response
        .body_mut()
        .read_to_vec()
        .map_err(|source| ThumbnailError::UrlRead { url, source })
}

fn validate_http_url(url: &str) -> ThumbnailResult<()> {
    let Some((scheme, rest)) = url.split_once(':') else {
        return Err(ThumbnailError::InvalidUrl {
            url: url.to_owned(),
        });
    };

    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(ThumbnailError::UnsupportedUrlScheme {
            url: url.to_owned(),
        });
    }

    if !rest.starts_with("//") || rest.len() <= 2 {
        return Err(ThumbnailError::InvalidUrl {
            url: url.to_owned(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::ThumbnailDecodeError;
    #[test]
    fn warm_banks_its_source_under_the_bare_url_not_the_variant_it_fetched() {
        let root = crate::test_support::TestDir::new("warm-source-keying");
        let cache = super::super::WorkerDiskCache::new(root.path().to_path_buf(), 4 * 1024 * 1024);
        let url = "https://images.steamusercontent.com/ugc/123/ABC/";

        let payload = solid_png_bytes();
        let fetched_urls = std::cell::RefCell::new(Vec::new());
        let outcome = fetch_source_bytes_with_fetch(
            url,
            &ThumbnailCancellation::default(),
            |fetch_url| {
                fetched_urls.borrow_mut().push(fetch_url.to_owned());
                Ok(Some(payload.clone()))
            },
            |fetch_url| {
                fetched_urls.borrow_mut().push(fetch_url.to_owned());
                Ok(payload.clone())
            },
        )
        .expect("warm fetch succeeds");

        let FetchOutcome::Fetched(bytes) = outcome else {
            panic!("expected banked bytes, got {outcome:?}");
        };
        super::super::write_source_bytes(&cache, url, &bytes);

        let fetched_urls = fetched_urls.into_inner();
        assert!(
            fetched_urls
                .first()
                .is_some_and(|fetched| fetched != url && fetched.contains("imw=512")),
            "warm should fetch a 512 variant, got {fetched_urls:?}"
        );
        assert!(
            super::super::read_source_bytes(&cache, url).is_some(),
            "the banked source must be readable by the bare URL the cache key carries"
        );
    }

    #[test]
    fn a_fetch_that_will_not_decode_yields_no_bytes_to_bank() {
        let mut decoder = ThumbnailDecoder::new();
        let banked = decoder.fetch_decode_and_resize_url_with_fetch(
            "https://example.invalid/corrupt.png",
            256,
            &ThumbnailCancellation::default(),
            |_| Ok(b"not an image".to_vec()),
        );

        assert!(
            banked.is_err(),
            "undecodable bytes must surface as an error, not as bankable bytes"
        );
    }

    #[test]
    fn warm_returns_raw_bytes_without_decoding_them() {
        let outcome = fetch_source_bytes_with_fetch(
            "https://images.steamusercontent.com/ugc/456/DEF/",
            &ThumbnailCancellation::default(),
            |_url| Ok(Some(b"not an image".to_vec())),
            |_url| Ok(b"not an image".to_vec()),
        )
        .expect("warm does not decode, so undecodable bytes are not an error");

        let FetchOutcome::Fetched(bytes) = outcome else {
            panic!("expected banked bytes, got {outcome:?}");
        };
        assert_eq!(bytes, b"not an image");
    }

    #[test]
    fn a_cancelled_warm_request_never_fetches() {
        let cancellation = ThumbnailCancellation::default();
        cancellation.cancel();
        let fetches = AtomicUsize::new(0);

        let result = fetch_source_bytes_with_fetch(
            "https://images.steamusercontent.com/ugc/preview.png",
            &cancellation,
            |_| {
                fetches.fetch_add(1, Ordering::Relaxed);
                Ok(Some(Vec::new()))
            },
            |_| {
                fetches.fetch_add(1, Ordering::Relaxed);
                Ok(Vec::new())
            },
        )
        .expect("cancelled request should not fail");

        assert!(matches!(result, FetchOutcome::Cancelled));
        assert_eq!(fetches.load(Ordering::Relaxed), 0);
    }

    fn solid_png_bytes() -> Vec<u8> {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([1, 2, 3, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("fixture encodes");
        png.into_inner()
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn thumbnails_invalid_urls_fail_before_network_fetch() {
        let agent = http_agent();
        let unsupported =
            fetch_url_bytes(&agent, "file:///tmp/preview.png").expect_err("file URLs are rejected");
        assert!(matches!(
            unsupported,
            ThumbnailError::UnsupportedUrlScheme { .. }
        ));

        let invalid =
            fetch_url_bytes(&agent, "https:/example.invalid").expect_err("malformed URL rejected");
        assert!(matches!(invalid, ThumbnailError::InvalidUrl { .. }));
    }

    #[test]
    fn thumbnails_decode_limits_are_app_owned() {
        let limits = super::super::thumbnail::thumbnail_decode_limits();

        assert!(limits.max_image_width.is_some());
        assert!(limits.max_image_height.is_some());
        assert!(limits.max_alloc.is_some());
    }

    #[test]
    fn thumbnails_zero_edge_uses_app_error_surface() {
        let mut decoder = ThumbnailDecoder::new();
        let error = decoder
            .decode_and_resize_bytes(b"not decoded", 0)
            .expect_err("zero max edge should be rejected before decode");

        assert!(matches!(
            error,
            ThumbnailError::Decode(ThumbnailDecodeError::InvalidMaxEdge)
        ));
        assert!(super::super::validate_max_edge(0).is_err());
    }

    #[test]
    fn steam_cdn_variant_url_adds_required_resize_parameters() {
        assert_eq!(
            steam_cdn_variant_url("https://images.steamusercontent.com/ugc/preview.png", 512),
            Some(String::from(
                "https://images.steamusercontent.com/ugc/preview.png?imw=512&imh=512&ima=fit&impolicy=Letterbox&letterbox=false"
            ))
        );
        assert_eq!(
            steam_cdn_variant_url("https://example.com/ugc/preview.png", 512),
            None
        );
        assert_eq!(
            steam_cdn_variant_url(
                "https://images.steamusercontent.com.evil.invalid/preview.png",
                512
            ),
            None
        );
    }

    #[test]
    fn the_warm_variant_is_large_enough_for_every_grid_size() {
        use crate::media::thumbnail_demand::physical_thumbnail_edge;
        use crate::widgets::grid_rows::ADDON_THUMBNAIL_MAX_EDGE;

        let ceiling = physical_thumbnail_edge(ADDON_THUMBNAIL_MAX_EDGE, f32::MAX);
        assert!(
            SOURCE_VARIANT_EDGE >= ceiling,
            "warm banks at {SOURCE_VARIANT_EDGE}px but grids can request {ceiling}px, \
             so every derived tile above the bank would be an upscale"
        );
    }

    #[test]
    fn steam_cdn_variant_failure_falls_back_to_bare_url() {
        let dir = crate::test_support::TestDir::new("gmpublished-cdn-variant-fallback");
        let image = std::fs::read(dir.image("fallback.png", image::ImageFormat::Png, 8, 6))
            .expect("PNG fixture");
        let bare_url = "https://images.steamusercontent.com/ugc/preview.png";
        let variant_url =
            steam_cdn_variant_url(bare_url, SOURCE_VARIANT_EDGE).expect("Steam variant URL");
        let requested = std::cell::RefCell::new(Vec::new());

        let result = fetch_source_bytes_with_fetch(
            bare_url,
            &ThumbnailCancellation::default(),
            |url| {
                requested.borrow_mut().push(url.to_owned());
                assert_eq!(url, variant_url);
                Err(ThumbnailError::UrlFetch {
                    url: url.to_owned(),
                    source: ureq::Error::StatusCode(503),
                })
            },
            |url| {
                requested.borrow_mut().push(url.to_owned());
                Ok(image.clone())
            },
        )
        .expect("bare URL fallback should fetch");

        assert!(matches!(result, FetchOutcome::Fetched(_)));
        assert_eq!(
            requested.into_inner(),
            vec![variant_url, bare_url.to_owned()]
        );
    }

    #[test]
    fn warm_variant_gif_rejection_falls_back_to_bare_url() {
        let dir = crate::test_support::TestDir::new("gmpublished-cdn-variant-gif");
        let image = std::fs::read(dir.image("anim.png", image::ImageFormat::Png, 8, 6))
            .expect("PNG fixture");
        let bare_url = "https://images.steamusercontent.com/ugc/preview.gif";
        let variant_url =
            steam_cdn_variant_url(bare_url, SOURCE_VARIANT_EDGE).expect("Steam variant URL");
        let requested = std::cell::RefCell::new(Vec::new());

        let result = fetch_source_bytes_with_fetch(
            bare_url,
            &ThumbnailCancellation::default(),
            |url| {
                requested.borrow_mut().push(url.to_owned());
                Ok(None)
            },
            |url| {
                requested.borrow_mut().push(url.to_owned());
                Ok(image.clone())
            },
        )
        .expect("bare URL fallback should fetch");

        assert!(matches!(result, FetchOutcome::Fetched(_)));
        assert_eq!(
            requested.into_inner(),
            vec![variant_url, bare_url.to_owned()]
        );
    }

    #[test]
    fn interactive_fetch_uses_only_the_bare_url() {
        let dir = crate::test_support::TestDir::new("gmpublished-interactive-bare");
        let image = std::fs::read(dir.image("bare.png", image::ImageFormat::Png, 8, 6))
            .expect("PNG fixture");
        let bare_url = "https://images.steamusercontent.com/ugc/preview.png";
        let requested = std::cell::RefCell::new(Vec::new());
        let mut decoder = ThumbnailDecoder::new();

        let result = decoder
            .fetch_decode_and_resize_url_with_fetch(
                bare_url,
                128,
                &ThumbnailCancellation::default(),
                |url| {
                    requested.borrow_mut().push(url.to_owned());
                    Ok(image.clone())
                },
            )
            .expect("bare URL should decode");

        assert!(result.is_some(), "a bare URL fetch should decode");
        assert_eq!(requested.into_inner(), vec![bare_url.to_owned()]);
    }

    #[test]
    fn non_allowlisted_fetch_uses_only_the_bare_url() {
        let dir = crate::test_support::TestDir::new("gmpublished-bare-thumbnail-fetch");
        let image = std::fs::read(dir.image("bare.png", image::ImageFormat::Png, 8, 6))
            .expect("PNG fixture");
        let bare_url = "https://example.com/ugc/preview.png";
        let requested = std::cell::RefCell::new(Vec::new());

        let result = fetch_source_bytes_with_fetch(
            bare_url,
            &ThumbnailCancellation::default(),
            |url| {
                requested.borrow_mut().push(url.to_owned());
                Ok(Some(image.clone()))
            },
            |url| {
                requested.borrow_mut().push(url.to_owned());
                Ok(image.clone())
            },
        )
        .expect("bare URL should fetch");

        assert!(matches!(result, FetchOutcome::Fetched(_)));
        assert_eq!(requested.into_inner(), vec![bare_url.to_owned()]);
    }

    #[test]
    fn cancelled_thumbnail_skips_fetch_and_decode_work() {
        let cancellation = ThumbnailCancellation::default();
        cancellation.cancel();
        let fetches = AtomicUsize::new(0);
        let mut decoder = ThumbnailDecoder::new();

        let result = decoder
            .fetch_decode_and_resize_url_with_fetch(
                "https://images.steamusercontent.com/ugc/preview.png",
                128,
                &cancellation,
                |_| {
                    fetches.fetch_add(1, Ordering::Relaxed);
                    Ok(Vec::new())
                },
            )
            .expect("cancelled request should not fail");

        assert!(result.is_none(), "a cancelled request decodes nothing");
        assert_eq!(fetches.load(Ordering::Relaxed), 0);
    }
}
