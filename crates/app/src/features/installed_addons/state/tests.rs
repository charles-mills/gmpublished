use crate::bridge::library::LibraryRefreshReason;

use super::model::MetadataPatch;
use super::*;

/// Builds a `State` pre-populated with `count` rows (the full discovered
/// addon library), each with a unique workshop id `1..=count`, mirroring
/// the shape the route reaches after a snapshot lands.
fn fixture_state(count: usize) -> State {
    let rows: Vec<Row> = (0..count)
        .map(|i| {
            Row::for_test(
                &format!("/addons/{i}.gma"),
                "Title",
                Some(
                    PublishedFileId::new(i as u64 + 1)
                        .expect("test fixture ids are always nonzero"),
                ),
            )
        })
        .collect();
    let workshop_index = build_workshop_index(&rows);

    State {
        generation: 1,
        rows: Some(rows),
        workshop_index,
        ..State::default()
    }
}

fn visible_fixture_state(count: usize) -> State {
    let mut state = fixture_state(count);
    state.route_visible = true;
    state.load_status = LoadStatus::Ready;
    state.sync_grid_items();
    let _ = addon_grid::update(
        state.grid_mut(),
        addon_grid::Message::ViewportResized(200, 160),
    );
    let visible = state.grid.visible_item_range();
    let (_, after) = thumbnail_demand::prefetch_ranges(visible.clone(), state.rows().len());
    assert!(!visible.is_empty(), "fixture must expose visible rows");
    assert!(!after.is_empty(), "fixture must expose an after-window");
    assert!(
        after.end < state.rows().len(),
        "fixture must leave rows beyond the after-window"
    );
    state
}

/// Ids match `fixture_state`'s rows, which are numbered `1..=n` (row `i`
/// carries id `i + 1`): `start` is the 0-based row index the batch begins
/// at, so the ids generated here are `start + 1 ..= start + count`.
fn patch_batch(start: u64, count: u64) -> Vec<MetadataPatch> {
    (start..start + count)
        .map(|id| {
            MetadataPatch::for_test(
                PublishedFileId::new(id + 1).expect("test fixture ids are always nonzero"),
                "Updated title",
                Some("https://example.test/p.jpg"),
            )
        })
        .collect()
}

#[test]
fn settings_refresh_resets_visible_projection_loudly() {
    let mut state = fixture_state(3);
    state.route_visible = true;
    state.load_status = LoadStatus::Ready;

    state.refresh_started(LibraryRefreshReason::SettingsChanged);

    assert_eq!(state.load_status, LoadStatus::Loading);
    assert!(state.rows.is_none());
}

#[test]
fn settings_refresh_invalidates_hidden_projection_without_loading() {
    let mut state = fixture_state(3);
    state.route_visible = false;
    state.load_status = LoadStatus::Ready;

    state.refresh_started(LibraryRefreshReason::SettingsChanged);

    assert_eq!(state.load_status, LoadStatus::Idle);
    assert!(state.rows.is_none());
}

#[test]
fn visible_metadata_request_includes_after_window_before_beyond_rows() {
    let mut state = visible_fixture_state(80);
    let visible = state.grid.visible_item_range();
    let (_, after) = thumbnail_demand::prefetch_ranges(visible.clone(), state.rows().len());

    let (_, ids) = state
        .take_visible_metadata_request()
        .expect("visible and after-window ids should be requested");

    // Row `i` carries id `i + 1` (see `fixture_state`).
    let visible_ids = visible
        .map(|index| {
            PublishedFileId::new(index as u64 + 1).expect("test fixture ids are always nonzero")
        })
        .collect::<Vec<_>>();
    assert_eq!(&ids[..visible_ids.len()], visible_ids.as_slice());
    assert_eq!(
        ids[visible_ids.len()],
        PublishedFileId::new(after.start as u64 + 1).expect("test fixture ids are always nonzero")
    );
    assert!(!ids.contains(
        &PublishedFileId::new(after.end as u64 + 1).expect("test fixture ids are always nonzero")
    ));
}

#[test]
fn visible_metadata_request_dedups_prefetch_window_against_known_ids() {
    let mut state = visible_fixture_state(80);
    let visible = state.grid.visible_item_range();
    let (_, after) = thumbnail_demand::prefetch_ranges(visible, state.rows().len());
    // Row `i` carries id `i + 1` (see `fixture_state`).
    let in_flight =
        PublishedFileId::new(after.start as u64 + 1).expect("test fixture ids are always nonzero");
    let finished = PublishedFileId::new(after.start.saturating_add(1) as u64 + 1)
        .expect("test fixture ids are always nonzero");
    let still_new = PublishedFileId::new(after.start.saturating_add(2) as u64 + 1)
        .expect("test fixture ids are always nonzero");
    state.metadata_in_flight.insert(in_flight);
    state.metadata_finished.insert(finished);

    let (_, ids) = state
        .take_visible_metadata_request()
        .expect("remaining visible and prefetch ids should be requested");

    assert!(!ids.contains(&in_flight));
    assert!(!ids.contains(&finished));
    assert!(ids.contains(&still_new));
}

/// Exercises metadata patching at large installed-library scale: 3000
/// rows and repeated UGC batches of 50 patches.
#[test]
fn apply_metadata_patches_matches_expected_at_scale() {
    const ROWS: usize = 3000;
    const BATCHES: u64 = 20;
    const BATCH_SIZE: u64 = 50;

    let mut state = fixture_state(ROWS);

    for batch in 0..BATCHES {
        let patches = patch_batch(batch * BATCH_SIZE, BATCH_SIZE);
        state.apply_metadata_patches(1, &patches);
    }

    let patched_count = (BATCHES * BATCH_SIZE) as usize;
    for (i, row) in state.rows().iter().enumerate() {
        if i < patched_count {
            assert_eq!(
                row.title_for_test(),
                "Updated title",
                "row {i} should be patched"
            );
        } else {
            assert_eq!(row.title_for_test(), "Title", "row {i} should be untouched");
        }
    }
}

/// Drives the real snapshot API rather than hand-building a fixture, to
/// confirm the workshop-id index stays consistent when maintained through
/// the actual mutation paths, then verifies patches land on the rows and
/// on the grid's items in place.
#[test]
fn workshop_index_stays_consistent_through_snapshot_and_patches() {
    let mut state = State::default();
    let rows: Vec<Row> = (0..120)
        .map(|i| {
            Row::for_test(
                &format!("/addons/{i}.gma"),
                "Title",
                Some(
                    PublishedFileId::new(i as u64 + 1)
                        .expect("test fixture ids are always nonzero"),
                ),
            )
        })
        .collect();
    state.apply_snapshot(LibraryRefreshReason::Startup, Ok(rows));
    let generation = state.generation;
    // The entire library lands in the grid at once; nothing is paged.
    assert_eq!(state.row_count(), 120);
    assert_eq!(state.grid().items_len(), 120);

    let patches = vec![
        MetadataPatch::for_test(
            PublishedFileId::new(10).expect("test fixture ids are always nonzero"),
            "Patched Early",
            None,
        ),
        MetadataPatch::for_test(
            PublishedFileId::new(115).expect("test fixture ids are always nonzero"),
            "Patched Late",
            None,
        ),
    ];
    state.apply_metadata_patches(generation, &patches);

    let early = state
        .rows()
        .iter()
        .find(|row| {
            row.workshop_id()
                == Some(PublishedFileId::new(10).expect("test fixture ids are always nonzero"))
        })
        .expect("row 10 should be present");
    assert_eq!(early.title_for_test(), "Patched Early");

    let late = state
        .rows()
        .iter()
        .find(|row| {
            row.workshop_id()
                == Some(PublishedFileId::new(115).expect("test fixture ids are always nonzero"))
        })
        .expect("row 115 should be present");
    assert_eq!(late.title_for_test(), "Patched Late");

    // The patches must reach the grid's own items: patching is the
    // hydration path, and no other test observes the grid side of it. The
    // grid resolves patches by id (the index is only a hint), so this pins
    // the end-to-end row -> grid item delivery. Row `i` carries id `i + 1`.
    assert_eq!(state.grid().item_title_for_test(9), Some("Patched Early"));
    assert_eq!(state.grid().item_title_for_test(114), Some("Patched Late"));
    assert_eq!(state.grid().item_title_for_test(0), Some("Title"));
}

/// Duplicate workshop ids (e.g. the same Workshop item installed at two
/// local paths) must both receive the patch -- guards against an index
/// implementation that only stores a single index per id.
#[test]
fn duplicate_workshop_ids_both_receive_patch() {
    let rows = vec![
        Row::for_test(
            "/addons/a.gma",
            "Title",
            Some(PublishedFileId::new(7).expect("test fixture ids are always nonzero")),
        ),
        Row::for_test(
            "/addons/b.gma",
            "Title",
            Some(PublishedFileId::new(7).expect("test fixture ids are always nonzero")),
        ),
    ];
    let workshop_index = build_workshop_index(&rows);
    let mut state = State {
        generation: 1,
        rows: Some(rows),
        workshop_index,
        ..State::default()
    };

    state.apply_metadata_patches(
        1,
        &[MetadataPatch::for_test(
            PublishedFileId::new(7).expect("test fixture ids are always nonzero"),
            "Patched",
            None,
        )],
    );

    assert!(
        state
            .rows()
            .iter()
            .all(|row| row.title_for_test() == "Patched")
    );
}

/// Builds a visible, settled state the way the route looks when a live
/// disk change arrives: rows on screen, nothing in flight.
fn settled_visible_state(count: usize) -> State {
    let mut state = fixture_state(count);
    state.route_visible = true;
    state.load_status = LoadStatus::Ready;
    state
}

#[test]
fn disk_change_swaps_quietly_and_carries_unchanged_rows() {
    let mut state = settled_visible_state(2);
    let rows = state.rows.as_mut().expect("fixture rows");
    rows[0] = rows[0].clone().with_ready_animation_for_test();
    rows[1] = rows[1].clone().with_ready_animation_for_test();

    state.refresh_started(LibraryRefreshReason::DiskChanged);
    assert_eq!(state.load_status, LoadStatus::Ready, "no loading flash");
    assert_eq!(state.rows().len(), 2, "grid keeps rows mid-scan");

    let fresh = vec![
        // Same fingerprint (0/0) as the fixture row: carried over.
        Row::for_test(
            "/addons/0.gma",
            "Title",
            Some(PublishedFileId::new(1).expect("test fixture ids are always nonzero")),
        ),
        // Changed fingerprint: replaced by the fresh scan row.
        Row::for_test(
            "/addons/1.gma",
            "Title",
            Some(PublishedFileId::new(1).expect("test fixture ids are always nonzero")),
        )
        .with_file_fingerprint_for_test(9, 9),
    ];
    state.apply_snapshot(LibraryRefreshReason::DiskChanged, Ok(fresh));

    assert!(state.rows()[0].thumbnail_ready_for_test());
    assert!(!state.rows()[1].thumbnail_ready_for_test());
    assert_eq!(state.load_status, LoadStatus::Ready);
}

#[test]
fn quiet_apply_replaces_the_full_row_set() {
    let mut state = settled_visible_state(80);

    state.refresh_started(LibraryRefreshReason::DiskChanged);
    let shrunk: Vec<Row> = (0..30)
        .map(|i| {
            Row::for_test(
                &format!("/addons/{i}.gma"),
                "Title",
                Some(
                    PublishedFileId::new(i as u64 + 1)
                        .expect("test fixture ids are always nonzero"),
                ),
            )
        })
        .collect();
    state.apply_snapshot(LibraryRefreshReason::DiskChanged, Ok(shrunk));
    assert_eq!(state.rows().len(), 30, "shrink lands in full");
    assert_eq!(state.grid().items_len(), 30);

    state.refresh_started(LibraryRefreshReason::DiskChanged);
    let grown: Vec<Row> = (0..80)
        .map(|i| {
            Row::for_test(
                &format!("/addons/{i}.gma"),
                "Title",
                Some(
                    PublishedFileId::new(i as u64 + 1)
                        .expect("test fixture ids are always nonzero"),
                ),
            )
        })
        .collect();
    state.apply_snapshot(LibraryRefreshReason::DiskChanged, Ok(grown));
    assert_eq!(state.rows().len(), 80, "growth lands in full");
    assert_eq!(state.grid().items_len(), 80);
}

#[test]
fn disk_snapshot_while_hidden_updates_projection_without_thumbnail_work() {
    let mut state = fixture_state(3);
    state.load_status = LoadStatus::Ready;

    state.refresh_started(LibraryRefreshReason::DiskChanged);
    state.apply_snapshot(
        LibraryRefreshReason::DiskChanged,
        Ok(vec![Row::for_test(
            "/addons/new.gma",
            "Title",
            Some(PublishedFileId::new(9).expect("test fixture ids are always nonzero")),
        )]),
    );

    assert_eq!(state.rows().len(), 1);
    assert_eq!(state.load_status, LoadStatus::Ready);
    assert!(state.thumbnail_demands().demands.is_empty());
}

#[test]
fn quiet_error_keeps_current_rows_on_screen() {
    let mut state = settled_visible_state(2);

    state.refresh_started(LibraryRefreshReason::DiskChanged);
    state.apply_snapshot(
        LibraryRefreshReason::DiskChanged,
        Err(UiError::detailed(
            gmpublished_backend::error_key::ErrorKey("ERR_TEST"),
            Some("scan raced a file move".to_owned()),
        )),
    );

    assert_eq!(state.rows().len(), 2);
    assert_eq!(state.load_status, LoadStatus::Ready);
    assert!(state.rows.is_some());
}

#[test]
fn degraded_watch_rearms_once_per_route_entry() {
    let mut state = State::default();
    assert_eq!(state.watch_arm_epoch(), 0);

    state.apply_watch_armed(true);
    state.enter_route();
    assert_eq!(state.watch_arm_epoch(), 1);

    // Still degraded after the retry: no second bump until re-entry.
    state.apply_watch_armed(true);
    state.exit_route();
    state.enter_route();
    assert_eq!(state.watch_arm_epoch(), 2);

    // Healthy watch never churns the subscription.
    state.apply_watch_armed(false);
    state.exit_route();
    state.enter_route();
    assert_eq!(state.watch_arm_epoch(), 2);
}
