use std::collections::HashSet;

use gmpublished_backend::bbcode::{Document, ElementKind, Node, SpoilerId};
use iced::font;
use iced::widget::{Column, Space, container, rich_text, row, span, text};
use iced::{Background, Border, Color, Element, Font, Length, border};

use crate::assets;
use crate::theme::{Rgba, Tokens};

type StyledSpan = iced::widget::text::Span<'static, Interaction>;

#[derive(Clone, Copy)]
struct RenderTokens {
    gap_xs: f32,
    gap_sm: f32,
    pad: f32,
    pad_xs: f32,
    pad_sm: f32,
    caption: f32,
    body_sm: f32,
    body_lg: f32,
    title_sm: f32,
    title: f32,
    radius_xs: f32,
    radius_base: f32,
    divider: Rgba,
    text_dim: Rgba,
    link: Rgba,
    border: Rgba,
    border_strong: Rgba,
    surface_muted: Rgba,
    surface_sunken: Rgba,
    surface_raised: Rgba,
}

impl From<&Tokens> for RenderTokens {
    fn from(tokens: &Tokens) -> Self {
        Self {
            gap_xs: tokens.spacing.gap_xs,
            gap_sm: tokens.spacing.gap_sm,
            pad: tokens.spacing.pad,
            pad_xs: tokens.spacing.pad_xs,
            pad_sm: tokens.spacing.pad_sm,
            caption: tokens.typography.caption,
            body_sm: tokens.typography.body_sm,
            body_lg: tokens.typography.body_lg,
            title_sm: tokens.typography.title_sm,
            title: tokens.typography.title,
            radius_xs: tokens.radii.xs,
            radius_base: tokens.radii.base,
            divider: tokens.colors.divider,
            text_dim: tokens.colors.text_dim,
            link: tokens.colors.link,
            border: tokens.colors.border,
            border_strong: tokens.colors.border_strong,
            surface_muted: tokens.colors.surface_muted,
            surface_sunken: tokens.colors.surface_sunken,
            surface_raised: tokens.colors.surface_raised,
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

#[must_use]
pub fn view<'a>(
    document: &'a Document,
    revealed_spoilers: &'a HashSet<SpoilerId>,
    tokens: &Tokens,
) -> Element<'a, Event> {
    render_nodes(
        document.nodes(),
        revealed_spoilers,
        RenderTokens::from(tokens),
    )
}

fn render_nodes<'a>(
    nodes: &'a [Node],
    revealed_spoilers: &'a HashSet<SpoilerId>,
    tokens: RenderTokens,
) -> Element<'a, Event> {
    let mut blocks = Column::new().spacing(tokens.gap_sm);
    let mut inline = Vec::new();

    for node in nodes {
        if let Node::Element(element) = node
            && is_block(element.kind())
        {
            blocks = flush_inline(blocks, &mut inline, revealed_spoilers, tokens);
            blocks = blocks.push(render_block(node, revealed_spoilers, tokens));
        } else {
            inline.push(node);
        }
    }
    flush_inline(blocks, &mut inline, revealed_spoilers, tokens)
        .width(Length::Fill)
        .into()
}

fn flush_inline<'a>(
    blocks: Column<'a, Event>,
    inline: &mut Vec<&'a Node>,
    revealed_spoilers: &'a HashSet<SpoilerId>,
    tokens: RenderTokens,
) -> Column<'a, Event> {
    if inline.is_empty() {
        return blocks;
    }
    let mut spans = inline_spans(
        inline.drain(..),
        revealed_spoilers,
        tokens,
        &InlineStyle::default(),
    );
    trim_block_edge_whitespace(&mut spans);
    let paragraphs = compact_paragraphs(spans);
    if paragraphs.is_empty() {
        blocks
    } else {
        blocks.push(paragraphs_view(paragraphs, tokens.body_sm, tokens))
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

fn render_block<'a>(
    node: &'a Node,
    revealed_spoilers: &'a HashSet<SpoilerId>,
    tokens: RenderTokens,
) -> Element<'a, Event> {
    let Node::Element(element) = node else {
        return render_nodes(std::slice::from_ref(node), revealed_spoilers, tokens);
    };
    match element.kind() {
        ElementKind::Heading(level) => {
            let size = match level {
                1 => tokens.title,
                2 => tokens.title_sm,
                _ => tokens.body_lg,
            };
            let style = InlineStyle {
                bold: true,
                ..InlineStyle::default()
            };
            let spans = inline_spans(element.children().iter(), revealed_spoilers, tokens, &style);
            rich_line(spans, size, tokens)
        }
        ElementKind::HorizontalRule => container(Space::new().height(1.0))
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(tokens.divider.into())),
                ..container::Style::default()
            })
            .into(),
        ElementKind::Image { source } => rich_line(
            vec![
                span("View image ↗")
                    .color(Color::from(tokens.link))
                    .underline(true)
                    .link(Interaction::OpenLink(source.clone())),
            ],
            tokens.body_sm,
            tokens,
        ),
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
                            .size(tokens.body_sm)
                            .color(Color::from(tokens.text_dim))
                            .width(Length::Fixed(tokens.pad)),
                        render_nodes(item.children(), revealed_spoilers, tokens),
                    ]
                    .spacing(tokens.gap_xs)
                    .width(Length::Fill),
                );
            }
            list.width(Length::Fill).into()
        }
        ElementKind::Quote { author } => {
            let mut content = Column::new().spacing(tokens.gap_xs);
            if let Some(author) = author {
                content = content.push(
                    text(author.clone())
                        .size(tokens.caption)
                        .font(Font {
                            weight: font::Weight::Semibold,
                            ..assets::fonts::default_font()
                        })
                        .color(Color::from(tokens.text_dim)),
                );
            }
            content = content.push(render_nodes(element.children(), revealed_spoilers, tokens));
            let rule = container(Space::new().width(3.0))
                .height(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(Background::Color(tokens.border_strong.into())),
                    ..container::Style::default()
                });
            container(row![rule, content].spacing(tokens.gap_sm))
                .padding([tokens.pad_sm, tokens.pad_sm])
                .width(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(Background::Color(tokens.surface_muted.into())),
                    border: border::rounded(tokens.radius_base),
                    ..container::Style::default()
                })
                .into()
        }
        ElementKind::Table { bordered, .. } => {
            render_table(element.children(), *bordered, revealed_spoilers, tokens)
        }
        ElementKind::Code => {
            let raw = element
                .children()
                .iter()
                .map(Node::plain_text)
                .collect::<String>();
            container(
                text(raw)
                    .font(Font::MONOSPACE)
                    .size(tokens.caption)
                    .width(Length::Fill),
            )
            .padding(tokens.pad_sm)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(tokens.surface_sunken.into())),
                border: Border {
                    color: tokens.border.into(),
                    width: 1.0,
                    radius: border::radius(tokens.radius_base),
                },
                ..container::Style::default()
            })
            .into()
        }
        _ => {
            let spans = inline_spans(
                std::iter::once(node),
                revealed_spoilers,
                tokens,
                &InlineStyle::default(),
            );
            rich_line(spans, tokens.body_sm, tokens)
        }
    }
}

fn is_block(kind: &ElementKind) -> bool {
    matches!(
        kind,
        ElementKind::Heading(_)
            | ElementKind::HorizontalRule
            | ElementKind::Image { .. }
            | ElementKind::List { .. }
            | ElementKind::Quote { .. }
            | ElementKind::Table { .. }
            | ElementKind::Code
    )
}

fn render_table<'a>(
    nodes: &'a [Node],
    bordered: bool,
    revealed_spoilers: &'a HashSet<SpoilerId>,
    tokens: RenderTokens,
) -> Element<'a, Event> {
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
                revealed_spoilers,
                tokens,
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
    revealed_spoilers: &'a HashSet<SpoilerId>,
    tokens: RenderTokens,
) -> Element<'a, Event> {
    let content = if header {
        let style = InlineStyle {
            bold: true,
            ..InlineStyle::default()
        };
        let mut spans = inline_spans(nodes.iter(), revealed_spoilers, tokens, &style);
        trim_block_edge_whitespace(&mut spans);
        paragraphs_view(compact_paragraphs(spans), tokens.body_sm, tokens)
    } else {
        render_nodes(nodes, revealed_spoilers, tokens)
    };
    container(content)
        .padding(tokens.pad_xs)
        .width(Length::FillPortion(1))
        .style(move |_| container::Style {
            background: header.then(|| Background::Color(tokens.surface_muted.into())),
            border: if bordered {
                Border {
                    color: tokens.border.into(),
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
    revealed_spoilers: &HashSet<SpoilerId>,
    tokens: RenderTokens,
    style: &InlineStyle,
) -> Vec<StyledSpan> {
    let mut spans = Vec::new();
    for node in nodes {
        collect_inline(node, revealed_spoilers, tokens, style.clone(), &mut spans);
    }
    spans
}

fn collect_inline(
    node: &Node,
    revealed_spoilers: &HashSet<SpoilerId>,
    tokens: RenderTokens,
    mut style: InlineStyle,
    spans: &mut Vec<StyledSpan>,
) {
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
                    if matches!(interaction, Interaction::OpenLink(_)) {
                        rendered = rendered.color(Color::from(tokens.link)).underline(true);
                    }
                    rendered = rendered.link(interaction);
                } else {
                    let cover = Color::from(tokens.surface_raised);
                    rendered = rendered
                        .color(cover)
                        .background(cover)
                        .border(border::rounded(tokens.radius_xs))
                        .padding([1.0, 2.0])
                        .link(Interaction::ToggleSpoiler(id));
                }
            } else if let Some(interaction) = style.interaction {
                rendered = rendered
                    .color(Color::from(tokens.link))
                    .underline(true)
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
            collect_inline(&Node::Text(value), revealed_spoilers, tokens, style, spans);
        }
        Node::Element(element) => {
            match element.kind() {
                ElementKind::Bold => style.bold = true,
                ElementKind::Underline => style.underline = true,
                ElementKind::Italic => style.italic = true,
                ElementKind::Strikethrough => style.strikethrough = true,
                ElementKind::Spoiler(id) => {
                    style.spoiler = Some((*id, revealed_spoilers.contains(id)));
                }
                ElementKind::Link { target } => {
                    style.interaction = Some(Interaction::OpenLink(target.clone()));
                }
                _ => {}
            }
            for child in element.children() {
                collect_inline(child, revealed_spoilers, tokens, style.clone(), spans);
            }
        }
    }
}

fn rich_line<'a>(spans: Vec<StyledSpan>, size: f32, tokens: RenderTokens) -> Element<'a, Event> {
    rich_text(spans)
        .on_link_click(Event::from)
        .size(size)
        .line_height(1.35)
        .color(Color::from(tokens.text_dim))
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

    #[test]
    fn single_newlines_inside_an_inline_run_are_preserved() {
        let mut spans = vec![span("First\nSecond")];

        trim_block_edge_whitespace(&mut spans);
        let paragraphs = compact_paragraphs(spans);

        assert_eq!(paragraphs.len(), 1);
        assert_eq!(paragraphs[0][0].text, "First\nSecond");
    }
}
