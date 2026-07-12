//! Garry's Mod addon archives (`.gma`), read and write.
//!
//! Clean-room implementation of the `.gma` wire format — deliberately
//! sharing no code with GPL-licensed readers; behavior is validated by
//! byte-level golden fixtures spelled out by hand from the format.
//!
//! Reading is strict (archive role: corruption fails loudly): [`parse`]
//! validates the header, the whole entry table, and every payload
//! extent up front, then serves entry bytes as zero-copy borrows.
//! Entry paths are NOT validated at parse — real workshop archives
//! carry traversal-shaped paths and losing the whole addon over one
//! entry is worse than skipping it. Extractors MUST check
//! [`GmaEntry::path_is_unsafe`] before touching the filesystem
//! (the writer still rejects unsafe paths). Entry CRCs are **not**
//! verified (matching the ecosystem's tools — real archives carry
//! wrong CRCs); callers who want the check compare
//! [`crate::crc32_ieee`]`(payload)` against [`GmaEntry::crc32`].
//!
//! [`parse`] operates on DECOMPRESSED bytes. Workshop downloads are
//! whole-file LZMA; detect with [`is_lzma_compressed`] and decompress
//! explicitly with [`decompress`] (`lzma` feature) — a deliberate
//! two-step so the caller controls the large allocation and `Gma`
//! stays borrowed.
//!
//! Writing streams through a [`Sink`]: the wire format puts entry
//! sizes and CRCs *before* payload bytes, so [`GmaWriter::new`] takes
//! the complete table up front (CRCs via [`crate::crc32_ieee`]) and
//! [`GmaWriter::write_payload`] streams content in table order.

use std::borrow::Cow;
use std::fmt;

use crate::Limits;
use crate::entry_path::is_unsafe_entry_path;
use crate::sink::Sink;

const MAGIC: &[u8; 4] = b"GMAD";
const WRITE_VERSION: u8 = 3;
const WRITE_ADDON_VERSION: i32 = 1;

/// A parsed addon archive; entry payloads are zero-copy borrows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gma<'a> {
    /// Header metadata.
    pub metadata: GmaMetadata<'a>,
    entries: Vec<GmaEntry<'a>>,
    /// Byte offset of each entry's payload within `bytes`.
    offsets: Vec<usize>,
    bytes: &'a [u8],
}

/// Addon header metadata. `description` is the raw string — version-3
/// writers store the `addon.json` body there, and interpreting it is
/// deliberately the caller's business (no JSON dependency).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GmaMetadata<'a> {
    /// Container format version (0–3).
    pub version: u8,
    /// Author SteamID64 (0 from current writers).
    pub steamid: u64,
    /// Creation timestamp, Unix seconds.
    pub timestamp: u64,
    /// Required-content strings (empty from current writers).
    pub required_content: Vec<Cow<'a, str>>,
    /// Addon title.
    pub name: Cow<'a, str>,
    /// Raw description string (often an `addon.json` body).
    pub description: Cow<'a, str>,
    /// Author display string.
    pub author: Cow<'a, str>,
    /// Unused numeric addon version (1 from current writers).
    pub addon_version: i32,
}

impl Default for GmaMetadata<'_> {
    fn default() -> Self {
        Self {
            version: WRITE_VERSION,
            steamid: 0,
            timestamp: 0,
            required_content: Vec::new(),
            name: Cow::Borrowed(""),
            description: Cow::Borrowed(""),
            author: Cow::Borrowed(""),
            addon_version: WRITE_ADDON_VERSION,
        }
    }
}

/// One entry-table record. Constructible by callers (the writer takes
/// a table of these); [`crate::crc32_ieee`] computes the `crc32`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GmaEntry<'a> {
    /// Entry path, forward slashes (writers lowercase it). NOT
    /// validated at parse: check
    /// [`path_is_unsafe`](Self::path_is_unsafe) before using it to
    /// build a filesystem path.
    pub path: Cow<'a, str>,
    /// Payload size in bytes.
    pub size: u64,
    /// IEEE CRC-32 of the payload.
    pub crc32: u32,
}

impl GmaEntry<'_> {
    /// Whether extracting this entry to disk under
    /// [`path`](Self::path) could escape the extraction root
    /// (traversal, absolute paths, backslashes — see
    /// [`crate::is_unsafe_entry_path`]).
    #[must_use]
    pub fn path_is_unsafe(&self) -> bool {
        is_unsafe_entry_path(&self.path)
    }
}

impl<'a> Gma<'a> {
    /// The entry table, in file order.
    #[must_use]
    pub fn entries(&self) -> &[GmaEntry<'a>] {
        &self.entries
    }

    /// Look up one entry and its payload by exact path. Writers store
    /// paths lowercased with forward slashes; this compares bytes, so
    /// lowercase the needle for wild content.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<(&GmaEntry<'a>, &'a [u8])> {
        let index = self.entries.iter().position(|entry| entry.path == path)?;
        let bytes = self.entry_bytes(index).ok()?;
        Some((&self.entries[index], bytes))
    }

    /// One entry's payload bytes, zero-copy. `index` is the position
    /// in [`entries`](Self::entries). Extents were validated at parse,
    /// so this only fails on an out-of-range index.
    pub fn entry_bytes(&self, index: usize) -> Result<&'a [u8], GmaError> {
        let entry = self.entries.get(index).ok_or(GmaError::NoSuchEntry)?;
        let offset = self.offsets[index];
        let len = usize::try_from(entry.size).map_err(|_| GmaError::NoSuchEntry)?;
        self.bytes
            .get(offset..offset + len)
            .ok_or(GmaError::NoSuchEntry)
    }
}

/// GMA parse failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GmaError {
    /// Input exceeds [`Limits::max_input_bytes`].
    InputTooLarge {
        /// Input length in bytes.
        len: u64,
        /// The configured cap.
        max: u64,
    },
    /// The file does not start with `GMAD`.
    BadMagic,
    /// Not a version 0–3 archive.
    UnsupportedVersion(u8),
    /// Input ends before a required structure or payload extent.
    Truncated {
        /// Bytes required.
        needed: u64,
        /// Bytes available.
        available: u64,
    },
    /// A table structure is malformed (negative size, overflow).
    Corrupt,
    /// Entry count exceeds [`Limits::max_entries`].
    TooManyEntries {
        /// The configured cap.
        max: usize,
    },
    /// An entry exceeds [`Limits::max_entry_bytes`].
    EntryTooLarge {
        /// Declared entry size.
        size: u64,
        /// The configured cap.
        max: u64,
    },
    /// An entry index passed to [`Gma::entry_bytes`] does not exist.
    NoSuchEntry,
}

impl fmt::Display for GmaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge { len, max } => {
                write!(f, "gma input is {len} bytes, over the {max}-byte limit")
            }
            Self::BadMagic => write!(f, "not a gma archive (bad magic)"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported gma version {version}")
            }
            Self::Truncated { needed, available } => {
                write!(f, "gma truncated: need {needed} bytes, have {available}")
            }
            Self::Corrupt => write!(f, "gma entry table is malformed"),
            Self::TooManyEntries { max } => {
                write!(f, "gma entry count exceeds the limit of {max}")
            }
            Self::EntryTooLarge { size, max } => {
                write!(f, "gma entry of {size} bytes exceeds the {max}-byte limit")
            }
            Self::NoSuchEntry => write!(f, "gma entry index out of range"),
        }
    }
}

impl std::error::Error for GmaError {}

impl crate::reader::ReadError for GmaError {
    fn truncated(needed: u64, available: u64) -> Self {
        Self::Truncated { needed, available }
    }
    fn overflow() -> Self {
        Self::Corrupt
    }
}

type Reader<'a> = crate::reader::Reader<'a, GmaError>;

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8, GmaError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, GmaError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, GmaError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    fn i64(&mut self) -> Result<i64, GmaError> {
        let b = self.take(8)?;
        Ok(i64::from_le_bytes(b.try_into().expect("8 bytes")))
    }

    fn i32(&mut self) -> Result<i32, GmaError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// NUL-terminated string, lossily decoded.
    fn c_string(&mut self) -> Result<Cow<'a, str>, GmaError> {
        let rest = self.bytes.get(self.pos..).ok_or(GmaError::Corrupt)?;
        let nul = rest
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(GmaError::Truncated {
                needed: self.bytes.len() as u64 + 1,
                available: self.bytes.len() as u64,
            })?;
        let value = &rest[..nul];
        self.pos += nul + 1;
        Ok(String::from_utf8_lossy(value))
    }
}

/// Parse a decompressed `.gma`. Every payload extent is validated here,
/// so entry access never fails on well-indexed calls.
pub fn parse<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Gma<'a>, GmaError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(GmaError::InputTooLarge {
            len: bytes.len() as u64,
            max: limits.max_input_bytes,
        });
    }
    let mut r = Reader::at(bytes, 0);
    if r.take(4)? != MAGIC {
        return Err(GmaError::BadMagic);
    }
    let version = r.u8()?;
    // gmad accepts any version <= 3; version-0 archives exist in the
    // wild and follow the version-1 layout (no required-content list).
    if version > 3 {
        return Err(GmaError::UnsupportedVersion(version));
    }
    let steamid = r.u64()?;
    let timestamp = r.u64()?;
    let mut required_content = Vec::new();
    if version > 1 {
        loop {
            let content = r.c_string()?;
            if content.is_empty() {
                break;
            }
            if required_content.len() >= limits.max_entries {
                return Err(GmaError::TooManyEntries {
                    max: limits.max_entries,
                });
            }
            required_content.push(content);
        }
    }
    let name = r.c_string()?;
    let description = r.c_string()?;
    let author = r.c_string()?;
    let addon_version = r.i32()?;

    let mut entries = Vec::new();
    loop {
        // Only the zero terminator is load-bearing; writers emit
        // 1, 2, 3, … but gaps and repeats are tolerated.
        if r.u32()? == 0 {
            break;
        }
        let path = r.c_string()?;
        let size = r.i64()?;
        let crc32 = r.u32()?;
        let size = u64::try_from(size).map_err(|_| GmaError::Corrupt)?;
        if size > limits.max_entry_bytes {
            return Err(GmaError::EntryTooLarge {
                size,
                max: limits.max_entry_bytes,
            });
        }

        if entries.len() >= limits.max_entries {
            return Err(GmaError::TooManyEntries {
                max: limits.max_entries,
            });
        }
        entries.push(GmaEntry { path, size, crc32 });
    }

    // Payloads follow the table in order; validate every extent now.
    let mut offsets = Vec::with_capacity(entries.len());
    let mut cursor = r.pos as u64;
    for entry in &entries {
        offsets.push(usize::try_from(cursor).map_err(|_| GmaError::Corrupt)?);
        cursor = cursor.checked_add(entry.size).ok_or(GmaError::Corrupt)?;
    }
    if cursor > bytes.len() as u64 {
        return Err(GmaError::Truncated {
            needed: cursor,
            available: bytes.len() as u64,
        });
    }
    // A trailing addon CRC may follow; readers must not require it.

    Ok(Gma {
        metadata: GmaMetadata {
            version,
            steamid,
            timestamp,
            required_content,
            name,
            description,
            author,
            addon_version,
        },
        entries,
        offsets,
        bytes,
    })
}

// ---------------------------------------------------------------
// Writer
// ---------------------------------------------------------------

/// Streaming `.gma` writer over a [`Sink`]. The wire format stores the
/// entry table (sizes + CRCs) before payloads, so the table comes
/// complete at construction and payload bytes stream in entry order —
/// no buffering, no backpatching.
pub struct GmaWriter<S: Sink> {
    sink: S,
    payload_remaining: u64,
}

// Manual: a `derive` would demand `S: Debug` of every sink.
impl<S: Sink> fmt::Debug for GmaWriter<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GmaWriter")
            .field("payload_remaining", &self.payload_remaining)
            .finish_non_exhaustive()
    }
}

/// GMA write failure. `E` is the sink's error type.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GmaWriteError<E> {
    /// The sink failed.
    Sink(E),
    /// An entry path is empty or would escape an extraction root.
    UnsafePath(String),
    /// An entry size exceeds `i64::MAX` (unrepresentable on the wire).
    Oversize,
    /// More payload bytes were streamed than the table declares.
    TooMuchPayload,
    /// [`GmaWriter::finish`] was called before all declared payload
    /// bytes were streamed.
    PayloadShortfall {
        /// Bytes still owed.
        missing: u64,
    },
}

impl<E: fmt::Debug> fmt::Display for GmaWriteError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sink(error) => write!(f, "gma sink error: {error:?}"),
            Self::UnsafePath(path) => write!(f, "gma entry path is unsafe: {path:?}"),
            Self::Oversize => write!(f, "gma entry size exceeds i64::MAX"),
            Self::TooMuchPayload => write!(f, "gma payload exceeds the declared table sizes"),
            Self::PayloadShortfall { missing } => {
                write!(f, "gma payload ended {missing} bytes short of the table")
            }
        }
    }
}

impl<E: fmt::Debug> std::error::Error for GmaWriteError<E> {}

impl<S: Sink> GmaWriter<S> {
    /// Write the header and complete entry table, ready for payload
    /// streaming. Always writes format version 3; `metadata.version`
    /// is ignored. Per-entry CRCs come from the caller
    /// ([`crate::crc32_ieee`]).
    pub fn new(
        mut sink: S,
        metadata: &GmaMetadata<'_>,
        entries: &[GmaEntry<'_>],
    ) -> Result<Self, GmaWriteError<S::Error>> {
        let mut payload_total = 0u64;
        for entry in entries {
            if entry.path.is_empty() || is_unsafe_entry_path(&entry.path) {
                return Err(GmaWriteError::UnsafePath(entry.path.clone().into_owned()));
            }
            if i64::try_from(entry.size).is_err() {
                return Err(GmaWriteError::Oversize);
            }
            payload_total = payload_total
                .checked_add(entry.size)
                .ok_or(GmaWriteError::Oversize)?;
        }

        let put = |sink: &mut S, bytes: &[u8]| sink.write_all(bytes).map_err(GmaWriteError::Sink);
        let put_c = |sink: &mut S, value: &str| -> Result<(), GmaWriteError<S::Error>> {
            sink.write_all(value.as_bytes())
                .and_then(|()| sink.write_all(&[0]))
                .map_err(GmaWriteError::Sink)
        };

        put(&mut sink, MAGIC)?;
        put(&mut sink, &[WRITE_VERSION])?;
        put(&mut sink, &metadata.steamid.to_le_bytes())?;
        put(&mut sink, &metadata.timestamp.to_le_bytes())?;
        for content in &metadata.required_content {
            put_c(&mut sink, content)?;
        }
        put_c(&mut sink, "")?; // required-content terminator
        put_c(&mut sink, &metadata.name)?;
        put_c(&mut sink, &metadata.description)?;
        put_c(&mut sink, &metadata.author)?;
        put(&mut sink, &metadata.addon_version.to_le_bytes())?;

        for (index, entry) in entries.iter().enumerate() {
            let file_number = u32::try_from(index + 1).map_err(|_| GmaWriteError::Oversize)?;
            put(&mut sink, &file_number.to_le_bytes())?;
            put_c(&mut sink, &entry.path)?;
            let size =
                i64::try_from(entry.size).expect("entry sizes were validated before writing");
            put(&mut sink, &size.to_le_bytes())?;
            put(&mut sink, &entry.crc32.to_le_bytes())?;
        }
        put(&mut sink, &0u32.to_le_bytes())?; // table terminator

        Ok(Self {
            sink,
            payload_remaining: payload_total,
        })
    }

    /// Stream payload bytes, in entry order. Chunk boundaries need not
    /// align with entry boundaries.
    pub fn write_payload(&mut self, chunk: &[u8]) -> Result<(), GmaWriteError<S::Error>> {
        let len = chunk.len() as u64;
        if len > self.payload_remaining {
            return Err(GmaWriteError::TooMuchPayload);
        }
        self.payload_remaining -= len;
        self.sink.write_all(chunk).map_err(GmaWriteError::Sink)
    }

    /// Validate the payload total and write the trailer, returning the
    /// sink. The trailer CRC is written as 0: nothing consumes it, and
    /// computing it would force the writer to buffer or hash every
    /// preceding byte.
    pub fn finish(mut self) -> Result<S, GmaWriteError<S::Error>> {
        if self.payload_remaining > 0 {
            return Err(GmaWriteError::PayloadShortfall {
                missing: self.payload_remaining,
            });
        }
        self.sink
            .write_all(&0u32.to_le_bytes())
            .map_err(GmaWriteError::Sink)?;
        Ok(self.sink)
    }
}

// ---------------------------------------------------------------
// Workshop transport compression
// ---------------------------------------------------------------

/// Whether bytes look like a whole-file LZMA workshop download rather than
/// a bare `.gma`. A heuristic: it reliably separates the two shapes GMA
/// pipelines see, but most arbitrary non-GMA binary data also passes — do
/// not use it as a general LZMA detector.
#[must_use]
pub fn is_lzma_compressed(bytes: &[u8]) -> bool {
    // First byte is the LZMA properties byte `(pb*5 + lp)*9 + lc`, whose
    // maximum valid value is `(4*5 + 4)*9 + 8 = 224`.
    !bytes.starts_with(MAGIC) && bytes.len() >= 13 && bytes[0] < 225
}

/// LZMA decompression failure (`lzma` feature).
#[cfg(feature = "lzma")]
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LzmaError {
    /// Shorter than the 13-byte LZMA-alone header.
    TooSmall,
    /// The declared output, declared dictionary, or produced output
    /// exceeds [`Limits::max_input_bytes`] (the decompressed archive is
    /// this crate's next input, so the input cap is the honest bound).
    OutputTooLarge {
        /// The configured cap.
        max: u64,
    },
    /// The LZMA stream is invalid.
    Decode,
}

#[cfg(feature = "lzma")]
impl fmt::Display for LzmaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooSmall => write!(f, "lzma input shorter than its 13-byte header"),
            Self::OutputTooLarge { max } => {
                write!(f, "lzma output exceeds the {max}-byte limit")
            }
            Self::Decode => write!(f, "lzma stream is invalid"),
        }
    }
}

#[cfg(feature = "lzma")]
impl std::error::Error for LzmaError {}

/// Decompress a whole-file LZMA workshop download to the bare `.gma`
/// bytes. Explicitly separate from [`parse`] so the caller controls
/// the allocation and `Gma` stays zero-copy over the result.
#[cfg(feature = "lzma")]
pub fn decompress(bytes: &[u8], limits: &Limits) -> Result<Vec<u8>, LzmaError> {
    use std::io::Read as _;

    if bytes.len() < 13 {
        return Err(LzmaError::TooSmall);
    }
    let declared = u64::from_le_bytes(bytes[5..13].try_into().expect("8 bytes"));
    // u64::MAX means "unknown size, read to end-marker"; anything else
    // over the cap can be rejected before decompressing a byte.
    if declared != u64::MAX && declared > limits.max_input_bytes {
        return Err(LzmaError::OutputTooLarge {
            max: limits.max_input_bytes,
        });
    }

    // The header's dictionary size drives an up-front allocation, so it
    // must honor the caller's budget even when the declared output size
    // is the "unknown" sentinel (which bypasses the pre-check above).
    let dict_size = u32::from_le_bytes(bytes[1..5].try_into().expect("4 bytes"));
    if u64::from(dict_size) > limits.max_input_bytes {
        return Err(LzmaError::OutputTooLarge {
            max: limits.max_input_bytes,
        });
    }
    // Belt and braces: cap the decoder's own allocations too (in KiB).
    let mem_limit_kib = u32::try_from(limits.max_input_bytes / 1024)
        .unwrap_or(u32::MAX)
        .max(64);
    let mut reader = lzma_rust2::LzmaReader::new_mem_limit(bytes, mem_limit_kib, None)
        .map_err(|_| LzmaError::Decode)?;
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|_| LzmaError::Decode)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() as u64 > limits.max_input_bytes {
            return Err(LzmaError::OutputTooLarge {
                max: limits.max_input_bytes,
            });
        }
    }
    Ok(out)
}
