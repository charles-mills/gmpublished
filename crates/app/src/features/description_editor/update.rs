use std::sync::Arc;
use std::time::Instant;

use iced::widget::text_editor::{Action, Edit, Motion};

use super::effect::{Effect, SaveRequest, SourceRequest};
use super::markup::{self, EnterBehavior, ToolbarAction};
use super::message::Message;
use super::state::State;
use crate::bridge::domain::WORKSHOP_LEGAL_URL;
use crate::widgets::bbcode::normalize_description_url;

pub fn update_at(state: &mut State, message: Message, now: Instant) -> Vec<Effect> {
    match message {
        Message::OpenLocalizedRequested {
            workshop_id,
            title,
            language,
        } => {
            state.set_initial_language(language);
            update_at(state, Message::OpenRequested { workshop_id, title }, now)
        }
        Message::OpenPendingTranslationsRequested {
            workshop_id,
            drafts,
        } => {
            state.open_pending_translations(workshop_id, drafts);
            vec![Effect::ModalOpenRequested, Effect::ThumbnailDemandsChanged]
        }
        Message::OpenRequested { workshop_id, title } => {
            let generation = state.open(workshop_id, title);
            vec![
                Effect::ModalOpenRequested,
                Effect::SourceFetchRequested(SourceRequest {
                    workshop_id,
                    language: state.language(),
                    generation,
                }),
            ]
        }
        Message::PanesResized { split, ratio } => {
            state.resize_panes(split, ratio);
            Vec::new()
        }
        Message::PanesLayoutChanged(width) => {
            state.layout_panes(width);
            Vec::new()
        }
        Message::PanesReset => {
            state.reset_panes();
            Vec::new()
        }
        Message::LanguageSelected(language) => {
            let request = state.switch_language(language).map(|workshop_id| {
                Effect::SourceFetchRequested(SourceRequest {
                    workshop_id,
                    language,
                    generation: state.generation(),
                })
            });
            request
                .into_iter()
                .chain([Effect::ThumbnailDemandsChanged])
                .collect()
        }
        Message::RetryFetchRequested => state
            .retry_fetch()
            .map(|workshop_id| {
                Effect::SourceFetchRequested(SourceRequest {
                    workshop_id,
                    language: state.language(),
                    generation: state.generation(),
                })
            })
            .into_iter()
            .collect(),
        Message::OpenLocalizedDraftRequested { language, drafts } => {
            state.open_localized_draft(language, drafts);
            vec![Effect::ModalOpenRequested, Effect::ThumbnailDemandsChanged]
        }
        Message::TitleEdited(title) => {
            state.edit_title(title);
            Vec::new()
        }
        Message::Undo | Message::Redo => {
            state.undo_redo(matches!(message, Message::Redo));
            vec![Effect::ThumbnailDemandsChanged]
        }
        Message::SourceFetched { generation, result } => {
            if generation != state.generation() || !state.loading() {
                return Vec::new();
            }
            match result {
                Ok(source) => {
                    state.apply_fetched(source.title, &source.description);
                    vec![Effect::ThumbnailDemandsChanged]
                }
                Err(error) => {
                    log::warn!("description fetch failed: {error:?}");
                    state.apply_fetch_failure();
                    Vec::new()
                }
            }
        }
        Message::SourceActionPerformed(action) => {
            // The editor is read-only while saving (see the view); drop any
            // action that slips through so the buffer can never diverge from
            // the submitted text before the auto-close on success.
            if state.saving() {
                return Vec::new();
            }
            let edited = action.is_edit();
            if edited {
                state.checkpoint();
            }
            state.perform(action);
            if edited {
                vec![Effect::ThumbnailDemandsChanged]
            } else {
                Vec::new()
            }
        }
        Message::ToolbarApplied(action) => apply_toolbar(state, action),
        Message::EnterPressed => apply_enter(state),
        Message::SpoilerToggled(id) => {
            state.toggle_spoiler(id);
            Vec::new()
        }
        Message::LinkOpenRequested(url) => normalize_description_url(&url)
            .map(Effect::OpenUrlRequested)
            .into_iter()
            .collect(),
        #[cfg(test)]
        Message::OpenDraftRequested { title, initial } => {
            let _generation = state.open_draft(title, &initial);
            vec![Effect::ModalOpenRequested, Effect::ThumbnailDemandsChanged]
        }
        Message::SaveRequested => {
            if !state.can_save() {
                return Vec::new();
            }
            if state.is_draft() {
                let language = state.language();
                let drafts = state.localized_drafts();
                return vec![
                    Effect::DraftStaged { language, drafts },
                    Effect::ModalCloseRequested,
                ];
            }
            state
                .next_save()
                .map(|edit| {
                    Effect::SaveRequested(SaveRequest {
                        edit,
                        generation: state.generation(),
                    })
                })
                .into_iter()
                .collect()
        }
        Message::SaveCompleted { generation, result } => {
            if generation != state.generation() || !state.saving() {
                return Vec::new();
            }
            let Ok(outcome) = result else {
                // The failure itself reaches the user through the task
                // overlay; the editor stays open with the text intact.
                state.fail_save();
                return vec![Effect::ThumbnailDemandsChanged];
            };
            let saved = state.mark_saved();
            let mut effects = Vec::with_capacity(3);
            if let Some(workshop_id) = state.workshop_id()
                && let Some((language, text)) = saved
            {
                effects.push(Effect::MetadataSaved {
                    workshop_id,
                    language,
                    text,
                });
            }
            // Same answer as the publish and icon flows: the revision went
            // through, but Steam wants the agreement accepted.
            if outcome.legal_agreement_required {
                effects.push(Effect::OpenUrlRequested(WORKSHOP_LEGAL_URL.to_owned()));
            }
            if let Some(edit) = state.next_save() {
                effects.push(Effect::SaveRequested(SaveRequest {
                    edit,
                    generation: state.generation(),
                }));
            } else {
                effects.push(Effect::ModalCloseRequested);
            }
            effects
        }
        Message::CloseRequested => {
            if state.saving() {
                return Vec::new();
            }
            if state.dirty() {
                state.set_confirm_discard(true);
                Vec::new()
            } else {
                vec![Effect::ModalCloseRequested]
            }
        }
        Message::DiscardConfirmed => {
            if state.saving() {
                Vec::new()
            } else {
                vec![Effect::ModalCloseRequested]
            }
        }
        Message::DiscardCancelled => {
            state.set_confirm_discard(false);
            Vec::new()
        }
        Message::CloseFinished => {
            state.close();
            vec![Effect::ThumbnailDemandsChanged]
        }
        Message::AnimationTick => {
            let _changed = state.advance_animations(now);
            Vec::new()
        }
    }
}

#[cfg(test)]
fn update(state: &mut State, message: Message) -> Vec<Effect> {
    update_at(state, message, Instant::now())
}

fn apply_toolbar(state: &mut State, action: ToolbarAction) -> Vec<Effect> {
    if !state.is_open() || state.loading() || state.saving() {
        return Vec::new();
    }
    if state.load_failed() {
        return Vec::new();
    }
    state.checkpoint();
    let selection = state.selection();
    let plan = markup::plan(action, selection.as_deref());
    state.perform(Action::Edit(Edit::Paste(Arc::new(plan.insert))));
    for _ in 0..plan.caret_back {
        state.perform(Action::Move(Motion::Left));
    }
    vec![Effect::ThumbnailDemandsChanged]
}

fn apply_enter(state: &mut State) -> Vec<Effect> {
    if state.saving() || state.loading() || state.load_failed() {
        return Vec::new();
    }
    state.checkpoint();
    // With a selection, Enter is a plain replace-with-newline: the marker
    // behaviors are planned from the caret's line, which deleting the
    // selection may change or remove entirely.
    let behavior = if state
        .selection()
        .is_some_and(|selection| !selection.is_empty())
    {
        EnterBehavior::Plain
    } else {
        state
            .cursor_line()
            .map_or(EnterBehavior::Plain, |(line, caret)| {
                markup::enter_behavior(&line, caret)
            })
    };
    match behavior {
        EnterBehavior::Plain => state.perform(Action::Edit(Edit::Enter)),
        EnterBehavior::ContinueList => {
            state.perform(Action::Edit(Edit::Enter));
            state.perform(Action::Edit(Edit::Paste(Arc::new("[*] ".to_owned()))));
        }
        // Enter on a bare marker dissolves it: the emptied line ends the
        // list, exactly like Steam's blank-line rule.
        EnterBehavior::ClearMarker => {
            state.perform(Action::Move(Motion::End));
            state.perform(Action::Select(Motion::Home));
            state.perform(Action::Edit(Edit::Backspace));
        }
    }
    vec![Effect::ThumbnailDemandsChanged]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::domain::PublishedFileId;
    use crate::features::description_editor::message::{FetchedSource, SaveOutcome};

    fn opened_state() -> State {
        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::OpenRequested {
                workshop_id: PublishedFileId::fixture(42),
                title: None,
            },
        );
        let generation = state.generation();
        let _effects = update(
            &mut state,
            Message::SourceFetched {
                generation,
                result: Ok(FetchedSource {
                    title: "Addon".to_owned(),
                    description: "Hello [b]world[/b]".to_owned(),
                }),
            },
        );
        state
    }

    #[test]
    fn open_fetches_the_live_description_before_editing() {
        let mut state = State::default();
        let effects = update(
            &mut state,
            Message::OpenRequested {
                workshop_id: PublishedFileId::fixture(42),
                title: Some("Addon".to_owned()),
            },
        );

        assert!(matches!(effects[0], Effect::ModalOpenRequested));
        assert!(matches!(effects[1], Effect::SourceFetchRequested(_)));
        assert!(state.loading());
        assert!(!state.can_save());
    }

    /// Both the current backend default and the one earlier builds wrote:
    /// items created by old versions still carry the legacy text.
    #[test]
    fn placeholder_descriptions_open_as_an_empty_editor() {
        for placeholder in super::super::state::PLACEHOLDER_DESCRIPTIONS {
            let mut state = State::default();
            let _effects = update(
                &mut state,
                Message::OpenRequested {
                    workshop_id: PublishedFileId::fixture(42),
                    title: None,
                },
            );
            let generation = state.generation();
            let _effects = update(
                &mut state,
                Message::SourceFetched {
                    generation,
                    result: Ok(FetchedSource {
                        title: "Addon".to_owned(),
                        description: placeholder.to_owned(),
                    }),
                },
            );

            assert!(state.source_is_empty(), "{placeholder:?}");
            assert!(!state.dirty(), "{placeholder:?}");
        }
    }

    #[test]
    fn stale_fetches_are_ignored() {
        let mut state = opened_state();
        let stale = state.generation().next();
        let _effects = update(
            &mut state,
            Message::SourceFetched {
                generation: stale,
                result: Ok(FetchedSource {
                    title: "Stale".to_owned(),
                    description: "stale".to_owned(),
                }),
            },
        );

        assert_eq!(state.title(), Some("Addon"));
    }

    #[test]
    fn toolbar_wraps_typed_text_and_marks_the_state_dirty() {
        let mut state = opened_state();
        let _effects = update(&mut state, Message::ToolbarApplied(ToolbarAction::Bold));
        let text = state.source().expect("open session").text();

        assert!(text.contains("[b][/b]"), "{text:?}");
        // Typing at the caret lands inside the pair.
        let _effects = update(
            &mut state,
            Message::SourceActionPerformed(Action::Edit(Edit::Insert('x'))),
        );
        let text = state.source().expect("open session").text();
        assert!(text.contains("[b]x[/b]"), "{text:?}");
        assert!(state.dirty());
        assert!(state.can_save());
    }

    #[test]
    fn enter_continues_list_items_and_dissolves_bare_markers() {
        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::OpenRequested {
                workshop_id: PublishedFileId::fixture(42),
                title: None,
            },
        );
        let generation = state.generation();
        let _effects = update(
            &mut state,
            Message::SourceFetched {
                generation,
                result: Ok(FetchedSource {
                    title: "Addon".to_owned(),
                    description: String::new(),
                }),
            },
        );
        let _effects = update(
            &mut state,
            Message::SourceActionPerformed(Action::Edit(Edit::Paste(Arc::new(
                "[*] first".to_owned(),
            )))),
        );

        let _effects = update(&mut state, Message::EnterPressed);
        let text = state.source().expect("open session").text();
        assert!(text.ends_with("[*] first\n[*] "), "{text:?}");

        // Enter again on the bare marker clears it instead of stacking.
        let _effects = update(&mut state, Message::EnterPressed);
        let text = state.source().expect("open session").text();
        assert!(text.ends_with("[*] first\n"), "{text:?}");
    }

    /// Enter with an active selection must replace the selection with a
    /// newline; the `ClearMarker` motion sequence would otherwise collapse
    /// the selection and leave the selected text standing.
    #[test]
    fn enter_with_a_selection_replaces_it_with_a_newline() {
        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::OpenDraftRequested {
                title: "Addon".to_owned(),
                initial: "abcdef\n[*] ".to_owned(),
            },
        );
        let _effects = update(
            &mut state,
            Message::SourceActionPerformed(Action::SelectAll),
        );

        let _effects = update(&mut state, Message::EnterPressed);
        let text = state.source().expect("open session").text();
        assert_eq!(text, "\n");
    }

    /// A delivery landing after the edit that removed its tag must be
    /// dropped: an invisible stored entry would otherwise keep state (and,
    /// for GIFs, the animation subscription) alive until the next edit.
    #[test]
    fn deliveries_for_no_longer_referenced_urls_are_dropped() {
        use crate::media::{
            thumbnail_demand,
            thumbnail_worker::{ThumbnailDecodeError, ThumbnailError, ThumbnailInput},
        };

        let referenced = "https://example.com/a.png";
        let removed = "https://example.com/removed.png";
        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::OpenDraftRequested {
                title: "Addon".to_owned(),
                initial: format!("[img]{referenced}[/img]"),
            },
        );

        let delivery_for = |url: &str, generation| thumbnail_demand::Delivery {
            owner: thumbnail_demand::Owner::DescriptionEditor,
            generation,
            id: thumbnail_demand::DemandId::row(url),
            key: ThumbnailInput::from_url(url).cache_key(1),
            result: thumbnail_demand::DeliveryResult::Failed {
                error: thumbnail_demand::ThumbnailDeliveryError::Thumbnail(Arc::new(
                    ThumbnailError::Decode(ThumbnailDecodeError::InvalidMaxEdge),
                )),
            },
        };

        let generation = state.generation();
        assert!(state.apply_thumbnail_delivery(&delivery_for(referenced, generation)));
        assert!(!state.apply_thumbnail_delivery(&delivery_for(removed, generation)));
    }

    /// Non-http sources are never demanded, so an absent media entry must
    /// read as "unavailable" (link fallback), not as loading forever.
    #[test]
    fn non_http_image_sources_render_as_link_fallbacks_not_loading() {
        use crate::widgets::bbcode::{MediaLookup, MediaView};

        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::OpenDraftRequested {
                title: "Addon".to_owned(),
                initial: "[img]C:\\not\\a\\url.png[/img][img]https://example.com/a.png[/img]"
                    .to_owned(),
            },
        );

        assert!(matches!(
            state.media("C:\\not\\a\\url.png"),
            MediaView::Unavailable
        ));
        assert!(matches!(
            state.media("https://example.com/a.png"),
            MediaView::Loading
        ));
    }

    /// The editor is read-only while a save is in flight; anything typed
    /// after the submitted snapshot would be discarded by the auto-close.
    #[test]
    fn edits_are_ignored_while_a_save_is_in_flight() {
        let mut state = opened_state();
        let _effects = update(
            &mut state,
            Message::SourceActionPerformed(Action::Edit(Edit::Insert('!'))),
        );
        let _effects = update(&mut state, Message::SaveRequested);
        assert!(state.saving());

        let submitted = state.source().expect("open session").text();
        let _effects = update(
            &mut state,
            Message::SourceActionPerformed(Action::Edit(Edit::Insert('x'))),
        );
        let _effects = update(&mut state, Message::ToolbarApplied(ToolbarAction::Bold));
        let _effects = update(&mut state, Message::EnterPressed);
        assert_eq!(state.source().expect("open session").text(), submitted);
    }

    #[test]
    fn dirty_close_asks_before_discarding() {
        let mut state = opened_state();
        let _effects = update(&mut state, Message::ToolbarApplied(ToolbarAction::Bold));
        assert!(state.dirty());

        let effects = update(&mut state, Message::CloseRequested);
        assert!(effects.is_empty());
        assert!(state.confirming_discard());

        let effects = update(&mut state, Message::DiscardConfirmed);
        assert_eq!(effects, vec![Effect::ModalCloseRequested]);
    }

    #[test]
    fn clean_close_needs_no_confirmation() {
        let mut state = opened_state();
        let effects = update(&mut state, Message::CloseRequested);
        assert_eq!(effects, vec![Effect::ModalCloseRequested]);
    }

    #[test]
    fn save_submits_trimmed_text_and_closes_on_success() {
        let mut state = opened_state();
        let _effects = update(
            &mut state,
            Message::SourceActionPerformed(Action::Edit(Edit::Insert('!'))),
        );
        assert!(state.can_save());

        let effects = update(&mut state, Message::SaveRequested);
        let Some(Effect::SaveRequested(request)) = effects.first() else {
            panic!("expected a save effect, got {effects:?}");
        };
        assert!(
            request
                .edit
                .description
                .as_ref()
                .unwrap()
                .value
                .contains('!')
        );
        assert!(state.saving());

        // A second request while in flight is refused.
        assert!(update(&mut state, Message::SaveRequested).is_empty());

        let generation = state.generation();
        let effects = update(
            &mut state,
            Message::SaveCompleted {
                generation,
                result: Ok(SaveOutcome {
                    legal_agreement_required: false,
                }),
            },
        );
        assert!(matches!(effects.last(), Some(Effect::ModalCloseRequested)));
        assert!(!state.dirty());
    }

    /// Steam can accept a revision while the account still owes the Workshop
    /// legal agreement; the editor answers exactly like the publish and icon
    /// flows — by opening the agreement page.
    #[test]
    fn saves_needing_the_legal_agreement_open_the_agreement_page() {
        let mut state = opened_state();
        let _effects = update(
            &mut state,
            Message::SourceActionPerformed(Action::Edit(Edit::Insert('!'))),
        );
        let _effects = update(&mut state, Message::SaveRequested);

        let generation = state.generation();
        let effects = update(
            &mut state,
            Message::SaveCompleted {
                generation,
                result: Ok(SaveOutcome {
                    legal_agreement_required: true,
                }),
            },
        );
        assert_eq!(
            effects,
            vec![
                Effect::MetadataSaved {
                    workshop_id: PublishedFileId::fixture(42),
                    language: state.language(),
                    text: state
                        .localized_drafts()
                        .get(&state.language())
                        .unwrap()
                        .clone()
                },
                Effect::OpenUrlRequested(WORKSHOP_LEGAL_URL.to_owned()),
                Effect::ModalCloseRequested,
            ]
        );
    }

    #[test]
    fn failed_saves_keep_the_editor_open_and_editable() {
        let mut state = opened_state();
        let _effects = update(
            &mut state,
            Message::SourceActionPerformed(Action::Edit(Edit::Insert('!'))),
        );
        let _effects = update(&mut state, Message::SaveRequested);

        let generation = state.generation();
        let effects = update(
            &mut state,
            Message::SaveCompleted {
                generation,
                result: Err(crate::bridge::ui_error::UiError::new(
                    gmpublished_backend::error_keys::STEAM_ERROR,
                )),
            },
        );
        assert_eq!(effects, vec![Effect::ThumbnailDemandsChanged]);
        assert!(!state.saving());
        assert!(state.dirty());
    }

    #[test]
    fn a_failed_fetch_refuses_to_save_over_the_live_description() {
        let mut state = State::default();
        let _effects = update(
            &mut state,
            Message::OpenRequested {
                workshop_id: PublishedFileId::fixture(42),
                title: None,
            },
        );
        let generation = state.generation();
        let _effects = update(
            &mut state,
            Message::SourceFetched {
                generation,
                result: Err(crate::bridge::ui_error::UiError::new(
                    gmpublished_backend::error_keys::STEAM_ERROR,
                )),
            },
        );

        assert!(state.load_failed());
        assert!(!state.can_save());
        assert!(update(&mut state, Message::SaveRequested).is_empty());
    }
}

#[cfg(test)]
mod localization_tests {
    use super::*;
    use crate::bridge::{
        domain::PublishedFileId,
        ui_error::UiError,
        workshop_text::{LocalizedDrafts, WorkshopLanguage as Language, WorkshopText},
    };
    use crate::features::description_editor::{FetchedSource, SaveOutcome};

    fn fetch(state: &mut State, title: &str, description: &str) {
        let generation = state.generation();
        update(
            state,
            Message::SourceFetched {
                generation,
                result: Ok(FetchedSource {
                    title: title.into(),
                    description: description.into(),
                }),
            },
        );
    }
    fn open() -> State {
        let mut state = State::default();
        update(
            &mut state,
            Message::OpenRequested {
                workshop_id: PublishedFileId::fixture(42),
                title: None,
            },
        );
        fetch(&mut state, "English", "original");
        state
    }
    fn replace(state: &mut State, text: &str) {
        update(state, Message::SourceActionPerformed(Action::SelectAll));
        update(
            state,
            Message::SourceActionPerformed(Action::Edit(Edit::Paste(Arc::new(text.into())))),
        );
    }
    fn complete(state: &mut State, result: Result<SaveOutcome, UiError>) -> Vec<Effect> {
        let generation = state.generation();
        update(state, Message::SaveCompleted { generation, result })
    }
    #[test]
    fn fetches_only_selected_languages_and_preserves_drafts_across_switches() {
        let mut state = open();
        replace(&mut state, "English draft");
        let effects = update(&mut state, Message::LanguageSelected(Language::French));
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, Effect::SourceFetchRequested(_)))
                .count(),
            1
        );
        let stale = state.generation();
        update(&mut state, Message::LanguageSelected(Language::Russian));
        update(
            &mut state,
            Message::SourceFetched {
                generation: stale,
                result: Ok(FetchedSource {
                    title: "Late French".into(),
                    description: "late".into(),
                }),
            },
        );
        assert!(state.loading());
        fetch(&mut state, "Russian", "русский");
        replace(&mut state, "русский черновик");
        let effects = update(&mut state, Message::LanguageSelected(Language::English));
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::SourceFetchRequested(_)))
        );
        assert_eq!(state.source().unwrap().text(), "English draft");
        let effects = update(&mut state, Message::LanguageSelected(Language::Russian));
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::SourceFetchRequested(_)))
        );
        assert_eq!(state.source().unwrap().text(), "русский черновик");
        let effects = update(&mut state, Message::LanguageSelected(Language::French));
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::SourceFetchRequested(_)))
        );
    }
    #[test]
    fn serializes_changed_languages_and_retries_only_failures() {
        let mut state = open();
        update(&mut state, Message::TitleEdited("New English title".into()));
        update(&mut state, Message::LanguageSelected(Language::French));
        fetch(&mut state, "French title", "bonjour");
        replace(&mut state, "nouveau");
        let effects = update(&mut state, Message::SaveRequested);
        let Effect::SaveRequested(first) = &effects[0] else {
            panic!("save required")
        };
        assert_eq!(first.edit.language, Language::French);
        assert!(first.edit.title.is_none());
        assert_eq!(
            first.edit.description.as_ref().unwrap().previous.as_deref(),
            Some("bonjour")
        );
        assert!(update(&mut state, Message::CloseRequested).is_empty());
        assert!(update(&mut state, Message::DiscardConfirmed).is_empty());
        update(&mut state, Message::LanguageSelected(Language::Turkish));
        assert_eq!(state.language(), Language::French);
        let effects = complete(
            &mut state,
            Ok(SaveOutcome {
                legal_agreement_required: false,
            }),
        );
        let second = effects
            .iter()
            .find_map(|effect| {
                if let Effect::SaveRequested(request) = effect {
                    Some(request)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(second.edit.language, Language::English);
        assert!(second.edit.description.is_none());
        complete(
            &mut state,
            Err(UiError::new(gmpublished_backend::error_keys::STEAM_ERROR)),
        );
        assert_eq!(state.language(), Language::English);
        assert!(state.dirty());
        let effects = update(&mut state, Message::SaveRequested);
        let Effect::SaveRequested(retry) = &effects[0] else {
            panic!("retry required")
        };
        assert_eq!(retry.edit.language, Language::English);
        let effects = complete(
            &mut state,
            Ok(SaveOutcome {
                legal_agreement_required: false,
            }),
        );
        assert!(matches!(effects.last(), Some(Effect::ModalCloseRequested)));
        assert!(!state.dirty());
    }
    #[test]
    fn repeated_close_never_discards_dirty_languages() {
        let mut state = open();
        replace(&mut state, "draft");
        update(&mut state, Message::LanguageSelected(Language::French));
        fetch(&mut state, "French", "bonjour");
        for _ in 0..3 {
            assert!(update(&mut state, Message::CloseRequested).is_empty());
            assert!(state.confirming_discard());
        }
        assert_eq!(
            update(&mut state, Message::DiscardConfirmed),
            vec![Effect::ModalCloseRequested]
        );
    }
    #[test]
    fn new_item_stages_multiple_languages_without_fetching_or_submitting() {
        let mut state = State::default();
        let drafts: LocalizedDrafts = [(
            Language::English,
            WorkshopText {
                title: "Addon".into(),
                description: "English".into(),
            },
        )]
        .into();
        let effects = update(
            &mut state,
            Message::OpenLocalizedDraftRequested {
                language: Language::English,
                drafts,
            },
        );
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::SourceFetchRequested(_)))
        );
        let effects = update(&mut state, Message::LanguageSelected(Language::French));
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::SourceFetchRequested(_)))
        );
        update(&mut state, Message::TitleEdited("Titre français".into()));
        replace(&mut state, "Bonjour");
        let effects = update(&mut state, Message::SaveRequested);
        let Effect::DraftStaged { language, drafts } = &effects[0] else {
            panic!("stage expected")
        };
        assert_eq!(*language, Language::French);
        assert_eq!(drafts[&Language::English].description, "English");
        assert_eq!(drafts[&Language::French].title, "Titre français");
    }
    #[test]
    fn undo_redo_treat_toolbar_insert_as_one_edit() {
        let mut state = open();
        update(&mut state, Message::ToolbarApplied(ToolbarAction::Bold));
        let formatted = state.source().unwrap().text();
        assert!(state.dirty());
        update(&mut state, Message::Undo);
        assert_eq!(state.source().unwrap().text(), "original");
        assert!(!state.dirty());
        update(&mut state, Message::Redo);
        assert_eq!(state.source().unwrap().text(), formatted);
        update(&mut state, Message::Undo);
        replace(&mut state, "different");
        update(&mut state, Message::Redo);
        assert_eq!(state.source().unwrap().text(), "different");
    }
    #[test]
    fn inactive_invalid_translation_blocks_save_without_losing_other_drafts() {
        let mut state = open();
        replace(&mut state, "English draft");
        update(&mut state, Message::LanguageSelected(Language::Turkish));
        fetch(&mut state, "Türkçe", "açıklama");
        update(&mut state, Message::TitleEdited("ş".repeat(65)));
        update(&mut state, Message::LanguageSelected(Language::English));
        assert_eq!(state.invalid_language(), Some(Language::Turkish));
        assert!(!state.can_save());
        assert!(update(&mut state, Message::SaveRequested).is_empty());
    }
    #[test]
    fn placeholder_previous_value_is_preserved_for_stale_read_reconciliation() {
        let mut state = open();
        let placeholder = gmpublished_backend::publishing::WORKSHOP_DEFAULT_DESCRIPTION;
        update(
            &mut state,
            Message::OpenRequested {
                workshop_id: PublishedFileId::fixture(42),
                title: None,
            },
        );
        fetch(&mut state, "Addon", placeholder);
        replace(&mut state, "Real description");
        let effects = update(&mut state, Message::SaveRequested);
        let Effect::SaveRequested(request) = &effects[0] else {
            panic!("save expected")
        };
        assert_eq!(
            request
                .edit
                .description
                .as_ref()
                .unwrap()
                .previous
                .as_deref(),
            Some(placeholder)
        );
    }
    #[test]
    fn retry_is_one_shot_and_late_results_cannot_replace_drafts() {
        let mut state = open();
        update(&mut state, Message::LanguageSelected(Language::French));
        let generation = state.generation();
        update(
            &mut state,
            Message::SourceFetched {
                generation,
                result: Err(UiError::new(gmpublished_backend::error_keys::STEAM_ERROR)),
            },
        );
        assert_eq!(update(&mut state, Message::RetryFetchRequested).len(), 1);
        assert!(update(&mut state, Message::RetryFetchRequested).is_empty());
        fetch(&mut state, "Français", "bonjour");
        replace(&mut state, "mon brouillon");
        fetch(&mut state, "Late", "late remote");
        assert_eq!(state.source().unwrap().text(), "mon brouillon");
    }
    #[test]
    fn each_save_has_its_own_generation_and_reports_its_actual_language() {
        let mut state = open();
        replace(&mut state, "new English");
        update(&mut state, Message::LanguageSelected(Language::French));
        fetch(&mut state, "Français", "bonjour");
        replace(&mut state, "nouveau");
        update(&mut state, Message::SaveRequested);
        let first_generation = state.generation();
        let effects = complete(
            &mut state,
            Ok(SaveOutcome {
                legal_agreement_required: false,
            }),
        );
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::MetadataSaved {
                language: Language::French,
                ..
            }
        )));
        assert_ne!(state.generation(), first_generation);
        assert!(
            update(
                &mut state,
                Message::SaveCompleted {
                    generation: first_generation,
                    result: Ok(SaveOutcome {
                        legal_agreement_required: false
                    }),
                }
            )
            .is_empty()
        );
        assert!(state.saving());
        let effects = complete(
            &mut state,
            Ok(SaveOutcome {
                legal_agreement_required: false,
            }),
        );
        assert!(effects.iter().any(|effect| matches!(effect, Effect::MetadataSaved { language: Language::English, text, .. } if text.description == "new English")));
        assert!(!state.dirty());
    }
    #[test]
    fn unicode_undo_restores_selection_and_language_switch_preserves_caret() {
        use iced::widget::text_editor::Motion;
        let mut state = open();
        replace(&mut state, "世界 · français\nрусский · Türkçe");
        update(
            &mut state,
            Message::SourceActionPerformed(Action::Move(Motion::DocumentStart)),
        );
        update(
            &mut state,
            Message::SourceActionPerformed(Action::Move(Motion::Right)),
        );
        update(
            &mut state,
            Message::SourceActionPerformed(Action::Select(Motion::Right)),
        );
        let cursor = state.source().unwrap().content().cursor();
        let selected = state.source().unwrap().selection().unwrap();
        update(&mut state, Message::ToolbarApplied(ToolbarAction::Bold));
        update(&mut state, Message::Undo);
        assert_eq!(
            state.source().unwrap().selection().as_deref(),
            Some(selected.as_str())
        );
        assert_eq!(state.source().unwrap().content().cursor(), cursor);
        update(&mut state, Message::LanguageSelected(Language::French));
        fetch(&mut state, "Français", "Bonjour");
        update(&mut state, Message::LanguageSelected(Language::English));
        assert_eq!(state.source().unwrap().content().cursor(), cursor);
        assert_eq!(
            state.source().unwrap().text(),
            "世界 · français\nрусский · Türkçe"
        );
    }
    #[test]
    fn horizontal_split_clamps_both_panes_and_resets_without_changing_drafts() {
        let mut state = open();
        replace(&mut state, "unsaved text");
        update(&mut state, Message::PanesLayoutChanged(704.0));
        let split = *state.panes().layout().splits().next().unwrap();
        update(&mut state, Message::PanesResized { split, ratio: 0.01 });
        assert!((state.source_pane_width() - 240.0).abs() < 0.1);
        update(&mut state, Message::PanesResized { split, ratio: 0.99 });
        assert!((702.0 - state.source_pane_width() - 280.0).abs() < 0.1);
        update(&mut state, Message::PanesLayoutChanged(560.0));
        assert!(state.source_pane_width() >= 240.0);
        assert!(558.0 - state.source_pane_width() >= 279.9);
        update(&mut state, Message::PanesReset);
        assert!((state.source_pane_width() - 278.0).abs() < 0.1);
        assert_eq!(state.source().unwrap().text(), "unsaved text");
    }
}
