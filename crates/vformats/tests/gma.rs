//! GMA tests. The load-bearing one is the byte-level golden fixture:
//! its bytes are spelled out by hand from the wire format, not
//! produced by any implementation — the parser must read them and the
//! writer must reproduce them exactly. Round-trips and rejection paths
//! cover the rest.

use std::borrow::Cow;

use vformats::gma::{GmaEntry, GmaError, GmaMetadata, GmaWriter, parse};
use vformats::{Limits, crc32_ieee};

const LUA: &[u8] = b"print('hi')\n";
const VMT: &[u8] = b"vmt!";

/// A two-entry v3 archive, byte by byte.
fn golden_bytes() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"GMAD"); // magic
    b.push(3); // version
    b.extend_from_slice(&0x123456789u64.to_le_bytes()); // steamid
    b.extend_from_slice(&1_700_000_000u64.to_le_bytes()); // timestamp
    b.push(0); // required-content terminator (empty list)
    b.extend_from_slice(b"Test Addon\0");
    b.extend_from_slice(b"{\"type\":\"tool\"}\0"); // description = addon.json body
    b.extend_from_slice(b"Author Name\0");
    b.extend_from_slice(&1i32.to_le_bytes()); // addon version
    // entry 1
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(b"lua/autorun/test.lua\0");
    b.extend_from_slice(&(LUA.len() as i64).to_le_bytes());
    b.extend_from_slice(&crc32_ieee(LUA).to_le_bytes());
    // entry 2
    b.extend_from_slice(&2u32.to_le_bytes());
    b.extend_from_slice(b"materials/x.vmt\0");
    b.extend_from_slice(&(VMT.len() as i64).to_le_bytes());
    b.extend_from_slice(&crc32_ieee(VMT).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes()); // table terminator
    b.extend_from_slice(LUA);
    b.extend_from_slice(VMT);
    b.extend_from_slice(&0u32.to_le_bytes()); // trailer CRC = 0
    b
}

fn golden_metadata() -> GmaMetadata<'static> {
    GmaMetadata {
        version: 3,
        steamid: 0x123456789,
        timestamp: 1_700_000_000,
        required_content: vec![],
        name: Cow::Borrowed("Test Addon"),
        description: Cow::Borrowed("{\"type\":\"tool\"}"),
        author: Cow::Borrowed("Author Name"),
        addon_version: 1,
    }
}

fn golden_entries() -> Vec<GmaEntry<'static>> {
    vec![
        GmaEntry {
            path: Cow::Borrowed("lua/autorun/test.lua"),
            size: LUA.len() as u64,
            crc32: crc32_ieee(LUA),
        },
        GmaEntry {
            path: Cow::Borrowed("materials/x.vmt"),
            size: VMT.len() as u64,
            crc32: crc32_ieee(VMT),
        },
    ]
}

#[test]
fn parses_the_golden_fixture() {
    let bytes = golden_bytes();
    let gma = parse(&bytes, &Limits::default()).expect("parse");

    assert_eq!(gma.metadata, golden_metadata());
    assert_eq!(gma.entries(), golden_entries());
    assert_eq!(gma.entry_bytes(0).unwrap(), LUA);
    assert_eq!(gma.entry_bytes(1).unwrap(), VMT);
    assert!(matches!(gma.entry_bytes(2), Err(GmaError::NoSuchEntry)));
}

#[test]
fn writer_reproduces_the_golden_fixture_byte_for_byte() {
    let mut writer =
        GmaWriter::new(Vec::new(), &golden_metadata(), &golden_entries()).expect("header");
    // Chunk boundaries deliberately misaligned with entry boundaries.
    let payload = [LUA, VMT].concat();
    for chunk in payload.chunks(5) {
        writer.write_payload(chunk).expect("payload");
    }
    let bytes = writer.finish().expect("finish");
    assert_eq!(bytes, golden_bytes());
}

#[test]
fn missing_trailer_is_tolerated_and_extra_content_lists_parse() {
    let bytes = golden_bytes();
    let no_trailer = &bytes[..bytes.len() - 4];
    assert!(parse(no_trailer, &Limits::default()).is_ok());

    // Version 2 with a non-empty required-content list.
    let mut v2 = Vec::new();
    v2.extend_from_slice(b"GMAD");
    v2.push(2);
    v2.extend_from_slice(&[0; 16]); // steamid + timestamp
    v2.extend_from_slice(b"some/content\0other\0\0");
    v2.extend_from_slice(b"Name\0Desc\0Author\0");
    v2.extend_from_slice(&1i32.to_le_bytes());
    v2.extend_from_slice(&0u32.to_le_bytes()); // empty table
    let gma = parse(&v2, &Limits::default()).expect("v2");
    assert_eq!(gma.metadata.required_content, ["some/content", "other"]);
    assert!(gma.entries().is_empty());

    // Version 1 has no required-content list at all.
    let mut v1 = Vec::new();
    v1.extend_from_slice(b"GMAD");
    v1.push(1);
    v1.extend_from_slice(&[0; 16]);
    v1.extend_from_slice(b"Name\0Desc\0Author\0");
    v1.extend_from_slice(&1i32.to_le_bytes());
    v1.extend_from_slice(&0u32.to_le_bytes());
    let gma = parse(&v1, &Limits::default()).expect("v1");
    assert!(gma.metadata.required_content.is_empty());
    assert_eq!(gma.metadata.name, "Name");

    // Version 0 exists in the wild (gmad accepts any version <= 3) and
    // follows the version-1 layout.
    let mut v0 = v1;
    v0[4] = 0;
    let gma = parse(&v0, &Limits::default()).expect("v0");
    assert_eq!(gma.metadata.version, 0);
    assert_eq!(gma.metadata.name, "Name");
}

#[test]
fn rejects_malformed_archives() {
    assert!(matches!(
        parse(b"LZMA junk here", &Limits::default()),
        Err(GmaError::BadMagic)
    ));

    let mut bad_version = golden_bytes();
    bad_version[4] = 4;
    assert!(matches!(
        parse(&bad_version, &Limits::default()),
        Err(GmaError::UnsupportedVersion(4))
    ));

    // Negative entry size.
    let mut negative = golden_bytes();
    let size_offset = golden_bytes()
        .windows(8)
        .position(|w| w == (LUA.len() as i64).to_le_bytes())
        .unwrap();
    negative[size_offset..size_offset + 8].copy_from_slice(&(-1i64).to_le_bytes());
    assert!(matches!(
        parse(&negative, &Limits::default()),
        Err(GmaError::Corrupt)
    ));

    // Payload extent past end of file.
    let bytes = golden_bytes();
    let truncated = &bytes[..bytes.len() - 8];
    assert!(matches!(
        parse(truncated, &Limits::default()),
        Err(GmaError::Truncated { .. })
    ));

    // Header cut mid-string.
    assert!(matches!(
        parse(
            b"GMAD\x03\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0Name without nul",
            &Limits::default()
        ),
        Err(GmaError::Truncated { .. })
    ));
}

#[test]
fn write_rejects_unsafe_paths_read_flags_them() {
    let evil_entries = vec![GmaEntry {
        path: Cow::Borrowed("../evil.lua"),
        size: 1,
        crc32: 0,
    }];
    assert!(matches!(
        GmaWriter::new(Vec::new(), &GmaMetadata::default(), &evil_entries),
        Err(vformats::gma::GmaWriteError::UnsafePath(_))
    ));

    // The read side tolerates and flags instead (real archives carry
    // such paths; extractors check per entry).
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"GMAD\x03");
    bytes.extend_from_slice(&[0; 16]);
    bytes.push(0);
    bytes.extend_from_slice(b"N\0D\0A\0");
    bytes.extend_from_slice(&1i32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(b"../evil.lua\0");
    bytes.extend_from_slice(&1i64.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.push(b'x');
    let gma = parse(&bytes, &Limits::default()).expect("tolerated");
    assert!(gma.entries()[0].path_is_unsafe());
    assert_eq!(gma.entry_bytes(0).expect("payload"), b"x");
    assert!(parse(&bytes, &Limits::default()).is_ok());

    let one_entry = Limits {
        max_entries: 1,
        ..Limits::default()
    };
    assert!(matches!(
        parse(&golden_bytes(), &one_entry),
        Err(GmaError::TooManyEntries { max: 1 })
    ));

    let small_entries = Limits {
        max_entry_bytes: 4,
        ..Limits::default()
    };
    assert!(matches!(
        parse(&golden_bytes(), &small_entries),
        Err(GmaError::EntryTooLarge { .. })
    ));
}

#[test]
fn writer_enforces_payload_accounting() {
    let entries = golden_entries();
    let mut writer = GmaWriter::new(Vec::new(), &golden_metadata(), &entries).unwrap();
    writer.write_payload(LUA).unwrap();
    assert!(matches!(
        writer.finish(),
        Err(vformats::gma::GmaWriteError::PayloadShortfall { missing }) if missing == VMT.len() as u64
    ));

    let mut writer = GmaWriter::new(Vec::new(), &golden_metadata(), &entries).unwrap();
    writer.write_payload(&[LUA, VMT].concat()).unwrap();
    assert!(matches!(
        writer.write_payload(b"extra"),
        Err(vformats::gma::GmaWriteError::TooMuchPayload)
    ));
}

#[cfg(feature = "lzma")]
mod lzma {
    use super::*;
    use vformats::gma::{LzmaError, decompress, is_lzma_compressed};

    fn compress(bytes: &[u8]) -> Vec<u8> {
        let options = lzma_rust2::LzmaOptions::with_preset(6);
        let mut encoder =
            lzma_rust2::LzmaWriter::new_use_header(Vec::new(), &options, Some(bytes.len() as u64))
                .expect("encoder");
        std::io::Write::write_all(&mut encoder, bytes).expect("compress");
        encoder.finish().expect("finish")
    }

    #[test]
    fn workshop_bin_round_trips_through_decompress() {
        let gma_bytes = golden_bytes();
        let bin = compress(&gma_bytes);

        assert!(is_lzma_compressed(&bin));
        assert!(!is_lzma_compressed(&gma_bytes));

        let restored = decompress(&bin, &Limits::default()).expect("decompress");
        assert_eq!(restored, gma_bytes);
        assert!(parse(&restored, &Limits::default()).is_ok());
    }

    #[test]
    fn decompression_respects_the_output_cap() {
        let gma_bytes = golden_bytes();
        let bin = compress(&gma_bytes);
        let tiny = Limits {
            max_input_bytes: 16,
            ..Limits::default()
        };
        assert!(matches!(
            decompress(&bin, &tiny),
            Err(LzmaError::OutputTooLarge { .. })
        ));
        assert!(matches!(
            decompress(&bin[..5], &Limits::default()),
            Err(LzmaError::TooSmall)
        ));
    }
}

#[cfg(feature = "lzma")]
#[test]
fn decompression_rejects_oversized_dictionaries_up_front() {
    use vformats::gma::{LzmaError, decompress};

    // A 13-byte header declaring the unknown-size sentinel and a ~4GiB
    // dictionary: must be rejected before any allocation happens.
    let mut header = vec![0x5D];
    header.extend_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
    header.extend_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(
        decompress(&header, &vformats::Limits::default()),
        Err(LzmaError::OutputTooLarge { .. })
    ));
}

/// The std Write adapter drives the writer identically to a Vec sink.
#[test]
fn io_sink_writes_the_same_bytes_as_a_vec_sink() {
    use vformats::IoSink;

    let mut writer =
        GmaWriter::new(IoSink(Vec::new()), &golden_metadata(), &golden_entries()).expect("header");
    for index in 0..golden_entries().len() {
        let gma_bytes = golden_bytes();
        let gma = parse(&gma_bytes, &Limits::default()).expect("parse");
        let payload = gma.entry_bytes(index).expect("payload").to_vec();
        writer.write_payload(&payload).expect("payload");
    }
    let sink = writer.finish().expect("finish");
    assert_eq!(sink.0, golden_bytes());
}

/// Path lookup pairs the entry with its payload.
#[test]
fn get_finds_entries_by_path() {
    let bytes = golden_bytes();
    let gma = parse(&bytes, &Limits::default()).expect("parse");
    let first_path = gma.entries()[0].path.clone();
    let (entry, payload) = gma.get(&first_path).expect("entry");
    assert_eq!(entry.path, first_path);
    assert_eq!(payload, gma.entry_bytes(0).expect("payload"));
    assert!(gma.get("no/such/file.lua").is_none());
}
