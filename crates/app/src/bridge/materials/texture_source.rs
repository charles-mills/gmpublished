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

pub(super) struct SourceEntryDescriptor<'a> {
    pub origin: super::texture_asset::TextureOrigin,
    pub size: u64,
    reader: EntryReader<'a>,
}

enum EntryReader<'a> {
    Pak {
        source: &'a super::PakSource,
        index: usize,
    },
    Addon {
        source: &'a super::PreviewArchiveSource,
        path: ContentPath,
    },
    File(File),
    Sibling {
        source: &'a super::SiblingGmaIndex,
        path: ContentPath,
    },
    Vpk {
        source: &'a super::VpkArchive,
        path: ContentPath,
    },
}

fn file_error(path: &ContentPath, error: &std::io::Error) -> SourceError {
    SourceError::Loose {
        path: path.as_str().to_owned(),
        message: error.to_string(),
    }
}

impl<'a> SourceRef<'a> {
    pub(super) fn descriptor(
        self,
        path: &ContentPath,
    ) -> Result<Option<SourceEntryDescriptor<'a>>, SourceError> {
        let origin = super::texture_asset::TextureOrigin {
            path: path.clone(),
            tier: self.tier(),
        };
        let (size, reader) = match self {
            Self::Pakfile(source) => {
                let Some(&(index, size)) = source.entries.get(path.as_str()) else {
                    return Ok(None);
                };
                (size, EntryReader::Pak { source, index })
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
                        EntryReader::File(file),
                    )
                } else {
                    (
                        entry.size,
                        EntryReader::Addon {
                            source,
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
                    EntryReader::File(file),
                )
            }
            Self::SiblingGma(source) => {
                let Some(size) = source.entry_size(path) else {
                    return Ok(None);
                };
                (
                    size,
                    EntryReader::Sibling {
                        source,
                        path: path.clone(),
                    },
                )
            }
            Self::GameVpk(source) => {
                let Some(size) = source.entry_size(path.as_str()) else {
                    return Ok(None);
                };
                (
                    size,
                    EntryReader::Vpk {
                        source,
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

impl SourceEntryDescriptor<'_> {
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
            EntryReader::File(file) => {
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
            EntryReader::Vpk { source, path } => if full {
                source.entry_bytes(path.as_str())
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
        let cost = SourceRef::Pakfile(resolver.config.pakfile.as_deref().unwrap())
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
            rejected.texture.load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(rejected.texture.mapping_dimensions(), Some((128, 128)));
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
            warm.texture.load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(warm.texture.mapping_dimensions(), Some((128, 128)));
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
            rejected.texture.load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(rejected.texture.mapping_dimensions(), None);
        assert_eq!(resolver.resident_texture_asset_count(), 0);
        let accepted = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(accepted.texture.mapping_dimensions(), Some((1, 1)));
        assert_eq!(
            accepted.texture.resident().unwrap().layers()[0].mips[0]
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
            warm.texture.load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(warm.texture.mapping_dimensions(), Some((1, 1)));
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
            material.texture.load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(material.texture.mapping_dimensions(), Some((2, 2)));
        assert_eq!(material.texture.flags(), Some(texture_flags::EIGHTBITALPHA));
        assert!(material.texture.has_effective_base_texture());
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
        assert_eq!(accepted.texture.load_outcome(), TextureLoadOutcome::Loaded);
        assert_eq!(large.encoded_bytes(), 84);
        assert_eq!(accepted.texture.rgba_bytes().unwrap(), &[19; 16]);
        let again = limited
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(
            again.texture.load_outcome(),
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
                material.texture.resident().unwrap(),
                original.texture.resident().unwrap()
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
            rejected.texture.load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert_eq!(rejected.texture.mapping_dimensions(), Some((4, 1)));
        let loaded = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(loaded.texture.rgba_bytes().unwrap(), &[77; 16]);
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
            material.texture.load_outcome(),
            TextureLoadOutcome::SourceBudgetRejected
        );
        assert!(material.texture.source_metadata().is_none());
        assert!(material.texture.has_effective_base_texture());
        assert_eq!(
            material.shader.mapping_dimensions(Some(&material.texture)),
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
            rejected.texture.load_outcome(),
            TextureLoadOutcome::Unplanned
        );
        assert_eq!(budget.encoded_bytes(), 0);
        assert_eq!(budget.admitted_textures(), 0);
        assert_eq!(budget.rejected_textures(), 0);
    }
}
