use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use tempfile::TempDir;

use super::*;

// Wire-format constants for the fixture builder (the parser itself
// lives in vformats).
const VPK_SIGNATURE: u32 = 0x55aa1234;
const VPK_DIR_ARCHIVE_INDEX: u16 = 0x7fff;
const VPK_ENTRY_TERMINATOR: u16 = 0xffff;

#[derive(Debug, Clone)]
struct FixtureEntrySpec {
    path: String,
    bytes: Vec<u8>,
    preload_len: usize,
    archive_index: u16,
    crc: u32,
}

impl FixtureEntrySpec {
    fn embedded(path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        let bytes = bytes.into();
        Self {
            path: path.into(),
            crc: crc32fast::hash(&bytes),
            bytes,
            preload_len: 0,
            archive_index: VPK_DIR_ARCHIVE_INDEX,
        }
    }

    fn external(path: impl Into<String>, bytes: impl Into<Vec<u8>>, archive_index: u16) -> Self {
        let bytes = bytes.into();
        Self {
            path: path.into(),
            crc: crc32fast::hash(&bytes),
            bytes,
            preload_len: 0,
            archive_index,
        }
    }

    fn preload(mut self, preload_len: usize) -> Self {
        self.preload_len = preload_len;
        self
    }

    fn crc(mut self, crc: u32) -> Self {
        self.crc = crc;
        self
    }
}

#[derive(Debug, Clone)]
struct PreparedEntry {
    extension: String,
    path: String,
    filename: String,
    crc: u32,
    preload: Vec<u8>,
    archive_index: u16,
    entry_offset: u32,
    entry_length: u32,
}

fn write_vpk_fixture(path: &Path, version: u32, entries: Vec<FixtureEntrySpec>) -> PathBuf {
    let mut archive_data = BTreeMap::<u16, Vec<u8>>::new();
    let mut prepared = Vec::new();

    for entry in entries {
        assert!(entry.preload_len <= entry.bytes.len());
        let preload = entry.bytes[..entry.preload_len].to_vec();
        let external = &entry.bytes[entry.preload_len..];
        let data = archive_data.entry(entry.archive_index).or_default();
        let entry_offset = u32::try_from(data.len()).expect("fixture entry offset fits u32");
        data.extend_from_slice(external);

        let (path_component, filename, extension) = split_fixture_path(&entry.path);
        prepared.push(PreparedEntry {
            extension,
            path: path_component,
            filename,
            crc: entry.crc,
            preload,
            archive_index: entry.archive_index,
            entry_offset,
            entry_length: u32::try_from(external.len()).expect("fixture entry length fits u32"),
        });
    }

    let tree = build_fixture_tree(prepared);
    let embedded_data = archive_data
        .remove(&VPK_DIR_ARCHIVE_INDEX)
        .unwrap_or_default();

    let mut bytes = Vec::new();
    bytes.write_all(&VPK_SIGNATURE.to_le_bytes()).unwrap();
    bytes.write_all(&version.to_le_bytes()).unwrap();
    bytes
        .write_all(&u32::try_from(tree.len()).unwrap().to_le_bytes())
        .unwrap();
    if version == 2 {
        bytes
            .write_all(&u32::try_from(embedded_data.len()).unwrap().to_le_bytes())
            .unwrap();
        bytes.write_all(&0u32.to_le_bytes()).unwrap();
        bytes.write_all(&0u32.to_le_bytes()).unwrap();
        bytes.write_all(&0u32.to_le_bytes()).unwrap();
    }
    bytes.extend_from_slice(&tree);
    bytes.extend_from_slice(&embedded_data);

    fs::write(path, bytes).unwrap();

    for (archive_index, data) in archive_data {
        fs::write(numbered_archive_path(path, archive_index), data).unwrap();
    }

    path.to_path_buf()
}

fn split_fixture_path(path: &str) -> (String, String, String) {
    let (directory, file_name) = path.rsplit_once('/').unwrap_or((" ", path));
    let path_component = if directory.is_empty() {
        " ".to_string()
    } else {
        directory.to_string()
    };
    let (filename, extension) = file_name
        .rsplit_once('.')
        .map_or((file_name, " "), |(filename, extension)| {
            (filename, extension)
        });

    (path_component, filename.to_string(), extension.to_string())
}

fn build_fixture_tree(entries: Vec<PreparedEntry>) -> Vec<u8> {
    let mut grouped = BTreeMap::<String, BTreeMap<String, Vec<PreparedEntry>>>::new();
    for entry in entries {
        grouped
            .entry(entry.extension.clone())
            .or_default()
            .entry(entry.path.clone())
            .or_default()
            .push(entry);
    }

    let mut tree = Vec::new();
    for (extension, paths) in grouped {
        write_c_string(&mut tree, &extension);
        for (path, mut entries) in paths {
            entries.sort_by(|left, right| left.filename.cmp(&right.filename));
            write_c_string(&mut tree, &path);
            for entry in entries {
                write_c_string(&mut tree, &entry.filename);
                tree.write_all(&entry.crc.to_le_bytes()).unwrap();
                tree.write_all(&(entry.preload.len() as u16).to_le_bytes())
                    .unwrap();
                tree.write_all(&entry.archive_index.to_le_bytes()).unwrap();
                tree.write_all(&entry.entry_offset.to_le_bytes()).unwrap();
                tree.write_all(&entry.entry_length.to_le_bytes()).unwrap();
                tree.write_all(&VPK_ENTRY_TERMINATOR.to_le_bytes()).unwrap();
                tree.extend_from_slice(&entry.preload);
            }
            tree.write_all(&[0]).unwrap();
        }
        tree.write_all(&[0]).unwrap();
    }
    tree.write_all(&[0]).unwrap();
    tree
}

fn write_c_string(bytes: &mut Vec<u8>, value: &str) {
    bytes.write_all(value.as_bytes()).unwrap();
    bytes.write_all(&[0]).unwrap();
}

fn numbered_archive_path(dir_path: &Path, archive_index: u16) -> PathBuf {
    let file_name = dir_path.file_name().unwrap().to_str().unwrap();
    let prefix = file_name.strip_suffix("_dir.vpk").unwrap();
    dir_path.with_file_name(format!("{prefix}_{archive_index:03}.vpk"))
}

fn write_headered_tree(path: &Path, version: u32, tree: &[u8]) {
    let mut bytes = Vec::new();
    bytes.write_all(&VPK_SIGNATURE.to_le_bytes()).unwrap();
    bytes.write_all(&version.to_le_bytes()).unwrap();
    bytes.write_all(&(tree.len() as u32).to_le_bytes()).unwrap();
    bytes.extend_from_slice(tree);
    fs::write(path, bytes).unwrap();
}

#[test]
fn reads_v1_dir_embedded_entries() {
    let dir = TempDir::new().unwrap();
    let path = write_vpk_fixture(
        &dir.path().join("pak01_dir.vpk"),
        1,
        vec![
            FixtureEntrySpec::embedded("materials/example.vmt", b"material".to_vec())
                .crc(0x12345678),
            FixtureEntrySpec::embedded("scripts/init.lua", b"print('ok')\n".to_vec()),
        ],
    );

    let vpk = VpkFile::open(&path).unwrap();

    assert_eq!(vpk.version, 1);
    assert_eq!(vpk.entries().len(), 2);
    assert_eq!(
        vpk.entries().get("materials/example.vmt").unwrap().crc,
        0x12345678
    );
    assert_eq!(
        vpk.read_entry_bytes("materials/example.vmt").unwrap(),
        b"material"
    );
    assert_eq!(
        vpk.read_entry_bytes("scripts/init.lua").unwrap(),
        b"print('ok')\n"
    );
}

#[test]
fn reads_v2_dir_embedded_entries() {
    let dir = TempDir::new().unwrap();
    let path = write_vpk_fixture(
        &dir.path().join("pak01_dir.vpk"),
        2,
        vec![FixtureEntrySpec::embedded(
            "materials/v2_example.vtf",
            b"texture".to_vec(),
        )],
    );

    let vpk = VpkFile::open(&path).unwrap();

    assert_eq!(vpk.version, 2);
    assert_eq!(
        vpk.read_entry_bytes("materials/v2_example.vtf").unwrap(),
        b"texture"
    );
}

#[test]
fn reads_sibling_archive_entries() {
    let dir = TempDir::new().unwrap();
    let path = write_vpk_fixture(
        &dir.path().join("pak01_dir.vpk"),
        1,
        vec![FixtureEntrySpec::external(
            "materials/from_sibling.vmt",
            b"sibling-data".to_vec(),
            0,
        )],
    );

    let vpk = VpkFile::open(&path).unwrap();

    assert!(dir.path().join("pak01_000.vpk").is_file());
    assert_eq!(
        vpk.read_entry_bytes("materials/from_sibling.vmt").unwrap(),
        b"sibling-data"
    );
}

#[test]
fn combines_preload_and_external_bytes() {
    let dir = TempDir::new().unwrap();
    let path = write_vpk_fixture(
        &dir.path().join("pak01_dir.vpk"),
        1,
        vec![
            FixtureEntrySpec::embedded("materials/preloaded.vmt", b"preload+external".to_vec())
                .preload(8),
        ],
    );

    let vpk = VpkFile::open(&path).unwrap();

    assert_eq!(
        vpk.read_entry_bytes("materials/preloaded.vmt").unwrap(),
        b"preload+external"
    );
    assert_eq!(
        vpk.entries().get("materials/preloaded.vmt").unwrap().size,
        16
    );
}

#[test]
fn reads_root_level_and_extensionless_entries() {
    let dir = TempDir::new().unwrap();
    let path = write_vpk_fixture(
        &dir.path().join("pak01_dir.vpk"),
        1,
        vec![
            FixtureEntrySpec::embedded("addon.json", b"{}".to_vec()),
            FixtureEntrySpec::embedded("LICENSE", b"license text".to_vec()),
        ],
    );

    let vpk = VpkFile::open(&path).unwrap();

    assert_eq!(vpk.read_entry_bytes("addon.json").unwrap(), b"{}");
    assert_eq!(vpk.read_entry_bytes("LICENSE").unwrap(), b"license text");
}

#[test]
fn rejects_bad_signature() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pak01_dir.vpk");
    fs::write(&path, [0_u8; 12]).unwrap();

    assert_eq!(VpkFile::open(&path), Err(VpkError::InvalidHeader));
}

#[test]
fn rejects_unsupported_version() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pak01_dir.vpk");
    write_headered_tree(&path, 3, &[0]);

    assert_eq!(VpkFile::open(&path), Err(VpkError::InvalidHeader));
}

#[test]
fn rejects_truncated_tree() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pak01_dir.vpk");
    write_headered_tree(&path, 1, b"vmt\0 \0file");

    assert_eq!(VpkFile::open(&path), Err(VpkError::FormatError));
}

#[test]
fn rejects_missing_entry_terminator() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("pak01_dir.vpk");
    let mut tree = Vec::new();
    write_c_string(&mut tree, "txt");
    write_c_string(&mut tree, " ");
    write_c_string(&mut tree, "readme");
    tree.write_all(&0u32.to_le_bytes()).unwrap();
    tree.write_all(&0u16.to_le_bytes()).unwrap();
    tree.write_all(&VPK_DIR_ARCHIVE_INDEX.to_le_bytes())
        .unwrap();
    tree.write_all(&0u32.to_le_bytes()).unwrap();
    tree.write_all(&0u32.to_le_bytes()).unwrap();
    tree.write_all(&0u16.to_le_bytes()).unwrap();
    write_headered_tree(&path, 1, &tree);

    assert_eq!(VpkFile::open(&path), Err(VpkError::FormatError));
}

#[test]
fn rejects_unsafe_entry_paths() {
    let dir = TempDir::new().unwrap();
    let path = write_vpk_fixture(
        &dir.path().join("pak01_dir.vpk"),
        1,
        vec![FixtureEntrySpec::embedded("../evil.txt", b"evil".to_vec())],
    );

    assert_eq!(VpkFile::open(&path), Err(VpkError::UnsafePath));
}

#[test]
fn missing_sibling_archive_is_missing_archive() {
    let dir = TempDir::new().unwrap();
    let path = write_vpk_fixture(
        &dir.path().join("pak01_dir.vpk"),
        1,
        vec![FixtureEntrySpec::external(
            "materials/missing.vmt",
            b"missing".to_vec(),
            0,
        )],
    );
    fs::remove_file(dir.path().join("pak01_000.vpk")).unwrap();
    let vpk = VpkFile::open(&path).unwrap();

    assert_eq!(
        vpk.read_entry_bytes("materials/missing.vmt"),
        Err(VpkError::MissingArchive)
    );
}

#[test]
fn unknown_entry_is_entry_not_found() {
    let dir = TempDir::new().unwrap();
    let path = write_vpk_fixture(
        &dir.path().join("pak01_dir.vpk"),
        1,
        vec![FixtureEntrySpec::embedded(
            "materials/known.vmt",
            b"known".to_vec(),
        )],
    );
    let vpk = VpkFile::open(&path).unwrap();

    assert_eq!(
        vpk.read_entry_bytes("materials/unknown.vmt"),
        Err(VpkError::EntryNotFound)
    );
}

#[test]
fn discover_game_vpks_is_depth_limited_and_sorted() {
    let dir = TempDir::new().unwrap();
    let garrysmod = dir.path().join("garrysmod");
    let platform = dir.path().join("platform");
    let sourceengine = dir.path().join("sourceengine");
    let too_deep = dir.path().join("a/b/c/d");
    fs::create_dir_all(&garrysmod).unwrap();
    fs::create_dir_all(&platform).unwrap();
    fs::create_dir_all(&sourceengine).unwrap();
    fs::create_dir_all(&too_deep).unwrap();
    fs::write(garrysmod.join("fallbacks_dir.vpk"), []).unwrap();
    fs::write(garrysmod.join("garrysmod_dir.vpk"), []).unwrap();
    fs::write(garrysmod.join("pak01_dir.vpk"), []).unwrap();
    fs::write(platform.join("platform_misc_dir.vpk"), []).unwrap();
    fs::write(sourceengine.join("content_cstrike_dir.vpk"), []).unwrap();
    fs::write(sourceengine.join("hl2_misc_dir.vpk"), []).unwrap();
    fs::write(sourceengine.join("pak01_003.vpk"), []).unwrap();
    fs::write(too_deep.join("too_deep_dir.vpk"), []).unwrap();

    assert_eq!(
        discover_game_vpks(dir.path()),
        vec![
            garrysmod.join("garrysmod_dir.vpk"),
            sourceengine.join("content_cstrike_dir.vpk"),
            sourceengine.join("hl2_misc_dir.vpk"),
            platform.join("platform_misc_dir.vpk"),
            garrysmod.join("pak01_dir.vpk"),
            garrysmod.join("fallbacks_dir.vpk"),
        ]
    );
}
