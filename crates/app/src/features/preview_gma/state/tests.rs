use super::*;
use crate::backend::gma::PreviewArchive;
use crate::test_support::GmaFixtureBuilder;

fn target() -> OpenTarget {
    OpenTarget::new(
        PathBuf::from("/tmp/local.gma"),
        "Local Addon",
        Some(PublishedFileId::new(42).expect("test fixture ids are always nonzero")),
    )
}

fn loaded_archive() -> LoadedArchive {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Fixture")
            .entry("lua/autorun/init.lua", b"print('ok')\n".to_vec())
            .entry("materials/icon.vmt", b"shader".to_vec())
            .build(),
    )
    .expect("fixture archive should load");
    LoadedArchive::from_archive(archive)
}

#[test]
fn begin_open_marks_modal_loading_and_emits_worker_request() {
    let mut state = State::default();

    let request = state.begin_open(target());

    assert!(state.is_open());
    assert!(state.loading());
    assert_eq!(state.title(), "Local Addon");
    assert_eq!(
        state.workshop_id(),
        Some(PublishedFileId::new(42).expect("test fixture ids are always nonzero"))
    );
    assert_eq!(request.request_id, 1);
    assert_eq!(request.path, PathBuf::from("/tmp/local.gma"));
}

#[test]
fn loaded_archive_populates_browser_snapshot() {
    let mut state = State::default();
    let request = state.begin_open(target());

    assert!(state.apply_archive_opened(request.request_id, Ok(loaded_archive())));

    assert!(!state.loading());
    assert!(state.error().is_none());
    assert!(state.archive().is_some());
    assert!(state.can_extract());
    assert!(state.browser_snapshot().visible());
    assert_eq!(state.browser_snapshot().total_files(), 2);
    assert_eq!(
        state
            .browser_snapshot()
            .rows()
            .iter()
            .map(|row| row.display_name.as_str())
            .collect::<Vec<_>>(),
        vec!["autorun", "materials"]
    );
}

#[test]
fn stale_worker_result_is_ignored() {
    let mut state = State::default();
    let first = state.begin_open(target());
    let _second = state.begin_open(OpenTarget::new(
        PathBuf::from("/tmp/other.gma"),
        "Other",
        None,
    ));

    assert!(!state.apply_archive_opened(first.request_id, Ok(loaded_archive())));
    assert!(state.loading());
    assert_eq!(state.title(), "Other");
    assert!(state.archive().is_none());
}

#[test]
fn browser_navigation_refreshes_visible_rows() {
    let mut state = State::default();
    let request = state.begin_open(target());
    state.apply_archive_opened(request.request_id, Ok(loaded_archive()));

    assert!(state.open_directory("lua/autorun"));
    assert_eq!(state.browser_snapshot().rows()[0].display_name, "init.lua");
    assert!(state.browser_snapshot().can_go_up());

    assert!(state.go_up());
    assert_eq!(state.browser_snapshot().rows()[0].display_name, "autorun");
}

#[test]
fn archive_extraction_request_is_current_and_one_shot() {
    let mut state = State::default();
    let request = state.begin_open(target());
    state.apply_archive_opened(request.request_id, Ok(loaded_archive()));

    assert!(state.request_archive_extraction());
    assert!(state.has_pending_extraction());

    let extraction = state
        .take_pending_archive_extraction()
        .expect("archive extraction should be pending");
    assert_eq!(extraction.request_id, request.request_id);
    assert!(matches!(
        extraction.intent,
        ExtractionIntent::Archive { total_bytes: 18 }
    ));
    assert!(!state.has_pending_extraction());
    assert!(state.take_pending_archive_extraction().is_none());
}

#[test]
fn pending_archive_extraction_is_cleared_when_modal_closes() {
    let mut state = State::default();
    let request = state.begin_open(target());
    state.apply_archive_opened(request.request_id, Ok(loaded_archive()));
    assert!(state.request_archive_extraction());

    state.close();

    assert!(!state.has_pending_extraction());
    assert!(state.take_pending_archive_extraction().is_none());
}

#[test]
fn entry_extraction_request_validates_archive_entry() {
    let mut state = State::default();
    let request = state.begin_open(target());
    state.apply_archive_opened(request.request_id, Ok(loaded_archive()));

    let extraction = state
        .entry_extraction_request("materials/icon.vmt")
        .expect("existing file should be extractable");

    assert_eq!(extraction.request_id, request.request_id);
    assert!(matches!(
        extraction.intent,
        ExtractionIntent::Entry {
            ref path,
            size_bytes: 6,
        } if path == "materials/icon.vmt"
    ));
    assert!(state.entry_extraction_request("missing.txt").is_none());
}

fn seeded_target() -> OpenTarget {
    OpenTarget::new(
        PathBuf::from("/tmp/local.gma"),
        "Grid Title",
        Some(PublishedFileId::new(42).expect("test fixture ids are always nonzero")),
    )
    .with_seed(OpenSeed {
        preview_url: Some("https://example.invalid/preview.jpg".to_owned()),
        subscription_count: Some(12_345),
        score_bucket: Some(4),
        score_label: Some("80.00%".to_owned()),
    })
}

fn ready_delivery(generation: u64) -> thumbnail_demand::Delivery {
    let input = ThumbnailInput::from_url("https://example.invalid/preview.jpg");
    let key = input.cache_key(PREVIEW_THUMBNAIL_MAX_EDGE);
    let metadata = crate::media::thumbnail_worker::ThumbnailMetadata {
        width: 8,
        height: 8,
        source_width: 8,
        source_height: 8,
        max_edge: PREVIEW_THUMBNAIL_MAX_EDGE,
    };
    thumbnail_demand::Delivery {
        owner: thumbnail_demand::Owner::PreviewGma,
        generation,
        id: thumbnail_demand::DemandId::new(PREVIEW_THUMBNAIL_DEMAND_ID),
        key: key.clone(),
        result: thumbnail_demand::DeliveryResult::Ready(
            thumbnail_demand::ReadyThumbnail::for_test(key, metadata, vec![7_u8; 8 * 8 * 4]),
        ),
    }
}

#[test]
fn open_seeds_the_thumbnail_demand_before_the_archive_loads() {
    let mut state = State::default();
    let request = state.begin_open(seeded_target());

    // The demand goes out immediately (grid RAM cache serves it), even
    // while the archive-open worker is still running.
    assert!(state.loading());
    assert!(state.thumbnail_loading());
    let demands = state.thumbnail_demands();
    assert_eq!(demands.demands.len(), 1);
    assert_eq!(demands.generation, request.request_id);

    assert!(state.apply_thumbnail_delivery(&ready_delivery(request.request_id)));
    assert!(state.thumbnail_handle().is_some());
    assert!(state.thumbnail_demands().demands.is_empty());

    // Metadata arriving with the SAME url must not reset the image.
    state.apply_archive_opened(request.request_id, Ok(loaded_archive()));
    let metadata = WorkshopMetadata {
        id: PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        title: "Remote".to_owned(),
        author: None,
        steamid64: None,
        avatar: None,
        time_created: 0,
        time_updated: 0,
        description: String::new(),
        tags: Vec::new(),
        preview_url: Some("https://example.invalid/preview.jpg".to_owned()),
        subscriptions: 12_345,
        score_bucket: 4,
        score_label: "80.00%".to_owned(),
    };
    assert!(state.apply_workshop_metadata(
        request.request_id,
        PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        Ok(Some(metadata))
    ));
    assert!(state.thumbnail_handle().is_some());
    assert!(!state.thumbnail_loading());
}

#[test]
fn pending_image_square_is_reserved_until_metadata_settles() {
    // Workshop id but no seeded URL: the square shows the spinner, never
    // a dead flash, until metadata resolves (or fails).
    let mut state = State::default();
    let request = state.begin_open(target());
    assert!(state.thumbnail_loading());

    state.apply_archive_opened(request.request_id, Ok(loaded_archive()));
    assert!(state.thumbnail_loading());

    assert!(state.apply_workshop_metadata(
        request.request_id,
        PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        Err(UiError::new(gmpublished_backend::error_key::ErrorKey(
            "ERR_TEST"
        )))
    ));
    assert!(!state.thumbnail_loading());
    assert!(state.thumbnail_handle().is_none());

    // No workshop id and no seed: settles dead immediately.
    let mut plain = State::default();
    let _request = plain.begin_open(OpenTarget::new(
        PathBuf::from("/tmp/plain.gma"),
        "Plain",
        None,
    ));
    assert!(!plain.thumbnail_loading());
}

#[test]
fn click_time_stats_and_title_render_before_hydration() {
    let mut state = State::default();
    let request = state.begin_open(seeded_target());
    state.apply_archive_opened(request.request_id, Ok(loaded_archive()));

    // First loaded frame: grid data, no zero flash, click title.
    let details = state.details();
    assert!(details.has_stats);
    assert_eq!(details.subscriptions, "12,345");
    assert_eq!(details.score_bucket, 4);
    assert_eq!(details.title, "Grid Title");

    let metadata = WorkshopMetadata {
        id: PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        title: "Workshop Title".to_owned(),
        author: None,
        steamid64: None,
        avatar: None,
        time_created: 0,
        time_updated: 0,
        description: String::new(),
        tags: Vec::new(),
        preview_url: None,
        subscriptions: 12_400,
        score_bucket: 5,
        score_label: "92.00%".to_owned(),
    };
    assert!(state.apply_workshop_metadata(
        request.request_id,
        PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        Ok(Some(metadata))
    ));
    let details = state.details();
    assert_eq!(details.subscriptions, "12,400");
    assert_eq!(details.title, "Workshop Title");
}

#[test]
fn author_fetch_is_one_shot_and_generation_guarded() {
    let mut state = State::default();
    let request = state.begin_open(target());
    state.apply_archive_opened(request.request_id, Ok(loaded_archive()));

    let metadata = WorkshopMetadata {
        id: PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        title: "Remote".to_owned(),
        author: None,
        steamid64: Some(76_561_197_990_735_296),
        avatar: None,
        time_created: 0,
        time_updated: 0,
        description: String::new(),
        tags: Vec::new(),
        preview_url: None,
        subscriptions: 0,
        score_bucket: 0,
        score_label: String::new(),
    };
    assert!(state.apply_workshop_metadata(
        request.request_id,
        PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        Ok(Some(metadata))
    ));

    let author_request = state
        .take_author_request()
        .expect("missing owner should request a profile fetch");
    assert_eq!(author_request.steamid64, 76_561_197_990_735_296);
    assert!(state.take_author_request().is_none(), "one-shot");

    assert!(!state.apply_author_result(
        author_request.request_id + 1,
        author_request.steamid64,
        Ok(super::AuthorInfo {
            name: "Ada".to_owned(),
            avatar: None,
        }),
    ));

    assert!(state.apply_author_result(
        author_request.request_id,
        author_request.steamid64,
        Ok(super::AuthorInfo {
            name: "Ada".to_owned(),
            avatar: None,
        }),
    ));
    let author = state.details().author.as_ref().expect("author row");
    assert_eq!(author.name, "Ada");
    assert!(!author.failed);

    // A live detail refresh racing the progressive author result must not
    // replace the resolved persona with its owner-id placeholder.
    let refreshed = WorkshopMetadata {
        id: PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        title: "Refreshed".to_owned(),
        author: None,
        steamid64: Some(author_request.steamid64),
        avatar: None,
        time_created: 0,
        time_updated: 0,
        description: "Fresh description".to_owned(),
        tags: Vec::new(),
        preview_url: None,
        subscriptions: 0,
        score_bucket: 0,
        score_label: String::new(),
    };
    assert!(state.apply_workshop_metadata(
        request.request_id,
        PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        Ok(Some(refreshed)),
    ));
    assert_eq!(
        state
            .details()
            .author
            .as_ref()
            .map(|author| author.name.as_str()),
        Some("Ada")
    );
    assert_eq!(
        state.details().description.plain_text(),
        "Fresh description"
    );

    assert!(state.apply_workshop_metadata(
        request.request_id,
        PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        Err(UiError::new(
            gmpublished_backend::error_key::keys::STEAM_ERROR
        )),
    ));
    assert_eq!(
        state.details().description.plain_text(),
        "Fresh description"
    );

    // A failed fetch keeps the placeholder and flags the row.
    let mut failed_state = State::default();
    let request = failed_state.begin_open(target());
    failed_state.apply_archive_opened(request.request_id, Ok(loaded_archive()));
    let metadata = WorkshopMetadata {
        id: PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        title: "Remote".to_owned(),
        author: None,
        steamid64: Some(76_561_197_990_735_296),
        avatar: None,
        time_created: 0,
        time_updated: 0,
        description: String::new(),
        tags: Vec::new(),
        preview_url: None,
        subscriptions: 0,
        score_bucket: 0,
        score_label: String::new(),
    };
    failed_state.apply_workshop_metadata(
        request.request_id,
        PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        Ok(Some(metadata)),
    );
    let author_request = failed_state.take_author_request().expect("fetch");
    assert!(failed_state.apply_author_result(
        author_request.request_id,
        author_request.steamid64,
        Err(UiError::new(
            gmpublished_backend::error_key::keys::STEAM_ERROR
        )),
    ));
    let author = failed_state.details().author.as_ref().expect("author row");
    assert_eq!(author.name, "STEAM_1:0:15234784");
    assert!(author.failed);
}

#[test]
fn window_unfocus_pauses_thumbnail_animation() {
    let mut state = State::default();
    let request = state.begin_open(target());
    state.apply_archive_opened(request.request_id, Ok(loaded_archive()));
    state.thumbnail = ThumbnailState::Ready {
        still: image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]),
        animation: Some(thumbnail_animation::Playback::for_test()),
    };
    assert!(state.has_active_animation());

    assert!(state.set_window_focused(false));
    assert!(!state.has_active_animation());
    // Spinner-driven ticks must not advance the paused GIF.
    let now = Instant::now();
    let _ = state.tick_animation(now);
    assert!(!state.tick_animation(now + std::time::Duration::from_secs(1)));

    assert!(state.set_window_focused(true));
    assert!(state.has_active_animation());
}

#[test]
fn copy_and_workshop_targets_are_derived_from_loaded_archive() {
    let mut state = State::default();
    let request = state.begin_open(target());
    state.apply_archive_opened(request.request_id, Ok(loaded_archive()));

    assert_eq!(
        state.workshop_link_url(),
        Some("https://steamcommunity.com/sharedfiles/filedetails/?id=42".to_owned())
    );
    assert_eq!(
        state.copy_current_path_text(),
        Some("/tmp/local.gma".replace('/', std::path::MAIN_SEPARATOR_STR))
    );
    assert!(state.open_directory("lua/autorun"));
    assert_eq!(
        state.copy_current_path_text(),
        Some("/tmp/local.gma/lua/autorun".replace('/', std::path::MAIN_SEPARATOR_STR))
    );
}
