use gmpublished_backend::model_compile::{BodygroupChoice, ModelBodygroup, validate_bodygroups};
use iced::widget::{button, checkbox, column, row, text, text_input};
use iced::{Element, Length};

use super::{Message, SourceKind, State};
use crate::theme::{self, ViewCtx};

impl State {
    pub(super) fn bodygroups_valid(&self) -> bool {
        self.bodygroups.is_empty()
            || (self.kind() == Some(SourceKind::Mesh)
                && self.inspection.as_ref().is_some_and(|inspection| {
                    validate_bodygroups(&self.bodygroups, &inspection.parts).is_ok()
                }))
    }
}

fn owns_part(group: &ModelBodygroup, selector: &str) -> bool {
    group.choices.iter().any(|choice| matches!(choice, BodygroupChoice::Mesh { parts, .. } if parts.iter().any(|part| part == selector)))
}

fn next_label(prefix: &str, labels: impl Iterator<Item = String>) -> String {
    let labels: Vec<_> = labels.collect();
    (1..)
        .map(|index| format!("{prefix}{index}"))
        .find(|label| {
            !labels
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(label))
        })
        .unwrap()
}

fn new_choice(state: &State, group: usize) -> BodygroupChoice {
    let name = next_label(
        "option",
        state
            .bodygroups
            .get(group)
            .into_iter()
            .flat_map(|group| &group.choices)
            .filter_map(|choice| match choice {
                BodygroupChoice::Mesh { name, .. } => Some(name.clone()),
                BodygroupChoice::Hidden => None,
            }),
    );
    let parts = state
        .inspection
        .as_ref()
        .and_then(|inspection| {
            inspection.parts.iter().find(|part| {
                !state
                    .bodygroups
                    .iter()
                    .any(|group| owns_part(group, &part.selector))
            })
        })
        .map(|part| vec![part.selector.clone()])
        .unwrap_or_default();
    BodygroupChoice::Mesh { name, parts }
}

pub(super) fn edit(state: &mut State, message: &Message) -> bool {
    if state.busy()
        || state.kind() != Some(SourceKind::Mesh)
        || state
            .inspection
            .as_ref()
            .is_none_or(|inspection| inspection.parts.is_empty())
    {
        return false;
    }
    match message {
        Message::BodygroupAdded if state.bodygroups.len() < 32 && state.lods.is_empty() => {
            let name = next_label(
                "group",
                state.bodygroups.iter().map(|group| group.name.clone()),
            );
            let choice = new_choice(state, state.bodygroups.len());
            state.bodygroups.push(ModelBodygroup {
                name,
                choices: vec![choice],
            });
            state.bodygroup_editor = Some((state.bodygroups.len() - 1, 0));
            state.bodygroup_filter.clear();
        }
        Message::BodygroupRemoved(index) if *index < state.bodygroups.len() => {
            state.bodygroups.remove(*index);
        }
        Message::BodygroupNameChanged(index, name) => {
            let Some(group) = state.bodygroups.get_mut(*index) else {
                return false;
            };
            group.name.clone_from(name);
        }
        Message::BodygroupChoiceAdded(index)
            if state
                .bodygroups
                .get(*index)
                .is_some_and(|group| group.choices.len() < 32) =>
        {
            let choice = new_choice(state, *index);
            state.bodygroups[*index].choices.push(choice);
            state.bodygroup_editor = Some((*index, state.bodygroups[*index].choices.len() - 1));
            state.bodygroup_filter.clear();
        }
        Message::BodygroupHiddenAdded(index) => {
            let Some(group) = state.bodygroups.get_mut(*index) else {
                return false;
            };
            if group.choices.len() >= 32 || group.choices.contains(&BodygroupChoice::Hidden) {
                return false;
            }
            group.choices.push(BodygroupChoice::Hidden);
        }
        Message::BodygroupChoiceRemoved(index, choice) => {
            let Some(group) = state.bodygroups.get_mut(*index) else {
                return false;
            };
            if *choice >= group.choices.len() {
                return false;
            }
            group.choices.remove(*choice);
        }
        Message::BodygroupChoiceMoved(index, from, to) => {
            let Some(group) = state.bodygroups.get_mut(*index) else {
                return false;
            };
            if *from >= group.choices.len() || *to >= group.choices.len() || from == to {
                return false;
            }
            let choice = group.choices.remove(*from);
            group.choices.insert(*to, choice);
        }
        Message::BodygroupChoiceNameChanged(index, choice, value) => {
            let Some(BodygroupChoice::Mesh { name, .. }) = state
                .bodygroups
                .get_mut(*index)
                .and_then(|group| group.choices.get_mut(*choice))
            else {
                return false;
            };
            name.clone_from(value);
        }
        Message::BodygroupPartToggled(index, choice, selector, selected) => {
            if !state.inspection.as_ref().is_some_and(|inspection| {
                inspection
                    .parts
                    .iter()
                    .any(|part| &part.selector == selector)
            }) || state
                .bodygroups
                .iter()
                .enumerate()
                .any(|(owner, group)| owner != *index && owns_part(group, selector))
            {
                return false;
            }
            let Some(BodygroupChoice::Mesh { parts, .. }) = state
                .bodygroups
                .get_mut(*index)
                .and_then(|group| group.choices.get_mut(*choice))
            else {
                return false;
            };
            if *selected {
                if parts.contains(selector) {
                    return false;
                }
                parts.push(selector.clone());
            } else {
                let Some(index) = parts.iter().position(|part| part == selector) else {
                    return false;
                };
                parts.remove(index);
            }
        }
        _ => return false,
    }
    if matches!(
        message,
        Message::BodygroupRemoved(_)
            | Message::BodygroupChoiceRemoved(_, _)
            | Message::BodygroupChoiceMoved(_, _, _)
    ) {
        state.bodygroup_editor = None;
        state.bodygroup_filter.clear();
    }
    true
}

pub(super) fn view<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let busy = state.busy();
    let mut content = column![
        text(i18n.tr("models-bodygroups")).size(tokens.typography.body_lg),
        text(i18n.tr("models-bodygroups-hint"))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
    ]
    .spacing(tokens.spacing.gap);
    let Some(inspection) = &state.inspection else {
        return content.into();
    };
    let mut name_counts = std::collections::HashMap::new();
    for part in &inspection.parts {
        *name_counts.entry(part.name.as_str()).or_insert(0_usize) += 1;
    }
    for (group_index, group) in state.bodygroups.iter().enumerate() {
        content = content.push(
            row![
                text_input(&i18n.tr("models-bodygroup-name"), &group.name)
                    .on_input_maybe((!busy).then_some(move |value| {
                        Message::BodygroupNameChanged(group_index, value)
                    }))
                    .padding(tokens.spacing.pad_control)
                    .style(move |_, status| theme::styles::input(&tokens, status)),
                button(text(i18n.tr("models-bodygroup-remove")))
                    .on_press_maybe((!busy).then_some(Message::BodygroupRemoved(group_index)))
                    .style(move |_, status| theme::styles::button(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap)
            .align_y(iced::Center),
        );
        for (choice_index, choice) in group.choices.iter().enumerate() {
            let mut option = column![].spacing(tokens.spacing.gap);
            let label: Element<'_, Message> = match choice {
                BodygroupChoice::Hidden => text(i18n.tr("models-bodygroup-hidden"))
                    .width(Length::Fill)
                    .into(),
                BodygroupChoice::Mesh { name, .. } => {
                    text_input(&i18n.tr("models-bodygroup-choice-name"), name)
                        .on_input_maybe((!busy).then_some(move |value| {
                            Message::BodygroupChoiceNameChanged(group_index, choice_index, value)
                        }))
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::input(&tokens, status))
                        .into()
                }
            };
            option =
                option.push(
                    row![
                        label,
                        button(text("↑"))
                            .on_press_maybe((!busy && choice_index > 0).then_some(
                                Message::BodygroupChoiceMoved(
                                    group_index,
                                    choice_index,
                                    choice_index.saturating_sub(1)
                                )
                            ))
                            .style(move |_, status| theme::styles::button(&tokens, status)),
                        button(text("↓"))
                            .on_press_maybe(
                                (!busy && choice_index + 1 < group.choices.len()).then_some(
                                    Message::BodygroupChoiceMoved(
                                        group_index,
                                        choice_index,
                                        choice_index + 1
                                    )
                                )
                            )
                            .style(move |_, status| theme::styles::button(&tokens, status)),
                        button(text(i18n.tr("models-bodygroup-remove-choice")))
                            .on_press_maybe((!busy).then_some(Message::BodygroupChoiceRemoved(
                                group_index,
                                choice_index
                            )))
                            .style(move |_, status| theme::styles::button(&tokens, status)),
                    ]
                    .spacing(tokens.spacing.gap)
                    .align_y(iced::Center),
                );
            if choice_index == 0 {
                option = option.push(
                    text(i18n.tr("models-bodygroup-default"))
                        .size(tokens.typography.body_sm)
                        .color(tokens.colors.text_dim),
                );
            }
            if let BodygroupChoice::Mesh { parts, .. } = choice {
                option = option.push(
                    button(text(i18n.trn(
                        "models-bodygroup-edit-meshes",
                        &[("count", crate::i18n::Arg::Number(&parts.len().to_string()))],
                    )))
                    .on_press_maybe(
                        (!busy)
                            .then_some(Message::BodygroupEditorToggled(group_index, choice_index)),
                    )
                    .style(move |_, status| theme::styles::button(&tokens, status)),
                );
                if state.bodygroup_editor == Some((group_index, choice_index)) {
                    option = option.push(
                        text_input(&i18n.tr("models-bodygroup-search"), &state.bodygroup_filter)
                            .on_input_maybe((!busy).then_some(Message::BodygroupFilterChanged))
                            .padding(tokens.spacing.pad_control)
                            .style(move |_, status| theme::styles::input(&tokens, status)),
                    );
                    let filter = state.bodygroup_filter.to_lowercase();
                    let matching: Vec<_> = inspection
                        .parts
                        .iter()
                        .enumerate()
                        .map(|(index, part)| {
                            let label = if name_counts[part.name.as_str()] > 1 {
                                format!("{} ({})", part.name, index + 1)
                            } else {
                                part.name.clone()
                            };
                            (part, label)
                        })
                        .filter(|(_, label)| {
                            filter.is_empty() || label.to_lowercase().contains(&filter)
                        })
                        .collect();
                    for (part, label) in matching.iter().take(100) {
                        let selector = part.selector.clone();
                        let available =
                            !state.bodygroups.iter().enumerate().any(|(owner, group)| {
                                owner != group_index && owns_part(group, &selector)
                            });
                        option = option.push(
                            checkbox(parts.contains(&selector))
                                .label(label.clone())
                                .on_toggle_maybe((!busy && available).then_some(move |selected| {
                                    Message::BodygroupPartToggled(
                                        group_index,
                                        choice_index,
                                        selector.clone(),
                                        selected,
                                    )
                                }))
                                .style(move |_, status| theme::styles::checkbox(&tokens, status)),
                        );
                    }
                    if matching.len() > 100 {
                        option = option.push(
                            text(i18n.tr("models-bodygroup-search-more"))
                                .size(tokens.typography.body_sm)
                                .color(tokens.colors.text_dim),
                        );
                    }
                }
            }
            content =
                content.push(iced::widget::container(option).padding(tokens.spacing.pad_control));
        }
        content = content.push(
            row![
                button(text(i18n.tr("models-bodygroup-add-choice")))
                    .on_press_maybe(
                        (!busy && group.choices.len() < 32)
                            .then_some(Message::BodygroupChoiceAdded(group_index))
                    )
                    .style(move |_, status| theme::styles::button(&tokens, status)),
                button(text(i18n.tr("models-bodygroup-add-hidden")))
                    .on_press_maybe(
                        (!busy
                            && group.choices.len() < 32
                            && !group.choices.contains(&BodygroupChoice::Hidden))
                        .then_some(Message::BodygroupHiddenAdded(group_index))
                    )
                    .style(move |_, status| theme::styles::button(&tokens, status)),
            ]
            .spacing(tokens.spacing.gap)
            .wrap(),
        );
    }
    content = content.push(
        button(text(i18n.tr("models-bodygroup-add")))
            .on_press_maybe(
                (!busy && state.bodygroups.len() < 32 && state.lods.is_empty())
                    .then_some(Message::BodygroupAdded),
            )
            .style(move |_, status| theme::styles::button(&tokens, status)),
    );
    if !state.lods.is_empty() {
        content = content.push(
            text(i18n.tr("models-lod-bodygroups"))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.text_dim),
        );
    }
    if !state.bodygroups_valid() {
        content = content.push(
            text(i18n.tr("models-bodygroups-invalid"))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.error),
        );
    }
    content.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::Generation;
    use crate::i18n::I18n;
    use crate::theme::Tokens;
    use gmpublished_backend::model_compile::{ModelInspection, SourcePart};

    fn state() -> State {
        let mut state = State::default();
        super::super::update(&mut state, Message::SourceDropped("prop.gltf".into()));
        state.destination = Some("output".into());
        state.inspection = Some(ModelInspection {
            parts: (0..3)
                .map(|index| SourcePart {
                    selector: format!("@node:{index}"),
                    name: format!("Part {index}"),
                })
                .collect(),
            animations: vec![],
            materials: vec![],
            material_directories: vec![],
            skins: vec![],
        });
        state
    }

    #[test]
    fn authoring_propagates_choices_resets_selection_and_invalidates_saved_output() {
        let mut state = state();
        super::super::update(&mut state, Message::BodygroupAdded);
        super::super::update(&mut state, Message::BodygroupChoiceAdded(0));
        super::super::update(&mut state, Message::BodygroupHiddenAdded(0));
        assert!(state.can_export());
        assert_eq!(
            state.preview_request().unwrap().bodygroups,
            state.bodygroups
        );
        assert_eq!(state.qc_request().unwrap().bodygroups, state.bodygroups);
        state.stage_bodygroups = vec![2];
        state.qc_saved = true;
        super::super::update(&mut state, Message::BodygroupChoiceMoved(0, 2, 0));
        assert_eq!(state.bodygroups[0].choices[0], BodygroupChoice::Hidden);
        assert!(state.stage_bodygroups.is_empty());
        assert!(!state.qc_saved);
        super::super::update(&mut state, Message::SourceDropped("new.fbx".into()));
        assert!(state.bodygroups.is_empty());
    }

    #[test]
    fn conflicting_assignments_and_busy_edits_are_rejected_and_invalid_choices_block_actions() {
        let mut state = state();
        super::super::update(&mut state, Message::BodygroupAdded);
        super::super::update(&mut state, Message::BodygroupAdded);
        let original = state.bodygroups.clone();
        super::super::update(
            &mut state,
            Message::BodygroupPartToggled(1, 0, "@node:0".into(), true),
        );
        assert_eq!(state.bodygroups, original);
        state.picker = Some(Generation::INITIAL);
        super::super::update(&mut state, Message::BodygroupRemoved(0));
        assert_eq!(state.bodygroups, original);
        state.picker = None;
        super::super::update(
            &mut state,
            Message::BodygroupPartToggled(0, 0, "@node:0".into(), false),
        );
        assert!(!state.can_export());
        assert!(state.preview_request().is_none());
        assert!(state.qc_request().is_none());
        super::super::update(&mut state, Message::BodygroupRemoved(0));
        assert!(state.can_export());
    }

    #[test]
    fn group_and_hidden_choice_controls_are_clickable_in_every_locale() {
        let mut state = state();
        let tokens = Tokens::dark();
        for locale in [
            "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
        ] {
            let i18n = I18n::for_locale(Some(locale));
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(900.0, 900.0),
                view(&state, ViewCtx::new(&tokens, &i18n)),
            );
            ui.click(i18n.tr("models-bodygroup-add").as_str()).unwrap();
            assert!(
                ui.into_messages()
                    .any(|message| matches!(message, Message::BodygroupAdded))
            );
        }
        super::super::update(&mut state, Message::BodygroupAdded);
        for locale in [
            "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
        ] {
            let i18n = I18n::for_locale(Some(locale));
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(560.0, 900.0),
                view(&state, ViewCtx::new(&tokens, &i18n)),
            );
            let control = ui
                .find(i18n.tr("models-bodygroup-add-hidden").as_str())
                .unwrap();
            assert!(
                control.bounds().x >= 0.0 && control.bounds().x + control.bounds().width <= 560.0,
                "bodygroup controls overflow in {locale}"
            );
            ui.click(i18n.tr("models-bodygroup-add-hidden").as_str())
                .unwrap();
            assert!(
                ui.into_messages()
                    .any(|message| matches!(message, Message::BodygroupHiddenAdded(0)))
            );
        }
    }
    #[test]
    fn mesh_search_reaches_duplicate_names_beyond_the_initial_page_without_invalidating_output() {
        let mut state = state();
        state.inspection.as_mut().unwrap().parts = (0..120)
            .map(|index| SourcePart {
                selector: format!("@node:{index}"),
                name: "Hat".into(),
            })
            .collect();
        super::super::update(&mut state, Message::BodygroupAdded);
        state.qc_saved = true;
        let groups = state.bodygroups.clone();
        super::super::update(&mut state, Message::BodygroupFilterChanged("120".into()));
        assert!(state.qc_saved);
        assert_eq!(state.bodygroups, groups);
        let tokens = Tokens::dark();
        let i18n = I18n::for_locale(Some("en"));
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(900.0, 900.0),
            view(&state, ViewCtx::new(&tokens, &i18n)),
        );
        ui.click("Hat (120)").unwrap();
        assert!(ui.into_messages().any(|message| matches!(message,Message::BodygroupPartToggled(0,0,selector,true) if selector=="@node:119")));
    }
}
