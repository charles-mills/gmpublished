use super::{Effect, Message, State};

/// Applies a Size Analyzer route message and returns outward effects as plain data.
pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    let effects = apply_message(state, message);
    // Hover mutates on many paths (moves, exits, resize reflow, route
    // exit), so the hidden tag label re-derives once per message instead of
    // per call site; it only invalidates the labels layer when the hovered
    // tag changed.
    let _invalidation = state.sync_hidden_tag();
    effects
}

fn apply_message(state: &mut State, message: Message) -> Vec<Effect> {
    match message {
        Message::RouteEntered => {
            state.enter_route();
            let mut effects = vec![Effect::ThumbnailDemandsChanged];
            append_preview_url_effect(&mut effects, state);
            effects
        }
        Message::RouteExited => {
            state.exit_route();
            vec![Effect::ThumbnailDemandsChanged]
        }
        Message::ViewportResized(size) => {
            state.note_viewport(size);
            let mut effects = vec![Effect::ThumbnailDemandsChanged];
            append_preview_url_effect(&mut effects, state);
            effects
        }
        Message::SnapshotPushed(_, result) => {
            state.apply_snapshot(result);
            let mut effects = vec![Effect::ThumbnailDemandsChanged];
            append_preview_url_effect(&mut effects, state);
            effects
        }
        Message::ScaleFactorChanged => {
            state.scale_factor_changed();
            Vec::new()
        }
        Message::PreviewUrlsResolved(urls) => {
            let _invalidation = state.apply_preview_urls(urls);
            vec![Effect::ThumbnailDemandsChanged]
        }
        Message::HoverMoved(point) => {
            state.update_hover_at(point);
            Vec::new()
        }
        Message::HoverExited => {
            state.clear_hover();
            Vec::new()
        }
        Message::TreemapClicked => state
            .preview_target()
            .map_or_else(Vec::new, |target| vec![Effect::PreviewRequested(target)]),
        Message::TreemapRightPressed(position) => state
            .request_context_menu(position)
            .map_or_else(Vec::new, |menu| vec![Effect::ContextMenuRequested(menu)]),
        Message::TreemapPressed => state.preview_target().map_or_else(Vec::new, |target| {
            vec![Effect::AddonDragPressed {
                card_id: target.path.display().to_string(),
                workshop_id: target.workshop_id,
            }]
        }),
        Message::TreemapReleased => vec![Effect::AddonDragReleased],
    }
}

fn append_preview_url_effect(effects: &mut Vec<Effect>, state: &mut State) {
    let ids = state.take_pending_preview_url_ids();
    if !ids.is_empty() {
        effects.push(Effect::PreviewUrlsResolveRequested(ids));
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::Arc,
    };

    use crate::backend::{
        domain::{InstalledAddon, PublishedFileId},
        gma::{GmaHeader, GmaMeta, GmaMetadata},
        library::{LibraryRefreshReason, LibrarySnapshot},
    };
    use iced::{Point, Size};

    use super::{Effect, Message, State, update};
    use crate::features::size_analyzer::state::LoadStatus;

    #[test]
    fn route_entry_marks_page_visible_and_waits_for_viewport() {
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
    fn viewport_resize_starts_initial_analysis_request() {
        let mut state = State::default();
        let _effects = update(&mut state, Message::RouteEntered);

        let effects = update(
            &mut state,
            Message::ViewportResized(Size::new(640.0, 360.0)),
        );

        assert_eq!(effects, vec![Effect::ThumbnailDemandsChanged]);
        assert_eq!(state.load_status(), &LoadStatus::Loading);
    }

    #[test]
    fn snapshot_projects_and_emits_label_thumbnail_and_preview_url_effects() {
        let mut state = State::default();
        let _effects = update(&mut state, Message::RouteEntered);
        let _effects = update(
            &mut state,
            Message::ViewportResized(Size::new(640.0, 360.0)),
        );

        let effects = update(
            &mut state,
            Message::SnapshotPushed(
                LibraryRefreshReason::Startup,
                Ok(Some(workshop_snapshot(1))),
            ),
        );

        assert_eq!(
            effects,
            vec![
                Effect::ThumbnailDemandsChanged,
                Effect::PreviewUrlsResolveRequested(vec![
                    PublishedFileId::new(123).expect("test fixture ids are always nonzero")
                ]),
            ]
        );
        assert_eq!(state.load_status(), &LoadStatus::Ready);
        assert!(!state.labels().is_empty());
    }

    #[test]
    fn preview_url_resolve_is_emitted_once_per_snapshot_epoch() {
        let mut state = ready_state_from_snapshot(workshop_snapshot(1));

        let effects = update(
            &mut state,
            Message::ViewportResized(Size::new(660.0, 360.0)),
        );

        assert!(effects.contains(&Effect::ThumbnailDemandsChanged));
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::PreviewUrlsResolveRequested(_)))
        );

        // A content-identical refresh under a bumped epoch must not re-arm.
        let effects = update(
            &mut state,
            Message::SnapshotPushed(
                LibraryRefreshReason::DiskChanged,
                Ok(Some(workshop_snapshot(2))),
            ),
        );
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::PreviewUrlsResolveRequested(_)))
        );

        // A genuinely changed library re-arms the resolve.
        let effects = update(
            &mut state,
            Message::SnapshotPushed(
                LibraryRefreshReason::DiskChanged,
                Ok(Some(workshop_snapshot_resized(3))),
            ),
        );
        assert!(effects.contains(&Effect::PreviewUrlsResolveRequested(vec![
            PublishedFileId::new(123).expect("test fixture ids are always nonzero")
        ])));

        // A metadata-only change (rename, same sizes) is still a content
        // change — the projection consumes titles for labels.
        let effects = update(
            &mut state,
            Message::SnapshotPushed(
                LibraryRefreshReason::DiskChanged,
                Ok(Some(workshop_snapshot_renamed(4))),
            ),
        );
        assert!(effects.contains(&Effect::PreviewUrlsResolveRequested(vec![
            PublishedFileId::new(123).expect("test fixture ids are always nonzero")
        ])));
    }

    #[test]
    fn scale_factor_changed_is_empty_without_visible_layout() {
        let mut state = State::default();

        assert!(update(&mut state, Message::ScaleFactorChanged).is_empty());
    }

    #[test]
    fn preview_urls_resolved_refreshes_thumbnail_demands() {
        let mut state = State::default();

        assert_eq!(
            update(
                &mut state,
                Message::PreviewUrlsResolved(HashMap::from([(
                    PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
                    "https://example.invalid/preview.jpg".to_owned(),
                )])),
            ),
            vec![Effect::ThumbnailDemandsChanged]
        );
    }

    #[test]
    fn hover_without_layout_is_ignored() {
        let mut state = State::default();
        let effects = update(&mut state, Message::HoverMoved(Point::new(10.0, 20.0)));

        assert!(state.hover().is_none());
        assert!(effects.is_empty());
    }

    #[test]
    fn left_press_does_not_prepare_context_menu() {
        let mut state = State::default();

        let effects = update(&mut state, Message::TreemapPressed);

        assert!(state.preview_target().is_none());
        assert!(effects.is_empty());
    }

    #[test]
    fn hovered_cell_emits_preview_context_menu_and_drag_effects() {
        let mut state = ready_state_from_snapshot(workshop_snapshot(1));
        hover_workshop_leaf(&mut state);

        let effects = update(&mut state, Message::TreemapClicked);
        assert!(matches!(
            effects.as_slice(),
            [Effect::PreviewRequested(target)]
                if target.path == Path::new("tool-a.gma")
                    && target.workshop_id == Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
        ));

        hover_workshop_leaf(&mut state);
        let effects = update(
            &mut state,
            Message::TreemapRightPressed(Point::new(20.0, 20.0)),
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::ContextMenuRequested(menu)]
                if menu.position == Point::new(20.0, 20.0)
                    && menu.target().workshop_id() == Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
        ));

        hover_workshop_leaf(&mut state);
        assert_eq!(
            update(&mut state, Message::TreemapPressed),
            vec![Effect::AddonDragPressed {
                card_id: "tool-a.gma".to_owned(),
                workshop_id: Some(
                    PublishedFileId::new(123).expect("test fixture ids are always nonzero")
                ),
            }]
        );
        assert_eq!(
            update(&mut state, Message::TreemapReleased),
            vec![Effect::AddonDragReleased]
        );
    }

    fn ready_state_from_snapshot(snapshot: LibrarySnapshot) -> State {
        let mut state = State::default();
        let _effects = update(&mut state, Message::RouteEntered);
        let _effects = update(
            &mut state,
            Message::ViewportResized(Size::new(640.0, 360.0)),
        );
        let _effects = update(
            &mut state,
            Message::SnapshotPushed(LibraryRefreshReason::Startup, Ok(Some(snapshot))),
        );
        state
    }

    fn hover_workshop_leaf(state: &mut State) {
        let leaf = state
            .layout()
            .unwrap()
            .leaf_rects()
            .into_iter()
            .find(|leaf| {
                leaf.addon.workshop_id
                    == Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
            })
            .expect("workshop leaf should exist");
        let point = Point::new(
            (leaf.rect.x + leaf.rect.width / 2.0) as f32,
            (leaf.rect.y + leaf.rect.height / 2.0) as f32,
        );
        let _effects = update(state, Message::HoverMoved(point));
    }

    fn workshop_snapshot(epoch: u64) -> LibrarySnapshot {
        LibrarySnapshot {
            addons: Arc::from(
                vec![
                    installed_addon("tool-a.gma", Some(123), "Tool A", "tool", 200),
                    installed_addon("map-c.gma", None, "Map C", "map", 75),
                ]
                .into_boxed_slice(),
            ),
            epoch,
        }
    }

    /// Same workshop item set as [`workshop_snapshot`] but a changed size, so
    /// the content differs under a new epoch.
    fn workshop_snapshot_resized(epoch: u64) -> LibrarySnapshot {
        LibrarySnapshot {
            addons: Arc::from(
                vec![
                    installed_addon("tool-a.gma", Some(123), "Tool A", "tool", 200),
                    installed_addon("map-c.gma", None, "Map C", "map", 90),
                ]
                .into_boxed_slice(),
            ),
            epoch,
        }
    }

    /// Same item set and sizes as [`workshop_snapshot_resized`] but a changed
    /// title, so the content differs under a new epoch.
    fn workshop_snapshot_renamed(epoch: u64) -> LibrarySnapshot {
        LibrarySnapshot {
            addons: Arc::from(
                vec![
                    installed_addon("tool-a.gma", Some(123), "Tool A v2", "tool", 200),
                    installed_addon("map-c.gma", None, "Map C", "map", 90),
                ]
                .into_boxed_slice(),
            ),
            epoch,
        }
    }

    fn installed_addon(
        path: &str,
        workshop_id: Option<u64>,
        title: &str,
        addon_type: &str,
        size: u64,
    ) -> InstalledAddon {
        let path = PathBuf::from(path);
        InstalledAddon {
            path: path.clone(),
            canonical_path: path.clone(),
            workshop_id: workshop_id.and_then(PublishedFileId::new),
            file_size_bytes: size,
            modified_epoch_seconds: 1,
            meta: GmaMeta {
                path,
                header: GmaHeader {
                    version: 3,
                    timestamp: 0,
                    metadata: GmaMetadata::Standard {
                        title: title.to_owned(),
                        addon_type: addon_type.to_owned(),
                        tags: Vec::new(),
                        ignore: Vec::new(),
                    },
                    author: String::new(),
                    addon_version: 1,
                },
                entries: Vec::new(),
            },
        }
    }
}
