use crate::widgets::{addon_grid, grid_rows};

use super::{Effect, Message, State};

/// Applies an Installed Addons route message and returns outward effects as plain data.
pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    match message {
        Message::RouteEntered => {
            state.enter_route();
            // Demands are visibility-gated: exit released every thumbnail,
            // so entry must re-sync them even when the visible range is
            // unchanged (an unchanged range emits no VisibleRangeChanged).
            let mut effects = metadata_request_effects(state);
            effects.push(Effect::ThumbnailDemandsChanged);
            effects
        }
        Message::RouteExited => {
            state.exit_route();
            vec![Effect::ThumbnailDemandsChanged]
        }
        Message::LibraryRefreshStarted(reason) => {
            state.refresh_started(reason);
            Vec::new()
        }
        Message::WatchArmed { degraded } => {
            state.apply_watch_armed(degraded);
            Vec::new()
        }
        Message::SnapshotPushed(reason, result) => {
            state.apply_snapshot(reason, result);
            let mut effects = metadata_request_effects(state);
            effects.push(Effect::ThumbnailDemandsChanged);
            effects
        }
        Message::MetadataCompleted(generation, item_ids, result) => {
            let mut effects = Vec::new();
            if let Some((generation, item_ids)) =
                state.finish_metadata_request(generation, &item_ids, result)
            {
                effects.push(Effect::MetadataRefreshRequested {
                    generation,
                    item_ids,
                });
            }
            effects.extend(metadata_request_effects(state));
            effects.push(Effect::ThumbnailDemandsChanged);
            effects
        }
        Message::MetadataRefreshCompleted(generation, result) => {
            state.apply_metadata_refresh(generation, result);
            let mut effects = metadata_request_effects(state);
            effects.push(Effect::ThumbnailDemandsChanged);
            effects
        }
        Message::AnimationTick(now) => {
            state.tick_visible_card_motion(now);
            state.tick_visible_animations(now);
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
            if let Some(target) = state.take_preview_target(&id) {
                effects.push(Effect::PreviewRequested(target));
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
            state.append_next_page();
            effects.extend(metadata_request_effects(state));
            effects.push(Effect::ThumbnailDemandsChanged);
        }
        addon_grid::Message::VisibleRangeChanged(_, _) => {
            effects.extend(metadata_request_effects(state));
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

fn metadata_request_effects(state: &mut State) -> Vec<Effect> {
    state
        .take_visible_metadata_request()
        .map_or_else(Vec::new, |(generation, item_ids)| {
            vec![Effect::MetadataRequested {
                generation,
                item_ids,
            }]
        })
}

#[cfg(test)]
mod tests {
    use crate::backend::domain::PublishedFileId;
    use crate::backend::library::LibraryRefreshReason;
    use crate::widgets::addon_grid;

    use super::super::model::{MetadataResolution, Row};
    use super::{Effect, Message, State, update};

    #[test]
    fn route_entry_marks_page_visible_and_syncs_thumbnails() {
        let mut state = State::default();

        let effects = update(&mut state, Message::RouteEntered);

        assert!(state.is_route_visible());
        assert_eq!(effects, vec![Effect::ThumbnailDemandsChanged]);
    }

    #[test]
    fn route_exit_hides_the_page() {
        let mut state = State::default();
        let _task = update(&mut state, Message::RouteEntered);

        let effects = update(&mut state, Message::RouteExited);

        assert!(!state.is_route_visible());
        assert_eq!(effects, vec![Effect::ThumbnailDemandsChanged]);
    }

    #[test]
    fn grid_messages_update_scroll_state() {
        let mut state = State::default();

        let effects = update(
            &mut state,
            Message::Grid(super::addon_grid::Message::Scrolled(32)),
        );

        assert_eq!(state.grid().scroll_offset(), 32.0);
        assert!(effects.is_empty());
    }

    #[test]
    fn snapshot_push_populates_first_page() {
        let mut state = State::default();
        let rows = (1..=55)
            .map(|index| {
                Row::for_test(
                    &format!("/tmp/{index}.gma"),
                    &format!("Addon {index}"),
                    Some(
                        PublishedFileId::new(index as u64)
                            .expect("test fixture ids are always nonzero"),
                    ),
                )
            })
            .collect();

        let effects = update(
            &mut state,
            Message::SnapshotPushed(LibraryRefreshReason::Startup, Ok(rows)),
        );

        assert_eq!(
            state.loaded_count(),
            crate::backend::domain::RESULTS_PER_PAGE
        );
        assert_eq!(state.total_count(), 55);
        assert_eq!(effects, vec![Effect::ThumbnailDemandsChanged]);
    }

    #[test]
    fn next_page_appends_discovered_rows() {
        let mut state = State::default();
        state.enter_route();
        let _messages = addon_grid::apply(
            state.grid_mut(),
            addon_grid::Message::ViewportResized(500, 500),
        );
        let rows = (1..=55)
            .map(|index| {
                Row::for_test(
                    &format!("/tmp/{index}.gma"),
                    &format!("Addon {index}"),
                    Some(
                        PublishedFileId::new(index as u64)
                            .expect("test fixture ids are always nonzero"),
                    ),
                )
            })
            .collect();
        state.apply_snapshot(LibraryRefreshReason::Startup, Ok(rows));

        let effects = update(
            &mut state,
            Message::Grid(addon_grid::Message::NextPageRequested),
        );

        assert_eq!(state.loaded_count(), 55);
        assert_eq!(
            effects,
            vec![
                Effect::MetadataRequested {
                    generation: 1,
                    item_ids: (1..=12)
                        .map(|id| PublishedFileId::new(id)
                            .expect("test fixture ids are always nonzero"))
                        .collect(),
                },
                Effect::ThumbnailDemandsChanged,
            ]
        );
    }

    #[test]
    fn visible_snapshot_requests_metadata_and_thumbnail_sync() {
        let mut state = State::default();
        let _effects = update(&mut state, Message::RouteEntered);
        let _messages = addon_grid::apply(
            state.grid_mut(),
            addon_grid::Message::ViewportResized(500, 500),
        );
        let rows = vec![
            Row::for_test(
                "/tmp/one.gma",
                "One",
                Some(PublishedFileId::new(1).expect("test fixture ids are always nonzero")),
            ),
            Row::for_test(
                "/tmp/two.gma",
                "Two",
                Some(PublishedFileId::new(2).expect("test fixture ids are always nonzero")),
            ),
        ];

        let effects = update(
            &mut state,
            Message::SnapshotPushed(LibraryRefreshReason::Startup, Ok(rows)),
        );

        assert_eq!(
            effects,
            vec![
                Effect::MetadataRequested {
                    generation: 1,
                    item_ids: vec![
                        PublishedFileId::new(1).expect("test fixture ids are always nonzero"),
                        PublishedFileId::new(2).expect("test fixture ids are always nonzero")
                    ],
                },
                Effect::ThumbnailDemandsChanged,
            ]
        );
    }

    #[test]
    fn metadata_completion_requests_stale_refresh_and_thumbnail_sync() {
        let mut state = State::default();
        let _effects = update(&mut state, Message::RouteEntered);
        let rows = vec![Row::for_test(
            "/tmp/one.gma",
            "One",
            Some(PublishedFileId::new(1).expect("test fixture ids are always nonzero")),
        )];
        let _effects = update(
            &mut state,
            Message::SnapshotPushed(LibraryRefreshReason::Startup, Ok(rows)),
        );

        let effects = update(
            &mut state,
            Message::MetadataCompleted(
                1,
                vec![PublishedFileId::new(1).expect("test fixture ids are always nonzero")],
                Ok(MetadataResolution {
                    patches: Vec::new(),
                    stale_ids: vec![
                        PublishedFileId::new(1).expect("test fixture ids are always nonzero"),
                    ],
                }),
            ),
        );

        assert_eq!(
            effects,
            vec![
                Effect::MetadataRefreshRequested {
                    generation: 1,
                    item_ids: vec![
                        PublishedFileId::new(1).expect("test fixture ids are always nonzero")
                    ],
                },
                Effect::ThumbnailDemandsChanged,
            ]
        );
    }

    #[test]
    fn card_click_and_context_menu_emit_effects_when_target_exists() {
        let mut state = State::default();
        state.enter_route();
        state.apply_snapshot(
            LibraryRefreshReason::Startup,
            Ok(vec![Row::for_test(
                "/tmp/one.gma",
                "One",
                Some(PublishedFileId::new(1).expect("test fixture ids are always nonzero")),
            )]),
        );

        let effects = update(
            &mut state,
            Message::Grid(addon_grid::Message::CardClicked("/tmp/one.gma".to_owned())),
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::PreviewRequested(target)]
                if target.path == std::path::Path::new("/tmp/one.gma")
                    && target.title == "One"
        ));

        let effects = update(
            &mut state,
            Message::Grid(addon_grid::Message::CardContextRequested(
                "/tmp/one.gma".to_owned(),
                iced::Point::new(10.0, 20.0),
            )),
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::ContextMenuRequested(menu)]
                if menu.path == std::path::Path::new("/tmp/one.gma")
                    && menu.position == iced::Point::new(10.0, 20.0)
        ));
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
    fn card_press_and_release_emit_drag_effects() {
        let mut state = State::default();
        state.apply_snapshot(
            LibraryRefreshReason::Startup,
            Ok(vec![Row::for_test(
                "/tmp/one.gma",
                "One",
                Some(PublishedFileId::new(1).expect("test fixture ids are always nonzero")),
            )]),
        );

        assert_eq!(
            update(
                &mut state,
                Message::Grid(addon_grid::Message::CardPressed("/tmp/one.gma".to_owned())),
            ),
            vec![Effect::AddonDragPressed {
                card_id: "/tmp/one.gma".to_owned(),
                workshop_id: Some(
                    PublishedFileId::new(1).expect("test fixture ids are always nonzero")
                ),
            }]
        );
        assert_eq!(
            update(
                &mut state,
                Message::Grid(addon_grid::Message::CardReleased("/tmp/one.gma".to_owned())),
            ),
            vec![Effect::AddonDragReleased]
        );
    }

    #[test]
    fn hover_request_can_arm_animated_thumbnail_when_default_is_disabled() {
        let mut state = State::default();
        state.set_play_gifs_by_default(false);
        state.enter_route();
        state.apply_snapshot(
            LibraryRefreshReason::Startup,
            Ok(vec![
                Row::for_test(
                    "/tmp/animated.gma",
                    "Animated",
                    Some(PublishedFileId::new(1).expect("test fixture ids are always nonzero")),
                )
                .with_ready_animation_for_test(),
            ]),
        );
        let _ = addon_grid::update(
            state.grid_mut(),
            addon_grid::Message::ViewportResized(500, 500),
        );

        assert!(!state.has_active_animations());

        let effects = update(
            &mut state,
            Message::Grid(addon_grid::Message::CardHoverChanged(
                "/tmp/animated.gma".to_owned(),
                true,
            )),
        );

        assert!(state.has_active_animations());
        assert!(effects.is_empty());
    }

    #[test]
    fn window_unfocus_pauses_animated_thumbnails() {
        let mut state = State::default();
        state.set_play_gifs_by_default(true);
        state.enter_route();
        state.apply_snapshot(
            LibraryRefreshReason::Startup,
            Ok(vec![
                Row::for_test(
                    "/tmp/animated.gma",
                    "Animated",
                    Some(PublishedFileId::new(1).expect("test fixture ids are always nonzero")),
                )
                .with_ready_animation_for_test(),
            ]),
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
