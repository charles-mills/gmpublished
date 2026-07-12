//! VPK directory tests: in-memory fixture builder (the parser is sans-io),
//! covering both payload locations, preloads, and the rejection paths.

use std::collections::BTreeMap;

use vformats::vpk::{VpkError, VpkLocation, parse, sibling_archive_name};
use vformats::{Limits, crc32_ieee};

const VPK_SIGNATURE: u32 = 0x55aa_1234;
const DIR_ARCHIVE_INDEX: u16 = 0x7fff;
const ENTRY_TERMINATOR: u16 = 0xffff;

struct Spec {
    path: &'static str,
    bytes: Vec<u8>,
    preload_len: usize,
    archive_index: u16,
}

impl Spec {
    fn embedded(path: &'static str, bytes: &[u8]) -> Self {
        Self {
            path,
            bytes: bytes.to_vec(),
            preload_len: 0,
            archive_index: DIR_ARCHIVE_INDEX,
        }
    }

    fn external(path: &'static str, bytes: &[u8], archive_index: u16) -> Self {
        Self {
            path,
            bytes: bytes.to_vec(),
            preload_len: 0,
            archive_index,
        }
    }

    fn preload(mut self, preload_len: usize) -> Self {
        self.preload_len = preload_len;
        self
    }
}

/// Builds a directory file plus sibling-archive bodies, in memory.
fn build_vpk(version: u32, specs: &[Spec]) -> (Vec<u8>, BTreeMap<u16, Vec<u8>>) {
    let mut archives = BTreeMap::<u16, Vec<u8>>::new();
    let mut tree = Vec::new();

    // Group by extension, then directory, preserving spec order within.
    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<&Spec>>> = BTreeMap::new();
    let split: Vec<(String, String, String, &Spec)> = specs
        .iter()
        .map(|spec| {
            let (directory, file) = spec.path.rsplit_once('/').unwrap_or((" ", spec.path));
            let directory = if directory.is_empty() { " " } else { directory };
            let (name, extension) = file.rsplit_once('.').unwrap_or((file, " "));
            (
                extension.to_string(),
                directory.to_string(),
                name.to_string(),
                spec,
            )
        })
        .collect();
    for (extension, directory, _, spec) in &split {
        grouped
            .entry(extension.clone())
            .or_default()
            .entry(directory.clone())
            .or_default()
            .push(spec);
    }

    let put_c = |tree: &mut Vec<u8>, s: &str| {
        tree.extend_from_slice(s.as_bytes());
        tree.push(0);
    };
    for (extension, directories) in &grouped {
        put_c(&mut tree, extension);
        for (directory, specs) in directories {
            put_c(&mut tree, directory);
            for spec in specs {
                let (_, file) = spec.path.rsplit_once('/').unwrap_or((" ", spec.path));
                let (name, _) = file.rsplit_once('.').unwrap_or((file, " "));
                let preload = &spec.bytes[..spec.preload_len];
                let external = &spec.bytes[spec.preload_len..];
                let body = archives.entry(spec.archive_index).or_default();
                let entry_offset = u32::try_from(body.len()).unwrap();
                body.extend_from_slice(external);

                put_c(&mut tree, name);
                tree.extend_from_slice(&crc32_ieee(&spec.bytes).to_le_bytes());
                tree.extend_from_slice(&u16::try_from(preload.len()).unwrap().to_le_bytes());
                tree.extend_from_slice(&spec.archive_index.to_le_bytes());
                tree.extend_from_slice(&entry_offset.to_le_bytes());
                tree.extend_from_slice(&u32::try_from(external.len()).unwrap().to_le_bytes());
                tree.extend_from_slice(&ENTRY_TERMINATOR.to_le_bytes());
                tree.extend_from_slice(preload);
            }
            tree.push(0);
        }
        tree.push(0);
    }
    tree.push(0);

    let embedded = archives.remove(&DIR_ARCHIVE_INDEX).unwrap_or_default();
    let mut dir = Vec::new();
    dir.extend_from_slice(&VPK_SIGNATURE.to_le_bytes());
    dir.extend_from_slice(&version.to_le_bytes());
    dir.extend_from_slice(&u32::try_from(tree.len()).unwrap().to_le_bytes());
    if version == 2 {
        dir.extend_from_slice(&u32::try_from(embedded.len()).unwrap().to_le_bytes());
        dir.extend_from_slice(&[0; 12]);
    }
    dir.extend_from_slice(&tree);
    dir.extend_from_slice(&embedded);
    (dir, archives)
}

/// Resolve an entry's full payload the way a real caller would.
fn read_entry(dir_bytes: &[u8], archives: &BTreeMap<u16, Vec<u8>>, path: &str) -> Vec<u8> {
    let vpk = parse(dir_bytes, &Limits::default()).expect("parse");
    let entry = vpk.get(path).expect("entry");
    let chunk = match entry.location {
        VpkLocation::InDirectory { offset, len } => {
            dir_bytes[offset as usize..(offset + len) as usize].to_vec()
        }
        VpkLocation::InArchive {
            archive,
            offset,
            len,
        } => archives[&archive][offset as usize..(offset + len) as usize].to_vec(),
    };
    let payload = entry.assemble(&chunk);
    assert_eq!(crc32_ieee(&payload), entry.crc32, "{path}: crc");
    assert_eq!(payload.len() as u64, entry.size(), "{path}: size");
    payload
}

#[test]
fn reads_v1_and_v2_embedded_entries() {
    for version in [1, 2] {
        let (dir, archives) = build_vpk(
            version,
            &[
                Spec::embedded("materials/example.vmt", b"material"),
                Spec::embedded("scripts/init.lua", b"print('ok')\n"),
            ],
        );
        let vpk = parse(&dir, &Limits::default()).expect("parse");
        assert_eq!(vpk.version(), version);
        assert_eq!(vpk.entries().len(), 2);
        assert_eq!(
            read_entry(&dir, &archives, "materials/example.vmt"),
            b"material"
        );
        assert_eq!(
            read_entry(&dir, &archives, "scripts/init.lua"),
            b"print('ok')\n"
        );
    }
}

#[test]
fn locates_sibling_archive_entries() {
    let (dir, archives) = build_vpk(
        1,
        &[Spec::external(
            "materials/from_sibling.vmt",
            b"sibling-data",
            7,
        )],
    );
    let vpk = parse(&dir, &Limits::default()).expect("parse");
    let entry = vpk.get("materials/from_sibling.vmt").expect("entry");
    assert!(matches!(
        entry.location,
        VpkLocation::InArchive {
            archive: 7,
            offset: 0,
            len: 12
        }
    ));
    assert_eq!(
        read_entry(&dir, &archives, "materials/from_sibling.vmt"),
        b"sibling-data"
    );
    assert_eq!(
        sibling_archive_name("pak01_dir.vpk", 7).as_deref(),
        Some("pak01_007.vpk")
    );
    assert_eq!(sibling_archive_name("pak01.vpk", 7), None);
}

#[test]
fn combines_preload_and_external_bytes() {
    let (dir, archives) = build_vpk(
        1,
        &[Spec::embedded("materials/preloaded.vmt", b"preload+external").preload(8)],
    );
    let vpk = parse(&dir, &Limits::default()).expect("parse");
    let entry = vpk.get("materials/preloaded.vmt").expect("entry");
    assert_eq!(entry.preload, b"preload+");
    assert_eq!(entry.location.len(), 8);
    assert_eq!(
        read_entry(&dir, &archives, "materials/preloaded.vmt"),
        b"preload+external"
    );
}

#[test]
fn reads_root_level_and_extensionless_entries() {
    let (dir, archives) = build_vpk(
        1,
        &[
            Spec::embedded("addon.json", b"{}"),
            Spec::embedded("LICENSE", b"license text"),
        ],
    );
    assert_eq!(read_entry(&dir, &archives, "addon.json"), b"{}");
    assert_eq!(read_entry(&dir, &archives, "LICENSE"), b"license text");
}

#[test]
fn entries_are_sorted_and_lookup_is_exact() {
    let (dir, _) = build_vpk(
        1,
        &[
            Spec::embedded("b/z.txt", b"1"),
            Spec::embedded("a/y.txt", b"2"),
            Spec::embedded("a/x.txt", b"3"),
        ],
    );
    let vpk = parse(&dir, &Limits::default()).expect("parse");
    let paths: Vec<&str> = vpk.entries().iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["a/x.txt", "a/y.txt", "b/z.txt"]);
    assert!(vpk.get("a/x.txt").is_some());
    assert!(
        vpk.get("a/X.txt").is_none(),
        "lookup is exact, not case-folded"
    );
}

#[test]
fn rejects_malformed_directories() {
    assert!(matches!(
        parse(b"not a vpk at all", &Limits::default()),
        Err(VpkError::BadMagic)
    ));

    let (mut dir, _) = build_vpk(1, &[Spec::embedded("a.txt", b"x")]);
    dir[4..8].copy_from_slice(&3u32.to_le_bytes());
    assert!(matches!(
        parse(&dir, &Limits::default()),
        Err(VpkError::UnsupportedVersion(3))
    ));

    // Truncated tree: header claims more tree bytes than exist.
    let mut truncated = Vec::new();
    truncated.extend_from_slice(&VPK_SIGNATURE.to_le_bytes());
    truncated.extend_from_slice(&1u32.to_le_bytes());
    truncated.extend_from_slice(&1000u32.to_le_bytes());
    truncated.extend_from_slice(b"vmt\0 \0file");
    assert!(matches!(
        parse(&truncated, &Limits::default()),
        Err(VpkError::Corrupt)
    ));

    // Missing entry terminator.
    let mut tree = Vec::new();
    for part in ["txt", " ", "readme"] {
        tree.extend_from_slice(part.as_bytes());
        tree.push(0);
    }
    tree.extend_from_slice(&0u32.to_le_bytes()); // crc
    tree.extend_from_slice(&0u16.to_le_bytes()); // preload
    tree.extend_from_slice(&DIR_ARCHIVE_INDEX.to_le_bytes());
    tree.extend_from_slice(&0u32.to_le_bytes()); // offset
    tree.extend_from_slice(&0u32.to_le_bytes()); // length
    tree.extend_from_slice(&0u16.to_le_bytes()); // bad terminator
    let mut bad = Vec::new();
    bad.extend_from_slice(&VPK_SIGNATURE.to_le_bytes());
    bad.extend_from_slice(&1u32.to_le_bytes());
    bad.extend_from_slice(&u32::try_from(tree.len()).unwrap().to_le_bytes());
    bad.extend_from_slice(&tree);
    assert!(matches!(
        parse(&bad, &Limits::default()),
        Err(VpkError::Corrupt)
    ));
}

#[test]
fn rejects_unsafe_entry_paths() {
    let (dir, _) = build_vpk(1, &[Spec::embedded("../evil.txt", b"evil")]);
    assert!(matches!(
        parse(&dir, &Limits::default()),
        Err(VpkError::UnsafePath(_))
    ));
}

#[test]
fn enforces_limits() {
    let (dir, _) = build_vpk(
        1,
        &[
            Spec::embedded("a.txt", b"1"),
            Spec::embedded("b.txt", b"2"),
            Spec::embedded("c.txt", b"3"),
        ],
    );
    let two = Limits {
        max_entries: 2,
        ..Limits::default()
    };
    assert!(matches!(
        parse(&dir, &two),
        Err(VpkError::TooManyEntries { max: 2 })
    ));
    let tiny = Limits {
        max_input_bytes: 8,
        ..Limits::default()
    };
    assert!(matches!(
        parse(&dir, &tiny),
        Err(VpkError::InputTooLarge { .. })
    ));
}

#[test]
fn oversized_declared_entry_is_rejected_at_parse_time() {
    // The parser locates payloads without reading them, so a declared
    // `entry_length` far beyond the entry's real bytes doesn't fail
    // until something tries to read it — unless max_entry_bytes is
    // enforced up front, which this checks.
    let real_bytes = [0u8; 4];
    let (mut dir, _) = build_vpk(1, &[Spec::embedded("a.bin", &real_bytes)]);

    let crc = crc32_ieee(&real_bytes).to_le_bytes();
    let crc_at = dir
        .windows(4)
        .position(|window| window == crc)
        .expect("crc32 bytes present in the tree");
    // crc32(4) + preload_len(2) + archive_index(2) + entry_offset(4)
    let entry_length_at = crc_at + 12;
    let declared: u32 = 100_000_000;
    dir[entry_length_at..entry_length_at + 4].copy_from_slice(&declared.to_le_bytes());

    let tight = Limits {
        max_entry_bytes: 1024,
        ..Limits::default()
    };
    assert!(matches!(
        parse(&dir, &tight),
        Err(VpkError::EntryTooLarge { size, max: 1024 }) if size == u64::from(declared)
    ));
    // Well under the generous default cap, so parsing still succeeds.
    assert!(parse(&dir, &Limits::default()).is_ok());
}

#[test]
fn duplicate_paths_keep_the_last_entry() {
    let (dir, _) = build_vpk(
        1,
        &[
            Spec::embedded("a/dup.txt", b"first"),
            Spec::embedded("a/dup.txt", b"second"),
        ],
    );
    let vpk = parse(&dir, &Limits::default()).expect("parse");
    let duplicates: Vec<_> = vpk
        .entries()
        .iter()
        .filter(|entry| entry.path == "a/dup.txt")
        .collect();
    assert_eq!(duplicates.len(), 1, "duplicates are deduplicated");
    // The two payloads differ in length; the later entry must win.
    let entry = vpk.get("a/dup.txt").expect("entry");
    match entry.location {
        vformats::vpk::VpkLocation::InDirectory { len, .. } => {
            assert_eq!(len, b"second".len() as u64, "the last duplicate wins");
        }
        other @ vformats::vpk::VpkLocation::InArchive { .. } => {
            panic!("unexpected location {other:?}");
        }
    }
}
