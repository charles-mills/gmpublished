use std::time::Instant;

use iced::widget::text::{Shaping as TextShaping, Wrapping};
use iced::widget::{Space, button, column, container, mouse_area, opaque, row, svg, text};
use iced::{Alignment, Background, Border, Color, Element, Length, Shadow, Size, alignment};

use crate::{
    assets,
    theme::{Tokens, ViewCtx},
    widgets,
};

use super::{Entry, Icon, Message, State};

const MENU_MIN_WIDTH: f32 = 1.0;
const MENU_WIDTH_SAFETY_PAD: f32 = 12.0;
const SEPARATOR_HEIGHT: f32 = 1.0;

pub fn view<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    viewport_size: Size,
    now: Instant,
) -> Element<'a, Message> {
    if !state.visible() {
        return container(Space::new()).width(0.0).height(0.0).into();
    }
    let tokens = *ctx.tokens;
    let opacity = state.opacity(now);
    let scale = state.scale(now);
    let interactive = accepts_pointer_input(state);

    let position = state.position();
    let menu_width = menu_width(state.entries(), ctx, viewport_size);
    let menu_height = menu_height(state.entries(), &tokens, viewport_size);
    let x = clamped_position(position.x, viewport_size.width, menu_width);
    let y = clamped_position(position.y, viewport_size.height, menu_height);

    let positioned_menu = container(column![
        Space::new().height(Length::Fixed(y)),
        row![
            Space::new().width(Length::Fixed(x)),
            widgets::scaled::scaled(
                opaque(menu_panel(
                    state.entries(),
                    ctx,
                    MenuPanelLayout {
                        width: menu_width,
                        height: menu_height,
                    },
                    MenuPanelMotion {
                        opacity,
                        interactive,
                    },
                )),
                scale,
            )
            .interactive(interactive)
        ]
    ])
    .width(Length::Fill)
    .height(Length::Fill);

    if interactive {
        let fill = container(Space::new().width(Length::Fill).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill);
        let dismiss_layer = opaque(
            mouse_area(fill)
                .on_press(Message::DismissRequested)
                .on_right_press(Message::DismissRequested)
                .on_middle_press(Message::DismissRequested),
        );

        iced::widget::stack![dismiss_layer, positioned_menu]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        positioned_menu.into()
    }
}

pub const fn accepts_pointer_input(state: &State) -> bool {
    state.open()
}

fn menu_panel<'a>(
    entries: &'a [Entry],
    ctx: ViewCtx<'a>,
    layout: MenuPanelLayout,
    motion: MenuPanelMotion,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let mut rows = column![].spacing(0.0);
    for entry in entries {
        rows = rows.push(menu_row(entry, ctx, motion.opacity, motion.interactive));
    }

    container(rows)
        .width(Length::Fixed(layout.width))
        .height(Length::Fixed(layout.height))
        .align_y(alignment::Vertical::Top)
        .clip(true)
        .style(move |_| context_menu_style(&tokens, motion.opacity))
        .into()
}

#[derive(Clone, Copy)]
struct MenuPanelLayout {
    width: f32,
    height: f32,
}

#[derive(Clone, Copy)]
struct MenuPanelMotion {
    opacity: f32,
    interactive: bool,
}

fn menu_row<'a>(
    entry: &'a Entry,
    ctx: ViewCtx<'a>,
    opacity: f32,
    interactive: bool,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    if entry.separator_row() {
        return separator(&tokens, opacity);
    }

    let icon: Element<'_, Message> = entry.icon().map_or_else(
        || {
            Space::new()
                .width(Length::Fixed(tokens.dims.icon_size))
                .into()
        },
        |icon| {
            // The renderer ignores the tint color's alpha for svgs, so the
            // fade has to go through the widget's own opacity.
            svg(icon_handle(icon))
                .width(Length::Fixed(tokens.dims.icon_size))
                .height(Length::Fixed(tokens.dims.icon_size))
                .style(move |_, _| svg::Style {
                    color: Some(Color::from(tokens.colors.text)),
                })
                .opacity(opacity)
                .into()
        },
    );

    let content = row![
        icon,
        text(i18n.tr(entry.label_key()))
            .font(assets::fonts::default_font())
            .size(tokens.typography.body)
            .color(Color::from(tokens.colors.text).scale_alpha(opacity))
            .height(Length::Fill)
            .align_y(alignment::Vertical::Center)
            .shaping(TextShaping::Advanced)
            .wrapping(Wrapping::None)
            .width(Length::Fill),
    ]
    .spacing(tokens.dims.context_menu_icon_gap)
    .align_y(Alignment::Center);

    button(content)
        .on_press_maybe(
            interactive
                .then_some(entry.action())
                .flatten()
                .map(Message::ActionSelected),
        )
        .width(Length::Fill)
        .height(Length::Fixed(tokens.dims.context_menu_row_height))
        .padding([0.0, tokens.dims.context_menu_padding_x])
        .style(move |_, status| menu_button_style(&tokens, interactive, status, opacity))
        .into()
}

fn separator<'a>(tokens: &Tokens, opacity: f32) -> Element<'a, Message> {
    let tokens = *tokens;
    container(Space::new().height(Length::Fixed(SEPARATOR_HEIGHT)))
        .width(Length::Fill)
        .height(Length::Fixed(SEPARATOR_HEIGHT))
        .style(move |_| container::Style {
            background: Some(
                Color::from(tokens.colors.border_subtle)
                    .scale_alpha(opacity)
                    .into(),
            ),
            ..container::Style::default()
        })
        .into()
}

fn menu_button_style(
    tokens: &Tokens,
    enabled: bool,
    status: button::Status,
    opacity: f32,
) -> button::Style {
    let background = match status {
        button::Status::Hovered if enabled => Some(
            Color::from(tokens.colors.hover_fill)
                .scale_alpha(opacity)
                .into(),
        ),
        button::Status::Pressed if enabled => Some(
            Color::from(tokens.colors.hover_fill)
                .scale_alpha(opacity)
                .into(),
        ),
        _ => None,
    };

    button::Style {
        background,
        text_color: if enabled {
            Color::from(tokens.colors.text).scale_alpha(opacity)
        } else {
            Color::from(tokens.colors.text_dim).scale_alpha(opacity)
        },
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

fn context_menu_style(tokens: &Tokens, opacity: f32) -> container::Style {
    container::Style {
        text_color: Some(Color::from(tokens.colors.text).scale_alpha(opacity)),
        background: Some(Background::Color(
            Color::from(tokens.colors.menu_bg).scale_alpha(opacity),
        )),
        border: Border {
            radius: tokens.radii.base.into(),
            ..Border::default()
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

fn icon_handle(icon: Icon) -> svg::Handle {
    match icon {
        Icon::Extract => assets::icons::context_folder_add(),
        Icon::OpenLocation => assets::icons::context_folder(),
        Icon::Copy => assets::icons::context_copy(),
        Icon::OpenExternal => assets::icons::context_link_out(),
        Icon::CopyLink => assets::icons::context_link_chain(),
        Icon::Download => assets::icons::context_cloud_download(),
        Icon::Image => assets::icons::context_image(),
        #[cfg(feature = "debug")]
        Icon::Hide => assets::icons::cross(),
        #[cfg(feature = "debug")]
        Icon::DebugPlus => assets::icons::circle_plus(),
        #[cfg(feature = "debug")]
        Icon::DebugMinus => assets::icons::akar_reduce(),
    }
}

fn menu_width(entries: &[Entry], ctx: ViewCtx<'_>, viewport_size: Size) -> f32 {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let content_width = entries
        .iter()
        .filter(|entry| !entry.separator_row())
        .map(|entry| menu_row_width(&i18n.tr(entry.label_key()), &tokens))
        .fold(MENU_MIN_WIDTH, f32::max);

    if viewport_size.width > 0.0 {
        content_width.min(viewport_size.width).max(MENU_MIN_WIDTH)
    } else {
        content_width
    }
}

fn menu_height(entries: &[Entry], tokens: &Tokens, viewport_size: Size) -> f32 {
    let rows = menu_content_height(entries, tokens);

    if viewport_size.height > 0.0 {
        rows.min(viewport_size.height).max(1.0)
    } else {
        rows
    }
}

fn menu_content_height(entries: &[Entry], tokens: &Tokens) -> f32 {
    entries
        .iter()
        .map(|entry| {
            if entry.separator_row() {
                SEPARATOR_HEIGHT
            } else {
                tokens.dims.context_menu_row_height
            }
        })
        .sum::<f32>()
}

fn menu_row_width(label: &str, tokens: &Tokens) -> f32 {
    tokens.dims.context_menu_padding_x.mul_add(
        2.0,
        tokens.dims.icon_size
            + tokens.dims.context_menu_icon_gap
            + measure_label_width(label, tokens.typography.body)
            + MENU_WIDTH_SAFETY_PAD,
    )
}

fn measure_label_width(label: &str, font_size: f32) -> f32 {
    crate::media::text_measure::measure_width(label, font_size)
}

fn clamped_position(position: f32, viewport_extent: f32, menu_extent: f32) -> f32 {
    if viewport_extent > 0.0 {
        position
            .clamp(0.0, (viewport_extent - menu_extent).max(0.0))
            .round()
    } else {
        position.max(0.0).round()
    }
}
