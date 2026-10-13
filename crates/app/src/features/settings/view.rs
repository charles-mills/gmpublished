use iced::mouse;
use iced::widget::{
    Space, button, column, container, mouse_area, opaque, row, scrollable, slider, stack, svg,
    text, text_input, toggler,
};
use iced::{
    Background, Border, Center, Color, Degrees, Element, Length, Padding, Shadow, Size, Vector,
    alignment, border, font, gradient,
};

use crate::{
    assets,
    bridge::{DownloadCountFormat, ExtractionOverwriteMode, ThemePreset, UiDensity},
    features::modal_stack::ResponsiveSize,
    i18n::{self, I18n},
    theme::{self, Tokens, ViewCtx},
    widgets::{
        keyboard_control::{activate, keyboard_control},
        select, tooltip as tooltip_widget,
    },
};

use super::state::{self, ColorChannel, ColorSetting, PathSetting, ResetAction, Tab};
use super::{Message, State};

const TAB_RAIL_WIDTH: f32 = 200.0;
const COMPACT_BREAKPOINT: f32 = 900.0;
const COMPACT_TAB_BAR_HEIGHT: f32 = 48.0;
const TAB_BUTTON_HEIGHT: f32 = 33.0;
const FIELD_LABEL_GAP: f32 = 12.0;
const SELECT_HINT_MAX_WIDTH: f32 = 320.0;
const CONTROL_HEIGHT: f32 = 40.0;
/// Vertical padding of a select control, per edge; also the padding of every
/// option row in its menu, so the two line up.
const SELECT_PAD_Y: f32 = 11.0;
const PATH_BROWSE_WIDTH: f32 = 112.0;
const COLOR_PICKER_POPOVER_MAX_WIDTH: f32 = 424.0;
const COLOR_PICKER_BUTTON_HEIGHT: f32 = 34.0;
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
    // A text-size drag must not replace the row/column widget tree mid-edit.
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
            .id("settings-modal")
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
        tabs = tabs.push(tab_button(tab, state.active_tab() == tab, ctx, false));
    }

    let content = column![
        tabs,
        Space::new().height(Length::Fill),
        container(
            column![
                text("gmpublished")
                    .size(tokens.typography.caption_xs)
                    .color(tokens.colors.text_dim),
                text(super::about::VERSION)
                    .size(tokens.typography.caption_xs)
                    .color(tokens.colors.text_dim),
            ]
            .spacing(tokens.space(2.0))
        )
        .padding(
            Padding::ZERO
                .left(tokens.spacing.pad + 11.0)
                .bottom(tokens.spacing.pad)
        ),
    ];

    container(content)
        .width(Length::Fixed(TAB_RAIL_WIDTH * tokens.text_scale()))
        .height(Length::Fill)
        .style(move |_| desktop_tab_rail_style(&tokens))
        .into()
}

fn compact_tab_bar<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let mut tabs = row![]
        .padding(tokens.space(6.0))
        .spacing(tokens.spacing.gap_sm);
    for tab in Tab::ALL {
        tabs = tabs.push(tab_button(tab, state.active_tab() == tab, ctx, true));
    }
    container(
        scrollable(tabs).direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::default(),
        )),
    )
    .width(Length::Fill)
    .height(Length::Fixed(tokens.row_height(COMPACT_TAB_BAR_HEIGHT)))
    .style(move |_| compact_tab_rail_style(&tokens))
    .into()
}

fn tab_button(tab: Tab, active: bool, ctx: ViewCtx<'_>, compact: bool) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let label = text(i18n.tr(tab.label_key()))
        .size(tokens.typography.body)
        .wrapping(text::Wrapping::None)
        .font(theme::styles::inter_font(font::Weight::Normal))
        .color(if active {
            Color::from(tokens.colors.text)
        } else {
            Color::from(tokens.colors.text_dim)
        })
        .width(if compact {
            Length::Shrink
        } else {
            Length::Fill
        })
        .height(Length::Fixed(tokens.row_height(TAB_BUTTON_HEIGHT)))
        .align_x(alignment::Horizontal::Left)
        .align_y(alignment::Vertical::Center);

    let control = button(label)
        .on_press(Message::TabSelected(tab))
        .width(if compact {
            Length::Shrink
        } else {
            Length::Fill
        })
        .height(Length::Fixed(tokens.row_height(TAB_BUTTON_HEIGHT)))
        .padding([0.0, tokens.space(11.0)])
        .style(move |_, status| tab_button_style(&tokens, active, status));
    keyboard_control(control, tab.label_key(), &tokens, move |key| {
        activate(key).then_some(Message::TabSelected(tab))
    })
}

fn content_panel<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    compact: bool,
) -> iced::widget::Container<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let body: Element<'a, Message> = match state.active_tab() {
        Tab::About => super::about_view::view(state, ctx, compact),
        Tab::General => general_tab(state, ctx, compact),
        Tab::Paths => paths_tab(state, ctx),
        Tab::Accessibility => accessibility_tab(state, ctx),
        Tab::Resets => resets_tab(state, ctx),
    };
    let body = crate::widgets::settle::settle(body, state.active_tab() as u64, &tokens);

    let horizontal_padding = if compact {
        tokens.space(16.0)
    } else {
        tokens.spacing.pad
    };
    let top_padding = if compact {
        tokens.space(12.0)
    } else {
        tokens.spacing.pad
    };
    let bottom_padding = if compact {
        tokens.space(16.0)
    } else {
        tokens.spacing.pad
    };

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
            .height(Length::Shrink)
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
            state.settings().backend.sounds,
            Message::SoundsToggled,
            &tokens,
        ),
        switch_row(
            i18n.tr("settings-general-play-gifs-by-default"),
            state.settings().ui.play_gifs_by_default,
            Message::PlayGifsByDefaultToggled,
            &tokens,
        ),
    ];

    #[cfg(target_os = "macos")]
    {
        content = content.push(switch_row(
            i18n.tr("settings-system-titlebar"),
            state.settings().backend.titlebar == crate::bridge::TitlebarPreference::System,
            Message::SystemTitlebarToggled,
            &tokens,
        ));
    }

    content = content.push(select_field(
        i18n.tr("settings-general-language"),
        None,
        language_options(i18n),
        &state.language(),
        |language| Message::LanguageSelected(language.map(str::to_owned)),
        &tokens,
    ));
    content = content.push(select_field(
        i18n.tr("settings-download-count-format-label"),
        None,
        download_count_options(i18n),
        &state.download_count_format(),
        Message::DownloadCountFormatSelected,
        &tokens,
    ));
    content = content.push(select_field(
        i18n.tr("settings-theme-label"),
        None,
        theme_options(i18n),
        &state.theme_preset(),
        Message::ThemeSelected,
        &tokens,
    ));
    content = content.push(select_field(
        i18n.tr("settings-overwrite-label"),
        Some(i18n.tr("settings-overwrite-tooltip")),
        overwrite_options(i18n),
        &state.overwrite_mode(),
        Message::OverwriteModeSelected,
        &tokens,
    ));

    content
        .spacing(if compact {
            tokens.spacing.gap
        } else {
            tokens.space(24.0)
        })
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
        switch_control(checked, on_toggle, &tokens),
    ]
    .align_y(Center)
    .spacing(tokens.space(6.0))
    .into()
}

fn switch_control<'a>(
    checked: bool,
    on_toggle: impl Fn(bool) -> Message + 'a,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let tokens = *tokens;
    toggler(checked)
        .on_toggle(on_toggle)
        .size(tokens.dims.switch_height)
        .style(move |_, status| switch_style(&tokens, status))
        .into()
}

fn select_field<'a, T: Clone + Eq>(
    label: String,
    hint: Option<String>,
    options: Vec<(T, String)>,
    selected: &T,
    on_selected: impl Fn(T) -> Message + 'a,
    tokens: &Tokens,
) -> Element<'a, Message> {
    column![
        field_label(label, tokens),
        select_control(hint, options, selected, on_selected, tokens),
    ]
    .spacing(tokens.space(FIELD_LABEL_GAP))
    .width(Length::Fill)
    .into()
}

fn select_control<'a, T: Clone + Eq>(
    hint: Option<String>,
    options: Vec<(T, String)>,
    selected: &T,
    on_selected: impl Fn(T) -> Message + 'a,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let tokens = *tokens;
    let current = options
        .iter()
        .find(|(value, _)| value == selected)
        .or_else(|| options.first())
        .map(|(_, label)| label.clone())
        .unwrap_or_default();
    let metrics = select_metrics(&tokens);
    let geometry = select::geometry(options.len(), metrics, &tokens);

    let rows = options
        .into_iter()
        .map(|(value, label)| select::Row {
            selected: &value == selected,
            label,
            on_select: on_selected(value),
        })
        .collect();

    let closed = select_face(current.clone(), select::Face::Collapsed, &tokens);
    let closed = match hint {
        Some(hint) => tooltip_widget::below(closed, hint, &tokens, SELECT_HINT_MAX_WIDTH),
        None => closed,
    };
    select::select(
        closed,
        select_face(current, select::Face::Expanded, &tokens),
        select::menu(rows, metrics, &tokens),
        geometry,
    )
    .into()
}

fn select_face<'a>(label: String, face: select::Face, tokens: &Tokens) -> Element<'a, Message> {
    let tokens = *tokens;
    let label_layer = container(
        text(label)
            .size(tokens.typography.body)
            .font(theme::styles::inter_font(font::Weight::Normal))
            .color(Color::from(tokens.colors.text))
            .align_x(alignment::Horizontal::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(control_height(&tokens)))
    .center_y(Length::Fixed(control_height(&tokens)))
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
        .padding([0.0, tokens.space(14.0)]),
    )
    .width(Length::Fill)
    .height(Length::Fixed(control_height(&tokens)))
    .center_y(Length::Fixed(control_height(&tokens)));

    container(stack![label_layer, arrow])
        .width(Length::Fill)
        .height(Length::Fixed(control_height(&tokens)))
        .clip(true)
        .style(move |_| theme::styles::select_face(&tokens, face.is_expanded()))
        .into()
}

fn paths_tab<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let mut fields = column![]
        .spacing(ctx.tokens.space(24.0))
        .width(Length::Fill);
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
        .padding([tokens.space(SELECT_PAD_Y), tokens.spacing.pad_sm])
        .size(tokens.typography.body)
        .font(theme::styles::inter_font(font::Weight::Normal))
        .style(move |_, status| input_style(&tokens, invalid, status));

    let browse = text_button(i18n.tr("browse"), &tokens)
        .on_press(Message::PathBrowseRequested(kind))
        .width(Length::Fixed(PATH_BROWSE_WIDTH * tokens.text_scale()))
        .height(Length::Fixed(control_height(&tokens)));

    let mut field = column![
        field_label(i18n.tr(kind.label_key()), &tokens),
        row![input, browse]
            .align_y(Center)
            .spacing(tokens.space(10.0))
            .height(Length::Fixed(control_height(&tokens))),
    ]
    .spacing(tokens.space(12.0))
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
    let tokens = *ctx.tokens;
    let mut fields = column![
        display_preferences(state, ctx),
        text(ctx.i18n.tr("settings-display-colors"))
            .size(tokens.typography.title_sm)
            .color(tokens.colors.text),
    ]
    .spacing(tokens.space(24.0))
    .width(Length::Fill);
    for kind in ColorSetting::ALL {
        fields = fields.push(color_field(state, kind, ctx));
    }
    fields.into()
}

fn display_preferences<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let value = state.settings().ui.text_size_percent;
    let spacing = select_control(
        None,
        vec![
            (
                UiDensity::Comfortable,
                i18n.tr("settings-display-comfortable"),
            ),
            (UiDensity::Compact, i18n.tr("settings-display-compact")),
        ],
        &state.settings().ui.density,
        Message::DensitySelected,
        &tokens,
    );
    let density = state.settings().ui.density;
    let spacing = keyboard_control(spacing, "settings-density", &tokens, move |key| {
        use iced::keyboard::{Key, key::Named};
        match key {
            Key::Named(Named::ArrowLeft | Named::Home) => {
                Some(Message::DensitySelected(UiDensity::Comfortable))
            }
            Key::Named(Named::ArrowRight | Named::End) => {
                Some(Message::DensitySelected(UiDensity::Compact))
            }
            _ if activate(key) => {
                Some(Message::DensitySelected(if density == UiDensity::Compact {
                    UiDensity::Comfortable
                } else {
                    UiDensity::Compact
                }))
            }
            _ => None,
        }
    });
    let spacing = column![
        field_label(i18n.tr("settings-display-spacing"), &tokens),
        spacing
    ]
    .spacing(tokens.space(FIELD_LABEL_GAP))
    .width(Length::Fill);
    let reset = text_button(i18n.tr("settings-display-reset-text"), &tokens)
        .on_press(Message::TextSizeReset)
        .width(Length::Shrink)
        .height(Length::Fixed(control_height(&tokens)));
    let reset = keyboard_control(reset, "settings-reset-text", &tokens, |key| {
        activate(key).then_some(Message::TextSizeReset)
    });
    let reset_display = text_button(i18n.tr("settings-display-reset"), &tokens)
        .on_press(Message::DisplayReset)
        .height(Length::Fixed(control_height(&tokens)));
    let reset_display = keyboard_control(reset_display, "settings-reset-display", &tokens, |key| {
        activate(key).then_some(Message::DisplayReset)
    });
    let text_slider = slider(
        crate::bridge::MIN_TEXT_SIZE_PERCENT..=crate::bridge::MAX_TEXT_SIZE_PERCENT,
        value,
        Message::TextSizeChanged,
    )
    .step(crate::bridge::TEXT_SIZE_STEP)
    .default(crate::bridge::DEFAULT_TEXT_SIZE_PERCENT)
    .on_release(Message::TextSizeCommitted)
    .height(tokens.row_height(32.0))
    .style(move |_, status| text_slider_style(&tokens, status));
    let text_slider = keyboard_control(text_slider, "settings-text-size", &tokens, move |key| {
        use iced::keyboard::{Key, key::Named};
        let next = match key {
            Key::Named(Named::ArrowUp | Named::ArrowRight) => {
                value.saturating_add(crate::bridge::TEXT_SIZE_STEP)
            }
            Key::Named(Named::ArrowDown | Named::ArrowLeft) => {
                value.saturating_sub(crate::bridge::TEXT_SIZE_STEP)
            }
            Key::Named(Named::Home) => crate::bridge::MIN_TEXT_SIZE_PERCENT,
            Key::Named(Named::End) => crate::bridge::MAX_TEXT_SIZE_PERCENT,
            _ => return None,
        };
        Some(Message::TextSizeChanged(next.clamp(
            crate::bridge::MIN_TEXT_SIZE_PERCENT,
            crate::bridge::MAX_TEXT_SIZE_PERCENT,
        )))
    });
    let reduce_motion = state.settings().ui.reduce_motion;
    let motion_toggle = keyboard_control(
        switch_control(reduce_motion, Message::ReduceMotionToggled, &tokens),
        "settings-reduce-motion",
        &tokens,
        move |key| activate(key).then_some(Message::ReduceMotionToggled(!reduce_motion)),
    );
    let motion_toggle = row![
        text(i18n.tr("settings-display-reduce-motion"))
            .size(tokens.typography.body)
            .color(tokens.colors.text),
        motion_toggle,
    ]
    .align_y(Center)
    .spacing(tokens.space(6.0));
    let sample = container(
        column![
            text(i18n.tr("settings-display-sample-heading"))
                .size(tokens.typography.title_sm)
                .color(tokens.colors.text),
            text(i18n.tr("settings-display-sample-body"))
                .size(tokens.typography.body)
                .color(tokens.colors.text),
            text(i18n.tr("settings-display-sample-supporting"))
                .size(tokens.typography.caption)
                .color(tokens.colors.text_dim),
            reset_display,
        ]
        .spacing(tokens.spacing.gap_sm),
    )
    .padding(tokens.spacing.pad_sm)
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: Some(Color::from(tokens.colors.surface).into()),
        border: Border {
            radius: tokens.radii.md.into(),
            ..Border::default()
        },
        ..container::Style::default()
    });
    column![
        text(i18n.tr("settings-display-heading"))
            .size(tokens.typography.title_sm)
            .color(tokens.colors.text),
        spacing,
        column![
            row![
                text(i18n.tr("settings-display-text-size"))
                    .size(tokens.typography.body)
                    .color(tokens.colors.text)
                    .width(Length::Fill),
                text(format!("{value}%"))
                    .size(tokens.typography.body)
                    .color(tokens.colors.text),
            ]
            .spacing(tokens.spacing.gap_sm)
            .align_y(Center),
            text_slider,
            reset,
        ]
        .spacing(tokens.spacing.gap_sm),
        column![
            motion_toggle,
            text(i18n.tr("settings-display-reduce-motion-hint"))
                .size(tokens.typography.caption)
                .color(tokens.colors.text_dim),
        ]
        .spacing(tokens.spacing.gap_sm),
        sample,
    ]
    .spacing(tokens.space(20.0))
    .width(Length::Fill)
    .into()
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
            .width(Length::Fixed(control_height(&tokens)))
            .height(Length::Fixed(control_height(&tokens)))
            .style(move |_| swatch_style(&tokens, rgb, expanded)),
    )
    .on_press(Message::ColorPickerToggled(kind))
    .interaction(mouse::Interaction::Pointer);

    let input = text_input("#RRGGBB", state.color_text(kind))
        .on_input(move |value| Message::ColorEdited(kind, value))
        .padding([tokens.space(SELECT_PAD_Y), tokens.spacing.pad_sm])
        .size(tokens.typography.body)
        .font(theme::styles::inter_font(font::Weight::Normal))
        .style(move |_, status| input_style(&tokens, invalid, status));

    let mut field = column![
        field_label(i18n.tr(kind.label_key()), &tokens),
        row![swatch, input]
            .align_y(Center)
            .spacing(tokens.space(10.0))
            .height(Length::Fixed(control_height(&tokens))),
    ]
    .spacing(tokens.space(12.0))
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
    _compact: bool,
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
    // Keep the editor reachable independently of the scrolled color field.
    let picker_width = (modal_size.width - tokens.spacing.pad * 2.0)
        .clamp(1.0, COLOR_PICKER_POPOVER_MAX_WIDTH * tokens.text_scale());
    let picker = opaque(
        container(scrollable(color_picker_popover(state, kind, ctx)))
            .width(Length::Fixed(picker_width))
            .max_height((modal_size.height - tokens.spacing.pad * 2.0).max(1.0)),
    );
    stack![dismiss_layer, container(picker).center(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
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
                    .width(Length::Shrink)
                    .height(Length::Fixed(tokens.row_height(COLOR_PICKER_BUTTON_HEIGHT))),
                primary_button(
                    i18n.tr("settings-accessibility-color-picker-apply"),
                    &tokens,
                    false,
                )
                .on_press_maybe(changed.then_some(Message::ColorPickerApplied(kind)))
                .width(Length::Shrink)
                .height(Length::Fixed(tokens.row_height(COLOR_PICKER_BUTTON_HEIGHT))),
            ]
            .align_y(Center)
            .spacing(tokens.space(10.0))
            .height(Length::Fixed(tokens.row_height(COLOR_PICKER_BUTTON_HEIGHT))),
        ]
        .spacing(tokens.space(10.0)),
    )
    .width(Length::Fill)
    .height(Length::Shrink)
    .padding(tokens.spacing.pad_sm)
    .style(move |_| color_picker_popover_style(&tokens))
    .into()
}

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
                .width(Length::Fixed(46.0 * tokens.text_scale())),
        ]
        .align_y(Center)
        .height(Length::Shrink),
        slider(range, value, move |value| {
            Message::ColorPickerChannelChanged(kind, channel, value)
        })
        .step(step)
        .height(tokens.row_height(HSV_SLIDER_CONTROL_HEIGHT))
        .style(move |_, status| hsv_slider_style(&tokens, rail, status)),
    ]
    .spacing(tokens.space(6.0))
    .height(Length::Shrink)
    .into()
}

fn resets_tab<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    column![
        reset_button(ResetAction::Settings, state.reset_busy(), ctx),
        reset_button(ResetAction::TempFiles, state.reset_busy(), ctx),
        reset_button(ResetAction::UserData, state.reset_busy(), ctx),
    ]
    .spacing(ctx.tokens.space(24.0))
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
            .height(Length::Fixed(control_height(&tokens))),
    )
    .height(Length::Shrink)
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
        Size::new(
            RESET_CONFIRM_WIDTH * tokens.text_scale(),
            RESET_CONFIRM_HEIGHT * tokens.text_scale(),
        ),
        Size::new(
            RESET_CONFIRM_WIDTH * tokens.text_scale(),
            RESET_CONFIRM_HEIGHT * tokens.text_scale(),
        ),
    )
    .resolve(viewport_size, tokens.dims.modal_viewport_ratio);
    let dialog = container(scrollable(
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
                    .height(Length::Fixed(control_height(&tokens))),
                primary_button(i18n.tr(action.label_key()), &tokens, true)
                    .on_press(Message::ResetConfirmed)
                    .width(Length::Shrink)
                    .height(Length::Fixed(control_height(&tokens))),
            ]
            .align_y(Center)
            .spacing(tokens.space(10.0)),
        ]
        .spacing(tokens.space(14.0))
        .padding(
            Padding::ZERO
                .horizontal(tokens.space(20.0))
                .vertical(tokens.space(18.0)),
        )
        .width(Length::Fill)
        .height(Length::Shrink),
    ))
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

fn control_height(tokens: &Tokens) -> f32 {
    tokens
        .row_height(CONTROL_HEIGHT)
        .max(tokens.typography.body * 1.3 + tokens.space(SELECT_PAD_Y) * 2.0)
        .ceil()
}

fn field_label(label: String, tokens: &Tokens) -> Element<'static, Message> {
    text(label)
        .size(tokens.typography.body)
        .font(theme::styles::inter_font(font::Weight::Normal))
        .color(Color::from(tokens.colors.text))
        .width(Length::Fill)
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

/// `None` is "follow the system language".
fn language_options(i18n: &I18n) -> Vec<(Option<&'static str>, String)> {
    let mut options = Vec::with_capacity(i18n::available_languages().len() + 1);
    options.push((None, i18n.tr("settings-general-language-default")));
    options.extend(
        i18n::available_languages()
            .iter()
            .map(|language| (Some(language.id), language.name.clone())),
    );
    options
}

fn theme_options(i18n: &I18n) -> Vec<(ThemePreset, String)> {
    vec![
        (ThemePreset::Auto, i18n.tr("settings-theme-auto")),
        (ThemePreset::Dark, i18n.tr("settings-theme-dark")),
        (ThemePreset::Light, i18n.tr("settings-theme-light")),
        (
            ThemePreset::ClassicSource,
            i18n.tr("settings-theme-classic-source"),
        ),
    ]
}

fn download_count_options(i18n: &I18n) -> Vec<(DownloadCountFormat, String)> {
    vec![
        (
            DownloadCountFormat::Automatic,
            i18n.tr("settings-download-count-format-automatic"),
        ),
        (
            DownloadCountFormat::Comma,
            i18n.tr("settings-download-count-format-comma"),
        ),
        (
            DownloadCountFormat::Period,
            i18n.tr("settings-download-count-format-period"),
        ),
        (
            DownloadCountFormat::Space,
            i18n.tr("settings-download-count-format-space"),
        ),
        (
            DownloadCountFormat::Plain,
            i18n.tr("settings-download-count-format-plain"),
        ),
    ]
}

fn overwrite_options(i18n: &I18n) -> Vec<(ExtractionOverwriteMode, String)> {
    vec![
        (
            ExtractionOverwriteMode::Recycle,
            i18n.tr("settings-overwrite-recycle"),
        ),
        (
            ExtractionOverwriteMode::Delete,
            i18n.tr("settings-overwrite-delete"),
        ),
        (
            ExtractionOverwriteMode::Overwrite,
            i18n.tr("settings-overwrite-overwrite"),
        ),
    ]
}

fn settings_modal_style(tokens: &Tokens) -> container::Style {
    container::Style {
        background: Some(Color::from(tokens.colors.bg).into()),
        text_color: Some(tokens.colors.text.into()),
        border: Border {
            radius: tokens.radii.lg.into(),
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
        border::radius(tokens.radii.lg)
            .top_right(0.0)
            .bottom_right(0.0),
    )
}

fn compact_tab_rail_style(tokens: &Tokens) -> container::Style {
    rail_style_with_radius(
        tokens,
        border::radius(tokens.radii.lg)
            .bottom_left(0.0)
            .bottom_right(0.0),
    )
}

fn content_panel_style(tokens: &Tokens, compact: bool) -> container::Style {
    let radius = if compact {
        border::radius(tokens.radii.lg).top_left(0.0).top_right(0.0)
    } else {
        border::radius(tokens.radii.lg)
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
        border::radius(tokens.radii.lg).top_left(0.0).top_right(0.0)
    } else {
        border::radius(0.0).bottom_right(tokens.radii.lg)
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
            radius: tokens.radii.sm.into(),
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

fn select_metrics(tokens: &Tokens) -> select::Metrics {
    select::Metrics {
        text_size: tokens.typography.body,
        padding: Padding::from([tokens.space(SELECT_PAD_Y), tokens.spacing.pad_sm]),
        radius: tokens.radii.lg,
        max_rows: select::MAX_ROWS,
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
            radius: tokens.radii.sm.into(),
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
            radius: tokens.radii.sm.into(),
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
            radius: tokens.radii.lg.into(),
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
                radius: tokens.radii.sm.into(),
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

fn text_slider_style(tokens: &Tokens, status: slider::Status) -> slider::Style {
    let active = matches!(status, slider::Status::Hovered | slider::Status::Dragged);
    slider::Style {
        rail: slider::Rail {
            backgrounds: (
                Color::from(tokens.colors.link).into(),
                Color::from(tokens.colors.border_subtle).into(),
            ),
            width: tokens.space(4.0),
            border: border::rounded(2.0),
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle {
                radius: tokens.icon_size(if active { 7.0 } else { 6.0 }),
            },
            background: Color::from(tokens.colors.link).into(),
            border_width: tokens.dims.border_width,
            border_color: tokens.colors.surface.into(),
        },
    }
}

pub(super) fn text_button_style(tokens: &Tokens, status: button::Status) -> button::Style {
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
            radius: tokens.radii.sm.into(),
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
            radius: tokens.radii.sm.into(),
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
            radius: tokens.radii.lg.into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_focus_bounds_exclude_field_labels() {
        let state = State::default();
        let i18n = I18n::for_locale(Some("en"));
        for (density, percent) in [(UiDensity::Comfortable, 100), (UiDensity::Compact, 150)] {
            let tokens = Tokens::dark().with_display_preferences(density, percent);
            let mut simulation = iced_test::Simulator::with_size(
                iced::Settings::default(),
                Size::new(700.0, 800.0),
                display_preferences(&state, ViewCtx::new(&tokens, &i18n)),
            );
            let dropdown = simulation
                .find(iced_test::selector::id("settings-density"))
                .unwrap()
                .bounds();
            assert_eq!(dropdown.height, control_height(&tokens));
            assert!(dropdown.y > tokens.typography.body * 1.3);
            let toggle = simulation
                .find(iced_test::selector::id("settings-reduce-motion"))
                .unwrap()
                .bounds();
            assert!(toggle.width <= tokens.dims.switch_height * 3.0);
        }
    }

    #[test]
    #[ignore = "opt-in clicked dropdown screenshot"]
    fn capture_clicked_dropdown_focus() {
        let state = State::default();
        let i18n = I18n::for_locale(Some("en"));
        let tokens = Tokens::dark().with_display_preferences(UiDensity::Comfortable, 100);
        let settings = iced::Settings {
            default_font: assets::fonts::default_font(),
            fonts: assets::fonts::bundled_fonts()
                .into_iter()
                .map(std::borrow::Cow::Borrowed)
                .collect(),
            ..Default::default()
        };
        let mut simulation = iced_test::Simulator::with_size(
            settings,
            Size::new(700.0, 800.0),
            display_preferences(&state, ViewCtx::new(&tokens, &i18n)),
        );
        simulation
            .click(iced_test::selector::id("settings-density"))
            .unwrap();
        let _ = simulation.simulate([iced::Event::Window(iced::window::Event::RedrawRequested(
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        ))]);
        let directory = std::env::var_os("GMPUBLISHED_UI_CAPTURE_DIR").map_or_else(
            || std::path::PathBuf::from("/tmp/gmpublished-ui-refactor"),
            std::path::PathBuf::from,
        );
        assert!(
            simulation
                .snapshot(&iced::Theme::Dark)
                .unwrap()
                .matches_image(directory.join("dropdown-clicked-focus"))
                .unwrap()
        );
        let control = simulation
            .find(iced_test::selector::id("settings-density"))
            .unwrap()
            .bounds();
        let metrics = select_metrics(&tokens);
        simulation.point_at(iced::Point::new(
            control.center_x(),
            control.y + control.height - tokens.dims.focus_border_width
                + metrics.radius
                + select::row_height(metrics) * 1.5,
        ));
        let _ = simulation.simulate([
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ]);
        assert!(
            simulation
                .snapshot(&iced::Theme::Dark)
                .unwrap()
                .matches_image(directory.join("dropdown-after-selection-focus"))
                .unwrap()
        );
    }

    #[test]
    fn settings_control_faces_accommodate_their_resolved_text_and_padding() {
        for density in [UiDensity::Comfortable, UiDensity::Compact] {
            for percent in [90, 100, 125, 150] {
                let tokens = Tokens::dark().with_display_preferences(density, percent);
                let menu_row = select::row_height(select_metrics(&tokens));
                assert!(control_height(&tokens) >= menu_row);
            }
        }
    }

    #[test]
    fn the_language_list_leads_with_the_follow_the_system_row() {
        let i18n = I18n::for_locale(Some("en"));

        let options = language_options(&i18n);

        let (first_value, first_label) = options.first().expect("language options are non-empty");
        assert_eq!(*first_value, None);
        assert_eq!(
            *first_label,
            i18n.tr("settings-general-language-default"),
            "the row must be translated, not the raw key"
        );
        assert!(
            options.iter().skip(1).all(|(value, _)| value.is_some()),
            "only the leading row clears the setting"
        );
    }
}

#[cfg(test)]
#[test]
#[ignore = "opt-in real UI screenshots"]
fn capture_settings_display_fixtures() {
    let root = tempfile::tempdir().unwrap();
    let paths = crate::bridge::AppPaths {
        settings_file: root.path().join("settings.json"),
        default_user_data_dir: root.path().join("user-data"),
        default_temp_dir: root.path().join("temp"),
        default_downloads_dir: Some(root.path().join("downloads")),
        temp_dir: root.path().join("temp"),
        user_data_dir: root.path().join("user-data"),
        downloads_dir: Some(root.path().join("downloads")),
        gmod_dir: None,
    };
    let i18n = I18n::for_locale(Some("en"));
    for (density, percent, size, name) in crate::test_support::capture_display_cases() {
        let tokens = Tokens::dark().with_display_preferences(density, percent);
        let mut preferences = crate::bridge::Settings::default();
        preferences.ui.density = density;
        preferences.ui.text_size_percent = percent;
        let mut state = State::default();
        state.open_with_snapshot(super::SettingsSnapshot::new(
            preferences,
            paths.clone(),
            crate::bridge::SystemColorScheme::Dark,
        ));
        for (tab, label) in [
            (Tab::Accessibility, "display"),
            (Tab::General, "general"),
            (Tab::Paths, "paths"),
        ] {
            state.select_tab(tab);
            crate::test_support::capture_ui_surface(
                &format!("settings-{label}-{name}"),
                size,
                view(&state, ViewCtx::new(&tokens, &i18n), size),
            );
        }
    }
}
