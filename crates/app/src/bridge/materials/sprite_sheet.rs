use super::{
    Arc, ContentPath, HashMap, MaterialResolutionOutcome, MaterialResolver, Mutex, OnceLock,
    definition::MaterialSourceFailure, material_paths, texture_asset::TextureSourceError,
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
    #[cfg(test)]
    parses: std::sync::atomic::AtomicUsize,
}

impl Default for SpriteSheetCache {
    fn default() -> Self {
        Self {
            allocation_id: SpriteSheetAllocationId(super::texture_source::source_token()),
            reloadable_parsed: Default::default(),
            cost: OnceLock::new(),
            #[cfg(test)]
            parses: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl std::fmt::Debug for SpriteSheetCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpriteSheetCache")
            .field("cost", &self.cost.get())
            .field("parsed", &self.reloadable_parsed.get().is_some())
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
                #[cfg(test)]
                self.parses
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        let source = self.selected_reloadable_texture(&path).map_err(|error| {
            SpriteSheetResolveError::Source {
                path: path.clone(),
                kind: match &error {
                    TextureSourceError::Missing => SpriteSheetSourceFailure::Missing,
                    TextureSourceError::Invalid { .. } => SpriteSheetSourceFailure::Invalid,
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
        source.sprite_sheet_cache.parse_reloadable(&source, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bridge::materials::TextureWorkingBudget, test_support::fixture_vtf_bytes};
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

    #[test]
    fn sheets_share_source_metadata_and_recheck_stricter_budgets() {
        let resolver = fixture_resolver(sheet_texture(&[2], 0.25), sheet_texture(&[1], 0.75));
        let budget = Arc::new(SpriteSheetBudget::new(2, 4096));
        let resolver = resolver.with_sprite_sheet_budget(Arc::clone(&budget));
        let first = resolver
            .resolve_particle_sprite_sheet("test/a")
            .unwrap()
            .unwrap();
        let alias = resolver
            .resolve_particle_sprite_sheet("test/alias")
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &alias));
        assert_eq!(budget.stats().admitted_frames, 2);
        assert!(matches!(
            resolver.resolve_particle_sprite_sheet("test/b"),
            Err(SpriteSheetResolveError::Budget {
                metric: SpriteSheetMetric::Frames,
                ..
            })
        ));
        let strict = resolver.with_sprite_sheet_budget(Arc::new(SpriteSheetBudget::new(1, 4096)));
        assert!(matches!(
            strict.resolve_particle_sprite_sheet("test/a"),
            Err(SpriteSheetResolveError::Budget { .. })
        ));
        assert!(Arc::ptr_eq(
            &first,
            &resolver
                .resolve_particle_sprite_sheet("test/a")
                .unwrap()
                .unwrap()
        ));
        let texture = resolver
            .resolve_primary(&[], "test/a")
            .into_material()
            .unwrap()
            .texture
            .unwrap();
        assert!(texture.resident().is_none());
        let source = texture.on_demand().unwrap().0;
        assert!(Arc::ptr_eq(
            source,
            &resolver
                .selected_reloadable_texture(&super::super::texture_path("test/a").unwrap())
                .unwrap()
        ));
    }

    #[test]
    fn concurrent_aliases_parse_and_charge_one_sheet() {
        let resolver = fixture_resolver(sheet_texture(&[2], 0.25), sheet_texture(&[1], 0.75));
        let budget = Arc::new(SpriteSheetBudget::new(2, 4096));
        let resolver = resolver.with_sprite_sheet_budget(Arc::clone(&budget));
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        resolver
                            .resolve_particle_sprite_sheet("test/alias")
                            .unwrap()
                            .unwrap()
                    })
                })
                .collect();
            let first = handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>();
            assert!(first.iter().all(|sheet| Arc::ptr_eq(&first[0], sheet)));
        });
        let source = resolver
            .selected_reloadable_texture(&super::super::texture_path("test/a").unwrap())
            .unwrap();
        assert_eq!(source.sprite_sheet_cache.parses.load(Ordering::Relaxed), 1);
        assert_eq!(budget.stats().admitted_frames, 2);
    }

    #[test]
    fn rejected_sheet_does_not_poison_a_larger_budget() {
        let resolver = fixture_resolver(sheet_texture(&[2, 2], 0.25), sheet_texture(&[1], 0.75));
        let budget = Arc::new(SpriteSheetBudget::new(3, 4096));
        let limited = resolver.with_sprite_sheet_budget(Arc::clone(&budget));
        assert!(matches!(
            limited.resolve_particle_sprite_sheet("test/a"),
            Err(SpriteSheetResolveError::Budget {
                required: 4,
                capacity: 3,
                ..
            })
        ));
        assert_eq!(budget.stats().admitted_frames, 0);
        assert!(
            limited
                .resolve_particle_sprite_sheet("test/b")
                .unwrap()
                .is_some()
        );
        assert!(
            resolver
                .resolve_particle_sprite_sheet("test/a")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn invalid_high_priority_source_does_not_fall_through() {
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
    }

    #[test]
    fn invalid_resource_releases_sheet_admission_for_the_next_source() {
        let mut broken = sheet_texture(&[2], 0.25);
        broken.pop();
        let resolver = fixture_resolver(broken, sheet_texture(&[2], 0.75));
        let budget = Arc::new(SpriteSheetBudget::new(2, 4096));
        let resolver = resolver.with_sprite_sheet_budget(Arc::clone(&budget));
        for _ in 0..2 {
            assert!(resolver.resolve_particle_sprite_sheet("test/a").is_err());
            assert_eq!(budget.stats().admitted_frames, 0);
        }
        assert!(
            resolver
                .resolve_particle_sprite_sheet("test/b")
                .unwrap()
                .is_some()
        );
        assert_eq!(budget.stats().admitted_frames, 2);
    }

    #[test]
    fn patch_selection_and_working_rejection_preserve_the_source() {
        let resolver = MaterialResolver::prop_lighting_fixture(vec![
            ("materials/test/base.vmt", br#"UnlitGeneric { "$basetexture" "test/a" }"#.to_vec()),
            ("materials/test/patch.vmt", br#"Patch { "include" "materials/test/base.vmt" "replace" { "$basetexture" "test/b" } }"#.to_vec()),
            ("materials/test/a.vtf", sheet_texture(&[1], 0.25)),
            ("materials/test/b.vtf", sheet_texture(&[2], 0.75)),
        ], Vec::new());
        let strict =
            resolver.with_texture_source_working_budget(Arc::new(TextureWorkingBudget::new(0)));
        assert!(matches!(
            strict.resolve_particle_sprite_sheet("test/patch"),
            Err(SpriteSheetResolveError::Source {
                kind: SpriteSheetSourceFailure::WorkingBudget,
                ..
            })
        ));
        let sheet = resolver
            .resolve_particle_sprite_sheet("test/patch")
            .unwrap()
            .unwrap();
        assert_eq!(sheet.sequences[0].frames.len(), 2);
        assert_eq!(sheet.sequences[0].frames[0].uv[0], 0.75);
    }
}
