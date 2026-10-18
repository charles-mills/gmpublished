//! Native, fixed-count indirect draws for adjacent whole transparent operations.
//! The existing ordered queue owns every operation; this stores only multi-runs.
use super::geometry::ViewerScene;
use super::lightmap_ranges::PageRange;
use super::reflections::SceneView;
use super::transparency::{Draw, Primitive};
use super::visibility::UploadedVisibility;
use super::{DrawItem, RenderMode, wgpu};
use gmpublished_domain::scene::map::LightmapPageId;
use std::ops::Range;

const ARGUMENT_STRIDE: usize = std::mem::size_of::<wgpu::util::DrawIndexedIndirectArgs>();
const MAX_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_ARGUMENTS: usize = MAX_ARGUMENT_BYTES / ARGUMENT_STRIDE;

#[derive(Debug, Default)]
pub(super) struct Slots {
    pub(super) world: Slot,
    pub(super) skybox: Slot,
    #[cfg(feature = "map-capture")]
    native_supported: bool,
    #[cfg(feature = "map-capture")]
    active: bool,
}

#[derive(Debug, Default)]
pub(super) struct Slot {
    buffer: Option<wgpu::Buffer>,
    capacity: u64,
    arguments: Vec<wgpu::util::DrawIndexedIndirectArgs>,
    runs: Vec<Run>,
    one_part_operations: usize,
    overflowed: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Run {
    pub(super) operations: Range<usize>,
    pub(super) page: Option<LightmapPageId>,
    pub(super) offset: u64,
}

impl Run {
    pub(super) fn count(&self) -> u32 {
        u32::try_from(self.operations.len()).expect("bounded indirect run")
    }
}

#[derive(Clone, Copy)]
pub(super) struct Program<'a> {
    pub(super) buffer: &'a wgpu::Buffer,
    pub(super) runs: &'a [Run],
    pub(super) operation_offset: usize,
}

impl Program<'_> {
    pub(super) fn segment(self, operations: Range<usize>) -> Option<Self> {
        let runs = segment_runs(self.runs, operations.clone());
        (!runs.is_empty()).then_some(Self {
            runs,
            operation_offset: operations.start,
            ..self
        })
    }
}

fn segment_runs(runs: &[Run], operations: Range<usize>) -> &[Run] {
    let first = runs.partition_point(|run| run.operations.start < operations.start);
    let count = runs[first..].partition_point(|run| run.operations.end <= operations.end);
    &runs[first..first + count]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    item: DrawItem,
    blend: RenderMode,
    visible: bool,
    page: Option<LightmapPageId>,
}

/// COUNT publicly guarantees native fixed-count MDI in wgpu 27. No count buffer
/// or COUNT entry point is used, and the emulated fixed-count path is excluded.
fn enabled(native_supported: bool) -> bool {
    if !native_supported {
        return false;
    }
    #[cfg(any(feature = "map-capture", feature = "gui-benchmark"))]
    {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ENABLED.get_or_init(|| {
            std::env::var_os("GMPUBLISHED_NATIVE_TRANSPARENT_MDI").as_deref()
                != Some(std::ffi::OsStr::new("0"))
        })
    }
    #[cfg(not(any(feature = "map-capture", feature = "gui-benchmark")))]
    true
}

pub(super) fn prepare(
    view: &mut SceneView,
    upload: &ViewerScene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) {
    let native_supported = device
        .features()
        .contains(wgpu::Features::MULTI_DRAW_INDIRECT_COUNT);
    if !enabled(native_supported) {
        view.transparent_indirect = Slots {
            #[cfg(feature = "map-capture")]
            native_supported,
            ..Slots::default()
        };
        return;
    }
    #[cfg(feature = "map-capture")]
    {
        view.transparent_indirect.native_supported = native_supported;
        view.transparent_indirect.active = true;
    }
    view.transparent_indirect.world.prepare(
        &view.transparency,
        upload,
        &view.visibility,
        device,
        queue,
    );
    view.transparent_indirect.skybox.prepare(
        &view.skybox_transparency,
        upload,
        &view.skybox_visibility,
        device,
        queue,
    );
}

/// Resolve precisely the page parts that draw_operations emits. Explicit ranges
/// always use original indices. Missing visible buffers never resurrect geometry.
fn one_part(
    item: DrawItem,
    range: Option<&Range<u32>>,
    upload: &ViewerScene,
    visibility: &UploadedVisibility,
) -> Option<(bool, PageRange)> {
    let mesh = &upload.meshes[item.mesh_index];
    upload.bindings.materials.get(item.material_slot)?;
    if let Some(range) = range {
        let mut parts = mesh.lightmaps.intersect(range.clone());
        let part = parts.next()?;
        return parts.next().is_none().then_some((false, part));
    }
    let (visible, parts) = if let Some(indices) = visibility.mesh_indices.get(&item.mesh_index) {
        indices.buffer.as_ref()?;
        (true, indices.lightmaps.as_slice())
    } else {
        (false, mesh.lightmaps.indexed.as_slice())
    };
    match parts {
        [part] => Some((visible, part.clone())),
        _ => None,
    }
}

impl Slot {
    fn prepare(
        &mut self,
        operations: &[Draw],
        upload: &ViewerScene,
        visibility: &UploadedVisibility,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        self.build(operations, |item, range| {
            one_part(item, range, upload, visibility)
        });
        self.upload_arguments(device, queue);
    }

    fn build(
        &mut self,
        operations: &[Draw],
        mut resolve: impl FnMut(DrawItem, Option<&Range<u32>>) -> Option<(bool, PageRange)>,
    ) {
        self.arguments.clear();
        self.runs.clear();
        self.one_part_operations = 0;
        self.overflowed = false;
        let mut pending: Option<(Key, Range<usize>, usize)> = None;
        for (index, operation) in operations.iter().enumerate() {
            let resolved = match &operation.primitive {
                Primitive::Mesh(item, range) => resolve(*item, range.as_ref())
                    .filter(|(_, part)| !part.indices.is_empty())
                    .map(|(visible, part)| {
                        (
                            Key {
                                item: *item,
                                blend: operation.blend,
                                visible,
                                page: part.page,
                            },
                            part,
                        )
                    }),
                Primitive::Overlay(_) | Primitive::Sprite(_) => None,
            };
            if pending.as_ref().map(|(key, _, _)| *key) != resolved.as_ref().map(|(key, _)| *key) {
                self.finish_run(pending.take());
            }
            let Some((key, part)) = resolved else {
                continue;
            };
            self.one_part_operations += 1;
            if !push_bounded(
                &mut self.arguments,
                wgpu::util::DrawIndexedIndirectArgs {
                    index_count: part.indices.end - part.indices.start,
                    instance_count: 1,
                    first_index: part.indices.start,
                    base_vertex: 0,
                    first_instance: 0,
                },
                MAX_ARGUMENTS,
            ) {
                // Indirect arguments remain bounded independently of map size.
                // Overflow preserves the entire original ordered direct loop.
                self.arguments.clear();
                self.runs.clear();
                self.overflowed = true;
                return;
            }
            if let Some((_, operations, _)) = &mut pending {
                operations.end = index + 1;
            } else {
                pending = Some((key, index..index + 1, self.arguments.len() - 1));
            }
        }
        self.finish_run(pending);
    }

    fn finish_run(&mut self, pending: Option<(Key, Range<usize>, usize)>) {
        let Some((key, operations, first_argument)) = pending else {
            return;
        };
        if operations.len() == 1 {
            self.arguments.pop();
        } else {
            let inserted = push_bounded(
                &mut self.runs,
                Run {
                    operations,
                    page: key.page,
                    offset: (first_argument * ARGUMENT_STRIDE) as u64,
                },
                MAX_ARGUMENTS / 2,
            );
            assert!(
                inserted,
                "every stored run has at least two bounded arguments"
            );
        }
    }

    fn upload_arguments(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.arguments.is_empty() {
            return;
        }
        let bytes = bytemuck::cast_slice::<_, u8>(&self.arguments);
        let required = bytes.len() as u64;
        if required > device.limits().max_buffer_size {
            self.runs.clear();
            self.arguments.clear();
            self.overflowed = true;
            return;
        }
        if required > self.capacity {
            let capacity = required
                .next_power_of_two()
                .min(MAX_ARGUMENT_BYTES as u64)
                .min(device.limits().max_buffer_size);
            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.transparent_indirect.arguments"),
                size: capacity,
                usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.capacity = capacity;
        }
        // Each SceneView/world/sky slot is prepared once before rendering it.
        // Other optical/intersection/pane views own separate buffers, so queue
        // writes cannot overwrite arguments for another draw in this submission.
        queue.write_buffer(self.buffer.as_ref().expect("argument buffer"), 0, bytes);
    }

    pub(super) fn program(&self) -> Option<Program<'_>> {
        if self.runs.is_empty() {
            return None;
        }
        Some(Program {
            buffer: self.buffer.as_ref()?,
            runs: &self.runs,
            operation_offset: 0,
        })
    }

    #[cfg(feature = "map-capture")]
    fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({
            "one_part_operations":self.one_part_operations,
            "multi_draw_runs":self.runs.len(),
            "indirect_parts":self.arguments.len(),
            "draw_calls_saved":self.arguments.len().saturating_sub(self.runs.len()),
            "argument_bytes":self.arguments.len()*ARGUMENT_STRIDE,
            "buffer_capacity_bytes":self.capacity,
            "overflowed":self.overflowed,
        })
    }
}

/// Vec's normal geometric growth can exceed the cap on the final allocation.
fn push_bounded<T>(values: &mut Vec<T>, value: T, maximum: usize) -> bool {
    if values.len() == maximum {
        return false;
    }
    if values.len() == values.capacity() {
        let capacity = values.capacity().max(4).saturating_mul(2).min(maximum);
        values.reserve_exact(capacity - values.len());
    }
    values.push(value);
    true
}

#[cfg(feature = "map-capture")]
impl Slots {
    pub(super) fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({
            "native_supported":self.native_supported,
            "active":self.active,
            "world":self.world.diagnostics(),
            "skybox":self.skybox.diagnostics(),
        })
    }
}

#[cfg(test)]
mod gpu_tests;
#[cfg(test)]
mod tests;
