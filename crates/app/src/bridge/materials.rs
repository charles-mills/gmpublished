use gmpublished_domain::math::Vec3;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

mod cubemap;
mod decal_format;
mod definition;
use definition::MaterialDefinitionCache;
mod discovery;
mod frames;
mod lightmap_layout;
mod mirror;
mod parameters;
use parameters::parse_material_color;
mod phong;
mod profile;
mod prop_lighting;
mod radiosity;
mod shader;
mod sibling_gma;
mod source_discovery;
mod source_index;
pub use source_discovery::{SourceDiscoveryFailure, SourceDiscoveryLimits, SourceDiscoveryStats};
pub use source_index::ArchiveIndexStats;
use source_index::SourceIndexBudget;
mod sky;
pub mod sprite_sheet;
pub use sprite_sheet::{
    DEFAULT_SPRITE_SHEET_FRAMES, DEFAULT_SPRITE_SHEET_METADATA_BYTES, SpriteSheetBudget,
};
mod surface;
mod texture;
mod texture_asset;
mod texture_budget;
pub use texture_budget::DecodedTextureBudget;
mod texture_plan;
mod texture_source;
mod texture_working;
pub use texture_source::EncodedTextureBudget;
#[cfg(feature = "map-capture")]
pub use texture_working::TextureHostLimits;
pub use texture_working::TextureWorkingBudget;
mod water;
#[cfg(test)]
pub use cubemap::ResolvedCubemap;
pub use cubemap::{CubemapFailure, CubemapResolution};
pub use cubemap::{EnvironmentMapMask, EnvironmentMapMaterial, EnvironmentMapSource};
pub use decal_format::SourceDecalFormat;
pub use lightmap_layout::ResolvedLightmapLayoutMaterial;
pub use mirror::MirrorMaterial;
pub use phong::{PhongMaterial, PhongTexture};
pub use radiosity::ResolvedSurfaceLighting;
pub use shader::MaterialShader;
pub use sky::{PaintedSkyMaterial, SkyMaterial};
#[cfg(test)]
use surface::DecalParameters;
pub use surface::{ModelShadingProfile, SourceSurfaceBaseBlend, SurfaceFamily, SurfaceMaterial};
pub use water::WaterMaterial;

pub use frames::{DecodedTextureFrames, TextureReadTransfer, TextureRole};
pub use texture::decode_vtf_rgba;
use texture::push_matching_path;
#[cfg(test)]
use texture::resolved_bc_texture;
pub use texture_asset::{
    ResidentTexture, ResidentTextureLayer, ResidentTextureMip, TextureAssetError, TextureBytes,
    TextureMetadata, TextureResidency, TextureResidentId, TextureStorage,
};
use texture_asset::{TextureAssetCache, TextureSourceError};
pub use texture_plan::{TextureAdmissions, TextureRequestCost, TextureRequestPlan};

use sibling_gma::{SiblingGmaIndex, SiblingGmaPath};
#[cfg(test)]
use sibling_gma::{SiblingGmaPathKind, build_sibling_gma_index};

#[cfg(test)]
use discovery::discover_sibling_gma_paths;
#[cfg(test)]
use discovery::normalize_source_path;
use discovery::{material_paths, normalize_texture_name, strip_prefix_ascii_case, texture_path};

use parking_lot::Mutex;

use crate::bridge::archive::{PreviewArchiveSource, PreviewArchiveSourceError};
use crate::bridge::content_path::{ContentPath, normalize_archive_path};
use crate::bridge::gma::PreviewArchive;
use crate::bridge::vpk::{VpkArchive, VpkError};
use gmpublished_domain::scene::map::MapPakFile;
use vformats::vtf::BcFormat;
use vformats::{Limits, soundscript};

const PATCH_INCLUDE_LIMIT: usize = 4;
const MAX_LEGACY_BIN_FETCH_BYTES: u64 = 1024 * 1024 * 1024;
pub const DEFAULT_WATER_FOG_LINEAR: Vec3 = Vec3::new(0.03, 0.10, 0.10);
const ENVMAP_FALLBACK_RGBA: [u8; 4] = [128, 128, 128, 255];
const NATIVE_ERROR_TEXTURE_FLAGS: u32 = 0x0008_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextureLoadOutcome {
    Loaded,
    Generated,
    MissingSource,
    InvalidSource,
    InvalidPayload,
    QualityRejected,
    SourceBudgetRejected,
    SourceUnavailable,
    Unsupported,
    Deferred,
    Unplanned,
}

impl TextureLoadOutcome {
    pub(crate) const fn is_unavailable(self) -> bool {
        matches!(
            self,
            Self::QualityRejected
                | Self::SourceBudgetRejected
                | Self::SourceUnavailable
                | Self::Unsupported
                | Self::Deferred
                | Self::Unplanned
        )
    }

    const fn uses_native_error_texture(self) -> bool {
        matches!(
            self,
            Self::MissingSource | Self::InvalidSource | Self::InvalidPayload
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTexture {
    resident: TextureResidency,
    source_metadata: Option<Arc<TextureMetadata>>,
    mapping_dimensions: Option<[u32; 2]>,
    load_outcome: TextureLoadOutcome,
    water_fallback: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTextureMip {
    pub(crate) rgba: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedTextureMipRef<'a> {
    pub(crate) rgba: &'a [u8],
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl ResolvedTexture {
    fn rgba(
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        original_width: u32,
        original_height: u32,
        water_fallback: bool,
    ) -> Self {
        let resident = texture_asset::generated_rgba(
            "material-fallback",
            None,
            vec![vec![ResolvedTextureMip {
                rgba,
                width,
                height,
            }]],
        )
        .expect("generated material image has valid dimensions");
        let source_metadata =
            if [original_width, original_height] == resident.metadata().original_dimensions {
                resident.metadata_arc()
            } else {
                let mut metadata = resident.metadata().clone();
                metadata.original_dimensions = [original_width, original_height];
                Arc::new(metadata)
            };
        Self {
            resident: TextureResidency::Resident(resident),
            source_metadata: Some(source_metadata),
            mapping_dimensions: (!water_fallback).then_some([original_width, original_height]),
            load_outcome: TextureLoadOutcome::Generated,
            water_fallback,
        }
    }

    fn from_resident(resident: TextureResidency, source_metadata: Arc<TextureMetadata>) -> Self {
        let load_outcome = resident.load_outcome();
        Self {
            mapping_dimensions: Some(source_metadata.original_dimensions),
            resident,
            source_metadata: Some(source_metadata),
            load_outcome,
            water_fallback: false,
        }
    }

    fn unavailable(
        source_metadata: Option<Arc<TextureMetadata>>,
        load_outcome: TextureLoadOutcome,
    ) -> Self {
        let error_texture = load_outcome.uses_native_error_texture();
        let resident = if error_texture {
            texture_asset::native_error_resident()
        } else {
            texture_asset::generated_rgba(
                "unavailable-resident-base",
                None,
                vec![vec![ResolvedTextureMip {
                    rgba: vec![
                        255, 0, 255, 255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 0, 255, 255,
                    ],
                    width: 2,
                    height: 2,
                }]],
            )
            .expect("fallback image has valid dimensions")
        };
        let mapping_dimensions = if error_texture {
            Some(resident.metadata().original_dimensions)
        } else {
            source_metadata
                .as_ref()
                .map(|metadata| metadata.original_dimensions)
        };
        Self {
            resident: TextureResidency::Resident(resident),
            source_metadata,
            mapping_dimensions,
            load_outcome,
            water_fallback: false,
        }
    }

    pub(crate) fn resident(&self) -> Option<&Arc<ResidentTexture>> {
        self.resident.resident()
    }
    pub(crate) fn source_metadata(&self) -> Option<&TextureMetadata> {
        self.source_metadata.as_deref()
    }
    pub(crate) fn load_outcome(&self) -> TextureLoadOutcome {
        self.load_outcome
    }
    #[cfg(any(test, feature = "map-capture"))]
    pub(crate) fn width(&self) -> u32 {
        self.dimensions()[0]
    }
    #[cfg(any(test, feature = "map-capture"))]
    pub(crate) fn height(&self) -> u32 {
        self.dimensions()[1]
    }
    #[cfg(any(test, feature = "map-capture"))]
    fn dimensions(&self) -> [u32; 2] {
        match &self.resident {
            TextureResidency::Resident(resident) => resident.layers()[0].mips[0].dimensions,
            TextureResidency::Deferred(_) => self.resident.dimensions(
                self.source_metadata()
                    .expect("deferred image retains source metadata"),
            ),
        }
    }
    pub(crate) fn flags(&self) -> Option<u32> {
        if self.load_outcome.uses_native_error_texture() {
            Some(NATIVE_ERROR_TEXTURE_FLAGS)
        } else {
            self.source_metadata().and_then(|metadata| metadata.flags)
        }
    }
    #[cfg(any(test, feature = "map-capture"))]
    pub(crate) fn reflectivity_bits(&self) -> Option<[u32; 3]> {
        if self.load_outcome.uses_native_error_texture() {
            Some([0.5_f32.to_bits(); 3])
        } else {
            self.source_metadata()
                .and_then(|metadata| metadata.reflectivity_bits)
        }
    }
    pub(crate) fn has_effective_base_texture(&self) -> bool {
        !self.water_fallback
    }
    #[cfg(any(test, feature = "map-capture"))]
    pub(crate) fn is_water_fallback(&self) -> bool {
        self.water_fallback
    }
    #[cfg(test)]
    pub(crate) fn rgba_bytes(&self) -> Option<&[u8]> {
        let resident = self.resident()?;
        matches!(resident.id().storage, TextureStorage::Rgba8)
            .then(|| resident.layers()[0].mips[0].bytes.as_slice())
    }
    pub(crate) fn bc_payload(&self) -> Option<(BcFormat, &[ResidentTextureMip])> {
        let resident = self.resident()?;
        if let TextureStorage::Bc(format) = resident.id().storage {
            Some((format, &resident.layers()[0].mips))
        } else {
            None
        }
    }
    pub(crate) fn is_bc(&self) -> bool {
        matches!(self.resident.id().storage, TextureStorage::Bc(_))
    }
    #[cfg(test)]
    pub(crate) fn mip_level_count(&self) -> u32 {
        u32::from(self.resident.id().mip_count)
    }
    pub(crate) fn mip_chain(&self) -> impl Iterator<Item = ResolvedTextureMipRef<'_>> {
        self.resident()
            .into_iter()
            .flat_map(|resident| resident.layers()[0].mips.iter())
            .filter(|_| matches!(self.resident.id().storage, TextureStorage::Rgba8))
            .map(|mip| ResolvedTextureMipRef {
                rgba: mip.bytes.as_slice(),
                width: mip.dimensions[0],
                height: mip.dimensions[1],
            })
    }
    #[cfg(any(test, feature = "map-capture"))]
    pub(crate) fn original_dimensions(&self) -> (u32, u32) {
        let [width, height] = self
            .source_metadata()
            .unwrap_or_else(|| {
                self.resident()
                    .expect("generated image has residency")
                    .metadata()
            })
            .original_dimensions;
        (width.max(1), height.max(1))
    }

    pub(crate) fn mapping_dimensions(&self) -> Option<(u32, u32)> {
        self.mapping_dimensions
            .map(|[width, height]| (width.max(1), height.max(1)))
    }

    fn unmapped(mut self) -> Self {
        self.mapping_dimensions = None;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedMaterialTextures {
    pub(crate) shader: MaterialShader,
    pub(crate) texture: Option<Arc<ResolvedTexture>>,
    pub(crate) texture2: Option<Arc<ResolvedTexture>>,
    pub(crate) render_mode: RenderMode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPrimaryMaterial {
    pub(crate) shader: MaterialShader,
    pub(crate) texture: Arc<ResolvedTexture>,
    pub(crate) texture2: Option<Arc<ResolvedTexture>>,
    pub(crate) render_mode: RenderMode,
}

fn canonical_error_primary() -> ResolvedPrimaryMaterial {
    static MATERIAL: OnceLock<ResolvedPrimaryMaterial> = OnceLock::new();
    MATERIAL
        .get_or_init(|| ResolvedPrimaryMaterial {
            shader: MaterialShader::Surface(Arc::new(SurfaceMaterial {
                shader: "UnlitGeneric".to_owned(),
                family: SurfaceFamily::Unlit,
                diffuse_bump: false,
                ..Default::default()
            })),
            texture: Arc::new(ResolvedTexture {
                resident: TextureResidency::Resident(texture_asset::missing_material_resident()),
                source_metadata: None,
                mapping_dimensions: Some([32, 32]),
                load_outcome: TextureLoadOutcome::Generated,
                water_fallback: false,
            }),
            texture2: None,
            render_mode: RenderMode::Opaque,
        })
        .clone()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MaterialResolutionOutcome {
    Loaded,
    Missing,
    Malformed,
    IndexUnavailable,
    SourceUnavailable,
    #[default]
    Unresolved,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MaterialResolution<T> {
    Loaded { canonical_name: String, material: T },
    Missing { fallback: T },
    Malformed { path: ContentPath },
    IndexUnavailable { error: vformats::ArchiveIndexLimit },
    SourceUnavailable { error: SourceDiscoveryFailure },
    Unresolved { canonical_name: String },
}

impl<T> MaterialResolution<T> {
    pub(crate) fn material(&self) -> Option<&T> {
        match self {
            Self::Loaded { material, .. } => Some(material),
            Self::Missing { fallback } => Some(fallback),
            Self::Malformed { .. }
            | Self::SourceUnavailable { .. }
            | Self::IndexUnavailable { .. }
            | Self::Unresolved { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn into_material(self) -> Option<T> {
        match self {
            Self::Loaded { material, .. } => Some(material),
            Self::Missing { fallback } => Some(fallback),
            Self::Malformed { .. }
            | Self::SourceUnavailable { .. }
            | Self::IndexUnavailable { .. }
            | Self::Unresolved { .. } => None,
        }
    }

    pub(crate) fn outcome(&self) -> MaterialResolutionOutcome {
        match self {
            Self::Loaded { .. } => MaterialResolutionOutcome::Loaded,
            Self::Missing { .. } => MaterialResolutionOutcome::Missing,
            Self::Malformed { .. } => MaterialResolutionOutcome::Malformed,
            Self::IndexUnavailable { .. } => MaterialResolutionOutcome::IndexUnavailable,
            Self::SourceUnavailable { .. } => MaterialResolutionOutcome::SourceUnavailable,
            Self::Unresolved { .. } => MaterialResolutionOutcome::Unresolved,
        }
    }

    pub(crate) fn canonical_name(&self) -> Option<&str> {
        match self {
            Self::Loaded { canonical_name, .. } | Self::Unresolved { canonical_name } => {
                Some(canonical_name)
            }
            Self::Missing { .. } => Some("___error"),
            Self::Malformed { .. }
            | Self::SourceUnavailable { .. }
            | Self::IndexUnavailable { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderMode {
    Opaque,
    Cutout,
    Translucent,
    Additive,
    Modulate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextureAlphaPolicy {
    Opaque,
    Sampled,
}

impl TextureAlphaPolicy {
    pub(crate) const fn force_opaque(self) -> bool {
        matches!(self, Self::Opaque)
    }
}

impl RenderMode {
    pub(crate) const fn texture_alpha_policy(self) -> TextureAlphaPolicy {
        if matches!(self, Self::Opaque) {
            TextureAlphaPolicy::Opaque
        } else {
            TextureAlphaPolicy::Sampled
        }
    }
}

#[derive(Clone)]
struct ResolverConfig {
    cancellation: crate::preparation::PreparationCancellation,
    hardware_profile: profile::HardwareProfile,
    pakfile: Option<Arc<PakSource>>,
    decoded_texture_max_dimension: Option<u32>,
    decoded_texture_budget: Option<Arc<DecodedTextureBudget>>,
    encoded_texture_budget: Option<Arc<EncodedTextureBudget>>,
    texture_working_budget: Option<Arc<TextureWorkingBudget>>,
    texture_source_working_budget: Option<Arc<TextureWorkingBudget>>,
    texture_decoder_limit: Option<usize>,
    texture_resource_limit: Option<usize>,
    sprite_sheet_budget: Arc<SpriteSheetBudget>,
    bc_textures: BcTextures,
    texture_access: texture_plan::TextureAccess,
    single_texture_mip: bool,
}

#[derive(Debug)]
struct IndexedVpks {
    archives: Vec<VpkArchive>,
    _reservations: Vec<source_index::IndexReservation>,
}

#[derive(Debug)]
struct ResolverShared {
    addon: Arc<PreviewArchiveSource>,
    addon_index_error: OnceLock<Option<vformats::ArchiveIndexLimit>>,
    index_budget: Arc<SourceIndexBudget>,
    discovery_stats: SourceDiscoveryStats,
    discovery_failures: BTreeMap<ContentSourceTier, SourceDiscoveryFailure>,
    loose_source_dirs: Vec<LooseSourceDir>,
    sibling_gma_paths: Vec<SiblingGmaPath>,
    game_vpk_paths: Vec<PathBuf>,
    sibling_gmas: crate::preparation::FlightCell<SiblingGmaIndex>,
    game_vpks: crate::preparation::FlightCell<IndexedVpks>,
    material_definitions: MaterialDefinitionCache,
    texture_assets: TextureAssetCache,
    sound_scripts: crate::preparation::FlightCell<SoundScriptLibrary>,
    resolved_sound_cache: Mutex<HashMap<String, Option<ResolvedSoundReference>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BcTextures {
    RetainAuthored,
    Never,
    #[cfg(test)]
    Forced(bool),
}

pub struct MaterialResolver {
    config: ResolverConfig,
    shared: Arc<ResolverShared>,
}

pub trait IntoPreviewArchiveSource {
    fn into_preview_archive_source(self) -> Arc<PreviewArchiveSource>;
}

impl IntoPreviewArchiveSource for Arc<PreviewArchiveSource> {
    fn into_preview_archive_source(self) -> Arc<PreviewArchiveSource> {
        self
    }
}

impl IntoPreviewArchiveSource for Arc<PreviewArchive> {
    fn into_preview_archive_source(self) -> Arc<PreviewArchiveSource> {
        PreviewArchiveSource::from_gma(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ContentSourceTier {
    Pakfile,
    Addon,
    Loose,
    SiblingGma,
    GameVpk,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedSoundReference {
    pub(crate) reference: String,
    pub(crate) sound_level: f32,
    pub(crate) volume: f32,
    pub(crate) waves: Vec<ResolvedSoundWave>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSoundWave {
    pub(crate) path: String,
    pub(crate) source_tier: ContentSourceTier,
    pub(crate) bytes: Arc<[u8]>,
}

#[derive(Debug)]
struct ResolvedContentBytes {
    path: String,
    tier: ContentSourceTier,
    bytes: Vec<u8>,
}

#[derive(Debug)]
struct StoredSoundScript {
    volume: Option<String>,
    sound_level: Option<String>,
    waves: Vec<String>,
}

impl From<&soundscript::SoundScript<'_>> for StoredSoundScript {
    fn from(script: &soundscript::SoundScript<'_>) -> Self {
        Self {
            volume: script.volume.as_deref().map(str::to_owned),
            sound_level: script.sound_level.as_deref().map(str::to_owned),
            waves: script.waves.iter().map(ToString::to_string).collect(),
        }
    }
}

#[derive(Debug)]
struct SoundScriptLibrary {
    scripts: HashMap<String, StoredSoundScript>,
    #[cfg(test)]
    script_files: Vec<String>,
}

impl MaterialResolver {
    #[cfg(test)]
    pub(crate) fn with_cancellation(
        &self,
        cancellation: crate::preparation::PreparationCancellation,
    ) -> Self {
        self.variant(ResolverConfig {
            cancellation,
            ..self.config.clone()
        })
    }
    pub(crate) fn check_cancelled(&self) -> Result<(), crate::preparation::Cancelled> {
        self.config.cancellation.check()
    }
    pub(crate) fn default_archive_index_working_budget() -> Arc<TextureWorkingBudget> {
        source_index::SourceIndexBudget::default_working()
    }
    fn variant(&self, config: ResolverConfig) -> Self {
        Self {
            config,
            shared: Arc::clone(&self.shared),
        }
    }

    #[cfg(test)]
    pub(crate) fn new(addon: impl IntoPreviewArchiveSource, gmod_dir: Option<PathBuf>) -> Self {
        Self::new_with_index_limits(
            addon,
            gmod_dir,
            vformats::ArchiveIndexLimits::default(),
            SourceIndexBudget::default_working(),
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_index_limits(
        addon: impl IntoPreviewArchiveSource,
        gmod_dir: Option<PathBuf>,
        limits: vformats::ArchiveIndexLimits,
        working: Arc<TextureWorkingBudget>,
    ) -> Self {
        Self::new_with_index_limits_cancellable(
            addon,
            gmod_dir,
            limits,
            working,
            crate::preparation::PreparationCancellation::default(),
        )
        .expect("uncancelled source construction")
    }

    #[cfg(test)]
    pub(crate) fn new_with_index_limits_cancellable(
        addon: impl IntoPreviewArchiveSource,
        gmod_dir: Option<PathBuf>,
        limits: vformats::ArchiveIndexLimits,
        working: Arc<TextureWorkingBudget>,
        cancellation: crate::preparation::PreparationCancellation,
    ) -> Result<Self, crate::preparation::Cancelled> {
        Self::new_with_source_policy_cancellable(
            addon,
            gmod_dir,
            limits,
            working,
            SourceDiscoveryLimits::default(),
            cancellation,
        )
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "Preview callers own the content-root configuration"
    )]
    pub(crate) fn new_with_source_policy_cancellable(
        addon: impl IntoPreviewArchiveSource,
        gmod_dir: Option<PathBuf>,
        limits: vformats::ArchiveIndexLimits,
        working: Arc<TextureWorkingBudget>,
        discovery_limits: SourceDiscoveryLimits,
        cancellation: crate::preparation::PreparationCancellation,
    ) -> Result<Self, crate::preparation::Cancelled> {
        use gmpublished_backend::source_discovery::{SourceCategory, SourceDiscoveryBudget};
        cancellation.check()?;
        let scratch_bytes = if gmod_dir.is_some() {
            discovery_limits
                .metadata_bytes
                .min(working.capacity() as u64) as usize
        } else {
            0
        };
        let scratch = if scratch_bytes == 0 {
            None
        } else {
            Some(
                working
                    .request(scratch_bytes)
                    .expect("discovery reservation fits working capacity")
                    .acquire_cancellable(&cancellation)?,
            )
        };
        let mut budget = SourceDiscoveryBudget::new(discovery_limits, scratch_bytes as u64);
        let mut failures = BTreeMap::new();
        let mut ctx = source_discovery::Discovery {
            budget: &mut budget,
            cancellation: &cancellation,
            category: SourceCategory::MountedGames,
        };
        let explicit = source_discovery::category_result(
            gmod_dir.as_deref().map_or_else(
                || Ok(Vec::new()),
                |dir| discovery::mount_cfg_dirs_checked(dir, &mut ctx),
            ),
            ctx.budget,
            &mut failures,
            &[ContentSourceTier::Loose, ContentSourceTier::GameVpk],
        )?;
        let mounted = if failures.contains_key(&ContentSourceTier::GameVpk) {
            Vec::new()
        } else {
            source_discovery::category_result(
                gmod_dir.as_deref().map_or_else(
                    || Ok(Vec::new()),
                    |dir| discovery::discover_mounted_game_dirs_checked(dir, &explicit, &mut ctx),
                ),
                ctx.budget,
                &mut failures,
                &[ContentSourceTier::GameVpk],
            )?
        };
        ctx.category = SourceCategory::Loose;
        let loose_source_dirs = if failures.contains_key(&ContentSourceTier::Loose) {
            Vec::new()
        } else {
            source_discovery::category_result(
                gmod_dir.as_deref().map_or_else(
                    || Ok(Vec::new()),
                    |dir| discovery::discover_loose_source_dirs_checked(dir, &explicit, &mut ctx),
                ),
                ctx.budget,
                &mut failures,
                &[ContentSourceTier::Loose],
            )?
        };
        ctx.category = SourceCategory::SiblingGma;
        let sibling_gma_paths = source_discovery::category_result(
            gmod_dir.as_deref().map_or_else(
                || Ok(Vec::new()),
                |dir| discovery::discover_sibling_gma_paths_checked(dir, &mut ctx),
            ),
            ctx.budget,
            &mut failures,
            &[ContentSourceTier::SiblingGma],
        )?;
        let game_vpk_paths = if failures.contains_key(&ContentSourceTier::GameVpk) {
            Vec::new()
        } else {
            source_discovery::category_result(
                gmod_dir.as_deref().map_or_else(
                    || Ok(Vec::new()),
                    |dir| {
                        gmpublished_backend::vpk::discover_game_vpks_checked(
                            dir,
                            &mounted,
                            ctx.budget,
                            &|| cancellation.is_cancelled(),
                        )
                    },
                ),
                ctx.budget,
                &mut failures,
                &[ContentSourceTier::GameVpk],
            )?
        };
        let discovery_stats = budget.stats().clone();
        drop(scratch);
        let index_budget = SourceIndexBudget::new(limits, working);
        let addon = addon.into_preview_archive_source();
        cancellation.check()?;
        Ok(Self {
            config: ResolverConfig {
                cancellation,
                hardware_profile: profile::HardwareProfile::LDR,
                pakfile: None,
                decoded_texture_max_dimension: None,
                decoded_texture_budget: None,
                encoded_texture_budget: None,
                texture_working_budget: None,
                texture_source_working_budget: None,
                texture_decoder_limit: None,
                texture_resource_limit: None,
                sprite_sheet_budget: Arc::new(SpriteSheetBudget::new(
                    DEFAULT_SPRITE_SHEET_FRAMES,
                    DEFAULT_SPRITE_SHEET_METADATA_BYTES,
                )),
                bc_textures: BcTextures::RetainAuthored,
                texture_access: texture_plan::TextureAccess::Direct,
                single_texture_mip: false,
            },
            shared: Arc::new(ResolverShared {
                addon,
                addon_index_error: OnceLock::new(),
                index_budget,
                discovery_stats,
                discovery_failures: failures,
                loose_source_dirs,
                sibling_gma_paths,
                game_vpk_paths,
                sibling_gmas: Default::default(),
                game_vpks: Default::default(),
                material_definitions: MaterialDefinitionCache::default(),
                texture_assets: TextureAssetCache::default(),
                sound_scripts: Default::default(),
                resolved_sound_cache: Mutex::new(HashMap::new()),
            }),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_pakfile_source(
        addon: impl IntoPreviewArchiveSource,
        gmod_dir: Option<PathBuf>,
        pakfile: MapPakFile,
    ) -> Self {
        Self::with_pakfile_source_and_index_limits(
            addon,
            gmod_dir,
            pakfile,
            vformats::ArchiveIndexLimits::default(),
            SourceIndexBudget::default_working(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_pakfile_source_and_index_limits(
        addon: impl IntoPreviewArchiveSource,
        gmod_dir: Option<PathBuf>,
        pakfile: MapPakFile,
        limits: vformats::ArchiveIndexLimits,
        working: Arc<TextureWorkingBudget>,
    ) -> Self {
        Self::with_pakfile_source_cancellable(
            addon,
            gmod_dir,
            pakfile,
            limits,
            working,
            crate::preparation::PreparationCancellation::default(),
        )
        .expect("uncancelled pak source construction")
    }

    #[cfg(test)]
    pub(crate) fn with_pakfile_source_cancellable(
        addon: impl IntoPreviewArchiveSource,
        gmod_dir: Option<PathBuf>,
        pakfile: MapPakFile,
        limits: vformats::ArchiveIndexLimits,
        working: Arc<TextureWorkingBudget>,
        cancellation: crate::preparation::PreparationCancellation,
    ) -> Result<Self, crate::preparation::Cancelled> {
        Self::with_pakfile_source_policy_cancellable(
            addon,
            gmod_dir,
            pakfile,
            limits,
            working,
            SourceDiscoveryLimits::default(),
            cancellation,
        )
    }

    pub(crate) fn with_pakfile_source_policy_cancellable(
        addon: impl IntoPreviewArchiveSource,
        gmod_dir: Option<PathBuf>,
        pakfile: MapPakFile,
        limits: vformats::ArchiveIndexLimits,
        working: Arc<TextureWorkingBudget>,
        discovery_limits: SourceDiscoveryLimits,
        cancellation: crate::preparation::PreparationCancellation,
    ) -> Result<Self, crate::preparation::Cancelled> {
        let mut resolver = Self::new_with_source_policy_cancellable(
            addon,
            gmod_dir,
            limits,
            working,
            discovery_limits,
            cancellation,
        )?;
        resolver.config.pakfile =
            PakSource::new_with_budget(pakfile, &resolver.shared.index_budget).map(Arc::new);
        resolver.check_cancelled()?;
        Ok(resolver)
    }

    pub(crate) fn with_decoded_texture_max_dimension(&self, max_dimension: u32) -> Self {
        self.variant(ResolverConfig {
            decoded_texture_max_dimension: Some(max_dimension.max(1)),
            ..self.config.clone()
        })
    }

    pub(crate) fn with_decoded_texture_budget(&self, budget: Arc<DecodedTextureBudget>) -> Self {
        self.variant(ResolverConfig {
            decoded_texture_budget: Some(budget),
            ..self.config.clone()
        })
    }

    pub(crate) fn with_bc_textures_disabled(&self) -> Self {
        self.variant(ResolverConfig {
            bc_textures: BcTextures::Never,
            ..self.config.clone()
        })
    }

    #[cfg(test)]
    fn with_bc_texture_support(&self, supported: bool) -> Self {
        self.variant(ResolverConfig {
            bc_textures: match self.config.bc_textures {
                BcTextures::Never => BcTextures::Never,
                BcTextures::RetainAuthored | BcTextures::Forced(_) => BcTextures::Forced(supported),
            },
            ..self.config.clone()
        })
    }

    #[cfg(test)]
    pub(crate) fn resolve(
        &self,
        material_dirs: &[String],
        material_name: &str,
    ) -> Option<Arc<ResolvedTexture>> {
        let material_paths = material_paths(material_dirs, material_name);
        let definition = self.material_definition(&material_paths)?;
        self.resolve_primary_material(&definition.material)
            .map(|material| Arc::clone(&material.texture))
    }

    pub(crate) fn resolve_primary(
        &self,
        material_dirs: &[String],
        material_name: &str,
    ) -> MaterialResolution<ResolvedPrimaryMaterial> {
        self.resolve_named_material(
            material_dirs,
            material_name,
            |material| self.resolve_primary_material(material),
            canonical_error_primary,
        )
    }

    pub(crate) fn resolve_with_base2(
        &self,
        material_dirs: &[String],
        material_name: &str,
    ) -> MaterialResolution<ResolvedMaterialTextures> {
        self.resolve_named_material(
            material_dirs,
            material_name,
            |material| self.resolve_material(material),
            || {
                let primary = canonical_error_primary();
                ResolvedMaterialTextures {
                    shader: primary.shader,
                    texture: Some(primary.texture),
                    texture2: None,
                    render_mode: primary.render_mode,
                }
            },
        )
    }

    fn resolve_named_material<T>(
        &self,
        material_dirs: &[String],
        material_name: &str,
        resolve: impl FnOnce(&EffectiveMaterial) -> Option<T>,
        missing: impl FnOnce() -> T,
    ) -> MaterialResolution<T> {
        let paths = material_paths(material_dirs, material_name);
        self.material_definition(&paths).map_or_else(
            || match self.material_source_failure(&paths) {
                Some(definition::MaterialSourceFailure::Missing) => MaterialResolution::Missing {
                    fallback: missing(),
                },
                Some(definition::MaterialSourceFailure::SourceUnavailable(error)) => {
                    MaterialResolution::SourceUnavailable { error }
                }
                Some(definition::MaterialSourceFailure::IndexUnavailable(error)) => {
                    MaterialResolution::IndexUnavailable { error }
                }
                Some(definition::MaterialSourceFailure::Malformed(path)) => {
                    MaterialResolution::Malformed { path }
                }
                None => MaterialResolution::Unresolved {
                    canonical_name: material_name.to_owned(),
                },
            },
            |definition| {
                let canonical_name = definition.canonical_name();
                match resolve(&definition.material) {
                    Some(material) => MaterialResolution::Loaded {
                        canonical_name,
                        material,
                    },
                    None => MaterialResolution::Unresolved { canonical_name },
                }
            },
        )
    }

    pub(crate) fn entry_bytes(&self, path: &str) -> Option<Vec<u8>> {
        self.check_cancelled().ok()?;
        self.entry_bytes_from_sources(&ContentPath::new(path)?)
    }

    pub(crate) fn sound_reference_key(reference: &str) -> Option<String> {
        let key = reference.trim().to_ascii_lowercase();
        (!key.is_empty()).then_some(key)
    }

    pub(crate) fn resolve_sound_reference(
        &self,
        reference: &str,
    ) -> Option<ResolvedSoundReference> {
        self.check_cancelled().ok()?;
        let key = Self::sound_reference_key(reference)?;
        {
            let cache = self.shared.resolved_sound_cache.lock();
            if let Some(cached) = cache.get(&key) {
                return cached.clone();
            }
        }

        let resolved = self.resolve_sound_reference_uncached(reference);
        self.check_cancelled().ok()?;
        self.shared
            .resolved_sound_cache
            .lock()
            .insert(key, resolved.clone());
        resolved
    }

    #[cfg(test)]
    pub(crate) fn sound_script_files(&self) -> Vec<String> {
        self.sound_scripts()
            .map_or_else(Vec::new, |library| library.script_files.clone())
    }

    fn resolve_material(&self, material: &EffectiveMaterial) -> Option<ResolvedMaterialTextures> {
        let render_mode = material.render_mode();
        let texture = material
            .effective_base_texture()
            .and_then(|base_texture| self.resolve_material_texture(material, base_texture))
            .or_else(|| self.mirror_fallback_texture(material))
            .or_else(|| self.water_fallback_texture(material))
            .or_else(|| self.envmap_fallback_texture(material));
        let has_base_texture = texture
            .as_ref()
            .is_some_and(|texture| texture.has_effective_base_texture());
        let accepts_secondary_base = shader::surface_family(&material.shader)
            .is_some_and(SurfaceFamily::accepts_secondary_base)
            && material.effective_base_texture().is_some()
            && has_base_texture;
        let texture2 = accepts_secondary_base
            .then(|| {
                material
                    .parameter("$basetexture2")
                    .and_then(|base_texture| self.resolve_material_texture(material, base_texture))
            })
            .flatten();

        let shader = self.resolve_shader(
            material,
            has_base_texture,
            texture2
                .as_ref()
                .is_some_and(|texture| texture.has_effective_base_texture()),
        );
        let render_mode = shader.surface().map_or(render_mode, |surface| {
            surface.draw_render_mode(
                texture.as_ref().and_then(|texture| texture.flags()),
                surface.alpha,
            )
        });
        (texture.is_some() || texture2.is_some() || shader.surface().is_some()).then_some(
            ResolvedMaterialTextures {
                shader,
                texture,
                texture2,
                render_mode,
            },
        )
    }

    fn resolve_primary_material(
        &self,
        material: &EffectiveMaterial,
    ) -> Option<ResolvedPrimaryMaterial> {
        let resolved = self.resolve_material(material)?;
        Some(ResolvedPrimaryMaterial {
            shader: resolved.shader,
            texture: resolved.texture?,
            texture2: resolved.texture2,
            render_mode: resolved.render_mode,
        })
    }

    fn resolve_texture(&self, base_texture: &str) -> Option<Arc<ResolvedTexture>> {
        self.resolve_texture_with_policy(base_texture, TextureFailurePolicy::Unverified)
    }

    fn resolve_material_texture(
        &self,
        material: &EffectiveMaterial,
        name: &str,
    ) -> Option<Arc<ResolvedTexture>> {
        self.resolve_texture_with_policy(name, texture_failure_policy(material))
    }

    fn resolve_texture_with_policy(
        &self,
        base_texture: &str,
        policy: TextureFailurePolicy,
    ) -> Option<Arc<ResolvedTexture>> {
        let path = texture_path(base_texture)?;
        let selected = match self.selected_texture_asset(&path) {
            Ok(selected) => selected,
            Err(TextureSourceError::Missing) => {
                return (policy == TextureFailurePolicy::NativeErrorTexture).then(|| {
                    Arc::new(ResolvedTexture::unavailable(
                        None,
                        TextureLoadOutcome::MissingSource,
                    ))
                });
            }
            Err(TextureSourceError::SourceUnavailable(_)) => {
                return Some(Arc::new(ResolvedTexture::unavailable(
                    None,
                    TextureLoadOutcome::SourceUnavailable,
                )));
            }
            Err(TextureSourceError::Preparation { .. }) => {
                return Some(Arc::new(ResolvedTexture::unavailable(
                    None,
                    TextureLoadOutcome::Unplanned,
                )));
            }
            Err(
                TextureSourceError::EncodedBudget { metadata, .. }
                | TextureSourceError::WorkingBudget { metadata, .. },
            ) => {
                return Some(Arc::new(ResolvedTexture::unavailable(
                    metadata,
                    TextureLoadOutcome::SourceBudgetRejected,
                )));
            }
            Err(TextureSourceError::Invalid {
                error: TextureAssetError::Vtf(error),
                ..
            }) if policy == TextureFailurePolicy::NativeErrorTexture
                && native_invalid_texture(&error) =>
            {
                log::warn!("texture {path}: {error}");
                return Some(Arc::new(ResolvedTexture::unavailable(
                    None,
                    TextureLoadOutcome::InvalidSource,
                )));
            }
            Err(error) => {
                log::warn!("texture {path}: {error}");
                return None;
            }
        };
        log::debug!("texture {path}: selected {:?}", selected.origin);
        let source_metadata = selected.asset.metadata_arc();
        let outcome = self.resident_texture(&selected, 0..1, !self.bc_textures_enabled());
        let texture = match outcome {
            Ok(resident) => ResolvedTexture::from_resident(resident, source_metadata),
            Err(error) => {
                log::warn!("texture {path}: {error}");
                if matches!(&error,TextureAssetError::Vtf(error) if native_invalid_texture(error))
                    && policy != TextureFailurePolicy::NativeErrorTexture
                {
                    return None;
                }
                let outcome = match &error {
                    TextureAssetError::Vtf(error) if native_invalid_texture(error) => {
                        TextureLoadOutcome::InvalidPayload
                    }
                    TextureAssetError::DimensionLimit(_)
                    | TextureAssetError::Budget
                    | TextureAssetError::ResidentImageBudget { .. }
                    | TextureAssetError::WorkingLimit { .. }
                    | TextureAssetError::Vtf(vformats::vtf::VtfError::DecodedTooLarge { .. })
                    | TextureAssetError::ImageCount(_) => TextureLoadOutcome::QualityRejected,
                    TextureAssetError::UnplannedRequest | TextureAssetError::ClosedPlan => {
                        TextureLoadOutcome::Unplanned
                    }
                    _ => TextureLoadOutcome::Unsupported,
                };
                ResolvedTexture::unavailable(Some(source_metadata), outcome)
            }
        };
        Some(Arc::new(texture))
    }

    fn envmap_fallback_texture(
        &self,
        material: &EffectiveMaterial,
    ) -> Option<Arc<ResolvedTexture>> {
        if material.parameter("$basetexture").is_some() {
            return None;
        }
        let env_map = material
            .parameter("$envmap")
            .and_then(normalize_texture_name)?;
        if env_map != "env_cubemap"
            && let Some(texture) = self.resolve_texture(&env_map)
        {
            return Some(Arc::new((*texture).clone().unmapped()));
        }
        Some(Arc::new(
            ResolvedTexture::rgba(ENVMAP_FALLBACK_RGBA.to_vec(), 1, 1, 1, 1, false).unmapped(),
        ))
    }

    fn mirror_fallback_texture(
        &self,
        material: &EffectiveMaterial,
    ) -> Option<Arc<ResolvedTexture>> {
        mirror::is_mirror_shader(&material.shader)
            .then(|| Arc::new(ResolvedTexture::rgba(vec![255; 4], 1, 1, 1, 1, false).unmapped()))
    }

    fn water_fallback_texture(&self, material: &EffectiveMaterial) -> Option<Arc<ResolvedTexture>> {
        is_water_shader(&material.shader).then(|| {
            Arc::new(ResolvedTexture::rgba(
                water_fog_rgba(material.parameter("$fogcolor")).to_vec(),
                1,
                1,
                1,
                1,
                true,
            ))
        })
    }

    fn bc_textures_enabled(&self) -> bool {
        match self.config.bc_textures {
            BcTextures::RetainAuthored => true,
            BcTextures::Never => false,
            #[cfg(test)]
            BcTextures::Forced(supported) => supported,
        }
    }

    pub(crate) fn archive_index_stats(&self) -> ArchiveIndexStats {
        self.shared.index_budget.stats()
    }

    fn game_vpks(&self) -> Result<&[VpkArchive], crate::preparation::Cancelled> {
        self.shared
            .game_vpks
            .get_or_try_init_ref(&self.config.cancellation, || {
                let mut archives = Vec::new();
                let mut reservations = Vec::new();
                for path in &self.shared.game_vpk_paths {
                    self.check_cancelled()?;
                    let result = (|| {
                        let source = gmpublished_backend::vpk::VpkIndexSource::open(
                            path,
                            self.shared.index_budget.limits(),
                        )?;
                        let _working = self
                            .shared
                            .index_budget
                            .working_request(source.prefix_bytes())
                            .map_err(VpkError::IndexLimit)?;
                        Ok::<_, VpkError>((source, _working))
                    })();
                    let (source, request) = match result {
                        Ok(selected) => selected,
                        Err(error @ VpkError::IndexLimit(_)) => {
                            if let VpkError::IndexLimit(limit) = &error {
                                self.shared.index_budget.reject(*limit);
                            }
                            archives.push(VpkArchive::unavailable(error));
                            break;
                        }
                        Err(error) => {
                            log::debug!("game VPK open failed for {}: {error}", path.display());
                            continue;
                        }
                    };
                    let _working = request.acquire_cancellable(&self.config.cancellation)?;
                    let result = (|| {
                        let prepared = source.read()?;
                        let mut cost = prepared.index_cost()?;
                        cost.record_bytes = cost
                            .record_bytes
                            .checked_add(
                                (size_of::<VpkArchive>()
                                    + size_of::<source_index::IndexReservation>())
                                    as u64,
                            )
                            .ok_or_else(|| {
                                VpkError::IndexLimit(
                                    vformats::ArchiveIndexLimit::arithmetic_overflow(),
                                )
                            })?;
                        cost.string_bytes = cost
                            .string_bytes
                            .checked_add(path.as_os_str().len() as u64)
                            .ok_or_else(|| {
                                VpkError::IndexLimit(
                                    vformats::ArchiveIndexLimit::arithmetic_overflow(),
                                )
                            })?;
                        let reservation = self
                            .shared
                            .index_budget
                            .reserve(cost)
                            .map_err(VpkError::IndexLimit)?;
                        let archive = VpkArchive::from_file(prepared.materialize()?);
                        reservations.push(reservation);
                        Ok(archive)
                    })();
                    match result {
                        Ok(archive) => archives.push(archive),
                        Err(error @ VpkError::IndexLimit(_)) => {
                            if let VpkError::IndexLimit(limit) = &error {
                                self.shared.index_budget.reject(*limit);
                            }
                            archives.push(VpkArchive::unavailable(error));
                            break;
                        }
                        Err(error) => {
                            log::debug!("game VPK open failed for {}: {error}", path.display());
                        }
                    }
                }
                Ok(IndexedVpks {
                    archives,
                    _reservations: reservations,
                })
            })
            .map(|indexed| indexed.archives.as_slice())
    }

    fn sibling_gmas(&self) -> Result<&SiblingGmaIndex, crate::preparation::Cancelled> {
        self.shared
            .sibling_gmas
            .get_or_try_init_ref(&self.config.cancellation, || {
                sibling_gma::build_sibling_gma_index_cancellable(
                    &self.shared.sibling_gma_paths,
                    &self.shared.index_budget,
                    &self.config.cancellation,
                )
            })
    }

    fn resolve_sound_reference_uncached(&self, reference: &str) -> Option<ResolvedSoundReference> {
        if soundscript::is_raw_wave_reference(reference) {
            let wave = self.resolve_sound_wave(reference)?;
            return Some(ResolvedSoundReference {
                reference: reference.trim().to_owned(),
                sound_level: soundscript::DEFAULT_SOUND_LEVEL_DB,
                volume: 1.0,
                waves: vec![wave],
            });
        }

        let key = Self::sound_reference_key(reference)?;
        let Some(script) = self.sound_scripts()?.scripts.get(&key) else {
            log::debug!("soundscript {reference:?} unresolved");
            return None;
        };
        let mut waves = Vec::new();
        for wave in &script.waves {
            if let Some(resolved) = self.resolve_sound_wave(wave) {
                waves.push(resolved);
            } else {
                log::debug!("soundscript {reference:?} wave {wave:?} unresolved");
            }
        }
        if waves.is_empty() {
            log::debug!("soundscript {reference:?} has no resolvable waves");
            return None;
        }
        Some(ResolvedSoundReference {
            reference: reference.trim().to_owned(),
            sound_level: soundscript::parse_sound_level_db(script.sound_level.as_deref()),
            volume: soundscript::parse_volume(script.volume.as_deref()),
            waves,
        })
    }

    fn resolve_sound_wave(&self, wave: &str) -> Option<ResolvedSoundWave> {
        let path = soundscript::sound_wave_archive_path(wave)?;
        if path == "sound/steinman/null.wav" {
            log::debug!("sound wave {wave:?} treated as silent null.wav");
            return None;
        }
        let resolved = self
            .find_content_bytes(std::slice::from_ref(&ContentPath::new(&path)?))
            .ok()??;
        Some(ResolvedSoundWave {
            path: resolved.path,
            source_tier: resolved.tier,
            bytes: Arc::from(resolved.bytes),
        })
    }

    fn sound_scripts(&self) -> Option<&SoundScriptLibrary> {
        self.shared
            .sound_scripts
            .get_or_try_init_ref(&self.config.cancellation, || {
                Ok(self.load_sound_script_library())
            })
            .ok()
    }

    fn load_sound_script_library(&self) -> SoundScriptLibrary {
        let mut script_files = Vec::new();
        let mut seen_files = HashSet::<String>::new();
        for manifest in self.content_bytes_from_all_sources("scripts/game_sounds_manifest.txt") {
            let text = String::from_utf8_lossy(&manifest.bytes);
            let files =
                soundscript::parse_manifest_files(&text, &Limits::default()).unwrap_or_default();
            for file in files {
                let Some(path) = soundscript::normalize_script_path(&file) else {
                    log::debug!("soundscript manifest path rejected: {file:?}");
                    continue;
                };
                if seen_files.insert(path.clone()) {
                    script_files.push(path);
                }
            }
        }
        for path in self.source_paths_matching(|path| {
            path.starts_with("scripts/game_sounds_")
                && Path::new(path)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
        }) {
            if seen_files.insert(path.clone()) {
                script_files.push(path);
            }
        }

        let mut scripts = HashMap::new();
        for script_file in &script_files {
            for content in self.content_bytes_from_all_sources(script_file) {
                let text = String::from_utf8_lossy(&content.bytes);
                let parsed =
                    soundscript::parse_sound_scripts(&text, &Limits::default()).unwrap_or_default();
                for (name, script) in parsed {
                    scripts
                        .entry(name)
                        .or_insert_with(|| StoredSoundScript::from(&script));
                }
            }
        }
        log::debug!(
            "soundscripts loaded: files={} entries={}",
            script_files.len(),
            scripts.len()
        );
        SoundScriptLibrary {
            scripts,
            #[cfg(test)]
            script_files,
        }
    }

    fn entry_bytes_from_sources(&self, path: &ContentPath) -> Option<Vec<u8>> {
        self.find_entry_by_source(std::slice::from_ref(path), |_, bytes| Some(bytes))
    }

    fn addon_index_error(&self) -> Option<vformats::ArchiveIndexLimit> {
        *self.shared.addon_index_error.get_or_init(|| {
            match self
                .shared
                .addon
                .index_cost_with_limits(self.shared.index_budget.limits())
            {
                Ok(Some(cost)) => match self.shared.index_budget.reserve(cost) {
                    Ok(reservation) => {
                        reservation.commit();
                        None
                    }
                    Err(error) => Some(self.shared.index_budget.reject(error)),
                },
                Ok(None) => None,
                Err(crate::bridge::gma::GmaError::IndexLimit(error)) => {
                    Some(self.shared.index_budget.reject(error))
                }
                Err(error) => {
                    log::debug!("primary GMA index unavailable: {error}");
                    None
                }
            }
        })
    }

    pub(crate) fn source_discovery_stats(&self) -> &SourceDiscoveryStats {
        &self.shared.discovery_stats
    }
    fn discovery_barrier(&self, tier: ContentSourceTier) -> Option<SourceRef<'_>> {
        self.shared
            .discovery_failures
            .get(&tier)
            .map(|error| SourceRef::SourceUnavailable(tier, error))
    }

    fn sources(&self) -> impl Iterator<Item = SourceRef<'_>> {
        self.config
            .pakfile
            .as_deref()
            .map(SourceRef::Pakfile)
            .into_iter()
            .chain(std::iter::once_with(|| {
                self.addon_index_error()
                    .map_or(SourceRef::Addon(&self.shared.addon), |error| {
                        SourceRef::IndexUnavailable(ContentSourceTier::Addon, error)
                    })
            }))
            .chain(self.discovery_barrier(ContentSourceTier::Loose))
            .chain(self.shared.loose_source_dirs.iter().map(SourceRef::Loose))
            .chain(self.discovery_barrier(ContentSourceTier::SiblingGma))
            .chain(
                std::iter::once_with(|| self.sibling_gmas().ok().map(SourceRef::SiblingGma))
                    .flatten(),
            )
            .chain(self.discovery_barrier(ContentSourceTier::GameVpk))
            .chain(
                std::iter::once_with(|| {
                    self.game_vpks()
                        .unwrap_or_default()
                        .iter()
                        .map(SourceRef::GameVpk)
                })
                .flatten(),
            )
    }

    fn tier_bytes(source: &SourceRef<'_>, path: &ContentPath) -> Option<Vec<u8>> {
        match source.entry_bytes(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                log::warn!(
                    "content source {:?} failed to read {path}: {error}",
                    source.tier()
                );
                None
            }
        }
    }

    fn find_content_bytes(
        &self,
        paths: &[ContentPath],
    ) -> Result<Option<ResolvedContentBytes>, ContentSourceFailure> {
        for source in self.sources() {
            if self.check_cancelled().is_err() {
                return Ok(None);
            }
            for path in paths {
                if self.check_cancelled().is_err() {
                    return Ok(None);
                }
                if let Some(error) = source.index_error(Some(path)) {
                    return Err(ContentSourceFailure::Index(error));
                }
                if let Some(error) = source.discovery_error() {
                    return Err(ContentSourceFailure::Discovery(error.clone()));
                }
                if let Some(bytes) = Self::tier_bytes(&source, path) {
                    return Ok(Some(ResolvedContentBytes {
                        path: path.as_str().to_owned(),
                        tier: source.tier(),
                        bytes,
                    }));
                }
            }
        }
        Ok(None)
    }

    fn content_bytes_from_all_sources(&self, path: &str) -> Vec<ResolvedContentBytes> {
        let Some(path) = ContentPath::new(path) else {
            return Vec::new();
        };
        let mut contents = Vec::new();
        for source in self.sources() {
            if self.check_cancelled().is_err() {
                break;
            }
            if source.index_error(Some(&path)).is_some() || source.discovery_error().is_some() {
                break;
            }
            if let Some(bytes) = Self::tier_bytes(&source, &path) {
                contents.push(ResolvedContentBytes {
                    path: path.as_str().to_owned(),
                    tier: source.tier(),
                    bytes,
                });
            }
        }
        contents
    }

    fn source_paths_matching(&self, matches: impl Fn(&str) -> bool) -> Vec<String> {
        let mut paths = Vec::new();
        let mut seen = BTreeSet::new();
        for source in self.sources() {
            if self.check_cancelled().is_err() {
                break;
            }
            source.for_each_path(&mut |path| {
                push_matching_path(path, &matches, &mut seen, &mut paths);
            });
            if source.index_error(None).is_some() || source.discovery_error().is_some() {
                break;
            }
        }
        paths
    }

    fn find_entry_by_source<T>(
        &self,
        paths: &[ContentPath],
        mut consume: impl FnMut(&ContentPath, Vec<u8>) -> Option<T>,
    ) -> Option<T> {
        for source in self.sources() {
            for path in paths {
                if source.index_error(Some(path)).is_some() || source.discovery_error().is_some() {
                    return None;
                }
                if let Some(value) =
                    Self::tier_bytes(&source, path).and_then(|bytes| consume(path, bytes))
                {
                    return Some(value);
                }
            }
        }
        None
    }
}

#[derive(Debug)]
enum ContentSourceFailure {
    Index(vformats::ArchiveIndexLimit),
    Discovery(SourceDiscoveryFailure),
}

#[derive(Debug, thiserror::Error)]
enum SourceError {
    #[error(transparent)]
    IndexLimit(vformats::ArchiveIndexLimit),
    #[error(transparent)]
    Discovery(SourceDiscoveryFailure),
    #[error("BSP pakfile: {0}")]
    Pakfile(String),
    #[error(transparent)]
    Addon(#[from] PreviewArchiveSourceError),
    #[error("loose file {path}: {message}")]
    Loose { path: String, message: String },
    #[error("sibling GMA: {0}")]
    SiblingGma(String),
    #[error(transparent)]
    GameVpk(#[from] VpkError),
}

enum SourceRef<'a> {
    SourceUnavailable(ContentSourceTier, &'a SourceDiscoveryFailure),
    IndexUnavailable(ContentSourceTier, vformats::ArchiveIndexLimit),
    Pakfile(&'a PakSource),
    Addon(&'a PreviewArchiveSource),
    Loose(&'a LooseSourceDir),
    SiblingGma(&'a SiblingGmaIndex),
    GameVpk(&'a VpkArchive),
}

impl SourceRef<'_> {
    fn discovery_error(&self) -> Option<&SourceDiscoveryFailure> {
        match self {
            Self::SourceUnavailable(_, error) => Some(error),
            _ => None,
        }
    }

    fn index_error(&self, path: Option<&ContentPath>) -> Option<vformats::ArchiveIndexLimit> {
        match self {
            Self::IndexUnavailable(_, error) => Some(*error),
            Self::Pakfile(source) => source.index_error,
            Self::GameVpk(source) => match source.index_error() {
                Some(VpkError::IndexLimit(error)) => Some(*error),
                _ => None,
            },
            Self::SiblingGma(source) => source.index_error(path),
            Self::Addon(_) | Self::Loose(_) | Self::SourceUnavailable(_, _) => None,
        }
    }

    fn tier(&self) -> ContentSourceTier {
        match self {
            Self::IndexUnavailable(tier, _) | Self::SourceUnavailable(tier, _) => *tier,
            Self::Pakfile(_) => ContentSourceTier::Pakfile,
            Self::Addon(_) => ContentSourceTier::Addon,
            Self::Loose(_) => ContentSourceTier::Loose,
            Self::SiblingGma(_) => ContentSourceTier::SiblingGma,
            Self::GameVpk(_) => ContentSourceTier::GameVpk,
        }
    }

    fn entry_bytes(&self, path: &ContentPath) -> Result<Option<Vec<u8>>, SourceError> {
        if let Some(error) = self.index_error(Some(path)) {
            return Err(SourceError::IndexLimit(error));
        }
        match self {
            Self::SourceUnavailable(_, error) => Err(SourceError::Discovery((*error).clone())),
            Self::IndexUnavailable(_, error) => Err(SourceError::IndexLimit(*error)),
            Self::Pakfile(pakfile) => pakfile.entry_bytes(path),
            Self::Addon(addon) => match addon.entry_bytes(path.as_str()) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(
                    PreviewArchiveSourceError::EntryNotFound(_)
                    | PreviewArchiveSourceError::Gma(crate::bridge::gma::GmaError::EntryNotFound),
                ) => Ok(None),
                Err(error) => Err(SourceError::Addon(error)),
            },
            Self::Loose(loose_dir) => loose_dir.entry_bytes(path),
            Self::SiblingGma(sibling_gmas) => sibling_gmas.entry_bytes(path),
            Self::GameVpk(vpk) => match vpk.entry_bytes(path.as_str()) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(VpkError::EntryNotFound) => Ok(None),
                Err(error) => Err(SourceError::GameVpk(error)),
            },
        }
    }

    fn for_each_path(&self, visit: &mut dyn FnMut(&str)) {
        match self {
            Self::IndexUnavailable(_, _) | Self::SourceUnavailable(_, _) => {}
            Self::Pakfile(pakfile) => pakfile.for_each_path(visit),
            Self::Addon(addon) => addon.for_each_path(visit),
            Self::Loose(loose_dir) => loose_dir.paths().iter().for_each(|path| visit(path)),
            Self::SiblingGma(sibling_gmas) => sibling_gmas.for_each_path(visit),
            Self::GameVpk(vpk) => vpk.paths().iter().for_each(|path| visit(path)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EffectiveMaterial {
    profile: profile::HardwareProfile,
    source: vformats::keyvalues::KvDocument<'static>,
    shader: String,
    parameters: BTreeMap<String, String>,
    proxies: Vec<parameters::MaterialProxy>,
}

impl EffectiveMaterial {
    #[cfg(test)]
    fn from_document(document: &vformats::vmt::VmtDocument<'_>) -> Self {
        Self::with_profile(document, profile::HardwareProfile::LDR)
    }

    fn with_profile(
        document: &vformats::vmt::VmtDocument<'_>,
        profile: profile::HardwareProfile,
    ) -> Self {
        let source = profile::owned(&document.kv);
        let (parameters, proxies) = profile::resolve(&source, &document.shader, profile);
        Self {
            profile,
            source,
            shader: document.shader.to_string(),
            parameters,
            proxies,
        }
    }

    fn parameter(&self, key: &str) -> Option<&str> {
        self.parameters.get(key).map(String::as_str)
    }

    fn effective_base_texture(&self) -> Option<&str> {
        let base = self.parameter("$basetexture")?;
        let family = shader::surface_family(&self.shader);
        let diffuse_bump = match family {
            Some(SurfaceFamily::VertexLit) => true,
            Some(SurfaceFamily::Lightmapped | SurfaceFamily::Transition) => {
                !vmt_bool(self.parameter("$nodiffusebumplighting"))
            }
            _ => false,
        };
        if self.effective_bumpmap().is_some() && diffuse_bump {
            return self.parameter("$albedo").or(Some(base));
        }
        Some(base)
    }

    fn effective_bumpmap(&self) -> Option<&str> {
        let family = shader::surface_family(&self.shader)?;
        family
            .accepts_primary_bumpmap(vmt_bool(self.parameter("$decal")))
            .then(|| self.parameter("$bumpmap"))
            .flatten()
    }

    fn effective_bumpmap2(&self) -> Option<&str> {
        shader::surface_family(&self.shader)?
            .accepts_secondary_bumpmap(
                vmt_bool(self.parameter("$decal")),
                self.parameter("$bumpmap").is_some(),
            )
            .then(|| self.parameter("$bumpmap2"))
            .flatten()
    }

    fn effective_envmap_mask(&self) -> Option<&str> {
        shader::surface_family(&self.shader)?
            .accepts_envmap_mask(
                vmt_bool(self.parameter("$decal")),
                self.parameter("$bumpmap").is_some(),
            )
            .then(|| self.parameter("$envmapmask"))
            .flatten()
    }

    fn render_mode(&self) -> RenderMode {
        if is_water_shader(&self.shader) {
            if vmt_bool(self.parameter("$alphatest")) {
                RenderMode::Cutout
            } else {
                RenderMode::Opaque
            }
        } else if vmt_bool(self.parameter("$additive")) {
            RenderMode::Additive
        } else if vmt_bool(self.parameter("$translucent"))
            || vmt_bool(self.parameter("$vertexalpha"))
            || parameters::scalar(self.parameter("$alpha"), 1.0) < 1.0
        {
            RenderMode::Translucent
        } else if vmt_bool(self.parameter("$alphatest")) {
            RenderMode::Cutout
        } else {
            RenderMode::Opaque
        }
    }

    fn apply_patch_values(
        &mut self,
        document: &vformats::vmt::VmtDocument<'_>,
        _patch: &vformats::vmt::VmtPatch<'_>,
    ) {
        profile::merge(&mut self.source, &document.kv, false);
        for (name, insert) in [("insert", true), ("replace", false)] {
            for block in document.kv.blocks(name) {
                profile::merge(&mut self.source, block, insert);
            }
        }
        (self.parameters, self.proxies) =
            profile::resolve(&self.source, &self.shader, self.profile);
    }
}

fn vmt_bool(value: Option<&str>) -> bool {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    value.eq_ignore_ascii_case("true")
        || value
            .parse::<f32>()
            .is_ok_and(|number| number.is_finite() && number != 0.0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextureFailurePolicy {
    NativeErrorTexture,
    Unverified,
}

fn texture_failure_policy(material: &EffectiveMaterial) -> TextureFailurePolicy {
    if matches!(
        shader::surface_family(&material.shader),
        Some(SurfaceFamily::Lightmapped | SurfaceFamily::Unlit | SurfaceFamily::VertexLit)
    ) {
        TextureFailurePolicy::NativeErrorTexture
    } else {
        TextureFailurePolicy::Unverified
    }
}

fn native_invalid_texture(error: &vformats::vtf::VtfError) -> bool {
    matches!(
        error,
        vformats::vtf::VtfError::BadMagic | vformats::vtf::VtfError::Truncated { .. }
    )
}

fn is_water_shader(shader: &str) -> bool {
    strip_prefix_ascii_case(shader, "water").is_some()
}

fn water_fog_rgba(value: Option<&str>) -> [u8; 4] {
    let linear = value
        .and_then(parse_material_color)
        .unwrap_or(DEFAULT_WATER_FOG_LINEAR);
    [
        linear_to_srgb_byte(linear[0]),
        linear_to_srgb_byte(linear[1]),
        linear_to_srgb_byte(linear[2]),
        255,
    ]
}

fn linear_to_srgb_byte(linear: f32) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let srgb = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round().clamp(0.0, 255.0) as u8
}

pub fn srgb_byte_to_linear(byte: u8) -> f32 {
    let srgb = f32::from(byte) / 255.0;
    if srgb <= 0.040_45 {
        srgb / 12.92
    } else {
        ((srgb + 0.055) / 1.055).powf(2.4)
    }
}

#[derive(Debug)]
struct PakSource {
    pakfile: MapPakFile,
    entries: HashMap<String, (usize, u64)>,
    index_error: Option<vformats::ArchiveIndexLimit>,
}

impl PakSource {
    #[cfg(test)]
    fn new(pakfile: MapPakFile) -> Option<Self> {
        Self::new_with_budget(pakfile, &SourceIndexBudget::defaults())
    }

    fn new_with_budget(pakfile: MapPakFile, budget: &Arc<SourceIndexBudget>) -> Option<Self> {
        let result = (|| {
            let mut cost = pakfile.index_cost()?;
            let overflow = || {
                vformats::bsp::ZipError::IndexLimit(
                    vformats::ArchiveIndexLimit::arithmetic_overflow(),
                )
            };
            let per_entry = size_of::<(String, (usize, u64))>()
                + size_of::<gmpublished_domain::scene::map::MapPakFileEntry>();
            cost.record_bytes = cost
                .record_bytes
                .checked_add(
                    (cost.entries as u64)
                        .checked_mul(per_entry as u64)
                        .ok_or_else(overflow)?,
                )
                .and_then(|bytes| bytes.checked_add(size_of::<Self>() as u64))
                .ok_or_else(overflow)?;
            cost.string_bytes = cost.string_bytes.checked_mul(2).ok_or_else(overflow)?;
            let reservation = budget
                .reserve(cost)
                .map_err(vformats::bsp::ZipError::IndexLimit)?;
            let entries = pakfile
                .indexed_entries_with_limits(budget.limits())?
                .into_iter()
                .map(|entry| (entry.path, (entry.index, entry.size)))
                .collect();
            reservation.commit();
            Ok(entries)
        })();
        match result {
            Ok(entries) => Some(Self {
                pakfile,
                entries,
                index_error: None,
            }),
            Err(vformats::bsp::ZipError::IndexLimit(error)) => Some(Self {
                pakfile,
                entries: HashMap::new(),
                index_error: Some(budget.reject(error)),
            }),
            Err(error) => {
                log::debug!("BSP pakfile source disabled: {error}");
                None
            }
        }
    }

    fn for_each_path(&self, visit: &mut dyn FnMut(&str)) {
        self.entries.keys().for_each(|path| visit(path));
    }

    fn entry_bytes(&self, path: &ContentPath) -> Result<Option<Vec<u8>>, SourceError> {
        let Some((index, _)) = self.entries.get(path.as_str()).copied() else {
            return Ok(None);
        };
        self.pakfile
            .entry_bytes_by_index(index)
            .map_err(|error| SourceError::Pakfile(error.to_string()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LooseSourceDir {
    root: PathBuf,
}

impl LooseSourceDir {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn entry_bytes(&self, path: &ContentPath) -> Result<Option<Vec<u8>>, SourceError> {
        let mut candidate = self.root.clone();
        for segment in path.as_str().split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Ok(None);
            }
            candidate.push(segment);
        }
        match fs::read(&candidate) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(SourceError::Loose {
                path: candidate.display().to_string(),
                message: error.to_string(),
            }),
        }
    }

    fn paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        let scripts = self.root.join("scripts");
        collect_loose_script_paths(&scripts, "scripts", 0, &mut paths);
        paths
    }
}

fn collect_loose_script_paths(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<String>) {
    if depth > 2 || out.len() >= 4096 {
        return;
    }
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.filter_map(Result::ok) {
        if out.len() >= 4096 {
            break;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let child = format!("{prefix}/{name}");
        if file_type.is_dir() {
            collect_loose_script_paths(&path, &child, depth + 1, out);
        } else if file_type.is_file()
            && let Some(normalized) = normalize_archive_path(&child)
        {
            out.push(normalized);
        }
    }
}

#[cfg(test)]
mod tests;
