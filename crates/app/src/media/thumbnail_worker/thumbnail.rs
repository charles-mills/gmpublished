#[cfg(test)]
use std::io::BufReader;
#[cfg(test)]
use std::path::{Path, PathBuf};
use std::{
    fmt,
    io::{BufRead, Cursor, Seek},
    sync::Arc,
};

use fast_image_resize::{
    FilterType, ResizeAlg, ResizeOptions, Resizer, images::Image as ResizeImage, pixels::PixelType,
};
use image::{ImageFormat, ImageReader, Limits};
use thiserror::Error;

use super::gif_preview::{GifPreviewError, LazyGifPreview, decode_lazy_gif_preview};

pub type ThumbnailDecodeResult<T> = Result<T, ThumbnailDecodeError>;

/// Decoded and resized thumbnail data safe to move between workers and UI code.
///
/// The payload is plain straight-RGBA bytes. UI code wraps those bytes in Iced
/// image handles at the presentation cache boundary.
#[derive(Clone)]
pub struct Thumbnail {
    rgba: Arc<[u8]>,
    metadata: ThumbnailMetadata,
    animation: Option<LazyGifPreview>,
    /// ThumbHash of this thumbnail, when one has been computed (fresh decode) or
    /// read back from the disk cache. Absent for pre-hash cache entries.
    thumbhash: Option<Arc<[u8]>>,
}

impl Thumbnail {
    /// Creates a still thumbnail payload from straight RGBA bytes and metadata.
    pub fn new(
        rgba: impl Into<Arc<[u8]>>,
        metadata: ThumbnailMetadata,
    ) -> ThumbnailDecodeResult<Self> {
        let rgba = rgba.into();
        validate_rgba_len(metadata.width, metadata.height, rgba.len())?;
        Ok(Self {
            rgba,
            metadata,
            animation: None,
            thumbhash: None,
        })
    }

    /// Creates an animated thumbnail from a decoded GIF preview.
    ///
    /// The first frame becomes the still RGBA payload; the preview is retained
    /// so UI code can prepare or play the remaining frames later.
    #[must_use]
    pub fn from_gif_preview(preview: LazyGifPreview, max_edge: u32) -> Self {
        let frame = preview.first_frame();
        let (width, height) = (frame.width(), frame.height());
        Self {
            rgba: Arc::<[u8]>::from(frame.rgba_bytes()),
            metadata: ThumbnailMetadata {
                width,
                height,
                source_width: width,
                source_height: height,
                max_edge,
            },
            animation: Some(preview),
            thumbhash: None,
        }
    }

    /// Returns the straight RGBA pixel bytes.
    #[must_use]
    pub fn rgba_bytes(&self) -> &[u8] {
        &self.rgba
    }

    /// Returns a cheap `Arc` clone of the straight RGBA pixel bytes, for
    /// callers that hand ownership to a zero-copy wrapper instead of
    /// borrowing.
    #[must_use]
    pub fn rgba_arc(&self) -> Arc<[u8]> {
        Arc::clone(&self.rgba)
    }

    /// Returns thumbnail dimensions and source metadata.
    #[must_use]
    pub fn metadata(&self) -> &ThumbnailMetadata {
        &self.metadata
    }

    /// Returns the animation source when this thumbnail is an animated GIF.
    #[must_use]
    pub fn animation(&self) -> Option<&LazyGifPreview> {
        self.animation.as_ref()
    }

    /// Returns this thumbnail's ThumbHash, when one is attached.
    #[must_use]
    pub fn thumbhash(&self) -> Option<&[u8]> {
        self.thumbhash.as_deref()
    }

    /// Returns a cheap `Arc` clone of this thumbnail's ThumbHash, for callers
    /// that hand ownership downstream instead of borrowing.
    #[must_use]
    pub fn thumbhash_arc(&self) -> Option<Arc<[u8]>> {
        self.thumbhash.clone()
    }

    /// Attaches (or clears) this thumbnail's ThumbHash. Callers pass the hash
    /// computed on a fresh decode or read back from the disk cache.
    pub fn set_thumbhash(&mut self, thumbhash: Option<Arc<[u8]>>) {
        self.thumbhash = thumbhash;
    }

    pub(crate) fn make_static(&mut self) {
        self.animation = None;
    }

    /// Returns the in-memory byte weight of this payload.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.rgba.len()
            + self
                .animation
                .as_ref()
                .map_or(0, LazyGifPreview::encoded_byte_len)
    }
}

impl fmt::Debug for Thumbnail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Thumbnail")
            .field("width", &self.metadata.width)
            .field("height", &self.metadata.height)
            .field("source_width", &self.metadata.source_width)
            .field("source_height", &self.metadata.source_height)
            .field("byte_len", &self.byte_len())
            .finish()
    }
}

/// Metadata describing a decoded thumbnail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThumbnailMetadata {
    /// Output thumbnail width in pixels.
    pub width: u32,
    /// Output thumbnail height in pixels.
    pub height: u32,
    /// Source image width before resizing.
    pub source_width: u32,
    /// Source image height before resizing.
    pub source_height: u32,
    /// Requested maximum output width or height.
    pub max_edge: u32,
}

/// Errors returned by thumbnail decode and resize helpers.
#[derive(Debug, Error)]
pub enum ThumbnailDecodeError {
    /// Requested size was zero.
    #[error("thumbnail max edge must be greater than zero")]
    InvalidMaxEdge,
    /// Source image has no pixels.
    #[error("source image has invalid dimensions {width}x{height}")]
    EmptyImage { width: u32, height: u32 },
    /// Output dimensions overflowed supported integer bounds.
    #[error("thumbnail dimensions overflowed")]
    DimensionOverflow,
    /// A trusted RGBA payload could not be represented.
    #[error("thumbnail RGBA dimensions {width}x{height} overflow byte length")]
    RgbaLengthOverflow { width: u32, height: u32 },
    /// A trusted RGBA payload has the wrong length.
    #[error("thumbnail RGBA buffer length mismatch: expected {expected}, got {actual}")]
    RgbaLengthMismatch { expected: usize, actual: usize },
    /// Failed to read a local image file.
    #[cfg(test)]
    #[error("failed to read image file {}", path.display())]
    FileRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Failed while guessing an encoded image format.
    #[error("failed to inspect image bytes")]
    ImageIo(#[source] std::io::Error),
    /// Failed to decode an encoded image.
    #[error("failed to decode image")]
    ImageDecode(#[source] image::ImageError),
    /// Failed to decode GIF thumbnail frames.
    #[error("failed to decode GIF thumbnail")]
    GifDecode(#[source] GifPreviewError),
    /// Failed to construct a resize image view.
    #[error("failed to construct resize image")]
    ResizeImage(#[source] fast_image_resize::ImageBufferError),
    /// Failed to resize decoded image data.
    #[error("failed to resize image")]
    Resize(#[source] fast_image_resize::ResizeError),
}

/// Thumbnail decoder for image bytes and local files.
pub struct ThumbnailDecoder {
    resizer: Resizer,
    resize_options: ResizeOptions,
}

impl ThumbnailDecoder {
    /// Creates a thumbnail decoder with the production resize policy.
    #[must_use]
    pub fn new() -> Self {
        Self {
            resizer: Resizer::new(),
            resize_options: thumbnail_resize_options(),
        }
    }

    /// Decodes encoded image bytes into a bounded straight-RGBA thumbnail.
    pub fn decode_and_resize_bytes(
        &mut self,
        bytes: &[u8],
        max_edge: u32,
    ) -> ThumbnailDecodeResult<Thumbnail> {
        validate_max_edge(max_edge)?;

        if image::guess_format(bytes).ok() == Some(ImageFormat::Gif) {
            return decode_gif_thumbnail(bytes, max_edge);
        }

        self.decode_and_resize_reader(Cursor::new(bytes), max_edge)
    }

    pub fn resize_static_thumbnail(
        &mut self,
        thumbnail: Thumbnail,
        max_edge: u32,
    ) -> ThumbnailDecodeResult<Thumbnail> {
        validate_max_edge(max_edge)?;
        let metadata = thumbnail.metadata().clone();
        if metadata.width.max(metadata.height) <= max_edge {
            let mut thumbnail = thumbnail;
            thumbnail.make_static();
            thumbnail.metadata.max_edge = max_edge;
            return Ok(thumbnail);
        }

        let thumbhash = thumbnail.thumbhash_arc();
        let (width, height) = fit_inside(metadata.width, metadata.height, max_edge)?;
        let mut resized = self.resize_rgba(
            metadata.width,
            metadata.height,
            width,
            height,
            max_edge,
            thumbnail.rgba_bytes().to_vec(),
        )?;
        resized.metadata.source_width = metadata.source_width;
        resized.metadata.source_height = metadata.source_height;
        resized.set_thumbhash(thumbhash);
        Ok(resized)
    }

    /// Decodes a local image file into a bounded straight-RGBA thumbnail.
    #[cfg(test)]
    pub fn decode_and_resize_file(
        &mut self,
        path: impl AsRef<Path>,
        max_edge: u32,
    ) -> ThumbnailDecodeResult<Thumbnail> {
        validate_max_edge(max_edge)?;
        let path = path.as_ref();
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"))
        {
            let bytes = std::fs::read(path).map_err(|source| ThumbnailDecodeError::FileRead {
                path: path.to_path_buf(),
                source,
            })?;
            return decode_gif_thumbnail(&bytes, max_edge);
        }

        let file = std::fs::File::open(path).map_err(|source| ThumbnailDecodeError::FileRead {
            path: path.to_path_buf(),
            source,
        })?;
        self.decode_and_resize_reader(BufReader::new(file), max_edge)
    }

    fn decode_and_resize_reader<R: BufRead + Seek>(
        &mut self,
        reader: R,
        max_edge: u32,
    ) -> ThumbnailDecodeResult<Thumbnail> {
        let decoded = {
            let mut reader = ImageReader::new(reader)
                .with_guessed_format()
                .map_err(ThumbnailDecodeError::ImageIo)?;
            reader.limits(thumbnail_decode_limits());
            reader.decode().map_err(ThumbnailDecodeError::ImageDecode)?
        };

        let source_width = decoded.width();
        let source_height = decoded.height();
        let (width, height) = fit_inside(source_width, source_height, max_edge)?;
        let rgba = decoded.into_rgba8().into_raw();

        self.resize_rgba(source_width, source_height, width, height, max_edge, rgba)
    }

    fn resize_rgba(
        &mut self,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
        max_edge: u32,
        rgba: Vec<u8>,
    ) -> ThumbnailDecodeResult<Thumbnail> {
        let src = ResizeImage::from_vec_u8(source_width, source_height, rgba, PixelType::U8x4)
            .map_err(ThumbnailDecodeError::ResizeImage)?;
        let output_len = crate::media::pixel::checked_rgba_len(width, height)
            .ok_or(ThumbnailDecodeError::RgbaLengthOverflow { width, height })?;
        let mut output = vec![0; output_len];
        {
            let mut dst = ResizeImage::from_slice_u8(width, height, &mut output, PixelType::U8x4)
                .map_err(ThumbnailDecodeError::ResizeImage)?;

            self.resizer
                .resize(&src, &mut dst, Some(&self.resize_options))
                .map_err(ThumbnailDecodeError::Resize)?;
        }

        Thumbnail::new(
            output,
            ThumbnailMetadata {
                width,
                height,
                source_width,
                source_height,
                max_edge,
            },
        )
    }
}

impl Default for ThumbnailDecoder {
    fn default() -> Self {
        Self::new()
    }
}

pub fn validate_max_edge(max_edge: u32) -> ThumbnailDecodeResult<()> {
    if max_edge == 0 {
        return Err(ThumbnailDecodeError::InvalidMaxEdge);
    }

    Ok(())
}

/// Returns the production image decode limits used by thumbnail generation.
#[must_use]
pub fn thumbnail_decode_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(DECODER_MAX_IMAGE_EDGE);
    limits.max_image_height = Some(DECODER_MAX_IMAGE_EDGE);
    limits.max_alloc = Some(DECODER_MAX_ALLOC_BYTES);
    limits
}

const DECODER_MAX_IMAGE_EDGE: u32 = 4096;
const DECODER_MAX_ALLOC_BYTES: u64 = 64 * 1024 * 1024;

fn thumbnail_resize_options() -> ResizeOptions {
    ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3))
        .use_alpha(true)
}

fn decode_gif_thumbnail(bytes: &[u8], max_edge: u32) -> ThumbnailDecodeResult<Thumbnail> {
    let preview = decode_lazy_gif_preview(Arc::<[u8]>::from(bytes), max_edge)
        .map_err(ThumbnailDecodeError::GifDecode)?;
    Ok(thumbnail_from_gif_preview(preview, max_edge))
}

fn thumbnail_from_gif_preview(preview: LazyGifPreview, max_edge: u32) -> Thumbnail {
    if preview.frame_count() > 1 {
        return Thumbnail::from_gif_preview(preview, max_edge);
    }

    let frame = preview.first_frame();
    Thumbnail::new(
        Arc::<[u8]>::from(frame.rgba_bytes()),
        ThumbnailMetadata {
            width: frame.width(),
            height: frame.height(),
            source_width: frame.width(),
            source_height: frame.height(),
            max_edge,
        },
    )
    .expect("validated GIF preview frame must have a valid RGBA length")
}

fn fit_inside(
    source_width: u32,
    source_height: u32,
    max_edge: u32,
) -> ThumbnailDecodeResult<(u32, u32)> {
    if source_width == 0 || source_height == 0 {
        return Err(ThumbnailDecodeError::EmptyImage {
            width: source_width,
            height: source_height,
        });
    }

    let source_max = source_width.max(source_height);
    if source_max <= max_edge {
        return Ok((source_width, source_height));
    }

    let width = scale_dimension(source_width, max_edge, source_max)?;
    let height = scale_dimension(source_height, max_edge, source_max)?;
    Ok((width, height))
}

fn scale_dimension(value: u32, max_edge: u32, source_max: u32) -> ThumbnailDecodeResult<u32> {
    let scaled = (u64::from(value) * u64::from(max_edge)) / u64::from(source_max);
    u32::try_from(scaled.max(1)).map_err(|_| ThumbnailDecodeError::DimensionOverflow)
}

fn validate_rgba_len(width: u32, height: u32, actual: usize) -> ThumbnailDecodeResult<()> {
    let expected = crate::media::pixel::checked_rgba_len(width, height)
        .ok_or(ThumbnailDecodeError::RgbaLengthOverflow { width, height })?;
    if actual != expected {
        return Err(ThumbnailDecodeError::RgbaLengthMismatch { expected, actual });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use image::{ColorType, ImageEncoder, codecs::png::PngEncoder};

    use super::*;

    #[test]
    fn thumbnails_decode_and_downscale_preserves_aspect_ratio() {
        let png = png_bytes(80, 40);
        let thumbnail = decode_and_resize_bytes(&png, 20).expect("valid png should resize");

        assert_eq!(thumbnail.metadata().source_width, 80);
        assert_eq!(thumbnail.metadata().source_height, 40);
        assert_eq!(thumbnail.metadata().width, 20);
        assert_eq!(thumbnail.metadata().height, 10);
        assert_eq!(thumbnail.rgba_bytes().len(), 20 * 10 * 4);
    }

    #[test]
    fn thumbnails_invalid_image_returns_decode_error() {
        let error = decode_and_resize_bytes(b"not an image", 64)
            .expect_err("invalid bytes should not decode");

        assert!(matches!(
            error,
            ThumbnailDecodeError::ImageIo(_) | ThumbnailDecodeError::ImageDecode(_)
        ));
    }

    #[test]
    fn thumbnails_oversized_png_dimensions_return_decode_limit_error() {
        let png = png_container_bytes(DECODER_MAX_IMAGE_EDGE + 1, 1, 8, 6);
        let error = decode_and_resize_bytes(&png, 64)
            .expect_err("oversized dimensions should fail before pixel allocation");

        assert_image_limit_error(&error);
    }

    #[test]
    fn thumbnails_decoder_allocation_limit_rejects_large_rgba_buffers() {
        let mut limits = thumbnail_decode_limits();
        let result = limits.reserve_buffer(
            DECODER_MAX_IMAGE_EDGE,
            DECODER_MAX_IMAGE_EDGE,
            ColorType::Rgba16,
        );

        assert!(matches!(result, Err(image::ImageError::Limits(_))));
    }

    #[test]
    fn thumbnails_alpha_aware_resize_avoids_transparent_color_bleed() {
        let png = rgba_png_bytes(
            2,
            1,
            &[
                255, 0, 0, 0, //
                0, 0, 255, 255,
            ],
        );
        let thumbnail = decode_and_resize_bytes(&png, 1).expect("valid png should resize");
        let pixel = thumbnail.rgba_bytes();

        assert_eq!(thumbnail.metadata().width, 1);
        assert_eq!(thumbnail.metadata().height, 1);
        assert!(
            pixel[0] <= 8,
            "transparent red should not bleed into output: {pixel:?}"
        );
        assert!(
            pixel[2] >= 240,
            "opaque blue should remain dominant after alpha-aware resize: {pixel:?}"
        );
        assert!(
            (120..=136).contains(&pixel[3]),
            "half coverage should preserve semi-transparent alpha: {pixel:?}"
        );
    }

    #[test]
    fn static_resize_keeps_only_the_first_gif_frame() {
        let dir = crate::test_support::TestDir::new("gmpublished-static-thumbnail-gif");
        let gif = dir.gif("animated.gif", 8, 8);
        let mut decoder = ThumbnailDecoder::new();
        let thumbnail = decoder
            .decode_and_resize_file(gif, 64)
            .expect("animated GIF thumbnail should decode");
        assert!(thumbnail.animation().is_some());

        let resized = decoder
            .resize_static_thumbnail(thumbnail, 4)
            .expect("first frame should resize");

        assert!(resized.animation().is_none());
        assert_eq!(
            (resized.metadata().width, resized.metadata().height),
            (4, 4)
        );
    }

    #[test]
    fn thumbnails_reject_invalid_trusted_rgba_lengths() {
        let error = Thumbnail::new(
            vec![0; 7],
            ThumbnailMetadata {
                width: 2,
                height: 1,
                source_width: 2,
                source_height: 1,
                max_edge: 2,
            },
        )
        .expect_err("short trusted payload should fail");

        assert!(matches!(
            error,
            ThumbnailDecodeError::RgbaLengthMismatch {
                expected: 8,
                actual: 7
            }
        ));
    }

    fn decode_and_resize_bytes(bytes: &[u8], max_edge: u32) -> ThumbnailDecodeResult<Thumbnail> {
        let mut decoder = ThumbnailDecoder::new();
        decoder.decode_and_resize_bytes(bytes, max_edge)
    }

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = vec![0_u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let offset = ((y * width + x) * 4) as usize;
                rgba[offset] = x as u8;
                rgba[offset + 1] = y as u8;
                rgba[offset + 2] = 128;
                rgba[offset + 3] = 255;
            }
        }

        rgba_png_bytes(width, height, &rgba)
    }

    fn rgba_png_bytes(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut encoded = Vec::new();
        PngEncoder::new(&mut encoded)
            .write_image(rgba, width, height, ColorType::Rgba8.into())
            .expect("test png should encode");
        encoded
    }

    fn png_container_bytes(width: u32, height: u32, bit_depth: u8, color_type: u8) -> Vec<u8> {
        let mut encoded = Vec::from([137, 80, 78, 71, 13, 10, 26, 10]);
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.push(bit_depth);
        ihdr.push(color_type);
        ihdr.extend_from_slice(&[0, 0, 0]);
        append_png_chunk(&mut encoded, *b"IHDR", &ihdr);
        append_png_chunk(&mut encoded, *b"IDAT", &[0x78, 0x9c, 0x03, 0, 0, 0, 0, 1]);
        append_png_chunk(&mut encoded, *b"IEND", &[]);
        encoded
    }

    fn append_png_chunk(encoded: &mut Vec<u8>, chunk_type: [u8; 4], data: &[u8]) {
        encoded.extend_from_slice(&(data.len() as u32).to_be_bytes());
        encoded.extend_from_slice(&chunk_type);
        encoded.extend_from_slice(data);
        encoded.extend_from_slice(&png_crc(chunk_type, data).to_be_bytes());
    }

    fn png_crc(chunk_type: [u8; 4], data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffff_u32;
        for byte in chunk_type.iter().chain(data) {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = 0_u32.wrapping_sub(crc & 1);
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }

    fn assert_image_limit_error(error: &ThumbnailDecodeError) {
        assert!(matches!(
            error,
            ThumbnailDecodeError::ImageDecode(image::ImageError::Limits(_))
        ));
    }
}
