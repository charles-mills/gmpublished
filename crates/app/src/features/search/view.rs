use std::time::Instant;

use iced::widget::{
    Space, button, column, container, image, mouse_area, opaque, row, scrollable, svg, text,
    text_input,
};
use iced::{
    Alignment, Background, Border, Color, ContentFit, Element, Length, Padding, Shadow, Size,
    Vector, alignment, border,
};

use crate::{
    assets,
    backend::domain::SearchMode,
    theme::{self, Tokens, ViewCtx},
    widgets,
};

use super::{
    Message, State,
    state::{RESULT_ROW_HEIGHT, Row, RowThumbnail},
    update::SEARCH_INPUT_ID,
};

const DROPDOWN_MIN_HEIGHT: f32 = 120.0;
const DROPDOWN_MAX_HEIGHT: f32 = 430.0;
const SEARCH_ICON_SIZE: f32 = 16.0;
const SEARCH_ICON_X: f32 = 12.0;
const SEARCH_TEXT_X: f32 = 38.0;
const SEARCH_FIELD_HEIGHT: f32 = 44.0;
const SEARCH_FIELD_VERTICAL_PADDING: f32 = 13.5;
const THUMBNAIL_SIZE: f32 = 38.0;
const DEAD_GLYPH_SIZE: f32 = 22.0;
const LOADING_GLYPH_SIZE: f32 = 18.0;
const SOURCE_LABEL_WIDTH: f32 = 128.0;
const STATUS_ROW_HEIGHT: f32 = 58.0;

pub fn dropdown_overlay<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    viewport_size: Size,
    now: Instant,
) -> Option<Element<'a, Message>> {
    if !state.palette_visible() {
        return None;
    }

    let interactive = state.palette_open();
    let opacity = state.opacity(now);
    let tokens = *ctx.tokens;
    let scrim = container(Space::new().width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| search_scrim_style(&tokens, opacity));
    let dismiss_layer: Element<'a, Message> = if interactive {
        opaque(mouse_area(scrim).on_press(Message::DismissRequested))
    } else {
        scrim.into()
    };

    let width = palette_width(viewport_size.width, &tokens);
    let height = dropdown_height(state, viewport_size, &tokens);
    let left = palette_left(viewport_size.width, width);
    let positioned = container(column![
        Space::new().height(Length::Fixed(tokens.dims.search_palette_top_offset)),
        row![
            Space::new().width(Length::Fixed(left)),
            widgets::scaled::scaled(
                opaque(palette_panel(
                    state,
                    ctx,
                    width,
                    height,
                    interactive,
                    opacity,
                )),
                state.scale(now),
            )
            .interactive(interactive),
            Space::new().width(Length::Fill),
        ]
    ])
    .width(Length::Fill)
    .height(Length::Fill);

    Some(
        iced::widget::stack![dismiss_layer, positioned]
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
    )
}

fn shortcut_chip<'a>(mode: SearchMode, tokens: &Tokens, opacity: f32) -> Element<'a, Message> {
    let tokens = *tokens;
    container(
        text(search_shortcut_hint(mode))
            .size(tokens.typography.caption_xs)
            .color(Color::from(tokens.colors.text_dim).scale_alpha(opacity)),
    )
    .height(Length::Fixed(tokens.dims.search_keycap_height))
    .padding([0.0, tokens.dims.search_keycap_padding_x])
    .style(move |_| keycap_style(&tokens, opacity))
    .into()
}

fn palette_panel<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    width: f32,
    height: f32,
    interactive: bool,
    opacity: f32,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mut content = column![palette_input(state, ctx, interactive, opacity)]
        .spacing(0.0)
        .width(Length::Fill);

    if state.query_active() && (state.loading() || !state.rows().is_empty() || state.show_empty()) {
        content = content.push(divider(&tokens, opacity));
        if state.rows().is_empty() && (state.loading() || state.show_empty()) {
            let label = if state.loading() {
                i18n.tr("search-loading")
            } else {
                i18n.tr("search-no-results")
            };
            content = content.push(status_row(label, state.loading(), &tokens, opacity));
        } else {
            content = content.push(result_list(state, ctx, height, interactive, opacity));
        }
    }

    container(content)
        .width(Length::Fixed(width))
        .clip(true)
        .style(move |_| dropdown_panel_style(&tokens, opacity))
        .into()
}

fn palette_input<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    interactive: bool,
    opacity: f32,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let active = !state.input().trim().is_empty();
    let connected = state.query_active() && (state.loading() || !state.rows().is_empty());

    let placeholder = match state.mode() {
        SearchMode::Addons => i18n.tr("search"),
        SearchMode::Files => i18n.tr("search-files"),
    };
    let input = text_input(&placeholder, state.input())
        .id(SEARCH_INPUT_ID)
        .on_input_maybe(interactive.then_some(Message::QueryEdited))
        .on_submit_maybe(interactive.then_some(Message::FullSearchSubmitted))
        .font(assets::fonts::default_font())
        .size(tokens.typography.body)
        .padding(Padding {
            top: SEARCH_FIELD_VERTICAL_PADDING,
            right: tokens.dims.search_palette_input_right_padding,
            bottom: SEARCH_FIELD_VERTICAL_PADDING,
            left: SEARCH_TEXT_X,
        })
        .width(Length::Fill)
        .style(move |_, status| search_text_input_style(&tokens, status, connected, opacity));

    let icon_color = if active {
        tokens.colors.text
    } else {
        tokens.colors.text_dim
    };
    let icon_overlay = row![
        Space::new().width(Length::Fixed(SEARCH_ICON_X)),
        svg(assets::icons::search())
            .width(Length::Fixed(SEARCH_ICON_SIZE))
            .height(Length::Fixed(SEARCH_ICON_SIZE))
            .style(move |_, _| svg::Style {
                color: Some(icon_color.into()),
            })
            .opacity(opacity),
        Space::new().width(Length::Fill),
    ]
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .height(Length::Fixed(SEARCH_FIELD_HEIGHT));

    let hit_base = container(Space::new().width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fixed(SEARCH_FIELD_HEIGHT));
    let hit_overlay: Element<'a, Message> = if interactive {
        mouse_area(hit_base)
            .on_press(Message::FocusRequested)
            .into()
    } else {
        hit_base.into()
    };

    let right_overlay = row![
        Space::new().width(Length::Fill),
        shortcut_chip(state.mode(), &tokens, opacity),
        Space::new().width(Length::Fixed(SEARCH_ICON_X)),
    ]
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .height(Length::Fixed(SEARCH_FIELD_HEIGHT));

    iced::widget::stack![input, icon_overlay, hit_overlay, right_overlay]
        .height(Length::Fixed(SEARCH_FIELD_HEIGHT))
        .width(Length::Fill)
        .clip(true)
        .into()
}

fn result_list<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    height: f32,
    interactive: bool,
    opacity: f32,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let list_height = dropdown_list_height(height);
    let virtual_rows = state.virtual_rows(list_height);
    let mut rows = column![].spacing(0.0).width(Length::Fill);

    if virtual_rows.top_padding > 0.0 {
        rows = rows.push(
            Space::new()
                .height(Length::Fixed(virtual_rows.top_padding))
                .width(Length::Fill),
        );
    }

    for row_model in &state.rows()[virtual_rows.range.clone()] {
        rows = rows.push(result_row(row_model, ctx, interactive, opacity));
    }

    if virtual_rows.bottom_padding > 0.0 {
        rows = rows.push(
            Space::new()
                .height(Length::Fixed(virtual_rows.bottom_padding))
                .width(Length::Fill),
        );
    }

    let list = scrollable(rows)
        .width(Length::Fill)
        .height(Length::Fixed(list_height))
        .direction(scrollable::Direction::Vertical(
            theme::styles::vertical_scrollbar(&tokens),
        ))
        .style(move |_, status| theme::styles::scrollbar(&tokens, status))
        .on_scroll(|viewport| Message::DropdownScrolled(viewport.absolute_offset().y));

    list.into()
}

fn result_row<'a>(
    row_model: &'a Row,
    ctx: ViewCtx<'a>,
    interactive: bool,
    opacity: f32,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let source = i18n.tr(row_model.source_label_key());
    let content = row![
        thumbnail(row_model, &tokens, opacity),
        column![
            text(row_model.title())
                .size(tokens.typography.body)
                .color(Color::from(tokens.colors.text).scale_alpha(opacity))
                .width(Length::Fill),
            text(row_model.association())
                .size(tokens.typography.caption)
                .color(Color::from(tokens.colors.text_dim).scale_alpha(opacity))
                .width(Length::Fill),
        ]
        .spacing(tokens.spacing.gap_xs)
        .width(Length::Fill),
        text(source)
            .size(tokens.typography.caption)
            .color(Color::from(tokens.colors.text_dim).scale_alpha(opacity))
            .width(Length::Fixed(SOURCE_LABEL_WIDTH))
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Right)
            .align_y(alignment::Vertical::Center),
    ]
    .spacing(tokens.spacing.gap)
    .align_y(Alignment::Center);

    button(content)
        .on_press_maybe(interactive.then_some(Message::ResultActivated(row_model.id())))
        .width(Length::Fill)
        .height(Length::Fixed(RESULT_ROW_HEIGHT))
        .padding([tokens.spacing.gap, tokens.spacing.gap])
        .style(move |_, status| row_button_style(&tokens, status, opacity))
        .into()
}

fn thumbnail<'a>(row_model: &Row, tokens: &Tokens, opacity: f32) -> Element<'a, Message> {
    let tokens = *tokens;
    let content: Element<'a, Message> = match row_model.thumbnail() {
        RowThumbnail::Ready(handle) => image(handle.clone())
            .width(Length::Fixed(THUMBNAIL_SIZE))
            .height(Length::Fixed(THUMBNAIL_SIZE))
            .content_fit(ContentFit::Cover)
            .opacity(opacity)
            .into(),
        RowThumbnail::Loading => loading_glyph(&tokens, LOADING_GLYPH_SIZE, opacity),
        RowThumbnail::Dead => dead_glyph(&tokens, DEAD_GLYPH_SIZE, opacity),
    };

    container(content)
        .width(Length::Fixed(THUMBNAIL_SIZE))
        .height(Length::Fixed(THUMBNAIL_SIZE))
        .center(Length::Fixed(THUMBNAIL_SIZE))
        .clip(true)
        .style(move |_| thumbnail_style(&tokens, opacity))
        .into()
}

fn status_row<'a>(
    label: String,
    loading: bool,
    tokens: &Tokens,
    opacity: f32,
) -> Element<'a, Message> {
    let icon = if loading {
        loading_glyph(tokens, 22.0, opacity)
    } else {
        dead_glyph(tokens, 22.0, opacity)
    };

    row![
        container(icon)
            .width(Length::Fixed(22.0))
            .height(Length::Fixed(22.0))
            .center(Length::Fixed(22.0)),
        text(label)
            .size(tokens.typography.body_sm)
            .color(Color::from(tokens.colors.text_dim).scale_alpha(opacity))
            .width(Length::Fill)
            .align_y(alignment::Vertical::Center),
    ]
    .padding(Padding {
        top: 13.0,
        right: tokens.spacing.gap,
        bottom: 13.0,
        left: tokens.spacing.gap,
    })
    .spacing(10.0)
    .align_y(Alignment::Center)
    .height(Length::Fixed(STATUS_ROW_HEIGHT))
    .width(Length::Fill)
    .into()
}

fn search_text_input_style(
    tokens: &Tokens,
    status: text_input::Status,
    connected: bool,
    opacity: f32,
) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    let value = if matches!(status, text_input::Status::Disabled) {
        tokens
            .colors
            .text
            .with_alpha((tokens.dims.disabled_opacity * 255.0).round() as u8)
    } else {
        tokens.colors.text
    };
    let radius = if connected {
        border::Radius::new(tokens.radii.base).bottom(0.0)
    } else {
        border::Radius::new(tokens.radii.base)
    };

    text_input::Style {
        background: Background::Color(
            theme::motion::scaled_alpha(tokens.colors.search_input_bg, opacity).into(),
        ),
        border: Border {
            color: Color::from(tokens.colors.focus_ring).scale_alpha(opacity),
            width: if focused {
                tokens.dims.focus_border_width
            } else {
                0.0
            },
            radius,
        },
        icon: Color::from(tokens.colors.icon_muted).scale_alpha(opacity),
        placeholder: Color::from(tokens.colors.text_dim).scale_alpha(opacity),
        value: Color::from(value).scale_alpha(opacity),
        selection: Color::from(tokens.colors.neutral).scale_alpha(opacity),
    }
}

pub fn dropdown_max_height(viewport_size: Size, tokens: &Tokens) -> f32 {
    if viewport_size.height <= 0.0 {
        return DROPDOWN_MAX_HEIGHT;
    }

    (viewport_size.height
        - tokens.dims.search_palette_top_offset
        - SEARCH_FIELD_HEIGHT
        - tokens.dims.search_palette_margin)
        .clamp(DROPDOWN_MIN_HEIGHT, DROPDOWN_MAX_HEIGHT)
}

pub fn dropdown_list_viewport_height(state: &State, viewport_size: Size, tokens: &Tokens) -> f32 {
    dropdown_list_height(dropdown_height(state, viewport_size, tokens))
}

fn dropdown_height(state: &State, viewport_size: Size, tokens: &Tokens) -> f32 {
    if !state.query_active() {
        return 0.0;
    }

    if state.rows().is_empty() && (state.loading() || state.show_empty()) {
        return STATUS_ROW_HEIGHT;
    }

    let max_height = dropdown_max_height(viewport_size, tokens);
    dropdown_list_height_for_count(state.rows().len(), max_height)
}

fn dropdown_list_height(panel_height: f32) -> f32 {
    panel_height.max(0.0)
}

fn dropdown_list_height_for_count(row_count: usize, max_height: f32) -> f32 {
    let row_height = row_count as f32 * RESULT_ROW_HEIGHT;
    row_height.min(max_height.max(0.0))
}

fn palette_width(viewport_width: f32, tokens: &Tokens) -> f32 {
    if !viewport_width.is_finite() || viewport_width <= 0.0 {
        return tokens.dims.search_palette_max_width;
    }

    let available = (viewport_width - tokens.dims.search_palette_margin * 2.0).max(1.0);
    let preferred = viewport_width * tokens.dims.search_palette_width_ratio;
    preferred
        .clamp(
            tokens.dims.search_palette_min_width,
            tokens.dims.search_palette_max_width,
        )
        .min(available)
}

fn palette_left(viewport_width: f32, width: f32) -> f32 {
    if !viewport_width.is_finite() || viewport_width <= width {
        return 0.0;
    }

    ((viewport_width - width) / 2.0).max(0.0)
}

fn dropdown_panel_style(tokens: &Tokens, opacity: f32) -> container::Style {
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

fn search_scrim_style(tokens: &Tokens, opacity: f32) -> container::Style {
    container::Style {
        background: Some(
            Color::from(theme::motion::scaled_alpha(
                tokens.colors.search_scrim,
                opacity,
            ))
            .into(),
        ),
        ..container::Style::default()
    }
}

fn keycap_style(tokens: &Tokens, opacity: f32) -> container::Style {
    container::Style {
        text_color: Some(Color::from(tokens.colors.text_dim).scale_alpha(opacity)),
        background: None,
        border: Border {
            color: Color::from(tokens.colors.search_keycap_border).scale_alpha(opacity),
            width: tokens.dims.border_width,
            radius: border::Radius::new(tokens.radii.base),
        },
        ..container::Style::default()
    }
}

fn divider<'a>(tokens: &Tokens, opacity: f32) -> Element<'a, Message> {
    let tokens = *tokens;
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
        })
        .into()
}

fn search_shortcut_hint(mode: SearchMode) -> &'static str {
    match (cfg!(target_os = "macos"), mode) {
        (true, SearchMode::Addons) => "⌘F",
        (true, SearchMode::Files) => "⌘K",
        (false, SearchMode::Addons) => "Ctrl+F",
        (false, SearchMode::Files) => "Ctrl+K",
    }
}

fn thumbnail_style(tokens: &Tokens, opacity: f32) -> container::Style {
    container::Style {
        background: Some(
            Color::from(tokens.colors.surface_2)
                .scale_alpha(opacity)
                .into(),
        ),
        border: Border {
            radius: tokens.radii.base.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

fn dead_glyph<'a>(tokens: &Tokens, size: f32, opacity: f32) -> Element<'a, Message> {
    let tokens = *tokens;
    svg(assets::icons::dead())
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .content_fit(ContentFit::Contain)
        .opacity(opacity)
        .style(move |_, _| svg::Style {
            color: Some(tokens.colors.text_dim.into()),
        })
        .into()
}

fn loading_glyph<'a>(tokens: &Tokens, size: f32, opacity: f32) -> Element<'a, Message> {
    let width = (size * 0.111).max(2.0);
    let heights = [0.56, 0.68, 1.0, 0.68, 0.56];
    let mut bars = row![]
        .spacing(size * 0.111)
        .align_y(alignment::Vertical::Center)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size));

    for factor in heights {
        bars = bars.push(loading_bar(width, size * factor, tokens, opacity));
    }

    container(bars)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .center(Length::Fixed(size))
        .into()
}

fn loading_bar<'a>(width: f32, height: f32, tokens: &Tokens, opacity: f32) -> Element<'a, Message> {
    let tokens = *tokens;
    container(
        Space::new()
            .width(Length::Fixed(width))
            .height(Length::Fixed(height.max(1.0))),
    )
    .width(Length::Fixed(width))
    .height(Length::Fixed(height.max(1.0)))
    .style(move |_| container::Style {
        background: Some(
            Color::from(tokens.colors.text_dim)
                .scale_alpha(opacity)
                .into(),
        ),
        border: Border {
            radius: (width * 0.4).into(),
            ..Border::default()
        },
        ..container::Style::default()
    })
    .into()
}

fn row_button_style(tokens: &Tokens, status: button::Status, opacity: f32) -> button::Style {
    let background = match status {
        button::Status::Hovered => Some(
            Color::from(theme::motion::scaled_alpha(
                tokens.colors.row_hover_fill_strong,
                opacity,
            ))
            .into(),
        ),
        button::Status::Pressed => Some(
            Color::from(theme::motion::scaled_alpha(
                tokens.colors.row_hover_fill,
                opacity,
            ))
            .into(),
        ),
        _ => None,
    };

    button::Style {
        background,
        text_color: Color::from(tokens.colors.text).scale_alpha(opacity),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}
