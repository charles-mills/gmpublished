use iced::border;
use iced::widget::{
    button as button_widget, checkbox as checkbox_widget, container,
    progress_bar as progress_bar_widget, scrollable, svg, text_editor as text_editor_widget,
    text_input,
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

pub fn card(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.surface,
        tokens.colors.text,
        border(tokens, tokens.colors.border, tokens.radii.base),
    )
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
            border::rounded(tokens.radii.md),
        )
    }
}

/// Chromeless preview modal: the outer shell owns the rounded silhouette.
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
                radius: border::radius(tokens.radii.md),
                ..Border::default()
            },
        )
    }
}

/// Square-cornered accent Extract button capping the preview sidebar.
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

/// Extract button used as the top-left cap of the Preview GMA sidebar.
pub fn preview_extract_button(
    tokens: &Tokens,
    status: button_widget::Status,
) -> button_widget::Style {
    button_widget::Style {
        border: Border {
            radius: border::radius(0.0).top_left(tokens.radii.md),
            ..Border::default()
        },
        ..extract_button(tokens, status)
    }
}

/// Preview sidebar surface with its right drop shadow.
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
                radius: border::radius(tokens.radii.md)
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
    browser_bar_with_radius(tokens, border::radius(0.0).top_right(tokens.radii.md))
}

pub fn preview_browser_bottom_bar(tokens: &Tokens) -> container::Style {
    browser_bar_with_radius(tokens, border::radius(0.0).bottom_right(tokens.radii.md))
}

pub fn file_preview_header_bar(tokens: &Tokens, expanded: bool) -> container::Style {
    let radius = if expanded {
        border::radius(tokens.radii.md)
            .bottom_left(0.0)
            .bottom_right(0.0)
    } else {
        border::radius(0.0).top_right(tokens.radii.md)
    };
    browser_bar_with_radius(tokens, radius)
}

pub fn file_preview_body_well(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.surface_sunken,
        tokens.colors.text,
        border(tokens, tokens.colors.border, tokens.radii.base),
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
        border(tokens, tokens.colors.border_subtle, tokens.radii.base),
    )
}

pub fn sunken_card(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.surface_raised,
        tokens.colors.text,
        border(tokens, tokens.colors.border, tokens.radii.lg),
    )
}

/// Destination Select chooser tile: a 7rem sunken card that darkens to the
/// input well while selected and bakes a `brightness(.5)` dim into opaque
/// colors while disabled.
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
        ..container_style(background, text, border(tokens, edge, tokens.radii.lg))
    }
}

pub fn icon_preview_well(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.surface_sunken,
        tokens.colors.text,
        border(tokens, tokens.colors.border, tokens.radii.lg),
    )
}

pub fn spinner_bar(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.text_on_neutral,
        tokens.colors.text_on_neutral,
        border::rounded(tokens.radii.xs),
    )
}

/// Alternating stripe fill for dense list rows inside sunken cards.
///
/// The "unshaded" rows are painted opaque with the sunken card's own
/// background (rather than a transparent fill) so they fully occlude the
/// card's inner border on every row; a transparent fill let that border
/// bleed through as a visible seam on alternating rows.
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
        border(tokens, tokens.colors.tooltip_border, tokens.radii.base),
    )
}

/// A select control's own face. `open` squares the bottom corners and lights
/// the focus ring, so the menu below reads as part of the same control.
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
                border::top(tokens.radii.base)
            } else {
                tokens.radii.base.into()
            },
        },
    )
}

/// Floating panel behind a select menu's option list.
///
/// It carries the control's own fill and focus ring so the pair reads as one
/// slab, and only the corners away from the join are rounded.
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

/// One option row inside a select menu. The chosen option keeps the accent fill
/// while the pointer moves, so the menu always shows what is currently set.
///
/// The accent is what makes that legible: `selected_fill` sits within 3% of the
/// panel's own `input_bg` on the dark themes — and *darker* than the hover fill —
/// so the current option would read as fainter than whatever the pointer
/// happens to be over. Hover stays theme-directional (`hover_fill` lightens on
/// the dark themes, darkens on the light one); the `row_*` fills are black in
/// every theme and would vanish into a dark panel.
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

pub fn context_menu(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.menu_bg,
        tokens.colors.text,
        border(tokens, tokens.colors.border_subtle, tokens.radii.base),
    )
}

/// Flat filled disc: no border, no shadow, radius half the edge.
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

pub fn avatar(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.surface_2,
        tokens.colors.text,
        border(
            tokens,
            tokens.colors.border_subtle,
            tokens.dims.avatar_size / 2.0,
        ),
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
            tokens.radii.base,
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
            tokens.radii.lg,
        ),
    )
}

pub fn select(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.dropdown_bg,
        tokens.colors.text,
        border(tokens, tokens.colors.border, tokens.radii.base),
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
        border: border::rounded(tokens.radii.base),
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

    button_widget::Style {
        background: Some(Color::from(background).into()),
        text_color: text.into(),
        border: border::rounded(tokens.radii.base),
        shadow: Shadow {
            color: tokens.colors.shadow_action.into(),
            offset: Vector::ZERO,
            blur_radius: 5.0,
        },
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
            border::rounded(tokens.radii.base)
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

/// Destination Select path input: borderless square dark well with no focus
/// ring.
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
        | text_editor_widget::Status::Disabled => border::rounded(tokens.radii.base),
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
            border: border(tokens, tokens.colors.border_subtle, tokens.radii.base),
            shadow: shadow(tokens.colors.shadow_soft),
            icon: tokens.colors.text.into(),
        },
    }
}

pub fn vertical_scrollbar(tokens: &Tokens) -> scrollable::Scrollbar {
    scrollable::Scrollbar::new()
        .width(tokens.dims.scrollbar_thumb_width)
        .scroller_width(tokens.dims.scrollbar_thumb_width)
        .margin(tokens.dims.scrollbar_track_inset / 2.0)
        .spacing(0.0)
}

/// Zero-width scrollbar for lists that scroll without visible chrome.
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
        border: border(tokens, tokens.colors.border, tokens.radii.base),
    }
}

pub fn svg_icon(tokens: &Tokens, status: svg::Status) -> svg::Style {
    let color = match status {
        svg::Status::Idle => tokens.colors.icon_muted,
        svg::Status::Hovered => tokens.colors.text,
    };
    svg::Style {
        color: Some(color.into()),
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
        radius: tokens.radii.base.into(),
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
    use iced::widget::{button, checkbox, scrollable, text_input};

    use super::{super::Tokens, input};
    use crate::theme::styles;

    #[test]
    fn button_style_uses_pressed_token() {
        let tokens = Tokens::dark();
        let style = styles::button(&tokens, button::Status::Pressed);

        assert_eq!(
            style.background,
            Some(Color::from(tokens.colors.button_pressed).into())
        );
        assert_eq!(style.text_color, tokens.colors.text.into());
    }

    #[test]
    fn active_sidebar_item_uses_active_tokens() {
        let tokens = Tokens::dark();
        let style = styles::sidebar_item_button(&tokens, true, button::Status::Active);

        assert_eq!(
            style.background,
            Some(Color::from(tokens.colors.sidebar_item_hover).into())
        );
        assert_eq!(style.text_color, tokens.colors.link.into());
    }

    #[test]
    fn inactive_sidebar_item_hover_uses_faint_hover_fill() {
        let tokens = Tokens::dark();
        let style = styles::sidebar_item_button(&tokens, false, button::Status::Hovered);

        assert_eq!(
            style.background,
            Some(Color::from(tokens.colors.hover_fill_faint).into())
        );
        assert_eq!(style.text_color, tokens.colors.text.into());
    }

    #[test]
    fn input_focus_uses_focus_ring_border() {
        let tokens = Tokens::dark();
        let style = input(&tokens, text_input::Status::Focused { is_hovered: false });

        assert_eq!(style.border.color, tokens.colors.focus_ring.into());
        assert_eq!(style.border.width, tokens.dims.focus_border_width);
    }

    #[test]
    fn idle_input_has_no_visible_border() {
        let tokens = Tokens::dark();
        let style = input(&tokens, text_input::Status::Active);

        assert_eq!(style.border.width, 0.0);
        assert_eq!(style.background, Color::from(tokens.colors.input_bg).into());
    }

    #[test]
    fn error_input_always_shows_the_error_ring() {
        let tokens = Tokens::dark();
        let style = styles::input_error(&tokens, text_input::Status::Active);

        assert_eq!(style.border.color, tokens.colors.error.into());
        assert_eq!(style.border.width, tokens.dims.focus_border_width);
    }

    #[test]
    fn action_button_greys_out_when_disabled() {
        let tokens = Tokens::dark();

        let active = styles::action_button(&tokens, button::Status::Active);
        let disabled = styles::action_button(&tokens, button::Status::Disabled);

        assert_eq!(
            active.background,
            Some(Color::from(tokens.colors.neutral).into())
        );
        assert_eq!(
            disabled.background,
            Some(Color::from(tokens.colors.button_bg).into())
        );
    }

    #[test]
    fn select_menu_rounds_only_the_edge_away_from_its_control() {
        let tokens = Tokens::dark();
        let style = styles::select_menu(&tokens, tokens.radii.md);

        assert_eq!(
            style.background,
            Some(Color::from(tokens.colors.input_bg).into())
        );
        assert_eq!(style.border.radius.top_left, 0.0);
        assert_eq!(style.border.radius.top_right, 0.0);
        assert_eq!(style.border.radius.bottom_left, tokens.radii.md);
        assert_eq!(style.border.radius.bottom_right, tokens.radii.md);
    }

    #[test]
    fn open_select_face_squares_the_seam_and_lights_the_focus_ring() {
        let tokens = Tokens::dark();
        let open = styles::select_face(&tokens, true);
        let closed = styles::select_face(&tokens, false);

        assert_eq!(open.border.width, tokens.dims.focus_border_width);
        assert_eq!(open.border.radius.bottom_left, 0.0);
        assert_eq!(open.border.radius.top_left, tokens.radii.base);
        assert_eq!(closed.border.width, 0.0);
        assert_eq!(closed.border.radius.bottom_left, tokens.radii.base);
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

    /// The chosen row has to out-read a hovered one in every theme; on the dark
    /// palettes `selected_fill` is within a few percent of the panel's own fill.
    #[test]
    fn the_chosen_option_outreads_a_hovered_one_in_every_theme() {
        for variant in [
            super::super::ThemeVariant::Dark,
            super::super::ThemeVariant::Light,
            super::super::ThemeVariant::ClassicSource,
        ] {
            let tokens = Tokens::for_variant(variant);
            let selected = styles::select_option(&tokens, true, button::Status::Active);
            let panel = styles::select_menu(&tokens, tokens.radii.md);

            assert_ne!(
                selected.background,
                panel.background,
                "{}: the chosen option is indistinguishable from the panel",
                variant.name(),
            );
        }
    }

    #[test]
    fn modal_uses_chromeless_modal_background() {
        let tokens = Tokens::dark();
        let style = styles::modal(&tokens);

        assert_eq!(
            style.background,
            Some(Color::from(tokens.colors.modal_bg).into())
        );
        assert_eq!(style.border.width, 0.0);
        assert!(style.shadow.blur_radius > 0.0);
    }

    #[test]
    fn checked_checkbox_uses_neutral_fill() {
        let tokens = Tokens::dark();
        let style = styles::checkbox(&tokens, checkbox::Status::Active { is_checked: true });

        assert_eq!(style.background, Color::from(tokens.colors.neutral).into());
    }

    #[test]
    fn scrollbar_uses_sidebar_matched_rail_and_grabber_tokens() {
        let tokens = Tokens::dark();
        let style = styles::scrollbar(
            &tokens,
            scrollable::Status::Active {
                is_horizontal_scrollbar_disabled: true,
                is_vertical_scrollbar_disabled: false,
            },
        );

        assert_eq!(
            style.vertical_rail.background,
            Some(Color::from(tokens.colors.scrollbar_rail).into())
        );
        assert_eq!(
            style.vertical_rail.scroller.background,
            Color::from(tokens.colors.scrollbar_grabber).into()
        );
    }
}
