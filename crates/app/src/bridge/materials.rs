use gmpublished_domain::math::Vec3;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

mod discovery;
mod mirror;
mod sibling_gma;
mod texture;
mod water;
pub use mirror::MirrorMaterial;
pub use water::{
    LightmappedMaterial, MaterialShader, UnlitMaterial, WaterMaterial, WaterNormalMap,
};

pub use texture::decode_vtf_rgba;
use texture::{
    bc_supported, decode_vtf_rgba_max, downscale_resolved_texture, force_opaque_alpha,
    push_matching_path, resolved_bc_texture, with_generated_mip_chain,
};
#[cfg(test)]
use texture::{
    downscaled_texture_dimensions, drop_bc_mips_to_max_dimension, generate_srgb_mip_chain,
};

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
/// Must match the shader's default water fog tint when a material omits $fogcolor.
pub const DEFAULT_WATER_FOG_LINEAR: Vec3 = Vec3::new(0.03, 0.10, 0.10);
const GMA_MAGIC: &[u8; 4] = b"GMAD";
/// Use gray for envmaps without a basetexture, matching Source's reflective-metal fallback.
const ENVMAP_FALLBACK_RGBA: [u8; 4] = [128, 128, 128, 255];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTexture {
    payload: ResolvedTexturePayload,
    pub(crate) width: u32,
    pub(crate) height: u32,
    original_width: u32,
    original_height: u32,
    water_fallback: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedTexturePayload {
    Rgba {
        rgba: Vec<u8>,
        mip_chain: Vec<ResolvedTextureMip>,
    },
    Bc {
        format: BcFormat,
        mips: Vec<ResolvedBcMip>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTextureMip {
    pub(crate) rgba: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedBcMip {
    pub(crate) data: Vec<u8>,
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
        Self {
            payload: ResolvedTexturePayload::Rgba {
                rgba,
                mip_chain: Vec::new(),
            },
            width,
            height,
            original_width,
            original_height,
            water_fallback,
        }
    }

    fn bc(
        format: BcFormat,
        mips: Vec<ResolvedBcMip>,
        original_width: u32,
        original_height: u32,
    ) -> Option<Self> {
        let (width, height) = mips
            .first()
            .map(|base| (base.width.max(1), base.height.max(1)))?;
        Some(Self {
            payload: ResolvedTexturePayload::Bc { format, mips },
            width,
            height,
            original_width,
            original_height,
            water_fallback: false,
        })
    }

    pub(crate) fn is_water_fallback(&self) -> bool {
        self.water_fallback
    }

    pub(crate) fn rgba_bytes(&self) -> Option<&[u8]> {
        match &self.payload {
            ResolvedTexturePayload::Rgba { rgba, .. } => Some(rgba),
            ResolvedTexturePayload::Bc { .. } => None,
        }
    }

    pub(crate) fn bc_payload(&self) -> Option<(BcFormat, &[ResolvedBcMip])> {
        match &self.payload {
            ResolvedTexturePayload::Rgba { .. } => None,
            ResolvedTexturePayload::Bc { format, mips } => Some((*format, mips)),
        }
    }

    pub(crate) fn is_bc(&self) -> bool {
        matches!(self.payload, ResolvedTexturePayload::Bc { .. })
    }

    #[cfg(test)]
    pub(crate) fn mip_level_count(&self) -> u32 {
        match &self.payload {
            ResolvedTexturePayload::Rgba { mip_chain, .. } => u32::try_from(mip_chain.len())
                .unwrap_or(u32::MAX)
                .saturating_add(1),
            ResolvedTexturePayload::Bc { mips, .. } => {
                u32::try_from(mips.len()).unwrap_or(u32::MAX)
            }
        }
    }

    pub(crate) fn mip_chain(&self) -> impl Iterator<Item = ResolvedTextureMipRef<'_>> {
        let (base_rgba, mip_chain) = match &self.payload {
            ResolvedTexturePayload::Rgba { rgba, mip_chain } => {
                (rgba.as_slice(), mip_chain.as_slice())
            }
            ResolvedTexturePayload::Bc { .. } => (&[][..], &[][..]),
        };
        std::iter::once(ResolvedTextureMipRef {
            rgba: base_rgba,
            width: self.width.max(1),
            height: self.height.max(1),
        })
        .filter(|mip| !mip.rgba.is_empty())
        .chain(mip_chain.iter().map(|mip| ResolvedTextureMipRef {
            rgba: mip.rgba.as_slice(),
            width: mip.width.max(1),
            height: mip.height.max(1),
        }))
    }

    pub(crate) fn mip_chain_byte_len(&self) -> usize {
        match &self.payload {
            ResolvedTexturePayload::Rgba { rgba, mip_chain } => rgba
                .len()
                .saturating_add(mip_chain.iter().map(|mip| mip.rgba.len()).sum::<usize>()),
            ResolvedTexturePayload::Bc { mips, .. } => mips.iter().map(|mip| mip.data.len()).sum(),
        }
    }

    pub(crate) fn without_mip_chain(&self) -> Self {
        let payload = match &self.payload {
            ResolvedTexturePayload::Rgba { rgba, .. } => ResolvedTexturePayload::Rgba {
                rgba: rgba.clone(),
                mip_chain: Vec::new(),
            },
            ResolvedTexturePayload::Bc { format, mips } => ResolvedTexturePayload::Bc {
                format: *format,
                mips: mips.first().cloned().into_iter().collect(),
            },
        };
        Self {
            payload,
            width: self.width,
            height: self.height,
            original_width: self.original_width,
            original_height: self.original_height,
            water_fallback: self.water_fallback,
        }
    }

    /// Pre-downscale dimensions: BSP texel UVs normalize against the source
    /// texture size, not whatever the preview uploaded.
    pub(crate) fn original_dimensions(&self) -> (u32, u32) {
        (self.original_width.max(1), self.original_height.max(1))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedMaterialTextures {
    pub(crate) shader: MaterialShader,
    pub(crate) texture: Option<Arc<ResolvedTexture>>,
    pub(crate) texture2: Option<Arc<ResolvedTexture>>,
    pub(crate) force_opaque: bool,
    pub(crate) render_mode: RenderMode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPrimaryMaterial {
    pub(crate) shader: MaterialShader,
    pub(crate) texture: Arc<ResolvedTexture>,
    pub(crate) force_opaque: bool,
    pub(crate) render_mode: RenderMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderMode {
    Opaque,
    Cutout,
    Translucent,
    Additive,
}

impl RenderMode {
    pub(crate) const fn force_opaque(self) -> bool {
        matches!(self, Self::Opaque)
    }

    const fn preserves_texture_alpha(self) -> bool {
        !matches!(self, Self::Opaque)
    }
}

#[derive(Clone)]
struct ResolverConfig {
    pakfile: Option<Arc<PakSource>>,
    decoded_texture_max_dimension: Option<u32>,
    decoded_texture_budget: Option<Arc<DecodedTextureBudget>>,
    bc_textures: BcTextures,
}

#[derive(Debug)]
struct ResolverShared {
    addon: Arc<PreviewArchiveSource>,
    loose_source_dirs: Vec<LooseSourceDir>,
    sibling_gma_paths: Vec<SiblingGmaPath>,
    game_vpk_paths: Vec<PathBuf>,
    sibling_gmas: OnceLock<SiblingGmaIndex>,
    game_vpks: OnceLock<Vec<VpkArchive>>,
    decoded_texture_cache: Mutex<HashMap<DecodedTextureCacheKey, Arc<ResolvedTexture>>>,
    normal_map_cache: Mutex<HashMap<water::WaterNormalKey, Arc<WaterNormalMap>>>,
    sound_scripts: OnceLock<SoundScriptLibrary>,
    resolved_sound_cache: Mutex<HashMap<String, Option<ResolvedSoundReference>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BcTextures {
    IfGpuSupports,
    /// Skybox and PHY uploads require RGBA, even when the GPU supports BC textures.
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

#[derive(Debug)]
pub struct DecodedTextureBudget {
    budget_bytes: usize,
    decoded_bytes: AtomicUsize,
    rejected_textures: AtomicUsize,
    exhausted: AtomicBool,
}

impl DecodedTextureBudget {
    pub(crate) fn new(budget_bytes: usize) -> Self {
        Self {
            budget_bytes,
            decoded_bytes: AtomicUsize::new(0),
            rejected_textures: AtomicUsize::new(0),
            exhausted: AtomicBool::new(false),
        }
    }

    pub(crate) fn decoded_bytes(&self) -> usize {
        self.decoded_bytes.load(Ordering::Acquire)
    }

    pub(crate) fn rejected_textures(&self) -> usize {
        self.rejected_textures.load(Ordering::Acquire)
    }

    fn try_reserve(&self, byte_len: usize) -> bool {
        if byte_len > self.budget_bytes {
            self.rejected_textures.fetch_add(1, Ordering::AcqRel);
            self.exhausted.store(true, Ordering::Release);
            return false;
        }
        let reserved = self
            .decoded_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(byte_len)
                    .filter(|total| *total <= self.budget_bytes)
            })
            .is_ok();
        if !reserved {
            self.rejected_textures.fetch_add(1, Ordering::AcqRel);
            self.exhausted.store(true, Ordering::Release);
        }
        reserved
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum DecodedTextureCacheKey {
    Rgba {
        path: ContentPath,
        preserve_alpha: bool,
        max_dimension: Option<u32>,
    },
    Bc {
        path: ContentPath,
        max_dimension: Option<u32>,
    },
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
        // Only explicit mount.cfg targets join the loose tier: sibling Steam
        // games ship their content entirely in the VPKs discovered above, and
        // a mount.cfg target is routinely a loose models/materials tree.
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
                pakfile: None,
                decoded_texture_max_dimension: None,
                decoded_texture_budget: None,
                bc_textures: BcTextures::IfGpuSupports,
            },
            shared: Arc::new(ResolverShared {
                addon: addon.into_preview_archive_source(),
                loose_source_dirs,
                sibling_gma_paths,
                game_vpk_paths,
                sibling_gmas: OnceLock::new(),
                game_vpks: OnceLock::new(),
                decoded_texture_cache: Mutex::new(HashMap::new()),
                normal_map_cache: Mutex::new(HashMap::new()),
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
                BcTextures::IfGpuSupports | BcTextures::Forced(_) => BcTextures::Forced(supported),
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
        self.find_entry_by_source(&material_paths, |material_path, vmt_bytes| {
            self.resolve_primary_material_bytes(material_path, &vmt_bytes)
                .map(|material| Arc::clone(&material.texture))
        })
    }

    pub(crate) fn resolve_primary(
        &self,
        material_dirs: &[String],
        material_name: &str,
    ) -> Option<ResolvedPrimaryMaterial> {
        let material_paths = material_paths(material_dirs, material_name);
        self.find_entry_by_source(&material_paths, |material_path, vmt_bytes| {
            self.resolve_primary_material_bytes(material_path, &vmt_bytes)
        })
    }

    pub(crate) fn resolve_with_base2(
        &self,
        material_dirs: &[String],
        material_name: &str,
    ) -> Option<ResolvedMaterialTextures> {
        let material_paths = material_paths(material_dirs, material_name);
        self.find_entry_by_source(&material_paths, |material_path, vmt_bytes| {
            self.resolve_material_bytes(material_path, &vmt_bytes)
        })
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

    pub(crate) fn resolve_base_texture_at_path(
        &self,
        material_path: &str,
    ) -> Option<Arc<ResolvedTexture>> {
        let material_path = ContentPath::new(material_path)?;
        self.find_entry_by_source(std::slice::from_ref(&material_path), |path, vmt_bytes| {
            self.resolve_base_texture_material_bytes(path, &vmt_bytes)
        })
    }

    fn resolve_material_bytes(
        &self,
        material_path: &ContentPath,
        vmt_bytes: &[u8],
    ) -> Option<ResolvedMaterialTextures> {
        let vmt_text = String::from_utf8_lossy(vmt_bytes);
        let mut visited_includes = vec![material_path.clone()];
        let material = self.effective_material(&vmt_text, 0, &mut visited_includes)?;
        let render_mode = material.render_mode();
        let texture = material
            .parameter("$basetexture")
            .and_then(|base_texture| {
                self.resolve_texture(
                    base_texture,
                    render_mode.preserves_texture_alpha() || is_water_shader(&material.shader),
                )
            })
            .or_else(|| self.mirror_fallback_texture(&material))
            .or_else(|| self.water_fallback_texture(&material))
            .or_else(|| self.envmap_fallback_texture(&material));
        let texture2 = material
            .parameter("$basetexture2")
            .and_then(|base_texture| {
                self.resolve_texture(
                    base_texture,
                    render_mode.preserves_texture_alpha() || is_water_shader(&material.shader),
                )
            });

        let shader = self.resolve_shader(
            &material,
            texture
                .as_ref()
                .is_some_and(|texture| !texture.is_water_fallback()),
        );
        // A missing bitmap changes the fallback image, not UnlitGeneric's
        // authored shading and blend identity.
        (texture.is_some() || texture2.is_some() || shader.unlit().is_some()).then_some(
            ResolvedMaterialTextures {
                shader,
                texture,
                texture2,
                force_opaque: render_mode.force_opaque(),
                render_mode,
            },
        )
    }

    fn resolve_primary_material_bytes(
        &self,
        material_path: &ContentPath,
        vmt_bytes: &[u8],
    ) -> Option<ResolvedPrimaryMaterial> {
        let vmt_text = String::from_utf8_lossy(vmt_bytes);
        let mut visited_includes = vec![material_path.clone()];
        let material = self.effective_material(&vmt_text, 0, &mut visited_includes)?;
        let render_mode = material.render_mode();
        let texture = material
            .parameter("$basetexture")
            .and_then(|base_texture| {
                self.resolve_texture(
                    base_texture,
                    render_mode.preserves_texture_alpha() || is_water_shader(&material.shader),
                )
            })
            .or_else(|| self.mirror_fallback_texture(&material))
            .or_else(|| self.water_fallback_texture(&material))
            .or_else(|| self.envmap_fallback_texture(&material))?;
        Some(ResolvedPrimaryMaterial {
            shader: self.resolve_shader(&material, !texture.is_water_fallback()),
            texture,
            force_opaque: render_mode.force_opaque(),
            render_mode,
        })
    }

    fn resolve_base_texture_material_bytes(
        &self,
        material_path: &ContentPath,
        vmt_bytes: &[u8],
    ) -> Option<Arc<ResolvedTexture>> {
        let vmt_text = String::from_utf8_lossy(vmt_bytes);
        let mut visited_includes = vec![material_path.clone()];
        let material = self.effective_material(&vmt_text, 0, &mut visited_includes)?;
        let render_mode = material.render_mode();
        material.parameter("$basetexture").and_then(|base_texture| {
            self.resolve_texture(
                base_texture,
                render_mode.preserves_texture_alpha() || is_water_shader(&material.shader),
            )
        })
    }

    fn effective_material(
        &self,
        vmt_text: &str,
        depth: usize,
        visited_includes: &mut Vec<ContentPath>,
    ) -> Option<EffectiveMaterial> {
        let document = vformats::vmt::parse(vmt_text, &Limits::default()).ok()?;
        if let Some(patch) = document.patch() {
            let mut material = self
                .effective_patch_include(&patch.include, depth, visited_includes)
                .unwrap_or_else(|| EffectiveMaterial::from_document(&document));
            material.apply_patch_values(&document, &patch);
            return Some(material);
        }

        Some(EffectiveMaterial::from_document(&document))
    }

    fn effective_patch_include(
        &self,
        include: &str,
        depth: usize,
        visited_includes: &mut Vec<ContentPath>,
    ) -> Option<EffectiveMaterial> {
        if depth >= PATCH_INCLUDE_LIMIT {
            log::debug!("material patch include recursion limit reached at {include}");
            return None;
        }
        let Some(include_path) = ContentPath::new(include) else {
            log::debug!("material patch include path rejected: {include}");
            return None;
        };
        if visited_includes.contains(&include_path) {
            log::debug!("material patch include cycle rejected at {include_path}");
            return None;
        }

        visited_includes.push(include_path.clone());
        let include_bytes = self.entry_bytes_from_sources(&include_path);
        let material = include_bytes.and_then(|include_bytes| {
            let include_text = String::from_utf8_lossy(&include_bytes);
            self.effective_material(&include_text, depth + 1, visited_includes)
        });
        visited_includes.pop();
        material
    }

    fn resolve_texture(
        &self,
        base_texture: &str,
        preserve_alpha: bool,
    ) -> Option<Arc<ResolvedTexture>> {
        let texture_path = texture_path(base_texture)?;
        // Include the size cap in the shared cache key or a bounded resolver can receive
        // an uncapped decode.
        let max_dimension = self.config.decoded_texture_max_dimension;
        let bc_cache_key = DecodedTextureCacheKey::Bc {
            path: texture_path.clone(),
            max_dimension,
        };
        if self.bc_textures_enabled()
            && let Some(texture) = self.cached_decoded_texture(&bc_cache_key)
        {
            return Some(texture);
        }

        let rgba_cache_key = DecodedTextureCacheKey::Rgba {
            path: texture_path.clone(),
            preserve_alpha,
            max_dimension,
        };
        if let Some(texture) = self.cached_decoded_texture(&rgba_cache_key) {
            return Some(texture);
        }

        self.find_entry_by_source(std::slice::from_ref(&texture_path), |_, vtf_bytes| {
            if self.bc_textures_enabled()
                && let Some(texture) =
                    resolved_bc_texture(&vtf_bytes, self.config.decoded_texture_max_dimension)
            {
                return self.cache_decoded_texture(bc_cache_key.clone(), texture);
            }

            match decode_vtf_rgba_max(&vtf_bytes, self.config.decoded_texture_max_dimension) {
                Ok((decoded, source_width, source_height)) => {
                    let mut rgba = decoded.rgba;
                    if !preserve_alpha {
                        force_opaque_alpha(&mut rgba);
                    }
                    let texture = with_generated_mip_chain(downscale_resolved_texture(
                        ResolvedTexture::rgba(
                            rgba,
                            decoded.width,
                            decoded.height,
                            source_width,
                            source_height,
                            false,
                        ),
                        self.config.decoded_texture_max_dimension,
                    ));
                    self.cache_decoded_texture(rgba_cache_key.clone(), texture)
                }
                Err(error) => {
                    log::debug!("material texture decode failed for {texture_path}: {error}");
                    None
                }
            }
        })
    }

    fn cached_decoded_texture(&self, key: &DecodedTextureCacheKey) -> Option<Arc<ResolvedTexture>> {
        self.shared.decoded_texture_cache.lock().get(key).cloned()
    }

    fn cache_decoded_texture(
        &self,
        key: DecodedTextureCacheKey,
        texture: ResolvedTexture,
    ) -> Option<Arc<ResolvedTexture>> {
        let texture = Arc::new(texture);
        let byte_len = texture.mip_chain_byte_len();
        let mut cache = self.shared.decoded_texture_cache.lock();
        if let Some(cached) = cache.get(&key) {
            return Some(Arc::clone(cached));
        }
        if let Some(budget) = &self.config.decoded_texture_budget
            && !budget.try_reserve(byte_len)
        {
            return None;
        }
        cache.insert(key, Arc::clone(&texture));
        drop(cache);
        Some(texture)
    }

    /// An envmap without a basetexture is chrome, not a missing material.
    /// Use the envmap if readable, else gray. A named but missing basetexture
    /// still gets the error checkerboard.
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
            && let Some(texture) = self.resolve_texture(&env_map, false)
        {
            return Some(texture);
        }
        Some(Arc::new(ResolvedTexture::rgba(
            ENVMAP_FALLBACK_RGBA.to_vec(),
            1,
            1,
            1,
            1,
            false,
        )))
    }

    fn mirror_fallback_texture(
        &self,
        material: &EffectiveMaterial,
    ) -> Option<Arc<ResolvedTexture>> {
        mirror::is_mirror_shader(&material.shader)
            .then(|| Arc::new(ResolvedTexture::rgba(vec![255; 4], 1, 1, 1, 1, false)))
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
            BcTextures::IfGpuSupports => bc_supported(),
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

    /// Defer sibling GMA and VPK indexing until earlier sources miss.
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

    /// `Addon` and `GameVpk` take `&str`: both are lower-level archive readers
    /// with their own path space and callers outside this resolver.
    /// `Ok(None)` when this tier simply does not have the path; `Err` when it
    /// has it and could not read it.
    fn entry_bytes(&self, path: &ContentPath) -> Result<Option<Vec<u8>>, SourceError> {
        match self {
            Self::Pakfile(pakfile) => pakfile.entry_bytes(path),
            Self::Addon(addon) => match addon.entry_bytes(path.as_str()) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(PreviewArchiveSourceError::EntryNotFound(_)) => Ok(None),
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
    shader: String,
    parameters: BTreeMap<String, String>,
    proxies: Vec<water::MaterialProxy>,
}

impl EffectiveMaterial {
    fn from_document(document: &vformats::vmt::VmtDocument<'_>) -> Self {
        Self {
            shader: document.shader.to_string(),
            parameters: water::string_values(&document.kv),
            proxies: water::proxies(&document.kv),
        }
    }

    fn parameter(&self, key: &str) -> Option<&str> {
        self.parameters.get(key).map(String::as_str)
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
        self.parameters.extend(water::string_values(&document.kv));
        water::merge_proxies(
            &mut self.proxies,
            water::proxies(&document.kv),
            water::PatchMode::Override,
        );
        for (name, mode) in [
            ("insert", water::PatchMode::Insert),
            ("replace", water::PatchMode::Replace),
        ] {
            for block in document.kv.blocks(name) {
                for (key, value) in water::string_values(block) {
                    let exists = self.parameters.contains_key(&key);
                    if (name == "insert" && !exists) || name == "replace" {
                        self.parameters.insert(key, value);
                    }
                }
                water::merge_proxies(&mut self.proxies, water::proxies(block), mode);
            }
        }
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

fn is_water_shader(shader: &str) -> bool {
    strip_prefix_ascii_case(shader, "water").is_some()
}

fn water_fog_rgba(value: Option<&str>) -> [u8; 4] {
    let linear = value
        .and_then(parse_water_fog_color)
        .unwrap_or(DEFAULT_WATER_FOG_LINEAR);
    [
        linear_to_srgb_byte(linear[0]),
        linear_to_srgb_byte(linear[1]),
        linear_to_srgb_byte(linear[2]),
        255,
    ]
}

fn parse_water_fog_color(value: &str) -> Option<Vec3> {
    let trimmed = value.trim();
    let (inner, scale) = if let Some(inner) = bracketed_value(trimmed, '[', ']') {
        (inner, 1.0)
    } else if let Some(inner) = bracketed_value(trimmed, '{', '}') {
        (inner, 1.0 / 255.0)
    } else {
        return None;
    };

    let mut components = inner
        .split(|char: char| char.is_ascii_whitespace() || char == ',')
        .filter(|component| !component.is_empty())
        .map(str::parse::<f32>);
    let red = components.next()?.ok()? * scale;
    let green = components.next()?.ok()? * scale;
    let blue = components.next()?.ok()? * scale;
    [red, green, blue]
        .into_iter()
        .all(f32::is_finite)
        .then_some(Vec3::new(red, green, blue))
}

fn bracketed_value(value: &str, open: char, close: char) -> Option<&str> {
    value
        .strip_prefix(open)
        .and_then(|value| value.strip_suffix(close))
        .map(str::trim)
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
    entries: HashMap<String, usize>,
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
            .map(|entry| (entry.path, entry.index))
            .collect::<HashMap<_, _>>();
        Some(Self { pakfile, entries })
    }

    fn for_each_path(&self, visit: &mut dyn FnMut(&str)) {
        self.entries.keys().for_each(|path| visit(path));
    }

    fn entry_bytes(&self, path: &ContentPath) -> Result<Option<Vec<u8>>, SourceError> {
        let Some(index) = self.entries.get(path.as_str()).copied() else {
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
        // Reject traversal here too, before turning a content path into a filesystem path.
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
