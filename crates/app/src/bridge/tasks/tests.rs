use super::*;
use crate::bridge::domain::SteamId;
use crate::bridge::{DownloadCountFormat, ThemePreset};
use gmpublished_backend::AppDataSnapshot as BackendAppDataSnapshot;
use gmpublished_backend::SteamRuntimeError;
use gmpublished_backend::TransactionId;
use gmpublished_backend::publishing as steam_publishing;
use gmpublished_backend::{TransactionError, TransactionPayload};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;
use std::{
    fs,
    path::{Path, PathBuf},
};

static BACKEND_EVENT_SINK_TEST_LOCK: Mutex<()> = Mutex::new(());

fn drain_updates(receiver: &mpsc::Receiver<TaskEvent>) -> Vec<(TaskId, TaskUpdate)> {
    receiver
        .try_iter()
        .map(|(id, update)| (id, update.into_update()))
        .collect()
}

fn appdata_snapshot_event_payload_for_test() -> gmpublished_backend::AppDataSnapshot {
    let root = std::env::temp_dir().join("gmpublished-appdata-event-test");
    gmpublished_backend::AppDataSnapshot {
        settings: gmpublished_backend::Settings::default(),
        settings_revision: 0,
        version: "test",
        paths: gmpublished_backend::AppDataPathsSnapshot {
            settings_file: root.join("settings.json"),
            default_user_data_dir: root.join("default-user-data"),
            default_temp_dir: root.join("default-temp"),
            default_downloads_dir: Some(root.join("default-downloads")),
            temp_dir: root.join("temp"),
            user_data_dir: root.join("user-data"),
            downloads_dir: Some(root.join("downloads")),
            gmod_dir: None,
        },
    }
}

#[test]
fn app_scheduler_uses_the_process_owned_blocking_and_network_executors() {
    let execution =
        gmpublished_backend::ExecutionResources::build(gmpublished_backend::ExecutionConfig {
            cpu_threads: 1,
            blocking_threads: 1,
            network_threads: 1,
            queue_capacity: 4,
        })
        .expect("test execution resources");
    let runtime = AppWorkerRuntime::new(execution);

    let (done_tx, done_rx) = mpsc::channel();
    runtime
        .spawn_blocking("blocking-job", move || {
            done_tx
                .send(std::thread::current().name().map(str::to_owned))
                .unwrap();
        })
        .unwrap();

    assert!(
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_some_and(|name| name.starts_with("gmpublished-blocking-"))
    );

    let (media_done_tx, media_done_rx) = mpsc::channel();
    runtime
        .spawn_media_job(
            std::sync::Arc::from("first-media-job"),
            Box::new(move || {
                media_done_tx
                    .send(std::thread::current().name().map(str::to_owned))
                    .unwrap();
            }),
        )
        .unwrap();

    assert!(
        media_done_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_some_and(|name| name.starts_with("gmpublished-network-"))
    );
}

#[test]
fn task_event_forwarder_delivers_on_its_own_thread() {
    use iced::futures::StreamExt;

    let (tasks, receiver) = Tasks::channel();
    let factory = TaskEventStreamFactory::new(receiver);
    let handle = tasks.create(TaskKind::Search, TransactionStatus::Searching);
    let id = handle.id();

    // The forwarder is spawned the first time the stream is polled; it
    // takes the receiver from `factory` and relays events on its own
    // thread, independent of any worker pool.
    let mut stream = std::pin::pin!(task_event_stream(&factory));
    let (event_id, update) =
        futures::executor::block_on(stream.next()).expect("forwarded task event");

    assert_eq!(event_id, id);
    assert_eq!(
        update.into_update(),
        TaskUpdate::Started {
            kind: TaskKind::Search,
            status: TransactionStatus::Searching,
        }
    );
}

#[test]
fn task_event_stream_restart_reattaches_to_live_forwarder() {
    use iced::futures::StreamExt;

    let (tasks, receiver) = Tasks::channel();
    let factory = TaskEventStreamFactory::new(receiver);
    let handle = tasks.create(TaskKind::Search, TransactionStatus::Searching);
    let id = handle.id();

    // Dropping the stream at the end of this scope models iced tearing the
    // subscription down; the forwarder thread must outlive it.
    {
        let mut stream = std::pin::pin!(task_event_stream(&factory));
        let (event_id, _started) =
            futures::executor::block_on(stream.next()).expect("forwarded task event");
        assert_eq!(event_id, id);
    }

    // Emitted with no live stream: it either waits in the std channel or
    // parks mid-delivery in the forwarder until the next run attaches.
    handle.progress(0.5);

    let mut stream = std::pin::pin!(task_event_stream(&factory));
    let (event_id, update) =
        futures::executor::block_on(stream.next()).expect("event after stream restart");

    assert_eq!(event_id, id);
    assert_eq!(update.into_update(), TaskUpdate::Progress(0.5));
}

#[test]
fn task_handles_emit_started_progress_and_terminal_updates() {
    let (tasks, receiver) = Tasks::channel();
    let task = tasks.create(TaskKind::Download, TransactionStatus::Downloading);
    let id = task.id();

    task.total(2_048);
    task.progress(0.5);
    task.finished();

    assert_eq!(
        drain_updates(&receiver),
        vec![
            (
                id,
                TaskUpdate::Started {
                    kind: TaskKind::Download,
                    status: TransactionStatus::Downloading,
                },
            ),
            (id, TaskUpdate::Total(2_048)),
            (id, TaskUpdate::Progress(0.5)),
            (id, TaskUpdate::Finished),
        ]
    );
}

#[test]
fn coalesced_updates_observe_last_values_and_accumulate_incremental_progress() {
    // `CoalescedTaskUpdate::observe` is the downloader's coalescing
    // primitive (features/downloader/model.rs pending_task_updates).
    let mut update = CoalescedTaskUpdate::default();
    update.observe(
        TaskUpdate::Started {
            kind: TaskKind::Publish,
            status: TransactionStatus::PublishPreparingConfig,
        },
        0.0,
    );
    update.observe(TaskUpdate::Total(1_000), 0.0);
    update.observe(TaskUpdate::Total(2_000), 0.0);
    update.observe(TaskUpdate::Progress(0.10), 0.0);
    update.observe(TaskUpdate::ProgressIncr(0.20), 0.0);
    update.observe(TaskUpdate::Status(TransactionStatus::PublishPacking), 0.0);
    update.observe(TaskUpdate::Finished, 0.0);

    assert_eq!(
        update.started,
        Some(CoalescedTaskStart {
            kind: TaskKind::Publish,
            status: TransactionStatus::PublishPreparingConfig,
        })
    );
    assert_eq!(update.status, Some(TransactionStatus::PublishPacking));
    assert!((update.progress.expect("coalesced progress") - 0.30).abs() < f64::EPSILON);
    assert_eq!(update.total_bytes, Some(2_000));
    assert_eq!(update.terminal, Some(CoalescedTaskTerminal::Finished));

    // An incremental update on a fresh entry accumulates from the
    // caller-supplied current progress.
    let mut other = CoalescedTaskUpdate::default();
    other.observe(TaskUpdate::ProgressIncr(0.05), 0.40);
    assert!((other.progress.expect("coalesced progress") - 0.45).abs() < f64::EPSILON);
}

#[test]
fn backend_context_exposes_settings_snapshot_and_task_cancellation() {
    let ctx = BackendContext::new().expect("test backend context");
    let (settings, paths) = ctx.settings_and_paths_snapshot();

    assert_eq!(settings, Settings::default());
    assert_eq!(paths.temp_dir, paths.default_temp_dir);

    // Not yet correlated with a backend transaction: nothing can cancel it.
    let handle = ctx.create_task(TaskKind::Search, TransactionStatus::Searching);
    let id = handle.id();
    assert!(!ctx.cancel_task(id));
    handle.finished();
}

#[test]
fn backend_context_unit_tests_use_disconnected_steam_runtime() {
    let ctx = BackendContext::new().expect("test backend context");

    assert!(!ctx.steam_connected());
    // Compared by key, not by rendered text: `Display` is a developer message
    // and only `HasErrorKey` is the wire contract.
    let error = ctx.connect_steam().expect_err("no Steam in tests");
    assert_eq!(
        error,
        UiError::from(&SteamRuntimeError::Unavailable),
        "{error}"
    );
    assert!(!ctx.steam_connected());
}

#[test]
fn standalone_ui_settings_are_migrated_before_the_old_file_is_retired() {
    let temp = tempfile::tempdir().expect("tempdir");
    let settings_dir = temp.path().join("gmpublished");
    let legacy_file = settings_dir.join("ui-settings.json");
    fs::create_dir_all(&settings_dir).expect("settings dir");
    fs::write(
        &legacy_file,
        r#"{"version":1,"play_gifs_by_default":false,"download_count_format":"comma","theme_preset":"light"}"#,
    )
    .expect("legacy UI settings");

    let services = BackendServices::for_test_with_data_root(temp.path());
    let settings = services.config().settings_snapshot();
    assert!(!settings.ui.play_gifs_by_default);
    assert_eq!(
        settings.ui.download_count_format,
        DownloadCountFormat::Comma
    );
    assert_eq!(settings.ui.theme_preset, ThemePreset::Light);

    let persisted: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(settings_dir.join("settings.json")).expect("unified settings"),
    )
    .expect("unified settings JSON");
    assert_eq!(persisted["ui"]["theme_preset"], "light");
    assert!(
        !legacy_file.exists(),
        "the old file is retired only after the unified write succeeds"
    );
}

#[test]
fn resetting_settings_persists_default_ui_settings() {
    let temp = tempfile::tempdir().expect("tempdir");
    let services = BackendServices::for_test_with_data_root(temp.path());

    services
        .config()
        .update_settings_snapshot(|settings| {
            settings.ui.play_gifs_by_default = false;
            settings.ui.download_count_format = DownloadCountFormat::Period;
            settings.ui.theme_preset = ThemePreset::ClassicSource;
        })
        .expect("non-default UI settings update");
    services.config().reset_settings().expect("reset settings");

    let settings = services.config().settings_snapshot();
    assert_eq!(UiSettings::from_settings(&settings), UiSettings::default());
    let restarted = BackendServices::for_test_with_data_root(temp.path());
    assert_eq!(
        UiSettings::from_settings(&restarted.config().settings_snapshot()),
        UiSettings::default()
    );
}

#[test]
fn appdata_refresh_preserves_persisted_ui_settings() {
    let temp = tempfile::tempdir().expect("tempdir");
    let services = BackendServices::for_test_with_data_root(temp.path());
    services
        .config()
        .update_settings_snapshot(|settings| {
            settings.ui.play_gifs_by_default = false;
            settings.ui.download_count_format = DownloadCountFormat::Space;
            settings.ui.theme_preset = ThemePreset::ClassicSource;
        })
        .expect("UI settings update");

    let temp_dir = temp.path().join("resolved-temp");
    let user_data_dir = temp.path().join("resolved-user-data");
    let downloads_dir = temp.path().join("resolved-downloads");
    fs::create_dir_all(&temp_dir).expect("temp dir");
    fs::create_dir_all(&user_data_dir).expect("user data dir");
    fs::create_dir_all(&downloads_dir).expect("downloads dir");

    let backend_settings = gmpublished_backend::Settings {
        sounds: false,
        language: Some("fr".to_owned()),
        ..Default::default()
    };
    let (settings, paths) = services
        .config()
        .apply_appdata_snapshot(BackendAppDataSnapshot {
            settings: backend_settings,
            settings_revision: 2,
            version: "test",
            paths: gmpublished_backend::AppDataPathsSnapshot {
                settings_file: temp.path().join("settings.json"),
                default_user_data_dir: temp.path().join("default-user-data"),
                default_temp_dir: temp.path().join("default-temp"),
                default_downloads_dir: Some(temp.path().join("default-downloads")),
                temp_dir: temp_dir.clone(),
                user_data_dir,
                downloads_dir: Some(downloads_dir),
                gmod_dir: None,
            },
        });

    assert!(!settings.backend.sounds);
    assert_eq!(settings.backend.language.as_deref(), Some("fr"));
    assert_eq!(paths.temp_dir, temp_dir);
    assert!(!settings.ui.play_gifs_by_default);
    assert_eq!(
        settings.ui.download_count_format,
        DownloadCountFormat::Space
    );
    assert_eq!(settings.ui.theme_preset, ThemePreset::ClassicSource);
}

#[test]
#[cfg(unix)]
fn failed_backend_settings_save_leaves_snapshot_and_live_appdata_unchanged() {
    use std::os::unix::fs::PermissionsExt;

    let services = BackendServices::for_test_with_settings_persist_enabled();
    let before = services.config().settings_snapshot();

    let settings_file = services.app_data_snapshot_for_test().paths.settings_file;
    let settings_dir = settings_file.parent().expect("settings dir");
    fs::create_dir_all(settings_dir).expect("settings dir");
    let original_mode = fs::metadata(settings_dir)
        .expect("settings dir metadata")
        .permissions();
    fs::set_permissions(settings_dir, fs::Permissions::from_mode(0o555))
        .expect("lock down settings dir");

    let result = services.config().update_settings_snapshot(|settings| {
        settings.backend.sounds = !settings.backend.sounds;
        settings.backend.language = Some("unsaved".to_owned());
    });

    fs::set_permissions(settings_dir, original_mode).expect("restore settings dir permissions");

    assert!(
        result.is_err(),
        "save into an unwritable directory must fail"
    );
    // The mutation happened on a private copy; a failed persist must never
    // publish it to the settings snapshot BackendServices hands out.
    assert_eq!(services.config().settings_snapshot(), before);
}

#[test]
fn concurrent_settings_mutations_are_serialized_without_lost_fields() {
    let services = Arc::new(BackendServices::for_test());
    let barrier = Arc::new(std::sync::Barrier::new(3));

    let backend_writer = {
        let services = Arc::clone(&services);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            services
                .config()
                .update_settings_snapshot(|settings| settings.backend.sounds = false)
                .expect("backend setting update");
        })
    };
    let ui_writer = {
        let services = Arc::clone(&services);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            services
                .config()
                .update_settings_snapshot(|settings| {
                    settings.ui.theme_preset = ThemePreset::Light;
                })
                .expect("UI setting update");
        })
    };

    barrier.wait();
    backend_writer.join().expect("backend writer");
    ui_writer.join().expect("UI writer");

    let settings = services.config().settings_snapshot();
    assert!(!settings.backend.sounds);
    assert_eq!(settings.ui.theme_preset, ThemePreset::Light);
}

#[test]
fn stale_backend_settings_events_cannot_replace_a_newer_publication() {
    let temp = tempfile::tempdir().expect("tempdir");
    let services = BackendServices::for_test_with_data_root(temp.path());
    let stale = services.app_data_snapshot_for_test();

    services
        .config()
        .update_settings_snapshot(|settings| settings.backend.sounds = false)
        .expect("newer settings update");
    let applied = services.config().apply_appdata_snapshot(stale).0;

    assert!(!applied.backend.sounds);
    assert!(!services.config().settings_snapshot().backend.sounds);
}

#[test]
fn backend_context_forwards_installed_backend_events() {
    let _lock = BACKEND_EVENT_SINK_TEST_LOCK.lock();
    let ctx = BackendContext::new_with_backend_event_sink_for_test();
    let receiver = ctx
        .backend_events
        .take_receiver()
        .expect("backend event receiver");

    ctx.emit_backend_event_for_test(gmpublished_backend::BackendEvent::SteamConnected);
    ctx.emit_backend_event_for_test(gmpublished_backend::BackendEvent::SteamDisconnected);
    let appdata_snapshot = appdata_snapshot_event_payload_for_test();
    ctx.emit_backend_event_for_test(gmpublished_backend::BackendEvent::AppDataUpdated(Box::new(
        appdata_snapshot.clone(),
    )));
    ctx.emit_backend_event_for_test(gmpublished_backend::BackendEvent::InstalledAddonsRefreshed);
    ctx.emit_backend_event_for_test(gmpublished_backend::BackendEvent::DownloadStarted(
        gmpublished_backend::DownloadStartedEvent {
            transaction_id: TransactionId::from_raw(40),
            request_id: None,
        },
    ));
    ctx.emit_backend_event_for_test(gmpublished_backend::BackendEvent::ExtractionStarted(
        gmpublished_backend::ExtractionStartedEvent {
            transaction_id: TransactionId::from_raw(41),
            source_path: Some(PathBuf::from("/tmp/addon.gma")),
            file_name: Some("addon.gma".to_owned()),
            workshop_id: Some(
                gmpublished_backend::WorkshopId::new(123).expect("fixture ids are nonzero"),
            ),
            request_id: None,
        },
    ));
    ctx.emit_backend_event_for_test(gmpublished_backend::BackendEvent::Transaction(
        gmpublished_backend::TransactionEvent::Status {
            id: TransactionId::from_raw(42),
            status: TransactionStatus::PublishPacking,
        },
    ));

    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        BackendRuntimeEvent::SteamConnected
    );
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        BackendRuntimeEvent::SteamDisconnected
    );
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        BackendRuntimeEvent::AppDataUpdated(Box::new(appdata_snapshot))
    );
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        BackendRuntimeEvent::InstalledAddonsRefreshed
    );
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        BackendRuntimeEvent::DownloadStarted {
            transaction_id: TransactionId::from_raw(40),
            request_id: None,
        }
    );
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        BackendRuntimeEvent::ExtractionStarted {
            transaction_id: TransactionId::from_raw(41),
            source_path: Some(PathBuf::from("/tmp/addon.gma")),
            file_name: Some("addon.gma".to_owned()),
            workshop_id: Some(PublishedFileId::fixture(123)),
            request_id: None,
        }
    );
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        BackendRuntimeEvent::Transaction(TransactionRuntimeEvent::Status {
            id: TransactionId::from_raw(42),
            status: TransactionStatus::PublishPacking,
        })
    );
}

#[test]
fn backend_event_sink_fans_out_to_worker_subscriptions() {
    let _lock = BACKEND_EVENT_SINK_TEST_LOCK.lock();
    let ctx = BackendContext::new_with_backend_event_sink_for_test();
    let root_receiver = ctx
        .backend_events
        .take_receiver()
        .expect("backend event receiver");
    let worker_events = ctx
        .services
        .subscribe_backend_events()
        .expect("worker event subscription");

    ctx.emit_backend_event_for_test(gmpublished_backend::BackendEvent::Transaction(
        gmpublished_backend::TransactionEvent::Progress {
            id: TransactionId::from_raw(81),
            progress: 5000,
        },
    ));

    let expected = BackendRuntimeEvent::Transaction(TransactionRuntimeEvent::Progress {
        id: TransactionId::from_raw(81),
        progress: 5000,
    });
    assert_eq!(
        root_receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        expected
    );
    assert_eq!(
        worker_events.recv_timeout(Duration::from_secs(1)).unwrap(),
        expected
    );
}

#[test]
fn terminal_event_is_never_dropped_even_when_progress_queue_is_saturated() {
    let _lock = BACKEND_EVENT_SINK_TEST_LOCK.lock();
    let ctx = BackendContext::new_with_backend_event_sink_for_test();
    let root_receiver = ctx
        .backend_events
        .take_receiver()
        .expect("backend event receiver");

    // Flood well past the bounded root queue's capacity with progress
    // events; these must be dropped under backpressure rather than block.
    for progress in 0..u16::try_from(BACKEND_EVENT_QUEUE_CAPACITY + 50).unwrap() {
        ctx.emit_backend_event_for_test(gmpublished_backend::BackendEvent::Transaction(
            gmpublished_backend::TransactionEvent::Progress {
                id: TransactionId::from_raw(900),
                progress,
            },
        ));
    }

    // The terminal event must still get through. Emit it from another
    // thread: with the queue already saturated, a blocking send would
    // otherwise deadlock this test until the drain loop below catches up.
    let emitter_context = ctx;
    let emitter = std::thread::spawn(move || {
        emitter_context.emit_backend_event_for_test(
            gmpublished_backend::BackendEvent::Transaction(
                gmpublished_backend::TransactionEvent::Finished {
                    id: TransactionId::from_raw(900),
                    payload: TransactionPayload::None,
                },
            ),
        );
    });

    let mut saw_finished = false;
    while let Ok(event) = root_receiver.recv_timeout(Duration::from_secs(2)) {
        if matches!(
            event,
            BackendRuntimeEvent::Transaction(TransactionRuntimeEvent::Finished { id, .. })
                if id == TransactionId::from_raw(900)
        ) {
            saw_finished = true;
            break;
        }
    }
    emitter.join().expect("emitter thread");
    assert!(
        saw_finished,
        "terminal event must not be dropped under a saturated progress queue"
    );
}

#[test]
fn backend_start_event_correlates_transaction_updates_to_task_events() {
    let ctx = BackendContext::new().expect("test backend context");
    let receiver = ctx
        .task_events
        .take_receiver()
        .expect("task event receiver");

    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::ExtractionStarted {
            transaction_id: TransactionId::from_raw(500),
            source_path: Some(PathBuf::from("/tmp/source.gma")),
            file_name: Some("source.gma".to_owned()),
            workshop_id: Some(PublishedFileId::fixture(765)),
            request_id: None,
        })
        .handled_event()
    );
    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Status {
                id: TransactionId::from_raw(500),
                status: TransactionStatus::Locating,
            },
        ))
        .handled_event()
    );
    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Data {
                id: TransactionId::from_raw(500),
                payload: TransactionPayload::ByteSize {
                    source: None,
                    bytes: 2_048,
                },
            },
        ))
        .handled_event()
    );
    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Progress {
                id: TransactionId::from_raw(500),
                progress: 2_500,
            },
        ))
        .handled_event()
    );
    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::IncrProgress {
                id: TransactionId::from_raw(500),
                incr: 2_500,
            },
        ))
        .handled_event()
    );
    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::ResetProgress {
                id: TransactionId::from_raw(500)
            },
        ))
        .handled_event()
    );
    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Progress {
                id: TransactionId::from_raw(500),
                progress: 7_500,
            },
        ))
        .handled_event()
    );
    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Finished {
                id: TransactionId::from_raw(500),
                payload: TransactionPayload::ExtractedPath(PathBuf::from("/tmp/extracted")),
            },
        ))
        .handled_event()
    );

    let updates = drain_updates(&receiver);
    assert!(updates.iter().any(|(_, update)| matches!(
        update,
        TaskUpdate::Started {
            kind: TaskKind::Extract,
            status,
        } if *status == TransactionStatus::Extracting
    )));
    assert!(updates.iter().any(|(_, update)| {
        matches!(update, TaskUpdate::Status(status) if *status == TransactionStatus::Locating)
    }));
    assert!(
        updates
            .iter()
            .any(|(_, update)| matches!(update, TaskUpdate::Total(2_048)))
    );
    assert!(
        updates
            .iter()
            .any(|(_, update)| matches!(update, TaskUpdate::Finished))
    );
}

#[test]
fn correlated_backend_transaction_error_finishes_task_with_error() {
    let ctx = BackendContext::new().expect("test backend context");
    let receiver = ctx
        .task_events
        .take_receiver()
        .expect("task event receiver");

    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::DownloadStarted {
            transaction_id: TransactionId::from_raw(501),
            request_id: None,
        })
        .handled_event()
    );
    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Error {
                id: TransactionId::from_raw(501),
                error: TransactionError::detailed(
                    gmpublished_backend::ErrorKey::for_test("ERR_TEST"),
                    Some("detail".to_owned()),
                ),
            },
        ))
        .handled_event()
    );

    assert_eq!(
        drain_updates(&receiver)
            .into_iter()
            .find_map(|(_, update)| match update {
                TaskUpdate::Error(error) => Some(error.to_string()),
                _ => None,
            }),
        Some("ERR_TEST:detail".to_owned())
    );
}

#[test]
fn direct_correlated_backend_transaction_error_removes_task() {
    let ctx = BackendContext::new().expect("test backend context");
    let receiver = ctx
        .task_events
        .take_receiver()
        .expect("task event receiver");
    let task = ctx.create_task(TaskKind::Extract, TransactionStatus::Extracting);
    ctx.correlate_backend_transaction(TransactionId::from_raw(502), task);

    assert!(ctx.error_backend_transaction_task(
        TransactionId::from_raw(502),
        UiError::new(gmpublished_backend::ErrorKey::for_test("ERR_DIRECT"))
    ));
    assert!(
        !ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Finished {
                id: TransactionId::from_raw(502),
                payload: TransactionPayload::None,
            },
        ))
        .handled_event()
    );

    assert_eq!(
        drain_updates(&receiver)
            .into_iter()
            .find_map(|(_, update)| match update {
                TaskUpdate::Error(error) => Some(error.to_string()),
                _ => None,
            }),
        Some("ERR_DIRECT".to_owned())
    );
}

#[test]
fn uncorrelated_backend_transaction_events_remain_noops() {
    let ctx = BackendContext::new().expect("test backend context");
    let receiver = ctx
        .task_events
        .take_receiver()
        .expect("task event receiver");

    assert!(
        !ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Status {
                id: TransactionId::from_raw(999),
                status: TransactionStatus::PublishPacking,
            },
        ))
        .handled_event()
    );
    assert!(receiver.try_iter().next().is_none());
}

#[test]
fn download_start_waits_for_item_payload_before_downloader_action() {
    let ctx = BackendContext::new().expect("test backend context");

    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::DownloadStarted {
        transaction_id: TransactionId::from_raw(610),
        request_id: None,
    });
    assert!(effects.handled_event());
    assert!(effects.into_actions().is_empty());

    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
        TransactionRuntimeEvent::Data {
            id: TransactionId::from_raw(610),
            payload: TransactionPayload::WorkshopItem(
                gmpublished_backend::WorkshopId::new(123).expect("fixture ids are nonzero"),
            ),
        },
    ));
    assert!(effects.handled_event());
    let actions = effects.into_actions();
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        &actions[0],
        BackendRuntimeAction::DownloadTaskStarted {
            kind: WorkshopDownloadTaskKind::Download,
            item_id,
            ..
        } if *item_id == PublishedFileId::fixture(123)
    ));
}

#[test]
fn extraction_start_and_finish_emit_downloader_actions() {
    let ctx = BackendContext::new().expect("test backend context");

    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::ExtractionStarted {
        transaction_id: TransactionId::from_raw(620),
        source_path: None,
        file_name: None,
        workshop_id: Some(PublishedFileId::fixture(456)),
        request_id: None,
    });
    assert!(effects.handled_event());
    let actions = effects.into_actions();
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        &actions[0],
        BackendRuntimeAction::DownloadTaskStarted {
            kind: WorkshopDownloadTaskKind::Extract,
            item_id,
            ..
        } if *item_id == PublishedFileId::fixture(456)
    ));

    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
        TransactionRuntimeEvent::Finished {
            id: TransactionId::from_raw(620),
            payload: TransactionPayload::ExtractedPath(PathBuf::from("/tmp/extracted/456")),
        },
    ));
    assert!(effects.handled_event());
    assert_eq!(
        effects.into_actions(),
        vec![BackendRuntimeAction::DownloadFinished {
            request_id: None,
            item_id: PublishedFileId::fixture(456),
            installed_path: None,
            extracted_path: PathBuf::from("/tmp/extracted/456"),
        }]
    );
}

#[test]
fn extraction_finish_carries_the_source_gma_from_the_started_event() {
    let ctx = BackendContext::new().expect("test backend context");
    let source_gma = PathBuf::from("/tmp/workshop/content/4000/457/addon.gma");

    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::ExtractionStarted {
        transaction_id: TransactionId::from_raw(621),
        source_path: Some(source_gma.clone()),
        file_name: None,
        workshop_id: Some(PublishedFileId::fixture(457)),
        request_id: None,
    });
    assert!(effects.handled_event());

    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
        TransactionRuntimeEvent::Finished {
            id: TransactionId::from_raw(621),
            payload: TransactionPayload::ExtractedPath(PathBuf::from("/tmp/extracted/457")),
        },
    ));
    assert!(effects.handled_event());
    assert_eq!(
        effects.into_actions(),
        vec![BackendRuntimeAction::DownloadFinished {
            request_id: None,
            item_id: PublishedFileId::fixture(457),
            installed_path: Some(source_gma),
            extracted_path: PathBuf::from("/tmp/extracted/457"),
        }]
    );
}

#[test]
fn workshop_snapshot_actions_keep_the_request_id_and_skip_downloader_rows() {
    let ctx = BackendContext::new().expect("test backend context");
    let workshop_id = PublishedFileId::fixture(458);

    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::ExtractionStarted {
        transaction_id: TransactionId::from_raw(622),
        source_path: None,
        file_name: None,
        workshop_id: Some(workshop_id),
        request_id: Some(WorkshopSnapshotId::new(77)),
    });
    assert!(effects.handled_event());
    assert!(effects.into_actions().is_empty());

    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
        TransactionRuntimeEvent::Finished {
            id: TransactionId::from_raw(622),
            payload: TransactionPayload::ExtractedPath(PathBuf::from("/tmp/extracted/458")),
        },
    ));
    assert_eq!(
        effects.into_actions(),
        vec![BackendRuntimeAction::DownloadFinished {
            request_id: Some(WorkshopSnapshotId::new(77)),
            item_id: workshop_id,
            installed_path: None,
            extracted_path: PathBuf::from("/tmp/extracted/458"),
        }]
    );
}

#[test]
fn workshop_snapshot_error_is_request_scoped() {
    let ctx = BackendContext::new().expect("test backend context");
    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::DownloadStarted {
        transaction_id: TransactionId::from_raw(623),
        request_id: Some(WorkshopSnapshotId::new(78)),
    });
    assert!(effects.handled_event());

    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
        TransactionRuntimeEvent::Error {
            id: TransactionId::from_raw(623),
            error: TransactionError::detailed(
                gmpublished_backend::ErrorKey::for_test("ERR_TEST"),
                Some("detail".to_owned()),
            ),
        },
    ));
    assert!(matches!(
        effects.into_actions().as_slice(),
        [BackendRuntimeAction::SnapshotFailed { request_id, .. }]
            if *request_id == WorkshopSnapshotId::new(78)
    ));
}

#[test]
fn cancelling_correlated_backend_task_aborts_registered_transaction() {
    let ctx = BackendContext::new().expect("test backend context");
    let receiver = ctx
        .task_events
        .take_receiver()
        .expect("task event receiver");
    let transaction = ctx.begin_transaction();

    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::DownloadStarted {
            transaction_id: transaction.id(),
            request_id: None,
        })
        .handled_event()
    );
    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
        TransactionRuntimeEvent::Data {
            id: transaction.id(),
            payload: TransactionPayload::WorkshopItem(
                gmpublished_backend::WorkshopId::new(123).expect("fixture ids are nonzero"),
            ),
        },
    ));
    let actions = effects.into_actions();
    let [BackendRuntimeAction::DownloadTaskStarted { task_id, .. }] = actions.as_slice() else {
        panic!("expected correlated downloader start action");
    };

    assert!(!transaction.aborted());
    assert!(ctx.cancel_task(*task_id));
    assert!(transaction.aborted());
    assert!(
        !ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Status {
                id: transaction.id(),
                status: TransactionStatus::Downloading,
            },
        ))
        .handled_event()
    );

    let updates = drain_updates(&receiver);
    assert!(updates.iter().any(|(_, update)| {
        matches!(
            update,
            TaskUpdate::Started {
                kind: TaskKind::Download,
                ..
            }
        )
    }));
    assert!(updates.iter().any(|(_, update)| {
        matches!(update, TaskUpdate::Error(error) if error.to_string() == "ERR_CANCELLED")
    }));
    assert!(!ctx.cancel_task(*task_id));
}

#[test]
fn extraction_pre_start_locating_status_is_buffered_until_downloader_start_event() {
    let ctx = BackendContext::new().expect("test backend context");
    let receiver = ctx
        .task_events
        .take_receiver()
        .expect("task event receiver");

    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
        TransactionRuntimeEvent::Status {
            id: TransactionId::from_raw(630),
            status: TransactionStatus::Locating,
        },
    ));
    assert!(effects.handled_event());
    assert!(effects.into_actions().is_empty());

    let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::ExtractionStarted {
        transaction_id: TransactionId::from_raw(630),
        source_path: None,
        file_name: None,
        workshop_id: Some(PublishedFileId::fixture(789)),
        request_id: None,
    });
    assert!(effects.handled_event());
    assert!(matches!(
        effects.into_actions().as_slice(),
        [BackendRuntimeAction::DownloadTaskStarted {
            kind: WorkshopDownloadTaskKind::Extract,
            item_id,
            ..
        }] if *item_id == PublishedFileId::fixture(789)
    ));

    let updates: Vec<_> = receiver
        .try_iter()
        .map(|(_task_id, update)| update.into_update())
        .collect();
    assert!(updates.iter().any(|update| matches!(
        update,
        TaskUpdate::Status(status) if *status == TransactionStatus::Locating
    )));
}

#[test]
fn extraction_pre_start_buffer_is_globally_bounded() {
    let ctx = BackendContext::new().expect("test backend context");

    for offset in 0..(MAX_PENDING_PRE_START_TRANSACTIONS + 4) {
        let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Status {
                id: TransactionId::from_raw(
                    10_000 + u32::try_from(offset).expect("test offset fits u32"),
                ),
                status: TransactionStatus::Locating,
            },
        ));
        assert!(effects.handled_event());
    }

    // One transaction gets more events than its own cap allows, so the
    // per-entry bound is exercised rather than asserted against entries of
    // length one.
    let noisy = TransactionId::from_raw(9_999);
    for _ in 0..(MAX_PENDING_PRE_START_EVENTS_PER_TRANSACTION + 5) {
        let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Status {
                id: noisy,
                status: TransactionStatus::Locating,
            },
        ));
        assert!(effects.handled_event());
    }

    let pending = ctx.transaction_tasks.pending_pre_start_snapshot();
    assert!(pending.len() <= MAX_PENDING_PRE_START_TRANSACTIONS);
    assert_eq!(
        pending
            .get(&noisy)
            .expect("the noisy transaction is the most recent, so it survives eviction")
            .len(),
        MAX_PENDING_PRE_START_EVENTS_PER_TRANSACTION,
        "the per-transaction cap must drop the oldest, not grow"
    );
    assert!(
        pending
            .values()
            .all(|events| events.len() <= MAX_PENDING_PRE_START_EVENTS_PER_TRANSACTION)
    );
}

#[test]
fn completed_transaction_never_rebuffers_a_late_pre_start_event() {
    let ctx = BackendContext::new().expect("test backend context");
    let transaction_id = TransactionId::from_raw(640);

    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::ExtractionStarted {
            transaction_id,
            source_path: None,
            file_name: None,
            workshop_id: None,
            request_id: None,
        })
        .handled_event()
    );
    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Finished {
                id: transaction_id,
                payload: TransactionPayload::None,
            },
        ))
        .handled_event()
    );
    assert!(ctx.transaction_tasks.is_completed(transaction_id));

    let late = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
        TransactionRuntimeEvent::Status {
            id: transaction_id,
            status: TransactionStatus::Locating,
        },
    ));
    assert!(!late.handled_event());
    assert!(
        !ctx.transaction_tasks
            .pending_pre_start_snapshot()
            .contains_key(&transaction_id)
    );

    let duplicate_start =
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::ExtractionStarted {
            transaction_id,
            source_path: None,
            file_name: None,
            workshop_id: None,
            request_id: None,
        });
    assert!(duplicate_start.handled_event());
    assert!(duplicate_start.into_actions().is_empty());
    assert!(!ctx.transaction_tasks.is_active(transaction_id));
    assert!(ctx.transaction_tasks.is_completed(transaction_id));
}

#[test]
fn completed_transaction_tombstones_are_bounded() {
    let ctx = BackendContext::new().expect("test backend context");

    for offset in 0..(MAX_COMPLETED_TRANSACTION_TOMBSTONES + 4) {
        let effects = ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Finished {
                id: TransactionId::from_raw(
                    20_000 + u32::try_from(offset).expect("test offset fits u32"),
                ),
                payload: TransactionPayload::None,
            },
        ));
        assert!(!effects.handled_event());
    }

    assert_eq!(
        ctx.transaction_tasks.completed_count(),
        MAX_COMPLETED_TRANSACTION_TOMBSTONES
    );
}

/// `game_paths` reports the configured Garry's Mod path beside the one that
/// actually resolved, and callers tell "never set up" from "set up and since
/// broken" by comparing them. That only works if the pair comes from one
/// authoritative snapshot — read from two locks, a reader racing a save could
/// see a new setting beside the old resolution and call a working install
/// broken.
#[test]
fn a_settings_save_publishes_the_configured_and_resolved_game_paths_together() {
    let temp = tempfile::tempdir().expect("tempdir");
    let gmod_dir = temp.path().join("GarrysMod");
    std::fs::create_dir_all(&gmod_dir).expect("gmod dir");
    let services = BackendServices::for_test();

    assert_eq!(services.config().game_paths(), (None, None));

    services
        .config()
        .update_settings_snapshot(|settings| {
            settings.backend.gmod = Some(gmod_dir.clone());
        })
        .expect("settings should persist");

    let (configured, resolved) = services.config().game_paths();
    assert_eq!(configured.as_deref(), Some(gmod_dir.as_path()));
    assert_eq!(
        resolved.as_deref(),
        Some(gmod_dir.as_path()),
        "the resolved path must reflect the settings published in the same write"
    );
    // The same pairing, whichever accessor a caller reaches for.
    let (settings, paths) = services.config().settings_and_paths_snapshot();
    assert_eq!(settings.backend.gmod, configured);
    assert_eq!(paths.gmod_dir, resolved);
}

#[test]
fn downloader_workshop_submission_requires_connected_steam_runtime() {
    let services = BackendServices::for_test();

    assert_eq!(
        services
            .workshop()
            .submit_downloads(vec![PublishedFileId::fixture(123)])
            .map_err(|error| error.to_string()),
        Err("ERR_STEAM_ERROR:STEAM_NOT_CONNECTED".to_owned())
    );
}

#[test]
fn workshop_metadata_refresh_requires_connected_steam_runtime() {
    let services = BackendServices::for_test();

    assert_eq!(
        services
            .workshop()
            .refresh_metadata(&[PublishedFileId::fixture(123)])
            .map_err(|error| error.to_string()),
        Err("ERR_STEAM_ERROR:STEAM_NOT_CONNECTED".to_owned())
    );
    assert_eq!(
        services
            .workshop()
            .item_details(PublishedFileId::fixture(123))
            .map_err(|error| error.to_string()),
        Err("ERR_STEAM_ERROR:STEAM_NOT_CONNECTED".to_owned())
    );
    assert_eq!(
        services
            .workshop()
            .user_details(SteamId::new(76561198000000000))
            .map_err(|error| error.to_string()),
        Err("ERR_STEAM_ERROR:STEAM_NOT_CONNECTED".to_owned())
    );
}

#[test]
fn my_workshop_page_requires_connected_steam_runtime() {
    let services = BackendServices::for_test();

    assert_eq!(
        services
            .workshop()
            .browse_my_page(1)
            .map_err(|error| error.to_string()),
        Err("ERR_STEAM_ERROR:STEAM_NOT_CONNECTED".to_owned())
    );
    assert_eq!(
        services
            .workshop()
            .refresh_subscription_counts(1)
            .map_err(|error| error.to_string()),
        Err("ERR_STEAM_ERROR:STEAM_NOT_CONNECTED".to_owned())
    );
}

#[test]
fn my_workshop_zero_page_stats_refresh_is_noop_without_steam() {
    let services = BackendServices::for_test();

    assert_eq!(
        services.workshop().refresh_subscription_counts(0),
        Ok(HashMap::new())
    );
}

#[test]
fn publish_submit_request_maps_default_preview_to_backend_submission() {
    let request = PublishSubmitRequest {
        mode: PublishSubmitMode::New,
        content_source_path: PathBuf::from("/tmp/source-addon"),
        title: "Boundary Proof".to_owned(),
        addon_type: "tool".to_owned(),
        tags: vec!["fun".to_owned()],
        changelog: None,
        description: None,
        preview: Some(PublishSubmitPreview::Default(PathBuf::from(
            "/tmp/request-default-icon.png",
        ))),
        ignore_globs: vec!["materials/private/*.png".to_owned()],
        total_size: 42,
        temp_dir: PathBuf::from("/tmp/app-publish-temp"),
    };

    let submission = publish_submission_from_app_request(request);

    assert_eq!(
        submission.content_path_src,
        PathBuf::from("/tmp/source-addon")
    );
    assert_eq!(submission.icon_path, None);
    assert!(!submission.upscale);
    assert!(matches!(
        submission.mode,
        steam_publishing::PublishSubmissionMode::Create
    ));
    assert_eq!(submission.title, "Boundary Proof");
    assert_eq!(submission.addon_type, "tool");
    assert_eq!(submission.tags, vec!["fun".to_owned()]);
    let settings = submission.settings.expect("publish settings snapshot");
    assert_eq!(settings.temp, Some(PathBuf::from("/tmp/app-publish-temp")));
    assert_eq!(
        settings.ignore_globs,
        vec!["materials/private/*.png".to_owned()]
    );
}

#[test]
fn publish_submit_request_maps_selected_update_preview_to_backend_submission() {
    let request = PublishSubmitRequest {
        mode: PublishSubmitMode::Update {
            workshop_id: PublishedFileId::fixture(987),
        },
        content_source_path: PathBuf::from("/tmp/source-addon"),
        title: "Ignored For Update".to_owned(),
        addon_type: "map".to_owned(),
        tags: vec!["scenic".to_owned()],
        changelog: Some("Updated icon".to_owned()),
        description: None,
        preview: Some(PublishSubmitPreview::Selected(PublishSelectedPreview {
            path: PathBuf::from("/tmp/icon.png"),
            upscale: true,
        })),
        ignore_globs: Vec::new(),
        total_size: 12,
        temp_dir: PathBuf::from("/tmp/app-publish-temp"),
    };

    let submission = publish_submission_from_app_request(request);

    assert_eq!(submission.icon_path, Some(PathBuf::from("/tmp/icon.png")));
    assert!(submission.upscale);
    let steam_publishing::PublishSubmissionMode::Update { id, changes } = &submission.mode else {
        panic!("an update request must project to an update mode");
    };
    assert_eq!(
        *id,
        gmpublished_backend::WorkshopId::new(987).expect("fixture ids are nonzero")
    );
    assert_eq!(changes.as_deref(), Some("Updated icon"));
}

#[test]
fn publish_submit_requires_connected_steam_runtime_and_errors_transaction() {
    let collector = gmpublished_backend::BackendEventCollector::default();
    let services = BackendServices::for_test_with_event_sink(Arc::new(collector.clone()));
    let transaction = services.begin_transaction();
    let transaction_id = transaction.id();

    let result = services.publish().submit(
        PublishSubmitRequest {
            mode: PublishSubmitMode::New,
            content_source_path: PathBuf::from("/tmp/source-addon"),
            title: "Boundary Proof".to_owned(),
            addon_type: "tool".to_owned(),
            tags: vec!["fun".to_owned()],
            changelog: None,
            description: None,
            preview: None,
            ignore_globs: Vec::new(),
            total_size: 0,
            temp_dir: PathBuf::from("/tmp/app-publish-temp"),
        },
        &transaction,
    );

    assert_eq!(
        result.map_err(|error| error.to_string()),
        Err("ERR_STEAM_ERROR:STEAM_NOT_CONNECTED".to_owned())
    );
    let events = collector.drain();
    assert!(events.iter().any(|event| matches!(
        event,
        gmpublished_backend::BackendEvent::Transaction(
            gmpublished_backend::TransactionEvent::Error { id, error }
        ) if *id == transaction_id
            && error.key.as_str() == "ERR_STEAM_ERROR"
            && error.detail.as_deref() == Some("STEAM_NOT_CONNECTED")
    )));
}

#[test]
fn publish_finished_transaction_retires_correlated_task() {
    let ctx = BackendContext::new().expect("test backend context");
    let receiver = ctx
        .task_events
        .take_receiver()
        .expect("task event receiver");
    let task = ctx.create_task(TaskKind::Publish, TransactionStatus::PublishStarting);
    let task_id = ctx.correlate_backend_transaction(TransactionId::from_raw(909), task);

    ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
        TransactionRuntimeEvent::Status {
            id: TransactionId::from_raw(909),
            status: TransactionStatus::PublishPacking,
        },
    ));
    ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
        TransactionRuntimeEvent::Progress {
            id: TransactionId::from_raw(909),
            progress: 5_000,
        },
    ));
    ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
        TransactionRuntimeEvent::Finished {
            id: TransactionId::from_raw(909),
            payload: TransactionPayload::None,
        },
    ));

    assert!(!ctx.is_backend_transaction_active(TransactionId::from_raw(909)));
    let updates = drain_updates(&receiver);
    assert!(updates.iter().any(|(id, update)| {
        *id == task_id
            && matches!(
                update,
                TaskUpdate::Status(status) if *status == TransactionStatus::PublishPacking
            )
    }));
    assert!(updates.iter().any(|(id, update)| {
        *id == task_id && matches!(update, TaskUpdate::Progress(progress) if *progress == 0.5)
    }));
    assert!(
        updates
            .iter()
            .any(|(id, update)| *id == task_id && matches!(update, TaskUpdate::Finished))
    );
}

#[test]
fn quick_search_backend_result_maps_to_app_batch() {
    let mut session = super::super::domain::SearchSession::default();
    let request = session
        .begin_query("needle", SearchMode::Addons)
        .quick_request
        .expect("quick request");
    let backend_item = gmpublished_backend::SearchItem::new(
        gmpublished_backend::SearchItemSource::InstalledAddons(
            PathBuf::from("/tmp/needle.gma"),
            None,
        ),
        "Needle Addon".to_owned(),
        vec!["servercontent".to_owned(), "needle".to_owned()],
        42_u64,
    );
    let batch = search_quick_batch_from_backend(
        &request,
        &gmpublished_backend::QuickSearchResult {
            hits: vec![gmpublished_backend::QuickSearchHit {
                score: 123,
                item: Arc::new(backend_item),
            }],
            has_more: true,
        },
    );

    assert_eq!(batch.key(), request.key());
    assert_eq!(batch.query(), "needle");
    assert_eq!(batch.generation(), request.generation());
    assert!(batch.has_more());
    let (hits, has_more) = session
        .accept_quick_batch(batch)
        .expect("current quick batch")
        .into_parts();

    assert!(has_more);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].score, 123);
    assert_eq!(hits[0].item.label, "Needle Addon");
    assert_eq!(
        hits[0].item.source,
        SearchItemSource::InstalledAddons(PathBuf::from("/tmp/needle.gma"), None)
    );
}

#[test]
fn full_search_transaction_payload_maps_to_app_batch() {
    let mut session = super::super::domain::SearchSession::default();
    let quick_request = session
        .begin_query("needle", SearchMode::Addons)
        .quick_request
        .expect("quick request");
    let quick_batch = SearchQuickBatch::new(
        quick_request.key().clone(),
        vec![SearchHit {
            score: 1,
            item: SearchItem::new(
                SearchItemSource::WorkshopItem(PublishedFileId::fixture(1)),
                "Needle Quick",
                Vec::<String>::new(),
                0,
            ),
        }],
        true,
    );
    session
        .accept_quick_batch(quick_batch)
        .expect("current quick batch");
    let full = session
        .begin_full_search(TaskId::from_raw(77), SearchMode::Addons)
        .expect("full search start");
    let backend_item = gmpublished_backend::SearchItem::new(
        gmpublished_backend::SearchItemSource::InstalledAddons(
            PathBuf::from("/tmp/needle-full.gma"),
            None,
        ),
        "Needle Full".to_owned(),
        vec!["needle".to_owned()],
        42_u64,
    );
    let payload = TransactionPayload::SearchHits(vec![gmpublished_backend::QuickSearchHit {
        score: 456,
        item: Arc::new(backend_item),
    }]);

    let batch = search_full_batch_from_transaction_payload(&full.request, 9, &payload)
        .expect("full batch projection");

    assert_eq!(batch.key(), full.request.key());
    assert_eq!(batch.task_id(), TaskId::from_raw(77));
    assert_eq!(batch.sequence(), 9);
    let hits = batch.to_hits();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].score, 456);
    assert_eq!(hits[0].item.label, "Needle Full");
    assert_eq!(
        hits[0].item.source,
        SearchItemSource::InstalledAddons(PathBuf::from("/tmp/needle-full.gma"), None)
    );
}

#[test]
fn workshop_metadata_cache_projects_live_items_and_skips_dead_items() {
    let services = BackendServices::for_test();
    let live_item = WorkshopItem {
        id: PublishedFileId::fixture(123),
        title: "  Example Addon  ".to_owned(),
        owner: None,
        steamid: Some(SteamId::new(76561198000000000)),
        time_created: 10,
        time_updated: 20,
        description: Some("description".to_owned()),
        score: 0.75,
        tags: vec!["addon".to_owned(), "fun".to_owned()],
        preview_url: Some("  https://example.test/preview.jpg  ".to_owned()),
        subscriptions: 42,
        local_file: None,
        dead: false,
    };
    let dead_item = WorkshopItem::dead(PublishedFileId::fixture(456));

    let metadata = services
        .workshop()
        .cache_items_for_test(&[live_item, dead_item]);

    assert_eq!(
        metadata,
        vec![WorkshopMetadata {
            id: PublishedFileId::fixture(123),
            title: "  Example Addon  ".to_owned(),
            time_created: 10,
            time_updated: 20,
            score: 0.75,
            tags: vec!["addon".to_owned(), "fun".to_owned()],
            preview_url: Some("https://example.test/preview.jpg".to_owned()),
            subscriptions: 42,
            full_description: None,
            owner_steamid: Some(SteamId::new(76561198000000000)),
            thumbhash: None,
        }]
    );

    let (cached, stale) = services.workshop().resolve_metadata(&[
        PublishedFileId::fixture(123),
        PublishedFileId::fixture(456),
        PublishedFileId::fixture(789),
    ]);
    assert_eq!(cached, metadata);
    assert_eq!(
        stale,
        vec![PublishedFileId::fixture(456), PublishedFileId::fixture(789)]
    );
}

#[test]
fn workshop_detail_cache_persists_full_description_across_summary_refreshes() {
    let services = BackendServices::for_test();
    let mut item = live_workshop_item_for_metadata_tests(123);
    item.description = Some("  Full Workshop description  ".to_owned());

    services.workshop().cache_item_details_for_test(&item);
    let id = item.id;
    let cached = services
        .workshop()
        .cached_item_details(id)
        .expect("detail query should populate the detail cache");
    assert_eq!(
        cached.full_description.as_deref(),
        Some("Full Workshop description")
    );
    assert_eq!(cached.owner_steamid, item.steamid);

    item.description = Some("short summary".to_owned());
    services.workshop().cache_items_for_test(&[item]);
    let cached = services
        .workshop()
        .cached_item_details(id)
        .expect("summary refresh should retain full details");
    assert_eq!(
        cached.full_description.as_deref(),
        Some("Full Workshop description")
    );
}

fn live_workshop_item_for_metadata_tests(id: u64) -> WorkshopItem {
    WorkshopItem {
        id: PublishedFileId::fixture(id),
        title: format!("Addon {id}"),
        owner: None,
        steamid: Some(SteamId::new(76561198000000000)),
        time_created: 10,
        time_updated: 20,
        description: Some("description".to_owned()),
        score: 0.75,
        tags: vec!["addon".to_owned()],
        preview_url: Some(format!("https://example.test/{id}.jpg")),
        subscriptions: 42,
        local_file: None,
        dead: false,
    }
}

#[test]
fn workshop_metadata_older_than_ttl_serves_stale_while_marking_for_refresh() {
    let services = BackendServices::for_test();
    let metadata = services
        .workshop()
        .cache_items_for_test(&[live_workshop_item_for_metadata_tests(123)]);

    let (cached, stale) = services
        .workshop()
        .resolve_metadata(&[PublishedFileId::fixture(123)]);
    assert_eq!(cached, metadata);
    assert!(stale.is_empty());

    let aged =
        metadata_snapshot::now_unix_seconds() - metadata_snapshot::METADATA_TTL.as_secs() - 1;
    services
        .workshop()
        .set_metadata_fetched_at_for_test(PublishedFileId::fixture(123), aged);

    // Stale-while-revalidate: the aged entry still renders AND is re-queued
    // for the background refresh.
    let (cached, stale) = services
        .workshop()
        .resolve_metadata(&[PublishedFileId::fixture(123)]);
    assert_eq!(cached, metadata);
    assert_eq!(stale, vec![PublishedFileId::fixture(123)]);
}

#[test]
fn workshop_metadata_snapshot_write_hydrates_across_restart() {
    let temp = tempfile::tempdir().expect("tempdir");
    let snapshot_file = temp.path().join("metadata.snap");

    let mut services = BackendServices::for_test();
    services.set_metadata_snapshot_file_for_test(snapshot_file.clone());
    let written = services
        .workshop()
        .cache_items_for_test(&[live_workshop_item_for_metadata_tests(123)]);
    services.workshop().persist_metadata_cache();
    assert!(snapshot_file.is_file());

    let mut restarted = BackendServices::for_test();
    restarted.set_metadata_snapshot_file_for_test(snapshot_file);
    restarted.workshop().hydrate_metadata_snapshot();

    let (cached, stale) = restarted
        .workshop()
        .resolve_metadata(&[PublishedFileId::fixture(123)]);
    assert_eq!(cached, written);
    assert!(stale.is_empty());
}

#[test]
fn my_workshop_subscription_counts_skip_dead_items() {
    let live_item = WorkshopItem {
        id: PublishedFileId::fixture(123),
        title: "Example Addon".to_owned(),
        owner: None,
        steamid: Some(SteamId::new(76561198000000000)),
        time_created: 10,
        time_updated: 20,
        description: None,
        score: 0.75,
        tags: vec!["addon".to_owned()],
        preview_url: Some("https://example.test/preview.jpg".to_owned()),
        subscriptions: 42,
        local_file: None,
        dead: false,
    };
    let dead_item = WorkshopItem::dead(PublishedFileId::fixture(456));

    assert_eq!(
        subscription_counts_from_items(&[live_item, dead_item]),
        HashMap::from([(PublishedFileId::fixture(123), 42)])
    );
}

#[test]
fn typed_transaction_total_payloads_preserve_upstream_overlay_behavior() {
    let ctx = BackendContext::new().expect("test backend context");
    let receiver = ctx
        .task_events
        .take_receiver()
        .expect("task event receiver");
    let task = ctx.create_task(TaskKind::Download, TransactionStatus::Downloading);
    ctx.correlate_backend_transaction(TransactionId::from_raw(701), task);

    for payload in [
        TransactionPayload::TotalBytes(4096),
        TransactionPayload::ByteSize {
            source: None,
            bytes: 2048,
        },
        TransactionPayload::ByteSize {
            source: Some("Example Addon".to_owned()),
            bytes: 8192,
        },
    ] {
        assert!(
            ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
                TransactionRuntimeEvent::Data {
                    id: TransactionId::from_raw(701),
                    payload
                },
            ))
            .handled_event()
        );
    }

    assert!(
        ctx.handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(
            TransactionRuntimeEvent::Data {
                id: TransactionId::from_raw(701),
                payload: TransactionPayload::WorkshopItem(
                    gmpublished_backend::WorkshopId::new(76561198000000000)
                        .expect("fixture ids are nonzero")
                ),
            },
        ))
        .handled_event()
    );

    let totals = drain_updates(&receiver)
        .into_iter()
        .filter_map(|(_, update)| match update {
            TaskUpdate::Total(total) => Some(total),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(totals, vec![4096, 2048, 8192]);
}

#[test]
fn correlated_local_gma_extraction_updates_task_from_backend_transaction_events() {
    use gmpublished_backend::{ExtractDestination, ExtractOptions, GmaFile, Whitelist};

    let _lock = BACKEND_EVENT_SINK_TEST_LOCK.lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let gma_path = temp.path().join("fixture.gma");
    write_raw_gma(&gma_path, &[("lua/autorun/safe.lua", b"safe")]);

    let ctx = BackendContext::new_with_backend_event_sink_for_test();
    let backend_receiver = ctx
        .backend_events
        .take_receiver()
        .expect("backend event receiver");
    let task_receiver = ctx
        .task_events
        .take_receiver()
        .expect("task event receiver");

    let gma = GmaFile::open(&gma_path).expect("fixture gma");
    let view = gma.view().expect("fixture view");
    let transaction = ctx.begin_transaction();
    let task = ctx.create_task(TaskKind::Extract, TransactionStatus::Extracting);
    ctx.correlate_backend_transaction(transaction.id(), task);

    let extract_dir = temp.path().join("extract");
    ctx.extract_gma_for_test(
        &view,
        &gma,
        ExtractDestination::Directory(extract_dir.clone()),
        ExtractOptions {
            open_after: false,
            whitelist: Whitelist::Ignore,
        },
        &transaction,
    )
    .expect("extract fixture");

    for event in backend_receiver.try_iter() {
        ctx.handle_backend_runtime_event(&event);
    }

    let updates = drain_updates(&task_receiver);
    assert!(
        updates
            .iter()
            .any(|(_, update)| matches!(update, TaskUpdate::Finished))
    );
    assert_eq!(
        std::fs::read_to_string(extract_dir.join("lua/autorun/safe.lua")).expect("extracted file"),
        "safe"
    );
}

fn write_nt_string(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(value.as_bytes());
    bytes.push(0);
}

fn write_raw_gma(path: &Path, entries: &[(&str, &[u8])]) {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"GMAD");
    bytes.push(3);
    bytes.extend_from_slice(&0_u64.to_le_bytes());
    bytes.extend_from_slice(&0_u64.to_le_bytes());
    write_nt_string(&mut bytes, "");
    write_nt_string(&mut bytes, "Task Correlation Fixture");
    write_nt_string(
        &mut bytes,
        r#"{"title":"","type":"tool","tags":["build"],"ignore":[]}"#,
    );
    write_nt_string(&mut bytes, "Author Name");
    bytes.extend_from_slice(&1_i32.to_le_bytes());

    for (index, (entry_path, contents)) in entries.iter().enumerate() {
        bytes.extend_from_slice(&((index + 1) as u32).to_le_bytes());
        write_nt_string(&mut bytes, entry_path);
        bytes.extend_from_slice(&(contents.len() as i64).to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
    }
    bytes.extend_from_slice(&0_u32.to_le_bytes());

    for (_, contents) in entries {
        bytes.extend_from_slice(contents);
    }
    bytes.extend_from_slice(&0_u32.to_le_bytes());

    std::fs::write(path, bytes).expect("write raw gma");
}

/// `status_text`'s fallback arm feeds the status's translation key straight to
/// Fluent, so a status without its own arm doubles as a message id. Missing an
/// entry puts the raw wire string in front of the user.
///
/// Walks [`TransactionStatus::ALL`], which is the whole set by construction —
/// a status cannot reach a task without being a variant, so unlike a list of
/// string constants there is nothing this can fail to know about.
#[test]
fn statuses_used_as_translation_keys_have_catalog_entries() {
    let i18n = crate::i18n::I18n::for_locale(Some("en"));
    // These two render through an arm of their own, against a message that
    // takes byte-progress placeholders rather than their plain label.
    let dedicated_arms = [
        TransactionStatus::PublishPacking,
        TransactionStatus::PublishProcessingIcon,
    ];

    for status in TransactionStatus::ALL.iter().copied() {
        if dedicated_arms.contains(&status) {
            continue;
        }
        let key = status.translation_key();
        assert_ne!(
            i18n.tr(key),
            key,
            "{status} reaches Fluent as a message id and has no catalog entry"
        );
    }
}
