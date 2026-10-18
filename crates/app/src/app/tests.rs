use crate::bridge::tasks::TransactionStatus;
use crate::media::preview_model;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::i18n::Arg;

use crate::bridge::domain::{
    AvatarRgba, InstalledAddon, PublishedFileId, SearchHit, SearchItem, SearchItemSource,
    SearchQuickBatch, SearchQuickRequest, SteamUser, WorkshopDownloadSuccess, workshop_url,
};
use gmpublished_backend::{
    AppDataPathsSnapshot as BackendAppDataPathsSnapshot, AppDataSnapshot as BackendAppDataSnapshot,
    Settings as BackendSettings,
};
use iced::{Point, Size, Task, keyboard, theme::Mode, widget::image};

use crate::bridge::domain::SteamId;
use crate::{bridge::archive::PreviewArchiveSource, features::file_preview};
use crate::{
    bridge::{
        DownloadCountFormat, ExtractDestination, SystemColorScheme, ThemePreset,
        gma::{GmaError, GmaHeader, GmaMeta, GmaMetadata, PreviewArchive},
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
use gmpublished_backend::TransactionId;

#[test]
fn staged_startup_activates_exactly_once_after_first_frame() {
    let mut phase = StartupPhase::WaitingForFirstFrame;

    assert!(!phase.started());
    assert!(phase.activate_after_first_frame());
    assert!(phase.started());
    assert!(!phase.activate_after_first_frame());
}

use super::{
    AddonDragMessage, AddonDragOutcome, AddonDragSource, AddonDragState, App, ContextMenuTarget,
    ExtractionHandoff, GlobalShortcut, LocalMenuTarget, RootMessage, StartupPhase, State,
    backend_runtime_action_message, map_global_shortcut, map_settings_toggle_shortcut,
    system_scheme_from_mode,
};
use crate::bridge::ui_error::UiError;
use crate::generation::Generation;
use crate::widgets::grid_rows::CardId;

fn task_messages(task: Task<RootMessage>) -> Vec<RootMessage> {
    use futures::StreamExt;

    futures::executor::block_on(async {
        let Some(mut actions) = iced_runtime::task::into_stream(task) else {
            return Vec::new();
        };
        let mut messages = Vec::new();
        while let Some(action) = actions.next().await {
            match action {
                iced_runtime::Action::Output(message) => messages.push(message),
                _ => panic!("expected a message task"),
            }
        }
        messages
    })
}

#[test]
fn gma_file_drop_routes_the_selected_path_to_bulk_extraction() {
    let mut app = App::new_for_test();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("selected.gma");
    std::fs::write(&path, b"GMAD").unwrap();

    let messages = task_messages(app.handle_file_drop(path.clone()));
    assert!(matches!(messages.as_slice(),
        [RootMessage::Downloader(downloader::Message::BulkExtractPathsSelected(paths))]
        if paths == &[path]
    ));
    assert!(matches!(
        task_messages(app.handle_file_drop(root.path().join("missing.gma"))).as_slice(),
        [RootMessage::Noop]
    ));
    assert!(task_messages(app.handle_file_drop(root.path().join("other.txt"))).is_empty());
}

fn assert_task_scheduled(task: &Task<RootMessage>) {
    assert!(task.units() > 0, "expected a scheduled task");
}

fn assert_no_task(task: &Task<RootMessage>) {
    assert_eq!(task.units(), 0, "expected no scheduled task");
}

#[test]
fn update_delegates_modal_stack_messages_to_modal_state() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::OpenDestinationSelect,
    ));

    assert!(app.state.features.modal_stack.overlay_active());
    assert_eq!(app.state.features.modal_stack.active(), None);
}

#[test]
fn modal_stack_close_clears_preview_gma_state() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::PreviewGma(
        preview_gma::Message::OpenRequested(preview_gma::OpenTarget::new(
            PathBuf::from("/tmp/test.gma"),
            "Test",
            Some(PublishedFileId::fixture(123)),
        )),
    ));

    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
    assert!(app.state.features.preview_gma.is_open());

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));

    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
    assert!(app.state.features.preview_gma.is_open());

    let _task = app.update(RootMessage::AnimationTick(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    ));

    assert_eq!(app.state.features.modal_stack.active(), None);
    assert!(!app.state.features.preview_gma.is_open());
}

#[test]
fn destination_overlay_layers_over_preview_and_closes_first() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::PreviewGma(
        preview_gma::Message::OpenRequested(preview_gma::OpenTarget::new(
            PathBuf::from("/tmp/test.gma"),
            "Test",
            Some(PublishedFileId::fixture(123)),
        )),
    ));
    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::OpenDestinationSelect,
    ));

    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
    assert!(app.state.features.modal_stack.overlay_active());
    assert!(app.state.features.preview_gma.is_open());

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));
    let _task = app.update(RootMessage::AnimationTick(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    ));

    assert!(!app.state.features.modal_stack.overlay_active());
    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
    assert!(app.state.features.preview_gma.is_open());
}

#[test]
fn prepare_publish_open_claims_modal_stack_slot() {
    let mut app = App::new_for_test();

    app.environment
        .ctx
        .update_settings_snapshot_for_test(|settings| {
            settings.backend.ignore_globs = vec!["lua/private/*".to_owned()];
            settings.backend.upscale_addon_icon = true;
        })
        .unwrap();
    let task = app.batch_effects(
        vec![my_workshop::Effect::PreparePublishRequested(
            my_workshop::PreparePublishTarget::New,
        )],
        App::run_my_workshop_effect,
    );
    let messages = task_messages(task);
    assert!(matches!(messages.as_slice(),
        [RootMessage::PreparePublish(prepare_publish::Message::OpenRequested {
            target: prepare_publish::OpenTarget::New,
            ignored_patterns,
            upscale_icon_default: true,
        })] if ignored_patterns.iter().any(|pattern| pattern.pattern == "lua/private/*" && !pattern.default_pattern)
    ));
    for message in messages {
        let _task = app.update(message);
    }

    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreparePublish)
    );
    assert!(app.state.features.prepare_publish.open());
}

#[test]
fn modal_stack_close_clears_prepare_publish_state() {
    let mut app = App::new_for_test();

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
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreparePublish)
    );
    assert!(app.state.features.prepare_publish.open());

    let _task = app.update(RootMessage::AnimationTick(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    ));

    assert_eq!(app.state.features.modal_stack.active(), None);
    assert!(!app.state.features.prepare_publish.open());
}

#[test]
fn a_displaced_modal_runs_the_close_teardown_it_never_animated_into() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::PreparePublish(
        prepare_publish::Message::OpenRequested {
            target: prepare_publish::OpenTarget::New,
            ignored_patterns: Vec::new(),
            upscale_icon_default: false,
        },
    ));
    assert!(app.state.features.prepare_publish.open());

    let snapshot = app.settings_snapshot();
    let _task = app.update(RootMessage::Settings(settings::Message::OpenRequested(
        snapshot,
    )));

    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::Settings)
    );
    assert!(!app.state.features.prepare_publish.open());
}

#[test]
fn a_preview_message_wrapped_by_prepare_publish_still_reaches_the_previewer() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::FilePreview(
        file_preview::Message::OpenRequested(file_preview_request()),
    ));
    assert!(!app.state.features.file_preview.expanded());

    let _task = app.update(RootMessage::PreparePublish(
        prepare_publish::Message::FilePreview(file_preview::Message::ExpandToggled),
    ));

    assert!(
        app.state.features.file_preview.expanded(),
        "the wrapped message must reach file_preview's own update"
    );
}

#[test]
fn settings_activation_claims_modal_stack_slot() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::Shell(shell::Message::SettingsActivated));
    let snapshot = app.settings_snapshot();
    let _task = app.update(RootMessage::Settings(settings::Message::OpenRequested(
        snapshot,
    )));

    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::Settings)
    );
    assert!(app.state.features.settings.open());
}

#[test]
fn downloader_executor_cancels_requested_tasks() {
    let mut app = App::new_for_test();
    let handle = app
        .environment
        .ctx
        .create_task(TaskKind::Download, TransactionStatus::Downloading);
    let task_id = handle.id();
    let transaction = app.environment.ctx.begin_transaction();
    app.environment
        .ctx
        .correlate_backend_transaction(transaction.id(), handle);

    let task = app.batch_effects(
        vec![downloader::Effect::TaskCancellationRequested(vec![task_id])],
        App::run_downloader_effect,
    );

    assert_no_task(&task);
    assert!(transaction.aborted());
}

#[test]
fn workshop_download_row_cancel_button_aborts_the_backend_transaction() {
    let mut app = App::new_for_test();

    let events = app.environment.ctx.take_task_events_for_test();
    let transaction = app.environment.ctx.begin_transaction();
    let _task = app.update(RootMessage::BackendEvent(
        BackendRuntimeEvent::DownloadStarted {
            transaction_id: transaction.id(),
            request_id: None,
            workshop_id: Some(PublishedFileId::fixture(123)),
        },
    ));
    let event = events
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("backend start must enter the ordered task stream");
    let _task = app.update(RootMessage::TaskEvent(event));
    assert_eq!(app.state.features.downloader.downloading().len(), 1);

    let row_id = app.state.features.downloader.downloading()[0].id();
    let _task = app.update(RootMessage::Downloader(
        downloader::Message::CancelRequested {
            section: downloader::Section::Downloading,
            row_id,
        },
    ));

    assert!(app.state.features.downloader.downloading().is_empty());
    assert!(transaction.aborted());
}

#[test]
fn downloader_row_cancel_button_is_clickable_through_the_full_app_view() {
    let mut app = App::new_for_test();
    let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
        shell::Route::Downloader,
    )));
    let _task = app.update(RootMessage::Downloader(
        downloader::Message::TaskEventsReceived(vec![crate::test_support::workshop_task_started(
            WorkshopDownloadTaskKind::Download,
            PublishedFileId::fixture(123),
            TaskId::from_raw(7),
        )]),
    ));
    assert_eq!(app.state.features.downloader.downloading().len(), 1);
    let row_id = app.state.features.downloader.downloading()[0].id();

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
fn local_extraction_transaction_is_correlated_and_cancellation_stops_it() {
    let app = App::new_for_test();
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Cancel Test")
            .entry("lua/autorun/cancel.lua", b"print('hi')\n".to_vec())
            .build(),
    )
    .expect("fixture archive should load");

    let task = app
        .environment
        .ctx
        .create_task(TaskKind::Extract, TransactionStatus::Extracting);
    let task_id = task.id();
    let transaction = app.environment.ctx.begin_correlated_transaction(task);

    assert!(app.environment.ctx.cancel_task(task_id));
    assert!(transaction.aborted());

    let result = app.environment.ctx.extract_preview_archive(
        &archive,
        ExtractDestination::Temp,
        &transaction,
    );

    assert!(matches!(result, Err(GmaError::Cancelled)));
    assert!(!app.environment.ctx.cancel_task(task_id));
}

#[test]
fn malformed_local_archive_finishes_its_identified_task_with_one_error() {
    use crate::bridge::tasks::{TaskIdentity, TaskUpdate};

    let app = App::new_for_test();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("broken.gma");
    std::fs::write(&path, b"invalid archive").unwrap();
    let receiver = app.environment.ctx.take_task_events_for_test();
    let task = app
        .environment
        .ctx
        .create_local_extraction_task(path.clone(), 15);
    let task_id = task.id();
    let result = crate::app::runners::run_local_gma_extraction(
        &app.environment.ctx,
        &path,
        ExtractDestination::Temp,
        task,
    );
    assert!(result.is_err());
    let updates = receiver.try_iter().collect::<Vec<_>>();
    assert_eq!(updates.len(), 2);
    assert!(updates.iter().all(|(id, _)| *id == task_id));
    assert!(matches!(updates[0].1.as_update(), TaskUpdate::Started {
        identity: Some(TaskIdentity::LocalExtraction { path: source, .. }), ..
    } if source == &path));
    assert!(matches!(updates[1].1.as_update(), TaskUpdate::Error(_)));
}

#[test]
fn downloader_executor_destination_selection_opens_overlay() {
    let mut app = App::new_for_test();

    let task = app.batch_effects(
        vec![downloader::Effect::DestinationSelectionRequested],
        App::run_downloader_effect,
    );

    assert!(app.state.features.modal_stack.overlay_active());
    assert_no_task(&task);
}

#[test]
fn destination_select_executor_modal_open_opens_overlay() {
    let mut app = App::new_for_test();

    let task = app.batch_effects(
        vec![destination_select::Effect::ModalOpenRequested],
        App::run_destination_select_effect,
    );

    assert_no_task(&task);
    assert!(app.state.features.modal_stack.overlay_active());
}

/// Saving starts the close animation and disables input. This test does not
/// tick the animation, so overlay_active must still be true.
#[test]
fn destination_select_executor_persisted_begins_closing_the_overlay() {
    let mut app = App::new_for_test();
    let _task = app.batch_effects(
        vec![destination_select::Effect::ModalOpenRequested],
        App::run_destination_select_effect,
    );

    let task = app.batch_effects(
        vec![destination_select::Effect::DestinationPersisted],
        App::run_destination_select_effect,
    );

    assert_no_task(&task);
    assert!(app.state.features.modal_stack.overlay_active());
    assert!(!app.state.features.modal_stack.overlay_interactive());
}

#[test]
fn destination_select_executor_dismissed_clears_extraction_handoff() {
    let mut app = App::new_for_test();
    app.state
        .context_menu_extraction
        .begin(vec![PathBuf::from("/tmp/context.gma")]);

    let task = app.batch_effects(
        vec![destination_select::Effect::DestinationDismissed],
        App::run_destination_select_effect,
    );

    assert_no_task(&task);
    assert_eq!(app.state.context_menu_extraction, ExtractionHandoff::Idle);
}

#[test]
fn downloader_executor_active_job_count_updates_shell_badge() {
    let mut app = App::new_for_test();

    let task = app.batch_effects(
        vec![downloader::Effect::ActiveJobCountChanged(3)],
        App::run_downloader_effect,
    );

    assert_no_task(&task);
    assert_eq!(app.state.features.shell.downloader_jobs(), 3);
    assert_eq!(
        app.state
            .features
            .shell
            .downloader_badge(Instant::now())
            .expect("active count should show badge")
            .count,
        3
    );
}

#[test]
fn search_executor_full_search_starts_stream_task() {
    let mut app = App::new_for_test();
    seed_search_result(&mut app, true);

    let task = app.batch_effects(
        vec![search::Effect::FullSearchRequested],
        App::run_search_effect,
    );

    assert_task_scheduled(&task);
    assert!(app.state.features.search.loading());
    assert!(!app.state.features.search.should_begin_full_search());
}

#[test]
fn search_executor_cancels_requested_task() {
    let mut app = App::new_for_test();
    let handle = app
        .environment
        .ctx
        .create_task(TaskKind::Search, TransactionStatus::Searching);
    let task_id = handle.id();
    let transaction = app.environment.ctx.begin_transaction();
    app.environment
        .ctx
        .correlate_backend_transaction(transaction.id(), handle);

    let task = app.batch_effects(
        vec![search::Effect::TaskCancellationRequested(task_id)],
        App::run_search_effect,
    );

    assert_no_task(&task);
    assert!(transaction.aborted());
}

#[test]
fn search_executor_metadata_refresh_defers_until_steam_connects() {
    let mut app = App::new_for_test();

    let _task = app.batch_effects(
        vec![search::Effect::MetadataRefreshRequested {
            generation: Generation::from_raw(7),
            item_ids: vec![PublishedFileId::fixture(123)],
        }],
        App::run_search_effect,
    );

    assert_eq!(
        app.state.features.steam_session.pending_retries(),
        [steam_session::PendingRetry::SearchMetadataRefresh {
            generation: Generation::from_raw(7),
            item_ids: vec![PublishedFileId::fixture(123)],
        }]
    );
}

#[test]
fn my_workshop_executor_page_request_defers_until_steam_connects() {
    let mut app = App::new_for_test();

    let _task = app.batch_effects(
        vec![my_workshop::Effect::PageRequested {
            generation: Generation::from_raw(7),
            page: 2,
        }],
        App::run_my_workshop_effect,
    );

    assert_eq!(
        app.state.features.steam_session.pending_retries(),
        [steam_session::PendingRetry::MyWorkshopPage {
            generation: Generation::from_raw(7),
            page: 2,
        }]
    );
}

#[test]
fn my_workshop_executor_stats_refresh_defers_until_steam_connects() {
    let mut app = App::new_for_test();

    let _task = app.batch_effects(
        vec![my_workshop::Effect::StatsRefreshRequested {
            generation: Generation::from_raw(7),
            pages: 2,
        }],
        App::run_my_workshop_effect,
    );

    assert_eq!(
        app.state.features.steam_session.pending_retries(),
        [steam_session::PendingRetry::MyWorkshopStats {
            generation: Generation::from_raw(7),
            pages: 2,
        }]
    );
}

#[test]
fn my_workshop_executor_drag_press_updates_drag_state() {
    let mut app = App::new_for_test();

    let task = app.batch_effects(
        vec![my_workshop::Effect::AddonDragPressed {
            card_id: CardId::from("123"),
            workshop_id: Some(PublishedFileId::fixture(123)),
        }],
        App::run_my_workshop_effect,
    );

    assert_no_task(&task);
    assert!(app.state.addon_drag.is_active());
}

#[test]
fn my_workshop_executor_drag_release_finishes_drag() {
    let mut app = App::new_for_test();
    app.state.addon_drag.press(
        AddonDragSource::MyWorkshop,
        CardId::from("123"),
        Some(PublishedFileId::fixture(123)),
        None,
    );

    let task = app.batch_effects(
        vec![my_workshop::Effect::AddonDragReleased],
        App::run_my_workshop_effect,
    );

    assert_task_scheduled(&task);
    assert!(!app.state.addon_drag.is_active());
}

#[test]
fn installed_addons_executor_metadata_request_defers_until_steam_connects() {
    let mut app = App::new_for_test();

    let _task = app.batch_effects(
        vec![installed_addons::Effect::MetadataRequested {
            generation: Generation::from_raw(7),
            item_ids: vec![PublishedFileId::fixture(123)],
        }],
        App::run_installed_addons_effect,
    );

    assert_eq!(
        app.state.features.steam_session.pending_retries(),
        [steam_session::PendingRetry::InstalledMetadata {
            generation: Generation::from_raw(7),
            item_ids: vec![PublishedFileId::fixture(123)],
        }]
    );
}

#[test]
fn installed_addons_executor_metadata_refresh_defers_until_steam_connects() {
    let mut app = App::new_for_test();

    let _task = app.batch_effects(
        vec![installed_addons::Effect::MetadataRefreshRequested {
            generation: Generation::from_raw(7),
            item_ids: vec![PublishedFileId::fixture(123)],
        }],
        App::run_installed_addons_effect,
    );

    assert_eq!(
        app.state.features.steam_session.pending_retries(),
        [steam_session::PendingRetry::InstalledMetadataRefresh {
            generation: Generation::from_raw(7),
            item_ids: vec![PublishedFileId::fixture(123)],
        }]
    );
}

#[test]
fn installed_addons_executor_preview_schedules_preview_open() {
    let mut app = App::new_for_test();

    let task = app.batch_effects(
        vec![installed_addons::Effect::PreviewRequested(
            installed_preview_target(),
        )],
        App::run_installed_addons_effect,
    );

    assert_task_scheduled(&task);
    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
}

#[test]
fn installed_addons_executor_drag_press_updates_drag_state() {
    let mut app = App::new_for_test();

    let task = app.batch_effects(
        vec![installed_addons::Effect::AddonDragPressed {
            card_id: CardId::from("/tmp/drag.gma"),
            workshop_id: Some(PublishedFileId::fixture(123)),
        }],
        App::run_installed_addons_effect,
    );

    assert_no_task(&task);
    assert!(app.state.addon_drag.is_active());
}

#[test]
fn installed_addons_executor_drag_release_finishes_drag() {
    let mut app = App::new_for_test();
    app.state.addon_drag.press(
        AddonDragSource::InstalledAddons,
        CardId::from("/tmp/drag.gma"),
        Some(PublishedFileId::fixture(123)),
        None,
    );

    let task = app.batch_effects(
        vec![installed_addons::Effect::AddonDragReleased],
        App::run_installed_addons_effect,
    );

    assert_task_scheduled(&task);
    assert!(!app.state.addon_drag.is_active());
}

#[test]
fn size_analyzer_executor_drag_press_updates_drag_state() {
    let mut app = App::new_for_test();

    let task = app.batch_effects(
        vec![size_analyzer::Effect::AddonDragPressed {
            card_id: CardId::from("/tmp/size-drag.gma"),
            workshop_id: Some(PublishedFileId::fixture(123)),
        }],
        App::run_size_analyzer_effect,
    );

    assert_no_task(&task);
    assert!(app.state.addon_drag.is_active());
}

#[test]
fn size_analyzer_executor_drag_release_finishes_drag() {
    let mut app = App::new_for_test();
    app.state.addon_drag.press(
        AddonDragSource::SizeAnalyzer,
        CardId::from("/tmp/size-drag.gma"),
        Some(PublishedFileId::fixture(123)),
        None,
    );

    let task = app.batch_effects(
        vec![size_analyzer::Effect::AddonDragReleased],
        App::run_size_analyzer_effect,
    );

    assert_task_scheduled(&task);
    assert!(!app.state.addon_drag.is_active());
}

#[test]
fn preview_gma_executor_modal_open_claims_modal_slot() {
    let mut app = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::ModalOpenRequested],
        App::run_preview_gma_effect,
    );

    assert_no_task(&task);
    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
}

#[test]
fn preview_gma_executor_destination_select_opens_overlay() {
    let mut app = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::DestinationSelectRequested],
        App::run_preview_gma_effect,
    );

    assert_no_task(&task);
    assert!(app.state.features.modal_stack.overlay_active());
}

#[test]
fn preview_gma_executor_entry_preview_opens_embedded_file_preview() {
    let mut app = App::new_for_test();

    let task = app.batch_effects(
        vec![preview_gma::Effect::EntryPreviewRequested(
            file_preview_request(),
        )],
        App::run_preview_gma_effect,
    );

    assert_task_scheduled(&task);
    assert_eq!(app.state.features.modal_stack.overlay_modal(), None);
    assert!(app.state.features.file_preview.loading());
}

#[test]
fn prepare_publish_executor_entry_preview_opens_embedded_file_preview() {
    let mut app = App::new_for_test();

    let task = app.batch_effects(
        vec![prepare_publish::Effect::EntryPreviewRequested(
            file_preview_request(),
        )],
        App::run_prepare_publish_effect,
    );

    assert_task_scheduled(&task);
    assert!(app.state.features.file_preview.loading());
}

#[test]
fn preview_gma_close_request_chains_expanded_preview_back_then_modal() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::PreviewGma(
        preview_gma::Message::OpenRequested(preview_gma::OpenTarget::new(
            PathBuf::from("/tmp/test.gma"),
            "Test",
            Some(PublishedFileId::fixture(123)),
        )),
    ));
    let _task = app.update(RootMessage::FilePreview(
        file_preview::Message::OpenRequested(file_preview_request()),
    ));
    let _task = app.update(RootMessage::FilePreview(
        file_preview::Message::ExpandToggled,
    ));

    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
    assert!(app.state.features.preview_gma.is_open());
    assert!(app.state.features.file_preview.is_open());
    assert!(app.state.features.file_preview.expanded());

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));

    assert!(app.state.features.file_preview.is_open());
    assert!(!app.state.features.file_preview.expanded());
    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));

    assert!(!app.state.features.file_preview.is_open());
    assert!(app.state.features.preview_gma.is_open());
    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));
    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
    assert!(!app.state.features.modal_stack.interactive());

    let _task = app.update(RootMessage::AnimationTick(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    ));

    assert_eq!(app.state.features.modal_stack.active(), None);
    assert!(!app.state.features.preview_gma.is_open());
}

#[test]
fn preview_gma_close_request_back_stops_embedded_audio_preview() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::PreviewGma(
        preview_gma::Message::OpenRequested(preview_gma::OpenTarget::new(
            PathBuf::from("/tmp/test.gma"),
            "Test",
            Some(PublishedFileId::fixture(123)),
        )),
    ));
    let _task = app.update(RootMessage::FilePreview(
        file_preview::Message::OpenRequested(file_preview_request()),
    ));
    let request = app
        .state
        .features
        .file_preview
        .request()
        .expect("preview request should be active")
        .clone();
    let data = preview_model::PreviewData::from_request(
        &request,
        preview_model::PreviewContent::Audio {
            bytes: Arc::from(vec![1, 2, 3]),
            duration_secs: Some(4.0),
        },
    );
    let _task = app.update(RootMessage::FilePreview(file_preview::Message::Loaded(
        request.request_id,
        Box::new(Ok(data)),
    )));
    let _task = app.update(RootMessage::FilePreview(
        file_preview::Message::AudioPlaybackStarted(request.request_id),
    ));

    assert!(app.state.features.file_preview.is_open());
    assert!(app.state.features.file_preview.audio_playing());

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));

    assert!(!app.state.features.file_preview.is_open());
    assert!(!app.state.features.file_preview.audio_playing());
    assert!(app.state.features.preview_gma.is_open());
    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
}

#[test]
fn settings_executor_modal_open_claims_modal_slot() {
    let mut app = App::new_for_test();

    let task = app.batch_effects(
        vec![settings::Effect::ModalOpenRequested],
        App::run_settings_effect,
    );

    assert_no_task(&task);
    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::Settings)
    );
}

#[test]
fn settings_executor_modal_close_starts_close_animation() {
    let mut app = App::new_for_test();
    let _task = app.batch_effects(
        vec![settings::Effect::ModalOpenRequested],
        App::run_settings_effect,
    );

    let task = app.batch_effects(
        vec![settings::Effect::ModalCloseRequested],
        App::run_settings_effect,
    );

    assert_no_task(&task);
    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::Settings)
    );
    assert!(!app.state.features.modal_stack.interactive());
}

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
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::Shell(shell::Message::AccountMenuToggled));
    assert!(app.state.features.shell.account_menu_open());

    let _task = app.batch_effects(
        vec![shell::Effect::OpenSearchPalette],
        App::run_shell_effect,
    );

    assert!(app.state.features.search.palette_open());
    assert!(!app.state.features.shell.account_menu_open());
}

#[test]
fn command_f_toggles_palette_when_global_shortcuts_are_enabled() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSearch));
    assert!(app.state.features.search.palette_open());

    let _task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSearch));
    assert!(!app.state.features.search.palette_open());
}

#[test]
fn global_shortcuts_are_guarded_while_context_or_modal_ui_is_active() {
    let mut app = App::new_for_test();
    let _task = app.update(RootMessage::ContextMenu(
        context_menu::Message::OpenRequested(context_menu::OpenRequest::new(
            Point::new(10.0, 10.0),
            vec![context_menu::Entry::copy_path()],
            ContextMenuTarget::Local(LocalMenuTarget {
                path: PathBuf::from("/tmp/addon.gma"),
                path_text: "/tmp/addon.gma".to_owned(),
                workshop_id: None,
                workshop_url: None,
                preview_url: None,
            }),
        )),
    ));

    let _task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSearch));

    assert!(!app.state.features.search.palette_open());

    let mut app = App::new_for_test();
    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::OpenDestinationSelect,
    ));

    let _task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSearch));

    assert!(!app.state.features.search.palette_open());
}

#[test]
fn command_comma_dismisses_account_menu_and_defers_to_settings_open_task() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::Shell(shell::Message::AccountMenuToggled));
    assert!(app.state.features.shell.account_menu_open());

    let _task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSettings));

    assert!(!app.state.features.shell.account_menu_open());
    assert_eq!(app.state.features.modal_stack.active(), None);
}

#[test]
fn requesting_a_settings_close_clears_the_modal_on_the_next_tick() {
    let mut app = App::new_for_test();
    let snapshot = app.settings_snapshot();
    let _task = app.update(RootMessage::Settings(settings::Message::OpenRequested(
        snapshot,
    )));
    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::Settings)
    );

    let task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSettings));
    let messages = task_messages(task);
    assert!(matches!(
        messages.as_slice(),
        [RootMessage::Settings(settings::Message::CloseRequested)]
    ));
    for message in messages {
        let _task = app.update(message);
    }

    let _task = app.update(RootMessage::AnimationTick(
        Instant::now() + Duration::from_secs(1),
    ));

    assert_eq!(app.state.features.modal_stack.active(), None);
    assert!(!app.state.features.settings.open());
}

#[test]
fn settings_toggle_mapper_maps_only_command_comma() {
    let command = keyboard::Modifiers::COMMAND;

    assert!(matches!(
        map_settings_toggle_shortcut(&keyboard::Key::Character(",".into()), command),
        Some(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSettings))
    ));
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
    let mut app = App::new_for_test();
    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::OpenPreviewGma,
    ));
    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );

    let task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSettings));

    assert_no_task(&task);
    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::PreviewGma)
    );
}

#[test]
fn search_result_activation_closes_palette() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSearch));
    assert!(app.state.features.search.palette_open());

    let _task = app.update(RootMessage::Search(search::Message::ResultActivated(123)));

    assert!(!app.state.features.search.palette_open());
}

#[test]
fn modal_stack_close_clears_settings_state() {
    let mut app = App::new_for_test();
    let snapshot = app.settings_snapshot();
    let _task = app.update(RootMessage::Settings(settings::Message::OpenRequested(
        snapshot,
    )));

    let _task = app.update(RootMessage::ModalStack(
        modal_stack::Message::CloseRequested,
    ));

    assert_eq!(
        app.state.features.modal_stack.active(),
        Some(modal_stack::ActiveModal::Settings)
    );
    assert!(app.state.features.settings.open());

    let _task = app.update(RootMessage::AnimationTick(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    ));

    assert_eq!(app.state.features.modal_stack.active(), None);
    assert!(!app.state.features.settings.open());
}

#[test]
fn context_menu_extract_opens_destination_select_with_pending_path() {
    let mut app = App::new_for_test();
    let path = PathBuf::from("/tmp/context-menu.gma");
    let _task = app.update(RootMessage::ContextMenu(
        context_menu::Message::OpenRequested(context_menu::OpenRequest::new(
            Point::ORIGIN,
            vec![context_menu::Entry::extract()],
            ContextMenuTarget::Local(LocalMenuTarget {
                path: path.clone(),
                path_text: path.display().to_string(),
                workshop_id: None,
                workshop_url: None,
                preview_url: None,
            }),
        )),
    ));

    let _task = app.update(RootMessage::ContextMenu(
        context_menu::Message::ActionSelected(context_menu::ContextMenuAction::Extract),
    ));

    assert!(app.state.features.modal_stack.overlay_active());
    assert_eq!(app.state.features.modal_stack.active(), None);
    assert_eq!(
        app.state.context_menu_extraction.paths(),
        Some([path].as_slice())
    );
    assert!(app.state.features.context_menu.target().is_none());
    assert!(!app.state.features.context_menu.open());
    assert!(app.state.features.context_menu.visible());
}

#[test]
fn context_menu_dismiss_retains_target_until_close_settles() {
    let mut app = App::new_for_test();
    let _task = app.update(RootMessage::ContextMenu(
        context_menu::Message::OpenRequested(context_menu::OpenRequest::new(
            Point::ORIGIN,
            vec![context_menu::Entry::steam_workshop()],
            ContextMenuTarget::MyWorkshop {
                workshop_id: PublishedFileId::fixture(42),
                workshop_url: workshop_url::workshop_item_url(PublishedFileId::fixture(42)),
                preview_url: None,
            },
        )),
    ));

    let _task = app.update(RootMessage::ContextMenu(
        context_menu::Message::DismissRequested,
    ));

    assert!(app.state.features.context_menu.target().is_some());
    assert!(app.state.features.context_menu.visible());
    let _task = app.update(RootMessage::AnimationTick(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    ));
    assert!(app.state.features.context_menu.target().is_none());
    assert!(!app.state.features.context_menu.visible());
}

#[cfg(feature = "debug")]
#[test]
fn hide_addon_context_action_is_session_scoped_and_removes_my_workshop_row() {
    let mut app = App::new_for_test();
    let workshop_id = PublishedFileId::fixture(123);
    app.state
        .features
        .my_workshop
        .push_rows_for_test(vec![my_workshop::Row::for_test(123, "Hidden", 10)], 1);
    let _task = app.update(RootMessage::ContextMenu(
        context_menu::Message::OpenRequested(context_menu::OpenRequest::new(
            Point::ORIGIN,
            vec![context_menu::Entry::hide_addon()],
            ContextMenuTarget::MyWorkshop {
                workshop_id,
                workshop_url: workshop_url::workshop_item_url(workshop_id),
                preview_url: None,
            },
        )),
    ));

    let _task = app.route_context_menu_action(context_menu::ContextMenuAction::HideAddon);

    assert!(app.state.hidden_addons.contains_workshop_id(workshop_id));
    assert!(app.state.features.my_workshop.row_for_test(123).is_none());
}

#[test]
fn update_delegates_search_messages_to_search_state() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::Search(search::Message::QueryEdited(
        "addons".to_owned(),
    )));

    assert_eq!(app.state.features.search.input(), "addons");
}

#[test]
fn addon_drag_state_preserves_click_when_released_before_drag_threshold() {
    let mut drag = AddonDragState::default();

    drag.press(
        AddonDragSource::MyWorkshop,
        CardId::from("42"),
        Some(PublishedFileId::fixture(42)),
        None,
    );
    drag.cursor_moved(Point::new(10.0, 10.0));

    assert_eq!(
        drag.release(false),
        Some(AddonDragOutcome::Click {
            source: AddonDragSource::MyWorkshop,
            card_id: CardId::from("42"),
        })
    );
    assert!(!drag.is_active());
}

#[test]
fn addon_drag_state_promotes_after_threshold_and_drops_on_target() {
    let mut drag = AddonDragState::default();

    drag.press(
        AddonDragSource::InstalledAddons,
        CardId::from("/tmp/a.gma"),
        Some(PublishedFileId::fixture(99)),
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
            workshop_id: PublishedFileId::fixture(99)
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
        CardId::from("42"),
        Some(PublishedFileId::fixture(42)),
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
        CardId::from("42"),
        Some(PublishedFileId::fixture(42)),
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
        CardId::from("/tmp/local.gma"),
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
            card_id: CardId::from("/tmp/local.gma"),
        })
    );
    assert!(!drag.is_active());
}

#[test]
fn addon_drag_update_events_promote_and_finish_drag() {
    let mut app = App::new_for_test();
    app.state.addon_drag.press(
        AddonDragSource::MyWorkshop,
        CardId::from("42"),
        Some(PublishedFileId::fixture(42)),
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
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connecting),
    ));

    assert_eq!(
        app.state.features.steam_session.status(),
        steam_session::ConnectionStatus::Connecting
    );
}

#[test]
fn backend_steam_events_update_session_and_shell_state() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::BackendEvent(
        BackendRuntimeEvent::SteamConnected,
    ));

    assert_eq!(
        app.state.features.steam_session.status(),
        steam_session::ConnectionStatus::Connected
    );
    assert!(app.state.features.shell.steam_status().connected());

    let _task = app.update(RootMessage::BackendEvent(
        BackendRuntimeEvent::SteamDisconnected,
    ));

    assert_eq!(
        app.state.features.steam_session.status(),
        steam_session::ConnectionStatus::Disconnected
    );
    assert!(!app.state.features.shell.steam_status().connected());
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
        settings_revision: 1,
        version: "test",
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
    let mut app = App::new_for_test();
    let initial_steam_status = app.state.features.steam_session.status();
    let root = tempfile::tempdir().expect("tempdir");
    app.environment
        .ctx
        .update_settings_snapshot_for_test(|settings| {
            settings.ui.play_gifs_by_default = false;
            settings.ui.download_count_format = DownloadCountFormat::Period;
            settings.ui.theme_preset = ThemePreset::ClassicSource;
        })
        .expect("ui-only settings update");
    let settings = BackendSettings {
        sounds: false,
        language: Some("en-US".to_owned()),
        ..BackendSettings::default()
    };
    let snapshot = backend_appdata_snapshot_for_test(settings, root.path());

    let task = app.update(RootMessage::BackendEvent(
        BackendRuntimeEvent::AppDataUpdated(Box::new(snapshot)),
    ));
    let messages = task_messages(task);
    assert!(matches!(
        messages.as_slice(),
        [RootMessage::AppDataSnapshotApplied(Ok(_))]
    ));
    for message in messages {
        let _task = app.update(message);
    }
    let (settings, paths) = app.environment.ctx.settings_and_paths_snapshot();

    assert!(!settings.backend.sounds);
    assert_eq!(settings.backend.language.as_deref(), Some("en-US"));
    assert_eq!(paths.temp_dir, root.path().join("temp"));
    assert!(!settings.ui.play_gifs_by_default);
    assert_eq!(
        settings.ui.download_count_format,
        DownloadCountFormat::Period
    );
    assert_eq!(settings.ui.theme_preset, ThemePreset::ClassicSource);
    assert_eq!(
        app.state.features.steam_session.status(),
        initial_steam_status
    );
}

#[test]
fn backend_appdata_events_are_serialized_and_coalesce_to_the_latest_snapshot() {
    let mut app = App::new_for_test();
    let root = tempfile::tempdir().expect("tempdir");
    let snapshot = |language: &str| {
        backend_appdata_snapshot_for_test(
            BackendSettings {
                language: Some(language.to_owned()),
                ..BackendSettings::default()
            },
            root.path(),
        )
    };

    let first = snapshot("first");
    let second = snapshot("second");
    let latest = snapshot("latest");
    let first_task = app.update(RootMessage::BackendEvent(
        BackendRuntimeEvent::AppDataUpdated(Box::new(first)),
    ));

    let task = app.update(RootMessage::BackendEvent(
        BackendRuntimeEvent::AppDataUpdated(Box::new(second)),
    ));
    assert_no_task(&task);
    let task = app.update(RootMessage::BackendEvent(
        BackendRuntimeEvent::AppDataUpdated(Box::new(latest)),
    ));
    assert_no_task(&task);
    assert_eq!(
        app.pending_appdata_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.settings.language.as_deref()),
        Some("latest")
    );

    let mut messages = task_messages(first_task);
    assert!(matches!(
        messages.as_slice(),
        [RootMessage::AppDataSnapshotApplied(Ok(_))]
    ));
    let latest_task = app.update(messages.pop().unwrap());
    assert!(app.appdata_snapshot_in_flight);
    assert!(app.pending_appdata_snapshot.is_none());

    let messages = task_messages(latest_task);
    assert!(matches!(
        messages.as_slice(),
        [RootMessage::AppDataSnapshotApplied(Ok(_))]
    ));
    for message in messages {
        let _task = app.update(message);
    }
    let (settings, _) = app.environment.ctx.settings_and_paths_snapshot();
    assert_eq!(settings.backend.language.as_deref(), Some("latest"));
    assert!(!app.appdata_snapshot_in_flight);
}

#[test]
fn download_count_format_mutation_updates_runtime_formatter() {
    let mut app = App::new_for_test();

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
    let mut app = App::new_for_test();
    app.environment.ctx.clear_search_for_test();
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
        epoch: Generation::from_raw(1),
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

    assert_eq!(app.state.features.installed_addons.row_count(), 1);
    let result = app
        .environment
        .ctx
        .quick_addon_search_for_test("root-needle");
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].item.label(), "Root Fanout Addon");

    let _task = app.update(RootMessage::SizeAnalyzer(
        size_analyzer::Message::ViewportResized(Size::new(640.0, 360.0)),
    ));
    let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
        shell::Route::SizeAnalyzer,
    )));
    assert!(
        app.state
            .features
            .size_analyzer
            .projection_key_for_test()
            .is_some()
    );
}

#[test]
fn backend_runtime_download_results_translate_to_their_consumers() {
    let snapshot = backend_runtime_action_message(BackendRuntimeAction::SnapshotDownloaded {
        request_id: crate::bridge::tasks::WorkshopSnapshotId::new(77),
        item_id: PublishedFileId::fixture(456),
        installed_path: None,
        extracted_path: PathBuf::from("/tmp/snapshot/456"),
    });
    assert!(matches!(
        snapshot,
        RootMessage::PreparePublish(prepare_publish::Message::WorkshopContentDownloaded(
            request_id,
            WorkshopDownloadSuccess { item_id, .. }
        )) if request_id == crate::bridge::tasks::WorkshopSnapshotId::new(77)
            && item_id == PublishedFileId::fixture(456)
    ));
}

#[test]
fn workshop_download_completion_starts_baseline_inspection() {
    let mut app = App::new_for_test();
    let workshop_id = PublishedFileId::fixture(456);
    let extracted_path = PathBuf::from("/tmp/prepare-publish-workshop/456-0");
    let _task = app.update(RootMessage::PreparePublish(
        prepare_publish::Message::OpenRequested {
            target: prepare_publish::OpenTarget::Update(prepare_publish::UpdateTarget {
                workshop_id,
                title: "Workshop Addon".to_owned(),
                tags: Vec::new(),
                preview_url: None,
                snapshot_request_id: crate::bridge::tasks::WorkshopSnapshotId::new(1),
                snapshot_destination: extracted_path.clone(),
            }),
            ignored_patterns: Vec::new(),
            upscale_icon_default: false,
        },
    ));

    let _task = app.update(RootMessage::PreparePublish(
        prepare_publish::Message::WorkshopContentDownloaded(
            crate::bridge::tasks::WorkshopSnapshotId::new(1),
            WorkshopDownloadSuccess {
                item_id: workshop_id,
                installed_path: None,
                extracted_path,
            },
        ),
    ));

    assert_eq!(app.state.features.prepare_publish.addon_path(), "");
    assert!(app.state.features.prepare_publish.path_pending());
}

#[test]
fn uncorrelated_backend_transaction_events_are_data_only() {
    let mut app = App::new_for_test();
    let initial_steam_status = app.state.features.steam_session.status();

    let _task = app.update(RootMessage::BackendEvent(BackendRuntimeEvent::Transaction(
        crate::bridge::tasks::TransactionRuntimeEvent::Progress {
            id: TransactionId::from_raw(42),
            progress: 5000,
        },
    )));

    assert_eq!(
        app.state.features.steam_session.status(),
        initial_steam_status
    );
}

#[test]
fn steam_backed_page_request_defers_until_connection() {
    let mut app = App::new_for_test();

    let _task = app.batch_effects(
        vec![my_workshop::Effect::PageRequested {
            generation: Generation::from_raw(7),
            page: 2,
        }],
        App::run_my_workshop_effect,
    );

    assert_eq!(
        app.state.features.steam_session.pending_retries(),
        [steam_session::PendingRetry::MyWorkshopPage {
            generation: Generation::from_raw(7),
            page: 2,
        }]
    );
    assert_eq!(
        app.state.features.steam_session.status(),
        steam_session::ConnectionStatus::Connecting
    );
}

#[test]
fn steam_backed_stats_refresh_defers_until_connection() {
    let mut app = App::new_for_test();

    let _task = app.batch_effects(
        vec![my_workshop::Effect::StatsRefreshRequested {
            generation: Generation::from_raw(7),
            pages: 2,
        }],
        App::run_my_workshop_effect,
    );

    assert_eq!(
        app.state.features.steam_session.pending_retries(),
        [steam_session::PendingRetry::MyWorkshopStats {
            generation: Generation::from_raw(7),
            pages: 2,
        }]
    );
    assert_eq!(
        app.state.features.steam_session.status(),
        steam_session::ConnectionStatus::Connecting
    );
}

#[test]
fn failed_steam_connection_clears_pending_retry() {
    let mut app = App::new_for_test();
    let _task = app.batch_effects(
        vec![my_workshop::Effect::PageRequested {
            generation: Generation::from_raw(7),
            page: 2,
        }],
        App::run_my_workshop_effect,
    );

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionAttemptCompleted(
            steam_session::ConnectionAttempt::unavailable(UiError::detailed(
                gmpublished_backend::error_keys::STEAM_ERROR,
                Some("steam unavailable".to_owned()),
            )),
        ),
    ));

    assert!(
        app.state
            .features
            .steam_session
            .pending_retries()
            .is_empty()
    );
    assert_eq!(
        app.state.features.steam_session.status(),
        steam_session::ConnectionStatus::Unavailable
    );
}

#[test]
fn successful_steam_connection_retries_pending_operation_once() {
    let mut app = App::new_for_test();
    let _task = app.batch_effects(
        vec![my_workshop::Effect::PageRequested {
            generation: Generation::from_raw(7),
            page: 2,
        }],
        App::run_my_workshop_effect,
    );

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionAttemptCompleted(
            steam_session::connect_for_operation_with(|| false, || Ok::<_, UiError>(())),
        ),
    ));

    assert!(
        app.state
            .features
            .steam_session
            .pending_retries()
            .is_empty()
    );
    assert_eq!(
        app.state.features.steam_session.status(),
        steam_session::ConnectionStatus::Connected
    );
    assert!(app.state.features.shell.steam_status().connected());

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionAttemptCompleted(
            steam_session::connect_for_operation_with(|| true, || Ok::<_, UiError>(())),
        ),
    ));

    assert!(
        app.state
            .features
            .steam_session
            .pending_retries()
            .is_empty()
    );
}

#[test]
fn connected_steam_attempt_retries_pending_operation_without_edge() {
    let mut app = App::new_for_test();
    let retry = steam_session::PendingRetry::MyWorkshopPage {
        generation: Generation::from_raw(7),
        page: 2,
    };

    let _task = steam_session::update(
        &mut app.state.features.steam_session,
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connected),
    );
    let _task = steam_session::update(
        &mut app.state.features.steam_session,
        steam_session::Message::PendingRetrySet(retry.clone()),
    );

    assert_eq!(
        app.state.features.steam_session.status(),
        steam_session::ConnectionStatus::Connected
    );
    assert_eq!(app.state.features.steam_session.pending_retries(), [retry]);

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionAttemptCompleted(
            steam_session::connect_for_operation_with(|| false, || Ok::<_, UiError>(())),
        ),
    ));

    assert!(
        app.state
            .features
            .steam_session
            .pending_retries()
            .is_empty()
    );
    assert_eq!(
        app.state.features.steam_session.status(),
        steam_session::ConnectionStatus::Connected
    );
}

#[test]
fn steam_identity_fetch_completion_updates_shell_identity() {
    let mut app = App::new_for_test();
    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connected),
    ));

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::IdentityFetched(Generation::from_raw(1), Ok(steam_identity("Ada"))),
    ));

    assert_eq!(app.state.features.shell.account_name(), Some("Ada"));
    assert!(app.state.features.shell.steam_avatar().is_some());
}

#[test]
fn steam_identity_fetch_failure_restores_anonymous_shell_identity() {
    let mut app = App::new_for_test();
    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connected),
    ));
    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::IdentityFetched(Generation::from_raw(1), Ok(steam_identity("Ada"))),
    ));

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::IdentityFetched(
            Generation::from_raw(1),
            Err(UiError::detailed(
                gmpublished_backend::error_keys::STEAM_ERROR,
                Some("steam unavailable".to_owned()),
            )),
        ),
    ));

    assert_eq!(app.state.features.shell.account_name(), None);
    assert!(app.state.features.shell.steam_avatar().is_none());
}

#[test]
fn shell_executor_navigated_enters_destination_and_exits_previous_route() {
    let mut app = App::new_for_test();

    let _task = app.batch_effects(
        vec![shell::Effect::Navigated {
            from: shell::Route::MyWorkshop,
            to: shell::Route::InstalledAddons,
        }],
        App::run_shell_effect,
    );

    assert!(!app.state.features.my_workshop.is_route_visible());
    assert!(app.state.features.installed_addons.is_route_visible());
}

#[test]
fn shell_executor_navigated_runs_size_analyzer_enter_and_exit_effects() {
    let mut app = App::new_for_test();
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
        epoch: Generation::from_raw(1),
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
    assert!(!app.state.features.size_analyzer.is_route_visible());
    assert!(
        app.state
            .features
            .size_analyzer
            .projection_key_for_test()
            .is_none()
    );

    let enter_task = app.batch_effects(
        vec![shell::Effect::Navigated {
            from: shell::Route::MyWorkshop,
            to: shell::Route::SizeAnalyzer,
        }],
        App::run_shell_effect,
    );

    assert!(app.state.features.size_analyzer.is_route_visible());
    assert!(
        app.state
            .features
            .size_analyzer
            .projection_key_for_test()
            .is_some()
    );
    assert_task_scheduled(&enter_task);

    let _exit_task = app.batch_effects(
        vec![shell::Effect::Navigated {
            from: shell::Route::SizeAnalyzer,
            to: shell::Route::Downloader,
        }],
        App::run_shell_effect,
    );

    assert!(!app.state.features.size_analyzer.is_route_visible());
}

#[test]
fn shell_navigation_to_current_route_does_not_reenter_route() {
    let mut app = App::new_for_test();
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
        epoch: Generation::from_raw(1),
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
    assert!(
        app.state
            .features
            .size_analyzer
            .projection_key_for_test()
            .is_none()
    );

    let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
        shell::Route::SizeAnalyzer,
    )));
    assert_eq!(app.state.features.shell.route(), shell::Route::SizeAnalyzer);
    let entered_projection = app.state.features.size_analyzer.projection_key_for_test();
    assert!(entered_projection.is_some());

    let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
        shell::Route::SizeAnalyzer,
    )));

    assert_eq!(
        app.state.features.size_analyzer.projection_key_for_test(),
        entered_projection
    );
}

#[test]
fn update_check_completion_marks_update_nag_available() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::UpdateCheckCompleted(
        super::UpdateCheckOutcome::ReleaseAvailable(shell::UpdateRelease::new(
            "v0.1.1".to_owned(),
            "https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1".to_owned(),
        )),
    ));

    assert!(app.state.features.shell.update_available());
    assert_eq!(app.state.features.shell.update_version(), "v0.1.1");
    assert_eq!(
        app.state.features.shell.update_release_url(),
        "https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1"
    );
}

#[test]
fn downloader_start_events_sync_shell_job_badge() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::Downloader(
        downloader::Message::TaskEventsReceived(vec![crate::test_support::workshop_task_started(
            WorkshopDownloadTaskKind::Download,
            PublishedFileId::fixture(123),
            TaskId::from_raw(7),
        )]),
    ));

    let badge = app
        .state
        .features
        .shell
        .downloader_badge(Instant::now())
        .expect("running Downloader job should project a shell badge");
    assert_eq!(badge.count, 1);
}

#[test]
fn language_switch_rebuilds_the_runtime_bundle() {
    let mut app = App::new_for_test();

    app.state.apply_runtime_language(Some("fr-CA"));

    assert_eq!(app.state.i18n.locale_id(), "fr");
    assert_eq!(
        app.state.i18n.trn(
            "my-workshop-count",
            &[("loaded", Arg::Number("2")), ("total", Arg::Number("8"))]
        ),
        "Affichage de 2 sur 8 addons"
    );
    assert_eq!(
        app.state.features.my_workshop.publish_new_title_for_test(),
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

fn file_preview_request() -> preview_model::PreviewRequest {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Preview")
            .entry("lua/autorun/init.lua", b"print('ok')\n".to_vec())
            .build(),
    )
    .expect("fixture archive should load");
    preview_model::PreviewRequest {
        cancellation: Default::default(),
        request_id: Generation::from_raw(1),
        archive: PreviewArchiveSource::from_gma(Arc::new(archive)),
        entry_path: "lua/autorun/init.lua".to_owned(),
        display_name: "init.lua".to_owned(),
        size_bytes: 12,
        crc32: 0x1234_5678,
        bypass_size_limits: false,
        map_texture_quality: Default::default(),
    }
}

#[test]
fn model_workspace_preserves_selection_without_replacing_archive_previews() {
    use crate::features::model_manager;
    use gmpublished_domain::math::Vec3;

    let mut app = App::new_for_test();
    let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
        shell::Route::Models,
    )));
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::SourceDropped("test.smd".into()),
    );
    app.state
        .features
        .model_manager
        .set_workspace_selection(1, vec![2]);
    let mut request = file_preview_request();
    request.entry_path = "models/test.mdl".into();
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::PreviewRequested,
    );
    let _task = app.update(RootMessage::ModelPreview(
        file_preview::Message::WorkspaceOpenRequested(request),
    ));
    let request = app.state.features.model_preview.request().unwrap().clone();
    let data = preview_model::PreviewData::from_request(
        &request,
        preview_model::PreviewContent::Model(Arc::new(preview_model::ModelPreview {
            model_animations: Vec::new(),
            model_animation_issues: Vec::new(),
            physics_hulls: Default::default(),
            pose_cache: Default::default(),
            root_lod: 0,
            selected_lod: 0,
            lods: vec![],
            animation: None,
            scene: Arc::new(preview_model::RenderScene {
                meshes: vec![],
                materials: vec![],
                phy_debug_meshes: vec![],
                stats: preview_model::ModelStats {
                    bone_count: 1,
                    sequence_count: 0,
                    vertex_count: 0,
                    triangle_count: 0,
                    mesh_count: 0,
                    material_count: 0,
                    resolved_material_count: 0,
                },
                bounds_min: Vec3::splat(0.0),
                bounds_max: Vec3::splat(1.0),
            }),
            skin_tables: vec![vec![0], vec![0]],
            bodygroups: vec![3],
        })),
    );
    let _task = app.update(RootMessage::ModelPreview(file_preview::Message::Loaded(
        request.request_id,
        Box::new(Ok(data.clone())),
    )));
    assert_eq!(app.state.features.model_preview.selected_skin(), 1);
    assert_eq!(app.state.features.model_preview.bodygroup_choices(), &[2]);
    let _task = app.update(RootMessage::ModelPreview(
        file_preview::Message::SkinSelected(0),
    ));
    let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
        shell::Route::Downloader,
    )));
    assert!(app.state.features.model_preview.is_open());
    assert_eq!(
        app.state.features.model_manager.preview_selection(),
        (0, vec![2])
    );
    let _task = app
        .apply_file_preview_message(file_preview::Message::OpenRequested(file_preview_request()));
    assert!(app.state.features.file_preview.is_open());
    let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
        shell::Route::Models,
    )));

    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::PreviewRequested,
    );
    let _task = app.update(RootMessage::ModelPreview(
        file_preview::Message::WorkspaceOpenRequested(request),
    ));
    let request = app.state.features.model_preview.request().unwrap().clone();
    let _task = app.update(RootMessage::ModelPreview(file_preview::Message::Loaded(
        request.request_id,
        Box::new(Ok(data)),
    )));
    assert_eq!(app.state.features.model_preview.bodygroup_choices(), &[2]);
    let _task = app.handle_file_drop("replacement.smd".into());
    assert!(!app.state.features.model_preview.is_open());
    assert!(app.state.features.file_preview.is_open());
    assert_eq!(
        app.state.features.model_manager.preview_selection(),
        (0, vec![])
    );
}

#[test]
fn live_skin_survives_playback_and_route_changes_until_another_skin_is_selected() {
    use crate::features::model_manager;
    use gmpublished_backend::model_compile::ModelInspection;

    let mut app = App::new_for_test();
    let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
        shell::Route::Models,
    )));
    let mut model = crate::model_animation_test_support::animated_model();
    Arc::make_mut(&mut model).bodygroups = vec![2];
    let material = model.scene.materials[0].name.clone();
    let effects = model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::SourceDropped("animated.smd".into()),
    );
    let [model_manager::Effect::Inspect(generation, _)] = effects.as_slice() else {
        panic!("source inspection");
    };
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::Inspected(
            *generation,
            Ok(Box::new(ModelInspection {
                source_model: None,
                collision_parts: vec![],
                collision_physics: None,
                parts: vec![],
                animations: vec![],
                materials: vec![material.clone()],
                material_directories: vec![],
                skins: vec![vec![material.clone()]],
            })),
        ),
    );
    let _task = app.apply_model_preview_message(file_preview::Message::WorkspaceOpenRequested(
        file_preview_request(),
    ));
    let request = app.state.features.model_preview.request().unwrap().clone();
    let data = preview_model::PreviewData::from_request(
        &request,
        preview_model::PreviewContent::Model(model),
    );
    let _task = app.apply_model_preview_message(file_preview::Message::Loaded(
        request.request_id,
        Box::new(Ok(data)),
    ));
    let _task = app.apply_model_manager_message(model_manager::Message::SkinAdded("Paint".into()));
    let _task = app.apply_model_manager_message(model_manager::Message::MaterialSelected(material));
    let _task = app.apply_model_manager_message(model_manager::Message::MaterialTintChanged(
        "#123456".into(),
    ));
    assert!(!app.state.features.model_manager.preview_busy());
    let tints = app
        .state
        .features
        .model_preview
        .workspace_material_tints()
        .to_vec();
    assert_eq!(tints.len(), 1);
    let _task = app.apply_model_manager_message(model_manager::Message::Viewer(
        file_preview::Message::BodygroupChoiceSelected {
            group: 0,
            choice: 1,
        },
    ));
    assert_eq!(
        app.state.features.model_manager.preview_selection(),
        (1, vec![1])
    );
    assert_eq!(
        app.state.features.model_preview.workspace_material_tints(),
        tints
    );

    let _task = app.apply_model_manager_message(model_manager::Message::Viewer(
        file_preview::Message::ModelPlaybackToggled,
    ));
    assert_eq!(app.state.features.model_manager.preview_selection().0, 1);
    assert_eq!(
        app.state.features.model_preview.workspace_material_tints(),
        tints
    );
    for route in [shell::Route::Downloader, shell::Route::Models] {
        let _task = app.update(RootMessage::Shell(shell::Message::Navigate(route)));
        assert_eq!(app.state.features.model_manager.preview_selection().0, 1);
        assert_eq!(
            app.state.features.model_preview.workspace_material_tints(),
            tints
        );
    }
    let _task = app.apply_model_manager_message(model_manager::Message::Viewer(
        file_preview::Message::SkinSelected(0),
    ));
    assert_eq!(app.state.features.model_manager.preview_selection().0, 0);
    assert!(
        app.state
            .features
            .model_preview
            .workspace_material_tints()
            .is_empty()
    );
}

#[test]
fn importing_a_source_prepares_the_workspace_before_choosing_an_export_folder() {
    use crate::features::model_manager;
    use gmpublished_backend::model_compile::ModelInspection;
    let mut app = App::new_for_test();
    let effects = model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::SourceDropped("model.glb".into()),
    );
    let [model_manager::Effect::Inspect(generation, _)] = effects.as_slice() else {
        panic!("inspection");
    };
    let _task = app.update(RootMessage::ModelManager(
        model_manager::Message::Inspected(
            *generation,
            Ok(Box::new(ModelInspection {
                source_model: None,
                collision_physics: None,
                collision_parts: vec![],
                parts: vec![],
                animations: vec![],
                materials: vec!["paint".into()],
                material_directories: vec![],
                skins: vec![],
            })),
        ),
    ));
    assert!(app.state.features.model_manager.preview_busy());
    assert!(!app.state.features.model_manager.busy());
    assert!(!app.state.features.model_manager.can_export());
    let _task = app.apply_model_manager_message(model_manager::Message::RouteExited);
    assert!(app.state.features.model_manager.preview_busy());
    assert!(!app.state.features.model_manager.busy());
    let _task = app.apply_model_manager_message(model_manager::Message::PreviewFailed(
        "invalid model".into(),
    ));
    assert!(!app.state.features.model_manager.busy());
    assert!(!app.state.features.model_manager.preview_busy());
}

#[test]
fn modeller_async_source_switch_retires_cancelled_decode_without_a_callback() {
    use crate::features::model_manager;
    let mut app = App::new_for_test();
    let _task =
        app.apply_model_manager_message(model_manager::Message::SourceDropped("old.smd".into()));
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::PreviewRequested,
    );
    let decode = app.apply_model_preview_message(file_preview::Message::WorkspaceOpenRequested(
        file_preview_request(),
    ));
    let request = app.state.features.model_preview.request().unwrap().clone();
    let _task =
        app.apply_model_manager_message(model_manager::Message::SourceDropped("new.smd".into()));
    assert!(request.cancellation.is_cancelled());
    assert!(
        task_messages(decode).is_empty(),
        "cancelled worker must stay silent"
    );
    assert!(
        !app.state.features.model_manager.preview_busy(),
        "source replacement must retire the cancelled decoder's manager job"
    );
    assert_eq!(
        app.state.features.model_manager.presentation_request(),
        None
    );
    assert!(!app.state.features.model_preview.is_open());

    let effects = model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::PreviewRequested,
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, model_manager::Effect::BuildPreview(..))),
        "replacement source must be able to start its preview"
    );
    let _task = app.apply_model_preview_message(file_preview::Message::Loaded(
        request.request_id,
        Box::new(Err(preview_model::PreviewLoadError::Decode(
            "late old completion".into(),
        ))),
    ));
    assert!(
        app.state.features.model_manager.preview_busy(),
        "late old callbacks must not finish the new source's job"
    );
    assert_eq!(
        app.state.features.model_manager.source_path(),
        Some(Path::new("new.smd"))
    );
}

fn installed_preview_target() -> installed_addons::PreviewTarget {
    installed_addons::PreviewTarget {
        row_id: CardId::from("/tmp/installed-preview.gma"),
        path: PathBuf::from("/tmp/installed-preview.gma"),
        title: "Installed Preview".to_owned(),
        workshop_id: Some(PublishedFileId::fixture(123)),
        preview_url: Some("https://example.invalid/installed.jpg".to_owned()),
        subscription_count: 1_234,
        score_bucket: 4,
        score_label: "95%".to_owned(),
    }
}

fn seed_search_request(app: &mut App) -> SearchQuickRequest {
    app.state
        .features
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
    );
    assert!(
        app.state
            .features
            .search
            .apply_quick_result(request.key(), Ok(batch))
    );
    app.state.viewport_size = Size::new(800.0, 600.0);
    let (generation, ids) = app
        .state
        .features
        .search
        .take_thumbnail_metadata_request(600.0)
        .expect("search thumbnail metadata request");
    assert!(app.state.features.search.apply_metadata_refresh(
        generation,
        &ids,
        Ok(vec![search::MetadataPatch::for_test(
            PublishedFileId::fixture(42),
            Some("https://example.test/alpha.png")
        )]),
    ));
    request
}

fn search_hit(label: &str, workshop_id: u64) -> SearchHit {
    SearchHit {
        score: 1,
        item: SearchItem::new(
            SearchItemSource::WorkshopItem(PublishedFileId::fixture(workshop_id)),
            label,
        ),
    }
}

fn steam_identity(name: &str) -> steam_session::SteamIdentity {
    steam_session::SteamIdentity::from_user(SteamUser {
        steamid: SteamId::new(76561198000000001),
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
            entries: Arc::from([]),
        },
    }
}

#[test]
fn a_reconnect_reloads_a_workshop_route_that_failed_while_steam_was_down() {
    let mut app = App::new_for_test();
    assert_eq!(app.state.features.shell.route(), shell::Route::MyWorkshop);

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Unavailable),
    ));
    let _task = app.update(RootMessage::MyWorkshop(my_workshop::Message::RouteEntered));
    let _task = app.update(RootMessage::MyWorkshop(
        my_workshop::Message::PageCompleted(
            Generation::from_raw(1),
            1,
            Err(UiError::detailed(
                gmpublished_backend::error_keys::STEAM_ERROR,
                Some("STEAM_UNAVAILABLE".to_owned()),
            )),
        ),
    ));
    assert!(matches!(
        app.state.features.my_workshop.page_status(),
        my_workshop::PageStatus::Failed(_)
    ));

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connected),
    ));

    assert!(
        !matches!(
            app.state.features.my_workshop.page_status(),
            my_workshop::PageStatus::Failed(_)
        ),
        "the stale Steam failure survived the reconnect that resolved it"
    );
}

#[test]
fn a_repeated_connected_message_does_not_re_enter_the_route() {
    let mut app = App::new_for_test();
    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connected),
    ));

    assert!(
        !app.state
            .features
            .prerequisites
            .observe_steam(steam_session::ConnectionStatus::Connected),
        "a repeated Connected reported a fresh reconnect edge"
    );
}

#[test]
fn a_reconnect_releases_installed_addon_metadata_parked_by_a_steam_outage() {
    let mut app = App::new_for_test();

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Unavailable),
    ));
    let _task = app.update(RootMessage::InstalledAddons(
        installed_addons::Message::RouteEntered,
    ));
    let workshop_id = PublishedFileId::fixture(1);
    // Advance the metadata generation; retries are parked by generation, not by row.
    let _task = app.update(RootMessage::InstalledAddons(
        installed_addons::Message::SnapshotPushed(LibraryRefreshReason::Startup, Ok(Vec::new())),
    ));

    let _task = app.update(RootMessage::InstalledAddons(
        installed_addons::Message::MetadataCompleted(
            Generation::from_raw(1),
            vec![workshop_id],
            Err(UiError::detailed(
                gmpublished_backend::error_keys::STEAM_ERROR,
                Some("STEAM_UNAVAILABLE".to_owned()),
            )),
        ),
    ));
    assert!(
        app.state.features.installed_addons.has_failed_metadata(),
        "the refused lookup should park its id for a retry"
    );

    let _task = app.update(RootMessage::SteamSession(
        steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connected),
    ));

    assert!(
        !app.state.features.installed_addons.has_failed_metadata(),
        "the reconnect that resolved the outage left the metadata parked"
    );
}

#[test]
fn root_message_stays_small_enough_for_the_hot_path() {
    const MAX_ROOT_MESSAGE_BYTES: usize = 160;

    let actual = std::mem::size_of::<RootMessage>();
    assert!(
        actual <= MAX_ROOT_MESSAGE_BYTES,
        "RootMessage grew to {actual} bytes (limit {MAX_ROOT_MESSAGE_BYTES}); \
         box the payload of whichever variant grew rather than raising this bound"
    );
}

#[test]
fn selected_interface_language_updates_workshop_query_language_at_runtime() {
    use gmpublished_backend::WorkshopLanguage;
    let mut app = App::new_for_test();
    let _task = app
        .apply_settings_mutation_runtime(&settings::SettingsMutation::Language(Some("fr".into())));
    assert_eq!(app.state.i18n.locale_id(), "fr");
    assert!(
        !app.environment
            .ctx
            .set_workshop_language(WorkshopLanguage::French)
    );
    let _task = app
        .apply_settings_mutation_runtime(&settings::SettingsMutation::Language(Some("kr".into())));
    assert!(
        !app.environment
            .ctx
            .set_workshop_language(WorkshopLanguage::Korean)
    );
    let _task = app
        .apply_settings_mutation_runtime(&settings::SettingsMutation::Language(Some("en".into())));
    assert!(
        !app.environment
            .ctx
            .set_workshop_language(WorkshopLanguage::English)
    );
}

#[test]
fn captured_map_escape_releases_mouse_without_requesting_modal_close() {
    let message = super::map_mouse_capture_shortcut(
        &keyboard::Key::Named(keyboard::key::Named::Escape),
        keyboard::Modifiers::default(),
    );
    assert!(matches!(
        message,
        Some(RootMessage::FilePreview(
            file_preview::Message::MouseCaptureRequested(false)
        ))
    ));
    assert!(
        super::map_mouse_capture_shortcut(
            &keyboard::Key::Character("v".into()),
            keyboard::Modifiers::default()
        )
        .is_none()
    );
}

fn capture_map_preview(app: &mut App, workspace: bool) {
    let state = if workspace {
        &mut app.state.features.model_preview
    } else {
        &mut app.state.features.file_preview
    };
    let mut request = file_preview_request();
    request.entry_path = "maps/capture.bsp".into();
    let request = state.begin_open(request);
    let scene = preview_model::MapPreview {
        model_animations: Vec::new(),
        model_animation_issues: Vec::new(),
        texture_preparation: Default::default(),
        prop_geometry: Default::default(),
        renderables: Vec::new(),
        map_skybox_renderables: Vec::new(),
        shading_mode: crate::media::preview_model::ShadingMode::Scene,
        surface_receivers: Vec::new(),
        surface_lightmaps: None,
        displacement_batches: Default::default(),
        water_volumes: None,
        entity_meshes: Vec::new(),
        skybox_entity_meshes: Vec::new(),
        scene: Arc::clone(&crate::model_animation_test_support::animated_model().scene),
        mesh_visibility: Vec::new(),
        map_skybox_meshes: Vec::new(),
        model_lighting: Vec::new(),
        model_lighting_inputs: None,
        lightmap: None,
        skybox: None,
        suns: Vec::new(),
        cubemap_probes: Vec::new(),
        sun_material_slots: Vec::new(),
        detail_sprites: Vec::new(),
        map_skybox_detail_sprites: Vec::new(),
        overlays: Vec::new(),
        map_skybox_overlays: Vec::new(),
        doors: Vec::new(),
        visibility: None,
        walk_collision: None,
    };
    let data = preview_model::PreviewData::from_request(
        &request,
        preview_model::PreviewContent::Map {
            scene: Arc::new(scene),
            fog: None,
            sky_camera: None,
            spawn: None,
            stats: preview_model::MapStats {
                lightmap: Default::default(),
                face_count: 0,
                displacement_count: 0,
                entity_count: 0,
                material_count: 0,
                resolved_material_count: 0,
                static_prop_count: 0,
                placed_prop_count: 0,
                skipped_prop_count: 0,
                detail_sprite_count: 0,
                invalid_detail_leaves: 0,
                overlay_count: 0,
                skybox_face_count: 0,
                skybox_prop_count: 0,
                skybox_detail_sprite_count: 0,
                skybox_overlay_count: 0,
                cluster_count: 0,
                version: 20,
            },
        },
    );
    assert!(state.apply_loaded(request.request_id, Ok(data)));
    let _effects = file_preview::update(state, file_preview::Message::MouseCaptureRequested(true));
    assert!(state.mouse_captured());
}

#[test]
fn capture_shortcuts_release_the_preview_that_owns_the_mouse() {
    for workspace in [false, true] {
        let mut app = App::new_for_test();
        capture_map_preview(&mut app, workspace);
        assert!(!app.global_shortcuts_enabled());
        let mapper = app.mouse_capture_shortcut_mapper().unwrap();
        assert!(
            mapper(
                &keyboard::Key::Character("v".into()),
                keyboard::Modifiers::default()
            )
            .is_none()
        );
        let release = mapper(
            &keyboard::Key::Named(keyboard::key::Named::Escape),
            keyboard::Modifiers::default(),
        )
        .unwrap();
        assert!(match &release {
            RootMessage::ModelPreview(file_preview::Message::MouseCaptureRequested(false)) =>
                workspace,
            RootMessage::FilePreview(file_preview::Message::MouseCaptureRequested(false)) =>
                !workspace,
            _ => false,
        });

        let _task = app.update(release);
        assert!(!app.preview_mouse_captured());
        assert!(app.mouse_capture_shortcut_mapper().is_none());
        assert!(app.state.features.modal_stack.active().is_none());
    }
}

#[test]
fn losing_window_focus_releases_both_preview_capture_states() {
    let mut app = App::new_for_test();
    capture_map_preview(&mut app, false);
    capture_map_preview(&mut app, true);

    let _task = app.update(RootMessage::WindowEvent(
        iced::window::Id::unique(),
        iced::window::Event::Unfocused,
    ));

    assert!(!app.state.features.file_preview.mouse_captured());
    assert!(!app.state.features.model_preview.mouse_captured());
}

#[test]
fn leaving_models_or_opening_a_modal_releases_workspace_capture() {
    for open_modal in [false, true] {
        let mut app = App::new_for_test();
        let _task = app.update(RootMessage::Shell(shell::Message::Navigate(
            shell::Route::Models,
        )));
        capture_map_preview(&mut app, true);

        let _task = if open_modal {
            app.open_modal_stack_task(modal_stack::ActiveModal::Settings)
        } else {
            app.update(RootMessage::Shell(shell::Message::Navigate(
                shell::Route::Downloader,
            )))
        };

        assert!(!app.preview_mouse_captured());
        assert!(app.state.features.model_preview.is_open());
    }
}

fn staged_modeller_preview(app: &mut App) {
    use crate::features::model_manager;
    use gmpublished_domain::math::Vec3;
    // Give the manager a displayed compile baseline so live transforms are meaningful.
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::SourceDropped("staged.smd".into()),
    );
    let effects = model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::PreviewRequested,
    );
    let generation = effects
        .iter()
        .find_map(|effect| match effect {
            model_manager::Effect::BuildPreview(generation, ..) => Some(*generation),
            _ => None,
        })
        .unwrap();
    let directory = Arc::new(tempfile::tempdir().unwrap());
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::PreviewBuilt(
            generation,
            Ok(model_manager::PreviewBuild {
                result: model_manager::ExportResult {
                    directory: directory.path().into(),
                    files: vec![],
                    bytes: 0,
                    warnings: vec![],
                    compiled: true,
                },
                _directory: Some(directory),
            }),
        ),
    );
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::PreviewPresented,
    );
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::ScaleChanged("2".into()),
    );
    capture_map_preview(app, true);
    let state = &mut app.state.features.model_preview;
    let data = state.current().unwrap().clone();
    let map = state.request().unwrap().clone();
    let mut model_request = file_preview_request();
    model_request.entry_path = "models/staged.mdl".into();
    let (_, map) = state.begin_workspace_stage(model_request, map);
    let mut model = crate::model_animation_test_support::animated_model();
    Arc::make_mut(&mut model).skin_tables = vec![vec![0], vec![0]];
    Arc::make_mut(&mut model).bodygroups = vec![2];
    assert!(state.apply_stage_loaded(map.request_id, Ok((data, model))));
    for message in [
        file_preview::Message::SkinSelected(1),
        file_preview::Message::BodygroupChoiceSelected {
            group: 0,
            choice: 1,
        },
        file_preview::Message::StageTransformChanged({
            let mut transform = state.stage_transform().unwrap();
            transform.position = [32.0, 64.0, 128.0].into();
            transform.angles = [0.0, 90.0, 0.0].into();
            transform
        }),
        file_preview::Message::MapFogToggled(false),
    ] {
        let _task = app.apply_model_preview_message(message);
    }
    app.sync_model_workspace();
    assert_eq!(
        app.state.features.model_preview.workspace_transform()[0][0],
        2.0
    );
    assert_eq!(
        app.state
            .features
            .model_preview
            .stage_transform()
            .unwrap()
            .position,
        Vec3::new(32.0, 64.0, 128.0)
    );
}

#[test]
fn modeller_async_quality_reload_preserves_staged_model_selection_and_transform() {
    for success in [false, true] {
        let mut app = App::new_for_test();
        staged_modeller_preview(&mut app);
        let model = Arc::clone(app.state.features.model_preview.stage_model().unwrap());
        let placement = app.state.features.model_preview.stage_transform();
        let live = app.state.features.model_preview.workspace_transform();
        let original = app.state.features.model_preview.current().unwrap().clone();
        let _task = app.apply_model_preview_message(
            file_preview::Message::MapReducedTextureDetailToggled(true),
        );
        let generation = app
            .state
            .features
            .model_manager
            .presentation_request()
            .unwrap();
        let result = if success {
            Ok(original.clone())
        } else {
            Err(preview_model::PreviewLoadError::Decode(
                "reload failed".into(),
            ))
        };
        let _task = app.apply_model_preview_message(file_preview::Message::Loaded(
            generation,
            Box::new(result),
        ));
        assert!(
            app.state.features.model_preview.current().is_some(),
            "quality reload must preserve the scene on success and failure"
        );
        assert!(Arc::ptr_eq(
            app.state.features.model_preview.stage_model().unwrap(),
            &model
        ));
        assert_eq!(
            app.state.features.model_preview.stage_transform(),
            placement
        );
        assert_eq!(app.state.features.model_preview.workspace_transform(), live);
        assert_eq!(app.state.features.model_preview.selected_skin(), 1);
        assert_eq!(app.state.features.model_preview.bodygroup_choices(), &[1]);
        assert!(!app.state.features.model_preview.map_fog_enabled());
        assert!(!app.state.features.model_manager.preview_busy());
        if !success {
            assert!(app.state.features.model_preview.error().is_some());
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                Size::new(1000.0, 800.0),
                crate::features::model_manager::workspace_view(
                    &app.state.features.model_manager,
                    &app.state.features.model_preview,
                    crate::theme::ViewCtx::new(&app.state.tokens, &app.state.i18n),
                ),
            );
            ui.find("reload failed")
                .expect("retained scene must not hide reload errors");
            drop(ui);
            let _retry = app.apply_model_preview_message(
                file_preview::Message::MapReducedTextureDetailToggled(true),
            );
            assert!(app.state.features.model_preview.error().is_none());
            assert!(app.state.features.model_preview.stage_model().is_some());
        }
    }
}

#[test]
fn modeller_async_quality_reload_does_not_finish_a_newer_manager_build() {
    use crate::features::model_manager;
    let mut app = App::new_for_test();
    staged_modeller_preview(&mut app);
    let original = app.state.features.model_preview.current().unwrap().clone();
    let _task = app
        .apply_model_preview_message(file_preview::Message::MapReducedTextureDetailToggled(true));
    let generation = app
        .state
        .features
        .model_manager
        .presentation_request()
        .unwrap();
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::AutomaticAxesChanged(false),
    );
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::PreviewRequested,
    );
    assert!(app.state.features.model_manager.preview_busy());
    let _task = app.apply_model_preview_message(file_preview::Message::Loaded(
        generation,
        Box::new(Ok(original)),
    ));
    assert!(
        app.state.features.model_manager.preview_busy(),
        "independent map reload must not complete a model compile"
    );
    assert!(app.state.features.model_manager.preview_dirty());
    assert!(app.state.features.model_preview.stage_model().is_some());
}

fn modeller_workflow_source(directory: &Path, name: &str) -> PathBuf {
    let source = directory.join(format!("{name}.smd"));
    std::fs::write(
        &source,
        include_bytes!("../../../vendor/mdlc/tests/fixtures/cube.smd"),
    )
    .unwrap();
    ::image::RgbaImage::from_pixel(2, 2, ::image::Rgba([80, 140, 200, 255]))
        .save(directory.join("material.png"))
        .unwrap();
    source
}

fn modeller_workflow_drain(app: &mut App, initial: Task<RootMessage>) {
    let mut pending = vec![initial];
    let mut steps = 0;
    while let Some(task) = pending.pop() {
        for message in task_messages(task) {
            steps += 1;
            assert!(steps < 100, "model workflow did not settle");
            pending.push(app.update(message));
        }
    }
}

fn modeller_workflow_until_decode(app: &mut App, source: PathBuf) -> Task<RootMessage> {
    use crate::features::model_manager;
    let mut task = app.apply_model_manager_message(model_manager::Message::SourceDropped(source));
    for _ in 0..10 {
        if app
            .state
            .features
            .model_manager
            .presentation_request()
            .is_some()
        {
            return task;
        }
        let messages = task_messages(task);
        assert!(
            !messages.is_empty(),
            "preview workflow stopped before decode"
        );
        task = Task::batch(messages.into_iter().map(|message| app.update(message)));
    }
    panic!("preview workflow did not start its decoder");
}

#[test]
fn modeller_async_real_source_switch_finishes_replacement_preview_and_export() {
    use crate::features::model_manager;
    let root = tempfile::tempdir().unwrap();
    let first = modeller_workflow_source(root.path(), "first");
    let second = modeller_workflow_source(root.path(), "second");
    let mut app = App::new_for_test();
    let old_decode = modeller_workflow_until_decode(&mut app, first);
    let old_request = app.state.features.model_preview.request().unwrap().clone();
    let replacement =
        app.apply_model_manager_message(model_manager::Message::SourceDropped(second.clone()));
    assert!(old_request.cancellation.is_cancelled());
    assert!(task_messages(old_decode).is_empty());
    modeller_workflow_drain(&mut app, replacement);
    assert!(!app.state.features.model_manager.preview_busy());
    assert_eq!(
        app.state.features.model_manager.source_path(),
        Some(second.as_path())
    );
    assert!(
        matches!(&app.state.features.model_preview.current().unwrap().content, preview_model::PreviewContent::Model(model) if model.scene.stats.triangle_count == 12)
    );

    let effects = model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::DestinationRequested,
    );
    let generation = effects
        .iter()
        .find_map(|effect| match effect {
            model_manager::Effect::PickDestination(generation, _) => Some(*generation),
            _ => None,
        })
        .unwrap();
    let destination = root.path().join("exported");
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::DestinationSelected(generation, Some(destination.clone())),
    );
    assert!(app.state.features.model_manager.can_export());
    let export = app.apply_model_manager_message(model_manager::Message::ExportRequested);
    modeller_workflow_drain(&mut app, export);
    assert!(destination.join("models/second.mdl").is_file());
    assert!(!app.state.features.model_manager.busy());
    let _task = app.apply_model_preview_message(file_preview::Message::Loaded(
        old_request.request_id,
        Box::new(Err(preview_model::PreviewLoadError::Decode(
            "late cancelled result".into(),
        ))),
    ));
    assert!(
        matches!(&app.state.features.model_preview.current().unwrap().content, preview_model::PreviewContent::Model(model) if model.scene.stats.triangle_count == 12)
    );
}

#[test]
fn modeller_async_texture_choice_waits_for_owned_stage_decode() {
    use crate::features::model_manager;
    let mut app = App::new_for_test();
    staged_modeller_preview(&mut app);
    let data = app.state.features.model_preview.current().unwrap().clone();
    let model = Arc::clone(app.state.features.model_preview.stage_model().unwrap());
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::MapSelected(model_manager::MapChoice("map.bsp".into())),
    );
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::StageRequested,
    );
    let mut model_request = file_preview_request();
    model_request.entry_path = "models/staged.mdl".into();
    let map = app.state.features.model_preview.request().unwrap().clone();
    let _decode = app.apply_model_preview_message(file_preview::Message::WorkspaceStageRequested {
        model: Box::new(model_request),
        map: Box::new(map),
    });
    let generation = app
        .state
        .features
        .model_manager
        .presentation_request()
        .unwrap();
    let manager = app.state.features.model_manager.preview_job_generation();
    for reduced in [true, false, true] {
        let _task = app.apply_model_preview_message(
            file_preview::Message::MapReducedTextureDetailToggled(reduced),
        );
        assert_eq!(
            app.state
                .features
                .model_manager
                .presentation_request()
                .unwrap(),
            generation
        );
        assert_eq!(
            app.state.features.model_manager.preview_job_generation(),
            manager
        );
    }
    let _task = app.apply_model_preview_message(file_preview::Message::StageLoaded(
        generation,
        Box::new(Ok((data.clone(), model))),
    ));
    let quality_generation = app
        .state
        .features
        .model_manager
        .presentation_request()
        .unwrap();
    assert_ne!(quality_generation, generation);
    assert_eq!(
        app.state
            .features
            .model_preview
            .requested_map_texture_quality(),
        Some(preview_model::MapTextureQuality::Reduced)
    );
    assert!(!app.state.features.model_manager.preview_busy());
    assert!(app.state.features.model_preview.stage_model().is_some());
    let _task = app.apply_model_preview_message(file_preview::Message::Loaded(
        quality_generation,
        Box::new(Ok(data)),
    ));
    assert!(app.state.features.model_preview.current().is_some());
    assert!(app.state.features.model_preview.stage_model().is_some());
}

fn modeller_workflow_map(directory: &Path) -> PathBuf {
    let map = directory.join("environment.bsp");
    let mut bsp = vec![0_u8; 4 + 4 + 64 * 16 + 4];
    bsp[..4].copy_from_slice(b"VBSP");
    bsp[4..8].copy_from_slice(&20_i32.to_le_bytes());
    let offset = bsp.len();
    let pak_entry = 8 + 40 * 16;
    bsp[pak_entry..pak_entry + 4].copy_from_slice(&(offset as i32).to_le_bytes());
    bsp[pak_entry + 4..pak_entry + 8].copy_from_slice(&22_i32.to_le_bytes());
    bsp.extend_from_slice(&[
        0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]);
    std::fs::write(&map, bsp).unwrap();
    map
}

#[test]
fn modeller_async_real_stage_quality_rebuild_and_cache_round_trip() {
    use crate::features::model_manager;
    let root = tempfile::tempdir().unwrap();
    let source = modeller_workflow_source(root.path(), "workflow");
    let map = modeller_workflow_map(root.path());
    let mut app = App::new_for_test();
    let decode = modeller_workflow_until_decode(&mut app, source);
    modeller_workflow_drain(&mut app, decode);
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::MapSelected(model_manager::MapChoice(map.clone())),
    );
    let stage = app.apply_model_manager_message(model_manager::Message::StageRequested);
    modeller_workflow_drain(&mut app, stage);
    assert!(app.state.features.model_preview.stage_model().is_some());
    let mut transform = app.state.features.model_preview.stage_transform().unwrap();
    transform.position = [32.0, 64.0, 128.0].into();
    transform.angles = [0.0, 90.0, 0.0].into();
    let _task =
        app.apply_model_preview_message(file_preview::Message::StageTransformChanged(transform));
    let placement = app.state.features.model_preview.stage_transform();
    let quality = app
        .apply_model_preview_message(file_preview::Message::MapReducedTextureDetailToggled(true));
    modeller_workflow_drain(&mut app, quality);
    let reduced_scene = match &app.state.features.model_preview.current().unwrap().content {
        preview_model::PreviewContent::Map { scene, .. } => {
            assert!(scene.texture_preparation.max_dimension.is_some());
            Arc::clone(scene)
        }
        _ => panic!("expected staged map"),
    };
    assert_eq!(
        app.state.features.model_preview.stage_transform(),
        placement
    );
    model_manager::update(
        &mut app.state.features.model_manager,
        model_manager::Message::AutomaticAxesChanged(false),
    );
    let rebuild = app.apply_model_manager_message(model_manager::Message::StageRequested);
    modeller_workflow_drain(&mut app, rebuild);
    assert_eq!(
        app.state
            .features
            .model_preview
            .requested_map_texture_quality(),
        Some(preview_model::MapTextureQuality::Reduced)
    );
    assert_eq!(
        app.state.features.model_preview.stage_transform(),
        placement
    );
    assert!(
        matches!(&app.state.features.model_preview.current().unwrap().content, preview_model::PreviewContent::Map { scene, .. } if Arc::ptr_eq(scene, &reduced_scene))
    );

    // An explicit Native stage request must not reuse the Reduced cache entry.
    let lease = app
        .state
        .features
        .model_manager
        .preview_directory()
        .unwrap();
    let (model, _) = super::model_stage::prepare_imported_model_preview(
        Generation::INITIAL,
        &lease.path().join("models/workflow.mdl"),
    )
    .unwrap();
    let native = super::model_stage::prepare_map_preview(Generation::INITIAL, &map).unwrap();
    let restage =
        app.run_model_manager_effect(model_manager::Effect::OpenStage { model, map: native });
    modeller_workflow_drain(&mut app, restage);
    assert!(
        matches!(&app.state.features.model_preview.current().unwrap().content, preview_model::PreviewContent::Map { scene, .. } if scene.texture_preparation.max_dimension.is_none() && !Arc::ptr_eq(scene, &reduced_scene))
    );
    assert_eq!(
        app.state
            .features
            .model_preview
            .requested_map_texture_quality(),
        Some(preview_model::MapTextureQuality::Native)
    );
    assert_eq!(
        app.state.features.model_preview.stage_transform(),
        placement
    );
    assert!(app.state.features.model_preview.stage_model().is_some());
}
