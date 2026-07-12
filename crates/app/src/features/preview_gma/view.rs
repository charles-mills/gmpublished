use iced::widget::{
    Space, button, column, container, image, mouse_area, opaque, row, scrollable, svg, text,
};
use iced::{Center, Color, Element, Length, Size};

use crate::{
    assets,
    features::{file_preview, modal_stack, modal_stack::ResponsiveSize},
    format::format_bytes,
    i18n::I18n,
    theme::{self, Tokens, ViewCtx},
    widgets::{
        download_count_icon::download_count_icon,
        file_browser::{self, Row as FileBrowserRowData, RowKind as FileBrowserEntryKind},
        spinner::spinner,
        star_rating::star_rating,
        tag_chip::tag_chip,
        tooltip as tooltip_widget,
    },
};

use super::details::{AuthorDisplay, Details, MetadataRow, MetadataValue, RelativeTime};
use super::update::nav_path_scrollable_id;
use super::{Message, State};

const TOOLTIP_MAX_WIDTH: f32 = 280.0;
const AVATAR_SIZE: f32 = 24.0;
const DEAD_GLYPH_SIZE: f32 = 32.0;
const SPINNER_SIZE: f32 = 32.0;
const DEAD_ICON_SIZE: f32 = 16.0;
const INFO_LABEL_WIDTH: f32 = 64.0;

pub fn view<'a>(
    state: &'a State,
    file_preview_state: &'a file_preview::State,
    ctx: ViewCtx<'a>,
    viewport_size: Size,
    chrome_clearance: f32,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let expanded = file_preview::embedded_expanded(file_preview_state);
    let modal_size = if expanded {
        modal_stack::expanded_size(
            Size::new(
                tokens.dims.preview_modal_width,
                tokens.dims.preview_modal_height,
            ),
            viewport_size,
            tokens.spacing.pad,
            chrome_clearance,
        )
    } else {
        ResponsiveSize::new(
            Size::new(
                tokens.dims.preview_modal_width,
                tokens.dims.preview_modal_height,
            ),
            Size::new(
                tokens.dims.preview_modal_max_width,
                tokens.dims.preview_modal_max_height,
            ),
        )
        .resolve(viewport_size, tokens.dims.modal_viewport_ratio)
    };

    let body: Element<'a, Message> = if state.loading() {
        container(spinner(&tokens, state.spinner_elapsed(), SPINNER_SIZE))
            .center(Length::Fill)
            .into()
    } else if state.error().is_some() {
        container(dead_icon(tokens.colors.text.into(), DEAD_GLYPH_SIZE))
            .center(Length::Fill)
            .into()
    } else if let Some(preview) = embedded_preview_body(state, file_preview_state, ctx, expanded) {
        preview
    } else {
        archive_body(state, ctx)
    };

    let panel = opaque(
        container(body)
            .width(Length::Fixed(modal_size.width))
            .height(Length::Fixed(modal_size.height))
            .clip(true)
            .style(move |_| theme::styles::preview_modal(&tokens)),
    );

    container(panel).center(Length::Fill).into()
}

fn embedded_preview_body<'a>(
    state: &'a State,
    file_preview_state: &'a file_preview::State,
    ctx: ViewCtx<'a>,
    expanded: bool,
) -> Option<Element<'a, Message>> {
    #[cfg(feature = "asset-studio")]
    {
        if !file_preview_state.is_open() {
            return None;
        }

        let pane = file_preview::pane(file_preview_state, ctx, !expanded).map(Message::FilePreview);
        if expanded {
            Some(pane)
        } else {
            Some(
                row![sidebar(state, ctx), pane]
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

fn archive_body<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    row![sidebar(state, ctx), browser(state, ctx)]
        .spacing(0.0)
        .height(Length::Fill)
        .into()
}

fn sidebar<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let extract = button(
        container(
            text(i18n.tr("preview-gma-extract"))
                .size(tokens.typography.body)
                .line_height(1.0),
        )
        .center(Length::Fill),
    )
    .on_press_maybe(
        state
            .can_extract()
            .then_some(Message::ExtractArchiveRequested),
    )
    .padding([0.0, tokens.spacing.pad_control])
    .width(Length::Fill)
    .height(Length::Fixed(tokens.dims.control_height))
    .style(move |_, status| theme::styles::preview_extract_button(&tokens, status));

    let card = scrollable(sidebar_card(state, ctx))
        .width(Length::Fill)
        .height(Length::Fill)
        .direction(scrollable::Direction::Vertical(
            theme::styles::hidden_vertical_scrollbar(),
        ))
        .style(move |_, status| theme::styles::scrollbar(&tokens, status));

    container(column![extract, card].height(Length::Fill))
        .width(Length::Fixed(tokens.dims.preview_sidebar_width))
        .height(Length::Fill)
        .style(move |_| theme::styles::preview_sidebar(&tokens))
        .into()
}

fn sidebar_card<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let details = state.details();
    let mut content = column![].padding(tokens.spacing.pad).spacing(0.0);

    // Render the stats row unconditionally (zeroes while pending) so nothing
    // below shifts when Workshop data hydrates.
    content = content.push(stats_row(details, &tokens));
    content = content.push(Space::new().height(tokens.spacing.gap_md));

    content = content.push(preview_image(state, &tokens));

    if !details.title.trim().is_empty() {
        content = content.push(Space::new().height(tokens.spacing.gap_md));
        content = content.push(
            text(details.title.as_str())
                .size(tokens.typography.body)
                .width(Length::Fill)
                .align_x(Center),
        );
    }

    if !details.tag_rows.is_empty() {
        content = content.push(Space::new().height(tokens.spacing.gap));
        content = content.push(tag_chips(details, &tokens));
    }

    if details.author.is_some() || !details.metadata_rows.is_empty() {
        content = content.push(Space::new().height(tokens.spacing.gap_sm));
        content = content.push(info_table(state, details, ctx));
    }

    if state.can_open_workshop_link() {
        content = content.push(Space::new().height(tokens.spacing.gap));
        content = content.push(workshop_link(ctx));
        content = content.push(Space::new().height(tokens.spacing.gap));
    }

    if !details.description.trim().is_empty() {
        content = content.push(Space::new().height(tokens.spacing.gap_sm));
        content = content.push(
            text(details.description.as_str())
                .size(tokens.typography.body_sm)
                .color(Color::from(tokens.colors.text_dim))
                .width(Length::Fill),
        );
    }

    content.into()
}

fn stats_row<'a>(details: &'a Details, tokens: &Tokens) -> Element<'a, Message> {
    let subscriptions = if details.subscriptions.is_empty() {
        "0"
    } else {
        details.subscriptions.as_str()
    };
    let stars = star_rating(details.score_bucket, tokens, 1.0);
    let stars: Element<'a, Message> = if details.score_label.is_empty() {
        stars
    } else {
        tooltip_widget::below(
            stars,
            details.score_label.clone(),
            tokens,
            TOOLTIP_MAX_WIDTH,
        )
    };

    row![
        row![
            download_count_icon(tokens, 16.0, 1.0),
            text(subscriptions).size(tokens.typography.body),
        ]
        .align_y(Center)
        .spacing(tokens.spacing.gap_xs)
        .width(Length::Fill),
        stars,
    ]
    .align_y(Center)
    .into()
}

fn preview_image<'a>(state: &'a State, tokens: &Tokens) -> Element<'a, Message> {
    // Natural aspect ratio: the image sizes itself to the sidebar width.
    let inner_width = tokens.dims.preview_sidebar_width - tokens.spacing.pad * 2.0;
    let content: Element<'a, Message> = state.thumbnail_handle().map_or_else(
        || {
            if state.thumbnail_loading() {
                container(spinner(tokens, state.spinner_elapsed(), SPINNER_SIZE))
                    .width(Length::Fill)
                    .height(Length::Fixed(inner_width))
                    .center(Length::Fill)
                    .into()
            } else {
                container(dead_icon(
                    tokens.colors.surface_muted.into(),
                    DEAD_GLYPH_SIZE * 2.0,
                ))
                .width(Length::Fill)
                .height(Length::Fixed(inner_width))
                .center(Length::Fill)
                .into()
            }
        },
        |handle| image(handle.clone()).width(Length::Fill).into(),
    );

    let tokens = *tokens;
    container(content)
        .width(Length::Fill)
        .style(move |_| theme::styles::preview_image_well(&tokens))
        .into()
}

fn tag_chips<'a>(details: &'a Details, tokens: &Tokens) -> Element<'a, Message> {
    let mut chips = row![].spacing(tokens.spacing.gap_xs);
    for tag in &details.tag_rows {
        chips = chips.push(tag_chip(&tag.label, tokens));
    }
    chips.wrap().into()
}

fn info_table<'a>(
    state: &'a State,
    details: &'a Details,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let (size_rows, other_rows): (Vec<_>, Vec<_>) = details
        .metadata_rows
        .iter()
        .partition(|row_data| row_data.label_key == "preview-gma-size");

    let mut rows = column![].spacing(tokens.spacing.gap_sm);
    for row_data in size_rows {
        rows = rows.push(info_row(
            i18n.tr(row_data.label_key),
            text(metadata_value(&row_data.value, i18n))
                .size(tokens.typography.body_sm)
                .into(),
            &tokens,
        ));
    }

    if let Some(author) = &details.author {
        rows = rows.push(info_row(
            i18n.tr("preview-gma-author"),
            author_value(state, author, &tokens),
            &tokens,
        ));
    }

    for row_data in other_rows {
        rows = rows.push(info_row(
            i18n.tr(row_data.label_key),
            timestamp_value(row_data, ctx),
            &tokens,
        ));
    }

    rows.into()
}

fn info_row<'a>(
    label: String,
    value: Element<'a, Message>,
    tokens: &Tokens,
) -> Element<'a, Message> {
    row![
        text(label)
            .size(tokens.typography.body_sm)
            .font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..iced::Font::default()
            })
            .width(Length::Fixed(INFO_LABEL_WIDTH)),
        container(value).width(Length::Fill),
    ]
    .align_y(Center)
    .spacing(tokens.spacing.gap_sm)
    .into()
}

fn author_value<'a>(
    state: &'a State,
    author: &'a AuthorDisplay,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let avatar = author
        .avatar
        .clone()
        .unwrap_or_else(assets::images::steam_anonymous);
    let mut value = row![
        container(
            image(avatar)
                .width(Length::Fixed(AVATAR_SIZE))
                .height(Length::Fixed(AVATAR_SIZE)),
        )
        .clip(true)
        .style(|_| iced::widget::container::Style {
            border: iced::border::rounded(AVATAR_SIZE / 2.0),
            ..iced::widget::container::Style::default()
        }),
        text(author.name.as_str()).size(tokens.typography.body_sm),
    ]
    .align_y(Center)
    .spacing(tokens.spacing.gap_xs);

    if author.failed {
        value = value.push(dead_icon(tokens.colors.text.into(), DEAD_ICON_SIZE));
    }

    if state.author_link_available() {
        mouse_area(value)
            .on_press(Message::AuthorLinkRequested)
            .interaction(iced::mouse::Interaction::Pointer)
            .into()
    } else {
        value.into()
    }
}

fn timestamp_value<'a>(row_data: &'a MetadataRow, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let value: iced::widget::text::Rich<'a, (), Message> = iced::widget::rich_text![
        iced::widget::span(metadata_value(&row_data.value, i18n))
            .size(tokens.typography.body_sm)
            .underline(true)
    ];

    if row_data.tooltip.trim().is_empty() {
        value.into()
    } else {
        tooltip_widget::below(value, row_data.tooltip.clone(), &tokens, TOOLTIP_MAX_WIDTH)
    }
}

fn metadata_value(value: &MetadataValue, i18n: &I18n) -> String {
    match value {
        MetadataValue::Bytes(value) => format_bytes(*value, i18n),
        MetadataValue::Relative(relative) => relative_text(relative, i18n),
    }
}

fn relative_text(relative: &RelativeTime, i18n: &I18n) -> String {
    if relative.count.is_empty() {
        i18n.tr(relative.key)
    } else {
        i18n.trn(relative.key, &[("arg0", relative.count.as_str())])
    }
}

fn workshop_link(ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let link = row![
        text(i18n.tr("preview-gma-steam-workshop"))
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

    container(
        mouse_area(link)
            .on_press(Message::WorkshopLinkRequested)
            .interaction(iced::mouse::Interaction::Pointer),
    )
    .center_x(Length::Fill)
    .into()
}

fn browser<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let snapshot = state.browser_snapshot();

    let body: Element<'a, Message> = if !snapshot.visible() || snapshot.total_files() == 0 {
        browser_empty_state(ctx)
    } else {
        let mut rows = column![].width(Length::Fill);
        for row_data in snapshot.rows() {
            rows = rows.push(browser_row(row_data, ctx));
        }
        scrollable(rows)
            .width(Length::Fill)
            .height(Length::Fill)
            .direction(scrollable::Direction::Vertical(
                theme::styles::vertical_scrollbar(&tokens),
            ))
            .style(move |_, status| theme::styles::scrollbar(&tokens, status))
            .into()
    };

    column![
        nav_bar(state, snapshot.can_go_up(), ctx),
        container(body).width(Length::Fill).height(Length::Fill),
        ribbon(snapshot, ctx),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn nav_bar<'a>(state: &'a State, can_go_up: bool, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let up = button(icon(
        assets::icons::chevron_up(),
        tokens.colors.text.into(),
        tokens.dims.icon_size,
    ))
    .on_press_maybe(can_go_up.then_some(Message::UpRequested))
    .padding(tokens.spacing.pad_xs)
    .style(move |_, status| theme::styles::ghost_button(&tokens, status));

    let path = scrollable(
        text(state.header_path_text())
            .size(tokens.typography.caption_xs)
            .wrapping(text::Wrapping::None),
    )
    .id(nav_path_scrollable_id())
    .width(Length::Fill)
    .direction(scrollable::Direction::Horizontal(
        theme::styles::hidden_vertical_scrollbar(),
    ));

    let copy = nav_control(
        assets::icons::context_copy(),
        state.can_copy_current_path(),
        Message::CopyCurrentPathRequested,
        i18n.tr("preview-gma-copy-path"),
        &tokens,
    );
    let open = nav_control(
        assets::icons::folder(),
        state.can_extract(),
        Message::OpenLocationRequested,
        i18n.tr("preview-gma-open-location"),
        &tokens,
    );

    container(
        row![up, path, copy, open]
            .align_y(Center)
            .spacing(tokens.spacing.gap_sm)
            .height(Length::Fill),
    )
    .padding([0.0, tokens.spacing.pad_sm])
    .width(Length::Fill)
    .height(Length::Fixed(tokens.dims.control_height))
    .style(move |_| theme::styles::preview_browser_top_bar(&tokens))
    .into()
}

fn nav_control<'a>(
    glyph: iced::widget::svg::Handle,
    enabled: bool,
    message: Message,
    tooltip: String,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let tokens = *tokens;
    let control = button(icon(
        glyph,
        tokens.colors.text.into(),
        tokens.dims.icon_size,
    ))
    .on_press_maybe(enabled.then_some(message))
    .padding(tokens.spacing.pad_xs)
    .style(move |_, status| theme::styles::ghost_button(&tokens, status));

    tooltip_widget::below(control, tooltip, &tokens, TOOLTIP_MAX_WIDTH)
}

fn browser_row<'a>(row_data: &FileBrowserRowData, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let message = match row_data.kind {
        FileBrowserEntryKind::Directory => Message::DirectoryOpened(row_data.current_path.clone()),
        FileBrowserEntryKind::File => file_row_activation_message(row_data.archive_path.clone()),
    };
    file_browser::row_view(row_data.clone(), Some(message), ctx)
}

#[cfg(feature = "asset-studio")]
fn file_row_activation_message(path: String) -> Message {
    Message::PreviewEntryRequested(path)
}

#[cfg(not(feature = "asset-studio"))]
fn file_row_activation_message(path: String) -> Message {
    Message::ExtractEntryRequested(path)
}

fn ribbon<'a>(
    snapshot: &'a super::state::BrowserSnapshot,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let total = snapshot.total_files();
    let items = if total == 1 {
        i18n.tr("prepare-publish-items-one")
    } else {
        let total = total.to_string();
        i18n.trn("prepare-publish-items-num", &[("arg0", total.as_str())])
    };
    let shown = snapshot.shown_count().to_string();
    let shown = i18n.trn("prepare-publish-items-shown", &[("arg0", shown.as_str())]);
    let size = format_bytes(snapshot.total_size_bytes(), i18n);

    container(
        text(format!("{items}  \u{2223}  {shown}  \u{2223}  {size}"))
            .size(tokens.typography.caption_xs)
            .width(Length::Fill)
            .align_x(Center),
    )
    .padding([tokens.spacing.pad_xs, tokens.spacing.pad_sm])
    .width(Length::Fill)
    .style(move |_| theme::styles::preview_browser_bottom_bar(&tokens))
    .into()
}

fn browser_empty_state(ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let dim = tokens.colors.browser_empty_dim.into();
    container(
        column![
            dead_icon(dim, tokens.dims.browser_empty_icon_size),
            text(i18n.tr("prepare-publish-no-files"))
                .size(tokens.typography.title)
                .color(dim),
        ]
        .align_x(Center)
        .spacing(tokens.spacing.gap),
    )
    .center(Length::Fill)
    .into()
}

fn dead_icon<'a>(color: Color, size: f32) -> Element<'a, Message> {
    icon(assets::icons::dead(), color, size)
}

fn icon<'a>(handle: iced::widget::svg::Handle, color: Color, size: f32) -> Element<'a, Message> {
    svg(handle)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_, _| svg::Style { color: Some(color) })
        .into()
}
