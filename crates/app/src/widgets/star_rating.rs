use iced::widget::{row, svg};
use iced::{Center, ContentFit, Element, Length};

use crate::{assets, theme::Tokens};

const STAR_COUNT: usize = 5;

pub fn star_rating<'a, M: 'a>(bucket: i32, tokens: &Tokens, opacity: f32) -> Element<'a, M> {
    let bucket = bucket.clamp(0, STAR_COUNT as i32) as usize;
    let star_size = tokens.dims.star_rating_height;
    let spacing = ((tokens.dims.star_rating_width - star_size * STAR_COUNT as f32)
        / (STAR_COUNT - 1) as f32)
        .max(0.0);
    let opacity = opacity.clamp(0.0, 1.0);
    let filled_color = tokens.colors.star_rating_filled.into();
    let empty_color = tokens.colors.star_rating_empty.into();

    let mut stars = row![]
        .align_y(Center)
        .spacing(spacing)
        .width(Length::Fixed(tokens.dims.star_rating_width))
        .height(Length::Fixed(tokens.dims.star_rating_height));

    for index in 0..STAR_COUNT {
        let color = if index < bucket {
            filled_color
        } else {
            empty_color
        };

        stars = stars.push(
            svg(assets::icons::star_filled())
                .width(Length::Fixed(star_size))
                .height(Length::Fixed(star_size))
                .content_fit(ContentFit::Contain)
                .opacity(opacity)
                .style(move |_, _| svg::Style { color: Some(color) }),
        );
    }

    stars.into()
}
