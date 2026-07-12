use iced::widget::{column, image};
use iced::{ContentFit, Length, Shadow, Vector};

use super::{
    AddonDragState, Element, Mode, RootMessage, Space, SystemColorScheme, ThemePreset, Tokens,
    container, effective_theme_preset, row, text, theme,
};

const DRAG_GHOST_EDGE: f32 = 48.0;
const DRAG_GHOST_CURSOR_OFFSET: f32 = 10.0;

pub(super) fn addon_drag_ghost<'a>(
    drag: &'a AddonDragState,
    tokens: &Tokens,
) -> Element<'a, RootMessage> {
    let Some(cursor) = drag.cursor() else {
        return Space::new().width(0.0).height(0.0).into();
    };
    let x = (cursor.x + DRAG_GHOST_CURSOR_OFFSET).max(0.0).round();
    let y = (cursor.y + DRAG_GHOST_CURSOR_OFFSET).max(0.0).round();
    let tokens = *tokens;
    let ghost: Element<'a, RootMessage> = if let Some(handle) = drag.thumbnail() {
        container(
            image(handle.clone())
                .width(DRAG_GHOST_EDGE)
                .height(DRAG_GHOST_EDGE)
                .content_fit(ContentFit::Cover)
                .border_radius(tokens.radii.base),
        )
        .padding(2.0)
        .style(move |_| {
            let mut style = theme::styles::tag(&tokens);
            style.shadow = Shadow {
                color: tokens.colors.shadow_strong.into(),
                offset: Vector::new(0.0, 2.0),
                blur_radius: 8.0,
            };
            style
        })
        .into()
    } else {
        container(text("Workshop item").size(tokens.typography.caption))
            .padding([tokens.spacing.pad_xs, tokens.spacing.pad_sm])
            .style(move |_| theme::styles::tag(&tokens))
            .into()
    };

    container(column![
        Space::new().height(Length::Fixed(y)),
        row![Space::new().width(Length::Fixed(x)), ghost]
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

pub(super) fn resolve_tokens(
    theme_preset: ThemePreset,
    system_scheme: SystemColorScheme,
    accent_inputs: theme::AccentInputs,
) -> Tokens {
    Tokens::from_effective(
        effective_theme_preset(theme_preset, system_scheme),
        accent_inputs,
    )
}

pub(super) fn system_scheme_from_mode(mode: Mode) -> SystemColorScheme {
    match mode {
        Mode::Light => SystemColorScheme::Light,
        Mode::None | Mode::Dark => SystemColorScheme::Dark,
    }
}
