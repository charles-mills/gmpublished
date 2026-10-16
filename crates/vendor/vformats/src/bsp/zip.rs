//! Minimal ZIP reading for BSP pakfile lumps — the crate's own
//! rather than a dependency on (a fork of) the `zip` crate.
//! Central directory driven; STORE entries are zero-copy
//! borrows, DEFLATE entries inflate via this crate's own decoder,
//! and LZMA entries decompress behind the `lzma` feature.

use std::borrow::Cow;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use super::inflate::{self, inflate};
use crate::Limits;

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const EOCD_BYTES: usize = 22;
const CENTRAL_BYTES: usize = 46;
const LOCAL_BYTES: usize = 30;

const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
#[cfg(feature = "lzma")]
const METHOD_LZMA: u16 = 14;

/// Monotonic source for [`ZipReader`] identity tokens (see
/// [`ZipReader::entry_bytes`]).
static NEXT_READER_ID: AtomicU64 = AtomicU64::new(1);

/// A ZIP archive view over borrowed bytes (central-directory driven).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipReader<'a> {
    entries: Vec<ZipEntry>,
    bytes: &'a [u8],
    id: u64,
}

/// One archive entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipEntry {
    /// Entry path as stored (lossily decoded). NOT validated: check
    /// [`path_is_unsafe`](Self::path_is_unsafe) before using it to
    /// build a filesystem path.
    pub path: String,
    /// IEEE CRC-32 of the uncompressed payload.
    pub crc32: u32,
    /// Uncompressed payload size.
    pub uncompressed_size: u64,
    method: u16,
    compressed_size: usize,
    local_offset: usize,
    /// Identity of the [`ZipReader`] that produced this entry; checked
    /// by [`ZipReader::entry_bytes`] so an entry from one archive can't
    /// be used to read bytes out of a different one.
    reader_id: u64,
}

impl ZipEntry {
    /// Whether extracting this entry to disk under [`path`](Self::path)
    /// could escape the extraction root (traversal, absolute paths,
    /// backslashes — see [`crate::is_unsafe_entry_path`]). Unlike the
    /// GMA and VPK parsers, the pakfile reader does not reject such
    /// entries at parse: third-party map packers write nonconforming
    /// paths, and lookup-by-path use is harmless. Extractors MUST check.
    #[must_use]
    pub fn path_is_unsafe(&self) -> bool {
        crate::is_unsafe_entry_path(&self.path)
    }
}

/// ZIP reading failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ZipError {
    /// No end-of-central-directory record found.
    MissingDirectory,
    /// A directory or local record is malformed or out of bounds.
    Corrupt,
    /// The entry uses a compression method this crate cannot decode
    /// (LZMA needs the `lzma` feature; anything past STORE, DEFLATE,
    /// and LZMA is out of scope).
    UnsupportedCompression {
        /// The ZIP method id (14 = LZMA).
        method: u16,
    },
    /// The decoder exceeds the declared working limit.
    #[allow(missing_docs)]
    DecoderTooLarge { required: u64, max: u64 },
    /// The entry exceeds [`Limits::max_entry_bytes`].
    EntryTooLarge {
        /// Declared uncompressed size.
        size: u64,
        /// The configured cap.
        max: u64,
    },
    /// A compressed entry's stream is invalid or disagrees with the
    /// declared uncompressed size.
    Decode,
    /// The entry was produced by a different [`ZipReader`], so its
    /// offsets are meaningless against this one's bytes.
    ForeignEntry,
}

impl fmt::Display for ZipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDirectory => write!(f, "zip end-of-central-directory not found"),
            Self::Corrupt => write!(f, "zip directory or entry record is malformed"),
            Self::UnsupportedCompression { method } => {
                write!(f, "zip compression method {method} is not supported")
            }
            Self::EntryTooLarge { size, max } => {
                write!(f, "zip entry of {size} bytes exceeds the {max}-byte limit")
            }
            Self::DecoderTooLarge { required, max } => {
                write!(f, "ZIP decoder requires {required} bytes; limit {max}")
            }
            Self::Decode => write!(f, "zip entry stream is invalid"),
            Self::ForeignEntry => write!(f, "zip entry belongs to a different reader"),
        }
    }
}

impl std::error::Error for ZipError {}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

impl<'a> ZipReader<'a> {
    /// Parse an archive's central directory. An empty input yields an
    /// empty reader (maps without embedded content have empty pakfile
    /// lumps).
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ZipError> {
        let id = NEXT_READER_ID.fetch_add(1, Ordering::Relaxed);
        if bytes.is_empty() {
            return Ok(Self {
                entries: Vec::new(),
                bytes,
                id,
            });
        }
        // EOCD: scan backwards over the (bounded, u16) trailing comment.
        let mut eocd = None;
        let earliest = bytes
            .len()
            .saturating_sub(EOCD_BYTES + usize::from(u16::MAX));
        let mut candidate = bytes.len().checked_sub(EOCD_BYTES);
        while let Some(at) = candidate {
            if read_u32(bytes, at) == Some(EOCD_SIGNATURE) {
                eocd = Some(at);
                break;
            }
            if at == earliest {
                break;
            }
            candidate = at.checked_sub(1);
        }
        let eocd = eocd.ok_or(ZipError::MissingDirectory)?;
        let entry_count = usize::from(read_u16(bytes, eocd + 10).ok_or(ZipError::Corrupt)?);
        let directory_offset =
            usize::try_from(read_u32(bytes, eocd + 16).ok_or(ZipError::Corrupt)?)
                .map_err(|_| ZipError::Corrupt)?;

        let mut entries = Vec::with_capacity(entry_count);
        let mut at = directory_offset;
        for _ in 0..entry_count {
            if read_u32(bytes, at) != Some(CENTRAL_SIGNATURE) {
                return Err(ZipError::Corrupt);
            }
            let method = read_u16(bytes, at + 10).ok_or(ZipError::Corrupt)?;
            let crc32 = read_u32(bytes, at + 16).ok_or(ZipError::Corrupt)?;
            let compressed_size =
                usize::try_from(read_u32(bytes, at + 20).ok_or(ZipError::Corrupt)?)
                    .map_err(|_| ZipError::Corrupt)?;
            let uncompressed_size = u64::from(read_u32(bytes, at + 24).ok_or(ZipError::Corrupt)?);
            let name_len = usize::from(read_u16(bytes, at + 28).ok_or(ZipError::Corrupt)?);
            let extra_len = usize::from(read_u16(bytes, at + 30).ok_or(ZipError::Corrupt)?);
            let comment_len = usize::from(read_u16(bytes, at + 32).ok_or(ZipError::Corrupt)?);
            let local_offset = usize::try_from(read_u32(bytes, at + 42).ok_or(ZipError::Corrupt)?)
                .map_err(|_| ZipError::Corrupt)?;
            let name = bytes
                .get(at + CENTRAL_BYTES..at + CENTRAL_BYTES + name_len)
                .ok_or(ZipError::Corrupt)?;
            entries.push(ZipEntry {
                path: String::from_utf8_lossy(name).into_owned(),
                crc32,
                uncompressed_size,
                method,
                compressed_size,
                local_offset,
                reader_id: id,
            });
            at = at
                .checked_add(CENTRAL_BYTES + name_len + extra_len + comment_len)
                .ok_or(ZipError::Corrupt)?;
        }

        Ok(Self { entries, bytes, id })
    }

    /// All entries, directory order.
    #[must_use]
    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// Look up one entry by exact path.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&ZipEntry> {
        self.entries.iter().find(|entry| entry.path == path)
    }

    fn entry_data(&self, entry: &ZipEntry) -> Result<&'a [u8], ZipError> {
        if entry.reader_id != self.id {
            return Err(ZipError::ForeignEntry);
        }
        let local = entry.local_offset;
        if read_u32(self.bytes, local) != Some(LOCAL_SIGNATURE) {
            return Err(ZipError::Corrupt);
        }
        let name_len = usize::from(read_u16(self.bytes, local + 26).ok_or(ZipError::Corrupt)?);
        let extra_len = usize::from(read_u16(self.bytes, local + 28).ok_or(ZipError::Corrupt)?);
        let start = local
            .checked_add(LOCAL_BYTES + name_len + extra_len)
            .ok_or(ZipError::Corrupt)?;
        let end = start
            .checked_add(entry.compressed_size)
            .ok_or(ZipError::Corrupt)?;
        self.bytes.get(start..end).ok_or(ZipError::Corrupt)
    }

    /// Decoder workspace excludes the caller's output allocation.
    pub fn decoder_working_bytes(&self, entry: &ZipEntry) -> Result<u64, ZipError> {
        let data = self.entry_data(entry)?;
        match entry.method {
            METHOD_STORE => Ok(0),
            METHOD_DEFLATE => Ok(inflate::WORKING_BYTES as u64),
            #[cfg(feature = "lzma")]
            METHOD_LZMA => {
                let (props, dict, _) = zip_lzma_properties(data)?;
                lzma_rust2::lzma_get_memory_usage_by_props(dict, props)
                    .map(|kib| u64::from(kib) * 1024)
                    .map_err(|_| ZipError::Decode)
            }
            method => Err(ZipError::UnsupportedCompression { method }),
        }
    }

    /// Reads a bounded prefix without validating the remainder of compressed data.
    pub fn entry_prefix(&self, entry: &ZipEntry, max_bytes: usize) -> Result<Vec<u8>, ZipError> {
        self.entry_prefix_with_decoder_limit(entry, max_bytes, Limits::default().max_input_bytes)
    }

    #[allow(missing_docs)]
    pub fn entry_prefix_with_decoder_limit(
        &self,
        entry: &ZipEntry,
        max_bytes: usize,
        decoder_limit: u64,
    ) -> Result<Vec<u8>, ZipError> {
        let required = self.decoder_working_bytes(entry)?;
        if required > decoder_limit {
            return Err(ZipError::DecoderTooLarge {
                required,
                max: decoder_limit,
            });
        }
        let data = self.entry_data(entry)?;
        let expected = usize::try_from(entry.uncompressed_size.min(max_bytes as u64))
            .map_err(|_| ZipError::Decode)?;
        match entry.method {
            METHOD_STORE => {
                if entry.uncompressed_size != data.len() as u64 {
                    return Err(ZipError::Corrupt);
                }
                Ok(data[..expected].to_vec())
            }
            METHOD_DEFLATE => inflate::prefix(data, expected).map_err(|_| ZipError::Decode),
            #[cfg(feature = "lzma")]
            METHOD_LZMA => decompress_zip_lzma(data, entry.uncompressed_size, expected, true),
            method => Err(ZipError::UnsupportedCompression { method }),
        }
    }

    /// Extracts one complete entry within the uncompressed byte limit.
    pub fn entry_bytes(
        &self,
        entry: &ZipEntry,
        limits: &Limits,
    ) -> Result<Cow<'a, [u8]>, ZipError> {
        self.entry_bytes_with_decoder_limit(entry, limits, limits.max_input_bytes)
    }

    #[allow(missing_docs)]
    pub fn entry_bytes_with_decoder_limit(
        &self,
        entry: &ZipEntry,
        limits: &Limits,
        decoder_limit: u64,
    ) -> Result<Cow<'a, [u8]>, ZipError> {
        let required = self.decoder_working_bytes(entry)?;
        if required > decoder_limit {
            return Err(ZipError::DecoderTooLarge {
                required,
                max: decoder_limit,
            });
        }
        if entry.uncompressed_size > limits.max_entry_bytes {
            return Err(ZipError::EntryTooLarge {
                size: entry.uncompressed_size,
                max: limits.max_entry_bytes,
            });
        }
        let data = self.entry_data(entry)?;

        match entry.method {
            METHOD_STORE => {
                if entry.uncompressed_size != data.len() as u64 {
                    return Err(ZipError::Corrupt);
                }
                Ok(Cow::Borrowed(data))
            }
            METHOD_DEFLATE => {
                let expected =
                    usize::try_from(entry.uncompressed_size).map_err(|_| ZipError::Decode)?;
                inflate(data, expected)
                    .map(Cow::Owned)
                    .map_err(|_| ZipError::Decode)
            }
            #[cfg(feature = "lzma")]
            METHOD_LZMA => decompress_zip_lzma(
                data,
                entry.uncompressed_size,
                usize::try_from(entry.uncompressed_size).map_err(|_| ZipError::Decode)?,
                false,
            )
            .map(Cow::Owned),
            method => Err(ZipError::UnsupportedCompression { method }),
        }
    }
}

#[cfg(feature = "lzma")]
fn zip_lzma_properties(data: &[u8]) -> Result<(u8, u32, &[u8]), ZipError> {
    if data.len() < 9 {
        return Err(ZipError::Corrupt);
    }
    let props_size = usize::from(u16::from_le_bytes([data[2], data[3]]));
    if props_size < 5 || data.len() < 4 + props_size {
        return Err(ZipError::Corrupt);
    }
    Ok((
        data[4],
        u32::from_le_bytes(data[5..9].try_into().expect("4 bytes")),
        &data[4 + props_size..],
    ))
}

#[cfg(feature = "lzma")]
fn decompress_zip_lzma(
    data: &[u8],
    uncompressed_size: u64,
    expected: usize,
    prefix: bool,
) -> Result<Vec<u8>, ZipError> {
    use std::io::Read as _;
    let (props, dict_size, stream) = zip_lzma_properties(data)?;
    let mut reader =
        lzma_rust2::LzmaReader::new_with_props(stream, uncompressed_size, props, dict_size, None)
            .map_err(|_| ZipError::Decode)?;
    let mut out = vec![0; expected];
    reader.read_exact(&mut out).map_err(|_| ZipError::Decode)?;
    if !prefix && reader.read(&mut [0]).map_err(|_| ZipError::Decode)? != 0 {
        return Err(ZipError::Decode);
    }
    Ok(out)
}
