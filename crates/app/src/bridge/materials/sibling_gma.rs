use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;

use gmpublished_backend::GmaView;

use super::{ContentPath, MAX_LEGACY_BIN_FETCH_BYTES, SourceError, normalize_archive_path};

#[derive(Debug)]
pub(super) struct SiblingGmaIndex {
    archives: Vec<SiblingGmaArchive>,
    entries: HashMap<String, SiblingGmaEntryRef>,
    legacy_bin_cache: Mutex<Option<LegacyBinCache>>,
    blocked: Option<vformats::ArchiveIndexLimit>,
    _reservations: Vec<super::source_index::IndexReservation>,
}

impl SiblingGmaIndex {
    pub(super) fn index_error(
        &self,
        path: Option<&ContentPath>,
    ) -> Option<vformats::ArchiveIndexLimit> {
        if path.is_some_and(|path| self.entries.contains_key(path.as_str())) {
            None
        } else {
            self.blocked
        }
    }

    pub(super) fn for_each_path(&self, visit: &mut dyn FnMut(&str)) {
        self.entries.keys().for_each(|path| visit(path));
    }

    pub(super) fn entry_size(&self, path: &ContentPath) -> Option<u64> {
        self.entries
            .get(path.as_str())
            .map(|entry| match entry.location {
                SiblingGmaEntryLocation::Plain { len, .. }
                | SiblingGmaEntryLocation::LegacyBin { len, .. } => len,
            })
    }

    pub(super) fn texture_decoder_working_bytes(
        &self,
        path: &ContentPath,
    ) -> Result<usize, SourceError> {
        let entry = self
            .entries
            .get(path.as_str())
            .ok_or_else(|| SourceError::SiblingGma("entry vanished".to_owned()))?;
        let archive = &self.archives[entry.archive_index];
        match &archive.kind {
            SiblingGmaArchiveKind::Plain { .. } => Ok(0),
            SiblingGmaArchiveKind::LegacyBin { header, .. } => Ok(header.working_bytes),
        }
    }

    pub(super) fn reloadable_reader(
        &self,
        path: &ContentPath,
    ) -> Result<Arc<dyn super::reloadable::TextureSourceReader>, super::TextureAssetError> {
        use super::reloadable::{FrozenPathSource, SubrangeSource};
        let entry = self
            .entries
            .get(path.as_str())
            .ok_or(super::TextureAssetError::SourceChanged)?;
        let archive = self
            .archives
            .get(entry.archive_index)
            .ok_or(super::TextureAssetError::SourceChanged)?;
        let (path, offset, len, header) = match (&archive.kind, &entry.location) {
            (
                SiblingGmaArchiveKind::Plain { path },
                SiblingGmaEntryLocation::Plain { offset, len },
            ) => (path, *offset, *len, None),
            (
                SiblingGmaArchiveKind::LegacyBin { path, header, .. },
                SiblingGmaEntryLocation::LegacyBin { offset, len },
            ) => (path, *offset, *len, Some(*header)),
            _ => return Err(super::TextureAssetError::SourceChanged),
        };
        let file = File::open(path)
            .map_err(|e| super::TextureAssetError::SourceRead(e.to_string().into()))?;
        let source = FrozenPathSource::new(path.clone(), &file)?;
        if let Some(header) = header {
            Ok(Arc::new(LegacyTextureReader {
                source,
                header,
                offset,
                len,
            }))
        } else {
            Ok(Arc::new(SubrangeSource {
                reader: Arc::new(source),
                range: offset
                    ..offset
                        .checked_add(len)
                        .ok_or(super::TextureAssetError::WorkingCostOverflow)?,
                prefix: Arc::from([]),
            }))
        }
    }

    pub(super) fn texture_entry_range(
        &self,
        path: &ContentPath,
        max_bytes: u64,
        decoder_limit: usize,
    ) -> Result<Vec<u8>, SourceError> {
        let entry = self
            .entries
            .get(path.as_str())
            .ok_or_else(|| SourceError::SiblingGma("entry vanished".to_owned()))?;
        let archive = self
            .archives
            .get(entry.archive_index)
            .ok_or_else(|| SourceError::SiblingGma("archive vanished".to_owned()))?;
        let result = match (&archive.kind, &entry.location) {
            (
                SiblingGmaArchiveKind::Plain { path },
                SiblingGmaEntryLocation::Plain { offset, len },
            ) => {
                return GmaView::open(path)
                    .and_then(|view| view.read_payload_bytes(*offset, (*len).min(max_bytes)))
                    .map_err(|error| SourceError::SiblingGma(error.to_string()));
            }
            (
                SiblingGmaArchiveKind::LegacyBin { path, header, .. },
                SiblingGmaEntryLocation::LegacyBin { offset, len },
            ) => {
                let mut reader = LegacyBinSource::open_matching(path, *header)
                    .and_then(|source| source.into_decoder(decoder_limit))
                    .map_err(|error| SourceError::SiblingGma(error.to_string()))?;
                let skipped = io::copy(&mut reader.by_ref().take(*offset), &mut io::sink())
                    .map_err(|error| SourceError::SiblingGma(error.to_string()))?;
                if skipped != *offset {
                    return Err(SourceError::SiblingGma(
                        "truncated archive prefix".to_owned(),
                    ));
                }
                let count = usize::try_from((*len).min(max_bytes))
                    .map_err(|error| SourceError::SiblingGma(error.to_string()))?;
                let mut bytes = vec![0; count];
                reader.read_exact(&mut bytes).map(|()| bytes)
            }
            _ => {
                return Err(SourceError::SiblingGma(
                    "inconsistent entry location".to_owned(),
                ));
            }
        };
        result.map_err(|error| SourceError::SiblingGma(error.to_string()))
    }

    pub(super) fn entry_bytes(&self, path: &ContentPath) -> Result<Option<Vec<u8>>, SourceError> {
        let Some(entry) = self.entries.get(path.as_str()) else {
            return Ok(None);
        };
        let Some(archive) = self.archives.get(entry.archive_index) else {
            return Ok(None);
        };
        match (&archive.kind, &entry.location) {
            // Open per read; retaining every sibling archive can exhaust file descriptors.
            (
                SiblingGmaArchiveKind::Plain { path },
                SiblingGmaEntryLocation::Plain { offset, len },
            ) => GmaView::open(path)
                .and_then(|view| view.read_payload_bytes(*offset, *len))
                .map(Some)
                .map_err(|error| SourceError::SiblingGma(error.to_string())),
            (
                SiblingGmaArchiveKind::LegacyBin {
                    path,
                    data_end,
                    header,
                },
                SiblingGmaEntryLocation::LegacyBin { offset, len },
            ) => Ok(self.legacy_bin_entry_bytes(
                entry.archive_index,
                path,
                *header,
                *data_end,
                *offset,
                *len,
            )),
            _ => Ok(None),
        }
    }

    fn legacy_bin_entry_bytes(
        &self,
        archive_index: usize,
        path: &Path,
        header: LegacyBinHeader,
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
        let bytes = match decompress_legacy_bin_prefix(path, header, target_end) {
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
                bytes: Arc::from(bytes),
            });
        }
        Some(entry_bytes)
    }
}

#[derive(Debug)]
struct LegacyTextureReader {
    source: super::reloadable::FrozenPathSource,
    header: LegacyBinHeader,
    offset: u64,
    len: u64,
}
impl super::reloadable::TextureSourceReader for LegacyTextureReader {
    fn len(&self) -> u64 {
        self.len
    }
    fn decoder_working_bytes(&self) -> usize {
        self.header.working_bytes
    }
    fn read_range(
        &self,
        range: std::ops::Range<u64>,
        cancel: &crate::preparation::PreparationCancellation,
    ) -> Result<Vec<u8>, super::TextureAssetError> {
        use super::TextureAssetError;
        cancel.check()?;
        if range.start > range.end || range.end > self.len {
            return Err(TextureAssetError::Selection);
        }
        let mut file = self.source.open_verified()?;
        let map_error = |e: io::Error| TextureAssetError::SourceRead(e.to_string().into());
        let actual = LegacyBinHeader::read(&mut file).map_err(map_error)?;
        if actual != self.header {
            return Err(TextureAssetError::SourceChanged);
        }
        let mut decoder = self.header.decoder(&mut file).map_err(map_error)?;
        let mut left = self
            .offset
            .checked_add(range.start)
            .ok_or(TextureAssetError::WorkingCostOverflow)?;
        let mut discard = [0; 8192];
        while left > 0 {
            cancel.check()?;
            let count = usize::try_from(left.min(discard.len() as u64)).expect("bounded chunk");
            decoder
                .read_exact(&mut discard[..count])
                .map_err(map_error)?;
            left -= count as u64;
        }
        let count = usize::try_from(range.end - range.start)
            .map_err(|_| TextureAssetError::WorkingCostOverflow)?;
        let mut output = vec![0; count];
        for chunk in output.chunks_mut(8192) {
            cancel.check()?;
            decoder.read_exact(chunk).map_err(map_error)?;
        }
        drop(decoder);
        self.source.verify(&file)?;
        cancel.check()?;
        Ok(output)
    }
}

#[derive(Debug)]
pub(super) struct SiblingGmaArchive {
    kind: SiblingGmaArchiveKind,
}

#[derive(Debug)]
pub(super) enum SiblingGmaArchiveKind {
    Plain {
        path: PathBuf,
    },
    LegacyBin {
        path: PathBuf,
        data_end: u64,
        header: LegacyBinHeader,
    },
}

#[derive(Clone, Debug)]
pub(super) struct LegacyBinCache {
    archive_index: usize,
    bytes: Arc<[u8]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SiblingGmaEntryRef {
    archive_index: usize,
    location: SiblingGmaEntryLocation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum SiblingGmaEntryLocation {
    Plain { offset: u64, len: u64 },
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

#[cfg(test)]
pub(super) fn build_sibling_gma_index(paths: &[SiblingGmaPath]) -> SiblingGmaIndex {
    build_sibling_gma_index_with_budget(paths, &super::SourceIndexBudget::defaults())
}

fn adapter_cost(
    mut cost: vformats::ArchiveIndexCost,
    path: &Path,
) -> Result<vformats::ArchiveIndexCost, gmpublished_backend::GmaError> {
    let overflow = || {
        gmpublished_backend::GmaError::IndexLimit(vformats::ArchiveIndexLimit::arithmetic_overflow())
    };
    let per_entry = size_of::<gmpublished_backend::GmaIndexedEntry>()
        + size_of::<(String, SiblingGmaEntryRef)>();
    cost.record_bytes = cost
        .record_bytes
        .checked_add(
            (cost.entries as u64)
                .checked_mul(per_entry as u64)
                .ok_or_else(overflow)?,
        )
        .and_then(|bytes| {
            bytes.checked_add(
                (size_of::<SiblingGmaArchive>()
                    + size_of::<super::source_index::IndexReservation>()) as u64,
            )
        })
        .ok_or_else(overflow)?;
    cost.string_bytes = cost
        .string_bytes
        .checked_mul(3)
        .and_then(|bytes| bytes.checked_add(path.as_os_str().len() as u64))
        .ok_or_else(overflow)?;
    Ok(cost)
}

#[cfg(test)]
pub(super) fn build_sibling_gma_index_with_budget(
    paths: &[SiblingGmaPath],
    budget: &Arc<super::SourceIndexBudget>,
) -> SiblingGmaIndex {
    build_sibling_gma_index_cancellable(
        paths,
        budget,
        &crate::preparation::PreparationCancellation::default(),
    )
    .expect("uncancelled index construction")
}

enum IndexBuildError {
    Cancelled(crate::preparation::Cancelled),
    Gma(gmpublished_backend::GmaError),
}
impl From<crate::preparation::Cancelled> for IndexBuildError {
    fn from(error: crate::preparation::Cancelled) -> Self {
        Self::Cancelled(error)
    }
}
impl From<std::io::Error> for IndexBuildError {
    fn from(error: std::io::Error) -> Self {
        Self::Gma(error.into())
    }
}
impl From<gmpublished_backend::GmaError> for IndexBuildError {
    fn from(error: gmpublished_backend::GmaError) -> Self {
        Self::Gma(error)
    }
}

pub(super) fn build_sibling_gma_index_cancellable(
    paths: &[SiblingGmaPath],
    budget: &Arc<super::SourceIndexBudget>,
    cancellation: &crate::preparation::PreparationCancellation,
) -> Result<SiblingGmaIndex, crate::preparation::Cancelled> {
    let mut index = SiblingGmaIndex {
        archives: Vec::new(),
        entries: HashMap::new(),
        legacy_bin_cache: Mutex::new(None),
        blocked: None,
        _reservations: Vec::new(),
    };
    for path in paths {
        cancellation.check()?;
        let result = (|| {
            let limits = budget.limits();
            let (entries, kind) = match path.kind {
                SiblingGmaPathKind::Plain => {
                    let view = GmaView::open(&path.path)?;
                    let _working = budget
                        .working_request(64 * 1024)
                        .map_err(gmpublished_backend::GmaError::IndexLimit)?
                        .acquire_cancellable(cancellation)?;
                    let cost = view.index_cost_with_limits(limits)?;
                    let reservation = budget
                        .reserve(adapter_cost(cost, &path.path)?)
                        .map_err(gmpublished_backend::GmaError::IndexLimit)?;
                    let bundle = view.meta_with_index_limits(&path.path, limits)?;
                    index._reservations.push(reservation);
                    (
                        bundle.entries,
                        SiblingGmaArchiveKind::Plain {
                            path: path.path.clone(),
                        },
                    )
                }
                SiblingGmaPathKind::LegacyBin => {
                    let mut source = LegacyBinSource::open(&path.path)?;
                    let decoder_bytes = source.header.working_bytes;
                    let working = decoder_bytes.checked_add(64 * 1024).ok_or_else(|| {
                        gmpublished_backend::GmaError::IndexLimit(
                            vformats::ArchiveIndexLimit::arithmetic_overflow(),
                        )
                    })?;
                    let _working = budget
                        .working_request(working)
                        .map_err(gmpublished_backend::GmaError::IndexLimit)?
                        .acquire_cancellable(cancellation)?;
                    let cost = GmaView::stream_index_cost(source.decoder()?, u64::MAX, limits)?;
                    let reservation = budget
                        .reserve(adapter_cost(cost, &path.path)?)
                        .map_err(gmpublished_backend::GmaError::IndexLimit)?;
                    let bundle = GmaView::stream_index(source.decoder()?, u64::MAX, limits, cost)?;
                    let data_end = bundle.entries.last().map_or(cost.directory_bytes, |entry| {
                        entry.data_offset.saturating_add(entry.size)
                    });
                    index._reservations.push(reservation);
                    (
                        bundle.entries,
                        SiblingGmaArchiveKind::LegacyBin {
                            path: path.path.clone(),
                            data_end,
                            header: source.header,
                        },
                    )
                }
            };
            Ok::<_, IndexBuildError>((entries, kind))
        })();
        let (entries, kind) = match result {
            Ok(indexed) => indexed,
            Err(IndexBuildError::Cancelled(error)) => return Err(error),
            Err(IndexBuildError::Gma(gmpublished_backend::GmaError::IndexLimit(error))) => {
                index.blocked = Some(budget.reject(error));
                break;
            }
            Err(IndexBuildError::Gma(error)) => {
                log::debug!(
                    "sibling GMA index failed for {}: {error}",
                    path.path.display()
                );
                continue;
            }
        };
        let archive_index = index.archives.len();
        let legacy = matches!(kind, SiblingGmaArchiveKind::LegacyBin { .. });
        for entry in entries {
            let Some(normalized) = normalize_archive_path(&entry.path) else {
                continue;
            };
            let location = if legacy {
                SiblingGmaEntryLocation::LegacyBin {
                    offset: entry.data_offset,
                    len: entry.size,
                }
            } else {
                SiblingGmaEntryLocation::Plain {
                    offset: entry.data_offset,
                    len: entry.size,
                }
            };
            index
                .entries
                .entry(normalized)
                .or_insert(SiblingGmaEntryRef {
                    archive_index,
                    location,
                });
        }
        index.archives.push(SiblingGmaArchive { kind });
    }
    cancellation.check()?;
    Ok(index)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LegacyBinHeader {
    bytes: [u8; 13],
    working_bytes: usize,
}

impl LegacyBinHeader {
    fn read(input: &mut impl Read) -> io::Result<Self> {
        let bytes = read_array::<13, _>(input)?;
        let working_bytes = legacy_decoder_working_bytes(
            u32::from_le_bytes(bytes[1..5].try_into().expect("4 bytes")),
            bytes[0],
        )?;
        Ok(Self {
            bytes,
            working_bytes,
        })
    }

    fn decoder<R: Read>(self, input: R) -> io::Result<lzma_rust2::LzmaReader<BufReader<R>>> {
        lzma_rust2::LzmaReader::new_with_props(
            BufReader::with_capacity(8192, input),
            u64::from_le_bytes(self.bytes[5..13].try_into().expect("8 bytes")),
            self.bytes[0],
            u32::from_le_bytes(self.bytes[1..5].try_into().expect("4 bytes")),
            None,
        )
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

struct LegacyBinSource {
    file: File,
    header: LegacyBinHeader,
}

impl LegacyBinSource {
    fn open(path: &Path) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let header = LegacyBinHeader::read(&mut file)?;
        Ok(Self { file, header })
    }

    fn open_matching(path: &Path, header: LegacyBinHeader) -> io::Result<Self> {
        let source = Self::open(path)?;
        if source.header != header {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "legacy GMA header changed after indexing",
            ));
        }
        Ok(source)
    }

    fn decoder(&mut self) -> io::Result<lzma_rust2::LzmaReader<BufReader<&mut File>>> {
        self.file.seek(SeekFrom::Start(13))?;
        self.header.decoder(&mut self.file)
    }

    fn into_decoder(self, limit: usize) -> io::Result<lzma_rust2::LzmaReader<BufReader<File>>> {
        if self.header.working_bytes > limit {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!(
                    "LZMA decoder requires {} bytes; limit {limit}",
                    self.header.working_bytes
                ),
            ));
        }
        self.header.decoder(self.file)
    }
}

fn legacy_decoder_working_bytes(dict: u32, props: u8) -> io::Result<usize> {
    lzma_rust2::lzma_get_memory_usage_by_props(dict, props)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        .and_then(|kib| {
            (kib as usize)
                .checked_mul(1024)
                .and_then(|bytes| bytes.checked_add(8192))
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "decoder cost overflow"))
        })
}

fn decompress_legacy_bin_prefix(
    path: &Path,
    header: LegacyBinHeader,
    target_len: u64,
) -> io::Result<Vec<u8>> {
    let target_len = target_len.min(MAX_LEGACY_BIN_FETCH_BYTES);
    let target_len_usize = usize::try_from(target_len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "legacy GMA target too large"))?;
    let mut decoder = LegacyBinSource::open_matching(path, header)?.into_decoder(
        usize::try_from(vformats::Limits::default().max_input_bytes).unwrap_or(usize::MAX),
    )?;
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

pub(super) fn read_array<const N: usize, R: Read>(reader: &mut R) -> io::Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{GmaFixtureBuilder, write_gma_fixture};
    use std::io::Write;

    fn fixture(root: &Path) -> PathBuf {
        let raw = write_gma_fixture(
            root.join("source.gma"),
            &GmaFixtureBuilder::new("Pinned header")
                .entry("materials/a.vmt", b"UnlitGeneric {}".to_vec())
                .build(),
        );
        let input = std::fs::read(raw).unwrap();
        let mut encoder = lzma_rust2::LzmaWriter::new_use_header(
            Vec::new(),
            &lzma_rust2::LzmaOptions::with_preset(1),
            None,
        )
        .unwrap();
        encoder.write_all(&input).unwrap();
        let path = root.join("source.bin");
        std::fs::write(&path, encoder.finish().unwrap()).unwrap();
        path
    }

    fn larger_dictionary(path: &Path, mut header: [u8; 13]) {
        header[1..5].copy_from_slice(&(1024u32 * 1024 * 1024).to_le_bytes());
        std::fs::write(path, header).unwrap();
    }

    #[test]
    fn index_cost_and_both_scans_keep_the_original_opened_file_and_header() {
        let root = tempfile::TempDir::new().unwrap();
        let path = fixture(root.path());
        let mut source = LegacyBinSource::open(&path).unwrap();
        let header = source.header;
        let budget = super::super::SourceIndexBudget::new(
            vformats::ArchiveIndexLimits::default(),
            Arc::new(super::super::TextureWorkingBudget::new(
                header.working_bytes + 64 * 1024,
            )),
        );
        let _working = budget.working(header.working_bytes + 64 * 1024).unwrap();
        std::fs::rename(&path, root.path().join("original.bin")).unwrap();
        larger_dictionary(&path, header.bytes);
        let cost = GmaView::stream_index_cost(source.decoder().unwrap(), u64::MAX, budget.limits())
            .unwrap();
        let bundle =
            GmaView::stream_index(source.decoder().unwrap(), u64::MAX, budget.limits(), cost)
                .unwrap();
        assert_eq!(bundle.entries.len(), 1);
        assert_eq!(bundle.entries[0].path, "materials/a.vmt");
        assert_eq!(source.header, header);
        assert!(
            matches!(LegacyBinSource::open_matching(&path, header), Err(error) if error.kind() == io::ErrorKind::InvalidData)
        );
    }

    #[test]
    fn excessive_dictionary_keeps_a_policy_barrier_before_any_later_archive() {
        let root = tempfile::TempDir::new().unwrap();
        let first = fixture(root.path());
        let header = LegacyBinSource::open(&first).unwrap().header;
        larger_dictionary(&first, header.bytes);
        let second = write_gma_fixture(
            root.path().join("later.gma"),
            &GmaFixtureBuilder::new("Later")
                .entry("materials/lower.vmt", b"UnlitGeneric {}".to_vec())
                .build(),
        );
        let budget = super::super::SourceIndexBudget::new(
            vformats::ArchiveIndexLimits::default(),
            Arc::new(super::super::TextureWorkingBudget::new(64 * 1024)),
        );
        let index = build_sibling_gma_index_with_budget(
            &[
                SiblingGmaPath::legacy_bin(first),
                SiblingGmaPath::plain(second),
            ],
            &budget,
        );
        assert!(index.archives.is_empty());
        assert_eq!(
            index
                .index_error(Some(&ContentPath::new("materials/lower.vmt").unwrap()))
                .unwrap()
                .kind,
            vformats::ArchiveIndexLimitKind::WorkingBytes
        );
        assert_eq!(budget.stats().archives, 0);
        assert_eq!(budget.stats().rejected_archives, 1);
    }

    #[test]
    fn payload_reads_reuse_the_admitted_header_cost_and_refuse_replacements() {
        let root = tempfile::TempDir::new().unwrap();
        let path = fixture(root.path());
        let header = LegacyBinSource::open(&path).unwrap().header;
        let index = build_sibling_gma_index(&[SiblingGmaPath::legacy_bin(path.clone())]);
        let content = ContentPath::new("materials/a.vmt").unwrap();
        assert_eq!(
            index.texture_decoder_working_bytes(&content).unwrap(),
            header.working_bytes
        );
        larger_dictionary(&path, header.bytes);
        assert_eq!(
            index.texture_decoder_working_bytes(&content).unwrap(),
            header.working_bytes
        );
        assert!(
            matches!(index.texture_entry_range(&content, 64, usize::MAX), Err(SourceError::SiblingGma(error)) if error == "legacy GMA header changed after indexing")
        );
    }
}
