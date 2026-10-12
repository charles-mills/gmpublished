use iced::border;
use iced::widget::{
    button as button_widget, checkbox as checkbox_widget, container,
    progress_bar as progress_bar_widget, scrollable, text_editor as text_editor_widget, text_input,
};
use iced::{Border, Color, Font, Shadow, Vector, font};

use crate::assets;

use super::tokens::{Rgba, Tokens};

pub fn inter_font(weight: font::Weight) -> Font {
    Font {
        weight,
        ..assets::fonts::default_font()
    }
}

pub fn surface(tokens: &Tokens) -> container::Style {
    container_style(tokens.colors.bg, tokens.colors.text, Border::default())
}

pub fn modal(tokens: &Tokens) -> container::Style {
    container::Style {
        shadow: Shadow {
            color: tokens.colors.shadow_soft.into(),
            offset: Vector::ZERO,
            blur_radius: 10.0,
        },
        ..container_style(
            tokens.colors.modal_bg,
            tokens.colors.text,
            border::rounded(tokens.radii.lg),
        )
    }
}

pub fn preview_modal(tokens: &Tokens) -> container::Style {
    container::Style {
        shadow: Shadow {
            color: tokens.colors.shadow_soft.into(),
            offset: Vector::ZERO,
            blur_radius: 10.0,
        },
        ..container_style(
            tokens.colors.preview_modal_bg,
            tokens.colors.text,
            Border {
                radius: border::radius(tokens.radii.lg),
                ..Border::default()
            },
        )
    }
}

pub fn extract_button(tokens: &Tokens, status: button_widget::Status) -> button_widget::Style {
    let (background, text) = match status {
        button_widget::Status::Active | button_widget::Status::Hovered => {
            (tokens.colors.neutral, tokens.colors.text_on_neutral)
        }
        button_widget::Status::Pressed => {
            (tokens.colors.neutral_dark, tokens.colors.text_on_neutral)
        }
        button_widget::Status::Disabled => (tokens.colors.extract_disabled_bg, tokens.colors.text),
    };
    button_widget::Style {
        background: Some(Color::from(background).into()),
        text_color: text.into(),
        border: Border::default(),
        shadow: Shadow {
            color: tokens.colors.shadow_action.into(),
            offset: Vector::ZERO,
            blur_radius: 5.0,
        },
        snap: true,
    }
}

pub fn preview_extract_button(
    tokens: &Tokens,
    status: button_widget::Status,
) -> button_widget::Style {
    button_widget::Style {
        border: Border {
            radius: border::radius(0.0).top_left(tokens.radii.lg),
            ..Border::default()
        },
        ..extract_button(tokens, status)
    }
}

pub fn preview_sidebar(tokens: &Tokens) -> container::Style {
    container::Style {
        shadow: Shadow {
            color: tokens.colors.shadow_soft.into(),
            offset: Vector::new(2.0, 0.0),
            blur_radius: 10.0,
        },
        ..container_style(
            tokens.colors.surface_muted,
            tokens.colors.text,
            Border {
                radius: border::radius(tokens.radii.lg)
                    .top_right(0.0)
                    .bottom_right(0.0),
                ..Border::default()
            },
        )
    }
}

pub fn preview_image_well(tokens: &Tokens) -> container::Style {
    container::Style {
        shadow: Shadow {
            color: tokens.colors.shadow_strong.into(),
            offset: Vector::ZERO,
            blur_radius: 2.0,
        },
        ..container_style(
            tokens.colors.surface_preview,
            tokens.colors.text,
            Border::default(),
        )
    }
}

pub fn browser_row(tokens: &Tokens, status: button_widget::Status) -> button_widget::Style {
    let background = match status {
        button_widget::Status::Hovered | button_widget::Status::Pressed => {
            Some(Color::from(tokens.colors.surface_muted).into())
        }
        button_widget::Status::Active | button_widget::Status::Disabled => None,
    };
    button_widget::Style {
        background,
        text_color: tokens.colors.text.into(),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn tag_chip(background: Rgba, text: Rgba) -> container::Style {
    container_style(background, text, Border::default())
}

pub fn browser_bar(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.chrome_deep,
        tokens.colors.text,
        Border::default(),
    )
}

pub fn preview_browser_top_bar(tokens: &Tokens) -> container::Style {
    browser_bar_with_radius(tokens, border::radius(0.0).top_right(tokens.radii.lg))
}

pub fn preview_browser_bottom_bar(tokens: &Tokens) -> container::Style {
    browser_bar_with_radius(tokens, border::radius(0.0).bottom_right(tokens.radii.lg))
}

pub fn file_preview_header_bar(tokens: &Tokens, expanded: bool) -> container::Style {
    let radius = if expanded {
        border::radius(tokens.radii.lg)
            .bottom_left(0.0)
            .bottom_right(0.0)
    } else {
        border::radius(0.0).top_right(tokens.radii.lg)
    };
    browser_bar_with_radius(tokens, radius)
}

pub fn file_preview_body_well(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.surface_sunken,
        tokens.colors.text,
        border(tokens, tokens.colors.border, tokens.radii.sm),
    )
}

pub fn file_preview_banner(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.neutral,
        tokens.colors.text_on_neutral,
        Border::default(),
    )
}

pub fn file_preview_speed_readout(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.overlay_panel_bg.with_alpha(220),
        tokens.colors.text,
        border(tokens, tokens.colors.border_subtle, tokens.radii.sm),
    )
}

pub fn sunken_card(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.surface_raised,
        tokens.colors.text,
        border(tokens, tokens.colors.border, tokens.radii.md),
    )
}

pub fn destination_tile(tokens: &Tokens, selected: bool, enabled: bool) -> container::Style {
    let (background, text, edge) = if !enabled {
        (
            tokens.colors.tile_disabled_bg,
            tokens.colors.tile_disabled_text,
            tokens.colors.border_strong,
        )
    } else if selected {
        (
            tokens.colors.destination_input_bg,
            tokens.colors.text,
            tokens.colors.border,
        )
    } else {
        (
            tokens.colors.surface_raised,
            tokens.colors.text,
            tokens.colors.border,
        )
    };
    container::Style {
        shadow: Shadow {
            color: tokens.colors.shadow_soft.into(),
            offset: Vector::ZERO,
            blur_radius: 6.0,
        },
        ..container_style(background, text, border(tokens, edge, tokens.radii.md))
    }
}

pub fn icon_preview_well(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.surface_sunken,
        tokens.colors.text,
        border(tokens, tokens.colors.border, tokens.radii.md),
    )
}

pub fn spinner_bar(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.text_on_neutral,
        tokens.colors.text_on_neutral,
        border::rounded(tokens.radii.xs),
    )
}

/// Use opaque row fills to cover the card's inner border on unshaded rows too.
pub fn striped_row(tokens: &Tokens, shaded: bool) -> container::Style {
    let background = if shaded {
        tokens.colors.row_stripe
    } else {
        tokens.colors.surface_raised
    };
    container_style(background, tokens.colors.text, Border::default())
}

pub fn tooltip(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.tooltip_bg,
        tokens.colors.tooltip_text,
        border(tokens, tokens.colors.tooltip_border, tokens.radii.sm),
    )
}

pub fn select_face(tokens: &Tokens, open: bool) -> container::Style {
    container_style(
        tokens.colors.input_bg,
        tokens.colors.text,
        Border {
            color: tokens.colors.focus_ring.into(),
            width: if open {
                tokens.dims.focus_border_width
            } else {
                0.0
            },
            radius: if open {
                border::top(tokens.radii.sm)
            } else {
                tokens.radii.sm.into()
            },
        },
    )
}

pub fn select_menu(tokens: &Tokens, radius: f32) -> container::Style {
    container::Style {
        shadow: Shadow {
            color: tokens.colors.shadow_dropdown.into(),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 12.0,
        },
        ..container_style(
            tokens.colors.input_bg,
            tokens.colors.text,
            Border {
                color: tokens.colors.focus_ring.into(),
                width: tokens.dims.focus_border_width,
                radius: border::bottom(radius),
            },
        )
    }
}

/// Select option row. Accent fill distinguishes the chosen option from hover;
/// `selected_fill` has too little contrast against dark input backgrounds.
pub fn select_option(
    tokens: &Tokens,
    selected: bool,
    status: button_widget::Status,
) -> button_widget::Style {
    let (background, text) = match (selected, status) {
        (true, _) => (Some(tokens.colors.neutral), tokens.colors.text_on_neutral),
        (false, button_widget::Status::Hovered | button_widget::Status::Pressed) => {
            (Some(tokens.colors.hover_fill), tokens.colors.text)
        }
        (false, _) => (None, tokens.colors.text),
    };

    button_widget::Style {
        background: background.map(|fill| Color::from(fill).into()),
        text_color: text.into(),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn circle(fill: Rgba, radius: f32) -> container::Style {
    container::Style {
        background: Some(Color::from(fill).into()),
        border: border::rounded(radius),
        ..container::Style::default()
    }
}

pub fn tag(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.surface_2,
        tokens.colors.text,
        border(tokens, tokens.colors.border_subtle, tokens.radii.xs),
    )
}

pub fn sidebar(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.sidebar_panel_bg,
        tokens.colors.text,
        border(
            tokens,
            tokens.colors.border_subtle,
            tokens.dims.sidebar_float_radius,
        ),
    )
}

pub fn sidebar_item_button(
    tokens: &Tokens,
    active: bool,
    status: button_widget::Status,
) -> button_widget::Style {
    let background = if active {
        tokens.colors.sidebar_item_hover
    } else {
        match status {
            button_widget::Status::Hovered => tokens.colors.hover_fill_faint,
            button_widget::Status::Pressed => tokens.colors.hover_fill_soft,
            button_widget::Status::Active | button_widget::Status::Disabled => {
                tokens.colors.sidebar_panel_bg.with_alpha(0)
            }
        }
    };

    button_widget::Style {
        background: Some(Color::from(background).into()),
        text_color: if active {
            tokens.colors.link.into()
        } else {
            tokens.colors.text.into()
        },
        border: border(
            tokens,
            tokens.colors.sidebar_panel_bg.with_alpha(0),
            tokens.radii.sm,
        ),
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn job_badge(tokens: &Tokens, opacity: f32) -> container::Style {
    let alpha = super::motion::opacity_byte(opacity);
    container_style(
        tokens.colors.neutral.with_alpha(alpha),
        tokens.colors.text_on_neutral.with_alpha(alpha),
        border(
            tokens,
            tokens.colors.neutral.with_alpha(alpha),
            tokens.radii.md,
        ),
    )
}

pub fn button(tokens: &Tokens, status: button_widget::Status) -> button_widget::Style {
    let background = match status {
        button_widget::Status::Active | button_widget::Status::Hovered => tokens.colors.button_bg,
        button_widget::Status::Pressed => tokens.colors.button_pressed,
        button_widget::Status::Disabled => tokens
            .colors
            .button_bg
            .with_alpha(super::motion::opacity_byte(tokens.dims.disabled_opacity)),
    };

    button_widget::Style {
        background: Some(Color::from(background).into()),
        text_color: tokens.colors.text.into(),
        border: border::rounded(tokens.radii.sm),
        shadow: control_shadow(tokens),
        snap: true,
    }
}

pub fn action_button(tokens: &Tokens, status: button_widget::Status) -> button_widget::Style {
    let (background, text) = match status {
        button_widget::Status::Active | button_widget::Status::Hovered => {
            (tokens.colors.neutral, tokens.colors.text_on_neutral)
        }
        button_widget::Status::Pressed => {
            (tokens.colors.neutral_dark, tokens.colors.text_on_neutral)
        }
        button_widget::Status::Disabled => (tokens.colors.button_bg, tokens.colors.text),
    };

    let shadow = if matches!(status, button_widget::Status::Disabled) {
        Shadow::default()
    } else {
        Shadow {
            color: tokens.colors.shadow_action.into(),
            offset: Vector::ZERO,
            blur_radius: 5.0,
        }
    };

    button_widget::Style {
        background: Some(Color::from(background).into()),
        text_color: text.into(),
        border: border::rounded(tokens.radii.sm),
        shadow,
        snap: true,
    }
}

pub fn ghost_button(tokens: &Tokens, _status: button_widget::Status) -> button_widget::Style {
    button_widget::Style {
        background: None,
        text_color: tokens.colors.text.into(),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn input(tokens: &Tokens, status: text_input::Status) -> text_input::Style {
    let border = match status {
        text_input::Status::Focused { .. } => focus_ring(tokens, tokens.colors.focus_ring),
        text_input::Status::Active | text_input::Status::Hovered | text_input::Status::Disabled => {
            border::rounded(tokens.radii.sm)
        }
    };

    let value = if matches!(status, text_input::Status::Disabled) {
        tokens
            .colors
            .text
            .with_alpha(super::motion::opacity_byte(tokens.dims.disabled_opacity))
    } else {
        tokens.colors.text
    };

    text_input::Style {
        background: Color::from(tokens.colors.input_bg).into(),
        border,
        icon: tokens.colors.icon_muted.into(),
        placeholder: tokens.colors.text_dim.into(),
        value: value.into(),
        selection: tokens.colors.selected_fill.into(),
    }
}

pub fn destination_input(tokens: &Tokens, _status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: Color::from(tokens.colors.destination_input_bg).into(),
        border: Border::default(),
        icon: tokens.colors.icon_muted.into(),
        placeholder: tokens.colors.text_dim.into(),
        value: tokens.colors.text.into(),
        selection: tokens.colors.selected_fill.into(),
    }
}

pub fn input_error(tokens: &Tokens, status: text_input::Status) -> text_input::Style {
    text_input::Style {
        border: focus_ring(tokens, tokens.colors.error),
        ..input(tokens, status)
    }
}

pub fn text_editor(
    tokens: &Tokens,
    status: text_editor_widget::Status,
) -> text_editor_widget::Style {
    let border = match status {
        text_editor_widget::Status::Focused { .. } => focus_ring(tokens, tokens.colors.focus_ring),
        text_editor_widget::Status::Active
        | text_editor_widget::Status::Hovered
        | text_editor_widget::Status::Disabled => border::rounded(tokens.radii.sm),
    };

    text_editor_widget::Style {
        background: Color::from(tokens.colors.input_bg).into(),
        border,
        placeholder: tokens.colors.text_dim.into(),
        value: tokens.colors.text.into(),
        selection: tokens.colors.selected_fill.into(),
    }
}

pub fn checkbox(tokens: &Tokens, status: checkbox_widget::Status) -> checkbox_widget::Style {
    let checked = match status {
        checkbox_widget::Status::Active { is_checked }
        | checkbox_widget::Status::Hovered { is_checked }
        | checkbox_widget::Status::Disabled { is_checked } => is_checked,
    };
    let disabled = matches!(status, checkbox_widget::Status::Disabled { .. });
    let background = if checked {
        tokens.colors.neutral
    } else {
        tokens.colors.control_bg_alt
    };
    let alpha = if disabled {
        super::motion::opacity_byte(tokens.dims.disabled_opacity)
    } else {
        255
    };

    checkbox_widget::Style {
        background: Color::from(background.with_alpha(alpha)).into(),
        icon_color: tokens.colors.text_on_neutral.into(),
        border: border(
            tokens,
            tokens.colors.checkbox_border.with_alpha(alpha),
            tokens.radii.xs,
        ),
        text_color: Some(tokens.colors.text.with_alpha(alpha).into()),
    }
}

pub fn scrollbar(tokens: &Tokens, status: scrollable::Status) -> scrollable::Style {
    let thumb = match status {
        scrollable::Status::Dragged {
            is_horizontal_scrollbar_dragged,
            is_vertical_scrollbar_dragged,
            ..
        } if is_horizontal_scrollbar_dragged || is_vertical_scrollbar_dragged => {
            tokens.colors.scrollbar_grabber_active
        }
        scrollable::Status::Hovered {
            is_horizontal_scrollbar_hovered,
            is_vertical_scrollbar_hovered,
            ..
        } if is_horizontal_scrollbar_hovered || is_vertical_scrollbar_hovered => {
            tokens.colors.scrollbar_grabber_hover
        }
        scrollable::Status::Active { .. } => tokens.colors.scrollbar_grabber,
        scrollable::Status::Hovered { .. } | scrollable::Status::Dragged { .. } => {
            tokens.colors.scrollbar_grabber
        }
    };
    let rail = scrollable::Rail {
        background: Some(Color::from(tokens.colors.scrollbar_rail).into()),
        border: border::rounded(tokens.dims.scrollbar_thumb_width / 2.0),
        scroller: scrollable::Scroller {
            background: Color::from(thumb).into(),
            border: border::rounded(tokens.dims.scrollbar_thumb_width / 2.0),
        },
    };

    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: Color::from(tokens.colors.overlay_fill).into(),
            border: border(tokens, tokens.colors.border_subtle, tokens.radii.sm),
            shadow: shadow(tokens.colors.shadow_soft),
            icon: tokens.colors.text.into(),
        },
    }
}

/// Select scrollbar colors derive from panel text for contrast against
/// `input_bg`; shared surface grabber tokens are too faint here.
pub fn select_menu_scrollbar(tokens: &Tokens, status: scrollable::Status) -> scrollable::Style {
    let opacity = match status {
        scrollable::Status::Dragged {
            is_horizontal_scrollbar_dragged,
            is_vertical_scrollbar_dragged,
            ..
        } if is_horizontal_scrollbar_dragged || is_vertical_scrollbar_dragged => 0.55,
        scrollable::Status::Hovered {
            is_horizontal_scrollbar_hovered,
            is_vertical_scrollbar_hovered,
            ..
        } if is_horizontal_scrollbar_hovered || is_vertical_scrollbar_hovered => 0.40,
        scrollable::Status::Active { .. } | scrollable::Status::Hovered { .. } => 0.25,
        scrollable::Status::Dragged { .. } => 0.25,
    };

    let rail = scrollable::Rail {
        background: None,
        border: border::rounded(tokens.dims.scrollbar_thumb_width / 2.0),
        scroller: scrollable::Scroller {
            background: Color::from(
                tokens
                    .colors
                    .text
                    .with_alpha(super::motion::opacity_byte(opacity)),
            )
            .into(),
            border: border::rounded(tokens.dims.scrollbar_thumb_width / 2.0),
        },
    };

    scrollable::Style {
        vertical_rail: rail,
        horizontal_rail: rail,
        ..scrollbar(tokens, status)
    }
}

pub fn vertical_scrollbar(tokens: &Tokens) -> scrollable::Scrollbar {
    scrollable::Scrollbar::new()
        .width(tokens.dims.scrollbar_thumb_width)
        .scroller_width(tokens.dims.scrollbar_thumb_width)
        .margin(tokens.dims.scrollbar_track_inset / 2.0)
        .spacing(0.0)
}

pub fn hidden_vertical_scrollbar() -> scrollable::Scrollbar {
    scrollable::Scrollbar::new()
        .width(0.0)
        .scroller_width(0.0)
        .margin(0.0)
        .spacing(0.0)
}

pub fn vertical_scrollbar_reserved_width(tokens: &Tokens) -> f32 {
    tokens.dims.scrollbar_thumb_width + tokens.dims.scrollbar_track_inset
}

pub fn progress_bar(tokens: &Tokens) -> progress_bar_widget::Style {
    progress_bar_widget::Style {
        background: Color::from(tokens.colors.control_bg_alt).into(),
        bar: Color::from(tokens.colors.neutral).into(),
        border: border(tokens, tokens.colors.border, tokens.radii.sm),
    }
}

fn container_style(background: Rgba, text: Rgba, border: Border) -> container::Style {
    container::Style {
        text_color: Some(text.into()),
        background: Some(Color::from(background).into()),
        border,
        shadow: Shadow::default(),
        snap: true,
    }
}

fn browser_bar_with_radius(tokens: &Tokens, radius: border::Radius) -> container::Style {
    container_style(
        tokens.colors.chrome_deep,
        tokens.colors.text,
        Border {
            radius,
            ..Border::default()
        },
    )
}

fn border(tokens: &Tokens, color: Rgba, radius: f32) -> Border {
    Border {
        color: color.into(),
        width: tokens.dims.border_width,
        radius: radius.into(),
    }
}

fn focus_ring(tokens: &Tokens, color: Rgba) -> Border {
    Border {
        color: color.into(),
        width: tokens.dims.focus_border_width,
        radius: tokens.radii.sm.into(),
    }
}

fn control_shadow(tokens: &Tokens) -> Shadow {
    Shadow {
        color: tokens.colors.shadow_control.into(),
        offset: Vector::ZERO,
        blur_radius: 2.0,
    }
}

fn shadow(color: Rgba) -> Shadow {
    Shadow {
        color: color.into(),
        offset: Vector::new(0.0, 2.0),
        blur_radius: 12.0,
    }
}

#[cfg(test)]
mod tests {
    use iced::Color;
    use iced::widget::button;

    use super::super::Tokens;
    use crate::theme::styles;

    #[test]
    fn select_menu_rounds_only_the_edge_away_from_its_control() {
        let tokens = Tokens::dark();
        let style = styles::select_menu(&tokens, tokens.radii.lg);

        assert_eq!(
            style.background,
            Some(Color::from(tokens.colors.input_bg).into())
        );
        assert_eq!(style.border.radius.top_left, 0.0);
        assert_eq!(style.border.radius.top_right, 0.0);
        assert_eq!(style.border.radius.bottom_left, tokens.radii.lg);
        assert_eq!(style.border.radius.bottom_right, tokens.radii.lg);
    }

    #[test]
    fn open_select_face_squares_the_seam_and_lights_the_focus_ring() {
        let tokens = Tokens::dark();
        let open = styles::select_face(&tokens, true);
        let closed = styles::select_face(&tokens, false);

        assert_eq!(open.border.width, tokens.dims.focus_border_width);
        assert_eq!(open.border.radius.bottom_left, 0.0);
        assert_eq!(open.border.radius.top_left, tokens.radii.sm);
        assert_eq!(closed.border.width, 0.0);
        assert_eq!(closed.border.radius.bottom_left, tokens.radii.sm);
    }

    #[test]
    fn the_chosen_option_keeps_its_accent_fill_while_the_pointer_moves() {
        let tokens = Tokens::dark();
        let selected = styles::select_option(&tokens, true, button::Status::Active);
        let hovered = styles::select_option(&tokens, false, button::Status::Hovered);
        let idle = styles::select_option(&tokens, false, button::Status::Active);

        assert_eq!(
            selected.background,
            Some(Color::from(tokens.colors.neutral).into())
        );
        assert_eq!(selected.text_color, tokens.colors.text_on_neutral.into());
        assert_eq!(
            hovered.background,
            Some(Color::from(tokens.colors.hover_fill).into())
        );
        assert_eq!(idle.background, None);
    }

    #[test]
    fn the_chosen_option_outreads_a_hovered_one_in_every_theme() {
        for variant in [
            super::super::ThemeVariant::Dark,
            super::super::ThemeVariant::Light,
            super::super::ThemeVariant::ClassicSource,
        ] {
            let tokens = Tokens::for_variant(variant);
            let selected = styles::select_option(&tokens, true, button::Status::Active);
            let panel = styles::select_menu(&tokens, tokens.radii.lg);

            assert_ne!(
                selected.background, panel.background,
                "{variant:?}: the chosen option is indistinguishable from the panel",
            );
        }
    }
}
