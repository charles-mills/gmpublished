use crate::widgets::{addon_grid, grid_rows};

use super::{Effect, Message, State};

pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    match message {
        Message::RouteEntered => {
            let mut effects = Vec::new();
            append_page_request_effect(state.enter_route(), &mut effects);
            append_stats_refresh_effect(state.request_stats_refresh(), &mut effects);
            effects.push(Effect::ThumbnailDemandsChanged);
            effects
        }
        Message::RouteExited => {
            state.exit_route();
            vec![Effect::ThumbnailDemandsChanged]
        }
        Message::PageCompleted(generation, page, result) => {
            state.apply_page(generation, page, result);
            vec![Effect::ThumbnailDemandsChanged]
        }
        Message::StatsRefreshTick => stats_refresh_effects(state),
        Message::StatsRefreshCompleted(generation, result) => {
            state.apply_stats_counts(generation, result);
            Vec::new()
        }
        Message::CountRollTick(now) => {
            state.tick_count_rolls(now);
            Vec::new()
        }
        Message::AnimationTick(now) => {
            state.tick_visible_card_motion(now);
            state.tick_visible_animations(now);
            Vec::new()
        }
        #[cfg(feature = "debug")]
        Message::DebugSubscribersAdjusted { workshop_id, delta } => {
            state.adjust_subscription_count(workshop_id, delta);
            Vec::new()
        }
        Message::Grid(message) => grid_update(state, message),
    }
}

fn grid_update(state: &mut State, message: addon_grid::Message) -> Vec<Effect> {
    let mut effects = Vec::new();
    apply_grid_message(state, message, &mut effects);
    effects
}

fn apply_grid_message(state: &mut State, message: addon_grid::Message, effects: &mut Vec<Effect>) {
    match message {
        addon_grid::Message::CardClicked(id) => {
            if let Some(target) = state.take_prepare_publish_target(&id) {
                effects.push(Effect::PreparePublishRequested(target));
            }
        }
        addon_grid::Message::CardPressed(id) => {
            let workshop_id = state.workshop_id_for_card(&id);
            effects.push(Effect::AddonDragPressed {
                card_id: id,
                workshop_id,
            });
        }
        addon_grid::Message::CardReleased(_) => {
            effects.push(Effect::AddonDragReleased);
        }
        addon_grid::Message::CardContextRequested(id, position) => {
            if let Some(menu) = state.take_context_menu(&id, position) {
                effects.push(Effect::ContextMenuRequested(menu));
            }
        }
        addon_grid::Message::NextPageRequested => {
            append_page_request_effect(state.begin_next_page(), effects);
            effects.push(Effect::ThumbnailDemandsChanged);
        }
        addon_grid::Message::VisibleRangeChanged(_, _) => {
            state.reconcile_visible_counts();
            effects.push(Effect::ThumbnailDemandsChanged);
        }
        addon_grid::Message::CardHoverChanged(id, hovered) => {
            state.set_card_hovered(&id, hovered);
        }
        message => {
            let follow_ups = addon_grid::apply(state.grid_mut(), message);
            grid_rows::append_grid_follow_up_effects(
                state,
                follow_ups,
                effects,
                apply_grid_message,
            );
        }
    }
}

fn append_page_request_effect(request: Option<(u64, u32)>, effects: &mut Vec<Effect>) {
    if let Some((generation, page)) = request {
        effects.push(Effect::PageRequested { generation, page });
    }
}

fn append_stats_refresh_effect(request: Option<(u64, u32)>, effects: &mut Vec<Effect>) {
    if let Some((generation, pages)) = request {
        effects.push(Effect::StatsRefreshRequested { generation, pages });
    }
}

fn stats_refresh_effects(state: &mut State) -> Vec<Effect> {
    let mut effects = Vec::new();
    append_stats_refresh_effect(state.request_stats_refresh(), &mut effects);
    effects
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::model::{PUBLISH_NEW_ROW_ID, PageResult, PreparePublishTarget, Row};
    use super::{Effect, Message, State, update};
    use crate::backend::domain::PublishedFileId;
    use crate::widgets::addon_grid;

    #[test]
    fn route_entry_marks_page_visible_and_requests_first_page() {
        let mut state = State::default();

        let effects = update(&mut state, Message::RouteEntered);

        assert!(state.is_route_visible());
        assert_eq!(
            effects,
            vec![
                Effect::PageRequested {
                    generation: 1,
                    page: 1,
                },
                Effect::ThumbnailDemandsChanged,
            ]
        );
    }

    #[test]
    fn route_exit_hides_the_page() {
        let mut state = State::default();
        let _effects = update(&mut state, Message::RouteEntered);

        let effects = update(&mut state, Message::RouteExited);

        assert!(!state.is_route_visible());
        assert_eq!(effects, vec![Effect::ThumbnailDemandsChanged]);
    }

    #[test]
    fn grid_messages_update_scroll_state() {
        let mut state = State::default();

        let effects = update(&mut state, Message::Grid(addon_grid::Message::Scrolled(32)));

        assert_eq!(state.grid().scroll_offset(), 32.0);
        assert!(effects.is_empty());
    }

    #[test]
    fn page_completion_populates_rows_and_syncs_thumbnail_demands() {
        let mut state = State::default();
        let _effects = update(&mut state, Message::RouteEntered);

        let effects = update(
            &mut state,
            Message::PageCompleted(
                1,
                1,
                Ok(PageResult {
                    page: 1,
                    total: 1,
                    rows: vec![Row::for_test(42, "Addon", 10)],
                }),
            ),
        );

        assert_eq!(state.loaded_count(), 1);
        assert_eq!(state.total_count(), 1);
        assert_eq!(effects, vec![Effect::ThumbnailDemandsChanged]);
    }

    #[test]
    fn stats_tick_requests_loaded_page_refresh_once() {
        let mut state = State::default();
        state.push_rows_for_test(vec![Row::for_test(42, "Addon", 10)], 1);

        let effects = update(&mut state, Message::StatsRefreshTick);

        assert_eq!(
            effects,
            vec![Effect::StatsRefreshRequested {
                generation: 1,
                pages: 1,
            }]
        );
        assert!(update(&mut state, Message::StatsRefreshTick).is_empty());
    }

    #[test]
    fn stats_completion_with_visible_count_delta_starts_a_roll() {
        let mut state = State::default();
        state.push_rows_for_test(vec![Row::for_test(42, "Addon", 10)], 1);
        let _effects = update(
            &mut state,
            Message::Grid(addon_grid::Message::ViewportResized(500, 500)),
        );

        let effects = update(
            &mut state,
            Message::StatsRefreshCompleted(
                1,
                Ok(HashMap::from([(
                    PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
                    25,
                )])),
            ),
        );

        assert!(effects.is_empty());
        assert!(state.has_active_count_rolls());
    }

    #[test]
    fn stats_tick_without_loaded_pages_emits_no_effects() {
        let mut state = State::default();

        assert!(update(&mut state, Message::StatsRefreshTick).is_empty());
    }

    #[test]
    fn publish_new_click_emits_prepare_publish_effect() {
        let mut state = State::default();

        let effects = update(
            &mut state,
            Message::Grid(addon_grid::Message::CardClicked(
                PUBLISH_NEW_ROW_ID.to_owned(),
            )),
        );

        assert_eq!(
            effects,
            vec![Effect::PreparePublishRequested(PreparePublishTarget::New)]
        );
    }

    #[test]
    fn addon_click_emits_prepare_publish_update_effect() {
        let mut state = State::default();
        state.push_rows_for_test(vec![Row::for_test(42, "Addon", 10)], 1);

        let effects = update(
            &mut state,
            Message::Grid(addon_grid::Message::CardClicked("42".to_owned())),
        );

        match effects.as_slice() {
            [Effect::PreparePublishRequested(PreparePublishTarget::Update(update))] => {
                assert_eq!(
                    update.workshop_id,
                    PublishedFileId::new(42).expect("test fixture ids are always nonzero")
                );
                assert_eq!(update.title, "Addon");
            }
            other => panic!("expected update target effect, got {other:?}"),
        }
    }

    #[test]
    fn context_menu_emits_effect_when_target_exists() {
        let mut state = State::default();
        state.push_rows_for_test(vec![Row::for_test(42, "Addon", 10)], 1);

        let effects = update(
            &mut state,
            Message::Grid(addon_grid::Message::CardContextRequested(
                "42".to_owned(),
                iced::Point::new(10.0, 20.0),
            )),
        );

        assert!(matches!(
            effects.as_slice(),
            [Effect::ContextMenuRequested(menu)]
                if menu.workshop_id == PublishedFileId::new(42).expect("test fixture ids are always nonzero") && menu.position == iced::Point::new(10.0, 20.0)
        ));
    }

    #[cfg(feature = "debug")]
    #[test]
    fn debug_subscriber_adjustment_updates_the_target_and_starts_a_roll() {
        let mut state = State::default();
        state.push_rows_for_test(vec![Row::for_test(42, "Addon", 10)], 1);

        let effects = update(
            &mut state,
            Message::DebugSubscribersAdjusted {
                workshop_id: PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
                delta: 1_000_000,
            },
        );

        assert_eq!(
            state
                .row_for_test(42)
                .expect("target row should remain visible")
                .subscription_count_for_test(),
            1_000_010
        );
        assert!(effects.is_empty());
        assert!(state.has_active_count_rolls());
    }

    #[test]
    fn missing_card_click_and_context_menu_emit_no_effects() {
        let mut state = State::default();

        assert!(
            update(
                &mut state,
                Message::Grid(addon_grid::Message::CardClicked("missing".to_owned())),
            )
            .is_empty()
        );
        assert!(
            update(
                &mut state,
                Message::Grid(addon_grid::Message::CardContextRequested(
                    "missing".to_owned(),
                    iced::Point::ORIGIN,
                )),
            )
            .is_empty()
        );
    }

    #[test]
    fn next_page_request_emits_page_request_and_thumbnail_sync() {
        let mut state = State::default();
        state.push_rows_for_test(vec![Row::for_test(42, "Addon", 10)], 2);

        let effects = update(
            &mut state,
            Message::Grid(addon_grid::Message::NextPageRequested),
        );

        assert_eq!(
            effects,
            vec![
                Effect::PageRequested {
                    generation: 1,
                    page: 2,
                },
                Effect::ThumbnailDemandsChanged,
            ]
        );
    }

    #[test]
    fn grid_follow_up_visible_range_applies_inline() {
        let mut state = State::default();
        let _effects = update(&mut state, Message::RouteEntered);
        let _effects = update(
            &mut state,
            Message::PageCompleted(
                1,
                1,
                Ok(PageResult {
                    page: 1,
                    total: 1,
                    rows: vec![Row::for_test(42, "Addon", 10)],
                }),
            ),
        );

        let effects = update(
            &mut state,
            Message::Grid(addon_grid::Message::ViewportResized(500, 500)),
        );

        assert_eq!(effects, vec![Effect::ThumbnailDemandsChanged]);
    }

    #[test]
    fn card_press_and_release_emit_drag_effects() {
        let mut state = State::default();
        state.push_rows_for_test(vec![Row::for_test(42, "Addon", 10)], 1);

        assert_eq!(
            update(
                &mut state,
                Message::Grid(addon_grid::Message::CardPressed("42".to_owned())),
            ),
            vec![Effect::AddonDragPressed {
                card_id: "42".to_owned(),
                workshop_id: Some(
                    PublishedFileId::new(42).expect("test fixture ids are always nonzero")
                ),
            }]
        );
        assert_eq!(
            update(
                &mut state,
                Message::Grid(addon_grid::Message::CardReleased("42".to_owned())),
            ),
            vec![Effect::AddonDragReleased]
        );
    }

    #[test]
    fn hover_request_can_arm_animated_thumbnail_when_default_is_disabled() {
        let mut state = State::default();
        state.set_play_gifs_by_default(false);
        state.push_rows_for_test(
            vec![Row::for_test(42, "Animated", 10).with_ready_animation_for_test()],
            1,
        );
        let _ = addon_grid::update(
            state.grid_mut(),
            addon_grid::Message::ViewportResized(500, 500),
        );

        assert!(!state.has_active_animations());

        let effects = update(
            &mut state,
            Message::Grid(addon_grid::Message::CardHoverChanged("42".to_owned(), true)),
        );

        assert!(state.has_active_animations());
        assert!(effects.is_empty());
    }

    #[test]
    fn window_unfocus_pauses_animated_thumbnails() {
        let mut state = State::default();
        state.push_rows_for_test(
            vec![Row::for_test(42, "Animated", 10).with_ready_animation_for_test()],
            1,
        );
        let _ = addon_grid::update(
            state.grid_mut(),
            addon_grid::Message::ViewportResized(500, 500),
        );
        assert!(state.has_active_animations());

        assert!(state.set_window_focused(false));
        assert!(!state.has_active_animations());

        assert!(state.set_window_focused(true));
        assert!(state.has_active_animations());
    }
}
