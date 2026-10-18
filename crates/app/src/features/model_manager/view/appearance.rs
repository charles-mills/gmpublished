use iced::Element;
use iced::widget::{button, column, row, text, text_input};

use super::super::update::parse_color;
use super::super::{Message, SourceKind, State};
use super::choice_field;
use crate::features::file_preview;
use crate::i18n::Arg;
use crate::theme::{self, ViewCtx};

pub(super) fn appearance_panel<'a>(
    state: &'a State,
    preview: Option<&'a file_preview::State>,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let mut body = column![].spacing(ctx.tokens.spacing.gap_lg);
    if let Some(preview) = preview {
        body = body
            .push(file_preview::workspace_selection_controls(preview, ctx).map(Message::Viewer));
    }
    if state.kind() != Some(SourceKind::Compiled) {
        body = body.push(skins_card(state, ctx));
    }
    body.into()
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
    let mut body = column![text(i18n.tr("models-skins")).size(tokens.typography.body_lg),]
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
                        .size(tokens.typography.body)
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
                    text_input("#ffffff", &state.material_tint)
                        .on_input_maybe((!busy).then_some(Message::MaterialTintChanged))
                        .on_submit_maybe(
                            (!busy && parse_color(&state.material_tint).is_some())
                                .then_some(Message::ApplyMaterialTint),
                        )
                        .padding(tokens.spacing.pad_control)
                        .size(tokens.typography.body)
                        .style(move |_, status| theme::styles::input(&tokens, status)),
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
                        .size(tokens.typography.body)
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
    body.into()
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
