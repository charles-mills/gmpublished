use iced::widget::{Space, container, mouse_area, opaque};
use iced::{Color, Element, Length};

use std::time::Instant;

use crate::{
    theme::{self, Tokens},
    widgets,
};

use super::{Message, State};

pub fn scrim(state: &State, tokens: &Tokens, now: Instant) -> Element<'static, Message> {
    dim_layer(tokens, state.opacity(now), state.interactive())
}

/// Renders the second scrim between the base modal and the Destination
/// Select overlay, keyed to the overlay's own presence. Clicking it closes
/// only the topmost layer.
pub fn overlay_scrim(state: &State, tokens: &Tokens, now: Instant) -> Element<'static, Message> {
    dim_layer(
        tokens,
        state.overlay_opacity(now),
        state.overlay_interactive(),
    )
}

/// Renders the heavy scrim behind an expanded (near-fullscreen) preview,
/// on top of the base scrim; together they black out the ring of app that
/// stays visible around the sheet. Clicking it routes through the same
/// close chain as Esc (collapse first).
pub fn expanded_scrim(state: &State, tokens: &Tokens, now: Instant) -> Element<'static, Message> {
    dim_layer_colored(
        tokens.colors.scrim_expanded,
        state.opacity(now),
        state.interactive(),
    )
}

fn dim_layer(tokens: &Tokens, opacity: f32, interactive: bool) -> Element<'static, Message> {
    dim_layer_colored(tokens.colors.scrim, opacity, interactive)
}

fn dim_layer_colored(
    color: theme::Rgba,
    opacity: f32,
    interactive: bool,
) -> Element<'static, Message> {
    let scrim = theme::motion::scaled_alpha(color, opacity);
    let fill = container(Space::new().width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| iced::widget::container::Style {
            background: Some(Color::from(scrim).into()),
            ..iced::widget::container::Style::default()
        });

    if interactive {
        opaque(mouse_area(fill).on_press(Message::CloseRequested))
    } else {
        fill.into()
    }
}

/// Hosts a modal body layer; bodies are content-sized and center themselves.
pub fn frame<'a, M>(content: Element<'a, M>, scale: f32, interactive: bool) -> Element<'a, M>
where
    M: 'a,
{
    widgets::scaled::scaled(
        container(content).width(Length::Fill).height(Length::Fill),
        scale,
    )
    .interactive(interactive)
    .into()
}
