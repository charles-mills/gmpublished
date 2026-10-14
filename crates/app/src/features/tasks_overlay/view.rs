use std::time::Instant;

use iced::widget::text::{Shaping as TextShaping, Wrapping};
use iced::widget::{Space, button, column, container, row, stack, svg, text};
use iced::{Alignment, Border, Color, Element, Length, Padding, Shadow, Size, Vector, alignment};

use crate::bridge::tasks::{TaskKind, TransactionStatus};
use crate::format::format_bytes;
use crate::i18n::{Arg, I18n, translated_error};
use crate::theme::{Rgba, Tokens, ViewCtx};
use crate::widgets::icon::svg_icon;
use crate::{assets, widgets};

use super::Message;
use super::state::{Outcome, State, TOAST_HEIGHT, Toast};

const WIDTH_RATIO: f32 = 0.45;
const FALLBACK_WIDTH: f32 = 500.0;
const CORNER_RADIUS: f32 = 6.4;
const ICON_SIZE: f32 = 16.0;
const CONTENT_PADDING: f32 = 16.0;

pub fn view<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    viewport_size: Size,
    now: Instant,
) -> Option<Element<'a, Message>> {
    if state.is_empty() {
        return None;
    }

    let tokens = *ctx.tokens;
    let width = if viewport_size.width > 0.0 {
        (viewport_size.width * WIDTH_RATIO)
            .max(FALLBACK_WIDTH * tokens.text_scale())
            .min(760.0 * tokens.text_scale())
            .min((viewport_size.width - tokens.spacing.pad * 2.0).max(1.0))
    } else {
        FALLBACK_WIDTH
    };
    let toast_gap = tokens.spacing.gap;
    let toast_height = tokens
        .row_height(TOAST_HEIGHT)
        .max(tokens.typography.body * 2.6 + tokens.spacing.pad_sm * 2.0);

    let max_visible = State::max_visible(viewport_size.height).min(
        (viewport_size.height * 0.4 / (toast_height + toast_gap))
            .floor()
            .max(1.0) as usize,
    );
    let visible = state
        .toasts()
        .iter()
        .skip(state.toasts().len().saturating_sub(max_visible));

    let mut cards = column![].spacing(toast_gap).align_x(Alignment::Center);
    for toast in visible {
        cards = cards.push(card(toast, ctx, width, now));
    }

    Some(
        container(cards)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Bottom)
            .padding(Padding::ZERO.bottom(toast_gap))
            .into(),
    )
}

fn card<'a>(toast: &'a Toast, ctx: ViewCtx<'a>, width: f32, now: Instant) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let presence = toast.presence(now);
    let errored = matches!(toast.outcome(), Outcome::Error { .. });

    let (background, text_color) = if errored {
        (tokens.colors.error, tokens.colors.text_on_error)
    } else {
        (tokens.colors.neutral, tokens.colors.text_on_neutral)
    };
    let faded_text = Color::from(text_color).scale_alpha(presence);

    let mut layers = stack![].width(Length::Fill).height(Length::Fill);

    if !errored {
        let ratio = match toast.outcome() {
            Outcome::Finished { .. } => 1.0,
            _ => toast.progress() as f32,
        };
        if ratio > 0.0 {
            let fill = container(Space::new())
                .width(Length::Fixed(width * ratio.clamp(0.0, 1.0)))
                .height(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(
                        Color::from(tokens.colors.success)
                            .scale_alpha(presence)
                            .into(),
                    ),
                    ..container::Style::default()
                });
            layers = layers.push(row![fill].width(Length::Fill).height(Length::Fill));
        }
    }

    let content = row![
        status_icon(toast, &tokens, faded_text, now),
        text(toast_label(toast, ctx.i18n))
            .font(assets::fonts::default_font())
            .size(tokens.typography.body)
            .color(faded_text)
            .shaping(TextShaping::Advanced)
            .width(Length::Fill)
            .wrapping(Wrapping::WordOrGlyph),
    ]
    .spacing(tokens.spacing.gap_sm)
    .align_y(Alignment::Center);
    layers = layers.push(
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
            .padding(
                Padding::ZERO
                    .horizontal(tokens.space(CONTENT_PADDING))
                    .right(
                        tokens.space(CONTENT_PADDING)
                            + tokens.icon_size(ICON_SIZE)
                            + tokens.spacing.gap,
                    ),
            ),
    );

    if toast.cancellable() {
        let task_id = toast.task_id();
        let cancel = button(
            svg(assets::icons::cross())
                .width(Length::Fixed(tokens.icon_size(ICON_SIZE)))
                .height(Length::Fixed(tokens.icon_size(ICON_SIZE)))
                .style(move |_, _| svg::Style {
                    color: Some(faded_text),
                }),
        )
        .on_press(Message::CancelPressed(task_id))
        .padding(tokens.spacing.pad_xs)
        .style(move |_, _| cancel_button_style(faded_text));
        layers = layers.push(
            container(cancel)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(alignment::Horizontal::Right)
                .align_y(alignment::Vertical::Center)
                .padding(Padding::ZERO.right(tokens.space(CONTENT_PADDING))),
        );
    }

    let height = tokens
        .row_height(TOAST_HEIGHT)
        .max(tokens.typography.body * 2.6 + tokens.spacing.pad_sm * 2.0);
    let content = container(layers)
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .clip(true)
        .style(move |_| card_style(&tokens, background, presence));
    container(content)
        .height(Length::Fixed((height * presence).max(1.0)))
        .align_y(alignment::Vertical::Bottom)
        .clip(true)
        .into()
}

fn status_icon<'a>(
    toast: &'a Toast,
    tokens: &Tokens,
    color: Color,
    now: Instant,
) -> Element<'a, Message> {
    let tokens = *tokens;
    match toast.outcome() {
        Outcome::Pending => {
            let mut spinner_tokens = tokens;
            spinner_tokens.colors.text_on_neutral = crate::theme::motion::scaled_alpha(
                spinner_tokens.colors.text_on_neutral,
                toast.presence(now),
            );
            widgets::spinner::spinner(
                &spinner_tokens,
                if tokens.motion.enabled() {
                    toast.spinner_elapsed(now)
                } else {
                    0.0
                },
                tokens.icon_size(ICON_SIZE),
            )
        }
        Outcome::Finished { .. } => {
            svg_icon(assets::icons::check(), color, tokens.icon_size(ICON_SIZE)).into()
        }
        Outcome::Error { .. } => svg_icon(
            assets::icons::circle_alert(),
            color,
            tokens.icon_size(ICON_SIZE),
        )
        .into(),
    }
}

fn toast_label(toast: &Toast, i18n: &I18n) -> String {
    match toast.outcome() {
        Outcome::Error { error, .. } => translated_error(i18n, error),
        Outcome::Finished { .. } => match toast.kind() {
            // Notices carry their message in the status key and finish at
            // birth, so the label stays the message rather than "Done".
            TaskKind::Notice => i18n.tr(toast.status().translation_key()),
            _ => i18n.tr("downloader-status-finished"),
        },
        Outcome::Pending => status_text(toast, i18n),
    }
}

fn status_text(toast: &Toast, i18n: &I18n) -> String {
    let status = toast.status();
    match status {
        // Byte-progress statuses share the downloader's formatting, which
        // needs a message taking percent/done/total placeholders rather than
        // the plain label this status renders as elsewhere.
        TransactionStatus::PublishPacking | TransactionStatus::Extracting => {
            let ftl_key = match status {
                TransactionStatus::PublishPacking => "publish-packing",
                _ => status.translation_key(),
            };
            let total = format_bytes(toast.total_bytes(), i18n);
            let done = format_bytes((toast.total_bytes() as f64 * toast.progress()) as u64, i18n);
            i18n.trn(
                ftl_key,
                &[
                    (
                        "percent",
                        Arg::Number(&format!("{:.0}", toast.progress() * 100.0)),
                    ),
                    ("done", Arg::Text(done.as_str())),
                    ("total", Arg::Text(total.as_str())),
                ],
            )
        }
        TransactionStatus::PublishProcessingIcon => i18n.tr("publish-processing-icon"),
        status => i18n.tr(status.translation_key()),
    }
}

fn card_style(tokens: &Tokens, background: Rgba, opacity: f32) -> container::Style {
    let tokens = *tokens;
    container::Style {
        background: Some(Color::from(background).scale_alpha(opacity).into()),
        border: Border {
            radius: CORNER_RADIUS.into(),
            ..Border::default()
        },
        shadow: Shadow {
            color: Color::from(tokens.colors.shadow).scale_alpha(opacity),
            offset: Vector::ZERO,
            blur_radius: 6.0,
        },
        snap: true,
        ..container::Style::default()
    }
}

fn cancel_button_style(text_color: Color) -> button::Style {
    button::Style {
        background: None,
        border: Border::default(),
        shadow: Shadow::default(),
        text_color,
        snap: true,
    }
}
