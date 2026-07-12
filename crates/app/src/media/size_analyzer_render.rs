//! Worker-safe label planning and rasterization for the Iced Size Analyzer
//! treemap, plus the shared color/geometry constants its canvas layers draw
//! with. The treemap itself is drawn on layered `canvas::Cache`s; only the
//! outlined tag-label bitmaps are produced on the CPU here.

use std::{collections::HashMap, sync::OnceLock};

use parking_lot::Mutex;
use quick_cache::unsync::Cache;

use crate::backend::size_analyzer::{
    Rect as TreemapRect, TreemapLayout, TreemapSquare, TreemapSquareData,
};
use crate::media::text;
use cosmic_text::{Align, Color as TextColor, FontSystem, SwashCache};
use iced::widget::image;

pub const BACKGROUND: RgbaColor = RgbaColor::rgb(0x1a, 0x1a, 0x1a);
const FALLBACK_TAG: RgbaColor = RgbaColor::rgb(0x0c, 0x0c, 0x0c);
pub const ADDON_PLACEHOLDER: RgbaColor = RgbaColor::rgb(0x0c, 0x0c, 0x0c);
pub const DEAD_GLYPH: RgbaColor = RgbaColor::rgb(0x34, 0x34, 0x34);
const DEAD_SIDE_RATIO: f32 = 0.42;
const DEAD_MIN_SIDE: f32 = 7.0;
const DEAD_FOLD_RATIO: f32 = 0.28;
const DEAD_STROKE_RATIO: f32 = 0.07;
const DEAD_STROKE_MIN: f32 = 1.0;
const DEAD_STROKE_MAX: f32 = 3.0;
const MIN_LABEL_SIDE: f64 = 24.0;
const MIN_FONT_SIZE: i32 = 8;
const MAX_FONT_SIZE: f64 = 150.0;
const LABEL_STROKE_RATIO: f64 = 0.08;
const LABEL_FIT_RATIO: f64 = 0.75;
const LABEL_MAX_FONT_RECT_RATIO: f64 = 0.70;
const VERTICAL_LABEL_ASPECT_RATIO: f64 = 0.33;
const LABEL_RASTER_CACHE_MAX_ENTRIES: usize = 256;
const RECORD_TEXT_STATS: bool = cfg!(test);

/// Returns the exact Size Analyzer tag color, or the fallback color.
#[must_use]
pub fn tag_color(tag: &str) -> RgbaColor {
    match tag {
        "addon" => RgbaColor::rgb(0x00, 0x6c, 0xc7),
        "weapon" => RgbaColor::rgb(0x8c, 0x01, 0x01),
        "servercontent" => RgbaColor::rgb(0x00, 0x00, 0x00),
        "fun" => RgbaColor::rgb(0x36, 0x8c, 0x01),
        "roleplay" => RgbaColor::rgb(0x00, 0xd4, 0xd4),
        "realism" => RgbaColor::rgb(0x84, 0x00, 0xd6),
        "vehicle" => RgbaColor::rgb(0x5d, 0x31, 0x31),
        "movie" => RgbaColor::rgb(0x47, 0xab, 0x94),
        "cartoon" => RgbaColor::rgb(0x64, 0x28, 0x65),
        "scenic" => RgbaColor::rgb(0xfb, 0x9e, 0x9e),
        "water" => RgbaColor::rgb(0x47, 0x54, 0xab),
        "comic" => RgbaColor::rgb(0x64, 0x28, 0x65),
        "build" => RgbaColor::rgb(0x3e, 0x6e, 0x79),
        "tool" => RgbaColor::rgb(0xb9, 0x85, 0x28),
        "gamemode" => RgbaColor::rgb(0x88, 0xcc, 0x86),
        "map" => RgbaColor::rgb(0x80, 0x41, 0x00),
        "npc" => RgbaColor::rgb(0xfd, 0xfa, 0x8e),
        "effects" => RgbaColor::rgb(0x27, 0xc5, 0x00),
        "model" => RgbaColor::rgb(0x80, 0x00, 0x7c),
        _ => FALLBACK_TAG,
    }
}

/// Cached tag label input in logical layout space.
#[derive(Clone, Debug, PartialEq)]
pub struct SizeAnalyzerTagLabel {
    /// Tag-region rectangle in logical layout space.
    pub(crate) rect: TreemapRect,
    pub(crate) text: String,
}

#[must_use]
pub fn tag_labels_for_layout(layout: &TreemapLayout) -> Vec<SizeAnalyzerTagLabel> {
    layout
        .squares
        .iter()
        .filter_map(|square| {
            let TreemapSquareData::Tag { tag, .. } = &square.data else {
                return None;
            };
            Some(SizeAnalyzerTagLabel {
                rect: square_rect(square),
                text: tag.clone(),
            })
        })
        .collect()
}

/// A pre-rasterized tag label ready to draw on the labels canvas layer.
#[derive(Clone, Debug, PartialEq)]
pub struct SizeAnalyzerLabelSprite {
    /// Tag text; the hidden-tag filter matches on it.
    pub(crate) text: String,
    /// Tag-region rectangle in logical layout space.
    pub(crate) rect: TreemapRect,
    /// Outlined label bitmap rasterized at `scale` × the fitted font size.
    pub(crate) handle: image::Handle,
    /// Bitmap width in physical pixels.
    pub(crate) width: u32,
    /// Bitmap height in physical pixels.
    pub(crate) height: u32,
    /// Raster scale bucket the bitmap was produced for.
    pub(crate) scale: f32,
    /// Draw rotated 90° clockwise (tall, narrow tag regions).
    pub(crate) vertical: bool,
}

/// Long-lived worker-side context for tag-label rasterization; owns the glyph
/// cache and label measure/raster caches over the shared Inter font system.
pub struct SizeAnalyzerLabelContext {
    text: TextRasterizer,
}

impl SizeAnalyzerLabelContext {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            text: TextRasterizer::new(),
        }
    }

    /// Plans and rasterizes every tag label that fits its region: font-fit
    /// runs in logical rect space, bitmaps rasterize at `logical font ×
    /// scale` so they draw crisply at logical size on a `scale`-factor
    /// display.
    pub(crate) fn rasterize_layout_labels(
        &mut self,
        layout: &TreemapLayout,
        scale: f32,
    ) -> Vec<SizeAnalyzerLabelSprite> {
        let mut font_system = text::shared_font_system().lock();
        tag_labels_for_layout(layout)
            .into_iter()
            .filter_map(|label| {
                self.rasterize_label(&mut font_system, label, sanitized_scale(scale))
            })
            .collect()
    }

    fn rasterize_label(
        &mut self,
        font_system: &mut FontSystem,
        label: SizeAnalyzerTagLabel,
        scale: f32,
    ) -> Option<SizeAnalyzerLabelSprite> {
        let rect = label.rect;
        if rect.width < MIN_LABEL_SIDE || rect.height < MIN_LABEL_SIDE {
            return None;
        }

        let vertical = rect.width / rect.height <= VERTICAL_LABEL_ASPECT_RATIO;
        let available_width = (rect.width * LABEL_FIT_RATIO).max(1.0) as f32;
        let available_height = (rect.height * LABEL_FIT_RATIO).max(1.0) as f32;
        let (fit_width, fit_height) = if vertical {
            (available_height, available_width)
        } else {
            (available_width, available_height)
        };
        let max_font = if vertical {
            rect.width * LABEL_MAX_FONT_RECT_RATIO
        } else {
            rect.height * LABEL_MAX_FONT_RECT_RATIO
        }
        .min(MAX_FONT_SIZE)
        .max(f64::from(MIN_FONT_SIZE));

        let (logical_font, _measured) = select_font_size(
            font_system,
            &mut self.text,
            &label.text,
            max_font,
            fit_width,
            fit_height,
            MIN_FONT_SIZE,
        )?;

        let raster = self
            .text
            .raster_label(font_system, &label.text, logical_font, scale)?;

        Some(SizeAnalyzerLabelSprite {
            text: label.text,
            rect,
            handle: raster.handle,
            width: raster.width,
            height: raster.height,
            scale,
            vertical,
        })
    }

    #[cfg(test)]
    pub(crate) fn text_stats(&self) -> TextRasterizerStats {
        self.text.stats()
    }
}

/// Runs `f` against the process-wide label context so repeated requests reuse
/// one font database instead of reloading it per request.
pub fn with_shared_label_context<T>(f: impl FnOnce(&mut SizeAnalyzerLabelContext) -> T) -> T {
    static SHARED: OnceLock<Mutex<SizeAnalyzerLabelContext>> = OnceLock::new();
    let mut context = SHARED
        .get_or_init(|| Mutex::new(SizeAnalyzerLabelContext::new()))
        .lock();
    f(&mut context)
}

fn sanitized_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale >= 1.0 {
        scale
    } else {
        1.0
    }
}

/// Vector geometry of the dead-file placeholder glyph, in the same coordinate
/// space as the input cell rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeadPlaceholder {
    pub(crate) left: f32,
    pub(crate) top: f32,
    pub(crate) right: f32,
    pub(crate) bottom: f32,
    pub(crate) fold: f32,
    pub(crate) stroke_width: f32,
}

/// Sizes the dead-file placeholder glyph centered in a cell; `None` when the
/// cell is too small to render it legibly.
#[must_use]
pub fn dead_placeholder_geometry(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Option<DeadPlaceholder> {
    let side = width.min(height) * DEAD_SIDE_RATIO;
    if !side.is_finite() || side < DEAD_MIN_SIDE {
        return None;
    }

    let center_x = x + width / 2.0;
    let center_y = y + height / 2.0;
    let half = side / 2.0;
    Some(DeadPlaceholder {
        left: center_x - half,
        top: center_y - half,
        right: center_x + half,
        bottom: center_y + half,
        fold: side * DEAD_FOLD_RATIO,
        stroke_width: (side * DEAD_STROKE_RATIO).clamp(DEAD_STROKE_MIN, DEAD_STROKE_MAX),
    })
}

/// RGBA color with byte channels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RgbaColor {
    pub(crate) red: u8,
    pub(crate) green: u8,
    pub(crate) blue: u8,
    pub(crate) alpha: u8,
}

impl RgbaColor {
    const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: 255,
        }
    }
}

fn scaled_pixel_constant(value: f64, px: f64) -> i32 {
    ((value * px).round() as i32).max(1)
}

fn label_stroke_width(font_size: f32, px: f64) -> i32 {
    let min = scaled_pixel_constant(2.0, px);
    let max = scaled_pixel_constant(5.0, px).max(min);
    ((f64::from(font_size) * LABEL_STROKE_RATIO).ceil() as i32).clamp(min, max)
}

fn select_font_size(
    font_system: &mut FontSystem,
    text: &mut TextRasterizer,
    label: &str,
    max_font: f64,
    fit_width: f32,
    fit_height: f32,
    min_font: i32,
) -> Option<(f32, (f32, f32))> {
    let mut selected = None;
    let mut low = min_font;
    let mut high = max_font.floor() as i32;
    while low <= high {
        let size = low + (high - low) / 2;
        let font_size = size as f32;
        let measured = text.measure(font_system, label, font_size);
        if measured.0 <= fit_width && measured.1 <= fit_height {
            selected = Some((font_size, measured));
            low = size + 1;
        } else {
            high = size - 1;
        }
    }
    selected
}

fn label_bitmap_size(measured: (f32, f32), stroke: i32) -> Option<(u32, u32)> {
    let stroke_padding = u32::try_from(stroke.saturating_mul(4)).ok()?;
    let width = (measured.0.ceil() as u32)
        .checked_add(stroke_padding)?
        .checked_add(8)?
        .max(1);
    let height = (measured.1.ceil() as u32)
        .checked_add(stroke_padding)?
        .checked_add(8)?
        .max(1);
    Some((width, height))
}

fn draw_label_outline(
    target: &mut PixelTarget<'_>,
    font_system: &mut FontSystem,
    text: &mut TextRasterizer,
    label: &str,
    font_size: f32,
    stroke: i32,
) {
    let Some(byte_len) = crate::media::pixel::checked_rgba_len(target.width, target.height) else {
        return;
    };

    // One shaping/raster pass: the white fill doubles as the coverage mask
    // whose dilation forms the outline ring.
    let mut fill = vec![0_u8; byte_len];
    {
        let mut fill_target = PixelTarget {
            data: &mut fill,
            width: target.width,
            height: target.height,
        };
        text.draw_text(
            font_system,
            &mut fill_target,
            label,
            font_size,
            [255, 255, 255, 255],
            (0.0, 0.0),
        );
    }

    let coverage = fill
        .chunks_exact(4)
        .map(|pixel| pixel[3])
        .collect::<Vec<_>>();
    let outline = dilate_disk_max(&coverage, target.width, target.height, stroke);
    for (y, row) in outline
        .chunks_exact(target.width.max(1) as usize)
        .enumerate()
    {
        for (x, alpha) in row.iter().copied().enumerate() {
            if alpha == 0 {
                continue;
            }
            target.blend_premul_pixel(x as i32, y as i32, [0, 0, 0, alpha]);
        }
    }

    let fill_source = PixelSource {
        data: &fill,
        width: target.width,
        height: target.height,
    };
    blit(target, &fill_source, 0, 0);
}

/// Dilates an alpha mask with a disk-shaped max filter, producing a
/// continuous outline ring with no detachment or scalloping on thin glyph
/// strokes.
fn dilate_disk_max(mask: &[u8], width: u32, height: u32, radius: i32) -> Vec<u8> {
    let mut dilated = vec![0_u8; mask.len()];
    if radius <= 0 {
        dilated.copy_from_slice(mask);
        return dilated;
    }

    let offsets = disk_offsets(radius);
    let width_i = width as i32;
    let height_i = height as i32;
    for (y, row) in mask.chunks_exact(width.max(1) as usize).enumerate() {
        for (x, alpha) in row.iter().copied().enumerate() {
            if alpha == 0 {
                continue;
            }
            for &(dx, dy) in &offsets {
                let dilated_x = x as i32 + dx;
                let dilated_y = y as i32 + dy;
                if dilated_x < 0 || dilated_y < 0 || dilated_x >= width_i || dilated_y >= height_i {
                    continue;
                }
                let dilated_index = (dilated_y * width_i + dilated_x) as usize;
                if let Some(slot) = dilated.get_mut(dilated_index)
                    && *slot < alpha
                {
                    *slot = alpha;
                }
            }
        }
    }
    dilated
}

fn disk_offsets(radius: i32) -> Vec<(i32, i32)> {
    let mut offsets = Vec::new();
    let limit = radius.saturating_mul(radius);
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy <= limit {
                offsets.push((dx, dy));
            }
        }
    }
    offsets
}

fn square_rect(square: &TreemapSquare) -> TreemapRect {
    TreemapRect {
        x: square.x,
        y: square.y,
        width: square.width,
        height: square.height,
    }
}

struct TextRasterizer {
    cache: SwashCache,
    measure_cache: HashMap<LabelMeasureKey, (f32, f32)>,
    raster_cache: Cache<LabelRasterKey, LabelRaster>,
    stats: TextRasterizerStats,
}

impl TextRasterizer {
    fn new() -> Self {
        Self {
            cache: SwashCache::new(),
            measure_cache: HashMap::new(),
            raster_cache: Cache::new(LABEL_RASTER_CACHE_MAX_ENTRIES),
            stats: TextRasterizerStats::default(),
        }
    }

    fn measure(&mut self, font_system: &mut FontSystem, text: &str, font_size: f32) -> (f32, f32) {
        let key = LabelMeasureKey::new(text, font_size);
        if let Some(measured) = self.measure_cache.get(&key).copied() {
            self.stats.record_measure_cache_hit();
            return measured;
        }

        self.stats.record_measure_cache_miss();

        let line_height = font_size * text::LINE_HEIGHT_RATIO;
        let buffer = text::shape(
            font_system,
            text,
            font_size,
            (None, Some(line_height)),
            Align::Left,
        );

        let mut width: f32 = 0.0;
        let mut height: f32 = line_height;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            height = height.max(run.line_height);
        }
        let measured = (width, height);
        self.measure_cache.insert(key, measured);
        measured
    }

    fn raster_label(
        &mut self,
        font_system: &mut FontSystem,
        text: &str,
        logical_font_size: f32,
        scale: f32,
    ) -> Option<LabelRaster> {
        let key = LabelRasterKey::new(text, logical_font_size, scale);
        if let Some(raster) = self.raster_cache.get(&key).cloned() {
            self.stats.record_raster_cache_hit();
            return Some(raster);
        }

        self.stats.record_raster_cache_miss();

        let font_size = logical_font_size * scale;
        let stroke = label_stroke_width(font_size, f64::from(scale));
        let measured = self.measure(font_system, text, font_size);

        let (width, height) = label_bitmap_size(measured, stroke)?;
        let byte_len = crate::media::pixel::checked_rgba_len(width, height)?;
        let mut data = vec![0_u8; byte_len];
        {
            let mut label_target = PixelTarget {
                data: &mut data,
                width,
                height,
            };
            draw_label_outline(
                &mut label_target,
                font_system,
                self,
                text,
                font_size,
                stroke,
            );
        }

        self.stats.record_raster_created();

        let raster = LabelRaster {
            handle: image::Handle::from_rgba(width, height, data),
            width,
            height,
        };
        // LRU-evicts the coldest entry past capacity instead of a full flush.
        self.raster_cache.insert(key, raster.clone());
        Some(raster)
    }

    fn draw_text(
        &mut self,
        font_system: &mut FontSystem,
        target: &mut PixelTarget<'_>,
        text: &str,
        font_size: f32,
        color: [u8; 4],
        offset: (f32, f32),
    ) {
        let line_height = font_size * text::LINE_HEIGHT_RATIO;
        let buffer = text::shape(
            font_system,
            text,
            font_size,
            (Some(target.width as f32), Some(line_height)),
            Align::Center,
        );
        let text_color = TextColor::rgba(color[0], color[1], color[2], color[3]);
        let vertical_offset = (target.height as f32 - line_height) / 2.0;

        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                let physical =
                    glyph.physical((offset.0, run.line_y + vertical_offset + offset.1), 1.0);
                self.cache.with_pixels(
                    font_system,
                    physical.cache_key,
                    text_color,
                    |dx, dy, glyph_color| {
                        let x = physical.x + dx;
                        let y = physical.y + dy;
                        target.blend_pixel(x, y, glyph_color.as_rgba());
                    },
                );
            }
        }
    }

    #[cfg(test)]
    fn stats(&self) -> TextRasterizerStats {
        TextRasterizerStats {
            cached_measurements: self.measure_cache.len(),
            cached_rasters: self.raster_cache.len(),
            ..self.stats
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LabelMeasureKey {
    text: Box<str>,
    font_size_bits: u32,
}

impl LabelMeasureKey {
    fn new(text: &str, font_size: f32) -> Self {
        Self {
            text: text.into(),
            font_size_bits: font_size.to_bits(),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LabelRasterKey {
    text: Box<str>,
    logical_font_size_bits: u32,
    scale_bits: u32,
}

impl LabelRasterKey {
    fn new(text: &str, logical_font_size: f32, scale: f32) -> Self {
        Self {
            text: text.into(),
            logical_font_size_bits: logical_font_size.to_bits(),
            scale_bits: scale.to_bits(),
        }
    }
}

#[derive(Clone, Debug)]
struct LabelRaster {
    handle: image::Handle,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TextRasterizerStats {
    pub(crate) measure_cache_hits: usize,
    pub(crate) measure_cache_misses: usize,
    pub(crate) cached_measurements: usize,
    pub(crate) raster_cache_hits: usize,
    pub(crate) raster_cache_misses: usize,
    pub(crate) rasters_created: usize,
    pub(crate) cached_rasters: usize,
}

impl TextRasterizerStats {
    fn record_measure_cache_hit(&mut self) {
        if RECORD_TEXT_STATS {
            self.measure_cache_hits += 1;
        }
    }

    fn record_measure_cache_miss(&mut self) {
        if RECORD_TEXT_STATS {
            self.measure_cache_misses += 1;
        }
    }

    fn record_raster_cache_hit(&mut self) {
        if RECORD_TEXT_STATS {
            self.raster_cache_hits += 1;
        }
    }

    fn record_raster_cache_miss(&mut self) {
        if RECORD_TEXT_STATS {
            self.raster_cache_misses += 1;
        }
    }

    fn record_raster_created(&mut self) {
        if RECORD_TEXT_STATS {
            self.rasters_created += 1;
        }
    }
}

struct PixelTarget<'a> {
    data: &'a mut [u8],
    width: u32,
    height: u32,
}

impl PixelTarget<'_> {
    fn blend_pixel(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        self.blend_premul_pixel(x, y, premultiply(rgba));
    }

    fn blend_premul_pixel(&mut self, x: i32, y: i32, src: [u8; 4]) {
        if x < 0 || y < 0 {
            return;
        }
        let Ok(x) = u32::try_from(x) else {
            return;
        };
        let Ok(y) = u32::try_from(y) else {
            return;
        };
        if x >= self.width || y >= self.height {
            return;
        }

        let index = ((y * self.width + x) * 4) as usize;
        let Some(dst) = self.data.get_mut(index..index + 4) else {
            return;
        };

        let inverse_alpha = 255_u16.saturating_sub(u16::from(src[3]));
        dst[0] = src_over_channel(src[0], dst[0], inverse_alpha);
        dst[1] = src_over_channel(src[1], dst[1], inverse_alpha);
        dst[2] = src_over_channel(src[2], dst[2], inverse_alpha);
        dst[3] = src[3].saturating_add(((u16::from(dst[3]) * inverse_alpha) / 255) as u8);
    }
}

struct PixelSource<'a> {
    data: &'a [u8],
    width: u32,
    height: u32,
}

fn blit(target: &mut PixelTarget<'_>, source: &PixelSource<'_>, dest_x: i32, dest_y: i32) {
    for sy in 0..source.height {
        for sx in 0..source.width {
            let source_index = ((sy * source.width + sx) * 4) as usize;
            let Some(pixel) = source.data.get(source_index..source_index + 4) else {
                continue;
            };
            target.blend_premul_pixel(
                dest_x + sx as i32,
                dest_y + sy as i32,
                [pixel[0], pixel[1], pixel[2], pixel[3]],
            );
        }
    }
}

fn src_over_channel(src: u8, dst: u8, inverse_alpha: u16) -> u8 {
    src.saturating_add(((u16::from(dst) * inverse_alpha) / 255) as u8)
}

fn premultiply(rgba: [u8; 4]) -> [u8; 4] {
    let alpha = u16::from(rgba[3]);
    [
        ((u16::from(rgba[0]) * alpha) / 255) as u8,
        ((u16::from(rgba[1]) * alpha) / 255) as u8,
        ((u16::from(rgba[2]) * alpha) / 255) as u8,
        rgba[3],
    ]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::backend::{
        domain::PublishedFileId,
        size_analyzer::{SizeAnalyzerAddon, TreemapBounds, TreemapLayout, analyze_addons},
    };

    use super::*;

    #[test]
    fn layout_labels_rasterize_with_expected_placement_data() {
        let layout = fixture_layout(240.0, 140.0);
        let mut context = SizeAnalyzerLabelContext::new();

        let sprites = context.rasterize_layout_labels(&layout, 1.0);

        assert!(!sprites.is_empty());
        for sprite in &sprites {
            assert!(sprite.width > 0);
            assert!(sprite.height > 0);
            assert!((sprite.scale - 1.0).abs() < f32::EPSILON);
            assert!(sprite.rect.width >= MIN_LABEL_SIDE);
            assert!(sprite.rect.height >= MIN_LABEL_SIDE);
        }
        let texts = sprites
            .iter()
            .map(|sprite| sprite.text.as_str())
            .collect::<Vec<_>>();
        assert!(texts.contains(&"tool"));
    }

    #[test]
    fn tall_narrow_regions_rasterize_vertical_sprites() {
        let layout = analyze_addons(
            vec![addon("skinny.gma", "Skinny", "servercontent", 1)],
            TreemapBounds::new(64.0, 320.0),
        )
        .unwrap();
        let mut context = SizeAnalyzerLabelContext::new();

        let sprites = context.rasterize_layout_labels(&layout, 1.0);

        assert_eq!(sprites.len(), 1);
        assert!(sprites[0].vertical);
    }

    #[test]
    fn regions_below_minimum_label_side_produce_no_sprites() {
        let layout = analyze_addons(
            vec![addon("skinny.gma", "Skinny", "servercontent", 1)],
            TreemapBounds::new(8.0, 80.0),
        )
        .unwrap();
        let mut context = SizeAnalyzerLabelContext::new();

        assert!(context.rasterize_layout_labels(&layout, 1.0).is_empty());
    }

    #[test]
    fn label_bitmaps_scale_with_the_raster_bucket() {
        let layout = fixture_layout(240.0, 140.0);
        let mut context = SizeAnalyzerLabelContext::new();

        let at_one = context.rasterize_layout_labels(&layout, 1.0);
        let at_two = context.rasterize_layout_labels(&layout, 2.0);

        assert_eq!(at_one.len(), at_two.len());
        for (small, large) in at_one.iter().zip(&at_two) {
            assert_eq!(small.text, large.text);
            // The bitmap roughly doubles; padding constants keep it inexact.
            assert!(large.width > small.width);
            assert!(large.height > small.height);
            assert!((large.scale - 2.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn context_reuses_measure_cache_across_requests() {
        let layout = fixture_layout(240.0, 140.0);
        let mut context = SizeAnalyzerLabelContext::new();

        let _sprites = context.rasterize_layout_labels(&layout, 1.0);
        let after_first = context.text_stats();
        assert!(after_first.measure_cache_misses > 0);
        assert!(after_first.cached_measurements > 0);

        let _sprites = context.rasterize_layout_labels(&layout, 1.0);
        let after_second = context.text_stats();
        assert_eq!(
            after_second.measure_cache_misses,
            after_first.measure_cache_misses
        );
        assert!(after_second.measure_cache_hits > after_first.measure_cache_hits);
        assert_eq!(
            after_second.cached_measurements,
            after_first.cached_measurements
        );
    }

    #[test]
    fn context_reuses_label_rasters_and_handles_across_requests() {
        let layout = fixture_layout(240.0, 140.0);
        let mut context = SizeAnalyzerLabelContext::new();

        let first = context.rasterize_layout_labels(&layout, 1.0);
        let after_first = context.text_stats();
        assert!(after_first.rasters_created > 0);
        assert_eq!(after_first.raster_cache_hits, 0);
        assert_eq!(after_first.cached_rasters, after_first.rasters_created);

        let second = context.rasterize_layout_labels(&layout, 1.0);
        let after_second = context.text_stats();
        assert_eq!(after_second.rasters_created, after_first.rasters_created);
        assert_eq!(
            after_second.raster_cache_misses,
            after_first.raster_cache_misses
        );
        assert!(after_second.raster_cache_hits > after_first.raster_cache_hits);
        assert_eq!(after_second.cached_rasters, after_first.cached_rasters);

        // Cached rasters keep the same GPU handle, so nothing re-uploads.
        for (a, b) in first.iter().zip(&second) {
            assert_eq!(a.handle, b.handle);
        }
    }

    #[test]
    fn context_reuses_label_rasters_across_pure_resize_when_font_bucket_matches() {
        let first_layout = analyze_addons(
            vec![addon("map-a.gma", "Map A", "map", 1)],
            TreemapBounds::new(1000.0, 1000.0),
        )
        .unwrap();
        let resized_layout = analyze_addons(
            vec![addon("map-a.gma", "Map A", "map", 1)],
            TreemapBounds::new(1200.0, 1000.0),
        )
        .unwrap();
        let mut context = SizeAnalyzerLabelContext::new();

        let first = context.rasterize_layout_labels(&first_layout, 1.0);
        let after_first = context.text_stats();
        let resized = context.rasterize_layout_labels(&resized_layout, 1.0);
        let after_resize = context.text_stats();

        assert_eq!(first.len(), 1);
        assert_eq!(resized.len(), 1);
        assert_eq!(after_resize.rasters_created, after_first.rasters_created);
        assert!(after_resize.raster_cache_hits > after_first.raster_cache_hits);
        assert_eq!(first[0].handle, resized[0].handle);
    }

    #[test]
    fn tag_color_map_covers_known_tags_and_falls_back() {
        assert_eq!(tag_color("addon"), RgbaColor::rgb(0x00, 0x6c, 0xc7));
        assert_eq!(tag_color("weapon"), RgbaColor::rgb(0x8c, 0x01, 0x01));
        assert_eq!(tag_color("servercontent"), RgbaColor::rgb(0x00, 0x00, 0x00));
        assert_eq!(tag_color("model"), RgbaColor::rgb(0x80, 0x00, 0x7c));
        assert_eq!(tag_color("not-a-real-tag"), FALLBACK_TAG);
    }

    #[test]
    fn dead_placeholder_geometry_keeps_upstream_proportions() {
        let glyph = dead_placeholder_geometry(10.0, 20.0, 100.0, 60.0).unwrap();

        let side = 60.0 * 0.42;
        assert!((glyph.right - glyph.left - side).abs() < 1e-4);
        assert!((glyph.bottom - glyph.top - side).abs() < 1e-4);
        assert!((glyph.fold - side * 0.28).abs() < 1e-4);
        assert!((glyph.stroke_width - (side * 0.07).clamp(1.0, 3.0)).abs() < 1e-4);
        assert!(((glyph.left + glyph.right) / 2.0 - 60.0).abs() < 1e-4);
        assert!(((glyph.top + glyph.bottom) / 2.0 - 50.0).abs() < 1e-4);
    }

    #[test]
    fn dead_placeholder_geometry_skips_tiny_cells() {
        assert!(dead_placeholder_geometry(0.0, 0.0, 16.0, 16.0).is_none());
        assert!(dead_placeholder_geometry(0.0, 0.0, 0.0, 0.0).is_none());
        assert!(dead_placeholder_geometry(0.0, 0.0, f32::NAN, 10.0).is_none());
    }

    #[test]
    fn disk_dilation_keeps_thin_features_connected_without_scalloping() {
        let width = 11_u32;
        let height = 17_u32;
        let radius = 4_i32;
        let line_x = 5_i32;
        let line_rows = 4_i32..13_i32;
        let mut mask = vec![0_u8; (width * height) as usize];
        for y in line_rows.clone() {
            mask[(y * width as i32 + line_x) as usize] = 255;
        }

        let dilated = dilate_disk_max(&mask, width, height, radius);

        for y in 0..height as i32 {
            for x in 0..width as i32 {
                let within_disk = line_rows.clone().any(|line_y| {
                    let dx = x - line_x;
                    let dy = y - line_y;
                    dx * dx + dy * dy <= radius * radius
                });
                let alpha = dilated[(y * width as i32 + x) as usize];
                assert_eq!(
                    alpha == 255,
                    within_disk,
                    "unexpected outline coverage at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn hit_testing_returns_addon_path_and_title() {
        let layout = fixture_layout(300.0, 180.0);
        let leaf = layout
            .leaf_rects()
            .into_iter()
            .find(|leaf| leaf.addon.title == "Map C")
            .unwrap();
        let hit = layout
            .hit_test_addon(
                leaf.rect.x + leaf.rect.width / 2.0,
                leaf.rect.y + leaf.rect.height / 2.0,
            )
            .unwrap();

        assert_eq!(hit.addon.title, "Map C");
        assert_eq!(hit.addon.path, PathBuf::from("map-c.gma"));
        assert_eq!(hit.tag, "map");
        assert_eq!(hit.addon.file_size_bytes, 75);
    }

    fn fixture_layout(width: f64, height: f64) -> TreemapLayout {
        analyze_addons(
            vec![
                addon("tool-a.gma", "Tool A", "tool", 200),
                addon("weapon-b.gma", "Weapon B", "weapon", 100),
                addon("map-c.gma", "Map C", "map", 75),
            ],
            TreemapBounds::new(width, height),
        )
        .unwrap()
    }

    fn addon(path: &str, title: &str, addon_type: &str, size: u64) -> SizeAnalyzerAddon {
        addon_with_workshop(path, title, addon_type, size, None)
    }

    fn addon_with_workshop(
        path: &str,
        title: &str,
        addon_type: &str,
        size: u64,
        workshop_id: Option<u64>,
    ) -> SizeAnalyzerAddon {
        SizeAnalyzerAddon::new(
            PathBuf::from(path),
            workshop_id.and_then(PublishedFileId::new),
            title,
            Some(addon_type.to_owned()),
            Vec::new(),
            size,
        )
    }
}
