use std::borrow::Cow;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use super::inflate::{self, inflate};
use crate::{ArchiveIndexCost, ArchiveIndexLimit, ArchiveIndexLimits, Limits};

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

static NEXT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(1);

/// A ZIP archive view over borrowed bytes (central-directory driven).
#[derive(Clone, PartialEq, Eq)]
pub struct ZipReader<'a> {
    directory: Arc<ZipDirectory>,
    bytes: &'a [u8],
}

impl fmt::Debug for ZipReader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZipReader")
            .field("byte_len", &self.bytes.len())
            .field("entry_count", &self.directory.entries.len())
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ZipDirectory {
    entries: Vec<ZipEntry>,
    id: u64,
}

/// Shared archive bytes with one lazily parsed directory.
#[derive(Clone)]
pub struct ZipArchive {
    inner: Arc<ArchiveData>,
}

struct ArchiveData {
    bytes: Arc<[u8]>,
    preflight: OnceLock<Result<DirectorySummary, ZipError>>,
    directory: OnceLock<Result<Arc<ZipDirectory>, ZipError>>,
    #[cfg(test)]
    parses: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    preflight_runs: std::sync::atomic::AtomicUsize,
}

#[allow(missing_docs)]
impl ZipArchive {
    /// Retain immutable archive bytes without parsing.
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            inner: Arc::new(ArchiveData {
                bytes: bytes.into(),
                preflight: OnceLock::new(),
                directory: OnceLock::new(),
                #[cfg(test)]
                parses: std::sync::atomic::AtomicUsize::new(0),
                #[cfg(test)]
                preflight_runs: std::sync::atomic::AtomicUsize::new(0),
            }),
        }
    }

    pub fn index_cost(&self) -> Result<ArchiveIndexCost, ZipError> {
        self.summary().map(|summary| summary.cost)
    }

    fn summary(&self) -> Result<&DirectorySummary, ZipError> {
        self.inner
            .preflight
            .get_or_init(|| {
                #[cfg(test)]
                self.inner.preflight_runs.fetch_add(1, Ordering::Relaxed);
                ZipDirectory::preflight(&self.inner.bytes)
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    pub fn reader(&self) -> Result<ZipReader<'_>, ZipError> {
        self.reader_with_index_limits(ArchiveIndexLimits::default())
    }

    pub fn reader_with_index_limits(
        &self,
        limits: ArchiveIndexLimits,
    ) -> Result<ZipReader<'_>, ZipError> {
        let summary = self.summary()?;
        limits.check(summary.cost).map_err(ZipError::IndexLimit)?;
        let directory = self.inner.directory.get_or_init(|| {
            #[cfg(test)]
            self.inner.parses.fetch_add(1, Ordering::Relaxed);
            ZipDirectory::materialize(&self.inner.bytes, summary).map(Arc::new)
        });
        match directory {
            Ok(directory) => Ok(ZipReader {
                bytes: &self.inner.bytes,
                directory: Arc::clone(directory),
            }),
            Err(error) => Err(error.clone()),
        }
    }
}

impl PartialEq for ZipArchive {
    fn eq(&self, other: &Self) -> bool {
        self.inner.bytes == other.inner.bytes
    }
}
impl Eq for ZipArchive {}

impl fmt::Debug for ZipArchive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZipArchive")
            .field("byte_len", &self.inner.bytes.len())
            .finish()
    }
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
    directory_id: u64,
}

impl ZipEntry {
    /// Whether the path escapes an extraction root; check before writing files.
    #[must_use]
    pub fn path_is_unsafe(&self) -> bool {
        crate::is_unsafe_entry_path(&self.path)
    }
}

/// ZIP reading failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ZipError {
    #[allow(missing_docs)]
    IndexLimit(ArchiveIndexLimit),
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
    /// The entry belongs to another archive directory.
    ForeignEntry,
}

impl fmt::Display for ZipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IndexLimit(error) => error.fmt(f),
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
            Self::ForeignEntry => write!(f, "zip entry belongs to another archive directory"),
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

#[derive(Clone, Copy, Debug)]
struct DirectorySummary {
    cost: ArchiveIndexCost,
    offset: usize,
    end: usize,
}

impl ZipDirectory {
    fn preflight(bytes: &[u8]) -> Result<DirectorySummary, ZipError> {
        if bytes.is_empty() {
            return Ok(DirectorySummary {
                cost: ArchiveIndexCost::default(),
                offset: 0,
                end: 0,
            });
        }
        let earliest = bytes
            .len()
            .saturating_sub(EOCD_BYTES + usize::from(u16::MAX));
        let mut candidate = bytes.len().checked_sub(EOCD_BYTES);
        let mut eocd = None;
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
        let entries = usize::from(read_u16(bytes, eocd + 10).ok_or(ZipError::Corrupt)?);
        if read_u16(bytes, eocd + 4) != Some(0)
            || read_u16(bytes, eocd + 6) != Some(0)
            || read_u16(bytes, eocd + 8) != Some(entries as u16)
        {
            return Err(ZipError::Corrupt);
        }
        let directory_bytes = u64::from(read_u32(bytes, eocd + 12).ok_or(ZipError::Corrupt)?);
        let offset = usize::try_from(read_u32(bytes, eocd + 16).ok_or(ZipError::Corrupt)?)
            .map_err(|_| ZipError::Corrupt)?;
        let end = offset
            .checked_add(usize::try_from(directory_bytes).map_err(|_| ZipError::Corrupt)?)
            .filter(|end| *end <= eocd)
            .ok_or(ZipError::Corrupt)?;
        if entries
            .checked_mul(CENTRAL_BYTES)
            .is_none_or(|minimum| minimum > end - offset)
        {
            return Err(ZipError::Corrupt);
        }
        let mut cost = ArchiveIndexCost {
            directory_bytes,
            entries,
            record_bytes: (entries as u64)
                .checked_mul(std::mem::size_of::<ZipEntry>() as u64)
                .ok_or(ZipError::Corrupt)?,
            ..ArchiveIndexCost::default()
        };
        let mut at = offset;
        for _ in 0..entries {
            if read_u32(bytes, at) != Some(CENTRAL_SIGNATURE) {
                return Err(ZipError::Corrupt);
            }
            let name_len = usize::from(read_u16(bytes, at + 28).ok_or(ZipError::Corrupt)?);
            let extra_len = usize::from(read_u16(bytes, at + 30).ok_or(ZipError::Corrupt)?);
            let comment_len = usize::from(read_u16(bytes, at + 32).ok_or(ZipError::Corrupt)?);
            let name_start = at.checked_add(CENTRAL_BYTES).ok_or(ZipError::Corrupt)?;
            let name_end = name_start.checked_add(name_len).ok_or(ZipError::Corrupt)?;
            let next = name_end
                .checked_add(extra_len)
                .and_then(|end| end.checked_add(comment_len))
                .filter(|next| *next <= end)
                .ok_or(ZipError::Corrupt)?;
            let name = bytes.get(name_start..name_end).ok_or(ZipError::Corrupt)?;
            cost.string_bytes = cost
                .string_bytes
                .checked_add(
                    crate::archive_index::lossy_utf8_len(name).ok_or(ZipError::Corrupt)? as u64,
                )
                .ok_or(ZipError::Corrupt)?;
            at = next;
        }
        Ok(DirectorySummary { cost, offset, end })
    }

    fn materialize(bytes: &[u8], summary: &DirectorySummary) -> Result<Self, ZipError> {
        let id = NEXT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let mut entries = Vec::with_capacity(summary.cost.entries);
        let mut at = summary.offset;
        for _ in 0..summary.cost.entries {
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
                directory_id: id,
            });
            at = at
                .checked_add(CENTRAL_BYTES + name_len + extra_len + comment_len)
                .filter(|at| *at <= summary.end)
                .ok_or(ZipError::Corrupt)?;
        }
        Ok(Self { entries, id })
    }
}

#[allow(missing_docs)]
impl<'a> ZipReader<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ZipError> {
        Self::parse_with_index_limits(bytes, ArchiveIndexLimits::default())
    }

    pub fn parse_with_index_limits(
        bytes: &'a [u8],
        limits: ArchiveIndexLimits,
    ) -> Result<Self, ZipError> {
        let summary = ZipDirectory::preflight(bytes)?;
        limits.check(summary.cost).map_err(ZipError::IndexLimit)?;
        Ok(Self {
            directory: Arc::new(ZipDirectory::materialize(bytes, &summary)?),
            bytes,
        })
    }

    /// All entries, directory order.
    #[must_use]
    pub fn entries(&self) -> &[ZipEntry] {
        &self.directory.entries
    }

    /// Look up one entry by exact path.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&ZipEntry> {
        self.directory
            .entries
            .iter()
            .find(|entry| entry.path == path)
    }

    fn entry_data(&self, entry: &ZipEntry) -> Result<&'a [u8], ZipError> {
        if entry.directory_id != self.directory.id {
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

#[cfg(test)]
mod tests;
