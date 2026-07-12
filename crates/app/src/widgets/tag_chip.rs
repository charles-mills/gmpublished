//! Shared workshop tag chip: a colored label pill with a trailing point,
//! used by the GMA preview metadata and the size analyzer tooltip.

use iced::widget::{container, row, svg, text};
use iced::{Color, Element, Length};

use crate::{assets, theme};

pub const TAG_TEXT_SIZE: f32 = 11.0;
pub const TAG_POINT_WIDTH: f32 = 9.0;

pub fn tag_chip<'a, M: 'a>(label: &str, tokens: &theme::Tokens) -> Element<'a, M> {
    let (background, text_color) = theme::tokens::workshop_tag_colors(label);
    let body = container(
        text(label.to_ascii_lowercase())
            .size(TAG_TEXT_SIZE)
            .color(Color::from(text_color))
            .line_height(1.0),
    )
    .padding([
        (tokens.dims.tag_height - TAG_TEXT_SIZE) / 2.0,
        tokens.spacing.pad_xs,
    ])
    .height(Length::Fixed(tokens.dims.tag_height))
    .style(move |_| theme::styles::tag_chip(background, text_color));

    let point = svg(assets::icons::tag_point())
        .width(Length::Fixed(TAG_POINT_WIDTH))
        .height(Length::Fixed(tokens.dims.tag_height))
        .style(move |_, _| svg::Style {
            color: Some(background.into()),
        });

    row![body, point].into()
}
