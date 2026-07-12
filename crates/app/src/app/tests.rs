use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::backend::domain::{
    AvatarRgba, InstalledAddon, PublishedFileId, SearchHit, SearchItem, SearchItemSource,
    SearchQuickBatch, SearchQuickCarry, SearchQuickRequest, SteamUser, workshop_url,
};
use gmpublished_backend::appdata::{
    AppDataPathsSnapshot as BackendAppDataPathsSnapshot, AppDataSnapshot as BackendAppDataSnapshot,
    Settings as BackendSettings,
};
use iced::{Point, Size, keyboard, theme::Mode, widget::image, window};

#[cfg(feature = "asset-studio")]
use crate::{backend::archive::PreviewArchiveSource, features::file_preview};
use crate::{
    backend::{
        DownloadCountFormat, ExtractDestination, SystemColorScheme, ThemePreset,
        gma::{GmaError, GmaHeader, GmaMeta, GmaMetadata, PreviewArchive, PreviewExtractOptions},
        library::{LibraryRefresh, LibraryRefreshReason, LibrarySnapshot},
        tasks::{
            BackendRuntimeAction, BackendRuntimeEvent, TaskId, TaskKind, WorkshopDownloadTaskKind,
        },
    },
    features::{
        context_menu, destination_select, downloader, installed_addons, modal_stack, my_workshop,
        prepare_publish, preview_gma, search, settings, shell, size_analyzer, steam_session,
    },
    test_support::GmaFixtureBuilder,
    theme::AccentInputs,
};

use super::{
    AddonDragMessage, AddonDragOutcome, AddonDragSource, AddonDragState, App, ContextMenuTarget,
    GlobalShortcut, LocalMenuTarget, RootMessage, State, backend_runtime_action_message,
    map_global_shortcut, map_settings_toggle_shortcut, system_scheme_from_mode,
};
use crate::backend::ui_error::UiError;

#[test]
fn new_uses_default_root_state_and_dark_theme() {
    let (app, _startup_task) = App::new_for_test();

    assert_eq!(app.state.title, State::default().title);
    assert_eq!(app.state.shell.route(), shell::Route::MyWorkshop);
    assert_eq!(app.state.shell.account_name(), None);
    assert!(app.state.my_workshop.is_route_visible());
    assert_eq!(
        app.state.tokens.iced_theme().palette().background,
        app.state.tokens.colors.bg.into()
    );
    assert_eq!(app.theme(), None);
    assert_eq!(app.title(), "gmpublished");
}

#[test]
fn update_delegates_modal_stack_messages_to_modal_state() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::OpenDestinationSelect,
    ));

    assert!(app.state.modal_stack.overlay_active());
    assert_eq!(app.state.modal_stack.active(), None);
}

#[test]
fn modal_stack_close_clears_preview_gma_state() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::PreviewGma(
        preview_gma::Message::OpenRequested(preview_gma::OpenTarget::new(
            PathBuf::from("/tmp/test.gma"),
            "Test",
            Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
        )),
    ));

    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
    assert!(app.state.preview_gma.is_open());

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));

    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
    assert!(app.state.preview_gma.is_open());

    let _task = app.update(RootMessage::AnimationTick(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    ));

    assert_eq!(app.state.modal_stack.active(), None);
    assert!(!app.state.preview_gma.is_open());
}

#[test]
fn destination_overlay_layers_over_preview_and_closes_first() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::PreviewGma(
        preview_gma::Message::OpenRequested(preview_gma::OpenTarget::new(
            PathBuf::from("/tmp/test.gma"),
            "Test",
            Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
        )),
    ));
    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::OpenDestinationSelect,
    ));

    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
    assert!(app.state.modal_stack.overlay_active());
    assert!(app.state.preview_gma.is_open());

    // Scrim/Escape close targets the overlay; the preview stays put.
    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));
    let _task = app.update(RootMessage::AnimationTick(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    ));

    assert!(!app.state.modal_stack.overlay_active());
    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
    assert!(app.state.preview_gma.is_open());
}

#[test]
fn prepare_publish_open_claims_modal_stack_slot() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::PreparePublish(
        prepare_publish::Message::OpenRequested {
            target: prepare_publish::OpenTarget::New,
            ignored_patterns: Vec::new(),
            upscale_icon_default: false,
        },
    ));

    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreparePublish)
    );
    assert!(app.state.prepare_publish.open());
}

#[test]
fn modal_stack_close_clears_prepare_publish_state() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::PreparePublish(
        prepare_publish::Message::OpenRequested {
            target: prepare_publish::OpenTarget::New,
            ignored_patterns: Vec::new(),
            upscale_icon_default: false,
        },
    ));
    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));

    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreparePublish)
    );
    assert!(app.state.prepare_publish.open());

    let _task = app.update(RootMessage::AnimationTick(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    ));

    assert_eq!(app.state.modal_stack.active(), None);
    assert!(!app.state.prepare_publish.open());
}

#[test]
fn settings_activation_claims_modal_stack_slot() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::Shell(shell::Message::SettingsActivated));
    let snapshot = app.settings_snapshot();
    let _task = app.update(RootMessage::Settings(settings::Message::OpenRequested(
        snapshot,
    )));

    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::Settings)
    );
    assert!(app.state.settings.open());
}

#[test]
fn shell_executor_open_settings_schedules_settings_task() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(vec![shell::Effect::OpenSettings], App::run_shell_effect);

    assert_eq!(task.units(), 1);
}

#[test]
fn shell_executor_open_url_schedules_native_open_task() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![shell::Effect::OpenUrl(
            "https://example.com/test".to_owned(),
        )],
        App::run_shell_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn shell_executor_begin_window_drag_schedules_window_drag_when_window_known() {
    let (mut app, _startup_task) = App::new_for_test();
    app.window_id = Some(window::Id::unique());

    let task = app.batch_effects(vec![shell::Effect::BeginWindowDrag], App::run_shell_effect);

    assert_eq!(task.units(), 1);
}

#[test]
fn shell_executor_toggle_maximize_schedules_window_toggle_when_window_known() {
    let (mut app, _startup_task) = App::new_for_test();
    app.window_id = Some(window::Id::unique());

    let task = app.batch_effects(vec![shell::Effect::ToggleMaximize], App::run_shell_effect);

    assert_eq!(task.units(), 1);
}

#[test]
fn downloader_executor_submission_schedules_worker_task() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![downloader::Effect::WorkshopSubmissionAccepted(vec![
            PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
        ])],
        App::run_downloader_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn downloader_executor_cancels_requested_tasks() {
    let (mut app, _startup_task) = App::new_for_test();
    let handle = app.ctx.create_task(TaskKind::Download, "working");
    let task_id = handle.id();
    let transaction = app.ctx.begin_transaction();
    app.ctx
        .correlate_backend_transaction(transaction.id, handle);

    let task = app.batch_effects(
        vec![downloader::Effect::TaskCancellationRequested(vec![task_id])],
        App::run_downloader_effect,
    );

    assert_eq!(task.units(), 0);
    assert!(transaction.aborted());
}

#[test]
fn workshop_download_row_cancel_button_aborts_the_backend_transaction() {
    let (mut app, _startup_task) = App::new_for_test();

    // The backend queues a Steam download: DownloadStarted correlates a
    // fresh task with the transaction (the same path backend_event_task
    // takes when the event arrives over the wire).
    let transaction = app.ctx.begin_transaction();
    let _task = app.update(RootMessage::BackendEvent(
        BackendRuntimeEvent::DownloadStarted {
            transaction_id: transaction.id,
        },
    ));

    // The item id arrives as transaction data; feeding the resulting action
    // back into update() mirrors backend_event_task's Task::done loop.
    let actions = app
        .ctx
        .handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            crate::backend::tasks::TransactionRuntimeEvent::Data {
                id: transaction.id,
                payload: gmpublished_backend::events::TransactionPayload::WorkshopItem(
                    gmpublished_backend::appdata::SettingsPublishedFileId(123),
                ),
            },
        ))
        .into_actions();
    assert!(!actions.is_empty(), "workshop item data must start the row");
    for action in actions {
        let _task = app.update(backend_runtime_action_message(action));
    }
    assert_eq!(app.state.downloader.downloading().len(), 1);

    // Pressing the row's X sends CancelRequested; the row leaves the list
    // immediately and its effect aborts the very transaction the download
    // worker polls.
    let row_id = app.state.downloader.downloading()[0].id();
    let _task = app.update(RootMessage::Downloader(
        downloader::Message::CancelRequested {
            section: downloader::Section::Downloading,
            row_id,
        },
    ));

    assert!(app.state.downloader.downloading().is_empty());
    assert!(transaction.aborted());
}

#[test]
fn downloader_row_cancel_button_is_clickable_through_the_full_app_view() {
    let (mut app, _startup_task) = App::new_for_test();
    let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
        shell::Route::Downloader,
    )));
    let _task = app.update(RootMessage::Downloader(downloader::Message::EventReceived(
        downloader::DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(7),
        },
    )));
    assert_eq!(app.state.downloader.downloading().len(), 1);
    let row_id = app.state.downloader.downloading()[0].id();

    let mut ui = iced_test::Simulator::new(app.view());
    let press = iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left));
    let release = iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
        iced::mouse::Button::Left,
    ));
    for x in (0..1024).step_by(6) {
        for y in (0..400).step_by(6) {
            ui.point_at(Point::new(x as f32, y as f32));
            let _statuses = ui.simulate([press.clone(), release.clone()]);
        }
    }

    let cancelled = ui.into_messages().any(|message| {
        matches!(
            message,
            RootMessage::Downloader(downloader::Message::CancelRequested {
                section: downloader::Section::Downloading,
                row_id: seen,
            }) if seen == row_id
        )
    });
    assert!(
        cancelled,
        "no CancelRequested reached the root message stream from the full app view"
    );
}

#[test]
fn downloader_executor_open_paths_schedules_native_open_task() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![downloader::Effect::PathsOpenRequested(vec![PathBuf::from(
            "/tmp/downloader-open",
        )])],
        App::run_downloader_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn downloader_executor_open_workshop_schedules_url_task() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![downloader::Effect::WorkshopPageOpenRequested(Some(
            PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
        ))],
        App::run_downloader_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn downloader_executor_bulk_extract_picker_schedules_dialog_task() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![downloader::Effect::BulkExtractPickerRequested],
        App::run_downloader_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn local_extraction_transaction_is_correlated_and_cancellation_stops_it() {
    let (app, _startup_task) = App::new_for_test();
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Cancel Test")
            .entry("lua/autorun/cancel.lua", b"print('hi')\n".to_vec())
            .build(),
    )
    .expect("fixture archive should load");

    // Mirrors `run_local_gma_extraction`: begin a transaction and correlate
    // it with the task so cancellation can reach the extraction.
    let transaction = app.ctx.begin_transaction();
    let task = app
        .ctx
        .create_task(TaskKind::Extract, "extracting_progress");
    let task_id = task.id();
    app.ctx.correlate_backend_transaction(transaction.id, task);

    // Cancelling through the same path the UI's cancel button uses reaches
    // the transaction the extraction call below is about to use.
    assert!(app.ctx.cancel_task(task_id));
    assert!(transaction.aborted());

    let result = archive.extract_all_with_transaction(
        ExtractDestination::Temp,
        &PreviewExtractOptions::default(),
        &transaction,
        app.ctx.backend(),
    );

    assert!(matches!(result, Err(GmaError::Cancelled)));
    // A second cancellation attempt has nothing left to cancel.
    assert!(!app.ctx.cancel_task(task_id));
}

#[test]
fn downloader_executor_local_extraction_schedules_worker_task() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![downloader::Effect::LocalExtractionRequested(vec![
            PathBuf::from("/tmp/local-extract.gma"),
        ])],
        App::run_downloader_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn gma_file_drop_reuses_bulk_local_extraction_path() {
    let (app, _startup_task) = App::new_for_test();
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("selected.gma");
    std::fs::write(&path, b"GMAD").expect("gma fixture");

    let task = app.handle_file_drop(path);

    assert_eq!(task.units(), 1);
}

#[test]
fn downloader_executor_destination_selection_opens_overlay() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![downloader::Effect::DestinationSelectionRequested],
        App::run_downloader_effect,
    );

    assert!(app.state.modal_stack.overlay_active());
    assert_eq!(task.units(), 0);
}

#[test]
fn destination_select_executor_modal_open_opens_overlay() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![destination_select::Effect::ModalOpenRequested],
        App::run_destination_select_effect,
    );

    assert_eq!(task.units(), 0);
    assert!(app.state.modal_stack.overlay_active());
}

#[test]
fn destination_select_executor_folder_picker_schedules_dialog() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![destination_select::Effect::FolderPickerRequested],
        App::run_destination_select_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn destination_select_executor_create_folder_schedules_persistence() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![destination_select::Effect::CreateFolderChanged(true)],
        App::run_destination_select_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn destination_select_executor_persist_request_schedules_save() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![destination_select::Effect::DestinationPersistRequested(
            destination_select::DestinationPersistRequest {
                destination: ExtractDestination::Temp,
                create_folder: false,
                history_path: None,
            },
        )],
        App::run_destination_select_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn destination_select_executor_persisted_closes_overlay_and_runs_handoffs() {
    let (mut app, _startup_task) = App::new_for_test();
    let _task = app.batch_effects(
        vec![destination_select::Effect::ModalOpenRequested],
        App::run_destination_select_effect,
    );

    let task = app.batch_effects(
        vec![destination_select::Effect::DestinationPersisted],
        App::run_destination_select_effect,
    );

    assert_eq!(task.units(), 0);
    assert!(app.state.modal_stack.overlay_active());
    assert!(!app.state.modal_stack.overlay_interactive());
}

#[test]
fn destination_select_executor_dismissed_clears_context_menu_extract_paths() {
    let (mut app, _startup_task) = App::new_for_test();
    app.state.context_menu_extract_paths = Some(vec![PathBuf::from("/tmp/context.gma")]);

    let task = app.batch_effects(
        vec![destination_select::Effect::DestinationDismissed],
        App::run_destination_select_effect,
    );

    assert_eq!(task.units(), 0);
    assert_eq!(app.state.context_menu_extract_paths, None);
}

#[test]
fn downloader_executor_title_query_schedules_metadata_task() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![downloader::Effect::WorkshopTitleQueryRequested(vec![
            PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
        ])],
        App::run_downloader_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn downloader_executor_active_job_count_updates_shell_badge() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![downloader::Effect::ActiveJobCountChanged(3)],
        App::run_downloader_effect,
    );

    assert_eq!(task.units(), 0);
    assert_eq!(app.state.shell.downloader_jobs(), 3);
    assert_eq!(
        app.state
            .shell
            .downloader_badge(Instant::now())
            .expect("active count should show badge")
            .count,
        3
    );
}

#[test]
fn search_executor_palette_opened_refreshes_thumbnail_demands() {
    let (mut app, _startup_task) = App::new_for_test();
    seed_search_result(&mut app, false);

    let task = app.batch_effects(vec![search::Effect::PaletteOpened], App::run_search_effect);

    assert!(task.units() > 0);
}

#[test]
fn search_executor_palette_dismissed_refreshes_thumbnail_demands() {
    let (mut app, _startup_task) = App::new_for_test();
    seed_search_result(&mut app, false);

    let task = app.batch_effects(
        vec![search::Effect::PaletteDismissed],
        App::run_search_effect,
    );

    assert!(task.units() > 0);
}

#[test]
fn search_executor_focus_input_schedules_focus_operation() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![search::Effect::FocusInputRequested],
        App::run_search_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn search_executor_quick_debounce_schedules_debounce_task() {
    let (mut app, _startup_task) = App::new_for_test();
    let request = seed_search_request(&mut app);

    let task = app.batch_effects(
        vec![search::Effect::QuickSearchDebounceRequested(request)],
        App::run_search_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn search_executor_quick_search_schedules_worker_task() {
    let (mut app, _startup_task) = App::new_for_test();
    let request = seed_search_request(&mut app);

    let task = app.batch_effects(
        vec![search::Effect::QuickSearchRequested(request)],
        App::run_search_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn search_executor_full_search_starts_stream_task() {
    let (mut app, _startup_task) = App::new_for_test();
    seed_search_result(&mut app, true);

    let task = app.batch_effects(
        vec![search::Effect::FullSearchRequested],
        App::run_search_effect,
    );

    assert_eq!(task.units(), 1);
    assert!(app.state.search.loading());
    assert!(!app.state.search.should_begin_full_search());
}

#[test]
fn search_executor_cancels_requested_task() {
    let (mut app, _startup_task) = App::new_for_test();
    let handle = app.ctx.create_task(TaskKind::Search, "search");
    let task_id = handle.id();
    let transaction = app.ctx.begin_transaction();
    app.ctx
        .correlate_backend_transaction(transaction.id, handle);

    let task = app.batch_effects(
        vec![search::Effect::TaskCancellationRequested(task_id)],
        App::run_search_effect,
    );

    assert_eq!(task.units(), 0);
    assert!(transaction.aborted());
}

#[test]
fn search_executor_result_activation_schedules_selection_task() {
    let (mut app, _startup_task) = App::new_for_test();
    seed_search_result(&mut app, false);

    let task = app.batch_effects(
        vec![search::Effect::ResultActivated(0)],
        App::run_search_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn search_executor_thumbnail_demands_changed_refreshes_thumbnail_demands() {
    let (mut app, _startup_task) = App::new_for_test();
    seed_search_result(&mut app, false);

    let task = app.batch_effects(
        vec![search::Effect::ThumbnailDemandsChanged],
        App::run_search_effect,
    );

    assert!(task.units() > 0);
}

#[test]
fn search_executor_metadata_refresh_defers_until_steam_connects() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.batch_effects(
        vec![search::Effect::MetadataRefreshRequested {
            generation: 7,
            item_ids: vec![PublishedFileId::new(123).expect("test fixture ids are always nonzero")],
        }],
        App::run_search_effect,
    );

    assert_eq!(
        app.state.steam_session.pending_retry(),
        Some(&steam_session::PendingRetry::SearchMetadataRefresh {
            generation: 7,
            item_ids: vec![PublishedFileId::new(123).expect("test fixture ids are always nonzero")],
        })
    );
}

#[test]
fn my_workshop_executor_page_request_defers_until_steam_connects() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.batch_effects(
        vec![my_workshop::Effect::PageRequested {
            generation: 7,
            page: 2,
        }],
        App::run_my_workshop_effect,
    );

    assert_eq!(
        app.state.steam_session.pending_retry(),
        Some(&steam_session::PendingRetry::MyWorkshopPage {
            generation: 7,
            page: 2,
        })
    );
}

#[test]
fn my_workshop_executor_stats_refresh_defers_until_steam_connects() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.batch_effects(
        vec![my_workshop::Effect::StatsRefreshRequested {
            generation: 7,
            pages: 2,
        }],
        App::run_my_workshop_effect,
    );

    assert_eq!(
        app.state.steam_session.pending_retry(),
        Some(&steam_session::PendingRetry::MyWorkshopStats {
            generation: 7,
            pages: 2,
        })
    );
}

#[test]
fn my_workshop_executor_prepare_publish_schedules_open() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![my_workshop::Effect::PreparePublishRequested(
            my_workshop::PreparePublishTarget::New,
        )],
        App::run_my_workshop_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn my_workshop_executor_context_menu_sets_target_and_schedules_open() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![my_workshop::Effect::ContextMenuRequested(
            my_workshop_context_menu_request(),
        )],
        App::run_my_workshop_effect,
    );

    assert_eq!(task.units(), 1);
    assert!(matches!(
        app.state.context_menu_target,
        Some(ContextMenuTarget::MyWorkshop { workshop_id, .. })
            if workshop_id == PublishedFileId::new(123).expect("test fixture ids are always nonzero")
    ));
}

#[test]
fn my_workshop_executor_thumbnail_demands_runs_owner_sync() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![my_workshop::Effect::ThumbnailDemandsChanged],
        App::run_my_workshop_effect,
    );

    assert_eq!(task.units(), 0);
}

#[test]
fn my_workshop_executor_drag_press_updates_drag_state() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![my_workshop::Effect::AddonDragPressed {
            card_id: "123".to_owned(),
            workshop_id: Some(
                PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            ),
        }],
        App::run_my_workshop_effect,
    );

    assert_eq!(task.units(), 0);
    assert!(app.state.addon_drag.is_active());
}

#[test]
fn my_workshop_executor_drag_release_finishes_drag() {
    let (mut app, _startup_task) = App::new_for_test();
    app.state.addon_drag.press(
        AddonDragSource::MyWorkshop,
        "123".to_owned(),
        Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
        None,
    );

    let task = app.batch_effects(
        vec![my_workshop::Effect::AddonDragReleased],
        App::run_my_workshop_effect,
    );

    assert_eq!(task.units(), 1);
    assert!(!app.state.addon_drag.is_active());
}

#[test]
fn installed_addons_executor_metadata_request_defers_until_steam_connects() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.batch_effects(
        vec![installed_addons::Effect::MetadataRequested {
            generation: 7,
            item_ids: vec![PublishedFileId::new(123).expect("test fixture ids are always nonzero")],
        }],
        App::run_installed_addons_effect,
    );

    assert_eq!(
        app.state.steam_session.pending_retry(),
        Some(&steam_session::PendingRetry::InstalledMetadata {
            generation: 7,
            item_ids: vec![PublishedFileId::new(123).expect("test fixture ids are always nonzero")],
        })
    );
}

#[test]
fn installed_addons_executor_metadata_refresh_defers_until_steam_connects() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.batch_effects(
        vec![installed_addons::Effect::MetadataRefreshRequested {
            generation: 7,
            item_ids: vec![PublishedFileId::new(123).expect("test fixture ids are always nonzero")],
        }],
        App::run_installed_addons_effect,
    );

    assert_eq!(
        app.state.steam_session.pending_retry(),
        Some(&steam_session::PendingRetry::InstalledMetadataRefresh {
            generation: 7,
            item_ids: vec![PublishedFileId::new(123).expect("test fixture ids are always nonzero")],
        })
    );
}

#[test]
fn installed_addons_executor_preview_schedules_preview_open() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![installed_addons::Effect::PreviewRequested(
            installed_preview_target(),
        )],
        App::run_installed_addons_effect,
    );

    // Modal + local archive + Workshop details now start together.
    assert_eq!(task.units(), 3);
    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
}

#[test]
fn installed_addons_executor_context_menu_sets_target_and_schedules_open() {
    let (mut app, _startup_task) = App::new_for_test();
    let request = installed_context_menu_request();

    let task = app.batch_effects(
        vec![installed_addons::Effect::ContextMenuRequested(request)],
        App::run_installed_addons_effect,
    );

    assert_eq!(task.units(), 1);
    assert!(matches!(
        app.state.context_menu_target,
        Some(ContextMenuTarget::Local(LocalMenuTarget { workshop_id, .. }))
            if workshop_id == Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
    ));
}

#[test]
fn installed_addons_executor_thumbnail_demands_runs_owner_sync() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![installed_addons::Effect::ThumbnailDemandsChanged],
        App::run_installed_addons_effect,
    );

    assert_eq!(task.units(), 0);
}

#[test]
fn installed_addons_executor_drag_press_updates_drag_state() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![installed_addons::Effect::AddonDragPressed {
            card_id: "/tmp/drag.gma".to_owned(),
            workshop_id: Some(
                PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            ),
        }],
        App::run_installed_addons_effect,
    );

    assert_eq!(task.units(), 0);
    assert!(app.state.addon_drag.is_active());
}

#[test]
fn installed_addons_executor_drag_release_finishes_drag() {
    let (mut app, _startup_task) = App::new_for_test();
    app.state.addon_drag.press(
        AddonDragSource::InstalledAddons,
        "/tmp/drag.gma".to_owned(),
        Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
        None,
    );

    let task = app.batch_effects(
        vec![installed_addons::Effect::AddonDragReleased],
        App::run_installed_addons_effect,
    );

    assert_eq!(task.units(), 1);
    assert!(!app.state.addon_drag.is_active());
}

#[test]
fn size_analyzer_executor_preview_url_resolve_schedules_worker() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![size_analyzer::Effect::PreviewUrlsResolveRequested(vec![
            PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
        ])],
        App::run_size_analyzer_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn size_analyzer_executor_preview_schedules_preview_open() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![size_analyzer::Effect::PreviewRequested(
            size_analyzer::PreviewTarget {
                path: PathBuf::from("/tmp/size-preview.gma"),
                title: "Size Preview".to_owned(),
                workshop_id: Some(
                    PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
                ),
            },
        )],
        App::run_size_analyzer_effect,
    );

    // Local archive and Workshop details start together from this route.
    assert_eq!(task.units(), 2);
}

#[test]
fn size_analyzer_executor_context_menu_sets_target_and_schedules_open() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![size_analyzer_context_menu_effect()],
        App::run_size_analyzer_effect,
    );

    assert_eq!(task.units(), 1);
    assert!(matches!(
        app.state.context_menu_target,
        Some(ContextMenuTarget::Local(LocalMenuTarget { workshop_id, .. }))
            if workshop_id == Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero"))
    ));
}

#[test]
fn size_analyzer_executor_thumbnail_demands_runs_owner_sync() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![size_analyzer::Effect::ThumbnailDemandsChanged],
        App::run_size_analyzer_effect,
    );

    assert_eq!(task.units(), 0);
}

#[test]
fn size_analyzer_executor_drag_press_updates_drag_state() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![size_analyzer::Effect::AddonDragPressed {
            card_id: "/tmp/size-drag.gma".to_owned(),
            workshop_id: Some(
                PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            ),
        }],
        App::run_size_analyzer_effect,
    );

    assert_eq!(task.units(), 0);
    assert!(app.state.addon_drag.is_active());
}

#[test]
fn size_analyzer_executor_drag_release_finishes_drag() {
    let (mut app, _startup_task) = App::new_for_test();
    app.state.addon_drag.press(
        AddonDragSource::SizeAnalyzer,
        "/tmp/size-drag.gma".to_owned(),
        Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
        None,
    );

    let task = app.batch_effects(
        vec![size_analyzer::Effect::AddonDragReleased],
        App::run_size_analyzer_effect,
    );

    assert_eq!(task.units(), 1);
    assert!(!app.state.addon_drag.is_active());
}

#[test]
fn preview_gma_executor_modal_open_claims_modal_slot() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::ModalOpenRequested],
        App::run_preview_gma_effect,
    );

    assert_eq!(task.units(), 0);
    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
}

#[test]
fn preview_gma_executor_archive_open_schedules_worker() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::ArchiveOpenRequested(
            preview_open_request(),
        )],
        App::run_preview_gma_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn preview_gma_executor_metadata_request_schedules_worker() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::WorkshopMetadataRequested(
            preview_metadata_request(),
        )],
        App::run_preview_gma_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn preview_gma_executor_author_request_schedules_worker() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::AuthorFetchRequested(
            preview_author_request(),
        )],
        App::run_preview_gma_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn preview_gma_executor_destination_select_opens_overlay() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::DestinationSelectRequested],
        App::run_preview_gma_effect,
    );

    assert_eq!(task.units(), 0);
    assert!(app.state.modal_stack.overlay_active());
}

#[cfg(not(feature = "asset-studio"))]
#[test]
fn preview_gma_executor_entry_extraction_schedules_worker() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::EntryExtractionRequested(
            preview_extraction_request(),
        )],
        App::run_preview_gma_effect,
    );

    assert_eq!(task.units(), 1);
}

#[cfg(feature = "asset-studio")]
#[test]
fn preview_gma_executor_entry_preview_opens_embedded_file_preview() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::EntryPreviewRequested(
            file_preview_request(),
        )],
        App::run_preview_gma_effect,
    );

    assert_eq!(task.units(), 1);
    assert_eq!(app.state.modal_stack.overlay_modal(), None);
    assert!(app.state.file_preview.loading());
}

#[cfg(feature = "asset-studio")]
#[test]
fn prepare_publish_executor_entry_preview_opens_embedded_file_preview() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![prepare_publish::Effect::EntryPreviewRequested(
            file_preview_request(),
        )],
        App::run_prepare_publish_effect,
    );

    assert_eq!(task.units(), 1);
    assert!(app.state.file_preview.loading());
}

#[cfg(feature = "asset-studio")]
#[test]
fn preview_gma_close_request_chains_expanded_preview_back_then_modal() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::PreviewGma(
        preview_gma::Message::OpenRequested(preview_gma::OpenTarget::new(
            PathBuf::from("/tmp/test.gma"),
            "Test",
            Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
        )),
    ));
    let _task = app.update(RootMessage::FilePreview(
        file_preview::Message::OpenRequested(file_preview_request()),
    ));
    let _task = app.update(RootMessage::FilePreview(
        file_preview::Message::ExpandToggled,
    ));

    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
    assert!(app.state.preview_gma.is_open());
    assert!(app.state.file_preview.is_open());
    assert!(app.state.file_preview.expanded());

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));

    assert!(app.state.file_preview.is_open());
    assert!(!app.state.file_preview.expanded());
    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));

    assert!(!app.state.file_preview.is_open());
    assert!(app.state.preview_gma.is_open());
    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));
    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
    assert!(!app.state.modal_stack.interactive());

    let _task = app.update(RootMessage::AnimationTick(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    ));

    assert_eq!(app.state.modal_stack.active(), None);
    assert!(!app.state.preview_gma.is_open());
}

#[cfg(feature = "asset-studio")]
#[test]
fn preview_gma_close_request_back_stops_embedded_audio_preview() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::PreviewGma(
        preview_gma::Message::OpenRequested(preview_gma::OpenTarget::new(
            PathBuf::from("/tmp/test.gma"),
            "Test",
            Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
        )),
    ));
    let _task = app.update(RootMessage::FilePreview(
        file_preview::Message::OpenRequested(file_preview_request()),
    ));
    let request = app
        .state
        .file_preview
        .request()
        .expect("preview request should be active")
        .clone();
    let data = file_preview::PreviewData::from_request(
        &request,
        file_preview::PreviewContent::Audio {
            bytes: Arc::new(vec![1, 2, 3]),
            duration_secs: Some(4.0),
        },
    );
    let _task = app.update(RootMessage::FilePreview(file_preview::Message::Loaded(
        request.request_id,
        Ok(data),
    )));
    let _task = app.update(RootMessage::FilePreview(
        file_preview::Message::AudioPlaybackStarted,
    ));

    assert!(app.state.file_preview.is_open());
    assert!(app.state.file_preview.audio_playing());

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));

    assert!(!app.state.file_preview.is_open());
    assert!(!app.state.file_preview.audio_playing());
    assert!(app.state.preview_gma.is_open());
    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
}

#[test]
fn preview_gma_executor_open_url_schedules_native_open() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::OpenUrlRequested(
            "https://example.invalid/preview".to_owned(),
        )],
        App::run_preview_gma_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn preview_gma_executor_copy_text_schedules_clipboard_write() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::CopyTextRequested("path".to_owned())],
        App::run_preview_gma_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn preview_gma_executor_reveal_path_schedules_native_open() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::RevealPathRequested(PathBuf::from(
            "/tmp/preview.gma",
        ))],
        App::run_preview_gma_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn preview_gma_executor_browser_path_changed_schedules_autoscroll() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::BrowserPathChanged],
        App::run_preview_gma_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn preview_gma_executor_thumbnail_demands_runs_owner_sync() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::ThumbnailDemandsChanged],
        App::run_preview_gma_effect,
    );

    assert_eq!(task.units(), 0);
}

#[test]
fn settings_executor_modal_open_claims_modal_slot() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![settings::Effect::ModalOpenRequested],
        App::run_settings_effect,
    );

    assert_eq!(task.units(), 0);
    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::Settings)
    );
}

#[test]
fn settings_executor_modal_close_starts_close_animation() {
    let (mut app, _startup_task) = App::new_for_test();
    let _task = app.batch_effects(
        vec![settings::Effect::ModalOpenRequested],
        App::run_settings_effect,
    );

    let task = app.batch_effects(
        vec![settings::Effect::ModalCloseRequested],
        App::run_settings_effect,
    );

    assert_eq!(task.units(), 0);
    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::Settings)
    );
    assert!(!app.state.modal_stack.interactive());
}

#[test]
fn settings_executor_path_browse_schedules_dialog() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![settings::Effect::PathBrowseRequested(
            settings::PathSetting::Temp,
        )],
        App::run_settings_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn settings_executor_path_validation_schedules_worker() {
    let (mut app, _startup_task) = App::new_for_test();
    let request = app
        .state
        .settings
        .path_validation_request(settings::PathSetting::Temp);

    let task = app.batch_effects(
        vec![settings::Effect::PathValidationRequested(request)],
        App::run_settings_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn settings_executor_mutation_applies_runtime_and_schedules_save() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![settings::Effect::MutationApplied(
            settings::SettingsMutation::PlayGifsByDefault(false),
        )],
        App::run_settings_effect,
    );

    assert_eq!(task.units(), 1);
}

#[test]
fn settings_executor_reset_run_schedules_worker() {
    let (mut app, _startup_task) = App::new_for_test();

    let task = app.batch_effects(
        vec![settings::Effect::ResetRunRequested(
            settings::ResetAction::Settings,
        )],
        App::run_settings_effect,
    );

    assert_eq!(task.units(), 1);
}

#[cfg(feature = "asset-studio")]
#[test]
fn global_shortcut_mapper_matches_command_k() {
    assert!(matches!(
        map_global_shortcut(
            &keyboard::Key::Character("k".into()),
            keyboard::Modifiers::COMMAND
        ),
        Some(RootMessage::GlobalShortcut(
            GlobalShortcut::ToggleFileSearch
        ))
    ));
}

#[test]
fn global_shortcut_mapper_matches_command_f_comma_and_route_numbers_only() {
    assert!(matches!(
        map_global_shortcut(
            &keyboard::Key::Character("f".into()),
            keyboard::Modifiers::COMMAND
        ),
        Some(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSearch))
    ));
    assert!(matches!(
        map_global_shortcut(
            &keyboard::Key::Character(",".into()),
            keyboard::Modifiers::COMMAND
        ),
        Some(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSettings))
    ));
    assert!(
        map_global_shortcut(
            &keyboard::Key::Character("s".into()),
            keyboard::Modifiers::COMMAND
        )
        .is_none()
    );
    assert!(matches!(
        map_global_shortcut(
            &keyboard::Key::Character("1".into()),
            keyboard::Modifiers::COMMAND
        ),
        Some(RootMessage::GlobalShortcut(GlobalShortcut::NavigateRoute(
            shell::Route::MyWorkshop
        )))
    ));
    assert!(matches!(
        map_global_shortcut(
            &keyboard::Key::Character("4".into()),
            keyboard::Modifiers::COMMAND
        ),
        Some(RootMessage::GlobalShortcut(GlobalShortcut::NavigateRoute(
            shell::Route::SizeAnalyzer
        )))
    ));
    assert!(
        map_global_shortcut(
            &keyboard::Key::Character("f".into()),
            keyboard::Modifiers::NONE
        )
        .is_none()
    );
    assert!(
        map_global_shortcut(
            &keyboard::Key::Character("s".into()),
            keyboard::Modifiers::NONE
        )
        .is_none()
    );
    assert!(
        map_global_shortcut(
            &keyboard::Key::Character("1".into()),
            keyboard::Modifiers::NONE
        )
        .is_none()
    );
    assert!(
        map_global_shortcut(
            &keyboard::Key::Character("f".into()),
            keyboard::Modifiers::COMMAND.union(keyboard::Modifiers::SHIFT)
        )
        .is_none()
    );
    assert!(
        map_global_shortcut(
            &keyboard::Key::Character("1".into()),
            keyboard::Modifiers::COMMAND.union(keyboard::Modifiers::SHIFT)
        )
        .is_none()
    );
}

#[test]
fn shell_executor_open_search_palette_focuses_search_and_dismisses_account_menu() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::Shell(shell::Message::AccountMenuToggled));
    assert!(app.state.shell.account_menu_open());

    let _task = app.batch_effects(
        vec![shell::Effect::OpenSearchPalette],
        App::run_shell_effect,
    );

    assert!(app.state.search.palette_open());
    assert!(!app.state.shell.account_menu_open());
}

#[test]
fn command_f_toggles_palette_when_global_shortcuts_are_enabled() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSearch));
    assert!(app.state.search.palette_open());

    let _task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSearch));
    assert!(!app.state.search.palette_open());
}

#[test]
fn global_shortcuts_are_guarded_while_context_or_modal_ui_is_active() {
    let (mut app, _startup_task) = App::new_for_test();
    let _task = app.update(RootMessage::ContextMenu(
        context_menu::Message::OpenRequested(context_menu::OpenRequest::new(
            context_menu::Owner::InstalledAddons,
            Point::new(10.0, 10.0),
            vec![context_menu::Entry::copy_path()],
        )),
    ));

    let _task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSearch));

    assert!(!app.state.search.palette_open());

    let (mut app, _startup_task) = App::new_for_test();
    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::OpenDestinationSelect,
    ));

    let _task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSearch));

    assert!(!app.state.search.palette_open());
}

#[test]
fn command_comma_dismisses_account_menu_and_defers_to_settings_open_task() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::Shell(shell::Message::AccountMenuToggled));
    assert!(app.state.shell.account_menu_open());

    let _task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSettings));

    assert!(!app.state.shell.account_menu_open());
    assert_eq!(app.state.modal_stack.active(), None);
}

#[test]
fn command_comma_closes_settings_when_it_is_the_active_modal() {
    let (mut app, _startup_task) = App::new_for_test();
    let snapshot = app.settings_snapshot();
    let _task = app.update(RootMessage::Settings(settings::Message::OpenRequested(
        snapshot,
    )));
    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::Settings)
    );

    // The toggle dispatches the same CloseRequested that Escape and the
    // scrim click dismissal paths emit.
    let task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSettings));
    assert_eq!(task.units(), 1);
    let _task = app.update(RootMessage::Settings(settings::Message::CloseRequested));

    // Close-then-clear: the layer settles on the next animation tick.
    let _task = app.update(RootMessage::AnimationTick(
        Instant::now() + Duration::from_secs(1),
    ));

    assert_eq!(app.state.modal_stack.active(), None);
    assert!(!app.state.settings.open());
}

#[test]
fn settings_toggle_mapper_maps_only_command_comma() {
    let command = keyboard::Modifiers::COMMAND;

    assert!(matches!(
        map_settings_toggle_shortcut(&keyboard::Key::Character(",".into()), command),
        Some(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSettings))
    ));
    // Everything else passes through to the modal's widgets.
    assert!(map_settings_toggle_shortcut(&keyboard::Key::Character("f".into()), command).is_none());
    assert!(map_settings_toggle_shortcut(&keyboard::Key::Character("1".into()), command).is_none());
    assert!(
        map_settings_toggle_shortcut(
            &keyboard::Key::Character(",".into()),
            keyboard::Modifiers::empty()
        )
        .is_none()
    );
}

#[test]
fn command_comma_stays_inert_over_other_modals() {
    let (mut app, _startup_task) = App::new_for_test();
    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::OpenPreviewGma,
    ));
    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );

    let task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSettings));

    assert_eq!(task.units(), 0);
    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
}

#[test]
fn search_result_activation_closes_palette() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSearch));
    assert!(app.state.search.palette_open());

    let _task = app.update(RootMessage::Search(search::Message::ResultActivated(123)));

    assert!(!app.state.search.palette_open());
}

#[test]
fn modal_stack_close_clears_settings_state() {
    let (mut app, _startup_task) = App::new_for_test();
    let snapshot = app.settings_snapshot();
    let _task = app.update(RootMessage::Settings(settings::Message::OpenRequested(
        snapshot,
    )));

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));

    assert_eq!(
        app.state.modal_stack.active(),
        Some(modal_stack::ActiveModal::Settings)
    );
    assert!(app.state.settings.open());

    let _task = app.update(RootMessage::AnimationTick(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    ));

    assert_eq!(app.state.modal_stack.active(), None);
    assert!(!app.state.settings.open());
}

#[test]
fn context_menu_extract_opens_destination_select_with_pending_path() {
    let (mut app, _startup_task) = App::new_for_test();
    let path = PathBuf::from("/tmp/context-menu.gma");
    app.state.context_menu_target = Some(ContextMenuTarget::Local(LocalMenuTarget {
        path: path.clone(),
        path_text: path.display().to_string(),
        workshop_id: None,
        workshop_url: None,
        preview_url: None,
    }));

    let _task = app.update(RootMessage::ContextMenu(
        context_menu::Message::ActionSelected(context_menu::ContextMenuAction::Extract),
    ));

    assert!(app.state.modal_stack.overlay_active());
    assert_eq!(app.state.modal_stack.active(), None);
    assert_eq!(app.state.context_menu_extract_paths, Some(vec![path]));
    assert_eq!(app.state.context_menu_target, None);
}

#[test]
fn context_menu_dismiss_clears_active_target() {
    let (mut app, _startup_task) = App::new_for_test();
    app.state.context_menu_target = Some(ContextMenuTarget::MyWorkshop {
        workshop_id: PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        workshop_url: workshop_url::workshop_item_url(
            PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
        ),
        preview_url: None,
    });

    let _task = app.update(RootMessage::ContextMenu(
        context_menu::Message::DismissRequested,
    ));

    assert_eq!(app.state.context_menu_target, None);
}

#[cfg(feature = "debug")]
#[test]
fn hide_addon_context_action_is_session_scoped_and_removes_my_workshop_row() {
    let (mut app, _startup_task) = App::new_for_test();
    let workshop_id = PublishedFileId::new(123).expect("test fixture ids are always nonzero");
    app.state
        .my_workshop
        .push_rows_for_test(vec![my_workshop::Row::for_test(123, "Hidden", 10)], 1);
    app.state.context_menu_target = Some(ContextMenuTarget::MyWorkshop {
        workshop_id,
        workshop_url: workshop_url::workshop_item_url(workshop_id),
        preview_url: None,
    });

    let _task = app.route_context_menu_action(context_menu::ContextMenuAction::HideAddon);

    assert!(app.state.hidden_workshop_ids.contains(&workshop_id));
    assert!(app.state.my_workshop.row_for_test(123).is_none());
}

#[test]
fn update_delegates_search_messages_to_search_state() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::Search(search::Message::QueryEdited(
        "addons".to_owned(),
    )));

    assert_eq!(app.state.search.input(), "addons");
}

#[test]
fn addon_drag_state_preserves_click_when_released_before_drag_threshold() {
    let mut drag = AddonDragState::default();

    drag.press(
        AddonDragSource::MyWorkshop,
        "42".to_owned(),
        Some(PublishedFileId::new(42).expect("test fixture ids are always nonzero")),
        None,
    );
    drag.cursor_moved(Point::new(10.0, 10.0));

    assert_eq!(
        drag.release(false),
        Some(AddonDragOutcome::Click {
            source: AddonDragSource::MyWorkshop,
            card_id: "42".to_owned(),
        })
    );
    assert!(!drag.is_active());
}

#[test]
fn addon_drag_state_promotes_after_threshold_and_drops_on_target() {
    let mut drag = AddonDragState::default();

    drag.press(
        AddonDragSource::InstalledAddons,
        "/tmp/a.gma".to_owned(),
        Some(PublishedFileId::new(99).expect("test fixture ids are always nonzero")),
        None,
    );
    drag.cursor_moved(Point::new(0.0, 0.0));
    drag.cursor_moved(Point::new(5.0, 0.0));
    assert!(drag.is_active());
    assert!(!drag.is_dragging());

    drag.cursor_moved(Point::new(6.0, 0.0));
    assert!(drag.is_dragging());
    assert_eq!(
        drag.release(true),
        Some(AddonDragOutcome::Drop {
            workshop_id: PublishedFileId::new(99).expect("test fixture ids are always nonzero")
        })
    );
    assert!(!drag.is_active());
}

#[test]
fn addon_drag_state_keeps_captured_thumbnail_while_dragging() {
    let mut drag = AddonDragState::default();
    let thumbnail = image::Handle::from_rgba(1, 1, vec![10, 20, 30, 255]);

    drag.press(
        AddonDragSource::MyWorkshop,
        "42".to_owned(),
        Some(PublishedFileId::new(42).expect("test fixture ids are always nonzero")),
        Some(thumbnail.clone()),
    );
    drag.cursor_moved(Point::ORIGIN);
    drag.cursor_moved(Point::new(10.0, 0.0));

    assert_eq!(drag.thumbnail(), Some(&thumbnail));
}

#[test]
fn addon_drag_state_cancels_active_drag_released_outside_target() {
    let mut drag = AddonDragState::default();

    drag.press(
        AddonDragSource::MyWorkshop,
        "42".to_owned(),
        Some(PublishedFileId::new(42).expect("test fixture ids are always nonzero")),
        None,
    );
    drag.cursor_moved(Point::new(1.0, 1.0));
    drag.cursor_moved(Point::new(20.0, 1.0));

    assert_eq!(drag.release(false), Some(AddonDragOutcome::Cancelled));
    assert!(!drag.is_active());
}

#[test]
fn addon_drag_state_preserves_size_analyzer_local_cell_click() {
    let mut drag = AddonDragState::default();

    drag.press(
        AddonDragSource::SizeAnalyzer,
        "/tmp/local.gma".to_owned(),
        None,
        None,
    );
    drag.cursor_moved(Point::new(1.0, 1.0));
    drag.cursor_moved(Point::new(20.0, 1.0));

    assert!(!drag.is_dragging());
    assert_eq!(
        drag.release(false),
        Some(AddonDragOutcome::Click {
            source: AddonDragSource::SizeAnalyzer,
            card_id: "/tmp/local.gma".to_owned(),
        })
    );
    assert!(!drag.is_active());
}

#[test]
fn addon_drag_update_events_promote_and_finish_drag() {
    let (mut app, _startup_task) = App::new_for_test();
    app.state.addon_drag.press(
        AddonDragSource::MyWorkshop,
        "42".to_owned(),
        Some(PublishedFileId::new(42).expect("test fixture ids are always nonzero")),
        None,
    );

    let _task = app.update(RootMessage::AddonDrag(AddonDragMessage::CursorMoved(
        Point::new(0.0, 0.0),
    )));
    let _task = app.update(RootMessage::AddonDrag(AddonDragMessage::CursorMoved(
        Point::new(10.0, 0.0),
    )));

    assert!(app.state.addon_drag.is_dragging());

    let _task = app.update(RootMessage::AddonDrag(AddonDragMessage::Released));

    assert!(!app.state.addon_drag.is_active());
}

#[test]
fn update_delegates_steam_session_messages_to_session_state() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connecting),
    ));

    assert_eq!(
        app.state.steam_session.status(),
        steam_session::ConnectionStatus::Connecting
    );
}

#[test]
fn backend_steam_events_update_session_and_shell_state() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::BackendEvent(
        BackendRuntimeEvent::SteamConnected,
    ));

    assert_eq!(
        app.state.steam_session.status(),
        steam_session::ConnectionStatus::Connected
    );
    assert!(app.state.shell.steam_status().connected());

    let _task = app.update(RootMessage::BackendEvent(
        BackendRuntimeEvent::SteamDisconnected,
    ));

    assert_eq!(
        app.state.steam_session.status(),
        steam_session::ConnectionStatus::Disconnected
    );
    assert!(!app.state.shell.steam_status().connected());
}

fn backend_appdata_snapshot_for_test(
    settings: BackendSettings,
    root: &Path,
) -> BackendAppDataSnapshot {
    let temp_dir = root.join("temp");
    let user_data_dir = root.join("user-data");
    let downloads_dir = root.join("downloads");
    std::fs::create_dir_all(&temp_dir).expect("temp dir");
    std::fs::create_dir_all(&user_data_dir).expect("user data dir");
    std::fs::create_dir_all(&downloads_dir).expect("downloads dir");

    BackendAppDataSnapshot {
        settings,
        version: "test",
        open_count: 0,
        paths: BackendAppDataPathsSnapshot {
            settings_file: root.join("settings.json"),
            default_user_data_dir: root.join("default-user-data"),
            default_temp_dir: root.join("default-temp"),
            default_downloads_dir: Some(root.join("default-downloads")),
            temp_dir,
            user_data_dir,
            downloads_dir: Some(downloads_dir),
            gmod_dir: None,
        },
    }
}

#[test]
fn backend_appdata_event_refreshes_settings_and_paths() {
    let (mut app, _startup_task) = App::new_for_test();
    let initial_steam_status = app.state.steam_session.status();
    let root = tempfile::tempdir().expect("tempdir");
    app.ctx
        .update_settings_snapshot_for_test(|settings| {
            settings.play_gifs_by_default = false;
            settings.download_count_format = DownloadCountFormat::Period;
            settings.theme_preset = ThemePreset::ClassicSource;
        })
        .expect("ui-only settings update");
    let settings = BackendSettings {
        sounds: false,
        language: Some("en-US".to_owned()),
        ..BackendSettings::default()
    };
    let snapshot = backend_appdata_snapshot_for_test(settings, root.path());

    let _task = app.update(RootMessage::BackendEvent(
        BackendRuntimeEvent::AppDataUpdated(Box::new(snapshot)),
    ));
    let (settings, paths) = app.ctx.settings_and_paths_snapshot();

    assert!(!settings.sounds);
    assert_eq!(settings.language.as_deref(), Some("en-US"));
    assert_eq!(paths.temp_dir, root.path().join("temp"));
    assert!(!settings.play_gifs_by_default);
    assert_eq!(settings.download_count_format, DownloadCountFormat::Period);
    assert_eq!(settings.theme_preset, ThemePreset::ClassicSource);
    assert_eq!(app.state.steam_session.status(), initial_steam_status);
}

#[test]
fn download_count_format_mutation_updates_runtime_formatter() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.apply_settings_mutation_runtime(
        &settings::SettingsMutation::DownloadCountFormat(DownloadCountFormat::Period),
    );

    assert_eq!(
        app.state.download_count_formatter().format_count(12_345),
        "12.345"
    );
}

#[test]
fn library_refreshed_fans_out_to_installed_addons_search_and_size_analyzer() {
    let (mut app, _startup_task) = App::new_for_test();
    app.ctx.backend().search.clear();
    let snapshot = LibrarySnapshot {
        addons: Arc::from(
            vec![installed_addon_for_library(
                "/tmp/root-fanout.gma",
                "Root Fanout Addon",
                &["root-needle"],
                Some(77),
            )]
            .into_boxed_slice(),
        ),
        epoch: 1,
    };
    let refresh = LibraryRefresh {
        reason: LibraryRefreshReason::DiskChanged,
        snapshot: Some(snapshot),
        rerun_after: None,
    };

    let _task = app.update(RootMessage::LibraryRefreshed(
        LibraryRefreshReason::DiskChanged,
        Ok(refresh),
    ));

    assert_eq!(app.state.installed_addons.total_count(), 1);
    let result = app
        .ctx
        .backend()
        .search
        .quick_search("root-needle".to_owned());
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item.label(), "Root Fanout Addon");

    let _task = app.update(RootMessage::SizeAnalyzer(
        size_analyzer::Message::ViewportResized(Size::new(640.0, 360.0)),
    ));
    let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
        shell::Route::SizeAnalyzer,
    )));
    assert!(app.state.size_analyzer.projection_key_for_test().is_some());
}

#[test]
fn backend_runtime_actions_translate_to_downloader_events() {
    let start = backend_runtime_action_message(BackendRuntimeAction::WorkshopDownloadTaskStarted {
        kind: WorkshopDownloadTaskKind::Download,
        item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
        task_id: TaskId::from_raw(7),
    });
    assert!(matches!(
        start,
        RootMessage::Downloader(downloader::Message::EventReceived(
            downloader::DownloaderEvent::TaskStarted {
                kind: WorkshopDownloadTaskKind::Download,
                item_id,
                task_id
            }
        )) if item_id == PublishedFileId::new(123).expect("test fixture ids are always nonzero") && task_id == TaskId::from_raw(7)
    ));

    let finished = backend_runtime_action_message(BackendRuntimeAction::WorkshopDownloadFinished {
        item_id: PublishedFileId::new(456).expect("test fixture ids are always nonzero"),
        installed_path: None,
        extracted_path: PathBuf::from("/tmp/extracted/456"),
    });
    assert!(matches!(
        finished,
        RootMessage::Downloader(downloader::Message::EventReceived(
            downloader::DownloaderEvent::WorkshopDownloadFinished(result)
        )) if result.item_id == PublishedFileId::new(456).expect("test fixture ids are always nonzero")
            && result.outcome.as_ref().is_ok_and(|success| {
                success.extracted_path == std::path::Path::new("/tmp/extracted/456")
                    && success.installed_path.is_none()
            })
    ));
}

#[test]
fn uncorrelated_backend_transaction_events_are_data_only() {
    let (mut app, _startup_task) = App::new_for_test();
    let initial_steam_status = app.state.steam_session.status();

    let _task = app.update(RootMessage::BackendEvent(BackendRuntimeEvent::Transaction(
        crate::backend::tasks::TransactionRuntimeEvent::Progress {
            id: 42,
            progress: 5000,
        },
    )));

    assert_eq!(app.state.steam_session.status(), initial_steam_status);
}

#[test]
fn steam_backed_page_request_defers_until_connection() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.batch_effects(
        vec![my_workshop::Effect::PageRequested {
            generation: 7,
            page: 2,
        }],
        App::run_my_workshop_effect,
    );

    assert_eq!(
        app.state.steam_session.pending_retry(),
        Some(&steam_session::PendingRetry::MyWorkshopPage {
            generation: 7,
            page: 2,
        })
    );
    assert_eq!(
        app.state.steam_session.status(),
        steam_session::ConnectionStatus::Connecting
    );
}

#[test]
fn steam_backed_stats_refresh_defers_until_connection() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.batch_effects(
        vec![my_workshop::Effect::StatsRefreshRequested {
            generation: 7,
            pages: 2,
        }],
        App::run_my_workshop_effect,
    );

    assert_eq!(
        app.state.steam_session.pending_retry(),
        Some(&steam_session::PendingRetry::MyWorkshopStats {
            generation: 7,
            pages: 2,
        })
    );
    assert_eq!(
        app.state.steam_session.status(),
        steam_session::ConnectionStatus::Connecting
    );
}

#[test]
fn failed_steam_connection_clears_pending_retry() {
    let (mut app, _startup_task) = App::new_for_test();
    let _task = app.batch_effects(
        vec![my_workshop::Effect::PageRequested {
            generation: 7,
            page: 2,
        }],
        App::run_my_workshop_effect,
    );

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionAttemptCompleted(
            steam_session::ConnectionAttempt::unavailable(UiError::detailed(
                gmpublished_backend::error_key::keys::STEAM_ERROR,
                Some("steam unavailable".to_owned()),
            )),
        ),
    ));

    assert_eq!(app.state.steam_session.pending_retry(), None);
    assert_eq!(
        app.state.steam_session.status(),
        steam_session::ConnectionStatus::Unavailable
    );
}

#[test]
fn successful_steam_connection_retries_pending_operation_once() {
    let (mut app, _startup_task) = App::new_for_test();
    let _task = app.batch_effects(
        vec![my_workshop::Effect::PageRequested {
            generation: 7,
            page: 2,
        }],
        App::run_my_workshop_effect,
    );

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionAttemptCompleted(
            steam_session::connect_for_operation_with(|| false, || Ok::<_, UiError>(())),
        ),
    ));

    assert_eq!(app.state.steam_session.pending_retry(), None);
    assert_eq!(
        app.state.steam_session.status(),
        steam_session::ConnectionStatus::Connected
    );
    assert!(app.state.shell.steam_status().connected());

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionAttemptCompleted(
            steam_session::connect_for_operation_with(|| true, || Ok::<_, UiError>(())),
        ),
    ));

    assert_eq!(app.state.steam_session.pending_retry(), None);
}

#[test]
fn connected_steam_attempt_retries_pending_operation_without_edge() {
    let (mut app, _startup_task) = App::new_for_test();
    let retry = steam_session::PendingRetry::MyWorkshopPage {
        generation: 7,
        page: 2,
    };

    let _task = steam_session::update(
        &mut app.state.steam_session,
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connected),
    );
    let _task = steam_session::update(
        &mut app.state.steam_session,
        steam_session::Message::PendingRetrySet(retry.clone()),
    );

    assert_eq!(
        app.state.steam_session.status(),
        steam_session::ConnectionStatus::Connected
    );
    assert_eq!(app.state.steam_session.pending_retry(), Some(&retry));

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionAttemptCompleted(
            steam_session::connect_for_operation_with(|| false, || Ok::<_, UiError>(())),
        ),
    ));

    assert_eq!(app.state.steam_session.pending_retry(), None);
    assert_eq!(
        app.state.steam_session.status(),
        steam_session::ConnectionStatus::Connected
    );
}

#[test]
fn steam_identity_fetch_completion_updates_shell_identity() {
    let (mut app, _startup_task) = App::new_for_test();
    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connected),
    ));

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::IdentityFetched(1, Ok(steam_identity("Ada"))),
    ));

    assert_eq!(app.state.shell.account_name(), Some("Ada"));
    assert!(app.state.shell.steam_avatar().is_some());
}

#[test]
fn steam_identity_fetch_failure_restores_anonymous_shell_identity() {
    let (mut app, _startup_task) = App::new_for_test();
    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connected),
    ));
    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::IdentityFetched(1, Ok(steam_identity("Ada"))),
    ));

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::IdentityFetched(
            1,
            Err(UiError::detailed(
                gmpublished_backend::error_key::keys::STEAM_ERROR,
                Some("steam unavailable".to_owned()),
            )),
        ),
    ));

    assert_eq!(app.state.shell.account_name(), None);
    assert!(app.state.shell.steam_avatar().is_none());
}

#[test]
fn shell_executor_navigated_enters_destination_and_exits_previous_route() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.batch_effects(
        vec![shell::Effect::Navigated {
            from: shell::Route::MyWorkshop,
            to: shell::Route::InstalledAddons,
        }],
        App::run_shell_effect,
    );

    assert!(!app.state.my_workshop.is_route_visible());
    assert!(app.state.installed_addons.is_route_visible());
}

#[test]
fn shell_executor_navigated_runs_size_analyzer_enter_and_exit_effects() {
    let (mut app, _startup_task) = App::new_for_test();
    let snapshot = LibrarySnapshot {
        addons: Arc::from(
            vec![installed_addon_for_library(
                "/tmp/executor-size-analyzer.gma",
                "Executor Size Analyzer Addon",
                &["executor"],
                Some(99),
            )]
            .into_boxed_slice(),
        ),
        epoch: 1,
    };
    let refresh = LibraryRefresh {
        reason: LibraryRefreshReason::DiskChanged,
        snapshot: Some(snapshot),
        rerun_after: None,
    };
    let _task = app.update(RootMessage::LibraryRefreshed(
        LibraryRefreshReason::DiskChanged,
        Ok(refresh),
    ));
    let _task = app.update(RootMessage::SizeAnalyzer(
        size_analyzer::Message::ViewportResized(Size::new(640.0, 360.0)),
    ));
    assert!(!app.state.size_analyzer.is_route_visible());
    assert!(app.state.size_analyzer.projection_key_for_test().is_none());

    let enter_task = app.batch_effects(
        vec![shell::Effect::Navigated {
            from: shell::Route::MyWorkshop,
            to: shell::Route::SizeAnalyzer,
        }],
        App::run_shell_effect,
    );

    assert!(app.state.size_analyzer.is_route_visible());
    assert!(app.state.size_analyzer.projection_key_for_test().is_some());
    assert!(enter_task.units() > 0);

    let _exit_task = app.batch_effects(
        vec![shell::Effect::Navigated {
            from: shell::Route::SizeAnalyzer,
            to: shell::Route::Downloader,
        }],
        App::run_shell_effect,
    );

    assert!(!app.state.size_analyzer.is_route_visible());
}

#[test]
fn shell_navigation_to_current_route_does_not_reenter_route() {
    let (mut app, _startup_task) = App::new_for_test();
    let snapshot = LibrarySnapshot {
        addons: Arc::from(
            vec![installed_addon_for_library(
                "/tmp/reentry.gma",
                "Reentry Addon",
                &["reentry"],
                Some(88),
            )]
            .into_boxed_slice(),
        ),
        epoch: 1,
    };
    let refresh = LibraryRefresh {
        reason: LibraryRefreshReason::DiskChanged,
        snapshot: Some(snapshot),
        rerun_after: None,
    };
    let _task = app.update(RootMessage::LibraryRefreshed(
        LibraryRefreshReason::DiskChanged,
        Ok(refresh),
    ));

    let _task = app.update(RootMessage::SizeAnalyzer(
        size_analyzer::Message::ViewportResized(Size::new(640.0, 360.0)),
    ));
    assert!(app.state.size_analyzer.projection_key_for_test().is_none());

    let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
        shell::Route::SizeAnalyzer,
    )));
    assert_eq!(app.state.shell.route(), shell::Route::SizeAnalyzer);
    let entered_projection = app.state.size_analyzer.projection_key_for_test();
    assert!(entered_projection.is_some());

    let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
        shell::Route::SizeAnalyzer,
    )));

    assert_eq!(
        app.state.size_analyzer.projection_key_for_test(),
        entered_projection
    );
}

#[test]
fn update_check_completion_marks_update_nag_available() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::UpdateCheckCompleted(Ok(Ok(Some(
        shell::UpdateRelease::new(
            "v0.1.1".to_owned(),
            "https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1".to_owned(),
        ),
    )))));

    assert!(app.state.shell.update_available());
    assert_eq!(app.state.shell.update_version(), "v0.1.1");
    assert_eq!(
        app.state.shell.update_release_url(),
        "https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1"
    );
}

#[test]
fn downloader_start_events_sync_shell_job_badge() {
    let (mut app, _startup_task) = App::new_for_test();

    let _task = app.update(RootMessage::Downloader(downloader::Message::EventReceived(
        downloader::DownloaderEvent::TaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            task_id: TaskId::from_raw(7),
        },
    )));

    let badge = app
        .state
        .shell
        .downloader_badge(Instant::now())
        .expect("running Downloader job should project a shell badge");
    assert_eq!(badge.count, 1);
}

#[test]
fn language_switch_rebuilds_the_runtime_bundle() {
    let (mut app, _startup_task) = App::new_for_test();

    app.state.apply_runtime_language(Some("fr-CA"));

    assert_eq!(app.state.i18n.locale_id(), "fr");
    assert_eq!(
        app.state
            .i18n
            .trn("my-workshop-count", &[("arg0", "2"), ("arg1", "8")]),
        "Affichage de 2 sur 8 addons"
    );
    assert_eq!(
        app.state.my_workshop.publish_new_title_for_test(),
        "Publier un nouveau..."
    );
    assert_eq!(app.title(), "gmpublished");
}

#[test]
fn system_theme_modes_map_none_to_dark_fallback() {
    assert_eq!(
        system_scheme_from_mode(Mode::Light),
        SystemColorScheme::Light
    );
    assert_eq!(system_scheme_from_mode(Mode::Dark), SystemColorScheme::Dark);
    assert_eq!(system_scheme_from_mode(Mode::None), SystemColorScheme::Dark);
}

#[test]
fn auto_theme_reapplies_palette_on_live_system_change_without_resetting_accents() {
    let accent_inputs = AccentInputs {
        neutral: 0x00E0_8A2E,
        success: 0x0087_9A57,
        error: 0x00B8_5E42,
    };
    let mut state = State {
        theme_preset: ThemePreset::Auto,
        accent_inputs,
        ..State::default()
    };

    state.apply_system_theme(Mode::Light);

    assert_eq!(state.system_scheme, SystemColorScheme::Light);
    assert_eq!(state.tokens.variant, crate::theme::ThemeVariant::Light);
    assert_eq!(state.accent_inputs, accent_inputs);
    assert_eq!(
        state.tokens.colors.neutral,
        crate::theme::Rgba::rgb(0xE08A2E)
    );
}

#[test]
fn concrete_theme_tracks_system_scheme_but_keeps_palette_fixed() {
    let mut state = State {
        theme_preset: ThemePreset::Dark,
        tokens: crate::theme::Tokens::dark(),
        ..State::default()
    };

    state.apply_system_theme(Mode::Light);

    assert_eq!(state.system_scheme, SystemColorScheme::Light);
    assert_eq!(state.tokens.variant, crate::theme::ThemeVariant::Dark);
}

fn preview_open_request() -> preview_gma::OpenRequest {
    preview_gma::OpenRequest {
        request_id: 1,
        path: PathBuf::from("/tmp/preview-open.gma"),
        workshop_id: Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
    }
}

fn preview_metadata_request() -> preview_gma::MetadataRequest {
    preview_gma::MetadataRequest {
        request_id: 1,
        workshop_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
    }
}

fn preview_author_request() -> preview_gma::AuthorRequest {
    preview_gma::AuthorRequest {
        request_id: 1,
        steamid64: 76_561_197_990_735_296,
    }
}

#[cfg(not(feature = "asset-studio"))]
fn preview_extraction_request() -> preview_gma::ExtractionRequest {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Preview")
            .entry("lua/autorun/init.lua", b"print('ok')\n".to_vec())
            .build(),
    )
    .expect("fixture archive should load");
    preview_gma::ExtractionRequest {
        request_id: 1,
        archive: Arc::new(archive),
        intent: preview_gma::ExtractionIntent::Entry {
            path: "lua/autorun/init.lua".to_owned(),
            size_bytes: 12,
        },
    }
}

#[cfg(feature = "asset-studio")]
fn file_preview_request() -> file_preview::PreviewRequest {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Preview")
            .entry("lua/autorun/init.lua", b"print('ok')\n".to_vec())
            .build(),
    )
    .expect("fixture archive should load");
    file_preview::PreviewRequest {
        request_id: 1,
        archive: PreviewArchiveSource::from_gma(Arc::new(archive)),
        entry_path: "lua/autorun/init.lua".to_owned(),
        display_name: "init.lua".to_owned(),
        size_bytes: 12,
        crc32: 0x1234_5678,
        bypass_size_limits: false,
    }
}

fn installed_preview_target() -> installed_addons::PreviewTarget {
    installed_addons::PreviewTarget {
        row_id: "/tmp/installed-preview.gma".to_owned(),
        path: PathBuf::from("/tmp/installed-preview.gma"),
        title: "Installed Preview".to_owned(),
        workshop_id: Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
        preview_url: Some("https://example.invalid/installed.jpg".to_owned()),
        subscription_count: 1_234,
        score_bucket: 4,
        score_label: "95%".to_owned(),
    }
}

fn installed_context_menu_request() -> installed_addons::ContextMenuRequest {
    installed_addons::ContextMenuRequest {
        position: Point::new(12.0, 24.0),
        row_id: "/tmp/installed-context.gma".to_owned(),
        path: PathBuf::from("/tmp/installed-context.gma"),
        path_text: "/tmp/installed-context.gma".to_owned(),
        workshop_id: Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
        workshop_url: Some(workshop_url::workshop_item_url(
            PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
        )),
        preview_url: Some("https://example.invalid/installed.jpg".to_owned()),
        entries: vec![context_menu::Entry::copy_path()],
    }
}

fn my_workshop_context_menu_request() -> my_workshop::ContextMenuRequest {
    my_workshop::ContextMenuRequest {
        position: Point::new(12.0, 24.0),
        row_id: "123".to_owned(),
        workshop_id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
        workshop_url: workshop_url::workshop_item_url(
            PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
        ),
        preview_url: Some("https://example.invalid/my-workshop.jpg".to_owned()),
        entries: vec![context_menu::Entry::copy_link()],
    }
}

fn size_analyzer_context_menu_effect() -> size_analyzer::Effect {
    let mut state = size_analyzer_state_with_snapshot();
    hover_size_analyzer_workshop_leaf(&mut state);
    let effects = size_analyzer::update(
        &mut state,
        size_analyzer::Message::TreemapRightPressed(Point::new(16.0, 32.0)),
    );
    effects
        .into_iter()
        .find(|effect| matches!(effect, size_analyzer::Effect::ContextMenuRequested(_)))
        .expect("right press should request context menu")
}

fn size_analyzer_state_with_viewport() -> size_analyzer::State {
    let mut state = size_analyzer::State::default();
    let _effects = size_analyzer::update(&mut state, size_analyzer::Message::RouteEntered);
    let _effects = size_analyzer::update(
        &mut state,
        size_analyzer::Message::ViewportResized(Size::new(640.0, 360.0)),
    );
    state
}

fn size_analyzer_state_with_snapshot() -> size_analyzer::State {
    let mut state = size_analyzer_state_with_viewport();
    let _effects = size_analyzer::update(
        &mut state,
        size_analyzer::Message::SnapshotPushed(
            LibraryRefreshReason::Startup,
            Ok(Some(size_analyzer_snapshot(1))),
        ),
    );
    state
}

fn hover_size_analyzer_workshop_leaf(state: &mut size_analyzer::State) {
    let leaf = state
        .layout()
        .expect("layout should exist")
        .leaf_rects()
        .into_iter()
        .find(|leaf| leaf.addon.workshop_id.map(PublishedFileId::get) == Some(123))
        .expect("workshop leaf should exist");
    let point = Point::new(
        (leaf.rect.x + leaf.rect.width / 2.0) as f32,
        (leaf.rect.y + leaf.rect.height / 2.0) as f32,
    );
    let _effects = size_analyzer::update(state, size_analyzer::Message::HoverMoved(point));
}

fn size_analyzer_snapshot(epoch: u64) -> LibrarySnapshot {
    LibrarySnapshot {
        addons: Arc::from(
            vec![
                installed_addon_for_library(
                    "/tmp/size-workshop.gma",
                    "Size Workshop",
                    &["tool"],
                    Some(123),
                ),
                installed_addon_for_library("/tmp/size-local.gma", "Size Local", &["map"], None),
            ]
            .into_boxed_slice(),
        ),
        epoch,
    }
}

fn seed_search_request(app: &mut App) -> SearchQuickRequest {
    app.state
        .search
        .edit_query("alpha".to_owned())
        .quick_request
        .expect("query should produce quick search request")
}

fn seed_search_result(app: &mut App, has_more: bool) -> SearchQuickRequest {
    let request = seed_search_request(app);
    let batch = SearchQuickBatch::new(
        request.key().clone(),
        vec![search_hit("Alpha", 42)],
        has_more,
        SearchQuickCarry::default(),
    );
    assert!(
        app.state
            .search
            .apply_quick_result(request.key(), Ok(batch))
    );
    app.state.viewport_size = Size::new(800.0, 600.0);
    let (generation, ids) = app
        .state
        .search
        .take_thumbnail_metadata_request(600.0)
        .expect("search thumbnail metadata request");
    assert!(app.state.search.apply_metadata_refresh(
        generation,
        &ids,
        Ok(vec![search::MetadataPatch::for_test(
            PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
            Some("https://example.test/alpha.png")
        )]),
    ));
    request
}

fn search_hit(label: &str, workshop_id: u64) -> SearchHit {
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

fn steam_identity(name: &str) -> steam_session::SteamIdentity {
    steam_session::SteamIdentity::from_user(SteamUser {
        steamid: 76561198000000001,
        name: name.to_owned(),
        avatar: Some(AvatarRgba::new(1, 1, vec![1, 2, 3, 4]).expect("test avatar should be valid")),
        dead: false,
    })
}

fn installed_addon_for_library(
    path: &str,
    title: &str,
    tags: &[&str],
    workshop_id: Option<u64>,
) -> InstalledAddon {
    InstalledAddon {
        path: PathBuf::from(path),
        canonical_path: PathBuf::from(path),
        workshop_id: workshop_id.and_then(PublishedFileId::new),
        file_size_bytes: 123,
        modified_epoch_seconds: 456,
        meta: GmaMeta {
            path: PathBuf::from(path),
            header: GmaHeader {
                version: 3,
                timestamp: 0,
                metadata: GmaMetadata::Standard {
                    title: title.to_owned(),
                    addon_type: "servercontent".to_owned(),
                    tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
                    ignore: Vec::new(),
                },
                author: String::new(),
                addon_version: 1,
            },
            entries: Vec::new(),
        },
    }
}
