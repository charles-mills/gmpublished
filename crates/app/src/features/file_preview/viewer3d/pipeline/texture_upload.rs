use super::wgpu;
use crate::bridge::materials::TextureStorage;
use std::collections::VecDeque;
use std::ops::Deref;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use tokio::sync::watch;

pub(super) type SharedUploader = Arc<Mutex<TextureUploader>>;

const PRESSURE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

pub(in crate::features::file_preview::viewer3d) const RETIREMENT_TIMEOUT: Duration =
    PRESSURE_WAIT_TIMEOUT;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::features::file_preview::viewer3d) struct RetirementState {
    pub epoch: u64,
    pub pending: u64,
    pub started: Option<Instant>,
}

#[derive(Debug)]
pub(in crate::features::file_preview::viewer3d) struct RetirementSignal {
    id: u64,
    sender: watch::Sender<RetirementState>,
}
impl Default for RetirementSignal {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("retirement signal identity exhausted"),
            sender: watch::channel(RetirementState::default()).0,
        }
    }
}
impl RetirementSignal {
    pub fn begin(&self) {
        self.sender.send_modify(|state| {
            if state.pending == 0 {
                state.started = Some(Instant::now());
            }
            state.pending = state.pending.checked_add(1).expect("retiring image count");
        });
    }
    pub fn complete(&self) {
        self.sender.send_modify(|state| {
            state.pending = state.pending.checked_sub(1).expect("retiring image count");
            state.epoch = state.epoch.checked_add(1).expect("image retirement epoch");
            if state.pending == 0 {
                state.started = None;
            }
        });
    }
    pub fn subscribe(&self) -> RetirementSubscription {
        RetirementSubscription {
            id: self.id,
            receiver: self.sender.subscribe(),
        }
    }
    pub fn state(&self) -> RetirementState {
        *self.sender.borrow()
    }
}

#[derive(Clone, Debug)]
pub(in crate::features::file_preview::viewer3d) struct RetirementSubscription {
    pub(in crate::features::file_preview::viewer3d) id: u64,
    pub(in crate::features::file_preview::viewer3d) receiver: watch::Receiver<RetirementState>,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub(in crate::features::file_preview::viewer3d) struct UploadPolicy {
    pub aggregate_staging_bytes: u64,
    pub batch_bytes: u64,
    pub material_image_bytes: u64,
    pub material_image_allocations: u64,
}
impl Default for UploadPolicy {
    fn default() -> Self {
        Self {
            aggregate_staging_bytes: 64 * 1024 * 1024,
            batch_bytes: 8 * 1024 * 1024,
            material_image_bytes: 2 * 1024 * 1024 * 1024,
            material_image_allocations: 65_536,
        }
    }
}

pub(in crate::features::file_preview::viewer3d) fn configured_policy() -> UploadPolicy {
    #[cfg(feature = "map-capture")]
    {
        static POLICY: std::sync::OnceLock<UploadPolicy> = std::sync::OnceLock::new();
        *POLICY.get_or_init(|| {
            let read = |name| match std::env::var(name) {
                Ok(value) => Some(value),
                Err(std::env::VarError::NotPresent) => None,
                Err(std::env::VarError::NotUnicode(_)) => panic!("{name} must be UTF-8"),
            };
            let mut policy = capture_policy(
                read("GMPUBLISHED_MAP_CAPTURE_UPLOAD_STAGING_BYTES").as_deref(),
                read("GMPUBLISHED_MAP_CAPTURE_UPLOAD_BATCH_BYTES").as_deref(),
            )
            .unwrap_or_else(|error| panic!("{error}"));
            for (name, target) in [
                (
                    "GMPUBLISHED_MAP_CAPTURE_GPU_IMAGE_BYTES",
                    &mut policy.material_image_bytes,
                ),
                (
                    "GMPUBLISHED_MAP_CAPTURE_GPU_IMAGE_ALLOCATIONS",
                    &mut policy.material_image_allocations,
                ),
            ] {
                if let Some(value) = read(name) {
                    *target = value
                        .parse::<u64>()
                        .ok()
                        .filter(|value| *value > 0)
                        .unwrap_or_else(|| panic!("{name} must be a positive integer"));
                }
            }
            policy
        })
    }
    #[cfg(not(feature = "map-capture"))]
    UploadPolicy::default()
}

#[cfg(any(test, feature = "map-capture"))]
fn capture_policy(
    staging: Option<&str>,
    batch: Option<&str>,
) -> Result<UploadPolicy, &'static str> {
    let mut policy = UploadPolicy::default();
    if let Some(value) = staging {
        policy.aggregate_staging_bytes = value.parse::<u64>().ok().filter(|value| *value >= 256)
            .ok_or("GMPUBLISHED_MAP_CAPTURE_UPLOAD_STAGING_BYTES must be an integer of at least 256 bytes")?;
    }
    if let Some(value) = batch {
        policy.batch_bytes = value.parse::<u64>().ok().filter(|value| *value >= 256)
            .ok_or("GMPUBLISHED_MAP_CAPTURE_UPLOAD_BATCH_BYTES must be an integer of at least 256 bytes")?;
    }
    Ok(policy)
}

#[derive(Debug, thiserror::Error)]
pub(super) enum UploadError {
    #[error("texture upload policy cannot hold one aligned copy row")]
    InvalidPolicy,
    #[error("texture upload arithmetic overflows")]
    Overflow,
    #[error("texture upload has invalid input or target layout")]
    InvalidLayout,
    #[error("texture upload exceeds a device limit")]
    DeviceLimit,
    #[error("texture upload format is unsupported")]
    UnsupportedFormat,
    #[error("texture upload metadata allocation failed")]
    HostAllocation,
    #[error("material image residency exceeds {bytes} bytes or {allocations} allocations")]
    ImageBudget { bytes: u64, allocations: u64 },
    #[error("texture upload completion failed: {0}")]
    Poll(wgpu::PollError),
    #[error("texture upload completion exceeded {0:?}")]
    CompletionTimeout(Duration),
    #[error(transparent)]
    Decode(#[from] vformats::vtf::VtfError),
}

impl From<wgpu::PollError> for UploadError {
    fn from(error: wgpu::PollError) -> Self {
        match error {
            wgpu::PollError::Timeout => Self::CompletionTimeout(PRESSURE_WAIT_TIMEOUT),
            error @ wgpu::PollError::WrongSubmissionIndex(..) => Self::Poll(error),
        }
    }
}

#[derive(Debug, Default)]
struct Ledger {
    staging: AtomicU64,
    peak_staging: AtomicU64,
    submissions: AtomicU64,
    pressure_waits: AtomicU64,
    pressure_flushes: AtomicU64,
    copied_bytes: AtomicU64,
    owned_images: AtomicU64,
    owned_texel_bytes: AtomicU64,
    material_images: Mutex<ImageAdmission>,
    retirements: RetirementSignal,
}

#[derive(Debug, Default)]
struct ImageAdmission {
    bytes: u64,
    allocations: u64,
    peak_bytes: u64,
    peak_allocations: u64,
    rejections: u64,
}
impl ImageAdmission {
    fn reserve(&mut self, policy: UploadPolicy, bytes: u64) -> Result<(), UploadError> {
        let total = self.bytes.checked_add(bytes).ok_or(UploadError::Overflow)?;
        let allocations = self
            .allocations
            .checked_add(1)
            .ok_or(UploadError::Overflow)?;
        if total > policy.material_image_bytes || allocations > policy.material_image_allocations {
            self.rejections = self.rejections.saturating_add(1);
            return Err(UploadError::ImageBudget {
                bytes: policy.material_image_bytes,
                allocations: policy.material_image_allocations,
            });
        }
        self.bytes = total;
        self.allocations = allocations;
        self.peak_bytes = self.peak_bytes.max(total);
        self.peak_allocations = self.peak_allocations.max(allocations);
        Ok(())
    }

    fn release(&mut self, bytes: u64) {
        self.bytes = self.bytes.checked_sub(bytes).expect("admitted image bytes");
        self.allocations = self
            .allocations
            .checked_sub(1)
            .expect("admitted image count");
    }
}

#[cfg(any(test, feature = "map-capture"))]
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub(super) struct UploadStats {
    pub policy: UploadPolicy,
    pub material_image_scope: &'static str,
    pub image_byte_measurement: &'static str,
    pub applied_batch_bytes: u64,
    pub pressure_wait_timeout_ms: u64,
    pub staging_bytes: u64,
    pub peak_staging_bytes: u64,
    pub submissions: u64,
    pub pressure_waits: u64,
    pub pressure_flushes: u64,
    pub copied_bytes: u64,
    pub owned_image_allocations: u64,
    pub owned_image_texel_bytes: u64,
    pub material_image_bytes: u64,
    pub material_image_allocations: u64,
    pub peak_material_image_bytes: u64,
    pub peak_material_image_allocations: u64,
    pub material_image_rejections: u64,
}

#[derive(Debug)]
struct StagingPermit {
    ledger: Arc<Ledger>,
    bytes: u64,
}
impl StagingPermit {
    fn new(ledger: &Arc<Ledger>, bytes: u64) -> Self {
        let current = ledger.staging.fetch_add(bytes, Ordering::AcqRel) + bytes;
        ledger.peak_staging.fetch_max(current, Ordering::Relaxed);
        Self {
            ledger: Arc::clone(ledger),
            bytes,
        }
    }
}
impl Drop for StagingPermit {
    fn drop(&mut self) {
        self.ledger.staging.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
#[derive(Debug)]
struct ImagePermit {
    ledger: Arc<Ledger>,
    bytes: u64,
    queue: wgpu::Queue,
    material: bool,
}
impl Drop for ImagePermit {
    fn drop(&mut self) {
        let ledger = Arc::clone(&self.ledger);
        let bytes = self.bytes;
        let material = self.material;
        if material {
            ledger.retirements.begin();
        }
        self.queue.on_submitted_work_done(move || {
            ledger.owned_images.fetch_sub(1, Ordering::AcqRel);
            ledger.owned_texel_bytes.fetch_sub(bytes, Ordering::AcqRel);
            if material {
                let mut admission = ledger.material_images.lock().expect("image admission lock");
                admission.release(bytes);
                drop(admission);
                ledger.retirements.complete();
            }
        });
    }
}
#[derive(Clone, Debug)]
pub(in crate::features::file_preview::viewer3d) struct UploadedTexture {
    texture: wgpu::Texture,
    _permit: Arc<ImagePermit>,
}
impl Deref for UploadedTexture {
    type Target = wgpu::Texture;
    fn deref(&self) -> &Self::Target {
        &self.texture
    }
}

impl UploadedTexture {
    pub(super) fn create_view(&self, desc: &wgpu::TextureViewDescriptor<'_>) -> UploadedView {
        UploadedView {
            view: self.texture.create_view(desc),
            _texture: self.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::features::file_preview::viewer3d) struct UploadedView {
    view: wgpu::TextureView,
    _texture: UploadedTexture,
}
impl Deref for UploadedView {
    type Target = wgpu::TextureView;
    fn deref(&self) -> &Self::Target {
        &self.view
    }
}
impl PartialEq for UploadedView {
    fn eq(&self, other: &Self) -> bool {
        self.view == other.view
    }
}
impl Eq for UploadedView {}

#[derive(Clone, Copy, Debug)]
pub(super) struct Input<'a> {
    pub storage: TextureStorage,
    pub dimensions: [u32; 2],
    pub bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CopyPlan {
    dimensions: [u32; 2],
    input_storage: TextureStorage,
    output_storage: TextureStorage,
    block: u32,
    block_bytes: u32,
    tile_blocks: [u32; 2],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tile {
    region: [u32; 4],
    block_origin: [u32; 2],
    block_count: [u32; 2],
    row_pitch: u32,
    bytes: u64,
}
struct Tiles {
    plan: CopyPlan,
    next: [u32; 2],
}
impl Iterator for Tiles {
    type Item = Tile;
    fn next(&mut self) -> Option<Tile> {
        let total = self
            .plan
            .dimensions
            .map(|value| value.div_ceil(self.plan.block));
        if self.next[0] >= total[0] {
            return None;
        }
        let origin = self.next;
        let count =
            std::array::from_fn(|axis| self.plan.tile_blocks[axis].min(total[axis] - origin[axis]));
        let row_pitch = align(u64::from(count[0]) * u64::from(self.plan.block_bytes), 256)? as u32;
        self.next[1] += count[1];
        if self.next[1] == total[1] {
            self.next = [origin[0] + count[0], 0];
        }
        Some(Tile {
            region: [
                origin[0] * self.plan.block,
                origin[1] * self.plan.block,
                count[0] * self.plan.block,
                count[1] * self.plan.block,
            ],
            block_origin: origin,
            block_count: count,
            row_pitch,
            bytes: u64::from(row_pitch) * u64::from(count[1]),
        })
    }
}
fn align(value: u64, alignment: u64) -> Option<u64> {
    value
        .checked_add(alignment - 1)
        .map(|value| value / alignment * alignment)
}
fn storage(format: wgpu::TextureFormat) -> Result<TextureStorage, UploadError> {
    use vformats::vtf::BcFormat;
    match format.remove_srgb_suffix() {
        wgpu::TextureFormat::Rgba8Unorm => Ok(TextureStorage::Rgba8),
        wgpu::TextureFormat::Bc1RgbaUnorm => Ok(TextureStorage::Bc(BcFormat::Bc1)),
        wgpu::TextureFormat::Bc2RgbaUnorm => Ok(TextureStorage::Bc(BcFormat::Bc2)),
        wgpu::TextureFormat::Bc3RgbaUnorm => Ok(TextureStorage::Bc(BcFormat::Bc3)),
        _ => Err(UploadError::UnsupportedFormat),
    }
}
fn block(storage: TextureStorage) -> (u32, u32) {
    match storage {
        TextureStorage::Rgba8 => (1, 4),
        TextureStorage::Bc(format) => (4, format.block_bytes()),
    }
}
fn byte_len(storage: TextureStorage, dimensions: [u32; 2]) -> Option<u64> {
    let (block, bytes) = block(storage);
    u64::from(dimensions[0].div_ceil(block))
        .checked_mul(u64::from(dimensions[1].div_ceil(block)))?
        .checked_mul(u64::from(bytes))
}
impl CopyPlan {
    fn new(input: Input<'_>, output: TextureStorage, batch: u64) -> Result<Self, UploadError> {
        if input.dimensions.contains(&0) {
            return Err(UploadError::InvalidLayout);
        }
        let required = byte_len(input.storage, input.dimensions).ok_or(UploadError::Overflow)?;
        if required != input.bytes.len() as u64
            || (input.storage != output
                && !matches!(
                    (input.storage, output),
                    (TextureStorage::Bc(_), TextureStorage::Rgba8)
                ))
        {
            return Err(UploadError::InvalidLayout);
        }
        let (block, block_bytes) = block(output);
        let available = batch / 256 * 256;
        let maximum_blocks =
            u32::try_from(available.min(u64::from(u32::MAX / 256 * 256)) / u64::from(block_bytes))
                .map_err(|_| UploadError::Overflow)?;
        if maximum_blocks == 0 {
            return Err(UploadError::InvalidPolicy);
        }
        let width = input.dimensions[0].div_ceil(block).min(maximum_blocks);
        let pitch =
            align(u64::from(width) * u64::from(block_bytes), 256).ok_or(UploadError::Overflow)?;
        let height = u32::try_from((available / pitch).min(u64::from(u32::MAX)))
            .map_err(|_| UploadError::Overflow)?;
        if height == 0 {
            return Err(UploadError::InvalidPolicy);
        }
        Ok(Self {
            dimensions: input.dimensions,
            input_storage: input.storage,
            output_storage: output,
            block,
            block_bytes,
            tile_blocks: [width, height],
        })
    }
    fn tiles(self) -> Tiles {
        Tiles {
            plan: self,
            next: [0; 2],
        }
    }
}

#[derive(Debug)]
struct Copy {
    texture: UploadedTexture,
    mip: u32,
    origin: wgpu::Origin3d,
    size: wgpu::Extent3d,
    offset: u64,
    pitch: u32,
    rows: u32,
}
#[derive(Debug)]
struct Batch {
    buffer: wgpu::Buffer,
    _permit: StagingPermit,
    used: u64,
    copies: Vec<Copy>,
}
#[derive(Debug)]
struct Submission {
    index: wgpu::SubmissionIndex,
    done: Arc<AtomicBool>,
}
#[derive(Debug)]
pub(super) struct TextureUploader {
    device: wgpu::Device,
    queue: wgpu::Queue,
    policy: UploadPolicy,
    batch_bytes: u64,
    ledger: Arc<Ledger>,
    pending: Option<Batch>,
    in_flight: VecDeque<Submission>,
}
fn image_texel_bytes(
    format: wgpu::TextureFormat,
    dimensions: [u32; 2],
    mips: u32,
    layers: u32,
) -> Result<u64, UploadError> {
    if dimensions.contains(&0)
        || layers == 0
        || mips == 0
        || mips > u32::BITS - dimensions[0].max(dimensions[1]).leading_zeros()
    {
        return Err(UploadError::InvalidLayout);
    }
    let storage = storage(format)?;
    (0..mips)
        .try_fold(0_u64, |sum, mip| {
            sum.checked_add(byte_len(
                storage,
                dimensions.map(|side| (side >> mip).max(1)),
            )?)
        })
        .and_then(|bytes| bytes.checked_mul(u64::from(layers)))
        .ok_or(UploadError::Overflow)
}

fn batch_capacity(policy: UploadPolicy, device_max: u64) -> Result<u64, UploadError> {
    let bytes = policy
        .batch_bytes
        .min(policy.aggregate_staging_bytes)
        .min(device_max)
        .min(isize::MAX as u64)
        / 256
        * 256;
    if bytes == 0 {
        Err(UploadError::InvalidPolicy)
    } else {
        Ok(bytes)
    }
}
impl TextureUploader {
    pub(super) fn shared(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        policy: UploadPolicy,
    ) -> Result<SharedUploader, UploadError> {
        let batch_bytes = batch_capacity(policy, device.limits().max_buffer_size)?;
        Ok(Arc::new(Mutex::new(Self {
            device: device.clone(),
            queue: queue.clone(),
            policy,
            batch_bytes,
            ledger: Arc::default(),
            pending: None,
            in_flight: VecDeque::new(),
        })))
    }
    #[cfg(any(test, feature = "map-capture"))]
    pub(super) fn stats(&self) -> UploadStats {
        let images = self
            .ledger
            .material_images
            .lock()
            .expect("image admission lock");
        UploadStats {
            policy: self.policy,
            material_image_scope: "shared renderer pool material images; excludes lightmaps, generated fallbacks and render targets",
            image_byte_measurement: "descriptor texel payload estimate; excludes driver, alignment and view overhead",
            applied_batch_bytes: self.batch_bytes,
            pressure_wait_timeout_ms: PRESSURE_WAIT_TIMEOUT.as_millis() as u64,
            staging_bytes: self.ledger.staging.load(Ordering::Acquire),
            peak_staging_bytes: self.ledger.peak_staging.load(Ordering::Relaxed),
            submissions: self.ledger.submissions.load(Ordering::Relaxed),
            pressure_waits: self.ledger.pressure_waits.load(Ordering::Relaxed),
            pressure_flushes: self.ledger.pressure_flushes.load(Ordering::Relaxed),
            copied_bytes: self.ledger.copied_bytes.load(Ordering::Relaxed),
            owned_image_allocations: self.ledger.owned_images.load(Ordering::Relaxed),
            owned_image_texel_bytes: self.ledger.owned_texel_bytes.load(Ordering::Relaxed),
            material_image_bytes: images.bytes,
            material_image_allocations: images.allocations,
            peak_material_image_bytes: images.peak_bytes,
            peak_material_image_allocations: images.peak_allocations,
            material_image_rejections: images.rejections,
        }
    }
    pub(super) fn plan(
        &self,
        input: Input<'_>,
        format: wgpu::TextureFormat,
    ) -> Result<CopyPlan, UploadError> {
        CopyPlan::new(input, storage(format)?, self.batch_bytes)
    }
    pub(super) fn allocate(
        &self,
        desc: &wgpu::TextureDescriptor<'_>,
    ) -> Result<UploadedTexture, UploadError> {
        self.allocate_image(desc, false)
    }
    pub(super) fn allocate_material(
        &self,
        desc: &wgpu::TextureDescriptor<'_>,
    ) -> Result<UploadedTexture, UploadError> {
        self.allocate_image(desc, true)
    }
    fn allocate_image(
        &self,
        desc: &wgpu::TextureDescriptor<'_>,
        material: bool,
    ) -> Result<UploadedTexture, UploadError> {
        let limits = self.device.limits();
        if desc.dimension != wgpu::TextureDimension::D2
            || desc.size.width == 0
            || desc.size.height == 0
            || desc.size.depth_or_array_layers == 0
            || desc.sample_count != 1
            || desc.size.width > limits.max_texture_dimension_2d
            || desc.size.height > limits.max_texture_dimension_2d
            || desc.size.depth_or_array_layers > limits.max_texture_array_layers
            || desc.mip_level_count == 0
            || desc.mip_level_count
                > u32::BITS - desc.size.width.max(desc.size.height).leading_zeros()
        {
            return Err(UploadError::DeviceLimit);
        }
        let image_storage = storage(desc.format)?;
        if matches!(image_storage, TextureStorage::Bc(_))
            && (!self
                .device
                .features()
                .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
                || !desc.size.width.is_multiple_of(4)
                || !desc.size.height.is_multiple_of(4))
        {
            return Err(UploadError::DeviceLimit);
        }
        let bytes = image_texel_bytes(
            desc.format,
            [desc.size.width, desc.size.height],
            desc.mip_level_count,
            desc.size.depth_or_array_layers,
        )?;
        self.ledger
            .owned_images
            .load(Ordering::Acquire)
            .checked_add(1)
            .ok_or(UploadError::Overflow)?;
        self.ledger
            .owned_texel_bytes
            .load(Ordering::Acquire)
            .checked_add(bytes)
            .ok_or(UploadError::Overflow)?;
        if material {
            self.device.poll(wgpu::PollType::Poll)?;
            let mut admission = self
                .ledger
                .material_images
                .lock()
                .expect("image admission lock");
            admission.reserve(self.policy, bytes)?;
        }
        self.ledger.owned_images.fetch_add(1, Ordering::AcqRel);
        self.ledger
            .owned_texel_bytes
            .fetch_add(bytes, Ordering::AcqRel);
        let permit = Arc::new(ImagePermit {
            ledger: Arc::clone(&self.ledger),
            bytes,
            queue: self.queue.clone(),
            material,
        });
        let texture = self.device.create_texture(desc);
        Ok(UploadedTexture {
            texture,
            _permit: permit,
        })
    }
    fn start_batch(&mut self) -> Result<(), UploadError> {
        self.device.poll(wgpu::PollType::Poll)?;
        while self
            .in_flight
            .front()
            .is_some_and(|submission| submission.done.load(Ordering::Acquire))
        {
            self.in_flight.pop_front();
        }
        let pressure_started = Instant::now();
        while self
            .ledger
            .staging
            .load(Ordering::Acquire)
            .checked_add(self.batch_bytes)
            .ok_or(UploadError::Overflow)?
            > self.policy.aggregate_staging_bytes
        {
            let timeout = PRESSURE_WAIT_TIMEOUT
                .checked_sub(pressure_started.elapsed())
                .filter(|remaining| !remaining.is_zero())
                .ok_or(UploadError::CompletionTimeout(PRESSURE_WAIT_TIMEOUT))?;
            let oldest = self.in_flight.front().ok_or(UploadError::InvalidPolicy)?;
            self.ledger.pressure_waits.fetch_add(1, Ordering::Relaxed);
            self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(oldest.index.clone()),
                timeout: Some(timeout),
            })?;
            while self
                .in_flight
                .front()
                .is_some_and(|submission| submission.done.load(Ordering::Acquire))
            {
                self.in_flight.pop_front();
            }
        }
        let permit = StagingPermit::new(&self.ledger, self.batch_bytes);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.texture_upload.batch"),
            size: self.batch_bytes,
            usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        self.pending = Some(Batch {
            buffer,
            _permit: permit,
            used: 0,
            copies: Vec::new(),
        });
        Ok(())
    }
    pub(super) fn write(
        &mut self,
        texture: &UploadedTexture,
        mip: u32,
        origin: wgpu::Origin3d,
        input: Input<'_>,
    ) -> Result<(), UploadError> {
        let plan = self.plan(input, texture.format())?;
        self.write_planned(texture, mip, origin, input, plan)
    }
    pub(super) fn write_planned(
        &mut self,
        texture: &UploadedTexture,
        mip: u32,
        origin: wgpu::Origin3d,
        input: Input<'_>,
        plan: CopyPlan,
    ) -> Result<(), UploadError> {
        if byte_len(input.storage, input.dimensions) != Some(input.bytes.len() as u64)
            || !origin.x.is_multiple_of(plan.block)
            || !origin.y.is_multiple_of(plan.block)
            || input.dimensions != plan.dimensions
            || input.storage != plan.input_storage
            || storage(texture.format())? != plan.output_storage
            || !texture.usage().contains(wgpu::TextureUsages::COPY_DST)
            || mip >= texture.mip_level_count()
            || origin.z >= texture.depth_or_array_layers()
        {
            return Err(UploadError::InvalidLayout);
        }
        let extent = [
            (texture.width() >> mip).max(1),
            (texture.height() >> mip).max(1),
        ];
        if [origin.x, origin.y]
            .into_iter()
            .zip(input.dimensions)
            .zip(extent)
            .any(|((at, count), limit)| at.checked_add(count).is_none_or(|end| end > limit))
        {
            return Err(UploadError::InvalidLayout);
        }
        let widest = plan.dimensions[0]
            .div_ceil(plan.block)
            .min(plan.tile_blocks[0]);
        let tallest = plan.dimensions[1]
            .div_ceil(plan.block)
            .min(plan.tile_blocks[1]);
        let maximum_copy = align(u64::from(widest) * u64::from(plan.block_bytes), 256)
            .and_then(|pitch| pitch.checked_mul(u64::from(tallest)))
            .ok_or(UploadError::Overflow)?;
        if maximum_copy > self.batch_bytes {
            return Err(UploadError::InvalidPolicy);
        }
        for tile in plan.tiles() {
            if self
                .pending
                .as_ref()
                .is_some_and(|batch| batch.used + tile.bytes > self.batch_bytes)
            {
                self.ledger.pressure_flushes.fetch_add(1, Ordering::Relaxed);
                self.flush();
            }
            if self.pending.is_none() {
                self.start_batch()?;
            }
            let batch = self.pending.as_mut().expect("staging batch initialized");
            batch
                .copies
                .try_reserve(1)
                .map_err(|_| UploadError::HostAllocation)?;
            let offset = batch.used;
            {
                let mut output = batch
                    .buffer
                    .slice(offset..offset + tile.bytes)
                    .get_mapped_range_mut();
                if let (TextureStorage::Bc(format), TextureStorage::Rgba8) =
                    (input.storage, plan.output_storage)
                {
                    vformats::vtf::decode_bc_rgba_region_into(
                        format,
                        input.dimensions,
                        tile.region,
                        input.bytes,
                        &mut output,
                        tile.row_pitch as usize,
                    )?;
                } else {
                    let source_pitch = input.dimensions[0].div_ceil(plan.block) as usize
                        * plan.block_bytes as usize;
                    let count = tile.block_count[0] as usize * plan.block_bytes as usize;
                    for row in 0..tile.block_count[1] as usize {
                        let start = (tile.block_origin[1] as usize + row) * source_pitch
                            + tile.block_origin[0] as usize * plan.block_bytes as usize;
                        output
                            [row * tile.row_pitch as usize..row * tile.row_pitch as usize + count]
                            .copy_from_slice(&input.bytes[start..start + count]);
                    }
                }
            }
            batch.copies.push(Copy {
                texture: texture.clone(),
                mip,
                origin: wgpu::Origin3d {
                    x: origin.x + tile.region[0],
                    y: origin.y + tile.region[1],
                    z: origin.z,
                },
                size: wgpu::Extent3d {
                    width: tile.region[2],
                    height: tile.region[3],
                    depth_or_array_layers: 1,
                },
                offset,
                pitch: tile.row_pitch,
                rows: tile.block_count[1],
            });
            batch.used += tile.bytes;
            self.ledger
                .copied_bytes
                .fetch_add(tile.bytes, Ordering::Relaxed);
        }
        Ok(())
    }
    pub(super) fn retirement_state(&self) -> RetirementState {
        self.ledger.retirements.state()
    }

    pub(super) fn retirement_subscription(&self) -> RetirementSubscription {
        self.ledger.retirements.subscribe()
    }

    pub(super) fn poll_retirements(&self) -> Result<(), UploadError> {
        if self.ledger.retirements.state().pending != 0 {
            self.device.poll(wgpu::PollType::Poll)?;
        }
        Ok(())
    }

    pub(super) fn flush(&mut self) {
        let Some(batch) = self.pending.take() else {
            return;
        };
        batch.buffer.unmap();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("file_preview.texture_upload.encoder"),
            });
        for copy in &batch.copies {
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &batch.buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: copy.offset,
                        bytes_per_row: Some(copy.pitch),
                        rows_per_image: Some(copy.rows),
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &copy.texture,
                    mip_level: copy.mip,
                    origin: copy.origin,
                    aspect: wgpu::TextureAspect::All,
                },
                copy.size,
            );
        }
        let index = self.queue.submit([encoder.finish()]);
        self.ledger.submissions.fetch_add(1, Ordering::Relaxed);
        let done = Arc::new(AtomicBool::new(false));
        let finished = Arc::clone(&done);
        self.queue.on_submitted_work_done(move || {
            drop(batch);
            finished.store(true, Ordering::Release);
        });
        self.in_flight.push_back(Submission { index, done });
    }
}

#[cfg(test)]
mod tests;
