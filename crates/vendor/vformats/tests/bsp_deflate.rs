//! Differential tests for the in-crate inflate: flate2 (a zlib-family
//! implementation) is the encoding oracle, exercised through the BSP
//! pakfile reader. Compression levels 0/1/6/9 select stored, fixed,
//! and dynamic Huffman blocks across payload shapes.

use std::borrow::Cow;
use std::io::Write;

use vformats::bsp::{ZipError, lump_ids, parse};
use vformats::{Limits, crc32_ieee};

const HEADER_BYTES: usize = 4 + 4 + 64 * 16 + 4;

fn build_bsp(lumps: &[(usize, Vec<u8>, i32)]) -> Vec<u8> {
    let mut b = vec![0u8; HEADER_BYTES];
    b[0..4].copy_from_slice(b"VBSP");
    b[4..8].copy_from_slice(&20i32.to_le_bytes());
    for (index, data, version) in lumps {
        let offset = b.len();
        let entry = 8 + index * 16;
        b[entry..entry + 4].copy_from_slice(&(offset as i32).to_le_bytes());
        b[entry + 4..entry + 8].copy_from_slice(&(data.len() as i32).to_le_bytes());
        b[entry + 8..entry + 12].copy_from_slice(&version.to_le_bytes());
        b.extend_from_slice(data);
    }
    b
}

fn build_zip(entries: &[(&str, u16, &[u8], u32, u32)]) -> Vec<u8> {
    let mut b = Vec::new();
    let mut locals = Vec::new();
    for (path, method, payload, uncompressed, crc) in entries {
        locals.push(b.len());
        b.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        b.extend_from_slice(&[20, 0, 0, 0]);
        b.extend_from_slice(&method.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&crc.to_le_bytes());
        b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        b.extend_from_slice(&uncompressed.to_le_bytes());
        b.extend_from_slice(&(path.len() as u16).to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(path.as_bytes());
        b.extend_from_slice(payload);
    }
    let directory = b.len();
    for ((path, method, payload, uncompressed, crc), local) in entries.iter().zip(&locals) {
        b.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        b.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
        b.extend_from_slice(&method.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&crc.to_le_bytes());
        b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        b.extend_from_slice(&uncompressed.to_le_bytes());
        b.extend_from_slice(&(path.len() as u16).to_le_bytes());
        b.extend_from_slice(&[0; 6]);
        b.extend_from_slice(&[0; 6]);
        b.extend_from_slice(&(*local as u32).to_le_bytes());
        b.extend_from_slice(path.as_bytes());
    }
    let directory_size = b.len() - directory;
    b.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    b.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    b.extend_from_slice(&(directory_size as u32).to_le_bytes());
    b.extend_from_slice(&(directory as u32).to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b
}

/// Raw DEFLATE (no zlib wrapper) at the given level.
fn deflate(payload: &[u8], level: u32) -> Vec<u8> {
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
    encoder.write_all(payload).expect("compress");
    encoder.finish().expect("finish")
}

/// Deterministic pseudo-random bytes (no `rand` dep; xorshift).
fn noise(len: usize, mut seed: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        out.push(seed as u8);
    }
    out
}

fn pakfile_round_trip(payload: &[u8], level: u32) {
    let compressed = deflate(payload, level);
    let entry = (
        "materials/thing.vmt",
        8u16,
        compressed.as_slice(),
        payload.len() as u32,
        crc32_ieee(payload),
    );
    let bytes = build_bsp(&[(lump_ids::PAKFILE, build_zip(&[entry]), 0)]);
    let limits = Limits::default();
    let pak = parse(&bytes, &limits)
        .expect("parse")
        .pakfile()
        .expect("pakfile");
    let data = pak
        .entry_bytes(&pak.entries()[0], &limits)
        .expect("inflate");
    if payload.is_empty() {
        assert!(data.is_empty());
    } else {
        assert!(matches!(data, Cow::Owned(_)));
    }
    assert_eq!(&*data, payload, "level {level}, {} bytes", payload.len());
    assert_eq!(crc32_ieee(&data), pak.entries()[0].crc32);
}

#[test]
fn deflate_entries_round_trip_against_flate2() {
    let mut corpus: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"a".to_vec(),
        b"hello world".to_vec(),
        // RLE-heavy: exercises near and overlapping match copies.
        vec![0u8; 4096],
        b"abcabcabcabc".repeat(500),
        // Text-like: dynamic Huffman with a rich literal alphabet.
        b"\"$basetexture\" \"concrete/concretefloor007a\"\n".repeat(200),
        // Incompressible: stored blocks even at high levels.
        noise(70_000, 0x1234_5678),
    ];
    // A long mixed payload crossing the 65535-byte stored-block and
    // 32768-byte window boundaries.
    let mut mixed = noise(100_000, 0x9E37_79B9);
    mixed.extend_from_slice(&vec![7u8; 100_000]);
    mixed.extend_from_slice(b"the quick brown fox ".repeat(5000).as_slice());
    corpus.push(mixed);

    for payload in &corpus {
        // Level 0 emits stored blocks; 1 favors fixed Huffman; 6 and 9
        // emit dynamic blocks with lazy matching.
        for level in [0, 1, 6, 9] {
            pakfile_round_trip(payload, level);
        }
    }
}

#[test]
fn corrupt_deflate_streams_error_not_panic() {
    let payload = b"\"$basetexture\" \"concrete/concretefloor007a\"\n".repeat(50);
    let compressed = deflate(&payload, 6);
    let limits = Limits::default();

    // Every single-byte truncation and a sweep of bit flips must
    // produce either a typed error or (for flips) possibly different
    // output — never a panic or an over-long buffer.
    for cut in 0..compressed.len().min(64) {
        let entry = (
            "a",
            8u16,
            &compressed[..cut],
            payload.len() as u32,
            crc32_ieee(&payload),
        );
        let bytes = build_bsp(&[(lump_ids::PAKFILE, build_zip(&[entry]), 0)]);
        let pak = parse(&bytes, &limits)
            .expect("parse")
            .pakfile()
            .expect("pakfile");
        assert!(
            pak.entry_bytes(&pak.entries()[0], &limits).is_err(),
            "truncation at {cut} should error"
        );
    }
    for at in 0..compressed.len() {
        let mut flipped = compressed.clone();
        flipped[at] ^= 0x10;
        let entry = (
            "a",
            8u16,
            flipped.as_slice(),
            payload.len() as u32,
            crc32_ieee(&payload),
        );
        let bytes = build_bsp(&[(lump_ids::PAKFILE, build_zip(&[entry]), 0)]);
        let pak = parse(&bytes, &limits)
            .expect("parse")
            .pakfile()
            .expect("pakfile");
        if let Ok(data) = pak.entry_bytes(&pak.entries()[0], &limits) {
            assert_eq!(data.len(), payload.len());
        }
    }

    // A declared size the stream disagrees with, both directions.
    for wrong in [payload.len() as u32 - 1, payload.len() as u32 + 1] {
        let entry = ("a", 8u16, compressed.as_slice(), wrong, 0u32);
        let bytes = build_bsp(&[(lump_ids::PAKFILE, build_zip(&[entry]), 0)]);
        let pak = parse(&bytes, &limits)
            .expect("parse")
            .pakfile()
            .expect("pakfile");
        assert_eq!(
            pak.entry_bytes(&pak.entries()[0], &limits),
            Err(ZipError::Decode)
        );
    }
}
