use std::sync::Arc;

use crate::bridge::{
    domain::{InstalledAddon, PublishedFileId},
    gma::{GmaHeader, GmaMeta, GmaMetadata},
    library::LibrarySnapshot,
    size_analyzer::SizeAnalyzerAddon,
};

use crate::media::{
    thumbnail_demand::{
        Delivery, DeliveryResult, DemandId, Owner, PlaceholderImage, ReadyThumbnail,
        ThumbnailDeliveryError,
    },
    thumbnail_worker::{ThumbnailError, ThumbnailInput, ThumbnailMetadata},
};

use super::*;

#[test]
fn hover_title_prefers_the_installed_title() {
    let addon = SizeAnalyzerAddon::new(
        "a.gma",
        Some(PublishedFileId::new(42).expect("test fixture ids are always nonzero")),
        "Cool Map",
        Some("map".to_owned()),
        Vec::new(),
        100,
    );
    assert_eq!(resolve_hover_title(&addon), "Cool Map");
}

#[test]
fn hover_title_falls_back_to_the_workshop_id_when_title_is_blank() {
    let addon = SizeAnalyzerAddon::new(
        "a.gma",
        Some(PublishedFileId::new(42).expect("test fixture ids are always nonzero")),
        "   ",
        Some("map".to_owned()),
        Vec::new(),
        100,
    );
    assert_eq!(resolve_hover_title(&addon), "42");
}

#[test]
fn hover_title_is_empty_for_a_blank_local_addon() {
    let addon = SizeAnalyzerAddon::new("a.gma", None, "", Some("map".to_owned()), Vec::new(), 100);
    assert_eq!(resolve_hover_title(&addon), "");
}

#[test]
fn route_entry_waits_for_viewport_before_projecting() {
    let mut state = State::default();

    state.enter_route();

    assert!(state.is_route_visible());
    assert_eq!(state.load_status(), &LoadStatus::WaitingForViewport);
}

#[test]
fn first_viewport_without_snapshot_shows_loading() {
    let mut state = State::default();
    state.enter_route();

    state.note_viewport(Size::new(640.0, 360.0));

    assert_eq!(state.load_status(), &LoadStatus::Loading);
}

#[test]
fn snapshot_push_projects_current_viewport_synchronously() {
    let mut state = State::default();
    state.enter_route();
    state.note_viewport(Size::new(640.0, 360.0));

    state.apply_snapshot(Ok(Some(fixture_snapshot(1))));

    assert_eq!(state.load_status(), &LoadStatus::Ready);
    assert_eq!(state.layout().unwrap().bounds.width.round() as u32, 640);
    assert_eq!(state.layout().unwrap().bounds.height.round() as u32, 360);
    assert!(state.projection_key_for_test().is_some());
    assert!(!state.labels().is_empty());
    assert_eq!(
        state.last_layer_invalidation_for_test(),
        Some(LayerInvalidation::ALL)
    );
}

#[test]
fn viewport_resize_reprojects_immediately() {
    let mut state = ready_state();
    let first_key = state.projection_key_for_test();

    state.note_viewport(Size::new(660.0, 360.0));

    assert_ne!(state.projection_key_for_test(), first_key);
    assert!(state.projection_key_for_test().is_some());
    assert_eq!(state.load_status(), &LoadStatus::Ready);
    assert!(!state.labels().is_empty());
}

#[test]
fn route_reentry_with_known_projection_does_no_work() {
    let mut state = ready_state();
    let key = state.projection_key_for_test();
    let invalidation_count = state.layers.recorded.borrow().len();

    state.exit_route();
    state.enter_route();

    assert_eq!(state.projection_key_for_test(), key);
    assert_eq!(state.load_status(), &LoadStatus::Ready);
    assert_eq!(state.layers.recorded.borrow().len(), invalidation_count);
}

#[test]
fn synchronous_projection_enables_hit_testing_after_labels_arrive() {
    let mut state = ready_state();

    assert_eq!(state.load_status(), &LoadStatus::Ready);
    assert!(state.layout().is_some());
    assert!(!state.labels().is_empty());
    let leaf = state
        .layout
        .as_ref()
        .unwrap()
        .leaf_rects()
        .into_iter()
        .find(|leaf| leaf.addon.title == "Map C")
        .unwrap();
    state.update_hover_at(Point::new(
        (leaf.rect.x + leaf.rect.width / 2.0) as f32,
        (leaf.rect.y + leaf.rect.height / 2.0) as f32,
    ));
    assert_eq!(state.hover().unwrap().title(), "Map C");
}

#[test]
fn resize_layout_replacement_invalidates_all_layers() {
    let mut state = ready_state();

    state.note_viewport(Size::new(660.0, 360.0));

    assert_eq!(
        state.last_layer_invalidation_for_test(),
        Some(LayerInvalidation::ALL)
    );
}

#[test]
fn empty_snapshot_projects_empty_state() {
    let mut state = State::default();
    state.enter_route();
    state.note_viewport(Size::new(640.0, 360.0));

    state.apply_snapshot(Ok(Some(empty_snapshot(1))));

    assert_eq!(state.load_status(), &LoadStatus::Empty);
    assert!(state.layout().is_none());
}

#[test]
fn context_menu_includes_download_for_workshop_cells() {
    let mut state = ready_state_with_workshop();
    let leaf = state
        .layout
        .as_ref()
        .unwrap()
        .leaf_rects()
        .into_iter()
        .find(|leaf| leaf.addon.workshop_id.is_some())
        .unwrap();
    state.update_hover_at(Point::new(
        (leaf.rect.x + leaf.rect.width / 2.0) as f32,
        (leaf.rect.y + leaf.rect.height / 2.0) as f32,
    ));

    let menu = state.request_context_menu(Point::ORIGIN).unwrap();

    assert!(
        menu.entries()
            .iter()
            .any(|entry| entry.action() == Some(context_menu::ContextMenuAction::Download))
    );
    assert!(
        menu.entries()
            .iter()
            .any(|entry| entry.action() == Some(context_menu::ContextMenuAction::CopyImageLink))
    );
    assert_eq!(
        menu.target().workshop_id(),
        Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
    );
    #[cfg(feature = "debug")]
    assert!(
        menu.entries()
            .iter()
            .any(|entry| { entry.action() == Some(context_menu::ContextMenuAction::HideAddon) })
    );
}

#[test]
fn preview_target_uses_active_hover_cell() {
    let mut state = ready_state_with_workshop();
    let leaf = state
        .layout
        .as_ref()
        .unwrap()
        .leaf_rects()
        .into_iter()
        .find(|leaf| leaf.addon.workshop_id.is_some())
        .unwrap();
    state.update_hover_at(Point::new(
        (leaf.rect.x + leaf.rect.width / 2.0) as f32,
        (leaf.rect.y + leaf.rect.height / 2.0) as f32,
    ));

    let target = state.preview_target().unwrap();

    assert_eq!(target.path, PathBuf::from("tool-a.gma"));
    assert_eq!(target.title, "Tool A");
    assert_eq!(
        target.workshop_id,
        Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
    );
}

#[test]
fn thumbnail_demands_include_current_layout_workshop_leaves() {
    let state = ready_state_with_workshop();

    let set = state.thumbnail_demands();

    assert_eq!(set.owner, Owner::SizeAnalyzer);
    assert_eq!(set.generation, ANALYZER_THUMBNAIL_GENERATION);
    assert_eq!(set.demands.len(), 1);
    assert_eq!(set.demands[0].id.as_str(), "123");
    assert_eq!(set.demands[0].logical_max_edge, ADDON_THUMBNAIL_MAX_EDGE);
}

#[test]
fn thumbnail_demands_include_every_addon_without_a_cutoff() {
    let addons = (1..=129)
        .map(|workshop_id| {
            installed_addon(
                &format!("{workshop_id}.gma"),
                Some(workshop_id),
                &format!("Addon {workshop_id}"),
                "tool",
                workshop_id,
            )
        })
        .collect();
    let mut state = ready_state_from_snapshot(snapshot_from_addons(1, addons));
    let urls = (1..=129)
        .map(|workshop_id| {
            (
                id(workshop_id),
                format!("https://example.invalid/{workshop_id}.jpg"),
            )
        })
        .collect();
    let _invalidation = state.apply_preview_urls(urls);

    let demands = state.thumbnail_demands();
    assert_eq!(demands.demands.len(), 129);
    assert!(demands.demands.iter().all(|demand| {
        demand.logical_max_edge.is_power_of_two()
            && (ADDON_THUMBNAIL_MIN_EDGE..=ADDON_THUMBNAIL_MAX_EDGE)
                .contains(&demand.logical_max_edge)
    }));
}

#[test]
fn thumbnail_plan_deduplicates_workshop_ids_at_the_largest_required_edge() {
    let duplicate = id(7);
    let addons = vec![
        installed_addon("small.gma", Some(7), "Small", "tool", 1),
        installed_addon("large.gma", Some(7), "Large", "tool", 10_000),
    ];
    let mut state = ready_state_from_snapshot(snapshot_from_addons(1, addons));
    let expected_edge = state
        .layout
        .as_ref()
        .expect("layout is ready")
        .leaf_rects()
        .into_iter()
        .filter(|leaf| leaf.addon.workshop_id == Some(duplicate))
        .map(|leaf| analyzer_thumbnail_edge(leaf.rect))
        .max()
        .expect("duplicate leaves exist");
    let _invalidation = state.apply_preview_urls(HashMap::from([(
        duplicate,
        "https://example.invalid/7.jpg".to_owned(),
    )]));

    let demands = state.thumbnail_demands();
    assert_eq!(demands.demands.len(), 1);
    assert_eq!(demands.demands[0].logical_max_edge, expected_edge);
}

#[test]
fn preview_url_refresh_merges_for_current_layout_only() {
    let mut state = ready_state_with_workshop();
    assert_eq!(
        state.preview_url_for_test(
            PublishedFileId::new(123).expect("test fixture ids are always nonzero")
        ),
        Some("https://example.invalid/preview.jpg")
    );

    assert_eq!(
        state.apply_preview_urls(HashMap::from([(
            PublishedFileId::new(456).expect("test fixture ids are always nonzero"),
            "https://example.invalid/stale.jpg".to_owned()
        )])),
        LayerInvalidation::NONE
    );
    assert_eq!(
        state.preview_url_for_test(
            PublishedFileId::new(456).expect("test fixture ids are always nonzero")
        ),
        None
    );

    assert_eq!(
        state.apply_preview_urls(HashMap::from([
            (
                PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
                "https://example.invalid/refreshed.jpg".to_owned()
            ),
            (
                PublishedFileId::new(456).expect("test fixture ids are always nonzero"),
                "https://example.invalid/new.jpg".to_owned()
            ),
        ]),),
        LayerInvalidation::THUMBNAILS
    );
    assert_eq!(
        state.preview_url_for_test(
            PublishedFileId::new(123).expect("test fixture ids are always nonzero")
        ),
        Some("https://example.invalid/refreshed.jpg")
    );
    assert_eq!(
        state.preview_url_for_test(
            PublishedFileId::new(456).expect("test fixture ids are always nonzero")
        ),
        None
    );
}

#[test]
fn preview_url_ids_are_taken_once_per_snapshot_epoch() {
    let mut state = ready_state_from_snapshot(workshop_snapshot(1));

    assert_eq!(
        state.take_pending_preview_url_ids(),
        vec![PublishedFileId::new(123).expect("test fixture ids are always nonzero")]
    );
    // Same epoch: a resize reprojects but must not redispatch a resolve.
    state.note_viewport(Size::new(660.0, 360.0));
    assert!(state.take_pending_preview_url_ids().is_empty());

    // A content-identical refresh under a bumped epoch must not re-arm.
    state.apply_snapshot(Ok(Some(workshop_snapshot(2))));
    assert!(state.take_pending_preview_url_ids().is_empty());

    // A genuinely changed library re-arms the resolve.
    state.apply_snapshot(Ok(Some(workshop_snapshot_resized(3))));
    assert_eq!(
        state.take_pending_preview_url_ids(),
        vec![PublishedFileId::new(123).expect("test fixture ids are always nonzero")]
    );
}

#[test]
fn content_identical_refresh_preserves_projection_thumbnails_and_urls() {
    let mut state = ready_state_with_workshop();
    let delivery = ready_delivery(ANALYZER_THUMBNAIL_GENERATION, 123, [44, 180, 90, 255]);
    let _invalidation = state.apply_thumbnail_delivery(&delivery);
    let _flushed = state.flush_thumbnail_invalidation();
    let key = state.projection_key_for_test();
    let invalidations_before = state.layers.recorded.borrow().len();

    // Same content, bumped epoch: nothing gets re-projected or wiped.
    state.apply_snapshot(Ok(Some(workshop_snapshot(2))));

    assert_eq!(state.projection_key_for_test(), key);
    assert!(state.thumbnail_tiles().contains_key(&id(123)));
    assert_eq!(
        state.preview_url_for_test(id(123)),
        Some("https://example.invalid/preview.jpg")
    );
    assert_eq!(state.layers.recorded.borrow().len(), invalidations_before);
}

#[test]
fn layout_workshop_ids_cache_matches_leaf_rects_projection() {
    let state = ready_state_with_workshop();

    let from_leaf_rects = state
        .layout()
        .unwrap()
        .leaf_rects()
        .into_iter()
        .filter_map(|leaf| leaf.addon.workshop_id)
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(state.layout_workshop_ids_for_test(), &from_leaf_rects);
    assert!(from_leaf_rects.contains(&id(123)));
}

#[test]
fn thumbnail_deliveries_coalesce_into_one_invalidation() {
    let mut state = ready_state_with_workshop();
    let first = ready_delivery(ANALYZER_THUMBNAIL_GENERATION, 123, [10, 20, 30, 255]);
    let second = ready_delivery(ANALYZER_THUMBNAIL_GENERATION, 123, [40, 50, 60, 255]);
    let thumbnails_before = state.thumbnail_invalidation_count_for_test();

    // Deliveries only mark the layer dirty; none clears the cache yet.
    assert_eq!(
        state.apply_thumbnail_delivery(&first),
        LayerInvalidation::THUMBNAILS
    );
    assert_eq!(
        state.apply_thumbnail_delivery(&second),
        LayerInvalidation::THUMBNAILS
    );
    assert_eq!(
        state.thumbnail_invalidation_count_for_test(),
        thumbnails_before
    );

    // The draw-time flush drains both arrivals as a single re-record.
    assert_eq!(
        state.flush_thumbnail_invalidation(),
        LayerInvalidation::THUMBNAILS
    );
    assert_eq!(
        state.thumbnail_invalidation_count_for_test(),
        thumbnails_before + 1
    );
    assert_eq!(
        state.flush_thumbnail_invalidation(),
        LayerInvalidation::NONE
    );
}

#[test]
fn ready_thumbnail_delivery_stores_tile_and_invalidates_thumbnail_layer() {
    let mut state = ready_state_with_workshop();
    let delivery = ready_delivery(ANALYZER_THUMBNAIL_GENERATION, 123, [44, 180, 90, 255]);

    let invalidation = state.apply_thumbnail_delivery(&delivery);

    assert_eq!(invalidation, LayerInvalidation::THUMBNAILS);
    let tile = state
        .thumbnail_tiles()
        .get(&PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
        .unwrap();
    assert_eq!((tile.width, tile.height), (8, 8));
    assert!(state.thumbnail_demands().demands.is_empty());
}

#[test]
fn placeholder_delivery_fills_tile_until_real_pixels_replace_it() {
    let mut state = ready_state_with_workshop();
    let id = PublishedFileId::new(123).expect("test fixture ids are always nonzero");

    let placeholder = placeholder_delivery(ANALYZER_THUMBNAIL_GENERATION, 123);
    assert_eq!(
        state.apply_thumbnail_delivery(&placeholder),
        LayerInvalidation::THUMBNAILS
    );
    // The placeholder paints (tile_for finds it) yet the sharp image is still
    // demanded.
    assert!(state.tile_for(id).is_some());
    assert!(state.thumbnail_tiles().is_empty());
    assert!(!state.thumbnail_demands().demands.is_empty());

    let ready = ready_delivery(ANALYZER_THUMBNAIL_GENERATION, 123, [44, 180, 90, 255]);
    let _invalidation = state.apply_thumbnail_delivery(&ready);
    assert!(state.thumbnail_tiles().contains_key(&id));
    assert!(state.thumbnail_demands().demands.is_empty());
}

#[test]
fn ready_thumbnail_delivery_reuses_the_delivered_handle() {
    let mut state = ready_state_with_workshop();
    let delivery = ready_delivery(ANALYZER_THUMBNAIL_GENERATION, 123, [44, 180, 90, 255]);
    let DeliveryResult::Ready(ready) = &delivery.result else {
        panic!("expected ready delivery");
    };
    let expected_handle = ready.handle().clone();

    let _invalidation = state.apply_thumbnail_delivery(&delivery);

    assert_eq!(
        state
            .thumbnail_tiles()
            .get(&PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
            .unwrap()
            .handle,
        expected_handle
    );
}

#[test]
fn failed_thumbnail_delivery_marks_cell_dead_and_invalidates_thumbnail_layer() {
    let mut state = ready_state_with_workshop();
    let delivery = failed_delivery(ANALYZER_THUMBNAIL_GENERATION, 123);

    let invalidation = state.apply_thumbnail_delivery(&delivery);

    assert_eq!(invalidation, LayerInvalidation::THUMBNAILS);
    assert!(
        state
            .failed_thumbnail_ids()
            .contains(&PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
    );
    assert!(state.thumbnail_demands().demands.is_empty());
}

#[test]
fn thumbnail_pending_tracks_deliverable_cells_only() {
    let mut state = ready_state_with_workshop();

    assert!(state.thumbnail_pending(Some(
        PublishedFileId::new(123).expect("test fixture ids are always nonzero")
    )));
    // Local addon (no workshop id) and unknown workshop cells never
    // deliver, so their cells show the dead placeholder immediately.
    assert!(!state.thumbnail_pending(None));
    assert!(!state.thumbnail_pending(Some(
        PublishedFileId::new(999).expect("test fixture ids are always nonzero")
    )));

    let _invalidation =
        state.apply_thumbnail_delivery(&failed_delivery(ANALYZER_THUMBNAIL_GENERATION, 123));
    assert!(!state.thumbnail_pending(Some(
        PublishedFileId::new(123).expect("test fixture ids are always nonzero")
    )));
}

#[test]
fn thumbnail_delivery_survives_stable_generation_echo() {
    let mut state = ready_state_with_workshop();
    let delivery = ready_delivery(999, 123, [44, 180, 90, 255]);

    assert_eq!(
        state.apply_thumbnail_delivery(&delivery),
        LayerInvalidation::THUMBNAILS
    );
    assert!(
        state
            .thumbnail_tiles()
            .contains_key(&PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
    );
}

#[test]
fn invalidating_ready_thumbnails_clears_tiles_and_thumbnail_layer() {
    let mut state = ready_state_with_workshop();
    let delivery = ready_delivery(ANALYZER_THUMBNAIL_GENERATION, 123, [44, 180, 90, 255]);
    let _invalidation = state.apply_thumbnail_delivery(&delivery);

    assert_eq!(
        state.invalidate_ready_thumbnails(),
        LayerInvalidation::THUMBNAILS
    );
    assert!(state.thumbnail_tiles().is_empty());
    assert_eq!(state.thumbnail_demands().demands.len(), 1);

    assert_eq!(state.invalidate_ready_thumbnails(), LayerInvalidation::NONE);
}

#[test]
fn thumbnails_are_retained_for_surviving_ids_across_snapshot_change() {
    let mut state = ready_state_with_workshop();
    let delivery = ready_delivery(ANALYZER_THUMBNAIL_GENERATION, 123, [44, 180, 90, 255]);
    let _invalidation = state.apply_thumbnail_delivery(&delivery);
    let handle = state
        .thumbnail_tiles()
        .get(&PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
        .unwrap()
        .handle
        .clone();

    state.apply_snapshot(Ok(Some(workshop_snapshot(2))));

    assert_eq!(
        state
            .thumbnail_tiles()
            .get(&PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
            .unwrap()
            .handle,
        handle
    );

    state.apply_snapshot(Ok(Some(fixture_snapshot(3))));

    assert!(
        !state
            .thumbnail_tiles()
            .contains_key(&PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
    );
}

#[test]
fn scale_factor_change_rerasters_labels_synchronously() {
    let mut state = ready_state();

    assert!(state.set_scale_factor(2.0));
    state.scale_factor_changed();

    assert_eq!(
        state.last_layer_invalidation_for_test(),
        Some(LayerInvalidation::LABELS)
    );
    assert!(!state.labels().is_empty());
    assert!(
        state
            .labels()
            .iter()
            .all(|label| (label.scale - 2.0).abs() < f32::EPSILON)
    );
}

#[test]
fn scale_factor_bucketing_absorbs_same_bucket_updates() {
    let mut state = State::default();

    assert!(state.set_scale_factor(2.0));
    assert!(!state.set_scale_factor(2.0));
    assert!(!state.set_scale_factor(1.75));
    assert!(state.set_scale_factor(1.0));
}

#[test]
fn scale_factor_change_without_layout_or_visibility_rerasters_nothing() {
    let mut state = State::default();
    assert!(state.set_scale_factor(2.0));
    state.scale_factor_changed();
    assert!(state.labels().is_empty());

    let mut state = ready_state();
    state.exit_route();
    let labels_before = state.labels().to_vec();
    assert!(state.set_scale_factor(2.0));
    state.scale_factor_changed();
    assert_eq!(state.labels(), labels_before.as_slice());
}

#[test]
fn same_viewport_does_not_reproject() {
    let mut state = ready_state();
    let key = state.projection_key_for_test();

    state.note_viewport(Size::new(640.0, 360.0));
    assert_eq!(state.projection_key_for_test(), key);

    state.note_viewport(Size::new(660.0, 360.0));
    assert_ne!(state.projection_key_for_test(), key);
}

#[test]
fn hovering_a_category_hides_its_label_and_invalidates_labels_layer() {
    let mut state = ready_state();
    hover_addon(&mut state, "Map C");

    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::LABELS);
    assert_eq!(state.hidden_tag(), Some("map"));

    let visible = visible_texts(&state);
    assert!(!visible.contains(&"map"));
    assert!(visible.contains(&"tool"));
    assert!(visible.contains(&"weapon"));
}

#[test]
fn moving_within_the_same_category_does_not_invalidate_labels() {
    let mut state = ready_state_with_two_maps();
    hover_addon(&mut state, "Map C");
    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::LABELS);

    hover_addon(&mut state, "Map D");

    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::NONE);
    assert_eq!(state.hidden_tag(), Some("map"));
}

#[test]
fn crossing_categories_swaps_hidden_label_with_one_invalidation() {
    let mut state = ready_state();
    hover_addon(&mut state, "Map C");
    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::LABELS);

    hover_addon(&mut state, "Tool A");

    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::LABELS);
    assert_eq!(state.hidden_tag(), Some("tool"));
    let visible = visible_texts(&state);
    assert!(visible.contains(&"map"));
    assert!(!visible.contains(&"tool"));
    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::NONE);
}

#[test]
fn hover_exit_restores_all_labels() {
    let mut state = ready_state();
    hover_addon(&mut state, "Map C");
    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::LABELS);

    state.clear_hover();

    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::LABELS);
    assert!(state.hidden_tag().is_none());
    assert_eq!(visible_texts(&state).len(), 3);
}

#[test]
fn thumbnail_delivery_keeps_hovered_label_hidden() {
    let mut state = ready_state_with_workshop();
    hover_addon(&mut state, "Tool A");
    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::LABELS);

    let delivery = ready_delivery(ANALYZER_THUMBNAIL_GENERATION, 123, [44, 180, 90, 255]);
    let invalidation = state.apply_thumbnail_delivery(&delivery);

    assert_eq!(invalidation, LayerInvalidation::THUMBNAILS);
    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::NONE);
    assert_eq!(state.hidden_tag(), Some("tool"));
    let visible = visible_texts(&state);
    assert!(!visible.contains(&"tool"));
    assert!(visible.contains(&"map"));
}

#[test]
fn resize_reprojection_clears_hover_and_restores_labels() {
    let mut state = ready_state();
    hover_addon(&mut state, "Map C");
    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::LABELS);

    state.note_viewport(Size::new(660.0, 360.0));

    assert!(state.hover().is_none());
    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::NONE);
    assert!(state.hidden_tag().is_none());
    assert_eq!(state.load_status(), &LoadStatus::Ready);
}

#[test]
fn reload_resets_hidden_tag_with_hover_state() {
    let mut state = ready_state();
    hover_addon(&mut state, "Map C");
    let _invalidation = state.sync_hidden_tag();

    state.exit_route();
    state.enter_route();

    assert_eq!(state.sync_hidden_tag(), LayerInvalidation::LABELS);
    assert!(state.hidden_tag().is_none());
}

#[test]
fn visible_tag_labels_excludes_only_hidden_tag() {
    let state = ready_state();
    let labels = state.labels();

    assert_eq!(visible_tag_labels(labels, None).count(), 3);
    assert_eq!(visible_tag_labels(labels, Some("unknown-tag")).count(), 3);

    let filtered = visible_tag_labels(labels, Some("map")).collect::<Vec<_>>();
    assert_eq!(filtered.len(), 2);
    assert!(filtered.iter().all(|label| label.text != "map"));
}

fn hover_addon(state: &mut State, title: &str) {
    let leaf = state
        .layout
        .as_ref()
        .unwrap()
        .leaf_rects()
        .into_iter()
        .find(|leaf| leaf.addon.title == title)
        .unwrap();
    state.update_hover_at(Point::new(
        (leaf.rect.x + leaf.rect.width / 2.0) as f32,
        (leaf.rect.y + leaf.rect.height / 2.0) as f32,
    ));
    assert_eq!(state.hover().unwrap().title(), title);
}

fn visible_texts(state: &State) -> Vec<&str> {
    visible_tag_labels(state.labels(), state.hidden_tag())
        .map(|label| label.text.as_str())
        .collect()
}

fn ready_state_with_two_maps() -> State {
    ready_state_from_snapshot(snapshot_from_addons(
        1,
        vec![
            installed_addon("tool-a.gma", None, "Tool A", "tool", 200),
            installed_addon("map-c.gma", None, "Map C", "map", 75),
            installed_addon("map-d.gma", None, "Map D", "map", 60),
        ],
    ))
}

fn ready_state() -> State {
    ready_state_from_snapshot(fixture_snapshot(1))
}

fn ready_state_with_workshop() -> State {
    let mut state = ready_state_from_snapshot(workshop_snapshot(1));
    let _invalidation = state.apply_preview_urls(HashMap::from([(
        PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
        "https://example.invalid/preview.jpg".to_owned(),
    )]));
    state
}

fn ready_state_from_snapshot(snapshot: LibrarySnapshot) -> State {
    let mut state = State::default();
    state.enter_route();
    state.note_viewport(Size::new(640.0, 360.0));
    state.apply_snapshot(Ok(Some(snapshot)));
    assert_eq!(state.load_status(), &LoadStatus::Ready);
    state
}

fn fixture_snapshot(epoch: u64) -> LibrarySnapshot {
    snapshot_from_addons(
        epoch,
        vec![
            installed_addon("tool-a.gma", None, "Tool A", "tool", 200),
            installed_addon("weapon-b.gma", None, "Weapon B", "weapon", 100),
            installed_addon("map-c.gma", None, "Map C", "map", 75),
        ],
    )
}

fn workshop_snapshot(epoch: u64) -> LibrarySnapshot {
    snapshot_from_addons(
        epoch,
        vec![
            installed_addon("tool-a.gma", Some(123), "Tool A", "tool", 200),
            installed_addon("map-c.gma", None, "Map C", "map", 75),
        ],
    )
}

/// Same addons as [`workshop_snapshot`] but with a changed file size, so the
/// content differs even though the workshop item set is unchanged.
fn workshop_snapshot_resized(epoch: u64) -> LibrarySnapshot {
    snapshot_from_addons(
        epoch,
        vec![
            installed_addon("tool-a.gma", Some(123), "Tool A", "tool", 200),
            installed_addon("map-c.gma", None, "Map C", "map", 90),
        ],
    )
}

fn id(workshop_id: u64) -> PublishedFileId {
    PublishedFileId::new(workshop_id).expect("test fixture ids are always nonzero")
}

fn empty_snapshot(epoch: u64) -> LibrarySnapshot {
    snapshot_from_addons(epoch, Vec::new())
}

fn snapshot_from_addons(epoch: u64, addons: Vec<InstalledAddon>) -> LibrarySnapshot {
    LibrarySnapshot {
        addons: Arc::from(addons.into_boxed_slice()),
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

fn ready_delivery(generation: u64, workshop_id: u64, color: [u8; 4]) -> Delivery {
    let url = "https://example.invalid/preview.jpg";
    let input = ThumbnailInput::from_url(url);
    let key = input.cache_key(ADDON_THUMBNAIL_MAX_EDGE);
    let metadata = ThumbnailMetadata {
        width: 8,
        height: 8,
        source_width: 8,
        source_height: 8,
        max_edge: ADDON_THUMBNAIL_MAX_EDGE,
    };
    Delivery {
        owner: Owner::SizeAnalyzer,
        generation,
        id: DemandId::new(workshop_id.to_string()),
        key: key.clone(),
        result: DeliveryResult::Ready(ReadyThumbnail::for_test(
            key,
            metadata,
            solid_rgba(8, 8, color),
        )),
    }
}

fn placeholder_delivery(generation: u64, workshop_id: u64) -> Delivery {
    let input = ThumbnailInput::from_url("https://example.invalid/preview.jpg");
    Delivery {
        owner: Owner::SizeAnalyzer,
        generation,
        id: DemandId::new(workshop_id.to_string()),
        key: input.cache_key(ADDON_THUMBNAIL_MAX_EDGE),
        result: DeliveryResult::Placeholder(PlaceholderImage::for_test(6, 6)),
    }
}

fn failed_delivery(generation: u64, workshop_id: u64) -> Delivery {
    let url = "https://example.invalid/preview.jpg";
    let input = ThumbnailInput::from_url(url);
    Delivery {
        owner: Owner::SizeAnalyzer,
        generation,
        id: DemandId::new(workshop_id.to_string()),
        key: input.cache_key(ADDON_THUMBNAIL_MAX_EDGE),
        result: DeliveryResult::Failed {
            error: ThumbnailDeliveryError::Thumbnail(Arc::new(ThumbnailError::InvalidMaxEdge)),
        },
    }
}

fn solid_rgba(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let mut pixels = vec![0; (width * height * 4) as usize];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&color);
    }
    pixels
}
