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
        Message::OpenRequested { workshop_id, title } => {
            let generation = state.open(workshop_id, title);
            vec![
                Effect::ModalOpenRequested,
                Effect::SourceFetchRequested(SourceRequest {
                    workshop_id,
                    generation,
                }),
            ]
        }
        Message::SourceFetched { generation, result } => {
            if generation != state.generation() {
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
        Message::OpenDraftRequested { title, initial } => {
            let _generation = state.open_draft(title, &initial);
            vec![Effect::ModalOpenRequested, Effect::ThumbnailDemandsChanged]
        }
        Message::SaveRequested => {
            if !state.can_save() {
                return Vec::new();
            }
            let Some(description) = state.trimmed_source() else {
                return Vec::new();
            };
            if state.is_draft() {
                state.mark_saved();
                return vec![
                    Effect::DraftStaged(description),
                    Effect::ModalCloseRequested,
                ];
            }
            let Some(workshop_id) = state.workshop_id() else {
                return Vec::new();
            };
            state.set_saving(true);
            vec![Effect::SaveRequested(SaveRequest {
                workshop_id,
                description,
                generation: state.generation(),
            })]
        }
        Message::SaveCompleted { generation, result } => {
            if generation != state.generation() {
                return Vec::new();
            }
            let Ok(outcome) = result else {
                // The failure itself reaches the user through the task
                // overlay; the editor stays open with the text intact.
                state.set_saving(false);
                return Vec::new();
            };
            state.mark_saved();
            let mut effects = Vec::with_capacity(2);
            // Same answer as the publish and icon flows: the revision went
            // through, but Steam wants the agreement accepted.
            if outcome.legal_agreement_required {
                effects.push(Effect::OpenUrlRequested(WORKSHOP_LEGAL_URL.to_owned()));
            }
            effects.push(Effect::ModalCloseRequested);
            effects
        }
        Message::CloseRequested => {
            if state.dirty() && !state.confirming_discard() {
                state.set_confirm_discard(true);
                Vec::new()
            } else {
                vec![Effect::ModalCloseRequested]
            }
        }
        Message::DiscardConfirmed => vec![Effect::ModalCloseRequested],
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
    let selection = state.selection();
    let plan = markup::plan(action, selection.as_deref());
    state.perform(Action::Edit(Edit::Paste(Arc::new(plan.insert))));
    for _ in 0..plan.caret_back {
        state.perform(Action::Move(Motion::Left));
    }
    vec![Effect::ThumbnailDemandsChanged]
}

fn apply_enter(state: &mut State) -> Vec<Effect> {
    if state.saving() {
        return Vec::new();
    }
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
        assert!(request.description.contains('!'));
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
        assert_eq!(effects, vec![Effect::ModalCloseRequested]);
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
        assert!(effects.is_empty());
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
