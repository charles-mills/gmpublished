//! Little-endian field readers for fixed-stride record decoding,
//! shared by the typed-lump and game-lump modules.
//!
//! These index directly and panic on out-of-range offsets: callers
//! guarantee `record.len() >= offset + width` by construction (records
//! come from exact-stride `chunks_exact` or table-validated slices).
//! For cursor-style reading over unvalidated bytes, use each module's
//! bounds-checked reader instead.

pub(super) fn f32_at(record: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(record[at..at + 4].try_into().expect("4 bytes"))
}

pub(super) fn i32_at(record: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(record[at..at + 4].try_into().expect("4 bytes"))
}

pub(super) fn u32_at(record: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(record[at..at + 4].try_into().expect("4 bytes"))
}

pub(super) fn u16_at(record: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(record[at..at + 2].try_into().expect("2 bytes"))
}

pub(super) fn i16_at(record: &[u8], at: usize) -> i16 {
    i16::from_le_bytes(record[at..at + 2].try_into().expect("2 bytes"))
}

pub(super) fn vec3_at(record: &[u8], at: usize) -> [f32; 3] {
    [
        f32_at(record, at),
        f32_at(record, at + 4),
        f32_at(record, at + 8),
    ]
}
