#[cfg(test)]
use std::path::Path;
use std::time::Duration;

use super::{
    FetchOutcome, Thumbnail, ThumbnailCancellation, ThumbnailError, ThumbnailResult,
    ThumbnailWorkerOutcome, thumbnail::ThumbnailDecoder as AppThumbnailDecoder,
};
use crate::net::build_agent_with_max_idle_connections_per_host;

const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const HTTP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(12);
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

    /// Fetches, decodes, and resizes a URL — the interactive path.
    ///
    /// Deliberately has no `FetchProfile`: it is only ever reached
    /// interactively. Background warming stops at bytes and takes
    /// [`fetch_source_bytes_with_agent`] instead, so a CDN-variant branch here
    /// would be dead code that reads as live — and two copies of the
    /// variant-then-bare fallback free to drift apart.
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

        // Interactive fetches bank their bytes too, so a URL the user scrolled
        // past is local for the next size that asks — the same product warming
        // produces, obtained for free.
        //
        // Banked *after* a successful decode, never from inside the fetch.
        // Banking on fetch would write undecodable bytes to the source tier;
        // the read path's poison-drop removes them on the next request, which
        // falls through to a fetch that banks them again. A permanently corrupt
        // URL would re-poison itself forever, paying a decode, a disk write and
        // index churn every time.
        if let (Some(cache), Some((bytes, _))) = (disk_cache, banked.as_ref()) {
            super::write_source_bytes(cache, url, bytes);
        }
        Ok(match banked {
            Some((_, thumbnail)) => ThumbnailWorkerOutcome::Completed(thumbnail),
            None => ThumbnailWorkerOutcome::Cancelled,
        })
    }

    /// Returns the fetched bytes alongside the thumbnail they decoded to, so
    /// the caller can bank a source it knows to be good.
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

        // The bytes are paid for — always decode so they reach the caches.
        let decoded = self.decode_and_resize_bytes(&bytes, max_edge);

        decoded.map(|thumbnail| Some((bytes, thumbnail)))
    }
}

/// Fetches a URL's bytes for the source tier, without decoding them.
///
/// This is the whole of a background-warm job. Warm's product is the source
/// tier: its bytes are what makes a later interactive request local. Decoding
/// and resizing here would produce a tile at whatever size warm happened to
/// ask for, which is not necessarily the size anything will paint — and would
/// spend the decode twice over when the interactive request arrives at a
/// different edge or a different DPI.
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

/// `fetch_variant` may decline a GIF body (`None`); `fetch_bare` takes whatever
/// the URL serves. Separate parameters because the two calls have different
/// contracts: one closure covering both would make a rejected body look
/// possible on the path that cannot receive one.
fn fetch_source_bytes_with_fetch(
    url: &str,
    cancellation: &ThumbnailCancellation,
    mut fetch_variant: impl FnMut(&str) -> ThumbnailResult<Option<Vec<u8>>>,
    mut fetch_bare: impl FnMut(&str) -> ThumbnailResult<Vec<u8>>,
) -> ThumbnailResult<FetchOutcome<Vec<u8>>> {
    if cancellation.is_cancelled() {
        return Ok(FetchOutcome::Cancelled);
    }

    // Same variant-then-bare fallback the decoding path uses, and for the same
    // reason: the CDN rendition is 10-100x fewer bytes, but a rejected GIF or
    // any variant failure has to fall back to the URL as authored. The variant
    // is requested at `SOURCE_VARIANT_EDGE` rather than at any one request's
    // size, since these bytes become the source every later size derives from.
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

/// Edge requested for the warm path's CDN variant.
///
/// Must be at least `physical_thumbnail_edge`'s ceiling, since the fetched
/// bytes become the source every later size derives from.
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
        HTTP_TIMEOUT,
        HTTP_CONNECT_TIMEOUT,
        HTTP_RESPONSE_TIMEOUT,
        HTTP_MAX_IDLE_CONNECTIONS_PER_HOST,
    )
}

fn fetch_url_bytes(agent: &ureq::Agent, url: &str) -> ThumbnailResult<Vec<u8>> {
    let (url, mut response) = start_fetch(agent, url)?;
    read_body(url, &mut response)
}

/// `None` when the response is a GIF. The CDN's GIF re-encodes are
/// byte-unpredictable (0.6x-2.1x the original, measured) and never carry more
/// pixels, so a variant fetch declines them by Content-Type — without reading
/// the body — and the caller retries bare.
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
    /// The warm path fetches a Steam CDN *variant* URL, but the source tier is
    /// read by the bare URL from the cache key. Banking under the fetch URL
    /// wrote a file nothing could find — budgeted and evicted, never read — so
    /// every source the background warm produced was dead weight.
    ///
    /// Asserted through the public read path rather than by file name, so it
    /// stays honest if the naming scheme changes.
    #[test]
    fn warm_banks_its_source_under_the_bare_url_not_the_variant_it_fetched() {
        let root = crate::test_support::TestDir::new("warm-source-keying");
        let cache = super::super::WorkerDiskCache::new(root.path().to_path_buf(), 4 * 1024 * 1024);
        // Only this host gets a variant rewrite, which is what made the bug
        // warm-and-Steam-specific.
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

    /// The read path drops a source that will not decode. If the fetch that
    /// follows banked its bytes regardless, the two fought forever: every
    /// request re-banked the corrupt bytes, and the next paid a local decode, a
    /// disk write, and index churn to drop them again.
    ///
    /// The poison-drop test in `mod.rs` cannot see this — its fetch fails
    /// (nothing is reachable from a test), so nothing re-banks there.
    #[test]
    fn a_fetch_that_will_not_decode_yields_no_bytes_to_bank() {
        let mut decoder = ThumbnailDecoder::new();
        let banked = decoder.fetch_decode_and_resize_url_with_fetch(
            "https://example.invalid/corrupt.png",
            256,
            &ThumbnailCancellation::default(),
            |_| Ok(b"not an image".to_vec()),
        );

        // Banking is the caller's job and is driven entirely by this result, so
        // a failed decode handing back nothing is exactly what keeps corrupt
        // bytes out of the source tier.
        assert!(
            banked.is_err(),
            "undecodable bytes must surface as an error, not as bankable bytes"
        );
    }

    /// Warming stops at bytes. It must not decode: the size it would pick is a
    /// guess, and the interactive request that eventually paints will decode at
    /// the size it actually needs.
    #[test]
    fn warm_returns_raw_bytes_without_decoding_them() {
        let outcome = fetch_source_bytes_with_fetch(
            "https://images.steamusercontent.com/ugc/456/DEF/",
            &ThumbnailCancellation::default(),
            // Bytes that are not a decodable image at all. A path that decoded
            // would fail here; a path that only banks does not care.
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

        assert!(matches!(error, ThumbnailError::InvalidMaxEdge));
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

    /// `SOURCE_VARIANT_EDGE`'s doc says it "must be at least
    /// `physical_thumbnail_edge`'s ceiling", and it is — but nothing tied the
    /// two together. Raising the display ceiling without raising this would
    /// silently make every warm-banked source too small to derive the largest
    /// size the app can ask for, and the only symptom would be soft thumbnails
    /// on hi-DPI displays.
    #[test]
    fn the_warm_variant_is_large_enough_for_every_size_the_app_can_request() {
        use crate::media::thumbnail_demand::physical_thumbnail_edge;

        // The ceiling is a clamp, so any absurd scale factor lands on it.
        let ceiling = physical_thumbnail_edge(4_096, f32::MAX);
        assert!(
            SOURCE_VARIANT_EDGE >= ceiling,
            "warm banks at {SOURCE_VARIANT_EDGE}px but the app can request {ceiling}px, \
             so every derived size above the bank would be an upscale"
        );
    }

    #[test]
    fn steam_cdn_variant_failure_falls_back_to_bare_url() {
        let dir = crate::test_support::TestDir::new("gmpublished-cdn-variant-fallback");
        let image = std::fs::read(dir.image("fallback.png", image::ImageFormat::Png, 8, 6))
            .expect("PNG fixture");
        let bare_url = "https://images.steamusercontent.com/ugc/preview.png";
        // Warm always requests the largest edge it may later need, independent
        // of this request's max_edge.
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
        // Warm always requests the largest edge it may later need, independent
        // of this request's max_edge.
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
