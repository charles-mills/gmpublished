//! App-local GIF preview decode, composition, resizing, and animation atlas data.

#[cfg(test)]
use std::cell::RefCell;
use std::{
    fmt,
    io::{Cursor, Read},
    sync::Arc,
    time::Duration,
};

use fast_image_resize::{
    FilterType, ResizeAlg, ResizeOptions, Resizer, images::Image as ResizeImage, pixels::PixelType,
};
use gif::{ColorOutput, DecodeOptions, DisposalMethod, MemoryLimit};
use thiserror::Error;

/// Minimum frame delay used for GIFs that declare a zero or tiny delay.
pub const MIN_FRAME_DELAY: Duration = Duration::from_millis(10);
/// Maximum width or height retained for decoded GIF preview frames in tests.
#[cfg(test)]
pub const GIF_PREVIEW_MAX_EDGE: u32 = 256;
/// Maximum frame count retained in a baked display atlas before decimation.
pub const BAKED_ANIMATION_MAX_FRAMES: usize = 64;
/// Maximum transient display-atlas byte weight.
pub const BAKED_ANIMATION_MAX_ATLAS_BYTES: usize = 16 * 1024 * 1024;

#[cfg(test)]
const NANOS_PER_MILLI: u128 = 1_000_000;
#[cfg(test)]
const NANOS_PER_SECOND: u128 = 1_000_000_000;
const GIF_DECODER_MAX_IMAGE_EDGE: u32 = 4096;
const GIF_DECODER_MAX_ALLOC_BYTES: u64 = 64 * 1024 * 1024;

pub type GifPreviewResult<T> = Result<T, GifPreviewError>;

/// One decoded GIF frame as plain RGBA bytes with a normalized frame delay.
#[derive(Clone, Eq, PartialEq)]
pub struct GifPreviewFrame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    delay: Duration,
}

impl GifPreviewFrame {
    #[cfg(test)]
    pub fn new(width: u32, height: u32, rgba: Vec<u8>, delay: Duration) -> GifPreviewResult<Self> {
        Self::from_rgba(0, width, height, rgba, delay)
    }

    fn from_rgba(
        frame_index: usize,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        delay: Duration,
    ) -> GifPreviewResult<Self> {
        validate_frame_dimensions(frame_index, width, height)?;
        validate_rgba_len(frame_index, width, height, rgba.len())?;

        Ok(Self {
            width,
            height,
            rgba,
            delay: normalize_duration_delay(delay),
        })
    }

    /// Returns the decoded preview frame width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Returns the decoded preview frame height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Returns the normalized delay before advancing to the next frame.
    #[must_use]
    pub fn delay(&self) -> Duration {
        self.delay
    }

    /// Returns the decoded RGBA pixel bytes for this frame.
    #[must_use]
    pub fn rgba_bytes(&self) -> &[u8] {
        &self.rgba
    }

    /// Returns the decoded byte weight of this frame.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.rgba.len()
    }
}

impl fmt::Debug for GifPreviewFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GifPreviewFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("delay", &self.delay)
            .field("byte_len", &self.byte_len())
            .finish()
    }
}

/// Pixel-space source clip for one frame in a baked animation atlas.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BakedAnimationFrameClip {
    /// Left edge of the frame tile inside the atlas.
    pub x: u32,
    /// Top edge of the frame tile inside the atlas.
    pub y: u32,
    /// Width of the frame tile.
    pub width: u32,
    /// Height of the frame tile.
    pub height: u32,
}

/// Display-resolution sprite atlas for one animated image.
#[derive(Clone, Debug)]
pub struct BakedAnimation {
    atlas_width: u32,
    tile_width: u32,
    tile_height: u32,
    columns: u32,
    atlas_rgba: Vec<u8>,
    cumulative_frame_times: Arc<[Duration]>,
}

impl BakedAnimation {
    /// Returns the atlas width in pixels.
    #[must_use]
    pub fn atlas_width(&self) -> u32 {
        self.atlas_width
    }

    /// Returns the atlas height in pixels.
    #[cfg(test)]
    #[must_use]
    pub fn atlas_height(&self) -> u32 {
        self.rows().saturating_mul(self.tile_height)
    }

    /// Returns the per-frame tile width in pixels.
    #[cfg(test)]
    #[must_use]
    pub fn tile_width(&self) -> u32 {
        self.tile_width
    }

    /// Returns the per-frame tile height in pixels.
    #[cfg(test)]
    #[must_use]
    pub fn tile_height(&self) -> u32 {
        self.tile_height
    }

    #[cfg(test)]
    #[must_use]
    pub fn columns(&self) -> u32 {
        self.columns
    }

    #[cfg(test)]
    #[must_use]
    pub fn rows(&self) -> u32 {
        let frame_count = u32::try_from(self.frame_count()).unwrap_or(u32::MAX);
        frame_count.div_ceil(self.columns.max(1))
    }

    /// Returns the number of animation frames retained in the atlas.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.cumulative_frame_times.len()
    }

    /// Returns the normalized total loop duration.
    #[cfg(test)]
    #[must_use]
    pub fn total_duration(&self) -> Duration {
        self.cumulative_frame_times
            .last()
            .copied()
            .unwrap_or_default()
    }

    /// Returns the atlas RGBA bytes, row-major, four bytes per pixel.
    #[must_use]
    pub fn atlas_rgba_bytes(&self) -> &[u8] {
        &self.atlas_rgba
    }

    #[cfg(test)]
    #[must_use]
    pub fn atlas_byte_len(&self) -> usize {
        self.atlas_rgba.len()
    }

    /// Returns the cumulative end time for each frame.
    #[must_use]
    pub fn cumulative_frame_times(&self) -> &[Duration] {
        &self.cumulative_frame_times
    }

    /// Returns the source clip for `frame_index`.
    #[must_use]
    pub fn frame_clip(&self, frame_index: usize) -> Option<BakedAnimationFrameClip> {
        if frame_index >= self.frame_count() {
            return None;
        }

        let index = u32::try_from(frame_index).ok()?;
        let column = index % self.columns;
        let row = index / self.columns;
        Some(BakedAnimationFrameClip {
            x: column.saturating_mul(self.tile_width),
            y: row.saturating_mul(self.tile_height),
            width: self.tile_width,
            height: self.tile_height,
        })
    }

    /// Returns the frame visible at `elapsed` in a looping playback.
    #[cfg(test)]
    #[must_use]
    pub fn frame_index_at(&self, elapsed: Duration) -> usize {
        let frame_count = self.frame_count();
        if frame_count <= 1 {
            return 0;
        }

        let total_nanos = duration_nanos(self.total_duration());
        if total_nanos == 0 {
            return 0;
        }
        let position = duration_nanos(elapsed) % total_nanos;
        self.cumulative_frame_times
            .partition_point(|end| duration_nanos(*end) <= position)
            .min(frame_count - 1)
    }
}

/// GIF preview data that keeps only encoded bytes, timing metadata, and the
/// first decoded frame at initial readiness.
#[derive(Clone, Debug)]
pub struct LazyGifPreview {
    bytes: Arc<[u8]>,
    delays: Arc<[Duration]>,
    first_frame: GifPreviewFrame,
    max_edge: u32,
}

impl LazyGifPreview {
    /// Returns the number of frames discovered in the GIF stream.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.delays.len()
    }

    /// Returns the first decoded frame, available immediately at readiness.
    #[must_use]
    pub fn first_frame(&self) -> &GifPreviewFrame {
        &self.first_frame
    }

    /// Returns the byte length of the retained encoded GIF stream.
    #[must_use]
    pub fn encoded_byte_len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns the retained encoded GIF stream, e.g. for persisting to a cache.
    #[must_use]
    pub fn encoded_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the display edge cap used when decoding this preview.
    #[must_use]
    pub fn max_edge(&self) -> u32 {
        self.max_edge
    }

    /// Returns the byte weight of decoded RGBA held immediately after preview preparation.
    #[cfg(test)]
    #[must_use]
    pub fn initial_decoded_byte_len(&self) -> usize {
        self.first_frame.byte_len()
    }

    /// Returns the peak decoded RGBA byte weight held while preparing the preview.
    ///
    /// Readiness decodes only frame 0, so the peak equals the retained first
    /// frame; later frames are decoded one at a time during playback.
    #[cfg(test)]
    #[must_use]
    pub fn initial_peak_decoded_byte_len(&self) -> usize {
        self.first_frame.byte_len()
    }
}

/// Lazily decodes GIF preview frames from retained encoded bytes.
#[cfg(test)]
pub struct LazyGifPlayback {
    preview: LazyGifPreview,
    stream: RefCell<LazyGifFrameStream>,
}

#[cfg(test)]
impl LazyGifPlayback {
    #[must_use]
    pub fn new(preview: LazyGifPreview) -> Self {
        Self {
            stream: RefCell::new(LazyGifFrameStream::new(&preview)),
            preview,
        }
    }

    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.preview.frame_count()
    }

    /// Decodes and returns the requested frame, resetting the lazy stream when needed.
    pub fn frame(&self, frame_index: usize) -> GifPreviewResult<GifPreviewFrame> {
        if frame_index >= self.preview.frame_count() {
            return Err(GifPreviewError::FrameIndexOutOfRange {
                frame_index,
                frame_count: self.preview.frame_count(),
            });
        }
        if frame_index == 0 {
            *self.stream.borrow_mut() = LazyGifFrameStream::new(&self.preview);
            return Ok(self.preview.first_frame.clone());
        }
        self.stream
            .borrow_mut()
            .decode_frame(&self.preview, frame_index)
    }
}

#[cfg(test)]
struct LazyGifFrameStream {
    next_index: usize,
    frames: CompositedGifFrameStream<Cursor<Arc<[u8]>>>,
    decoder: GifFrameDecoder,
}

#[cfg(test)]
impl LazyGifFrameStream {
    fn new(preview: &LazyGifPreview) -> Self {
        Self {
            next_index: 0,
            frames: gif_preview_frame_stream(Cursor::new(Arc::clone(&preview.bytes)))
                .expect("validated lazy GIF bytes should reopen"),
            decoder: GifFrameDecoder::new(preview.max_edge),
        }
    }

    fn decode_frame(
        &mut self,
        preview: &LazyGifPreview,
        requested_frame_index: usize,
    ) -> GifPreviewResult<GifPreviewFrame> {
        if requested_frame_index < self.next_index {
            *self = Self::new(preview);
        }

        while self.next_index <= requested_frame_index {
            let frame_index = self.next_index;
            let frame = self.frames.decode_next_frame(frame_index)?.ok_or(
                GifPreviewError::FrameIndexOutOfRange {
                    frame_index: requested_frame_index,
                    frame_count: preview.frame_count(),
                },
            )?;
            self.next_index += 1;
            if frame_index == requested_frame_index {
                return self.decoder.decode_frame(frame_index, frame);
            }
        }

        Err(GifPreviewError::FrameIndexOutOfRange {
            frame_index: requested_frame_index,
            frame_count: preview.frame_count(),
        })
    }
}

/// Decodes all GIF frames into plain RGBA buffers with normalized delays.
///
/// Frames larger than [`GIF_PREVIEW_MAX_EDGE`] are downscaled with bilinear,
/// alpha-aware filtering. Smaller frames are copied without upscaling.
#[cfg(test)]
pub fn decode_gif_preview_frames(bytes: &[u8]) -> GifPreviewResult<Vec<GifPreviewFrame>> {
    let mut frame_stream = gif_preview_frame_stream(Cursor::new(bytes))?;
    let mut frame_decoder = GifFrameDecoder::new(GIF_PREVIEW_MAX_EDGE);
    let mut decoded = Vec::new();
    let mut frame_index = 0_usize;

    while let Some(frame) = frame_stream.decode_next_frame(frame_index)? {
        decoded.push(frame_decoder.decode_frame(frame_index, frame)?);
        frame_index += 1;
    }

    if decoded.is_empty() {
        return Err(GifPreviewError::EmptyFrameSet);
    }

    Ok(decoded)
}

/// Bakes encoded GIF bytes into one display-resolution sprite atlas.
pub fn bake_gif_animation(bytes: &[u8], display_max_edge: u32) -> GifPreviewResult<BakedAnimation> {
    // A baked atlas must be loop-invariant, but GIF frame 0 differs between the
    // first loop and later loops when disposal leaves previous-frame pixels.
    let (primed_canvas, frame_count) = prime_gif_canvas(bytes)?;
    let tile_max_edge = budgeted_tile_edge(
        display_max_edge,
        frame_count.min(BAKED_ANIMATION_MAX_FRAMES),
        BAKED_ANIMATION_MAX_ATLAS_BYTES,
    )?;

    let mut frame_stream = gif_preview_frame_stream(Cursor::new(bytes))?;
    frame_stream.seed_canvas(primed_canvas);
    let mut frame_decoder = GifFrameDecoder::new(tile_max_edge);
    let mut frames = Vec::new();
    let mut frame_index = 0_usize;

    while let Some(frame) = frame_stream.decode_next_frame(frame_index)? {
        frames.push(frame_decoder.decode_frame(frame_index, frame)?);
        frame_index += 1;
    }

    bake_gif_preview_frames(frames)
}

fn budgeted_tile_edge(
    display_max_edge: u32,
    frame_count: usize,
    budget_bytes: usize,
) -> GifPreviewResult<u32> {
    Ok(display_max_edge.min(atlas_budget_edge_cap(frame_count, budget_bytes)?))
}

/// Returns the largest square tile edge whose packed atlas stays within `budget_bytes`.
///
/// The budget divides across the same `columns x rows` grid that
/// `pack_baked_animation` allocates, which can hold more tiles than
/// `frame_count`. Non-square tiles are capped on their long edge and the short
/// edge rounds down, so the atlas can only get smaller than this bound.
fn atlas_budget_edge_cap(frame_count: usize, budget_bytes: usize) -> GifPreviewResult<u32> {
    let columns = atlas_columns(frame_count)?;
    let rows = atlas_rows(frame_count, columns)?;
    let grid_bytes_per_square_pixel = u64::from(columns)
        .saturating_mul(u64::from(rows))
        .saturating_mul(4);
    let tile_pixel_budget =
        u64::try_from(budget_bytes).unwrap_or(u64::MAX) / grid_bytes_per_square_pixel;
    Ok(u32::try_from(tile_pixel_budget.isqrt()).unwrap_or(u32::MAX))
}

/// Composites one full loop and returns the steady-state canvas and frame count.
fn prime_gif_canvas(bytes: &[u8]) -> GifPreviewResult<(Vec<u8>, usize)> {
    let mut frame_stream = gif_preview_frame_stream(Cursor::new(bytes))?;
    let mut frame_count = 0_usize;
    while frame_stream.decode_next_frame(frame_count)?.is_some() {
        frame_count += 1;
    }
    Ok((frame_stream.into_canvas(), frame_count))
}

pub fn bake_lazy_gif_preview(preview: &LazyGifPreview) -> GifPreviewResult<BakedAnimation> {
    bake_gif_animation(preview.encoded_bytes(), preview.max_edge())
}

pub fn bake_gif_preview_frames(
    frames: impl Into<Vec<GifPreviewFrame>>,
) -> GifPreviewResult<BakedAnimation> {
    let frames = frames.into();
    if frames.is_empty() {
        return Err(GifPreviewError::EmptyFrameSet);
    }
    let frames = decimate_baked_frames(frames);
    pack_baked_animation(&frames)
}

/// Prepares a GIF preview by decoding only the first frame.
///
/// The frame count and per-frame delays come from `gif_frame_delays`, a
/// metadata-only walk that never decodes pixels. Initial readiness then decodes
/// exactly frame 0: the only frame needed to paint the preview.
pub fn decode_lazy_gif_preview(
    bytes: impl Into<Arc<[u8]>>,
    max_edge: u32,
) -> GifPreviewResult<LazyGifPreview> {
    let bytes = bytes.into();
    let delays = gif_frame_delays(&bytes)?;
    if delays.is_empty() {
        return Err(GifPreviewError::EmptyFrameSet);
    }
    let first_frame = decode_first_frame(&bytes, max_edge)?;
    Ok(LazyGifPreview {
        bytes,
        delays: delays.into(),
        first_frame,
        max_edge,
    })
}

/// Builds a preview whose retained bytes cannot be re-decoded, to exercise an
/// animation bake failure while a healthy first frame exists.
#[cfg(test)]
#[must_use]
pub fn broken_multi_frame_gif_preview() -> LazyGifPreview {
    LazyGifPreview {
        bytes: Arc::from(&b"not a gif"[..]),
        delays: vec![MIN_FRAME_DELAY; 2].into(),
        first_frame: GifPreviewFrame::new(2, 2, vec![255; 2 * 2 * 4], MIN_FRAME_DELAY)
            .expect("test frame dimensions are valid"),
        max_edge: GIF_PREVIEW_MAX_EDGE,
    }
}

fn decode_first_frame(bytes: &[u8], max_edge: u32) -> GifPreviewResult<GifPreviewFrame> {
    let mut frame_stream = gif_preview_frame_stream(Cursor::new(bytes))?;
    let mut frame_decoder = GifFrameDecoder::new(max_edge);
    let frame = frame_stream
        .decode_next_frame(0)?
        .ok_or(GifPreviewError::EmptyFrameSet)?;
    frame_decoder.decode_frame(0, frame)
}

fn gif_frame_delays(bytes: &[u8]) -> GifPreviewResult<Vec<Duration>> {
    let mut decoder = gif_decoder_from_reader(Cursor::new(bytes), true, GifPreviewError::GifParse)?;

    let mut delays = Vec::new();
    while let Some(frame) = decoder
        .next_frame_info()
        .map_err(GifPreviewError::GifParse)?
    {
        delays.push(normalize_gif_delay(frame.delay));
    }
    Ok(delays)
}

fn gif_preview_frame_stream<R: Read>(reader: R) -> GifPreviewResult<CompositedGifFrameStream<R>> {
    let decoder = gif_decoder_from_reader(reader, false, GifPreviewError::Decode)?;
    CompositedGifFrameStream::new(decoder)
}

fn gif_decoder_from_reader<R: Read>(
    reader: R,
    skip_frame_decoding: bool,
    map_error: impl Fn(gif::DecodingError) -> GifPreviewError,
) -> GifPreviewResult<gif::Decoder<R>> {
    let options = gif_decode_options(skip_frame_decoding);
    let decoder = options.read_info(reader).map_err(map_error)?;
    validate_gif_logical_screen(decoder.width(), decoder.height())?;
    Ok(decoder)
}

fn gif_decode_options(skip_frame_decoding: bool) -> DecodeOptions {
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    options.set_memory_limit(gif_memory_limit());
    options.skip_frame_decoding(skip_frame_decoding);
    options
}

fn gif_memory_limit() -> MemoryLimit {
    MemoryLimit::Bytes(
        std::num::NonZeroU64::new(GIF_DECODER_MAX_ALLOC_BYTES)
            .expect("GIF decoder limit is non-zero"),
    )
}

fn validate_gif_logical_screen(width: u16, height: u16) -> GifPreviewResult<()> {
    if u32::from(width) > GIF_DECODER_MAX_IMAGE_EDGE
        || u32::from(height) > GIF_DECODER_MAX_IMAGE_EDGE
    {
        return Err(GifPreviewError::LogicalScreenTooLarge {
            width: u32::from(width),
            height: u32::from(height),
        });
    }
    Ok(())
}

/// Returns the saturated decoded byte weight for a set of GIF preview frames.
#[cfg(test)]
#[must_use]
pub fn decoded_byte_len(frames: &[GifPreviewFrame]) -> usize {
    frames.iter().fold(0_usize, |total, frame| {
        total.saturating_add(frame.byte_len())
    })
}

fn decimate_baked_frames(mut frames: Vec<GifPreviewFrame>) -> Vec<GifPreviewFrame> {
    let original_count = frames.len();
    if original_count <= BAKED_ANIMATION_MAX_FRAMES {
        return frames;
    }

    log::warn!(
        "decimating GIF display atlas from {original_count} to {BAKED_ANIMATION_MAX_FRAMES} frames"
    );

    let mut decimated = Vec::with_capacity(BAKED_ANIMATION_MAX_FRAMES);
    for bucket in 0..BAKED_ANIMATION_MAX_FRAMES {
        let start = bucket * original_count / BAKED_ANIMATION_MAX_FRAMES;
        let end = ((bucket + 1) * original_count / BAKED_ANIMATION_MAX_FRAMES).max(start + 1);
        let delay = frames[start..end]
            .iter()
            .fold(Duration::ZERO, |total, frame| {
                total.saturating_add(frame.delay())
            });
        let mut frame = frames[start].clone();
        frame.delay = normalize_duration_delay(delay);
        decimated.push(frame);
    }
    frames.clear();
    decimated
}

fn pack_baked_animation(frames: &[GifPreviewFrame]) -> GifPreviewResult<BakedAnimation> {
    let frame_count = frames.len();
    let tile_width = frames[0].width();
    let tile_height = frames[0].height();
    for (frame_index, frame) in frames.iter().enumerate() {
        if frame.width() != tile_width || frame.height() != tile_height {
            return Err(GifPreviewError::InconsistentFrameDimensions {
                frame_index,
                expected_width: tile_width,
                expected_height: tile_height,
                actual_width: frame.width(),
                actual_height: frame.height(),
            });
        }
    }

    let columns = atlas_columns(frame_count)?;
    let rows = atlas_rows(frame_count, columns)?;
    let atlas_width = tile_width
        .checked_mul(columns)
        .ok_or(GifPreviewError::AtlasDimensionOverflow { frame_count })?;
    let atlas_height = tile_height
        .checked_mul(rows)
        .ok_or(GifPreviewError::AtlasDimensionOverflow { frame_count })?;
    let atlas_byte_len = crate::media::pixel::checked_rgba_len(atlas_width, atlas_height)
        .ok_or(GifPreviewError::AtlasDimensionOverflow { frame_count })?;
    if atlas_byte_len > BAKED_ANIMATION_MAX_ATLAS_BYTES {
        return Err(GifPreviewError::atlas_too_large(
            atlas_width,
            atlas_height,
            frame_count,
            atlas_byte_len,
        ));
    }

    let mut atlas_rgba = vec![0; atlas_byte_len];
    let row_bytes = usize::try_from(u64::from(tile_width) * 4)
        .map_err(|_| GifPreviewError::AtlasDimensionOverflow { frame_count })?;
    let row_bytes_u64 = u64::try_from(row_bytes)
        .map_err(|_| GifPreviewError::AtlasDimensionOverflow { frame_count })?;
    for (frame_index, frame) in frames.iter().enumerate() {
        let frame_index = u32::try_from(frame_index).unwrap_or(u32::MAX);
        let clip = BakedAnimationFrameClip {
            x: frame_index % columns * tile_width,
            y: frame_index / columns * tile_height,
            width: tile_width,
            height: tile_height,
        };
        for y in 0..tile_height {
            let source_start = usize::try_from(u64::from(y) * row_bytes_u64)
                .map_err(|_| GifPreviewError::AtlasDimensionOverflow { frame_count })?;
            let target_start = rgba_index(atlas_width, clip.x, clip.y + y)
                .ok_or(GifPreviewError::AtlasDimensionOverflow { frame_count })?;
            atlas_rgba[target_start..target_start + row_bytes]
                .copy_from_slice(&frame.rgba_bytes()[source_start..source_start + row_bytes]);
        }
    }

    let mut total_duration = Duration::ZERO;
    let cumulative_frame_times = frames
        .iter()
        .map(|frame| {
            total_duration = total_duration.saturating_add(frame.delay());
            total_duration
        })
        .collect::<Vec<_>>();

    Ok(BakedAnimation {
        atlas_width,
        tile_width,
        tile_height,
        columns,
        atlas_rgba,
        cumulative_frame_times: cumulative_frame_times.into(),
    })
}

fn atlas_columns(frame_count: usize) -> GifPreviewResult<u32> {
    let columns = (frame_count as f64).sqrt().ceil() as usize;
    u32::try_from(columns.max(1))
        .map_err(|_| GifPreviewError::AtlasDimensionOverflow { frame_count })
}

fn atlas_rows(frame_count: usize, columns: u32) -> GifPreviewResult<u32> {
    let columns = usize::try_from(columns)
        .map_err(|_| GifPreviewError::AtlasDimensionOverflow { frame_count })?;
    let rows = frame_count.div_ceil(columns.max(1));
    u32::try_from(rows.max(1)).map_err(|_| GifPreviewError::AtlasDimensionOverflow { frame_count })
}

struct CompositedGifFrame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    delay: Duration,
}

#[derive(Clone, Copy)]
struct RawGifFrameInfo {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    dispose: DisposalMethod,
    delay: Duration,
}

struct CompositedGifFrameStream<R: Read> {
    decoder: gif::Decoder<R>,
    logical_width: u32,
    logical_height: u32,
    canvas: Vec<u8>,
}

impl<R: Read> CompositedGifFrameStream<R> {
    fn new(decoder: gif::Decoder<R>) -> GifPreviewResult<Self> {
        let logical_width = u32::from(decoder.width());
        let logical_height = u32::from(decoder.height());
        let Some(canvas_len) = crate::media::pixel::checked_rgba_len(logical_width, logical_height)
        else {
            return Err(GifPreviewError::FrameTooLarge {
                frame_index: 0,
                width: logical_width,
                height: logical_height,
            });
        };
        if u64::try_from(canvas_len).unwrap_or(u64::MAX) > GIF_DECODER_MAX_ALLOC_BYTES {
            return Err(GifPreviewError::FrameTooLarge {
                frame_index: 0,
                width: logical_width,
                height: logical_height,
            });
        }

        Ok(Self {
            decoder,
            logical_width,
            logical_height,
            canvas: vec![0; canvas_len],
        })
    }

    /// Seeds the compositing canvas. Ignored if dimensions do not match.
    fn seed_canvas(&mut self, canvas: Vec<u8>) {
        if canvas.len() == self.canvas.len() {
            self.canvas = canvas;
        }
    }

    fn into_canvas(self) -> Vec<u8> {
        self.canvas
    }

    fn decode_next_frame(
        &mut self,
        frame_index: usize,
    ) -> GifPreviewResult<Option<CompositedGifFrame>> {
        let Some(info) = self.next_frame_info()? else {
            return Ok(None);
        };
        validate_frame_dimensions(frame_index, info.width, info.height)?;
        let expected = bounded_frame_rgba_len(frame_index, info.width, info.height)?;
        let actual = self.decoder.buffer_size();
        if actual != expected {
            return Err(GifPreviewError::BufferSizeMismatch {
                frame_index,
                expected,
                actual,
            });
        }

        let mut rgba = vec![0; expected];
        self.decoder
            .read_into_buffer(&mut rgba)
            .map_err(GifPreviewError::Decode)?;

        let composited = self.compose_frame(info, &rgba);
        Ok(Some(CompositedGifFrame {
            width: self.logical_width,
            height: self.logical_height,
            rgba: composited,
            delay: info.delay,
        }))
    }

    fn next_frame_info(&mut self) -> GifPreviewResult<Option<RawGifFrameInfo>> {
        self.decoder
            .next_frame_info()
            .map(|frame| {
                frame.map(|frame| RawGifFrameInfo {
                    left: u32::from(frame.left),
                    top: u32::from(frame.top),
                    width: u32::from(frame.width),
                    height: u32::from(frame.height),
                    dispose: frame.dispose,
                    delay: normalize_gif_delay(frame.delay),
                })
            })
            .map_err(GifPreviewError::Decode)
    }

    fn compose_frame(&mut self, info: RawGifFrameInfo, frame_rgba: &[u8]) -> Vec<u8> {
        let x_start = info.left.min(self.logical_width);
        let y_start = info.top.min(self.logical_height);
        let x_end = info.left.saturating_add(info.width).min(self.logical_width);
        let y_end = info
            .top
            .saturating_add(info.height)
            .min(self.logical_height);

        let logical_width = self.logical_width;
        let blend = |target: &mut [u8]| {
            if x_start >= x_end {
                return;
            }
            for y in y_start..y_end {
                let source_y = y - info.top;
                let Some(source_row) = rgba_index(info.width, x_start - info.left, source_y) else {
                    continue;
                };
                let Some(target_row) = rgba_index(logical_width, x_start, y) else {
                    continue;
                };
                let row_px = (x_end - x_start) as usize;
                let Some(source) = frame_rgba.get(source_row..source_row + row_px * 4) else {
                    continue;
                };
                let Some(target) = target.get_mut(target_row..target_row + row_px * 4) else {
                    continue;
                };

                // Copy contiguous opaque runs; GIF alpha is binary.
                let mut x = 0;
                while x < row_px {
                    if source[x * 4 + 3] == 0 {
                        x += 1;
                        continue;
                    }
                    let run_start = x;
                    while x < row_px && source[x * 4 + 3] != 0 {
                        x += 1;
                    }
                    target[run_start * 4..x * 4].copy_from_slice(&source[run_start * 4..x * 4]);
                }
            }
        };

        match info.dispose {
            // The canvas must survive this frame untouched; compose a copy.
            DisposalMethod::Previous => {
                let mut composited = self.canvas.clone();
                blend(&mut composited);
                composited
            }
            DisposalMethod::Any | DisposalMethod::Keep => {
                blend(&mut self.canvas);
                self.canvas.clone()
            }
            DisposalMethod::Background => {
                blend(&mut self.canvas);
                let composited = self.canvas.clone();
                self.clear_canvas_rect(x_start, y_start, x_end, y_end);
                composited
            }
        }
    }

    fn clear_canvas_rect(&mut self, x_start: u32, y_start: u32, x_end: u32, y_end: u32) {
        for y in y_start..y_end {
            for x in x_start..x_end {
                if let Some(index) = rgba_index(self.logical_width, x, y) {
                    self.canvas[index..index + 4].fill(0);
                }
            }
        }
    }
}

struct GifFrameDecoder {
    resizer: Resizer,
    resize_options: ResizeOptions,
    max_edge: u32,
}

impl GifFrameDecoder {
    fn new(max_edge: u32) -> Self {
        Self {
            resizer: Resizer::new(),
            resize_options: gif_preview_resize_options(),
            max_edge: max_edge.max(1),
        }
    }

    fn decode_frame(
        &mut self,
        frame_index: usize,
        frame: CompositedGifFrame,
    ) -> GifPreviewResult<GifPreviewFrame> {
        self.frame_from_rgba(
            frame_index,
            frame.width,
            frame.height,
            frame.rgba,
            frame.delay,
        )
    }

    fn frame_from_rgba(
        &mut self,
        frame_index: usize,
        source_width: u32,
        source_height: u32,
        rgba: Vec<u8>,
        delay: Duration,
    ) -> GifPreviewResult<GifPreviewFrame> {
        validate_frame_dimensions(frame_index, source_width, source_height)?;
        validate_rgba_len(frame_index, source_width, source_height, rgba.len())?;

        let (width, height) =
            fit_preview_inside(frame_index, source_width, source_height, self.max_edge)?;
        let rgba = if (width, height) == (source_width, source_height) {
            rgba
        } else {
            self.resize_rgba(
                frame_index,
                source_width,
                source_height,
                width,
                height,
                rgba,
            )?
        };

        GifPreviewFrame::from_rgba(frame_index, width, height, rgba, delay)
    }

    fn resize_rgba(
        &mut self,
        frame_index: usize,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> GifPreviewResult<Vec<u8>> {
        let src = ResizeImage::from_vec_u8(source_width, source_height, rgba, PixelType::U8x4)
            .map_err(GifPreviewError::ResizeImage)?;
        let mut dst = ResizeImage::new(width, height, PixelType::U8x4);

        self.resizer
            .resize(&src, &mut dst, Some(&self.resize_options))
            .map_err(GifPreviewError::Resize)?;

        validate_rgba_len(frame_index, width, height, dst.buffer().len())?;
        Ok(dst.buffer().to_vec())
    }
}

fn gif_preview_resize_options() -> ResizeOptions {
    ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
        .use_alpha(true)
}

fn validate_frame_dimensions(frame_index: usize, width: u32, height: u32) -> GifPreviewResult<()> {
    if width == 0 || height == 0 {
        return Err(GifPreviewError::EmptyFrame {
            frame_index,
            width,
            height,
        });
    }

    Ok(())
}

fn fit_preview_inside(
    frame_index: usize,
    source_width: u32,
    source_height: u32,
    max_edge: u32,
) -> GifPreviewResult<(u32, u32)> {
    let source_max = source_width.max(source_height);
    if source_max <= max_edge {
        return Ok((source_width, source_height));
    }

    let width = scale_dimension(frame_index, source_width, source_max, max_edge)?;
    let height = scale_dimension(frame_index, source_height, source_max, max_edge)?;
    Ok((width, height))
}

fn scale_dimension(
    frame_index: usize,
    value: u32,
    source_max: u32,
    max_edge: u32,
) -> GifPreviewResult<u32> {
    let scaled = (u64::from(value) * u64::from(max_edge)) / u64::from(source_max);
    u32::try_from(scaled.max(1)).map_err(|_| GifPreviewError::DimensionOverflow { frame_index })
}

fn validate_rgba_len(
    frame_index: usize,
    width: u32,
    height: u32,
    actual: usize,
) -> GifPreviewResult<()> {
    let Some(expected) = crate::media::pixel::checked_rgba_len(width, height) else {
        return Err(GifPreviewError::FrameTooLarge {
            frame_index,
            width,
            height,
        });
    };

    if actual != expected {
        return Err(GifPreviewError::BufferSizeMismatch {
            frame_index,
            expected,
            actual,
        });
    }

    Ok(())
}

fn bounded_frame_rgba_len(frame_index: usize, width: u32, height: u32) -> GifPreviewResult<usize> {
    let Some(expected) = crate::media::pixel::checked_rgba_len(width, height) else {
        return Err(GifPreviewError::FrameTooLarge {
            frame_index,
            width,
            height,
        });
    };
    if u64::try_from(expected).unwrap_or(u64::MAX) > GIF_DECODER_MAX_ALLOC_BYTES {
        return Err(GifPreviewError::FrameTooLarge {
            frame_index,
            width,
            height,
        });
    }
    Ok(expected)
}

fn rgba_index(width: u32, x: u32, y: u32) -> Option<usize> {
    let pixels = u64::from(y)
        .checked_mul(u64::from(width))?
        .checked_add(u64::from(x))?;
    usize::try_from(pixels.checked_mul(4)?).ok()
}

fn normalize_gif_delay(delay_cs: u16) -> Duration {
    normalize_duration_delay(Duration::from_millis(u64::from(delay_cs) * 10))
}

#[cfg(test)]
fn duration_nanos(duration: Duration) -> u128 {
    duration.as_nanos()
}

#[cfg(test)]
fn duration_from_ms_ratio(numerator: u32, denominator: u32) -> Duration {
    let denominator = u128::from(denominator.max(1));
    let nanos = u128::from(numerator).saturating_mul(NANOS_PER_MILLI) / denominator;
    normalize_duration_delay(duration_from_nanos_saturating(nanos))
}

fn normalize_duration_delay(delay: Duration) -> Duration {
    if delay < MIN_FRAME_DELAY {
        MIN_FRAME_DELAY
    } else {
        delay
    }
}

#[cfg(test)]
fn duration_from_nanos_saturating(nanos: u128) -> Duration {
    let seconds = nanos / NANOS_PER_SECOND;
    let subsecond_nanos = nanos % NANOS_PER_SECOND;

    let Ok(seconds) = u64::try_from(seconds) else {
        return Duration::MAX;
    };
    let Ok(subsecond_nanos) = u32::try_from(subsecond_nanos) else {
        return Duration::MAX;
    };

    Duration::new(seconds, subsecond_nanos)
}

/// Errors returned by GIF preview decoding and playback setup.
#[derive(Debug, Error)]
pub enum GifPreviewError {
    /// Failed to decode the GIF container or one of its frames.
    #[error("failed to decode GIF preview")]
    Decode(#[source] gif::DecodingError),
    /// Failed to parse GIF metadata without decoding frame pixels.
    #[error("failed to parse GIF preview metadata")]
    GifParse(#[source] gif::DecodingError),
    /// Failed to encode a generated GIF fixture.
    #[cfg(test)]
    #[error("failed to encode GIF preview fixture")]
    Encode(#[source] gif::EncodingError),
    /// The GIF decoded successfully but did not contain any frames.
    #[error("GIF preview contains no frames")]
    EmptyFrameSet,
    /// Requested GIF frame was not present.
    #[error("GIF preview frame {frame_index} was out of range for {frame_count} frames")]
    FrameIndexOutOfRange {
        frame_index: usize,
        frame_count: usize,
    },
    /// GIF logical screen exceeds the preview decoder safety limit.
    #[error("GIF preview logical screen is too large: {width}x{height}")]
    LogicalScreenTooLarge { width: u32, height: u32 },
    /// A decoded frame has invalid zero dimensions.
    #[error("decoded GIF frame {frame_index} has invalid dimensions {width}x{height}")]
    EmptyFrame {
        frame_index: usize,
        width: u32,
        height: u32,
    },
    /// A decoded frame is too large to address in memory.
    #[error("decoded GIF frame {frame_index} is too large: {width}x{height}")]
    FrameTooLarge {
        frame_index: usize,
        width: u32,
        height: u32,
    },
    /// A decoded RGBA frame did not contain the expected number of bytes.
    #[error(
        "decoded GIF frame {frame_index} byte count mismatch: expected {expected}, got {actual}"
    )]
    BufferSizeMismatch {
        frame_index: usize,
        expected: usize,
        actual: usize,
    },
    /// Resized frame dimensions overflowed supported integer bounds.
    #[error("decoded GIF frame {frame_index} resize dimensions overflowed")]
    DimensionOverflow { frame_index: usize },
    /// A decoded frame set changed dimensions before atlas packing.
    #[error(
        "decoded GIF frame {frame_index} dimensions changed: expected {expected_width}x{expected_height}, got {actual_width}x{actual_height}"
    )]
    InconsistentFrameDimensions {
        frame_index: usize,
        expected_width: u32,
        expected_height: u32,
        actual_width: u32,
        actual_height: u32,
    },
    /// The packed display atlas would exceed the transient animation budget.
    #[error(
        "GIF display atlas is too large: {width}x{height}, {frame_count} frames, {byte_len} bytes"
    )]
    AtlasTooLarge {
        width: u32,
        height: u32,
        frame_count: usize,
        byte_len: usize,
    },
    /// An atlas-packing size computation overflowed integer bounds, as
    /// distinct from `AtlasTooLarge`, which reports a fully measured atlas
    /// that exceeded the transient animation budget.
    #[error("GIF atlas packing arithmetic overflowed for {frame_count} frames")]
    AtlasDimensionOverflow { frame_count: usize },
    /// Failed to construct a resize image view.
    #[error("failed to construct GIF preview resize image")]
    ResizeImage(#[source] fast_image_resize::ImageBufferError),
    /// Failed to resize decoded GIF frame data.
    #[error("failed to resize GIF preview frame")]
    Resize(#[source] fast_image_resize::ResizeError),
}

impl GifPreviewError {
    /// The one genuinely-too-large case: a fully measured atlas that
    /// exceeds the transient animation budget.
    fn atlas_too_large(width: u32, height: u32, frame_count: usize, byte_len: usize) -> Self {
        Self::AtlasTooLarge {
            width,
            height,
            frame_count,
            byte_len,
        }
    }
}

#[cfg(test)]
mod tests;
