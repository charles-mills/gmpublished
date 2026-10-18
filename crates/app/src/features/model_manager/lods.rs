use std::path::PathBuf;

use gmpublished_backend::model_compile::{ModelLod, validate_lod_values};
use iced::widget::{button, column, row, text, text_input};
use iced::{Element, Length};

use super::{Message, SourceKind, State};
use crate::theme::{self, ViewCtx};

#[derive(Clone, Debug)]
pub(super) struct LodEditor {
    pub source: PathBuf,
    pub threshold: String,
}

impl State {
    fn lod_values(&self) -> impl ExactSizeIterator<Item = (&std::path::Path, Option<f32>)> {
        self.lods
            .iter()
            .map(|lod| (lod.source.as_path(), lod.threshold.trim().parse().ok()))
    }

    pub(super) fn lods_valid(&self) -> bool {
        validate_lod_values(self.lod_values(), !self.bodygroups.is_empty()).is_ok()
    }

    pub(super) fn parsed_lods(&self) -> Option<Vec<ModelLod>> {
        if !self.lods_valid() {
            return None;
        }
        self.lod_values()
            .map(|(source, switch_point)| {
                Some(ModelLod {
                    source: source.to_path_buf(),
                    switch_point: switch_point?,
                })
            })
            .collect()
    }
}

pub(super) fn edit(state: &mut State, message: &Message) -> bool {
    let picker_reply = state.running.is_none()
        && matches!(message, Message::LodSourceSelected(generation, _) if state.picker == Some(*generation));
    if (state.busy() && !picker_reply) || state.kind() != Some(SourceKind::Mesh) {
        return false;
    }
    match message {
        Message::LodSourceSelected(generation, path) if state.picker == Some(*generation) => {
            state.picker = None;
            if let Some(path) = path {
                if state.lods.len() >= 7
                    || !state.bodygroups.is_empty()
                    || SourceKind::of(path) != Some(SourceKind::Mesh)
                {
                    return false;
                }
                let threshold = state
                    .lods
                    .last()
                    .and_then(|lod| lod.threshold.parse::<f32>().ok())
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .map_or(20.0, |value| value * 2.0);
                state.lods.push(LodEditor {
                    source: path.clone(),
                    threshold: threshold.to_string(),
                });
            } else {
                return false;
            }
        }
        Message::LodRemoved(index) if *index < state.lods.len() => {
            state.lods.remove(*index);
        }
        Message::LodThresholdChanged(index, value) => {
            let Some(lod) = state.lods.get_mut(*index) else {
                return false;
            };
            lod.threshold.clone_from(value);
        }
        _ => return false,
    }
    true
}

pub(super) fn view<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let busy = state.busy();
    let mut content = column![
        text(i18n.tr("models-lod-title")).size(tokens.typography.body_lg),
        text(i18n.tr("models-lod-hint"))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
        text(i18n.tr("models-lod-facial"))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
    ]
    .spacing(tokens.spacing.gap);
    for (index, lod) in state.lods.iter().enumerate() {
        let name = lod.source.file_name().unwrap_or_default().to_string_lossy();
        content = content.push(
            column![
                text(format!("LOD {} · {name}", index + 1))
                    .size(tokens.typography.body_sm)
                    .wrapping(text::Wrapping::WordOrGlyph),
                text(i18n.tr("models-lod-threshold")).size(tokens.typography.body_sm),
                row![
                    text_input("20", &lod.threshold)
                        .on_input_maybe(
                            (!busy)
                                .then_some(move |value| Message::LodThresholdChanged(index, value))
                        )
                        .padding(tokens.spacing.pad_control)
                        .width(Length::Fill)
                        .style(move |_, status| theme::styles::input(&tokens, status)),
                    button(text(i18n.tr("models-lod-remove")).size(tokens.typography.body_sm))
                        .on_press_maybe((!busy).then_some(Message::LodRemoved(index)))
                        .style(move |_, status| theme::styles::button(&tokens, status)),
                ]
                .spacing(tokens.spacing.gap)
                .align_y(iced::Center),
            ]
            .spacing(tokens.spacing.gap_xs),
        );
    }
    if !state.bodygroups.is_empty() {
        content = content.push(
            text(i18n.tr("models-lod-bodygroups"))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.text_dim),
        );
    } else if !state.lods_valid() {
        content = content.push(
            text(i18n.tr("models-lod-invalid"))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.text_dim),
        );
    }
    content
        .push(
            button(text(i18n.tr("models-lod-add")))
                .on_press_maybe(
                    (!busy && state.lods.len() < 7 && state.bodygroups.is_empty())
                        .then_some(Message::LodSourceRequested),
                )
                .style(move |_, status| theme::styles::button(&tokens, status)),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::model_manager::{Effect, update};

    #[test]
    fn lod_edits_validate_distances_ignore_stale_pickers_and_reset_with_source() {
        let mut state = State {
            source: Some("mesh.smd".into()),
            destination: Some("output".into()),
            ..State::default()
        };
        let Effect::PickLodSource(generation, _) =
            update(&mut state, Message::LodSourceRequested)[0]
        else {
            panic!()
        };
        assert!(
            update(
                &mut state,
                Message::LodSourceSelected(generation, Some("low.smd".into()))
            )
            .is_empty()
        );
        assert_eq!(state.parsed_lods().unwrap()[0].switch_point, 20.0);
        update(
            &mut state,
            Message::LodSourceSelected(generation, Some("stale.smd".into())),
        );
        assert_eq!(state.lods.len(), 1);
        let Effect::PickLodSource(generation, _) =
            update(&mut state, Message::LodSourceRequested)[0]
        else {
            panic!()
        };
        update(
            &mut state,
            Message::LodSourceSelected(generation, Some("lower.glb".into())),
        );
        assert_eq!(state.preview_request().unwrap().lods.len(), 2);
        for value in ["20", "NaN", "-1", "0"] {
            update(&mut state, Message::LodThresholdChanged(1, value.into()));
            assert!(!state.can_export());
            assert!(state.preview_request().is_none());
        }
        update(&mut state, Message::LodThresholdChanged(1, "60".into()));
        assert!(state.can_export());
        update(&mut state, Message::LodRemoved(0));
        assert_eq!(
            state.request().unwrap().lods[0].source,
            PathBuf::from("lower.glb")
        );
        update(&mut state, Message::SourceDropped("other.smd".into()));
        assert!(state.lods.is_empty());
    }

    #[test]
    fn lod_controls_are_localized_and_fit_the_model_manager_panel() {
        use crate::i18n::I18n;
        use crate::theme::Tokens;
        let state = State {
            source: Some("mesh.smd".into()),
            lods: vec![LodEditor {
                source: "low.smd".into(),
                threshold: "20".into(),
            }],
            ..State::default()
        };
        let tokens = Tokens::dark().with_display_preferences(
            crate::bridge::UiDensity::Comfortable,
            crate::bridge::MAX_TEXT_SIZE_PERCENT,
        );
        for locale in [
            "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
        ] {
            let i18n = I18n::for_locale(Some(locale));
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(310.0, 900.0),
                view(&state, ViewCtx::new(&tokens, &i18n)),
            );
            for key in [
                "models-lod-title",
                "models-lod-threshold",
                "models-lod-add",
                "models-lod-remove",
                "models-lod-facial",
            ] {
                let label = i18n.tr(key);
                assert_ne!(label, key);
                let widget = ui.find(label.as_str()).unwrap();
                assert!(
                    widget.bounds().x >= 0.0 && widget.bounds().x + widget.bounds().width <= 310.0,
                    "{locale}: {key}"
                );
            }
            ui.click(i18n.tr("models-lod-remove").as_str()).unwrap();
            assert!(
                ui.into_messages()
                    .any(|message| matches!(message, Message::LodRemoved(0)))
            );
        }
    }
}
