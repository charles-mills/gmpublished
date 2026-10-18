use super::super::wgpu;
use crate::media::preview_model::ModelVertex;
use gmpublished_domain::math::Vec3;
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::{Arc, Weak};
use vformats::mdl::ModelSkinVertex;

pub(in super::super) const WORKGROUP_SIZE: u32 = 64;

#[derive(Clone, Copy, Debug)]
pub(in super::super) struct Placement {
    pub origin: Vec3,
    pub angles: Vec3,
    pub scale: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SkinVertex {
    position: [f32; 4],
    normal: [f32; 4],
    tangent: [f32; 4],
    weights: [f32; 4],
    bones: [u32; 4],
}

pub(in super::super) const SOURCE_VERTEX_BYTES: u64 = std::mem::size_of::<SkinVertex>() as u64;

pub(in super::super) fn supports_storage_windows(limits: &wgpu::Limits) -> bool {
    let alignment = u64::from(limits.min_storage_buffer_offset_alignment);
    alignment != 0
        && limits.max_compute_workgroups_per_dimension != 0
        && [
            SOURCE_VERTEX_BYTES,
            std::mem::size_of::<ModelVertex>() as u64,
        ]
        .into_iter()
        .all(|stride| {
            stride / gcd(stride, alignment) * alignment
                <= u64::from(limits.max_storage_buffer_binding_size)
        })
}

impl From<&ModelSkinVertex> for SkinVertex {
    fn from(source: &ModelSkinVertex) -> Self {
        Self {
            position: [
                source.vertex.position[0],
                source.vertex.position[1],
                source.vertex.position[2],
                1.0,
            ],
            normal: [
                source.vertex.normal[0],
                source.vertex.normal[1],
                source.vertex.normal[2],
                0.0,
            ],
            tangent: [
                source.vertex.tangent_s[0],
                source.vertex.tangent_s[1],
                source.vertex.tangent_s[2],
                source.vertex.tangent_sign,
            ],
            weights: [source.weights[0], source.weights[1], source.weights[2], 0.0],
            bones: [
                source.bones[0],
                source.bones[1],
                source.bones[2],
                source.bone_count,
            ],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Parameters {
    placement: [[f32; 4]; 4],
    direction: [[f32; 4]; 3],
    source_start: u32,
    target_start: u32,
    vertex_count: u32,
    padding: u32,
}

#[derive(Debug)]
pub(in super::super) struct Source {
    pub buffer: wgpu::Buffer,
    pub vertices: Arc<[ModelSkinVertex]>,
    pub centroid: super::pose::Centroid,
}

#[derive(Debug, Default)]
pub(in super::super) struct SourceCache(HashMap<usize, Weak<Source>>);

impl SourceCache {
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertices: &Arc<[ModelSkinVertex]>,
    ) -> Arc<Source> {
        let key = vertices.as_ptr() as usize;
        if let Some(source) = self.0.get(&key).and_then(Weak::upgrade) {
            return source;
        }
        let packed: Vec<_> = vertices.iter().map(SkinVertex::from).collect();
        let bytes = bytemuck::cast_slice(&packed);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.animation.bind_vertices"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, bytes);
        let source = Arc::new(Source {
            buffer,
            centroid: super::pose::Centroid::new(vertices),
            vertices: Arc::clone(vertices),
        });
        self.0.insert(key, Arc::downgrade(&source));
        source
    }

    pub fn trim(&mut self) {
        self.0.retain(|_, source| source.strong_count() != 0);
    }
}

#[derive(Debug)]
pub(in super::super) struct Resources {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

impl Resources {
    pub fn new(device: &wgpu::Device) -> Self {
        let entries = [
            (0, wgpu::BufferBindingType::Storage { read_only: true }),
            (1, wgpu::BufferBindingType::Storage { read_only: true }),
            (2, wgpu::BufferBindingType::Storage { read_only: false }),
            (3, wgpu::BufferBindingType::Uniform),
        ]
        .map(|(binding, ty)| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.animation.layout"),
            entries: &entries,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.animation.skin"),
            source: wgpu::ShaderSource::Wgsl(include_str!("skin.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("file_preview.animation.pipeline_layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("file_preview.animation.pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("skin"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { layout, pipeline }
    }

    pub fn dispatches(
        &self,
        device: &wgpu::Device,
        source: &Source,
        output: &wgpu::Buffer,
        palette: &wgpu::Buffer,
        placement: Placement,
        vertex_start: u32,
    ) -> Vec<Dispatch> {
        let limits = device.limits();
        let source_stride = SOURCE_VERTEX_BYTES;
        let target_stride = std::mem::size_of::<ModelVertex>() as u64;
        let alignment = u64::from(limits.min_storage_buffer_offset_alignment);
        let source_alignment = alignment / gcd(alignment, source_stride);
        let target_alignment = alignment / gcd(alignment, target_stride);
        let maximum = u64::from(limits.max_storage_buffer_binding_size);
        let mut offset = 0_u64;
        let total = source.vertices.len() as u64;
        let mut dispatches = Vec::new();
        let directions = [
            gmpublished_domain::math::Vec3::new(1.0, 0.0, 0.0),
            gmpublished_domain::math::Vec3::new(0.0, 1.0, 0.0),
            gmpublished_domain::math::Vec3::new(0.0, 0.0, 1.0),
        ]
        .map(|axis| axis.rotate_source(placement.angles));
        while offset < total {
            let source_base = offset / source_alignment * source_alignment;
            let absolute_target = u64::from(vertex_start) + offset;
            let target_base = absolute_target / target_alignment * target_alignment;
            let relative_source = offset - source_base;
            let relative_target = absolute_target - target_base;
            let count = (total - offset)
                .min(maximum / source_stride - relative_source)
                .min(maximum / target_stride - relative_target)
                .min(
                    u64::from(limits.max_compute_workgroups_per_dimension)
                        * u64::from(WORKGROUP_SIZE),
                );
            assert!(count != 0, "admitted animation storage window");
            let parameters = Parameters {
                placement: [
                    [
                        directions[0][0] * placement.scale,
                        directions[0][1] * placement.scale,
                        directions[0][2] * placement.scale,
                        0.0,
                    ],
                    [
                        directions[1][0] * placement.scale,
                        directions[1][1] * placement.scale,
                        directions[1][2] * placement.scale,
                        0.0,
                    ],
                    [
                        directions[2][0] * placement.scale,
                        directions[2][1] * placement.scale,
                        directions[2][2] * placement.scale,
                        0.0,
                    ],
                    [
                        placement.origin[0],
                        placement.origin[1],
                        placement.origin[2],
                        1.0,
                    ],
                ],
                direction: directions.map(|axis| [axis[0], axis[1], axis[2], 0.0]),
                source_start: u32::try_from(relative_source).expect("admitted source window"),
                target_start: u32::try_from(relative_target).expect("admitted target window"),
                vertex_count: u32::try_from(count).expect("admitted vertex count"),
                padding: 0,
            };
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.animation.parameters"),
                size: std::mem::size_of::<Parameters>() as u64,
                usage: wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: true,
            });
            uniform
                .slice(..)
                .get_mapped_range_mut()
                .copy_from_slice(bytemuck::bytes_of(&parameters));
            uniform.unmap();
            let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("file_preview.animation.vertices"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer_binding(
                            &source.buffer,
                            source_base * source_stride,
                            (relative_source + count) * source_stride,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: palette.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: buffer_binding(
                            output,
                            target_base * target_stride,
                            (relative_target + count) * target_stride,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: uniform.as_entire_binding(),
                    },
                ],
            });
            dispatches.push(Dispatch {
                binding,
                workgroups: parameters.vertex_count.div_ceil(WORKGROUP_SIZE),
            });
            offset += count;
        }
        dispatches
    }

    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, dispatches: &[Dispatch]) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("file_preview.animation.skin"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        for dispatch in dispatches {
            pass.set_bind_group(0, &dispatch.binding, &[]);
            pass.dispatch_workgroups(dispatch.workgroups, 1, 1);
        }
    }
}

#[derive(Debug)]
pub(in super::super) struct Dispatch {
    binding: wgpu::BindGroup,
    workgroups: u32,
}

const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a
}

fn buffer_binding(buffer: &wgpu::Buffer, offset: u64, size: u64) -> wgpu::BindingResource<'_> {
    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
        buffer,
        offset,
        size: NonZeroU64::new(size),
    })
}
