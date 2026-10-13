use iced::widget::{Space, button, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Element, Length};

use super::update::parse_color;
use super::{Message, SourceKind, State};
use crate::assets;
use crate::format::format_bytes;
use crate::i18n::Arg;
use crate::theme::{self, ViewCtx};
use crate::widgets::icon::svg_icon;
use crate::widgets::select;

pub fn view<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let busy = state.busy();
    let compiled = state.kind() == Some(SourceKind::Compiled);
    let heading = row![
        svg_icon(
            assets::icons::route_models(),
            tokens.colors.text.into(),
            28.0
        ),
        text(i18n.tr("models")).size(tokens.typography.title),
        Space::new().width(Length::Fill),
        button(text(i18n.tr("models-import")))
            .on_press_maybe((!busy).then_some(Message::ImportRequested))
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
    ]
    .spacing(tokens.spacing.gap_lg)
    .align_y(iced::Center);

    let source_label = state.source.as_ref().map_or_else(
        || i18n.tr("models-import-hint"),
        |path| path.display().to_string(),
    );
    let source = column![
        text(i18n.tr("models-source")).size(tokens.typography.body_lg),
        text(source_label)
            .size(tokens.typography.body)
            .color(tokens.colors.text),
        text(i18n.tr(if compiled {
            "models-decompile-hint"
        } else {
            "models-formats"
        }))
        .size(tokens.typography.body_sm)
        .color(tokens.colors.text_dim),
    ]
    .spacing(tokens.spacing.gap);

    let destination = row![
        column![
            text(i18n.tr("models-destination")).size(tokens.typography.body_lg),
            text(state.destination.as_ref().map_or_else(
                || i18n.tr("models-choose-destination"),
                |path| path.display().to_string(),
            ))
            .size(tokens.typography.body)
            .color(tokens.colors.text_dim),
        ]
        .spacing(tokens.spacing.gap)
        .width(Length::Fill),
        button(text(i18n.tr("browse")))
            .on_press_maybe((!busy).then_some(Message::DestinationRequested))
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
    ]
    .spacing(tokens.spacing.gap_lg)
    .align_y(iced::Center);

    let mut form = column![card(source.into(), ctx), card(destination.into(), ctx)]
        .spacing(tokens.spacing.gap_lg);
    if state.is_qc() {
        form = form.push(card(super::qc_editor::view(state, ctx), ctx));
    }
    if state.source.is_some() && !compiled {
        let mut options = column![
            text(i18n.tr("models-options")).size(tokens.typography.body_lg),
            text(i18n.tr("models-name")).size(tokens.typography.body_sm),
            text_input(&i18n.tr("models-name-placeholder"), &state.model_name)
                .on_input_maybe((!busy).then_some(Message::ModelNameChanged))
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::input(&tokens, status)),
            text(i18n.tr("models-material-directory")).size(tokens.typography.body_sm),
            text_input(
                &i18n.tr("models-material-placeholder"),
                &state.material_directory
            )
            .on_input_maybe((!busy).then_some(Message::MaterialDirectoryChanged))
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::input(&tokens, status)),
        ]
        .spacing(tokens.spacing.gap);
        if state.kind() == Some(SourceKind::Mesh) {
            options = options.push(
                row![
                    column![
                        text(i18n.tr("models-scale")).size(tokens.typography.body_sm),
                        text_input("1", &state.scale)
                            .on_input_maybe((!busy).then_some(Message::ScaleChanged))
                            .padding(tokens.spacing.pad_control)
                            .width(Length::Fixed(120.0))
                            .style(move |_, status| theme::styles::input(&tokens, status)),
                    ]
                    .spacing(tokens.spacing.gap),
                    checkbox(state.static_prop)
                        .label(i18n.tr("models-static-prop"))
                        .on_toggle_maybe((!busy).then_some(Message::StaticPropChanged))
                        .style(move |_, status| theme::styles::checkbox(&tokens, status)),
                ]
                .spacing(tokens.spacing.gap_lg)
                .align_y(iced::Center),
            );
            if state.parsed_scale().is_none() {
                options = options.push(
                    text(i18n.tr("models-invalid-scale"))
                        .size(tokens.typography.body_sm)
                        .color(tokens.colors.error),
                );
            }
            options = options
                .push(
                    checkbox(state.generate_collision)
                        .label(i18n.tr("models-generate-collision"))
                        .on_toggle_maybe((!busy).then_some(Message::GenerateCollisionChanged))
                        .style(move |_, status| theme::styles::checkbox(&tokens, status)),
                )
                .push(
                    text(i18n.tr("models-generate-collision-hint"))
                        .size(tokens.typography.body_sm)
                        .color(tokens.colors.text_dim),
                );
            if state.generate_collision {
                options = options
                    .push(text(i18n.tr("models-collision-mass")).size(tokens.typography.body_sm))
                    .push(
                        text_input("1", &state.collision_mass)
                            .on_input_maybe((!busy).then_some(Message::CollisionMassChanged))
                            .padding(tokens.spacing.pad_control)
                            .width(Length::Fixed(120.0))
                            .style(move |_, status| theme::styles::input(&tokens, status)),
                    );
                if state.parsed_collision_mass().is_none() {
                    options = options.push(
                        text(i18n.tr("models-invalid-mass"))
                            .size(tokens.typography.body_sm)
                            .color(tokens.colors.error),
                    );
                }
            }
            options = options
                .push(text(i18n.tr("models-surface-property")).size(tokens.typography.body_sm))
                .push(
                    text_input("default", &state.surface_property)
                        .on_input_maybe((!busy).then_some(Message::SurfacePropertyChanged))
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::input(&tokens, status)),
                )
                .push(
                    text(i18n.tr("models-surface-property-hint"))
                        .size(tokens.typography.body_sm)
                        .color(tokens.colors.text_dim),
                );
            if !state.surface_property_valid() {
                options = options.push(
                    text(i18n.tr("models-invalid-surface-property"))
                        .size(tokens.typography.body_sm)
                        .color(tokens.colors.error),
                );
            }
            if state
                .source
                .iter()
                .chain(state.lods.iter().map(|lod| &lod.source))
                .any(|source| {
                    source.extension().is_some_and(|extension| {
                        ["fbx", "gltf", "glb"]
                            .iter()
                            .any(|format| extension.eq_ignore_ascii_case(format))
                    })
                })
            {
                options = options
                    .push(
                        checkbox(state.automatic_axes)
                            .label(i18n.tr("models-automatic-axes"))
                            .on_toggle_maybe((!busy).then_some(Message::AutomaticAxesChanged))
                            .style(move |_, status| theme::styles::checkbox(&tokens, status)),
                    )
                    .push(
                        text(i18n.tr("models-automatic-axes-hint"))
                            .size(tokens.typography.body_sm)
                            .color(tokens.colors.text_dim),
                    );
            }
            {
                let mut angles = row![].spacing(tokens.spacing.gap);
                for (axis, label) in ["X", "Y", "Z"].into_iter().enumerate() {
                    angles = angles.push(
                        column![
                            text(label).size(tokens.typography.body_sm),
                            text_input("0", &state.orientation[axis])
                                .on_input_maybe((!busy && state.static_prop).then_some(
                                    move |value| Message::OrientationChanged(axis, value)
                                ))
                                .padding(tokens.spacing.pad_control)
                                .style(move |_, status| theme::styles::input(&tokens, status)),
                        ]
                        .spacing(tokens.spacing.gap_xs),
                    );
                }
                options = options
                    .push(text(i18n.tr("models-orientation")).size(tokens.typography.body_sm))
                    .push(angles);
            }
            if !state.static_prop {
                options = options.push(
                    text(i18n.tr("models-orientation-hint"))
                        .size(tokens.typography.body_sm)
                        .color(tokens.colors.text_dim),
                );
            }
        }
        options = options
            .push(
                checkbox(state.export_materials)
                    .label(i18n.tr("models-export-materials"))
                    .on_toggle_maybe((!busy).then_some(Message::ExportMaterialsChanged))
                    .style(move |_, status| theme::styles::checkbox(&tokens, status)),
            )
            .push(
                checkbox(state.overwrite)
                    .label(i18n.tr("models-overwrite"))
                    .on_toggle_maybe((!busy).then_some(Message::OverwriteChanged))
                    .style(move |_, status| theme::styles::checkbox(&tokens, status)),
            );
        form = form.push(card(options.into(), ctx));
    }
    if state.kind() == Some(SourceKind::Mesh)
        && state
            .inspection
            .as_ref()
            .is_some_and(|inspection| !inspection.parts.is_empty())
    {
        form = form.push(card(super::bodygroups::view(state, ctx), ctx));
    }
    if state.kind() == Some(SourceKind::Mesh) {
        form = form.push(card(super::lods::view(state, ctx), ctx));
    }
    if state.kind() == Some(SourceKind::Mesh) && !state.static_prop && !state.animations.is_empty()
    {
        form = form.push(animations_card(state, ctx));
    }
    if state.source.is_some() && !compiled {
        form = form.push(skins_card(state, ctx));
    }
    if state.source.is_some() {
        form = form.push(stage_card(state, ctx));
    }
    let mut actions = row![
        button(text(i18n.tr(if compiled {
            "models-decompile"
        } else {
            "models-compile"
        })))
        .on_press_maybe(state.can_export().then_some(Message::ExportRequested))
        .padding(tokens.spacing.pad_control)
        .style(move |_, status| theme::styles::action_button(&tokens, status)),
    ]
    .spacing(tokens.spacing.gap_lg)
    .align_y(iced::Center);
    if state.running.is_none() {
        actions = actions.push(
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
    if let Some(running) = &state.running {
        actions = row![
            text(i18n.tr(if running.cancellation_requested {
                "models-cancelling"
            } else if compiled {
                "models-decompiling"
            } else {
                "models-compiling"
            }))
            .size(tokens.typography.body_sm)
            .width(Length::Fill),
            button(text(i18n.tr("cancel")))
                .on_press_maybe(
                    (!running.cancellation_requested).then_some(Message::CancelRequested),
                )
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
        ]
        .spacing(tokens.spacing.gap_lg)
        .align_y(iced::Center);
    }
    if state.cancelled {
        form = form.push(
            text(i18n.tr("models-cancelled"))
                .size(tokens.typography.body)
                .color(tokens.colors.text_dim),
        );
    }
    if let Some(error) = &state.error {
        form = form.push(card(
            column![
                text(i18n.tr("models-export-failed"))
                    .size(tokens.typography.body_lg)
                    .color(tokens.colors.error),
                text(error).size(tokens.typography.body_sm),
                copy_button(ctx),
            ]
            .spacing(tokens.spacing.gap)
            .into(),
            ctx,
        ));
    }
    if let Some(result) = &state.result {
        let mut output_actions = row![
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
            output_actions = output_actions.push(
                button(text(i18n.tr("models-edit-exported")))
                    .on_press_maybe((!busy).then(|| Message::SourceDropped(source.clone())))
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::button(&tokens, status)),
            );
        }
        let mut output = column![
            row![
                text(i18n.tr("models-exported"))
                    .size(tokens.typography.body_lg)
                    .color(tokens.colors.success),
                Space::new().width(Length::Fill),
                text(format_bytes(result.bytes, i18n)).size(tokens.typography.body_sm),
            ]
            .align_y(iced::Center),
            output_actions,
        ]
        .spacing(tokens.spacing.gap);
        for path in &result.files {
            let label = path
                .strip_prefix(&result.directory)
                .unwrap_or(path)
                .display()
                .to_string();
            output = output.push(
                button(text(label).size(tokens.typography.body_sm))
                    .on_press(Message::RevealRequested(path.clone()))
                    .style(move |_, status| theme::styles::ghost_button(&tokens, status)),
            );
        }
        if !result.warnings.is_empty() {
            output = output.push(
                text(i18n.tr("models-warnings"))
                    .size(tokens.typography.body_lg)
                    .color(tokens.colors.warn),
            );
            for warning in &result.warnings {
                output = output.push(text(warning).size(tokens.typography.body_sm));
            }
            output = output.push(copy_button(ctx));
        }
        form = form.push(card(output.into(), ctx));
    } else if let Some(preview) = &state.preview
        && !preview.result.warnings.is_empty()
    {
        let mut warnings = column![
            text(i18n.tr("models-warnings"))
                .size(tokens.typography.body_lg)
                .color(tokens.colors.warn),
        ]
        .spacing(tokens.spacing.gap);
        for warning in &preview.result.warnings {
            warnings = warnings.push(text(warning).size(tokens.typography.body_sm));
        }
        warnings = warnings.push(copy_button(ctx));
        form = form.push(card(warnings.into(), ctx));
    }

    let mut project_actions = row![
        button(text(i18n.tr("models-save-qc")))
            .on_press_maybe(
                (!busy && !compiled && state.qc_request().is_some())
                    .then_some(Message::SaveQcRequested)
            )
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
    ]
    .spacing(tokens.spacing.gap)
    .align_y(iced::Center);
    if state.qc_saved {
        if let Some(path) = &state.qc_path {
            project_actions = project_actions.push(
                button(text(i18n.tr("models-qc-editor")))
                    .on_press_maybe((!busy).then(|| Message::SourceDropped(path.clone())))
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::button(&tokens, status)),
            );
        }
        project_actions = project_actions.push(
            text(i18n.tr("models-qc-saved"))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.success),
        );
    }

    container(
        column![
            heading,
            text(i18n.tr("models-description"))
                .size(tokens.typography.body)
                .color(tokens.colors.text_dim),
            scrollable(form.padding(iced::Padding::ZERO.right(tokens.spacing.pad)))
                .height(Length::Fill)
                .style(move |_, status| theme::styles::scrollbar(&tokens, status)),
            actions,
            if state.picker.is_some() {
                Element::from(text(i18n.tr("models-working")).size(tokens.typography.body_sm))
            } else {
                Element::from(Space::new().height(0))
            },
            project_actions,
        ]
        .spacing(tokens.spacing.gap_lg),
    )
    .padding(tokens.spacing.pad)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn animations_card<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let busy = state.busy();
    let mut content = column![
        text(i18n.tr("models-animations")).size(tokens.typography.body_lg),
        text(i18n.tr("models-animations-hint"))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
    ]
    .spacing(tokens.spacing.gap_lg);
    for (index, animation) in state.animations.iter().enumerate() {
        let mut controls = column![
            row![
                checkbox(animation.included)
                    .label(i18n.tr("models-animation-include"))
                    .on_toggle_maybe(
                        (!busy).then_some(move |value| Message::AnimationIncluded(index, value))
                    )
                    .style(move |_, status| theme::styles::checkbox(&tokens, status)),
                text(animation.source.as_deref().unwrap_or("idle"))
                    .width(Length::Fill)
                    .size(tokens.typography.body_sm),
            ]
            .spacing(tokens.spacing.gap_lg)
            .align_y(iced::Center),
            text(i18n.tr("models-animation-name")).size(tokens.typography.body_sm),
            text_input("idle", &animation.name)
                .on_input_maybe(
                    (!busy && animation.included)
                        .then_some(move |value| Message::AnimationNameChanged(index, value))
                )
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::input(&tokens, status)),
            row![
                column![
                    text(i18n.tr("models-animation-fps")).size(tokens.typography.body_sm),
                    text_input("30", &animation.fps)
                        .on_input_maybe(
                            (!busy && animation.included)
                                .then_some(move |value| Message::AnimationFpsChanged(index, value))
                        )
                        .padding(tokens.spacing.pad_control)
                        .width(Length::Fixed(120.0))
                        .style(move |_, status| theme::styles::input(&tokens, status)),
                ]
                .spacing(tokens.spacing.gap_xs),
                checkbox(animation.looping)
                    .label(i18n.tr("models-animation-loop"))
                    .on_toggle_maybe(
                        (!busy && animation.included)
                            .then_some(move |value| Message::AnimationLoopChanged(index, value))
                    )
                    .style(move |_, status| theme::styles::checkbox(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap_lg)
            .align_y(iced::Center),
        ]
        .spacing(tokens.spacing.gap);
        controls = controls.push(
            button(
                row![
                    text(if animation.advanced_expanded {
                        "▾"
                    } else {
                        "▸"
                    }),
                    text(i18n.tr("models-animation-advanced"))
                ]
                .spacing(tokens.spacing.gap_xs),
            )
            .on_press_maybe(
                (!busy && animation.included).then_some(Message::AnimationAdvancedToggled(index)),
            )
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
        );
        if animation.advanced_expanded {
            controls = controls.push(animation_advanced(index, animation, ctx, busy));
        }
        content = content.push(controls);
    }
    if state.parsed_animation_settings().is_none() {
        content = content.push(
            text(i18n.tr("models-animation-invalid"))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.error),
        );
    }
    if state.parsed_animation_settings().is_none()
        && state
            .animations
            .iter()
            .any(|animation| !animation.activity.trim().is_empty() || !animation.events.is_empty())
    {
        content = content.push(
            text(i18n.tr("models-animation-advanced-invalid"))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.error),
        );
    }
    card(content.into(), ctx)
}

fn animation_advanced<'a>(
    index: usize,
    animation: &'a super::state::AnimationEditor,
    ctx: ViewCtx<'a>,
    busy: bool,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let enabled = !busy && animation.included;
    let last = animation.frame_count.saturating_sub(1).to_string();
    let mut controls = column![
        text(i18n.tr("models-animation-activity-hint"))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
        row![
            column![
                text(i18n.tr("models-animation-activity")).size(tokens.typography.body_sm),
                text_input("ACT_VM_IDLE", &animation.activity)
                    .on_input_maybe(
                        enabled.then_some(move |value| Message::AnimationActivityChanged(
                            index, value
                        ))
                    )
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::input(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap_xs)
            .width(Length::Fill),
            column![
                text(i18n.tr("models-animation-activity-weight")).size(tokens.typography.body_sm),
                text_input("1", &animation.activity_weight)
                    .on_input_maybe(
                        (enabled && !animation.activity.trim().is_empty()).then_some(
                            move |value| Message::AnimationActivityWeightChanged(index, value)
                        )
                    )
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::input(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap_xs)
            .width(Length::Fixed(120.0)),
        ]
        .spacing(tokens.spacing.gap),
        text(i18n.tr("models-animation-events")).size(tokens.typography.body),
        text(i18n.trn(
            "models-animation-event-range",
            &[("last", Arg::Number(&last))]
        ))
        .size(tokens.typography.body_sm)
        .color(tokens.colors.text_dim),
    ]
    .spacing(tokens.spacing.gap);
    for (event_index, event) in animation.events.iter().enumerate() {
        controls = controls.push(
            column![
                row![
                    column![
                        text(i18n.tr("models-animation-event-frame"))
                            .size(tokens.typography.body_sm),
                        text_input("0", &event.frame)
                            .on_input_maybe(enabled.then_some(move |value| {
                                Message::AnimationEventFrameChanged(index, event_index, value)
                            }))
                            .padding(tokens.spacing.pad_control)
                            .style(move |_, status| theme::styles::input(&tokens, status)),
                    ]
                    .spacing(tokens.spacing.gap_xs)
                    .width(Length::Fixed(100.0)),
                    column![
                        text(i18n.tr("models-animation-event-code"))
                            .size(tokens.typography.body_sm),
                        text_input("5004 / AE_CL_PLAYSOUND", &event.event)
                            .on_input_maybe(enabled.then_some(move |value| {
                                Message::AnimationEventCodeChanged(index, event_index, value)
                            }))
                            .padding(tokens.spacing.pad_control)
                            .style(move |_, status| theme::styles::input(&tokens, status)),
                    ]
                    .spacing(tokens.spacing.gap_xs)
                    .width(Length::Fill),
                    button(text(i18n.tr("models-animation-event-remove")))
                        .on_press_maybe(
                            enabled.then_some(Message::AnimationEventRemoved(index, event_index))
                        )
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::button(&tokens, status)),
                ]
                .spacing(tokens.spacing.gap)
                .align_y(iced::Bottom),
                text(i18n.tr("models-animation-event-options")).size(tokens.typography.body_sm),
                text_input("", &event.options)
                    .on_input_maybe(enabled.then_some(move |value| {
                        Message::AnimationEventOptionsChanged(index, event_index, value)
                    }))
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::input(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap_xs),
        );
    }
    controls = controls.push(
        button(text(i18n.tr("models-animation-event-add")))
            .on_press_maybe(
                (enabled && animation.events.len() < 1024)
                    .then_some(Message::AnimationEventAdded(index)),
            )
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
    );
    controls.into()
}

fn skins_card<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let busy = state.busy();
    let count = state
        .inspection
        .as_ref()
        .map_or(1, |inspection| inspection.skins.len().max(1));
    let next = (count + state.skins.len()).to_string();
    let mut body = column![
        text(i18n.tr("models-skins")).size(tokens.typography.body_lg),
        text(i18n.tr("models-skins-hint"))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
    ]
    .spacing(tokens.spacing.gap);
    if let Some(inspection) = &state.inspection {
        let count_text = count.to_string();
        body = body.push(
            text(i18n.trn(
                "models-skins-imported",
                &[("count", Arg::Number(&count_text))],
            ))
            .size(tokens.typography.body_sm),
        );
        if !state.skins.is_empty() {
            let choices = state
                .skins
                .iter()
                .enumerate()
                .map(|(index, skin)| SkinChoice {
                    index,
                    label: format!("{} · {}", skin.name, count + index),
                })
                .collect::<Vec<_>>();
            let selected = state
                .selected_skin
                .and_then(|index| choices.get(index))
                .cloned();
            body = body.push(choice_field(
                choices,
                selected.as_ref(),
                busy,
                |choice| Message::SkinSelected(choice.index),
                ctx,
            ));
        }
        if let Some(skin) = state.selected_skin.and_then(|index| state.skins.get(index)) {
            let materials = inspection
                .skins
                .first()
                .filter(|materials| !materials.is_empty())
                .unwrap_or(&inspection.materials)
                .clone();
            body = body
                .push(
                    text_input(&i18n.tr("models-skin-name"), &skin.name)
                        .on_input_maybe((!busy).then_some(Message::SkinNameChanged))
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::input(&tokens, status)),
                )
                .push(text(i18n.tr("models-skin-slot")).size(tokens.typography.body_sm))
                .push(choice_field(
                    materials,
                    state.selected_material.as_ref(),
                    busy,
                    Message::MaterialSelected,
                    ctx,
                ))
                .push(text(i18n.tr("models-skin-tint")).size(tokens.typography.body_sm))
                .push(
                    row![
                        text_input("#ffffff", &state.material_tint)
                            .on_input_maybe((!busy).then_some(Message::MaterialTintChanged))
                            .padding(tokens.spacing.pad_control)
                            .style(move |_, status| theme::styles::input(&tokens, status)),
                        button(text(i18n.tr("models-apply")))
                            .on_press_maybe(
                                (!busy && parse_color(&state.material_tint).is_some())
                                    .then_some(Message::ApplyMaterialTint)
                            )
                            .padding(tokens.spacing.pad_control)
                            .style(move |_, status| theme::styles::button(&tokens, status)),
                    ]
                    .spacing(tokens.spacing.gap),
                )
                .push(text(i18n.tr("models-skin-material")).size(tokens.typography.body_sm))
                .push(
                    row![
                        text_input(
                            &i18n.tr("models-skin-material"),
                            &state.replacement_material
                        )
                        .on_input_maybe((!busy).then_some(Message::ReplacementMaterialChanged))
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::input(&tokens, status)),
                        button(text(i18n.tr("models-apply")))
                            .on_press_maybe(
                                (!busy && !state.replacement_material.trim().is_empty())
                                    .then_some(Message::ApplyReplacementMaterial)
                            )
                            .padding(tokens.spacing.pad_control)
                            .style(move |_, status| theme::styles::button(&tokens, status)),
                    ]
                    .spacing(tokens.spacing.gap),
                )
                .push(
                    row![
                        button(text(i18n.tr("models-skin-texture")))
                            .on_press_maybe((!busy).then_some(Message::SkinTextureRequested))
                            .padding(tokens.spacing.pad_control)
                            .style(move |_, status| theme::styles::button(&tokens, status)),
                        button(text(i18n.tr("models-skin-reset")))
                            .on_press_maybe((!busy).then_some(Message::MaterialReset))
                            .padding(tokens.spacing.pad_control)
                            .style(move |_, status| theme::styles::button(&tokens, status)),
                    ]
                    .spacing(tokens.spacing.gap)
                    .wrap(),
                );
            if let Some(material) = &state.selected_material
                && let Some(gmpublished_backend::model_project::SkinMaterial::Texture { path }) =
                    skin.replacements.get(material)
            {
                body = body.push(text(path.display().to_string()).size(tokens.typography.body_sm));
            }
        }
        body = body.push(
            button(text(i18n.tr("models-add-skin")))
                .on_press_maybe((!busy).then(|| {
                    Message::SkinAdded(
                        i18n.trn("models-skin-number", &[("index", Arg::Number(&next))]),
                    )
                }))
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
        );
    } else {
        body = body.push(text(i18n.tr("models-working")).size(tokens.typography.body_sm));
    }
    card(body.into(), ctx)
}

fn stage_card<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let busy = state.busy();
    let mut maps = state.maps.clone();
    if let Some(map) = &state.selected_map
        && !maps.contains(map)
    {
        maps.push(map.clone());
    }
    let mut body = column![
        text(i18n.tr("models-stage-map")).size(tokens.typography.body_lg),
        text(i18n.tr("models-stage-hint"))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
        row![
            choice_field(
                maps,
                state.selected_map.as_ref(),
                busy,
                Message::MapSelected,
                ctx
            ),
            button(text(i18n.tr("browse")))
                .on_press_maybe((!busy).then_some(Message::MapBrowseRequested))
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
        ]
        .spacing(tokens.spacing.gap),
        button(text(i18n.tr("models-refresh-maps")))
            .on_press_maybe(
                (!busy && state.map_generation.is_none()).then_some(Message::MapsRequested)
            )
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
        row![
            button(text(i18n.tr("models-stage")))
                .on_press_maybe(
                    (!busy && state.selected_map.is_some() && state.preview_request().is_some())
                        .then_some(Message::StageRequested)
                )
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
            button(text(i18n.tr("models-test-game")))
                .on_press_maybe(
                    (!busy
                        && state.selected_map.is_some()
                        && (state.preview.is_some()
                            || state.result.as_ref().is_some_and(|result| result.compiled)))
                    .then_some(Message::EngineTestRequested)
                )
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
        ]
        .spacing(tokens.spacing.gap)
        .wrap(),
    ]
    .spacing(tokens.spacing.gap);
    if state.maps.is_empty() {
        body = body.push(text(i18n.tr("models-no-maps")).size(tokens.typography.body_sm));
    }
    if state.engine_test_confirmation {
        body = body
            .push(text(i18n.tr("models-test-confirm")).size(tokens.typography.body_sm))
            .push(
                row![
                    button(text(i18n.tr("models-test-launch")))
                        .on_press(Message::EngineTestConfirmed)
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::action_button(&tokens, status)),
                    button(text(i18n.tr("cancel")))
                        .on_press(Message::EngineTestDismissed)
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::button(&tokens, status)),
                ]
                .spacing(tokens.spacing.gap)
                .wrap(),
            );
    }
    if let Some(test) = &state.engine_test {
        body = body
            .push(
                text(i18n.trn(
                    "models-test-ready",
                    &[("command", Arg::Text(&test.console_command))],
                ))
                .size(tokens.typography.body_sm),
            )
            .push(
                button(
                    text(test.addon_directory.display().to_string())
                        .size(tokens.typography.body_sm),
                )
                .on_press(Message::RevealRequested(test.addon_directory.clone()))
                .style(move |_, status| theme::styles::ghost_button(&tokens, status)),
            );
    }
    card(body.into(), ctx)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SkinChoice {
    index: usize,
    label: String,
}

impl std::fmt::Display for SkinChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.label.fmt(f)
    }
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

fn card<'a>(body: Element<'a, Message>, ctx: ViewCtx<'_>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    container(body)
        .width(Length::Fill)
        .padding(tokens.spacing.pad)
        .style(move |_| theme::styles::sunken_card(&tokens))
        .into()
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
