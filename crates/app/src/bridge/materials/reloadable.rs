#[cfg(test)]
mod test_gate;

use super::texture_asset::{TextureOrigin, metadata_from_header};
use super::{
    Arc, Mutex, ResidentTexture, ResidentTextureLayer, ResidentTextureMip, TextureAssetError,
    TextureBytes, TextureMetadata, TextureStorage, TextureWorkingBudget,
};
use crate::preparation::PreparationCancellation;
use std::{
    fmt,
    fs::File,
    io::{Read, Seek, SeekFrom},
    ops::Range,
    time::SystemTime,
};
use vformats::{
    Limits,
    vtf::{VtfFormat, VtfLayout},
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TextureSourceId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TexturePageCost {
    pub resident_bytes: usize,
    pub decode_working_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TexturePageLimits {
    pub resident_bytes: usize,
    pub decode_working_bytes: usize,
}
impl Default for TexturePageLimits {
    fn default() -> Self {
        let limits = Limits::default();
        Self {
            resident_bytes: usize::try_from(limits.max_entry_bytes).unwrap_or(usize::MAX),
            decode_working_bytes: usize::try_from(limits.max_input_bytes).unwrap_or(usize::MAX),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TexturePageIdentity {
    pub source_id: TextureSourceId,
    pub frame: u32,
    pub faces: Range<u32>,
    pub first_source_mip: u8,
    pub mip_count: u8,
    pub storage: TextureStorage,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TextureFrameRequest {
    pub source_id: TextureSourceId,
    pub frame: u32,
    pub faces: Range<u32>,
    pub first_source_mip: u8,
    pub mip_count: u8,
    pub storage: TextureStorage,
    pub limits: super::TexturePageLimits,
}
impl TextureFrameRequest {
    // Admission is checked on every request; compatible callers share physical pages.
    pub fn identity(&self) -> TexturePageIdentity {
        TexturePageIdentity {
            source_id: self.source_id,
            frame: self.frame,
            faces: self.faces.clone(),
            first_source_mip: self.first_source_mip,
            mip_count: self.mip_count,
            storage: self.storage,
        }
    }
}

pub(super) trait TextureSourceReader: fmt::Debug + Send + Sync {
    fn len(&self) -> u64;
    fn decoder_working_bytes(&self) -> usize {
        0
    }
    fn read_range(
        &self,
        range: Range<u64>,
        cancellation: &PreparationCancellation,
    ) -> Result<Vec<u8>, TextureAssetError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    unix: (u64, u64, i64, i64),
}
impl FileStamp {
    fn inspect(file: &File) -> Result<Self, TextureAssetError> {
        let metadata = file.metadata().map_err(|error| source_read(&error))?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            unix: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        })
    }
}
fn source_read(error: &std::io::Error) -> TextureAssetError {
    TextureAssetError::SourceRead(error.to_string().into())
}

#[derive(Debug)]
pub(super) struct FrozenFileSource {
    file: Mutex<File>,
    stamp: FileStamp,
}
impl FrozenFileSource {
    #[cfg(test)]
    pub(super) fn new(file: File) -> Result<Self, TextureAssetError> {
        let stamp = FileStamp::inspect(&file)?;
        Ok(Self {
            file: Mutex::new(file),
            stamp,
        })
    }
}
impl TextureSourceReader for FrozenFileSource {
    fn len(&self) -> u64 {
        self.stamp.len
    }
    fn read_range(
        &self,
        range: Range<u64>,
        cancellation: &PreparationCancellation,
    ) -> Result<Vec<u8>, TextureAssetError> {
        cancellation.check()?;
        if range.start > range.end || range.end > self.stamp.len {
            return Err(TextureAssetError::Selection);
        }
        let mut file = self.file.lock();
        if FileStamp::inspect(&file)? != self.stamp {
            return Err(TextureAssetError::SourceChanged);
        }
        file.seek(SeekFrom::Start(range.start))
            .map_err(|error| source_read(&error))?;
        let size = usize::try_from(range.end - range.start)
            .map_err(|_| TextureAssetError::WorkingCostOverflow)?;
        let mut bytes = vec![0; size];
        for chunk in bytes.chunks_mut(64 * 1024) {
            cancellation.check()?;
            file.read_exact(chunk)
                .map_err(|error| source_read(&error))?;
        }
        if FileStamp::inspect(&file)? != self.stamp {
            return Err(TextureAssetError::SourceChanged);
        }
        drop(file);
        cancellation.check()?;
        Ok(bytes)
    }
}

#[derive(Debug)]
pub(super) struct FrozenPathSource {
    path: std::path::PathBuf,
    stamp: FileStamp,
}
impl FrozenPathSource {
    pub(super) fn new(path: std::path::PathBuf, file: &File) -> Result<Self, TextureAssetError> {
        Ok(Self {
            path,
            stamp: FileStamp::inspect(file)?,
        })
    }
    pub(super) fn open_verified(&self) -> Result<File, TextureAssetError> {
        let file = File::open(&self.path).map_err(|error| source_read(&error))?;
        self.verify(&file)?;
        Ok(file)
    }
    pub(super) fn verify(&self, file: &File) -> Result<(), TextureAssetError> {
        if FileStamp::inspect(file)? != self.stamp {
            return Err(TextureAssetError::SourceChanged);
        }
        Ok(())
    }
}
impl TextureSourceReader for FrozenPathSource {
    fn len(&self) -> u64 {
        self.stamp.len
    }
    fn read_range(
        &self,
        range: Range<u64>,
        cancellation: &PreparationCancellation,
    ) -> Result<Vec<u8>, TextureAssetError> {
        cancellation.check()?;
        let file = File::open(&self.path).map_err(|error| source_read(&error))?;
        if FileStamp::inspect(&file)? != self.stamp {
            return Err(TextureAssetError::SourceChanged);
        }
        FrozenFileSource {
            file: Mutex::new(file),
            stamp: self.stamp.clone(),
        }
        .read_range(range, cancellation)
    }
}

#[derive(Debug)]
pub(super) struct PreloadSource(pub Arc<[u8]>);
impl TextureSourceReader for PreloadSource {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_range(
        &self,
        range: Range<u64>,
        cancel: &PreparationCancellation,
    ) -> Result<Vec<u8>, TextureAssetError> {
        cancel.check()?;
        let start = usize::try_from(range.start).map_err(|_| TextureAssetError::Selection)?;
        let end = usize::try_from(range.end).map_err(|_| TextureAssetError::Selection)?;
        self.0
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or(TextureAssetError::Selection)
    }
}

#[derive(Debug)]
pub(super) struct SubrangeSource {
    pub reader: Arc<dyn TextureSourceReader>,
    pub range: Range<u64>,
    pub prefix: Arc<[u8]>,
}
impl TextureSourceReader for SubrangeSource {
    fn len(&self) -> u64 {
        self.prefix.len() as u64 + self.range.end - self.range.start
    }
    fn read_range(
        &self,
        range: Range<u64>,
        cancellation: &PreparationCancellation,
    ) -> Result<Vec<u8>, TextureAssetError> {
        cancellation.check()?;
        if range.start > range.end || range.end > self.len() {
            return Err(TextureAssetError::Selection);
        }
        let split = self.prefix.len() as u64;
        let mut output = if range.end > split {
            let start = self.range.start + range.start.saturating_sub(split);
            let end = self.range.start + range.end - split;
            self.reader.read_range(start..end, cancellation)?
        } else {
            Vec::new()
        };
        if range.start < split {
            let prefix = &self.prefix[range.start as usize..range.end.min(split) as usize];
            output.splice(0..0, prefix.iter().copied());
        }
        Ok(output)
    }
}

pub struct ReloadableTextureSource {
    id: TextureSourceId,
    origin: TextureOrigin,
    metadata: Arc<TextureMetadata>,
    layout: VtfLayout,
    reader: Arc<dyn TextureSourceReader>,
    limits: Limits,
    pub(super) sprite_sheet_cache: super::sprite_sheet::SpriteSheetCache,
    #[cfg(test)]
    page_read_gate: Arc<test_gate::PageReadGate>,
}
impl fmt::Debug for ReloadableTextureSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReloadableTextureSource")
            .field("id", &self.id)
            .field("origin", &self.origin)
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}
impl PartialEq for ReloadableTextureSource {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ReloadableTextureSource {}

pub(super) struct TexturePage {
    request: TextureFrameRequest,
    resident: Arc<ResidentTexture>,
}
impl fmt::Debug for TexturePage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TexturePage")
            .field("request", &self.request)
            .field("resident", &self.resident)
            .finish()
    }
}
impl TexturePage {
    pub fn request(&self) -> &TextureFrameRequest {
        &self.request
    }
    pub fn resident(&self) -> &ResidentTexture {
        &self.resident
    }
}

impl ReloadableTextureSource {
    pub(super) fn inspect(
        origin: TextureOrigin,
        reader: Arc<dyn TextureSourceReader>,
        limits: Limits,
        cancellation: &PreparationCancellation,
        working: &Arc<TextureWorkingBudget>,
    ) -> Result<Arc<Self>, TextureAssetError> {
        cancellation.check()?;
        let count = reader.len().min(vformats::vtf::MAX_HEADER_BYTES as u64);
        let cost = usize::try_from(count)
            .ok()
            .and_then(|n| n.checked_mul(2))
            .and_then(|n| n.checked_add(reader.decoder_working_bytes()))
            .ok_or(TextureAssetError::WorkingCostOverflow)?;
        let _working = working.request(cost)?.acquire_cancellable(cancellation)?;
        let prefix = reader.read_range(0..count, cancellation)?;
        let layout = vformats::vtf::inspect_layout(&prefix, reader.len(), &limits)?;
        let metadata = Arc::new(metadata_from_header(&layout.header()));
        Ok(Arc::new(Self {
            id: TextureSourceId(super::texture_source::source_token()),
            origin,
            metadata,
            layout,
            reader,
            limits,
            sprite_sheet_cache: Default::default(),
            #[cfg(test)]
            page_read_gate: Arc::default(),
        }))
    }
    #[cfg(all(test, feature = "map-capture"))]
    pub(crate) fn pause_page_reads_for_test(&self) -> test_gate::PageReadPause {
        self.page_read_gate.pause()
    }
    pub fn source_id(&self) -> TextureSourceId {
        self.id
    }
    pub fn metadata(&self) -> &TextureMetadata {
        &self.metadata
    }
    pub fn frame_request(
        &self,
        frame: u32,
        faces: Range<u32>,
        first_source_mip: u8,
        mip_count: u8,
        storage: TextureStorage,
    ) -> Result<TextureFrameRequest, TextureAssetError> {
        let request = TextureFrameRequest {
            source_id: self.id,
            frame,
            faces,
            first_source_mip,
            mip_count,
            storage,
            limits: TexturePageLimits::default(),
        };
        self.images(&request)?;
        Ok(request)
    }
    fn images(
        &self,
        request: &TextureFrameRequest,
    ) -> Result<Vec<vformats::vtf::StoredImage>, TextureAssetError> {
        if request.source_id != self.id
            || request.faces.is_empty()
            || request.mip_count == 0
            || request.frame >= self.metadata.frame_count
            || request.faces.end > self.metadata.face_count
        {
            return Err(TextureAssetError::Selection);
        }
        if let TextureStorage::Bc(format) = request.storage
            && self.metadata.format.and_then(VtfFormat::bc) != Some(format)
        {
            return Err(TextureAssetError::Storage);
        }
        let end = request
            .first_source_mip
            .checked_add(request.mip_count)
            .ok_or(TextureAssetError::Selection)?;
        if end > self.metadata.mip_count {
            return Err(TextureAssetError::Selection);
        }
        let count =
            u64::from(request.faces.end - request.faces.start) * u64::from(request.mip_count);
        if count > self.limits.max_entries as u64 {
            return Err(TextureAssetError::ImageCount(self.limits.max_entries));
        }
        let mut images = Vec::with_capacity(count as usize);
        for face in request.faces.clone() {
            for mip in request.first_source_mip..end {
                images.push(self.layout.stored_image(request.frame, face, mip)?);
            }
        }
        Ok(images)
    }
    pub fn page_cost(
        &self,
        request: &TextureFrameRequest,
    ) -> Result<super::TexturePageCost, TextureAssetError> {
        let images = self.images(request)?;
        let mut resident_bytes = 0usize;
        let mut image_peak = 0usize;
        for image in images {
            let bytes = match request.storage {
                TextureStorage::Bc(_) => image.byte_range.len(),
                TextureStorage::Rgba8 => usize::try_from(
                    u64::from(image.dimensions[0]) * u64::from(image.dimensions[1]) * 4,
                )
                .map_err(|_| TextureAssetError::WorkingCostOverflow)?,
            };
            resident_bytes = resident_bytes
                .checked_add(bytes)
                .ok_or(TextureAssetError::WorkingCostOverflow)?;
            let converted_output = match request.storage {
                TextureStorage::Rgba8 if self.metadata.format != Some(VtfFormat::Rgba8888) => bytes,
                _ => 0,
            };
            image_peak = image_peak.max(
                image
                    .byte_range
                    .len()
                    .checked_add(converted_output)
                    .ok_or(TextureAssetError::WorkingCostOverflow)?,
            );
        }
        let decode_working_bytes = image_peak
            .checked_add(self.reader.decoder_working_bytes())
            .ok_or(TextureAssetError::WorkingCostOverflow)?;
        Ok(TexturePageCost {
            resident_bytes,
            decode_working_bytes,
        })
    }
    pub fn validate_page(
        &self,
        request: &TextureFrameRequest,
    ) -> Result<TexturePageCost, TextureAssetError> {
        let cost = self.page_cost(request)?;
        if cost.resident_bytes > request.limits.resident_bytes {
            return Err(vformats::vtf::VtfError::DecodedTooLarge {
                size: cost.resident_bytes as u64,
                max: request.limits.resident_bytes as u64,
            }
            .into());
        }
        if cost.decode_working_bytes > request.limits.decode_working_bytes {
            return Err(TextureAssetError::WorkingLimit {
                required: cost.decode_working_bytes,
                capacity: request.limits.decode_working_bytes,
            });
        }
        Ok(cost)
    }

    pub(super) fn load_page(
        &self,
        request: &TextureFrameRequest,
        cancellation: &PreparationCancellation,
        working: &Arc<TextureWorkingBudget>,
    ) -> Result<TexturePage, TextureAssetError> {
        cancellation.check()?;
        let images = self.images(request)?;
        let format = self.metadata.format.ok_or(TextureAssetError::Storage)?;
        let cost = self.validate_page(request)?;
        #[cfg(test)]
        self.page_read_gate.wait(cancellation)?;
        let decode_limits = Limits {
            max_entry_bytes: cost.resident_bytes as u64,
            ..self.limits
        };
        let _lease = working
            .request(cost.decode_working_bytes)?
            .acquire_cancellable(cancellation)?;
        let mut layers = Vec::with_capacity((request.faces.end - request.faces.start) as usize);
        for image in images {
            cancellation.check()?;
            let bytes = self.reader.read_range(
                image.byte_range.start as u64..image.byte_range.end as u64,
                cancellation,
            )?;
            if bytes.len() != image.byte_range.len() {
                return Err(TextureAssetError::SourceChanged);
            }
            let bytes = match request.storage {
                TextureStorage::Bc(_) => bytes,
                TextureStorage::Rgba8 if format == VtfFormat::Rgba8888 => bytes,
                TextureStorage::Rgba8 => {
                    vformats::vtf::decode_image_rgba(
                        format,
                        image.dimensions,
                        &bytes,
                        &decode_limits,
                    )?
                    .rgba
                }
            };
            if layers
                .last()
                .is_none_or(|layer: &ResidentTextureLayer| layer.face != image.face)
            {
                layers.push(ResidentTextureLayer {
                    frame: request.frame,
                    face: image.face,
                    mips: Vec::with_capacity(usize::from(request.mip_count)),
                });
            }
            layers
                .last_mut()
                .expect("image creates its layer")
                .mips
                .push(ResidentTextureMip {
                    source_mip: image.source_mip,
                    dimensions: image.dimensions,
                    bytes: TextureBytes::owned(bytes),
                });
        }
        let end = request.first_source_mip + request.mip_count;
        let resident = ResidentTexture::from_page(
            Arc::clone(&self.metadata),
            request.frame..request.frame + 1,
            request.faces.clone(),
            request.first_source_mip..end,
            request.storage,
            layers,
        )?;
        cancellation.check()?;
        Ok(TexturePage {
            request: request.clone(),
            resident,
        })
    }
}

impl super::MaterialResolver {
    #[cfg(test)]
    pub(crate) fn selected_reloadable_paths(&self) -> Vec<String> {
        let mut paths = self
            .shared
            .reloadable_sources
            .cells
            .lock()
            .keys()
            .map(|path| path.as_str().to_owned())
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    pub(crate) fn with_on_demand_textures(&self) -> Self {
        self.variant(super::ResolverConfig {
            texture_loading: super::TextureLoading::OnDemand,
            ..self.config.clone()
        })
    }
    pub(crate) fn texture_loading(&self) -> super::TextureLoading {
        self.config.texture_loading
    }
}

#[derive(Debug)]
struct SelectedReader {
    origin: TextureOrigin,
    reader: Arc<dyn TextureSourceReader>,
}
#[derive(Debug, Default)]
struct SourceCell {
    selected:
        crate::preparation::FlightCell<Result<Arc<SelectedReader>, super::TextureSourceError>>,
    source: std::sync::OnceLock<Arc<ReloadableTextureSource>>,
}
#[derive(Debug, Default)]
pub(super) struct TextureSourceCache {
    cells: Mutex<std::collections::HashMap<super::ContentPath, Arc<SourceCell>>>,
}
impl super::MaterialResolver {
    pub(super) fn selected_reloadable_texture(
        &self,
        path: &super::ContentPath,
    ) -> Result<Arc<ReloadableTextureSource>, super::TextureSourceError> {
        use super::TextureSourceError;
        let cancelled = |error: crate::preparation::Cancelled| TextureSourceError::Preparation {
            path: path.clone(),
            error: error.into(),
        };
        self.check_cancelled().map_err(cancelled)?;
        let cell = self
            .shared
            .reloadable_sources
            .cells
            .lock()
            .entry(path.clone())
            .or_default()
            .clone();
        if let Some(source) = cell.source.get() {
            self.validate_source_decoder(&source.origin, &*source.reader)?;
            return Ok(Arc::clone(source));
        }
        let selected = cell
            .selected
            .get_or_try_init(&self.config.cancellation, || {
                let mut selected = Err(TextureSourceError::Missing);
                for source in self.sources() {
                    self.check_cancelled()?;
                    let origin = TextureOrigin {
                        path: path.clone(),
                        tier: source.tier(),
                    };
                    match source.descriptor(path) {
                        Ok(None) => {}
                        Ok(Some(descriptor)) => {
                            selected = descriptor
                                .into_reader()
                                .map(|(origin, reader)| Arc::new(SelectedReader { origin, reader }))
                                .map_err(|error| TextureSourceError::Read {
                                    origin,
                                    message: error.to_string().into(),
                                });
                            break;
                        }
                        Err(super::SourceError::Discovery(error)) => {
                            selected = Err(TextureSourceError::SourceUnavailable(error));
                            break;
                        }
                        Err(error) => {
                            selected = Err(TextureSourceError::Read {
                                origin,
                                message: error.to_string().into(),
                            });
                            break;
                        }
                    }
                }
                self.check_cancelled()?;
                Ok(selected)
            })
            .map_err(cancelled)??;
        self.validate_source_decoder(&selected.origin, &*selected.reader)?;
        let working = self
            .config
            .texture_source_working_budget
            .clone()
            .or_else(|| self.config.texture_working_budget.clone())
            .unwrap_or_else(|| {
                Arc::new(TextureWorkingBudget::new(
                    usize::try_from(Limits::default().max_input_bytes).unwrap_or(usize::MAX),
                ))
            });
        let source = ReloadableTextureSource::inspect(
            selected.origin.clone(),
            Arc::clone(&selected.reader),
            Limits::default(),
            &self.config.cancellation,
            &working,
        )
        .map_err(|error| match error {
            TextureAssetError::Cancelled(error) => cancelled(error),
            error @ TextureAssetError::WorkingLimit { .. } => TextureSourceError::WorkingBudget {
                origin: selected.origin.clone(),
                error,
                metadata: None,
                inspection: None,
            },
            error => TextureSourceError::Invalid {
                origin: selected.origin.clone(),
                bytes: Arc::new(TextureBytes::owned(Vec::new())),
                error,
            },
        })?;
        let _ = cell.source.set(source);
        Ok(Arc::clone(cell.source.get().expect("source installed")))
    }

    fn validate_source_decoder(
        &self,
        origin: &TextureOrigin,
        reader: &dyn TextureSourceReader,
    ) -> Result<(), super::TextureSourceError> {
        let capacity = self.config.texture_decoder_limit.unwrap_or_else(|| {
            usize::try_from(Limits::default().max_input_bytes).unwrap_or(usize::MAX)
        });
        let required = reader.decoder_working_bytes();
        if required > capacity {
            return Err(super::TextureSourceError::WorkingBudget {
                origin: origin.clone(),
                error: TextureAssetError::WorkingLimit { required, capacity },
                metadata: None,
                inspection: None,
            });
        }
        Ok(())
    }
}

impl super::MaterialResolver {
    pub(super) fn reloadable_residency(
        &self,
        source: Arc<ReloadableTextureSource>,
        frame: u32,
        faces: Range<u32>,
        force_rgba: bool,
    ) -> Result<super::TextureResidency, TextureAssetError> {
        let metadata = source.metadata();
        let max = self
            .config
            .decoded_texture_max_dimension
            .unwrap_or(u32::MAX)
            .max(1);
        let locked = metadata.flags.is_some_and(|flags| {
            flags & (vformats::vtf::texture_flags::NOMIP | vformats::vtf::texture_flags::NOLOD) != 0
        });
        let first = if locked {
            0
        } else {
            (0..metadata.mip_count)
                .find(|&mip| {
                    metadata
                        .original_dimensions
                        .iter()
                        .all(|side| side.checked_shr(u32::from(mip)).unwrap_or(0).max(1) <= max)
                })
                .or_else(|| metadata.mip_count.checked_sub(1))
                .ok_or(TextureAssetError::DimensionLimit(max))?
        };
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
        let mut recipe = source.frame_request(frame, faces, first, end - first, storage)?;
        let defaults = TexturePageLimits::default();
        recipe.limits = TexturePageLimits {
            resident_bytes: self
                .config
                .texture_resource_limit
                .unwrap_or(defaults.resident_bytes),
            decode_working_bytes: self
                .config
                .texture_decoder_limit
                .unwrap_or(defaults.decode_working_bytes),
        };
        source.validate_page(&recipe)?;
        Ok(super::TextureResidency::on_demand_source(source, recipe))
    }
}

impl super::MaterialResolver {
    pub(super) fn resolve_on_demand_texture(
        &self,
        path: &super::ContentPath,
        policy: super::TextureFailurePolicy,
    ) -> Option<Arc<super::ResolvedTexture>> {
        use super::{
            ResolvedTexture, TextureFailurePolicy, TextureLoadOutcome, TextureSourceError,
        };
        let source = match self.selected_reloadable_texture(path) {
            Ok(source) => source,
            Err(TextureSourceError::Missing) => {
                return (policy == TextureFailurePolicy::NativeErrorTexture).then(|| {
                    Arc::new(ResolvedTexture::unavailable(
                        None,
                        TextureLoadOutcome::MissingSource,
                    ))
                });
            }
            Err(TextureSourceError::Invalid {
                error: TextureAssetError::Vtf(error),
                ..
            }) if policy == TextureFailurePolicy::NativeErrorTexture
                && super::native_invalid_texture(&error) =>
            {
                return Some(Arc::new(ResolvedTexture::unavailable(
                    None,
                    TextureLoadOutcome::InvalidSource,
                )));
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
                TextureSourceError::WorkingBudget { metadata, .. }
                | TextureSourceError::EncodedBudget { metadata, .. },
            ) => {
                return Some(Arc::new(ResolvedTexture::unavailable(
                    metadata,
                    TextureLoadOutcome::SourceBudgetRejected,
                )));
            }
            Err(error) => {
                log::warn!("texture {path}: {error}");
                return None;
            }
        };
        let metadata = Arc::new(source.metadata().clone());
        match self.reloadable_residency(source, 0, 0..1, !self.bc_textures_enabled()) {
            Ok(resident) => Some(Arc::new(ResolvedTexture::from_resident(resident, metadata))),
            Err(error) => {
                log::warn!("texture {path}: {error}");
                Some(Arc::new(ResolvedTexture::unavailable(
                    Some(metadata),
                    TextureLoadOutcome::QualityRejected,
                )))
            }
        }
    }
}

pub(super) struct ResourceBytes {
    pub bytes: Vec<u8>,
    _working: super::texture_working::TextureWorkingLease,
}
impl ReloadableTextureSource {
    pub(super) fn sprite_sheet_payload(
        &self,
        cancel: &PreparationCancellation,
        working: &Arc<TextureWorkingBudget>,
        limit: usize,
        extra_working: usize,
    ) -> Result<Option<ResourceBytes>, super::sprite_sheet::SpriteSheetResolveError> {
        let Some(length_range) = self.layout.sprite_sheet_length_range()? else {
            return Ok(None);
        };
        let header_cost = self
            .reader
            .decoder_working_bytes()
            .checked_add(8)
            .ok_or(TextureAssetError::WorkingCostOverflow)?;
        let header_lease = working.request(header_cost)?.acquire_cancellable(cancel)?;
        let length = self.reader.read_range(length_range, cancel)?;
        let size = u32::from_le_bytes(
            length
                .as_slice()
                .try_into()
                .map_err(|_| vformats::vtf::SpriteSheetError::InvalidResource)?,
        );
        drop(header_lease);
        let range = self
            .layout
            .sprite_sheet_payload_range(size)?
            .ok_or(vformats::vtf::SpriteSheetError::InvalidResource)?;
        if size as usize > limit {
            return Err(vformats::vtf::SpriteSheetError::Limit {
                metric: vformats::vtf::SpriteSheetMetric::MetadataBytes,
                required: size as usize,
                capacity: limit,
            }
            .into());
        }
        let cost = (size as usize)
            .checked_mul(2)
            .and_then(|n| n.checked_add(self.reader.decoder_working_bytes()))
            .and_then(|n| n.checked_add(extra_working))
            .ok_or(TextureAssetError::WorkingCostOverflow)?;
        let lease = working.request(cost)?.acquire_cancellable(cancel)?;
        let bytes = self.reader.read_range(range, cancel)?;
        Ok(Some(ResourceBytes {
            bytes,
            _working: lease,
        }))
    }
}

pub(super) enum SelectedTexture {
    Resident(Arc<super::texture_asset::SelectedTextureAsset>),
    Reloadable(Arc<ReloadableTextureSource>),
}
impl SelectedTexture {
    pub(super) fn metadata(&self) -> &TextureMetadata {
        match self {
            Self::Resident(selected) => selected.asset.metadata(),
            Self::Reloadable(source) => source.metadata(),
        }
    }
    pub(super) fn metadata_arc(&self) -> Arc<TextureMetadata> {
        match self {
            Self::Resident(selected) => selected.asset.metadata_arc(),
            Self::Reloadable(source) => Arc::clone(&source.metadata),
        }
    }
    pub(super) fn validate_payload(&self) -> Result<(), vformats::vtf::VtfError> {
        match self {
            Self::Resident(selected) => selected.asset.validate_payload(),
            Self::Reloadable(_) => Ok(()),
        }
    }
}
impl super::MaterialResolver {
    pub(super) fn selected_texture_source(
        &self,
        path: &super::ContentPath,
    ) -> Result<SelectedTexture, super::TextureSourceError> {
        if self.config.texture_loading == super::TextureLoading::OnDemand {
            self.selected_reloadable_texture(path)
                .map(SelectedTexture::Reloadable)
        } else {
            self.selected_texture_asset(path)
                .map(SelectedTexture::Resident)
        }
    }
    pub(super) fn selected_source_residency(
        &self,
        source: &SelectedTexture,
        frame: u32,
        faces: Range<u32>,
        force_rgba: bool,
    ) -> Result<super::TextureResidency, TextureAssetError> {
        match source {
            SelectedTexture::Resident(selected) => {
                self.resident_texture(selected, faces, force_rgba)
            }
            SelectedTexture::Reloadable(source) => {
                self.reloadable_residency(Arc::clone(source), frame, faces, force_rgba)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::materials::{ContentPath, ContentSourceTier};
    use crate::test_support::fixture_vtf_bytes;
    #[derive(Debug)]
    struct RecordingSource {
        bytes: Vec<u8>,
        reads: Mutex<Vec<Range<u64>>>,
    }
    impl TextureSourceReader for RecordingSource {
        fn len(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn read_range(
            &self,
            range: Range<u64>,
            cancel: &PreparationCancellation,
        ) -> Result<Vec<u8>, TextureAssetError> {
            cancel.check()?;
            self.reads.lock().push(range.clone());
            Ok(self.bytes[range.start as usize..range.end as usize].to_vec())
        }
    }
    fn source(bytes: Vec<u8>) -> (Arc<ReloadableTextureSource>, Arc<RecordingSource>) {
        let reader = Arc::new(RecordingSource {
            bytes,
            reads: Default::default(),
        });
        let source = ReloadableTextureSource::inspect(
            TextureOrigin {
                path: ContentPath::new("materials/page.vtf").unwrap(),
                tier: ContentSourceTier::Addon,
            },
            reader.clone(),
            Limits::default(),
            &PreparationCancellation::default(),
            &Arc::new(TextureWorkingBudget::new(1 << 20)),
        )
        .unwrap();
        reader.reads.lock().clear();
        (source, reader)
    }
    #[test]
    fn selected_frame_owns_only_its_mips_and_preserves_authored_count() {
        let mut bytes =
            fixture_vtf_bytes(4, 4, VtfFormat::Rgba8888, &[&[1; 8], &[2; 32], &[3; 128]]);
        bytes[24..26].copy_from_slice(&2u16.to_le_bytes());
        let (source, reader) = source(bytes);
        assert_eq!(source.metadata().frame_count, 2);
        let request = source
            .frame_request(1, 0..1, 0, 3, TextureStorage::Rgba8)
            .unwrap();
        let budget = Arc::new(TextureWorkingBudget::new(1 << 20));
        let page = source
            .load_page(&request, &PreparationCancellation::default(), &budget)
            .unwrap();
        assert_eq!(budget.current_bytes(), 0);
        assert_eq!(reader.reads.lock().as_slice(), &[168..232, 88..104, 68..72]);
        assert_eq!(page.resident().metadata().frame_count, 2);
        assert_eq!(page.resident().layers().len(), 1);
        assert_eq!(page.resident().layers()[0].frame, 1);
        for mip in &page.resident().layers()[0].mips {
            assert_eq!(mip.bytes.backing_len(), mip.bytes.as_slice().len());
        }
        assert_eq!(source.page_cost(&request).unwrap().resident_bytes, 84);
    }
    #[test]
    fn equal_pages_deduplicate_content_without_conflating_source_identity() {
        let bytes = fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[13; 4]]);
        let (a, _) = source(bytes.clone());
        let (b, _) = source(bytes);
        assert_ne!(a.source_id(), b.source_id());
        let budget = Arc::new(TextureWorkingBudget::new(1024));
        let a = a
            .load_page(
                &a.frame_request(0, 0..1, 0, 1, TextureStorage::Rgba8)
                    .unwrap(),
                &PreparationCancellation::default(),
                &budget,
            )
            .unwrap();
        let b = b
            .load_page(
                &b.frame_request(0, 0..1, 0, 1, TextureStorage::Rgba8)
                    .unwrap(),
                &PreparationCancellation::default(),
                &budget,
            )
            .unwrap();
        assert_eq!(a.resident().id(), b.resident().id());
    }
    #[test]
    fn malformed_requests_and_cancellation_do_not_read_payload() {
        let (source, reader) = source(fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[0; 4]]));
        assert!(
            source
                .frame_request(1, 0..1, 0, 1, TextureStorage::Rgba8)
                .is_err()
        );
        assert!(
            source
                .frame_request(0, 0..u32::MAX, 0, 1, TextureStorage::Rgba8)
                .is_err()
        );
        assert!(
            source
                .frame_request(0, 0..1, 0, 2, TextureStorage::Rgba8)
                .is_err()
        );
        let request = source
            .frame_request(0, 0..1, 0, 1, TextureStorage::Rgba8)
            .unwrap();
        let cancel = PreparationCancellation::default();
        cancel.cancel();
        assert!(matches!(
            source.load_page(&request, &cancel, &Arc::new(TextureWorkingBudget::new(64))),
            Err(TextureAssetError::Cancelled(_))
        ));
        assert!(reader.reads.lock().is_empty());
    }
    #[test]
    fn selected_path_replacement_and_in_place_changes_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source");
        std::fs::write(&path, [1; 4]).unwrap();
        let file = File::open(&path).unwrap();
        let source = FrozenPathSource::new(path.clone(), &file).unwrap();
        std::fs::write(dir.path().join("replacement"), [2; 4]).unwrap();
        std::fs::rename(dir.path().join("replacement"), &path).unwrap();
        assert!(matches!(
            source.read_range(0..4, &PreparationCancellation::default()),
            Err(TextureAssetError::SourceChanged)
        ));
        let source = FrozenFileSource::new(File::open(&path).unwrap()).unwrap();
        std::fs::write(&path, [3; 5]).unwrap();
        assert!(matches!(
            source.read_range(0..4, &PreparationCancellation::default()),
            Err(TextureAssetError::SourceChanged)
        ));
    }
    #[test]
    fn on_demand_material_resolution_retains_no_encoded_payload() {
        let resolver=super::super::MaterialResolver::prop_lighting_fixture(vec![
            ("materials/test/image.vtf",fixture_vtf_bytes(4,4,VtfFormat::Rgba8888,&[&[7;64]])),
            ("materials/test/image.vmt",br#"LightmappedGeneric { "$basetexture" "test/image" "$bumpmap" "test/image" }"#.to_vec()),
        ],Vec::new()).with_on_demand_textures();
        let resolved = resolver.resolve_primary(&[], "test/image");
        let texture = resolved.into_material().unwrap().texture.unwrap();
        assert_eq!(
            texture.load_outcome(),
            super::super::TextureLoadOutcome::Loaded
        );
        assert!(texture.resident().is_none());
        assert!(texture.on_demand().is_some());
        assert_eq!(resolver.resident_texture_asset_count(), 0);
        assert!(resolver.shared.texture_assets.selected_paths().is_empty());
    }
}

#[cfg(test)]
mod large_source_tests {
    use super::*;
    use crate::bridge::materials::{ContentPath, ContentSourceTier};

    #[derive(Debug)]
    struct VirtualAnimation {
        header: Vec<u8>,
        reads: Mutex<Vec<Range<u64>>>,
    }
    impl TextureSourceReader for VirtualAnimation {
        fn len(&self) -> u64 {
            self.header.len() as u64 + 4096 * 524_288
        }
        fn read_range(
            &self,
            range: Range<u64>,
            cancellation: &PreparationCancellation,
        ) -> Result<Vec<u8>, TextureAssetError> {
            cancellation.check()?;
            assert!(range.end <= self.len());
            assert!(range.end - range.start <= 524_288);
            self.reads.lock().push(range.clone());
            let mut bytes = vec![173; (range.end - range.start) as usize];
            for (offset, byte) in bytes.iter_mut().enumerate() {
                if let Some(header) = self.header.get(range.start as usize + offset) {
                    *byte = *header;
                }
            }
            Ok(bytes)
        }
    }

    #[test]
    fn large_virtual_animation_materializes_only_one_late_frame() {
        let mut header =
            crate::test_support::fixture_vtf_bytes(1024, 1024, VtfFormat::Dxt1, &[&[]]);
        header[24..26].copy_from_slice(&4096_u16.to_le_bytes());
        let reader = Arc::new(VirtualAnimation {
            header,
            reads: Mutex::default(),
        });
        let working = Arc::new(TextureWorkingBudget::new(524_288));
        let source = ReloadableTextureSource::inspect(
            TextureOrigin {
                path: ContentPath::new("materials/test/large.vtf").unwrap(),
                tier: ContentSourceTier::Addon,
            },
            reader.clone(),
            Limits::default(),
            &PreparationCancellation::default(),
            &working,
        )
        .unwrap();
        assert!(reader.len() > Limits::default().max_input_bytes);
        let request = source
            .frame_request(
                4095,
                0..1,
                0,
                1,
                TextureStorage::Bc(vformats::vtf::BcFormat::Bc1),
            )
            .unwrap();
        let page = source
            .load_page(&request, &PreparationCancellation::default(), &working)
            .unwrap();
        assert_eq!(page.resident().byte_len(), 524_288);
        assert_eq!(page.resident().metadata().frame_count, 4096);
        assert!(
            page.resident().layers()[0].mips[0]
                .bytes
                .as_slice()
                .iter()
                .all(|&byte| byte == 173)
        );
        let reads = reader.reads.lock();
        assert_eq!(reads.len(), 2);
        assert_eq!(reads[0], 0..vformats::vtf::MAX_HEADER_BYTES as u64);
        assert_eq!(reads[1], (reader.len() - 524_288)..reader.len());
        assert!(reads[1].start > Limits::default().max_input_bytes);
        drop(reads);
        assert_eq!(working.peak_bytes(), 524_288);
        assert_eq!(working.current_bytes(), 0);
    }
}
