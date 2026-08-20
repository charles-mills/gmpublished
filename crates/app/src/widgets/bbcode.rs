//! Renders a parsed BBCode [`Document`] as iced widgets, in one of two visual
//! contexts sharing a single code path:
//!
//! - the app theme ([`view`]), driven by the live [`Tokens`], for surfaces
//!   like the preview sidebar;
//! - Steam's Workshop item page ([`steam_view`]), driven by
//!   [`RenderTokens::steam`] — values measured from Steam's own stylesheets
//!   and computed styles (`shared_global.css` and a live
//!   `.workshopItemDescription`), so the description editor's preview shows
//!   what Workshop visitors will actually see.
//!
//! Remote media (`[img]`, `[previewyoutube]` thumbnails) is supplied by the
//! caller through [`MediaLookup`]: the enclosing feature owns fetching (via
//! the thumbnail pipeline) and hands handles in; surfaces that do not fetch
//! pass [`NoMedia`] and get textual link fallbacks instead.

use std::collections::HashSet;

use gmpublished_backend::bbcode::{
    Document, ElementKind, Node, SpoilerId, youtube_thumbnail_url, youtube_watch_url,
};
use iced::widget::{
    Column, Space, container, image, mouse_area, rich_text, row, span, stack, svg, text,
};
use iced::{Background, Border, Color, ContentFit, Element, Font, Length, border, font};

use crate::assets;
use crate::i18n::{Arg, I18n};
use crate::theme::{Rgba, Tokens};

type StyledSpan = iced::widget::text::Span<'static, Interaction>;

/// The Workshop item page's canvas color; the surface a Steam-context
/// preview should paint behind [`steam_view`].
pub const STEAM_PAGE_BACKGROUND: Rgba = Rgba::rgb(0x1B2838);

/// The width of the Workshop page's description column. A Steam-context
/// preview laid out at this width wraps exactly where the live page does.
pub const STEAM_DESCRIPTION_WIDTH: f32 = 647.0;

/// Height of the box shown while an image is still being fetched; replaced by
/// the image's own aspect ratio once its dimensions are known.
const MEDIA_PLACEHOLDER_HEIGHT: f32 = 96.0;

/// `img.sharedFilePreviewImage { max-width: 630px }`.
const STEAM_IMAGE_MAX_WIDTH: f32 = 630.0;

/// Every visual decision the renderer makes, resolved per context. Field
/// values for the app come from [`Tokens`]; the Steam context is a fixed
/// palette measured from the Workshop page (each field notes its CSS source).
#[derive(Clone, Copy)]
struct RenderTokens {
    gap_xs: f32,
    gap_sm: f32,
    /// List marker column width.
    marker_width: f32,
    /// Table cell padding.
    pad_xs: f32,
    /// Quote and code-block padding.
    pad_sm: f32,
    /// Quote author line size.
    caption: f32,
    body: f32,
    /// Sizes for `[h1]`/`[h2]`/`[h3]`.
    heading_sizes: [f32; 3],
    /// Steam headings are regular weight (`div.bb_h1 { font-weight: normal }`).
    heading_bold: bool,
    line_height: f32,
    radius_xs: f32,
    radius_base: f32,
    text: Rgba,
    heading: Rgba,
    link: Rgba,
    /// Steam's `.bb_link` has no underline.
    link_underline: bool,
    /// List marker color.
    marker: Rgba,
    divider: Rgba,
    quote: QuoteStyle,
    code: CodeStyle,
    table: TableStyle,
    spoiler: SpoilerStyle,
    /// `img.sharedFilePreviewImage { max-width: 630px }`.
    image_max_width: f32,
    media_placeholder: Rgba,
}

/// How one context draws `[quote]` blocks.
#[derive(Clone, Copy)]
struct QuoteStyle {
    shape: QuoteShape,
    bg: Option<Rgba>,
    /// Body size inside quotes (Steam shrinks quotes to 92%).
    size: f32,
}

/// The two quote silhouettes. The shape also decides the author line: the
/// bordered Steam box reproduces Steam's localized "Originally posted by …:"
/// header, the app's rule bar shows the raw author.
#[derive(Clone, Copy)]
enum QuoteShape {
    /// Steam's bordered box (`blockquote.bb_blockquote`).
    Bordered(Rgba),
    /// The app's left rule bar.
    RuleBar(Rgba),
}

/// How one context draws `[code]` blocks.
#[derive(Clone, Copy)]
struct CodeStyle {
    border: Rgba,
    bg: Option<Rgba>,
    size: f32,
}

/// How one context draws `[table]` blocks.
#[derive(Clone, Copy)]
struct TableStyle {
    border: Rgba,
    th_bg: Option<Rgba>,
    size: f32,
}

/// How one context draws `[spoiler]` runs.
#[derive(Clone, Copy)]
struct SpoilerStyle {
    cover: Rgba,
    /// Revealed spoiler text when the cover is kept (Steam shows white text
    /// on the black chip; the app removes the cover entirely).
    text: Rgba,
    pad: [f32; 2],
    reveal_keeps_cover: bool,
}

impl From<&Tokens> for RenderTokens {
    fn from(tokens: &Tokens) -> Self {
        Self {
            gap_xs: tokens.spacing.gap_xs,
            gap_sm: tokens.spacing.gap_sm,
            marker_width: tokens.spacing.pad,
            pad_xs: tokens.spacing.pad_xs,
            pad_sm: tokens.spacing.pad_sm,
            caption: tokens.typography.caption,
            body: tokens.typography.body_sm,
            heading_sizes: [
                tokens.typography.title,
                tokens.typography.title_sm,
                tokens.typography.body_lg,
            ],
            heading_bold: true,
            line_height: 1.35,
            radius_xs: tokens.radii.xs,
            radius_base: tokens.radii.sm,
            text: tokens.colors.text_dim,
            heading: tokens.colors.text_dim,
            link: tokens.colors.link,
            link_underline: true,
            marker: tokens.colors.text_dim,
            divider: tokens.colors.divider,
            quote: QuoteStyle {
                shape: QuoteShape::RuleBar(tokens.colors.border_strong),
                bg: Some(tokens.colors.surface_muted),
                size: tokens.typography.body_sm,
            },
            code: CodeStyle {
                border: tokens.colors.border,
                bg: Some(tokens.colors.surface_sunken),
                size: tokens.typography.caption,
            },
            table: TableStyle {
                border: tokens.colors.border,
                th_bg: Some(tokens.colors.surface_muted),
                size: tokens.typography.body_sm,
            },
            spoiler: SpoilerStyle {
                cover: tokens.colors.surface_raised,
                text: tokens.colors.text_dim,
                pad: [1.0, 2.0],
                reveal_keeps_cover: false,
            },
            image_max_width: STEAM_IMAGE_MAX_WIDTH,
            media_placeholder: tokens.colors.surface_muted,
        }
    }
}

impl RenderTokens {
    /// The Workshop item page context. Sources, measured 2026-08:
    /// `.workshopItemDescription` computed style (body), `div.bb_h1/h2/h3`,
    /// `a.bb_link`, `span.bb_spoiler`, `blockquote.bb_blockquote`,
    /// `div.bb_code`, `div.bb_table*` (all `shared_global.css`), and the
    /// page's computed `<hr>`/image bounds.
    const fn steam() -> Self {
        Self {
            gap_xs: 4.0,
            gap_sm: 8.0,
            marker_width: 20.0,
            pad_xs: 4.0,
            pad_sm: 12.0,
            caption: 12.9,
            body: 14.0,
            heading_sizes: [20.0, 18.0, 16.0],
            heading_bold: false,
            // 20px line height over 14px text.
            line_height: 20.0 / 14.0,
            radius_xs: 0.0,
            radius_base: 3.0,
            text: Rgba::rgb(0xACB2B8),
            heading: Rgba::rgb(0x5AA9D6),
            link: Rgba::rgb(0xEBEBEB),
            link_underline: false,
            marker: Rgba::rgb(0xACB2B8),
            divider: Rgba::from_rgba(0x808080, 140),
            quote: QuoteStyle {
                shape: QuoteShape::Bordered(Rgba::rgb(0x56707F)),
                bg: None,
                size: 12.9,
            },
            code: CodeStyle {
                border: Rgba::rgb(0x535354),
                bg: None,
                size: 11.0,
            },
            table: TableStyle {
                border: Rgba::rgb(0x4D4D4D),
                th_bg: None,
                size: 12.0,
            },
            spoiler: SpoilerStyle {
                cover: Rgba::rgb(0x000000),
                text: Rgba::rgb(0xFFFFFF),
                pad: [0.0, 8.0],
                reveal_keeps_cover: true,
            },
            image_max_width: STEAM_IMAGE_MAX_WIDTH,
            media_placeholder: Rgba::from_rgba(0x000000, 64),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    OpenLink(String),
    ToggleSpoiler(SpoilerId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Interaction {
    OpenLink(String),
    ToggleSpoiler(SpoilerId),
}

impl From<Interaction> for Event {
    fn from(value: Interaction) -> Self {
        match value {
            Interaction::OpenLink(url) => Self::OpenLink(url),
            Interaction::ToggleSpoiler(id) => Self::ToggleSpoiler(id),
        }
    }
}

/// Display state of one fetched media source, keyed by the exact URLs
/// reported by [`media_urls`].
#[derive(Clone, Copy, Debug)]
pub enum MediaView<'a> {
    /// This surface does not fetch media at all; render link fallbacks.
    Unavailable,
    Loading,
    Ready {
        handle: &'a image::Handle,
        /// Source dimensions (pre-downscale), for aspect-correct layout.
        width: u32,
        height: u32,
    },
    Failed,
}

/// Supplier of fetched description media. The renderer never fetches;
/// whichever feature shows the preview owns demands and deliveries.
pub trait MediaLookup {
    fn media(&self, url: &str) -> MediaView<'_>;
}

/// The lookup for surfaces that render descriptions without fetching remote
/// media: images and videos degrade to the textual link fallbacks.
pub struct NoMedia;

impl MediaLookup for NoMedia {
    fn media(&self, _url: &str) -> MediaView<'_> {
        MediaView::Unavailable
    }
}

/// Every remote media URL `document` would render, in source order without
/// duplicates: `[img]` sources plus YouTube preview thumbnails. Only
/// http(s) sources are reported — anything else can never be fetched and
/// falls back to a link at render time.
#[must_use]
pub fn media_urls(document: &Document) -> Vec<String> {
    let mut urls = Vec::<String>::new();
    let mut pending: Vec<&Node> = document.nodes().iter().rev().collect();
    while let Some(node) = pending.pop() {
        let Node::Element(element) = node else {
            continue;
        };
        match element.kind() {
            ElementKind::Image { source } if is_http_url(source) => {
                if !urls.iter().any(|url| url == source) {
                    urls.push(source.clone());
                }
            }
            ElementKind::YoutubeVideo { id } => {
                let url = youtube_thumbnail_url(id);
                if !urls.contains(&url) {
                    urls.push(url);
                }
            }
            _ => {}
        }
        pending.extend(element.children().iter().rev());
    }
    urls
}

/// Whether a media source is fetchable at all; anything else renders as a
/// link fallback and must never be reported as loading.
#[must_use]
pub fn is_http_url(url: &str) -> bool {
    let lowercase = url.to_ascii_lowercase();
    lowercase.starts_with("http://") || lowercase.starts_with("https://")
}

/// The index into per-level style tables for a `[h1]`–`[h3]` heading,
/// clamped to Steam's range like the tags themselves.
#[must_use]
pub fn heading_index(level: u8) -> usize {
    usize::from(level.saturating_sub(1)).min(2)
}

/// The link policy for every surface that opens [`Event::OpenLink`] targets:
/// only web URLs leave the app, and scheme-less ones are promoted to https.
/// Returns the URL to hand to the OS, or `None` for anything unopenable.
#[must_use]
pub fn normalize_description_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.chars().any(char::is_whitespace) {
        return None;
    }
    if is_http_url(url) {
        let (_, remainder) = url.split_once("://")?;
        return (!remainder.is_empty()).then(|| url.to_owned());
    }
    if url.contains("://") {
        return None;
    }
    Some(format!("https://{url}"))
}

/// Bundled per-render inputs, so the recursion threads one value.
#[derive(Clone, Copy)]
struct Ctx<'a> {
    revealed_spoilers: &'a HashSet<SpoilerId>,
    media: &'a dyn MediaLookup,
    /// Localized renderer copy: the media link fallbacks in both contexts,
    /// and the "Originally posted by …:" quote header the Steam context
    /// reproduces from Steam's own UI.
    i18n: &'a I18n,
    tokens: RenderTokens,
}

/// Renders `document` in the app's own theme, without remote media.
#[must_use]
pub fn view<'a>(
    document: &'a Document,
    revealed_spoilers: &'a HashSet<SpoilerId>,
    tokens: &Tokens,
    i18n: &'a I18n,
) -> Element<'a, Event> {
    render_nodes(
        document.nodes(),
        Ctx {
            revealed_spoilers,
            media: &NoMedia,
            i18n,
            tokens: RenderTokens::from(tokens),
        },
    )
}

/// Renders `document` as the Steam Workshop page renders it, with remote
/// media resolved through `media`. Lay the result out on
/// [`STEAM_PAGE_BACKGROUND`] at [`STEAM_DESCRIPTION_WIDTH`] for full
/// fidelity.
#[must_use]
pub fn steam_view<'a>(
    document: &'a Document,
    revealed_spoilers: &'a HashSet<SpoilerId>,
    media: &'a dyn MediaLookup,
    i18n: &'a I18n,
) -> Element<'a, Event> {
    render_nodes(
        document.nodes(),
        Ctx {
            revealed_spoilers,
            media,
            i18n,
            tokens: RenderTokens::steam(),
        },
    )
}

fn render_nodes<'a>(nodes: &'a [Node], ctx: Ctx<'a>) -> Element<'a, Event> {
    let mut blocks = Column::new().spacing(ctx.tokens.gap_sm);
    let mut inline = Vec::new();

    for node in nodes {
        if let Node::Element(element) = node
            && is_block(element.kind())
        {
            blocks = flush_inline(blocks, &mut inline, ctx);
            blocks = blocks.push(render_block(node, ctx));
        } else {
            inline.push(node);
        }
    }
    flush_inline(blocks, &mut inline, ctx)
        .width(Length::Fill)
        .into()
}

fn flush_inline<'a>(
    blocks: Column<'a, Event>,
    inline: &mut Vec<&'a Node>,
    ctx: Ctx<'a>,
) -> Column<'a, Event> {
    if inline.is_empty() {
        return blocks;
    }
    let mut spans = inline_spans(inline.drain(..), ctx, &InlineStyle::default());
    trim_block_edge_whitespace(&mut spans);
    let paragraphs = compact_paragraphs(spans);
    if paragraphs.is_empty() {
        blocks
    } else {
        blocks.push(paragraphs_view(paragraphs, ctx.tokens.body, ctx.tokens))
    }
}

fn trim_block_edge_whitespace(spans: &mut Vec<StyledSpan>) {
    for span in spans.iter_mut() {
        let trimmed = span.text.trim_start();
        if trimmed.len() != span.text.len() {
            span.text = trimmed.to_owned().into();
        }
        if !span.text.is_empty() {
            break;
        }
    }
    for span in spans.iter_mut().rev() {
        let trimmed = span.text.trim_end();
        if trimmed.len() != span.text.len() {
            span.text = trimmed.to_owned().into();
        }
        if !span.text.is_empty() {
            break;
        }
    }
    spans.retain(|span| !span.text.is_empty());
}

fn compact_paragraphs(spans: Vec<StyledSpan>) -> Vec<Vec<StyledSpan>> {
    let mut paragraphs = Vec::new();
    let mut current = Vec::new();
    let mut pending_newlines = 0_u8;

    for template in spans {
        let mut segment = String::new();
        for character in template.text.chars() {
            match character {
                '\r' => {}
                '\n' => pending_newlines = pending_newlines.saturating_add(1),
                character if pending_newlines > 0 && character.is_whitespace() => {}
                character if pending_newlines > 0 => {
                    if pending_newlines >= 2 {
                        push_styled_text(&mut current, &template, &mut segment);
                        if !current.is_empty() {
                            paragraphs.push(std::mem::take(&mut current));
                        }
                    } else {
                        segment.push('\n');
                    }
                    pending_newlines = 0;
                    segment.push(character);
                }
                character => segment.push(character),
            }
        }
        push_styled_text(&mut current, &template, &mut segment);
    }

    if !current.is_empty() {
        paragraphs.push(current);
    }
    paragraphs
}

fn push_styled_text(current: &mut Vec<StyledSpan>, template: &StyledSpan, text: &mut String) {
    if text.is_empty() {
        return;
    }
    let mut span = template.clone();
    span.text = std::mem::take(text).into();
    current.push(span);
}

fn paragraphs_view<'a>(
    paragraphs: Vec<Vec<StyledSpan>>,
    size: f32,
    tokens: RenderTokens,
) -> Element<'a, Event> {
    let mut content = Column::new().spacing(tokens.gap_xs);
    for paragraph in paragraphs {
        content = content.push(rich_line(paragraph, size, tokens));
    }
    content.width(Length::Fill).into()
}

fn render_block<'a>(node: &'a Node, ctx: Ctx<'a>) -> Element<'a, Event> {
    let Node::Element(element) = node else {
        return render_nodes(std::slice::from_ref(node), ctx);
    };
    let tokens = ctx.tokens;
    match element.kind() {
        ElementKind::Heading(level) => {
            let size = tokens.heading_sizes[heading_index(*level)];
            let style = InlineStyle {
                bold: tokens.heading_bold,
                ..InlineStyle::default()
            };
            let spans = inline_spans(element.children().iter(), ctx, &style);
            rich_line_colored(spans, size, tokens.heading, tokens)
        }
        ElementKind::HorizontalRule => container(Space::new().height(1.0))
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(tokens.divider.into())),
                ..container::Style::default()
            })
            .into(),
        ElementKind::Image { source } => render_image(source, ctx),
        ElementKind::YoutubeVideo { id } => render_youtube(id, ctx),
        ElementKind::List { ordered } => {
            let mut list = Column::new().spacing(tokens.gap_xs);
            let mut index = 0_usize;
            for child in element.children() {
                let Node::Element(item) = child else {
                    continue;
                };
                if !matches!(item.kind(), ElementKind::ListItem) {
                    continue;
                }
                index += 1;
                let marker = if *ordered {
                    format!("{index}.")
                } else {
                    "•".to_owned()
                };
                list = list.push(
                    row![
                        text(marker)
                            .size(tokens.body)
                            .color(Color::from(tokens.marker))
                            .width(Length::Fixed(tokens.marker_width)),
                        render_nodes(item.children(), ctx),
                    ]
                    .spacing(tokens.gap_xs)
                    .width(Length::Fill),
                );
            }
            list.width(Length::Fill).into()
        }
        ElementKind::Quote { author } => render_quote(author.as_deref(), element.children(), ctx),
        ElementKind::Table { bordered, .. } => render_table(element.children(), *bordered, ctx),
        ElementKind::Code => {
            let raw = element
                .children()
                .iter()
                .map(Node::plain_text)
                .collect::<String>();
            container(
                text(raw)
                    .font(Font::MONOSPACE)
                    .size(tokens.code.size)
                    .color(Color::from(tokens.text))
                    .wrapping(text::Wrapping::WordOrGlyph)
                    .width(Length::Fill),
            )
            .padding(tokens.pad_sm)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: tokens.code.bg.map(|bg| Background::Color(bg.into())),
                border: Border {
                    color: tokens.code.border.into(),
                    width: 1.0,
                    radius: border::radius(tokens.radius_base),
                },
                ..container::Style::default()
            })
            .into()
        }
        _ => {
            let spans = inline_spans(std::iter::once(node), ctx, &InlineStyle::default());
            rich_line(spans, tokens.body, tokens)
        }
    }
}

fn render_quote<'a>(
    author: Option<&'a str>,
    children: &'a [Node],
    ctx: Ctx<'a>,
) -> Element<'a, Event> {
    let tokens = ctx.tokens;
    // Steam renders quote bodies at 92% (`blockquote.bb_blockquote`).
    let mut inner_ctx = ctx;
    inner_ctx.tokens.body = tokens.quote.size;

    let author_line = |author_text: String, font: Font| {
        text(author_text)
            .size(tokens.caption)
            .color(Color::from(tokens.text))
            .font(font)
    };
    let body = |author: Option<iced::widget::Text<'a>>| {
        let mut content = Column::new().spacing(tokens.gap_xs);
        if let Some(author) = author {
            content = content.push(author);
        }
        content.push(render_nodes(children, inner_ctx))
    };

    match tokens.quote.shape {
        QuoteShape::Bordered(quote_border) => {
            // Steam's page renders the author as its localized
            // "Originally posted by <author>:" line (`div.bb_quoteauthor`).
            let author = author.map(|author| {
                author_line(
                    ctx.i18n
                        .trn("bbcode-quote-author", &[("author", Arg::Text(author))]),
                    Font {
                        style: font::Style::Italic,
                        ..assets::fonts::default_font()
                    },
                )
            });
            container(body(author))
                .padding(tokens.pad_sm)
                .width(Length::Fill)
                .style(move |_| container::Style {
                    background: tokens.quote.bg.map(|bg| Background::Color(bg.into())),
                    border: Border {
                        color: quote_border.into(),
                        width: 1.0,
                        radius: border::radius(tokens.radius_base),
                    },
                    ..container::Style::default()
                })
                .into()
        }
        QuoteShape::RuleBar(rule_color) => {
            // The app's shape shows the raw author.
            let author = author.map(|author| {
                author_line(
                    author.to_owned(),
                    Font {
                        weight: font::Weight::Semibold,
                        ..assets::fonts::default_font()
                    },
                )
            });
            let rule = container(Space::new().width(3.0))
                .height(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(Background::Color(rule_color.into())),
                    ..container::Style::default()
                });
            container(row![rule, body(author)].spacing(tokens.gap_sm))
                .padding([tokens.pad_sm, tokens.pad_sm])
                .width(Length::Fill)
                .style(move |_| container::Style {
                    background: tokens.quote.bg.map(|bg| Background::Color(bg.into())),
                    border: border::rounded(tokens.radius_base),
                    ..container::Style::default()
                })
                .into()
        }
    }
}

fn render_image<'a>(source: &'a str, ctx: Ctx<'a>) -> Element<'a, Event> {
    let tokens = ctx.tokens;
    match ctx.media.media(source) {
        MediaView::Ready {
            handle,
            width,
            height,
        } if width > 0 && height > 0 => {
            let display_width = precise_f32(width).min(tokens.image_max_width);
            let display_height = precise_f32(height) * display_width / precise_f32(width);
            mouse_area(
                image(handle.clone())
                    .content_fit(ContentFit::Fill)
                    .width(Length::Fixed(display_width))
                    .height(Length::Fixed(display_height)),
            )
            .interaction(iced::mouse::Interaction::Pointer)
            .on_press(Event::OpenLink(source.to_owned()))
            .into()
        }
        MediaView::Loading => container(Space::new())
            .width(Length::Fixed(tokens.image_max_width))
            .height(Length::Fixed(MEDIA_PLACEHOLDER_HEIGHT))
            .style(move |_| container::Style {
                background: Some(Background::Color(tokens.media_placeholder.into())),
                border: border::rounded(tokens.radius_base),
                ..container::Style::default()
            })
            .into(),
        MediaView::Ready { .. } | MediaView::Unavailable | MediaView::Failed => {
            media_link_line(ctx.i18n.tr("bbcode-view-image"), source.to_owned(), tokens)
        }
    }
}

fn render_youtube<'a>(id: &'a str, ctx: Ctx<'a>) -> Element<'a, Event> {
    let tokens = ctx.tokens;
    let watch_url = youtube_watch_url(id);
    let thumbnail_url = youtube_thumbnail_url(id);
    let thumbnail = ctx.media.media(&thumbnail_url);
    if matches!(thumbnail, MediaView::Unavailable) {
        return media_link_line(ctx.i18n.tr("bbcode-watch-youtube"), watch_url, tokens);
    }

    // Steam embeds players at the shared media width (`image_max_width`,
    // the same 630px cap it applies to images) in 16:9.
    let width = tokens.image_max_width;
    let height = width * 9.0 / 16.0;
    let backdrop: Element<'a, Event> = match thumbnail {
        MediaView::Ready { handle, .. } => image(handle.clone())
            .content_fit(ContentFit::Cover)
            .width(Length::Fixed(width))
            .height(Length::Fixed(height))
            .into(),
        _ => container(Space::new())
            .width(Length::Fixed(width))
            .height(Length::Fixed(height))
            .style(|_| container::Style {
                background: Some(Background::Color(Color::BLACK)),
                ..container::Style::default()
            })
            .into(),
    };
    let badge = container(
        svg(assets::icons::play())
            .style(|_, _| svg::Style {
                color: Some(Color::WHITE),
            })
            .width(Length::Fixed(22.0))
            .height(Length::Fixed(22.0)),
    )
    .padding([10.0, 18.0])
    .style(|_| container::Style {
        background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.65))),
        border: border::rounded(6.0),
        ..container::Style::default()
    });

    mouse_area(stack![
        backdrop,
        container(badge)
            .width(Length::Fixed(width))
            .height(Length::Fixed(height))
            .center(Length::Fill),
    ])
    .interaction(iced::mouse::Interaction::Pointer)
    .on_press(Event::OpenLink(watch_url))
    .into()
}

fn media_link_line<'a>(label: String, target: String, tokens: RenderTokens) -> Element<'a, Event> {
    rich_line(
        vec![
            span(label)
                .color(Color::from(tokens.link))
                .underline(true)
                .link(Interaction::OpenLink(target)),
        ],
        tokens.body,
        tokens,
    )
}

/// Lossless u32 → f32 for dimensions (decode limits cap them far below 2^24).
fn precise_f32(value: u32) -> f32 {
    debug_assert!(value < (1 << 24));
    value as f32
}

fn is_block(kind: &ElementKind) -> bool {
    matches!(
        kind,
        ElementKind::Heading(_)
            | ElementKind::HorizontalRule
            | ElementKind::Image { .. }
            | ElementKind::YoutubeVideo { .. }
            | ElementKind::List { .. }
            | ElementKind::Quote { .. }
            | ElementKind::Table { .. }
            | ElementKind::Code
    )
}

fn render_table<'a>(nodes: &'a [Node], bordered: bool, ctx: Ctx<'a>) -> Element<'a, Event> {
    let mut table = Column::new().spacing(0.0).width(Length::Fill);
    for node in nodes {
        let Node::Element(row_element) = node else {
            continue;
        };
        if !matches!(row_element.kind(), ElementKind::TableRow) {
            continue;
        }

        let mut table_row = iced::widget::Row::new().spacing(0.0).width(Length::Fill);
        for cell in row_element.children() {
            let Node::Element(cell_element) = cell else {
                continue;
            };
            let header = matches!(cell_element.kind(), ElementKind::TableHeader);
            if !header && !matches!(cell_element.kind(), ElementKind::TableCell) {
                continue;
            }
            table_row = table_row.push(render_table_cell(
                cell_element.children(),
                header,
                bordered,
                ctx,
            ));
        }
        table = table.push(table_row);
    }
    table.into()
}

fn render_table_cell<'a>(
    nodes: &'a [Node],
    header: bool,
    bordered: bool,
    ctx: Ctx<'a>,
) -> Element<'a, Event> {
    let tokens = ctx.tokens;
    // Steam tables render at 12px (`div.bb_table { font-size: 12px }`).
    let mut inner_ctx = ctx;
    inner_ctx.tokens.body = tokens.table.size;

    let content = if header {
        let style = InlineStyle {
            bold: true,
            ..InlineStyle::default()
        };
        let mut spans = inline_spans(nodes.iter(), inner_ctx, &style);
        trim_block_edge_whitespace(&mut spans);
        paragraphs_view(compact_paragraphs(spans), tokens.table.size, tokens)
    } else {
        render_nodes(nodes, inner_ctx)
    };
    container(content)
        .padding(tokens.pad_xs)
        .width(Length::FillPortion(1))
        .style(move |_| container::Style {
            background: header
                .then_some(tokens.table.th_bg)
                .flatten()
                .map(|bg| Background::Color(bg.into())),
            border: if bordered {
                Border {
                    color: tokens.table.border.into(),
                    width: 1.0,
                    ..Border::default()
                }
            } else {
                Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent inline BBCode styles compose rather than form exclusive states"
)]
#[derive(Clone, Debug, Default)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    interaction: Option<Interaction>,
    spoiler: Option<(SpoilerId, bool)>,
}

fn inline_spans<'a>(
    nodes: impl IntoIterator<Item = &'a Node>,
    ctx: Ctx<'a>,
    style: &InlineStyle,
) -> Vec<StyledSpan> {
    let mut spans = Vec::new();
    for node in nodes {
        collect_inline(node, ctx, style.clone(), &mut spans);
    }
    spans
}

fn collect_inline(node: &Node, ctx: Ctx<'_>, mut style: InlineStyle, spans: &mut Vec<StyledSpan>) {
    let tokens = ctx.tokens;
    match node {
        Node::Text(value) => {
            if value.is_empty() {
                return;
            }
            let mut rendered = span(value.clone());
            if style.bold || style.italic {
                rendered = rendered.font(Font {
                    weight: if style.bold {
                        font::Weight::Bold
                    } else {
                        font::Weight::Normal
                    },
                    style: if style.italic {
                        font::Style::Italic
                    } else {
                        font::Style::Normal
                    },
                    ..assets::fonts::default_font()
                });
            }
            rendered = rendered
                .underline(style.underline)
                .strikethrough(style.strikethrough);

            if let Some((id, revealed)) = style.spoiler {
                if revealed {
                    let interaction = style
                        .interaction
                        .clone()
                        .unwrap_or(Interaction::ToggleSpoiler(id));
                    if tokens.spoiler.reveal_keeps_cover {
                        // Steam reveals white text over the black chip.
                        rendered = rendered
                            .color(Color::from(tokens.spoiler.text))
                            .background(Color::from(tokens.spoiler.cover))
                            .border(border::rounded(tokens.radius_xs))
                            .padding(tokens.spoiler.pad)
                            .link(interaction);
                    } else {
                        if matches!(interaction, Interaction::OpenLink(_)) {
                            rendered = rendered
                                .color(Color::from(tokens.link))
                                .underline(tokens.link_underline || style.underline);
                        }
                        rendered = rendered.link(interaction);
                    }
                } else {
                    let cover = Color::from(tokens.spoiler.cover);
                    rendered = rendered
                        .color(cover)
                        .background(cover)
                        .border(border::rounded(tokens.radius_xs))
                        .padding(tokens.spoiler.pad)
                        .link(Interaction::ToggleSpoiler(id));
                }
            } else if let Some(interaction) = style.interaction {
                rendered = rendered
                    .color(Color::from(tokens.link))
                    .underline(tokens.link_underline || style.underline)
                    .link(interaction);
            }
            spans.push(rendered);
        }
        Node::Element(element) if is_block(element.kind()) => {
            let value = element
                .children()
                .iter()
                .map(Node::plain_text)
                .collect::<String>();
            collect_inline(&Node::Text(value), ctx, style, spans);
        }
        Node::Element(element) => {
            match element.kind() {
                ElementKind::Bold => style.bold = true,
                ElementKind::Underline => style.underline = true,
                ElementKind::Italic => style.italic = true,
                ElementKind::Strikethrough => style.strikethrough = true,
                ElementKind::Spoiler(id) => {
                    style.spoiler = Some((*id, ctx.revealed_spoilers.contains(id)));
                }
                ElementKind::Link { target } => {
                    style.interaction = Some(Interaction::OpenLink(target.clone()));
                }
                _ => {}
            }
            for child in element.children() {
                collect_inline(child, ctx, style.clone(), spans);
            }
        }
    }
}

fn rich_line<'a>(spans: Vec<StyledSpan>, size: f32, tokens: RenderTokens) -> Element<'a, Event> {
    rich_line_colored(spans, size, tokens.text, tokens)
}

fn rich_line_colored<'a>(
    spans: Vec<StyledSpan>,
    size: f32,
    color: Rgba,
    tokens: RenderTokens,
) -> Element<'a, Event> {
    rich_text(spans)
        .on_link_click(Event::from)
        .size(size)
        .line_height(tokens.line_height)
        .color(Color::from(color))
        .width(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_edge_whitespace_does_not_stack_with_column_spacing() {
        let mut spans = vec![span("\n\n"), span("  Content  "), span("\n")];

        trim_block_edge_whitespace(&mut spans);

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "Content");
    }

    #[test]
    fn repeated_newlines_become_compact_paragraphs() {
        let mut spans = vec![span("First\n\nSecond")];

        trim_block_edge_whitespace(&mut spans);
        let paragraphs = compact_paragraphs(spans);

        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0][0].text, "First");
        assert_eq!(paragraphs[1][0].text, "Second");
    }

    #[test]
    fn paragraph_boundaries_can_cross_styled_spans() {
        let paragraphs = compact_paragraphs(vec![span("First\n"), span("\nSecond")]);

        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0][0].text, "First");
        assert_eq!(paragraphs[1][0].text, "Second");
    }

    /// `render_nodes`/`render_block`/`collect_inline` recurse once per
    /// nesting level, so before the parser capped nesting a description
    /// carrying a few thousand nested tags overflowed the stack and aborted
    /// the process — on the UI thread, on the first frame it was shown.
    /// 5000 is well past the measured debug-build limit of roughly 900, so
    /// this only passes while `MAX_NESTING_DEPTH` is enforced.
    #[test]
    fn deeply_nested_source_renders_without_overflowing_the_stack() {
        let depth = 5_000;
        let source = format!("{}deep{}", "[b]".repeat(depth), "[/b]".repeat(depth));
        let document = gmpublished_backend::bbcode::Document::parse(&source);
        let revealed = HashSet::new();
        let tokens = crate::theme::Tokens::dark();
        let i18n = crate::i18n::I18n::from_user_or_system(None);

        let element = view(&document, &revealed, &tokens, &i18n);

        drop(element);
    }

    #[test]
    fn single_newlines_inside_an_inline_run_are_preserved() {
        let mut spans = vec![span("First\nSecond")];

        trim_block_edge_whitespace(&mut spans);
        let paragraphs = compact_paragraphs(spans);

        assert_eq!(paragraphs.len(), 1);
        assert_eq!(paragraphs[0][0].text, "First\nSecond");
    }

    #[test]
    fn steam_view_renders_media_documents_with_and_without_a_lookup() {
        struct EveryStateLookup(image::Handle);
        impl MediaLookup for EveryStateLookup {
            fn media(&self, url: &str) -> MediaView<'_> {
                if url.contains("ytimg") {
                    MediaView::Ready {
                        handle: &self.0,
                        width: 480,
                        height: 360,
                    }
                } else if url.contains("loading") {
                    MediaView::Loading
                } else {
                    MediaView::Failed
                }
            }
        }

        let document = gmpublished_backend::bbcode::Document::parse(
            "[h1]Title[/h1][img]https://example.com/loading.png[/img]\
             [img]https://example.com/broken.png[/img]\
             [previewyoutube=dQw4w9WgXcQ;full][/previewyoutube]\
             [spoiler]secret[/spoiler][quote=author]q[/quote]",
        );
        let revealed = HashSet::new();

        let lookup = EveryStateLookup(image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]));
        let i18n = crate::i18n::I18n::from_user_or_system(None);
        drop(steam_view(&document, &revealed, &lookup, &i18n));
        drop(steam_view(&document, &revealed, &NoMedia, &i18n));
    }

    #[test]
    fn description_links_only_open_safe_web_urls() {
        assert_eq!(
            normalize_description_url(" example.com/page "),
            Some("https://example.com/page".to_owned())
        );
        assert_eq!(
            normalize_description_url("https://example.com"),
            Some("https://example.com".to_owned())
        );
        assert_eq!(normalize_description_url("steam://run/4000"), None);
        assert_eq!(normalize_description_url("not a url"), None);
        assert_eq!(normalize_description_url("https://"), None);
    }

    #[test]
    fn media_urls_reports_http_sources_and_youtube_thumbnails_once() {
        let document = gmpublished_backend::bbcode::Document::parse(
            "[img]https://example.com/a.png[/img]\
             [quote][img]https://example.com/a.png[/img][/quote]\
             [img]C:\\not\\a\\url.png[/img]\
             [previewyoutube=dQw4w9WgXcQ][/previewyoutube]",
        );

        assert_eq!(
            media_urls(&document),
            vec![
                "https://example.com/a.png".to_owned(),
                "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg".to_owned(),
            ]
        );
    }
}
