use iced::widget::{button, column, row, text};
use iced::{Element, Length};

use super::super::{ExportResult, ExportTarget, Message, SourceKind, State, WorkspaceTab};
use super::copy_button;
use crate::format::format_bytes;
use crate::theme::{self, ViewCtx};

pub(super) fn pane_footer<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Option<Element<'a, Message>> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let compiled = state.kind() == Some(SourceKind::Compiled);
    let mut body = column![].spacing(tokens.spacing.gap).width(Length::Fill);
    if let Some(running) = &state.running {
        body = body.push(
            row![
                text(i18n.tr(if running.cancellation_requested {
                    "models-cancelling"
                } else if running.target == ExportTarget::Blender {
                    "models-exporting-blender"
                } else if compiled {
                    "models-decompiling"
                } else {
                    "models-compiling"
                }))
                .size(tokens.typography.body_sm)
                .width(Length::Fill),
                button(text(i18n.tr("cancel")))
                    .on_press_maybe(
                        (!running.cancellation_requested).then_some(Message::CancelRequested)
                    )
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::button(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap)
            .align_y(iced::Center),
        );
    }
    if state.picker.is_some() || state.preview_busy() {
        body = body.push(text(i18n.tr("models-working")).size(tokens.typography.body_sm));
    }
    if state.workspace_tab != WorkspaceTab::Export {
        if state.install_generation.is_some() {
            body = body.push(text(i18n.tr("models-installing")).size(tokens.typography.body_sm));
        }
        if state.remove_installed_generation.is_some() {
            body = body
                .push(text(i18n.tr("models-installed-removing")).size(tokens.typography.body_sm));
        }
    }
    if state.cancelled {
        body = body.push(
            text(i18n.tr("models-cancelled"))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.text_dim),
        );
    }
    if state.source.is_some()
        && state.workspace_tab == WorkspaceTab::Export
        && state.running.is_none()
    {
        if compiled {
            body = body.push(project_actions(state, ctx));
        }
        body = body.push(
            button(text(i18n.tr(if compiled {
                "models-export-blender"
            } else {
                "models-compile"
            })))
            .on_press_maybe(state.can_export().then_some(if compiled {
                Message::BlenderExportRequested
            } else {
                Message::ExportRequested
            }))
            .width(Length::Fill)
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::action_button(&tokens, status)),
        );
    }
    let visible = state.running.is_some()
        || state.picker.is_some()
        || state.preview_busy()
        || state.install_generation.is_some()
        || state.remove_installed_generation.is_some()
        || state.cancelled
        || (state.source.is_some() && state.workspace_tab == WorkspaceTab::Export);
    visible.then(|| body.into())
}

pub(super) fn project_actions<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let compiled = state.kind() == Some(SourceKind::Compiled);
    let mut body = column![
        button(text(ctx.i18n.tr(if compiled {
            "models-decompile"
        } else {
            "models-save-qc"
        })))
        .on_press_maybe(if compiled {
            state.can_export().then_some(Message::ExportRequested)
        } else {
            state.can_save_qc().then_some(Message::SaveQcRequested)
        })
        .padding(tokens.spacing.pad_control)
        .style(move |_, status| theme::styles::button(&tokens, status))
    ]
    .spacing(tokens.spacing.gap)
    .width(Length::Fill);
    if state.qc_saved {
        if let Some(path) = &state.qc_path {
            body = body.push(
                button(text(ctx.i18n.tr("models-qc-editor")))
                    .on_press_maybe((!state.busy()).then(|| Message::SourceDropped(path.clone())))
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::button(&tokens, status)),
            );
        }
        body = body.push(
            text(ctx.i18n.tr("models-qc-saved"))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.success),
        );
    }
    body.into()
}

pub(super) fn export_result<'a>(
    state: &'a State,
    result: &'a ExportResult,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mut output = column![
        row![
            text(i18n.tr("models-exported"))
                .size(tokens.typography.body_lg)
                .color(tokens.colors.success)
                .width(Length::Fill),
            text(format_bytes(result.bytes, i18n)).size(tokens.typography.body_sm),
        ]
        .align_y(iced::Center),
        button(text(i18n.tr("models-open-output")))
            .on_press(Message::RevealRequested(result.directory.clone()))
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
    ]
    .spacing(tokens.spacing.gap);
    if !result.compiled
        && let Some(source) = result.files.iter().find(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("qc"))
        })
    {
        output = output.push(
            button(text(i18n.tr("models-edit-exported")))
                .on_press_maybe((!state.busy()).then(|| Message::SourceDropped(source.clone())))
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
        );
    }
    if result.blender_model().is_some() {
        output = output.push(
            button(text(i18n.tr("models-open-blender")))
                .on_press_maybe((!state.busy()).then_some(Message::OpenBlenderRequested))
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
        );
        if let Some(error) = &state.blender_launch_error {
            output = output
                .push(
                    text(error)
                        .size(tokens.typography.body_sm)
                        .color(tokens.colors.error),
                )
                .push(copy_button(ctx));
        }
    }
    for path in &result.files {
        let label = path
            .strip_prefix(&result.directory)
            .unwrap_or(path)
            .display()
            .to_string();
        output = output.push(
            button(
                text(label)
                    .size(tokens.typography.body_sm)
                    .wrapping(text::Wrapping::WordOrGlyph),
            )
            .on_press(Message::RevealRequested(path.clone()))
            .style(move |_, status| theme::styles::ghost_button(&tokens, status)),
        );
    }
    if !result.warnings.is_empty() {
        output = output.push(warnings(&result.warnings, ctx));
    }
    output.into()
}

pub(super) fn warnings<'a>(items: &'a [String], ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let mut body = column![
        text(ctx.i18n.tr("models-warnings"))
            .size(ctx.tokens.typography.body)
            .color(ctx.tokens.colors.warn)
    ]
    .spacing(ctx.tokens.spacing.gap);
    for warning in items {
        body = body.push(text(warning).size(ctx.tokens.typography.body_sm));
    }
    body.push(copy_button(ctx)).into()
}
