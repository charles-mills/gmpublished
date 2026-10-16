use super::{ContentPath, ContentSourceTier, MaterialResolver};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt,
    ops::Range,
    sync::{Arc, OnceLock},
};
use vformats::{
    Limits,
    vtf::{BcFormat, VtfError, VtfFormat},
};

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
    Vtf,
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
        f.debug_struct("TextureAssetId")
            .field("sha256", &format_args!("{:02x?}", self.digest))
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
    Deferred(TextureResidentId),
}

impl TextureResidency {
    pub(crate) fn resident(&self) -> Option<&Arc<ResidentTexture>> {
        if let Self::Resident(resident) = self {
            Some(resident)
        } else {
            None
        }
    }
    pub(crate) fn id(&self) -> &TextureResidentId {
        match self {
            Self::Resident(resident) => resident.id(),
            Self::Deferred(id) => id,
        }
    }
    pub(super) fn load_outcome(&self) -> super::TextureLoadOutcome {
        match self {
            Self::Resident(_) => super::TextureLoadOutcome::Loaded,
            Self::Deferred(_) => super::TextureLoadOutcome::Deferred,
        }
    }
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

#[derive(Clone, Eq, PartialEq)]
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
    pub(super) fn backing_id(&self) -> usize {
        self.backing.as_ptr() as usize
    }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResidentTextureMip {
    pub source_mip: u8,
    pub dimensions: [u32; 2],
    pub bytes: TextureBytes,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResidentTextureLayer {
    pub frame: u32,
    pub face: u32,
    pub mips: Vec<ResidentTextureMip>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResidentTexture {
    id: TextureResidentId,
    metadata: Arc<TextureMetadata>,
    layers: Vec<ResidentTextureLayer>,
}

impl ResidentTexture {
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

    #[cfg(any(test, feature = "map-capture"))]
    pub fn layer(&self, frame: u32, face: u32) -> Option<&ResidentTextureLayer> {
        self.layers.get(self.layer_index(frame, face)? as usize)
    }

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
    Vtf(#[from] VtfError),
    #[error("texture storage does not match its source format")]
    Storage,
    #[error("no stored mip satisfies dimension limit {0}")]
    DimensionLimit(u32),
    #[error("invalid texture frame, face, or mip selection")]
    Selection,
    #[error("texture image count exceeds {0}")]
    ImageCount(usize),
    #[error("texture resident image budget exceeded: requires {required}; capacity {capacity}")]
    ResidentImageBudget { required: usize, capacity: usize },
    #[error("texture residency budget exhausted")]
    Budget,
    #[error("texture residency was not included in the frozen request plan")]
    UnplannedRequest,
    #[error(
        "texture preparation requires {required} working bytes; declared capacity is {capacity}"
    )]
    WorkingLimit { required: usize, capacity: usize },
    #[error("texture preparation working cost overflows")]
    WorkingCostOverflow,
    #[error("texture request plan is closed")]
    ClosedPlan,
}

#[derive(Clone, Debug)]
struct TextureRecipe {
    id: TextureResidentId,
    resident_bytes: usize,
    image_count: usize,
    layer_count: usize,
    payload_working_bytes: usize,
}

fn metadata_from_header(header: &vformats::vtf::VtfHeader) -> TextureMetadata {
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

pub(super) struct TextureAsset {
    id: TextureAssetId,
    bytes: Arc<[u8]>,
    metadata: Arc<TextureMetadata>,
    validation: OnceLock<Result<(), VtfError>>,
    sprite_sheet: OnceLock<Arc<super::sprite_sheet::SpriteSheetCache>>,
}

impl fmt::Debug for TextureAsset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextureAsset")
            .field("id", &self.id)
            .field("metadata", &self.metadata)
            .finish()
    }
}

impl TextureAsset {
    pub(super) fn sprite_sheet_cache(
        &self,
        cache: &TextureAssetCache,
    ) -> &super::sprite_sheet::SpriteSheetCache {
        self.sprite_sheet.get_or_init(|| {
            cache
                .sprite_sheets
                .lock()
                .entry(self.id.clone())
                .or_default()
                .clone()
        })
    }
    pub(super) fn source_bytes(&self) -> TextureBytes {
        TextureBytes {
            backing: Arc::clone(&self.bytes),
            range: 0..self.bytes.len(),
        }
    }
    pub fn from_bytes(bytes: Arc<[u8]>, limits: &Limits) -> Result<Self, TextureAssetError> {
        let vtf = vformats::vtf::parse(&bytes, limits)?;
        let metadata = metadata_from_header(&vtf.header());
        Ok(Self {
            id: TextureAssetId::new(AssetKind::Vtf, &bytes),
            bytes,
            metadata: Arc::new(metadata),
            validation: OnceLock::new(),
            sprite_sheet: OnceLock::new(),
        })
    }

    pub fn metadata(&self) -> &TextureMetadata {
        &self.metadata
    }
    pub fn metadata_arc(&self) -> Arc<TextureMetadata> {
        Arc::clone(&self.metadata)
    }
    pub fn first_mip(&self, max: Option<u32>) -> Result<u8, TextureAssetError> {
        let max = max.unwrap_or(u32::MAX).max(1);
        let mips = if self
            .metadata
            .flags
            .is_some_and(|flags| flags & vformats::vtf::texture_flags::NOMIP != 0)
        {
            1
        } else {
            self.metadata.mip_count
        };
        (0..mips)
            .find(|&mip| {
                self.metadata
                    .original_dimensions
                    .iter()
                    .all(|&side| side.checked_shr(u32::from(mip)).unwrap_or(0).max(1) <= max)
            })
            .ok_or(TextureAssetError::DimensionLimit(max))
    }

    pub fn validate_payload(&self) -> Result<(), VtfError> {
        self.validation
            .get_or_init(|| {
                vformats::vtf::inspect_header(
                    &self.bytes,
                    self.bytes.len() as u64,
                    &Limits::default(),
                )
                .map(|_| ())
            })
            .clone()
    }

    fn plan(
        &self,
        frames: Range<u32>,
        faces: Range<u32>,
        mips: Range<u8>,
        storage: TextureStorage,
        limits: &Limits,
    ) -> Result<TextureRecipe, TextureAssetError> {
        if frames.is_empty()
            || faces.is_empty()
            || mips.is_empty()
            || frames.end > self.metadata.frame_count
            || faces.end > self.metadata.face_count
            || mips.end > self.metadata.mip_count
        {
            return Err(TextureAssetError::Selection);
        }
        if let TextureStorage::Bc(format) = storage
            && self.metadata.format.and_then(VtfFormat::bc) != Some(format)
        {
            return Err(TextureAssetError::Storage);
        }
        let count = u64::from(frames.end - frames.start)
            .checked_mul(u64::from(faces.end - faces.start))
            .and_then(|count| count.checked_mul(u64::from(mips.end - mips.start)))
            .ok_or(TextureAssetError::ImageCount(limits.max_entries))?;
        if count > limits.max_entries as u64 {
            return Err(TextureAssetError::ImageCount(limits.max_entries));
        }
        let vtf = vformats::vtf::parse(&self.bytes, limits)?;
        let layers = u64::from(frames.end - frames.start) * u64::from(faces.end - faces.start);
        let mut chain_bytes = 0_u64;
        let mut largest_rgba = 0_u64;
        for mip in mips.clone() {
            let image = vtf.stored_image(frames.end - 1, faces.end - 1, mip)?;
            let rgba = u64::from(image.dimensions[0]) * u64::from(image.dimensions[1]) * 4;
            largest_rgba = largest_rgba.max(rgba);
            chain_bytes = chain_bytes
                .checked_add(match storage {
                    TextureStorage::Rgba8 => rgba,
                    TextureStorage::Bc(_) => image.byte_range.len() as u64,
                })
                .ok_or(VtfError::BadHeader)?;
        }
        let size = chain_bytes.checked_mul(layers).ok_or(VtfError::BadHeader)?;
        if size > limits.max_entry_bytes {
            return Err(VtfError::DecodedTooLarge {
                size,
                max: limits.max_entry_bytes,
            }
            .into());
        }
        let resident_bytes = usize::try_from(size).map_err(|_| VtfError::BadHeader)?;
        let direct = matches!(storage, TextureStorage::Bc(_))
            || self.metadata.format == Some(VtfFormat::Rgba8888);
        Ok(TextureRecipe {
            id: TextureResidentId {
                asset: self.id.clone(),
                frames,
                faces,
                first_source_mip: mips.start,
                mip_count: mips.end - mips.start,
                storage,
            },
            resident_bytes,
            image_count: usize::try_from(count).map_err(|_| VtfError::BadHeader)?,
            layer_count: usize::try_from(layers).map_err(|_| VtfError::BadHeader)?,
            payload_working_bytes: if direct {
                resident_bytes
            } else {
                usize::try_from(largest_rgba).map_err(|_| VtfError::BadHeader)?
            },
        })
    }

    #[cfg(test)]
    pub fn materialize(
        &self,
        frames: Range<u32>,
        faces: Range<u32>,
        mips: Range<u8>,
        storage: TextureStorage,
        limits: &Limits,
    ) -> Result<Arc<ResidentTexture>, TextureAssetError> {
        let recipe = self.plan(frames, faces, mips, storage, limits)?;
        self.materialize_recipe(&recipe, limits)
    }

    fn materialize_recipe(
        &self,
        recipe: &TextureRecipe,
        limits: &Limits,
    ) -> Result<Arc<ResidentTexture>, TextureAssetError> {
        let id = &recipe.id;
        let frames = id.frames.clone();
        let faces = id.faces.clone();
        let mips = id.first_source_mip..id.first_source_mip + id.mip_count;
        let storage = id.storage;
        let expected_bytes = recipe.resident_bytes;
        let vtf = vformats::vtf::parse(&self.bytes, limits)?;
        let direct = matches!(storage, TextureStorage::Bc(_))
            || self.metadata.format == Some(VtfFormat::Rgba8888);
        let compact: Option<Arc<[u8]>> = if direct {
            let mut bytes = Vec::with_capacity(expected_bytes);
            for frame in frames.clone() {
                for face in faces.clone() {
                    for mip in mips.clone() {
                        let image = vtf.stored_image(frame, face, mip)?;
                        bytes.extend_from_slice(&self.bytes[image.byte_range]);
                    }
                }
            }
            Some(bytes.into())
        } else {
            None
        };
        let mut offset = 0;
        let mut layers =
            Vec::with_capacity(((frames.end - frames.start) * (faces.end - faces.start)) as usize);
        for frame in frames {
            for face in faces.clone() {
                let mut chain = Vec::with_capacity(usize::from(id.mip_count));
                for source_mip in mips.clone() {
                    let image = vtf.stored_image(frame, face, source_mip)?;
                    let bytes = if let Some(backing) = &compact {
                        let end = offset + image.byte_range.len();
                        let bytes = TextureBytes {
                            backing: Arc::clone(backing),
                            range: offset..end,
                        };
                        offset = end;
                        bytes
                    } else {
                        TextureBytes::owned(vtf.decode_rgba(frame, face, source_mip, limits)?.rgba)
                    };
                    chain.push(ResidentTextureMip {
                        source_mip,
                        dimensions: image.dimensions,
                        bytes,
                    });
                }
                layers.push(ResidentTextureLayer {
                    frame,
                    face,
                    mips: chain,
                });
            }
        }
        let selected = ResidentTexture {
            id: id.clone(),
            metadata: Arc::clone(&self.metadata),
            layers,
        };
        selected.validate()?;
        debug_assert_eq!(selected.byte_len(), expected_bytes);
        Ok(Arc::new(selected))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TextureOrigin {
    pub path: ContentPath,
    pub tier: ContentSourceTier,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum TextureSourceError {
    #[error("texture source {path} has no frozen preparation authorization: {error}")]
    Preparation {
        path: ContentPath,
        error: TextureAssetError,
    },
    #[error("texture source is absent")]
    Missing,
    #[error(
        "texture source {origin:?}: {declared_bytes} encoded bytes rejected; header inspection: {inspection:?}"
    )]
    EncodedBudget {
        origin: TextureOrigin,
        declared_bytes: u64,
        metadata: Option<Arc<TextureMetadata>>,
        inspection: Option<Arc<str>>,
    },
    #[error(
        "texture source {origin:?}: working policy rejected: {error}; header inspection: {inspection:?}"
    )]
    WorkingBudget {
        origin: TextureOrigin,
        error: TextureAssetError,
        metadata: Option<Arc<TextureMetadata>>,
        inspection: Option<Arc<str>>,
    },
    #[error("texture source {origin:?}: {message}")]
    Read {
        origin: TextureOrigin,
        message: Arc<str>,
    },
    #[error("texture source {origin:?}: {error}")]
    Invalid {
        origin: TextureOrigin,
        bytes: TextureBytes,
        error: TextureAssetError,
    },
}

#[derive(Debug)]
pub(super) struct SelectedTextureAsset {
    pub origin: TextureOrigin,
    pub asset: TextureAsset,
}

#[derive(Debug)]
struct CachedSource {
    token: u64,
    read_cost: OnceLock<super::texture_source::TextureSourceReadCost>,
    payload: OnceLock<Result<Arc<SelectedTextureAsset>, TextureSourceError>>,
}
impl Default for CachedSource {
    fn default() -> Self {
        Self {
            token: super::texture_source::source_token(),
            read_cost: OnceLock::new(),
            payload: OnceLock::new(),
        }
    }
}
type SourceCell = Arc<CachedSource>;
type ResidentCell = Arc<OnceLock<Result<Arc<ResidentTexture>, TextureAssetError>>>;

#[derive(Debug, Default)]
pub(super) struct TextureAssetCache {
    sources: Mutex<HashMap<ContentPath, SourceCell>>,
    residents: Mutex<HashMap<TextureResidentId, ResidentCell>>,
    sprite_sheets: Mutex<HashMap<TextureAssetId, Arc<super::sprite_sheet::SpriteSheetCache>>>,
}

#[cfg(test)]
impl TextureAssetCache {
    pub(super) fn selected_paths(&self) -> Vec<String> {
        let mut paths = self
            .sources
            .lock()
            .keys()
            .map(|path| path.as_str().to_owned())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        paths
    }
    pub fn resident_count(&self) -> usize {
        self.residents.lock().len()
    }
}

impl MaterialResolver {
    fn source_working_request(
        &self,
        decoder_bytes: usize,
        extra_working_bytes: usize,
    ) -> Result<Option<super::texture_working::TextureWorkingRequest>, TextureAssetError> {
        let capacity = self.config.texture_decoder_limit.unwrap_or_else(|| {
            usize::try_from(Limits::default().max_input_bytes).unwrap_or(usize::MAX)
        });
        if decoder_bytes > capacity {
            return Err(TextureAssetError::WorkingLimit {
                required: decoder_bytes,
                capacity,
            });
        }
        self.config
            .texture_source_working_budget
            .as_ref()
            .map(|budget| budget.request(extra_working_bytes))
            .transpose()
    }

    fn source_header(
        &self,
        descriptor: &mut super::texture_source::SourceEntryDescriptor<'_>,
        cost: super::texture_source::TextureSourceReadCost,
    ) -> (Option<Arc<TextureMetadata>>, Option<Arc<str>>) {
        let header = cost
            .prefix_working_bytes(vformats::vtf::MAX_HEADER_BYTES)
            .and_then(|bytes| self.source_working_request(cost.decoder_bytes, bytes))
            .map_err(|error| error.to_string())
            .and_then(|working| {
                let _working = working.map(super::texture_working::TextureWorkingRequest::acquire);
                descriptor
                    .prefix(vformats::vtf::MAX_HEADER_BYTES, cost.decoder_bytes)
                    .map_err(|error| error.to_string())
                    .and_then(|prefix| {
                        vformats::vtf::inspect_header(
                            &prefix,
                            descriptor.size,
                            &Limits {
                                max_input_bytes: descriptor.size,
                                ..Limits::default()
                            },
                        )
                        .map(|header| metadata_from_header(&header))
                        .map_err(|error| error.to_string())
                    })
            });
        match header {
            Ok(metadata) => (Some(Arc::new(metadata)), None),
            Err(error) => (None, Some(error.into())),
        }
    }

    pub(super) fn selected_texture_asset(
        &self,
        path: &ContentPath,
    ) -> Result<Arc<SelectedTextureAsset>, TextureSourceError> {
        let cell = self
            .shared
            .texture_assets
            .sources
            .lock()
            .entry(path.clone())
            .or_default()
            .clone();
        self.config
            .texture_access
            .source_request(cell.token)
            .map_err(|error| TextureSourceError::Preparation {
                path: path.clone(),
                error,
            })?;
        let budget = self.config.encoded_texture_budget.as_ref();
        let result = if let Some(cached) = cell.payload.get() {
            let (origin, size, metadata) = match cached {
                Ok(selected) => (
                    Some(selected.origin.clone()),
                    selected.asset.bytes.len() as u64,
                    Some(selected.asset.metadata_arc()),
                ),
                Err(TextureSourceError::Invalid { origin, bytes, .. }) => {
                    (Some(origin.clone()), bytes.backing_len() as u64, None)
                }
                Err(TextureSourceError::Read { origin, .. }) if cell.read_cost.get().is_some() => (
                    Some(origin.clone()),
                    cell.read_cost
                        .get()
                        .map_or(0, |cost| cost.snapshot_bytes as u64),
                    None,
                ),
                _ => (None, 0, None),
            };
            if let Some(origin) = &origin
                && let Some(cost) = cell.read_cost.get()
                && let Err(error) =
                    self.source_working_request(cost.decoder_bytes, cost.full_read_extra_bytes())
            {
                return Err(TextureSourceError::WorkingBudget {
                    origin: origin.clone(),
                    error,
                    metadata,
                    inspection: None,
                });
            }
            if let Some(origin) = origin
                && budget.is_some_and(|budget| !budget.admit(cell.token, size))
            {
                return Err(TextureSourceError::EncodedBudget {
                    origin,
                    declared_bytes: size,
                    metadata,
                    inspection: None,
                });
            }
            cached.clone()
        } else {
            let mut descriptor = None;
            for source in self.sources() {
                let origin = TextureOrigin {
                    path: path.clone(),
                    tier: source.tier(),
                };
                match source.descriptor(path) {
                    Ok(Some(selected)) => {
                        descriptor = Some(selected);
                        break;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return cell
                            .payload
                            .get_or_init(|| {
                                Err(TextureSourceError::Read {
                                    origin,
                                    message: error.to_string().into(),
                                })
                            })
                            .clone();
                    }
                }
            }
            let Some(mut descriptor) = descriptor else {
                return cell
                    .payload
                    .get_or_init(|| Err(TextureSourceError::Missing))
                    .clone();
            };
            let cost = match descriptor.cost() {
                Ok(cost) => *cell.read_cost.get_or_init(|| cost),
                Err(error) => {
                    return cell
                        .payload
                        .get_or_init(|| {
                            Err(TextureSourceError::Read {
                                origin: descriptor.origin.clone(),
                                message: error.to_string().into(),
                            })
                        })
                        .clone();
                }
            };
            let working = match self
                .source_working_request(cost.decoder_bytes, cost.full_read_extra_bytes())
            {
                Ok(request) => request,
                Err(error) => {
                    let (metadata, inspection) = self.source_header(&mut descriptor, cost);
                    return Err(TextureSourceError::WorkingBudget {
                        origin: descriptor.origin,
                        error,
                        metadata,
                        inspection,
                    });
                }
            };
            if budget.is_some_and(|budget| !budget.admit(cell.token, descriptor.size)) {
                let (metadata, inspection) = self.source_header(&mut descriptor, cost);
                return Err(TextureSourceError::EncodedBudget {
                    origin: descriptor.origin,
                    declared_bytes: descriptor.size,
                    metadata,
                    inspection,
                });
            }
            cell.payload
                .get_or_init(|| {
                    let _working =
                        working.map(super::texture_working::TextureWorkingRequest::acquire);
                    let origin = descriptor.origin.clone();
                    let bytes: Arc<[u8]> = descriptor
                        .bytes(cost.decoder_bytes)
                        .map_err(|error| TextureSourceError::Read {
                            origin: origin.clone(),
                            message: error.to_string().into(),
                        })?
                        .into();
                    let asset = TextureAsset::from_bytes(Arc::clone(&bytes), &Limits::default())
                        .map_err(|error| TextureSourceError::Invalid {
                            origin: origin.clone(),
                            bytes: TextureBytes {
                                range: 0..bytes.len(),
                                backing: bytes,
                            },
                            error,
                        })?;
                    Ok(Arc::new(SelectedTextureAsset { origin, asset }))
                })
                .clone()
        };
        if matches!(result, Err(TextureSourceError::Read { .. }))
            && let Some(budget) = budget
        {
            budget.release(cell.token);
        }
        match &result {
            Ok(selected) => self
                .config
                .texture_access
                .source(selected.asset.source_bytes()),
            Err(TextureSourceError::Invalid { bytes, .. }) => {
                self.config.texture_access.source(bytes.clone());
            }
            _ => {}
        }
        result
    }

    pub(super) fn resident_texture(
        &self,
        selected: &Arc<SelectedTextureAsset>,
        faces: Range<u32>,
        force_rgba: bool,
    ) -> Result<TextureResidency, TextureAssetError> {
        let asset = &selected.asset;
        let metadata = asset.metadata();
        asset.validate_payload()?;
        let first = asset.first_mip(self.config.decoded_texture_max_dimension)?;
        let end = if self.config.single_texture_mip
            || metadata
                .flags
                .is_some_and(|flags| flags & vformats::vtf::texture_flags::NOMIP != 0)
        {
            first + 1
        } else {
            metadata.mip_count
        };
        let storage = if force_rgba {
            TextureStorage::Rgba8
        } else {
            metadata
                .format
                .and_then(VtfFormat::bc)
                .map_or(TextureStorage::Rgba8, TextureStorage::Bc)
        };
        let limits = Limits {
            max_entry_bytes: self
                .config
                .texture_resource_limit
                .map_or_else(|| Limits::default().max_entry_bytes, |bytes| bytes as u64),
            ..Limits::default()
        };
        let frames = 0..metadata.frame_count;
        let recipe = asset.plan(frames, faces, first..end, storage, &limits)?;
        let id = &recipe.id;
        let size = recipe.resident_bytes;
        let transient = recipe.payload_working_bytes;
        let cost = super::TextureRequestCost {
            resident_bytes: size,
            image_count: recipe.image_count,
            layer_count: recipe.layer_count,
            resident_record_bytes: recipe
                .image_count
                .checked_mul(std::mem::size_of::<ResidentTextureMip>())
                .and_then(|mips| {
                    recipe
                        .layer_count
                        .checked_mul(std::mem::size_of::<ResidentTextureLayer>())
                        .and_then(|layers| mips.checked_add(layers))
                })
                .ok_or(VtfError::BadHeader)?,
            source_encoded_bytes: asset.bytes.len(),
            decode_peak_bytes: size.checked_add(transient).ok_or(VtfError::BadHeader)?,
            payload_working_bytes: transient,
        };
        let working = self
            .config
            .texture_working_budget
            .as_ref()
            .map(|budget| budget.request(transient))
            .transpose();
        let preflight_error = working.as_ref().err().cloned();
        if self.config.texture_access.record(
            id,
            cost,
            Arc::clone(selected),
            preflight_error.clone(),
        )? {
            return preflight_error.map_or_else(|| Ok(TextureResidency::Deferred(id.clone())), Err);
        }
        if let super::texture_plan::TextureAccess::Admitted(admissions) =
            &self.config.texture_access
        {
            admissions.check(id)?;
        }
        let working = match working {
            Ok(request) => request,
            Err(error) => {
                if let super::texture_plan::TextureAccess::Admitted(admissions) =
                    &self.config.texture_access
                {
                    admissions.release(id);
                }
                return Err(error);
            }
        };
        let cell = self
            .shared
            .texture_assets
            .residents
            .lock()
            .entry(id.clone())
            .or_default()
            .clone();
        if let Some(Err(error)) = cell.get() {
            if let super::texture_plan::TextureAccess::Admitted(admissions) =
                &self.config.texture_access
            {
                admissions.release(id);
            }
            return Err(error.clone());
        }
        if matches!(
            self.config.texture_access,
            super::texture_plan::TextureAccess::Direct
        ) && let Some(budget) = &self.config.decoded_texture_budget
        {
            budget.try_admit(id, size, recipe.image_count)?;
        }
        let result = cell
            .get_or_init(|| {
                let _working = working.map(super::texture_working::TextureWorkingRequest::acquire);
                asset.materialize_recipe(&recipe, &limits)
            })
            .clone();
        if result.is_err() {
            match &self.config.texture_access {
                super::texture_plan::TextureAccess::Admitted(admissions) => admissions.release(id),
                super::texture_plan::TextureAccess::Direct => {
                    if let Some(budget) = &self.config.decoded_texture_budget {
                        budget.release(id);
                    }
                }
                super::texture_plan::TextureAccess::Planning(_) => unreachable!(),
            }
        }
        result.map(TextureResidency::Resident)
    }
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
impl MaterialResolver {
    pub(crate) fn selected_texture_paths(&self) -> Vec<String> {
        self.shared.texture_assets.selected_paths()
    }
    pub(crate) fn resident_texture_asset_count(&self) -> usize {
        self.shared.texture_assets.resident_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::materials::{DecodedTextureBudget, ResolvedTextureMip, TextureLoadOutcome};
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
                    texture.resident().unwrap(),
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
                TextureLoadOutcome::InvalidPayload,
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
                texture.resident().unwrap(),
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
                texture.resident().unwrap().layers()[0].mips[0]
                    .bytes
                    .as_slice()
                    .chunks_exact(4)
                    .all(|pixel| pixel[3] == 255)
            );
            if outcome == TextureLoadOutcome::InvalidPayload {
                assert_eq!(
                    texture.source_metadata().unwrap().flags,
                    Some(texture_flags::EIGHTBITALPHA)
                );
                assert_eq!(texture.original_dimensions(), (2, 2));
            }
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
        assert_eq!(resolver.shared.texture_assets.resident_count(), 0);
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
            material.texture.resident().unwrap(),
            normal.resident().unwrap()
        ));
        assert_eq!(normal.frame_count(), 2);
        assert_eq!(normal.frame(0).unwrap()[0].bytes.as_slice(), &[33; 8]);
        assert_eq!(normal.frame(1).unwrap()[0].bytes.as_slice(), &[44; 8]);
        assert_eq!(normal.frame(0).unwrap()[1].bytes.as_slice(), &[11; 4]);
        assert_eq!(normal.frame(1).unwrap()[1].bytes.as_slice(), &[22; 4]);
        assert_eq!(normal.resident().unwrap().layer_index(1, 0), Some(1));
        assert_eq!(normal.resident().unwrap().layer_index(2, 0), None);
        assert_eq!(normal.resident().unwrap().id().mip_count, 2);
        assert_eq!(resolver.shared.texture_assets.sources.lock().len(), 1);
        assert_eq!(resolver.shared.texture_assets.residents.lock().len(), 1);
    }

    #[test]
    fn bc_storage_compacts_selected_ranges_and_rgba_matches_shared_decoder() {
        let block = [0x03, 0, 0, 0, 0, 0, 0, 0];
        let bytes: Arc<[u8]> =
            fixture_vtf_bytes(4, 4, VtfFormat::Dxt1, &[&block, &block, &block]).into();
        let asset = TextureAsset::from_bytes(bytes, &Limits::default()).unwrap();
        let bc = asset
            .materialize(
                0..1,
                0..1,
                1..3,
                TextureStorage::Bc(BcFormat::Bc1),
                &Limits::default(),
            )
            .unwrap();
        let rgba = asset
            .materialize(0..1, 0..1, 1..3, TextureStorage::Rgba8, &Limits::default())
            .unwrap();
        assert_eq!(bc.id().first_source_mip, 1);
        assert_eq!(bc.layers()[0].mips[0].dimensions, [2, 2]);
        for (compressed, decoded) in bc.layers()[0].mips.iter().zip(&rgba.layers()[0].mips) {
            assert!(!Arc::ptr_eq(&compressed.bytes.backing, &asset.bytes));
            assert_eq!(compressed.bytes.backing.len(), bc.byte_len());
            let image = vformats::vtf::decode_bc_rgba(
                BcFormat::Bc1,
                compressed.dimensions[0],
                compressed.dimensions[1],
                compressed.bytes.as_slice(),
                &Limits::default(),
            )
            .unwrap();
            assert_eq!(decoded.bytes.as_slice(), image.rgba);
        }
    }

    #[test]
    fn resident_identity_covers_allocated_mips_and_generated_shape() {
        let asset = TextureAsset::from_bytes(rgba_asset(0).into(), &Limits::default()).unwrap();
        let full = asset
            .materialize(0..1, 0..1, 0..2, TextureStorage::Rgba8, &Limits::default())
            .unwrap();
        let one = asset
            .materialize(0..1, 0..1, 0..1, TextureStorage::Rgba8, &Limits::default())
            .unwrap();
        assert_ne!(full.id(), one.id());
        assert_eq!(full.id().asset, one.id().asset);
        assert_eq!(
            full.layers()[0].mips[0].bytes.as_slice(),
            one.layers()[0].mips[0].bytes.as_slice()
        );
        assert_eq!(one.layers()[0].mips[0].bytes.backing.len(), one.byte_len());
        let generated = |namespace, width, height| {
            generated_rgba(
                namespace,
                None,
                vec![vec![ResolvedTextureMip {
                    rgba: vec![255; 8],
                    width,
                    height,
                }]],
            )
            .unwrap()
        };
        assert_ne!(generated("one", 1, 2).id(), generated("one", 2, 1).id());
        assert_ne!(generated("one", 1, 2).id(), generated("two", 1, 2).id());
        let first = TextureAssetId::new(AssetKind::Vtf, &[1_u8]);
        let different = TextureAssetId::new(AssetKind::Vtf, &[2_u8]);
        assert_ne!(first, different);
    }

    #[test]
    fn malformed_first_source_never_falls_through_and_is_cached_with_its_bytes() {
        let path = ContentPath::new("materials/test/image.vtf").unwrap();
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![
                (path.as_str(), b"invalid packed VTF".to_vec()),
                (
                    "materials/test/image.vmt",
                    br#"LightmappedGeneric { "$basetexture" "test/image" }"#.to_vec(),
                ),
            ],
            vec![(path.as_str(), rgba_asset(0))],
        );
        let error = resolver.selected_texture_asset(&path).unwrap_err();
        assert!(
            matches!(&error,TextureSourceError::Invalid {origin,bytes,error:TextureAssetError::Vtf(VtfError::BadMagic)} if origin.tier==ContentSourceTier::Pakfile && bytes.as_slice()==b"invalid packed VTF")
        );
        assert_eq!(error, resolver.selected_texture_asset(&path).unwrap_err());
        let texture = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap()
            .texture;
        assert_eq!(texture.load_outcome(), TextureLoadOutcome::InvalidSource);
        assert!(texture.has_effective_base_texture());
        assert!(Arc::ptr_eq(
            texture.resident().unwrap(),
            &native_error_resident()
        ));
        assert_eq!(
            resolver
                .resolve_surface_lighting("test/image")
                .unwrap()
                .reflectivity,
            Vec3::splat(0.5)
        );
        assert_eq!(resolver.shared.texture_assets.resident_count(), 0);
    }

    #[test]
    fn truncated_payload_overrides_quality_policy_without_losing_raw_header() {
        let mut bytes = rgba_asset(0);
        bytes.pop();
        let budget = Arc::new(DecodedTextureBudget::new(1));
        let resolver = resolver(bytes)
            .with_decoded_texture_max_dimension(1)
            .with_decoded_texture_budget(Arc::clone(&budget));
        let texture = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap()
            .texture;
        assert_eq!(texture.load_outcome(), TextureLoadOutcome::InvalidPayload);
        assert_eq!(
            texture
                .source_metadata()
                .unwrap()
                .reflectivity_bits
                .unwrap()
                .map(f32::from_bits),
            [0.125, 0.25, 0.5]
        );
        assert_eq!(texture.reflectivity_bits(), Some([0.5_f32.to_bits(); 3]));
        assert_eq!(
            texture.flags(),
            Some(super::super::NATIVE_ERROR_TEXTURE_FLAGS)
        );
        assert_eq!(texture.original_dimensions(), (2, 2));
        assert_eq!(texture.mapping_dimensions(), Some((32, 32)));
        assert!(Arc::ptr_eq(
            texture.resident().unwrap(),
            &native_error_resident()
        ));
        assert_eq!(budget.decoded_bytes(), 0);
        assert_eq!(budget.rejected_textures(), 0);
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
            .texture;
        assert_eq!(texture.load_outcome(), TextureLoadOutcome::MissingSource);
        assert_eq!(texture.mapping_dimensions(), Some((32, 32)));
        assert!(texture.has_effective_base_texture());
        assert!(Arc::ptr_eq(
            texture.resident().unwrap(),
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
            fallback.texture.resident().unwrap(),
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
            .texture;
        assert_eq!(loaded.source_metadata().unwrap().mip_count, 2);
        assert_eq!(loaded.resident().unwrap().id().mip_count, 1);
        let rejected = resolver
            .with_decoded_texture_max_dimension(1)
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap()
            .texture;
        assert_eq!(rejected.load_outcome(), TextureLoadOutcome::QualityRejected);
        assert_eq!(rejected.mapping_dimensions(), Some((2, 2)));
        assert!(rejected.has_effective_base_texture());
        assert_eq!(
            rejected.flags(),
            Some(texture_flags::NOMIP | texture_flags::EIGHTBITALPHA)
        );
        assert_eq!(
            rejected.reflectivity_bits().unwrap().map(f32::from_bits),
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
    fn budget_admission_charges_cached_residency_and_does_not_poison_other_budgets() {
        let resolver = resolver(rgba_asset(0));
        let original = resolver
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        let small = Arc::new(DecodedTextureBudget::new(19));
        let rejected = resolver
            .with_decoded_texture_budget(Arc::clone(&small))
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert_eq!(
            rejected.texture.load_outcome(),
            TextureLoadOutcome::QualityRejected
        );
        assert_eq!(
            rejected
                .shader
                .surface()
                .unwrap()
                .normal
                .as_ref()
                .unwrap()
                .load_outcome(),
            TextureLoadOutcome::QualityRejected
        );
        assert_eq!(small.decoded_bytes(), 0);
        assert_eq!(small.rejected_textures(), 1);
        let exact = Arc::new(DecodedTextureBudget::new(20));
        let accepted = resolver
            .with_decoded_texture_budget(Arc::clone(&exact))
            .resolve_primary(&[], "test/image")
            .into_material()
            .unwrap();
        assert!(Arc::ptr_eq(
            original.texture.resident().unwrap(),
            accepted.texture.resident().unwrap()
        ));
        assert!(Arc::ptr_eq(
            accepted.texture.resident().unwrap(),
            accepted
                .shader
                .surface()
                .unwrap()
                .normal
                .as_ref()
                .unwrap()
                .resident()
                .unwrap()
        ));
        assert_eq!(exact.decoded_bytes(), 20);
        assert_eq!(exact.rejected_textures(), 0);
    }

    #[test]
    fn decode_failure_rolls_back_admission_and_is_cached_independently_of_budget() {
        let mut bytes = fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[42]]);
        bytes[52..56].copy_from_slice(&7_i32.to_le_bytes());
        let resolver = resolver(bytes);
        let budget = Arc::new(DecodedTextureBudget::new(4));
        let resolver = resolver.with_decoded_texture_budget(Arc::clone(&budget));
        for _ in 0..2 {
            let material = resolver
                .resolve_primary(&[], "test/image")
                .into_material()
                .unwrap();
            assert_eq!(
                material.texture.load_outcome(),
                TextureLoadOutcome::Unsupported
            );
            assert_eq!(budget.decoded_bytes(), 0);
            assert_eq!(budget.rejected_textures(), 0);
        }
        assert_eq!(resolver.shared.texture_assets.resident_count(), 1);
    }
    #[test]
    fn selected_payload_and_cache_identity_do_not_retain_encoded_file() {
        let encoded: Arc<[u8]> = rgba_asset(0).into();
        let weak = Arc::downgrade(&encoded);
        let asset = TextureAsset::from_bytes(encoded, &Limits::default()).unwrap();
        let resident = asset
            .materialize(0..1, 0..1, 1..2, TextureStorage::Rgba8, &Limits::default())
            .unwrap();
        let identity = resident.id().clone();
        assert_eq!(resident.byte_len(), 4);
        assert_eq!(resident.layers()[0].mips[0].bytes.backing_len(), 4);
        drop(asset);
        assert!(weak.upgrade().is_none());
        assert_eq!(identity.first_source_mip, 1);
        assert_eq!(
            resident.layers()[0].mips[0].bytes.as_slice(),
            &[17, 31, 47, 63]
        );
    }
}
