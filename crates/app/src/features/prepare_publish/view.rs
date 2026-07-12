use std::time::Instant;

use iced::widget::{
    Space, button, checkbox, column, container, image, mouse_area, opaque, pick_list, row,
    scrollable, stack, svg, text, text_editor, text_input,
};
use iced::{Alignment, Center, Color, ContentFit, Element, Length, Size};

use crate::{
    assets,
    features::file_preview,
    features::modal_stack::{self, ResponsiveSize},
    format::format_bytes,
    i18n::translated_error,
    theme::{self, Tokens, ViewCtx},
    widgets::file_browser::{self, Row as FileBrowserRowData, RowKind as FileBrowserEntryKind},
    widgets::tooltip as tooltip_widget,
};

use super::{Message, State};

const TOOLTIP_MAX_WIDTH: f32 = 320.0;

pub fn view<'a>(
    state: &'a State,
    file_preview_state: &'a file_preview::State,
    ctx: ViewCtx<'a>,
    viewport_size: Size,
    chrome_clearance: f32,
    now: Instant,
) -> Element<'a, Message> {
    if !state.open() {
        return container(Space::new()).center(Length::Fill).into();
    }

    let tokens = *ctx.tokens;
    let expanded = file_preview::embedded_expanded(file_preview_state);

    let modal_size = if expanded {
        modal_stack::expanded_size(
            Size::new(
                tokens.dims.publish_modal_width,
                tokens.publish_modal_height(state.update_mode()),
            ),
            viewport_size,
            tokens.spacing.pad,
            chrome_clearance,
        )
    } else {
        ResponsiveSize::new(
            Size::new(
                tokens.dims.publish_modal_width,
                tokens.publish_modal_height(state.update_mode()),
            ),
            Size::new(
                tokens.dims.publish_modal_max_width,
                tokens.dims.publish_modal_max_height,
            ),
        )
        .resolve(viewport_size, tokens.dims.modal_viewport_ratio)
    };

    let preview = embedded_preview_body(state, file_preview_state, ctx, expanded);
    let preview_open = preview.is_some();
    let body: Element<'a, Message> = preview.unwrap_or_else(|| {
        row![
            left_column(state, ctx).width(Length::Fixed(tokens.dims.publish_left_column_width)),
            middle_column(state, ctx, now).width(Length::Fill),
            right_column(state, ctx).width(Length::Fixed(tokens.dims.publish_right_column_width)),
        ]
        .spacing(tokens.spacing.gap_lg)
        .height(Length::Fill)
        .into()
    });

    let panel = container(body)
        .padding(if preview_open {
            0.0
        } else {
            tokens.spacing.pad
        })
        .width(Length::Fixed(modal_size.width))
        .height(Length::Fixed(modal_size.height))
        .clip(true)
        .style(move |_| theme::styles::modal(&tokens));

    container(opaque(panel)).center(Length::Fill).into()
}

fn embedded_preview_body<'a>(
    state: &'a State,
    file_preview_state: &'a file_preview::State,
    ctx: ViewCtx<'a>,
    expanded: bool,
) -> Option<Element<'a, Message>> {
    #[cfg(feature = "asset-studio")]
    {
        let tokens = *ctx.tokens;
        if !file_preview_state.is_open() {
            return None;
        }

        let pane = file_preview::pane(file_preview_state, ctx, !expanded).map(Message::FilePreview);
        if expanded {
            Some(pane)
        } else {
            Some(
                row![
                    container(
                        left_column(state, ctx)
                            .width(Length::Fixed(tokens.dims.publish_left_column_width)),
                    )
                    .padding(tokens.spacing.pad),
                    pane,
                ]
                .spacing(0.0)
                .height(Length::Fill)
                .into(),
            )
        }
    }
    #[cfg(not(feature = "asset-studio"))]
    {
        let _ = (state, file_preview_state, ctx, expanded);
        None
    }
}

fn left_column<'a>(state: &'a State, ctx: ViewCtx<'a>) -> iced::widget::Column<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mut content = column![].spacing(tokens.spacing.gap);

    if state.update_mode() {
        content = content.push(workshop_link(ctx));
    }

    content
        .push(icon_preview(state, ctx))
        .push(icon_controls(state, ctx))
        .push(
            text(i18n.tr("prepare-publish-icon-instructions"))
                .size(tokens.typography.body)
                .width(Length::Fill)
                .align_x(Center),
        )
        .push(upscale_row(state, ctx))
        .push(addon_path_row(state, ctx))
        .push(title_input(state, ctx))
        .push(addon_type_select(state, ctx))
        .push(tag_selects(state, ctx))
        .push(submit_button(state, ctx))
}

fn workshop_link(ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let link = row![
        text(i18n.tr("prepare-publish-workshop-page"))
            .size(tokens.typography.body)
            .color(Color::from(tokens.colors.link)),
        icon(
            assets::icons::context_link_out(),
            tokens.colors.link.into(),
            tokens.dims.icon_size_sm,
        ),
    ]
    .align_y(Center)
    .spacing(tokens.spacing.gap_xs);

    let link = mouse_area(link)
        .on_press(Message::WorkshopLinkRequested)
        .interaction(iced::mouse::Interaction::Pointer);

    container(link).center_x(Length::Fill).into()
}

fn icon_preview<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let well = tokens.colors.surface_sunken;
    let backdrop = state
        .icon_backdrop_handle()
        .cloned()
        .unwrap_or_else(|| assets::images::default_icon_backdrop([well.r, well.g, well.b]));
    let foreground = state
        .icon_handle()
        .cloned()
        .unwrap_or_else(assets::images::default_icon);

    let layers = stack![
        image(backdrop)
            .content_fit(ContentFit::Cover)
            .width(Length::Fill)
            .height(Length::Fill),
        image(foreground)
            .content_fit(ContentFit::Contain)
            .width(Length::Fill)
            .height(Length::Fill),
    ];

    let preview = mouse_area(
        container(layers)
            .width(Length::Fill)
            .height(Length::Fixed(tokens.dims.publish_icon_preview_height))
            .clip(true)
            .style(move |_| theme::styles::icon_preview_well(&tokens)),
    )
    .on_press(Message::IconBrowseRequested)
    .interaction(iced::mouse::Interaction::Pointer);

    match state.icon_error() {
        Some(error) => tooltip_widget::below(
            preview,
            translated_error(i18n, error),
            &tokens,
            TOOLTIP_MAX_WIDTH,
        ),
        None => preview.into(),
    }
}

fn icon_controls<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let (label, glyph, message) = if !state.update_mode() && state.icon_selected() {
        (
            i18n.tr("prepare-publish-remove-icon"),
            assets::icons::cross(),
            Message::IconRemoveRequested,
        )
    } else {
        (
            i18n.tr("destination-browse"),
            assets::icons::folder(),
            Message::IconBrowseRequested,
        )
    };

    let primary = button(
        container(
            row![
                icon(glyph, tokens.colors.text.into(), tokens.dims.icon_size),
                text(label).size(tokens.typography.body),
            ]
            .align_y(Center)
            .spacing(tokens.spacing.gap_sm),
        )
        .center_x(Length::Fill),
    )
    .on_press(message)
    .padding(tokens.spacing.pad_control)
    .width(Length::Fill)
    .style(move |_, status| theme::styles::button(&tokens, status));

    let mut controls = row![primary].spacing(tokens.spacing.gap_sm);

    if state.update_mode() && !state.submit_pending() {
        let glyph_color = if state.can_publish_icon() {
            tokens.colors.text_on_neutral.into()
        } else {
            tokens.colors.text.into()
        };
        let upload = button(
            container(icon(
                assets::icons::cloud_upload(),
                glyph_color,
                tokens.dims.icon_size,
            ))
            .center(Length::Fill),
        )
        .on_press_maybe(
            state
                .can_publish_icon()
                .then_some(Message::PublishIconRequested),
        )
        .padding(0)
        .width(Length::Fixed(tokens.dims.icon_button_size))
        .height(Length::Fixed(tokens.dims.icon_button_size))
        .style(move |_, status| theme::styles::action_button(&tokens, status));

        controls = controls.push(tooltip_widget::below(
            upload,
            i18n.tr("prepare-publish-publish-icon"),
            &tokens,
            TOOLTIP_MAX_WIDTH,
        ));
    }

    controls.into()
}

fn upscale_row<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mut upscale = checkbox(state.upscale_icon())
        .label(i18n.tr("prepare-publish-upscale-icon"))
        .text_size(tokens.typography.body)
        .style(move |_, status| theme::styles::checkbox(&tokens, status));
    if state.can_upscale_icon() {
        upscale = upscale.on_toggle(Message::IconUpscaleToggled);
    }

    container(upscale).center_x(Length::Fill).into()
}

fn addon_path_row<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let has_error = state.path_error().is_some();
    let field = text_input(
        &i18n.tr("prepare-publish-addon-path-placeholder"),
        state.addon_path(),
    )
    .on_input(Message::AddonPathEdited)
    .on_submit(Message::AddonPathAccepted)
    .padding(tokens.spacing.pad_control)
    .size(tokens.typography.caption)
    .width(Length::Fill)
    .style(move |_, status| {
        if has_error {
            theme::styles::input_error(&tokens, status)
        } else {
            theme::styles::input(&tokens, status)
        }
    });

    let field: Element<'a, Message> = match state.path_error() {
        Some(error) => tooltip_widget::below(
            field,
            translated_error(i18n, error),
            &tokens,
            TOOLTIP_MAX_WIDTH,
        ),
        None => field.into(),
    };

    let browse = button(
        container(icon(
            assets::icons::folder(),
            tokens.colors.text.into(),
            tokens.dims.icon_size,
        ))
        .center(Length::Fill),
    )
    .on_press(Message::AddonPathBrowseRequested)
    .padding(0)
    .width(Length::Fixed(tokens.dims.icon_button_size))
    .height(Length::Fixed(tokens.dims.icon_button_size))
    .style(move |_, status| theme::styles::button(&tokens, status));

    row![field, browse]
        .align_y(Center)
        .spacing(tokens.spacing.gap_md)
        .into()
}

fn title_input<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mut field = text_input(&i18n.tr("prepare-publish-title-placeholder"), state.title())
        .padding(tokens.spacing.pad_control)
        .size(tokens.typography.caption)
        .style(move |_, status| theme::styles::input(&tokens, status));
    if !state.update_mode() {
        field = field.on_input(Message::TitleEdited);
    }

    if state.update_mode() {
        tooltip_widget::below(
            field,
            i18n.tr("prepare-publish-title-locked"),
            &tokens,
            TOOLTIP_MAX_WIDTH,
        )
    } else {
        field.into()
    }
}

fn addon_type_select<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    pick_list(
        state.addon_type_options(i18n),
        Some(state.selected_addon_type_option(i18n)),
        Message::AddonTypeSelected,
    )
    .handle(pick_list::Handle::None)
    .width(Length::Fill)
    .padding(tokens.spacing.pad_control)
    .text_size(tokens.typography.caption)
    .style(move |_, status| theme::styles::pick_list(&tokens, status))
    .menu_style(move |_| theme::styles::pick_list_menu(&tokens))
    .into()
}

fn tag_selects<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mut tag_row = row![].spacing(tokens.spacing.gap);
    for index in 0..3 {
        tag_row = tag_row.push(
            pick_list(
                state.tag_options(index, i18n),
                Some(state.selected_tag_option(index, i18n)),
                move |option| Message::TagSelected(index, option),
            )
            .handle(pick_list::Handle::None)
            .width(Length::Fill)
            .padding(tokens.spacing.pad_control)
            .text_size(tokens.typography.caption)
            .style(move |_, status| theme::styles::pick_list(&tokens, status))
            .menu_style(move |_| theme::styles::pick_list_menu(&tokens)),
        );
    }
    tag_row.into()
}

fn submit_button<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let glyph_color = if state.can_submit() {
        tokens.colors.text_on_neutral.into()
    } else {
        tokens.colors.text.into()
    };
    let content: Element<'a, Message> = if state.submit_pending() {
        crate::widgets::spinner::spinner(&tokens, state.spinner_elapsed(), tokens.dims.icon_size_md)
    } else {
        let label = if state.update_mode() {
            i18n.tr("prepare-publish-update-exclamation")
        } else {
            i18n.tr("prepare-publish-publish-exclamation")
        };
        row![
            icon(
                assets::icons::cloud_upload(),
                glyph_color,
                tokens.dims.icon_size,
            ),
            text(label).size(tokens.typography.body),
        ]
        .align_y(Center)
        .spacing(tokens.spacing.gap_xs)
        .into()
    };

    let submit = button(container(content).center_x(Length::Fill))
        .on_press_maybe(state.can_submit().then_some(Message::SubmitRequested))
        .padding(tokens.spacing.pad_control)
        .width(Length::Fill)
        .style(move |_, status| theme::styles::action_button(&tokens, status));

    match state.update_warning(i18n) {
        Some(warning) => tooltip_widget::below(submit, warning, &tokens, TOOLTIP_MAX_WIDTH),
        None => submit.into(),
    }
}

fn middle_column<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    now: Instant,
) -> iced::widget::Column<'a, Message> {
    let tokens = *ctx.tokens;
    let mut content = column![file_browser(state, ctx, now)]
        .spacing(tokens.spacing.gap_lg)
        .height(Length::Fill);
    if state.update_mode() {
        content = content.push(changelog_editor(state, ctx));
    }
    content
}

fn file_browser<'a>(state: &'a State, ctx: ViewCtx<'a>, now: Instant) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let browser = state.browser_snapshot();

    let header: Element<'a, Message> = if browser.visible() {
        row![
            up_button(browser.can_go_up(), &tokens),
            text(browser.header_path().to_owned())
                .size(tokens.typography.caption_xs)
                .width(Length::Fill),
        ]
        .align_y(Center)
        .spacing(tokens.spacing.gap_sm)
        .into()
    } else {
        text(i18n.tr("prepare-publish-file-browser"))
            .size(tokens.typography.caption_xs)
            .width(Length::Fill)
            .align_x(Center)
            .into()
    };
    let header = container(header)
        .padding([tokens.spacing.pad_xs, tokens.spacing.pad_sm])
        .width(Length::Fill)
        .style(move |_| theme::styles::browser_bar(&tokens));

    let body: Element<'a, Message> = if state.path_pending() {
        container(
            text(i18n.tr("prepare-publish-verifying-content"))
                .size(tokens.typography.caption)
                .color(Color::from(tokens.colors.text_dim)),
        )
        .center(Length::Fill)
        .into()
    } else if state.path_error().is_some() || (browser.visible() && browser.rows().is_empty()) {
        browser_empty_state(
            assets::icons::dead(),
            i18n.tr("prepare-publish-no-files"),
            &tokens,
            0.0,
        )
    } else if browser.visible() {
        browser_rows(&browser, ctx)
    } else {
        mouse_area(browser_empty_state(
            assets::icons::folder_add(),
            i18n.tr("prepare-publish-browser-select"),
            &tokens,
            state.browser_select_hover_progress(now),
        ))
        .on_press(Message::AddonPathBrowseRequested)
        .on_enter(Message::BrowserSelectHoverChanged(true))
        .on_exit(Message::BrowserSelectHoverChanged(false))
        .interaction(iced::mouse::Interaction::Pointer)
        .into()
    };

    let footer = container(browser_footer(&browser, ctx))
        .padding([tokens.spacing.pad_xs, tokens.spacing.pad_sm])
        .width(Length::Fill)
        .style(move |_| theme::styles::browser_bar(&tokens));

    container(column![
        header,
        container(body).width(Length::Fill).height(Length::Fill),
        footer,
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .clip(true)
    .style(move |_| theme::styles::sunken_card(&tokens))
    .into()
}

fn browser_empty_state<'a>(
    glyph: svg::Handle,
    label: String,
    tokens: &Tokens,
    brightness: f32,
) -> Element<'a, Message> {
    // Rest tone is the pre-composited 25%-over-well dim; hover fades to the
    // full text color.
    let tone = theme::motion::mix_color(
        tokens.colors.browser_empty_dim.into(),
        tokens.colors.text.into(),
        brightness,
    );
    container(
        column![
            icon(glyph, tone, tokens.dims.browser_empty_icon_size),
            text(label).size(tokens.typography.title).color(tone),
        ]
        .align_x(Alignment::Center)
        .spacing(tokens.spacing.gap),
    )
    .center(Length::Fill)
    .into()
}

fn browser_rows<'a>(
    browser: &super::state::BrowserSnapshot,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let mut rows = column![].width(Length::Fill);
    for row_data in browser.rows() {
        rows = rows.push(file_row(row_data, ctx));
    }

    scrollable(rows)
        .width(Length::Fill)
        .height(Length::Fill)
        .direction(scrollable::Direction::Vertical(
            theme::styles::vertical_scrollbar(&tokens),
        ))
        .style(move |_, status| theme::styles::scrollbar(&tokens, status))
        .into()
}

fn file_row<'a>(row_data: &FileBrowserRowData, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let message = match row_data.kind {
        FileBrowserEntryKind::Directory => {
            Some(Message::DirectoryOpened(row_data.current_path.clone()))
        }
        FileBrowserEntryKind::File => file_activation_message(row_data.archive_path.clone()),
    };
    file_browser::row_view(row_data.clone(), message, ctx)
}

#[cfg(feature = "asset-studio")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature is shared with the non-asset-studio variant, which returns None"
)]
fn file_activation_message(path: String) -> Option<Message> {
    Some(Message::PreviewEntryRequested(path))
}

#[cfg(not(feature = "asset-studio"))]
fn file_activation_message(_path: String) -> Option<Message> {
    None
}

fn browser_footer<'a>(
    browser: &super::state::BrowserSnapshot,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let total = browser.total_files();
    let items = if total == 1 {
        i18n.tr("prepare-publish-items-one")
    } else {
        let total = total.to_string();
        i18n.trn("prepare-publish-items-num", &[("arg0", total.as_str())])
    };
    let shown = browser.shown_count().to_string();
    let shown = i18n.trn("prepare-publish-items-shown", &[("arg0", shown.as_str())]);
    let size = format_bytes(browser.total_size_bytes(), i18n);

    text(format!("{items}  \u{2223}  {shown}  \u{2223}  {size}"))
        .size(tokens.typography.caption_xs)
        .width(Length::Fill)
        .align_x(Center)
        .into()
}

fn up_button<'a>(enabled: bool, tokens: &Tokens) -> Element<'a, Message> {
    let tokens = *tokens;
    let chevron = button(icon(
        assets::icons::chevron_up(),
        tokens.colors.text.into(),
        tokens.dims.icon_size,
    ))
    .on_press_maybe(enabled.then_some(Message::UpRequested))
    .padding(tokens.spacing.pad_xs)
    .style(move |_, status| theme::styles::ghost_button(&tokens, status));

    chevron.into()
}

fn changelog_editor<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let editor = text_editor(state.changelog_content())
        .on_action(Message::ChangelogActionPerformed)
        .height(Length::Fixed(tokens.dims.publish_changelog_height))
        .padding(tokens.spacing.pad_control)
        .size(tokens.typography.caption)
        .style(move |_, status| theme::styles::text_editor(&tokens, status));

    let mut layers = stack![editor];
    if state.changelog_is_empty() {
        layers = layers.push(
            container(
                text(i18n.tr("prepare-publish-changelog"))
                    .size(tokens.typography.display_xs)
                    .color(Color::from(tokens.colors.text_watermark)),
            )
            .center(Length::Fill),
        );
    }

    container(layers)
        .width(Length::Fill)
        .height(Length::Fixed(tokens.dims.publish_changelog_height))
        .into()
}

fn right_column<'a>(state: &'a State, ctx: ViewCtx<'a>) -> iced::widget::Column<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mut rows = column![].width(Length::Fill);
    for (index, pattern) in state.ignored_patterns().iter().enumerate() {
        rows = rows.push(ignored_pattern_row(
            pattern.pattern.clone(),
            pattern.default_pattern,
            index % 2 == 0,
            ctx,
        ));
    }

    let input = text_input(
        &i18n.tr("prepare-publish-ignore-placeholder"),
        state.ignore_pattern_input(),
    )
    .on_input(Message::IgnorePatternEdited)
    .on_submit(Message::IgnorePatternAccepted)
    .padding(tokens.spacing.pad_control)
    .size(tokens.typography.caption)
    .style(move |_, status| theme::styles::input(&tokens, status));

    let patterns = scrollable(rows)
        .width(Length::Fill)
        .height(Length::Fill)
        .direction(scrollable::Direction::Vertical(
            theme::styles::hidden_vertical_scrollbar(),
        ))
        .style(move |_, status| theme::styles::scrollbar(&tokens, status));

    column![
        text(i18n.tr("prepare-publish-ignored-patterns"))
            .size(tokens.typography.body)
            .width(Length::Fill)
            .align_x(Center),
        input,
        container(patterns)
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true)
            .style(move |_| theme::styles::sunken_card(&tokens)),
    ]
    .spacing(tokens.spacing.gap)
    .height(Length::Fill)
}

fn ignored_pattern_row(
    pattern: String,
    default_pattern: bool,
    shaded: bool,
    ctx: ViewCtx<'_>,
) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let color = if default_pattern {
        Color::from(tokens.colors.text_watermark)
    } else {
        tokens.colors.text.into()
    };
    let cell = container(
        text(pattern.clone())
            .size(tokens.typography.body_sm)
            .color(color),
    )
    .padding([tokens.spacing.gap_sm, tokens.spacing.pad_sm])
    .width(Length::Fill)
    .style(move |_| theme::styles::striped_row(&tokens, shaded));

    if default_pattern {
        tooltip_widget::below(
            cell,
            i18n.tr("prepare-publish-ignored-for-convenience"),
            &tokens,
            TOOLTIP_MAX_WIDTH,
        )
    } else {
        mouse_area(cell)
            .on_press(Message::IgnorePatternRemoveRequested(pattern))
            .interaction(iced::mouse::Interaction::Pointer)
            .into()
    }
}

fn icon<'a>(handle: svg::Handle, color: Color, size: f32) -> Element<'a, Message> {
    svg(handle)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_, _| svg::Style { color: Some(color) })
        .into()
}
