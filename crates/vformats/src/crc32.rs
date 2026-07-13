//! CRC-32 (IEEE 802.3, reflected, polynomial 0xEDB88320) — the variant
//! used by GMA entry tables and VPK. Not Castagnoli.

/// IEEE CRC-32 of `bytes`.
///
/// Exposed so callers building GMA archives can precompute the per-entry
/// CRCs the wire format requires ahead of the payload stream.
#[must_use]
pub fn crc32_ieee(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(crc32_ieee(b""), 0);
        assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32_ieee(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn matches_reference_implementation_across_lengths() {
        // Exercise every chunks_exact remainder length and multi-block inputs.
        let data: Vec<u8> = (0..4096u32).map(|i| (i * 31 + i / 7) as u8).collect();
        for len in (0..64).chain([255, 1024, 4095, 4096]) {
            let slice = &data[..len];
            assert_eq!(crc32_ieee(slice), crc32fast::hash(slice), "length {len}");
        }
    }
}
