use std::time::Instant;

use iced::widget::text::{Shaping as TextShaping, Wrapping};
use iced::widget::{Space, button, column, container, row, stack, svg, text};
use iced::{Alignment, Border, Color, Element, Length, Padding, Shadow, Size, Vector, alignment};

use crate::backend::tasks::TaskKind;
use crate::format::format_bytes;
use crate::i18n::{I18n, translated_error};
use crate::theme::{Rgba, Tokens, ViewCtx};
use crate::{assets, widgets};

use super::Message;
use super::state::{Outcome, State, TOAST_GAP, TOAST_HEIGHT, Toast};

/// Upstream sizes the stack at 45vw, centered at the bottom of the window.
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

    let width = if viewport_size.width > 0.0 {
        viewport_size.width * WIDTH_RATIO
    } else {
        FALLBACK_WIDTH
    };

    // Oldest toasts sit at the top of the stack, newest at the bottom edge;
    // overflow beyond the viewport-scaled cap stays queued until a slot
    // frees up.
    let max_visible = State::max_visible(viewport_size.height);
    let visible = state
        .toasts()
        .iter()
        .skip(state.toasts().len().saturating_sub(max_visible));

    let mut cards = column![].spacing(TOAST_GAP).align_x(Alignment::Center);
    for toast in visible {
        cards = cards.push(card(toast, ctx, width, now));
    }

    Some(
        container(cards)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Bottom)
            .padding(Padding::ZERO.bottom(TOAST_GAP))
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

    // Success-colored progress sweep behind the label; errors drop it, like
    // upstream hiding the progress fill on error cards.
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
            .wrapping(Wrapping::None),
    ]
    .spacing(8.0)
    .align_y(Alignment::Center);
    layers = layers.push(
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center),
    );

    if toast.cancellable() {
        let task_id = toast.task_id();
        let cancel = button(
            svg(assets::icons::cross())
                .width(Length::Fixed(ICON_SIZE))
                .height(Length::Fixed(ICON_SIZE))
                .style(move |_, _| svg::Style {
                    color: Some(faded_text),
                }),
        )
        .on_press(Message::CancelPressed(task_id))
        .padding(0.0)
        .style(move |_, _| cancel_button_style(faded_text));
        layers = layers.push(
            container(cancel)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(alignment::Horizontal::Right)
                .align_y(alignment::Vertical::Center)
                .padding(Padding::ZERO.right(CONTENT_PADDING)),
        );
    }

    // iced's uniform scale cannot express upstream's scale-in keyframe, so
    // enter/exit reuse the context menu's clip-grow-plus-fade instead; the
    // height collapse doubles as the stack-shift animation when a toast
    // above expires.
    let height = (TOAST_HEIGHT * presence).max(1.0);
    container(layers)
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .clip(true)
        .style(move |_| card_style(&tokens, background, presence))
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
            widgets::spinner::spinner(&tokens, toast.spinner_elapsed(now), ICON_SIZE)
        }
        Outcome::Finished { .. } => icon(assets::icons::check(), color),
        Outcome::Error { .. } => icon(assets::icons::circle_alert(), color),
    }
}

fn icon<'a>(handle: svg::Handle, color: Color) -> Element<'a, Message> {
    svg(handle)
        .width(Length::Fixed(ICON_SIZE))
        .height(Length::Fixed(ICON_SIZE))
        .style(move |_, _| svg::Style { color: Some(color) })
        .into()
}

fn toast_label(toast: &Toast, i18n: &I18n) -> String {
    match toast.outcome() {
        Outcome::Error { error, .. } => translated_error(i18n, error),
        Outcome::Finished { .. } => match toast.kind() {
            // Notices carry their message in the status key and finish at
            // birth, so the label stays the message rather than "Done".
            TaskKind::Notice => i18n.tr(&toast.status().key),
            _ => i18n.tr("downloader-status-finished"),
        },
        Outcome::Pending => status_text(toast, i18n),
    }
}

fn status_text(toast: &Toast, i18n: &I18n) -> String {
    let key = toast.status().key.as_str();
    match key {
        // Byte-progress statuses share the downloader's formatting; the
        // publish keys keep their upstream wire names but map onto the
        // existing translated entries.
        "PUBLISH_PACKING" | "extracting_progress" => {
            let ftl_key = if key == "PUBLISH_PACKING" {
                "publish-packing"
            } else {
                key
            };
            let total = format_bytes(toast.total_bytes(), i18n);
            let done = format_bytes((toast.total_bytes() as f64 * toast.progress()) as u64, i18n);
            i18n.trn(
                ftl_key,
                &[
                    ("arg0", &format!("{:.0}", toast.progress() * 100.0)),
                    ("arg1", done.as_str()),
                    ("arg2", total.as_str()),
                ],
            )
        }
        "PUBLISH_PROCESSING_ICON" => i18n.tr("publish-processing-icon"),
        key => i18n.tr(key),
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
