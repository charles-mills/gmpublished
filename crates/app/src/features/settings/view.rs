use iced::mouse;
use iced::widget::{
    Space, button, column, container, mouse_area, opaque, pick_list, row, scrollable, slider,
    stack, svg, text, text_input, toggler, tooltip,
};
use iced::{
    Background, Border, Center, Color, Degrees, Element, Length, Padding, Shadow, Size, Vector,
    alignment, border, font, gradient,
};

use crate::{
    assets,
    features::modal_stack::ResponsiveSize,
    i18n::{self, I18n},
    theme::{self, Tokens, ViewCtx},
};

use super::state::{self, ColorChannel, ColorSetting, PathSetting, ResetAction, SelectOption, Tab};
use super::{Message, State};

const TAB_RAIL_WIDTH: f32 = 200.0;
const COMPACT_BREAKPOINT: f32 = 900.0;
const COMPACT_TAB_BAR_HEIGHT: f32 = 48.0;
const TAB_BUTTON_HEIGHT: f32 = 33.0;
const FIELD_LABEL_HEIGHT: f32 = 18.0;
const CONTROL_HEIGHT: f32 = 40.0;
const SELECT_MENU_MAX_HEIGHT: f32 = 240.0;
const SELECT_ITEM_HEIGHT: f32 = 36.0;
const PATH_BROWSE_WIDTH: f32 = 112.0;
const COLOR_FIELD_COLLAPSED_HEIGHT: f32 = FIELD_LABEL_HEIGHT + 12.0 + CONTROL_HEIGHT;
const COLOR_FIELD_SPACING: f32 = 24.0;
const COLOR_PICKER_POPOVER_GAP: f32 = 8.0;
const COLOR_PICKER_POPOVER_MAX_WIDTH: f32 = 424.0;
const COLOR_PICKER_POPOVER_HEIGHT: f32 = 220.0;
const COLOR_PICKER_BUTTON_HEIGHT: f32 = 34.0;
const HSV_SLIDER_HEIGHT: f32 = 44.0;
const HSV_SLIDER_CONTROL_HEIGHT: f32 = 20.0;
const HSV_SLIDER_RAIL_HEIGHT: f32 = 18.0;
const HSV_SLIDER_HANDLE_WIDTH: u16 = 10;
const RESET_CONFIRM_WIDTH: f32 = 420.0;
const RESET_CONFIRM_HEIGHT: f32 = 220.0;

pub fn view<'a>(state: &'a State, ctx: ViewCtx<'a>, viewport_size: Size) -> Element<'a, Message> {
    if !state.open() {
        return container(Space::new()).center(Length::Fill).into();
    }

    let tokens = *ctx.tokens;
    let modal_size = ResponsiveSize::new(
        Size::new(
            tokens.dims.settings_modal_width,
            tokens.dims.settings_modal_height,
        ),
        Size::new(
            tokens.dims.settings_modal_max_width,
            tokens.dims.settings_modal_max_height,
        ),
    )
    .resolve(viewport_size, tokens.dims.modal_viewport_ratio);
    let compact = viewport_size.width < COMPACT_BREAKPOINT;
    let content: Element<'a, Message> = if compact {
        column![
            compact_tab_bar(state, ctx),
            content_panel(state, ctx, true).width(Length::Fill),
        ]
        .spacing(0.0)
        .height(Length::Fill)
        .into()
    } else {
        row![
            tab_rail(state, ctx),
            content_panel(state, ctx, false).width(Length::Fill),
        ]
        .spacing(0.0)
        .height(Length::Fill)
        .into()
    };

    let panel = opaque(
        container(content)
            .width(Length::Fixed(modal_size.width))
            .height(Length::Fixed(modal_size.height))
            .clip(true)
            .style(move |_| settings_modal_style(&tokens)),
    );

    let mut layers = stack![container(panel).center(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill);

    if let Some(kind) = state.active_color_picker() {
        layers = layers.push(color_picker_overlay(state, kind, ctx, modal_size, compact));
    }

    if let Some(action) = state.pending_reset() {
        layers = layers.push(confirm_overlay(action, ctx, viewport_size));
    }

    layers.into()
}

fn tab_rail<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let mut tabs = column![]
        .padding(tokens.spacing.pad)
        .spacing(tokens.spacing.gap_sm);

    for tab in Tab::ALL {
        tabs = tabs.push(tab_button(tab, state.active_tab() == tab, ctx));
    }

    container(tabs)
        .width(Length::Fixed(TAB_RAIL_WIDTH))
        .height(Length::Fill)
        .style(move |_| desktop_tab_rail_style(&tokens))
        .into()
}

fn compact_tab_bar<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let mut tabs = row![]
        .padding(Padding::ZERO.horizontal(12.0).vertical(6.0))
        .spacing(6.0)
        .height(Length::Fixed(COMPACT_TAB_BAR_HEIGHT));

    for tab in Tab::ALL {
        tabs = tabs.push(tab_button(tab, state.active_tab() == tab, ctx));
    }

    container(tabs)
        .width(Length::Fill)
        .height(Length::Fixed(COMPACT_TAB_BAR_HEIGHT))
        .style(move |_| compact_tab_rail_style(&tokens))
        .into()
}

fn tab_button(tab: Tab, active: bool, ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let label = text(i18n.tr(tab.label_key()))
        .size(tokens.typography.body)
        .font(theme::styles::inter_font(font::Weight::Normal))
        .color(if active {
            Color::from(tokens.colors.text)
        } else {
            Color::from(tokens.colors.text_dim)
        })
        .width(Length::Fill)
        .height(Length::Fixed(TAB_BUTTON_HEIGHT))
        .align_x(alignment::Horizontal::Left)
        .align_y(alignment::Vertical::Center);

    button(label)
        .on_press(Message::TabSelected(tab))
        .width(Length::Fill)
        .height(Length::Fixed(TAB_BUTTON_HEIGHT))
        .padding([0.0, 11.0])
        .style(move |_, status| tab_button_style(&tokens, active, status))
        .into()
}

fn content_panel<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    compact: bool,
) -> iced::widget::Container<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let body: Element<'a, Message> = match state.active_tab() {
        Tab::General => general_tab(state, ctx, compact),
        Tab::Paths => paths_tab(state, ctx),
        Tab::Accessibility => accessibility_tab(state, ctx),
        Tab::Resets => resets_tab(state, ctx),
    };

    let horizontal_padding = if compact { 16.0 } else { tokens.spacing.pad };
    let top_padding = if compact { 12.0 } else { tokens.spacing.pad };
    let bottom_padding = if compact { 16.0 } else { tokens.spacing.pad };

    let scrolling_content = scrollable(
        container(body)
            .padding(Padding::ZERO.horizontal(horizontal_padding))
            .width(Length::Fill),
    )
    .height(Length::Fill)
    .direction(scrollable::Direction::Vertical(
        theme::styles::vertical_scrollbar(&tokens),
    ))
    .style(move |_, status| theme::styles::scrollbar(&tokens, status));

    let mut content = column![
        container(scrolling_content)
            .padding(Padding::ZERO.top(top_padding).bottom(bottom_padding))
            .width(Length::Fill)
            .height(Length::Fill),
    ]
    .spacing(0.0);

    if let Some(status_key) = state.status_key() {
        content = content.push(
            container(
                text(i18n.tr(status_key))
                    .size(tokens.typography.body_sm)
                    .font(theme::styles::inter_font(font::Weight::Normal))
                    .color(if state.status_error() {
                        Color::from(tokens.colors.error)
                    } else {
                        Color::from(tokens.colors.text_dim)
                    }),
            )
            .height(Length::Fixed(42.0))
            .width(Length::Fill)
            .padding([8.0, horizontal_padding])
            .style(move |_| status_rail_style(&tokens, compact)),
        );
    }

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| content_panel_style(&tokens, compact))
}

fn general_tab<'a>(state: &'a State, ctx: ViewCtx<'a>, compact: bool) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mut content = column![
        switch_row(
            i18n.tr("settings-general-sounds"),
            state.settings().sounds,
            Message::SoundsToggled,
            &tokens,
        ),
        switch_row(
            i18n.tr("settings-general-play-gifs-by-default"),
            state.settings().play_gifs_by_default,
            Message::PlayGifsByDefaultToggled,
            &tokens,
        ),
    ];

    #[cfg(target_os = "macos")]
    {
        content = content.push(switch_row(
            i18n.tr("settings-system-titlebar"),
            state.settings().titlebar == crate::backend::TitlebarPreference::System,
            Message::SystemTitlebarToggled,
            &tokens,
        ));
    }

    content = content.push(select_field(
        i18n.tr("settings-general-language"),
        language_options(i18n),
        &state.language_value(),
        Message::LanguageSelected,
        &tokens,
    ));
    content = content.push(select_field(
        i18n.tr("settings-download-count-format-label"),
        download_count_options(i18n),
        state.download_count_format_value(),
        Message::DownloadCountFormatSelected,
        &tokens,
    ));
    content = content.push(select_field(
        i18n.tr("settings-theme-label"),
        theme_options(i18n),
        state.theme_value(),
        Message::ThemeSelected,
        &tokens,
    ));
    content = content.push(overwrite_field(state, ctx));

    content
        .spacing(if compact { 12.0 } else { 24.0 })
        .width(Length::Fill)
        .into()
}

fn switch_row<'a>(
    label: String,
    checked: bool,
    on_toggle: impl Fn(bool) -> Message + 'a,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let tokens = *tokens;
    row![
        text(label)
            .size(tokens.typography.body)
            .font(theme::styles::inter_font(font::Weight::Normal))
            .color(Color::from(tokens.colors.text)),
        toggler(checked)
            .on_toggle(on_toggle)
            .size(20.0)
            .style(move |_, status| switch_style(&tokens, status)),
    ]
    .align_y(Center)
    .spacing(6.0)
    .into()
}

fn select_field<'a>(
    label: String,
    options: Vec<SelectOption>,
    selected_value: &str,
    on_selected: impl Fn(SelectOption) -> Message + 'a,
    tokens: &Tokens,
) -> Element<'a, Message> {
    column![
        field_label(label, tokens),
        select_control(options, selected_value, on_selected, tokens),
    ]
    .spacing(12.0)
    .width(Length::Fill)
    .into()
}

fn overwrite_field<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let field = column![
        field_label(i18n.tr("settings-overwrite-label"), &tokens),
        select_control(
            overwrite_options(i18n),
            state.overwrite_mode_value(),
            Message::OverwriteModeSelected,
            &tokens,
        ),
    ]
    .spacing(8.0)
    .width(Length::Fill);

    tooltip(
        field,
        container(
            text(i18n.tr("settings-overwrite-tooltip"))
                .size(tokens.typography.body_sm)
                .font(theme::styles::inter_font(font::Weight::Normal))
                .color(Color::from(tokens.colors.text))
                .width(Length::Fixed(320.0)),
        )
        .padding(8.0)
        .style(move |_| theme::styles::tooltip(&tokens)),
        tooltip::Position::Bottom,
    )
    .gap(8.0)
    .into()
}

fn select_control<'a>(
    options: Vec<SelectOption>,
    selected_value: &str,
    on_selected: impl Fn(SelectOption) -> Message + 'a,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let tokens = *tokens;
    let selected = selected_option(&options, selected_value);
    let label = selected
        .as_ref()
        .map(|option| option.label.clone())
        .unwrap_or_default();
    let menu_height = SELECT_MENU_MAX_HEIGHT.min(options.len() as f32 * SELECT_ITEM_HEIGHT);

    let pick = pick_list(options, selected, on_selected)
        .width(Length::Fill)
        .padding([11.0, tokens.spacing.pad_sm])
        .text_size(tokens.typography.body)
        .font(theme::styles::inter_font(font::Weight::Normal))
        .handle(iced::widget::pick_list::Handle::None)
        .menu_height(Length::Fixed(menu_height))
        .style(move |_, status| select_pick_style(&tokens, status))
        .menu_style(move |_| select_menu_style(&tokens));

    let label_layer = container(
        text(label)
            .size(tokens.typography.body)
            .font(theme::styles::inter_font(font::Weight::Normal))
            .color(Color::from(tokens.colors.text))
            .align_x(alignment::Horizontal::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(CONTROL_HEIGHT))
    .center_y(Length::Fixed(CONTROL_HEIGHT))
    .padding([0.0, tokens.spacing.pad_sm]);

    let arrow = container(
        row![
            Space::new().width(Length::Fill),
            svg(assets::icons::chevron_down())
                .width(Length::Fixed(tokens.dims.icon_size_sm))
                .height(Length::Fixed(tokens.dims.icon_size_sm))
                .style(move |_, _| svg::Style {
                    color: Some(tokens.colors.text.into()),
                }),
        ]
        .align_y(Center)
        .padding([0.0, 14.0]),
    )
    .width(Length::Fill)
    .height(Length::Fixed(CONTROL_HEIGHT))
    .center_y(Length::Fixed(CONTROL_HEIGHT));

    container(stack![pick, label_layer, arrow])
        .width(Length::Fill)
        .height(Length::Fixed(CONTROL_HEIGHT))
        .clip(true)
        .into()
}

fn paths_tab<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let mut fields = column![].spacing(24.0).width(Length::Fill);
    for kind in PathSetting::ALL {
        fields = fields.push(path_field(state, kind, ctx));
    }
    fields.into()
}

fn path_field<'a>(state: &'a State, kind: PathSetting, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let invalid = state.path_error_key(kind).is_some();
    let input = text_input(&state.path_placeholder(kind), state.path_text(kind))
        .on_input(move |value| Message::PathEdited(kind, value))
        .on_submit(Message::PathAccepted(kind))
        .padding([11.0, tokens.spacing.pad_sm])
        .size(tokens.typography.body)
        .font(theme::styles::inter_font(font::Weight::Normal))
        .style(move |_, status| input_style(&tokens, invalid, status));

    let browse = text_button(i18n.tr("browse"), &tokens)
        .on_press(Message::PathBrowseRequested(kind))
        .width(Length::Fixed(PATH_BROWSE_WIDTH))
        .height(Length::Fixed(CONTROL_HEIGHT));

    let mut field = column![
        field_label(i18n.tr(kind.label_key()), &tokens),
        row![input, browse]
            .align_y(Center)
            .spacing(10.0)
            .height(Length::Fixed(CONTROL_HEIGHT)),
    ]
    .spacing(12.0)
    .width(Length::Fill);

    if let Some(error_key) = state.path_error_key(kind) {
        field = field.push(
            text(i18n.tr(error_key))
                .size(tokens.typography.caption)
                .font(theme::styles::inter_font(font::Weight::Normal))
                .color(Color::from(tokens.colors.error)),
        );
    }

    field.into()
}

fn accessibility_tab<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let mut fields = column![].spacing(24.0).width(Length::Fill);
    for kind in ColorSetting::ALL {
        fields = fields.push(color_field(state, kind, ctx));
    }
    fields.into()
}

fn color_field<'a>(state: &'a State, kind: ColorSetting, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let expanded = state.picker_expanded(kind);
    let rgb = if expanded {
        state.color_preview_rgb(kind)
    } else {
        state.color_rgb(kind)
    };
    let invalid = state.color_invalid(kind);
    let swatch = mouse_area(
        container(Space::new())
            .width(Length::Fixed(CONTROL_HEIGHT))
            .height(Length::Fixed(CONTROL_HEIGHT))
            .style(move |_| swatch_style(&tokens, rgb, expanded)),
    )
    .on_press(Message::ColorPickerToggled(kind))
    .interaction(mouse::Interaction::Pointer);

    let input = text_input("#RRGGBB", state.color_text(kind))
        .on_input(move |value| Message::ColorEdited(kind, value))
        .padding([11.0, tokens.spacing.pad_sm])
        .size(tokens.typography.body)
        .font(theme::styles::inter_font(font::Weight::Normal))
        .style(move |_, status| input_style(&tokens, invalid, status));

    let mut field = column![
        field_label(i18n.tr(kind.label_key()), &tokens),
        row![swatch, input]
            .align_y(Center)
            .spacing(10.0)
            .height(Length::Fixed(CONTROL_HEIGHT)),
    ]
    .spacing(12.0)
    .width(Length::Fill);

    if invalid {
        field = field.push(
            text(i18n.tr("settings-accessibility-invalid-hex"))
                .size(tokens.typography.caption)
                .font(theme::styles::inter_font(font::Weight::Normal))
                .color(Color::from(tokens.colors.error)),
        );
    }

    field.into()
}

fn color_picker_overlay<'a>(
    state: &'a State,
    kind: ColorSetting,
    ctx: ViewCtx<'a>,
    modal_size: Size,
    compact: bool,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let dismiss_layer = opaque(
        mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::ColorPickerCancelled),
    );

    let content_left = if compact { 0.0 } else { TAB_RAIL_WIDTH };
    let content_top = if compact { COMPACT_TAB_BAR_HEIGHT } else { 0.0 };
    let horizontal_padding = if compact { 16.0 } else { tokens.spacing.pad };
    let content_width = (modal_size.width - content_left).max(1.0);
    let picker_width =
        (content_width - horizontal_padding * 2.0).clamp(1.0, COLOR_PICKER_POPOVER_MAX_WIDTH);
    let top = color_picker_popover_top(kind, content_top, horizontal_padding, modal_size.height);

    let positioned = container(column![
        Space::new().height(Length::Fixed(top)),
        row![
            Space::new().width(Length::Fixed(content_left + horizontal_padding)),
            opaque(
                container(color_picker_popover(state, kind, ctx))
                    .width(Length::Fixed(picker_width))
            ),
        ]
    ])
    .width(Length::Fixed(modal_size.width))
    .height(Length::Fixed(modal_size.height));

    stack![dismiss_layer, container(positioned).center(Length::Fill),]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn color_picker_popover_top(
    kind: ColorSetting,
    content_top: f32,
    content_padding: f32,
    modal_height: f32,
) -> f32 {
    let row_index = kind.index() as f32;
    let field_top = content_top
        + content_padding
        + row_index * (COLOR_FIELD_COLLAPSED_HEIGHT + COLOR_FIELD_SPACING);
    let desired = field_top + COLOR_FIELD_COLLAPSED_HEIGHT + COLOR_PICKER_POPOVER_GAP;
    let max_top = modal_height - COLOR_PICKER_POPOVER_HEIGHT - content_padding;
    desired.min(max_top.max(content_top + content_padding))
}

fn color_picker_popover<'a>(
    state: &'a State,
    kind: ColorSetting,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let hsv = state.color_hsv(kind);
    let changed = state.color_picker_changed(kind);

    container(
        column![
            hsv_slider(
                HsvSliderSpec {
                    label: i18n.tr("settings-accessibility-color-picker-hue"),
                    value_text: format!("{:.0}°", hsv.hue.round()),
                    range: 0.0..=360.0,
                    value: hsv.hue,
                    step: 1.0,
                    channel: ColorChannel::Hue,
                    rail: hue_gradient(),
                },
                kind,
                &tokens,
            ),
            hsv_slider(
                HsvSliderSpec {
                    label: i18n.tr("settings-accessibility-color-picker-saturation"),
                    value_text: format!("{:.0}%", (hsv.saturation * 100.0).round()),
                    range: 0.0..=1.0,
                    value: hsv.saturation,
                    step: 0.01,
                    channel: ColorChannel::Saturation,
                    rail: two_stop_gradient(
                        hsv_to_color(hsv.hue, 0.0, hsv.value),
                        hsv_to_color(hsv.hue, 1.0, hsv.value),
                    ),
                },
                kind,
                &tokens,
            ),
            hsv_slider(
                HsvSliderSpec {
                    label: i18n.tr("settings-accessibility-color-picker-value"),
                    value_text: format!("{:.0}%", (hsv.value * 100.0).round()),
                    range: 0.0..=1.0,
                    value: hsv.value,
                    step: 0.01,
                    channel: ColorChannel::Value,
                    rail: two_stop_gradient(
                        Color::BLACK,
                        hsv_to_color(hsv.hue, hsv.saturation, 1.0)
                    ),
                },
                kind,
                &tokens,
            ),
            row![
                Space::new().width(Length::Fill),
                text_button(i18n.tr("cancel"), &tokens)
                    .on_press(Message::ColorPickerCancelled)
                    .width(Length::Fixed(88.0))
                    .height(Length::Fixed(COLOR_PICKER_BUTTON_HEIGHT)),
                primary_button(
                    i18n.tr("settings-accessibility-color-picker-apply"),
                    &tokens,
                    false,
                )
                .on_press_maybe(changed.then_some(Message::ColorPickerApplied(kind)))
                .width(Length::Fixed(88.0))
                .height(Length::Fixed(COLOR_PICKER_BUTTON_HEIGHT)),
            ]
            .align_y(Center)
            .spacing(10.0)
            .height(Length::Fixed(COLOR_PICKER_BUTTON_HEIGHT)),
        ]
        .spacing(10.0),
    )
    .width(Length::Fill)
    .height(Length::Fixed(COLOR_PICKER_POPOVER_HEIGHT))
    .padding(tokens.spacing.pad_sm)
    .style(move |_| color_picker_popover_style(&tokens))
    .into()
}

// Per-slider display state (label/value text/range/step/channel/rail),
// bundled so the three `hsv_slider` call sites read as named fields instead
// of a run of positional strings and f32s that are easy to transpose.
struct HsvSliderSpec {
    label: String,
    value_text: String,
    range: std::ops::RangeInclusive<f32>,
    value: f32,
    step: f32,
    channel: ColorChannel,
    rail: Background,
}

fn hsv_slider<'a>(
    spec: HsvSliderSpec,
    kind: ColorSetting,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let tokens = *tokens;
    let HsvSliderSpec {
        label,
        value_text,
        range,
        value,
        step,
        channel,
        rail,
    } = spec;

    column![
        row![
            text(label)
                .size(tokens.typography.body_sm)
                .font(theme::styles::inter_font(font::Weight::Normal))
                .color(Color::from(tokens.colors.text))
                .width(Length::Fill),
            text(value_text)
                .size(tokens.typography.caption)
                .font(theme::styles::inter_font(font::Weight::Normal))
                .color(Color::from(tokens.colors.text_dim))
                .align_x(alignment::Horizontal::Right)
                .width(Length::Fixed(46.0)),
        ]
        .align_y(Center)
        .height(Length::Fixed(18.0)),
        slider(range, value, move |value| {
            Message::ColorPickerChannelChanged(kind, channel, value)
        })
        .step(step)
        .height(HSV_SLIDER_CONTROL_HEIGHT)
        .style(move |_, status| hsv_slider_style(&tokens, rail, status)),
    ]
    .spacing(6.0)
    .height(Length::Fixed(HSV_SLIDER_HEIGHT))
    .into()
}

fn resets_tab<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    column![
        reset_button(ResetAction::Settings, state.reset_busy(), ctx),
        reset_button(ResetAction::TempFiles, state.reset_busy(), ctx),
        reset_button(ResetAction::UserData, state.reset_busy(), ctx),
    ]
    .spacing(24.0)
    .width(Length::Fill)
    .into()
}

fn reset_button(action: ResetAction, busy: bool, ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    container(
        primary_button(i18n.tr(action.label_key()), &tokens, true)
            .on_press_maybe((!busy).then_some(Message::ResetRequested(action)))
            .width(Length::Fill)
            .height(Length::Fixed(CONTROL_HEIGHT)),
    )
    .height(Length::Fixed(46.0))
    .width(Length::Fill)
    .into()
}

fn confirm_overlay(
    action: ResetAction,
    ctx: ViewCtx<'_>,
    viewport_size: Size,
) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let dialog_size = ResponsiveSize::new(
        Size::new(RESET_CONFIRM_WIDTH, RESET_CONFIRM_HEIGHT),
        Size::new(RESET_CONFIRM_WIDTH, RESET_CONFIRM_HEIGHT),
    )
    .resolve(viewport_size, tokens.dims.modal_viewport_ratio);
    let dialog = container(
        column![
            text(i18n.tr(action.label_key()))
                .size(tokens.typography.title_sm)
                .font(theme::styles::inter_font(font::Weight::Bold))
                .color(Color::from(tokens.colors.text)),
            text(i18n.tr("settings-resets-confirm-body"))
                .size(tokens.typography.body)
                .font(theme::styles::inter_font(font::Weight::Normal))
                .color(Color::from(tokens.colors.text_dim))
                .width(Length::Fill),
            row![
                Space::new().width(Length::Fill),
                text_button(i18n.tr("cancel"), &tokens)
                    .on_press(Message::ResetCancelled)
                    .height(Length::Fixed(CONTROL_HEIGHT)),
                primary_button(i18n.tr(action.label_key()), &tokens, true)
                    .on_press(Message::ResetConfirmed)
                    .width(Length::Fixed(132.0))
                    .height(Length::Fixed(CONTROL_HEIGHT)),
            ]
            .align_y(Center)
            .spacing(10.0),
        ]
        .spacing(14.0)
        .padding(Padding::ZERO.horizontal(20.0).vertical(18.0))
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fixed(dialog_size.width))
    .height(Length::Fixed(dialog_size.height))
    .style(move |_| confirm_dialog_style(&tokens));

    let scrim = container(dialog)
        .center(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Color::from(tokens.colors.scrim_strong).into()),
            ..container::Style::default()
        });

    opaque(scrim)
}

fn field_label(label: String, tokens: &Tokens) -> Element<'static, Message> {
    text(label)
        .size(tokens.typography.body)
        .font(theme::styles::inter_font(font::Weight::Normal))
        .color(Color::from(tokens.colors.text))
        .height(Length::Fixed(FIELD_LABEL_HEIGHT))
        .into()
}

fn text_button<'a>(label: String, tokens: &Tokens) -> iced::widget::Button<'a, Message> {
    let tokens = *tokens;
    button(
        text(label)
            .size(tokens.typography.body)
            .font(theme::styles::inter_font(font::Weight::Semibold))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center),
    )
    .padding([0.0, tokens.spacing.pad_control_x])
    .style(move |_, status| text_button_style(&tokens, status))
}

fn primary_button<'a>(
    label: String,
    tokens: &Tokens,
    danger: bool,
) -> iced::widget::Button<'a, Message> {
    let tokens = *tokens;
    button(
        text(label)
            .size(tokens.typography.body)
            .font(theme::styles::inter_font(font::Weight::Semibold))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center),
    )
    .padding([0.0, tokens.spacing.pad_control_x])
    .style(move |_, status| primary_button_style(&tokens, danger, status))
}

fn language_options(i18n: &I18n) -> Vec<SelectOption> {
    let mut options = Vec::with_capacity(i18n::available_languages().len() + 1);
    options.push(SelectOption::new(
        i18n.tr("settings-general-language-default"),
        state::default_language_value(),
    ));
    options.extend(
        i18n::available_languages()
            .iter()
            .map(|language| SelectOption::new(language.name.clone(), language.id)),
    );
    options
}

fn theme_options(i18n: &I18n) -> Vec<SelectOption> {
    vec![
        SelectOption::new(i18n.tr("settings-theme-auto"), "auto"),
        SelectOption::new(i18n.tr("settings-theme-dark"), "dark"),
        SelectOption::new(i18n.tr("settings-theme-light"), "light"),
        SelectOption::new(i18n.tr("settings-theme-classic-source"), "classic_source"),
    ]
}

fn download_count_options(i18n: &I18n) -> Vec<SelectOption> {
    vec![
        SelectOption::new(
            i18n.tr("settings-download-count-format-automatic"),
            "automatic",
        ),
        SelectOption::new(i18n.tr("settings-download-count-format-comma"), "comma"),
        SelectOption::new(i18n.tr("settings-download-count-format-period"), "period"),
        SelectOption::new(i18n.tr("settings-download-count-format-space"), "space"),
        SelectOption::new(i18n.tr("settings-download-count-format-plain"), "plain"),
    ]
}

fn overwrite_options(i18n: &I18n) -> Vec<SelectOption> {
    vec![
        SelectOption::new(i18n.tr("settings-overwrite-recycle"), "recycle"),
        SelectOption::new(i18n.tr("settings-overwrite-delete"), "delete"),
        SelectOption::new(i18n.tr("settings-overwrite-overwrite"), "overwrite"),
    ]
}

fn selected_option(options: &[SelectOption], value: &str) -> Option<SelectOption> {
    options
        .iter()
        .find(|option| option.value == value)
        .cloned()
        .or_else(|| options.first().cloned())
}

fn settings_modal_style(tokens: &Tokens) -> container::Style {
    container::Style {
        background: Some(Color::from(tokens.colors.bg).into()),
        text_color: Some(tokens.colors.text.into()),
        border: Border {
            radius: tokens.radii.md.into(),
            ..Border::default()
        },
        shadow: Shadow {
            color: tokens.colors.shadow_soft.into(),
            offset: Vector::ZERO,
            blur_radius: 10.0,
        },
        snap: true,
    }
}

fn desktop_tab_rail_style(tokens: &Tokens) -> container::Style {
    rail_style_with_radius(
        tokens,
        border::radius(tokens.radii.md)
            .top_right(0.0)
            .bottom_right(0.0),
    )
}

fn compact_tab_rail_style(tokens: &Tokens) -> container::Style {
    rail_style_with_radius(
        tokens,
        border::radius(tokens.radii.md)
            .bottom_left(0.0)
            .bottom_right(0.0),
    )
}

fn content_panel_style(tokens: &Tokens, compact: bool) -> container::Style {
    let radius = if compact {
        border::radius(tokens.radii.md).top_left(0.0).top_right(0.0)
    } else {
        border::radius(tokens.radii.md)
            .top_left(0.0)
            .bottom_left(0.0)
    };

    container::Style {
        background: Some(Color::from(tokens.colors.bg).into()),
        text_color: Some(tokens.colors.text.into()),
        border: Border {
            radius,
            ..Border::default()
        },
        ..container::Style::default()
    }
}

fn status_rail_style(tokens: &Tokens, compact: bool) -> container::Style {
    let radius = if compact {
        border::radius(tokens.radii.md).top_left(0.0).top_right(0.0)
    } else {
        border::radius(0.0).bottom_right(tokens.radii.md)
    };
    rail_style_with_radius(tokens, radius)
}

fn rail_style_with_radius(tokens: &Tokens, radius: border::Radius) -> container::Style {
    container::Style {
        background: Some(Color::from(tokens.colors.row_fill_subtle).into()),
        text_color: Some(tokens.colors.text.into()),
        border: Border {
            radius,
            ..Border::default()
        },
        ..container::Style::default()
    }
}

fn tab_button_style(tokens: &Tokens, active: bool, status: button::Status) -> button::Style {
    let background = if active {
        tokens.colors.surface_2
    } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        tokens.colors.sidebar_item_hover
    } else {
        tokens.colors.bg.with_alpha(0)
    };

    button::Style {
        background: Some(Color::from(background).into()),
        text_color: tokens.colors.text.into(),
        border: Border {
            radius: tokens.radii.base.into(),
            ..Border::default()
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

fn switch_style(tokens: &Tokens, status: toggler::Status) -> toggler::Style {
    let checked = match status {
        toggler::Status::Active { is_toggled }
        | toggler::Status::Hovered { is_toggled }
        | toggler::Status::Disabled { is_toggled } => is_toggled,
    };
    let disabled = matches!(status, toggler::Status::Disabled { .. });
    let alpha = if disabled {
        theme::motion::opacity_byte(tokens.dims.disabled_opacity)
    } else {
        255
    };
    toggler::Style {
        background: if checked {
            Color::from(tokens.colors.switch_on.with_alpha(alpha)).into()
        } else {
            Color::from(tokens.colors.switch_off.with_alpha(alpha)).into()
        },
        background_border_width: 0.0,
        background_border_color: Color::TRANSPARENT,
        foreground: Color::from(tokens.colors.switch_knob.with_alpha(alpha)).into(),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: Some(tokens.colors.text.into()),
        border_radius: Some((CONTROL_HEIGHT / 4.0).into()),
        padding_ratio: 0.1,
    }
}

fn select_pick_style(
    tokens: &Tokens,
    status: iced::widget::pick_list::Status,
) -> iced::widget::pick_list::Style {
    let open = matches!(status, iced::widget::pick_list::Status::Opened { .. });
    iced::widget::pick_list::Style {
        text_color: Color::TRANSPARENT,
        placeholder_color: Color::TRANSPARENT,
        handle_color: Color::TRANSPARENT,
        background: Color::from(tokens.colors.input_bg).into(),
        border: Border {
            color: tokens.colors.focus_ring.into(),
            width: if open {
                tokens.dims.focus_border_width
            } else {
                0.0
            },
            radius: tokens.radii.base.into(),
        },
    }
}

fn select_menu_style(tokens: &Tokens) -> iced::overlay::menu::Style {
    iced::overlay::menu::Style {
        background: Color::from(tokens.colors.menu_bg).into(),
        border: Border {
            radius: tokens.radii.md.into(),
            ..Border::default()
        },
        text_color: tokens.colors.text.into(),
        selected_text_color: tokens.colors.text.into(),
        selected_background: Color::from(tokens.colors.selected_fill).into(),
        shadow: Shadow {
            color: tokens.colors.shadow_dropdown.into(),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 12.0,
        },
    }
}

fn input_style(tokens: &Tokens, error: bool, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    let disabled = matches!(status, text_input::Status::Disabled);
    let border_color = if error {
        tokens.colors.error_dark
    } else {
        tokens.colors.focus_ring
    };
    let value = if disabled {
        tokens
            .colors
            .text
            .with_alpha(theme::motion::opacity_byte(tokens.dims.disabled_opacity))
    } else {
        tokens.colors.text
    };

    text_input::Style {
        background: Color::from(tokens.colors.input_bg).into(),
        border: Border {
            color: border_color.into(),
            width: if focused || error {
                tokens.dims.focus_border_width
            } else {
                0.0
            },
            radius: tokens.radii.base.into(),
        },
        icon: tokens.colors.icon_muted.into(),
        placeholder: tokens.colors.text_dim.into(),
        value: value.into(),
        selection: tokens.colors.neutral.into(),
    }
}

fn swatch_style(tokens: &Tokens, rgb: u32, expanded: bool) -> container::Style {
    container::Style {
        background: Some(Background::Color(rgb_color(rgb))),
        border: Border {
            color: if expanded {
                tokens.colors.focus_ring.into()
            } else {
                tokens.colors.border_subtle.into()
            },
            width: if expanded {
                tokens.dims.focus_border_width
            } else {
                tokens.dims.border_width
            },
            radius: tokens.radii.base.into(),
        },
        ..container::Style::default()
    }
}

fn color_picker_popover_style(tokens: &Tokens) -> container::Style {
    container::Style {
        background: Some(Color::from(tokens.colors.surface_raised).into()),
        text_color: Some(tokens.colors.text.into()),
        border: Border {
            color: tokens.colors.border_subtle.into(),
            width: tokens.dims.border_width,
            radius: tokens.radii.md.into(),
        },
        shadow: Shadow {
            color: tokens.colors.shadow_dropdown.into(),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 14.0,
        },
        snap: true,
    }
}

fn hsv_slider_style(tokens: &Tokens, rail: Background, _status: slider::Status) -> slider::Style {
    slider::Style {
        rail: slider::Rail {
            backgrounds: (rail, rail),
            width: HSV_SLIDER_RAIL_HEIGHT,
            border: Border {
                color: tokens.colors.border_subtle.into(),
                width: tokens.dims.border_width,
                radius: tokens.radii.base.into(),
            },
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Rectangle {
                width: HSV_SLIDER_HANDLE_WIDTH,
                border_radius: tokens.radii.xs.into(),
            },
            background: Color::from(tokens.colors.text).into(),
            border_width: tokens.dims.border_width,
            border_color: tokens.colors.shadow_strong.into(),
        },
    }
}

fn text_button_style(tokens: &Tokens, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Active => tokens.colors.button_bg,
        button::Status::Hovered => tokens.colors.control_bg,
        button::Status::Pressed => tokens.colors.button_pressed,
        button::Status::Disabled => tokens
            .colors
            .button_bg
            .with_alpha(theme::motion::opacity_byte(tokens.dims.disabled_opacity)),
    };

    button::Style {
        background: Some(Color::from(background).into()),
        text_color: tokens.colors.text.into(),
        border: Border {
            radius: tokens.radii.base.into(),
            ..Border::default()
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

fn primary_button_style(tokens: &Tokens, danger: bool, status: button::Status) -> button::Style {
    let (base, pressed, text_color) = if danger {
        (
            tokens.colors.error,
            tokens.colors.error_dark,
            tokens.colors.text_on_error,
        )
    } else {
        (
            tokens.colors.neutral,
            tokens.colors.neutral_dark,
            tokens.colors.text_on_neutral,
        )
    };
    let background = match status {
        button::Status::Active => base,
        button::Status::Hovered | button::Status::Pressed => pressed,
        button::Status::Disabled => {
            base.with_alpha(theme::motion::opacity_byte(tokens.dims.disabled_opacity))
        }
    };

    button::Style {
        background: Some(Color::from(background).into()),
        text_color: text_color.into(),
        border: Border {
            radius: tokens.radii.base.into(),
            ..Border::default()
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

fn confirm_dialog_style(tokens: &Tokens) -> container::Style {
    container::Style {
        background: Some(Color::from(tokens.colors.surface).into()),
        text_color: Some(tokens.colors.text.into()),
        border: Border {
            radius: tokens.radii.md.into(),
            ..Border::default()
        },
        shadow: Shadow {
            color: tokens.colors.shadow.into(),
            offset: Vector::new(0.0, 0.0),
            blur_radius: 10.0,
        },
        snap: true,
    }
}

fn hue_gradient() -> Background {
    gradient::Linear::new(Degrees(90.0))
        .add_stop(0.0, Color::from_rgb8(255, 0, 0))
        .add_stop(0.16666, Color::from_rgb8(255, 255, 0))
        .add_stop(0.33333, Color::from_rgb8(0, 255, 0))
        .add_stop(0.5, Color::from_rgb8(0, 255, 255))
        .add_stop(0.66666, Color::from_rgb8(0, 0, 255))
        .add_stop(0.83333, Color::from_rgb8(255, 0, 255))
        .add_stop(1.0, Color::from_rgb8(255, 0, 0))
        .into()
}

fn two_stop_gradient(start: Color, end: Color) -> Background {
    gradient::Linear::new(Degrees(90.0))
        .add_stop(0.0, start)
        .add_stop(1.0, end)
        .into()
}

fn hsv_to_color(hue: f32, saturation: f32, value: f32) -> Color {
    let rgb = state::HsvColor {
        hue,
        saturation,
        value,
    }
    .to_rgb();
    rgb_color(rgb)
}

fn rgb_color(rgb: u32) -> Color {
    let [_, red, green, blue] = (rgb & 0xFF_FFFF).to_be_bytes();
    Color::from_rgb8(red, green, blue)
}
