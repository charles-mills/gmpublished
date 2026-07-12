use super::*;
use crate::appdata::{AppData, AppDataPaths, Settings};
use crate::events::{BackendEvent, BackendEventCollector, TransactionEvent};
use crate::steam::Steam;
use crate::transactions::Transactions;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const FIXTURE_TITLE: &str = "Overwrite Fixture";
const FIXTURE_EXTRACTED_NAME: &str = "overwrite_fixture";

struct Fixture {
    app_data: AppData,
    steam: Steam,
    whitelist: AddonWhitelist,
    transactions: Transactions,
    collector: BackendEventCollector,
    _temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let collector = BackendEventCollector::default();
        let transactions = Transactions::new(Arc::new(collector.clone()), false);
        let app_data = AppData::load(
            AppDataPaths::for_test_root(temp.path()),
            transactions.clone(),
        );
        Self {
            app_data,
            steam: Steam::new(transactions.clone()),
            whitelist: AddonWhitelist::new(),
            transactions,
            collector,
            _temp: temp,
        }
    }

    fn configure_settings(&self, configure: impl FnMut(&mut Settings)) {
        self.app_data.mutate_settings(configure);
    }

    fn extract(
        &self,
        gma: &(GMAFile, GmaView),
        destination: ExtractDestination,
    ) -> Result<PathBuf, GMAError> {
        let (handle, view) = gma;
        let transaction = self.transactions.begin();
        view.extract(
            handle,
            destination,
            &transaction,
            ExtractOptions {
                open_after: false,
                whitelist: Whitelist::Ignore,
            },
            &self.whitelist,
            &self.app_data,
            &self.steam,
        )
    }
}

fn with_overwrite_mode(mode: &ExtractionOverwriteMode) -> Fixture {
    let fixture = Fixture::new();
    fixture.configure_settings(|settings| {
        settings.extract_overwrite_mode = mode.clone();
    });
    fixture
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
    write_nt_string(&mut bytes, FIXTURE_TITLE);
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

fn open_fixture_gma(path: &Path) -> (GMAFile, GmaView) {
    let gma = GMAFile::open(path).expect("fixture gma");
    let view = gma.view().expect("fixture view");
    assert_eq!(gma.extracted_name, FIXTURE_EXTRACTED_NAME);
    (gma, view)
}

fn write_fixture_gma(root: &Path) -> PathBuf {
    let gma_path = root.join("overwrite-fixture.gma");
    write_raw_gma(
        &gma_path,
        &[("lua/autorun/overwrite.lua", b"print('fresh')\n")],
    );
    gma_path
}

fn existing_destination(root: &Path) -> PathBuf {
    let path = root.join(FIXTURE_EXTRACTED_NAME);
    std::fs::create_dir_all(path.join("lua/autorun")).expect("existing destination");
    std::fs::write(path.join("stale.txt"), "stale").expect("stale marker");
    std::fs::write(path.join("lua/autorun/overwrite.lua"), "old").expect("old addon file");
    path
}

#[test]
fn overwrite_mode_removes_existing_destination_before_extracting() {
    let fixture = with_overwrite_mode(&ExtractionOverwriteMode::Overwrite);
    let temp = tempfile::tempdir().expect("tempdir");
    let gma_path = write_fixture_gma(temp.path());
    let gma = open_fixture_gma(&gma_path);
    let extract_root = temp.path().join("extract");
    let existing = existing_destination(&extract_root);

    let extracted = fixture
        .extract(&gma, ExtractDestination::NamedDirectory(extract_root))
        .expect("extract fixture");

    assert_eq!(extracted, existing);
    assert_eq!(
        std::fs::read_to_string(existing.join("lua/autorun/overwrite.lua")).expect("fresh file"),
        "print('fresh')\n"
    );
    // True overwrite: the whole prior destination is gone, not merged into.
    assert!(!existing.join("stale.txt").exists());
    assert_transaction_finished_with_full_progress(&fixture.collector, &existing);
}

#[test]
fn cancelling_before_extraction_stops_it_with_no_finish() {
    let fixture = Fixture::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let gma_path = write_fixture_gma(temp.path());
    let gma = open_fixture_gma(&gma_path);
    let (handle, view) = &gma;
    let destination = temp.path().join("cancelled-extract");

    let transaction = fixture.transactions.begin();
    assert!(transaction.cancel());

    let result = view.extract(
        handle,
        ExtractDestination::Directory(destination.clone()),
        &transaction,
        ExtractOptions {
            open_after: false,
            whitelist: Whitelist::Ignore,
        },
        &fixture.whitelist,
        &fixture.app_data,
        &fixture.steam,
    );

    assert!(matches!(result, Err(GMAError::Cancelled)));
    assert!(!destination.exists());
    assert!(fixture.collector.drain().into_iter().all(|event| !matches!(
        event,
        BackendEvent::Transaction(TransactionEvent::Finished { .. })
    )));
}

#[test]
fn delete_mode_removes_existing_destination_before_extracting() {
    let fixture = with_overwrite_mode(&ExtractionOverwriteMode::Delete);
    let temp = tempfile::tempdir().expect("tempdir");
    let gma_path = write_fixture_gma(temp.path());
    let gma = open_fixture_gma(&gma_path);
    let extract_root = temp.path().join("extract");
    let existing = existing_destination(&extract_root);

    let extracted = fixture
        .extract(&gma, ExtractDestination::NamedDirectory(extract_root))
        .expect("extract fixture");

    assert_eq!(extracted, existing);
    assert_eq!(
        std::fs::read_to_string(existing.join("lua/autorun/overwrite.lua")).expect("fresh file"),
        "print('fresh')\n"
    );
    assert!(!existing.join("stale.txt").exists());
    assert_transaction_finished_with_full_progress(&fixture.collector, &existing);
}

#[test]
fn directory_destination_bypasses_overwrite_cleanup() {
    let fixture = with_overwrite_mode(&ExtractionOverwriteMode::Delete);
    let temp = tempfile::tempdir().expect("tempdir");
    let gma_path = write_fixture_gma(temp.path());
    let gma = open_fixture_gma(&gma_path);
    let directory = temp.path().join("explicit-directory");
    std::fs::create_dir_all(&directory).expect("directory");
    std::fs::write(directory.join("stale.txt"), "stale").expect("stale marker");
    let extracted = fixture
        .extract(&gma, ExtractDestination::Directory(directory.clone()))
        .expect("extract fixture");

    assert_eq!(extracted, directory);
    assert!(directory.join("stale.txt").is_file());
    assert_eq!(
        std::fs::read_to_string(directory.join("lua/autorun/overwrite.lua")).expect("fresh file"),
        "print('fresh')\n"
    );
}

#[test]
fn recycle_cleanup_failure_uses_first_available_suffix_without_claiming_trash_success() {
    let temp = tempfile::tempdir().expect("tempdir");
    let extract_root = temp.path().join("extract");
    let existing = existing_destination(&extract_root);
    let first_suffix = extract_root.join(format!("{FIXTURE_EXTRACTED_NAME} (1)"));
    std::fs::create_dir_all(&first_suffix).expect("first suffix");
    let context = ExtractionAppDataContext {
        temp_dir: temp.path().join("temp-root"),
        downloads_dir: None,
        gmod_dir: None,
        overwrite_mode: ExtractionOverwriteMode::Recycle,
    };

    let prepared = ExtractDestination::NamedDirectory(extract_root.clone())
        .prepare_with_context(FIXTURE_EXTRACTED_NAME, &context, |path, mode| {
            assert_eq!(path, existing);
            assert_eq!(mode, &ExtractionOverwriteMode::Recycle);
            false
        })
        .expect("suffix fallback available");

    assert_eq!(
        prepared,
        extract_root.join(format!("{FIXTURE_EXTRACTED_NAME} (2)"))
    );
    assert!(existing.join("stale.txt").exists());
}

#[test]
fn suffix_exhaustion_errors_instead_of_falling_back_to_parent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let extract_root = temp.path().join("extract");
    let existing = existing_destination(&extract_root);
    for i in 1..=255u16 {
        std::fs::create_dir_all(extract_root.join(format!("{FIXTURE_EXTRACTED_NAME} ({i})")))
            .expect("suffix dir");
    }
    let context = ExtractionAppDataContext {
        temp_dir: temp.path().join("temp-root"),
        downloads_dir: None,
        gmod_dir: None,
        overwrite_mode: ExtractionOverwriteMode::Recycle,
    };

    let result = ExtractDestination::NamedDirectory(extract_root).prepare_with_context(
        FIXTURE_EXTRACTED_NAME,
        &context,
        |path, mode| {
            assert_eq!(path, existing);
            assert_eq!(mode, &ExtractionOverwriteMode::Recycle);
            false
        },
    );

    assert!(matches!(result, Err(GMAError::DestinationUnavailable)));
    // The pre-existing destination is untouched: cleanup failed, so nothing
    // was ever removed, and no fallback path was written into either.
    assert!(existing.join("stale.txt").exists());
}

#[test]
fn destination_roots_extract_under_appdata_temp_downloads_and_addons_paths() {
    let fixture = Fixture::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let gma_path = write_fixture_gma(temp.path());
    let temp_root = temp.path().join("temp-root");
    let downloads_root = temp.path().join("downloads-root");
    let gmod_root = temp.path().join("gmod-root");
    std::fs::create_dir_all(&temp_root).expect("temp root");
    std::fs::create_dir_all(&downloads_root).expect("downloads root");
    std::fs::create_dir_all(gmod_root.join("GarrysMod/addons")).expect("addons root");

    fixture.configure_settings(|settings| {
        settings.temp = Some(temp_root.clone());
        settings.downloads = Some(downloads_root.clone());
        settings.gmod = Some(gmod_root.clone());
        settings.extract_overwrite_mode = ExtractionOverwriteMode::Overwrite;
    });

    let cases = [
        (
            ExtractDestination::Temp,
            temp_root.join(FIXTURE_EXTRACTED_NAME),
        ),
        (
            ExtractDestination::Downloads,
            downloads_root.join(FIXTURE_EXTRACTED_NAME),
        ),
        (
            ExtractDestination::Addons,
            gmod_root
                .join("GarrysMod/addons")
                .join(FIXTURE_EXTRACTED_NAME),
        ),
    ];

    for (destination, expected) in cases {
        let gma = open_fixture_gma(&gma_path);
        let extracted = fixture.extract(&gma, destination).expect("extract fixture");
        assert_eq!(extracted, expected);
        assert_extracted_fixture_contents(&expected);
        assert_transaction_finished_with_full_progress(&fixture.collector, &expected);
    }
}

#[test]
fn explicit_and_named_directory_destinations_preserve_requested_roots() {
    let fixture = Fixture::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let gma_path = write_fixture_gma(temp.path());
    let explicit = temp.path().join("explicit-root");
    let named_root = temp.path().join("named-root");
    std::fs::create_dir_all(&explicit).expect("explicit root");
    std::fs::create_dir_all(&named_root).expect("named root");
    let gma = open_fixture_gma(&gma_path);
    let explicit_extracted = fixture
        .extract(&gma, ExtractDestination::Directory(explicit.clone()))
        .expect("extract fixture");
    assert_eq!(explicit_extracted, explicit);
    assert_extracted_fixture_contents(&explicit);

    let gma = open_fixture_gma(&gma_path);
    let named_extracted = fixture
        .extract(&gma, ExtractDestination::NamedDirectory(named_root.clone()))
        .expect("extract fixture");
    let expected_named = named_root.join(FIXTURE_EXTRACTED_NAME);
    assert_eq!(named_extracted, expected_named);
    assert_extracted_fixture_contents(&expected_named);
}

#[test]
fn extract_entry_uses_appdata_temp_root() {
    let fixture = Fixture::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let gma_path = write_fixture_gma(temp.path());
    let temp_root = temp.path().join("temp-root");
    std::fs::create_dir_all(&temp_root).expect("temp root");

    fixture.configure_settings(|settings| {
        settings.temp = Some(temp_root.clone());
    });

    let (handle, view) = open_fixture_gma(&gma_path);
    let transaction = fixture.transactions.begin();
    let extracted = view
        .extract_entry(
            &handle,
            "lua/autorun/overwrite.lua".to_owned(),
            &transaction,
            false,
            &fixture.app_data,
            &fixture.steam,
        )
        .expect("extract entry");
    let expected = temp_root
        .join("gmpublisher")
        .join(FIXTURE_EXTRACTED_NAME)
        .join("lua/autorun/overwrite.lua");

    assert_eq!(extracted, expected);
    assert_eq!(
        std::fs::read_to_string(&expected).expect("entry contents"),
        "print('fresh')\n"
    );
    assert!(fixture.collector.drain().iter().any(|event| matches!(
        event,
        BackendEvent::Transaction(TransactionEvent::Finished { payload, .. })
            if payload
                == &crate::transactions::TransactionPayload::ExtractedPath(
                    expected.clone()
                )
    )));
}

#[test]
fn partial_entry_failure_reports_error_with_counts() {
    let fixture = Fixture::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let gma_path = temp.path().join("partial-failure.gma");
    write_raw_gma(
        &gma_path,
        &[
            ("lua/autorun/good.lua", b"print('good')\n"),
            ("blocked/bad.lua", b"print('bad')\n"),
        ],
    );
    let gma = GMAFile::open(&gma_path).expect("fixture gma");
    let view = gma.view().expect("fixture view");

    let destination = temp.path().join("partial-failure-dest");
    // "blocked" is a plain file, not a directory: the "blocked/bad.lua"
    // entry can never create its parent directory and fails to write,
    // while the unrelated "good.lua" entry still succeeds.
    std::fs::create_dir_all(&destination).expect("destination");
    std::fs::write(destination.join("blocked"), b"not a directory").expect("blocking file");

    let transaction = fixture.transactions.begin();
    let result = view.extract(
        &gma,
        ExtractDestination::Directory(destination.clone()),
        &transaction,
        ExtractOptions {
            open_after: false,
            whitelist: Whitelist::Ignore,
        },
        &fixture.whitelist,
        &fixture.app_data,
        &fixture.steam,
    );

    match result {
        Err(GMAError::ExtractionFailed {
            extracted,
            failed,
            rejected,
            first_error,
        }) => {
            assert_eq!(extracted, 1);
            assert_eq!(failed, 1);
            assert_eq!(rejected, 0);
            assert!(first_error.is_some());
        }
        other => panic!("expected ExtractionFailed, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(destination.join("lua/autorun/good.lua")).expect("good entry"),
        "print('good')\n"
    );
    assert!(fixture.collector.drain().into_iter().any(|event| matches!(
        event,
        BackendEvent::Transaction(TransactionEvent::Error { .. })
    )));
}

#[test]
fn all_entries_rejected_by_whitelist_reports_error() {
    let fixture = Fixture::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let gma_path = temp.path().join("all-rejected.gma");
    write_raw_gma(&gma_path, &[("malware.exe", b"nope")]);
    let gma = GMAFile::open(&gma_path).expect("fixture gma");
    let view = gma.view().expect("fixture view");
    let destination = temp.path().join("all-rejected-dest");

    let transaction = fixture.transactions.begin();
    let result = view.extract(
        &gma,
        ExtractDestination::Directory(destination.clone()),
        &transaction,
        ExtractOptions {
            open_after: false,
            whitelist: Whitelist::Enforce,
        },
        &fixture.whitelist,
        &fixture.app_data,
        &fixture.steam,
    );

    match result {
        Err(GMAError::ExtractionFailed {
            extracted,
            failed,
            rejected,
            ..
        }) => {
            assert_eq!(extracted, 0);
            assert_eq!(failed, 0);
            assert_eq!(rejected, 1);
        }
        other => panic!("expected ExtractionFailed, got {other:?}"),
    }
    assert!(!destination.join("malware.exe").exists());
}

#[cfg(unix)]
#[test]
fn refuses_to_extract_through_a_pre_existing_symlinked_directory() {
    let fixture = Fixture::new();
    let temp = tempfile::tempdir().expect("tempdir");
    let gma_path = temp.path().join("symlink-entry.gma");
    write_raw_gma(&gma_path, &[("evil/payload.lua", b"print('pwned')\n")]);
    let gma = GMAFile::open(&gma_path).expect("fixture gma");
    let view = gma.view().expect("fixture view");

    let destination = temp.path().join("symlink-dest");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&destination).expect("destination");
    std::fs::create_dir_all(&outside).expect("outside dir");
    std::os::unix::fs::symlink(&outside, destination.join("evil")).expect("plant symlink");

    let transaction = fixture.transactions.begin();
    let result = view.extract(
        &gma,
        ExtractDestination::Directory(destination.clone()),
        &transaction,
        ExtractOptions {
            open_after: false,
            whitelist: Whitelist::Ignore,
        },
        &fixture.whitelist,
        &fixture.app_data,
        &fixture.steam,
    );

    match result {
        Err(GMAError::ExtractionFailed {
            extracted, failed, ..
        }) => {
            assert_eq!(extracted, 0);
            assert_eq!(failed, 1);
        }
        other => panic!("expected ExtractionFailed, got {other:?}"),
    }
    assert!(!outside.join("payload.lua").exists());
    // The symlink itself is left alone; only writing through it is refused.
    assert!(destination.join("evil").is_symlink());
}

#[test]
fn addon_json_exists_before_finished_event_fires() {
    let temp = tempfile::tempdir().expect("tempdir");
    let gma_path = write_fixture_gma(temp.path());
    let gma = open_fixture_gma(&gma_path);
    let extract_root = temp.path().join("ordering-extract");
    let expected_dest = extract_root.join(FIXTURE_EXTRACTED_NAME);

    let addon_json_existed_at_finish = Arc::new(AtomicBool::new(false));
    let saw_finish = Arc::new(AtomicBool::new(false));
    let sink_dest = expected_dest.clone();
    let sink_flag = Arc::clone(&addon_json_existed_at_finish);
    let sink_saw = Arc::clone(&saw_finish);
    let sink: Arc<dyn crate::events::BackendEventSink> = Arc::new(move |event: BackendEvent| {
        if matches!(
            event,
            BackendEvent::Transaction(TransactionEvent::Finished { .. })
        ) {
            sink_flag.store(sink_dest.join("addon.json").is_file(), Ordering::SeqCst);
            sink_saw.store(true, Ordering::SeqCst);
        }
    });

    let app_temp = tempfile::tempdir().expect("appdata tempdir");
    let transactions = Transactions::new(sink, false);
    let app_data = AppData::load(
        AppDataPaths::for_test_root(app_temp.path()),
        transactions.clone(),
    );
    let steam = Steam::new(transactions.clone());
    let whitelist = AddonWhitelist::new();

    let (handle, view) = &gma;
    let transaction = transactions.begin();
    let extracted = view
        .extract(
            handle,
            ExtractDestination::NamedDirectory(extract_root),
            &transaction,
            ExtractOptions {
                open_after: false,
                whitelist: Whitelist::Ignore,
            },
            &whitelist,
            &app_data,
            &steam,
        )
        .expect("extract fixture");

    assert_eq!(extracted, expected_dest);
    assert!(
        saw_finish.load(Ordering::SeqCst),
        "finished event never fired"
    );
    assert!(
        addon_json_existed_at_finish.load(Ordering::SeqCst),
        "addon.json must exist by the time the finished event fires"
    );
}

fn assert_extracted_fixture_contents(extracted_path: &Path) {
    assert_eq!(
        std::fs::read_to_string(extracted_path.join("lua/autorun/overwrite.lua"))
            .expect("fresh file"),
        "print('fresh')\n"
    );
}

fn assert_transaction_finished_with_full_progress(
    collector: &BackendEventCollector,
    extracted_path: &Path,
) {
    let events = collector.drain();
    assert!(events.iter().any(|event| matches!(
        event,
        BackendEvent::Transaction(TransactionEvent::Progress { progress, .. })
            if *progress == 10_000
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        BackendEvent::Transaction(TransactionEvent::Finished { payload, .. })
            if payload
                == &crate::transactions::TransactionPayload::ExtractedPath(
                    extracted_path.to_owned()
                )
    )));
}
