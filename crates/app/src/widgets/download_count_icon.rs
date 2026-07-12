use iced::widget::svg;
use iced::{ContentFit, Element, Length};

use crate::{assets, theme::Tokens};

pub fn download_count_icon<'a, M: 'a>(tokens: &Tokens, size: f32, opacity: f32) -> Element<'a, M> {
    let color = tokens.colors.download_count_icon.into();

    svg(assets::icons::download_count())
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .content_fit(ContentFit::Contain)
        .opacity(opacity.clamp(0.0, 1.0))
        .style(move |_, _| svg::Style { color: Some(color) })
        .into()
}
