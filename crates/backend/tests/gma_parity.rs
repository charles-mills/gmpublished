use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use gmpublished_backend::{
    ExtractDestination, ExtractOptions, ExtractionContext, GmaError, GmaFile, GmaMetadata, GmaView,
    HasErrorKey, Whitelist,
    test_support::{
        AddonWhitelist, AppData, AppDataPaths, BackendEventCollector, NullEventSink, Steam,
        Transactions, is_default_ignored, is_ignored, is_whitelisted_in,
    },
};
use lzma_rust2::{LzmaOptions, LzmaWriter};
use std::sync::Arc;
use tempfile::TempDir;

/// Bundles the service handles that GMA read/write/extract take
/// explicitly. One `Fixture` per test keeps everything private to that
/// test (no shared settings file, no shared Steam instance).
struct Fixture {
    app_data: AppData,
    steam: Steam,
    whitelist: AddonWhitelist,
    transactions: Transactions,
    cpu: gmpublished_backend::CpuExecutor,
    _temp: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let transactions = Transactions::new(Arc::new(NullEventSink));
        let app_data = AppData::load(
            AppDataPaths::for_test_root(temp.path()),
            transactions.clone(),
        );
        Self {
            app_data,
            steam: Steam::new(transactions.clone()),
            whitelist: AddonWhitelist::new(),
            transactions,
            cpu: gmpublished_backend::CpuExecutor::build(2).expect("test CPU executor"),
            _temp: temp,
        }
    }

    /// Same as [`Self::new`], but with a live event sink attached, so a test
    /// exercising a transaction path runs against real emission rather than a
    /// null sink.
    ///
    /// The collector is not retained: packing reports failure by *returning*
    /// an error, and the tests below assert on that. Emission is the publish
    /// layer's job and is covered where it happens.
    fn with_collected_events() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let collector = BackendEventCollector::default();
        let transactions = Transactions::new(Arc::new(collector));
        let app_data = AppData::load(
            AppDataPaths::for_test_root(temp.path()),
            transactions.clone(),
        );
        Self {
            app_data,
            steam: Steam::new(transactions.clone()),
            whitelist: AddonWhitelist::new(),
            transactions,
            cpu: gmpublished_backend::CpuExecutor::build(2).expect("test CPU executor"),
            _temp: temp,
        }
    }

    fn extraction_context(
        &self,
        handle: &GmaFile,
        destination: ExtractDestination,
        options: ExtractOptions,
    ) -> ExtractionContext {
        ExtractionContext::resolve(
            handle,
            destination,
            options,
            &self.whitelist,
            &self.app_data,
            &self.steam,
        )
        .expect("resolve extraction")
    }
}

fn write_nt_string(mut writer: impl Write, value: &str) {
    writer.write_all(value.as_bytes()).unwrap();
    writer.write_all(&[0]).unwrap();
}

fn write_fixture_source(root: &Path) {
    let lua_dir = root.join("lua/autorun");
    fs::create_dir_all(&lua_dir).unwrap();
    fs::write(lua_dir.join("round_trip.lua"), b"print('round trip')\n").unwrap();
    fs::write(lua_dir.join("ignored.lua"), b"print('ignored')\n").unwrap();
}

fn fixture_metadata(title: &str) -> GmaMetadata {
    GmaMetadata::Standard {
        title: title.to_string(),
        addon_type: "tool".to_string(),
        tags: vec!["build".to_string(), "fun".to_string()],
        ignore: vec!["lua/autorun/ignored.lua".to_string()],
    }
}

fn create_fixture_gma(fixture: &Fixture, dir: &TempDir, title: &str) -> PathBuf {
    let source = dir.path().join("source");
    write_fixture_source(&source);

    let gma_path = dir.path().join("fixture.gma");
    let gma = GmaFile::for_creation(gma_path.clone(), fixture_metadata(title));

    let transaction = fixture.transactions.begin();
    gma.create(&source, &transaction, &fixture.whitelist, &fixture.cpu)
        .unwrap();
    transaction.cancel();

    gma_path
}

fn read_generated_gma(path: &Path) -> (GmaFile, GmaView) {
    let gma = GmaFile::open(path).unwrap();
    let view = gma.view().unwrap();
    (gma, view)
}

fn compress_lzma(input: &[u8]) -> Vec<u8> {
    // Match the legacy Workshop payload shape: LZMA-alone header with an
    // unknown (u64::MAX) unpacked size, terminated by an end marker.
    let options = LzmaOptions::with_preset(1);
    let mut encoder = LzmaWriter::new_use_header(Vec::new(), &options, None).unwrap();
    encoder.write_all(input).unwrap();
    encoder.finish().unwrap()
}

fn write_raw_gma(path: &Path, entries: &[(&str, &[u8])]) {
    let mut bytes = Vec::new();
    bytes.write_all(b"GMAD").unwrap();
    bytes.write_all(&[3]).unwrap();
    bytes.write_all(&0u64.to_le_bytes()).unwrap();
    bytes.write_all(&0u64.to_le_bytes()).unwrap();
    write_nt_string(&mut bytes, "");
    write_nt_string(&mut bytes, "Unsafe Entry Fixture");
    write_nt_string(
        &mut bytes,
        r#"{"title":"","type":"tool","tags":["build"],"ignore":[]}"#,
    );
    write_nt_string(&mut bytes, "Author Name");
    bytes.write_all(&1i32.to_le_bytes()).unwrap();

    for (index, (entry_path, contents)) in entries.iter().enumerate() {
        bytes
            .write_all(&((index + 1) as u32).to_le_bytes())
            .unwrap();
        write_nt_string(&mut bytes, entry_path);
        bytes
            .write_all(&(contents.len() as i64).to_le_bytes())
            .unwrap();
        bytes.write_all(&0u32.to_le_bytes()).unwrap();
    }
    bytes.write_all(&0u32.to_le_bytes()).unwrap();

    for (_, contents) in entries {
        bytes.write_all(contents).unwrap();
    }
    bytes.write_all(&0u32.to_le_bytes()).unwrap();

    fs::write(path, bytes).unwrap();
}

#[test]
fn gma_write_read_extract_round_trip_from_generated_fixture() {
    let fixture = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let gma_path = create_fixture_gma(&fixture, &dir, "Round Trip Fixture");

    let (gma, view) = read_generated_gma(&gma_path);
    assert_eq!(gma.version(), 3);
    assert_eq!(gma.metadata().title(), "Round Trip Fixture");
    assert_eq!(gma.metadata().addon_type(), Some("tool"));
    assert_eq!(
        gma.metadata().tags().unwrap(),
        &vec!["build".to_string(), "fun".to_string()]
    );

    let entries = view.entries().unwrap();
    assert!(entries.contains_key("lua/autorun/round_trip.lua"));
    assert!(!entries.contains_key("lua/autorun/ignored.lua"));

    let extract_dir = dir.path().join("extract");
    let transaction = fixture.transactions.begin();
    let context = fixture.extraction_context(
        &gma,
        ExtractDestination::Directory(extract_dir.clone()),
        ExtractOptions {
            open_after: false,
            whitelist: Whitelist::Enforce,
        },
    );
    let extracted_path = view
        .extract(&gma, &transaction, context, &fixture.cpu)
        .unwrap();

    assert_eq!(extracted_path, extract_dir);
    assert_eq!(
        fs::read_to_string(extracted_path.join("lua/autorun/round_trip.lua")).unwrap(),
        "print('round trip')\n"
    );
    assert!(extracted_path.join("addon.json").is_file());
    assert!(!extracted_path.join("lua/autorun/ignored.lua").exists());

    // Trailer CRC is 0 like fastgmad writes; GMod never reads the field and
    // upstream's value was garbage (it hashed the unflushed BufWriter tail).
    let bytes = fs::read(&gma_path).unwrap();
    assert_eq!(&bytes[bytes.len() - 4..], &[0, 0, 0, 0]);
}

#[test]
fn gma_create_streams_large_files_with_correct_crc_and_round_trips() {
    let fixture = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let lua_dir = source.join("lua");
    fs::create_dir_all(&lua_dir).unwrap();

    // Over the 8MB direct-streaming threshold, plus small batched siblings
    // on both sides of it in sort order.
    let big: Vec<u8> = (0..9 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    fs::write(lua_dir.join("big.lua"), &big).unwrap();
    fs::write(lua_dir.join("a_before.lua"), b"print('before')\n").unwrap();
    fs::write(lua_dir.join("z_after.lua"), b"print('after')\n").unwrap();

    let gma_path = dir.path().join("large.gma");
    let gma = GmaFile::for_creation(gma_path.clone(), fixture_metadata("Large Stream Fixture"));
    let transaction = fixture.transactions.begin();
    gma.create(&source, &transaction, &fixture.whitelist, &fixture.cpu)
        .unwrap();
    transaction.cancel();

    let (gma, view) = read_generated_gma(&gma_path);
    let entries = view.entries().unwrap();

    let mut crc32 = crc32fast::Hasher::new();
    crc32.update(&big);
    assert_eq!(entries.get("lua/big.lua").unwrap().crc, crc32.finalize());
    assert_eq!(entries.get("lua/big.lua").unwrap().size, big.len() as u64);

    let extract_dir = dir.path().join("extract");
    let transaction = fixture.transactions.begin();
    let context = fixture.extraction_context(
        &gma,
        ExtractDestination::Directory(extract_dir.clone()),
        ExtractOptions {
            open_after: false,
            whitelist: Whitelist::Enforce,
        },
    );
    view.extract(&gma, &transaction, context, &fixture.cpu)
        .unwrap();
    assert_eq!(fs::read(extract_dir.join("lua/big.lua")).unwrap(), big);
    assert_eq!(
        fs::read_to_string(extract_dir.join("lua/a_before.lua")).unwrap(),
        "print('before')\n"
    );
    assert_eq!(
        fs::read_to_string(extract_dir.join("lua/z_after.lua")).unwrap(),
        "print('after')\n"
    );
}

#[test]
fn gma_create_success_leaves_only_the_final_file_no_temp_debris() {
    let fixture = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let gma_path = create_fixture_gma(&fixture, &dir, "No Debris Fixture");

    let files: Vec<PathBuf> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .collect();
    assert_eq!(
        files,
        vec![gma_path],
        "the destination directory must contain only the final archive, no leftover temp file"
    );
}

#[test]
fn gma_create_failure_leaves_nothing_at_the_final_path() {
    let fixture = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    write_fixture_source(&source);

    // Block the rename target: a pre-existing directory at the final path
    // makes the closing `fs::rename` fail after packing has done real work,
    // proving a late failure still leaves nothing usable at the final path.
    let gma_path = dir.path().join("blocked.gma");
    fs::create_dir_all(&gma_path).unwrap();

    let gma = GmaFile::for_creation(gma_path.clone(), fixture_metadata("Blocked Fixture"));

    let transaction = fixture.transactions.begin();
    let result = gma.create(&source, &transaction, &fixture.whitelist, &fixture.cpu);
    transaction.cancel();

    assert!(result.is_err());
    assert!(
        gma_path.is_dir() && fs::read_dir(&gma_path).unwrap().next().is_none(),
        "the pre-existing directory at the final path must be untouched"
    );

    let temp_debris: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name != "source" && name != "blocked.gma")
        .collect();
    assert!(
        temp_debris.is_empty(),
        "expected no leftover temp files, found {temp_debris:?}"
    );
}

#[test]
#[cfg(unix)]
fn gma_create_walk_error_fails_the_pack_and_leaves_no_final_file() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::with_collected_events();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let locked = source.join("locked");
    fs::create_dir_all(&locked).unwrap();
    fs::write(locked.join("secret.lua"), b"print('locked')\n").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    let gma_path = dir.path().join("walk-error.gma");
    let gma = GmaFile::for_creation(gma_path.clone(), fixture_metadata("Walk Error Fixture"));

    let transaction = fixture.transactions.begin();
    let result = gma.create(&source, &transaction, &fixture.whitelist, &fixture.cpu);
    transaction.cancel();

    // Restore permissions so the tempdir can clean itself up regardless of
    // whether the assertions below pass.
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(!gma_path.exists());

    // The path travels on the error, not alongside it: whoever receives this
    // can report it, and a caller that reports nothing loses nothing that was
    // already reported behind its back.
    let error = result.expect_err("an unreadable directory must fail the pack");
    assert!(
        matches!(&error, GmaError::PathIo { path } if path.starts_with(&source)),
        "expected a path-naming error for the unreadable directory, got {error:?}"
    );
    assert_eq!(error.error_key().as_str(), "ERR_PATH_IO_ERROR");
    assert!(
        error
            .error_detail()
            .is_some_and(|detail| detail.contains("locked")),
        "the failing path must survive into the reportable detail"
    );
}

#[test]
// APFS (macOS) rejects invalid-UTF8 names at the filesystem level, so this
// fixture can only be materialized on filesystems that allow arbitrary
// bytes in a filename (ext4 et al.).
#[cfg(target_os = "linux")]
fn gma_create_non_utf8_entry_name_errors_naming_the_path() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let fixture = Fixture::with_collected_events();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir_all(&source).unwrap();

    // Not valid UTF-8: a lone continuation byte.
    let bad_name = OsStr::from_bytes(b"invalid-\x80-name.lua");
    fs::write(source.join(bad_name), b"print('bad name')\n").unwrap();

    let gma_path = dir.path().join("non-utf8.gma");
    let gma = GmaFile::for_creation(gma_path.clone(), fixture_metadata("Non-UTF8 Fixture"));

    let transaction = fixture.transactions.begin();
    let result = gma.create(&source, &transaction, &fixture.whitelist, &fixture.cpu);
    transaction.cancel();

    assert!(!gma_path.exists());

    let error = result.expect_err("a non-UTF8 entry name must fail the pack");
    assert!(
        matches!(&error, GmaError::PathIo { path } if path.starts_with(&source)),
        "expected a path-naming error for the non-UTF8 entry, got {error:?}"
    );
    assert_eq!(error.error_key().as_str(), "ERR_PATH_IO_ERROR");
}

#[test]
fn gma_lzma_bin_decompresses_generated_gma_payload() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::new();
    let gma_path = create_fixture_gma(&fixture, &dir, "LZMA Fixture");
    let encoded = compress_lzma(&fs::read(&gma_path).unwrap());
    let bin_path = dir.path().join("fixture_legacy.bin");
    fs::write(&bin_path, encoded).unwrap();

    let transaction = fixture.transactions.begin();
    let (mut decompressed, view) =
        GmaFile::decompress(&bin_path, &transaction, &fixture.app_data, &fixture.steam).unwrap();
    transaction.cancel();

    assert_eq!(decompressed.metadata().title(), "LZMA Fixture");
    decompressed.set_workshop_id(gmpublished_backend::WorkshopId::new(4242).unwrap());
    assert_eq!(
        decompressed.extracted_name(),
        "lzma_fixture_4242",
        "assigning the downloaded item id must refresh a legacy .bin's extraction name"
    );
    assert!(
        view.entries()
            .unwrap()
            .contains_key("lua/autorun/round_trip.lua")
    );
    assert_eq!(decompressed.size(), fs::metadata(&gma_path).unwrap().len());
}

#[test]
fn gma_lzma_bin_with_known_size_decompresses_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::new();
    let gma_path = create_fixture_gma(&fixture, &dir, "LZMA Sized Fixture");
    let raw = fs::read(&gma_path).unwrap();

    // Valve-style payload: LZMA-alone header carrying the exact unpacked size.
    let options = LzmaOptions::with_preset(1);
    let mut encoder =
        LzmaWriter::new_use_header(Vec::new(), &options, Some(raw.len() as u64)).unwrap();
    encoder.write_all(&raw).unwrap();
    let bin_path = dir.path().join("fixture_sized.bin");
    fs::write(&bin_path, encoder.finish().unwrap()).unwrap();

    let transaction = fixture.transactions.begin();
    let (decompressed, view) =
        GmaFile::decompress(&bin_path, &transaction, &fixture.app_data, &fixture.steam).unwrap();
    transaction.cancel();

    assert!(!view.is_temp_backed());

    assert_eq!(decompressed.metadata().title(), "LZMA Sized Fixture");
    assert_eq!(decompressed.size(), raw.len() as u64);
}

#[test]
fn gma_read_entry_bytes_round_trips_from_disk_archive() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::new();
    let gma_path = create_fixture_gma(&fixture, &dir, "Read Bytes Fixture");
    let (_gma, view) = read_generated_gma(&gma_path);

    let bytes = view
        .read_entry_bytes("lua/autorun/round_trip.lua")
        .expect("entry bytes should read from disk gma");

    assert_eq!(bytes, b"print('round trip')\n");
}

#[test]
fn gma_read_entry_bytes_round_trips_from_lzma_membuffer_archive() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::new();
    let gma_path = create_fixture_gma(&fixture, &dir, "Read Bytes LZMA Fixture");
    let raw = fs::read(&gma_path).unwrap();

    let options = LzmaOptions::with_preset(1);
    let mut encoder =
        LzmaWriter::new_use_header(Vec::new(), &options, Some(raw.len() as u64)).unwrap();
    encoder.write_all(&raw).unwrap();
    let bin_path = dir.path().join("fixture_read_bytes.bin");
    fs::write(&bin_path, encoder.finish().unwrap()).unwrap();

    let transaction = fixture.transactions.begin();
    let (_decompressed, view) =
        GmaFile::decompress(&bin_path, &transaction, &fixture.app_data, &fixture.steam).unwrap();
    transaction.cancel();
    assert!(!view.is_temp_backed());

    let bytes = view
        .read_entry_bytes("lua/autorun/round_trip.lua")
        .expect("entry bytes should read from LZMA membuffer gma");

    assert_eq!(bytes, b"print('round trip')\n");
}

#[test]
fn gma_lzma_bin_with_unknown_size_spills_to_disk_and_cleans_up_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::new();
    let gma_path = create_fixture_gma(&fixture, &dir, "LZMA Spill Fixture");
    let encoded = compress_lzma(&fs::read(&gma_path).unwrap());
    let bin_path = dir.path().join("fixture_spill.bin");
    fs::write(&bin_path, encoded).unwrap();

    let transaction = fixture.transactions.begin();
    let (decompressed, view) =
        GmaFile::decompress(&bin_path, &transaction, &fixture.app_data, &fixture.steam).unwrap();
    transaction.cancel();

    assert!(view.is_temp_backed());
    let spill_path = view
        .temp_backing_path()
        .expect("unknown-size payload should spill to disk")
        .to_path_buf();
    assert!(spill_path.is_file());

    assert_eq!(decompressed.metadata().title(), "LZMA Spill Fixture");

    drop((decompressed, view));
    assert!(!spill_path.exists());
}

#[test]
fn gma_unsafe_entry_paths_are_skipped_without_shifting_following_data() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::new();
    let gma_path = dir.path().join("unsafe-entry.gma");
    write_raw_gma(
        &gma_path,
        &[
            ("../outside.lua", b"evil"),
            ("lua/autorun/safe.lua", b"safe"),
            ("C:\\absolute.lua", b"nope"),
        ],
    );

    let (gma, view) = read_generated_gma(&gma_path);
    let entries = view.entries().unwrap();
    assert!(!entries.contains_key("../outside.lua"));
    assert!(!entries.contains_key("C:\\absolute.lua"));
    // Skipped entries must not shift the payload addressing of what
    // follows them: the safe entry's bytes come back intact.
    assert_eq!(
        view.read_entry_bytes("lua/autorun/safe.lua").unwrap(),
        b"safe"
    );

    let extract_dir = dir.path().join("extract");
    let transaction = fixture.transactions.begin();
    let context = fixture.extraction_context(
        &gma,
        ExtractDestination::Directory(extract_dir.clone()),
        ExtractOptions {
            open_after: false,
            whitelist: Whitelist::Ignore,
        },
    );
    view.extract(&gma, &transaction, context, &fixture.cpu)
        .unwrap();

    assert_eq!(
        fs::read_to_string(extract_dir.join("lua/autorun/safe.lua")).unwrap(),
        "safe"
    );
    assert!(!dir.path().join("outside.lua").exists());
}

#[test]
fn gma_header_projects_full_fields_matching_the_constructed_handle() {
    let dir = tempfile::tempdir().unwrap();
    let gma_path = dir.path().join("header-projection.gma");
    write_raw_gma(&gma_path, &[("lua/autorun/safe.lua", b"safe")]);

    let gma = GmaFile::open(&gma_path).unwrap();
    let header = gma.header().unwrap();

    assert_eq!(header.version, 3);
    assert_eq!(header.timestamp, 0);
    assert_eq!(header.metadata.title(), "Unsafe Entry Fixture");
    assert_eq!(header.metadata.addon_type(), Some("tool"));
    assert_eq!(header.metadata.tags().unwrap(), &vec!["build".to_string()]);
    assert_eq!(header.author, "Author Name");
    assert_eq!(header.addon_version, 1);

    // `header()` independently re-derives fields the handle doesn't keep
    // (timestamp, author, addon_version); its title still agrees with the
    // one `open()` already stamped onto the handle.
    assert_eq!(gma.metadata().title(), header.metadata.title());
}

#[test]
fn gma_whitelist_and_default_ignore_match_expected_paths() {
    let whitelist = AddonWhitelist::new();
    let snapshot = whitelist.snapshot();
    assert!(is_whitelisted_in(&snapshot, "lua/autorun/round_trip.lua"));
    assert!(!is_whitelisted_in(&snapshot, "lua/autorun/round_trip.exe"));
    assert!(is_default_ignored("addon.json"));
    assert!(!is_default_ignored("lua/autorun/round_trip.lua"));
    assert!(is_ignored(
        "lua/autorun/ignored.lua",
        &["lua/autorun/ignored.lua".to_string()]
    ));
}

/// `data_offset` is a byte offset into the mapped view, produced by pointer
/// arithmetic against the parsed payload. Nothing else re-derives it, so if it
/// is wrong the preview modal serves one entry's bytes for another.
#[test]
fn indexed_entry_offsets_address_their_own_payload() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let gma_path = create_fixture_gma(&fixture, &dir, "Offset Round Trip");

    let bundle = GmaFile::open_meta(&gma_path).unwrap();
    let view = GmaView::open(&gma_path).unwrap();
    let entries = view.entries().unwrap();

    assert!(
        !bundle.entries.is_empty(),
        "fixture should contain at least one entry"
    );
    for indexed in &bundle.entries {
        let by_offset = view
            .read_payload_bytes(indexed.data_offset, indexed.size)
            .unwrap();
        let expected = entries
            .get(&indexed.path)
            .unwrap_or_else(|| panic!("entry {} missing from the map", indexed.path));

        assert_eq!(by_offset.len() as u64, indexed.size);
        assert_eq!(indexed.crc, expected.crc);
        assert_eq!(crc32fast::hash(&by_offset), indexed.crc, "{}", indexed.path);
    }
}
