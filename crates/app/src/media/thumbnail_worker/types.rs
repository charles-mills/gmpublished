use std::{sync::Arc, time::Duration};

use thiserror::Error;

use super::{
    gif_preview::{
        BakedAnimation, BakedAnimationFrameClip, GifPreviewError, LazyGifPreview,
        bake_gif_animation, bake_lazy_gif_preview,
    },
    thumbnail::ThumbnailDecodeError,
    thumbnail_key::{ThumbnailKey, ThumbnailMode, normalize_url},
};

pub use super::thumbnail::{Thumbnail, ThumbnailMetadata};

pub type ThumbnailResult<T> = Result<T, ThumbnailError>;

/// Input for a thumbnail request.
#[derive(Clone, Debug)]
pub enum ThumbnailInput {
    /// Fetch and decode an image from an HTTP(S) URL.
    Url { url: Arc<str> },
}

impl ThumbnailInput {
    #[must_use]
    pub fn from_url(url: impl Into<String>) -> Self {
        Self::Url {
            url: Arc::from(normalize_url(url.into())),
        }
    }

    #[must_use]
    #[cfg(test)]
    pub fn cache_key(&self, max_edge: u32) -> ThumbnailKey {
        match self {
            Self::Url { url } => {
                ThumbnailKey::for_normalized_url(url.clone(), max_edge, ThumbnailMode::Animated)
            }
        }
    }

    #[must_use]
    pub fn cache_key_with_mode(&self, max_edge: u32, mode: ThumbnailMode) -> ThumbnailKey {
        match self {
            Self::Url { url } => ThumbnailKey::for_normalized_url(url.clone(), max_edge, mode),
        }
    }

    #[must_use]
    pub fn source_url(&self) -> &str {
        match self {
            Self::Url { url } => url,
        }
    }
}

/// Thumbnail payload prepared for Iced presentation.
///
/// Static pixels remain in [`Thumbnail`]. Animated GIFs additionally carry baked
/// per-frame RGBA payloads and delays. The expensive GIF bake happens on the
/// blocking worker; Iced handles are created once at the UI cache boundary.
#[derive(Clone, Debug)]
pub struct PreparedThumbnail {
    thumbnail: Thumbnail,
    animation: Option<PreparedAnimation>,
}

impl PreparedThumbnail {
    /// Prepares a thumbnail for presentation, baking any animation frames.
    ///
    /// Animation is presentation garnish: when the bake fails, the already
    /// decoded first frame must still ship, so bake failures degrade to a
    /// still thumbnail instead of failing the whole request.
    pub(crate) fn from_thumbnail(thumbnail: Thumbnail) -> Self {
        let animation = thumbnail
            .animation()
            .filter(|preview| preview.frame_count() > 1)
            .and_then(prepare_animation_fail_open);

        Self {
            thumbnail,
            animation,
        }
    }

    pub(crate) fn thumbnail(&self) -> &Thumbnail {
        &self.thumbnail
    }

    pub(crate) fn animation(&self) -> Option<&PreparedAnimation> {
        self.animation.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn byte_len(&self) -> usize {
        self.thumbnail.byte_len()
            + self
                .animation
                .as_ref()
                .map_or(0, PreparedAnimation::byte_len)
    }
}

/// Baked animated thumbnail frames ready to become stable Iced handles.
#[derive(Clone, Debug)]
pub struct PreparedAnimation {
    frames: Vec<PreparedAnimationFrame>,
}

impl PreparedAnimation {
    pub(crate) fn from_encoded_gif(bytes: &[u8], max_edge: u32) -> ThumbnailResult<Self> {
        Self::from_baked(
            &bake_gif_animation(bytes, max_edge).map_err(ThumbnailDecodeError::GifDecode)?,
        )
    }

    fn from_lazy_preview(preview: &LazyGifPreview) -> ThumbnailResult<Self> {
        Self::from_baked(&bake_lazy_gif_preview(preview).map_err(ThumbnailDecodeError::GifDecode)?)
    }

    fn from_baked(animation: &BakedAnimation) -> ThumbnailResult<Self> {
        let cumulative = animation.cumulative_frame_times();
        let mut frames = Vec::with_capacity(animation.frame_count());
        for frame_index in 0..animation.frame_count() {
            let clip = animation.frame_clip(frame_index).ok_or_else(|| {
                ThumbnailDecodeError::GifDecode(GifPreviewError::FrameIndexOutOfRange {
                    frame_index,
                    frame_count: animation.frame_count(),
                })
            })?;
            frames.push(PreparedAnimationFrame {
                width: clip.width,
                height: clip.height,
                rgba: frame_rgba(animation, clip)?.into(),
                delay: frame_delay(cumulative, frame_index),
            });
        }
        Ok(Self { frames })
    }

    pub(crate) fn frames(&self) -> &[PreparedAnimationFrame] {
        &self.frames
    }

    #[cfg(test)]
    pub(crate) fn frame_count(&self) -> usize {
        self.frames.len()
    }

    #[cfg(test)]
    fn byte_len(&self) -> usize {
        self.frames
            .iter()
            .map(PreparedAnimationFrame::byte_len)
            .sum()
    }
}

#[derive(Clone, Debug)]
pub struct PreparedAnimationFrame {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
    delay: Duration,
}

impl PreparedAnimationFrame {
    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    pub(crate) fn rgba_bytes(&self) -> &[u8] {
        &self.rgba
    }

    /// Returns a cheap `Arc` clone of the frame's RGBA pixel bytes, for
    /// callers that hand ownership to a zero-copy wrapper instead of
    /// borrowing.
    pub(crate) fn rgba_arc(&self) -> Arc<[u8]> {
        Arc::clone(&self.rgba)
    }

    pub(crate) const fn delay(&self) -> Duration {
        self.delay
    }

    #[cfg(test)]
    fn byte_len(&self) -> usize {
        self.rgba.len()
    }
}

fn prepare_animation_fail_open(preview: &LazyGifPreview) -> Option<PreparedAnimation> {
    match PreparedAnimation::from_lazy_preview(preview) {
        Ok(animation) => Some(animation),
        Err(error) => {
            let frame = preview.first_frame();
            log::warn!(
                "animated thumbnail bake failed; keeping the static frame ({}x{}, {} frames, max edge {}): {error:?}",
                frame.width(),
                frame.height(),
                preview.frame_count(),
                preview.max_edge(),
            );
            None
        }
    }
}

fn frame_delay(cumulative: &[Duration], frame_index: usize) -> Duration {
    let current = cumulative.get(frame_index).copied().unwrap_or_default();
    if frame_index == 0 {
        current
    } else {
        current.saturating_sub(cumulative.get(frame_index - 1).copied().unwrap_or_default())
    }
}

fn frame_rgba(
    animation: &BakedAnimation,
    clip: BakedAnimationFrameClip,
) -> ThumbnailResult<Vec<u8>> {
    let row_len = crate::media::pixel::checked_rgba_len(clip.width, 1).ok_or(
        ThumbnailDecodeError::RgbaLengthOverflow {
            width: clip.width,
            height: 1,
        },
    )?;
    let frame_len = crate::media::pixel::checked_rgba_len(clip.width, clip.height).ok_or(
        ThumbnailDecodeError::RgbaLengthOverflow {
            width: clip.width,
            height: clip.height,
        },
    )?;
    let mut rgba = Vec::with_capacity(frame_len);
    let atlas = animation.atlas_rgba_bytes();
    let atlas_width = u64::from(animation.atlas_width());

    for row in 0..clip.height {
        let y = u64::from(clip.y)
            .checked_add(u64::from(row))
            .ok_or(ThumbnailDecodeError::DimensionOverflow)?;
        let pixel_offset = y
            .checked_mul(atlas_width)
            .and_then(|offset| offset.checked_add(u64::from(clip.x)))
            .ok_or(ThumbnailDecodeError::DimensionOverflow)?;
        let byte_offset = pixel_offset
            .checked_mul(4)
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or(ThumbnailDecodeError::DimensionOverflow)?;
        let end = byte_offset
            .checked_add(row_len)
            .ok_or(ThumbnailDecodeError::DimensionOverflow)?;
        let Some(row_bytes) = atlas.get(byte_offset..end) else {
            return Err(ThumbnailDecodeError::RgbaLengthMismatch {
                expected: end,
                actual: atlas.len(),
            }
            .into());
        };
        rgba.extend_from_slice(row_bytes);
    }

    Ok(rgba)
}

/// Errors returned while fetching a thumbnail's bytes, plus whatever decoding
/// them reported.
///
/// Decoding has its own error type, carried whole rather than re-listed: a
/// second enum mirroring its variants needs a hand-written map between them,
/// which is one edit away from disagreeing.
#[derive(Debug, Error)]
pub enum ThumbnailError {
    #[error(transparent)]
    Decode(#[from] ThumbnailDecodeError),
    /// URL is not a valid HTTP(S) URL.
    #[error("invalid thumbnail URL {url}")]
    InvalidUrl { url: String },
    /// URL uses a scheme unsupported by the thumbnail fetcher.
    #[error("unsupported thumbnail URL scheme for {url}")]
    UnsupportedUrlScheme { url: String },
    /// Failed to fetch image bytes from a URL.
    #[error("failed to fetch thumbnail URL {url}")]
    UrlFetch {
        url: String,
        #[source]
        source: ureq::Error,
    },
    /// Failed to read URL response bytes.
    #[error("failed to read thumbnail URL response {url}")]
    UrlRead {
        url: String,
        #[source]
        source: ureq::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnails_url_cache_keys_are_trimmed_and_distinct_from_other_sources() {
        let input = ThumbnailInput::from_url(" https://example.invalid/preview.jpg ");
        let url_key = input.cache_key(128);
        let direct_key = ThumbnailKey::for_url("https://example.invalid/preview.jpg", 128);
        let bytes_key = ThumbnailKey::for_bytes("https://example.invalid/preview.jpg", 128);

        assert_eq!(url_key, direct_key);
        assert_ne!(url_key, bytes_key);
        assert_ne!(
            url_key.disk_file_name(),
            ThumbnailKey::for_url("https://example.invalid/preview.jpg", 256).disk_file_name()
        );
    }

    #[test]
    fn prepared_thumbnail_bakes_animated_gif_frames() {
        let dir = crate::test_support::TestDir::new("gmpublished-prepared-thumbnail-gif");
        let gif = dir.gif("animated.gif", 8, 8);
        let thumbnail = crate::media::thumbnail_worker::ThumbnailDecoder::new()
            .decode_and_resize_file(gif, 64)
            .expect("animated GIF thumbnail should decode");

        let prepared = PreparedThumbnail::from_thumbnail(thumbnail);
        let animation = prepared
            .animation()
            .expect("multi-frame GIF should have prepared animation");

        assert_eq!(animation.frame_count(), 2);
        assert!(prepared.byte_len() > prepared.thumbnail().byte_len());
        assert!(animation.frames().iter().all(|frame| frame.width() == 8
            && frame.height() == 8
            && frame.delay() > Duration::ZERO
            && frame.rgba_bytes().len() == 8 * 8 * 4));
    }

    #[test]
    fn animation_bake_failure_degrades_to_static_thumbnail() {
        let preview = super::super::gif_preview::broken_multi_frame_gif_preview();
        let thumbnail =
            Thumbnail::from_gif_preview(preview, super::super::gif_preview::GIF_PREVIEW_MAX_EDGE);

        let prepared = PreparedThumbnail::from_thumbnail(thumbnail);

        assert!(prepared.animation().is_none());
        assert_eq!(prepared.thumbnail().rgba_bytes().len(), 2 * 2 * 4);
    }
}

/// One thumbnail job: what to fetch, at what size, and the cache key those two
/// imply.
///
/// The key is derived here rather than passed alongside, so it cannot describe
/// a different input or edge than the fetch actually uses.
#[derive(Clone, Debug)]
pub struct ThumbnailRequest {
    key: ThumbnailKey,
    input: ThumbnailInput,
    physical_max_edge: u32,
}

impl ThumbnailRequest {
    #[must_use]
    pub fn new(input: ThumbnailInput, physical_max_edge: u32, mode: ThumbnailMode) -> Self {
        Self {
            key: input.cache_key_with_mode(physical_max_edge, mode),
            input,
            physical_max_edge,
        }
    }

    #[must_use]
    pub const fn key(&self) -> &ThumbnailKey {
        &self.key
    }

    #[must_use]
    pub const fn input(&self) -> &ThumbnailInput {
        &self.input
    }

    #[must_use]
    pub const fn physical_max_edge(&self) -> u32 {
        self.physical_max_edge
    }
}
