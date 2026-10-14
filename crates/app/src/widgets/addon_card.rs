use std::{cell::RefCell, collections::HashMap, panic::AssertUnwindSafe, time::Instant};

use iced::advanced::graphics::text::Paragraph as IcedParagraph;
use iced::advanced::text::Paragraph as _;
use iced::alignment;
use iced::animation::Easing;
use iced::widget::{Space, canvas, column, container, image, mouse_area, row, svg, text};
use iced::widget::{container as container_widget, text as text_widget};
use iced::{
    Background, Border, Color, ContentFit, Element, Length, Pixels, Point, Rectangle, Renderer,
    Shadow, Size, Theme, Vector, mouse,
};

use crate::assets;
use crate::theme::{self, LayoutMetrics, Tokens, motion};
use crate::widgets::context_area::context_area;
use crate::widgets::download_count_icon::download_count_icon;
use crate::widgets::grid_rows::CardId;
use crate::widgets::star_rating::star_rating;

const PUBLISH_NEW_FALLBACK: &str = "Publish new";
const DOWNLOAD_ICON_SIZE: f32 = 16.0;
const DEAD_GLYPH_SIZE: f32 = 64.0;
const LOADING_GLYPH_SIZE: f32 = 32.0;
const STATS_LINE_HEIGHT: f32 = 1.3;
const TITLE_LINE_HEIGHT: f32 = 1.12;
const MAX_TITLE_LINES: usize = 3;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    Addon,
    PublishNew,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Thumbnail {
    Loading,
    Dead,
    Placeholder(image::Handle),
    Ready(image::Handle),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubscriptionRoll {
    pub from: String,
    pub to: String,
    pub progress: f32,
    pub up: bool,
}

#[derive(Clone, Debug)]
pub struct Data {
    id: CardId,
    kind: Kind,
    title: String,
    subscriptions: String,
    subscription_count: u64,
    subscription_roll: Option<SubscriptionRoll>,
    score_bucket: i32,
    score_label: String,
    thumbnail: Thumbnail,
    enabled: bool,
    hovered: bool,
    hover_motion: motion::Presence<bool>,
    reveal_motion: motion::Presence<bool>,
    reveal_under: Option<Thumbnail>,
    /// Remember displayed pixels after releasing off-screen handles so returning cards do
    /// not fade in again.
    has_shown_pixels: bool,
}

impl PartialEq for Data {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.kind == other.kind
            && self.title == other.title
            && self.subscriptions == other.subscriptions
            && self.subscription_count == other.subscription_count
            && self.subscription_roll == other.subscription_roll
            && self.score_bucket == other.score_bucket
            && self.score_label == other.score_label
            && self.thumbnail == other.thumbnail
            && self.enabled == other.enabled
            && self.hovered == other.hovered
            && self.hover_motion == other.hover_motion
            && self.reveal_motion == other.reveal_motion
            && self.reveal_under == other.reveal_under
        // Ignore arrival history when comparing cards that draw the same frame.
    }
}

impl Data {
    pub(crate) fn addon(id: CardId, title: impl Into<String>) -> Self {
        Self {
            id,
            kind: Kind::Addon,
            title: title.into(),
            subscriptions: "0".to_owned(),
            subscription_count: 0,
            subscription_roll: None,
            score_bucket: 0,
            score_label: String::new(),
            thumbnail: Thumbnail::Loading,
            enabled: true,
            hovered: false,
            hover_motion: hover_animation(false),
            reveal_motion: reveal_animation(true),
            reveal_under: None,
            has_shown_pixels: false,
        }
    }

    pub(crate) fn publish_new(id: CardId, title: impl Into<String>) -> Self {
        Self {
            kind: Kind::PublishNew,
            ..Self::addon(id, title)
        }
    }

    pub(crate) const fn id(&self) -> &CardId {
        &self.id
    }

    pub(crate) fn display_title(&self) -> &str {
        if self.kind == Kind::PublishNew && self.title.is_empty() {
            PUBLISH_NEW_FALLBACK
        } else {
            &self.title
        }
    }

    #[cfg(test)]
    pub(crate) const fn is_hovered(&self) -> bool {
        self.hovered
    }

    pub(crate) const fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn set_hovered_at(&mut self, hovered: bool, now: Instant) {
        if self.hovered == hovered {
            return;
        }

        self.hovered = hovered;
        self.hover_motion.go(hovered, now);
    }

    pub(crate) fn needs_motion_ticks(&self) -> bool {
        (self.enabled && self.hover_motion.needs_ticks()) || self.reveal_motion.needs_ticks()
    }

    pub(crate) fn finish_motion(&mut self) {
        self.hover_motion.snap(self.hovered);
        self.reveal_motion.snap(true);
        self.reveal_under = None;
    }

    pub(crate) fn tick_motion(&mut self, now: Instant) {
        self.hover_motion.tick(now);
        if self.reveal_motion.tick(now) {
            self.reveal_under = None;
        }
    }

    pub(crate) fn preserve_motion_from(&mut self, previous: &Self) {
        self.hovered = previous.hovered;
        self.hover_motion = previous.hover_motion.clone();
        self.reveal_motion = previous.reveal_motion.clone();
        self.reveal_under.clone_from(&previous.reveal_under);
        let had_pixels = previous.has_shown_pixels;
        self.has_shown_pixels |= had_pixels;
        self.reveal_on_arrival(&previous.thumbnail, had_pixels, Instant::now());
    }

    /// Do not restart the fade for GIF frames or previously displayed pixels.
    fn reveal_on_arrival(&mut self, previous: &Thumbnail, had_pixels: bool, now: Instant) {
        if !matches!(self.thumbnail, Thumbnail::Ready(_))
            || !matches!(previous, Thumbnail::Loading | Thumbnail::Placeholder(_))
        {
            return;
        }

        if had_pixels {
            // A reveal interrupted by the release still has to be retired, or
            // the returning pixels would resume a fade nobody is watching.
            self.reveal_motion = reveal_animation(true);
            self.reveal_under = None;
            return;
        }

        self.reveal_motion = reveal_animation(false);
        self.reveal_motion.go(true, now);
        self.reveal_under = Some(previous.clone());
    }

    fn reveal_progress(&self, now: Instant) -> f32 {
        self.reveal_motion.interpolate(0.0, 1.0, now)
    }

    pub(crate) fn with_subscriptions(mut self, label: impl Into<String>, count: u64) -> Self {
        let label = label.into();
        self.subscriptions = if label.is_empty() {
            count.to_string()
        } else {
            label
        };
        self.subscription_count = count;
        self
    }

    pub(crate) fn with_subscription_roll(mut self, roll: Option<SubscriptionRoll>) -> Self {
        self.subscription_roll = roll;
        self
    }

    pub(crate) fn with_score(mut self, bucket: i32, label: impl Into<String>) -> Self {
        self.score_bucket = bucket;
        self.score_label = label.into();
        self
    }

    pub(crate) fn with_thumbnail(mut self, thumbnail: Thumbnail) -> Self {
        self.has_shown_pixels = matches!(thumbnail, Thumbnail::Ready(_));
        self.thumbnail = thumbnail;
        self
    }

    pub(crate) fn set_thumbnail(&mut self, thumbnail: Thumbnail) {
        let had_pixels = self.has_shown_pixels;
        let previous = std::mem::replace(&mut self.thumbnail, thumbnail);
        self.has_shown_pixels |= matches!(self.thumbnail, Thumbnail::Ready(_));
        self.reveal_on_arrival(&previous, had_pixels, Instant::now());
    }

    pub(crate) const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    #[cfg(test)]
    pub(crate) fn subscriptions_label_for_test(&self) -> &str {
        self.subscriptions_label()
    }

    #[cfg(test)]
    pub(crate) const fn subscription_roll_for_test(&self) -> Option<&SubscriptionRoll> {
        self.subscription_roll.as_ref()
    }

    fn subscriptions_label(&self) -> &str {
        &self.subscriptions
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Pressed(CardId),
    Released(CardId),
    ContextRequested(CardId, Point),
}

#[cfg(test)]
pub fn preferred_height(data: &Data, width: f32, tokens: &LayoutMetrics) -> f32 {
    let measured_title_height = measure_title_height(data.display_title(), width, tokens);

    preferred_height_for_title_height(width, measured_title_height, tokens)
}

pub fn preferred_height_for_title_height(
    width: f32,
    title_height: f32,
    tokens: &LayoutMetrics,
) -> f32 {
    fixed_content_height(width, tokens) + clamp_title_height(title_height, tokens)
}

fn stats_height(tokens: &LayoutMetrics) -> f32 {
    tokens
        .dims
        .card_stats_height
        .max(tokens.typography.body * STATS_LINE_HEIGHT)
}

fn stacked_stats(width: f32, tokens: &LayoutMetrics) -> bool {
    // Reserve a useful count label before placing the rating beside it. The
    // same rule drives measured card height and the rendered arrangement.
    let count_width =
        tokens.typography.body * 6.0 + tokens.dims.star_rating_height * (DOWNLOAD_ICON_SIZE / 14.0);
    width < tokens.dims.star_rating_width + count_width + tokens.spacing.gap_sm
}

fn stats_block_height(width: f32, tokens: &LayoutMetrics) -> f32 {
    if stacked_stats(width, tokens) {
        stats_height(tokens) + tokens.spacing.gap_xs + tokens.dims.star_rating_height
    } else {
        stats_height(tokens)
    }
}

fn fixed_content_height(width: f32, tokens: &LayoutMetrics) -> f32 {
    let preview_size = preview_size(width, tokens);

    tokens.dims.card_padding * 2.0
        + stats_block_height(content_width(width, tokens), tokens)
        + tokens.dims.card_inner_gap
        + preview_size
        + tokens.dims.card_inner_gap
}

pub fn view<'a>(
    data: &'a Data,
    width: f32,
    cell_height: f32,
    content_height: f32,
    tokens: &Tokens,
    now: Instant,
    opacity: f32,
) -> Element<'a, Message> {
    let opacity = opacity.clamp(0.0, 1.0);
    let mut tokens = *tokens;
    for color in [
        &mut tokens.colors.text,
        &mut tokens.colors.icon_muted,
        &mut tokens.colors.surface_preview,
        &mut tokens.colors.sidebar_item_hover,
        &mut tokens.colors.shadow_card,
        &mut tokens.colors.shadow_card_strong,
    ] {
        *color = motion::scaled_alpha(*color, opacity);
    }
    let layout = &tokens.layout();
    let content_width = content_width(width, layout);
    let preview_size = preview_size(width, layout);
    let title_height = title_height_from_content_height(content_height, width, layout);
    let hover_progress = if tokens.motion.enabled() {
        data.hover_progress(now)
    } else {
        f32::from(data.hovered && data.enabled)
    };

    let stats = stats_row(data, content_width, &tokens, opacity);
    let preview = preview(data, preview_size, &tokens, hover_progress, now, opacity);
    let title = title(data, content_width, title_height, &tokens);

    let body = column![stats, preview, title]
        .spacing(layout.dims.card_inner_gap)
        .width(Length::Fixed(content_width))
        .height(Length::Shrink);

    let id = data.id.clone();
    let hovered = data.hovered;
    let enabled = data.enabled;
    let content_height = finite_nonnegative(content_height).max(1.0);
    let cell_height = finite_nonnegative(cell_height).max(content_height).max(1.0);
    let content = container(body)
        .width(Length::Fixed(width))
        .height(Length::Fixed(content_height))
        .padding(layout.dims.card_padding)
        .align_y(alignment::Vertical::Top)
        .clip(true)
        .style(move |_| card_style(&tokens, hover_progress, hovered, enabled));

    let area = mouse_area(content);
    let interactive: Element<'a, Message> = if enabled {
        let context_id = id.clone();
        context_area(
            area.on_press(Message::Pressed(id.clone()))
                .on_release(Message::Released(id))
                .interaction(mouse::Interaction::Pointer),
            move |position| Message::ContextRequested(context_id.clone(), position),
        )
        .into()
    } else {
        area.into()
    };

    container(interactive)
        .width(Length::Fixed(width))
        .height(Length::Fixed(cell_height))
        .align_y(alignment::Vertical::Top)
        .into()
}

fn stats_row<'a>(
    data: &'a Data,
    width: f32,
    tokens: &Tokens,
    opacity: f32,
) -> Element<'a, Message> {
    let opacity = enabled_opacity(data.enabled, tokens) * opacity;
    let text_color = text_color(data.enabled, tokens);

    let count = subscription_count_label(data, text_color.into(), tokens);

    let count_block = row![
        download_count_icon(tokens, tokens.icon_size(DOWNLOAD_ICON_SIZE), opacity),
        count,
    ]
    .spacing(tokens.space(3.0))
    .align_y(alignment::Vertical::Center)
    .width(Length::Fill)
    .height(Length::Fixed(stats_height(&tokens.layout())));

    let stars = star_rating(data.score_bucket, tokens, opacity);
    let layout = tokens.layout();
    if stacked_stats(width, &layout) {
        return column![count_block, stars]
            .spacing(layout.spacing.gap_xs)
            .width(Length::Fixed(width))
            .height(Length::Fixed(stats_block_height(width, &layout)))
            .into();
    }

    row![count_block, stars]
        .spacing(tokens.spacing.gap_sm)
        .align_y(alignment::Vertical::Center)
        .width(Length::Fixed(width))
        .height(Length::Fixed(stats_height(&tokens.layout())))
        .into()
}

fn subscription_count_label<'a>(
    data: &'a Data,
    color: Color,
    tokens: &Tokens,
) -> Element<'a, Message> {
    if data.enabled
        && let Some(roll) = &data.subscription_roll
    {
        return canvas(RollingCountText {
            roll: roll.clone(),
            color,
            size: tokens.typography.body,
            line_height: STATS_LINE_HEIGHT,
        })
        .width(Length::Fill)
        .height(Length::Fixed(stats_height(&tokens.layout())))
        .into();
    }

    text(data.subscriptions_label())
        .size(tokens.typography.body)
        .font(assets::fonts::default_font())
        .line_height(STATS_LINE_HEIGHT)
        .wrapping(text_widget::Wrapping::None)
        .color(color)
        .width(Length::Fill)
        .into()
}

#[derive(Clone, Debug)]
struct RollingCountText {
    roll: SubscriptionRoll,
    color: Color,
    size: f32,
    line_height: f32,
}

impl RollingCountText {
    fn fill_glyph(&self, frame: &mut canvas::Frame, glyph: char, center: Point, alpha: f32) {
        if alpha <= 0.0 {
            return;
        }
        frame.fill_text(canvas::Text {
            content: glyph.to_string(),
            position: center,
            max_width: self.size * 2.0,
            color: self.color.scale_alpha(alpha),
            size: Pixels(self.size),
            line_height: iced::advanced::text::LineHeight::Relative(self.line_height),
            font: assets::fonts::default_font(),
            align_x: iced::advanced::text::Alignment::Center,
            align_y: alignment::Vertical::Center,
            shaping: iced::advanced::text::Shaping::default(),
        });
    }
}

impl canvas::Program<Message> for RollingCountText {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center_y = bounds.height / 2.0;
        let eased = ease_out_cubic(self.roll.progress);
        let travel = self.size;
        let direction = if self.roll.up { -1.0 } else { 1.0 };

        let from: Vec<char> = self.roll.from.chars().collect();
        let to: Vec<char> = self.roll.to.chars().collect();

        // Pair glyphs by position only when the labels are the same length;
        // a length change (9,999 -> 10,000) rolls the whole label as one.
        let paired = from.len() == to.len();

        let mut x = 0.0;
        for (index, &to_glyph) in to.iter().enumerate() {
            let width =
                measure_stats_text_width(&to_glyph.to_string(), self.size, self.line_height);
            let center_x = x + width / 2.0;
            let from_glyph = paired.then(|| from[index]);

            if from_glyph == Some(to_glyph) {
                self.fill_glyph(&mut frame, to_glyph, Point::new(center_x, center_y), 1.0);
            } else {
                if let Some(from_glyph) = from_glyph {
                    let out_y = center_y + direction * eased * travel;
                    self.fill_glyph(
                        &mut frame,
                        from_glyph,
                        Point::new(center_x, out_y),
                        1.0 - eased,
                    );
                }
                let in_y = center_y + direction * (eased - 1.0) * travel;
                self.fill_glyph(&mut frame, to_glyph, Point::new(center_x, in_y), eased);
            }

            x += width;
        }

        if !paired {
            let mut x = 0.0;
            for &from_glyph in &from {
                let width =
                    measure_stats_text_width(&from_glyph.to_string(), self.size, self.line_height);
                let out_y = center_y + direction * eased * travel;
                self.fill_glyph(
                    &mut frame,
                    from_glyph,
                    Point::new(x + width / 2.0, out_y),
                    1.0 - eased,
                );
                x += width;
            }
        }

        vec![frame.into_geometry()]
    }
}

fn ease_out_cubic(progress: f32) -> f32 {
    let inverse = 1.0 - progress.clamp(0.0, 1.0);
    1.0 - inverse * inverse * inverse
}

fn preview<'a>(
    data: &Data,
    size: f32,
    tokens: &Tokens,
    hover_progress: f32,
    now: Instant,
    opacity: f32,
) -> Element<'a, Message> {
    let tokens = *tokens;
    let opacity = enabled_opacity(data.enabled, &tokens) * opacity;
    let content: Element<'a, Message> = match (&data.kind, &data.thumbnail) {
        (Kind::PublishNew, _) => plus_glyph(data, &tokens, hover_progress),
        (Kind::Addon, Thumbnail::Placeholder(handle)) => thumb_image(handle, size, opacity),
        (Kind::Addon, Thumbnail::Ready(handle)) => {
            let reveal = if tokens.motion.enabled() {
                data.reveal_progress(now)
            } else {
                1.0
            };
            let top = thumb_image(handle, size, opacity * reveal);
            match &data.reveal_under {
                Some(under) if reveal < 1.0 => {
                    iced::widget::stack![under_element(under, size, &tokens, opacity), top,].into()
                }
                _ => top,
            }
        }
        (Kind::Addon, Thumbnail::Loading) => loading_glyph(&tokens),
        (Kind::Addon, Thumbnail::Dead) => dead_glyph(&tokens),
    };

    container(content)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .center(Length::Fixed(size))
        .clip(true)
        .style(move |_| preview_style(&tokens))
        .into()
}

fn thumb_image<'a>(handle: &image::Handle, size: f32, opacity: f32) -> Element<'a, Message> {
    image(handle.clone())
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .content_fit(ContentFit::Cover)
        .opacity(opacity)
        .into()
}

/// The base layer must fill the preview box. Iced sizes the stack from it,
/// so using the loading glyph's bounds would also shrink the arriving image.
fn under_element<'a>(
    under: &Thumbnail,
    size: f32,
    tokens: &Tokens,
    opacity: f32,
) -> Element<'a, Message> {
    let content: Element<'a, Message> = match under {
        Thumbnail::Placeholder(handle) => thumb_image(handle, size, opacity),
        _ => loading_glyph(tokens),
    };

    container(content)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .center(Length::Fixed(size))
        .into()
}

fn plus_glyph<'a>(data: &Data, tokens: &Tokens, hover_progress: f32) -> Element<'a, Message> {
    let color = motion::mix_color(
        tokens.colors.icon_muted.into(),
        tokens.colors.text.into(),
        hover_progress,
    );

    svg(assets::icons::circle_plus())
        .width(Length::Fixed(tokens.dims.plus_glyph_size))
        .height(Length::Fixed(tokens.dims.plus_glyph_size))
        .content_fit(ContentFit::Contain)
        .style(move |_, _| svg::Style { color: Some(color) })
        .opacity(enabled_opacity(data.enabled, tokens))
        .into()
}

fn dead_glyph<'a>(tokens: &Tokens) -> Element<'a, Message> {
    let tokens = *tokens;
    svg(assets::icons::dead())
        .width(Length::Fixed(tokens.icon_size(DEAD_GLYPH_SIZE)))
        .height(Length::Fixed(tokens.icon_size(DEAD_GLYPH_SIZE)))
        .content_fit(ContentFit::Contain)
        .style(move |_, _| svg::Style {
            color: Some(tokens.colors.icon_muted.into()),
        })
        .into()
}

fn loading_glyph<'a>(tokens: &Tokens) -> Element<'a, Message> {
    let size = tokens.icon_size(LOADING_GLYPH_SIZE);
    let width = (size * 0.111).max(2.0);
    let heights = [0.56, 0.68, 1.0, 0.68, 0.56];
    let mut bars = row![]
        .spacing(size * 0.111)
        .align_y(alignment::Vertical::Center)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size));

    for factor in heights {
        bars = bars.push(bar(width, size * factor, tokens));
    }

    container(bars)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .center(Length::Fixed(size))
        .into()
}

fn bar<'a>(width: f32, height: f32, tokens: &Tokens) -> Element<'a, Message> {
    let tokens = *tokens;
    container(
        Space::new()
            .width(Length::Fixed(width))
            .height(Length::Fixed(height.max(1.0))),
    )
    .width(Length::Fixed(width))
    .height(Length::Fixed(height.max(1.0)))
    .style(move |_| container_widget::Style {
        background: Some(Color::from(tokens.colors.icon_muted).into()),
        border: Border {
            radius: (width * 0.4).into(),
            ..Border::default()
        },
        ..container_widget::Style::default()
    })
    .into()
}

fn title<'a>(data: &Data, width: f32, height: f32, tokens: &Tokens) -> Element<'a, Message> {
    let label = text(bounded_title(
        data.display_title(),
        width,
        tokens.typography.body,
    ))
    .size(tokens.typography.body)
    .font(assets::fonts::default_font())
    .line_height(TITLE_LINE_HEIGHT)
    .width(Length::Fill)
    .height(Length::Fixed(height))
    .align_x(alignment::Horizontal::Center)
    .align_y(alignment::Vertical::Top)
    .wrapping(text_widget::Wrapping::WordOrGlyph)
    .color(Color::from(text_color(data.enabled, tokens)));
    label.into()
}

thread_local! {
    static DISPLAY_TITLE_CACHE: RefCell<HashMap<(String, u32, u32), String>> =
        RefCell::new(HashMap::new());
}

fn title_paragraph(content: &str, width: f32, size: f32) -> IcedParagraph {
    IcedParagraph::with_text(iced::advanced::Text {
        content,
        bounds: Size::new(width.max(1.0), f32::INFINITY),
        size: Pixels(size),
        line_height: iced::advanced::text::LineHeight::Relative(TITLE_LINE_HEIGHT),
        font: assets::fonts::default_font(),
        align_x: iced::advanced::text::Alignment::Default,
        align_y: alignment::Vertical::Top,
        shaping: iced::advanced::text::Shaping::default(),
        wrapping: iced::advanced::text::Wrapping::WordOrGlyph,
    })
}

/// Iced's text bounds clip glyphs rather than complete lines. Truncate the
/// shaped label explicitly, so an overflowing line never leaves partial glyphs.
fn bounded_title(title: &str, width: f32, size: f32) -> String {
    let key = (title.to_owned(), width.to_bits(), size.to_bits());
    if let Some(label) = DISPLAY_TITLE_CACHE.with(|cache| cache.borrow().get(&key).cloned()) {
        return label;
    }
    let label = std::panic::catch_unwind(AssertUnwindSafe(|| {
        use unicode_segmentation::UnicodeSegmentation as _;
        if title_paragraph(title, width, size)
            .buffer()
            .layout_runs()
            .count()
            <= MAX_TITLE_LINES
        {
            return title.to_owned();
        }
        let boundaries = title
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .chain(std::iter::once(title.len()))
            .collect::<Vec<_>>();
        let mut lower = 0;
        let mut upper = boundaries.len() - 1;
        while lower < upper {
            let middle = (lower + upper).div_ceil(2);
            let candidate = format!("{}…", title[..boundaries[middle]].trim_end());
            if title_paragraph(&candidate, width, size)
                .buffer()
                .layout_runs()
                .count()
                <= MAX_TITLE_LINES
            {
                lower = middle;
            } else {
                upper = middle - 1;
            }
        }
        format!("{}…", title[..boundaries[lower]].trim_end())
    }))
    .unwrap_or_else(|_| title.to_owned());
    DISPLAY_TITLE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= 1024 {
            cache.clear();
        }
        let _ = cache.insert(key, label.clone());
    });
    label
}

fn content_width(width: f32, tokens: &LayoutMetrics) -> f32 {
    (width - tokens.dims.card_padding * 2.0).max(1.0)
}

fn preview_size(width: f32, tokens: &LayoutMetrics) -> f32 {
    content_width(width, tokens).max(1.0)
}

pub fn single_line_title_height(tokens: &LayoutMetrics) -> f32 {
    title_line_height(tokens)
}

#[cfg(test)]
pub fn measure_title_height(title: &str, width: f32, tokens: &LayoutMetrics) -> f32 {
    measure_title_cached(title, width, tokens, &mut None).0
}

pub(super) fn measure_title_cached(
    title: &str,
    width: f32,
    tokens: &LayoutMetrics,
    cached: &mut Option<IcedParagraph>,
) -> (f32, Option<f32>) {
    let line_height = title_line_height(tokens);
    let bounds = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let bounds = Size::new(content_width(width, tokens), f32::INFINITY);
        let paragraph = cached
            .get_or_insert_with(|| title_paragraph(title, bounds.width, tokens.typography.body));
        paragraph.resize(bounds);
        paragraph.min_bounds()
    }))
    .ok()
    .filter(|bounds| bounds.height.is_finite() && bounds.height > 0.0)
    .unwrap_or_else(|| Size::new(width, line_height));
    let height = clamp_title_height(bounds.height, tokens);
    let single_line_at = (height <= line_height + 0.5).then_some(bounds.width + 1.0);
    (height, single_line_at)
}

thread_local! {
    static STATS_CHAR_WIDTH_CACHE: RefCell<HashMap<(char, u32, u32), f32>> =
        RefCell::new(HashMap::new());
}

fn measure_stats_text_width(content: &str, size: f32, line_height: f32) -> f32 {
    let mut chars = content.chars();
    let single_char = match (chars.next(), chars.next()) {
        (Some(character), None) => Some(character),
        _ => None,
    };

    if let Some(character) = single_char {
        let key = (character, size.to_bits(), line_height.to_bits());
        if let Some(width) = STATS_CHAR_WIDTH_CACHE.with(|cache| cache.borrow().get(&key).copied())
        {
            return width;
        }
        let width = measure_stats_text_width_uncached(content, size, line_height);
        STATS_CHAR_WIDTH_CACHE.with(|cache| cache.borrow_mut().insert(key, width));
        return width;
    }

    measure_stats_text_width_uncached(content, size, line_height)
}

fn measure_stats_text_width_uncached(content: &str, size: f32, line_height: f32) -> f32 {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let paragraph = IcedParagraph::with_text(iced::advanced::Text {
            content,
            bounds: Size::new(f32::INFINITY, f32::INFINITY),
            size: Pixels(size),
            line_height: iced::advanced::text::LineHeight::Relative(line_height),
            font: assets::fonts::default_font(),
            align_x: iced::advanced::text::Alignment::Default,
            align_y: alignment::Vertical::Top,
            shaping: iced::advanced::text::Shaping::default(),
            wrapping: iced::advanced::text::Wrapping::None,
        });

        paragraph.min_bounds().width
    }))
    .ok()
    .filter(|width| width.is_finite() && *width > 0.0)
    .unwrap_or_else(|| {
        if content.trim().is_empty() {
            size * 0.33
        } else {
            size * 0.6 * content.chars().count().max(1) as f32
        }
    })
}

fn title_height_from_content_height(
    content_height: f32,
    width: f32,
    tokens: &LayoutMetrics,
) -> f32 {
    clamp_title_height(content_height - fixed_content_height(width, tokens), tokens)
}

fn clamp_title_height(height: f32, tokens: &LayoutMetrics) -> f32 {
    let height = if height.is_finite() && height > 0.0 {
        height
    } else {
        title_line_height(tokens)
    };

    height.clamp(title_line_height(tokens), max_title_height(tokens))
}

fn title_line_height(tokens: &LayoutMetrics) -> f32 {
    tokens.typography.body * TITLE_LINE_HEIGHT
}

fn max_title_height(tokens: &LayoutMetrics) -> f32 {
    title_line_height(tokens) * MAX_TITLE_LINES as f32
}

fn finite_nonnegative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn text_color(enabled: bool, tokens: &Tokens) -> crate::theme::tokens::Rgba {
    if enabled {
        tokens.colors.text
    } else {
        motion::scaled_alpha(tokens.colors.text, tokens.dims.disabled_opacity)
    }
}

fn enabled_opacity(enabled: bool, tokens: &Tokens) -> f32 {
    if enabled {
        1.0
    } else {
        tokens.dims.disabled_opacity
    }
}

impl Data {
    fn hover_progress(&self, now: Instant) -> f32 {
        if self.enabled {
            self.hover_motion.interpolate(0.0, 1.0, now)
        } else {
            0.0
        }
    }
}

fn card_style(
    tokens: &Tokens,
    hover_progress: f32,
    hovered: bool,
    enabled: bool,
) -> container_widget::Style {
    let hover_progress = if enabled { hover_progress } else { 0.0 };
    let background = if hover_progress > 0.0 {
        Some(Background::Color(
            Color::from(tokens.colors.sidebar_item_hover).scale_alpha(hover_progress),
        ))
    } else {
        None
    };

    container_widget::Style {
        text_color: Some(text_color(enabled, tokens).into()),
        background,
        border: Border {
            radius: tokens.radii.sm.into(),
            ..Border::default()
        },
        shadow: if (hovered || hover_progress > 0.0) && enabled {
            Shadow {
                color: Color::from(tokens.colors.shadow_card).scale_alpha(hover_progress),
                offset: Vector::new(0.0, 1.0),
                blur_radius: 4.0 * hover_progress,
            }
        } else {
            Shadow::default()
        },
        snap: true,
    }
}

fn hover_animation(initial: bool) -> motion::Presence<bool> {
    motion::boolean(
        initial,
        theme::invariant().motion.fast_duration(),
        Easing::EaseInOut,
    )
}

fn reveal_animation(initial: bool) -> motion::Presence<bool> {
    motion::boolean(
        initial,
        theme::invariant().motion.thumb_reveal_duration(),
        Easing::EaseOut,
    )
}

fn preview_style(tokens: &Tokens) -> container_widget::Style {
    container_widget::Style {
        background: Some(Color::from(tokens.colors.surface_preview).into()),
        border: Border {
            radius: 0.0.into(),
            ..Border::default()
        },
        shadow: Shadow {
            color: tokens.colors.shadow_card_strong.into(),
            offset: Vector::new(0.0, 1.0),
            blur_radius: 2.0,
        },
        snap: true,
        ..container_widget::Style::default()
    }
}

#[cfg(test)]
mod tests {
    use crate::widgets::grid_rows::CardId;
    use iced::widget::image;

    use super::{
        Data, Kind, Thumbnail, measure_title_height, preferred_height,
        preferred_height_for_title_height,
    };
    use crate::theme;

    #[test]
    fn publish_new_uses_fallback_title_when_empty() {
        let card = Data::publish_new(CardId::from("publish"), "");

        assert_eq!(card.display_title(), "Publish new");
        assert_eq!(card.kind, Kind::PublishNew);
    }

    #[test]
    fn hover_animation_tracks_target_state() {
        let started = std::time::Instant::now();
        let mut card = Data::addon(CardId::from("1"), "Addon");

        card.set_hovered_at(true, started);

        assert!(card.needs_motion_ticks());
        assert_eq!(
            card.hover_progress(started + std::time::Duration::from_millis(150)),
            1.0
        );
        card.tick_motion(started + std::time::Duration::from_millis(150));
        assert!(!card.needs_motion_ticks());

        card.set_hovered_at(false, started + std::time::Duration::from_millis(160));

        assert!(card.needs_motion_ticks());
        assert_eq!(
            card.hover_progress(started + std::time::Duration::from_millis(300)),
            0.0
        );
        card.tick_motion(started + std::time::Duration::from_millis(300));
        assert!(!card.needs_motion_ticks());
    }

    #[test]
    fn motion_state_can_be_preserved_across_rebuilt_cards() {
        let started = std::time::Instant::now();
        let mut previous = Data::addon(CardId::from("1"), "Old");
        previous.set_hovered_at(true, started);
        let mut next = Data::addon(CardId::from("1"), "New");

        next.preserve_motion_from(&previous);

        assert!(next.is_hovered());
        assert!(next.needs_motion_ticks());
    }

    #[test]
    fn subscriptions_fall_back_to_raw_count() {
        let card = Data::addon(CardId::from("1"), "Addon").with_subscriptions("", 42);

        assert_eq!(card.subscriptions_label(), "42");
    }

    #[test]
    fn arriving_pixels_fade_in_over_the_stand_in() {
        let placeholder = image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]);
        let sharp = image::Handle::from_rgba(1, 1, vec![255, 255, 255, 255]);
        let mut card = Data::addon(CardId::from("1"), "Addon")
            .with_thumbnail(Thumbnail::Placeholder(placeholder.clone()));

        assert!(
            !Data::addon(CardId::from("2"), "Cached")
                .with_thumbnail(Thumbnail::Ready(sharp.clone()))
                .needs_motion_ticks()
        );

        card.set_thumbnail(Thumbnail::Ready(sharp.clone()));

        assert!(card.needs_motion_ticks());
        assert_eq!(card.reveal_under, Some(Thumbnail::Placeholder(placeholder)));
        let mid =
            card.reveal_progress(std::time::Instant::now() + std::time::Duration::from_millis(75));
        assert!(mid > 0.0 && mid < 1.0);

        card.tick_motion(std::time::Instant::now() + std::time::Duration::from_millis(300));
        assert!(!card.needs_motion_ticks());
        assert_eq!(card.reveal_under, None);
        card.set_thumbnail(Thumbnail::Ready(sharp));
        assert!(!card.needs_motion_ticks());
    }

    #[test]
    fn rehydrated_pixels_snap_back_without_replaying_the_reveal() {
        let sharp = image::Handle::from_rgba(1, 1, vec![255, 255, 255, 255]);
        let mut card = Data::addon(CardId::from("1"), "Addon");
        card.set_thumbnail(Thumbnail::Ready(sharp.clone()));
        card.tick_motion(std::time::Instant::now() + std::time::Duration::from_millis(300));
        assert!(!card.needs_motion_ticks());

        card.set_thumbnail(Thumbnail::Loading);
        card.set_thumbnail(Thumbnail::Ready(sharp.clone()));

        assert!(!card.needs_motion_ticks());
        assert_eq!(card.reveal_under, None);
        assert_eq!(card.reveal_progress(std::time::Instant::now()), 1.0);

        let mut released =
            Data::addon(CardId::from("1"), "Addon").with_thumbnail(Thumbnail::Loading);
        released.preserve_motion_from(&card);
        let mut returned =
            Data::addon(CardId::from("1"), "Addon").with_thumbnail(Thumbnail::Ready(sharp));
        returned.preserve_motion_from(&released);

        assert!(!returned.needs_motion_ticks());
        assert_eq!(returned.reveal_under, None);
    }

    #[test]
    fn a_reveal_cut_short_by_a_release_does_not_resume_on_return() {
        let started = std::time::Instant::now();
        let sharp = image::Handle::from_rgba(1, 1, vec![255, 255, 255, 255]);
        let mut card = Data::addon(CardId::from("1"), "Addon");
        card.set_thumbnail(Thumbnail::Ready(sharp.clone()));

        assert!(card.reveal_under.is_some());
        card.set_thumbnail(Thumbnail::Loading);
        card.set_thumbnail(Thumbnail::Ready(sharp));

        assert_eq!(card.reveal_under, None);
        assert_eq!(card.reveal_progress(started), 1.0);
    }

    #[test]
    fn preferred_height_composes_square_preview_and_clamped_title() {
        let tokens = theme::invariant();
        let width = 200.0;
        let preview = width - tokens.dims.card_padding * 2.0;
        let base = tokens.dims.card_padding * 2.0
            + super::stats_block_height(preview, tokens)
            + tokens.dims.card_inner_gap
            + preview
            + tokens.dims.card_inner_gap;
        let one_line = super::title_line_height(tokens);

        assert_eq!(
            preferred_height_for_title_height(width, one_line, tokens),
            base + one_line
        );

        assert_eq!(
            preferred_height_for_title_height(width, tokens.dims.card_title_height * 2.0, tokens),
            base + super::max_title_height(tokens)
        );
    }

    #[test]
    fn measured_title_height_is_total_and_clamped() {
        let tokens = theme::invariant();
        let line = super::title_line_height(tokens);
        let measured = measure_title_height(
            "A very long addon name that may wrap when the font system is populated",
            120.0,
            tokens,
        );

        assert!(measured >= line);
        assert!(measured <= tokens.dims.card_title_height);

        let preferred = preferred_height(&Data::addon(CardId::from("1"), "Short"), 200.0, tokens);
        assert!(preferred.is_finite());
    }
    #[test]
    fn enlarged_text_stacks_metadata_using_the_measured_card_geometry() {
        let tokens = crate::theme::Tokens::dark()
            .with_display_preferences(crate::bridge::UiDensity::Compact, 150);
        let metrics = tokens.layout();
        let width = 220.0;
        let inner_width = super::content_width(width, &metrics);
        assert!(super::stacked_stats(inner_width, &metrics));
        assert_eq!(
            super::max_title_height(&metrics),
            super::title_line_height(&metrics) * 3.0
        );
        assert_eq!(
            super::stats_block_height(inner_width, &metrics),
            super::stats_height(&metrics)
                + metrics.spacing.gap_xs
                + metrics.dims.star_rating_height,
        );
        assert!(!super::stacked_stats(360.0, &metrics));
        let card = Data::addon(CardId::from("large"), "Title");
        let measured = preferred_height(&card, width, &metrics);
        let _element = super::view(
            &card,
            width,
            measured,
            measured,
            &tokens,
            std::time::Instant::now(),
            1.0,
        );
    }
    #[test]
    fn long_titles_end_at_a_complete_line_and_keep_unicode_graphemes() {
        let title = "An unusually long Workshop title with 日本語 and 한국어 labels that must not paint a partial fourth line on a compact card";
        let width = 150.0;
        let size = 21.0;
        let label = super::bounded_title(title, width, size);
        assert!(label.ends_with('…'));
        assert!(title.starts_with(label.trim_end_matches('…').trim_end()));
        assert!(
            super::title_paragraph(&label, width, size)
                .buffer()
                .layout_runs()
                .count()
                <= 3
        );
        assert_eq!(super::bounded_title("日本語", width, size), "日本語");
    }
}
