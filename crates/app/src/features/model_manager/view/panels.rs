use iced::widget::{button, checkbox, column, row, text, text_input};
use iced::{Element, Length};

use super::super::{Message, SourceKind, State, WorkspaceTab};
use super::animations::animations_card;
use super::choice_field;
use super::environment::stage_card;
use super::status::{export_result, project_actions, warnings};
use crate::assets;
use crate::features::file_preview;
use crate::format::format_bytes;
use crate::i18n::Arg;
use crate::theme::{self, ViewCtx};
use crate::widgets::icon::svg_icon;

pub(super) fn import_panel<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    column![
        text(ctx.i18n.tr("models-import-title")).size(tokens.typography.body_lg),
        button(
            row![
                svg_icon(
                    assets::icons::folder(),
                    tokens.colors.text.into(),
                    tokens.icon_size(20.0)
                ),
                text(ctx.i18n.tr("browse")),
            ]
            .spacing(tokens.spacing.gap)
            .align_y(iced::Center)
        )
        .on_press_maybe((!state.busy()).then_some(Message::ImportRequested))
        .padding(tokens.spacing.pad)
        .width(Length::Fill)
        .style(move |_, status| theme::styles::button(&tokens, status)),
        text(ctx.i18n.tr("models-import-hint"))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
        text(ctx.i18n.tr("models-formats"))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
    ]
    .spacing(tokens.spacing.gap_lg)
    .into()
}

pub(super) fn model_panel<'a>(
    state: &'a State,
    preview: Option<&'a file_preview::State>,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let busy = state.busy();
    let mesh = state.kind() == Some(SourceKind::Mesh);
    let mut body = column![].spacing(tokens.spacing.gap_lg);
    if preview.is_some_and(|preview| preview.stage_model().is_some()) {
        body = body.push(
            button(text(i18n.tr("models-preview")))
                .on_press_maybe(
                    state
                        .preview_request()
                        .is_some()
                        .then_some(Message::PreviewRequested),
                )
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
        );
    }
    if state.kind() != Some(SourceKind::Compiled) {
        body = body.push(
            column![
                text(i18n.tr("models-name")).size(tokens.typography.body_sm),
                text_input(&i18n.tr("models-name-placeholder"), &state.model_name)
                    .on_input_maybe((!busy).then_some(Message::ModelNameChanged))
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::input(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap_sm),
        );
    }
    if mesh {
        body = body.push(
            column![
                text(i18n.tr("models-model-type")).size(tokens.typography.body_sm),
                model_type(state, ctx),
            ]
            .spacing(tokens.spacing.gap_sm),
        );
        body = body.push(
            column![
                text(i18n.tr("models-scale")).size(tokens.typography.body_sm),
                text_input("1", &state.scale)
                    .on_input_maybe((!busy).then_some(Message::ScaleChanged))
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::input(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap_sm),
        );
        if state.parsed_scale().is_none() {
            body = body.push(
                text(i18n.tr("models-invalid-scale"))
                    .size(tokens.typography.body_sm)
                    .color(tokens.colors.error),
            );
        }
        if supports_automatic_axes(state) {
            body = body.push(
                checkbox(state.automatic_axes)
                    .label(i18n.tr("models-automatic-axes"))
                    .on_toggle_maybe((!busy).then_some(Message::AutomaticAxesChanged))
                    .style(move |_, status| theme::styles::checkbox(&tokens, status)),
            );
        }
    }
    if let Some(preview) = preview {
        body = body
            .push(file_preview::workspace_animation_controls(preview, ctx).map(Message::Viewer));
        body = body.push(file_preview::workspace_stage_controls(preview, ctx).map(Message::Viewer));
    }
    body = body.push(
        button(
            row![
                svg_icon(
                    if state.advanced_open {
                        assets::icons::chevron_up()
                    } else {
                        assets::icons::chevron_down()
                    },
                    tokens.colors.text.into(),
                    tokens.icon_size(14.0)
                ),
                text(i18n.tr("models-advanced")).size(tokens.typography.body),
            ]
            .spacing(tokens.spacing.gap)
            .width(Length::Fill)
            .align_y(iced::Center),
        )
        .on_press(Message::AdvancedToggled)
        .padding(tokens.spacing.pad_control)
        .width(Length::Fill)
        .style(move |_, status| theme::styles::ghost_button(&tokens, status)),
    );
    if state.advanced_open {
        if let Some(source) = &state.source {
            body = body.push(
                column![
                    text(i18n.tr("models-source"))
                        .size(tokens.typography.body_sm)
                        .color(tokens.colors.text_dim),
                    text(source.display().to_string())
                        .size(tokens.typography.body_sm)
                        .wrapping(text::Wrapping::WordOrGlyph),
                ]
                .spacing(tokens.spacing.gap_xs),
            );
        }

        if state.is_qc() {
            body = body.push(super::super::qc_editor::view(state, ctx));
        }
        if mesh {
            let mut angles = row![].spacing(tokens.spacing.gap);
            for (axis, label) in ["X", "Y", "Z"].into_iter().enumerate() {
                angles =
                    angles.push(
                        column![
                            text(label).size(tokens.typography.body_sm),
                            text_input("0", &state.orientation[axis])
                                .on_input_maybe((!busy && state.static_prop).then_some(
                                    move |value| Message::OrientationChanged(axis, value)
                                ))
                                .padding(tokens.spacing.pad_control)
                                .style(move |_, status| theme::styles::input(&tokens, status)),
                        ]
                        .spacing(tokens.spacing.gap_xs)
                        .width(Length::Fill),
                    );
            }
            body = body.push(
                column![
                    text(i18n.tr("models-orientation")).size(tokens.typography.body_sm),
                    angles
                ]
                .spacing(tokens.spacing.gap_sm),
            );
            if !state.static_prop {
                body = body.push(
                    text(i18n.tr("models-orientation-hint"))
                        .size(tokens.typography.body_sm)
                        .color(tokens.colors.text_dim),
                );
            }
            if state
                .inspection
                .as_ref()
                .is_some_and(|inspection| !inspection.parts.is_empty())
            {
                body = body.push(super::super::bodygroups::view(state, ctx));
            }
            if !state.static_prop && !state.animations.is_empty() {
                body = body.push(animations_card(state, ctx));
            }
            body = body.push(super::super::lods::view(state, ctx));
        }
        body = body.push(project_actions(state, ctx));
    }
    body = body.push(stage_card(state, ctx));
    body.into()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelTypeChoice {
    static_prop: bool,
    label: String,
}

impl std::fmt::Display for ModelTypeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.label.fmt(f)
    }
}

fn model_type<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let choices = vec![
        ModelTypeChoice {
            static_prop: false,
            label: ctx.i18n.tr(
                if state
                    .animations
                    .iter()
                    .any(|animation| animation.frame_count > 1)
                {
                    "models-type-animated"
                } else {
                    "models-type-physics"
                },
            ),
        },
        ModelTypeChoice {
            static_prop: true,
            label: ctx.i18n.tr("models-type-static"),
        },
    ];
    let selected = choices
        .iter()
        .find(|choice| choice.static_prop == state.static_prop)
        .cloned();
    choice_field(
        choices,
        selected.as_ref(),
        state.busy(),
        |choice| Message::StaticPropChanged(choice.static_prop),
        ctx,
    )
}

fn supports_automatic_axes(state: &State) -> bool {
    state
        .source
        .iter()
        .chain(state.lods.iter().map(|lod| &lod.source))
        .any(|source| {
            source.extension().is_some_and(|extension| {
                ["dmx", "fbx", "gltf", "glb"]
                    .iter()
                    .any(|format| extension.eq_ignore_ascii_case(format))
            })
        })
}

pub(super) fn physics_panel<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    if state.kind() == Some(SourceKind::Compiled) {
        return column![
            text(i18n.tr("models-blender-export-hint"))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.text_dim),
            button(text(i18n.tr("models-export-blender")))
                .on_press(Message::WorkspaceTabSelected(WorkspaceTab::Export))
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
        ]
        .spacing(tokens.spacing.gap_lg)
        .into();
    }
    let mut body =
        column![super::super::collision::view(state, ctx)].spacing(tokens.spacing.gap_lg);
    if state.kind() == Some(SourceKind::Mesh) {
        body = body.push(
            column![
                text(i18n.tr("models-surface-property")).size(tokens.typography.body_sm),
                text_input("default", &state.surface_property)
                    .on_input_maybe((!state.busy()).then_some(Message::SurfacePropertyChanged))
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::input(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap_sm),
        );
        if !state.surface_property_valid() {
            body = body.push(
                text(i18n.tr("models-invalid-surface-property"))
                    .size(tokens.typography.body_sm)
                    .color(tokens.colors.error),
            );
        }
    }
    body.into()
}

pub(super) fn export_panel<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    if state.source.is_none() {
        return installed_models_panel(state, ctx);
    }
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let busy = state.busy();
    let compiled = state.kind() == Some(SourceKind::Compiled);
    let mut body = column![
        install_controls(state, ctx),
        text(i18n.tr("models-destination")).size(tokens.typography.body_sm),
        text(state.destination.as_ref().map_or_else(
            || i18n.tr("models-choose-destination"),
            |path| path.display().to_string()
        ))
        .size(tokens.typography.body_sm)
        .wrapping(text::Wrapping::WordOrGlyph),
        button(text(i18n.tr("browse")))
            .on_press_maybe((!busy).then_some(Message::DestinationRequested))
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
    ]
    .spacing(tokens.spacing.gap);
    if !compiled {
        body = body.push(
            column![
                text(i18n.tr("models-material-directory")).size(tokens.typography.body_sm),
                text_input(
                    &i18n.tr("models-material-placeholder"),
                    &state.material_directory
                )
                .on_input_maybe((!busy).then_some(Message::MaterialDirectoryChanged))
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::input(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap),
        );
        body = body.push(
            checkbox(state.export_materials)
                .label(i18n.tr("models-export-materials"))
                .on_toggle_maybe((!busy).then_some(Message::ExportMaterialsChanged))
                .style(move |_, status| theme::styles::checkbox(&tokens, status)),
        );
    }
    body = body.push(
        checkbox(state.overwrite)
            .label(i18n.tr("models-overwrite"))
            .on_toggle_maybe((!busy).then_some(Message::OverwriteChanged))
            .style(move |_, status| theme::styles::checkbox(&tokens, status)),
    );
    if !compiled {
        body = body.push(project_actions(state, ctx));
    }
    if let Some(result) = &state.result {
        body = body.push(export_result(state, result, ctx));
    }
    if let Some(preview) = &state.preview
        && state.result.is_none()
        && !preview.result.warnings.is_empty()
    {
        body = body.push(warnings(&preview.result.warnings, ctx));
    }
    body = body.push(installed_models_panel(state, ctx));
    body.into()
}

fn install_controls<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mut body = column![
        button(text(i18n.tr(if state.install_busy() {
            "models-installing"
        } else {
            "models-install-game"
        })))
        .on_press_maybe(
            (!state.busy()
                && !state.installed_mutation_busy()
                && state.preview_request().is_some())
            .then_some(Message::InstallRequested)
        )
        .padding(tokens.spacing.pad_control)
        .width(Length::Fill)
        .style(move |_, status| theme::styles::button(&tokens, status)),
    ]
    .spacing(tokens.spacing.gap);
    if state.install_confirmation {
        body = body
            .push(text(i18n.tr("models-install-confirm")).size(tokens.typography.body_sm))
            .push(
                row![
                    button(text(i18n.tr("models-install")))
                        .on_press(Message::InstallConfirmed)
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::action_button(&tokens, status)),
                    button(text(i18n.tr("cancel")))
                        .on_press(Message::InstallDismissed)
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::button(&tokens, status)),
                ]
                .spacing(tokens.spacing.gap)
                .wrap(),
            );
    }
    if let Some(model) = &state.install_result {
        body = body.push(
            text(i18n.trn("models-installed", &[("name", Arg::Text(&model.name))]))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.success),
        );
    }
    if let Some(error) = &state.install_error {
        body = body.push(
            text(error)
                .size(tokens.typography.body_sm)
                .color(tokens.colors.error),
        );
    }
    body.into()
}

fn installed_models_panel<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mutation_busy = state.installed_mutation_busy();
    let mut body = column![
        row![
            text(i18n.tr("models-installed-title"))
                .size(tokens.typography.body_lg)
                .width(Length::Fill),
            button(text(i18n.tr("models-installed-refresh")))
                .on_press_maybe(
                    (!mutation_busy && state.installed_models_generation.is_none())
                        .then_some(Message::InstalledModelsRequested)
                )
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
        ]
        .spacing(tokens.spacing.gap)
        .align_y(iced::Center),
    ]
    .spacing(tokens.spacing.gap_lg);
    if state.installed_models.is_empty() {
        body = body.push(
            text(i18n.tr(if state.installed_models_generation.is_some() {
                "models-installed-loading"
            } else {
                "models-installed-empty"
            }))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
        );
    }
    if let Some(error) = &state.installed_models_error {
        body = body.push(
            text(error)
                .size(tokens.typography.body_sm)
                .color(tokens.colors.error),
        );
    }
    for model in &state.installed_models {
        let removing = state
            .remove_installed_generation
            .as_ref()
            .is_some_and(|(_, path)| path == &model.directory);
        let mut item = column![
            text(&model.name)
                .size(tokens.typography.body_sm)
                .wrapping(text::Wrapping::WordOrGlyph),
            text(&model.model)
                .size(tokens.typography.body_sm)
                .wrapping(text::Wrapping::WordOrGlyph)
                .color(tokens.colors.text_dim),
            text(format_bytes(model.bytes, i18n))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.text_dim),
            row![
                button(text(i18n.tr("models-installed-open")))
                    .on_press(Message::RevealRequested(model.directory.clone()))
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::button(&tokens, status)),
                button(text(i18n.tr(if removing {
                    "models-installed-removing"
                } else {
                    "models-installed-remove"
                })))
                .on_press_maybe(
                    (!mutation_busy)
                        .then(|| Message::RemoveInstalledRequested(model.directory.clone()))
                )
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap)
            .wrap(),
        ]
        .spacing(tokens.spacing.gap_sm);
        if state.remove_installed_confirmation.as_ref() == Some(&model.directory) {
            item = item.push(
                text(i18n.trn(
                    "models-installed-remove-confirm",
                    &[("name", Arg::Text(&model.name))],
                ))
                .size(tokens.typography.body_sm),
            );
            if model.modified {
                item = item.push(
                    text(i18n.tr("models-installed-modified"))
                        .size(tokens.typography.body_sm)
                        .color(tokens.colors.error),
                );
            }
            item = item.push(
                row![
                    button(text(i18n.tr("models-installed-remove")))
                        .on_press_maybe(
                            (!mutation_busy).then_some(Message::RemoveInstalledConfirmed)
                        )
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::action_button(&tokens, status)),
                    button(text(i18n.tr("cancel")))
                        .on_press(Message::RemoveInstalledDismissed)
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::button(&tokens, status)),
                ]
                .spacing(tokens.spacing.gap)
                .wrap(),
            );
        }
        body = body.push(item);
    }
    body.into()
}
