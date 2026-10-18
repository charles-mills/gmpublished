use super::{Arc, ContentPath, MaterialResolver, SourceError, SourceRef, TextureAssetError};
use std::fs::File;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SOURCE_TOKEN: AtomicU64 = AtomicU64::new(1);

pub(super) fn source_token() -> u64 {
    NEXT_SOURCE_TOKEN.fetch_add(1, Ordering::Relaxed)
}

impl MaterialResolver {
    pub(crate) fn with_texture_source_working_budget(
        &self,
        budget: Arc<super::TextureWorkingBudget>,
    ) -> Self {
        self.variant(super::ResolverConfig {
            texture_source_working_budget: Some(budget),
            ..self.config.clone()
        })
    }

    pub(crate) fn with_texture_decoder_limit(&self, bytes: usize) -> Self {
        self.variant(super::ResolverConfig {
            texture_decoder_limit: Some(bytes),
            ..self.config.clone()
        })
    }
}

#[derive(Debug)]
pub(super) struct SourceEntryDescriptor {
    pub origin: super::texture_asset::TextureOrigin,
    pub size: u64,
    reader: EntryReader,
}

#[derive(Debug)]
enum EntryReader {
    Pak {
        source: Arc<super::PakSource>,
        index: usize,
    },
    Addon {
        source: Arc<super::PreviewArchiveSource>,
        path: ContentPath,
    },
    File {
        file: File,
        path: std::path::PathBuf,
    },
    Sibling {
        source: Arc<super::SiblingGmaIndex>,
        path: ContentPath,
    },
    Vpk {
        source: Arc<super::VpkArchive>,
        catalog: Arc<super::IndexedVpks>,
        path: ContentPath,
    },
}

fn file_error(path: &ContentPath, error: &std::io::Error) -> SourceError {
    SourceError::Loose {
        path: path.as_str().to_owned(),
        message: error.to_string(),
    }
}

impl SourceRef<'_> {
    pub(super) fn descriptor(
        self,
        path: &ContentPath,
    ) -> Result<Option<SourceEntryDescriptor>, SourceError> {
        if let Some(error) = self.index_error(Some(path)) {
            return Err(SourceError::IndexLimit(error));
        }
        let origin = super::texture_asset::TextureOrigin {
            path: path.clone(),
            tier: self.tier(),
        };
        let (size, reader) = match self {
            Self::SourceUnavailable(_, error) => return Err(SourceError::Discovery(error.clone())),
            Self::IndexUnavailable(_, error) => return Err(SourceError::IndexLimit(error)),
            Self::Pakfile(source) => {
                let Some(&(index, size)) = source.entries.get(path.as_str()) else {
                    return Ok(None);
                };
                (
                    size,
                    EntryReader::Pak {
                        source: Arc::clone(source),
                        index,
                    },
                )
            }
            Self::Addon(source) => {
                let entry = match source.entry(path.as_str()) {
                    Ok(entry) => entry,
                    Err(
                        super::PreviewArchiveSourceError::EntryNotFound(_)
                        | super::PreviewArchiveSourceError::Gma(
                            crate::bridge::gma::GmaError::EntryNotFound,
                        ),
                    ) => return Ok(None),
                    Err(error) => return Err(SourceError::Addon(error)),
                };
                if let Some(file) = source
                    .folder_entry_file(path.as_str())
                    .map_err(SourceError::Addon)?
                {
                    (
                        file.metadata()
                            .map_err(|error| file_error(path, &error))?
                            .len(),
                        EntryReader::File {
                            file,
                            path: source
                                .folder_entry_path(path.as_str())
                                .expect("folder entry path")
                                .to_owned(),
                        },
                    )
                } else {
                    (
                        entry.size,
                        EntryReader::Addon {
                            source: Arc::clone(source),
                            path: path.clone(),
                        },
                    )
                }
            }
            Self::Loose(source) => {
                let file = match File::open(source.root.join(path.as_str())) {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(file_error(path, &error)),
                };
                (
                    file.metadata()
                        .map_err(|error| file_error(path, &error))?
                        .len(),
                    EntryReader::File {
                        file,
                        path: source.root.join(path.as_str()),
                    },
                )
            }
            Self::SiblingGma(source) => {
                let Some(size) = source.entry_size(path) else {
                    return Ok(None);
                };
                (
                    size,
                    EntryReader::Sibling {
                        source: Arc::clone(source),
                        path: path.clone(),
                    },
                )
            }
            Self::GameVpk(catalog, source) => {
                let Some(size) = source.entry_size(path.as_str()) else {
                    return Ok(None);
                };
                (
                    size,
                    EntryReader::Vpk {
                        source: Arc::clone(source),
                        catalog: Arc::clone(catalog),
                        path: path.clone(),
                    },
                )
            }
        };
        Ok(Some(SourceEntryDescriptor {
            origin,
            size,
            reader,
        }))
    }
}

#[derive(Debug)]
struct VpkSelectedReader {
    _catalog: Arc<super::IndexedVpks>,
    reader: Arc<dyn super::reloadable::TextureSourceReader>,
}
impl super::reloadable::TextureSourceReader for VpkSelectedReader {
    fn len(&self) -> u64 {
        self.reader.len()
    }
    fn read_range(
        &self,
        range: std::ops::Range<u64>,
        cancel: &crate::preparation::PreparationCancellation,
    ) -> Result<Vec<u8>, TextureAssetError> {
        self.reader.read_range(range, cancel)
    }
}

#[derive(Debug)]
struct PakEntryReader {
    source: Arc<super::PakSource>,
    index: usize,
    size: u64,
    decoder: usize,
}
impl super::reloadable::TextureSourceReader for PakEntryReader {
    fn len(&self) -> u64 {
        self.size
    }
    fn decoder_working_bytes(&self) -> usize {
        self.decoder
    }
    fn read_range(
        &self,
        range: std::ops::Range<u64>,
        cancel: &crate::preparation::PreparationCancellation,
    ) -> Result<Vec<u8>, TextureAssetError> {
        cancel.check()?;
        let result = self.source.pakfile.entry_range_by_index_with_decoder_limit(
            self.index,
            range,
            self.decoder as u64,
            || cancel.is_cancelled(),
        );
        cancel.check()?;
        result
            .map_err(|error| TextureAssetError::SourceRead(error.to_string().into()))?
            .ok_or(TextureAssetError::SourceChanged)
    }
}
#[derive(Debug)]
struct AddonEntryReader {
    source: Arc<super::PreviewArchive>,
    path: ContentPath,
    size: u64,
}
impl super::reloadable::TextureSourceReader for AddonEntryReader {
    fn len(&self) -> u64 {
        self.size
    }
    fn read_range(
        &self,
        range: std::ops::Range<u64>,
        cancel: &crate::preparation::PreparationCancellation,
    ) -> Result<Vec<u8>, TextureAssetError> {
        cancel.check()?;
        if !self
            .source
            .texture_source_unchanged()
            .map_err(|e| TextureAssetError::SourceRead(e.to_string().into()))?
        {
            return Err(TextureAssetError::SourceChanged);
        }
        let count = usize::try_from(
            range
                .end
                .checked_sub(range.start)
                .ok_or(TextureAssetError::Selection)?,
        )
        .map_err(|_| TextureAssetError::WorkingCostOverflow)?;
        if range.end > self.size {
            return Err(TextureAssetError::Selection);
        }
        let mut bytes = Vec::with_capacity(count);
        let mut start = range.start;
        while start < range.end {
            cancel.check()?;
            let end = start.saturating_add(64 * 1024).min(range.end);
            bytes.extend(
                self.source
                    .entry_range(self.path.as_str(), start..end)
                    .map_err(|e| TextureAssetError::SourceRead(e.to_string().into()))?,
            );
            start = end;
        }
        if !self
            .source
            .texture_source_unchanged()
            .map_err(|e| TextureAssetError::SourceRead(e.to_string().into()))?
        {
            return Err(TextureAssetError::SourceChanged);
        }
        cancel.check()?;
        Ok(bytes)
    }
}
impl SourceEntryDescriptor {
    pub(super) fn into_reader(
        self,
    ) -> Result<
        (
            super::texture_asset::TextureOrigin,
            Arc<dyn super::reloadable::TextureSourceReader>,
        ),
        TextureAssetError,
    > {
        use super::reloadable::{FrozenPathSource, SubrangeSource, TextureSourceReader};
        let reader: Arc<dyn TextureSourceReader> = match self.reader {
            EntryReader::File { file, path } => Arc::new(FrozenPathSource::new(path, &file)?),
            EntryReader::Pak { source, index } => {
                let decoder = source
                    .pakfile
                    .entry_decoder_working_bytes(index)
                    .map_err(|e| TextureAssetError::SourceRead(e.to_string().into()))?
                    .ok_or(TextureAssetError::SourceChanged)?;
                Arc::new(PakEntryReader {
                    source,
                    index,
                    size: self.size,
                    decoder: usize::try_from(decoder)
                        .map_err(|_| TextureAssetError::WorkingCostOverflow)?,
                })
            }
            EntryReader::Addon { source, path } => {
                let super::PreviewArchiveSource::Gma(archive) = &*source else {
                    return Err(TextureAssetError::SourceChanged);
                };
                Arc::new(AddonEntryReader {
                    source: Arc::clone(archive),
                    path,
                    size: self.size,
                })
            }
            EntryReader::Vpk {
                source,
                catalog,
                path,
            } => {
                let gmpublished_backend::vpk::VpkEntryStorage {
                    archive_path: disk,
                    data_range: range,
                    preload: prefix,
                } = source
                    .entry_storage(path.as_str())
                    .map_err(|e| TextureAssetError::SourceRead(e.to_string().into()))?;
                let reader: Arc<dyn TextureSourceReader> = if range.is_empty() {
                    Arc::new(super::reloadable::PreloadSource(prefix))
                } else {
                    let file = File::open(&disk)
                        .map_err(|e| TextureAssetError::SourceRead(e.to_string().into()))?;
                    let reader = Arc::new(FrozenPathSource::new(disk, &file)?);
                    Arc::new(SubrangeSource {
                        reader,
                        range,
                        prefix,
                    })
                };
                Arc::new(VpkSelectedReader {
                    _catalog: catalog,
                    reader,
                })
            }
            EntryReader::Sibling { source, path } => source.reloadable_reader(&path)?,
        };
        if reader.len() != self.size {
            return Err(TextureAssetError::SourceChanged);
        }
        Ok((self.origin, reader))
    }
}
