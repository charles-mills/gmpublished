use iced::font::Weight;
use iced::mouse;
use iced::widget::{
    Space, canvas, column, container, mouse_area, progress_bar, row, sensor, stack, svg, text,
};
use iced::{
    Background, Border, Center, Color, Element, Font, Length, Point, Radians, Rectangle, Renderer,
    Shadow, Size, Theme,
};

use crate::bridge::size_analyzer::{
    Rect as TreemapRect, TreemapBounds, TreemapLayout, TreemapSquareData,
};
use crate::format::format_bytes;
use crate::media::size_analyzer_render::{
    ADDON_PLACEHOLDER, BACKGROUND, DEAD_GLYPH, RgbaColor, SizeAnalyzerLabelSprite,
    dead_placeholder_geometry, tag_color,
};
use crate::media::text_measure;
use crate::widgets::context_area::context_area;
use crate::widgets::tag_chip::{TAG_POINT_WIDTH, TAG_TEXT_SIZE, tag_chip};
use crate::{
    assets,
    i18n::I18n,
    theme::{self, Tokens, ViewCtx},
};

use super::state::{HoverProbe, LoadStatus, visible_tag_labels};
use super::{Message, State};

const PAGE_PADDING: f32 = 24.0;
const HOVER_BORDER_WIDTH: f32 = 4.0;

/// Height of the tooltip arrow triangle; also the gap the panel keeps from
/// the hovered square's edge so the arrow tip touches it.
const TOOLTIP_ARROW_H: f32 = 8.0;
/// Full width of the arrow triangle svg (16x8).
const TOOLTIP_ARROW_W: f32 = 16.0;
/// Inset the panel keeps from the surface edges when clamped.
const TOOLTIP_MARGIN: f32 = 8.0;
/// Hard cap on panel width (tippy's default max-width).
const TOOLTIP_MAX_WIDTH: f32 = 350.0;
/// Grid gap between the label and value columns / between rows (.5rem).
const TOOLTIP_GRID_GAP: f32 = 8.0;
/// Panel inner padding, matching `theme::styles::tooltip` usage.
const TOOLTIP_PAD_Y: f32 = 7.0;
const TOOLTIP_PAD_X: f32 = 10.0;
const TOOLTIP_TEXT_SIZE: f32 = 13.0;
/// Line-height multiple used for wrapped-row height estimates.
const TOOLTIP_LINE_HEIGHT: f32 = 1.3;
/// Bold labels are measured with the Regular face; nudge up to cover the
/// heavier weight (labels are three short words).
const TOOLTIP_BOLD_FUDGE: f32 = 1.05;

/// Renders the Size Analyzer route surface.
pub fn view<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let viewport = sensor(treemap_surface(state, ctx)).on_resize(Message::ViewportResized);

    container(viewport)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(PAGE_PADDING)
        .style(move |_| theme::styles::surface(&tokens))
        .into()
}

fn treemap_surface<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let mut layers = stack![base_surface(state, ctx)]
        .width(Length::Fill)
        .height(Length::Fill);

    if matches!(state.load_status(), LoadStatus::Ready)
        && let Some(hover) = state.hover()
    {
        layers = layers.push(hover_outline(hover));
        if let Some(surface) = state.surface_size() {
            layers = layers.push(hover_tooltip(hover, ctx, surface));
        }
    }

    if matches!(state.load_status(), LoadStatus::Loading) {
        layers = layers.push(loading_overlay(ctx));
    }

    if matches!(
        state.load_status(),
        LoadStatus::Error(_) | LoadStatus::Empty
    ) {
        layers = layers.push(error_overlay(state, ctx));
    }

    container(layers)
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true)
        .style(move |_| theme::styles::surface(&tokens))
        .into()
}

fn base_surface<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    if state.layout().is_some() {
        let treemap = canvas(TreemapProgram { state })
            .width(Length::Fill)
            .height(Length::Fill);
        return if matches!(state.load_status(), LoadStatus::Ready) {
            context_area(
                mouse_area(treemap)
                    .on_move(Message::HoverMoved)
                    .on_exit(Message::HoverExited)
                    .on_press(Message::TreemapPressed)
                    .on_release(Message::TreemapReleased),
                Message::TreemapRightPressed,
            )
            .into()
        } else {
            treemap.into()
        };
    }

    if matches!(state.load_status(), LoadStatus::Loading) {
        return Space::new().width(Length::Fill).height(Length::Fill).into();
    }

    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    container(
        text(empty_surface_text(state, i18n))
            .size(tokens.typography.body)
            .color(Color::from(tokens.colors.text_dim)),
    )
    .center(Length::Fill)
    .into()
}

/// Purely visual treemap canvas: three cached layers replayed until state
/// invalidates them. Interaction stays on the wrapping `mouse_area`, so the
/// program handles no events and returns no messages.
struct TreemapProgram<'a> {
    state: &'a State,
}

impl canvas::Program<Message> for TreemapProgram<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let Some(layout) = self.state.layout() else {
            return Vec::new();
        };

        let size = bounds.size();
        let scale = SurfaceScale::new(layout.bounds, size);
        // Drain any coalesced thumbnail arrivals so a burst of deliveries
        // clears the cache once, right before it re-records below.
        let _invalidation = self.state.flush_thumbnail_invalidation();
        let layers = self.state.layers();

        let geometry = layers.geometry().draw(renderer, size, |frame| {
            draw_geometry_layer(frame, layout, scale);
        });
        let thumbnails = layers.thumbnails().draw(renderer, size, |frame| {
            draw_thumbnail_layer(frame, layout, self.state, scale);
        });
        let labels = layers.labels().draw(renderer, size, |frame| {
            draw_label_layer(frame, self.state, scale);
        });

        vec![geometry, thumbnails, labels]
    }
}

/// Layout→canvas scale per axis: identity once analysis matches the
/// viewport, with a fill-stretch fallback if Iced draws against a bounds
/// sample that differs from the last measured viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SurfaceScale {
    x: f32,
    y: f32,
}

impl SurfaceScale {
    fn new(bounds: TreemapBounds, size: Size) -> Self {
        Self {
            x: axis_scale(size.width, bounds.width),
            y: axis_scale(size.height, bounds.height),
        }
    }

    fn rect(self, rect: TreemapRect) -> Rectangle {
        Rectangle {
            x: rect.x as f32 * self.x,
            y: rect.y as f32 * self.y,
            width: rect.width as f32 * self.x,
            height: rect.height as f32 * self.y,
        }
    }
}

fn axis_scale(output: f32, layout: f64) -> f32 {
    let layout = layout as f32;
    if layout.is_finite() && layout > 0.0 {
        output / layout
    } else {
        1.0
    }
}

fn draw_geometry_layer(frame: &mut canvas::Frame, layout: &TreemapLayout, scale: SurfaceScale) {
    frame.fill_rectangle(Point::ORIGIN, frame.size(), analyzer_color(BACKGROUND));

    for square in &layout.squares {
        let TreemapSquareData::Tag { tag, .. } = &square.data else {
            continue;
        };
        let rect = scale.rect(TreemapRect {
            x: square.x,
            y: square.y,
            width: square.width,
            height: square.height,
        });
        fill_rect(frame, rect, tag_color(tag));
    }

    for leaf in layout.leaf_rects() {
        fill_rect(frame, scale.rect(leaf.rect), ADDON_PLACEHOLDER);
    }
}

fn draw_thumbnail_layer(
    frame: &mut canvas::Frame,
    layout: &TreemapLayout,
    state: &State,
    scale: SurfaceScale,
) {
    for leaf in layout.leaf_rects() {
        let workshop_id = leaf.addon.workshop_id;
        let cell = scale.rect(leaf.rect);
        let tile = workshop_id.and_then(|id| state.tile_for(id));
        if let Some(tile) = tile {
            let Some(cover) = cover_fit_bounds(cell, tile.width, tile.height) else {
                continue;
            };
            frame.with_clip(cell, |clipped| {
                clipped.draw_image(cover, &tile.handle);
            });
        } else if !state.thumbnail_pending(workshop_id) {
            // Failed or never-deliverable (local addon, no preview URL):
            // draw the dead-file glyph.
            draw_dead_placeholder(frame, cell);
        }
    }
}

fn draw_label_layer(frame: &mut canvas::Frame, state: &State, scale: SurfaceScale) {
    for sprite in visible_tag_labels(state.labels(), state.hidden_tag()) {
        draw_label_sprite(frame, sprite, scale);
    }
}

fn draw_label_sprite(
    frame: &mut canvas::Frame,
    sprite: &SizeAnalyzerLabelSprite,
    scale: SurfaceScale,
) {
    let Some(bounds) = label_sprite_bounds(sprite, scale) else {
        return;
    };
    let image = canvas::Image::new(&sprite.handle);
    if sprite.vertical {
        frame.draw_image(bounds, image.rotation(Radians(std::f32::consts::FRAC_PI_2)));
    } else {
        frame.draw_image(bounds, image);
    }
}

/// Bounds that draw the physical label bitmap at logical size, centered in
/// its tag region and snapped to the physical pixel grid for crispness.
/// Vertical sprites get unrotated (horizontal) bounds; the 90° clockwise
/// rotation happens on the image around the bounds' center.
fn label_sprite_bounds(sprite: &SizeAnalyzerLabelSprite, scale: SurfaceScale) -> Option<Rectangle> {
    if !sprite.scale.is_finite() || sprite.scale <= 0.0 {
        return None;
    }

    // Pre-rotation bitmap axes map to swapped visual axes for vertical
    // sprites, so the fill-stretch factors swap with them.
    let (stretch_x, stretch_y) = if sprite.vertical {
        (scale.y, scale.x)
    } else {
        (scale.x, scale.y)
    };
    let width = sprite.width as f32 / sprite.scale * stretch_x;
    let height = sprite.height as f32 / sprite.scale * stretch_y;
    let region = scale.rect(sprite.rect);
    let center_x = region.x + region.width / 2.0;
    let center_y = region.y + region.height / 2.0;

    Some(Rectangle {
        x: snap_to_physical(center_x - width / 2.0, sprite.scale),
        y: snap_to_physical(center_y - height / 2.0, sprite.scale),
        width,
        height,
    })
}

fn snap_to_physical(value: f32, scale: f32) -> f32 {
    (value * scale).round() / scale
}

/// Cover-fit: uniform scale `max(cell.w/src.w, cell.h/src.h)`, centered on
/// the cell; the caller clips the overflow to the cell.
fn cover_fit_bounds(cell: Rectangle, source_width: u32, source_height: u32) -> Option<Rectangle> {
    if cell.width <= 0.0 || cell.height <= 0.0 || source_width == 0 || source_height == 0 {
        return None;
    }

    let source_width = source_width as f32;
    let source_height = source_height as f32;
    let scale = (cell.width / source_width).max(cell.height / source_height);
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }

    let width = source_width * scale;
    let height = source_height * scale;
    Some(Rectangle {
        x: cell.x + (cell.width - width) / 2.0,
        y: cell.y + (cell.height - height) / 2.0,
        width,
        height,
    })
}

fn fill_rect(frame: &mut canvas::Frame, rect: Rectangle, color: RgbaColor) {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    frame.fill_rectangle(rect.position(), rect.size(), analyzer_color(color));
}

fn draw_dead_placeholder(frame: &mut canvas::Frame, cell: Rectangle) {
    let Some(glyph) = dead_placeholder_geometry(cell.x, cell.y, cell.width, cell.height) else {
        return;
    };

    let path = canvas::Path::new(|builder| {
        builder.move_to(Point::new(glyph.left, glyph.top));
        builder.line_to(Point::new(glyph.right - glyph.fold, glyph.top));
        builder.line_to(Point::new(glyph.right, glyph.top + glyph.fold));
        builder.line_to(Point::new(glyph.right, glyph.bottom));
        builder.line_to(Point::new(glyph.left, glyph.bottom));
        builder.close();
        builder.move_to(Point::new(glyph.right - glyph.fold, glyph.top));
        builder.line_to(Point::new(glyph.right - glyph.fold, glyph.top + glyph.fold));
        builder.line_to(Point::new(glyph.right, glyph.top + glyph.fold));
    });

    frame.stroke(
        &path,
        canvas::Stroke::default()
            .with_color(analyzer_color(DEAD_GLYPH))
            .with_width(glyph.stroke_width),
    );
}

fn hover_outline<'a>(hover: &HoverProbe) -> Element<'a, Message> {
    let color = analyzer_color(hover.color());
    // The container is sized to exactly the hovered square and iced strokes
    // borders inward from the bounds, so this 4px ring reads *inside* the
    // square edges.
    // Kept instant (no grow/shrink animation) to preserve the idle-0% rule;
    // upstream's .1s transition is a sanctioned deviation.
    let outline = container(Space::new().width(Length::Fill).height(Length::Fill))
        .width(Length::Fixed(hover.rect_width().max(1.0)))
        .height(Length::Fixed(hover.rect_height().max(1.0)))
        .style(move |_| container::Style {
            border: Border {
                color,
                width: HOVER_BORDER_WIDTH,
                radius: 0.0.into(),
            },
            ..container::Style::default()
        });

    column![
        Space::new().height(Length::Fixed(hover.rect_y().max(0.0))),
        row![
            Space::new().width(Length::Fixed(hover.rect_x().max(0.0))),
            outline
        ]
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Placement of the anchored tooltip within the treemap surface: the panel's
/// top-left corner, its size, and whether it was flipped below the square.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TooltipPlacement {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    below: bool,
}

/// Positions the tooltip panel over the treemap surface: horizontally
/// centered on the hovered square and clamped inside the surface margins,
/// sitting above the square with the arrow tip touching its top edge — and
/// flipping below when there is no room above.
fn place_tooltip(square: Rectangle, panel: Size, surface: Size) -> TooltipPlacement {
    let max_x = (surface.width - panel.width - TOOLTIP_MARGIN).max(TOOLTIP_MARGIN);
    let x = (square.center_x() - panel.width / 2.0).clamp(TOOLTIP_MARGIN, max_x);

    let above_y = square.y - TOOLTIP_ARROW_H - panel.height;
    let (y, below) = if above_y < TOOLTIP_MARGIN {
        (square.y + square.height + TOOLTIP_ARROW_H, true)
    } else {
        (above_y, false)
    };

    TooltipPlacement {
        x,
        y,
        width: panel.width,
        height: panel.height,
        below,
    }
}

/// Arrow's left offset within the surface: centered on the square, clamped so
/// it stays inside the panel's horizontal span (minus its own half-width) and
/// never detaches when the panel is edge-clamped.
fn tooltip_arrow_x(square: Rectangle, placement: TooltipPlacement) -> f32 {
    let min = placement.x;
    let max = (placement.x + placement.width - TOOLTIP_ARROW_W).max(min);
    (square.center_x() - TOOLTIP_ARROW_W / 2.0).clamp(min, max)
}

fn hover_tooltip<'a>(
    hover: &'a HoverProbe,
    ctx: ViewCtx<'a>,
    surface: Size,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let title = hover.title();
    let tag = hover.tag();
    let size_text = format_bytes(hover.size_bytes(), i18n);

    let name_label = i18n.tr("size-analyzer-name");
    let type_label = i18n.tr("size-analyzer-type");
    let size_label = i18n.tr("size-analyzer-size");

    let label_w = label_column_width(&name_label, &type_label, &size_label);
    let panel_size = tooltip_panel_size(title, tag, &size_text, label_w, &tokens);

    let square = Rectangle {
        x: hover.rect_x(),
        y: hover.rect_y(),
        width: hover.rect_width(),
        height: hover.rect_height(),
    };
    let placement = place_tooltip(square, panel_size, surface);
    let arrow_x = tooltip_arrow_x(square, placement);

    let value_width = value_column_width(panel_size.width, label_w);

    let grid = column![
        tooltip_row(
            &name_label,
            tooltip_title_value(title, value_width, &tokens)
        ),
        tooltip_row(&type_label, tag_chip::<Message>(tag, &tokens)),
        tooltip_row(&size_label, tooltip_plain_value(size_text, &tokens)),
    ]
    .spacing(TOOLTIP_GRID_GAP);

    let panel = container(grid)
        .padding([TOOLTIP_PAD_Y, TOOLTIP_PAD_X])
        .width(Length::Fixed(panel_size.width))
        .style(move |_| theme::styles::tooltip(&tokens));

    let arrow_color = tokens.colors.tooltip_bg.into();
    let arrow = svg(assets::icons::tooltip_arrow())
        .width(Length::Fixed(TOOLTIP_ARROW_W))
        .height(Length::Fixed(TOOLTIP_ARROW_H))
        .rotation(if placement.below {
            Radians(std::f32::consts::PI)
        } else {
            Radians(0.0)
        })
        .style(move |_, _| svg::Style {
            color: Some(arrow_color),
        });

    // The arrow sits against the square edge: below the panel when the
    // tooltip is above, above the panel when it flips below.
    let stack_column = if placement.below {
        column![
            positioned_row(arrow_x, arrow),
            positioned_row(placement.x, panel)
        ]
    } else {
        column![
            positioned_row(placement.x, panel),
            positioned_row(arrow_x, arrow)
        ]
    };

    let top_offset = if placement.below {
        (placement.y - TOOLTIP_ARROW_H).max(0.0)
    } else {
        placement.y.max(0.0)
    };

    column![Space::new().height(Length::Fixed(top_offset)), stack_column,]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Wraps a child in a leading horizontal spacer so it sits at `x` within the
/// full-width surface (the `hover_outline` positioning technique).
fn positioned_row<'a>(x: f32, child: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![Space::new().width(Length::Fixed(x.max(0.0))), child.into()]
        .width(Length::Fill)
        .into()
}

fn tooltip_row<'a>(label: &str, value: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![
        text(label.to_owned())
            .size(TOOLTIP_TEXT_SIZE)
            .font(bold_font()),
        value.into(),
    ]
    .spacing(TOOLTIP_GRID_GAP)
    .align_y(iced::Alignment::Center)
    .into()
}

fn tooltip_title_value<'a>(
    title: &'a str,
    value_width: f32,
    tokens: &Tokens,
) -> Element<'a, Message> {
    text(title)
        .size(TOOLTIP_TEXT_SIZE)
        .color(Color::from(tokens.colors.text))
        .wrapping(text::Wrapping::WordOrGlyph)
        .width(Length::Fixed(value_width))
        .into()
}

fn tooltip_plain_value<'a>(value: String, tokens: &Tokens) -> Element<'a, Message> {
    text(value)
        .size(TOOLTIP_TEXT_SIZE)
        .color(Color::from(tokens.colors.text))
        .into()
}

fn bold_font() -> Font {
    Font {
        weight: Weight::Bold,
        ..assets::fonts::default_font()
    }
}

/// Bold label-column width: the widest of the three labels measured with the
/// Regular face and fudged up for the heavier Bold weight.
fn label_column_width(name_label: &str, type_label: &str, size_label: &str) -> f32 {
    [name_label, type_label, size_label]
        .into_iter()
        .map(|label| text_measure::measure_width(label, TOOLTIP_TEXT_SIZE) * TOOLTIP_BOLD_FUDGE)
        .fold(0.0, f32::max)
}

/// Rendered width of the flag chip value: lowercased label at the chip's font
/// size + the chip body's horizontal padding + the trailing point svg
/// (reusing the chip's own constants).
fn chip_value_width(tag: &str, tokens: &Tokens) -> f32 {
    let label = tag.to_ascii_lowercase();
    text_measure::measure_width(&label, TAG_TEXT_SIZE)
        + tokens.spacing.pad_xs * 2.0
        + TAG_POINT_WIDTH
}

/// Value-column width once the panel width is fixed: the panel's inner width
/// minus the label column and the grid gap. At least 1.
fn value_column_width(panel_width: f32, label_w: f32) -> f32 {
    (panel_width - TOOLTIP_PAD_X * 2.0 - label_w - TOOLTIP_GRID_GAP).max(1.0)
}

/// Computes the panel size before layout: width capped at `TOOLTIP_MAX_WIDTH`,
/// height = padding + three rows (the title row wraps at the clamped value
/// column width) + two inter-row gaps.
fn tooltip_panel_size(
    title: &str,
    tag: &str,
    size_text: &str,
    label_w: f32,
    tokens: &Tokens,
) -> Size {
    let title_w = text_measure::measure_width(title, TOOLTIP_TEXT_SIZE);
    let type_w = chip_value_width(tag, tokens);
    let size_w = text_measure::measure_width(size_text, TOOLTIP_TEXT_SIZE);
    let widest_value = title_w.max(type_w).max(size_w);

    let content_w = label_w + TOOLTIP_GRID_GAP + widest_value;
    let width = (content_w + TOOLTIP_PAD_X * 2.0).min(TOOLTIP_MAX_WIDTH);

    // The title wraps at whatever the value column collapses to once width is
    // capped; the other two rows are single-line.
    let value_col_w = value_column_width(width, label_w);
    let title_lines =
        text_measure::wrapped_line_count(title, TOOLTIP_TEXT_SIZE, value_col_w).max(1);

    let row_h = TOOLTIP_TEXT_SIZE * TOOLTIP_LINE_HEIGHT;
    let title_row_h = row_h * title_lines as f32;
    // The type row renders at the chip's fixed height, which exceeds one
    // text line; underestimating it pushes the arrow into the square.
    let type_row_h = row_h.max(tokens.dims.tag_height);
    let height = TOOLTIP_PAD_Y * 2.0 + title_row_h + type_row_h + row_h + TOOLTIP_GRID_GAP * 2.0;

    Size::new(width, height)
}

fn loading_overlay(ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let panel = container(
        column![
            progress_bar(0.0..=100.0, 0.0)
                .girth(10.0)
                .style(move |_| theme::styles::progress_bar(&tokens)),
            text(i18n.tr("size-analyzer-loading"))
                .size(tokens.typography.body)
                .color(Color::from(tokens.colors.text)),
        ]
        .width(Length::Fill)
        .spacing(tokens.spacing.gap_sm),
    )
    .width(Length::Fill)
    .max_width(360.0)
    .padding(tokens.spacing.pad)
    .style(move |_| loading_panel_style(&tokens));

    container(panel)
        .width(Length::Fill)
        .height(Length::Fill)
        .center(Length::Fill)
        .style(move |_| overlay_background_style(&tokens))
        .into()
}

fn error_overlay<'a>(state: &State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let message = empty_surface_text(state, i18n);
    container(
        column![
            svg(assets::icons::dead())
                .width(32.0)
                .height(32.0)
                .style(move |_, _| svg::Style {
                    color: Some(tokens.colors.text.into()),
                }),
            text(message)
                .size(tokens.typography.body)
                .color(Color::from(tokens.colors.text)),
        ]
        .spacing(tokens.spacing.gap_sm)
        .align_x(Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .center(Length::Fill)
    .style(move |_| theme::styles::surface(&tokens))
    .into()
}

fn empty_surface_text(state: &State, i18n: &I18n) -> String {
    match state.load_status() {
        LoadStatus::Idle | LoadStatus::WaitingForViewport => i18n.tr("size-analyzer-waiting"),
        LoadStatus::Loading => i18n.tr("size-analyzer-loading"),
        LoadStatus::Ready => String::new(),
        LoadStatus::Empty => i18n.tr("size-analyzer-empty"),
        LoadStatus::Error(error) => i18n.trn("size-analyzer-error", &[("arg0", error.as_str())]),
    }
}

fn analyzer_color(color: RgbaColor) -> Color {
    Color::from_rgba8(
        color.red,
        color.green,
        color.blue,
        f32::from(color.alpha) / 255.0,
    )
}

fn overlay_background_style(tokens: &Tokens) -> container::Style {
    container::Style {
        background: Some(Background::Color(tokens.colors.scrim_soft.into())),
        ..container::Style::default()
    }
}

fn loading_panel_style(tokens: &Tokens) -> container::Style {
    container::Style {
        background: Some(Background::Color(tokens.colors.scrim.into())),
        text_color: Some(tokens.colors.text.into()),
        border: Border {
            radius: 0.0.into(),
            ..Border::default()
        },
        shadow: Shadow::default(),
        ..container::Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rectangle {
        Rectangle {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn tooltip_sits_centered_above_a_mid_surface_square() {
        let square = rect(400.0, 200.0, 40.0, 40.0);
        let placement = place_tooltip(square, Size::new(100.0, 60.0), Size::new(1000.0, 600.0));

        // Centered on center_x (420) → x = 420 - 50; above the square with the
        // arrow gap → y = 200 - ARROW_H - panel.height.
        assert!((placement.x - 370.0).abs() < 1e-4);
        assert!((placement.y - 132.0).abs() < 1e-4);
        assert!(!placement.below);
    }

    #[test]
    fn tooltip_clamps_to_the_left_margin() {
        let square = rect(0.0, 200.0, 40.0, 40.0);
        let placement = place_tooltip(square, Size::new(100.0, 60.0), Size::new(1000.0, 600.0));

        assert!((placement.x - TOOLTIP_MARGIN).abs() < 1e-4);
    }

    #[test]
    fn tooltip_clamps_to_the_right_margin() {
        let square = rect(990.0, 200.0, 10.0, 40.0);
        let placement = place_tooltip(square, Size::new(100.0, 60.0), Size::new(1000.0, 600.0));

        // max_x = surface.width - panel.width - MARGIN = 892.
        assert!((placement.x - 892.0).abs() < 1e-4);
    }

    #[test]
    fn tooltip_flips_below_when_there_is_no_room_above() {
        let square = rect(400.0, 4.0, 40.0, 40.0);
        let placement = place_tooltip(square, Size::new(100.0, 60.0), Size::new(1000.0, 600.0));

        assert!(placement.below);
        // Below: y = square.bottom + ARROW_H = 4 + 40 + 8.
        assert!((placement.y - 52.0).abs() < 1e-4);
    }

    #[test]
    fn arrow_stays_centered_on_the_square_within_the_panel() {
        let square = rect(400.0, 200.0, 40.0, 40.0);
        let placement = place_tooltip(square, Size::new(100.0, 60.0), Size::new(1000.0, 600.0));
        let arrow_x = tooltip_arrow_x(square, placement);

        // Centered on center_x (420) minus half the arrow width (8) = 412,
        // comfortably inside the panel span [370, 454].
        assert!((arrow_x - 412.0).abs() < 1e-4);
    }

    #[test]
    fn arrow_clamps_inside_the_panel_when_the_panel_is_edge_clamped() {
        let square = rect(990.0, 200.0, 10.0, 40.0);
        let placement = place_tooltip(square, Size::new(100.0, 60.0), Size::new(1000.0, 600.0));
        let arrow_x = tooltip_arrow_x(square, placement);

        assert!(arrow_x >= placement.x - 1e-4);
        assert!(arrow_x <= placement.x + placement.width - TOOLTIP_ARROW_W + 1e-4);
        // Panel clamped to x=892; arrow must stay within [892, 892+100-16=976].
        assert!((arrow_x - 976.0).abs() < 1e-4);
    }

    #[test]
    fn cover_fit_scales_uniformly_to_the_dominant_axis_and_centers() {
        // Wide source into a square cell: height dominates, width overflows.
        let cover = cover_fit_bounds(rect(10.0, 20.0, 100.0, 100.0), 200, 100).unwrap();

        assert!((cover.height - 100.0).abs() < 1e-4);
        assert!((cover.width - 200.0).abs() < 1e-4);
        // Centered: overflow splits evenly on both sides.
        assert!((cover.x - (10.0 - 50.0)).abs() < 1e-4);
        assert!((cover.y - 20.0).abs() < 1e-4);
    }

    #[test]
    fn cover_fit_of_tall_source_overflows_vertically() {
        let cover = cover_fit_bounds(rect(0.0, 0.0, 100.0, 50.0), 100, 200).unwrap();

        assert!((cover.width - 100.0).abs() < 1e-4);
        assert!((cover.height - 200.0).abs() < 1e-4);
        assert!((cover.y - (0.0 - 75.0)).abs() < 1e-4);
    }

    #[test]
    fn cover_fit_of_matching_aspect_fills_the_cell_exactly() {
        let cell = rect(5.0, 5.0, 128.0, 64.0);
        let cover = cover_fit_bounds(cell, 256, 128).unwrap();

        assert!((cover.x - cell.x).abs() < 1e-4);
        assert!((cover.y - cell.y).abs() < 1e-4);
        assert!((cover.width - cell.width).abs() < 1e-4);
        assert!((cover.height - cell.height).abs() < 1e-4);
    }

    #[test]
    fn cover_fit_rejects_degenerate_inputs() {
        assert!(cover_fit_bounds(rect(0.0, 0.0, 0.0, 10.0), 8, 8).is_none());
        assert!(cover_fit_bounds(rect(0.0, 0.0, 10.0, 10.0), 0, 8).is_none());
        assert!(cover_fit_bounds(rect(0.0, 0.0, 10.0, 10.0), 8, 0).is_none());
    }

    #[test]
    fn surface_scale_is_identity_when_layout_matches_viewport() {
        let scale = SurfaceScale::new(TreemapBounds::new(640.0, 360.0), Size::new(640.0, 360.0));

        assert!((scale.x - 1.0).abs() < 1e-6);
        assert!((scale.y - 1.0).abs() < 1e-6);
    }

    #[test]
    fn surface_scale_stretches_when_draw_bounds_differ_from_layout() {
        let scale = SurfaceScale::new(TreemapBounds::new(640.0, 360.0), Size::new(1280.0, 360.0));

        assert!((scale.x - 2.0).abs() < 1e-6);
        assert!((scale.y - 1.0).abs() < 1e-6);

        let scaled = scale.rect(TreemapRect {
            x: 10.0,
            y: 10.0,
            width: 50.0,
            height: 50.0,
        });
        assert!((scaled.x - 20.0).abs() < 1e-4);
        assert!((scaled.width - 100.0).abs() < 1e-4);
        assert!((scaled.y - 10.0).abs() < 1e-4);
    }

    #[test]
    fn label_sprite_bounds_draw_bitmaps_at_logical_size_centered() {
        let sprite = test_sprite(64, 32, 2.0, false);
        let scale = SurfaceScale::new(TreemapBounds::new(200.0, 100.0), Size::new(200.0, 100.0));

        let bounds = label_sprite_bounds(&sprite, scale).unwrap();

        // 64x32 physical at 2.0 → 32x16 logical, centered in the 100x50 rect
        // at (10, 10) and snapped to the half-pixel physical grid.
        assert!((bounds.width - 32.0).abs() < 1e-4);
        assert!((bounds.height - 16.0).abs() < 1e-4);
        assert!((bounds.x - 44.0).abs() <= 0.25);
        assert!((bounds.y - 27.0).abs() <= 0.25);
    }

    #[test]
    fn vertical_label_bounds_stay_unrotated_for_image_rotation() {
        let sprite = test_sprite(64, 32, 1.0, true);
        let scale = SurfaceScale::new(TreemapBounds::new(200.0, 100.0), Size::new(200.0, 100.0));

        let bounds = label_sprite_bounds(&sprite, scale).unwrap();

        // Bounds keep the bitmap's horizontal orientation; the canvas image
        // rotates it around the center at draw time.
        assert!((bounds.width - 64.0).abs() < 1e-4);
        assert!((bounds.height - 32.0).abs() < 1e-4);
        let center_x = bounds.x + bounds.width / 2.0;
        let center_y = bounds.y + bounds.height / 2.0;
        assert!((center_x - 60.0).abs() <= 0.5);
        assert!((center_y - 35.0).abs() <= 0.5);
    }

    fn test_sprite(width: u32, height: u32, scale: f32, vertical: bool) -> SizeAnalyzerLabelSprite {
        let pixels = vec![255_u8; (width * height * 4) as usize];
        SizeAnalyzerLabelSprite {
            text: "tool".to_owned(),
            rect: TreemapRect {
                x: 10.0,
                y: 10.0,
                width: 100.0,
                height: 50.0,
            },
            handle: iced::widget::image::Handle::from_rgba(width, height, pixels),
            width,
            height,
            scale,
            vertical,
        }
    }
}
