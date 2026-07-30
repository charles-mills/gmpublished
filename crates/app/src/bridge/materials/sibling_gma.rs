//! Sibling GMAs: the addons installed next to the one being previewed, which
//! Source content routinely depends on.
//!
//! Two archive shapes to read. Plain `.gma` files go through the backend's
//! reader; Steam's legacy `.bin` downloads are LZMA-compressed whole, so this
//! carries its own index parser and a decompressed-prefix cache rather than
//! inflating a multi-gigabyte archive to reach one entry.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufRead, BufReader, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;
use rayon::prelude::*;

use gmpublished_backend::GmaView;

use super::{
    ContentPath, GMA_MAGIC, MAX_LEGACY_BIN_ENTRY_TABLE_BYTES, MAX_LEGACY_BIN_FETCH_BYTES,
    MAX_SIBLING_GMA_ARCHIVES, SourceError, normalize_archive_path,
};

#[derive(Debug)]
pub(super) struct SiblingGmaIndex {
    archives: Vec<SiblingGmaArchive>,
    entries: HashMap<String, SiblingGmaEntryRef>,
    legacy_bin_cache: Mutex<Option<LegacyBinCache>>,
}

impl SiblingGmaIndex {
    pub(super) fn for_each_path(&self, visit: &mut dyn FnMut(&str)) {
        self.entries.keys().for_each(|path| visit(path));
    }

    pub(super) fn entry_bytes(&self, path: &ContentPath) -> Result<Option<Vec<u8>>, SourceError> {
        let Some(entry) = self.entries.get(path.as_str()) else {
            return Ok(None);
        };
        let Some(archive) = self.archives.get(entry.archive_index) else {
            return Ok(None);
        };
        match (&archive.kind, &entry.location) {
            (
                SiblingGmaArchiveKind::Plain { view, .. },
                SiblingGmaEntryLocation::Plain { entry_path },
            ) => view
                .read_entry_bytes(entry_path)
                .map(Some)
                .map_err(|error| SourceError::SiblingGma(error.to_string())),
            (
                SiblingGmaArchiveKind::LegacyBin { path, data_end },
                SiblingGmaEntryLocation::LegacyBin { offset, len },
            ) => Ok(self.legacy_bin_entry_bytes(
                entry.archive_index,
                path,
                *data_end,
                *offset,
                *len,
            )),
            // The index records a location shape its archive cannot serve;
            // treated as absent rather than broken, since nothing was read.
            _ => Ok(None),
        }
    }

    fn legacy_bin_entry_bytes(
        &self,
        archive_index: usize,
        path: &Path,
        data_end: u64,
        offset: u64,
        len: u64,
    ) -> Option<Vec<u8>> {
        let end = offset.checked_add(len)?;
        if end > MAX_LEGACY_BIN_FETCH_BYTES {
            log::debug!(
                "sibling material legacy GMA entry over fetch cap for {}: end {end}",
                path.display()
            );
            return None;
        }

        if let Some(bytes) = self
            .legacy_bin_cache
            .lock()
            .as_ref()
            .filter(|cache| cache.archive_index == archive_index)
            .cloned()
            && let Some(entry) = slice_legacy_bin_entry(&bytes.bytes, offset, len)
        {
            return Some(entry.to_vec());
        }

        let target_end = if data_end <= MAX_LEGACY_BIN_FETCH_BYTES {
            data_end
        } else {
            end
        };
        let bytes = match decompress_legacy_bin_prefix(path, target_end) {
            Ok(bytes) => bytes,
            Err(error) => {
                log::debug!(
                    "sibling material legacy GMA fetch failed for {}: {error}",
                    path.display()
                );
                return None;
            }
        };
        let entry_bytes = slice_legacy_bin_entry(&bytes, offset, len)?.to_vec();
        if u64::try_from(bytes.len()).ok() == Some(data_end) {
            *self.legacy_bin_cache.lock() = Some(LegacyBinCache {
                archive_index,
                bytes: Arc::new(bytes),
            });
        }
        Some(entry_bytes)
    }
}

#[derive(Debug)]
pub(super) struct SiblingGmaArchive {
    kind: SiblingGmaArchiveKind,
}

/// `view` has no `Debug` of its own; the derive on this enum only needs `gma`.
pub(super) enum SiblingGmaArchiveKind {
    Plain {
        gma: Box<gmpublished_backend::GmaFile>,
        view: Box<gmpublished_backend::GmaView>,
    },
    LegacyBin {
        path: PathBuf,
        data_end: u64,
    },
}

impl std::fmt::Debug for SiblingGmaArchiveKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plain { gma, .. } => f
                .debug_struct("Plain")
                .field("gma", gma)
                .finish_non_exhaustive(),
            Self::LegacyBin { path, data_end } => f
                .debug_struct("LegacyBin")
                .field("path", path)
                .field("data_end", data_end)
                .finish(),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct LegacyBinCache {
    archive_index: usize,
    bytes: Arc<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SiblingGmaEntryRef {
    archive_index: usize,
    location: SiblingGmaEntryLocation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SiblingGmaEntryLocation {
    Plain { entry_path: String },
    LegacyBin { offset: u64, len: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SiblingGmaPath {
    pub(super) path: PathBuf,
    pub(super) kind: SiblingGmaPathKind,
}

impl SiblingGmaPath {
    pub(super) fn plain(path: PathBuf) -> Self {
        Self {
            path,
            kind: SiblingGmaPathKind::Plain,
        }
    }

    pub(super) fn legacy_bin(path: PathBuf) -> Self {
        Self {
            path,
            kind: SiblingGmaPathKind::LegacyBin,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SiblingGmaPathKind {
    Plain,
    LegacyBin,
}

#[derive(Debug)]
pub(super) struct LegacyBinEntry {
    normalized_path: String,
    offset: u64,
    len: u64,
}

#[derive(Debug)]
pub(super) struct LegacyBinIndex {
    entries: Vec<LegacyBinEntry>,
    data_end: u64,
}

pub(super) fn build_sibling_gma_index(paths: &[SiblingGmaPath]) -> SiblingGmaIndex {
    let skipped = paths.len().saturating_sub(MAX_SIBLING_GMA_ARCHIVES);
    if skipped > 0 {
        log::debug!(
            "skipping {skipped} sibling material GMA archives over cap {MAX_SIBLING_GMA_ARCHIVES}"
        );
    }

    // Opening every archive (and LZMA-decompressing every legacy-bin entry
    // table) is the expensive part and each path is independent — do it in
    // parallel, then merge sequentially so first-wins entry priority keeps
    // the original path order.
    type IndexedArchive = (SiblingGmaArchive, Vec<(String, SiblingGmaEntryLocation)>);
    let indexed: Vec<Option<IndexedArchive>> = paths[..paths.len().min(MAX_SIBLING_GMA_ARCHIVES)]
        .par_iter()
        .map(|path| match path.kind {
            SiblingGmaPathKind::Plain => {
                // One file open + one parse for the entry table; the view is
                // retained for entry fetches.
                let view = match GmaView::open(&path.path) {
                    Ok(view) => view,
                    Err(error) => {
                        log::debug!(
                            "sibling material GMA open failed for {}: {error}",
                            path.path.display()
                        );
                        return None;
                    }
                };
                let bundle = match view.meta(&path.path) {
                    Ok(bundle) => bundle,
                    Err(error) => {
                        log::debug!(
                            "sibling material GMA entry table failed for {}: {error}",
                            path.path.display()
                        );
                        return None;
                    }
                };
                let entries = bundle
                    .entries
                    .into_iter()
                    .filter_map(|entry| {
                        let normalized = normalize_archive_path(&entry.path)?;
                        Some((
                            normalized,
                            SiblingGmaEntryLocation::Plain {
                                entry_path: entry.path,
                            },
                        ))
                    })
                    .collect();
                Some((
                    SiblingGmaArchive {
                        kind: SiblingGmaArchiveKind::Plain {
                            gma: Box::new(bundle.handle),
                            view: Box::new(view),
                        },
                    },
                    entries,
                ))
            }
            SiblingGmaPathKind::LegacyBin => {
                let legacy_index = match read_legacy_bin_index(&path.path) {
                    Ok(index) => index,
                    Err(error) => {
                        log::debug!(
                            "sibling material legacy GMA index failed for {}: {error}",
                            path.path.display()
                        );
                        return None;
                    }
                };
                let entries = legacy_index
                    .entries
                    .into_iter()
                    .map(|entry| {
                        (
                            entry.normalized_path,
                            SiblingGmaEntryLocation::LegacyBin {
                                offset: entry.offset,
                                len: entry.len,
                            },
                        )
                    })
                    .collect();
                Some((
                    SiblingGmaArchive {
                        kind: SiblingGmaArchiveKind::LegacyBin {
                            path: path.path.clone(),
                            data_end: legacy_index.data_end,
                        },
                    },
                    entries,
                ))
            }
        })
        .collect();

    let mut archives = Vec::new();
    let mut entries = HashMap::new();
    for (archive, archive_entries) in indexed.into_iter().flatten() {
        let archive_index = archives.len();
        for (normalized, location) in archive_entries {
            entries
                .entry(normalized)
                .or_insert_with(|| SiblingGmaEntryRef {
                    archive_index,
                    location,
                });
        }
        archives.push(archive);
    }

    SiblingGmaIndex {
        archives,
        entries,
        legacy_bin_cache: Mutex::new(None),
    }
}

pub(super) fn read_legacy_bin_index(path: &Path) -> io::Result<LegacyBinIndex> {
    let decoder = legacy_bin_decoder(path)?;
    let limited = LimitedReader::new(decoder, MAX_LEGACY_BIN_ENTRY_TABLE_BYTES);
    let mut reader = BufReader::with_capacity(64 * 1024, limited);
    let magic = read_array::<4, _>(&mut reader)?;
    if &magic != GMA_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing GMA header",
        ));
    }
    let version = read_u8(&mut reader)?;
    read_u64_le(&mut reader)?; // steamid
    read_u64_le(&mut reader)?; // timestamp
    if version > 1 {
        read_nt_string(&mut reader)?;
    }
    read_nt_string(&mut reader)?; // title
    read_nt_string(&mut reader)?; // description
    read_nt_string(&mut reader)?; // author
    read_i32_le(&mut reader)?; // addon version

    let mut entries = Vec::new();
    let mut entry_cursor = 0_u64;
    loop {
        let index = read_u32_le(&mut reader)?;
        if index == 0 {
            break;
        }
        let entry_path = read_nt_string(&mut reader)?;
        let size = read_i64_le(&mut reader)?;
        read_u32_le(&mut reader)?; // crc
        let size = u64::try_from(size)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative GMA entry size"))?;
        let offset = entry_cursor;
        entry_cursor = entry_cursor.checked_add(size).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "GMA entry table overflow")
        })?;
        if let Some(normalized_path) = normalize_archive_path(&entry_path) {
            entries.push(LegacyBinEntry {
                normalized_path,
                offset,
                len: size,
            });
        }
    }

    // `BufReader` may have decompressed part of the payload while filling its
    // buffer. Only bytes already handed to the parser belong to the table.
    let data_start = reader
        .get_ref()
        .bytes_read()
        .saturating_sub(reader.buffer().len() as u64);
    for entry in &mut entries {
        entry.offset = entry.offset.checked_add(data_start).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "GMA entry offset overflow")
        })?;
    }
    let data_end = data_start
        .checked_add(entry_cursor)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "GMA data offset overflow"))?;
    Ok(LegacyBinIndex { entries, data_end })
}

pub(super) fn legacy_bin_decoder(
    path: &Path,
) -> io::Result<lzma_rust2::LzmaReader<BufReader<File>>> {
    let mut input = File::open(path)?;
    let header = read_array::<13, _>(&mut input)?;
    let props = header[0];
    let dict_size = u32::from_le_bytes(
        header[1..5]
            .try_into()
            .expect("slice length was checked above"),
    );
    let unpacked_size = u64::from_le_bytes(
        header[5..13]
            .try_into()
            .expect("slice length was checked above"),
    );
    lzma_rust2::LzmaReader::new_with_props(
        BufReader::new(input),
        unpacked_size,
        props,
        dict_size,
        None,
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(super) fn decompress_legacy_bin_prefix(path: &Path, target_len: u64) -> io::Result<Vec<u8>> {
    let target_len = target_len.min(MAX_LEGACY_BIN_FETCH_BYTES);
    let target_len_usize = usize::try_from(target_len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "legacy GMA target too large"))?;
    let mut decoder = legacy_bin_decoder(path)?;
    let mut bytes = Vec::with_capacity(target_len_usize.min(1024 * 1024));
    let mut chunk = [0_u8; 16 * 1024];
    while bytes.len() < target_len_usize {
        let remaining = target_len_usize - bytes.len();
        let read_len = remaining.min(chunk.len());
        match decoder.read(&mut chunk[..read_len]) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    if bytes.len() < target_len_usize {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "legacy GMA ended before requested entry",
        ));
    }
    Ok(bytes)
}

pub(super) fn slice_legacy_bin_entry(bytes: &[u8], offset: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let len = usize::try_from(len).ok()?;
    let end = start.checked_add(len)?;
    bytes.get(start..end)
}

pub(super) struct LimitedReader<R> {
    inner: R,
    bytes_read: u64,
    limit: u64,
}

impl<R> LimitedReader<R> {
    fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            bytes_read: 0,
            limit,
        }
    }

    fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.bytes_read >= self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "legacy GMA entry table exceeded decompressed cap",
            ));
        }
        let remaining = usize::try_from(self.limit - self.bytes_read).unwrap_or(usize::MAX);
        let read_len = output.len().min(remaining);
        let n = self.inner.read(&mut output[..read_len])?;
        self.bytes_read = self.bytes_read.saturating_add(n as u64);
        Ok(n)
    }
}

pub(super) fn read_array<const N: usize, R: Read>(reader: &mut R) -> io::Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn read_u8(reader: &mut impl Read) -> io::Result<u8> {
    Ok(read_array::<1, _>(reader)?[0])
}

pub(super) fn read_u32_le(reader: &mut impl Read) -> io::Result<u32> {
    Ok(u32::from_le_bytes(read_array::<4, _>(reader)?))
}

pub(super) fn read_i32_le(reader: &mut impl Read) -> io::Result<i32> {
    Ok(i32::from_le_bytes(read_array::<4, _>(reader)?))
}

pub(super) fn read_u64_le(reader: &mut impl Read) -> io::Result<u64> {
    Ok(u64::from_le_bytes(read_array::<8, _>(reader)?))
}

pub(super) fn read_i64_le(reader: &mut impl Read) -> io::Result<i64> {
    Ok(i64::from_le_bytes(read_array::<8, _>(reader)?))
}

pub(super) fn read_nt_string(reader: &mut impl BufRead) -> io::Result<String> {
    let mut bytes = Vec::new();
    let read = reader.read_until(0, &mut bytes)?;
    if read == 0 || bytes.last() != Some(&0) {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unterminated GMA string",
        ));
    }
    bytes.pop();
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
