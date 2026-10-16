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

#[cfg(test)]
use sibling_gma::SiblingGmaPathKind;
use sibling_gma::{SiblingGmaIndex, SiblingGmaPath, build_sibling_gma_index};

#[cfg(test)]
use discovery::normalize_source_path;
use discovery::{
    discover_loose_source_dirs, discover_mounted_game_dirs, discover_sibling_gma_paths,
    material_paths, normalize_texture_name, strip_prefix_ascii_case, texture_path,
};

use parking_lot::Mutex;

use crate::bridge::archive::{PreviewArchiveSource, PreviewArchiveSourceError};
use crate::bridge::content_path::{ContentPath, normalize_archive_path};
use crate::bridge::gma::PreviewArchive;
use crate::bridge::vpk::{VpkArchive, VpkError};
use gmpublished_domain::scene::map::MapPakFile;
use vformats::vtf::BcFormat;
use vformats::{Limits, soundscript};

const PATCH_INCLUDE_LIMIT: usize = 4;
const MAX_SIBLING_GMA_ARCHIVES: usize = 2048;
const MAX_LEGACY_BIN_ENTRY_TABLE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LEGACY_BIN_FETCH_BYTES: u64 = 1024 * 1024 * 1024;
pub const DEFAULT_WATER_FOG_LINEAR: Vec3 = Vec3::new(0.03, 0.10, 0.10);
const GMA_MAGIC: &[u8; 4] = b"GMAD";
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
    Unsupported,
    Deferred,
    Unplanned,
}

impl TextureLoadOutcome {
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
    pub(crate) fn width(&self) -> u32 {
        self.dimensions()[0]
    }
    pub(crate) fn height(&self) -> u32 {
        self.dimensions()[1]
    }
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
    #[default]
    Unresolved,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MaterialResolution<T> {
    Loaded { canonical_name: String, material: T },
    Missing { fallback: T },
    Malformed { path: ContentPath },
    Unresolved { canonical_name: String },
}

impl<T> MaterialResolution<T> {
    pub(crate) fn material(&self) -> Option<&T> {
        match self {
            Self::Loaded { material, .. } => Some(material),
            Self::Missing { fallback } => Some(fallback),
            Self::Malformed { .. } | Self::Unresolved { .. } => None,
        }
    }

    pub(crate) fn into_material(self) -> Option<T> {
        match self {
            Self::Loaded { material, .. } => Some(material),
            Self::Missing { fallback } => Some(fallback),
            Self::Malformed { .. } | Self::Unresolved { .. } => None,
        }
    }

    pub(crate) fn outcome(&self) -> MaterialResolutionOutcome {
        match self {
            Self::Loaded { .. } => MaterialResolutionOutcome::Loaded,
            Self::Missing { .. } => MaterialResolutionOutcome::Missing,
            Self::Malformed { .. } => MaterialResolutionOutcome::Malformed,
            Self::Unresolved { .. } => MaterialResolutionOutcome::Unresolved,
        }
    }

    pub(crate) fn canonical_name(&self) -> Option<&str> {
        match self {
            Self::Loaded { canonical_name, .. } | Self::Unresolved { canonical_name } => {
                Some(canonical_name)
            }
            Self::Missing { .. } => Some("___error"),
            Self::Malformed { .. } => None,
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
struct ResolverShared {
    addon: Arc<PreviewArchiveSource>,
    loose_source_dirs: Vec<LooseSourceDir>,
    sibling_gma_paths: Vec<SiblingGmaPath>,
    game_vpk_paths: Vec<PathBuf>,
    sibling_gmas: OnceLock<SiblingGmaIndex>,
    game_vpks: OnceLock<Vec<VpkArchive>>,
    material_definitions: MaterialDefinitionCache,
    texture_assets: TextureAssetCache,
    sound_scripts: OnceLock<SoundScriptLibrary>,
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
    fn variant(&self, config: ResolverConfig) -> Self {
        Self {
            config,
            shared: Arc::clone(&self.shared),
        }
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "gmod_dir is threaded by value through many preview-pipeline call sites upstream of this leaf consumer"
    )]
    pub(crate) fn new(addon: impl IntoPreviewArchiveSource, gmod_dir: Option<PathBuf>) -> Self {
        let mounted_game_dirs = gmod_dir
            .as_deref()
            .map(discover_mounted_game_dirs)
            .unwrap_or_default();
        let game_vpk_paths = gmod_dir
            .as_deref()
            .map(|dir| {
                gmpublished_backend::vpk::discover_game_vpks_with_mounts(dir, &mounted_game_dirs)
            })
            .unwrap_or_default();
        let mut loose_source_dirs = gmod_dir
            .as_deref()
            .map(discover_loose_source_dirs)
            .unwrap_or_default();
        loose_source_dirs.extend(
            gmod_dir
                .as_deref()
                .map(discovery::existing_mount_cfg_dirs)
                .unwrap_or_default()
                .into_iter()
                .map(LooseSourceDir::new),
        );
        let sibling_gma_paths = gmod_dir
            .as_deref()
            .map(discover_sibling_gma_paths)
            .unwrap_or_default();
        Self {
            config: ResolverConfig {
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
                addon: addon.into_preview_archive_source(),
                loose_source_dirs,
                sibling_gma_paths,
                game_vpk_paths,
                sibling_gmas: OnceLock::new(),
                game_vpks: OnceLock::new(),
                material_definitions: MaterialDefinitionCache::default(),
                texture_assets: TextureAssetCache::default(),
                sound_scripts: OnceLock::new(),
                resolved_sound_cache: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub(crate) fn with_pakfile_source(
        addon: impl IntoPreviewArchiveSource,
        gmod_dir: Option<PathBuf>,
        pakfile: MapPakFile,
    ) -> Self {
        let mut resolver = Self::new(addon, gmod_dir);
        resolver.config.pakfile = PakSource::new(pakfile).map(Arc::new);
        resolver
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
        let key = Self::sound_reference_key(reference)?;
        {
            let cache = self.shared.resolved_sound_cache.lock();
            if let Some(cached) = cache.get(&key) {
                return cached.clone();
            }
        }

        let resolved = self.resolve_sound_reference_uncached(reference);
        self.shared
            .resolved_sound_cache
            .lock()
            .insert(key, resolved.clone());
        resolved
    }

    #[cfg(test)]
    pub(crate) fn sound_script_files(&self) -> Vec<String> {
        self.sound_scripts().script_files.clone()
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

    fn game_vpks(&self) -> &[VpkArchive] {
        self.shared
            .game_vpks
            .get_or_init(|| {
                self.shared
                    .game_vpk_paths
                    .iter()
                    .filter_map(|path| match VpkArchive::open(path) {
                        Ok(archive) => Some(archive),
                        Err(error) => {
                            log::debug!("game VPK open failed for {}: {error}", path.display());
                            None
                        }
                    })
                    .collect()
            })
            .as_slice()
    }

    fn sibling_gmas(&self) -> &SiblingGmaIndex {
        self.shared
            .sibling_gmas
            .get_or_init(|| build_sibling_gma_index(&self.shared.sibling_gma_paths))
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
        let Some(script) = self.sound_scripts().scripts.get(&key) else {
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
        let resolved = self.find_content_bytes(std::slice::from_ref(&ContentPath::new(&path)?))?;
        Some(ResolvedSoundWave {
            path: resolved.path,
            source_tier: resolved.tier,
            bytes: Arc::from(resolved.bytes),
        })
    }

    fn sound_scripts(&self) -> &SoundScriptLibrary {
        self.shared
            .sound_scripts
            .get_or_init(|| self.load_sound_script_library())
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

    fn sources(&self) -> impl Iterator<Item = SourceRef<'_>> {
        self.config
            .pakfile
            .as_deref()
            .map(SourceRef::Pakfile)
            .into_iter()
            .chain(std::iter::once(SourceRef::Addon(&self.shared.addon)))
            .chain(self.shared.loose_source_dirs.iter().map(SourceRef::Loose))
            .chain(std::iter::once_with(|| {
                SourceRef::SiblingGma(self.sibling_gmas())
            }))
            .chain(
                std::iter::once_with(|| self.game_vpks().iter().map(SourceRef::GameVpk)).flatten(),
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

    fn find_content_bytes(&self, paths: &[ContentPath]) -> Option<ResolvedContentBytes> {
        self.sources().find_map(|source| {
            paths.iter().find_map(|path| {
                Self::tier_bytes(&source, path).map(|bytes| ResolvedContentBytes {
                    path: path.as_str().to_owned(),
                    tier: source.tier(),
                    bytes,
                })
            })
        })
    }

    fn content_bytes_from_all_sources(&self, path: &str) -> Vec<ResolvedContentBytes> {
        let Some(path) = ContentPath::new(path) else {
            return Vec::new();
        };
        self.sources()
            .filter_map(|source| {
                Self::tier_bytes(&source, &path).map(|bytes| ResolvedContentBytes {
                    path: path.as_str().to_owned(),
                    tier: source.tier(),
                    bytes,
                })
            })
            .collect()
    }

    fn source_paths_matching(&self, matches: impl Fn(&str) -> bool) -> Vec<String> {
        let mut paths = Vec::new();
        let mut seen = BTreeSet::new();
        for source in self.sources() {
            source.for_each_path(&mut |path| {
                push_matching_path(path, &matches, &mut seen, &mut paths);
            });
        }
        paths
    }

    fn find_entry_by_source<T>(
        &self,
        paths: &[ContentPath],
        mut consume: impl FnMut(&ContentPath, Vec<u8>) -> Option<T>,
    ) -> Option<T> {
        self.sources().find_map(|source| {
            paths.iter().find_map(|path| {
                Self::tier_bytes(&source, path).and_then(|bytes| consume(path, bytes))
            })
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum SourceError {
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
    Pakfile(&'a PakSource),
    Addon(&'a PreviewArchiveSource),
    Loose(&'a LooseSourceDir),
    SiblingGma(&'a SiblingGmaIndex),
    GameVpk(&'a VpkArchive),
}

impl SourceRef<'_> {
    fn tier(&self) -> ContentSourceTier {
        match self {
            Self::Pakfile(_) => ContentSourceTier::Pakfile,
            Self::Addon(_) => ContentSourceTier::Addon,
            Self::Loose(_) => ContentSourceTier::Loose,
            Self::SiblingGma(_) => ContentSourceTier::SiblingGma,
            Self::GameVpk(_) => ContentSourceTier::GameVpk,
        }
    }

    fn entry_bytes(&self, path: &ContentPath) -> Result<Option<Vec<u8>>, SourceError> {
        match self {
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
}

impl PakSource {
    fn new(pakfile: MapPakFile) -> Option<Self> {
        if let Some(error) = pakfile.read_error() {
            log::debug!("bsp pakfile source disabled: {error}");
            return None;
        }
        let entries = match pakfile.indexed_entries() {
            Ok(entries) => entries,
            Err(error) => {
                log::debug!("bsp pakfile source index failed: {error}");
                return None;
            }
        };
        let entries = entries
            .into_iter()
            .map(|entry| (entry.path, (entry.index, entry.size)))
            .collect::<HashMap<_, _>>();
        Some(Self { pakfile, entries })
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
