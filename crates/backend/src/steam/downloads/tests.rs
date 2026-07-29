use super::*;
use crate::events::{BackendEvent, BackendEventCollector, TransactionEvent};
use crate::transactions::TransactionId;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::workshop_id::workshop_id as id;

struct InstalledExtractFixture {
    installed: PathBuf,
    extract_root: PathBuf,
    item: WorkshopId,
    expected_extracted: PathBuf,
}

fn downloads_for_test(temp_root: &Path) -> (Arc<Downloads>, BackendEventCollector) {
    let collector = BackendEventCollector::default();
    let transactions = Transactions::new(Arc::new(collector.clone()));
    let app_data = Arc::new(AppData::load(
        crate::appdata::AppDataPaths::for_test_root(temp_root),
        transactions.clone(),
    ));
    let steam = Arc::new(Steam::new(transactions.clone()));
    let whitelist = AddonWhitelist::builtin_only();
    (
        Arc::new(Downloads::new(app_data, steam, whitelist, transactions)),
        collector,
    )
}

fn with_installed_extract_events<T>(
    build: impl FnOnce(&Path) -> InstalledExtractFixture,
    test: impl FnOnce(Arc<Downloads>, BackendEventCollector, InstalledExtractFixture) -> T,
) -> T {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = build(temp.path());
    let (downloads, collector) = downloads_for_test(temp.path());

    test(downloads, collector, fixture)
}

fn installed_fixture(root: &Path, item: WorkshopId) -> InstalledExtractFixture {
    let installed = root.join("installed");
    let extract_root = root.join("extract");
    fs::create_dir_all(&installed).expect("installed dir");
    fs::create_dir_all(&extract_root).expect("extract root");
    InstalledExtractFixture {
        installed,
        extract_root: extract_root.clone(),
        item,
        expected_extracted: extract_root,
    }
}

fn write_installed_gma(
    root: &Path,
    installed: &Path,
    file_name: &str,
    title: &str,
    downloads: &Downloads,
) -> PathBuf {
    let source = root.join(format!("source-{file_name}"));
    fs::create_dir_all(source.join("lua")).expect("source lua dir");
    fs::write(source.join("lua/installed.lua"), "print('installed')\n").expect("source file");

    let gma_path = installed.join(file_name);
    let gma = GmaFile {
        path: gma_path.clone(),
        size: 0,
        id: None,
        metadata: crate::gma::GmaMetadata::Standard {
            title: title.to_owned(),
            addon_type: "servercontent".to_owned(),
            tags: vec!["build".to_owned()],
            ignore: Vec::new(),
        },
        version: 3,
        extracted_name: String::new(),
        modified: None,
    };
    let transaction = downloads.transactions.begin();
    gma.create(&source, &transaction, &downloads.whitelist)
        .expect("write fixture gma");
    transaction.finished(crate::transactions::TransactionPayload::None);
    gma_path
}

fn wait_for_installed_extract_terminal(
    collector: &BackendEventCollector,
    item: WorkshopId,
) -> (TransactionId, Vec<BackendEvent>) {
    let started = Instant::now();
    loop {
        let events = collector.snapshot();
        let transaction_id = events.iter().find_map(|event| match event {
            BackendEvent::ExtractionStarted(event) if event.workshop_id == Some(item) => {
                Some(event.transaction_id)
            }
            _ => None,
        });
        if let Some(transaction_id) = transaction_id {
            let terminal = events.iter().any(|event| {
                matches!(event,
                    BackendEvent::Transaction(TransactionEvent::Finished { id, .. } |
TransactionEvent::Error { id, .. })
                        if *id == transaction_id)
            });
            if terminal {
                return (transaction_id, events);
            }
        }

        if started.elapsed() > Duration::from_secs(5) {
            panic!("timed out waiting for installed extraction events: {events:#?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn assert_extraction_started(
    events: &[BackendEvent],
    transaction_id: TransactionId,
    item: WorkshopId,
    source_path: Option<&std::path::Path>,
) {
    assert!(events.iter().any(|event| matches!(
        event,
        BackendEvent::ExtractionStarted(event)
            if event.transaction_id == transaction_id
                && event.source_path.as_deref() == source_path
                && event.file_name.is_none()
                && event.workshop_id == Some(item)
    )));
}

fn statuses_for(events: &[BackendEvent], transaction_id: TransactionId) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            BackendEvent::Transaction(TransactionEvent::Status { id, status })
                if *id == transaction_id =>
            {
                Some(status.as_str())
            }
            _ => None,
        })
        .collect()
}

fn assert_download_missing_error(events: &[BackendEvent], transaction_id: TransactionId) {
    assert!(events.iter().any(|event| matches!(
        event,
        BackendEvent::Transaction(TransactionEvent::Error { id, error })
            if *id == transaction_id
                && error.key.as_str() == "ERR_DOWNLOAD_MISSING"
                && error.detail.is_none()
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        BackendEvent::Transaction(TransactionEvent::Finished { id, .. })
            if *id == transaction_id
    )));
}

#[test]
fn installed_folder_without_gma_emits_download_missing_error() {
    with_installed_extract_events(
        |root| {
            let fixture = installed_fixture(root, id(424240));
            fs::write(fixture.installed.join("readme.txt"), "not a gma").expect("readme");
            fixture
        },
        |downloads, collector, fixture| {
            downloads.extract(
                fixture.installed,
                fixture.item,
                ExtractDestination::Directory(fixture.extract_root.clone()),
                None,
            );

            let (transaction_id, events) =
                wait_for_installed_extract_terminal(&collector, fixture.item);
            assert_extraction_started(&events, transaction_id, fixture.item, None);
            assert_eq!(statuses_for(&events, transaction_id), vec!["locating"]);
            assert_download_missing_error(&events, transaction_id);
            assert!(
                !fixture
                    .expected_extracted
                    .join("lua/installed.lua")
                    .exists()
            );
        },
    );
}

#[test]
fn installed_folder_with_single_gma_extracts_and_finishes_without_live_steam_client() {
    with_installed_extract_events(
        |root| installed_fixture(root, id(424242)),
        |downloads, collector, fixture| {
            write_installed_gma(
                fixture.installed.parent().unwrap(),
                &fixture.installed,
                "installed.gma",
                "Installed Folder Proof",
                &downloads,
            );
            let installed_gma = fixture.installed.join("installed.gma");
            downloads.extract(
                fixture.installed,
                fixture.item,
                ExtractDestination::Directory(fixture.extract_root.clone()),
                None,
            );

            let (transaction_id, events) =
                wait_for_installed_extract_terminal(&collector, fixture.item);
            assert_extraction_started(&events, transaction_id, fixture.item, Some(&installed_gma));
            assert_eq!(
                statuses_for(&events, transaction_id),
                vec!["locating", "reading_metadata"]
            );
            assert!(events.iter().any(|event| matches!(
                event,
                BackendEvent::Transaction(TransactionEvent::Data { id, payload })
                    if *id == transaction_id
                        && matches!(
                            payload,
                            crate::transactions::TransactionPayload::ByteSize {
                                source: Some(title),
                                bytes
                            } if title == "Installed Folder Proof" && *bytes > 0
                        )
            )));
            assert!(events.iter().any(|event| matches!(
                event,
                BackendEvent::Transaction(TransactionEvent::Progress { id, progress })
                    if *id == transaction_id && *progress == 10_000
            )));
            assert!(events.iter().any(|event| matches!(
                event,
                BackendEvent::Transaction(TransactionEvent::Finished { id, payload })
                    if *id == transaction_id
                        && payload
                            == &crate::transactions::TransactionPayload::ExtractedPath(
                                fixture.expected_extracted.clone()
                            )
            )));
            assert_eq!(
                fs::read_to_string(fixture.expected_extracted.join("lua/installed.lua"))
                    .expect("extracted file"),
                "print('installed')\n"
            );
        },
    );
}

#[test]
fn installed_folder_with_multiple_gma_files_emits_download_missing_error() {
    with_installed_extract_events(
        |root| installed_fixture(root, id(424244)),
        |downloads, collector, fixture| {
            let root = fixture.installed.parent().unwrap().to_path_buf();
            write_installed_gma(
                &root,
                &fixture.installed,
                "first.gma",
                "Installed Folder Proof",
                &downloads,
            );
            write_installed_gma(
                &root,
                &fixture.installed,
                "second.gma",
                "Installed Folder Proof",
                &downloads,
            );

            downloads.extract(
                fixture.installed,
                fixture.item,
                ExtractDestination::Directory(fixture.extract_root.clone()),
                None,
            );

            let (transaction_id, events) =
                wait_for_installed_extract_terminal(&collector, fixture.item);
            assert_extraction_started(&events, transaction_id, fixture.item, None);
            assert_eq!(statuses_for(&events, transaction_id), vec!["locating"]);
            assert_download_missing_error(&events, transaction_id);
            assert!(
                !fixture
                    .expected_extracted
                    .join("lua/installed.lua")
                    .exists()
            );
        },
    );
}

#[test]
fn known_workshop_items_bypass_collection_queries() {
    let mut ids = vec![id(10), id(20), id(30)];
    let workshop_cache = HashSet::from([id(10), id(30)]);

    let state = PossibleCollectionsState::split_initial(&mut ids, Some(&workshop_cache));

    assert_eq!(ids, vec![id(10), id(30)]);
    assert_eq!(state.queue, vec![id(20)]);
}

#[test]
fn unavailable_workshop_cache_queries_every_input_as_possible_collection() {
    let mut ids = vec![id(10), id(20), id(30)];

    let state = PossibleCollectionsState::split_initial(&mut ids, None);

    assert!(ids.is_empty());
    assert_eq!(state.queue, vec![id(10), id(20), id(30)]);
}

#[test]
fn collection_expansion_recurses_and_preserves_upstream_action_order() {
    let mut ids = vec![id(10), id(20)];
    let workshop_cache = HashSet::from([id(10)]);
    let mut state = PossibleCollectionsState::split_initial(&mut ids, Some(&workshop_cache));
    let mut actions = Vec::new();

    while let Some(query) = state.next_query(true) {
        let results = match query.as_slice() {
            [collection] if *collection == id(20) => {
                vec![Some(WorkshopDownloadQueryItem::Collection {
                    children: vec![id(21), id(22)],
                })]
            }
            [first, nested_collection] if *first == id(21) && *nested_collection == id(22) => {
                vec![
                    Some(WorkshopDownloadQueryItem::Item(id(21))),
                    Some(WorkshopDownloadQueryItem::Collection {
                        children: vec![id(23)],
                    }),
                ]
            }
            [item] if *item == id(23) => vec![Some(WorkshopDownloadQueryItem::Item(id(23)))],
            other => panic!("unexpected query batch: {other:?}"),
        };

        actions.extend(state.apply_query_results(&query, results));
    }

    assert_eq!(
        actions,
        vec![
            WorkshopDownloadAction::FetchWorkshopItems(vec![id(21), id(22)]),
            WorkshopDownloadAction::QueueDownload(id(21)),
            WorkshopDownloadAction::FetchWorkshopItems(vec![id(23)]),
            WorkshopDownloadAction::FetchWorkshopItems(vec![id(21)]),
            WorkshopDownloadAction::QueueDownload(id(23)),
            WorkshopDownloadAction::FetchWorkshopItems(vec![id(23)]),
        ]
    );

    let queued = actions
        .iter()
        .filter_map(|action| match action {
            WorkshopDownloadAction::QueueDownload(item) => Some(*item),
            _ => None,
        })
        .chain(ids)
        .collect::<Vec<_>>();
    assert_eq!(queued, vec![id(21), id(23), id(10)]);
}

#[test]
fn missing_collection_query_rows_emit_item_not_found_actions() {
    let mut ids = vec![id(42)];
    let mut state = PossibleCollectionsState::split_initial(&mut ids, None);
    let query = state.next_query(true).expect("initial query");

    let actions = state.apply_query_results(&query, vec![None]);

    assert_eq!(actions, vec![WorkshopDownloadAction::MissingItem(id(42))]);
    assert!(state.next_query(true).is_none());
}

#[test]
fn disconnected_state_stops_collection_expansion_without_dropping_queue() {
    let mut state = PossibleCollectionsState::new(vec![id(1), id(2)]);

    assert!(state.next_query(false).is_none());
    assert_eq!(state.queue, vec![id(1), id(2)]);
}

#[test]
fn pending_batch_append_moves_all_items_in_one_scheduling_batch() {
    let mut downloading = vec![id(1)];
    let mut pending = (2..=31).map(id).collect::<Vec<_>>();

    let batch_len = append_pending_batch(&mut downloading, &mut pending);

    assert_eq!(batch_len, 30);
    assert!(pending.is_empty());
    assert_eq!(downloading, (1..=31).map(id).collect::<Vec<_>>());
}

/// The lost-wakeup regression.
///
/// The shutdown flag lives under the mutex its condvar is paired with, so the
/// park predicate observes it whichever side wins the race. Signalling with a
/// bare `notify_all()` against a flag under a different mutex loses a shutdown
/// landing between the loop-top check and the `wait`, and the thread sleeps
/// until `join_all_within` gives up and detaches it.
#[test]
fn shutdown_landing_before_the_park_is_not_lost() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (downloads, _events) = downloads_for_test(temp.path());

    // Shutdown wins: the latch is set while no thread is parked.
    downloads.wake_watchdog();

    let mut queue = downloads.downloading.lock();
    assert!(
        !queue.should_park(),
        "a latched shutdown must leave the park predicate false"
    );

    // The real park. It must return without waiting out the timeout.
    let result = downloads.watchdog.wait_while_for(
        &mut queue,
        |queue| queue.should_park(),
        Duration::from_secs(5),
    );
    drop(queue);

    assert!(
        !result.timed_out(),
        "the watchdog parked despite an already-latched shutdown"
    );
}

/// The other ordering: the watchdog is already parked when shutdown arrives.
/// This half worked before the fix and is kept so a future change to the park
/// cannot regress it while satisfying the test above.
#[test]
fn a_parked_watchdog_wakes_on_shutdown() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (downloads, _events) = downloads_for_test(temp.path());

    let parked = Arc::clone(&downloads);
    let waiter = std::thread::spawn(move || {
        let result = {
            let mut queue = parked.downloading.lock();
            parked.watchdog.wait_while_for(
                &mut queue,
                |queue| queue.should_park(),
                Duration::from_secs(5),
            )
        };
        !result.timed_out()
    });

    // Long enough that the waiter is parked rather than still starting up; the
    // assertion holds either way, this just keeps the test honest about which
    // ordering it exercises.
    std::thread::sleep(Duration::from_millis(50));
    downloads.wake_watchdog();

    assert!(
        waiter.join().expect("waiter thread"),
        "a parked watchdog was not woken by shutdown"
    );
}

/// `start()` must also release the park — the queue-activity path, distinct
/// from shutdown.
#[test]
fn queued_work_releases_the_park() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (downloads, _events) = downloads_for_test(temp.path());

    assert!(
        downloads.downloading.lock().should_park(),
        "an idle, running watchdog parks"
    );

    downloads.pending.lock().push(Arc::new(DownloadInner {
        item: id(1),
        transaction: downloads.transactions.begin(),
        extract_destination: ExtractDestination::default(),
        request_id: None,
        sent_total: std::sync::atomic::AtomicBool::new(false),
    }));
    downloads.start();

    assert!(
        !downloads.downloading.lock().should_park(),
        "queued work must leave the park predicate false"
    );
}
