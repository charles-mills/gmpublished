use std::time::Instant;

use iced::widget::{
    Space, button, column, container, image, mouse_area, opaque, row, stack, svg, text,
};
use iced::{
    Alignment, Border, Center, Color, ContentFit, Element, Length, Padding, Shadow, Vector, border,
    font, mouse,
};

use crate::{
    assets,
    features::steam_session::ConnectionStatus,
    theme::{self, Rgba, Tokens, ViewCtx},
    widgets::{self, scaled::Origin, tooltip as tooltip_widget},
};

use super::{ChromeStrategy, Message, Route, State, sidebar_rail_width, sidebar_width};

/// Renders the full-height shared shell sidebar.
pub fn sidebar<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    chrome_strategy: ChromeStrategy,
    drag_active: bool,
    now: Instant,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let margin = tokens.dims.sidebar_float_margin;
    let rail = container(rail_sidebar(state, ctx, chrome_strategy, drag_active, now))
        .width(Length::Fixed(sidebar_rail_width(&tokens, chrome_strategy)))
        .height(Length::Fill)
        .style(move |_| theme::styles::sidebar(&tokens));

    container(rail)
        .padding(Padding {
            top: margin,
            right: margin,
            bottom: margin,
            left: margin,
        })
        .width(Length::Fixed(sidebar_width(&tokens, chrome_strategy)))
        .height(Length::Fill)
        .into()
}

fn rail_sidebar<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    chrome_strategy: ChromeStrategy,
    drag_active: bool,
    now: Instant,
) -> Element<'a, Message> {
    column![
        rail_top_clearance(ctx.tokens, chrome_strategy),
        rail_navigation(state, ctx, drag_active, now),
        Space::new().height(Length::Fill),
        rail_account_button(state, ctx.tokens, chrome_strategy, drag_active),
    ]
    .spacing(0.0)
    .align_x(Alignment::Center)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn rail_navigation<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    drag_active: bool,
    now: Instant,
) -> Element<'a, Message> {
    let mut routes = column![rail_search_button(ctx, drag_active)]
        .spacing(ctx.tokens.dims.sidebar_route_spacing)
        .align_x(Alignment::Center)
        .width(Length::Fill);
    for route in Route::ALL {
        routes = routes.push(rail_route_item(route, state, ctx, drag_active, now));
    }

    container(routes).width(Length::Fill).into()
}

fn rail_search_button(ctx: ViewCtx<'_>, drag_active: bool) -> Element<'_, Message> {
    let tokens = ctx.tokens;
    let i18n = ctx.i18n;
    let search_label = format!("{} {}", i18n.tr("search"), search_shortcut_hint());
    // The press is dropped while a drag is in flight so the button reports
    // no interaction of its own; a mouse_area's cursor only shows through
    // over interaction-free content.
    let control = sidebar_icon_button(
        assets::icons::search(),
        search_label,
        (!drag_active).then_some(Message::SearchActivated),
        true,
        tokens.dims.sidebar_rail_icon_button_size,
        tokens.dims.sidebar_rail_icon_glyph,
        tokens,
        !drag_active,
    );

    // The search palette is not a drop target, so it gets the same
    // can't-drop cursor as the non-Downloader routes below it.
    if drag_active {
        mouse_area(control)
            .interaction(mouse::Interaction::NoDrop)
            .into()
    } else {
        control
    }
}

fn rail_top_clearance<'a>(
    tokens: &Tokens,
    chrome_strategy: ChromeStrategy,
) -> Element<'a, Message> {
    let height = rail_top_clearance_height(tokens, chrome_strategy);
    if chrome_strategy == ChromeStrategy::MacNativeInset {
        return drag_region(Length::Fill, height);
    }

    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(height))
        .into()
}

fn rail_top_clearance_height(tokens: &Tokens, _chrome_strategy: ChromeStrategy) -> f32 {
    tokens.dims.sidebar_band_height
}

#[cfg(test)]
fn rail_first_nav_center_y(tokens: &Tokens, chrome_strategy: ChromeStrategy) -> f32 {
    rail_top_clearance_height(tokens, chrome_strategy)
        + tokens.dims.sidebar_rail_icon_button_size / 2.0
}

#[cfg(test)]
fn rail_next_nav_center_y(tokens: &Tokens, chrome_strategy: ChromeStrategy) -> f32 {
    rail_first_nav_center_y(tokens, chrome_strategy)
        + tokens.dims.sidebar_rail_icon_button_size
        + tokens.dims.sidebar_route_spacing
}

#[cfg(test)]
fn rail_nav_center_gap(tokens: &Tokens) -> f32 {
    tokens.dims.sidebar_rail_icon_button_size + tokens.dims.sidebar_route_spacing
}

fn drag_region<'a>(width: Length, height: f32) -> Element<'a, Message> {
    mouse_area(
        container(Space::new())
            .width(width)
            .height(Length::Fixed(height)),
    )
    .on_press(Message::DragRegionPressed)
    .on_double_click(Message::DragRegionDoubleClicked)
    .into()
}

/// Renders the floating account menu layer anchored to the sidebar bottom.
pub fn account_menu_overlay<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    now: Instant,
) -> Option<Element<'a, Message>> {
    if !state.account_menu_visible() {
        return None;
    }

    let tokens = ctx.tokens;
    let interactive = state.account_menu_open();
    let opacity = state.account_menu_opacity(now);
    let dismiss: Element<'a, Message> = if interactive {
        opaque(
            mouse_area(
                container(Space::new().width(Length::Fill).height(Length::Fill))
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .on_press(Message::AccountMenuDismissed),
        )
    } else {
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    };

    let positioned = column![
        Space::new().height(Length::Fill),
        row![
            Space::new().width(Length::Fixed(account_menu_anchor_x(tokens))),
            widgets::scaled::scaled(
                opaque(account_menu_panel(state, ctx, interactive, opacity)),
                state.account_menu_scale(now),
            )
            .origin(Origin::BottomLeft)
            .interactive(interactive),
            Space::new().width(Length::Fill),
        ],
        Space::new().height(Length::Fixed(account_menu_bottom_offset(tokens))),
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    Some(
        iced::widget::stack![dismiss, positioned]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
    )
}

fn account_menu_anchor_x(tokens: &Tokens) -> f32 {
    tokens.dims.sidebar_float_margin + tokens.dims.account_menu_margin
}

fn account_menu_bottom_offset(tokens: &Tokens) -> f32 {
    tokens.dims.sidebar_float_margin
        + tokens.dims.sidebar_account_row_height
        + tokens.dims.account_menu_bottom_gap
}

fn rail_route_item<'a>(
    route: Route,
    state: &State,
    ctx: ViewCtx<'a>,
    drag_active: bool,
    now: Instant,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let active = state.route() == route;
    let drop_hovered =
        drag_active && route == Route::Downloader && state.downloader_drop_target_hovered();
    let icon_color = if active {
        tokens.colors.link
    } else {
        tokens.colors.text
    };
    let control = button(
        container(svg_icon(
            route.icon(),
            tokens.dims.sidebar_rail_icon_glyph,
            icon_color.into(),
            1.0,
        ))
        .center(Length::Fixed(tokens.dims.sidebar_rail_icon_button_size)),
    )
    .on_press_maybe((!drag_active).then_some(Message::Navigate(route)))
    .padding(0.0)
    .width(Length::Fixed(tokens.dims.sidebar_rail_icon_button_size))
    .height(Length::Fixed(tokens.dims.sidebar_rail_icon_button_size))
    .style(move |_, status| {
        let status = if drop_hovered {
            button::Status::Hovered
        } else {
            status
        };
        if active {
            theme::styles::sidebar_item_button(&tokens, true, status)
        } else {
            icon_button_style(&tokens, status)
        }
    });

    // The active-jobs badge overlays the icon's top-right corner; a plain
    // container passes pointer events through to the button beneath it.
    let control: Element<'a, Message> = if route == Route::Downloader
        && let Some(badge) = state.downloader_badge(now)
    {
        stack![
            control,
            container(job_badge(badge.count, &tokens, badge.opacity))
                .align_right(Length::Fill)
                .height(Length::Fill),
        ]
        .width(Length::Fixed(tokens.dims.sidebar_rail_icon_button_size))
        .height(Length::Fixed(tokens.dims.sidebar_rail_icon_button_size))
        .into()
    } else {
        control.into()
    };

    let control: Element<'a, Message> = if route == Route::Downloader {
        let area = mouse_area(control)
            .on_enter(Message::DownloaderDropTargetEntered)
            .on_exit(Message::DownloaderDropTargetExited);
        if drag_active {
            area.interaction(mouse::Interaction::Copy).into()
        } else {
            area.into()
        }
    } else if drag_active {
        mouse_area(control)
            .interaction(mouse::Interaction::NoDrop)
            .into()
    } else {
        control
    };

    if drag_active {
        control
    } else {
        tooltip_widget::below(
            control,
            format!(
                "{} {}",
                i18n.tr(route.label_key()),
                route_shortcut_hint(route)
            ),
            &tokens,
            260.0,
        )
    }
}

fn job_badge(count: u32, tokens: &Tokens, opacity: f32) -> Element<'static, Message> {
    let tokens = *tokens;
    container(text(count.to_string()).size(tokens.typography.caption))
        .padding([2.0, 6.0])
        .style(move |_| theme::styles::job_badge(&tokens, opacity))
        .into()
}

fn rail_account_button<'a>(
    state: &'a State,
    tokens: &Tokens,
    chrome_strategy: ChromeStrategy,
    drag_active: bool,
) -> Element<'a, Message> {
    let tokens = *tokens;
    // Rail hover feedback is a ring on the avatar circle itself (the
    // presence badge layers over it); it has to come from the tracked
    // hover state because the button spans the whole row (click target).
    let hovered = state.account_row_hovered();
    let button = button(
        container(account_avatar(
            state,
            &tokens,
            tokens.dims.sidebar_account_rail_avatar_size,
            true,
            hovered,
        ))
        .center(Length::Fixed(tokens.dims.sidebar_account_rail_box_size)),
    )
    // The press is dropped while a drag is in flight so the button reports
    // no interaction of its own; a mouse_area's cursor only shows through
    // over interaction-free content.
    .on_press_maybe((!drag_active).then_some(Message::AccountMenuToggled))
    .padding(0.0)
    .width(Length::Fixed(sidebar_rail_width(&tokens, chrome_strategy)))
    .height(Length::Fixed(tokens.dims.sidebar_account_row_height))
    .style(move |_, _| account_row_style(&tokens));

    let area = mouse_area(button)
        .on_enter(Message::AccountRowHoverChanged(true))
        .on_exit(Message::AccountRowHoverChanged(false));

    // The account menu is not a drop target either; match the rail icons'
    // can't-drop cursor while a drag is in flight.
    if drag_active {
        area.interaction(mouse::Interaction::NoDrop).into()
    } else {
        area.into()
    }
}

fn account_avatar<'a>(
    state: &State,
    tokens: &Tokens,
    size: f32,
    show_presence: bool,
    hover_ring: bool,
) -> Element<'a, Message> {
    let tokens = *tokens;
    let badge_offset = tokens.dims.presence_badge_ring;
    let badge_outer = presence_badge_outer(&tokens);
    let badge_origin = if show_presence {
        size - badge_outer + badge_offset
    } else {
        size - badge_outer
    }
    .max(0.0);
    let canvas = account_avatar_canvas_size(&tokens, size, show_presence);
    let handle = state
        .steam_avatar()
        .cloned()
        .unwrap_or_else(assets::images::steam_anonymous);

    let avatar = container(
        image(handle)
            .width(size)
            .height(size)
            .content_fit(ContentFit::Cover)
            .border_radius(size / 2.0),
    )
    .width(size)
    .height(size)
    .clip(true)
    .style(move |_| small_avatar_style(&tokens, size));

    // The hover ring is its own layer: a container border would draw
    // underneath the image, so it goes above the avatar and below the
    // badges, sized like the avatar circle so it's exactly concentric.
    let ring: Element<'a, Message> = if hover_ring {
        container(Space::new())
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .style(move |_| avatar_ring_style(&tokens, size))
            .into()
    } else {
        container(Space::new())
            .width(Length::Fixed(canvas))
            .height(Length::Fixed(canvas))
            .into()
    };

    let presence = if show_presence {
        avatar_badge_layer(
            presence_badge(state.steam_status(), &tokens),
            badge_origin,
            badge_origin,
            canvas,
        )
    } else {
        container(Space::new())
            .width(Length::Fixed(canvas))
            .height(Length::Fixed(canvas))
            .into()
    };
    let update = if state.update_available() {
        avatar_badge_layer(
            avatar_badge(tokens.colors.link, &tokens),
            badge_origin,
            0.0,
            canvas,
        )
    } else {
        container(Space::new())
            .width(Length::Fixed(canvas))
            .height(Length::Fixed(canvas))
            .into()
    };

    iced::widget::stack![avatar, ring, presence, update]
        .width(Length::Fixed(canvas))
        .height(Length::Fixed(canvas))
        .into()
}

fn presence_badge<'a>(status: ConnectionStatus, tokens: &Tokens) -> Element<'a, Message> {
    avatar_badge(status_color(status, tokens), tokens)
}

fn avatar_badge_layer(
    badge: Element<'_, Message>,
    x: f32,
    y: f32,
    canvas: f32,
) -> Element<'_, Message> {
    column![
        Space::new().height(Length::Fixed(y)),
        row![Space::new().width(Length::Fixed(x)), badge]
    ]
    .width(Length::Fixed(canvas))
    .height(Length::Fixed(canvas))
    .into()
}

fn avatar_badge<'a>(fill: Rgba, tokens: &Tokens) -> Element<'a, Message> {
    let tokens = *tokens;
    let outer = presence_badge_outer(&tokens);
    container(
        container(Space::new())
            .width(tokens.dims.presence_badge_size)
            .height(tokens.dims.presence_badge_size)
            .style(move |_| circle_style(fill, tokens.dims.presence_badge_size / 2.0)),
    )
    .width(outer)
    .height(outer)
    .center(Length::Fixed(outer))
    .style(move |_| circle_style(tokens.colors.sidebar_panel_bg, outer / 2.0))
    .into()
}

fn account_avatar_canvas_size(tokens: &Tokens, avatar_size: f32, show_presence: bool) -> f32 {
    if show_presence {
        avatar_size + tokens.dims.presence_badge_ring
    } else {
        avatar_size
    }
}

fn presence_badge_outer(tokens: &Tokens) -> f32 {
    tokens.dims.presence_badge_size + 2.0 * tokens.dims.presence_badge_ring
}

fn sidebar_icon_button<'a>(
    handle: svg::Handle,
    tooltip: String,
    message: Option<Message>,
    enabled: bool,
    button_size: f32,
    glyph_size: f32,
    tokens: &Tokens,
    show_tooltip: bool,
) -> Element<'a, Message> {
    let tokens = *tokens;
    let color = if enabled {
        tokens.colors.text
    } else {
        tokens.colors.text_dim
    };
    let opacity = if enabled {
        1.0
    } else {
        tokens.dims.disabled_opacity
    };
    let control = button(
        container(svg_icon(handle, glyph_size, color.into(), opacity))
            .center(Length::Fixed(button_size)),
    )
    .on_press_maybe(if enabled { message } else { None })
    .padding(0.0)
    .width(Length::Fixed(button_size))
    .height(Length::Fixed(button_size))
    .style(move |_, status| icon_button_style(&tokens, status));

    if show_tooltip {
        tooltip_widget::below(control, tooltip, &tokens, 260.0)
    } else {
        control.into()
    }
}

fn account_menu_panel<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    interactive: bool,
    opacity: f32,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let mut content = column![settings_row(ctx, interactive, opacity)]
        .spacing(0.0)
        .width(Length::Fill);

    if state.update_available() {
        content = content.push(divider(&tokens, opacity)).push(update_row(
            state,
            ctx,
            interactive,
            opacity,
        ));
    }

    content =
        content
            .push(divider(&tokens, opacity))
            .push(footer(state, ctx, interactive, opacity));

    container(content)
        .width(Length::Fixed(tokens.dims.account_menu_width))
        .padding([
            tokens.dims.account_menu_padding_y,
            tokens.dims.account_menu_padding_x,
        ])
        .clip(true)
        .style(move |_| popover_style(&tokens, opacity))
        .into()
}

fn settings_row(ctx: ViewCtx<'_>, interactive: bool, opacity: f32) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let content = row![
        menu_icon_slot(
            svg_icon(
                assets::icons::gear(),
                tokens.dims.icon_size,
                tokens.colors.text_dim.into(),
                opacity,
            ),
            &tokens,
        ),
        text(i18n.tr("settings-settings"))
            .size(tokens.typography.body_sm)
            .font(theme::styles::inter_font(font::Weight::Semibold))
            .color(Color::from(tokens.colors.text).scale_alpha(opacity))
            .width(Length::Fill),
        text(settings_shortcut_hint())
            .size(tokens.typography.caption_xs)
            .color(Color::from(tokens.colors.text_dim).scale_alpha(opacity)),
    ]
    .spacing(tokens.spacing.gap_sm)
    .align_y(Center)
    .width(Length::Fill);

    button(content)
        .on_press_maybe(interactive.then_some(Message::SettingsActivated))
        .padding([
            tokens.dims.account_menu_row_padding_y,
            tokens.dims.account_menu_row_padding_x,
        ])
        .width(Length::Fill)
        .style(move |_, status| menu_row_style(&tokens, status, opacity))
        .into()
}

fn update_row<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    interactive: bool,
    opacity: f32,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let caption = i18n.trn(
        "account-update-caption",
        &[("arg0", state.update_version())],
    );
    let content = row![
        menu_icon_slot(
            svg_icon(
                assets::icons::cloud_download(),
                tokens.dims.icon_size,
                tokens.colors.link.into(),
                opacity,
            ),
            &tokens,
        ),
        column![
            text(i18n.tr("account-update-title"))
                .size(tokens.typography.body_sm)
                .font(theme::styles::inter_font(font::Weight::Semibold))
                .color(Color::from(tokens.colors.text).scale_alpha(opacity)),
            text(caption)
                .size(tokens.typography.caption_xs)
                .color(Color::from(tokens.colors.link).scale_alpha(opacity)),
        ]
        .spacing(1.0)
        .width(Length::Fill),
    ]
    .spacing(tokens.spacing.gap_sm)
    .align_y(Center)
    .width(Length::Fill);

    button(content)
        .on_press_maybe(interactive.then_some(Message::UpdateNagActivated))
        .padding([
            tokens.dims.account_menu_update_padding_y,
            tokens.dims.account_menu_row_padding_x,
        ])
        .width(Length::Fill)
        .style(move |_, status| update_row_style(&tokens, status, opacity))
        .into()
}

fn footer<'a>(
    state: &State,
    ctx: ViewCtx<'a>,
    interactive: bool,
    opacity: f32,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let version = if state.app_version().is_empty() {
        i18n.tr("gmpublished-name")
    } else {
        i18n.trn("gmpublished-version", &[("arg0", state.app_version())])
    };

    let upstream_link = button(
        text(i18n.tr("based-on-gmpublisher"))
            .size(tokens.typography.caption_xs)
            .font(theme::styles::inter_font(font::Weight::Normal)),
    )
    .on_press_maybe(interactive.then_some(Message::UpstreamRepoActivated))
    .padding(0.0)
    .style(move |_, status| footer_link_style(&tokens, status, opacity));

    column![
        text(version)
            .size(tokens.typography.caption_xs)
            .color(Color::from(tokens.colors.text_dim).scale_alpha(opacity)),
        upstream_link,
    ]
    .spacing(tokens.dims.account_menu_footer_gap)
    .padding([
        tokens.dims.account_menu_padding_y,
        tokens.dims.account_menu_row_padding_x,
    ])
    .width(Length::Fill)
    .into()
}

fn divider<'a>(tokens: &Tokens, opacity: f32) -> Element<'a, Message> {
    let tokens = *tokens;
    row![
        Space::new().width(Length::Fixed(tokens.dims.account_menu_divider_inset)),
        container(Space::new())
            .height(Length::Fixed(tokens.dims.sidebar_divider_width))
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(
                    Color::from(theme::motion::scaled_alpha(
                        tokens.colors.overlay_divider,
                        opacity,
                    ))
                    .into(),
                ),
                ..container::Style::default()
            }),
        Space::new().width(Length::Fixed(tokens.dims.account_menu_divider_inset)),
    ]
    .height(Length::Fixed(tokens.dims.sidebar_divider_width))
    .width(Length::Fill)
    .into()
}

fn menu_icon_slot<'a>(content: Element<'a, Message>, tokens: &Tokens) -> Element<'a, Message> {
    container(content)
        .center_x(Length::Fixed(tokens.dims.account_menu_icon_column_width))
        .center_y(Length::Fixed(tokens.dims.icon_size))
        .into()
}

fn svg_icon<'a>(
    handle: svg::Handle,
    size: f32,
    color: Color,
    opacity: f32,
) -> Element<'a, Message> {
    svg(handle)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .content_fit(ContentFit::Contain)
        .style(move |_, _| svg::Style { color: Some(color) })
        .opacity(opacity)
        .into()
}

fn status_color(status: ConnectionStatus, tokens: &Tokens) -> Rgba {
    match status {
        ConnectionStatus::Connected => tokens.colors.success,
        ConnectionStatus::Connecting => tokens.colors.neutral,
        ConnectionStatus::Disconnected => tokens.colors.text_dim,
        ConnectionStatus::Unavailable => tokens.colors.error,
    }
}

fn search_shortcut_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘F"
    } else {
        "Ctrl+F"
    }
}

fn settings_shortcut_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘,"
    } else {
        "Ctrl+,"
    }
}

fn route_shortcut_hint(route: Route) -> String {
    let index = Route::ALL
        .iter()
        .position(|candidate| *candidate == route)
        .map_or(1, |index| index + 1);
    if cfg!(target_os = "macos") {
        format!("⌘{index}")
    } else {
        format!("Ctrl+{index}")
    }
}

fn popover_style(tokens: &Tokens, opacity: f32) -> container::Style {
    container::Style {
        text_color: Some(Color::from(tokens.colors.text).scale_alpha(opacity)),
        background: Some(
            Color::from(theme::motion::scaled_alpha(
                tokens.colors.overlay_panel_bg,
                opacity,
            ))
            .into(),
        ),
        border: border::rounded(tokens.radii.lg),
        shadow: Shadow {
            color: Color::from(tokens.colors.shadow_dropdown).scale_alpha(opacity),
            offset: Vector::ZERO,
            blur_radius: 10.0,
        },
        snap: true,
    }
}

fn small_avatar_style(tokens: &Tokens, avatar_size: f32) -> container::Style {
    container::Style {
        background: Some(Color::from(tokens.colors.surface_2).into()),
        border: Border {
            radius: border::Radius::new(avatar_size / 2.0),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

fn avatar_ring_style(tokens: &Tokens, avatar_size: f32) -> container::Style {
    container::Style {
        border: Border {
            color: tokens.colors.link.into(),
            width: tokens.dims.focus_border_width,
            radius: border::Radius::new(avatar_size / 2.0),
        },
        ..container::Style::default()
    }
}

fn circle_style(color: Rgba, radius: f32) -> container::Style {
    container::Style {
        background: Some(Color::from(color).into()),
        border: Border {
            radius: border::Radius::new(radius),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

fn account_row_style(tokens: &Tokens) -> button::Style {
    button::Style {
        background: None,
        text_color: tokens.colors.text.into(),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

fn icon_button_style(tokens: &Tokens, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered => Some(Color::from(tokens.colors.hover_fill_faint).into()),
        button::Status::Pressed => Some(Color::from(tokens.colors.hover_fill_soft).into()),
        button::Status::Active | button::Status::Disabled => None,
    };
    let text_color = if matches!(status, button::Status::Disabled) {
        tokens.colors.text_dim.into()
    } else {
        tokens.colors.text.into()
    };

    button::Style {
        background,
        text_color,
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

fn menu_row_style(tokens: &Tokens, status: button::Status, opacity: f32) -> button::Style {
    let background = match status {
        button::Status::Hovered => Some(
            Color::from(theme::motion::scaled_alpha(
                tokens.colors.hover_fill_faint,
                opacity,
            ))
            .into(),
        ),
        button::Status::Pressed => Some(
            Color::from(theme::motion::scaled_alpha(
                tokens.colors.hover_fill_soft,
                opacity,
            ))
            .into(),
        ),
        button::Status::Active | button::Status::Disabled => None,
    };

    button::Style {
        background,
        text_color: Color::from(tokens.colors.text).scale_alpha(opacity),
        border: border::rounded(tokens.radii.base),
        shadow: Shadow::default(),
        snap: true,
    }
}

fn update_row_style(tokens: &Tokens, status: button::Status, opacity: f32) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => tokens.colors.account_update_hover_bg,
        button::Status::Active => tokens.colors.account_update_bg,
        button::Status::Disabled => tokens.colors.account_update_bg.with_alpha(0),
    };

    button::Style {
        background: Some(Color::from(theme::motion::scaled_alpha(background, opacity)).into()),
        text_color: Color::from(tokens.colors.text).scale_alpha(opacity),
        border: border::rounded(tokens.radii.base),
        shadow: Shadow::default(),
        snap: true,
    }
}

fn footer_link_style(tokens: &Tokens, status: button::Status, opacity: f32) -> button::Style {
    let color = match status {
        button::Status::Hovered | button::Status::Pressed => Color::from(tokens.colors.link),
        button::Status::Active | button::Status::Disabled => Color::from(tokens.colors.text_dim),
    };

    button::Style {
        background: None,
        text_color: color.scale_alpha(opacity),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_menu_anchor_tracks_floating_rail() {
        let tokens = Tokens::dark();

        assert_eq!(
            sidebar_width(&tokens, ChromeStrategy::SystemDefault),
            tokens.dims.sidebar_rail_width + tokens.dims.sidebar_float_margin * 2.0
        );
        assert_eq!(
            sidebar_width(&tokens, ChromeStrategy::MacNativeInset),
            tokens.dims.sidebar_rail_width_inset + tokens.dims.sidebar_float_margin * 2.0
        );
        assert_eq!(
            account_menu_anchor_x(&tokens),
            tokens.dims.sidebar_float_margin + tokens.dims.account_menu_margin
        );
        assert_eq!(
            account_menu_bottom_offset(&tokens),
            tokens.dims.sidebar_float_margin
                + tokens.dims.sidebar_account_row_height
                + tokens.dims.account_menu_bottom_gap
        );
        assert_eq!(
            account_avatar_canvas_size(&tokens, tokens.dims.sidebar_account_rail_avatar_size, true),
            42.0
        );
    }

    /// Lays out the real sidebar during a drag and reads back the cursor
    /// interaction the widget tree reports at each rail control: the red
    /// no-drop cross everywhere except the Downloader drop target.
    #[test]
    fn dragging_shows_no_drop_cursor_over_search_routes_and_account() {
        use iced_test::core::clipboard;
        use iced_test::core::renderer::Headless as _;
        use iced_test::runtime::{UserInterface, user_interface};

        let state = State::default();
        let tokens = Tokens::dark();
        let i18n = crate::i18n::I18n::for_locale(Some("en"));
        let ctx = ViewCtx::new(&tokens, &i18n);

        let width = sidebar_width(&tokens, ChromeStrategy::SystemDefault);
        let height = 600.0;
        let mut renderer = iced_test::futures::futures::executor::block_on(
            iced_test::renderer::Renderer::new(iced::Font::default(), iced::Pixels(16.0), None),
        )
        .expect("headless renderer");

        let center_x = tokens.dims.sidebar_float_margin + tokens.dims.sidebar_rail_width / 2.0;
        let search_y = tokens.dims.sidebar_float_margin
            + rail_first_nav_center_y(&tokens, ChromeStrategy::SystemDefault);
        let gap = rail_nav_center_gap(&tokens);
        let account_y = height
            - tokens.dims.sidebar_float_margin
            - tokens.dims.sidebar_account_row_height / 2.0;

        let mut interaction_at = |y: f32| {
            let position = iced::Point::new(center_x, y);
            let element = sidebar(
                &state,
                ctx,
                ChromeStrategy::SystemDefault,
                true,
                Instant::now(),
            );
            let mut ui = UserInterface::build(
                element,
                iced::Size::new(width, height),
                user_interface::Cache::default(),
                &mut renderer,
            );
            let (ui_state, _statuses) = ui.update(
                &[iced::Event::Mouse(mouse::Event::CursorMoved { position })],
                mouse::Cursor::Available(position),
                &mut renderer,
                &mut clipboard::Null,
                &mut Vec::new(),
            );
            match ui_state {
                user_interface::State::Updated {
                    mouse_interaction, ..
                } => mouse_interaction,
                user_interface::State::Outdated => panic!("sidebar interface became outdated"),
            }
        };

        assert_eq!(interaction_at(search_y), mouse::Interaction::NoDrop);
        assert_eq!(interaction_at(search_y + gap), mouse::Interaction::NoDrop);
        // Route::ALL[2] is the Downloader: the one live drop target.
        assert_eq!(
            interaction_at(search_y + 3.0 * gap),
            mouse::Interaction::Copy
        );
        assert_eq!(interaction_at(account_y), mouse::Interaction::NoDrop);
    }

    #[test]
    fn rail_nav_starts_below_the_chrome_clearance_in_both_strategies() {
        let tokens = Tokens::dark();

        assert_eq!(
            rail_top_clearance_height(&tokens, ChromeStrategy::SystemDefault),
            tokens.dims.sidebar_band_height
        );
        assert_eq!(
            rail_top_clearance_height(&tokens, ChromeStrategy::MacNativeInset),
            tokens.dims.sidebar_band_height
        );
        assert_eq!(
            rail_first_nav_center_y(&tokens, ChromeStrategy::SystemDefault),
            rail_first_nav_center_y(&tokens, ChromeStrategy::MacNativeInset)
        );
        assert_eq!(
            rail_next_nav_center_y(&tokens, ChromeStrategy::SystemDefault)
                - rail_first_nav_center_y(&tokens, ChromeStrategy::SystemDefault),
            rail_nav_center_gap(&tokens)
        );
    }
}
