#[cfg(test)]
use std::path::Path;
use std::time::Duration;

use super::{
    FetchProfile, Thumbnail, ThumbnailCancellation, ThumbnailError, ThumbnailResult,
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
        profile: FetchProfile,
        cancellation: &ThumbnailCancellation,
    ) -> ThumbnailResult<ThumbnailWorkerOutcome<Thumbnail>> {
        self.fetch_decode_and_resize_url_with_fetch(
            url,
            max_edge,
            profile,
            cancellation,
            |fetch_url, gif_policy| fetch_url_bytes(agent, fetch_url, gif_policy),
        )
    }

    fn fetch_decode_and_resize_url_with_fetch(
        &mut self,
        url: &str,
        max_edge: u32,
        profile: FetchProfile,
        cancellation: &ThumbnailCancellation,
        mut fetch: impl FnMut(&str, GifPolicy) -> ThumbnailResult<FetchedBody>,
    ) -> ThumbnailResult<ThumbnailWorkerOutcome<Thumbnail>> {
        if cancellation.is_cancelled() {
            return Ok(ThumbnailWorkerOutcome::Cancelled);
        }

        if profile == FetchProfile::BackgroundWarm
            && let Some(variant_url) = steam_cdn_variant_url(url, max_edge)
        {
            // A rejected GIF or any variant failure falls back to the bare
            // URL below.
            if let Ok(Some(outcome)) = fetch_decode_candidate(
                self,
                &variant_url,
                max_edge,
                cancellation,
                GifPolicy::Reject,
                &mut fetch,
            ) {
                return Ok(outcome);
            }
        }

        fetch_decode_candidate(
            self,
            url,
            max_edge,
            cancellation,
            GifPolicy::Allow,
            &mut fetch,
        )
        .map(|outcome| outcome.expect("GifPolicy::Allow never rejects a body"))
    }
}

/// Whether a fetch should refuse a GIF body. The CDN's GIF re-encodes are
/// byte-unpredictable (0.6x-2.1x the original, measured) and never carry
/// more pixels, so variant fetches reject them by Content-Type — without
/// reading the body — and retry bare.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GifPolicy {
    Allow,
    Reject,
}

#[derive(Debug)]
pub(super) enum FetchedBody {
    Bytes(Vec<u8>),
    RejectedGif,
}

fn fetch_decode_candidate(
    decoder: &mut ThumbnailDecoder,
    url: &str,
    max_edge: u32,
    cancellation: &ThumbnailCancellation,
    gif_policy: GifPolicy,
    fetch: &mut impl FnMut(&str, GifPolicy) -> ThumbnailResult<FetchedBody>,
) -> ThumbnailResult<Option<ThumbnailWorkerOutcome<Thumbnail>>> {
    if cancellation.is_cancelled() {
        return Ok(Some(ThumbnailWorkerOutcome::Cancelled));
    }
    let bytes = match fetch(url, gif_policy)? {
        FetchedBody::Bytes(bytes) => bytes,
        FetchedBody::RejectedGif => return Ok(None),
    };
    // The bytes are paid for — always decode so they reach the caches.
    decoder
        .decode_and_resize_bytes(&bytes, max_edge)
        .map(|thumbnail| Some(ThumbnailWorkerOutcome::Completed(thumbnail)))
}

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

fn fetch_url_bytes(
    agent: &ureq::Agent,
    url: &str,
    gif_policy: GifPolicy,
) -> ThumbnailResult<FetchedBody> {
    let url = super::thumbnail_key::normalize_url(url.to_owned());
    validate_http_url(&url)?;
    let mut response = agent
        .get(&url)
        .call()
        .map_err(|source| ThumbnailError::UrlFetch {
            url: url.clone(),
            source,
        })?;

    if gif_policy == GifPolicy::Reject
        && response
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
        return Ok(FetchedBody::RejectedGif);
    }

    response
        .body_mut()
        .read_to_vec()
        .map(FetchedBody::Bytes)
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn thumbnails_invalid_urls_fail_before_network_fetch() {
        let agent = http_agent();
        let unsupported = fetch_url_bytes(&agent, "file:///tmp/preview.png", GifPolicy::Allow)
            .expect_err("file URLs are rejected");
        assert!(matches!(
            unsupported,
            ThumbnailError::UnsupportedUrlScheme { .. }
        ));

        let invalid = fetch_url_bytes(&agent, "https:/example.invalid", GifPolicy::Allow)
            .expect_err("malformed URL rejected");
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

    #[test]
    fn steam_cdn_variant_failure_falls_back_to_bare_url() {
        let dir = crate::test_support::TestDir::new("gmpublished-cdn-variant-fallback");
        let image = std::fs::read(dir.image("fallback.png", image::ImageFormat::Png, 8, 6))
            .expect("PNG fixture");
        let bare_url = "https://images.steamusercontent.com/ugc/preview.png";
        let variant_url = steam_cdn_variant_url(bare_url, 128).expect("Steam variant URL");
        let requested = std::cell::RefCell::new(Vec::new());
        let mut decoder = ThumbnailDecoder::new();

        let result = decoder
            .fetch_decode_and_resize_url_with_fetch(
                bare_url,
                128,
                FetchProfile::BackgroundWarm,
                &ThumbnailCancellation::default(),
                |url, _| {
                    requested.borrow_mut().push(url.to_owned());
                    if url == variant_url {
                        Err(ThumbnailError::UrlFetch {
                            url: url.to_owned(),
                            source: ureq::Error::StatusCode(503),
                        })
                    } else {
                        Ok(FetchedBody::Bytes(image.clone()))
                    }
                },
            )
            .expect("bare URL fallback should decode");

        assert!(matches!(result, ThumbnailWorkerOutcome::Completed(_)));
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
        let variant_url = steam_cdn_variant_url(bare_url, 128).expect("Steam variant URL");
        let requested = std::cell::RefCell::new(Vec::new());
        let mut decoder = ThumbnailDecoder::new();

        let result = decoder
            .fetch_decode_and_resize_url_with_fetch(
                bare_url,
                128,
                FetchProfile::BackgroundWarm,
                &ThumbnailCancellation::default(),
                |url, gif_policy| {
                    requested.borrow_mut().push(url.to_owned());
                    if gif_policy == GifPolicy::Reject {
                        Ok(FetchedBody::RejectedGif)
                    } else {
                        Ok(FetchedBody::Bytes(image.clone()))
                    }
                },
            )
            .expect("bare URL fallback should decode");

        assert!(matches!(result, ThumbnailWorkerOutcome::Completed(_)));
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
                FetchProfile::Interactive,
                &ThumbnailCancellation::default(),
                |url, _| {
                    requested.borrow_mut().push(url.to_owned());
                    Ok(FetchedBody::Bytes(image.clone()))
                },
            )
            .expect("bare URL should decode");

        assert!(matches!(result, ThumbnailWorkerOutcome::Completed(_)));
        assert_eq!(requested.into_inner(), vec![bare_url.to_owned()]);
    }

    #[test]
    fn non_allowlisted_fetch_uses_only_the_bare_url() {
        let dir = crate::test_support::TestDir::new("gmpublished-bare-thumbnail-fetch");
        let image = std::fs::read(dir.image("bare.png", image::ImageFormat::Png, 8, 6))
            .expect("PNG fixture");
        let bare_url = "https://example.com/ugc/preview.png";
        let requested = std::cell::RefCell::new(Vec::new());
        let mut decoder = ThumbnailDecoder::new();

        let result = decoder
            .fetch_decode_and_resize_url_with_fetch(
                bare_url,
                128,
                FetchProfile::BackgroundWarm,
                &ThumbnailCancellation::default(),
                |url, _| {
                    requested.borrow_mut().push(url.to_owned());
                    Ok(FetchedBody::Bytes(image.clone()))
                },
            )
            .expect("bare URL should decode");

        assert!(matches!(result, ThumbnailWorkerOutcome::Completed(_)));
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
                FetchProfile::Interactive,
                &cancellation,
                |_, _| {
                    fetches.fetch_add(1, Ordering::Relaxed);
                    Ok(FetchedBody::Bytes(Vec::new()))
                },
            )
            .expect("cancelled request should not fail");

        assert!(matches!(result, ThumbnailWorkerOutcome::Cancelled));
        assert_eq!(fetches.load(Ordering::Relaxed), 0);
    }
}
