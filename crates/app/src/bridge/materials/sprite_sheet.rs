use super::{
    Arc, ContentPath, HashMap, MaterialResolutionOutcome, MaterialResolver, Mutex, OnceLock,
    definition::MaterialSourceFailure,
    material_paths,
    texture_asset::{TextureAsset, TextureSourceError},
};
use vformats::{
    Limits,
    vtf::{SpriteSheet, SpriteSheetCost, SpriteSheetError, SpriteSheetLimits, SpriteSheetMetric},
};

pub const DEFAULT_SPRITE_SHEET_FRAMES: usize = 1_000_000;
pub const DEFAULT_SPRITE_SHEET_METADATA_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpriteSheetSourceFailure {
    Missing,
    Invalid,
    Read,
    EncodedBudget,
    WorkingBudget,
    SourceUnavailable,
    Preparation,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SpriteSheetResolveError {
    #[error(transparent)]
    Cancelled(#[from] crate::preparation::Cancelled),
    #[error("particle material {name} is unavailable: {outcome:?}")]
    Material {
        name: String,
        outcome: MaterialResolutionOutcome,
    },
    #[error("particle texture {path} is unavailable: {kind:?}: {message}")]
    Source {
        path: ContentPath,
        kind: SpriteSheetSourceFailure,
        message: Arc<str>,
    },
    #[error("{0}")]
    Invalid(#[from] SpriteSheetError),
    #[error("sprite-sheet {metric:?} requires {required}, capacity {capacity}")]
    Budget {
        metric: SpriteSheetMetric,
        required: usize,
        capacity: usize,
    },
    #[error("sprite-sheet working allocation rejected: {0}")]
    Working(#[from] super::TextureAssetError),
}

#[derive(Debug, Default)]
struct SheetAdmissions {
    frames: usize,
    metadata_bytes: usize,
    admitted: HashMap<SpriteSheetAllocationId, SpriteSheetCost>,
    rejected: HashMap<SpriteSheetAllocationId, SpriteSheetResolveError>,
}

#[derive(Debug)]
pub struct SpriteSheetBudget {
    frames: usize,
    metadata_bytes: usize,
    state: Mutex<SheetAdmissions>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct SpriteSheetStats {
    pub frame_limit: usize,
    pub metadata_byte_limit: usize,
    pub admitted_frames: usize,
    pub admitted_metadata_bytes: usize,
    pub rejected_sheets: usize,
}

impl SpriteSheetBudget {
    pub(crate) fn new(frames: usize, metadata_bytes: usize) -> Self {
        Self {
            frames,
            metadata_bytes,
            state: Mutex::new(SheetAdmissions::default()),
        }
    }

    pub(crate) fn stats(&self) -> SpriteSheetStats {
        let state = self.state.lock();
        SpriteSheetStats {
            frame_limit: self.frames,
            metadata_byte_limit: self.metadata_bytes,
            admitted_frames: state.frames,
            admitted_metadata_bytes: state.metadata_bytes,
            rejected_sheets: state.rejected.len(),
        }
    }

    fn admit(
        &self,
        id: SpriteSheetAllocationId,
        cost: SpriteSheetCost,
    ) -> Result<(), SpriteSheetResolveError> {
        let mut state = self.state.lock();
        if state.admitted.contains_key(&id) {
            return Ok(());
        }
        if let Some(error) = state.rejected.get(&id) {
            return Err(error.clone());
        }
        let frames = state.frames.checked_add(cost.frames);
        let metadata_bytes = state.metadata_bytes.checked_add(cost.metadata_bytes);
        let error = if frames.is_none_or(|total| total > self.frames) {
            Some(SpriteSheetResolveError::Budget {
                metric: SpriteSheetMetric::Frames,
                required: frames.unwrap_or(usize::MAX),
                capacity: self.frames,
            })
        } else if metadata_bytes.is_none_or(|total| total > self.metadata_bytes) {
            Some(SpriteSheetResolveError::Budget {
                metric: SpriteSheetMetric::MetadataBytes,
                required: metadata_bytes.unwrap_or(usize::MAX),
                capacity: self.metadata_bytes,
            })
        } else {
            None
        };
        if let Some(error) = error {
            state.rejected.insert(id, error.clone());
            return Err(error);
        }
        state.frames = frames.expect("checked above");
        state.metadata_bytes = metadata_bytes.expect("checked above");
        state.admitted.insert(id, cost);
        drop(state);
        Ok(())
    }

    fn release(&self, id: SpriteSheetAllocationId) {
        let mut state = self.state.lock();
        if let Some(cost) = state.admitted.remove(&id) {
            state.frames -= cost.frames;
            state.metadata_bytes -= cost.metadata_bytes;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SpriteSheetAllocationId(u64);

struct SheetReservation {
    budget: Arc<SpriteSheetBudget>,
    id: SpriteSheetAllocationId,
}
impl Drop for SheetReservation {
    fn drop(&mut self) {
        self.budget.release(self.id);
    }
}
struct ReloadableSheet {
    sheet: Arc<SpriteSheet>,
    _reservation: SheetReservation,
}

pub(super) struct SpriteSheetCache {
    allocation_id: SpriteSheetAllocationId,
    reloadable_parsed: crate::preparation::FlightCell<Option<Arc<ReloadableSheet>>>,
    cost: OnceLock<Result<Option<SpriteSheetCost>, SpriteSheetError>>,
    parsed: crate::preparation::FlightCell<Result<Option<Arc<SpriteSheet>>, SpriteSheetError>>,
    #[cfg(test)]
    parses: std::sync::atomic::AtomicUsize,
}

impl Default for SpriteSheetCache {
    fn default() -> Self {
        Self {
            allocation_id: SpriteSheetAllocationId(super::texture_source::source_token()),
            reloadable_parsed: Default::default(),
            cost: OnceLock::new(),
            parsed: Default::default(),
            #[cfg(test)]
            parses: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl std::fmt::Debug for SpriteSheetCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpriteSheetCache")
            .field("cost", &self.cost.get())
            .field("parsed", &self.parsed.get().is_some())
            .finish()
    }
}

impl SpriteSheetCache {
    fn parse_reloadable(
        &self,
        source: &super::ReloadableTextureSource,
        resolver: &MaterialResolver,
    ) -> Result<Option<Arc<SpriteSheet>>, SpriteSheetResolveError> {
        let parsed = self.reloadable_parsed.get_or_try_init_ref_with_error(
            &resolver.config.cancellation,
            || {
                let working = resolver
                    .config
                    .texture_working_budget
                    .clone()
                    .or_else(|| resolver.config.texture_source_working_budget.clone())
                    .unwrap_or_else(|| {
                        Arc::new(super::TextureWorkingBudget::new(
                            usize::try_from(Limits::default().max_input_bytes)
                                .unwrap_or(usize::MAX),
                        ))
                    });
                let Some(payload) = source.sprite_sheet_payload(
                    &resolver.config.cancellation,
                    &working,
                    resolver.config.sprite_sheet_budget.metadata_bytes,
                    0,
                )?
                else {
                    let _ = self.cost.set(Ok(None));
                    return Ok::<_, SpriteSheetResolveError>(None);
                };
                let cost =
                    vformats::vtf::SpriteSheetDescriptor::from_payload(&payload.bytes)?.cost();
                drop(payload);
                let _ = self.cost.set(Ok(Some(cost)));
                resolver
                    .config
                    .sprite_sheet_budget
                    .admit(self.allocation_id, cost)?;
                let reservation = SheetReservation {
                    budget: Arc::clone(&resolver.config.sprite_sheet_budget),
                    id: self.allocation_id,
                };
                let payload = source
                    .sprite_sheet_payload(
                        &resolver.config.cancellation,
                        &working,
                        resolver.config.sprite_sheet_budget.metadata_bytes,
                        cost.metadata_bytes,
                    )?
                    .ok_or(SpriteSheetError::InvalidResource)?;
                let descriptor =
                    vformats::vtf::SpriteSheetDescriptor::from_payload(&payload.bytes)?;
                let sheet = Arc::new(descriptor.decode(SpriteSheetLimits {
                    max_sequences: 1024,
                    max_frames_per_sequence: 4096,
                    max_frames: cost.frames,
                    max_metadata_bytes: cost.metadata_bytes,
                })?);
                resolver.config.cancellation.check()?;
                Ok(Some(Arc::new(ReloadableSheet {
                    sheet,
                    _reservation: reservation,
                })))
            },
        )?;
        if let Some(Ok(Some(cost))) = self.cost.get() {
            resolver
                .config
                .sprite_sheet_budget
                .admit(self.allocation_id, *cost)?;
        }
        Ok(parsed.as_ref().map(|parsed| Arc::clone(&parsed.sheet)))
    }

    fn cost(&self, asset: &TextureAsset) -> Result<Option<SpriteSheetCost>, SpriteSheetError> {
        *self.cost.get_or_init(|| {
            let bytes = asset.source_bytes();
            let vtf = vformats::vtf::parse(bytes.as_slice(), &Limits::default())
                .map_err(|_| SpriteSheetError::InvalidResource)?;
            vtf.sprite_sheet_descriptor()
                .map(|descriptor| descriptor.map(|descriptor| descriptor.cost()))
        })
    }

    fn parse(
        &self,
        asset: &TextureAsset,
        working: Option<super::texture_working::TextureWorkingRequest>,
        cancellation: &crate::preparation::PreparationCancellation,
    ) -> Result<Option<Arc<SpriteSheet>>, SpriteSheetResolveError> {
        self.parsed
            .get_or_try_init(cancellation, || {
                let _working = working
                    .map(|request| request.acquire_cancellable(cancellation))
                    .transpose()?;
                Ok((|| {
                    #[cfg(test)]
                    self.parses
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let bytes = asset.source_bytes();
                    let vtf = vformats::vtf::parse(bytes.as_slice(), &Limits::default())
                        .map_err(|_| SpriteSheetError::InvalidResource)?;
                    vtf.sprite_sheet_descriptor()?
                        .map(|descriptor| {
                            let cost = descriptor.cost();
                            descriptor
                                .decode(SpriteSheetLimits {
                                    max_sequences: 1024,
                                    max_frames_per_sequence: 4096,
                                    max_frames: cost.frames,
                                    max_metadata_bytes: cost.metadata_bytes,
                                })
                                .map(Arc::new)
                        })
                        .transpose()
                })())
            })?
            .map_err(SpriteSheetResolveError::Invalid)
    }
}

impl MaterialResolver {
    pub(crate) fn with_sprite_sheet_budget(&self, budget: Arc<SpriteSheetBudget>) -> Self {
        self.variant(super::ResolverConfig {
            sprite_sheet_budget: budget,
            ..self.config.clone()
        })
    }

    pub(crate) fn resolve_particle_sprite_sheet(
        &self,
        name: &str,
    ) -> Result<Option<Arc<SpriteSheet>>, SpriteSheetResolveError> {
        let paths = material_paths(&[], name);
        let definition =
            self.material_definition(&paths)
                .ok_or_else(|| SpriteSheetResolveError::Material {
                    name: name.to_owned(),
                    outcome: match self.material_source_failure(&paths) {
                        Some(MaterialSourceFailure::Missing) => MaterialResolutionOutcome::Missing,
                        Some(MaterialSourceFailure::SourceUnavailable(_)) => {
                            MaterialResolutionOutcome::SourceUnavailable
                        }
                        Some(MaterialSourceFailure::IndexUnavailable(_)) => {
                            MaterialResolutionOutcome::IndexUnavailable
                        }
                        Some(MaterialSourceFailure::Malformed(_)) => {
                            MaterialResolutionOutcome::Malformed
                        }
                        None => MaterialResolutionOutcome::Unresolved,
                    },
                })?;
        let Some(base) = definition.material.effective_base_texture() else {
            return Ok(None);
        };
        let path = super::texture_path(base).ok_or_else(|| SpriteSheetResolveError::Material {
            name: name.to_owned(),
            outcome: MaterialResolutionOutcome::Unresolved,
        })?;
        if self.config.texture_loading == super::TextureLoading::OnDemand {
            let source = self.selected_reloadable_texture(&path).map_err(|error| {
                SpriteSheetResolveError::Source {
                    path: path.clone(),
                    kind: match &error {
                        TextureSourceError::Missing => SpriteSheetSourceFailure::Missing,
                        TextureSourceError::Invalid { .. } => SpriteSheetSourceFailure::Invalid,
                        TextureSourceError::EncodedBudget { .. } => {
                            SpriteSheetSourceFailure::EncodedBudget
                        }
                        TextureSourceError::WorkingBudget { .. } => {
                            SpriteSheetSourceFailure::WorkingBudget
                        }
                        TextureSourceError::Preparation { .. } => {
                            SpriteSheetSourceFailure::Preparation
                        }
                        TextureSourceError::Read { .. } => SpriteSheetSourceFailure::Read,
                        TextureSourceError::SourceUnavailable(_) => {
                            SpriteSheetSourceFailure::SourceUnavailable
                        }
                    },
                    message: error.to_string().into(),
                }
            })?;
            return source.sprite_sheet_cache.parse_reloadable(&source, self);
        }
        let selected = self.selected_texture_asset(&path).map_err(|error| {
            SpriteSheetResolveError::Source {
                path,
                kind: match &error {
                    TextureSourceError::Missing => SpriteSheetSourceFailure::Missing,
                    TextureSourceError::Invalid { .. } => SpriteSheetSourceFailure::Invalid,
                    TextureSourceError::EncodedBudget { .. } => {
                        SpriteSheetSourceFailure::EncodedBudget
                    }
                    TextureSourceError::WorkingBudget { .. } => {
                        SpriteSheetSourceFailure::WorkingBudget
                    }
                    TextureSourceError::Preparation { .. } => SpriteSheetSourceFailure::Preparation,
                    TextureSourceError::Read { .. } => SpriteSheetSourceFailure::Read,
                    TextureSourceError::SourceUnavailable(_) => {
                        SpriteSheetSourceFailure::SourceUnavailable
                    }
                },
                message: error.to_string().into(),
            }
        })?;
        let asset = &selected.asset;
        let cache = asset.sprite_sheet_cache(&self.shared.texture_assets);
        let Some(cost) = cache.cost(asset)? else {
            return Ok(None);
        };
        let working = self
            .config
            .texture_working_budget
            .as_ref()
            .map(|budget| budget.request(cost.metadata_bytes))
            .transpose()?;
        let id = cache.allocation_id;
        self.config.sprite_sheet_budget.admit(id, cost)?;
        let result = cache.parse(asset, working, &self.config.cancellation);
        if result.is_err() {
            self.config.sprite_sheet_budget.release(id);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bridge::materials::{EncodedTextureBudget, TextureWorkingBudget},
        test_support::fixture_vtf_bytes,
    };
    use std::sync::atomic::Ordering;
    use vformats::vtf::VtfFormat;

    fn sheet_texture(counts: &[u32], marker: f32) -> Vec<u8> {
        let mut payload = Vec::new();
        for value in [0_u32, u32::try_from(counts.len()).unwrap()] {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        for (number, &count) in counts.iter().enumerate() {
            for value in [u32::try_from(number).unwrap(), 0, count, 1_f32.to_bits()] {
                payload.extend_from_slice(&value.to_le_bytes());
            }
            for _ in 0..count {
                for value in [1_f32, marker, 0.0, 1.0, 1.0] {
                    payload.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        let mut bytes = fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[127, 128, 129, 255]]);
        bytes.truncate(64);
        bytes[8..12].copy_from_slice(&3_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&96_u32.to_le_bytes());
        bytes.resize(96, 0);
        bytes[63..65].copy_from_slice(&1_u16.to_le_bytes());
        bytes[68..72].copy_from_slice(&2_u32.to_le_bytes());
        bytes[80] = 0x30;
        bytes[84..88].copy_from_slice(&96_u32.to_le_bytes());
        bytes[88] = 0x10;
        bytes[92..96].copy_from_slice(&100_u32.to_le_bytes());
        bytes.extend_from_slice(&[127, 128, 129, 255]);
        bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes
    }

    fn fixture_resolver(first: Vec<u8>, second: Vec<u8>) -> MaterialResolver {
        MaterialResolver::prop_lighting_fixture(
            vec![
                ("materials/test/a.vtf", first),
                ("materials/test/b.vtf", second),
                (
                    "materials/test/a.vmt",
                    br#"UnlitGeneric { "$basetexture" "test/a" }"#.to_vec(),
                ),
                (
                    "materials/test/alias.vmt",
                    br#"UnlitGeneric { "$basetexture" "test/a" }"#.to_vec(),
                ),
                (
                    "materials/test/b.vmt",
                    br#"UnlitGeneric { "$basetexture" "test/b" }"#.to_vec(),
                ),
            ],
            Vec::new(),
        )
    }

    fn selected(
        resolver: &MaterialResolver,
        name: &str,
    ) -> Arc<super::super::texture_asset::SelectedTextureAsset> {
        resolver
            .selected_texture_asset(&super::super::texture_path(name).unwrap())
            .unwrap()
    }

    #[test]
    fn on_demand_sheets_reuse_only_metadata_and_readmit_stricter_budgets() {
        let resolver = fixture_resolver(sheet_texture(&[2], 0.25), sheet_texture(&[1], 0.5))
            .with_on_demand_textures()
            .with_sprite_sheet_budget(Arc::new(SpriteSheetBudget::new(8, 4096)));
        let first = resolver
            .resolve_particle_sprite_sheet("test/a")
            .unwrap()
            .unwrap();
        let alias = resolver
            .resolve_particle_sprite_sheet("test/alias")
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &alias));
        assert_eq!(
            resolver.config.sprite_sheet_budget.stats().admitted_frames,
            2
        );
        let strict = resolver.with_sprite_sheet_budget(Arc::new(SpriteSheetBudget::new(1, 4096)));
        assert!(matches!(
            strict.resolve_particle_sprite_sheet("test/a"),
            Err(SpriteSheetResolveError::Budget {
                metric: SpriteSheetMetric::Frames,
                ..
            })
        ));
        assert!(Arc::ptr_eq(
            &first,
            &resolver
                .resolve_particle_sprite_sheet("test/a")
                .unwrap()
                .unwrap()
        ));
        assert!(resolver.shared.texture_assets.selected_paths().is_empty());
    }

    #[test]
    fn effective_patch_and_profile_select_the_same_primary_as_rendering() {
        let extra = MaterialResolver::prop_lighting_fixture(vec![
            ("materials/test/base.vmt", br#"UnlitGeneric { "$basetexture" "test/a" }"#.to_vec()),
            ("materials/test/patch.vmt", br#"Patch { "include" "materials/test/base.vmt" "replace" { "$basetexture" "test/b" } }"#.to_vec()),
            ("materials/test/profile.vmt", br#"UnlitGeneric { "$basetexture" "test/a" "hdr" { "$basetexture" "test/b" } }"#.to_vec()),
            ("materials/test/a.vtf", sheet_texture(&[1], 0.25)),
            ("materials/test/b.vtf", sheet_texture(&[2], 0.75)),
        ], Vec::new());
        let resolver = extra;
        let patch = resolver
            .resolve_particle_sprite_sheet("test/patch")
            .unwrap()
            .unwrap();
        assert_eq!(patch.sequences[0].frames.len(), 2);
        assert_eq!(patch.sequences[0].frames[0].uv[0], 0.75);
        let ldr = resolver
            .resolve_particle_sprite_sheet("test/profile")
            .unwrap()
            .unwrap();
        assert_eq!(ldr.sequences[0].frames[0].uv[0], 0.25);
        let hdr = resolver.variant(super::super::ResolverConfig {
            hardware_profile: super::super::profile::HardwareProfile::HDR,
            ..resolver.config.clone()
        });
        let hdr = hdr
            .resolve_particle_sprite_sheet("test/profile")
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&patch, &hdr));
    }

    #[test]
    fn shared_asset_single_flight_charges_once_and_warm_strict_profiles_reject() {
        let resolver = fixture_resolver(sheet_texture(&[2], 0.25), sheet_texture(&[2], 0.75));
        let asset = selected(&resolver, "test/a");
        let cost = asset
            .asset
            .sprite_sheet_cache(&resolver.shared.texture_assets)
            .cost(&asset.asset)
            .unwrap()
            .unwrap();
        let budget = Arc::new(SpriteSheetBudget::new(3, cost.metadata_bytes * 2));
        let resolver = resolver.with_sprite_sheet_budget(Arc::clone(&budget));
        let first = resolver
            .resolve_particle_sprite_sheet("test/a")
            .unwrap()
            .unwrap();
        std::thread::scope(|scope| {
            let mut tasks = Vec::new();
            for _ in 0..8 {
                tasks.push(scope.spawn(|| {
                    resolver
                        .resolve_particle_sprite_sheet("test/alias")
                        .unwrap()
                        .unwrap()
                }));
            }
            for task in tasks {
                assert!(Arc::ptr_eq(&first, &task.join().unwrap()));
            }
        });
        assert_eq!(budget.stats().admitted_frames, 2);
        assert_eq!(budget.stats().admitted_metadata_bytes, cost.metadata_bytes);
        assert!(matches!(
            resolver.resolve_particle_sprite_sheet("test/b"),
            Err(SpriteSheetResolveError::Budget {
                metric: SpriteSheetMetric::Frames,
                ..
            })
        ));
        assert_eq!(budget.stats().rejected_sheets, 1);
        let asset = selected(&resolver, "test/a");
        assert_eq!(
            asset
                .asset
                .sprite_sheet_cache(&resolver.shared.texture_assets)
                .parses
                .load(Ordering::Relaxed),
            1
        );
        let strict =
            resolver.with_sprite_sheet_budget(Arc::new(SpriteSheetBudget::new(1, usize::MAX)));
        assert!(matches!(
            strict.resolve_particle_sprite_sheet("test/a"),
            Err(SpriteSheetResolveError::Budget { .. })
        ));
        let strict = resolver
            .with_sprite_sheet_budget(Arc::new(SpriteSheetBudget::new(2, cost.metadata_bytes - 1)));
        assert!(matches!(
            strict.resolve_particle_sprite_sheet("test/a"),
            Err(SpriteSheetResolveError::Budget {
                metric: SpriteSheetMetric::MetadataBytes,
                ..
            })
        ));
        let strict = resolver.with_texture_working_budget(Arc::new(TextureWorkingBudget::new(
            cost.metadata_bytes - 1,
        )));
        assert!(matches!(
            strict.resolve_particle_sprite_sheet("test/a"),
            Err(SpriteSheetResolveError::Working(_))
        ));
        let strict =
            resolver.with_texture_source_working_budget(Arc::new(TextureWorkingBudget::new(0)));
        assert!(matches!(
            strict.resolve_particle_sprite_sheet("test/a"),
            Err(SpriteSheetResolveError::Source {
                kind: SpriteSheetSourceFailure::WorkingBudget,
                ..
            })
        ));
        assert_eq!(
            asset
                .asset
                .sprite_sheet_cache(&resolver.shared.texture_assets)
                .parses
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn rendering_and_sheet_metadata_reuse_the_same_admitted_source_snapshot() {
        let bytes = sheet_texture(&[2], 0.25);
        let length = bytes.len();
        let resolver = fixture_resolver(bytes, sheet_texture(&[1], 0.75));
        let encoded = Arc::new(EncodedTextureBudget::new(length));
        let source_working = Arc::new(TextureWorkingBudget::new(length));
        let resolver = resolver
            .with_encoded_texture_budget(Arc::clone(&encoded))
            .with_texture_source_working_budget(Arc::clone(&source_working));
        let material = resolver
            .resolve_primary(&[], "test/a")
            .into_material()
            .unwrap();
        assert_eq!(
            material.texture.as_ref().unwrap().mapping_dimensions(),
            Some((1, 1))
        );
        let source = selected(&resolver, "test/a");
        let first = resolver
            .resolve_particle_sprite_sheet("test/a")
            .unwrap()
            .unwrap();
        let second = resolver
            .resolve_particle_sprite_sheet("test/alias")
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(&source, &selected(&resolver, "test/a")));
        assert_eq!(encoded.encoded_bytes(), length as u64);
        assert_eq!(encoded.admitted_textures(), 1);
        assert_eq!(source_working.peak_bytes(), length);
        assert_eq!(source_working.current_bytes(), 0);
    }

    #[test]
    fn identical_content_under_distinct_paths_shares_the_charged_parsed_allocation() {
        let bytes = sheet_texture(&[2], 0.25);
        let resolver = fixture_resolver(bytes.clone(), bytes);
        let first_asset = selected(&resolver, "test/a");
        let second_asset = selected(&resolver, "test/b");
        assert!(!Arc::ptr_eq(&first_asset, &second_asset));
        let first_cache = first_asset
            .asset
            .sprite_sheet_cache(&resolver.shared.texture_assets);
        let second_cache = second_asset
            .asset
            .sprite_sheet_cache(&resolver.shared.texture_assets);
        assert!(std::ptr::eq(first_cache, second_cache));
        let cost = first_cache.cost(&first_asset.asset).unwrap().unwrap();
        let budget = Arc::new(SpriteSheetBudget::new(2, cost.metadata_bytes));
        let resolver = resolver.with_sprite_sheet_budget(Arc::clone(&budget));
        let first = resolver
            .resolve_particle_sprite_sheet("test/a")
            .unwrap()
            .unwrap();
        let second = resolver
            .resolve_particle_sprite_sheet("test/b")
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(budget.stats().admitted_frames, 2);
        assert_eq!(budget.stats().admitted_metadata_bytes, cost.metadata_bytes);
        assert_eq!(first_cache.parses.load(Ordering::Relaxed), 1);
        assert!(format!("{first_cache:?}").len() < 256);
    }

    #[test]
    fn a_budget_shared_across_independent_source_caches_charges_both_physical_sheets() {
        let bytes = sheet_texture(&[2], 0.25);
        let first = fixture_resolver(bytes.clone(), bytes.clone());
        let second = fixture_resolver(bytes.clone(), bytes);
        let cost = selected(&first, "test/a")
            .asset
            .sprite_sheet_cache(&first.shared.texture_assets)
            .cost(&selected(&first, "test/a").asset)
            .unwrap()
            .unwrap();
        let budget = Arc::new(SpriteSheetBudget::new(4, cost.metadata_bytes * 2));
        let first = first.with_sprite_sheet_budget(Arc::clone(&budget));
        let second = second.with_sprite_sheet_budget(Arc::clone(&budget));
        let first = first
            .resolve_particle_sprite_sheet("test/a")
            .unwrap()
            .unwrap();
        let second = second
            .resolve_particle_sprite_sheet("test/a")
            .unwrap()
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(budget.stats().admitted_frames, 4);
        assert_eq!(
            budget.stats().admitted_metadata_bytes,
            cost.metadata_bytes * 2
        );
    }

    #[test]
    fn aggregate_count_rejection_does_not_parse_or_poison_asset() {
        let resolver = fixture_resolver(sheet_texture(&[2, 2], 0.25), sheet_texture(&[1], 0.75));
        let budget = Arc::new(SpriteSheetBudget::new(3, usize::MAX));
        let limited = resolver.with_sprite_sheet_budget(Arc::clone(&budget));
        assert!(matches!(
            limited.resolve_particle_sprite_sheet("test/a"),
            Err(SpriteSheetResolveError::Budget {
                required: 4,
                capacity: 3,
                ..
            })
        ));
        let asset = selected(&resolver, "test/a");
        assert_eq!(
            asset
                .asset
                .sprite_sheet_cache(&resolver.shared.texture_assets)
                .parses
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(budget.stats().admitted_frames, 0);
        assert!(
            limited
                .resolve_particle_sprite_sheet("test/b")
                .unwrap()
                .is_some()
        );
        assert_eq!(budget.stats().admitted_frames, 1);
        assert!(
            resolver
                .resolve_particle_sprite_sheet("test/a")
                .unwrap()
                .is_some()
        );
        assert_eq!(
            asset
                .asset
                .sprite_sheet_cache(&resolver.shared.texture_assets)
                .parses
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn first_invalid_source_and_zero_encoded_budget_never_read_lower_sources() {
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![
                (
                    "materials/test/a.vmt",
                    br#"UnlitGeneric { "$basetexture" "test/a" }"#.to_vec(),
                ),
                ("materials/test/a.vtf", b"bad packed image".to_vec()),
            ],
            vec![("materials/test/a.vtf", sheet_texture(&[1], 0.75))],
        );
        for _ in 0..2 {
            assert!(matches!(
                resolver.resolve_particle_sprite_sheet("test/a"),
                Err(SpriteSheetResolveError::Source {
                    kind: SpriteSheetSourceFailure::Invalid,
                    ..
                })
            ));
        }
        assert_eq!(
            resolver.config.sprite_sheet_budget.stats().admitted_frames,
            0
        );
        let resolver = fixture_resolver(sheet_texture(&[1], 0.25), sheet_texture(&[1], 0.75));
        let budget = Arc::new(EncodedTextureBudget::new(0));
        let rejected = resolver.with_encoded_texture_budget(Arc::clone(&budget));
        assert!(matches!(
            rejected.resolve_particle_sprite_sheet("test/a"),
            Err(SpriteSheetResolveError::Source {
                kind: SpriteSheetSourceFailure::EncodedBudget,
                ..
            })
        ));
        assert_eq!(budget.encoded_bytes(), 0);
        assert_eq!(
            resolver.config.sprite_sheet_budget.stats().admitted_frames,
            0
        );
        assert!(
            resolver
                .resolve_particle_sprite_sheet("test/a")
                .unwrap()
                .is_some()
        );
        assert!(matches!(
            rejected.resolve_particle_sprite_sheet("test/a"),
            Err(SpriteSheetResolveError::Source {
                kind: SpriteSheetSourceFailure::EncodedBudget,
                ..
            })
        ));
    }

    #[test]
    fn invalid_resource_keeps_admission_available_for_later_valid_sheet() {
        let mut broken = sheet_texture(&[2], 0.25);
        broken.pop();
        let resolver = fixture_resolver(broken, sheet_texture(&[2], 0.75));
        let budget = Arc::new(SpriteSheetBudget::new(2, usize::MAX));
        let resolver = resolver.with_sprite_sheet_budget(Arc::clone(&budget));
        for _ in 0..2 {
            assert!(matches!(
                resolver.resolve_particle_sprite_sheet("test/a"),
                Err(SpriteSheetResolveError::Invalid(
                    SpriteSheetError::InvalidResource
                ))
            ));
            assert_eq!(budget.stats().admitted_frames, 0);
            assert_eq!(budget.stats().admitted_metadata_bytes, 0);
        }
        assert!(
            resolver
                .resolve_particle_sprite_sheet("test/b")
                .unwrap()
                .is_some()
        );
        assert_eq!(budget.stats().admitted_frames, 2);
        assert_eq!(
            selected(&resolver, "test/a")
                .asset
                .sprite_sheet_cache(&resolver.shared.texture_assets)
                .parsed
                .get(),
            None
        );
    }
}
