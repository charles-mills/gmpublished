use iced::widget::{button, column, row, text, text_editor};
use iced::{Element, Length};

use super::{Effect, Message, State as ModelState};
use crate::generation::Generation;
use crate::theme::{self, ViewCtx};

#[derive(Debug)]
pub(super) struct State {
    content: text_editor::Content,
    original: Option<String>,
    pub load_generation: Option<Generation>,
    expanded: bool,
    pub validated: bool,
    pub diagnostic: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            content: text_editor::Content::new(),
            original: None,
            load_generation: None,
            expanded: true,
            validated: false,
            diagnostic: None,
        }
    }
}

impl State {
    pub fn text(&self) -> Option<String> {
        self.original.as_ref().map(|_| self.content.text())
    }

    fn dirty(&self) -> bool {
        self.original
            .as_ref()
            .is_some_and(|original| *original != self.content.text())
    }
}

impl ModelState {
    pub(super) fn is_qc(&self) -> bool {
        self.source.as_ref().is_some_and(|source| {
            source
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("qc"))
        })
    }

    pub(super) fn qc_editor_ready(&self) -> bool {
        !self.is_qc() || self.qc_editor.original.is_some()
    }
}

pub(super) fn update(state: &mut ModelState, message: &Message) -> Option<Vec<Effect>> {
    match message {
        Message::QcLoaded(generation, result)
            if state.qc_editor.load_generation == Some(*generation) && state.is_qc() =>
        {
            state.qc_editor.load_generation = None;
            match result {
                Ok(text) => {
                    state.qc_editor.content = text_editor::Content::with_text(text);
                    state.qc_editor.original = Some(text.clone());
                    let generation = state.generation.bump();
                    state.inspection_generation = Some(generation);
                    state.error = None;
                    return Some(vec![Effect::ValidateQc(
                        generation,
                        state.source.clone().unwrap(),
                        text.clone(),
                    )]);
                }
                Err(error) => state.error = Some(error.clone()),
            }
        }
        Message::QcEditorToggled if state.is_qc() && !state.busy() => {
            state.qc_editor.expanded = !state.qc_editor.expanded;
        }
        Message::QcAction(action) if state.is_qc() && state.qc_editor_ready() && !state.busy() => {
            if action.is_edit() {
                invalidate(state);
            }
            state.qc_editor.content.perform(action.clone());
        }
        Message::QcReset if state.is_qc() && !state.busy() && state.qc_editor.dirty() => {
            let original = state.qc_editor.original.clone().unwrap_or_default();
            state.qc_editor.content = text_editor::Content::with_text(&original);
            invalidate(state);
        }
        Message::QcCopyDiagnostic if state.is_qc() => {
            return Some(
                state
                    .qc_editor
                    .diagnostic
                    .as_ref()
                    .map(|text| vec![Effect::Copy(text.clone())])
                    .unwrap_or_default(),
            );
        }
        Message::QcValidate if state.is_qc() && !state.busy() && state.qc_editor_ready() => {
            let generation = state.generation.bump();
            state.inspection_generation = Some(generation);
            state.qc_editor.validated = false;
            state.qc_editor.diagnostic = None;
            state.error = None;
            return Some(vec![Effect::ValidateQc(
                generation,
                state.source.clone().unwrap(),
                state.qc_editor.text().unwrap(),
            )]);
        }
        _ => return None,
    }
    Some(vec![])
}

fn invalidate(state: &mut ModelState) {
    state.inspection_generation = None;
    state.inspection = None;
    state.qc_editor.validated = false;
    state.qc_editor.diagnostic = None;
    state.selected_material = None;
    super::update::clear_outcome(state);
}

pub(super) fn view<'a>(state: &'a ModelState, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let enabled = !state.busy() && state.qc_editor_ready();
    let mut content = column![
        button(text(i18n.tr("models-qc-editor")))
            .on_press_maybe((!state.busy()).then_some(Message::QcEditorToggled))
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::button(&tokens, status)),
        text(i18n.tr("models-qc-editor-hint"))
            .size(tokens.typography.body_sm)
            .color(tokens.colors.text_dim),
    ]
    .spacing(tokens.spacing.gap);
    if state.qc_editor.expanded {
        if state.qc_editor.original.is_some() {
            let editor = text_editor(&state.qc_editor.content)
                .font(iced::Font::MONOSPACE)
                .height(Length::Fixed(340.0))
                .padding(tokens.spacing.pad_control)
                .size(tokens.typography.body_sm)
                .style(move |_, status| theme::styles::text_editor(&tokens, status));
            content = content.push(if enabled {
                editor.on_action(Message::QcAction)
            } else {
                editor
            });
            content = content.push(
                row![
                    button(text(i18n.tr("models-qc-validate")))
                        .on_press_maybe(enabled.then_some(Message::QcValidate))
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::button(&tokens, status)),
                    button(text(i18n.tr("models-qc-reset")))
                        .on_press_maybe(
                            (enabled && state.qc_editor.dirty()).then_some(Message::QcReset)
                        )
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::button(&tokens, status)),
                ]
                .spacing(tokens.spacing.gap)
                .align_y(iced::Center),
            );
            content = content.push(
                text(i18n.tr(if state.qc_editor.validated {
                    "models-qc-valid"
                } else if state.qc_editor.dirty() {
                    "models-qc-modified"
                } else {
                    "models-qc-unvalidated"
                }))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.text_dim),
            );
        } else if state.qc_editor.load_generation.is_some() {
            content = content.push(text(i18n.tr("models-qc-loading")));
        }
    }
    if let Some(diagnostic) = &state.qc_editor.diagnostic {
        content = content.push(
            text(diagnostic)
                .size(tokens.typography.body_sm)
                .color(tokens.colors.error),
        );
        content = content.push(
            button(text(i18n.tr("models-copy-diagnostics")))
                .on_press(Message::QcCopyDiagnostic)
                .padding(tokens.spacing.pad_control)
                .style(move |_, status| theme::styles::button(&tokens, status)),
        );
    }
    content.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::text_editor::{Action, Edit};
    use std::sync::Arc;

    fn imported() -> ModelState {
        let mut state = ModelState::default();
        super::super::update(&mut state, Message::SourceDropped("source.qc".into()));
        let generation = state.qc_editor.load_generation.unwrap();
        super::super::update(
            &mut state,
            Message::QcLoaded(generation, Ok("$scale 1\n".into())),
        );
        state.destination = Some("out".into());
        state
    }

    fn replace(state: &mut ModelState, text: &str) {
        super::super::update(state, Message::QcAction(Action::SelectAll));
        super::super::update(
            state,
            Message::QcAction(Action::Edit(Edit::Paste(Arc::new(text.into())))),
        );
    }

    #[test]
    fn edits_share_one_snapshot_across_validation_preview_export_and_save() {
        let mut state = imported();
        state.qc_saved = true;
        state.qc_editor.validated = true;
        replace(&mut state, "$scale 2\n");
        assert!(!state.qc_saved);
        assert!(!state.qc_editor.validated);
        for request in [state.request(), state.preview_request(), state.qc_request()] {
            assert_eq!(request.unwrap().qc_text.as_deref(), Some("$scale 2\n"));
        }
        let effects = super::super::update(&mut state, Message::QcValidate);
        assert!(
            matches!(&effects[..], [Effect::ValidateQc(_, source, text)] if source == std::path::Path::new("source.qc") && text == "$scale 2\n")
        );
        super::super::update(&mut state, Message::QcReset);
        assert_eq!(
            state.request().unwrap().qc_text.as_deref(),
            Some("$scale 1\n")
        );
    }

    #[test]
    fn cursor_actions_preserve_results_and_busy_edits_are_ignored() {
        let mut state = imported();
        state.qc_saved = true;
        state.qc_editor.validated = true;
        super::super::update(&mut state, Message::QcAction(Action::SelectAll));
        assert!(state.qc_saved);
        assert!(state.qc_editor.validated);
        state.picker = Some(state.generation.bump());
        replace(&mut state, "ignored");
        super::super::update(&mut state, Message::QcReset);
        assert_eq!(state.qc_editor.text().as_deref(), Some("$scale 1\n"));
        assert!(super::super::update(&mut state, Message::QcValidate).is_empty());
    }

    #[test]
    fn reimport_and_edits_discard_stale_text_and_validation_replies() {
        let mut state = imported();
        super::super::update(&mut state, Message::QcValidate);
        let stale = state.inspection_generation.unwrap();
        replace(&mut state, "$scale 3\n");
        super::super::update(
            &mut state,
            Message::Inspected(stale, Err("stale syntax error".into())),
        );
        assert!(state.error.is_none());
        super::super::update(&mut state, Message::SourceDropped("second.qc".into()));
        let stale = state.qc_editor.load_generation.unwrap();
        assert!(state.preview_request().is_none());
        super::super::update(&mut state, Message::SourceDropped("third.qc".into()));
        super::super::update(
            &mut state,
            Message::QcLoaded(stale, Ok("stale source".into())),
        );
        assert!(state.qc_editor.text().is_none());
        let current = state.qc_editor.load_generation.unwrap();
        super::super::update(
            &mut state,
            Message::QcLoaded(current, Ok("$scale 4\n".into())),
        );
        assert_eq!(
            state.preview_request().unwrap().qc_text.as_deref(),
            Some("$scale 4\n")
        );
        super::super::update(&mut state, Message::SourceDropped("mesh.smd".into()));
        assert!(state.preview_request().unwrap().qc_text.is_none());
    }

    #[test]
    fn validation_diagnostics_stay_by_the_editor_clear_on_changes_and_ignore_stale_results() {
        let mut state = imported();
        let generation = state.inspection_generation.unwrap();
        super::super::update(
            &mut state,
            Message::Inspected(generation, Err("source.qc:2: bad command".into())),
        );
        assert_eq!(
            state.qc_editor.diagnostic.as_deref(),
            Some("source.qc:2: bad command")
        );
        assert!(state.error.is_none());
        let effects = super::super::update(&mut state, Message::QcCopyDiagnostic);
        assert!(matches!(&effects[..], [Effect::Copy(text)] if text == "source.qc:2: bad command"));
        replace(&mut state, "$scale 2\n");
        assert!(state.qc_editor.diagnostic.is_none());
        super::super::update(
            &mut state,
            Message::Inspected(generation, Err("stale".into())),
        );
        assert!(state.qc_editor.diagnostic.is_none());
        super::super::update(&mut state, Message::QcValidate);
        let generation = state.inspection_generation.unwrap();
        super::super::update(
            &mut state,
            Message::Inspected(generation, Err("current".into())),
        );
        assert_eq!(state.qc_editor.diagnostic.as_deref(), Some("current"));
        super::super::update(&mut state, Message::QcReset);
        assert!(state.qc_editor.diagnostic.is_none());
        super::super::update(&mut state, Message::QcValidate);
        let generation = state.inspection_generation.unwrap();
        super::super::update(
            &mut state,
            Message::Inspected(generation, Err("current".into())),
        );
        super::super::update(&mut state, Message::QcValidate);
        assert!(state.qc_editor.diagnostic.is_none());
        state.error = Some("export failed".into());
        let generation = state.inspection_generation.unwrap();
        super::super::update(
            &mut state,
            Message::Inspected(generation, Err("syntax failed".into())),
        );
        assert_eq!(state.error.as_deref(), Some("export failed"));
        assert_eq!(state.qc_editor.diagnostic.as_deref(), Some("syntax failed"));
    }

    #[test]
    fn editor_controls_are_localized_and_busy_guarded() {
        let state = imported();
        let tokens = crate::theme::Tokens::dark();
        for locale in [
            "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
        ] {
            let i18n = crate::i18n::I18n::for_locale(Some(locale));
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(560.0, 620.0),
                view(&state, ViewCtx::new(&tokens, &i18n)),
            );
            for key in ["models-qc-validate", "models-qc-reset"] {
                let bounds = ui.find(i18n.tr(key).as_str()).unwrap().bounds();
                assert!(
                    bounds.x >= 0.0
                        && bounds.x + bounds.width <= 560.0
                        && bounds.y >= 0.0
                        && bounds.y + bounds.height <= 620.0,
                    "{locale}: {key}: {bounds:?}"
                );
            }
            ui.click(i18n.tr("models-qc-validate").as_str()).unwrap();
            assert!(
                ui.into_messages()
                    .any(|message| matches!(message, Message::QcValidate)),
                "{locale}"
            );
        }
        let mut state = state;
        state.picker = Some(state.generation.bump());
        let i18n = crate::i18n::I18n::for_locale(Some("en"));
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(560.0, 620.0),
            view(&state, ViewCtx::new(&tokens, &i18n)),
        );
        ui.click(i18n.tr("models-qc-validate").as_str()).unwrap();
        assert!(
            !ui.into_messages()
                .any(|message| matches!(message, Message::QcValidate))
        );
    }
}
