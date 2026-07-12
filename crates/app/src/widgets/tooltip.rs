use iced::Color;
use iced::Element;
use iced::widget::{container, text, tooltip};

use crate::theme::{self, Tokens};

const GAP: f32 = 4.0;
const HORIZONTAL_PADDING: f32 = 10.0;
const VERTICAL_PADDING: f32 = 7.0;

pub fn below<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    label: String,
    tokens: &Tokens,
    max_width: f32,
) -> Element<'a, Message> {
    let content = content.into();
    if label.trim().is_empty() {
        return content;
    }
    let tokens = *tokens;

    tooltip(
        content,
        container(
            text(label)
                .size(tokens.typography.body_sm)
                .color(Color::from(tokens.colors.text))
                .wrapping(text::Wrapping::WordOrGlyph),
        )
        .padding([VERTICAL_PADDING, HORIZONTAL_PADDING])
        .max_width(max_width)
        .style(move |_| theme::styles::tooltip(&tokens)),
        tooltip::Position::Bottom,
    )
    .gap(GAP)
    .padding(0)
    .into()
}
