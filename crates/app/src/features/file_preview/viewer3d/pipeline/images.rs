use super::wgpu;
use crate::bridge::materials::{
    CachedTexturePage, ResidentTexture, TextureFrameRequest, TextureMetadata, TexturePageIdentity,
    TextureReadTransfer, TextureResidentId, TextureStorage,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum ImageView {
    D2(u32),
    Array,
    Cube(u32),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ViewKey {
    dimension: ImageView,
    transfer: TextureReadTransfer,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ImageKey {
    resident: TextureResidentId,
    transfer: Option<TextureReadTransfer>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PageImageKey {
    identity: TexturePageIdentity,
    transfer: Option<TextureReadTransfer>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum UploadError {
    #[error("texture has no resident image data")]
    Empty,
    #[error(transparent)]
    Upload(#[from] super::texture_upload::UploadError),
    #[error("texture exceeds device dimensions or layer limits")]
    DeviceLimit,
    #[error("texture layer {layer}, mip {mip} has invalid dimensions or payload")]
    InvalidMip { layer: usize, mip: usize },
    #[error("texture allocation size overflows")]
    SizeOverflow,
    #[error("texture page request is inconsistent with source metadata")]
    InvalidPage,
    #[error("texture view is incompatible with its image allocation")]
    InvalidView,
    #[error(transparent)]
    Decode(#[from] vformats::vtf::VtfError),
}

#[derive(Debug)]
pub(super) struct Image {
    resident_id: TextureResidentId,
    texture: super::texture_upload::UploadedTexture,
    format: wgpu::TextureFormat,
    layers: u32,
    dimensions: [u32; 2],
    bytes: u64,
    views: Mutex<HashMap<ViewKey, super::texture_upload::UploadedView>>,
}

impl Image {
    pub(super) fn resident_id(&self) -> &TextureResidentId {
        &self.resident_id
    }
    pub(super) fn byte_len(&self) -> u64 {
        self.bytes
    }

    fn view(&self, key: ViewKey) -> Result<super::texture_upload::UploadedView, UploadError> {
        let (dimension, first, count) = match key.dimension {
            ImageView::D2(layer) => (wgpu::TextureViewDimension::D2, layer, 1),
            ImageView::Array => (wgpu::TextureViewDimension::D2Array, 0, self.layers),
            ImageView::Cube(first) if self.dimensions[0] == self.dimensions[1] => {
                (wgpu::TextureViewDimension::Cube, first, 6)
            }
            ImageView::Cube(_) => return Err(UploadError::InvalidView),
        };
        if first.checked_add(count).is_none_or(|end| end > self.layers) {
            return Err(UploadError::InvalidView);
        }
        let mut views = self.views.lock().expect("image view cache lock");
        Ok(views
            .entry(key)
            .or_insert_with(|| {
                self.texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("file_preview.image.view"),
                    format: Some(match key.transfer {
                        TextureReadTransfer::Unmodified => self.format.remove_srgb_suffix(),
                        TextureReadTransfer::SrgbToLinear => self.format.add_srgb_suffix(),
                    }),
                    dimension: Some(dimension),
                    base_array_layer: first,
                    array_layer_count: Some(count),
                    ..Default::default()
                })
            })
            .clone())
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub(super) struct ImageStats {
    pub allocations: usize,
    pub estimated_bytes: u64,
    pub uploads: u64,
    pub cache_hits: u64,
    pub compatible_transfer_views: bool,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub(super) struct ImageResidency {
    pub allocations: usize,
    pub estimated_bytes: u64,
}

#[cfg(test)]
pub(super) fn lease_stats(images: &[Arc<Image>]) -> ImageResidency {
    let mut seen = std::collections::HashSet::new();
    let mut stats = ImageResidency::default();
    for image in images {
        if seen.insert(Arc::as_ptr(image)) {
            stats.allocations += 1;
            stats.estimated_bytes = stats.estimated_bytes.saturating_add(image.bytes);
        }
    }
    stats
}

#[derive(Debug, Default)]
pub(super) struct ImageCache {
    images: HashMap<ImageKey, Weak<Image>>,
    pages: HashMap<PageImageKey, Weak<Image>>,
    compatible_views: bool,
    uploads: u64,
    hits: u64,
}

impl ImageCache {
    pub(super) fn new(capabilities: Option<&wgpu::DownlevelCapabilities>) -> Self {
        Self {
            compatible_views: capabilities.is_some_and(|capabilities| {
                capabilities
                    .flags
                    .contains(wgpu::DownlevelFlags::VIEW_FORMATS)
            }),
            ..Self::default()
        }
    }

    pub(super) fn compatible_transfer_views(&self) -> bool {
        self.compatible_views
    }

    pub(super) fn page_request_for_device(
        device: &wgpu::Device,
        request: &TextureFrameRequest,
        metadata: &TextureMetadata,
    ) -> TextureFrameRequest {
        let mut normalized = request.clone();
        normalized.storage = storage_for_device(
            request.storage,
            selected_dimensions(request, metadata),
            super::materials::bc_upload_enabled(device),
        );
        normalized
    }

    pub(super) fn alias_page(
        &mut self,
        request: &TextureFrameRequest,
        transfer: TextureReadTransfer,
        image: &Arc<Image>,
    ) {
        self.pages
            .insert(self.page_key(request, transfer), Arc::downgrade(image));
    }

    pub(super) fn forget_page(
        &mut self,
        request: &TextureFrameRequest,
        transfer: TextureReadTransfer,
    ) {
        self.pages.remove(&self.page_key(request, transfer));
    }

    pub(super) fn lookup_allocation(
        &mut self,
        resident: &TextureResidentId,
        transfer: TextureReadTransfer,
    ) -> Option<Arc<Image>> {
        let image = self
            .images
            .get(&ImageKey {
                resident: resident.clone(),
                transfer: (!self.compatible_views).then_some(transfer),
            })
            .and_then(Weak::upgrade)?;
        self.hits = self.hits.saturating_add(1);
        Some(image)
    }

    pub(super) fn allocation_id(
        device: &wgpu::Device,
        source: &ResidentTexture,
    ) -> TextureResidentId {
        let dimensions = source
            .layers()
            .first()
            .and_then(|layer| layer.mips.first())
            .map_or([0, 0], |mip| mip.dimensions);
        let mut id = source.id().clone();
        id.storage = storage_for_device(
            id.storage,
            dimensions,
            super::materials::bc_upload_enabled(device),
        );
        id
    }

    pub(super) fn resident_allocation_cost(
        device: &wgpu::Device,
        source: &ResidentTexture,
    ) -> Result<u64, UploadError> {
        let storage = Self::allocation_id(device, source).storage;
        source
            .layers()
            .iter()
            .flat_map(|layer| &layer.mips)
            .try_fold(0u64, |total, mip| {
                total
                    .checked_add(
                        byte_len(storage, mip.dimensions).ok_or(UploadError::SizeOverflow)?,
                    )
                    .ok_or(UploadError::SizeOverflow)
            })
    }

    pub(super) fn page_allocation_cost(
        device: &wgpu::Device,
        request: &TextureFrameRequest,
        metadata: &TextureMetadata,
    ) -> Result<u64, UploadError> {
        let limits = device.limits();
        page_cost(
            request,
            metadata,
            &limits,
            super::materials::bc_upload_enabled(device),
        )
    }

    #[cfg(test)]
    pub(super) fn stats(&self) -> ImageStats {
        let mut stats = ImageStats {
            uploads: self.uploads,
            cache_hits: self.hits,
            compatible_transfer_views: self.compatible_views,
            ..ImageStats::default()
        };
        for image in self.images.values().filter_map(Weak::upgrade) {
            stats.allocations += 1;
            stats.estimated_bytes = stats.estimated_bytes.saturating_add(image.bytes);
        }
        stats
    }

    pub(super) fn trim(&mut self) {
        self.images.retain(|_, image| image.strong_count() != 0);
        self.pages.retain(|_, image| image.strong_count() != 0);
    }

    fn page_key(
        &self,
        request: &TextureFrameRequest,
        transfer: TextureReadTransfer,
    ) -> PageImageKey {
        PageImageKey {
            identity: request.identity(),
            transfer: (!self.compatible_views).then_some(transfer),
        }
    }

    pub(super) fn lookup_page(
        &mut self,
        request: &TextureFrameRequest,
        transfer: TextureReadTransfer,
    ) -> Option<Arc<Image>> {
        let image = self
            .pages
            .get(&self.page_key(request, transfer))
            .and_then(Weak::upgrade)?;
        self.hits = self.hits.saturating_add(1);
        Some(image)
    }

    fn get(
        &mut self,
        device: &wgpu::Device,
        uploader: &super::texture_upload::SharedUploader,
        source: &ResidentTexture,
        transfer: TextureReadTransfer,
    ) -> Result<Arc<Image>, UploadError> {
        source
            .layers()
            .first()
            .and_then(|layer| layer.mips.first())
            .ok_or(UploadError::Empty)?;
        let key = ImageKey {
            resident: Self::allocation_id(device, source),
            transfer: (!self.compatible_views).then_some(transfer),
        };
        if let Some(image) = self.images.get(&key).and_then(Weak::upgrade) {
            self.hits = self.hits.saturating_add(1);
            return Ok(image);
        }
        let mut format = format(key.resident.storage);
        if !self.compatible_views && transfer == TextureReadTransfer::SrgbToLinear {
            format = format.add_srgb_suffix();
        }
        let image = Arc::new(upload(
            device,
            uploader,
            source,
            key.resident.storage,
            format,
            self.compatible_views,
        )?);
        self.images.insert(key, Arc::downgrade(&image));
        self.uploads = self.uploads.saturating_add(1);
        Ok(image)
    }

    pub(super) fn lease(
        &mut self,
        uploader: &super::texture_upload::SharedUploader,
    ) -> ImageLease<'_> {
        ImageLease {
            cache: self,
            uploader: Arc::clone(uploader),
            batch: None,
            images: Vec::new(),
            issues: Vec::new(),
        }
    }

    pub(super) fn lease_batched<'a>(
        &'a mut self,
        batch: &'a super::texture_upload::UploadLeaseBatch,
    ) -> ImageLease<'a> {
        let mut lease = self.lease(batch.uploader());
        lease.batch = Some(batch);
        lease
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub(super) enum ImageUploadCause {
    Resource,
    Capacity,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct ImageUploadIssue {
    pub cause: ImageUploadCause,
    pub material: String,
    pub role: String,
    pub reason: String,
}

impl ImageUploadIssue {
    fn from_upload(material: &str, role: &str, error: &UploadError) -> Self {
        let cause = if matches!(
            error,
            UploadError::Upload(super::texture_upload::UploadError::ImageBudget { .. })
        ) {
            ImageUploadCause::Capacity
        } else {
            ImageUploadCause::Resource
        };
        Self {
            cause,
            material: material.to_owned(),
            role: role.to_owned(),
            reason: error.to_string(),
        }
    }
}

pub(super) struct ImageLease<'a> {
    cache: &'a mut ImageCache,
    uploader: super::texture_upload::SharedUploader,
    batch: Option<&'a super::texture_upload::UploadLeaseBatch>,
    images: Vec<Arc<Image>>,
    issues: Vec<ImageUploadIssue>,
}

impl ImageLease<'_> {
    pub(super) fn cached_page_view(
        &mut self,
        request: &TextureFrameRequest,
        transfer: TextureReadTransfer,
        dimension: ImageView,
    ) -> Option<Result<super::texture_upload::UploadedView, UploadError>> {
        let image = self.cache.lookup_page(request, transfer)?;
        Some(
            image
                .view(ViewKey {
                    dimension,
                    transfer,
                })
                .inspect(|_| self.images.push(Arc::clone(&image))),
        )
    }

    pub(super) fn page_view(
        &mut self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        page: &CachedTexturePage,
        transfer: TextureReadTransfer,
        dimension: ImageView,
    ) -> Result<super::texture_upload::UploadedView, UploadError> {
        validate_view(page.resident(), dimension)?;
        let image = self
            .cache
            .get(device, &self.uploader, page.resident(), transfer)?;
        let view = image.view(ViewKey {
            dimension,
            transfer,
        })?;
        self.cache.alias_page(page.request(), transfer, &image);
        self.images.push(image);
        Ok(view)
    }

    pub(super) fn view(
        &mut self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        source: &ResidentTexture,
        transfer: TextureReadTransfer,
        dimension: ImageView,
    ) -> Result<super::texture_upload::UploadedView, UploadError> {
        validate_view(source, dimension)?;
        let image = self.cache.get(device, &self.uploader, source, transfer)?;
        let view = image.view(ViewKey {
            dimension,
            transfer,
        })?;
        self.images.push(image);
        Ok(view)
    }

    pub(super) fn reject(&mut self, material: &str, role: &str, reason: impl std::fmt::Display) {
        let reason = reason.to_string();
        log::warn!("material {material} {role}: {reason}");
        self.issues.push(ImageUploadIssue {
            cause: ImageUploadCause::Resource,
            material: material.to_owned(),
            role: role.to_owned(),
            reason,
        });
    }

    pub(super) fn reject_upload(&mut self, material: &str, role: &str, error: &UploadError) {
        let issue = ImageUploadIssue::from_upload(material, role, error);
        log::warn!("material {material} {role}: {error}");
        self.issues.push(issue);
    }

    pub(super) fn finish(mut self) -> (Vec<Arc<Image>>, Vec<ImageUploadIssue>) {
        self.flush();
        let mut seen = std::collections::HashSet::new();
        self.images.retain(|image| seen.insert(Arc::as_ptr(image)));
        (
            std::mem::take(&mut self.images),
            std::mem::take(&mut self.issues),
        )
    }

    fn flush(&self) {
        if self.batch.is_none() {
            self.uploader.lock().expect("texture uploader lock").flush();
        }
    }
}

impl Drop for ImageLease<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

fn validate_view(source: &ResidentTexture, view: ImageView) -> Result<(), UploadError> {
    let dimensions = source
        .layers()
        .first()
        .and_then(|layer| layer.mips.first())
        .ok_or(UploadError::Empty)?
        .dimensions;
    let layers = u32::try_from(source.layers().len()).map_err(|_| UploadError::DeviceLimit)?;
    let (first, count) = match view {
        ImageView::D2(first) => (first, 1),
        ImageView::Array => (0, layers),
        ImageView::Cube(first) if dimensions[0] == dimensions[1] => (first, 6),
        ImageView::Cube(_) => return Err(UploadError::InvalidView),
    };
    if count == 0 || first.checked_add(count).is_none_or(|end| end > layers) {
        Err(UploadError::InvalidView)
    } else {
        Ok(())
    }
}

fn storage_for_device(
    storage: TextureStorage,
    dimensions: [u32; 2],
    supports_bc: bool,
) -> TextureStorage {
    if matches!(storage, TextureStorage::Bc(_))
        && (!supports_bc || dimensions.iter().any(|side| side % 4 != 0))
    {
        TextureStorage::Rgba8
    } else {
        storage
    }
}

fn format(storage: TextureStorage) -> wgpu::TextureFormat {
    use vformats::vtf::BcFormat;
    match storage {
        TextureStorage::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
        TextureStorage::Bc(BcFormat::Bc1) => wgpu::TextureFormat::Bc1RgbaUnorm,
        TextureStorage::Bc(BcFormat::Bc2) => wgpu::TextureFormat::Bc2RgbaUnorm,
        TextureStorage::Bc(BcFormat::Bc3) => wgpu::TextureFormat::Bc3RgbaUnorm,
    }
}

fn selected_dimensions(request: &TextureFrameRequest, metadata: &TextureMetadata) -> [u32; 2] {
    metadata.original_dimensions.map(|side| {
        side.checked_shr(u32::from(request.first_source_mip))
            .unwrap_or(0)
            .max(1)
    })
}

fn page_cost(
    request: &TextureFrameRequest,
    metadata: &TextureMetadata,
    limits: &wgpu::Limits,
    supports_bc: bool,
) -> Result<u64, UploadError> {
    let layers = request
        .faces
        .end
        .checked_sub(request.faces.start)
        .filter(|&count| count != 0)
        .ok_or(UploadError::InvalidPage)?;
    let end = request
        .first_source_mip
        .checked_add(request.mip_count)
        .ok_or(UploadError::InvalidPage)?;
    if metadata.original_dimensions.contains(&0)
        || request.frame >= metadata.frame_count
        || request.faces.end > metadata.face_count
        || request.mip_count == 0
        || end > metadata.mip_count
        || matches!(request.storage, TextureStorage::Bc(format) if metadata.format.and_then(vformats::vtf::VtfFormat::bc) != Some(format))
    {
        return Err(UploadError::InvalidPage);
    }
    let dimensions = selected_dimensions(request, metadata);
    if dimensions
        .iter()
        .any(|&side| side > limits.max_texture_dimension_2d)
        || layers > limits.max_texture_array_layers
    {
        return Err(UploadError::DeviceLimit);
    }
    if usize::from(request.mip_count)
        > (u32::BITS - dimensions[0].max(dimensions[1]).leading_zeros()) as usize
    {
        return Err(UploadError::InvalidPage);
    }
    chain_byte_len(
        storage_for_device(request.storage, dimensions, supports_bc),
        dimensions,
        request.mip_count,
        layers,
    )
    .ok_or(UploadError::SizeOverflow)
}

fn chain_byte_len(
    storage: TextureStorage,
    dimensions: [u32; 2],
    mips: u8,
    layers: u32,
) -> Option<u64> {
    (0..mips).try_fold(0_u64, |total, mip| {
        let dimensions =
            dimensions.map(|side| side.checked_shr(u32::from(mip)).unwrap_or(0).max(1));
        total.checked_add(byte_len(storage, dimensions)?.checked_mul(u64::from(layers))?)
    })
}

fn byte_len(storage: TextureStorage, dimensions: [u32; 2]) -> Option<u64> {
    let [width, height] = dimensions.map(u64::from);
    match storage {
        TextureStorage::Rgba8 => width.checked_mul(height)?.checked_mul(4),
        TextureStorage::Bc(format) => width
            .div_ceil(4)
            .checked_mul(height.div_ceil(4))?
            .checked_mul(u64::from(format.block_bytes())),
    }
}

fn upload(
    device: &wgpu::Device,
    uploader: &super::texture_upload::SharedUploader,
    source: &ResidentTexture,
    storage: TextureStorage,
    format: wgpu::TextureFormat,
    compatible_views: bool,
) -> Result<Image, UploadError> {
    let layers = source.layers();
    let first = layers
        .first()
        .and_then(|layer| layer.mips.first())
        .ok_or(UploadError::Empty)?;
    let dimensions = first.dimensions;
    let limits = device.limits();
    let layer_count = u32::try_from(layers.len()).map_err(|_| UploadError::DeviceLimit)?;
    if dimensions.contains(&0)
        || dimensions
            .iter()
            .any(|&side| side > limits.max_texture_dimension_2d)
        || layer_count > limits.max_texture_array_layers
    {
        return Err(UploadError::DeviceLimit);
    }
    let frame_count = source
        .id()
        .frames
        .end
        .checked_sub(source.id().frames.start)
        .ok_or(UploadError::Empty)?;
    let face_count = source
        .id()
        .faces
        .end
        .checked_sub(source.id().faces.start)
        .ok_or(UploadError::Empty)?;
    if face_count == 0 || frame_count.checked_mul(face_count) != Some(layer_count) {
        return Err(UploadError::Empty);
    }
    let mip_count = layers[0].mips.len();
    if mip_count != usize::from(source.id().mip_count)
        || first.source_mip != source.id().first_source_mip
        || dimensions
            != source.metadata().original_dimensions.map(|side| {
                side.checked_shr(u32::from(first.source_mip))
                    .unwrap_or(0)
                    .max(1)
            })
        || mip_count > (u32::BITS - dimensions[0].max(dimensions[1]).leading_zeros()) as usize
    {
        return Err(UploadError::InvalidMip {
            layer: 0,
            mip: mip_count,
        });
    }
    let mut uploader = uploader.lock().expect("texture uploader lock");
    let mut plans = Vec::new();
    plans
        .try_reserve(
            layers
                .len()
                .checked_mul(mip_count)
                .ok_or(UploadError::SizeOverflow)?,
        )
        .map_err(|_| super::texture_upload::UploadError::HostAllocation)?;
    let mut bytes = 0_u64;
    for (layer_index, layer) in layers.iter().enumerate() {
        if layer.mips.len() != mip_count
            || layer.frame != source.id().frames.start + layer_index as u32 / face_count
            || layer.face != source.id().faces.start + layer_index as u32 % face_count
        {
            return Err(UploadError::InvalidMip {
                layer: layer_index,
                mip: layer.mips.len(),
            });
        }
        for (level, mip) in layer.mips.iter().enumerate() {
            let expected =
                byte_len(source.id().storage, mip.dimensions).ok_or(UploadError::SizeOverflow)?;
            if mip.dimensions != dimensions.map(|side| (side >> level).max(1))
                || mip.bytes.as_slice().len() as u64 != expected
                || usize::from(mip.source_mip) != usize::from(first.source_mip) + level
            {
                return Err(UploadError::InvalidMip {
                    layer: layer_index,
                    mip: level,
                });
            }
            plans.push(uploader.plan(
                super::texture_upload::Input {
                    storage: source.id().storage,
                    dimensions: mip.dimensions,
                    bytes: mip.bytes.as_slice(),
                },
                format,
            )?);
            bytes = bytes
                .checked_add(byte_len(storage, mip.dimensions).ok_or(UploadError::SizeOverflow)?)
                .ok_or(UploadError::SizeOverflow)?;
        }
    }
    let alternate_format = [format.add_srgb_suffix()];
    let texture = uploader.allocate_sampled(&wgpu::TextureDescriptor {
        label: Some("file_preview.image"),
        size: wgpu::Extent3d {
            width: dimensions[0],
            height: dimensions[1],
            depth_or_array_layers: layer_count,
        },
        mip_level_count: mip_count as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: if compatible_views {
            &alternate_format
        } else {
            &[]
        },
    })?;
    for (layer_index, layer) in layers.iter().enumerate() {
        for (level, mip) in layer.mips.iter().enumerate() {
            uploader.write_planned(
                &texture,
                level as u32,
                wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer_index as u32,
                },
                super::texture_upload::Input {
                    storage: source.id().storage,
                    dimensions: mip.dimensions,
                    bytes: mip.bytes.as_slice(),
                },
                plans[layer_index * mip_count + level],
            )?;
        }
    }
    drop(uploader);
    let mut resident_id = source.id().clone();
    resident_id.storage = storage;
    Ok(Image {
        resident_id,
        texture,
        format,
        layers: layer_count,
        dimensions,
        bytes,
        views: Mutex::new(HashMap::new()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vformats::vtf::BcFormat;

    #[test]
    fn selected_page_cost_counts_all_mips_faces_and_actual_bc_fallback() {
        let native = chain_byte_len(TextureStorage::Bc(BcFormat::Bc1), [8, 8], 4, 6).unwrap();
        assert_eq!(native, (32 + 8 + 8 + 8) * 6);
        let fallback = storage_for_device(TextureStorage::Bc(BcFormat::Bc1), [8, 8], false);
        assert_eq!(
            chain_byte_len(fallback, [8, 8], 4, 6),
            Some((256 + 64 + 16 + 4) * 6)
        );
        let non_block_base = storage_for_device(TextureStorage::Bc(BcFormat::Bc1), [6, 8], true);
        assert_eq!(
            chain_byte_len(non_block_base, [6, 8], 3, 1),
            Some(192 + 48 + 8)
        );
        assert!(chain_byte_len(TextureStorage::Rgba8, [u32::MAX; 2], 1, u32::MAX).is_none());
    }

    #[test]
    fn compressed_base_extents_require_both_device_support_and_complete_blocks() {
        for format in [BcFormat::Bc1, BcFormat::Bc2, BcFormat::Bc3] {
            let storage = TextureStorage::Bc(format);
            assert_eq!(storage_for_device(storage, [8, 4], true), storage);
            assert_eq!(
                storage_for_device(storage, [8, 4], false),
                TextureStorage::Rgba8
            );
            for dimensions in [[1, 1], [2, 2], [8, 2], [6, 8]] {
                assert_eq!(
                    storage_for_device(storage, dimensions, true),
                    TextureStorage::Rgba8
                );
            }
        }
    }

    #[test]
    fn color_space_views_preserve_decal_modulate_encoded_channels() {
        use crate::bridge::materials::{SurfaceFamily, TextureRole};
        let transfer = TextureRole::Base {
            family: SurfaceFamily::DecalModulate,
        }
        .read_transfer();
        assert_eq!(transfer, TextureReadTransfer::Unmodified);
        for storage in [
            TextureStorage::Rgba8,
            TextureStorage::Bc(BcFormat::Bc1),
            TextureStorage::Bc(BcFormat::Bc2),
            TextureStorage::Bc(BcFormat::Bc3),
        ] {
            assert!(!format(storage).is_srgb());
            assert!(format(storage).add_srgb_suffix().is_srgb());
        }
    }

    #[test]
    fn image_view_sharing_requires_observed_adapter_support() {
        assert!(!ImageCache::new(None).compatible_views);
        let mut capabilities = wgpu::DownlevelCapabilities::default();
        capabilities
            .flags
            .remove(wgpu::DownlevelFlags::VIEW_FORMATS);
        assert!(!ImageCache::new(Some(&capabilities)).compatible_views);
        capabilities
            .flags
            .insert(wgpu::DownlevelFlags::VIEW_FORMATS);
        assert!(ImageCache::new(Some(&capabilities)).compatible_views);
    }
    #[test]
    fn view_ranges_are_validated_from_resident_metadata_without_gpu_work() {
        use crate::bridge::materials::{DecodedTextureFrames, ResolvedTextureMip};
        let frames = DecodedTextureFrames::from_rgba(
            0,
            (0..6)
                .map(|_| {
                    vec![ResolvedTextureMip {
                        rgba: [12, 24, 36, 255].repeat(16),
                        width: 4,
                        height: 4,
                    }]
                })
                .collect(),
        )
        .unwrap();
        let source = frames.resident().unwrap();
        assert!(validate_view(source, ImageView::Array).is_ok());
        assert!(validate_view(source, ImageView::D2(5)).is_ok());
        assert!(validate_view(source, ImageView::Cube(0)).is_ok());
        for view in [
            ImageView::D2(6),
            ImageView::D2(u32::MAX),
            ImageView::Cube(1),
            ImageView::Cube(u32::MAX),
        ] {
            assert!(matches!(
                validate_view(source, view),
                Err(UploadError::InvalidView)
            ));
        }
    }
    #[test]
    #[ignore = "requires a Vulkan GPU"]
    fn gpu_image_leases_share_views_across_scenes_and_release_unused_allocations() {
        use crate::bridge::materials::{DecodedTextureFrames, ResolvedCubemap, ResolvedTextureMip};
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let adapter = iced::futures::executor::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        )
        .expect("Vulkan adapter");
        let (device, queue) = iced::futures::executor::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor::default()),
        )
        .expect("Vulkan device");
        let _completion_scope =
            crate::features::file_preview::viewer3d::pipeline::completion::TestDeviceScope::new(
                &device,
            );
        assert_eq!(device.features(), wgpu::Features::empty());
        let capabilities = adapter.get_downlevel_capabilities();
        let frames = || {
            DecodedTextureFrames::from_rgba(
                0,
                [17, 83]
                    .map(|value| {
                        [4, 2, 1]
                            .map(|side| ResolvedTextureMip {
                                rgba: [value, 0, 0, 255].repeat((side * side) as usize),
                                width: side,
                                height: side,
                            })
                            .to_vec()
                    })
                    .to_vec(),
            )
            .unwrap()
        };
        let first_scene = frames();
        let second_scene = frames();
        assert_eq!(
            first_scene.resident().expect("resident fixture").id(),
            second_scene.resident().expect("resident fixture").id()
        );
        assert!(!Arc::ptr_eq(
            first_scene.resident().expect("resident fixture"),
            second_scene.resident().expect("resident fixture")
        ));
        let uploader = super::super::texture_upload::TextureUploader::shared(
            &device,
            &queue,
            Default::default(),
        )
        .unwrap();
        let mut cache = ImageCache::new(Some(&capabilities));
        let before = uploader.lock().unwrap().stats();
        let mut invalid = cache.lease(&uploader);
        assert!(matches!(
            invalid.view(
                &device,
                &queue,
                first_scene.resident().unwrap(),
                TextureReadTransfer::SrgbToLinear,
                ImageView::Cube(0)
            ),
            Err(UploadError::InvalidView)
        ));
        let (invalid, _) = invalid.finish();
        assert!(invalid.is_empty());
        assert_eq!(
            uploader.lock().unwrap().stats().owned_image_allocations,
            before.owned_image_allocations
        );
        assert_eq!(cache.stats().uploads, 0);
        let mut first = cache.lease(&uploader);
        first
            .view(
                &device,
                &queue,
                first_scene.resident().expect("resident fixture"),
                TextureReadTransfer::SrgbToLinear,
                ImageView::D2(0),
            )
            .unwrap();
        first
            .view(
                &device,
                &queue,
                first_scene.resident().expect("resident fixture"),
                TextureReadTransfer::Unmodified,
                ImageView::Array,
            )
            .unwrap();
        let (first, issues) = first.finish();
        assert!(issues.is_empty());
        let expected = if cache.compatible_views { 1 } else { 2 };
        assert_eq!(first.len(), expected);
        assert_eq!(lease_stats(&first).allocations, expected);
        assert_eq!(cache.stats().uploads, expected as u64);
        let mut second = cache.lease(&uploader);
        second
            .view(
                &device,
                &queue,
                second_scene.resident().expect("resident fixture"),
                TextureReadTransfer::SrgbToLinear,
                ImageView::D2(1),
            )
            .unwrap();
        let (second, issues) = second.finish();
        assert!(issues.is_empty());
        assert!(Arc::ptr_eq(&first[0], &second[0]));
        assert_eq!(cache.stats().uploads, expected as u64);
        drop(first);
        cache.trim();
        assert_eq!(cache.stats().allocations, 1);
        drop(second);
        cache.trim();
        assert!(cache.images.is_empty());
        let mut conservative = ImageCache::new(None);
        let mut lease = conservative.lease(&uploader);
        for transfer in [
            TextureReadTransfer::Unmodified,
            TextureReadTransfer::SrgbToLinear,
        ] {
            lease
                .view(
                    &device,
                    &queue,
                    first_scene.resident().expect("resident fixture"),
                    transfer,
                    ImageView::D2(0),
                )
                .unwrap();
        }
        let (lease, _) = lease.finish();
        assert_eq!(lease.len(), 2);
        assert!(!Arc::ptr_eq(&lease[0], &lease[1]));
        let cube = ResolvedCubemap::from_rgba(
            0,
            0,
            std::array::from_fn(|face| {
                vec![ResolvedTextureMip {
                    rgba: [face as u8, 0, 0, 255].repeat(16),
                    width: 4,
                    height: 4,
                }]
            }),
        )
        .unwrap();
        let mut cube_lease = cache.lease(&uploader);
        cube_lease
            .view(
                &device,
                &queue,
                cube.resident().expect("resident fixture"),
                TextureReadTransfer::SrgbToLinear,
                ImageView::Cube(0),
            )
            .unwrap();
        assert!(matches!(
            cube_lease.view(
                &device,
                &queue,
                cube.resident().expect("resident fixture"),
                TextureReadTransfer::SrgbToLinear,
                ImageView::Cube(1)
            ),
            Err(UploadError::InvalidView)
        ));
        let (cube_lease, _) = cube_lease.finish();
        assert_eq!(cube_lease.len(), 1);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    }
    fn wait_for_image_retirement(uploader: &super::super::texture_upload::SharedUploader) {
        use std::time::{Duration, Instant};
        let (completion, retirement) = {
            let uploader = uploader.lock().unwrap();
            let pair = (uploader.completion(), uploader.retirement_subscription());
            drop(uploader);
            pair
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            completion.check().expect("GPU completion worker");
            let stats = uploader.lock().unwrap().stats();
            let retirement = *retirement.receiver.borrow();
            assert_eq!(retirement.failure, None);
            if stats.sampled_image_bytes == 0
                && stats.sampled_image_allocations == 0
                && stats.owned_image_allocations == 0
                && stats.owned_image_texel_bytes == 0
                && stats.staging_bytes == 0
                && retirement.pending == 0
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "image retirement did not drain autonomously: {stats:?}; {retirement:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    #[ignore = "requires an exclusively reserved Vulkan GPU"]
    fn gpu_material_budget_shares_views_retires_and_flushes_cancelled_leases() {
        use super::super::texture_upload::{TextureUploader, UploadPolicy};
        use crate::bridge::materials::{DecodedTextureFrames, ResolvedTextureMip};
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let adapter = iced::futures::executor::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        )
        .unwrap();
        let (device, queue) = iced::futures::executor::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor::default()),
        )
        .unwrap();
        let _completion_scope =
            crate::features::file_preview::viewer3d::pipeline::completion::TestDeviceScope::new(
                &device,
            );
        let caps = adapter.get_downlevel_capabilities();
        assert!(caps.flags.contains(wgpu::DownlevelFlags::VIEW_FORMATS));
        let frames = |value| {
            DecodedTextureFrames::from_rgba(
                0,
                (0..2)
                    .map(|_| {
                        [4, 2, 1]
                            .map(|side| ResolvedTextureMip {
                                rgba: [value, 0, 0, 255].repeat((side * side) as usize),
                                width: side,
                                height: side,
                            })
                            .to_vec()
                    })
                    .collect(),
            )
            .unwrap()
        };
        let source = frames(21);
        let equivalent = frames(21);
        let distinct = frames(22);
        let uploader = TextureUploader::shared(
            &device,
            &queue,
            UploadPolicy {
                aggregate_staging_bytes: 512,
                batch_bytes: 256,
                sampled_image_bytes: 168,
                sampled_image_allocations: 1,
            },
        )
        .unwrap();
        let mut cache = ImageCache::new(Some(&caps));
        let mut first = cache.lease(&uploader);
        let raw = first
            .view(
                &device,
                &queue,
                source.resident().unwrap(),
                TextureReadTransfer::Unmodified,
                ImageView::Array,
            )
            .unwrap();
        let srgb = first
            .view(
                &device,
                &queue,
                source.resident().unwrap(),
                TextureReadTransfer::SrgbToLinear,
                ImageView::D2(0),
            )
            .unwrap();
        let (first, _) = first.finish();
        let mut second = cache.lease(&uploader);
        let shared = second
            .view(
                &device,
                &queue,
                equivalent.resident().unwrap(),
                TextureReadTransfer::Unmodified,
                ImageView::D2(1),
            )
            .unwrap();
        let (second, _) = second.finish();
        assert!(Arc::ptr_eq(&first[0], &second[0]));
        assert_eq!((cache.stats().uploads, cache.stats().cache_hits), (1, 2));
        let copied = uploader.lock().unwrap().stats().copied_bytes;
        let mut rejected = cache.lease(&uploader);
        assert!(matches!(
            rejected.view(
                &device,
                &queue,
                distinct.resident().unwrap(),
                TextureReadTransfer::Unmodified,
                ImageView::Array
            ),
            Err(UploadError::Upload(
                super::super::texture_upload::UploadError::ImageBudget { .. }
            ))
        ));
        drop(rejected);
        assert_eq!(uploader.lock().unwrap().stats().copied_bytes, copied);
        drop(first);
        drop(second);
        assert_eq!(uploader.lock().unwrap().stats().sampled_image_bytes, 168);
        drop(raw);
        drop(srgb);
        drop(shared);
        wait_for_image_retirement(&uploader);
        assert_eq!(uploader.lock().unwrap().stats().sampled_image_bytes, 0);
        cache.trim();
        let mut reload = cache.lease(&uploader);
        let reloaded = reload
            .view(
                &device,
                &queue,
                source.resident().unwrap(),
                TextureReadTransfer::Unmodified,
                ImageView::D2(0),
            )
            .unwrap();
        let (reload, _) = reload.finish();
        drop(reload);
        drop(reloaded);
        wait_for_image_retirement(&uploader);
        assert_eq!(cache.stats().uploads, 2);
        let submissions = uploader.lock().unwrap().stats().submissions;
        let mut cancelled = cache.lease(&uploader);
        cancelled
            .view(
                &device,
                &queue,
                distinct.resident().unwrap(),
                TextureReadTransfer::Unmodified,
                ImageView::Array,
            )
            .unwrap();
        drop(cancelled);
        assert!(uploader.lock().unwrap().stats().submissions > submissions);
        wait_for_image_retirement(&uploader);
        assert_eq!(uploader.lock().unwrap().stats().sampled_image_bytes, 0);
        let mut incompatible = ImageCache::new(None);
        let mut lease = incompatible.lease(&uploader);
        lease
            .view(
                &device,
                &queue,
                source.resident().unwrap(),
                TextureReadTransfer::Unmodified,
                ImageView::Array,
            )
            .unwrap();
        assert!(matches!(
            lease.view(
                &device,
                &queue,
                source.resident().unwrap(),
                TextureReadTransfer::SrgbToLinear,
                ImageView::Array
            ),
            Err(UploadError::Upload(
                super::super::texture_upload::UploadError::ImageBudget { .. }
            ))
        ));
        let (images, _) = lease.finish();
        assert_eq!(images.len(), 1);
        assert_eq!(uploader.lock().unwrap().stats().sampled_image_bytes, 168);
        drop(images);
        wait_for_image_retirement(&uploader);
        assert_eq!(
            uploader.lock().unwrap().stats().sampled_image_allocations,
            0
        );
    }
}

#[cfg(test)]
mod classification_tests {
    use super::*;
    #[test]
    fn capacity_pressure_remains_distinct_from_invalid_resources() {
        let capacity = ImageUploadIssue::from_upload(
            "wall",
            "base",
            &UploadError::Upload(super::super::texture_upload::UploadError::ImageBudget {
                bytes: 64,
                allocations: 1,
            }),
        );
        let device = ImageUploadIssue::from_upload("wall", "base", &UploadError::DeviceLimit);
        let view = ImageUploadIssue::from_upload("wall", "base", &UploadError::InvalidView);
        assert_eq!(capacity.cause, ImageUploadCause::Capacity);
        for intrinsic in [device, view] {
            assert_eq!(intrinsic.cause, ImageUploadCause::Resource);
        }
    }
}
