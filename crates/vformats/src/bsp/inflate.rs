//! Minimal RFC 1951 DEFLATE decompression for ZIP pakfile entries,
//! implemented in-crate to avoid a dependency. Canonical Huffman
//! decoding in the style of zlib's `puff` reference: bit-by-bit,
//! favoring verifiability over table-driven speed (pakfile entries
//! are small; revisit if that changes).
//!
//! The caller knows the uncompressed size (ZIP records it), so output
//! is bounded up front and a size mismatch in either direction is an
//! error.

/// The stream is malformed, truncated, or disagrees with the expected
/// output size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InflateError;

const MAX_BITS: usize = 15;
const MAX_LIT_CODES: usize = 286;
const MAX_DIST_CODES: usize = 30;

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
    buffer: u32,
    count: u32,
}

impl Reader<'_> {
    /// The next `need` bits, LSB-first. After extraction the buffer
    /// always holds fewer than 8 bits, so byte alignment (stored
    /// blocks) is just dropping it.
    fn bits(&mut self, need: u32) -> Result<u32, InflateError> {
        while self.count < need {
            let byte = *self.data.get(self.at).ok_or(InflateError)?;
            self.buffer |= u32::from(byte) << self.count;
            self.at += 1;
            self.count += 8;
        }
        let value = self.buffer & ((1 << need) - 1);
        self.buffer >>= need;
        self.count -= need;
        Ok(value)
    }

    fn align_to_byte(&mut self) {
        self.buffer = 0;
        self.count = 0;
    }
}

/// A canonical Huffman code: symbol counts per code length plus the
/// symbols sorted by (length, symbol).
struct Huffman {
    count: [u16; MAX_BITS + 1],
    symbol: Vec<u16>,
}

impl Huffman {
    /// Build from per-symbol code lengths. Over-subscribed codes are
    /// rejected; incomplete codes are permitted (their unassigned
    /// codes fail at decode), matching zlib's tolerance for the
    /// single-code distance tables real encoders emit.
    fn build(lengths: &[u16]) -> Result<Self, InflateError> {
        let mut count = [0u16; MAX_BITS + 1];
        for &len in lengths {
            count[usize::from(len)] += 1;
        }
        let mut left = 1i32;
        for &len_count in &count[1..] {
            left <<= 1;
            left -= i32::from(len_count);
            if left < 0 {
                return Err(InflateError);
            }
        }
        let mut offset = [0u16; MAX_BITS + 1];
        for len in 1..MAX_BITS {
            offset[len + 1] = offset[len] + count[len];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (sym, &len) in lengths.iter().enumerate() {
            if len != 0 {
                let len = usize::from(len);
                symbol[usize::from(offset[len])] = u16::try_from(sym).map_err(|_| InflateError)?;
                offset[len] += 1;
            }
        }
        Ok(Self { count, symbol })
    }

    fn decode(&self, reader: &mut Reader<'_>) -> Result<u16, InflateError> {
        let mut code = 0u32;
        let mut first = 0u32;
        let mut index = 0u32;
        for len in 1..=MAX_BITS {
            code |= reader.bits(1)?;
            let count = u32::from(self.count[len]);
            if code < first + count {
                return Ok(self.symbol[(index + code - first) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(InflateError)
    }
}

/// Decompress a raw DEFLATE stream that must inflate to exactly
/// `expected` bytes.
pub(super) fn inflate(data: &[u8], expected: usize) -> Result<Vec<u8>, InflateError> {
    let mut reader = Reader {
        data,
        at: 0,
        buffer: 0,
        count: 0,
    };
    let mut out = Vec::with_capacity(expected);
    loop {
        let last = reader.bits(1)? == 1;
        match reader.bits(2)? {
            0 => stored_block(&mut reader, &mut out, expected)?,
            1 => {
                let (lit, dist) = fixed_tables();
                coded_block(&mut reader, &mut out, expected, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut reader)?;
                coded_block(&mut reader, &mut out, expected, &lit, &dist)?;
            }
            _ => return Err(InflateError),
        }
        if last {
            break;
        }
    }
    if out.len() != expected {
        return Err(InflateError);
    }
    Ok(out)
}

fn stored_block(
    reader: &mut Reader<'_>,
    out: &mut Vec<u8>,
    expected: usize,
) -> Result<(), InflateError> {
    reader.align_to_byte();
    let header = reader
        .data
        .get(reader.at..reader.at + 4)
        .ok_or(InflateError)?;
    let len = u16::from_le_bytes(header[0..2].try_into().expect("2 bytes"));
    let nlen = u16::from_le_bytes(header[2..4].try_into().expect("2 bytes"));
    if len != !nlen {
        return Err(InflateError);
    }
    reader.at += 4;
    let len = usize::from(len);
    let payload = reader
        .data
        .get(reader.at..reader.at + len)
        .ok_or(InflateError)?;
    if out.len() + len > expected {
        return Err(InflateError);
    }
    out.extend_from_slice(payload);
    reader.at += len;
    Ok(())
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn coded_block(
    reader: &mut Reader<'_>,
    out: &mut Vec<u8>,
    expected: usize,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), InflateError> {
    loop {
        let symbol = lit.decode(reader)?;
        match symbol {
            0..=255 => {
                if out.len() >= expected {
                    return Err(InflateError);
                }
                out.push(u8::try_from(symbol).expect("literal symbol is in the byte range"));
            }
            256 => return Ok(()),
            257..=285 => {
                let index = usize::from(symbol - 257);
                let length =
                    usize::from(LENGTH_BASE[index]) + reader.bits(LENGTH_EXTRA[index])? as usize;
                let dsym = usize::from(dist.decode(reader)?);
                if dsym >= MAX_DIST_CODES {
                    return Err(InflateError);
                }
                let distance =
                    usize::from(DIST_BASE[dsym]) + reader.bits(DIST_EXTRA[dsym])? as usize;
                if distance > out.len() || out.len() + length > expected {
                    return Err(InflateError);
                }
                // Overlapping copies are the RLE case: byte by byte.
                for _ in 0..length {
                    out.push(out[out.len() - distance]);
                }
            }
            _ => return Err(InflateError),
        }
    }
}

fn fixed_tables() -> (Huffman, Huffman) {
    let mut lengths = [8u16; 288];
    lengths[144..256].fill(9);
    lengths[256..280].fill(7);
    let lit = Huffman::build(&lengths).expect("the fixed literal code is complete");
    // 30 five-bit codes: incomplete by design (codes 30/31 never appear).
    let dist = Huffman::build(&[5u16; 30]).expect("the fixed distance code is not over-subscribed");
    (lit, dist)
}

fn dynamic_tables(reader: &mut Reader<'_>) -> Result<(Huffman, Huffman), InflateError> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let hlit = reader.bits(5)? as usize + 257;
    let hdist = reader.bits(5)? as usize + 1;
    let hclen = reader.bits(4)? as usize + 4;
    if hlit > MAX_LIT_CODES || hdist > MAX_DIST_CODES {
        return Err(InflateError);
    }
    let mut cl_lengths = [0u16; 19];
    for &index in &ORDER[..hclen] {
        cl_lengths[index] =
            u16::try_from(reader.bits(3)?).expect("three decoded bits always fit in u16");
    }
    let cl = Huffman::build(&cl_lengths)?;

    let mut lengths = [0u16; MAX_LIT_CODES + MAX_DIST_CODES];
    let total = hlit + hdist;
    let mut at = 0;
    while at < total {
        let symbol = cl.decode(reader)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol, 1),
            16 => {
                if at == 0 {
                    return Err(InflateError);
                }
                (lengths[at - 1], 3 + reader.bits(2)? as usize)
            }
            17 => (0, 3 + reader.bits(3)? as usize),
            18 => (0, 11 + reader.bits(7)? as usize),
            _ => return Err(InflateError),
        };
        if at + repeat > total {
            return Err(InflateError);
        }
        lengths[at..at + repeat].fill(value);
        at += repeat;
    }
    if lengths[256] == 0 {
        // No end-of-block code: the block could never terminate.
        return Err(InflateError);
    }
    let lit = Huffman::build(&lengths[..hlit])?;
    let dist = Huffman::build(&lengths[hlit..total])?;
    Ok((lit, dist))
}

#[cfg(test)]
mod tests {
    use super::{InflateError, inflate};

    #[test]
    fn stored_blocks_round_trip() {
        // Final stored block: 5 bytes.
        let mut data = vec![0x01, 5, 0, !5, 0xFF];
        data.extend_from_slice(b"hello");
        assert_eq!(inflate(&data, 5).expect("inflate"), b"hello");

        // Complement mismatch.
        let bad = [0x01, 5, 0, 0, 0, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(inflate(&bad, 5), Err(InflateError));
    }

    /// Hand-packed fixed-Huffman streams: the literal `a` (code
    /// 0x30 + 0x61, eight bits), and `a` followed by a length-3
    /// distance-1 match (the RLE case).
    #[test]
    fn fixed_huffman_literals_and_matches() {
        assert_eq!(inflate(&[0x4B, 0x04, 0x00], 1).expect("inflate"), b"a");
        assert_eq!(
            inflate(&[0x4B, 0x04, 0x02, 0x00], 4).expect("inflate"),
            b"aaaa"
        );
    }

    #[test]
    fn malformed_streams_error_not_panic() {
        // Reserved block type.
        assert_eq!(inflate(&[0x07], 1), Err(InflateError));
        // Truncated mid-header.
        assert_eq!(inflate(&[], 1), Err(InflateError));
        // A match as the first symbol: its distance reaches before the
        // start of the output.
        assert_eq!(inflate(&[0x03, 0x02], 400), Err(InflateError));
        // Output size disagreement (stream says 1 byte, caller 2).
        assert_eq!(inflate(&[0x4B, 0x04, 0x00], 2), Err(InflateError));
    }
}
