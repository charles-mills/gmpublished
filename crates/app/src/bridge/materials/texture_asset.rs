use super::{ContentPath, ContentSourceTier};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    ops::Range,
    sync::{Arc, OnceLock},
};
use vformats::vtf::{BcFormat, VtfError, VtfFormat};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TextureStorage {
    Rgba8,
    Bc(BcFormat),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TextureMetadata {
    pub flags: Option<u32>,
    pub original_dimensions: [u32; 2],
    pub frame_count: u32,
    pub face_count: u32,
    pub mip_count: u8,
    pub format: Option<VtfFormat>,
    pub first_frame: u16,
    pub version_minor: Option<u32>,
    pub reflectivity_bits: Option<[u32; 3]>,
    pub bumpmap_scale_bits: Option<u32>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum AssetKind {
    Source {
        token: super::TextureSourceId,
        metadata: TextureMetadata,
    },
    Page {
        metadata: TextureMetadata,
    },
    Generated {
        namespace: String,
        metadata: TextureMetadata,
    },
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct TextureAssetId {
    digest: [u8; 32],
    kind: AssetKind,
    encoded_len: usize,
}

impl fmt::Debug for TextureAssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("TextureAssetId");
        match &self.kind {
            AssetKind::Source { token, .. } => {
                debug.field("source_token", token);
            }
            _ => {
                debug.field("sha256", &format_args!("{:02x?}", self.digest));
            }
        }
        debug
            .field("kind", &self.kind)
            .field("byte_len", &self.encoded_len)
            .finish()
    }
}

impl TextureAssetId {
    fn new(kind: AssetKind, bytes: &[u8]) -> Self {
        Self {
            digest: Sha256::digest(bytes).into(),
            kind,
            encoded_len: bytes.len(),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TextureResidentId {
    pub asset: TextureAssetId,
    pub frames: Range<u32>,
    pub faces: Range<u32>,
    pub first_source_mip: u8,
    pub mip_count: u8,
    pub storage: TextureStorage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextureResidency {
    Resident(Arc<ResidentTexture>),
    OnDemand {
        source: Arc<super::ReloadableTextureSource>,
        recipe: super::TextureFrameRequest,
        id: TextureResidentId,
        #[cfg(test)]
        test_read: Arc<TestTextureRead>,
    },
}

#[cfg(test)]
#[derive(Debug, Default)]
pub struct TestTextureRead(std::sync::OnceLock<Arc<ResidentTexture>>);
#[cfg(test)]
impl PartialEq for TestTextureRead {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
#[cfg(test)]
impl Eq for TestTextureRead {}

impl TextureResidency {
    /// Explicitly read authored pages for format regression assertions. Runtime callers
    /// keep using `resident` and `on_demand`, so tests never hide a missing GPU page.
    #[cfg(test)]
    pub(crate) fn read_for_test(&self) -> &Arc<ResidentTexture> {
        match self {
            Self::Resident(resident) => resident,
            Self::OnDemand {
                source,
                recipe,
                test_read,
                ..
            } => test_read.0.get_or_init(|| {
                let mut layers = Vec::new();
                let working = Arc::new(super::TextureWorkingBudget::new(usize::MAX));
                for frame in 0..source.metadata().frame_count {
                    let mut request = recipe.clone();
                    request.frame = frame;
                    let page = source
                        .load_page(&request, &Default::default(), &working)
                        .expect("fixture streaming page must load");
                    layers.extend(page.resident().layers().iter().map(|layer| {
                        ResidentTextureLayer {
                            frame: layer.frame,
                            face: layer.face,
                            mips: layer
                                .mips
                                .iter()
                                .map(|mip| ResidentTextureMip {
                                    source_mip: mip.source_mip,
                                    dimensions: mip.dimensions,
                                    bytes: TextureBytes::owned(mip.bytes.as_slice().to_vec()),
                                })
                                .collect(),
                        }
                    }));
                }
                ResidentTexture::from_page(
                    source.metadata_arc(),
                    0..source.metadata().frame_count,
                    recipe.faces.clone(),
                    recipe.first_source_mip..recipe.first_source_mip + recipe.mip_count,
                    recipe.storage,
                    layers,
                )
                .expect("validated fixture pages")
            }),
        }
    }
    pub(crate) fn resident(&self) -> Option<&Arc<ResidentTexture>> {
        if let Self::Resident(resident) = self {
            Some(resident)
        } else {
            None
        }
    }
    pub(crate) fn on_demand(
        &self,
    ) -> Option<(
        &Arc<super::ReloadableTextureSource>,
        &super::TextureFrameRequest,
    )> {
        match self {
            Self::OnDemand { source, recipe, .. } => Some((source, recipe)),
            Self::Resident(_) => None,
        }
    }
    pub(super) fn on_demand_source(
        source: Arc<super::ReloadableTextureSource>,
        recipe: super::TextureFrameRequest,
    ) -> Self {
        let id = TextureResidentId {
            asset: TextureAssetId {
                digest: [0; 32],
                kind: AssetKind::Source {
                    token: source.source_id(),
                    metadata: source.metadata().clone(),
                },
                encoded_len: 0,
            },
            frames: recipe.frame..recipe.frame + 1,
            faces: recipe.faces.clone(),
            first_source_mip: recipe.first_source_mip,
            mip_count: recipe.mip_count,
            storage: recipe.storage,
        };
        Self::OnDemand {
            source,
            recipe,
            id,
            #[cfg(test)]
            test_read: Arc::default(),
        }
    }
    pub(crate) fn id(&self) -> &TextureResidentId {
        match self {
            Self::Resident(resident) => resident.id(),
            Self::OnDemand { id, .. } => id,
        }
    }
    pub(super) fn load_outcome(&self) -> super::TextureLoadOutcome {
        match self {
            Self::Resident(_) | Self::OnDemand { .. } => super::TextureLoadOutcome::Loaded,
        }
    }
    #[cfg(test)]
    pub(super) fn dimensions(&self, metadata: &TextureMetadata) -> [u32; 2] {
        self.resident().map_or_else(
            || {
                metadata.original_dimensions.map(|side| {
                    side.checked_shr(u32::from(self.id().first_source_mip))
                        .unwrap_or(0)
                        .max(1)
                })
            },
            |resident| resident.layers()[0].mips[0].dimensions,
        )
    }
}

#[derive(Eq, PartialEq)]
pub struct TextureBytes {
    backing: Arc<[u8]>,
    range: Range<usize>,
}

impl fmt::Debug for TextureBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextureBytes")
            .field("range", &self.range)
            .field("backing_len", &self.backing.len())
            .finish()
    }
}

impl TextureBytes {
    #[cfg(test)]
    pub(super) fn backing_len(&self) -> usize {
        self.backing.len()
    }

    pub(super) fn owned(bytes: Vec<u8>) -> Self {
        let range = 0..bytes.len();
        Self {
            backing: bytes.into(),
            range,
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.backing[self.range.clone()]
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct ResidentTextureMip {
    pub source_mip: u8,
    pub dimensions: [u32; 2],
    pub bytes: TextureBytes,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ResidentTextureLayer {
    pub frame: u32,
    pub face: u32,
    pub mips: Vec<ResidentTextureMip>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ResidentTexture {
    id: TextureResidentId,
    metadata: Arc<TextureMetadata>,
    layers: Vec<ResidentTextureLayer>,
}

impl ResidentTexture {
    pub(super) fn from_page(
        metadata: Arc<TextureMetadata>,
        frames: Range<u32>,
        faces: Range<u32>,
        mips: Range<u8>,
        storage: TextureStorage,
        layers: Vec<ResidentTextureLayer>,
    ) -> Result<Arc<Self>, TextureAssetError> {
        if frames.is_empty() || faces.is_empty() || mips.is_empty() {
            return Err(TextureAssetError::Selection);
        }
        let mut hash = Sha256::new();
        let mut byte_len = 0usize;
        for mip in layers.iter().flat_map(|layer| &layer.mips) {
            hash.update(mip.bytes.as_slice());
            byte_len = byte_len
                .checked_add(mip.bytes.as_slice().len())
                .ok_or(TextureAssetError::WorkingCostOverflow)?;
        }
        let asset = TextureAssetId {
            digest: hash.finalize().into(),
            kind: AssetKind::Page {
                metadata: (*metadata).clone(),
            },
            encoded_len: byte_len,
        };
        let resident = Self {
            id: TextureResidentId {
                asset,
                frames,
                faces,
                first_source_mip: mips.start,
                mip_count: mips.end - mips.start,
                storage,
            },
            metadata,
            layers,
        };
        resident.validate()?;
        Ok(Arc::new(resident))
    }

    pub fn id(&self) -> &TextureResidentId {
        &self.id
    }
    pub fn metadata(&self) -> &TextureMetadata {
        &self.metadata
    }
    pub(super) fn metadata_arc(&self) -> Arc<TextureMetadata> {
        Arc::clone(&self.metadata)
    }
    pub fn layers(&self) -> &[ResidentTextureLayer] {
        &self.layers
    }

    pub fn layer_index(&self, frame: u32, face: u32) -> Option<u32> {
        if !self.id.frames.contains(&frame) || !self.id.faces.contains(&face) {
            return None;
        }
        (frame - self.id.frames.start)
            .checked_mul(self.id.faces.end - self.id.faces.start)?
            .checked_add(face - self.id.faces.start)
    }

    #[cfg(test)]
    pub fn layer(&self, frame: u32, face: u32) -> Option<&ResidentTextureLayer> {
        self.layers.get(self.layer_index(frame, face)? as usize)
    }

    #[cfg(test)]
    pub fn byte_len(&self) -> usize {
        self.layers
            .iter()
            .flat_map(|layer| &layer.mips)
            .map(|mip| mip.bytes.as_slice().len())
            .sum()
    }

    pub(super) fn generated(
        namespace: &str,
        metadata: TextureMetadata,
        frames: Range<u32>,
        faces: Range<u32>,
        layers: Vec<ResidentTextureLayer>,
        storage: TextureStorage,
    ) -> Result<Arc<Self>, TextureAssetError> {
        let first = layers
            .first()
            .and_then(|layer| layer.mips.first())
            .ok_or(TextureAssetError::Selection)?;
        let first_source_mip = first.source_mip;
        let mip_count =
            u8::try_from(layers[0].mips.len()).map_err(|_| TextureAssetError::Selection)?;
        let mut identity_bytes = Vec::new();
        for layer in &layers {
            for mip in &layer.mips {
                identity_bytes.extend_from_slice(mip.bytes.as_slice());
            }
        }
        let backing: Arc<[u8]> = identity_bytes.into();
        let mut layers = layers;
        let mut offset = 0;
        for layer in &mut layers {
            for mip in &mut layer.mips {
                let end = offset + mip.bytes.as_slice().len();
                mip.bytes = TextureBytes {
                    backing: Arc::clone(&backing),
                    range: offset..end,
                };
                offset = end;
            }
        }
        let asset = TextureAssetId::new(
            AssetKind::Generated {
                namespace: namespace.to_owned(),
                metadata: metadata.clone(),
            },
            &backing,
        );
        let selected = Self {
            id: TextureResidentId {
                asset,
                frames,
                faces,
                first_source_mip,
                mip_count,
                storage,
            },
            metadata: Arc::new(metadata),
            layers,
        };
        selected.validate()?;
        Ok(Arc::new(selected))
    }

    fn validate(&self) -> Result<(), TextureAssetError> {
        if self.metadata.original_dimensions.contains(&0) {
            return Err(TextureAssetError::Selection);
        }
        let frames = self
            .id
            .frames
            .end
            .checked_sub(self.id.frames.start)
            .ok_or(TextureAssetError::Selection)?;
        let faces = self
            .id
            .faces
            .end
            .checked_sub(self.id.faces.start)
            .ok_or(TextureAssetError::Selection)?;
        if frames == 0
            || faces == 0
            || frames.checked_mul(faces).map(|count| count as usize) != Some(self.layers.len())
            || self.id.mip_count == 0
        {
            return Err(TextureAssetError::Selection);
        }
        for (index, layer) in self.layers.iter().enumerate() {
            if layer.frame != self.id.frames.start + index as u32 / faces
                || layer.face != self.id.faces.start + index as u32 % faces
                || layer.mips.len() != usize::from(self.id.mip_count)
            {
                return Err(TextureAssetError::Selection);
            }
            for (level, mip) in layer.mips.iter().enumerate() {
                if usize::from(mip.source_mip) != usize::from(self.id.first_source_mip) + level
                    || mip.dimensions
                        != self.metadata.original_dimensions.map(|side| {
                            side.checked_shr(u32::from(mip.source_mip))
                                .unwrap_or(0)
                                .max(1)
                        })
                {
                    return Err(TextureAssetError::Selection);
                }
                let [width, height] = mip.dimensions.map(u64::from);
                let bytes = match self.id.storage {
                    TextureStorage::Rgba8 => width
                        .checked_mul(height)
                        .and_then(|size| size.checked_mul(4)),
                    TextureStorage::Bc(format) => width
                        .div_ceil(4)
                        .checked_mul(height.div_ceil(4))
                        .and_then(|size| size.checked_mul(u64::from(format.block_bytes()))),
                };
                if bytes != Some(mip.bytes.as_slice().len() as u64) {
                    return Err(TextureAssetError::Selection);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TextureAssetError {
    #[error(transparent)]
    Cancelled(#[from] crate::preparation::Cancelled),
    #[error(transparent)]
    ArchiveIndex(vformats::ArchiveIndexLimit),
    #[error(transparent)]
    Vtf(#[from] VtfError),
    #[error("selected texture source changed after selection")]
    SourceChanged,
    #[error("selected texture source read failed: {0}")]
    SourceRead(Arc<str>),
    #[error("texture storage does not match its source format")]
    Storage,
    #[error("no stored mip satisfies dimension limit {0}")]
    DimensionLimit(u32),
    #[error("invalid texture frame, face, or mip selection")]
    Selection,
    #[error("texture image count exceeds {0}")]
    ImageCount(usize),
    #[error(
        "texture preparation requires {required} working bytes; declared capacity is {capacity}"
    )]
    WorkingLimit { required: usize, capacity: usize },
    #[error("texture preparation working cost overflows")]
    WorkingCostOverflow,
}

pub(super) fn metadata_from_header(header: &vformats::vtf::VtfHeader) -> TextureMetadata {
    TextureMetadata {
        flags: Some(header.flags),
        original_dimensions: header.dimensions,
        frame_count: header.frames,
        face_count: header.faces,
        mip_count: header.mip_count,
        format: Some(header.format),
        first_frame: header.first_frame,
        version_minor: Some(header.version_minor),
        reflectivity_bits: Some(header.reflectivity.map(f32::to_bits)),
        bumpmap_scale_bits: Some(header.bumpmap_scale.to_bits()),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TextureOrigin {
    pub path: ContentPath,
    pub tier: ContentSourceTier,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum TextureSourceError {
    #[error("texture source {path} preparation stopped: {error}")]
    Preparation {
        path: ContentPath,
        error: TextureAssetError,
    },
    #[error("texture source is absent")]
    Missing,
    #[error(transparent)]
    SourceUnavailable(super::SourceDiscoveryFailure),
    #[error("texture source {origin:?}: working policy rejected: {error}")]
    WorkingBudget {
        origin: TextureOrigin,
        error: TextureAssetError,
        metadata: Option<Arc<TextureMetadata>>,
    },
    #[error("texture source {origin:?}: {message}")]
    Read {
        origin: TextureOrigin,
        message: Arc<str>,
    },
    #[error("texture source {origin:?}: {error}")]
    Invalid {
        origin: TextureOrigin,
        error: TextureAssetError,
    },
}

pub(super) fn generated_rgba(
    namespace: &str,
    flags: Option<u32>,
    frames: Vec<Vec<super::ResolvedTextureMip>>,
) -> Result<Arc<ResidentTexture>, TextureAssetError> {
    let first = frames
        .first()
        .and_then(|frame| frame.first())
        .ok_or(TextureAssetError::Selection)?;
    let dimensions = [first.width, first.height];
    let mip_count = u8::try_from(frames[0].len()).map_err(|_| TextureAssetError::Selection)?;
    let count = u32::try_from(frames.len()).map_err(|_| TextureAssetError::Selection)?;
    let metadata = generated_metadata(dimensions, flags, count, 1, mip_count);
    let layers = frames
        .into_iter()
        .enumerate()
        .map(|(frame, mips)| ResidentTextureLayer {
            frame: frame as u32,
            face: 0,
            mips: mips
                .into_iter()
                .enumerate()
                .map(|(level, mip)| ResidentTextureMip {
                    source_mip: level as u8,
                    dimensions: [mip.width, mip.height],
                    bytes: TextureBytes::owned(mip.rgba),
                })
                .collect(),
        })
        .collect();
    ResidentTexture::generated(
        namespace,
        metadata,
        0..count,
        0..1,
        layers,
        TextureStorage::Rgba8,
    )
}

pub(super) fn native_error_resident() -> Arc<ResidentTexture> {
    static IMAGE: OnceLock<Arc<ResidentTexture>> = OnceLock::new();
    Arc::clone(IMAGE.get_or_init(|| {
        generated_error_resident(
            "source-error-rgb32",
            255,
            Some(super::NATIVE_ERROR_TEXTURE_FLAGS),
        )
    }))
}

pub(super) fn missing_material_resident() -> Arc<ResidentTexture> {
    static IMAGE: OnceLock<Arc<ResidentTexture>> = OnceLock::new();
    Arc::clone(IMAGE.get_or_init(|| generated_error_resident("source-error-rgba32", 128, None)))
}

fn generated_error_resident(
    namespace: &str,
    black_alpha: u8,
    flags: Option<u32>,
) -> Arc<ResidentTexture> {
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32 {
        for x in 0..32 {
            let value = if (x / 4 + y / 4) % 2 == 0 { 255 } else { 0 };
            rgba.extend_from_slice(&[value, 0, value, if value == 0 { black_alpha } else { 255 }]);
        }
    }
    generated_rgba(
        namespace,
        flags,
        vec![vec![super::ResolvedTextureMip {
            rgba,
            width: 32,
            height: 32,
        }]],
    )
    .expect("error image has valid dimensions")
}

pub(super) fn generated_metadata(
    original_dimensions: [u32; 2],
    flags: Option<u32>,
    frame_count: u32,
    face_count: u32,
    mip_count: u8,
) -> TextureMetadata {
    TextureMetadata {
        flags,
        original_dimensions,
        frame_count,
        face_count,
        mip_count,
        format: None,
        first_frame: 0,
        version_minor: None,
        reflectivity_bits: None,
        bumpmap_scale_bits: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::materials::{MaterialResolver, TextureLoadOutcome};
    use crate::test_support::fixture_vtf_bytes;
    use gmpublished_domain::math::Vec3;
    use vformats::vtf::texture_flags;

    #[test]
    fn generated_source_error_matches_native_texture_and_is_shared() {
        let expected = include_bytes!("testdata/native-lmg-error.rgba8");
        let resident = native_error_resident();
        let mip = &resident.layers()[0].mips[0];
        assert_eq!(mip.dimensions, [32, 32]);
        assert_eq!(mip.source_mip, 0);
        assert_eq!(mip.bytes.as_slice(), expected);
        assert_eq!(crc32fast::hash(mip.bytes.as_slice()), 0x3660_0d5a);
        assert_eq!(resident.id().mip_count, 1);
        assert_eq!(
            resident.metadata().flags,
            Some(super::super::NATIVE_ERROR_TEXTURE_FLAGS)
        );
        assert!(Arc::ptr_eq(&resident, &native_error_resident()));
        let missing = missing_material_resident();
        assert_eq!(
            missing.layers()[0].mips[0].bytes.as_slice(),
            include_bytes!("testdata/native-missing-vmt-error.rgba8")
        );
        assert_ne!(missing.id(), resident.id());
        assert!(Arc::ptr_eq(&missing, &missing_material_resident()));
    }

    #[test]
    fn measured_generic_families_preserve_materials_with_native_error_images() {
        for shader in ["LightmappedGeneric", "UnlitGeneric", "VertexLitGeneric"] {
            for source in [None, Some(b"bad selected texture".to_vec())] {
                let mut files = vec![(
                    "materials/test/image.vmt",
                    format!("{shader} {{ \"$basetexture\" \"test/image\" }}").into_bytes(),
                )];
                if let Some(source) = source.clone() {
                    files.push(("materials/test/image.vtf", source));
                }
                let resolver = MaterialResolver::prop_lighting_fixture(files, Vec::new());
                let resolution = resolver.resolve_with_base2(&[], "TEST/IMAGE.VMT");
                assert_eq!(
                    resolution.outcome(),
                    super::super::MaterialResolutionOutcome::Loaded
                );
                assert_eq!(resolution.canonical_name(), Some("test/image"));
                let material = resolution.material().unwrap();
                assert_eq!(material.shader.surface().unwrap().shader, shader);
                let texture = material.texture.as_ref().unwrap();
                assert_eq!(
                    texture.load_outcome(),
                    if source.is_some() {
                        TextureLoadOutcome::InvalidSource
                    } else {
                        TextureLoadOutcome::MissingSource
                    }
                );
                assert!(texture.has_effective_base_texture());
                assert!(Arc::ptr_eq(
                    texture.read_for_test(),
                    &native_error_resident()
                ));
                assert_eq!(
                    resolver
                        .resolve_surface_lighting("test/image")
                        .unwrap()
                        .reflectivity,
                    Vec3::splat(0.5)
                );
            }
        }
    }

    #[test]
    fn native_error_primary_retains_secondary_blending_without_raw_header_alpha() {
        use super::super::{SourceSurfaceBaseBlend, SurfaceFamily};
        for (source, outcome) in [
            (None, TextureLoadOutcome::MissingSource),
            (Some(b"FAIL".to_vec()), TextureLoadOutcome::InvalidSource),
            (
                Some({
                    let mut source = rgba_asset(texture_flags::EIGHTBITALPHA);
                    source.pop();
                    source
                }),
                TextureLoadOutcome::InvalidSource,
            ),
        ] {
            let mut files = vec![
                ("materials/test/blend.vmt", br#"LightmappedGeneric { "$basetexture" "test/primary" "$basetexture2" "test/secondary" "$translucent" "1" "$nofog" "1" "$selfillum" "1" }"#.to_vec()),
                ("materials/test/secondary.vtf", rgba_asset(0)),
            ];
            if let Some(source) = source {
                files.push(("materials/test/primary.vtf", source));
            }
            let resolver = MaterialResolver::prop_lighting_fixture(files, Vec::new());
            let resolved = resolver
                .resolve_with_base2(&[], "test/blend")
                .into_material()
                .unwrap();
            let texture = resolved.texture.as_ref().unwrap();
            assert_eq!(texture.load_outcome(), outcome);
            assert!(texture.has_effective_base_texture());
            assert_eq!(
                texture.flags(),
                Some(super::super::NATIVE_ERROR_TEXTURE_FLAGS)
            );
            assert_eq!(texture.mapping_dimensions(), Some((32, 32)));
            assert!(Arc::ptr_eq(
                texture.read_for_test(),
                &native_error_resident()
            ));
            assert_eq!(texture.reflectivity_bits(), Some([0.5_f32.to_bits(); 3]));
            let surface = resolved.shader.surface().unwrap();
            assert_eq!(surface.family, SurfaceFamily::Lightmapped);
            assert_eq!(surface.base_blend, SourceSurfaceBaseBlend::VertexAlpha);
            assert_eq!(surface.effective_emission_mode(false), 0);
            assert_eq!(
                resolved.texture2.as_ref().unwrap().load_outcome(),
                TextureLoadOutcome::Loaded
            );
            assert!(
                texture.read_for_test().layers()[0].mips[0]
                    .bytes
                    .as_slice()
                    .chunks_exact(4)
                    .all(|pixel| pixel[3] == 255)
            );
        }
    }

    #[test]
    fn missing_materials_share_canonical_error_without_hiding_the_failure() {
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![("materials/test/malformed.vmt", Vec::new())],
            Vec::new(),
        );
        let first = resolver.resolve_with_base2(&[], "test/missing_a");
        let second = resolver.resolve_with_base2(&[], "TEST/MISSING_B.VMT");
        assert_eq!(
            first.outcome(),
            super::super::MaterialResolutionOutcome::Missing
        );
        assert_eq!(first.canonical_name(), Some("___error"));
        assert_eq!(second.canonical_name(), first.canonical_name());
        let first = first.material().unwrap();
        let second = second.material().unwrap();
        assert_eq!(first.render_mode, super::super::RenderMode::Opaque);
        assert_eq!(
            first.shader.surface().unwrap().family,
            super::super::SurfaceFamily::Unlit
        );
        assert!(Arc::ptr_eq(
            first.texture.as_ref().unwrap(),
            second.texture.as_ref().unwrap()
        ));
        assert_eq!(
            resolver
                .resolve_surface_lighting("test/missing_a")
                .unwrap()
                .reflectivity,
            Vec3::ZERO
        );
        assert!(
            matches!(resolver.resolve_with_base2(&[], "test/malformed"), super::super::MaterialResolution::Malformed { path } if path.as_str()=="materials/test/malformed.vmt")
        );
    }

    fn rgba_asset(flags: u32) -> Vec<u8> {
        let mut bytes =
            fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[17, 31, 47, 63], &[128; 16]]);
        bytes[20..24].copy_from_slice(&flags.to_le_bytes());
        for (channel, value) in [0.125_f32, 0.25, 0.5].into_iter().enumerate() {
            bytes[32 + channel * 4..36 + channel * 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    fn resolver(bytes: Vec<u8>) -> MaterialResolver {
        MaterialResolver::prop_lighting_fixture(vec![
            ("materials/test/image.vtf", bytes),
            ("materials/test/image.vmt", br#"LightmappedGeneric { "$basetexture" "test/image" "$bumpmap" "test/image" }"#.to_vec()),
        ], Vec::new())
    }

    #[test]
    fn missing_texture_and_missing_material_keep_distinct_native_contracts() {
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![(
                "materials/test/image.vmt",
                br#"LightmappedGeneric { "$basetexture" "test/absent" }"#.to_vec(),
            )],
            Vec::new(),
        );
        let texture = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap()
            .texture
            .unwrap();
        assert_eq!(texture.load_outcome(), TextureLoadOutcome::MissingSource);
        assert_eq!(texture.mapping_dimensions(), Some((32, 32)));
        assert!(texture.has_effective_base_texture());
        assert!(Arc::ptr_eq(
            texture.read_for_test(),
            &native_error_resident()
        ));
        assert_eq!(
            resolver
                .resolve_surface_lighting("test/image")
                .unwrap()
                .reflectivity,
            Vec3::splat(0.5)
        );
        let missing = resolver.resolve_primary(&[], "test/missing_material");
        assert_eq!(
            missing.outcome(),
            super::super::MaterialResolutionOutcome::Missing
        );
        assert_eq!(missing.canonical_name(), Some("___error"));
        let fallback = missing.into_material().unwrap();
        assert_eq!(
            fallback.shader.surface().unwrap().family,
            super::super::SurfaceFamily::Unlit
        );
        assert!(Arc::ptr_eq(
            fallback.texture.as_ref().unwrap().read_for_test(),
            &missing_material_resident()
        ));
        assert_eq!(
            resolver
                .resolve_surface_lighting("test/missing_material")
                .unwrap()
                .reflectivity,
            Vec3::ZERO
        );
    }

    #[test]
    fn nomip_metadata_retains_authored_chain_but_residency_exposes_only_base() {
        let resolver = resolver(rgba_asset(
            texture_flags::NOMIP | texture_flags::EIGHTBITALPHA,
        ));
        let loaded = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap()
            .texture
            .unwrap();
        assert_eq!(loaded.source_metadata().unwrap().mip_count, 2);
        assert_eq!(loaded.read_for_test().id().mip_count, 1);
        let retained = resolver
            .with_decoded_texture_max_dimension(1)
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap()
            .texture
            .unwrap();
        assert_eq!(retained.load_outcome(), TextureLoadOutcome::Loaded);
        assert_eq!(retained.read_for_test().id().first_source_mip, 0);
        assert_eq!(retained.mapping_dimensions(), Some((2, 2)));
        assert!(retained.has_effective_base_texture());
        assert_eq!(
            retained.flags(),
            Some(texture_flags::NOMIP | texture_flags::EIGHTBITALPHA)
        );
        assert_eq!(
            retained.reflectivity_bits().unwrap().map(f32::from_bits),
            [0.125, 0.25, 0.5]
        );
        assert_eq!(
            resolver
                .resolve_surface_lighting("test/image")
                .unwrap()
                .reflectivity,
            Vec3::new(0.125, 0.25, 0.5)
        );
    }

    #[test]
    fn quality_reduction_uses_authored_mips_and_preserves_nolod_residency() {
        for (flags, first, count) in [(0, 1, 1), (texture_flags::NOLOD, 0, 2)] {
            let resolver = resolver(rgba_asset(flags));
            let native = resolver
                .resolve_primary(&[], "test/image")
                .into_material()
                .unwrap()
                .texture
                .unwrap();
            assert_eq!(native.read_for_test().id().first_source_mip, 0);
            assert_eq!(native.read_for_test().id().mip_count, 2);
            let reduced = resolver
                .with_decoded_texture_max_dimension(1)
                .resolve_primary(&[], "test/image")
                .into_material()
                .unwrap()
                .texture
                .unwrap();
            assert_eq!(reduced.load_outcome(), TextureLoadOutcome::Loaded);
            assert_eq!(reduced.read_for_test().id().first_source_mip, first);
            assert_eq!(reduced.read_for_test().id().mip_count, count);
            assert_eq!(reduced.mapping_dimensions(), native.mapping_dimensions());
        }
    }

    #[test]
    fn authored_layers_preserve_frames_mips_and_share_payload_between_roles() {
        let mut bytes = fixture_vtf_bytes(
            2,
            1,
            VtfFormat::Rgba8888,
            &[&[11; 4], &[22; 4], &[33; 8], &[44; 8]],
        );
        bytes[24..26].copy_from_slice(&2_u16.to_le_bytes());
        bytes[56] = 2;
        let resolver = resolver(bytes);
        let material = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        let normal = material.shader.surface().unwrap().normal.as_ref().unwrap();
        assert!(Arc::ptr_eq(
            material.texture.as_ref().unwrap().on_demand().unwrap().0,
            normal.on_demand().unwrap().0
        ));
        assert_eq!(normal.frame_count(), 2);
        assert_eq!(normal.frame(0).unwrap()[0].bytes.as_slice(), &[33; 8]);
        assert_eq!(normal.frame(1).unwrap()[0].bytes.as_slice(), &[44; 8]);
        assert_eq!(normal.frame(0).unwrap()[1].bytes.as_slice(), &[11; 4]);
        assert_eq!(normal.frame(1).unwrap()[1].bytes.as_slice(), &[22; 4]);
        assert_eq!(normal.read_for_test().layer_index(1, 0), Some(1));
        assert_eq!(normal.read_for_test().layer_index(2, 0), None);
        assert_eq!(normal.read_for_test().id().mip_count, 2);
        assert_eq!(resolver.selected_reloadable_paths().len(), 1);
        assert!(normal.resident().is_none());
    }
}
