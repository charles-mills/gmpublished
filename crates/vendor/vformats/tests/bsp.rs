//! BSP container increment tests: hand-built headers, entity lumps,
//! and pakfile ZIPs (STORE, unsupported-method, and LZMA paths).

use std::borrow::Cow;

use vformats::bsp::{BspError, ZipError, ZipReader, lump_ids, parse};
use vformats::{Limits, crc32_ieee};

const HEADER_BYTES: usize = 4 + 4 + 64 * 16 + 4;

fn build_bsp(version: i32, lumps: &[(usize, &[u8], i32)]) -> Vec<u8> {
    let mut b = vec![0u8; HEADER_BYTES];
    b[0..4].copy_from_slice(b"VBSP");
    b[4..8].copy_from_slice(&version.to_le_bytes());
    b[HEADER_BYTES - 4..].copy_from_slice(&42i32.to_le_bytes()); // revision
    for (index, data, lump_version) in lumps {
        let offset = b.len();
        let entry = 8 + index * 16;
        b[entry..entry + 4].copy_from_slice(&(offset as i32).to_le_bytes());
        b[entry + 4..entry + 8].copy_from_slice(&(data.len() as i32).to_le_bytes());
        b[entry + 8..entry + 12].copy_from_slice(&lump_version.to_le_bytes());
        b.extend_from_slice(data);
    }
    b
}

/// A ZIP with the given (path, method, compressed payload,
/// uncompressed size, crc) entries, plus an EOCD trailing comment to
/// exercise the backwards scan.
fn build_zip(entries: &[(&str, u16, &[u8], u32, u32)], comment: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    let mut locals = Vec::new();
    for (path, method, payload, uncompressed, crc) in entries {
        locals.push(b.len());
        b.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        b.extend_from_slice(&[20, 0, 0, 0]); // version, flags
        b.extend_from_slice(&method.to_le_bytes());
        b.extend_from_slice(&[0; 4]); // time, date
        b.extend_from_slice(&crc.to_le_bytes());
        b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        b.extend_from_slice(&uncompressed.to_le_bytes());
        b.extend_from_slice(&(path.len() as u16).to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes()); // extra
        b.extend_from_slice(path.as_bytes());
        b.extend_from_slice(payload);
    }
    let directory = b.len();
    for ((path, method, payload, uncompressed, crc), local) in entries.iter().zip(&locals) {
        b.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        b.extend_from_slice(&[20, 0, 20, 0, 0, 0]); // versions, flags
        b.extend_from_slice(&method.to_le_bytes());
        b.extend_from_slice(&[0; 4]); // time, date
        b.extend_from_slice(&crc.to_le_bytes());
        b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        b.extend_from_slice(&uncompressed.to_le_bytes());
        b.extend_from_slice(&(path.len() as u16).to_le_bytes());
        b.extend_from_slice(&[0; 6]); // extra len, comment len, disk
        b.extend_from_slice(&[0; 6]); // internal + external attributes
        b.extend_from_slice(&(*local as u32).to_le_bytes());
        b.extend_from_slice(path.as_bytes());
    }
    let directory_size = b.len() - directory;
    b.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    b.extend_from_slice(&[0; 4]); // disk numbers
    b.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    b.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    b.extend_from_slice(&(directory_size as u32).to_le_bytes());
    b.extend_from_slice(&(directory as u32).to_le_bytes());
    b.extend_from_slice(&(comment.len() as u16).to_le_bytes());
    b.extend_from_slice(comment);
    b
}

fn store_entry<'a>(path: &'a str, payload: &'a [u8]) -> (&'a str, u16, &'a [u8], u32, u32) {
    (path, 0, payload, payload.len() as u32, crc32_ieee(payload))
}

#[test]
fn container_lumps_entities_and_pakfile_round_trip() {
    let entities = b"{\n\"classname\" \"worldspawn\"\n\"skyname\" \"sky_day01_01\"\n}\n{\n\"classname\" \"info_player_start\"\n\"origin\" \"0 0 64\"\n}\n\0";
    let zip = build_zip(
        &[
            store_entry("materials/custom/wall.vmt", b"\"LightmappedGeneric\"{}"),
            store_entry("sound/custom/door.wav", b"RIFFdata"),
        ],
        b"embedded by test",
    );
    let planes = [7u8; 20];
    let bytes = build_bsp(
        20,
        &[
            (lump_ids::ENTITIES, entities, 0),
            (lump_ids::PAKFILE, &zip, 0),
            (lump_ids::PLANES, &planes, 1),
        ],
    );
    let limits = Limits::default();
    let bsp = parse(&bytes, &limits).expect("parse");

    assert_eq!(bsp.version(), 20);
    assert_eq!(bsp.map_revision(), 42);
    assert_eq!(bsp.lump(lump_ids::PLANES), Some(&planes[..]));
    assert_eq!(bsp.lump_version(lump_ids::PLANES), Some(1));
    assert_eq!(bsp.lump(lump_ids::VERTICES), Some(&[][..]));
    assert_eq!(bsp.lump(64), None);

    let ents = bsp.entities(&limits).expect("entities");
    assert_eq!(ents.len(), 2);
    assert_eq!(ents[0].get_str("classname"), Some("worldspawn"));
    assert_eq!(ents[0].get_str("SKYNAME"), Some("sky_day01_01"));
    assert_eq!(ents[1].get_str("origin"), Some("0 0 64"));

    let pak = bsp.pakfile().expect("pakfile");
    assert_eq!(pak.entries().len(), 2);
    let entry = pak.get("materials/custom/wall.vmt").expect("entry");
    let data = pak.entry_bytes(entry, &limits).expect("bytes");
    assert!(matches!(data, Cow::Borrowed(_)), "STORE must borrow");
    assert_eq!(&*data, b"\"LightmappedGeneric\"{}");
    assert_eq!(crc32_ieee(&data), entry.crc32);
}

#[test]
fn empty_pakfile_and_missing_entities_are_empty_not_errors() {
    let bytes = build_bsp(19, &[]);
    let limits = Limits::default();
    let bsp = parse(&bytes, &limits).expect("parse");
    assert!(bsp.entities(&limits).expect("entities").is_empty());
    assert!(bsp.pakfile().expect("pakfile").entries().is_empty());
}

#[test]
fn rejects_malformed_containers() {
    let limits = Limits::default();
    assert!(matches!(
        parse(b"IBSP wrong engine", &limits),
        Err(BspError::BadMagic)
    ));

    let bytes = build_bsp(22, &[]);
    assert!(matches!(
        parse(&bytes, &limits),
        Err(BspError::UnsupportedVersion(22))
    ));

    let bytes = build_bsp(20, &[]);
    assert!(matches!(
        parse(&bytes[..500], &limits),
        Err(BspError::Truncated { .. })
    ));

    // Lump length overhanging the end of file is clamped to the bytes
    // present (repacked maps overhang; the engine reads what's there).
    let mut oversized = build_bsp(20, &[(lump_ids::PLANES, &[1, 2, 3], 0)]);
    let entry = 8 + lump_ids::PLANES * 16;
    oversized[entry + 4..entry + 8].copy_from_slice(&1000i32.to_le_bytes());
    let offset =
        u32::from_le_bytes(oversized[entry..entry + 4].try_into().expect("4 bytes")) as usize;
    let clamped = parse(&oversized, &limits).expect("overhanging lump length is clamped");
    assert_eq!(
        clamped.lump(lump_ids::PLANES).expect("planes lump").len(),
        oversized.len() - offset
    );

    // A lump offset past the end of file is still rejected.
    let mut oversized = build_bsp(20, &[(lump_ids::PLANES, &[1, 2, 3], 0)]);
    oversized[entry..entry + 4].copy_from_slice(&1_000_000i32.to_le_bytes());
    assert!(matches!(
        parse(&oversized, &limits),
        Err(BspError::Truncated { .. })
    ));

    // Negative lump offset.
    let mut negative = build_bsp(20, &[(lump_ids::PLANES, &[1, 2, 3], 0)]);
    negative[entry..entry + 4].copy_from_slice(&(-5i32).to_le_bytes());
    assert!(matches!(
        parse(&negative, &limits),
        Err(BspError::CorruptLump { index }) if index == lump_ids::PLANES
    ));
}

#[test]
fn zip_rejects_unsupported_and_malformed_entries() {
    let limits = Limits::default();
    let bzipped = ("scripts/thing.txt", 12u16, &b"BZh9"[..], 0u32, 0u32);
    let zip = build_zip(&[bzipped], b"");
    let bytes = build_bsp(20, &[(lump_ids::PAKFILE, &zip, 0)]);
    let bsp = parse(&bytes, &limits).expect("parse");
    let pak = bsp.pakfile().expect("pakfile");
    assert!(matches!(
        pak.entry_bytes(&pak.entries()[0], &limits),
        Err(ZipError::UnsupportedCompression { method: 12 })
    ));

    // No EOCD anywhere.
    let mut no_eocd = build_zip(&[store_entry("a.txt", b"data")], b"");
    let eocd_at = no_eocd.len() - 22;
    no_eocd[eocd_at..eocd_at + 4].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
    let bytes = build_bsp(20, &[(lump_ids::PAKFILE, &no_eocd, 0)]);
    assert!(matches!(
        parse(&bytes, &limits).expect("parse").pakfile(),
        Err(BspError::Pakfile(ZipError::MissingDirectory))
    ));

    // Entry over the per-entry cap.
    let zip = build_zip(&[store_entry("big.bin", &[0u8; 64])], b"");
    let bytes = build_bsp(20, &[(lump_ids::PAKFILE, &zip, 0)]);
    let bsp = parse(&bytes, &limits).expect("parse");
    let pak = bsp.pakfile().expect("pakfile");
    let tiny = Limits {
        max_entry_bytes: 16,
        ..Limits::default()
    };
    assert!(matches!(
        pak.entry_bytes(&pak.entries()[0], &tiny),
        Err(ZipError::EntryTooLarge { size: 64, max: 16 })
    ));

    // STORE size mismatch between directory and reality.
    let bad = ("a.txt", 0u16, &b"data"[..], 99u32, 0u32);
    let zip = build_zip(&[bad], b"");
    let bytes = build_bsp(20, &[(lump_ids::PAKFILE, &zip, 0)]);
    let bsp = parse(&bytes, &limits).expect("parse");
    let pak = bsp.pakfile().expect("pakfile");
    assert!(matches!(
        pak.entry_bytes(&pak.entries()[0], &limits),
        Err(ZipError::Corrupt)
    ));
}

#[test]
fn zip_entry_bytes_rejects_an_entry_from_a_different_reader() {
    let limits = Limits::default();
    let zip_a = build_zip(&[store_entry("a.txt", b"hello a")], b"");
    let zip_b = build_zip(&[store_entry("b.txt", b"hello b")], b"");
    let reader_a = ZipReader::parse(&zip_a).expect("zip a");
    let reader_b = ZipReader::parse(&zip_b).expect("zip b");

    // Same offsets, different bytes behind them: a foreign entry must
    // be rejected outright, not read against the wrong archive.
    let entry_from_b = reader_b.entries()[0].clone();
    assert!(matches!(
        reader_a.entry_bytes(&entry_from_b, &limits),
        Err(ZipError::ForeignEntry)
    ));
    // The entry still works against the reader that produced it.
    assert_eq!(
        reader_b
            .entry_bytes(&entry_from_b, &limits)
            .expect("own entry reads fine")
            .as_ref(),
        b"hello b"
    );
}

#[cfg(feature = "lzma")]
#[test]
fn zip_lzma_entries_decompress() {
    let payload = b"The quick brown fox jumps over the lazy dog. ".repeat(50);
    // Encode as LZMA-alone, then rewrap as ZIP-flavored LZMA:
    // version(2) + props size(2) + props(5) + raw stream.
    let options = lzma_rust2::LzmaOptions::with_preset(6);
    let mut encoder =
        lzma_rust2::LzmaWriter::new_use_header(Vec::new(), &options, Some(payload.len() as u64))
            .expect("encoder");
    std::io::Write::write_all(&mut encoder, &payload).expect("compress");
    let alone = encoder.finish().expect("finish");
    let mut zip_flavored = vec![20, 0, 5, 0];
    zip_flavored.extend_from_slice(&alone[0..5]); // props + dict size
    zip_flavored.extend_from_slice(&alone[13..]); // raw stream

    let entry = (
        "materials/big.vtf",
        14u16,
        zip_flavored.as_slice(),
        payload.len() as u32,
        crc32_ieee(&payload),
    );
    let zip = build_zip(&[entry], b"");
    let bytes = build_bsp(21, &[(lump_ids::PAKFILE, &zip, 0)]);
    let limits = Limits::default();
    let bsp = parse(&bytes, &limits).expect("parse");
    let pak = bsp.pakfile().expect("pakfile");
    let data = pak
        .entry_bytes(&pak.entries()[0], &limits)
        .expect("decompress");
    assert!(matches!(data, Cow::Owned(_)));
    assert_eq!(&*data, payload.as_slice());
    assert_eq!(crc32_ieee(&data), pak.entries()[0].crc32);
}

/// Real compilers leave arbitrary offsets on zero-length lumps; every
/// accessor must treat them as empty rather than slicing out of range.
#[test]
fn zero_length_lumps_with_garbage_offsets_read_as_empty() {
    let limits = Limits::default();
    let mut bytes = build_bsp(20, &[]);
    for index in [lump_ids::ENTITIES, lump_ids::VERTICES, lump_ids::PAKFILE] {
        let entry = 8 + index * 16;
        bytes[entry..entry + 4].copy_from_slice(&i32::MAX.to_le_bytes());
    }
    let bsp = parse(&bytes, &limits).expect("parse");
    assert_eq!(bsp.lump(lump_ids::VERTICES), Some(&[][..]));
    assert!(bsp.vertices(&limits).expect("vertices").is_empty());
    assert!(bsp.entities(&limits).expect("entities").is_empty());
    assert!(bsp.pakfile().expect("pakfile").entries().is_empty());
}
