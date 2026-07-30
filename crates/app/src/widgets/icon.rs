//! Shared presentation for monochrome SVG glyphs.

use iced::widget::{Svg, svg};
use iced::{Color, ContentFit, Length};

pub fn svg_icon<'a>(handle: svg::Handle, color: Color, size: f32) -> Svg<'a> {
    svg(handle)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .content_fit(ContentFit::Contain)
        .style(move |_, _| svg::Style { color: Some(color) })
}

pub fn svg_icon_with_opacity<'a>(
    handle: svg::Handle,
    color: Color,
    size: f32,
    opacity: f32,
) -> Svg<'a> {
    svg_icon(handle, color, size).opacity(opacity)
}
