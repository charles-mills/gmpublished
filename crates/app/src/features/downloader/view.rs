use std::time::Instant;

use iced::widget::{
    Stack, button, column, container, progress_bar, responsive, row, scrollable, svg, text,
    text_input,
};
use iced::{Background, Border, Center, Color, Element, Length, Padding, Shadow};
use iced::{alignment, font};

use crate::{
    assets,
    format::format_bytes,
    i18n::I18n,
    theme::{self, Tokens, ViewCtx},
    widgets::tooltip as tooltip_widget,
};

use super::model::{DownloaderJob, EXTRACT_STATUS, JobProgress, Section};
use super::{Message, State};

const JOB_ROW_HEIGHT: f32 = 44.0;
const HEADER_HEIGHT: f32 = 36.0;
const BOTTOM_BAR_HEIGHT: f32 = 40.0;
const TOP_INPUT_ICON_LEFT: f32 = 16.0;
const TOP_INPUT_ICON_SIZE: f32 = 16.0;
const TOP_INPUT_LEFT_PADDING: f32 = 40.0;
const TOP_INPUT_RIGHT_PADDING: f32 = 12.0;
const TOP_INPUT_VERTICAL_PADDING: f32 = 15.0;
const TOP_ICON_SIZE: f32 = 22.0;
const WIDE_LAYOUT_MIN_WIDTH: f32 = 1120.0;
const COMPACT_TAB_HEIGHT: f32 = 40.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LayoutMode {
    Compact,
    Wide,
}

fn layout_mode(available_width: f32) -> LayoutMode {
    if available_width >= WIDE_LAYOUT_MIN_WIDTH {
        LayoutMode::Wide
    } else {
        LayoutMode::Compact
    }
}

pub fn view<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let now = Instant::now();
    let tokens = *ctx.tokens;
    let sections = responsive(move |size| match layout_mode(size.width) {
        LayoutMode::Wide => wide_sections(state, ctx, now),
        LayoutMode::Compact => compact_sections(state, ctx, now),
    });

    column![input_row(state, ctx), sections]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(tokens.spacing.gap)
        .into()
}

fn wide_sections<'a>(state: &'a State, ctx: ViewCtx<'a>, now: Instant) -> Element<'a, Message> {
    row![
        section_view(Section::Downloading, state.downloading(), ctx, now, true)
            .width(Length::FillPortion(1)),
        section_view(Section::Extracting, state.extracting(), ctx, now, true)
            .width(Length::FillPortion(1)),
    ]
    .spacing(ctx.tokens.spacing.gap)
    .height(Length::Fill)
    .into()
}

fn compact_sections<'a>(state: &'a State, ctx: ViewCtx<'a>, now: Instant) -> Element<'a, Message> {
    let selected = state.compact_section();
    let jobs = match selected {
        Section::Downloading => state.downloading(),
        Section::Extracting => state.extracting(),
    };

    column![
        compact_tabs(state, ctx),
        section_view(selected, jobs, ctx, now, false),
    ]
    .spacing(ctx.tokens.spacing.gap_sm)
    .height(Length::Fill)
    .into()
}

fn compact_tabs<'a>(state: &State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    container(
        row![
            compact_tab(Section::Downloading, state, ctx),
            compact_tab(Section::Extracting, state, ctx),
        ]
        .spacing(tokens.spacing.gap_xs),
    )
    .padding(4.0)
    .width(Length::Fill)
    .height(COMPACT_TAB_HEIGHT)
    .style(move |_| compact_tab_bar_style(&tokens))
    .into()
}

fn compact_tab<'a>(section: Section, state: &State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let label = format!(
        "{} ({})",
        ctx.i18n.tr(section.label_key()),
        state.section_count(section)
    );
    let active = state.compact_section() == section;

    button(
        text(label)
            .size(tokens.typography.body_sm)
            .font(theme::styles::inter_font(font::Weight::Semibold))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center),
    )
    .on_press(Message::CompactSectionSelected(section))
    .padding(0.0)
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_, status| compact_tab_style(&tokens, active, status))
    .into()
}

fn input_row<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let input = text_input(&i18n.tr("downloader-workshop-input"), state.input_text())
        .on_input(Message::InputEdited)
        .on_submit(Message::InputSubmitted)
        .padding(
            Padding::ZERO
                .vertical(TOP_INPUT_VERTICAL_PADDING)
                .left(TOP_INPUT_LEFT_PADDING)
                .right(TOP_INPUT_RIGHT_PADDING),
        )
        .size(tokens.typography.body)
        .width(Length::Fill)
        .style(move |_, status| top_input_style(&tokens, state.input_error(), status));

    let input_icon = container(icon_with_opacity(
        assets::icons::link_chain(),
        if state.input_error() {
            tokens.colors.error.into()
        } else {
            tokens.colors.text.into()
        },
        TOP_INPUT_ICON_SIZE,
        if state.input_error() {
            1.0
        } else {
            tokens.dims.icon_rest_opacity
        },
    ))
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(Padding::ZERO.left(TOP_INPUT_ICON_LEFT))
    .align_x(alignment::Horizontal::Left)
    .align_y(alignment::Vertical::Center);

    let input = Stack::new()
        .push(input)
        .push(input_icon)
        .width(Length::Fill)
        .height(tokens.dims.control_height_xl);

    let folder = tooltip_widget::below(
        top_icon_button(
            assets::icons::folder(),
            &tokens,
            Message::BulkExtractRequested,
        ),
        i18n.tr("downloader-bulk-extract"),
        &tokens,
        220.0,
    );
    let mut rows = column![
        row![folder, input]
            .align_y(Center)
            .spacing(tokens.spacing.gap_md)
            .height(tokens.dims.control_height_xl)
    ]
    .spacing(tokens.spacing.gap_xs);

    if state.input_error() {
        rows = rows.push(
            text(i18n.tr("downloader-input-error"))
                .size(tokens.typography.caption)
                .color(Color::from(tokens.colors.error)),
        );
    }

    rows.into()
}

fn section_view<'a>(
    section: Section,
    jobs: &'a [DownloaderJob],
    ctx: ViewCtx<'a>,
    now: Instant,
    show_title: bool,
) -> iced::widget::Container<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let title = row![
        section_icon(section, &tokens),
        text(i18n.tr(section.label_key()))
            .size(tokens.typography.display_sm)
            .font(theme::styles::inter_font(font::Weight::Bold))
            .line_height(1.0)
            .color(Color::from(tokens.colors.text)),
    ]
    .align_y(Center)
    .spacing(tokens.spacing.gap_sm);

    let body: Element<'a, Message> = if jobs.is_empty() {
        container(empty_state(section, ctx))
            .center(Length::Fill)
            .into()
    } else {
        let rows = jobs
            .iter()
            .enumerate()
            .fold(column![], |rows, (index, job)| {
                rows.push(job_row(section, job, index % 2 == 0, ctx, now))
            });
        scrollable(rows)
            .height(Length::Fill)
            .direction(scrollable::Direction::Vertical(
                theme::styles::vertical_scrollbar(&tokens),
            ))
            .style(move |_, status| theme::styles::scrollbar(&tokens, status))
            .into()
    };

    let panel_content = if jobs.is_empty() {
        column![body].height(Length::Fill)
    } else {
        column![table_header(&tokens), body].height(Length::Fill)
    };

    let panel = container(panel_content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| panel_style(&tokens));

    let content = column![panel, section_actions(section, ctx)]
        .spacing(10.0)
        .height(Length::Fill);
    let content = if show_title {
        column![container(title).center_x(Length::Fill), content]
            .spacing(10.0)
            .height(Length::Fill)
    } else {
        column![content].height(Length::Fill)
    };

    container(content).width(Length::Fill).height(Length::Fill)
}

fn table_header<'a>(tokens: &Tokens) -> Element<'a, Message> {
    let tokens = *tokens;
    container(
        row![
            text("").width(48.0),
            header_text("Addon", &tokens, alignment::Horizontal::Left).width(Length::Fill),
            header_text("Speed", &tokens, alignment::Horizontal::Right).width(84.0),
            header_text("Total", &tokens, alignment::Horizontal::Right).width(84.0),
            header_text("Progress", &tokens, alignment::Horizontal::Center).width(180.0),
        ]
        .align_y(Center)
        .spacing(tokens.spacing.gap_sm)
        .padding(
            Padding::ZERO
                .left(tokens.spacing.gap_sm)
                .right(tokens.spacing.gap_md),
        ),
    )
    .height(HEADER_HEIGHT)
    .align_y(alignment::Vertical::Center)
    .into()
}

fn header_text(
    label: &'static str,
    tokens: &Tokens,
    align_x: alignment::Horizontal,
) -> iced::widget::Text<'static> {
    text(label)
        .size(tokens.typography.body_sm)
        .font(theme::styles::inter_font(font::Weight::Semibold))
        .color(Color::from(tokens.colors.text_dim))
        .align_x(align_x)
}

fn job_row<'a>(
    section: Section,
    job: &'a DownloaderJob,
    odd: bool,
    ctx: ViewCtx<'a>,
    now: Instant,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    container(
        row![
            row_actions(section, job, &tokens),
            text(job.title())
                .size(tokens.typography.body_sm)
                .color(job_title_color(job, &tokens))
                .width(Length::Fill),
            text(speed_text(job, i18n, now))
                .size(tokens.typography.body_sm)
                .color(Color::from(tokens.colors.text_dim))
                .width(84.0)
                .align_x(alignment::Horizontal::Right),
            text(total_text(job, i18n))
                .size(tokens.typography.body_sm)
                .color(Color::from(tokens.colors.text_dim))
                .width(84.0)
                .align_x(alignment::Horizontal::Right),
            progress_cell(section, job, ctx, now).width(180.0),
        ]
        .align_y(Center)
        .spacing(tokens.spacing.gap_sm)
        .padding(
            Padding::ZERO
                .vertical(tokens.spacing.gap_xs)
                .left(tokens.spacing.gap_sm)
                .right(tokens.spacing.gap_md),
        ),
    )
    .height(JOB_ROW_HEIGHT)
    .style(move |_| row_style(&tokens, odd))
    .into()
}

fn row_actions<'a>(section: Section, job: &DownloaderJob, tokens: &Tokens) -> Element<'a, Message> {
    let cancel = mini_icon_button(
        assets::icons::cross(),
        tokens,
        Message::CancelRequested {
            section,
            row_id: job.id(),
        },
    );

    let mut actions = row![cancel].spacing(tokens.spacing.gap_xs).width(48.0);
    if let Some(workshop_id) = job.workshop_id() {
        actions = actions.push(mini_icon_button(
            assets::icons::link_chain(),
            tokens,
            Message::OpenWorkshopRequested(Some(workshop_id)),
        ));
    }

    actions.into()
}

fn progress_cell<'a>(
    section: Section,
    job: &'a DownloaderJob,
    ctx: ViewCtx<'a>,
    now: Instant,
) -> iced::widget::Container<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    match job.progress() {
        JobProgress::Finished if job.open_path().is_some() => {
            let open = button(centered_text(
                i18n.tr("downloader-open"),
                tokens.typography.body_sm,
                if job.previewable() {
                    font::Weight::Normal
                } else {
                    font::Weight::Bold
                },
                &tokens,
            ))
            .on_press(Message::OpenRequested {
                section,
                row_id: job.id(),
            })
            .padding(0)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_, status| downloader_button_style(&tokens, status));

            let mut actions = row![open].spacing(tokens.spacing.gap_xs);
            if job.previewable() {
                actions = actions.push(
                    button(centered_text(
                        i18n.tr("downloader-preview"),
                        tokens.typography.body_sm,
                        font::Weight::Bold,
                        &tokens,
                    ))
                    .on_press(Message::PreviewRequested {
                        section,
                        row_id: job.id(),
                    })
                    .padding(0)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(move |_, status| downloader_button_style(&tokens, status)),
                );
            }

            container(actions).height(32.0)
        }
        JobProgress::Error(error) => container(
            text(i18n.trn(
                "downloader-status-error",
                &[("arg0", error.to_string().as_str())],
            ))
            .size(tokens.typography.body_sm)
            .color(Color::from(tokens.colors.error)),
        )
        .height(32.0)
        .center_y(Length::Fill),
        _ => container(
            column![
                progress_bar(0.0..=100.0, job.smoothed_ratio(now) * 100.0)
                    .girth(8.0)
                    .style(move |_| theme::styles::progress_bar(&tokens)),
                text(progress_label(job, i18n, now))
                    .size(tokens.typography.caption_xs)
                    .color(Color::from(tokens.colors.text_dim)),
            ]
            .spacing(tokens.spacing.gap_xs),
        )
        .height(36.0)
        .center_y(Length::Fill),
    }
}

fn empty_state(section: Section, ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let (label, message): (String, Message) = match section {
        Section::Downloading => (
            i18n.tr("downloader-open-workshop"),
            Message::OpenWorkshopRequested(None),
        ),
        Section::Extracting => (
            i18n.tr("downloader-set-destination"),
            Message::DestinationRequested,
        ),
    };

    column![
        text(i18n.tr("downloader-empty"))
            .size(tokens.typography.body_sm)
            .color(Color::from(tokens.colors.text)),
        idle_button(label, &tokens, message),
    ]
    .align_x(Center)
    .spacing(tokens.spacing.gap_sm)
    .into()
}

fn section_actions(section: Section, ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let remove = text_button(
        i18n.tr("downloader-remove-all"),
        &tokens,
        Message::RemoveAllRequested(section),
    )
    .width(Length::Fill)
    .height(BOTTOM_BAR_HEIGHT);

    match section {
        Section::Downloading => row![remove].height(BOTTOM_BAR_HEIGHT).into(),
        Section::Extracting => row![
            remove,
            text_button(
                i18n.tr("downloader-open-all"),
                &tokens,
                Message::OpenAllRequested,
            )
            .width(Length::Fill)
            .height(BOTTOM_BAR_HEIGHT),
        ]
        .spacing(tokens.spacing.gap_md)
        .height(BOTTOM_BAR_HEIGHT)
        .into(),
    }
}

fn top_icon_button<'a>(
    handle: svg::Handle,
    tokens: &Tokens,
    message: Message,
) -> iced::widget::Button<'a, Message> {
    let tokens = *tokens;
    let glyph = icon_with_opacity(
        handle,
        tokens.colors.text.into(),
        TOP_ICON_SIZE,
        tokens.dims.icon_rest_opacity,
    );

    button(
        container(glyph)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center),
    )
    .on_press(message)
    .padding(0)
    .width(tokens.dims.control_height_xl)
    .height(tokens.dims.control_height_xl)
    .style(move |_, status| top_icon_button_style(&tokens, status))
}

fn mini_icon_button<'a>(
    handle: svg::Handle,
    tokens: &Tokens,
    message: Message,
) -> iced::widget::Button<'a, Message> {
    let tokens = *tokens;
    button(container(icon(handle, tokens.colors.text_dim.into(), 16.0)).center(Length::Fill))
        .on_press(message)
        .padding(0)
        .width(22.0)
        .height(36.0)
        .style(move |_, status| transparent_button_style(&tokens, status))
}

fn section_icon<'a>(section: Section, tokens: &Tokens) -> Element<'a, Message> {
    let handle = match section {
        Section::Downloading => assets::icons::cloud_download(),
        Section::Extracting => assets::icons::folder_add(),
    };
    icon(handle, tokens.colors.text.into(), 32.0)
}

fn icon<'a>(handle: svg::Handle, color: Color, size: f32) -> Element<'a, Message> {
    icon_with_opacity(handle, color, size, 1.0)
}

fn icon_with_opacity<'a>(
    handle: svg::Handle,
    color: Color,
    size: f32,
    opacity: f32,
) -> Element<'a, Message> {
    container(
        svg(handle)
            .width(size)
            .height(size)
            .style(move |_, _| svg::Style { color: Some(color) })
            .opacity(opacity),
    )
    .into()
}

fn centered_text<'a>(
    label: String,
    size: f32,
    weight: font::Weight,
    tokens: &Tokens,
) -> iced::widget::Text<'a> {
    text(label)
        .size(size)
        .font(theme::styles::inter_font(weight))
        .color(Color::from(tokens.colors.text))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center)
}

fn compact_centered_text<'a>(
    label: String,
    size: f32,
    weight: font::Weight,
    tokens: &Tokens,
) -> iced::widget::Text<'a> {
    text(label)
        .size(size)
        .font(theme::styles::inter_font(weight))
        .color(Color::from(tokens.colors.text))
        .height(Length::Fill)
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center)
}

fn text_button<'a>(
    label: String,
    tokens: &Tokens,
    message: Message,
) -> iced::widget::Button<'a, Message> {
    let tokens = *tokens;
    button(centered_text(
        label,
        tokens.typography.body,
        font::Weight::Semibold,
        &tokens,
    ))
    .on_press(message)
    .padding(0)
    .style(move |_, status| downloader_button_style(&tokens, status))
}

fn idle_button<'a>(
    label: String,
    tokens: &Tokens,
    message: Message,
) -> iced::widget::Button<'a, Message> {
    let tokens = *tokens;
    button(compact_centered_text(
        label,
        tokens.typography.body_sm,
        font::Weight::Semibold,
        &tokens,
    ))
    .on_press(message)
    .padding(Padding::ZERO.horizontal(16.0))
    .width(Length::Shrink)
    .height(tokens.dims.control_height_sm)
    .style(move |_, status| idle_button_style(&tokens, status))
}

fn job_title_color(job: &DownloaderJob, tokens: &Tokens) -> Color {
    match job.progress() {
        JobProgress::Error(_) => tokens.colors.error.into(),
        JobProgress::Running { .. } | JobProgress::Finished => tokens.colors.text.into(),
    }
}

fn progress_label(job: &DownloaderJob, i18n: &I18n, now: Instant) -> String {
    match job.progress() {
        JobProgress::Running { ratio, status_key } if status_key == EXTRACT_STATUS => {
            let total = format_bytes(job.total_bytes(), i18n);
            let done = format_bytes((job.total_bytes() as f64 * ratio) as u64, i18n);
            i18n.trn(
                status_key,
                &[
                    ("arg0", &format!("{:.0}", ratio * 100.0)),
                    ("arg1", done.as_str()),
                    ("arg2", total.as_str()),
                ],
            )
        }
        JobProgress::Running { ratio, .. } => {
            let speed = speed_text(job, i18n, now);
            i18n.trn(
                "downloader-progress-percent",
                &[
                    ("arg0", &format!("{:.0}", ratio * 100.0)),
                    ("arg1", speed.as_str()),
                ],
            )
        }
        JobProgress::Finished => i18n.tr("downloader-status-finished"),
        JobProgress::Error(error) => i18n.trn(
            "downloader-status-error",
            &[("arg0", error.to_string().as_str())],
        ),
    }
}

fn speed_text(job: &DownloaderJob, i18n: &I18n, now: Instant) -> String {
    let JobProgress::Running { ratio, status_key } = job.progress() else {
        return String::new();
    };
    if status_key == EXTRACT_STATUS {
        return String::new();
    }

    job.started_at()
        .and_then(|started| {
            let elapsed = now.saturating_duration_since(started).as_secs_f64();
            (elapsed > 0.0).then_some(job.total_bytes() as f64 * ratio / elapsed)
        })
        .map(|bytes_per_second| {
            let formatted = format_bytes(bytes_per_second as u64, i18n);
            i18n.trn("byte-rate-per-second", &[("arg0", formatted.as_str())])
        })
        .unwrap_or_default()
}

fn total_text(job: &DownloaderJob, i18n: &I18n) -> String {
    if job.total_bytes() == 0 {
        String::new()
    } else {
        format_bytes(job.total_bytes(), i18n)
    }
}

fn panel_style(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.surface_raised.into(),
        tokens.colors.text.into(),
        Border {
            color: tokens.colors.border.into(),
            width: tokens.dims.border_width,
            radius: tokens.radii.lg.into(),
        },
    )
}

fn compact_tab_bar_style(tokens: &Tokens) -> container::Style {
    container_style(
        tokens.colors.row_fill_subtle.into(),
        tokens.colors.text.into(),
        Border {
            color: tokens.colors.border.into(),
            width: tokens.dims.border_width,
            radius: tokens.radii.lg.into(),
        },
    )
}

fn compact_tab_style(tokens: &Tokens, active: bool, status: button::Status) -> button::Style {
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

fn row_style(tokens: &Tokens, odd: bool) -> container::Style {
    let background = if odd {
        tokens.colors.row_fill_alt
    } else {
        tokens.colors.row_fill
    };
    container_style(
        background.into(),
        tokens.colors.text.into(),
        Border::default(),
    )
}

fn top_input_style(
    tokens: &Tokens,
    error: bool,
    status: iced::widget::text_input::Status,
) -> iced::widget::text_input::Style {
    let disabled = matches!(status, iced::widget::text_input::Status::Disabled);
    let border_width =
        if error || matches!(status, iced::widget::text_input::Status::Focused { .. }) {
            tokens.dims.focus_border_width
        } else {
            0.0
        };
    let border_color = if error {
        tokens.colors.error_dark
    } else {
        tokens.colors.focus_ring
    };
    let control_bg = if disabled {
        tokens
            .colors
            .control_bg
            .with_alpha(theme::motion::opacity_byte(
                tokens.dims.disabled_opacity_strong,
            ))
    } else {
        tokens.colors.control_bg
    };
    let value = if error {
        tokens.colors.error
    } else if disabled {
        tokens
            .colors
            .text
            .with_alpha(theme::motion::opacity_byte(tokens.dims.disabled_opacity))
    } else {
        tokens.colors.text
    };
    let placeholder = if error {
        tokens.colors.error
    } else {
        tokens.colors.text_dim
    };

    iced::widget::text_input::Style {
        background: Color::from(control_bg).into(),
        border: Border {
            color: border_color.into(),
            width: border_width,
            radius: tokens.radii.base.into(),
        },
        icon: if error {
            tokens.colors.error.into()
        } else {
            tokens.colors.text.into()
        },
        placeholder: placeholder.into(),
        value: value.into(),
        selection: tokens.colors.neutral.into(),
    }
}

fn top_icon_button_style(tokens: &Tokens, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Pressed => tokens.colors.control_pressed,
        button::Status::Active | button::Status::Hovered => tokens.colors.control_bg,
        button::Status::Disabled => tokens
            .colors
            .control_bg
            .with_alpha(theme::motion::opacity_byte(tokens.dims.disabled_opacity)),
    };

    button::Style {
        background: Some(Color::from(background).into()),
        text_color: tokens.colors.text.into(),
        border: Border {
            color: tokens.colors.bg.with_alpha(0).into(),
            width: 0.0,
            radius: tokens.radii.base.into(),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

fn downloader_button_style(tokens: &Tokens, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Pressed => tokens.colors.button_pressed,
        button::Status::Active | button::Status::Hovered => tokens.colors.button_bg,
        button::Status::Disabled => tokens
            .colors
            .button_bg
            .with_alpha(theme::motion::opacity_byte(tokens.dims.disabled_opacity)),
    };

    button::Style {
        background: Some(Color::from(background).into()),
        text_color: tokens.colors.text.into(),
        border: Border {
            color: tokens.colors.bg.with_alpha(0).into(),
            width: 0.0,
            radius: tokens.radii.base.into(),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

fn idle_button_style(tokens: &Tokens, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Pressed => tokens.colors.control_pressed,
        button::Status::Active | button::Status::Hovered => tokens.colors.control_bg_alt,
        button::Status::Disabled => tokens
            .colors
            .control_bg_alt
            .with_alpha(theme::motion::opacity_byte(tokens.dims.disabled_opacity)),
    };

    button::Style {
        background: Some(Color::from(background).into()),
        text_color: tokens.colors.text.into(),
        border: Border {
            color: tokens.colors.bg.into(),
            width: tokens.dims.border_width,
            radius: tokens.radii.md.into(),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

fn transparent_button_style(tokens: &Tokens, status: button::Status) -> button::Style {
    let icon = match status {
        button::Status::Hovered | button::Status::Pressed => tokens.colors.text,
        button::Status::Active | button::Status::Disabled => tokens.colors.text_dim,
    };
    button::Style {
        background: Some(Color::from(tokens.colors.bg.with_alpha(0)).into()),
        text_color: icon.into(),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: true,
    }
}

fn container_style(background: Color, text: Color, border: Border) -> container::Style {
    container::Style {
        text_color: Some(text),
        background: Some(Background::Color(background)),
        border,
        shadow: Shadow::default(),
        snap: true,
    }
}

#[cfg(test)]
mod tests {
    use iced::mouse;
    use iced::{Event, Settings, Size};

    use std::path::PathBuf;

    use crate::bridge::domain::PublishedFileId;
    use crate::bridge::tasks::{TaskId, WorkshopDownloadTaskKind};
    use crate::i18n::I18n;
    use crate::theme::{Tokens, ViewCtx};

    use super::super::model::{DownloaderEvent, workshop_result_success_with_gma};
    use super::super::update;
    use super::{LayoutMode, Message, State, layout_mode, view};

    #[test]
    fn layout_mode_switches_at_the_supported_wide_boundary() {
        assert_eq!(layout_mode(1119.0), LayoutMode::Compact);
        assert_eq!(layout_mode(1120.0), LayoutMode::Wide);
        assert_eq!(layout_mode(1280.0), LayoutMode::Wide);
    }

    #[test]
    fn compact_tabs_dispatch_the_selected_section() {
        let state = State::default();
        let tokens = Tokens::dark();
        let i18n = I18n::for_locale(Some("en"));
        let ctx = ViewCtx::new(&tokens, &i18n);
        let mut ui = iced_test::Simulator::with_size(
            Settings::default(),
            Size::new(800.0, 600.0),
            view(&state, ctx),
        );

        ui.point_at(iced::Point::new(600.0, 80.0));
        let _statuses = ui.simulate([
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ]);

        assert!(ui.into_messages().any(|message| {
            message == Message::CompactSectionSelected(super::Section::Extracting)
        }));
    }

    /// Drives a real click through the widget tree at every grid point of
    /// the Downloading section and asserts the row's X button dispatched
    /// its CancelRequested message. Guards the widget layer itself; the
    /// message/effect pipeline below it is covered by update tests.
    #[test]
    fn cancel_button_click_dispatches_cancel_requested() {
        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::EventReceived(DownloaderEvent::TaskStarted {
                kind: WorkshopDownloadTaskKind::Download,
                item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
                task_id: TaskId::from_raw(7),
            }),
        );
        assert_eq!(state.downloading().len(), 1);
        let row_id = state.downloading()[0].id();

        let tokens = Tokens::dark();
        let i18n = I18n::for_locale(Some("en"));
        let ctx = ViewCtx::new(&tokens, &i18n);
        let mut ui = iced_test::Simulator::with_size(
            Settings::default(),
            Size::new(1280.0, 720.0),
            view(&state, ctx),
        );

        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        for x in (0..520).step_by(6) {
            for y in (60..280).step_by(6) {
                ui.point_at(iced::Point::new(x as f32, y as f32));
                let _statuses = ui.simulate([press.clone(), release.clone()]);
            }
        }

        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            messages.contains(&Message::CancelRequested {
                section: super::Section::Downloading,
                row_id,
            }),
            "no CancelRequested dispatched; messages seen: {messages:?}",
        );
    }

    /// A finished workshop row whose source `.gma` survived shows Open and
    /// Preview side by side; clicking across the Extracting section must
    /// dispatch both row actions.
    #[test]
    fn finished_row_dispatches_both_open_and_preview() {
        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::EventReceived(DownloaderEvent::TaskStarted {
                kind: WorkshopDownloadTaskKind::Extract,
                item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
                task_id: TaskId::from_raw(7),
            }),
        );
        let _effects = update(
            &mut state,
            Message::EventReceived(DownloaderEvent::WorkshopDownloadFinished(
                workshop_result_success_with_gma(
                    123,
                    PathBuf::from("/tmp/extracted/Addon"),
                    Some(PathBuf::from("/tmp/workshop/addon_123.gma")),
                ),
            )),
        );
        assert_eq!(state.extracting().len(), 1);
        assert!(state.extracting()[0].previewable());
        let row_id = state.extracting()[0].id();

        let tokens = Tokens::dark();
        let i18n = I18n::for_locale(Some("en"));
        let ctx = ViewCtx::new(&tokens, &i18n);
        let mut ui = iced_test::Simulator::new(view(&state, ctx));

        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        for x in (500..1024).step_by(6) {
            for y in (60..280).step_by(6) {
                ui.point_at(iced::Point::new(x as f32, y as f32));
                let _statuses = ui.simulate([press.clone(), release.clone()]);
            }
        }

        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            messages.contains(&Message::OpenRequested {
                section: super::Section::Extracting,
                row_id,
            }),
            "no OpenRequested dispatched; messages seen: {messages:?}",
        );
        assert!(
            messages.contains(&Message::PreviewRequested {
                section: super::Section::Extracting,
                row_id,
            }),
            "no PreviewRequested dispatched; messages seen: {messages:?}",
        );
    }
}
