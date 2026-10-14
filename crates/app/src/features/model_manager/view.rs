use iced::widget::{button, column, container, opaque, responsive, row, scrollable, text};
use iced::{Element, Length};

use super::{Message, State, WorkspaceTab};
use crate::assets;
use crate::features::file_preview;
use crate::theme::{self, ViewCtx};
use crate::widgets::icon::svg_icon;
use crate::widgets::{select, tooltip};

mod animations;
mod appearance;
mod environment;
mod panels;
mod status;
mod title;

#[cfg(test)]
use animations::animations_card;
use appearance::appearance_panel;
use panels::{export_panel, import_panel, model_panel, physics_panel};
use status::pane_footer;
#[cfg(test)]
use status::project_actions;

#[cfg(test)]
pub fn view<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    overlay(state, None, ctx)
}

pub fn workspace_view<'a>(
    state: &'a State,
    preview: &'a file_preview::State,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    overlay(state, Some(preview), ctx)
}

pub fn pane_reservation(state: &State, ctx: ViewCtx<'_>, width: f32, height: f32) -> (f32, f32) {
    if !state.pane_open {
        return (0.0, 0.0);
    }
    let inset = ctx.tokens.spacing.pad_sm;
    if width / ctx.tokens.text_scale() < 700.0 {
        (
            0.0,
            pane_height(height, inset, ctx.tokens.text_size(260.0)) + inset * 2.0,
        )
    } else {
        (
            pane_width(ctx).min((width - inset * 2.0).max(0.0)) + inset * 2.0,
            0.0,
        )
    }
}

fn pane_width(ctx: ViewCtx<'_>) -> f32 {
    ctx.tokens.text_size(420.0)
}

fn pane_height(height: f32, inset: f32, minimum: f32) -> f32 {
    (height * 0.48)
        .max(minimum)
        .min((height - inset * 2.0).max(0.0))
}

fn overlay<'a>(
    state: &'a State,
    preview: Option<&'a file_preview::State>,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    responsive(move |size| {
        if !state.pane_open {
            return container(icon_button(
                assets::icons::gear(),
                "models-pane-open",
                Message::PaneToggled,
                ctx,
            ))
            .padding(tokens.spacing.pad_sm)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Right)
            .align_y(iced::Top)
            .into();
        }
        let inset = tokens.spacing.pad_sm;
        let compact = size.width / tokens.text_scale() < 700.0;
        let panel = container(pane(state, preview, ctx, compact))
            .width(if compact {
                Length::Fill
            } else {
                Length::Fixed(pane_width(ctx))
            })
            .height(if compact {
                Length::Fixed(pane_height(size.height, inset, tokens.text_size(260.0)))
            } else {
                Length::Fill
            })
            .style(move |_| theme::styles::modal(&tokens));
        container(opaque(panel))
            .padding(inset)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Right)
            .align_y(if compact { iced::Bottom } else { iced::Top })
            .into()
    })
    .into()
}

fn pane<'a>(
    state: &'a State,
    preview: Option<&'a file_preview::State>,
    ctx: ViewCtx<'a>,
    compact: bool,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let title = state
        .source
        .as_ref()
        .and_then(|path| path.file_name())
        .map_or_else(
            || i18n.tr("models"),
            |name| name.to_string_lossy().into_owned(),
        );
    let title_widget = title::view(title, ctx);
    let header = row![
        title_widget,
        button(text(i18n.tr("models-import-action")).size(tokens.typography.body_sm))
            .on_press_maybe(
                (!state.busy()).then_some(Message::WorkspaceTabSelected(WorkspaceTab::Import))
            )
            .padding([tokens.spacing.pad_control_y, tokens.spacing.gap_sm])
            .style(move |_, status| theme::styles::ghost_button(&tokens, status)),
        icon_button(
            assets::icons::cross(),
            "models-pane-close",
            Message::PaneToggled,
            ctx
        ),
    ]
    .spacing(tokens.spacing.gap_xs)
    .align_y(iced::Center);
    let mut tabs = row![].spacing(tokens.spacing.gap_xs);
    for (tab, key) in [
        (WorkspaceTab::Model, "models-tab-model"),
        (WorkspaceTab::Appearance, "models-tab-appearance"),
        (WorkspaceTab::Physics, "models-tab-physics"),
        (WorkspaceTab::Export, "models-tab-export"),
    ] {
        let selected = state.workspace_tab == tab;
        tabs = tabs.push(
            button(
                text(i18n.tr(key))
                    .size(tokens.typography.body_sm)
                    .width(Length::Fill)
                    .wrapping(text::Wrapping::WordOrGlyph)
                    .align_x(iced::Center),
            )
            .on_press(Message::WorkspaceTabSelected(tab))
            .width(Length::Fill)
            .padding([tokens.spacing.pad_control_y, tokens.spacing.gap_xs])
            .style(move |_, status| {
                if selected {
                    theme::styles::button(&tokens, status)
                } else {
                    theme::styles::ghost_button(&tokens, status)
                }
            }),
        );
    }
    let body = if (state.source.is_none() && state.workspace_tab != WorkspaceTab::Export)
        || state.workspace_tab == WorkspaceTab::Import
    {
        import_panel(state, ctx)
    } else {
        match state.workspace_tab {
            WorkspaceTab::Model => model_panel(state, preview, ctx),
            WorkspaceTab::Appearance => appearance_panel(state, preview, ctx),
            WorkspaceTab::Physics => physics_panel(state, ctx),
            WorkspaceTab::Export => export_panel(state, ctx),
            WorkspaceTab::Import => import_panel(state, ctx),
        }
    };
    let mut content = column![].spacing(tokens.spacing.gap_lg);
    if let Some(error) = &state.error {
        content = content.push(
            column![
                text(error)
                    .size(tokens.typography.body_sm)
                    .color(tokens.colors.error),
                copy_button(ctx),
            ]
            .spacing(tokens.spacing.gap),
        );
    }
    content = content.push(body);
    let mut layout = column![
        header,
        tabs,
        scrollable(
            container(content)
                .width(Length::Fill)
                .padding(iced::Padding::ZERO.right(tokens.spacing.gap_sm))
        )
        .height(Length::Fill)
        .style(move |_, status| theme::styles::scrollbar(&tokens, status)),
    ]
    .spacing(if compact {
        tokens.spacing.gap_sm
    } else {
        tokens.spacing.gap_lg
    });
    if let Some(footer) = pane_footer(state, ctx) {
        layout = layout.push(footer);
    }
    container(layout)
        .padding(if compact {
            tokens.spacing.pad_sm
        } else {
            tokens.spacing.pad
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn icon_button<'a>(
    handle: iced::widget::svg::Handle,
    key: &str,
    message: Message,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    tooltip::below(
        button(svg_icon(
            handle,
            tokens.colors.text.into(),
            tokens.icon_size(16.0),
        ))
        .on_press(message)
        .padding(tokens.spacing.pad_control)
        .style(move |_, status| theme::styles::ghost_button(&tokens, status)),
        ctx.i18n.tr(key),
        &tokens,
        tokens.text_size(220.0),
    )
}

fn choice_field<'a, T: Clone + PartialEq + std::fmt::Display + 'a>(
    choices: Vec<T>,
    selected: Option<&T>,
    busy: bool,
    on_selected: impl Fn(T) -> Message + 'a,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let label = selected.map_or_else(String::new, ToString::to_string);
    if busy {
        return text(label)
            .size(ctx.tokens.typography.body)
            .width(Length::Fill)
            .into();
    }
    select::field(
        label,
        choices
            .into_iter()
            .map(|choice| select::Row {
                selected: selected == Some(&choice),
                label: choice.to_string(),
                on_select: on_selected(choice),
            })
            .collect(),
        select::Metrics {
            text_size: ctx.tokens.typography.body,
            padding: iced::Padding::from(ctx.tokens.spacing.pad_control),
            radius: ctx.tokens.radii.sm,
            max_rows: select::MAX_ROWS,
        },
        select::Chevron::Shown,
        ctx.tokens,
    )
}

fn copy_button<'a>(ctx: ViewCtx<'_>) -> iced::widget::Button<'a, Message> {
    let tokens = *ctx.tokens;
    button(text(ctx.i18n.tr("models-copy-diagnostics")))
        .on_press(Message::CopyDiagnostics)
        .padding(tokens.spacing.pad_control)
        .style(move |_, status| theme::styles::button(&tokens, status))
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod view_tests;
