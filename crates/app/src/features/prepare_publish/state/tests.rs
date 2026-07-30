use std::{path::PathBuf, sync::Arc};

use crate::bridge::{
    domain::{PublishedFileId, WorkshopDownloadSuccess, workshop_url},
    publish::{
        DEFAULT_WORKSHOP_ICON_FILE_NAME, IconFormat, PublishSubmitMode, PublishSubmitPreview,
    },
    ui_error::UiError,
};
use gmpublished_backend::ErrorKey;
use iced::widget::image;

use super::ChangelogContent;
use super::{
    AddonTag, AddonType, Mode, OpenTarget, Requirement, State, UpdateTarget, Verification,
};
use crate::bridge::tasks::WorkshopSnapshotId;
use crate::features::prepare_publish::model::{
    ContentPathVerificationRequest, IgnorePatternMutationResult, IgnoredPattern,
    PublishSubmitContext, PublishSubmitResult, VerifiedContentPathState, VerifiedIcon,
    VerifiedIconPreview, WorkshopContentRequest, inspect_workshop_snapshot,
};
use crate::generation::Generation;
use crate::media::{
    thumbnail_demand,
    thumbnail_worker::{ThumbnailDecodeError, ThumbnailError, ThumbnailInput},
};
use crate::test_support::TestDir;

#[test]
fn open_new_resets_to_blank_new_mode() {
    let mut state = State::default();
    let _request = open_update(
        &mut state,
        UpdateTarget {
            workshop_id: PublishedFileId::fixture(42),
            title: "Old".to_owned(),
            tags: vec!["map".to_owned()],
            preview_url: None,
            snapshot_request_id: WorkshopSnapshotId::new(1),
            snapshot_destination: "/tmp/workshop-42".into(),
        },
    );

    let _request = open_new(&mut state);

    assert!(state.open());
    assert_eq!(state.mode(), &Mode::New);
    assert_eq!(state.title(), "");
    assert_eq!(state.addon_path(), "");
}

#[test]
fn open_update_prefills_workshop_metadata() {
    let mut state = State::default();

    let request = open_update(
        &mut state,
        UpdateTarget {
            workshop_id: PublishedFileId::fixture(99),
            title: "Workshop Addon".to_owned(),
            tags: vec!["Addon".to_owned(), "map".to_owned(), "scenic".to_owned()],
            preview_url: Some("https://example.invalid/preview.png".to_owned()),
            snapshot_request_id: WorkshopSnapshotId::new(1),
            snapshot_destination: "/tmp/workshop-99".into(),
        },
    );

    assert!(state.open());
    assert!(state.update_mode());
    assert_eq!(state.workshop_id_text(), "99");
    assert_eq!(state.title(), "Workshop Addon");
    assert_eq!(state.addon_path(), "");
    assert_eq!(state.addon_type, Some(AddonType::Map));
    assert_eq!(state.tags[0], Some(AddonTag::Scenic));
    assert_eq!(
        state.workshop_url(),
        Some(workshop_url::workshop_item_url(PublishedFileId::fixture(
            99
        )))
    );
    assert_eq!(
        state.update_warning(&crate::i18n::I18n::for_locale(Some("en"))),
        Some("You are pushing an UPDATE to Workshop Addon (99)".to_owned())
    );
    assert!(state.path_pending());
    assert_eq!(
        request.expect("Workshop content should load").destination,
        PathBuf::from("/tmp/workshop-99")
    );
}

#[test]
fn workshop_download_hydrates_the_current_update_path() {
    let mut state = State::default();
    let workshop_id = PublishedFileId::fixture(99);
    let destination = PathBuf::from("/tmp/workshop-99");
    let _request = open_update(
        &mut state,
        UpdateTarget {
            workshop_id,
            title: "Workshop Addon".to_owned(),
            tags: Vec::new(),
            preview_url: None,
            snapshot_request_id: WorkshopSnapshotId::new(1),
            snapshot_destination: destination.clone(),
        },
    );

    let verification = state
        .apply_workshop_download(
            WorkshopSnapshotId::new(1),
            WorkshopDownloadSuccess {
                item_id: workshop_id,
                installed_path: None,
                extracted_path: destination.clone(),
            },
        )
        .expect("current Workshop snapshot should verify");

    assert_eq!(verification.path, destination);
    assert!(state.path_pending());
}

#[test]
fn inspected_workshop_baseline_is_visible_but_not_a_publish_source() {
    let root = TestDir::new("prepare-publish-baseline");
    root.file("lua/autorun/init.lua", b"print('ready')");
    let destination = root.path().to_path_buf();
    let workshop_id = PublishedFileId::fixture(99);
    let mut state = State::default();
    let _request = open_update(
        &mut state,
        UpdateTarget {
            workshop_id,
            title: "Workshop Addon".to_owned(),
            tags: Vec::new(),
            preview_url: None,
            snapshot_request_id: WorkshopSnapshotId::new(1),
            snapshot_destination: destination.clone(),
        },
    );
    let inspection = state
        .apply_workshop_download(
            WorkshopSnapshotId::new(1),
            WorkshopDownloadSuccess {
                item_id: workshop_id,
                installed_path: None,
                extracted_path: destination,
            },
        )
        .expect("current snapshot should be inspected");
    let snapshot = inspect_workshop_snapshot(inspection.clone()).expect("snapshot inventory");

    assert!(state.apply_snapshot_inspection_result(inspection.generation, Ok(snapshot)));
    assert!(state.browser_snapshot().visible());
    assert_eq!(state.addon_path(), "");
    assert!(!state.can_submit());
}

#[test]
fn late_workshop_download_is_cleaned_after_close() {
    let mut state = State::default();
    let workshop_id = PublishedFileId::fixture(99);
    let destination = PathBuf::from("/tmp/workshop-99");
    let _request = open_update(
        &mut state,
        UpdateTarget {
            workshop_id,
            title: "Workshop Addon".to_owned(),
            tags: Vec::new(),
            preview_url: None,
            snapshot_request_id: WorkshopSnapshotId::new(1),
            snapshot_destination: destination.clone(),
        },
    );

    assert!(state.close().is_empty());
    assert!(
        state
            .apply_workshop_download(
                WorkshopSnapshotId::new(1),
                WorkshopDownloadSuccess {
                    item_id: workshop_id,
                    installed_path: None,
                    extracted_path: destination.clone(),
                }
            )
            .is_none()
    );
    assert_eq!(state.take_pending_cleanup(), vec![destination]);
}

#[test]
fn workshop_snapshot_error_leaves_manual_selection_available() {
    let mut state = State::default();
    let workshop_id = PublishedFileId::fixture(99);
    let destination = PathBuf::from("/tmp/workshop-99");
    let _request = open_update(
        &mut state,
        UpdateTarget {
            workshop_id,
            title: "Workshop Addon".to_owned(),
            tags: Vec::new(),
            preview_url: None,
            snapshot_request_id: WorkshopSnapshotId::new(1),
            snapshot_destination: destination.clone(),
        },
    );
    state.apply_workshop_submission_result(
        WorkshopSnapshotId::new(1),
        Err(UiError::new(ErrorKey::new("DOWNLOAD_FAILED"))),
    );

    assert!(!state.path_pending());
    assert!(state.path_error().is_some());
    assert_eq!(state.take_pending_cleanup(), vec![destination]);
    assert!(
        state
            .begin_content_path_verification("/tmp/manual-addon")
            .is_some()
    );
}

#[test]
fn update_mode_title_is_read_only() {
    let mut state = State::default();
    let _request = open_update(
        &mut state,
        UpdateTarget {
            workshop_id: PublishedFileId::fixture(99),
            title: "Original".to_owned(),
            tags: Vec::new(),
            preview_url: None,
            snapshot_request_id: WorkshopSnapshotId::new(1),
            snapshot_destination: "/tmp/workshop-99".into(),
        },
    );

    state.edit_title("Edited".to_owned());

    assert_eq!(state.title(), "Original");
}

#[test]
fn accepting_addon_path_starts_generation_tagged_verification() {
    let mut state = State::default();
    let _request = open_new(&mut state);
    state.edit_addon_path(" /tmp/addon ".to_owned());

    let request = state
        .begin_current_path_verification()
        .expect("path should verify");

    assert!(state.path_pending());
    assert_eq!(state.addon_path(), "/tmp/addon");
    assert_eq!(request.display_path, "/tmp/addon");
    assert_eq!(request.generation, Generation::from_raw(2));
}

#[test]
fn stale_verification_result_is_ignored() {
    let mut state = State::default();
    let _request = open_new(&mut state);
    let first = state
        .begin_content_path_verification("/tmp/first")
        .expect("first request");
    let _second = state
        .begin_content_path_verification("/tmp/second")
        .expect("second request");

    assert!(!state.apply_verification_result(
        first.generation,
        Err(UiError::new(ErrorKey::new("ERR_BAD")))
    ));
    assert!(state.path_pending());
    assert!(state.path_error().is_none());
}

#[test]
fn duplicate_tag_selection_clears_the_new_slot() {
    let mut state = State::default();
    let _request = open_new(&mut state);

    state.set_tag(0, Some(AddonTag::Fun));
    state.set_tag(1, Some(AddonTag::Fun));

    assert_eq!(state.tags[0], Some(AddonTag::Fun));
    assert_eq!(state.tags[1], None);
}

#[test]
fn changed_ignore_patterns_revalidate_current_path() {
    let mut state = State::default();
    let _request = open_new(&mut state);
    let first = state
        .begin_content_path_verification("/tmp/addon")
        .expect("path should verify before ignore mutation");

    let request = state.apply_ignore_pattern_mutation_result(Ok(IgnorePatternMutationResult {
        changed: true,
        ignored_patterns: vec![IgnoredPattern {
            pattern: "*.tmp".to_owned(),
            default_pattern: false,
        }],
        save_error: None,
    }));

    let request = request.expect("changed ignored patterns should revalidate");
    assert!(request.generation > first.generation);
    assert_eq!(request.display_path, "/tmp/addon");
    assert_eq!(state.ignored_patterns[0].pattern, "*.tmp");
}

#[test]
fn begin_new_submit_builds_core_request() {
    let mut state = ready_new_submit_state();

    let envelope = state
        .begin_submit(submit_context())
        .expect("valid state should submit");

    assert!(state.submit_pending());
    assert_eq!(envelope.generation, Generation::from_raw(1));
    assert_eq!(envelope.request.mode, PublishSubmitMode::New);
    assert_eq!(
        envelope.request.content_source_path,
        PathBuf::from("/tmp/addon")
    );
    assert_eq!(envelope.request.title, "New Addon");
    assert_eq!(envelope.request.addon_type, "map");
    assert_eq!(envelope.request.tags, vec!["fun".to_owned()]);
    assert_eq!(envelope.request.changelog, None);
    assert_eq!(envelope.request.ignore_globs, vec!["*.tmp".to_owned()]);
    assert_eq!(envelope.request.total_size, 42);
    assert_eq!(
        envelope.request.temp_dir,
        PathBuf::from("/tmp/prepare-temp")
    );
    assert_eq!(
        envelope.request.preview,
        Some(PublishSubmitPreview::Default(
            PathBuf::from("/tmp/prepare-temp").join(DEFAULT_WORKSHOP_ICON_FILE_NAME)
        ))
    );
}

#[test]
fn begin_update_submit_uses_workshop_id_changelog_and_no_default_preview() {
    let mut state = State::default();
    let _request = open_update(
        &mut state,
        UpdateTarget {
            workshop_id: PublishedFileId::fixture(99),
            title: "Existing Addon".to_owned(),
            tags: vec!["Addon".to_owned(), "map".to_owned(), "scenic".to_owned()],
            preview_url: None,
            snapshot_request_id: WorkshopSnapshotId::new(1),
            snapshot_destination: "/tmp/workshop-99".into(),
        },
    );
    set_verified_path(&mut state);
    state.edit_changelog("  Fixed things  ");

    let envelope = state
        .begin_submit(submit_context())
        .expect("valid update state should submit");

    assert_eq!(
        envelope.request.mode,
        PublishSubmitMode::Update {
            workshop_id: PublishedFileId::fixture(99),
        }
    );
    assert_eq!(envelope.request.title, "Existing Addon");
    assert_eq!(envelope.request.tags, vec!["scenic".to_owned()]);
    assert_eq!(envelope.request.changelog, Some("Fixed things".to_owned()));
    assert_eq!(envelope.request.preview, None);
}

#[test]
fn stale_submit_completion_is_ignored() {
    let mut state = ready_new_submit_state();
    let envelope = state
        .begin_submit(submit_context())
        .expect("valid state should submit");

    assert!(!state.apply_submit_completion(
        envelope.generation.next(),
        Err(UiError::new(ErrorKey::new("ERR_STALE"))),
    ));
    assert!(state.submit_pending());

    assert!(state.apply_submit_completion(
        envelope.generation,
        Ok(PublishSubmitResult {
            published_file_id: PublishedFileId::fixture(123),
            legal_agreement_required: false,
        }),
    ));
    assert!(!state.submit_pending());
}

#[test]
fn icon_success_stores_selection_and_upscale_flag() {
    let mut state = State::default();
    let _request = open_new(&mut state);
    let request = state
        .begin_icon_verification(
            "/tmp/icon.png".into(),
            "/tmp/prepare-temp".into(),
            [0x10, 0x10, 0x10],
        )
        .expect("open modal should verify icons");

    assert!(state.icon_pending());

    assert!(state.apply_icon_verification_result(
        request.generation,
        Ok(Arc::new(verified_icon_preview("/tmp/icon.png", true))),
    ));

    assert!(state.icon_selected());
    assert!(!state.icon_pending());
    assert!(state.icon_error().is_none());
    assert!(state.can_upscale_icon());
}

#[test]
fn stale_icon_result_is_ignored_after_remove() {
    let mut state = State::default();
    let _request = open_new(&mut state);
    let request = state
        .begin_icon_verification(
            "/tmp/icon.png".into(),
            "/tmp/prepare-temp".into(),
            [0x10, 0x10, 0x10],
        )
        .expect("open modal should verify icons");

    assert!(state.remove_icon());
    assert!(!state.apply_icon_verification_result(
        request.generation,
        Ok(Arc::new(verified_icon_preview("/tmp/icon.png", true))),
    ));

    assert!(!state.icon_selected());
    assert!(!state.can_upscale_icon());
}

#[test]
fn window_unfocus_pauses_icon_animation() {
    let mut state = State::default();
    let _request = open_new(&mut state);
    let mut preview = verified_icon_preview("/tmp/icon.gif", false);
    preview.animation = Some(crate::media::thumbnail_animation::Playback::for_test());
    state.icon = Verification::Verified(preview);
    assert!(state.has_active_icon_animation());

    state.set_window_focused(false);
    assert!(!state.has_active_icon_animation());

    state.set_window_focused(true);
    assert!(state.has_active_icon_animation());
}

#[test]
fn open_seeds_upscale_checkbox_from_settings_default() {
    let mut state = State::default();

    let _request = state.open_target(OpenTarget::New, Vec::new(), true);

    assert!(state.upscale_icon());
    assert!(!state.can_upscale_icon());
}

#[test]
fn removing_the_icon_keeps_the_upscale_preference() {
    let mut state = State::default();
    let _request = state.open_target(OpenTarget::New, Vec::new(), true);
    let request = state
        .begin_icon_verification(
            "/tmp/icon.png".into(),
            "/tmp/prepare-temp".into(),
            [0x10, 0x10, 0x10],
        )
        .expect("open modal should verify icons");
    assert!(state.apply_icon_verification_result(
        request.generation,
        Ok(Arc::new(verified_icon_preview("/tmp/icon.png", true))),
    ));

    assert!(state.remove_icon());

    assert!(state.upscale_icon());
    assert!(!state.can_upscale_icon());
}

#[test]
fn accepted_path_verification_announces_success_but_browse_does_not() {
    let mut state = State::default();
    let _request = open_new(&mut state);

    state.edit_addon_path("/tmp/addon".to_owned());
    let _request = state.begin_accepted_path_verification();
    assert!(state.announce_path_success());

    let _request = state.begin_content_path_verification("/tmp/other");
    assert!(!state.announce_path_success());
}

#[test]
fn publish_icon_submit_requires_update_mode_and_selected_icon() {
    let mut state = State::default();
    let _request = open_new(&mut state);
    assert!(state.begin_publish_icon().is_none());

    let mut state = State::default();
    let _request = open_update(
        &mut state,
        UpdateTarget {
            workshop_id: PublishedFileId::fixture(99),
            title: "Existing".to_owned(),
            tags: Vec::new(),
            preview_url: None,
            snapshot_request_id: WorkshopSnapshotId::new(1),
            snapshot_destination: "/tmp/workshop-99".into(),
        },
    );
    assert!(state.begin_publish_icon().is_none());

    let request = state
        .begin_icon_verification(
            "/tmp/icon.png".into(),
            "/tmp/prepare-temp".into(),
            [0x10, 0x10, 0x10],
        )
        .expect("open modal should verify icons");
    assert!(state.apply_icon_verification_result(
        request.generation,
        Ok(Arc::new(verified_icon_preview("/tmp/icon.png", true))),
    ));
    state.toggle_upscale_icon(true);

    let envelope = state
        .begin_publish_icon()
        .expect("selected icon should submit");

    assert!(state.submit_pending());
    assert_eq!(envelope.workshop_id, PublishedFileId::fixture(99));
    assert_eq!(envelope.icon_source_path, PathBuf::from("/tmp/icon.png"));
    assert!(envelope.upscale);

    assert!(state.begin_publish_icon().is_none());

    assert!(!state.apply_publish_icon_completion(
        envelope.generation.next(),
        Err(UiError::new(ErrorKey::new("ERR_STALE"))),
    ));
    assert!(state.submit_pending());
    assert!(state.apply_publish_icon_completion(
        envelope.generation,
        Ok(
            crate::features::prepare_publish::model::PublishIconSubmitResult {
                legal_agreement_required: false,
            }
        ),
    ));
    assert!(!state.submit_pending());
}

#[test]
fn browser_empty_state_hover_fades_between_dim_and_full() {
    let mut state = State::default();
    let now = std::time::Instant::now();

    state.set_browser_select_hover(true, now);
    assert_eq!(state.browser_select_hover_progress(now), 0.0);

    let _request = open_new(&mut state);
    state.set_browser_select_hover(true, now);

    assert!(state.browser_select_hover_animating(now + std::time::Duration::from_millis(45)));
    let mid = state.browser_select_hover_progress(now + std::time::Duration::from_millis(45));
    assert!(mid > 0.0 && mid < 1.0);
    assert_eq!(
        state.browser_select_hover_progress(now + std::time::Duration::from_millis(300)),
        1.0
    );
    assert!(!state.browser_select_hover_animating(now + std::time::Duration::from_millis(300)));

    state.set_browser_select_hover(false, now + std::time::Duration::from_millis(300));
    assert_eq!(
        state.browser_select_hover_progress(now + std::time::Duration::from_millis(600)),
        0.0
    );
}

#[test]
fn spinner_ticks_only_while_a_submit_is_pending() {
    let mut state = ready_new_submit_state();
    let now = std::time::Instant::now();

    assert!(!state.tick_submit_spinner(now));
    assert_eq!(state.spinner_elapsed(), 0.0);

    let _envelope = state
        .begin_submit(submit_context())
        .expect("valid state should submit");

    assert!(state.tick_submit_spinner(now + std::time::Duration::from_millis(500)));
    assert!(state.spinner_elapsed() > 0.0);
}

#[test]
fn changelog_content_round_trips_through_editor_actions() {
    let mut state = State::default();
    let _request = open_update(
        &mut state,
        UpdateTarget {
            workshop_id: PublishedFileId::fixture(7),
            title: "Existing".to_owned(),
            tags: Vec::new(),
            preview_url: None,
            snapshot_request_id: WorkshopSnapshotId::new(1),
            snapshot_destination: "/tmp/workshop-7".into(),
        },
    );

    assert!(state.changelog_is_empty());

    state.perform_changelog_action(iced::widget::text_editor::Action::Edit(
        iced::widget::text_editor::Edit::Paste(Arc::new("Fixed things".to_owned())),
    ));

    assert!(!state.changelog_is_empty());
    assert_eq!(state.changelog_trimmed(), "Fixed things");
}

/// `text_editor::Content` is not `Clone`, so [`ChangelogContent`] hand-rolls
/// `Clone` (reconstruct from text) and `PartialEq` (compare text). Guarded
/// directly rather than through a `State`-level `assert_eq!(state.clone(),
/// state)`, where a break in either impl reads as an unrelated state failure.
#[test]
fn changelog_content_clone_round_trips_its_text() {
    let content = ChangelogContent::from_text("Fixed things");
    let cloned = content.clone();

    assert_eq!(cloned, content);
    assert_eq!(cloned.text(), "Fixed things");
}

#[test]
fn open_update_seeds_workshop_preview_display_only() {
    let mut state = State::default();
    let _request = open_update(
        &mut state,
        UpdateTarget {
            workshop_id: PublishedFileId::fixture(99),
            title: "Existing".to_owned(),
            tags: Vec::new(),
            preview_url: Some("https://example.invalid/preview.png".to_owned()),
            snapshot_request_id: WorkshopSnapshotId::new(1),
            snapshot_destination: "/tmp/workshop-99".into(),
        },
    );

    let set = state.thumbnail_demands();
    assert_eq!(set.demands.len(), 1);
    assert_eq!(
        set.demands[0].id.workshop_id(),
        Some(PublishedFileId::fixture(99))
    );
    assert!(state.icon_handle().is_none());

    // Stale generation and failures never seed.
    assert!(!state.apply_thumbnail_delivery(
        &seed_delivery(set.generation.next(), 99),
        [0x10, 0x10, 0x10],
    ));
    assert!(!state.apply_thumbnail_delivery(
        &failed_seed_delivery(set.generation, 99),
        [0x10, 0x10, 0x10],
    ));
    assert!(state.icon_handle().is_none());

    assert!(state.apply_thumbnail_delivery(&seed_delivery(set.generation, 99), [0x10, 0x10, 0x10]));

    // Display-only: no icon file is selected.
    assert!(state.icon_handle().is_some());
    assert!(state.icon_backdrop_handle().is_some());
    assert!(!state.icon_selected());
    assert!(!state.can_publish_icon());
    assert!(state.begin_publish_icon().is_none());
    // The seed satisfied the demand; nothing further is requested.
    assert!(state.thumbnail_demands().demands.is_empty());

    // New-publish mode never demands a seed.
    let mut fresh = State::default();
    let _request = open_new(&mut fresh);
    assert!(fresh.thumbnail_demands().demands.is_empty());
}

#[test]
fn browsed_icon_replaces_the_seeded_preview() {
    let mut state = State::default();
    let _request = open_update(
        &mut state,
        UpdateTarget {
            workshop_id: PublishedFileId::fixture(99),
            title: "Existing".to_owned(),
            tags: Vec::new(),
            preview_url: Some("https://example.invalid/preview.png".to_owned()),
            snapshot_request_id: WorkshopSnapshotId::new(1),
            snapshot_destination: "/tmp/workshop-99".into(),
        },
    );
    let set = state.thumbnail_demands();
    assert!(state.apply_thumbnail_delivery(&seed_delivery(set.generation, 99), [0x10, 0x10, 0x10]));
    let seeded_id = state.icon_handle().expect("seeded preview").id();

    let request = state
        .begin_icon_verification(
            "/tmp/icon.png".into(),
            "/tmp/prepare-temp".into(),
            [0x10, 0x10, 0x10],
        )
        .expect("open modal should verify icons");
    assert!(state.apply_icon_verification_result(
        request.generation,
        Ok(Arc::new(verified_icon_preview("/tmp/icon.png", true))),
    ));

    assert!(state.icon_selected());
    assert!(state.can_publish_icon());
    assert_ne!(state.icon_handle().expect("picked icon").id(), seeded_id);
}

fn seed_delivery(generation: Generation, workshop_id: u64) -> thumbnail_demand::Delivery {
    let input = ThumbnailInput::from_url("https://example.invalid/preview.png");
    let key = input.cache_key(super::SEED_THUMBNAIL_MAX_EDGE);
    let metadata = crate::media::thumbnail_worker::ThumbnailMetadata {
        width: 8,
        height: 8,
        source_width: 8,
        source_height: 8,
        max_edge: super::SEED_THUMBNAIL_MAX_EDGE,
    };
    thumbnail_demand::Delivery {
        owner: thumbnail_demand::Owner::PreparePublish,
        generation,
        id: thumbnail_demand::DemandId::workshop(PublishedFileId::fixture(workshop_id)),
        key: key.clone(),
        result: thumbnail_demand::DeliveryResult::Ready(
            thumbnail_demand::ReadyThumbnail::for_test(key, metadata, vec![200_u8; 8 * 8 * 4]),
        ),
    }
}

fn failed_seed_delivery(generation: Generation, workshop_id: u64) -> thumbnail_demand::Delivery {
    let input = ThumbnailInput::from_url("https://example.invalid/preview.png");
    thumbnail_demand::Delivery {
        owner: thumbnail_demand::Owner::PreparePublish,
        generation,
        id: thumbnail_demand::DemandId::workshop(PublishedFileId::fixture(workshop_id)),
        key: input.cache_key(super::SEED_THUMBNAIL_MAX_EDGE),
        result: thumbnail_demand::DeliveryResult::Failed {
            error: thumbnail_demand::ThumbnailDeliveryError::Thumbnail(Arc::new(
                ThumbnailError::Decode(ThumbnailDecodeError::InvalidMaxEdge),
            )),
        },
    }
}

fn verified_icon_preview(display_path: &str, can_upscale: bool) -> VerifiedIconPreview {
    VerifiedIconPreview {
        icon: VerifiedIcon {
            display_path: display_path.to_owned(),
            source_path: display_path.into(),
            path: display_path.into(),
            format: IconFormat::Png,
            width: 1,
            height: 1,
            byte_size: 4,
            can_upscale,
        },
        still: image::Handle::from_rgba(1, 1, vec![255, 0, 0, 255]),
        backdrop: image::Handle::from_rgba(1, 1, vec![255, 0, 0, 255]),
        animation: None,
    }
}

fn open_new(state: &mut State) -> Option<ContentPathVerificationRequest> {
    assert!(
        state
            .open_target(OpenTarget::New, Vec::new(), false)
            .is_none()
    );
    None
}

fn open_update(state: &mut State, target: UpdateTarget) -> Option<WorkshopContentRequest> {
    state.open_target(OpenTarget::Update(target), Vec::new(), false)
}

fn ready_new_submit_state() -> State {
    let mut state = State::default();
    let _request = open_new(&mut state);
    set_verified_path(&mut state);
    state.edit_title("  New Addon  ".to_owned());
    state.set_addon_type(Some(AddonType::Map));
    state.set_tag(0, Some(AddonTag::Fun));
    state
}

fn set_verified_path(state: &mut State) {
    state.active_workshop_request = None;
    state.workshop_loads.clear();
    state.addon_path = "/tmp/addon".to_owned();
    state.content_path = Verification::Verified(VerifiedContentPathState {
        display_path: "/tmp/addon".to_owned(),
        path: PathBuf::from("/tmp/addon"),
        total_size: 42,
    });
}

#[test]
fn entry_preview_request_reads_the_verified_folder_source() {
    use crate::bridge::archive::PreviewArchiveSource;

    let mut state = State::default();
    let _request = state.open_target(OpenTarget::New, Vec::new(), true);
    assert!(state.entry_preview_request("lua/init.lua").is_none());

    state.preview_source = Some(PreviewArchiveSource::from_folder([(
        "lua/init.lua".to_owned(),
        9,
        PathBuf::from("/tmp/addon/lua/init.lua"),
    )]));

    let request = state
        .entry_preview_request("lua/init.lua")
        .expect("verified source should produce a preview request");
    assert_eq!(request.entry_path, "lua/init.lua");
    assert_eq!(request.display_name, "init.lua");
    assert_eq!(request.size_bytes, 9);
    assert!(state.entry_preview_request("lua/missing.lua").is_none());

    state.edit_addon_path("/tmp/other".to_owned());
    assert!(state.entry_preview_request("lua/init.lua").is_none());
}

fn submit_context() -> PublishSubmitContext {
    PublishSubmitContext {
        ignore_globs: vec!["*.tmp".to_owned()],
        temp_dir: PathBuf::from("/tmp/prepare-temp"),
    }
}

fn en() -> crate::i18n::I18n {
    crate::i18n::I18n::for_locale(Some("en"))
}

fn workshop_update_target() -> UpdateTarget {
    UpdateTarget {
        workshop_id: PublishedFileId::fixture(77),
        title: "Workshop Addon".to_owned(),
        tags: Vec::new(),
        preview_url: None,
        snapshot_request_id: WorkshopSnapshotId::new(1),
        snapshot_destination: PathBuf::from("/tmp/workshop-77"),
    }
}

#[test]
fn a_blank_new_addon_names_every_requirement_it_needs() {
    let mut state = State::default();
    let _request = open_new(&mut state);

    let missing = state.blockers().missing().collect::<Vec<_>>();

    assert_eq!(
        missing,
        vec![
            Requirement::AddonPath,
            Requirement::Title,
            Requirement::AddonType,
            Requirement::Tag,
        ]
    );
    assert!(!state.can_submit());
}

#[test]
fn an_update_owes_a_changelog_instead_of_a_title() {
    let mut state = State::default();
    let _request = open_update(&mut state, workshop_update_target());
    set_verified_path(&mut state);
    state.set_addon_type(Some(AddonType::Map));
    state.set_tag(0, Some(AddonTag::Fun));

    let blockers = state.blockers();

    assert!(blockers.contains(Requirement::Changelog));
    // The title is locked to the workshop page in update mode, so it is never
    // something the user can be asked to supply here.
    assert!(!blockers.contains(Requirement::Title));
}

#[test]
fn verification_in_flight_holds_the_missing_list_back() {
    let mut state = State::default();
    let _request = open_new(&mut state);
    state.content_path = Verification::Pending;

    let blockers = state.blockers();

    assert!(blockers.pending());
    assert_eq!(blockers.missing().count(), 0);
    assert_eq!(
        state.submit_tooltip(&en()).as_deref(),
        Some("Verifying addon contents...")
    );
}

#[test]
fn the_submit_tooltip_lists_what_is_missing_beneath_the_update_warning() {
    let mut state = State::default();
    let _request = open_update(&mut state, workshop_update_target());
    set_verified_path(&mut state);
    state.set_addon_type(Some(AddonType::Map));
    state.set_tag(0, Some(AddonTag::Fun));

    let tooltip = state
        .submit_tooltip(&en())
        .expect("an update always carries its warning");

    assert!(tooltip.contains("Workshop Addon"));
    assert!(tooltip.contains("Still needed:"));
    assert!(tooltip.contains("\u{2022} Changelog"));
    assert!(!tooltip.contains("\u{2022} Title"));
}

#[test]
fn an_unchosen_select_does_not_repeat_its_own_label() {
    let i18n = en();
    let mut state = State::default();
    let _request = open_new(&mut state);

    let face = state.addon_type_label(&i18n);
    let unchosen = state.addon_type_options(&i18n);
    assert!(
        !unchosen.iter().any(|(_, label)| *label == face),
        "the menu echoed the control's own face: {face:?}"
    );

    // Once something is chosen the row becomes a way back to nothing, so it
    // earns its place rather than duplicating the face.
    state.set_addon_type(Some(AddonType::Map));
    let chosen = state.addon_type_options(&i18n);
    assert_eq!(chosen.len(), unchosen.len() + 1);
    assert_ne!(state.addon_type_label(&i18n), face);
    assert!(chosen.iter().any(|(_, label)| *label == face));
}

#[test]
fn an_unchosen_tag_slot_does_not_repeat_its_own_label() {
    let i18n = en();
    let mut state = State::default();
    let _request = open_new(&mut state);

    let face = state.tag_label(0, &i18n);
    assert!(
        !state
            .tag_options(0, &i18n)
            .iter()
            .any(|(_, label)| *label == face),
        "the menu echoed the slot's own face: {face:?}"
    );

    state.set_tag(0, Some(AddonTag::Fun));

    assert!(
        state
            .tag_options(0, &i18n)
            .iter()
            .any(|(_, label)| *label == face)
    );
}
