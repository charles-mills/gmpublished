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
use crate::theme::{self, Tokens, motion};
use crate::widgets::context_area::context_area;
use crate::widgets::download_count_icon::download_count_icon;
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Thumbnail {
    Loading,
    Dead,
    /// Blurred ThumbHash stand-in shown until the real pixels decode.
    Placeholder(image::Handle),
    Ready(image::Handle),
}

/// An in-flight subscription-count change: the stats row rolls each changed
/// digit from the `from` label to the `to` label, odometer-style.
#[derive(Clone, Debug, PartialEq)]
pub struct SubscriptionRoll {
    pub from: String,
    pub to: String,
    /// Raw 0..=1 roll progress; easing is applied at draw time.
    pub progress: f32,
    /// True when subscriptions were gained; the digits roll upward.
    pub up: bool,
}

#[derive(Clone, Debug)]
pub struct Data {
    id: String,
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
    /// The visual the arriving thumbnail fades in over; dropped once settled.
    reveal_under: Option<Thumbnail>,
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
    }
}

impl Data {
    pub(crate) fn addon(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: Kind::Addon,
            title: title.into(),
            subscriptions: String::new(),
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
        }
    }

    pub(crate) fn publish_new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            kind: Kind::PublishNew,
            ..Self::addon(id, title)
        }
    }

    pub(crate) fn id(&self) -> &str {
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

    #[cfg(test)]
    pub(crate) fn set_hovered(&mut self, hovered: bool) {
        self.set_hovered_at(hovered, Instant::now());
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
        self.reveal_on_arrival(&previous.thumbnail, Instant::now());
    }

    /// Starts the fade-in when real pixels replace a stand-in, keeping the
    /// stand-in underneath for the crossfade. Ready-to-Ready swaps (GIF
    /// frames) never retrigger.
    fn reveal_on_arrival(&mut self, previous: &Thumbnail, now: Instant) {
        let arrived = matches!(self.thumbnail, Thumbnail::Ready(_))
            && matches!(previous, Thumbnail::Loading | Thumbnail::Placeholder(_));
        if arrived {
            self.reveal_motion = reveal_animation(false);
            self.reveal_motion.go(true, now);
            self.reveal_under = Some(previous.clone());
        }
    }

    fn reveal_progress(&self, now: Instant) -> f32 {
        self.reveal_motion.interpolate(0.0, 1.0, now)
    }

    pub(crate) fn with_subscriptions(mut self, label: impl Into<String>, count: u64) -> Self {
        self.subscriptions = label.into();
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
        self.thumbnail = thumbnail;
        self
    }

    pub(crate) fn set_thumbnail(&mut self, thumbnail: Thumbnail) {
        let previous = std::mem::replace(&mut self.thumbnail, thumbnail);
        self.reveal_on_arrival(&previous, Instant::now());
    }

    pub(crate) const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    #[cfg(test)]
    pub(crate) fn subscriptions_label_for_test(&self) -> String {
        self.subscriptions_label()
    }

    #[cfg(test)]
    pub(crate) const fn subscription_roll_for_test(&self) -> Option<&SubscriptionRoll> {
        self.subscription_roll.as_ref()
    }

    fn subscriptions_label(&self) -> String {
        if self.subscriptions.is_empty() {
            self.subscription_count.to_string()
        } else {
            self.subscriptions.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Pressed(String),
    Released(String),
    ContextRequested(String, Point),
}

pub fn preferred_height(data: &Data, width: f32, tokens: &Tokens) -> f32 {
    let measured_title_height = measure_title_height(data.display_title(), width, tokens);

    preferred_height_for_title_height(width, measured_title_height, tokens)
}

pub fn preferred_height_for_title_height(width: f32, title_height: f32, tokens: &Tokens) -> f32 {
    fixed_content_height(width, tokens) + clamp_title_height(title_height, tokens)
}

fn fixed_content_height(width: f32, tokens: &Tokens) -> f32 {
    let preview_size = preview_size(width, tokens);

    tokens.dims.card_padding * 2.0
        + tokens.dims.card_stats_height
        + tokens.dims.card_inner_gap
        + preview_size
        + tokens.dims.card_inner_gap
}

pub fn view<'a>(
    data: &Data,
    width: f32,
    cell_height: f32,
    content_height: f32,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let tokens = *tokens;
    let content_width = content_width(width, &tokens);
    let preview_size = preview_size(width, &tokens);
    let title_height = title_height_from_content_height(content_height, width, &tokens);
    let hover_progress = data.hover_progress(Instant::now());

    let stats = stats_row(data, content_width, &tokens);
    let preview = preview(data, preview_size, &tokens, hover_progress);
    let title = title(data, title_height, &tokens);

    let body = column![stats, preview, title]
        .spacing(tokens.dims.card_inner_gap)
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
        .padding(tokens.dims.card_padding)
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

fn stats_row<'a>(data: &Data, width: f32, tokens: &Tokens) -> Element<'a, Message> {
    let opacity = enabled_opacity(data.enabled, tokens);
    let text_color = text_color(data.enabled, tokens);

    let count = subscription_count_label(data, text_color.into(), tokens);

    let count_block = row![
        download_count_icon(tokens, DOWNLOAD_ICON_SIZE, opacity),
        count,
    ]
    .spacing(3.0)
    .align_y(alignment::Vertical::Center)
    .width(Length::Fill)
    .height(Length::Fixed(tokens.dims.card_stats_height));

    let stars = star_rating(data.score_bucket, tokens, opacity);

    row![count_block, stars]
        .spacing(8.0)
        .align_y(alignment::Vertical::Center)
        .width(Length::Fixed(width))
        .height(Length::Fixed(tokens.dims.card_stats_height))
        .into()
}

fn subscription_count_label<'a>(
    data: &Data,
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
        .height(Length::Fixed(tokens.dims.card_stats_height))
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

/// Odometer-style count change: glyphs shared by the outgoing and incoming
/// labels stay put; each changed glyph slides out (up for gains, down for
/// losses) and fades while its replacement slides in from the opposite side.
/// The canvas clips to the stats row, so glyphs vanish at the line edges.
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
            // The outgoing label rolls out whole, laid out independently.
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
) -> Element<'a, Message> {
    let tokens = *tokens;
    let opacity = enabled_opacity(data.enabled, &tokens);
    let content: Element<'a, Message> = match (&data.kind, &data.thumbnail) {
        (Kind::PublishNew, _) => plus_glyph(data, &tokens, hover_progress),
        (Kind::Addon, Thumbnail::Placeholder(handle)) => thumb_image(handle, size, opacity),
        (Kind::Addon, Thumbnail::Ready(handle)) => {
            let reveal = data.reveal_progress(Instant::now());
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

/// The visual an arriving thumbnail fades in over: the blurred stand-in when
/// there was one, otherwise the loading glyph.
fn under_element<'a>(
    under: &Thumbnail,
    size: f32,
    tokens: &Tokens,
    opacity: f32,
) -> Element<'a, Message> {
    match under {
        Thumbnail::Placeholder(handle) => thumb_image(handle, size, opacity),
        _ => loading_glyph(tokens),
    }
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
        .width(Length::Fixed(DEAD_GLYPH_SIZE))
        .height(Length::Fixed(DEAD_GLYPH_SIZE))
        .content_fit(ContentFit::Contain)
        .style(move |_, _| svg::Style {
            color: Some(tokens.colors.icon_muted.into()),
        })
        .into()
}

fn loading_glyph<'a>(tokens: &Tokens) -> Element<'a, Message> {
    let size = LOADING_GLYPH_SIZE;
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

fn title<'a>(data: &Data, height: f32, tokens: &Tokens) -> Element<'a, Message> {
    text(data.display_title().to_owned())
        .size(tokens.typography.body)
        .font(assets::fonts::default_font())
        .line_height(TITLE_LINE_HEIGHT)
        .width(Length::Fill)
        .height(Length::Fixed(height))
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Top)
        .wrapping(text_widget::Wrapping::WordOrGlyph)
        .color(Color::from(text_color(data.enabled, tokens)))
        .into()
}

fn content_width(width: f32, tokens: &Tokens) -> f32 {
    (width - tokens.dims.card_padding * 2.0).max(1.0)
}

fn preview_size(width: f32, tokens: &Tokens) -> f32 {
    content_width(width, tokens).max(1.0)
}

pub fn measure_title_height(title: &str, width: f32, tokens: &Tokens) -> f32 {
    let line_height = title_line_height(tokens);
    let measured = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let paragraph = IcedParagraph::with_text(iced::advanced::Text {
            content: title,
            bounds: Size::new(content_width(width, tokens), f32::INFINITY),
            size: Pixels(tokens.typography.body),
            line_height: iced::advanced::text::LineHeight::Relative(TITLE_LINE_HEIGHT),
            font: assets::fonts::default_font(),
            align_x: iced::advanced::text::Alignment::Default,
            align_y: alignment::Vertical::Top,
            shaping: iced::advanced::text::Shaping::default(),
            wrapping: iced::advanced::text::Wrapping::WordOrGlyph,
        });

        paragraph.min_bounds().height
    }))
    .ok()
    .filter(|height| height.is_finite() && *height > 0.0)
    .unwrap_or(line_height);

    clamp_title_height(measured, tokens)
}

thread_local! {
    // `RollingCountText::draw` measures one glyph at a time, every canvas
    // frame, for as long as a count roll runs. The glyph set is tiny
    // (digits, comma, etc.), so caching per (char, size, line_height) turns
    // repeat frames into a hash lookup instead of a fresh paragraph shape.
    // Single-character-only: `measure_stats_text_width`'s only other
    // possible inputs (multi-char content) fall through uncached.
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

fn title_height_from_content_height(content_height: f32, width: f32, tokens: &Tokens) -> f32 {
    clamp_title_height(content_height - fixed_content_height(width, tokens), tokens)
}

fn clamp_title_height(height: f32, tokens: &Tokens) -> f32 {
    let height = if height.is_finite() && height > 0.0 {
        height
    } else {
        title_line_height(tokens)
    };

    height.clamp(title_line_height(tokens), max_title_height(tokens))
}

fn title_line_height(tokens: &Tokens) -> f32 {
    tokens.typography.body * TITLE_LINE_HEIGHT
}

fn max_title_height(tokens: &Tokens) -> f32 {
    (title_line_height(tokens) * MAX_TITLE_LINES as f32).min(tokens.dims.card_title_height)
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
        tokens
            .colors
            .text
            .with_alpha(motion::opacity_byte(tokens.dims.disabled_opacity))
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
            radius: tokens.radii.base.into(),
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
    use iced::widget::image;

    use super::{
        Data, Kind, Thumbnail, measure_title_height, preferred_height,
        preferred_height_for_title_height,
    };
    use crate::theme::Tokens;

    #[test]
    fn publish_new_uses_fallback_title_when_empty() {
        let card = Data::publish_new("publish", "");

        assert_eq!(card.display_title(), "Publish new");
        assert_eq!(card.kind, Kind::PublishNew);
    }

    #[test]
    fn hover_state_is_explicit_card_data() {
        let mut card = Data::addon("1", "Addon");

        assert!(!card.is_hovered());
        card.set_hovered(true);

        assert!(card.is_hovered());
    }

    #[test]
    fn hover_animation_tracks_target_state() {
        let started = std::time::Instant::now();
        let mut card = Data::addon("1", "Addon");

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
        let mut previous = Data::addon("1", "Old");
        previous.set_hovered_at(true, started);
        let mut next = Data::addon("1", "New");

        next.preserve_motion_from(&previous);

        assert!(next.is_hovered());
        assert!(next.needs_motion_ticks());
    }

    #[test]
    fn subscriptions_fall_back_to_raw_count() {
        let card = Data::addon("1", "Addon").with_subscriptions("", 42);

        assert_eq!(card.subscriptions_label(), "42");
    }

    #[test]
    fn arriving_pixels_fade_in_over_the_stand_in() {
        let placeholder = image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]);
        let sharp = image::Handle::from_rgba(1, 1, vec![255, 255, 255, 255]);
        let mut card =
            Data::addon("1", "Addon").with_thumbnail(Thumbnail::Placeholder(placeholder.clone()));

        // Building a card straight to Ready is not an arrival.
        assert!(
            !Data::addon("2", "Cached")
                .with_thumbnail(Thumbnail::Ready(sharp.clone()))
                .needs_motion_ticks()
        );

        card.set_thumbnail(Thumbnail::Ready(sharp.clone()));

        assert!(card.needs_motion_ticks());
        assert_eq!(card.reveal_under, Some(Thumbnail::Placeholder(placeholder)));
        let mid =
            card.reveal_progress(std::time::Instant::now() + std::time::Duration::from_millis(75));
        assert!(mid > 0.0 && mid < 1.0);

        // GIF-style Ready-to-Ready swaps never retrigger the fade.
        card.tick_motion(std::time::Instant::now() + std::time::Duration::from_millis(300));
        assert!(!card.needs_motion_ticks());
        assert_eq!(card.reveal_under, None);
        card.set_thumbnail(Thumbnail::Ready(sharp));
        assert!(!card.needs_motion_ticks());
    }

    #[test]
    fn preferred_height_composes_square_preview_and_clamped_title() {
        let tokens = Tokens::dark();
        let width = 200.0;
        let preview = width - tokens.dims.card_padding * 2.0;
        let base = tokens.dims.card_padding * 2.0
            + tokens.dims.card_stats_height
            + tokens.dims.card_inner_gap
            + preview
            + tokens.dims.card_inner_gap;
        let one_line = super::title_line_height(&tokens);

        assert_eq!(
            preferred_height_for_title_height(width, one_line, &tokens),
            base + one_line
        );

        assert_eq!(
            preferred_height_for_title_height(width, tokens.dims.card_title_height * 2.0, &tokens),
            base + super::max_title_height(&tokens)
        );
    }

    #[test]
    fn measured_title_height_is_total_and_clamped() {
        let tokens = Tokens::dark();
        let line = super::title_line_height(&tokens);
        let measured = measure_title_height(
            "A very long addon name that may wrap when the font system is populated",
            120.0,
            &tokens,
        );

        assert!(measured >= line);
        assert!(measured <= tokens.dims.card_title_height);

        let preferred = preferred_height(&Data::addon("1", "Short"), 200.0, &tokens);
        assert!(preferred.is_finite());
    }
}
