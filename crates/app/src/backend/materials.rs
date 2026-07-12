use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io::{self, BufReader, Read},
    panic,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use parking_lot::Mutex;

use crate::backend::{archive::PreviewArchiveSource, gma::PreviewArchive, vpk::VpkArchive};
use gmpublished_backend::scene::map::MapPakFile;
use iced::wgpu;
use vformats::vtf::BcFormat;
use vformats::{Limits, soundscript};

const PATCH_INCLUDE_LIMIT: usize = 4;
const MAX_SIBLING_GMA_ARCHIVES: usize = 2048;
const MAX_LEGACY_BIN_ENTRY_TABLE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LEGACY_BIN_FETCH_BYTES: u64 = 1024 * 1024 * 1024;
const DEFAULT_WATER_FOG_LINEAR: [f32; 3] = [0.03, 0.10, 0.10];
const GMA_MAGIC: &[u8; 4] = b"GMAD";

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMaterialTextures {
    pub(crate) texture: Option<Arc<ResolvedTexture>>,
    pub(crate) texture2: Option<Arc<ResolvedTexture>>,
    pub(crate) force_opaque: bool,
    pub(crate) render_mode: RenderMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPrimaryMaterial {
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
    addon: Arc<PreviewArchiveSource>,
    prepended: Option<Arc<HashMap<String, Vec<u8>>>>,
    pakfile: Option<Arc<PakSource>>,
    loose_source_dirs: Vec<LooseSourceDir>,
    sibling_gma_paths: Vec<SiblingGmaPath>,
    game_vpk_paths: Vec<PathBuf>,
    decoded_texture_max_dimension: Option<u32>,
    decoded_texture_budget: Option<Arc<DecodedTextureBudget>>,
    bc_textures_allowed: bool,
    bc_texture_support_override: Option<bool>,
}

pub struct MaterialResolver {
    config: ResolverConfig,
    sibling_gmas: OnceLock<SiblingGmaIndex>,
    game_vpks: OnceLock<Vec<VpkArchive>>,
    decoded_texture_cache: Mutex<HashMap<DecodedTextureCacheKey, Arc<ResolvedTexture>>>,
    sound_scripts: OnceLock<SoundScriptLibrary>,
    resolved_sound_cache: Mutex<HashMap<String, Option<ResolvedSoundReference>>>,
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
    Prepended,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSoundWave {
    pub(crate) path: String,
    pub(crate) source_tier: ContentSourceTier,
    pub(crate) bytes: Arc<Vec<u8>>,
}

#[derive(Debug)]
struct ResolvedContentBytes {
    path: String,
    tier: ContentSourceTier,
    bytes: Vec<u8>,
}

/// Owned mirror of the fields this app reads from a parsed
/// soundscript (the parse borrows from a transient file buffer).
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
    // Only read by `sound_script_files()`, which is test-only (asserts manifest
    // discovery found the expected `scripts/game_sounds_*.txt` files). Gating the
    // field itself keeps release builds honest instead of suppressing dead_code.
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
    Rgba { path: String, preserve_alpha: bool },
    Bc { path: String },
}

impl MaterialResolver {
    fn from_config(config: ResolverConfig) -> Self {
        Self {
            config,
            sibling_gmas: OnceLock::new(),
            game_vpks: OnceLock::new(),
            decoded_texture_cache: Mutex::new(HashMap::new()),
            sound_scripts: OnceLock::new(),
            resolved_sound_cache: Mutex::new(HashMap::new()),
        }
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "gmod_dir is threaded by value through many preview-pipeline call sites upstream of this leaf consumer"
    )]
    pub(crate) fn new(addon: impl IntoPreviewArchiveSource, gmod_dir: Option<PathBuf>) -> Self {
        let game_vpk_paths = gmod_dir
            .as_deref()
            .map(gmpublished_backend::vpk::discover_game_vpks)
            .unwrap_or_default();
        let loose_source_dirs = gmod_dir
            .as_deref()
            .map(discover_loose_source_dirs)
            .unwrap_or_default();
        let sibling_gma_paths = gmod_dir
            .as_deref()
            .map(discover_sibling_gma_paths)
            .unwrap_or_default();
        Self::from_config(ResolverConfig {
            addon: addon.into_preview_archive_source(),
            prepended: None,
            pakfile: None,
            loose_source_dirs,
            sibling_gma_paths,
            game_vpk_paths,
            decoded_texture_max_dimension: None,
            decoded_texture_budget: None,
            bc_textures_allowed: true,
            bc_texture_support_override: None,
        })
    }

    #[cfg(test)]
    fn with_prepended_source(
        addon: impl IntoPreviewArchiveSource,
        gmod_dir: Option<PathBuf>,
        entries: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Self {
        let mut resolver = Self::new(addon, gmod_dir);
        resolver.config.prepended = Some(Arc::new(
            entries
                .into_iter()
                .filter_map(|(path, bytes)| normalize_archive_path(&path).map(|path| (path, bytes)))
                .collect(),
        ));
        resolver
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
        Self::from_config(ResolverConfig {
            decoded_texture_max_dimension: Some(max_dimension.max(1)),
            ..self.config.clone()
        })
    }

    pub(crate) fn with_decoded_texture_budget(&self, budget: Arc<DecodedTextureBudget>) -> Self {
        Self::from_config(ResolverConfig {
            decoded_texture_budget: Some(budget),
            ..self.config.clone()
        })
    }

    pub(crate) fn with_bc_textures_disabled(&self) -> Self {
        Self::from_config(ResolverConfig {
            bc_textures_allowed: false,
            ..self.config.clone()
        })
    }

    #[cfg(test)]
    fn with_bc_texture_support(&self, supported: bool) -> Self {
        Self::from_config(ResolverConfig {
            bc_texture_support_override: Some(supported),
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
        let path = normalize_archive_path(path)?;
        self.entry_bytes_from_sources(&path)
    }

    pub(crate) fn resolve_sound_reference(
        &self,
        reference: &str,
    ) -> Option<ResolvedSoundReference> {
        let key = reference.trim().to_ascii_lowercase();
        if key.is_empty() {
            return None;
        }
        {
            let cache = self.resolved_sound_cache.lock();
            if let Some(cached) = cache.get(&key) {
                return cached.clone();
            }
        }

        let resolved = self.resolve_sound_reference_uncached(reference);
        self.resolved_sound_cache
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
        let material_path = normalize_archive_path(material_path)?;
        self.find_entry_by_source(std::slice::from_ref(&material_path), |path, vmt_bytes| {
            self.resolve_base_texture_material_bytes(path, &vmt_bytes)
        })
    }

    fn resolve_material_bytes(
        &self,
        material_path: &str,
        vmt_bytes: &[u8],
    ) -> Option<ResolvedMaterialTextures> {
        let vmt_text = String::from_utf8_lossy(vmt_bytes);
        let mut visited_includes = vec![material_path.to_ascii_lowercase()];
        let material = self.effective_material(&vmt_text, 0, &mut visited_includes)?;
        let render_mode = material.render_mode();
        let texture = material
            .base_texture
            .as_deref()
            .and_then(|base_texture| {
                self.resolve_texture(base_texture, render_mode.preserves_texture_alpha())
            })
            .or_else(|| self.water_fallback_texture(&material));
        let texture2 = material.base_texture2.as_deref().and_then(|base_texture| {
            self.resolve_texture(base_texture, render_mode.preserves_texture_alpha())
        });

        (texture.is_some() || texture2.is_some()).then_some(ResolvedMaterialTextures {
            texture,
            texture2,
            force_opaque: render_mode.force_opaque(),
            render_mode,
        })
    }

    fn resolve_primary_material_bytes(
        &self,
        material_path: &str,
        vmt_bytes: &[u8],
    ) -> Option<ResolvedPrimaryMaterial> {
        let vmt_text = String::from_utf8_lossy(vmt_bytes);
        let mut visited_includes = vec![material_path.to_ascii_lowercase()];
        let material = self.effective_material(&vmt_text, 0, &mut visited_includes)?;
        let render_mode = material.render_mode();
        let texture = material
            .base_texture
            .as_deref()
            .and_then(|base_texture| {
                self.resolve_texture(base_texture, render_mode.preserves_texture_alpha())
            })
            .or_else(|| self.water_fallback_texture(&material))?;
        Some(ResolvedPrimaryMaterial {
            texture,
            force_opaque: render_mode.force_opaque(),
            render_mode,
        })
    }

    fn resolve_base_texture_material_bytes(
        &self,
        material_path: &str,
        vmt_bytes: &[u8],
    ) -> Option<Arc<ResolvedTexture>> {
        let vmt_text = String::from_utf8_lossy(vmt_bytes);
        let mut visited_includes = vec![material_path.to_ascii_lowercase()];
        let material = self.effective_material(&vmt_text, 0, &mut visited_includes)?;
        let render_mode = material.render_mode();
        material.base_texture.as_deref().and_then(|base_texture| {
            self.resolve_texture(base_texture, render_mode.preserves_texture_alpha())
        })
    }

    fn effective_material(
        &self,
        vmt_text: &str,
        depth: usize,
        visited_includes: &mut Vec<String>,
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
        visited_includes: &mut Vec<String>,
    ) -> Option<EffectiveMaterial> {
        if depth >= PATCH_INCLUDE_LIMIT {
            log::debug!("material patch include recursion limit reached at {include}");
            return None;
        }
        let Some(include_path) = normalize_archive_path(include) else {
            log::debug!("material patch include path rejected: {include}");
            return None;
        };
        if visited_includes
            .iter()
            .any(|visited| visited.eq_ignore_ascii_case(&include_path))
        {
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
        let bc_cache_key = DecodedTextureCacheKey::Bc {
            path: texture_path.clone(),
        };
        if self.bc_textures_enabled()
            && let Some(texture) = self.cached_decoded_texture(&bc_cache_key)
        {
            return Some(texture);
        }

        let rgba_cache_key = DecodedTextureCacheKey::Rgba {
            path: texture_path.clone(),
            preserve_alpha,
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

            match decode_vtf_rgba(&vtf_bytes) {
                Ok(decoded) => {
                    let mut rgba = decoded.rgba;
                    if !preserve_alpha {
                        force_opaque_alpha(&mut rgba);
                    }
                    let texture = with_generated_mip_chain(downscale_resolved_texture(
                        ResolvedTexture::rgba(
                            rgba,
                            decoded.width,
                            decoded.height,
                            decoded.width,
                            decoded.height,
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
        self.decoded_texture_cache.lock().get(key).cloned()
    }

    fn cache_decoded_texture(
        &self,
        key: DecodedTextureCacheKey,
        texture: ResolvedTexture,
    ) -> Option<Arc<ResolvedTexture>> {
        let texture = Arc::new(texture);
        let byte_len = texture.mip_chain_byte_len();
        let mut cache = self.decoded_texture_cache.lock();
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

    fn water_fallback_texture(&self, material: &EffectiveMaterial) -> Option<Arc<ResolvedTexture>> {
        is_water_shader(&material.shader).then(|| {
            Arc::new(ResolvedTexture::rgba(
                water_fog_rgba(material.fog_color.as_deref()).to_vec(),
                1,
                1,
                1,
                1,
                true,
            ))
        })
    }

    fn bc_textures_enabled(&self) -> bool {
        self.config.bc_textures_allowed
            && self
                .config
                .bc_texture_support_override
                .unwrap_or_else(bc_supported)
    }

    fn game_vpks(&self) -> &[VpkArchive] {
        self.game_vpks
            .get_or_init(|| {
                self.config
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
        self.sibling_gmas
            .get_or_init(|| build_sibling_gma_index(&self.config.sibling_gma_paths))
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

        let key = reference.trim().to_ascii_lowercase();
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
        let resolved = self.find_content_bytes(std::slice::from_ref(&path))?;
        Some(ResolvedSoundWave {
            path: resolved.path,
            source_tier: resolved.tier,
            bytes: Arc::new(resolved.bytes),
        })
    }

    fn sound_scripts(&self) -> &SoundScriptLibrary {
        self.sound_scripts
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

    fn entry_bytes_from_sources(&self, path: &str) -> Option<Vec<u8>> {
        self.find_entry_by_source(std::slice::from_ref(&path), |_, bytes| Some(bytes))
    }

    /// Every content source this resolver can read from, in lookup-priority order.
    fn sources(&self) -> impl Iterator<Item = SourceRef<'_>> {
        self.config
            .prepended
            .as_deref()
            .map(SourceRef::Prepended)
            .into_iter()
            .chain(self.config.pakfile.as_deref().map(SourceRef::Pakfile))
            .chain(std::iter::once(SourceRef::Addon(&self.config.addon)))
            .chain(self.config.loose_source_dirs.iter().map(SourceRef::Loose))
            .chain(std::iter::once(SourceRef::SiblingGma(self.sibling_gmas())))
            .chain(self.game_vpks().iter().map(SourceRef::GameVpk))
    }

    fn find_content_bytes<P: AsRef<str>>(&self, paths: &[P]) -> Option<ResolvedContentBytes> {
        self.sources().find_map(|source| {
            paths.iter().find_map(|path| {
                let path = path.as_ref();
                source.entry_bytes(path).map(|bytes| ResolvedContentBytes {
                    path: path.to_owned(),
                    tier: source.tier(),
                    bytes,
                })
            })
        })
    }

    fn content_bytes_from_all_sources(&self, path: &str) -> Vec<ResolvedContentBytes> {
        let Some(path) = normalize_archive_path(path) else {
            return Vec::new();
        };
        self.sources()
            .filter_map(|source| {
                source.entry_bytes(&path).map(|bytes| ResolvedContentBytes {
                    path: path.clone(),
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
            push_matching_paths(source.paths(), &matches, &mut seen, &mut paths);
        }
        paths
    }

    fn find_entry_by_source<T, P: AsRef<str>>(
        &self,
        paths: &[P],
        mut consume: impl FnMut(&str, Vec<u8>) -> Option<T>,
    ) -> Option<T> {
        self.sources().find_map(|source| {
            paths.iter().find_map(|path| {
                let path = path.as_ref();
                source
                    .entry_bytes(path)
                    .and_then(|bytes| consume(path, bytes))
            })
        })
    }
}

/// One content source a material/texture/sound lookup can be read from, in
/// the tier order `sources()` yields them.
enum SourceRef<'a> {
    Prepended(&'a HashMap<String, Vec<u8>>),
    Pakfile(&'a PakSource),
    Addon(&'a PreviewArchiveSource),
    Loose(&'a LooseSourceDir),
    SiblingGma(&'a SiblingGmaIndex),
    GameVpk(&'a VpkArchive),
}

impl SourceRef<'_> {
    fn tier(&self) -> ContentSourceTier {
        match self {
            Self::Prepended(_) => ContentSourceTier::Prepended,
            Self::Pakfile(_) => ContentSourceTier::Pakfile,
            Self::Addon(_) => ContentSourceTier::Addon,
            Self::Loose(_) => ContentSourceTier::Loose,
            Self::SiblingGma(_) => ContentSourceTier::SiblingGma,
            Self::GameVpk(_) => ContentSourceTier::GameVpk,
        }
    }

    fn entry_bytes(&self, path: &str) -> Option<Vec<u8>> {
        match self {
            Self::Prepended(prepended) => prepended.get(path).cloned(),
            Self::Pakfile(pakfile) => pakfile.entry_bytes(path),
            Self::Addon(addon) => addon.entry_bytes(path).ok(),
            Self::Loose(loose_dir) => loose_dir.entry_bytes(path),
            Self::SiblingGma(sibling_gmas) => sibling_gmas.entry_bytes(path),
            Self::GameVpk(vpk) => vpk.entry_bytes(path).ok(),
        }
    }

    fn paths(&self) -> Vec<String> {
        match self {
            Self::Prepended(prepended) => prepended.keys().cloned().collect(),
            Self::Pakfile(pakfile) => pakfile.paths(),
            Self::Addon(addon) => addon
                .entries()
                .into_iter()
                .map(|entry| entry.path)
                .collect(),
            Self::Loose(loose_dir) => loose_dir.paths(),
            Self::SiblingGma(sibling_gmas) => sibling_gmas.paths(),
            Self::GameVpk(vpk) => vpk
                .entries()
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
        }
    }
}

/// Frame 0 / face 0 / mip 0 of a VTF as RGBA8 — the app's standard
/// "show me this texture" decode.
pub fn decode_vtf_rgba(bytes: &[u8]) -> Result<vformats::vtf::RgbaImage, vformats::vtf::VtfError> {
    vformats::vtf::parse(bytes, &Limits::default())?.decode_rgba(0, 0, 0, &Limits::default())
}

fn push_matching_paths<'a, I, P>(
    paths: I,
    matches: &impl Fn(&str) -> bool,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<String>,
) where
    I: IntoIterator<Item = P>,
    P: AsRef<str> + 'a,
{
    for path in paths {
        let Some(path) = normalize_archive_path(path.as_ref()) else {
            continue;
        };
        if matches(&path) && seen.insert(path.clone()) {
            out.push(path);
        }
    }
}

fn downscale_resolved_texture(
    texture: ResolvedTexture,
    max_dimension: Option<u32>,
) -> ResolvedTexture {
    let Some(max_dimension) = max_dimension else {
        return texture;
    };
    let source_width = texture.width;
    let source_height = texture.height;
    let Some((width, height)) =
        downscaled_texture_dimensions(source_width, source_height, max_dimension)
    else {
        return texture;
    };
    if width == source_width && height == source_height {
        return texture;
    }
    let Some(expected_len) = rgba_len(source_width, source_height) else {
        return texture;
    };
    let ResolvedTexture {
        payload,
        original_width,
        original_height,
        water_fallback,
        ..
    } = texture;
    let ResolvedTexturePayload::Rgba { rgba, .. } = payload else {
        return ResolvedTexture {
            payload,
            width: source_width,
            height: source_height,
            original_width,
            original_height,
            water_fallback,
        };
    };
    if rgba.len() != expected_len {
        return ResolvedTexture {
            payload: ResolvedTexturePayload::Rgba {
                rgba,
                mip_chain: Vec::new(),
            },
            width: source_width,
            height: source_height,
            original_width,
            original_height,
            water_fallback,
        };
    }
    let image = ::image::RgbaImage::from_raw(source_width, source_height, rgba)
        .expect("RGBA length was checked before resize");
    let resized = ::image::imageops::resize(
        &image,
        width,
        height,
        ::image::imageops::FilterType::Triangle,
    );
    ResolvedTexture::rgba(
        resized.into_raw(),
        width,
        height,
        original_width,
        original_height,
        water_fallback,
    )
}

fn with_generated_mip_chain(mut tex: ResolvedTexture) -> ResolvedTexture {
    if tex.water_fallback {
        return tex;
    }
    if let ResolvedTexturePayload::Rgba { rgba, mip_chain } = &mut tex.payload {
        *mip_chain = generate_srgb_mip_chain(rgba, tex.width, tex.height).unwrap_or_default();
    }
    tex
}

fn resolved_bc_texture(bytes: &[u8], max_dimension: Option<u32>) -> Option<ResolvedTexture> {
    let vtf = vformats::vtf::parse(bytes, &Limits::default()).ok()?;
    let raw = vtf.raw_bc(0, 0)?;
    let mips = raw
        .mips
        .iter()
        .rev()
        .map(|mip| ResolvedBcMip {
            data: mip.data.to_vec(),
            width: mip.width.max(1),
            height: mip.height.max(1),
        })
        .collect::<Vec<_>>();
    let mips = drop_bc_mips_to_max_dimension(mips, max_dimension);
    ResolvedTexture::bc(raw.format, mips, raw.width.max(1), raw.height.max(1))
}

fn drop_bc_mips_to_max_dimension(
    mut mips: Vec<ResolvedBcMip>,
    max_dimension: Option<u32>,
) -> Vec<ResolvedBcMip> {
    let Some(max_dimension) = max_dimension.filter(|dimension| *dimension > 0) else {
        return mips;
    };
    while mips.len() > 1
        && mips
            .first()
            .is_some_and(|mip| mip.width.max(mip.height) > max_dimension)
    {
        mips.remove(0);
    }
    mips
}

pub fn generate_srgb_mip_chain(
    base_rgba: &[u8],
    width: u32,
    height: u32,
) -> Option<Vec<ResolvedTextureMip>> {
    if width == 0 || height == 0 || base_rgba.len() != rgba_len(width, height)? {
        return None;
    }

    let mut levels = Vec::new();
    let mut previous_rgba = base_rgba.to_vec();
    let mut previous_width = width;
    let mut previous_height = height;

    while previous_width > 1 || previous_height > 1 {
        let next_width = previous_width.div_ceil(2);
        let next_height = previous_height.div_ceil(2);
        let next_rgba = downsample_srgb_mip_level(&previous_rgba, previous_width, previous_height)?;

        levels.push(ResolvedTextureMip {
            rgba: next_rgba.clone(),
            width: next_width,
            height: next_height,
        });
        previous_rgba = next_rgba;
        previous_width = next_width;
        previous_height = next_height;
    }

    Some(levels)
}

fn downsample_srgb_mip_level(rgba: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || rgba.len() != rgba_len(width, height)? {
        return None;
    }
    let next_width = width.div_ceil(2);
    let next_height = height.div_ceil(2);
    let mut next = Vec::with_capacity(rgba_len(next_width, next_height)?);

    for y in 0..next_height {
        for x in 0..next_width {
            let mut rgb_linear = [0.0_f32; 3];
            let mut alpha = 0.0_f32;
            let mut count = 0.0_f32;
            for source_y in (y * 2)..((y * 2 + 2).min(height)) {
                for source_x in (x * 2)..((x * 2 + 2).min(width)) {
                    let offset = rgba_offset(source_x, source_y, width)?;
                    rgb_linear[0] += srgb_byte_to_linear(rgba[offset]);
                    rgb_linear[1] += srgb_byte_to_linear(rgba[offset + 1]);
                    rgb_linear[2] += srgb_byte_to_linear(rgba[offset + 2]);
                    alpha += f32::from(rgba[offset + 3]);
                    count += 1.0;
                }
            }
            next.push(linear_to_srgb_byte(rgb_linear[0] / count));
            next.push(linear_to_srgb_byte(rgb_linear[1] / count));
            next.push(linear_to_srgb_byte(rgb_linear[2] / count));
            next.push((alpha / count).round().clamp(0.0, 255.0) as u8);
        }
    }

    Some(next)
}

fn rgba_offset(x: u32, y: u32, width: u32) -> Option<usize> {
    let pixel = u64::from(y)
        .checked_mul(u64::from(width))?
        .checked_add(u64::from(x))?;
    usize::try_from(pixel.checked_mul(4)?).ok()
}

fn downscaled_texture_dimensions(
    width: u32,
    height: u32,
    max_dimension: u32,
) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || max_dimension == 0 {
        return None;
    }
    if width <= max_dimension && height <= max_dimension {
        return Some((width, height));
    }
    let scale = f64::from(max_dimension) / f64::from(width.max(height));
    let scaled_width = (f64::from(width) * scale)
        .round()
        .clamp(1.0, f64::from(max_dimension)) as u32;
    let scaled_height = (f64::from(height) * scale)
        .round()
        .clamp(1.0, f64::from(max_dimension)) as u32;
    Some((scaled_width, scaled_height))
}

fn force_opaque_alpha(rgba: &mut [u8]) {
    for alpha in rgba.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }
}

fn bc_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        panic::catch_unwind(|| {
            let instance = wgpu::Instance::default();
            block_on_worker(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .is_ok_and(|adapter| {
                    adapter
                        .features()
                        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
                })
        })
        .unwrap_or(false)
    })
}

fn block_on_worker<F: std::future::Future>(future: F) -> F::Output {
    futures::executor::block_on(future)
}

fn rgba_len(width: u32, height: u32) -> Option<usize> {
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    let bytes = pixels.checked_mul(4)?;
    usize::try_from(bytes).ok()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EffectiveMaterial {
    shader: String,
    base_texture: Option<String>,
    base_texture2: Option<String>,
    fog_color: Option<String>,
    alpha_test: bool,
    translucent: bool,
    additive: bool,
}

impl EffectiveMaterial {
    fn from_document(document: &vformats::vmt::VmtDocument<'_>) -> Self {
        Self {
            shader: document.shader.to_string(),
            base_texture: document
                .value("$basetexture")
                .and_then(normalize_texture_name),
            base_texture2: document
                .value("$basetexture2")
                .and_then(normalize_texture_name),
            fog_color: document.value("$fogcolor").map(str::to_owned),
            alpha_test: vmt_bool(document.value("$alphatest")),
            translucent: vmt_bool(document.value("$translucent")),
            additive: vmt_bool(document.value("$additive")),
        }
    }

    fn render_mode(&self) -> RenderMode {
        if is_water_shader(&self.shader) {
            if self.alpha_test {
                RenderMode::Cutout
            } else {
                RenderMode::Opaque
            }
        } else if self.additive {
            RenderMode::Additive
        } else if self.translucent {
            RenderMode::Translucent
        } else if self.alpha_test {
            RenderMode::Cutout
        } else {
            RenderMode::Opaque
        }
    }

    fn apply_patch_values(
        &mut self,
        document: &vformats::vmt::VmtDocument<'_>,
        patch: &vformats::vmt::VmtPatch<'_>,
    ) {
        if let Some(value) = document
            .value("$basetexture")
            .and_then(normalize_texture_name)
        {
            self.base_texture = Some(value);
        }
        if let Some(value) = patch.value("$basetexture").and_then(normalize_texture_name) {
            self.base_texture = Some(value);
        }
        if let Some(value) = document
            .value("$basetexture2")
            .and_then(normalize_texture_name)
        {
            self.base_texture2 = Some(value);
        }
        if let Some(value) = patch
            .value("$basetexture2")
            .and_then(normalize_texture_name)
        {
            self.base_texture2 = Some(value);
        }
        if let Some(value) = document.value("$fogcolor") {
            self.fog_color = Some(value.to_owned());
        }
        if let Some(value) = patch.value("$fogcolor") {
            self.fog_color = Some(value.to_owned());
        }
        if let Some(value) = document.value("$alphatest") {
            self.alpha_test = vmt_bool(Some(value));
        }
        if let Some(value) = patch.value("$alphatest") {
            self.alpha_test = vmt_bool(Some(value));
        }
        if let Some(value) = document.value("$translucent") {
            self.translucent = vmt_bool(Some(value));
        }
        if let Some(value) = patch.value("$translucent") {
            self.translucent = vmt_bool(Some(value));
        }
        if let Some(value) = document.value("$additive") {
            self.additive = vmt_bool(Some(value));
        }
        if let Some(value) = patch.value("$additive") {
            self.additive = vmt_bool(Some(value));
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

fn parse_water_fog_color(value: &str) -> Option<[f32; 3]> {
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
        .then_some([red, green, blue])
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
    template: MapPakFile,
    readers: Mutex<Vec<MapPakFile>>,
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
        let reader = pakfile.clone();
        Some(Self {
            template: pakfile,
            readers: Mutex::new(vec![reader]),
            entries,
        })
    }

    fn entry_bytes(&self, path: &str) -> Option<Vec<u8>> {
        let path = normalize_archive_path(path)?;
        let index = *self.entries.get(&path)?;
        let reader = self.checkout_reader();
        let result = reader.entry_bytes_by_index(index);
        self.checkin_reader(reader);
        match result {
            Ok(bytes) => bytes,
            Err(error) => {
                log::debug!("bsp pakfile entry fetch failed for {path}: {error}");
                None
            }
        }
    }

    fn paths(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    fn checkout_reader(&self) -> MapPakFile {
        self.readers
            .lock()
            .pop()
            .unwrap_or_else(|| self.template.clone())
    }

    fn checkin_reader(&self, reader: MapPakFile) {
        self.readers.lock().push(reader);
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

    fn entry_bytes(&self, path: &str) -> Option<Vec<u8>> {
        let path = normalize_archive_path(path)?;
        let mut candidate = self.root.clone();
        for segment in path.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return None;
            }
            candidate.push(segment);
        }
        fs::read(candidate).ok()
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

#[derive(Debug)]
struct SiblingGmaIndex {
    archives: Vec<SiblingGmaArchive>,
    entries: HashMap<String, SiblingGmaEntryRef>,
    legacy_bin_cache: Mutex<Option<LegacyBinCache>>,
}

impl SiblingGmaIndex {
    fn entry_bytes(&self, path: &str) -> Option<Vec<u8>> {
        let normalized = normalize_archive_path(path)?;
        let entry = self.entries.get(&normalized)?;
        let archive = self.archives.get(entry.archive_index)?;
        match (&archive.kind, &entry.location) {
            (
                SiblingGmaArchiveKind::Plain { view, .. },
                SiblingGmaEntryLocation::Plain { entry_path },
            ) => view.read_entry_bytes(entry_path).ok(),
            (
                SiblingGmaArchiveKind::LegacyBin { path, data_end },
                SiblingGmaEntryLocation::LegacyBin { offset, len },
            ) => self.legacy_bin_entry_bytes(entry.archive_index, path, *data_end, *offset, *len),
            _ => None,
        }
    }

    fn paths(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
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
struct SiblingGmaArchive {
    kind: SiblingGmaArchiveKind,
}

/// `view` has no `Debug` of its own (it wraps a memory map); the derive
/// on this enum only needs `gma`.
enum SiblingGmaArchiveKind {
    Plain {
        gma: Box<gmpublished_backend::GMAFile>,
        view: Box<gmpublished_backend::gma::read::GmaView>,
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
struct LegacyBinCache {
    archive_index: usize,
    bytes: Arc<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SiblingGmaEntryRef {
    archive_index: usize,
    location: SiblingGmaEntryLocation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SiblingGmaEntryLocation {
    Plain { entry_path: String },
    LegacyBin { offset: u64, len: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SiblingGmaPath {
    path: PathBuf,
    kind: SiblingGmaPathKind,
}

impl SiblingGmaPath {
    fn plain(path: PathBuf) -> Self {
        Self {
            path,
            kind: SiblingGmaPathKind::Plain,
        }
    }

    fn legacy_bin(path: PathBuf) -> Self {
        Self {
            path,
            kind: SiblingGmaPathKind::LegacyBin,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SiblingGmaPathKind {
    Plain,
    LegacyBin,
}

#[derive(Debug)]
struct LegacyBinEntry {
    normalized_path: String,
    offset: u64,
    len: u64,
}

#[derive(Debug)]
struct LegacyBinIndex {
    entries: Vec<LegacyBinEntry>,
    data_end: u64,
}

fn build_sibling_gma_index(paths: &[SiblingGmaPath]) -> SiblingGmaIndex {
    let skipped = paths.len().saturating_sub(MAX_SIBLING_GMA_ARCHIVES);
    if skipped > 0 {
        log::debug!(
            "skipping {skipped} sibling material GMA archives over cap {MAX_SIBLING_GMA_ARCHIVES}"
        );
    }

    let mut archives = Vec::new();
    let mut entries = HashMap::new();
    for path in paths.iter().take(MAX_SIBLING_GMA_ARCHIVES) {
        let archive_index = archives.len();
        let archive = match path.kind {
            SiblingGmaPathKind::Plain => {
                let gma = match gmpublished_backend::GMAFile::open(&path.path) {
                    Ok(gma) => gma,
                    Err(error) => {
                        log::debug!(
                            "sibling material GMA open failed for {}: {error}",
                            path.path.display()
                        );
                        continue;
                    }
                };
                let view = match gma.view() {
                    Ok(view) => view,
                    Err(error) => {
                        log::debug!(
                            "sibling material GMA view failed for {}: {error}",
                            path.path.display()
                        );
                        continue;
                    }
                };
                let gma_entries = match view.entries() {
                    Ok(entries) => entries,
                    Err(error) => {
                        log::debug!(
                            "sibling material GMA entry table failed for {}: {error}",
                            path.path.display()
                        );
                        continue;
                    }
                };

                for entry in gma_entries.values() {
                    if let Some(normalized) = normalize_archive_path(&entry.path) {
                        entries
                            .entry(normalized)
                            .or_insert_with(|| SiblingGmaEntryRef {
                                archive_index,
                                location: SiblingGmaEntryLocation::Plain {
                                    entry_path: entry.path.clone(),
                                },
                            });
                    }
                }
                SiblingGmaArchive {
                    kind: SiblingGmaArchiveKind::Plain {
                        gma: Box::new(gma),
                        view: Box::new(view),
                    },
                }
            }
            SiblingGmaPathKind::LegacyBin => {
                let legacy_index = match read_legacy_bin_index(&path.path) {
                    Ok(index) => index,
                    Err(error) => {
                        log::debug!(
                            "sibling material legacy GMA index failed for {}: {error}",
                            path.path.display()
                        );
                        continue;
                    }
                };
                for entry in &legacy_index.entries {
                    entries
                        .entry(entry.normalized_path.clone())
                        .or_insert_with(|| SiblingGmaEntryRef {
                            archive_index,
                            location: SiblingGmaEntryLocation::LegacyBin {
                                offset: entry.offset,
                                len: entry.len,
                            },
                        });
                }
                SiblingGmaArchive {
                    kind: SiblingGmaArchiveKind::LegacyBin {
                        path: path.path.clone(),
                        data_end: legacy_index.data_end,
                    },
                }
            }
        };
        archives.push(archive);
    }

    SiblingGmaIndex {
        archives,
        entries,
        legacy_bin_cache: Mutex::new(None),
    }
}

fn read_legacy_bin_index(path: &Path) -> io::Result<LegacyBinIndex> {
    let decoder = legacy_bin_decoder(path)?;
    let mut reader = LimitedReader::new(decoder, MAX_LEGACY_BIN_ENTRY_TABLE_BYTES);
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

    let data_start = reader.bytes_read();
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

fn legacy_bin_decoder(path: &Path) -> io::Result<lzma_rust2::LzmaReader<BufReader<File>>> {
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

fn decompress_legacy_bin_prefix(path: &Path, target_len: u64) -> io::Result<Vec<u8>> {
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

fn slice_legacy_bin_entry(bytes: &[u8], offset: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let len = usize::try_from(len).ok()?;
    let end = start.checked_add(len)?;
    bytes.get(start..end)
}

struct LimitedReader<R> {
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

fn read_array<const N: usize, R: Read>(reader: &mut R) -> io::Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_u8(reader: &mut impl Read) -> io::Result<u8> {
    Ok(read_array::<1, _>(reader)?[0])
}

fn read_u32_le(reader: &mut impl Read) -> io::Result<u32> {
    Ok(u32::from_le_bytes(read_array::<4, _>(reader)?))
}

fn read_i32_le(reader: &mut impl Read) -> io::Result<i32> {
    Ok(i32::from_le_bytes(read_array::<4, _>(reader)?))
}

fn read_u64_le(reader: &mut impl Read) -> io::Result<u64> {
    Ok(u64::from_le_bytes(read_array::<8, _>(reader)?))
}

fn read_i64_le(reader: &mut impl Read) -> io::Result<i64> {
    Ok(i64::from_le_bytes(read_array::<8, _>(reader)?))
}

fn read_nt_string(reader: &mut impl Read) -> io::Result<String> {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        reader.read_exact(&mut byte)?;
        if byte[0] == 0 {
            break;
        }
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn discover_loose_source_dirs(gmod_dir: &Path) -> Vec<LooseSourceDir> {
    let mut dirs = Vec::new();
    let garrysmod = gmod_dir.join("garrysmod");
    push_loose_source_dir(&mut dirs, garrysmod.clone());

    let addons = garrysmod.join("addons");
    for addon_path in sorted_child_paths(&addons) {
        if addon_path.is_dir() {
            push_loose_source_dir(&mut dirs, addon_path);
        }
    }

    push_loose_source_dir(&mut dirs, garrysmod.join("download"));
    dirs
}

fn push_loose_source_dir(dirs: &mut Vec<LooseSourceDir>, path: PathBuf) {
    if path.is_dir() {
        dirs.push(LooseSourceDir::new(path));
    }
}

fn discover_sibling_gma_paths(gmod_dir: &Path) -> Vec<SiblingGmaPath> {
    let mut paths = Vec::new();
    if let Ok(workshop_dir) = fs::canonicalize(gmod_dir.join("../../workshop/content/4000")) {
        for workshop_item in sorted_child_paths(&workshop_dir) {
            if workshop_item.is_dir() {
                let children = sorted_child_paths(&workshop_item);
                let plain_gmas = children
                    .iter()
                    .filter(|path| path.is_file() && is_plain_gma_path(path))
                    .cloned()
                    .collect::<Vec<_>>();
                if plain_gmas.is_empty() {
                    paths.extend(
                        children
                            .into_iter()
                            .filter(|path| path.is_file() && is_legacy_bin_path(path))
                            .map(SiblingGmaPath::legacy_bin),
                    );
                } else {
                    paths.extend(plain_gmas.into_iter().map(SiblingGmaPath::plain));
                }
            }
        }
    }

    for path in sorted_child_paths(&gmod_dir.join("garrysmod/addons")) {
        if path.is_file() && is_plain_gma_path(&path) {
            paths.push(SiblingGmaPath::plain(path));
        }
    }

    collect_download_gma_paths(&gmod_dir.join("garrysmod/download"), 0, &mut paths);

    paths
}

fn collect_download_gma_paths(dir: &Path, depth: usize, paths: &mut Vec<SiblingGmaPath>) {
    if depth > 3 {
        return;
    }
    for path in sorted_child_paths(dir) {
        if path.is_file() && is_plain_gma_path(&path) {
            paths.push(SiblingGmaPath::plain(path));
        } else if path.is_dir() {
            collect_download_gma_paths(&path, depth + 1, paths);
        }
    }
}

fn sorted_child_paths(dir: &Path) -> Vec<PathBuf> {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths
}

fn is_plain_gma_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gma"))
}

fn is_legacy_bin_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("bin"))
}

fn material_paths(material_dirs: &[String], material_name: &str) -> Vec<String> {
    let Some(name) = normalize_material_name(material_name) else {
        return Vec::new();
    };
    let dirs = normalized_material_dirs(material_dirs);
    let mut paths = Vec::with_capacity(dirs.len() + 1);
    for dir in dirs {
        let path = if dir.is_empty() {
            format!("materials/{name}.vmt")
        } else {
            format!("materials/{dir}/{name}.vmt")
        };
        push_unique(&mut paths, path);
    }
    if let Some(depatched) = cubemap_depatched_material_name(&name) {
        push_unique(&mut paths, format!("materials/{depatched}.vmt"));
    }
    paths
}

fn texture_path(base_texture: &str) -> Option<String> {
    normalize_texture_name(base_texture).map(|texture| format!("materials/{texture}.vtf"))
}

fn normalized_material_dirs(material_dirs: &[String]) -> Vec<String> {
    let mut dirs = Vec::new();
    if material_dirs.is_empty() {
        dirs.push(String::new());
        return dirs;
    }

    for dir in material_dirs {
        let normalized = normalize_source_path(dir, None).unwrap_or_default();
        push_unique(&mut dirs, normalized);
    }
    if dirs.is_empty() {
        dirs.push(String::new());
    }
    dirs
}

fn normalize_material_name(material_name: &str) -> Option<String> {
    normalize_source_path(material_name, Some(".vmt"))
}

fn normalize_texture_name(texture_name: &str) -> Option<String> {
    normalize_source_path(texture_name, Some(".vtf"))
}

fn cubemap_depatched_material_name(material_name: &str) -> Option<String> {
    let without_maps = strip_prefix_ascii_case(material_name, "maps/")?;
    let (_, original) = without_maps.split_once('/')?;
    let suffix_start = cubemap_suffix_start(original)?;
    let original = &original[..suffix_start];
    (!original.is_empty()).then(|| original.to_owned())
}

fn cubemap_suffix_start(value: &str) -> Option<usize> {
    let z_start = trailing_group_start(value)?;
    let y_start = trailing_group_start(value.get(..z_start)?)?;
    let x_start = trailing_group_start(value.get(..y_start)?)?;
    Some(x_start)
}

fn trailing_group_start(value: &str) -> Option<usize> {
    let (prefix, group) = value.rsplit_once('_')?;
    (is_signed_integer(group) && !prefix.is_empty()).then_some(prefix.len())
}

fn is_signed_integer(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn normalize_source_path(path: &str, extension: Option<&str>) -> Option<String> {
    let mut path = path.trim().replace('\\', "/");
    path = path.trim_matches('/').to_owned();
    if let Some(stripped) = strip_prefix_ascii_case(&path, "materials/") {
        path = stripped.to_owned();
    }
    if let Some(extension) = extension
        && path
            .get(path.len().saturating_sub(extension.len())..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(extension))
    {
        path.truncate(path.len() - extension.len());
    }

    let path = path
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect::<Vec<_>>()
        .join("/");

    (!path.is_empty() || extension.is_none()).then_some(path.to_ascii_lowercase())
}

fn normalize_archive_path(path: &str) -> Option<String> {
    let path = path.trim().replace('\\', "/");
    let path = path.trim_matches('/');
    let mut normalized = Vec::new();
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return None;
        }
        normalized.push(segment);
    }
    let path = normalized.join("/");
    (!path.is_empty()).then_some(path.to_ascii_lowercase())
}

fn strip_prefix_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .then(|| &value[prefix.len()..])
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests;
