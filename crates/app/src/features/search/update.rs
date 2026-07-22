use std::time::Instant;

use iced::{Event, Subscription, event, keyboard};

use super::{Effect, Message, State};
use crate::bridge::tasks::TaskId;

pub const SEARCH_INPUT_ID: &str = "search-palette-input";

pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    match message {
        Message::QueryEdited(query) => {
            let palette_open_before = state.palette_open();
            let outcome = state.edit_query(query);
            let mut effects = Vec::new();
            let palette_changed =
                append_palette_transition_effect(&mut effects, palette_open_before, state);
            append_cancel_effect(&mut effects, outcome.cancel_task);
            if let Some(request) = outcome.quick_request {
                effects.push(Effect::QuickSearchDebounceRequested(request));
            }
            if state.palette_open() && !palette_changed {
                effects.push(Effect::ThumbnailDemandsChanged);
            }
            effects
        }
        Message::FocusRequested => {
            let palette_open_before = state.palette_open();
            let _changed = state.focus(Instant::now());
            let mut effects = vec![Effect::FocusInputRequested];
            let _palette_changed =
                append_palette_transition_effect(&mut effects, palette_open_before, state);
            append_full_search_effect_if_needed(&mut effects, state);
            effects
        }
        Message::ModeFocusRequested(mode) => {
            let palette_open_before = state.palette_open();
            let outcome = state.focus_mode(mode, Instant::now());
            let mut effects = vec![Effect::FocusInputRequested];
            let palette_changed =
                append_palette_transition_effect(&mut effects, palette_open_before, state);
            append_cancel_effect(&mut effects, outcome.cancel_task);
            if outcome.mode_changed && !palette_changed {
                effects.push(Effect::ThumbnailDemandsChanged);
            }
            append_full_search_effect_if_needed(&mut effects, state);
            effects
        }
        Message::DropdownScrolled(offset) => {
            let mut effects = Vec::new();
            if state.set_scroll_offset(offset) {
                effects.push(Effect::ThumbnailDemandsChanged);
            }
            append_full_search_effect_if_needed(&mut effects, state);
            effects
        }
        Message::QuickDebounced(request) => state
            .take_debounced_request(&request)
            .map_or_else(Vec::new, |request| {
                vec![Effect::QuickSearchRequested(request)]
            }),
        Message::QuickSearchCompleted(key, result) => {
            if state.apply_quick_result(&key, result) {
                let mut effects = vec![Effect::ThumbnailDemandsChanged];
                append_full_search_effect_if_needed(&mut effects, state);
                effects
            } else {
                Vec::new()
            }
        }
        Message::FullSearchBatchReceived(batch) => {
            if state.apply_full_batch(batch) {
                vec![Effect::ThumbnailDemandsChanged]
            } else {
                Vec::new()
            }
        }
        Message::FullSearchFinished(request) => {
            let _changed = state.finish_full_search(&request);
            Vec::new()
        }
        Message::MetadataCompleted(generation, item_ids, result) => {
            let completion = state.finish_metadata_request(generation, &item_ids, result);
            let mut effects = Vec::new();
            if !completion.stale_ids.is_empty() {
                effects.push(Effect::MetadataRefreshRequested {
                    generation,
                    item_ids: completion.stale_ids,
                });
            }
            if completion.changed {
                effects.push(Effect::ThumbnailDemandsChanged);
            }
            effects
        }
        Message::MetadataRefreshCompleted(generation, item_ids, result) => {
            if state.apply_metadata_refresh(generation, &item_ids, result) {
                vec![Effect::ThumbnailDemandsChanged]
            } else {
                Vec::new()
            }
        }
        Message::ResultActivated(row_id) => {
            let mut effects = vec![Effect::ResultActivated(row_id)];
            append_dismiss_effects(&mut effects, state);
            effects
        }
        Message::DismissRequested => {
            let mut effects = Vec::new();
            append_dismiss_effects(&mut effects, state);
            effects
        }
        Message::EscapePressed => {
            let mut effects = Vec::new();
            if state.input().is_empty() {
                append_dismiss_effects(&mut effects, state);
            } else {
                append_cancel_effect(&mut effects, state.clear());
                effects.push(Effect::ThumbnailDemandsChanged);
            }
            effects
        }
        Message::FullSearchSubmitted => {
            let mut effects = Vec::new();
            append_full_search_effect_if_needed(&mut effects, state);
            effects
        }
    }
}

pub fn subscription(state: &State) -> Subscription<Message> {
    if state.palette_open() {
        event::listen_with(keyboard_event)
    } else {
        Subscription::none()
    }
}

fn append_dismiss_effects(effects: &mut Vec<Effect>, state: &mut State) {
    let palette_open_before = state.palette_open();
    let cancel_task = state.dismiss(Instant::now());
    append_palette_transition_effect(effects, palette_open_before, state);
    append_cancel_effect(effects, cancel_task);
}

fn append_cancel_effect(effects: &mut Vec<Effect>, task_id: Option<TaskId>) {
    if let Some(task_id) = task_id {
        effects.push(Effect::TaskCancellationRequested(task_id));
    }
}

fn append_full_search_effect_if_needed(effects: &mut Vec<Effect>, state: &State) {
    if state.should_begin_full_search() {
        effects.push(Effect::FullSearchRequested);
    }
}

fn append_palette_transition_effect(
    effects: &mut Vec<Effect>,
    palette_open_before: bool,
    state: &State,
) -> bool {
    if !palette_open_before && state.palette_open() {
        effects.push(Effect::PaletteOpened);
        true
    } else if palette_open_before && !state.palette_open() {
        effects.push(Effect::PaletteDismissed);
        true
    } else {
        false
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "must match iced's fn(Event, Status, window::Id) callback signature required by event::listen_with"
)]
fn keyboard_event(
    event: Event,
    _status: event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        }) => Some(Message::EscapePressed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::bridge::domain::{
        PublishedFileId, SearchHit, SearchItem, SearchItemSource, SearchQuickBatch,
        SearchQuickCarry,
    };
    use crate::bridge::tasks::TaskId;
    use crate::features::search::state::{MetadataPatch, MetadataResolution};

    use super::{Effect, Message, State, update};

    #[test]
    fn escape_with_query_clears_and_keeps_palette_open() {
        let mut state = State::default();
        let _effects = update(&mut state, Message::QueryEdited("wire".to_owned()));

        let effects = update(&mut state, Message::EscapePressed);

        assert_eq!(state.input(), "");
        assert!(!state.dropdown_open());
        assert!(state.palette_open());
        assert_eq!(effects, vec![Effect::ThumbnailDemandsChanged]);
    }

    #[test]
    fn escape_with_empty_query_dismisses_palette() {
        let mut state = State::default();
        let _effects = update(&mut state, Message::FocusRequested);

        let effects = update(&mut state, Message::EscapePressed);

        assert!(!state.palette_open());
        assert_eq!(effects, vec![Effect::PaletteDismissed]);
    }

    #[test]
    fn escape_during_active_full_search_requests_cancellation() {
        let mut state = State::default();
        let request =
            quick_request_from(update(&mut state, Message::QueryEdited("alpha".to_owned())));
        let batch = SearchQuickBatch::new(
            request.key().clone(),
            vec![hit("Alpha", 42)],
            true,
            SearchQuickCarry::default(),
        );
        let _effects = update(
            &mut state,
            Message::QuickSearchCompleted(request.key().clone(), Ok(batch)),
        );
        let task_id = TaskId::from_raw(7);
        let _start = state.begin_full_search(task_id).expect("full search start");

        let effects = update(&mut state, Message::EscapePressed);

        assert_eq!(
            effects,
            vec![
                Effect::TaskCancellationRequested(task_id),
                Effect::ThumbnailDemandsChanged,
            ]
        );
    }

    #[test]
    fn editing_query_opens_palette_and_requests_debounced_quick_search() {
        let mut state = State::default();

        let effects = update(&mut state, Message::QueryEdited("wire".to_owned()));
        let request = match &effects[1] {
            Effect::QuickSearchDebounceRequested(request) => request.clone(),
            effect => panic!("expected quick debounce effect, got {effect:?}"),
        };

        assert_eq!(
            effects,
            vec![
                Effect::PaletteOpened,
                Effect::QuickSearchDebounceRequested(request.clone()),
            ]
        );
        assert_eq!(request.query(), "wire");
    }

    #[test]
    fn editing_open_palette_refreshes_thumbnail_demands_without_reopening() {
        let mut state = State::default();
        let _effects = update(&mut state, Message::FocusRequested);

        let effects = update(&mut state, Message::QueryEdited("wire".to_owned()));
        let request = match &effects[0] {
            Effect::QuickSearchDebounceRequested(request) => request.clone(),
            effect => panic!("expected quick debounce effect, got {effect:?}"),
        };

        assert_eq!(
            effects,
            vec![
                Effect::QuickSearchDebounceRequested(request),
                Effect::ThumbnailDemandsChanged,
            ]
        );
    }

    #[test]
    fn focus_requests_input_focus_and_palette_open_effect() {
        let mut state = State::default();

        let effects = update(&mut state, Message::FocusRequested);

        assert_eq!(
            effects,
            vec![Effect::FocusInputRequested, Effect::PaletteOpened]
        );
        assert!(state.palette_open());
    }

    #[test]
    fn stale_debounced_quick_search_emits_no_effect() {
        let mut state = State::default();
        let first =
            quick_request_from(update(&mut state, Message::QueryEdited("first".to_owned())));
        let _second = quick_request_from(update(
            &mut state,
            Message::QueryEdited("second".to_owned()),
        ));

        assert!(update(&mut state, Message::QuickDebounced(first)).is_empty());
    }

    #[test]
    fn fresh_debounced_quick_search_emits_worker_request_effect() {
        let mut state = State::default();
        let request =
            quick_request_from(update(&mut state, Message::QueryEdited("wire".to_owned())));

        let effects = update(&mut state, Message::QuickDebounced(request.clone()));

        assert_eq!(effects, vec![Effect::QuickSearchRequested(request)]);
    }

    #[test]
    fn accepted_quick_result_refreshes_thumbnails_and_requests_full_search_when_needed() {
        let mut state = State::default();
        let request =
            quick_request_from(update(&mut state, Message::QueryEdited("alpha".to_owned())));
        let batch = SearchQuickBatch::new(
            request.key().clone(),
            vec![hit("Alpha", 42)],
            true,
            SearchQuickCarry::default(),
        );

        let effects = update(
            &mut state,
            Message::QuickSearchCompleted(request.key().clone(), Ok(batch)),
        );

        assert_eq!(
            effects,
            vec![Effect::ThumbnailDemandsChanged, Effect::FullSearchRequested,]
        );
    }

    #[test]
    fn rejected_quick_result_emits_no_effect() {
        let mut state = State::default();
        let first =
            quick_request_from(update(&mut state, Message::QueryEdited("first".to_owned())));
        let second = quick_request_from(update(
            &mut state,
            Message::QueryEdited("second".to_owned()),
        ));
        let batch = SearchQuickBatch::new(
            first.key().clone(),
            vec![hit("First", 1)],
            true,
            SearchQuickCarry::default(),
        );

        assert!(
            update(
                &mut state,
                Message::QuickSearchCompleted(first.key().clone(), Ok(batch)),
            )
            .is_empty()
        );
        assert_eq!(
            update(&mut state, Message::QuickDebounced(second.clone())),
            vec![Effect::QuickSearchRequested(second)]
        );
    }

    #[test]
    fn metadata_completion_requests_stale_refresh_and_thumbnail_sync() {
        let mut state = State::default();
        let request =
            quick_request_from(update(&mut state, Message::QueryEdited("alpha".to_owned())));
        let batch = SearchQuickBatch::new(
            request.key().clone(),
            vec![hit("Alpha", 42)],
            false,
            SearchQuickCarry::default(),
        );
        let _effects = update(
            &mut state,
            Message::QuickSearchCompleted(request.key().clone(), Ok(batch)),
        );
        let (generation, ids) = state
            .take_thumbnail_metadata_request(600.0)
            .expect("metadata request");

        let effects = update(
            &mut state,
            Message::MetadataCompleted(
                generation,
                ids,
                Ok(MetadataResolution {
                    patches: vec![MetadataPatch::for_test(
                        PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
                        Some("https://example.test/alpha.png"),
                    )],
                    stale_ids: vec![
                        PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
                    ],
                }),
            ),
        );

        assert_eq!(
            effects,
            vec![
                Effect::MetadataRefreshRequested {
                    generation,
                    item_ids: vec![
                        PublishedFileId::new(42).expect("test fixture ids are always nonzero")
                    ],
                },
                Effect::ThumbnailDemandsChanged,
            ]
        );
    }

    #[test]
    fn full_submit_requests_full_search_only_when_state_allows_it() {
        let mut state = State::default();
        assert!(update(&mut state, Message::FullSearchSubmitted).is_empty());

        let request =
            quick_request_from(update(&mut state, Message::QueryEdited("alpha".to_owned())));
        let batch = SearchQuickBatch::new(
            request.key().clone(),
            vec![hit("Alpha", 42)],
            true,
            SearchQuickCarry::default(),
        );
        let _effects = update(
            &mut state,
            Message::QuickSearchCompleted(request.key().clone(), Ok(batch)),
        );

        assert_eq!(
            update(&mut state, Message::FullSearchSubmitted),
            vec![Effect::FullSearchRequested]
        );
    }

    #[test]
    fn result_activation_emits_activation_before_dismissal() {
        let mut state = State::default();
        let _effects = update(&mut state, Message::FocusRequested);

        let effects = update(&mut state, Message::ResultActivated(0));

        assert_eq!(
            effects,
            vec![Effect::ResultActivated(0), Effect::PaletteDismissed,]
        );
        assert!(!state.palette_open());
    }

    #[test]
    fn dismiss_when_closed_emits_no_effect() {
        let mut state = State::default();

        assert!(update(&mut state, Message::DismissRequested).is_empty());
    }

    fn quick_request_from(effects: Vec<Effect>) -> crate::bridge::domain::SearchQuickRequest {
        effects
            .into_iter()
            .find_map(|effect| match effect {
                Effect::QuickSearchDebounceRequested(request) => Some(request),
                _ => None,
            })
            .expect("quick search debounce request")
    }

    fn hit(label: &str, workshop_id: u64) -> SearchHit {
        SearchHit {
            score: 1,
            item: SearchItem::new(
                SearchItemSource::WorkshopItem(
                    PublishedFileId::new(workshop_id).expect("test fixture ids are always nonzero"),
                ),
                label,
                Vec::<String>::new(),
                0,
            ),
        }
    }
}
