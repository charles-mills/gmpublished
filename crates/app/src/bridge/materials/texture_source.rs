use super::{Arc, ContentPath, MaterialResolver, Mutex, SourceError, SourceRef, TextureAssetError};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TextureSourceReadCost {
    pub snapshot_bytes: usize,
    pub decoder_bytes: usize,
}

impl TextureSourceReadCost {
    pub fn full_read_extra_bytes(self) -> usize {
        self.snapshot_bytes.max(self.decoder_bytes)
    }

    pub fn prefix_working_bytes(self, maximum: usize) -> Result<usize, TextureAssetError> {
        self.decoder_bytes
            .checked_add(self.snapshot_bytes.min(maximum))
            .ok_or(TextureAssetError::WorkingCostOverflow)
    }
}

static NEXT_SOURCE_TOKEN: AtomicU64 = AtomicU64::new(1);

pub(super) fn source_token() -> u64 {
    NEXT_SOURCE_TOKEN.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug)]
pub struct EncodedTextureBudget {
    limit: u64,
    state: Mutex<EncodedAdmissionState>,
}

#[derive(Debug, Default)]
struct EncodedAdmissionState {
    bytes: u64,
    admitted: HashMap<u64, u64>,
    declined: HashSet<u64>,
}

impl EncodedTextureBudget {
    pub(crate) fn new(bytes: usize) -> Self {
        Self {
            limit: bytes as u64,
            state: Mutex::new(EncodedAdmissionState::default()),
        }
    }
    pub(crate) fn encoded_bytes(&self) -> u64 {
        self.state.lock().bytes
    }
    pub(crate) fn admitted_textures(&self) -> usize {
        self.state.lock().admitted.len()
    }
    pub(crate) fn rejected_textures(&self) -> usize {
        self.state.lock().declined.len()
    }
    pub(super) fn admit(&self, token: u64, bytes: u64) -> bool {
        let mut state = self.state.lock();
        if state.admitted.contains_key(&token) {
            return true;
        }
        if state.declined.contains(&token) {
            return false;
        }
        let Some(total) = state
            .bytes
            .checked_add(bytes)
            .filter(|total| *total <= self.limit)
        else {
            state.declined.insert(token);
            return false;
        };
        state.bytes = total;
        state.admitted.insert(token, bytes);
        true
    }
    pub(super) fn release(&self, token: u64) {
        let mut state = self.state.lock();
        if let Some(bytes) = state.admitted.remove(&token) {
            state.bytes -= bytes;
        }
    }
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

    pub(crate) fn with_encoded_texture_budget(&self, budget: Arc<EncodedTextureBudget>) -> Self {
        self.variant(super::ResolverConfig {
            encoded_texture_budget: Some(budget),
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

impl SourceEntryDescriptor {
    pub(super) fn cost(&self) -> Result<TextureSourceReadCost, SourceError> {
        let decoder_bytes = match &self.reader {
            EntryReader::Pak { source, index } => usize::try_from(
                source
                    .pakfile
                    .entry_decoder_working_bytes(*index)
                    .map_err(|error| SourceError::Pakfile(error.to_string()))?
                    .ok_or_else(|| SourceError::Pakfile("entry vanished".to_owned()))?,
            )
            .map_err(|error| SourceError::Pakfile(error.to_string()))?,
            EntryReader::Sibling { source, path } => source.texture_decoder_working_bytes(path)?,
            _ => 0,
        };
        let snapshot_bytes =
            usize::try_from(self.size).map_err(|error| SourceError::Pakfile(error.to_string()))?;
        Ok(TextureSourceReadCost {
            snapshot_bytes,
            decoder_bytes,
        })
    }

    pub(super) fn prefix(
        &mut self,
        maximum: usize,
        decoder_limit: usize,
    ) -> Result<Vec<u8>, SourceError> {
        self.read(maximum as u64, false, decoder_limit as u64)
    }
    pub(super) fn bytes(&mut self, decoder_limit: usize) -> Result<Vec<u8>, SourceError> {
        self.read(self.size, true, decoder_limit as u64)
    }
    fn read(
        &mut self,
        maximum: u64,
        full: bool,
        decoder_limit: u64,
    ) -> Result<Vec<u8>, SourceError> {
        let count = self.size.min(maximum);
        let bytes = match &mut self.reader {
            EntryReader::Pak { source, index } => {
                let bytes = if full {
                    source
                        .pakfile
                        .entry_bytes_by_index_with_limits(*index, count, decoder_limit)
                } else {
                    source.pakfile.entry_prefix_by_index_with_decoder_limit(
                        *index,
                        usize::try_from(count)
                            .map_err(|error| SourceError::Pakfile(error.to_string()))?,
                        decoder_limit,
                    )
                };
                bytes
                    .map_err(|error| SourceError::Pakfile(error.to_string()))?
                    .ok_or_else(|| SourceError::Pakfile("entry vanished".to_owned()))?
            }
            EntryReader::Addon { source, path } => if full {
                source.entry_bytes(path.as_str())
            } else {
                source.entry_prefix(
                    path.as_str(),
                    usize::try_from(count)
                        .map_err(|error| SourceError::Pakfile(error.to_string()))?,
                )
            }
            .map_err(SourceError::Addon)?,
            EntryReader::File { file, .. } => {
                file.seek(SeekFrom::Start(0))
                    .map_err(|error| file_error(&self.origin.path, &error))?;
                let length = usize::try_from(count).map_err(|error| SourceError::Loose {
                    path: self.origin.path.as_str().to_owned(),
                    message: error.to_string(),
                })?;
                let mut bytes = vec![0; length];
                file.read_exact(&mut bytes)
                    .map_err(|error| file_error(&self.origin.path, &error))?;
                if full {
                    let mut extra = [0];
                    if file
                        .read(&mut extra)
                        .map_err(|error| file_error(&self.origin.path, &error))?
                        != 0
                    {
                        return Err(SourceError::Loose {
                            path: self.origin.path.as_str().to_owned(),
                            message: "entry grew after selection".to_owned(),
                        });
                    }
                }
                bytes
            }
            EntryReader::Sibling { source, path } => source.texture_entry_range(
                path,
                count,
                usize::try_from(decoder_limit).unwrap_or(usize::MAX),
            )?,
            EntryReader::Vpk { source, path, .. } => if full {
                source.texture_entry_bytes(path.as_str(), count)
            } else {
                source.entry_prefix(
                    path.as_str(),
                    usize::try_from(count).map_err(|_| super::VpkError::FormatError)?,
                )
            }
            .map_err(SourceError::GameVpk)?,
        };
        if bytes.len() as u64 != count {
            return Err(SourceError::Loose {
                path: self.origin.path.as_str().to_owned(),
                message: "entry length disagrees with descriptor".to_owned(),
            });
        }
        Ok(bytes)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::materials::{
        TextureLoadOutcome,
        tests::{zip_fixture, zip_lzma_entry, zip_stored_entry},
    };
    use crate::test_support::fixture_vtf_bytes;
    use vformats::vtf::{VtfFormat, texture_flags};

    fn fixture() -> MaterialResolver {
        let mut bytes = fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[0; 4], &[19; 16]]);
        bytes[20..24].copy_from_slice(&texture_flags::EIGHTBITALPHA.to_le_bytes());
        for (index, value) in [0.125_f32, 0.25, 0.75].into_iter().enumerate() {
            bytes[32 + index * 4..36 + index * 4].copy_from_slice(&value.to_le_bytes());
        }
        MaterialResolver::prop_lighting_fixture(vec![
            ("materials/test/image.vtf",bytes),
            ("materials/test/image.vmt",br#"LightmappedGeneric { "$basetexture" "test/image" "$bumpmap" "test/image" }"#.to_vec()),
        ],Vec::new())
    }

    #[test]
    fn compressed_declined_prefix_charges_decoder_and_unreserved_output() {
        use super::super::TextureWorkingBudget;
        use super::super::texture_asset::TextureSourceError;

        let path = ContentPath::new("materials/test/prefix.vtf").unwrap();
        let bytes = fixture_vtf_bytes(128, 128, VtfFormat::Rgba8888, &[&vec![37; 128 * 128 * 4]]);
        let pak = gmpublished_domain::scene::map::MapPakFile::from_pak_bytes(zip_fixture([
            zip_lzma_entry(path.as_str(), bytes.clone()),
        ]));
        let resolver = MaterialResolver::with_pakfile_source(
            Arc::new(
                crate::bridge::gma::PreviewArchive::from_gma(
                    crate::test_support::GmaFixtureBuilder::new("prefix").build(),
                )
                .unwrap(),
            ),
            None,
            pak,
        );
        let cost = SourceRef::Pakfile(resolver.config.pakfile.as_ref().unwrap())
            .descriptor(&path)
            .unwrap()
            .unwrap()
            .cost()
            .unwrap();
        let decoder = cost.decoder_bytes;
        let prefix = bytes.len().min(vformats::vtf::MAX_HEADER_BYTES);
        assert!(decoder > bytes.len());
        assert_eq!(cost.full_read_extra_bytes(), decoder);
        assert_eq!(
            cost.prefix_working_bytes(vformats::vtf::MAX_HEADER_BYTES)
                .unwrap(),
            decoder + prefix
        );

        let zero_encoded = Arc::new(EncodedTextureBudget::new(0));
        let tight = Arc::new(TextureWorkingBudget::new(decoder));
        let limited = resolver
            .with_encoded_texture_budget(Arc::clone(&zero_encoded))
            .with_texture_decoder_limit(decoder)
            .with_texture_source_working_budget(Arc::clone(&tight));
        for _ in 0..2 {
            let error = limited.selected_texture_asset(&path).unwrap_err();
            let TextureSourceError::EncodedBudget {
                metadata,
                inspection,
                ..
            } = error
            else {
                panic!("expected encoded decline");
            };
            assert!(metadata.is_none());
            let expected = TextureAssetError::WorkingLimit {
                required: decoder + prefix,
                capacity: decoder,
            }
            .to_string();
            assert_eq!(inspection.as_deref(), Some(expected.as_str()));
            assert_eq!(tight.peak_bytes(), 0);
            assert_eq!(tight.current_bytes(), 0);
            assert_eq!(zero_encoded.encoded_bytes(), 0);
        }

        let exact = Arc::new(TextureWorkingBudget::new(decoder + prefix));
        let inspectable = resolver
            .with_encoded_texture_budget(Arc::clone(&zero_encoded))
            .with_texture_decoder_limit(decoder)
            .with_texture_source_working_budget(Arc::clone(&exact));
        let TextureSourceError::EncodedBudget {
            metadata,
            inspection,
            ..
        } = inspectable.selected_texture_asset(&path).unwrap_err()
        else {
            panic!("expected encoded decline with header");
        };
        assert_eq!(metadata.unwrap().original_dimensions, [128, 128]);
        assert!(inspection.is_none());
        assert_eq!(exact.peak_bytes(), decoder + prefix);
        assert_eq!(exact.current_bytes(), 0);
        assert_eq!(zero_encoded.encoded_bytes(), 0);

        let ceiling = Arc::new(TextureWorkingBudget::new(decoder + prefix));
        let below_decoder = resolver
            .with_encoded_texture_budget(Arc::clone(&zero_encoded))
            .with_texture_decoder_limit(decoder - 1)
            .with_texture_source_working_budget(Arc::clone(&ceiling));
        assert!(matches!(
            below_decoder.selected_texture_asset(&path),
            Err(TextureSourceError::WorkingBudget { metadata: None, .. })
        ));
        assert_eq!(ceiling.peak_bytes(), 0);

        let full_budget = Arc::new(EncodedTextureBudget::new(bytes.len()));
        let full_working = Arc::new(TextureWorkingBudget::new(decoder));
        let full = resolver
            .with_encoded_texture_budget(Arc::clone(&full_budget))
            .with_texture_decoder_limit(decoder)
            .with_texture_source_working_budget(Arc::clone(&full_working));
        let selected = full.selected_texture_asset(&path).unwrap();
        assert_eq!(selected.asset.source_bytes().as_slice(), bytes);
        assert_eq!(full_working.peak_bytes(), decoder);
        assert_eq!(full_budget.encoded_bytes(), bytes.len() as u64);
        let TextureSourceError::EncodedBudget {
            metadata,
            inspection,
            ..
        } = limited.selected_texture_asset(&path).unwrap_err()
        else {
            panic!("expected warm encoded decline");
        };
        assert_eq!(metadata.unwrap().original_dimensions, [128, 128]);
        assert!(inspection.is_none());
        assert_eq!(tight.peak_bytes(), 0);
    }

    #[test]
    fn prefix_cost_overflow_is_a_working_policy_error() {
        let cost = TextureSourceReadCost {
            snapshot_bytes: 1,
            decoder_bytes: usize::MAX,
        };
        assert_eq!(cost.full_read_extra_bytes(), usize::MAX);
        assert_eq!(
            cost.prefix_working_bytes(1),
            Err(TextureAssetError::WorkingCostOverflow)
        );
        assert_eq!(cost.prefix_working_bytes(0).unwrap(), usize::MAX);
    }

    #[test]
    fn source_working_rejection_preserves_header_and_warm_cold_fit() {
        let bytes = fixture_vtf_bytes(128, 128, VtfFormat::Rgba8888, &[&vec![37; 128 * 128 * 4]]);
        let length = bytes.len();
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![
                ("materials/test/large.vtf", bytes),
                (
                    "materials/test/large.vmt",
                    br#"LightmappedGeneric { "$basetexture" "test/large" }"#.to_vec(),
                ),
            ],
            Vec::new(),
        );
        let encoded = Arc::new(EncodedTextureBudget::new(length));
        let small = Arc::new(super::super::TextureWorkingBudget::new(
            vformats::vtf::MAX_HEADER_BYTES,
        ));
        let limited = resolver
            .with_encoded_texture_budget(Arc::clone(&encoded))
            .with_texture_source_working_budget(Arc::clone(&small));
        let rejected = limited
            .resolve_primary(&[], "test/large")
            .into_material()
            .unwrap();
        assert_eq!(
            rejected.texture.as_ref().unwrap().load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(
            rejected.texture.as_ref().unwrap().mapping_dimensions(),
            Some((128, 128))
        );
        assert_eq!(encoded.encoded_bytes(), 0);
        assert_eq!(small.current_bytes(), 0);
        assert_eq!(small.peak_bytes(), vformats::vtf::MAX_HEADER_BYTES);
        let large = Arc::new(super::super::TextureWorkingBudget::new(length));
        let admitted = resolver.with_texture_source_working_budget(Arc::clone(&large));
        assert_eq!(
            admitted
                .resolve_primary(&[], "test/large")
                .into_material()
                .unwrap()
                .texture
                .as_ref()
                .unwrap()
                .load_outcome(),
            TextureLoadOutcome::Loaded
        );
        assert_eq!(large.peak_bytes(), length);
        assert_eq!(large.current_bytes(), 0);
        let warm = limited
            .resolve_primary(&[], "test/large")
            .into_material()
            .unwrap();
        assert_eq!(
            warm.texture.as_ref().unwrap().load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(
            warm.texture.as_ref().unwrap().mapping_dimensions(),
            Some((128, 128))
        );
        assert_eq!(encoded.encoded_bytes(), 0);
    }

    #[test]
    fn source_working_lease_releases_on_invalid_content() {
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![
                ("materials/test/bad.vtf", vec![0; 100]),
                (
                    "materials/test/bad.vmt",
                    br#"LightmappedGeneric { "$basetexture" "test/bad" }"#.to_vec(),
                ),
            ],
            Vec::new(),
        );
        let working = Arc::new(super::super::TextureWorkingBudget::new(100));
        let resolver = resolver.with_texture_source_working_budget(Arc::clone(&working));
        for _ in 0..2 {
            assert_eq!(
                resolver
                    .resolve_primary(&[], "test/bad")
                    .into_material()
                    .unwrap()
                    .texture
                    .as_ref()
                    .unwrap()
                    .load_outcome(),
                TextureLoadOutcome::InvalidSource
            );
            assert_eq!(working.current_bytes(), 0);
        }
        assert_eq!(working.peak_bytes(), 100);
    }

    #[test]
    fn cached_read_failure_rechecks_working_and_encoded_fit_without_retaining_charges() {
        let path = ContentPath::new("materials/test/image.vtf").unwrap();
        let bytes = fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[41, 42, 43, 255]]);
        let declared = u32::try_from(bytes.len() + 1).unwrap();
        let mut zip = zip_fixture([zip_stored_entry(path.as_str(), bytes)]);
        let central = zip
            .windows(4)
            .position(|bytes| bytes == [0x50, 0x4b, 0x01, 0x02])
            .unwrap();
        zip[central + 24..central + 28].copy_from_slice(&declared.to_le_bytes());
        let make_resolver = || {
            let lower = MaterialResolver::prop_lighting_fixture(
                Vec::new(),
                vec![(
                    path.as_str(),
                    fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[19; 16]]),
                )],
            );
            let pakfile = gmpublished_domain::scene::map::MapPakFile::from_pak_bytes(zip.clone());
            lower.variant(super::super::ResolverConfig {
                pakfile: Some(Arc::new(super::super::PakSource::new(pakfile).unwrap())),
                ..lower.config.clone()
            })
        };
        let resolver = make_resolver();
        let encoded = Arc::new(EncodedTextureBudget::new(declared as usize));
        let working = Arc::new(super::super::TextureWorkingBudget::new(declared as usize));
        let admitted = resolver
            .with_encoded_texture_budget(Arc::clone(&encoded))
            .with_texture_source_working_budget(Arc::clone(&working));
        let original = admitted.selected_texture_asset(&path).unwrap_err();
        assert!(matches!(
            original,
            super::super::texture_asset::TextureSourceError::Read { .. }
        ));
        for _ in 0..2 {
            assert_eq!(
                admitted.selected_texture_asset(&path).unwrap_err(),
                original
            );
            assert_eq!(encoded.encoded_bytes(), 0);
            assert_eq!(encoded.admitted_textures(), 0);
            assert_eq!(working.current_bytes(), 0);
        }
        assert_eq!(working.peak_bytes(), declared as usize);
        for source in [&resolver, &make_resolver()] {
            let limited = source.with_texture_source_working_budget(Arc::new(
                super::super::TextureWorkingBudget::new(declared as usize - 1),
            ));
            assert!(matches!(
                limited.selected_texture_asset(&path),
                Err(super::super::texture_asset::TextureSourceError::WorkingBudget { .. })
            ));
            let small = Arc::new(EncodedTextureBudget::new(declared as usize - 1));
            let limited = source.with_encoded_texture_budget(Arc::clone(&small));
            assert!(matches!(
                limited.selected_texture_asset(&path),
                Err(super::super::texture_asset::TextureSourceError::EncodedBudget { .. })
            ));
            assert_eq!(small.encoded_bytes(), 0);
            assert_eq!(small.rejected_textures(), 1);
        }
        assert_eq!(
            resolver.selected_texture_asset(&path).unwrap_err(),
            original
        );
    }

    #[test]
    fn decoder_rejection_keeps_first_source_and_allows_a_larger_policy_later() {
        let lower = MaterialResolver::prop_lighting_fixture(
            Vec::new(),
            vec![
                (
                    "materials/test/image.vmt",
                    br#"LightmappedGeneric { "$basetexture" "test/image" }"#.to_vec(),
                ),
                (
                    "materials/test/image.vtf",
                    fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[19; 16]]),
                ),
            ],
        );
        let bytes = fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[41, 42, 43, 255]]);
        let pakfile = gmpublished_domain::scene::map::MapPakFile::from_pak_bytes(zip_fixture([
            super::super::tests::zip_lzma_entry("materials/test/image.vtf", bytes),
        ]));
        let resolver = lower.variant(super::super::ResolverConfig {
            pakfile: Some(Arc::new(super::super::PakSource::new(pakfile).unwrap())),
            ..lower.config.clone()
        });
        let rejected = resolver
            .with_texture_decoder_limit(1)
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(
            rejected.texture.as_ref().unwrap().load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(
            rejected.texture.as_ref().unwrap().mapping_dimensions(),
            None
        );
        assert_eq!(resolver.resident_texture_asset_count(), 0);
        let accepted = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(
            accepted.texture.as_ref().unwrap().mapping_dimensions(),
            Some((1, 1))
        );
        assert_eq!(
            accepted
                .texture
                .as_ref()
                .unwrap()
                .resident()
                .unwrap()
                .layers()[0]
                .mips[0]
                .bytes
                .as_slice(),
            &[41, 42, 43, 255]
        );
        let warm = resolver
            .with_texture_decoder_limit(1)
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(
            warm.texture.as_ref().unwrap().load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(
            warm.texture.as_ref().unwrap().mapping_dimensions(),
            Some((1, 1))
        );
    }

    #[test]
    fn rejected_source_preserves_header_and_roles_without_poisoning_shared_payload() {
        let resolver = fixture();
        let small = Arc::new(EncodedTextureBudget::new(1));
        let limited = resolver.with_encoded_texture_budget(Arc::clone(&small));
        let material = limited
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(
            material.texture.as_ref().unwrap().load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(
            material.texture.as_ref().unwrap().mapping_dimensions(),
            Some((2, 2))
        );
        assert_eq!(
            material.texture.as_ref().unwrap().flags(),
            Some(texture_flags::EIGHTBITALPHA)
        );
        assert!(
            material
                .texture
                .as_ref()
                .unwrap()
                .has_effective_base_texture()
        );
        let normal = material.shader.surface().unwrap().normal.as_ref().unwrap();
        assert_eq!(
            normal.load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(
            normal.source_metadata().unwrap().original_dimensions,
            [2, 2]
        );
        assert_eq!(small.encoded_bytes(), 0);
        assert_eq!(small.rejected_textures(), 1);
        assert_eq!(resolver.resident_texture_asset_count(), 0);
        let large = Arc::new(EncodedTextureBudget::new(84));
        let accepted = resolver
            .with_encoded_texture_budget(Arc::clone(&large))
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(
            accepted.texture.as_ref().unwrap().load_outcome(),
            TextureLoadOutcome::Loaded
        );
        assert_eq!(large.encoded_bytes(), 84);
        assert_eq!(
            accepted.texture.as_ref().unwrap().rgba_bytes().unwrap(),
            &[19; 16]
        );
        let again = limited
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(
            again.texture.as_ref().unwrap().load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(small.encoded_bytes(), 0);
    }

    #[test]
    fn warm_cache_charges_new_budget_once() {
        let resolver = fixture();
        let original = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        let budget = Arc::new(EncodedTextureBudget::new(84));
        let limited = resolver.with_encoded_texture_budget(Arc::clone(&budget));
        for _ in 0..2 {
            let material = limited
                .resolve_primary(&[], "test/image")
                .into_material()
                .unwrap();
            assert!(Arc::ptr_eq(
                material.texture.as_ref().unwrap().resident().unwrap(),
                original.texture.as_ref().unwrap().resident().unwrap()
            ));
        }
        assert_eq!(budget.encoded_bytes(), 84);
        assert_eq!(budget.rejected_textures(), 0);
    }

    #[test]
    fn budget_rejection_does_not_advance_to_lower_priority_content() {
        let archive = crate::bridge::gma::PreviewArchive::from_gma(
            crate::test_support::GmaFixtureBuilder::new("precedence")
                .entry(
                    "materials/test/image.vmt",
                    br#"LightmappedGeneric { "$basetexture" "test/image" }"#.to_vec(),
                )
                .entry(
                    "materials/test/image.vtf",
                    fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[0; 4], &[19; 16]]),
                )
                .build(),
        )
        .unwrap();
        let pak = gmpublished_domain::scene::map::MapPakFile::from_pak_bytes(zip_fixture([
            zip_stored_entry(
                "materials/test/image.vtf",
                fixture_vtf_bytes(4, 1, VtfFormat::Rgba8888, &[&[77; 16]]),
            ),
        ]));
        let resolver = MaterialResolver::with_pakfile_source(Arc::new(archive), None, pak);
        let limited = resolver.with_encoded_texture_budget(Arc::new(EncodedTextureBudget::new(1)));
        let rejected = limited
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(
            rejected.texture.as_ref().unwrap().load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(
            rejected.texture.as_ref().unwrap().mapping_dimensions(),
            Some((4, 1))
        );
        let loaded = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(
            loaded.texture.as_ref().unwrap().rgba_bytes().unwrap(),
            &[77; 16]
        );
    }

    #[test]
    fn same_open_handle_bounds_short_and_growing_loose_reads() {
        use std::io::Write as _;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sample.vtf");
        std::fs::write(&path, [1, 2, 3, 4]).unwrap();
        let source = super::super::LooseSourceDir::new(temp.path().to_owned());
        let name = ContentPath::new("sample.vtf").unwrap();
        let mut descriptor = SourceRef::Loose(&source)
            .descriptor(&name)
            .unwrap()
            .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[5])
            .unwrap();
        assert!(descriptor.bytes(0).is_err());
        let mut descriptor = SourceRef::Loose(&source)
            .descriptor(&name)
            .unwrap()
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(1)
            .unwrap();
        assert!(descriptor.bytes(0).is_err());
    }
    #[test]
    fn rejected_encoded_payload_retains_qualified_radiosity_without_auxiliary_pixels() {
        let budget = Arc::new(EncodedTextureBudget::new(1));
        let resolver = fixture().with_encoded_texture_budget(budget);
        let lighting = resolver.resolve_surface_lighting("test/image").unwrap();
        assert_eq!(
            lighting.reflectivity,
            super::super::Vec3::new(0.125, 0.25, 0.75)
        );
        assert_eq!(
            lighting.source_outcome,
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(resolver.resident_texture_asset_count(), 0);
    }
    #[test]
    fn oversized_pak_descriptors_remain_first_present_even_when_header_is_unavailable() {
        let mut zip = zip_fixture([zip_stored_entry(
            "materials/test/image.vtf",
            fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[77; 4]]),
        )]);
        let central = zip
            .windows(4)
            .position(|bytes| bytes == [0x50, 0x4b, 0x01, 0x02])
            .unwrap();
        let declared = 256 * 1024 * 1024 + 1_u32;
        zip[central + 24..central + 28].copy_from_slice(&declared.to_le_bytes());
        let pak = gmpublished_domain::scene::map::MapPakFile::from_pak_bytes(zip);
        assert_eq!(pak.indexed_entries().unwrap()[0].size, u64::from(declared));
        let archive = crate::bridge::gma::PreviewArchive::from_gma(
            crate::test_support::GmaFixtureBuilder::new("oversize")
                .entry(
                    "materials/test/image.vmt",
                    br#"LightmappedGeneric { "$basetexture" "test/image" }"#.to_vec(),
                )
                .entry(
                    "materials/test/image.vtf",
                    fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[19; 4]]),
                )
                .build(),
        )
        .unwrap();
        let resolver = MaterialResolver::with_pakfile_source(Arc::new(archive), None, pak)
            .with_encoded_texture_budget(Arc::new(EncodedTextureBudget::new(1)));
        let material = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(
            material.texture.as_ref().unwrap().load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert!(
            material
                .texture
                .as_ref()
                .unwrap()
                .source_metadata()
                .is_none()
        );
        assert!(
            material
                .texture
                .as_ref()
                .unwrap()
                .has_effective_base_texture()
        );
        assert_eq!(
            material
                .shader
                .mapping_dimensions(material.texture.as_deref()),
            None
        );
    }
    #[test]
    fn frozen_source_authorization_precedes_encoded_admission_even_on_warm_cache() {
        let resolver = fixture();
        let _warm = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        let budget = Arc::new(EncodedTextureBudget::new(84));
        let resolver = resolver.with_encoded_texture_budget(Arc::clone(&budget));
        let (_, empty) = resolver.plan_texture_requests(|_| ());
        let admitted = resolver.with_texture_admissions(resolver.admit_texture_requests(empty));
        let rejected = admitted
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(
            rejected.texture.as_ref().unwrap().load_outcome(),
            TextureLoadOutcome::Unplanned
        );
        assert_eq!(budget.encoded_bytes(), 0);
        assert_eq!(budget.admitted_textures(), 0);
        assert_eq!(budget.rejected_textures(), 0);
    }
}
