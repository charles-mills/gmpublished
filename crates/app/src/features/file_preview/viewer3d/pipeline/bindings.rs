pub(super) mod warm;

use super::frame_selection::MaterialFrames;
#[cfg(feature = "map-capture")]
use super::images::ImageResidency;
#[cfg(any(test, feature = "map-capture"))]
use super::images::ImageStats;
use super::images::{Image, ImageCache, ImageUploadIssue, ImageView};
use super::materials::{
    BaseTextureFrame, MaterialTextureViews, MaterialUploadMode, PreparedMaterial, SampledTexture,
};
use super::residency_hash::{Map as ResidencyMap, Set as ResidencySet};
use super::resources::RenderResources;
use super::{MaterialSlot, PreviewScene, SceneKey, wgpu};
use crate::bridge::materials::{
    DecodedTextureFrames, EnvironmentMapSource, PageStatus, PageTicket, ReloadableTextureSource,
    TextureFrameRequest, TexturePageCache, TextureReadTransfer, TextureRole, TextureSourceId,
};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct MaterialKey {
    pub scene: SceneKey,
    pub slot: usize,
    pub frames: MaterialFrames,
}

#[derive(Debug)]
pub(super) struct MaterialBinding {
    pub main: wgpu::BindGroup,
    pub water: wgpu::BindGroup,
    _uniform: wgpu::Buffer,
    _images: Vec<Arc<Image>>,
    pub issues: Vec<ImageUploadIssue>,
    #[cfg(feature = "map-capture")]
    pub samplers: Vec<super::samplers::SamplerDiagnostic>,
}

#[derive(Debug, Default)]
pub(super) struct MaterialBindings(HashMap<usize, Arc<MaterialBinding>>);
impl MaterialBindings {
    pub fn get(&self, slot: usize) -> Option<&wgpu::BindGroup> {
        self.0.get(&slot).map(|binding| &binding.main)
    }
    pub fn water(&self, slot: usize) -> &wgpu::BindGroup {
        &self.0[&slot].water
    }
}
impl std::ops::Index<usize> for MaterialBindings {
    type Output = wgpu::BindGroup;
    fn index(&self, slot: usize) -> &Self::Output {
        self.get(slot).expect("prepared draw material is resident")
    }
}

#[derive(Debug, Default)]
pub(super) struct BackgroundBindings {
    pub skybox: Option<Arc<super::upload::UploadedSkybox>>,
    pub screen_refraction_normals: HashMap<String, Arc<super::screen_refraction::Texture>>,
    _images: Vec<Arc<Image>>,
    #[cfg(feature = "map-capture")]
    samplers: Vec<super::samplers::SamplerDiagnostic>,
    issues: Vec<ImageUploadIssue>,
}
#[derive(Debug, Default)]
pub(super) struct BindingSnapshot {
    pub materials: MaterialBindings,
    pub background: Arc<BackgroundBindings>,
}
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct BackgroundKey {
    scene: SceneKey,
    sky: bool,
    immersion: Option<String>,
}
impl FrameRequest {
    fn background_key(&self) -> BackgroundKey {
        BackgroundKey {
            scene: self.source.key(),
            sky: self.sky,
            immersion: self.immersion.clone(),
        }
    }
}
impl BindingSnapshot {
    pub fn covers(&self, request: &FrameRequest) -> bool {
        request
            .materials
            .iter()
            .all(|key| self.materials.0.contains_key(&key.slot))
            && (!request.sky
                || request
                    .source
                    .map()
                    .and_then(|map| map.skybox.as_ref())
                    .is_none()
                || self.background.skybox.is_some())
            && request.immersion.as_ref().is_none_or(|effect| {
                self.background
                    .screen_refraction_normals
                    .contains_key(effect)
            })
    }
    #[cfg(feature = "map-capture")]
    pub(super) fn image_stats(&self) -> ImageResidency {
        let images: Vec<_> = self
            .background
            ._images
            .iter()
            .cloned()
            .chain(
                self.materials
                    .0
                    .values()
                    .flat_map(|binding| binding._images.iter().cloned()),
            )
            .collect();
        super::images::lease_stats(&images)
    }
    pub(super) fn issues(&self) -> impl Iterator<Item = &ImageUploadIssue> {
        self.background.issues.iter().chain(
            self.materials
                .0
                .values()
                .flat_map(|binding| binding.issues.iter()),
        )
    }
    #[cfg(feature = "map-capture")]
    pub(super) fn samplers(&self) -> impl Iterator<Item = &super::samplers::SamplerDiagnostic> {
        self.background.samplers.iter().chain(
            self.materials
                .0
                .values()
                .flat_map(|binding| binding.samplers.iter()),
        )
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct PageKey {
    pub request: TextureFrameRequest,
    pub transfer: TextureReadTransfer,
}
pub(super) type PageNeed = (PageKey, Arc<ReloadableTextureSource>);
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct AllocationKey {
    resident: crate::bridge::materials::TextureResidentId,
    transfer: Option<TextureReadTransfer>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum SampledAllocation {
    Texture(AllocationKey),
    Lightmap(super::lightmaps::Key),
    Pending {
        identity: crate::bridge::materials::TexturePageIdentity,
        transfer: Option<TextureReadTransfer>,
    },
}

pub(super) type WorkingSet = super::admission::Cost<SampledAllocation>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PageDevicePolicy {
    supports_bc: bool,
    max_dimension: u32,
    max_array_layers: u32,
}
impl PageDevicePolicy {
    fn new(device: &wgpu::Device) -> Self {
        let limits = device.limits();
        Self {
            supports_bc: super::materials::bc_upload_enabled(device),
            max_dimension: limits.max_texture_dimension_2d,
            max_array_layers: limits.max_texture_array_layers,
        }
    }
}

#[derive(Clone, Debug)]
struct ValidatedPage {
    normalized: TextureFrameRequest,
    bytes: u64,
}

#[derive(Debug, Default)]
struct PageValidation {
    cached: Option<(PageDevicePolicy, Result<ValidatedPage, String>)>,
}
impl PageValidation {
    fn get_with(
        &mut self,
        policy: PageDevicePolicy,
        enabled: bool,
        validate: impl FnOnce() -> Result<ValidatedPage, String>,
    ) -> Result<Cow<'_, ValidatedPage>, String> {
        if enabled {
            self.get_or_init(policy, validate).map(Cow::Borrowed)
        } else {
            validate().map(Cow::Owned)
        }
    }

    fn get_or_init(
        &mut self,
        policy: PageDevicePolicy,
        validate: impl FnOnce() -> Result<ValidatedPage, String>,
    ) -> Result<&ValidatedPage, String> {
        if self
            .cached
            .as_ref()
            .is_none_or(|(cached, _)| *cached != policy)
        {
            self.cached = Some((policy, validate()));
        }
        self.cached
            .as_ref()
            .expect("page validation initialized")
            .1
            .as_ref()
            .map_err(Clone::clone)
    }
}

fn page_validation_cache_enabled() -> bool {
    #[cfg(feature = "map-capture")]
    {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| {
            std::env::var_os("GMPUBLISHED_PAGE_VALIDATION_CACHE").as_deref()
                != Some(std::ffi::OsStr::new("0"))
        })
    }
    #[cfg(not(feature = "map-capture"))]
    true
}

fn validate_page(
    device: &wgpu::Device,
    key: &PageKey,
    source: &ReloadableTextureSource,
    include_allocation_cost: bool,
) -> Result<ValidatedPage, String> {
    let normalized = ImageCache::page_request_for_device(device, &key.request, source.metadata());
    source
        .validate_page(&normalized)
        .map_err(|error| error.to_string())?;
    let bytes = if include_allocation_cost {
        ImageCache::page_allocation_cost(device, &key.request, source.metadata())
            .map_err(|error| error.to_string())?
    } else {
        0
    };
    Ok(ValidatedPage { normalized, bytes })
}

#[derive(Debug)]
struct PendingPage {
    // The owning map keys validation by full request policy, not compatible image identity.
    validation: PageValidation,
    source: Arc<ReloadableTextureSource>,
    ticket: Option<PageTicket>,
    image: Option<Arc<Image>>,
    allocation: Option<(AllocationKey, u64)>,
    error: Option<String>,
}

#[derive(Debug)]
enum WeakScene {
    Model(Weak<super::ModelPreview>),
    Map(Weak<super::MapPreview>),
}
impl WeakScene {
    fn new(scene: &PreviewScene) -> Self {
        match scene {
            PreviewScene::Model(model) => Self::Model(Arc::downgrade(model)),
            PreviewScene::Map(map) => Self::Map(Arc::downgrade(map)),
        }
    }
    fn alive(&self) -> bool {
        match self {
            Self::Model(model) => model.strong_count() != 0,
            Self::Map(map) => map.strong_count() != 0,
        }
    }
}
#[derive(Debug)]
struct SceneSources {
    owner: WeakScene,
    sources: HashSet<TextureSourceId>,
}

#[derive(Debug)]
pub(super) struct BindingCache {
    images: ImageCache,
    service: Result<Arc<TexturePageCache>, crate::bridge::materials::PageCacheInitError>,
    pages: ResidencyMap<PageKey, PendingPage>,
    materials: ResidencyMap<MaterialKey, Arc<MaterialBinding>>,
    capacity_blocked: bool,
    external_active: super::lightmaps::MissingCost,
    lightmap_warm: Option<Arc<Mutex<super::lightmaps::WarmPages>>>,
    retiring_lightmaps: Vec<Weak<super::lightmaps::Page>>,
    backgrounds: HashMap<BackgroundKey, Arc<BackgroundBindings>>,
    warm: WarmPages,
    source_scenes: HashMap<SceneKey, SceneSources>,
    open_sources: HashSet<TextureSourceId>,
    retiring_warm: Vec<Weak<Image>>,
    staged_keys: ResidencySet<PageKey>,
    active_keys: ResidencySet<PageKey>,
    staging_sources: bool,
}
type WarmPages = warm::WarmImages<AllocationKey, PageKey, ReloadableTextureSource, Image>;

fn allocation_key(
    images: &ImageCache,
    image: &Image,
    transfer: TextureReadTransfer,
) -> AllocationKey {
    AllocationKey {
        resident: image.resident_id().clone(),
        transfer: (!images.compatible_transfer_views()).then_some(transfer),
    }
}
fn forget_aliases(images: &mut ImageCache, aliases: Vec<PageKey>) {
    for alias in aliases {
        images.forget_page(&alias.request, alias.transfer);
    }
}

struct EvictionPools<'a> {
    warm: &'a mut WarmPages,
    images: &'a mut ImageCache,
    retiring: &'a mut Vec<Weak<Image>>,
    lightmaps: Option<&'a Arc<Mutex<super::lightmaps::WarmPages>>>,
    retiring_lightmaps: &'a mut Vec<Weak<super::lightmaps::Page>>,
}

fn reserve_room_for(
    caches: EvictionPools<'_>,
    uploader: &super::texture_upload::SharedUploader,
    bytes: u64,
    allocations: u64,
) -> Result<bool, String> {
    let EvictionPools {
        warm,
        images,
        retiring,
        lightmaps,
        retiring_lightmaps,
    } = caches;
    let (mut used_bytes, mut used_allocations, capacity_bytes, capacity_allocations, pending) = {
        let uploader = uploader.lock().expect("texture uploader lock");
        uploader
            .poll_retirements()
            .map_err(|error| error.to_string())?;
        let (used_bytes, used_allocations) = uploader.sampled_usage();
        let (capacity_bytes, capacity_allocations) = uploader.sampled_capacity();
        let pending = uploader.retirement_subscription().receiver.borrow().pending;
        drop(uploader);
        (
            used_bytes,
            used_allocations,
            capacity_bytes,
            capacity_allocations,
            pending,
        )
    };
    if bytes > capacity_bytes || allocations > capacity_allocations {
        return Ok(false);
    }
    retiring.retain(|image| image.strong_count() != 0 || pending != 0);
    retiring_lightmaps.retain(|page| page.strong_count() != 0 || pending != 0);
    let fits = |used_bytes: u64, used_allocations: u64| {
        used_bytes
            .checked_add(bytes)
            .is_some_and(|total| total <= capacity_bytes)
            && used_allocations
                .checked_add(allocations)
                .is_some_and(|total| total <= capacity_allocations)
    };
    if fits(used_bytes, used_allocations) {
        return Ok(true);
    }
    if pending != 0 || !retiring.is_empty() || !retiring_lightmaps.is_empty() {
        return Ok(false);
    }
    while !fits(used_bytes, used_allocations) {
        let lightmap_touch =
            lightmaps.and_then(|pages| pages.lock().expect("lightmap cache lock").oldest_touch());
        match warm::oldest_pool(warm.oldest_touch(), lightmap_touch) {
            Some(warm::InactiveImageKind::Material) => {
                let evicted = warm.evict_oldest().expect("oldest material image");
                retiring.push(evicted.image);
                used_bytes = used_bytes.saturating_sub(evicted.bytes);
                used_allocations = used_allocations.saturating_sub(1);
                forget_aliases(images, evicted.aliases);
            }
            Some(warm::InactiveImageKind::Lightmap) => {
                let (bytes, page) = lightmaps
                    .expect("oldest lightmap pool")
                    .lock()
                    .expect("lightmap cache lock")
                    .evict_oldest()
                    .expect("oldest lightmap image");
                retiring_lightmaps.push(page);
                used_bytes = used_bytes.saturating_sub(bytes);
                used_allocations = used_allocations.saturating_sub(1);
            }
            None => break,
        }
    }
    let uploader = uploader.lock().expect("texture uploader lock");
    uploader
        .poll_retirements()
        .map_err(|error| error.to_string())?;
    let (used_bytes, used_allocations) = uploader.sampled_usage();
    drop(uploader);
    Ok(fits(used_bytes, used_allocations))
}

impl BindingCache {
    pub fn new(capabilities: Option<&wgpu::DownlevelCapabilities>) -> Self {
        let service = TexturePageCache::shared();
        let warm_limit = service
            .as_ref()
            .map_or(0, |service| service.policy().max_entries);
        Self {
            images: ImageCache::new(capabilities),
            service,
            pages: ResidencyMap::default(),
            materials: ResidencyMap::default(),
            capacity_blocked: false,
            external_active: super::lightmaps::MissingCost::default(),
            lightmap_warm: None,
            retiring_lightmaps: Vec::new(),
            backgrounds: HashMap::new(),
            warm: WarmPages::new(warm_limit),
            source_scenes: HashMap::new(),
            open_sources: HashSet::new(),
            retiring_warm: Vec::new(),
            staged_keys: ResidencySet::default(),
            active_keys: ResidencySet::default(),
            staging_sources: false,
        }
    }
    pub fn reserve_lightmaps(
        &mut self,
        uploader: &super::texture_upload::SharedUploader,
        lightmaps: &super::lightmaps::Pool,
        missing: super::lightmaps::MissingCost,
        active: super::lightmaps::MissingCost,
    ) -> Result<bool, String> {
        self.external_active = active;
        self.lightmap_warm = Some(lightmaps.warm_cache());
        let (capacity_bytes, capacity_allocations) = uploader
            .lock()
            .expect("texture uploader lock")
            .sampled_capacity();
        if active.bytes > capacity_bytes || active.allocations > capacity_allocations {
            self.capacity_blocked = true;
            return Err(format!(
                "visible lightmap pages require {} bytes and {} allocations; shared sampled-image capacity is {capacity_bytes} bytes and {capacity_allocations} allocations",
                active.bytes, active.allocations
            ));
        }
        let ready = reserve_room_for(
            EvictionPools {
                warm: &mut self.warm,
                images: &mut self.images,
                retiring: &mut self.retiring_warm,
                lightmaps: self.lightmap_warm.as_ref(),
                retiring_lightmaps: &mut self.retiring_lightmaps,
            },
            uploader,
            missing.bytes,
            missing.allocations,
        )?;
        self.capacity_blocked |= !ready;
        Ok(ready)
    }

    pub fn pressure(&self, resources: &RenderResources) -> Option<PoolPressure> {
        self.pressure_for(&resources.texture_uploader)
    }
    fn pressure_for(
        &self,
        uploader: &super::texture_upload::SharedUploader,
    ) -> Option<PoolPressure> {
        if !self.capacity_blocked
            || (self.active_keys.is_empty() && self.external_active.allocations == 0)
        {
            return None;
        }
        let allocations = self
            .pages
            .iter()
            .filter(|(key, _)| self.active_keys.contains(*key))
            .map(|(key, page)| {
                page.allocation.clone().or_else(|| {
                    page.image.as_ref().map(|image| {
                        (
                            AllocationKey {
                                resident: image.resident_id().clone(),
                                transfer: (!self.images.compatible_transfer_views())
                                    .then_some(key.transfer),
                            },
                            image.byte_len(),
                        )
                    })
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let distinct: HashMap<_, _> = allocations.into_iter().collect();
        let required_bytes = distinct
            .values()
            .try_fold(self.external_active.bytes, |total, bytes| {
                total.checked_add(*bytes)
            })?;
        let required_allocations =
            (distinct.len() as u64).checked_add(self.external_active.allocations)?;
        let (capacity_bytes, capacity_allocations) = uploader
            .lock()
            .expect("texture uploader lock")
            .sampled_capacity();
        (required_bytes > capacity_bytes || required_allocations > capacity_allocations).then_some(
            PoolPressure {
                required_bytes,
                required_allocations,
                capacity_bytes,
                capacity_allocations,
            },
        )
    }
    pub fn capacity_blocked(&self) -> bool {
        self.capacity_blocked
    }
    #[cfg(feature = "map-capture")]
    pub fn page_diagnostics(&self) -> serde_json::Value {
        match &self.service {
            Ok(service) => {
                serde_json::json!({"policy":service.policy(),"stats":service.stats(),"warm_gpu":self.warm.stats(),"scope":"shared process page payload; reserved output includes pending jobs"})
            }
            Err(error) => serde_json::json!({"error":error.to_string()}),
        }
    }
    #[cfg(any(test, feature = "map-capture"))]
    pub fn stats(&self) -> ImageStats {
        self.images.stats()
    }
    pub fn subscription(&self) -> Option<crate::bridge::materials::PageSubscription> {
        self.service
            .as_ref()
            .ok()
            .map(|service| service.subscribe())
    }
    pub fn retain(&mut self, keys: &HashSet<MaterialKey>) {
        self.materials.retain(|key, _| keys.contains(key));
    }
    pub fn trim(&mut self) {
        forget_aliases(
            &mut self.images,
            self.warm
                .retain_sources(|source| self.open_sources.contains(&source.source_id())),
        );
        self.images.trim();
        if let Ok(service) = &self.service {
            service.trim_orphaned();
        }
    }

    pub fn material(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        key: MaterialKey,
        slot: &MaterialSlot,
    ) -> Result<Option<Arc<MaterialBinding>>, String> {
        self.material_with_batch(device, queue, resources, key, slot, None)
    }

    fn material_with_batch(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        key: MaterialKey,
        slot: &MaterialSlot,
        batch: Option<&super::texture_upload::UploadLeaseBatch>,
    ) -> Result<Option<Arc<MaterialBinding>>, String> {
        if let Some(binding) = self.materials.get(&key) {
            return Ok(Some(Arc::clone(binding)));
        }
        super::texture_barriers::validate_material(slot).map_err(|error| error.to_string())?;
        let needs = material_pages(slot, &key.frames);
        let ready = if batch.is_some() {
            self.prepare_pages_with_batch(
                device,
                queue,
                &resources.texture_uploader,
                &needs,
                batch,
            )?
        } else {
            self.prepare_pages(device, queue, &resources.texture_uploader, &needs)?
        };
        if !ready {
            return Ok(None);
        }
        let frames = &key.frames;
        let mut images = match batch {
            Some(batch) => self.images.lease_batched(batch),
            None => self.images.lease(&resources.texture_uploader),
        };
        let flat_normal = &resources.flat_normal;
        let flat_sample = SampledTexture {
            view: flat_normal,
            flags: None,
        };
        let mut sampler_diagnostics = Vec::new();
        let (main, water, uniform) = {
            let base_view = resources.create_slot_texture(
                device,
                queue,
                slot,
                slot.texture.as_deref(),
                &mut images,
                BaseTextureFrame::Base(frames.base),
            );
            let base2_view = slot.texture2.as_deref().map(|texture| {
                resources.create_slot_texture(
                    device,
                    queue,
                    slot,
                    Some(texture),
                    &mut images,
                    BaseTextureFrame::Base2(frames.base2),
                )
            });
            let prepared = PreparedMaterial::from(&slot.shader);
            let normal_source = prepared.normal();
            let normal2_source = prepared.normal2();
            let normal = normal_source.and_then(|normal| {
                RenderResources::create_data_frame_view(
                    device,
                    queue,
                    normal,
                    frames.normal,
                    TextureRole::Normal,
                    &mut images,
                    &slot.name,
                )
            });
            let normal2 = normal2_source.and_then(|normal| {
                RenderResources::create_data_frame_view(
                    device,
                    queue,
                    normal,
                    frames.normal2,
                    TextureRole::Normal,
                    &mut images,
                    &slot.name,
                )
            });
            resources.create_material(
                device,
                queue,
                MaterialTextureViews {
                    base: base_view.sampled(),
                    base2: base2_view.as_ref().unwrap_or(&base_view).sampled(),
                    normal: normal.as_ref().map_or(flat_sample, |view| SampledTexture {
                        view,
                        flags: normal_source
                            .map(crate::bridge::materials::DecodedTextureFrames::flags),
                    }),
                    normal2: normal2
                        .as_ref()
                        .map(|view| SampledTexture {
                            view,
                            flags: normal2_source
                                .map(crate::bridge::materials::DecodedTextureFrames::flags),
                        })
                        .or_else(|| {
                            normal.as_ref().map(|view| SampledTexture {
                                view,
                                flags: normal_source
                                    .map(crate::bridge::materials::DecodedTextureFrames::flags),
                            })
                        })
                        .unwrap_or(flat_sample),
                },
                MaterialUploadMode {
                    frames: Some(frames),
                    name: &slot.name,
                    material: PreparedMaterial::from(&slot.shader),
                    base_flags: slot.texture.as_ref().and_then(|texture| texture.flags()),
                    base2_flags: slot
                        .texture2
                        .as_ref()
                        .or(slot.texture.as_ref())
                        .and_then(|texture| texture.flags()),
                    render_mode: slot.render_mode,
                    alpha_policy: slot.alpha_policy,
                },
                &mut images,
                &mut sampler_diagnostics,
            )
        };
        let (images, issues) = images.finish();
        if issues
            .iter()
            .any(|issue| issue.cause == super::images::ImageUploadCause::Capacity)
        {
            self.capacity_blocked = true;
            return Ok(None);
        }
        let binding = Arc::new(MaterialBinding {
            main,
            water,
            _uniform: uniform,
            _images: images,
            issues,
            #[cfg(feature = "map-capture")]
            samplers: sampler_diagnostics,
        });
        self.materials.insert(key, Arc::clone(&binding));
        Ok(Some(binding))
    }

    fn prepare_pages(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        uploader: &super::texture_upload::SharedUploader,
        needs: &[(PageKey, Arc<ReloadableTextureSource>)],
    ) -> Result<bool, String> {
        self.prepare_pages_with_batch(device, queue, uploader, needs, None)
    }

    fn prepare_pages_with_batch(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        uploader: &super::texture_upload::SharedUploader,
        needs: &[(PageKey, Arc<ReloadableTextureSource>)],
        batch: Option<&super::texture_upload::UploadLeaseBatch>,
    ) -> Result<bool, String> {
        if needs.is_empty() {
            return Ok(true);
        }
        let service = self.service.as_ref().map_err(ToString::to_string)?;
        let cache_enabled = page_validation_cache_enabled();
        let policy = PageDevicePolicy::new(device);
        let mut ready = true;
        for (key, source) in needs {
            let state = self
                .pages
                .entry(key.clone())
                .or_insert_with(|| PendingPage {
                    validation: PageValidation::default(),
                    source: Arc::clone(source),
                    ticket: None,
                    image: None,
                    allocation: None,
                    error: None,
                });
            let validation = state.validation.get_with(policy, cache_enabled, || {
                validate_page(device, key, &state.source, cache_enabled)
            })?;
            let bytes = validation.bytes;
            if let Some(image) = &state.image {
                let allocation = allocation_key(&self.images, image, key.transfer);
                self.retiring_warm
                    .retain(|retiring| !retiring.ptr_eq(&Arc::downgrade(image)));
                forget_aliases(&mut self.images, self.warm.activate(&allocation));
                self.images.alias_page(&key.request, key.transfer, image);
                state.allocation = Some((allocation, image.byte_len()));
                continue;
            }
            if let Some(image) = self.images.lookup_page(&key.request, key.transfer) {
                let allocation = allocation_key(&self.images, &image, key.transfer);
                self.retiring_warm
                    .retain(|retiring| !retiring.ptr_eq(&Arc::downgrade(&image)));
                forget_aliases(&mut self.images, self.warm.activate(&allocation));
                self.images.alias_page(&key.request, key.transfer, &image);
                state.allocation = Some((allocation, image.byte_len()));
                state.image = Some(image);
                state.ticket = None;
                continue;
            }
            let ticket = state.ticket.get_or_insert_with(|| {
                service.request(Arc::clone(&state.source), validation.normalized.clone())
            });
            match ticket.status() {
                PageStatus::Ready(page) => {
                    let bytes = if cache_enabled {
                        bytes
                    } else {
                        ImageCache::page_allocation_cost(
                            device,
                            &key.request,
                            state.source.metadata(),
                        )
                        .map_err(|error| error.to_string())?
                    };
                    let allocation = AllocationKey {
                        resident: ImageCache::allocation_id(device, page.resident()),
                        transfer: (!self.images.compatible_transfer_views())
                            .then_some(key.transfer),
                    };
                    state.allocation = Some((allocation.clone(), bytes));
                    if let Some(image) = self
                        .images
                        .lookup_allocation(&allocation.resident, key.transfer)
                    {
                        self.retiring_warm
                            .retain(|retiring| !retiring.ptr_eq(&Arc::downgrade(&image)));
                        forget_aliases(&mut self.images, self.warm.activate(&allocation));
                        self.images.alias_page(&key.request, key.transfer, &image);
                        state.image = Some(image);
                        state.ticket = None;
                        continue;
                    }
                    if !reserve_room_for(
                        EvictionPools {
                            warm: &mut self.warm,
                            images: &mut self.images,
                            retiring: &mut self.retiring_warm,
                            lightmaps: self.lightmap_warm.as_ref(),
                            retiring_lightmaps: &mut self.retiring_lightmaps,
                        },
                        uploader,
                        bytes,
                        1,
                    )? {
                        self.capacity_blocked = true;
                        state.ticket = None;
                        ready = false;
                        continue;
                    }
                    let mut lease = match batch {
                        Some(batch) => self.images.lease_batched(batch),
                        None => self.images.lease(uploader),
                    };
                    let dimension = if key.request.faces.len() == 6 {
                        ImageView::Cube(0)
                    } else {
                        ImageView::Array
                    };
                    match lease.page_view(device, queue, &page, key.transfer, dimension) {
                        Ok(_) => {
                            let (images, _) = lease.finish();
                            state.image = images.into_iter().next();
                            if let Some(image) = &state.image {
                                self.images.alias_page(&key.request, key.transfer, image);
                            }
                            state.ticket = None;
                        }
                        Err(super::images::UploadError::Upload(
                            super::texture_upload::UploadError::ImageBudget { .. },
                        )) => {
                            self.capacity_blocked = true;
                            state.ticket = None;
                            ready = false;
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                }
                PageStatus::Failed(error) => return Err(error.to_string()),
                PageStatus::PendingRetry(reason) => {
                    match reason {
                        crate::bridge::materials::PageRetry::ResidentBytes {
                            required,
                            capacity,
                        }
                        | crate::bridge::materials::PageRetry::WorkingBytes {
                            required,
                            capacity,
                        } if required > capacity => {
                            return Err(format!(
                                "authored texture page requires {required} bytes; page limit is {capacity}"
                            ));
                        }
                        crate::bridge::materials::PageRetry::Closed => {
                            return Err("texture page service is closed".to_owned());
                        }
                        _ => {}
                    }
                    state.ticket = None;
                    ready = false;
                }
                PageStatus::Pending => ready = false,
            }
        }
        Ok(ready)
    }
}

pub(super) fn push_page(
    output: &mut Vec<(PageKey, Arc<ReloadableTextureSource>)>,
    source: Option<(&Arc<ReloadableTextureSource>, &TextureFrameRequest)>,
    frame: u32,
    transfer: TextureReadTransfer,
) {
    if let Some((source, recipe)) = source {
        let mut request = recipe.clone();
        request.frame = frame;
        output.push((PageKey { request, transfer }, Arc::clone(source)));
    }
}
struct TextureNeed<'a> {
    resident: Option<&'a crate::bridge::materials::ResidentTexture>,
    source: Option<(&'a Arc<ReloadableTextureSource>, &'a TextureFrameRequest)>,
    frame: u32,
    transfer: TextureReadTransfer,
}

fn visit_material_textures<'a, E>(
    slot: &'a MaterialSlot,
    frames: &MaterialFrames,
    mut visit: impl FnMut(TextureNeed<'a>) -> Result<(), E>,
) -> Result<(), E> {
    if !slot.render_mode.draws() {
        return Ok(());
    }
    let mut add = |resident, source, frame, transfer| {
        visit(TextureNeed {
            resident,
            source,
            frame,
            transfer,
        })
    };
    let surface = slot.shader.surface();
    let transfer = surface.map_or(
        TextureReadTransfer::SrgbToLinear,
        crate::bridge::materials::SurfaceMaterial::base_read_transfer,
    );
    add(
        slot.texture
            .as_ref()
            .filter(|texture| !texture.load_outcome().is_unavailable())
            .and_then(|texture| texture.resident())
            .map(AsRef::as_ref),
        slot.texture
            .as_ref()
            .and_then(|texture| texture.on_demand()),
        frames.base,
        transfer,
    )?;
    add(
        slot.texture2
            .as_ref()
            .filter(|texture| !texture.load_outcome().is_unavailable())
            .and_then(|texture| texture.resident())
            .map(AsRef::as_ref),
        slot.texture2
            .as_ref()
            .and_then(|texture| texture.on_demand()),
        frames.base2,
        transfer,
    )?;
    let prepared = PreparedMaterial::from(&slot.shader);
    let mut data =
        |source: Option<&'a DecodedTextureFrames>, frame, role: TextureRole| -> Result<(), E> {
            add(
                source
                    .filter(|texture| !texture.load_outcome().is_unavailable())
                    .and_then(DecodedTextureFrames::resident)
                    .map(AsRef::as_ref),
                source.and_then(DecodedTextureFrames::on_demand),
                frame,
                role.read_transfer(),
            )?;
            Ok(())
        };
    data(prepared.normal(), frames.normal, TextureRole::Normal)?;
    data(prepared.normal2(), frames.normal2, TextureRole::Normal)?;
    if let Some(surface) = surface {
        let detail = surface.detail.as_ref();
        data(
            detail.and_then(|detail| detail.texture.as_deref()),
            frames.detail,
            TextureRole::Detail {
                family: surface.family,
                mode: surface.effective_detail_mode(),
            },
        )?;
        data(
            surface.blend_modulation.as_deref(),
            0,
            TextureRole::BlendModulation,
        )?;
        data(
            surface
                .emission
                .as_ref()
                .and_then(|emission| emission.mask.as_deref()),
            0,
            TextureRole::EmissionMask,
        )?;
        if let Some(environment) = surface.envmap.as_ref() {
            data(
                environment.mask_texture.as_deref(),
                frames.environment_mask,
                TextureRole::EnvironmentMask,
            )?;
        }
        if let Some(phong) = surface.phong.as_ref() {
            data(
                phong
                    .exponent_texture
                    .as_ref()
                    .and_then(|texture| texture.texture.as_deref()),
                0,
                TextureRole::PhongExponent,
            )?;
            data(
                phong
                    .diffuse_warp
                    .as_ref()
                    .and_then(|texture| texture.texture.as_deref()),
                0,
                TextureRole::PhongWarp,
            )?;
            data(
                phong
                    .specular_warp
                    .as_ref()
                    .and_then(|texture| texture.texture.as_deref()),
                0,
                TextureRole::PhongWarp,
            )?;
        }
        if let Some(environment) = surface.envmap.as_ref()
            && let EnvironmentMapSource::Named(cube) = &environment.source
        {
            add(
                cube.resident()
                    .filter(|_| !cube.load_outcome().is_unavailable())
                    .map(AsRef::as_ref),
                cube.on_demand(),
                frames.environment,
                TextureReadTransfer::SrgbToLinear,
            )?;
        }
    }
    Ok(())
}

fn material_need_visitor_enabled() -> bool {
    #[cfg(feature = "map-capture")]
    {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| {
            std::env::var_os("GMPUBLISHED_MATERIAL_NEED_VISITOR").as_deref()
                != Some(std::ffi::OsStr::new("0"))
        })
    }
    #[cfg(not(feature = "map-capture"))]
    true
}

fn visit_material_needs<'a, E>(
    slot: &'a MaterialSlot,
    frames: &MaterialFrames,
    visit: impl FnMut(TextureNeed<'a>) -> Result<(), E>,
) -> Result<(), E> {
    visit_material_needs_with_mode(slot, frames, material_need_visitor_enabled(), visit)
}

fn visit_material_needs_with_mode<'a, E>(
    slot: &'a MaterialSlot,
    frames: &MaterialFrames,
    use_visitor: bool,
    visit: impl FnMut(TextureNeed<'a>) -> Result<(), E>,
) -> Result<(), E> {
    #[cfg(not(any(test, feature = "map-capture")))]
    let _ = use_visitor;
    #[cfg(any(test, feature = "map-capture"))]
    if !use_visitor {
        let mut visit = visit;
        let mut needs = Vec::new();
        infallible(visit_material_textures(slot, frames, |need| {
            needs.push(need);
            Ok(())
        }));
        for need in needs {
            visit(need)?;
        }
        return Ok(());
    }
    visit_material_textures(slot, frames, visit)
}

fn infallible<T>(result: Result<T, std::convert::Infallible>) -> T {
    match result {
        Ok(value) => value,
        Err(never) => match never {},
    }
}

fn material_pages(slot: &MaterialSlot, frames: &MaterialFrames) -> Vec<PageNeed> {
    let mut pages = Vec::new();
    collect_material_pages(&mut pages, slot, frames, material_need_visitor_enabled());
    pages
}

fn collect_material_pages(
    pages: &mut Vec<PageNeed>,
    slot: &MaterialSlot,
    frames: &MaterialFrames,
    use_visitor: bool,
) {
    infallible(visit_material_needs_with_mode(
        slot,
        frames,
        use_visitor,
        |need| {
            push_page(pages, need.source, need.frame, need.transfer);
            Ok(())
        },
    ));
}

fn append_material_pages(pages: &mut Vec<PageNeed>, slot: &MaterialSlot, frames: &MaterialFrames) {
    append_material_pages_with_mode(pages, slot, frames, material_need_visitor_enabled());
}

fn append_material_pages_with_mode(
    pages: &mut Vec<PageNeed>,
    slot: &MaterialSlot,
    frames: &MaterialFrames,
    use_visitor: bool,
) {
    #[cfg(any(test, feature = "map-capture"))]
    if !use_visitor {
        let mut material = Vec::new();
        collect_material_pages(&mut material, slot, frames, false);
        pages.extend(material);
        return;
    }
    collect_material_pages(pages, slot, frames, use_visitor);
}

#[derive(Debug)]
pub(super) struct FrameRequest {
    page_needs: OnceLock<Vec<PageNeed>>,
    pub source: PreviewScene,
    pub materials: Vec<MaterialKey>,
    pub sky: bool,
    pub immersion: Option<String>,
    pub eye: super::Vec3,
    pub water_time: f32,
    pub clock: crate::bridge::materials::AnimationClock,
    pub frame_water_time: f32,
    #[cfg(feature = "map-capture")]
    pub exact_frame: bool,
}
impl FrameRequest {
    pub fn new(
        primitive: &super::ModelPrimitive,
        demand: super::residency_demand::FrameMaterialDemand,
        (clock, water_time): (crate::bridge::materials::AnimationClock, f32),
    ) -> Result<Self, crate::bridge::materials::AnimationError> {
        let slots: std::collections::BTreeSet<_> = demand
            .views
            .iter()
            .flat_map(|view| view.materials.iter().copied())
            .collect();
        let source = &primitive.preview;
        let materials = slots
            .into_iter()
            .filter(|&slot| source.render_scene().materials[slot].render_mode.draws())
            .map(|slot| {
                Ok(MaterialKey {
                    scene: source.key(),
                    slot,
                    frames: MaterialFrames::select(
                        &source.render_scene().materials[slot],
                        clock,
                        water_time,
                    )?,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            page_needs: OnceLock::new(),
            #[cfg(feature = "map-capture")]
            exact_frame: primitive.exact_texture_frame,
            source: source.clone(),
            materials,
            sky: demand.views.iter().any(|view| view.sky_background),
            immersion: demand.immersion_effect,
            eye: super::Vec3::new(
                primitive.uniforms.camera_position[0],
                primitive.uniforms.camera_position[1],
                primitive.uniforms.camera_position[2],
            ),
            water_time: primitive.uniforms.water_time_sky_tint[0],
            clock,
            frame_water_time: water_time,
        })
    }
    fn page_needs(&self) -> Cow<'_, [PageNeed]> {
        #[cfg(feature = "map-capture")]
        let enabled = {
            static ENABLED: OnceLock<bool> = OnceLock::new();
            *ENABLED.get_or_init(|| {
                std::env::var_os("GMPUBLISHED_PAGE_NEEDS_CACHE").as_deref()
                    != Some(std::ffi::OsStr::new("0"))
            })
        };
        #[cfg(not(feature = "map-capture"))]
        let enabled = true;
        self.page_needs_with_cache(enabled)
    }

    fn page_needs_with_cache(&self, enabled: bool) -> Cow<'_, [PageNeed]> {
        if enabled {
            Cow::Borrowed(self.page_needs.get_or_init(|| self.collect_page_needs()))
        } else {
            Cow::Owned(self.collect_page_needs())
        }
    }

    fn collect_page_needs(&self) -> Vec<PageNeed> {
        let mut needs = Vec::new();
        for key in &self.materials {
            append_material_pages(
                &mut needs,
                &self.source.render_scene().materials[key.slot],
                &key.frames,
            );
        }
        if self.sky {
            needs.extend(super::background_bindings::sky_pages(&self.source));
        }
        if let Some(effect) = self.immersion() {
            needs.extend(super::background_bindings::immersion_pages(effect));
        }
        needs
    }

    fn immersion(&self) -> Option<&crate::bridge::materials::ScreenRefraction> {
        self.source
            .map()?
            .immersion_material(self.eye)?
            .underwater_overlay
            .as_deref()
    }
}

impl BindingCache {
    fn update_sources(&mut self, requests: &[&FrameRequest]) {
        let open_scenes: HashSet<_> = requests
            .iter()
            .map(|request| request.source.key())
            .collect();
        self.source_scenes
            .retain(|key, scene| open_scenes.contains(key) && scene.owner.alive());
        for request in requests {
            let scene = self
                .source_scenes
                .entry(request.source.key())
                .or_insert_with(|| SceneSources {
                    owner: WeakScene::new(&request.source),
                    sources: HashSet::new(),
                });
            scene.sources.extend(
                request
                    .page_needs()
                    .iter()
                    .map(|(_, source)| source.source_id()),
            );
        }
        self.open_sources.clear();
        self.open_sources.extend(
            self.source_scenes
                .values()
                .flat_map(|scene| scene.sources.iter().copied()),
        );
    }

    pub fn stage_visible(&mut self, requests: &[&FrameRequest]) {
        self.update_sources(requests);
        self.staging_sources = true;
        self.staged_keys.clear();
        for request in requests {
            for (key, source) in request.page_needs().iter() {
                self.staged_keys.insert(key.clone());
                self.pages
                    .entry(key.clone())
                    .or_insert_with(|| PendingPage {
                        validation: PageValidation::default(),
                        source: Arc::clone(source),
                        ticket: None,
                        image: None,
                        allocation: None,
                        error: None,
                    });
            }
        }
    }

    pub fn demand(&mut self, device: &wgpu::Device, requests: &[&FrameRequest]) {
        self.capacity_blocked = false;
        let keys: HashSet<_> = requests
            .iter()
            .flat_map(|request| request.materials.iter().cloned())
            .collect();
        let mut needs = Vec::new();
        for request in requests {
            needs.extend(request.page_needs().iter().cloned());
        }
        if !self.staging_sources {
            self.update_sources(requests);
        }
        self.staging_sources = false;
        self.demand_pages(device, &needs);
        self.retain(&keys);
        let backgrounds: HashSet<_> = requests
            .iter()
            .map(|request| request.background_key())
            .collect();
        self.backgrounds.retain(|key, _| backgrounds.contains(key));
    }
    fn demand_pages(&mut self, device: &wgpu::Device, needs: &[PageNeed]) {
        let cache_enabled = page_validation_cache_enabled();
        let policy = PageDevicePolicy::new(device);
        let active: ResidencySet<_> = needs.iter().map(|(key, _)| key.clone()).collect();
        self.active_keys.clone_from(&active);
        let pins: Vec<_> = needs
            .iter()
            .filter_map(|(key, source)| {
                let state = self
                    .pages
                    .entry(key.clone())
                    .or_insert_with(|| PendingPage {
                        validation: PageValidation::default(),
                        source: Arc::clone(source),
                        ticket: None,
                        image: None,
                        allocation: None,
                        error: None,
                    });
                if state
                    .validation
                    .get_with(policy, cache_enabled, || {
                        validate_page(device, key, &state.source, cache_enabled)
                    })
                    .is_err()
                {
                    return None;
                }
                let image = state
                    .image
                    .as_ref()
                    .cloned()
                    .or_else(|| self.images.lookup_page(&key.request, key.transfer))?;
                Some((key.clone(), image))
            })
            .collect();
        let mut pinned = HashSet::new();
        for (key, image) in pins {
            let allocation = allocation_key(&self.images, &image, key.transfer);
            pinned.insert(allocation.clone());
            self.retiring_warm
                .retain(|retiring| !retiring.ptr_eq(&Arc::downgrade(&image)));
            forget_aliases(&mut self.images, self.warm.activate(&allocation));
            let state = self.pages.get_mut(&key).expect("active page is tracked");
            state.ticket = None;
            state.allocation = Some((allocation, image.byte_len()));
            state.error = None;
            state.image = Some(image);
        }
        let warm = &mut self.warm;
        let images = &mut self.images;
        self.pages.retain(|key, page| {
            if active.contains(key) {
                return true;
            }
            if let Some(image) = page.image.take() {
                let allocation = allocation_key(images, &image, key.transfer);
                if !pinned.contains(&allocation) {
                    let bytes = image.byte_len();
                    let removed = warm.insert(
                        allocation,
                        key.clone(),
                        Arc::downgrade(&page.source),
                        image,
                        bytes,
                    );
                    forget_aliases(images, removed);
                } else {
                    images.forget_page(&key.request, key.transfer);
                }
            }
            self.staged_keys.contains(key)
        });
        forget_aliases(
            &mut self.images,
            self.warm
                .retain_sources(|source| self.open_sources.contains(&source.source_id())),
        );
        for (key, page) in &self.pages {
            if let Some(image) = &page.image {
                self.images.alias_page(&key.request, key.transfer, image);
            }
        }
    }
    pub fn stage_pages(&mut self, device: &wgpu::Device) {
        let cache_enabled = page_validation_cache_enabled();
        let policy = PageDevicePolicy::new(device);
        let Ok(service) = &self.service else {
            return;
        };
        for (key, state) in &mut self.pages {
            if self.staging_sources && !self.staged_keys.contains(key) {
                continue;
            }
            if state.image.is_some() || state.allocation.is_some() || state.error.is_some() {
                continue;
            }
            let validation = match state.validation.get_with(policy, cache_enabled, || {
                validate_page(device, key, &state.source, true)
            }) {
                Ok(validation) => validation,
                Err(error) => {
                    state.error = Some(error);
                    continue;
                }
            };
            let bytes = validation.bytes;
            let ticket = state.ticket.get_or_insert_with(|| {
                service.request(Arc::clone(&state.source), validation.normalized.clone())
            });
            match ticket.status() {
                PageStatus::Ready(page) => {
                    let bytes = if cache_enabled {
                        bytes
                    } else {
                        match ImageCache::page_allocation_cost(
                            device,
                            &key.request,
                            state.source.metadata(),
                        ) {
                            Ok(bytes) => bytes,
                            Err(error) => {
                                state.error = Some(error.to_string());
                                state.ticket = None;
                                continue;
                            }
                        }
                    };
                    state.allocation = Some((
                        AllocationKey {
                            resident: ImageCache::allocation_id(device, page.resident()),
                            transfer: (!self.images.compatible_transfer_views())
                                .then_some(key.transfer),
                        },
                        bytes,
                    ));
                    state.ticket = None;
                }
                PageStatus::Failed(error) => {
                    state.error = Some(error.to_string());
                    state.ticket = None;
                }
                PageStatus::PendingRetry(reason) => {
                    state.error = match reason {
                        crate::bridge::materials::PageRetry::ResidentBytes {
                            required,
                            capacity,
                        }
                        | crate::bridge::materials::PageRetry::WorkingBytes {
                            required,
                            capacity,
                        } if required > capacity => Some(format!(
                            "authored texture page requires {required} bytes; page limit is {capacity}"
                        )),
                        crate::bridge::materials::PageRetry::Closed => {
                            Some("texture page service is closed".to_owned())
                        }
                        _ => None,
                    };
                    state.ticket = None;
                }
                PageStatus::Pending => {}
            }
        }
    }

    pub fn staged_error(&self, request: &FrameRequest) -> Option<String> {
        let needs = request.page_needs();
        if !needs.is_empty()
            && let Err(error) = &self.service
        {
            return Some(error.to_string());
        }
        needs
            .iter()
            .find_map(|(key, _)| self.pages.get(key).and_then(|page| page.error.clone()))
    }

    pub fn working_set(
        &self,
        device: &wgpu::Device,
        request: &FrameRequest,
    ) -> Result<WorkingSet, String> {
        if let Some(error) = self.staged_error(request) {
            return Err(error);
        }
        let mut cost = WorkingSet::new();
        for (key, source) in request.page_needs().iter() {
            if let Some((allocation, bytes)) = self
                .pages
                .get(key)
                .and_then(|page| page.allocation.as_ref())
            {
                cost.insert(SampledAllocation::Texture(allocation.clone()), *bytes, true);
            } else {
                let bytes =
                    ImageCache::page_allocation_cost(device, &key.request, source.metadata())
                        .map_err(|error| error.to_string())?;
                cost.insert(
                    SampledAllocation::Pending {
                        identity: key.request.identity(),
                        transfer: (!self.images.compatible_transfer_views())
                            .then_some(key.transfer),
                    },
                    bytes,
                    false,
                );
            }
        }
        let mut resident =
            |source: &crate::bridge::materials::ResidentTexture, transfer| -> Result<(), String> {
                let bytes = ImageCache::resident_allocation_cost(device, source)
                    .map_err(|error| error.to_string())?;
                cost.insert(
                    SampledAllocation::Texture(AllocationKey {
                        resident: ImageCache::allocation_id(device, source),
                        transfer: (!self.images.compatible_transfer_views()).then_some(transfer),
                    }),
                    bytes,
                    true,
                );
                Ok(())
            };
        for key in &request.materials {
            super::texture_barriers::validate_material(
                &request.source.render_scene().materials[key.slot],
            )
            .map_err(|error| error.to_string())?;
            visit_material_needs(
                &request.source.render_scene().materials[key.slot],
                &key.frames,
                |need| -> Result<(), String> {
                    if let Some(source) = need.resident {
                        resident(source, need.transfer)?;
                    }
                    Ok(())
                },
            )?;
        }
        if request.sky {
            super::texture_barriers::validate_sky(&request.source)
                .map_err(|error| error.to_string())?;
            super::background_bindings::visit_sky_residents(&request.source, &mut resident)?;
        }
        if let Some(effect) = request.immersion() {
            super::texture_barriers::validate_immersion(effect)
                .map_err(|error| error.to_string())?;
            if !effect.normal_map.load_outcome().is_unavailable()
                && let Some(source) = effect.normal_map.resident()
            {
                resident(source, TextureRole::Normal.read_transfer())?;
            }
        }
        Ok(cost)
    }

    pub fn cached(&self, request: &FrameRequest) -> Result<Option<BindingSnapshot>, String> {
        if let Some(error) = self.staged_error(request) {
            return Err(error);
        }
        let mut snapshot = BindingSnapshot::default();
        for key in &request.materials {
            let Some(binding) = self.materials.get(key) else {
                return Ok(None);
            };
            snapshot.materials.0.insert(key.slot, Arc::clone(binding));
        }
        if let Some(background) = self.backgrounds.get(&request.background_key()) {
            snapshot.background = Arc::clone(background);
        } else if request.sky || request.immersion.is_some() {
            return Ok(None);
        }
        Ok(Some(snapshot))
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        request: &FrameRequest,
    ) -> Result<Option<BindingSnapshot>, String> {
        if let Some(error) = self.staged_error(request) {
            return Err(error);
        }
        // The guard submits participating leases before any return, including errors.
        let batch = super::texture_upload::lease_batching_enabled()
            .then(|| super::texture_upload::UploadLeaseBatch::new(&resources.texture_uploader));
        let mut snapshot = BindingSnapshot::default();
        let mut ready = true;
        for key in &request.materials {
            let slot = &request.source.render_scene().materials[key.slot];
            let material = match batch.as_ref() {
                Some(batch) => self.material_with_batch(
                    device,
                    queue,
                    resources,
                    key.clone(),
                    slot,
                    Some(batch),
                ),
                None => self.material(device, queue, resources, key.clone(), slot),
            }?;
            match material {
                Some(binding) => {
                    snapshot.materials.0.insert(key.slot, binding);
                }
                None => ready = false,
            }
        }
        if request.sky {
            super::texture_barriers::validate_sky(&request.source)
                .map_err(|error| error.to_string())?;
        }
        if let Some(effect) = request.immersion() {
            super::texture_barriers::validate_immersion(effect)
                .map_err(|error| error.to_string())?;
        }
        let mut needs = Vec::new();
        if request.sky {
            needs.extend(super::background_bindings::sky_pages(&request.source));
        }
        if let Some(effect) = request.immersion() {
            needs.extend(super::background_bindings::immersion_pages(effect));
        }
        ready &= self.prepare_pages_with_batch(
            device,
            queue,
            &resources.texture_uploader,
            &needs,
            batch.as_ref(),
        )?;
        if !ready {
            return Ok(None);
        }
        let background_key = request.background_key();
        if let Some(background) = self.backgrounds.get(&background_key) {
            snapshot.background = Arc::clone(background);
        } else {
            let mut background = BackgroundBindings::default();
            let mut sampler_diagnostics = Vec::new();
            let mut lease = match batch.as_ref() {
                Some(batch) => self.images.lease_batched(batch),
                None => self.images.lease(&resources.texture_uploader),
            };
            if request.sky {
                background.skybox = super::background_bindings::sky(
                    device,
                    queue,
                    resources,
                    &request.source,
                    &mut lease,
                    &mut sampler_diagnostics,
                )
                .map(Arc::new);
            }
            if let Some(effect) = request.immersion() {
                background.screen_refraction_normals.insert(
                    effect.name.clone(),
                    super::background_bindings::immersion(
                        device,
                        queue,
                        resources,
                        effect,
                        &mut lease,
                        &mut sampler_diagnostics,
                    ),
                );
            }
            let (images, issues) = lease.finish();
            if issues
                .iter()
                .any(|issue| issue.cause == super::images::ImageUploadCause::Capacity)
            {
                self.capacity_blocked = true;
                return Ok(None);
            }
            background.issues = issues;
            background._images = images;
            #[cfg(feature = "map-capture")]
            {
                background.samplers = sampler_diagnostics;
            }
            let background = Arc::new(background);
            self.backgrounds
                .insert(background_key, Arc::clone(&background));
            snapshot.background = background;
        }
        Ok(Some(snapshot))
    }
}

impl Drop for BindingCache {
    fn drop(&mut self) {
        self.pages.clear();
        self.materials.clear();
        self.backgrounds.clear();
        if let Ok(service) = &self.service {
            service.trim_orphaned();
        }
    }
}

#[derive(Clone, Debug, thiserror::Error)]
#[error(
    "visible texture working set requires {required_bytes} bytes in {required_allocations} allocations; GPU pool capacity is {capacity_bytes} bytes in {capacity_allocations} allocations"
)]
pub(super) struct PoolPressure {
    pub required_bytes: u64,
    pub required_allocations: u64,
    pub capacity_bytes: u64,
    pub capacity_allocations: u64,
}

#[cfg(test)]
mod tests;
