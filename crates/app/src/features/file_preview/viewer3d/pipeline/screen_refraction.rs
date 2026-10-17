use super::{RenderResources, ViewerScene, wgpu};

#[derive(Debug)]
pub struct Resources {
    pub pipeline: wgpu::RenderPipeline,
    pub layout: wgpu::BindGroupLayout,
    scene_sampler: wgpu::Sampler,
}

#[derive(Debug)]
pub struct Texture {
    pub view: super::texture_upload::UploadedView,
    pub sampler: wgpu::Sampler,
}

#[derive(Debug)]
pub struct Frame {
    texture: std::sync::Arc<Texture>,
    uniform: wgpu::Buffer,
    pub bind_group: wgpu::BindGroup,
    parameters: Uniform,
}

#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    texture: std::sync::Arc<Texture>,
    parameters: Uniform,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniform {
    bump_s: [f32; 4],
    bump_t: [f32; 4],
    tint_amount: [f32; 4],
    params: [f32; 4],
}

impl Resources {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        blit_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.underwater.overlay_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: fragment,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: fragment,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: fragment,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: fragment,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.underwater.overlay_shader"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "{}\n{}",
                    super::BLIT_SHADER_SOURCE,
                    include_str!("../../underwater.wgsl")
                )
                .into(),
            ),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("file_preview.underwater.overlay_pipeline_layout"),
            bind_group_layouts: &[blit_layout, &layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("file_preview.underwater.overlay_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_underwater"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let scene_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("file_preview.underwater.scene_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            scene_sampler,
        }
    }
}

impl Frame {
    fn new(
        device: &wgpu::Device,
        resources: &RenderResources,
        normal: &std::sync::Arc<Texture>,
    ) -> Self {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.underwater.overlay_uniform"),
            size: std::mem::size_of::<Uniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let r = &resources.screen_refraction;
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.underwater.overlay_bind_group"),
            layout: &r.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&normal.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&normal.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&r.scene_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        Self {
            texture: std::sync::Arc::clone(normal),
            uniform,
            bind_group,
            parameters: Uniform::default(),
        }
    }
    fn write(&mut self, queue: &wgpu::Queue, parameters: Uniform) {
        self.parameters = parameters;
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&parameters));
    }
    pub(super) fn snapshot(&self) -> Snapshot {
        Snapshot {
            texture: std::sync::Arc::clone(&self.texture),
            parameters: self.parameters,
        }
    }
}
impl Snapshot {
    pub(super) fn matches(&self, frame: &Frame) -> bool {
        std::sync::Arc::ptr_eq(&self.texture, &frame.texture) && self.parameters == frame.parameters
    }
    pub(super) fn freeze(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
    ) -> Frame {
        let mut frame = Frame::new(device, resources, &self.texture);
        frame.write(queue, self.parameters);
        frame
    }
}

pub fn prepare(
    slot: &mut Option<Frame>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    resources: &RenderResources,
    upload: &ViewerScene,
    eye: super::Vec3,
    water_time: f32,
) {
    let overlay = upload
        .assets
        .map()
        .and_then(|map| map.immersion_material(eye))
        .and_then(|water| water.underwater_overlay.as_deref());
    let Some(effect) = overlay else {
        *slot = None;
        return;
    };
    let Some(normal) = upload
        .bindings
        .background
        .screen_refraction_normals
        .get(&effect.name)
    else {
        *slot = None;
        return;
    };
    if slot
        .as_ref()
        .is_none_or(|frame| !std::sync::Arc::ptr_eq(&frame.texture, normal))
    {
        *slot = Some(Frame::new(device, resources, normal));
    }
    let mut bump = effect.bump_transform;
    for (row, scroll) in bump.iter_mut().zip(effect.proxy_scroll) {
        row[3] += scroll * water_time;
    }
    let data = Uniform {
        bump_s: bump[0],
        bump_t: bump[1],
        tint_amount: [
            effect.tint_linear[0],
            effect.tint_linear[1],
            effect.tint_linear[2],
            effect.refract_amount,
        ],
        params: [
            if effect.normal_map.on_demand().is_some() {
                0.0
            } else {
                effect
                    .bump_frame
                    .floor()
                    .rem_euclid(effect.normal_map.frame_count().max(1) as f32)
            },
            f32::from(effect.blur),
            0.0,
            0.0,
        ],
    };
    slot.as_mut().unwrap().write(queue, data);
}

#[cfg(test)]
mod tests {
    use iced::wgpu::naga;
    #[test]
    fn screen_effect_shader_and_cpu_uniform_layout_agree() {
        let source = format!(
            "{}\n{}",
            super::super::BLIT_SHADER_SOURCE,
            include_str!("../../underwater.wgsl")
        );
        let module = naga::front::wgsl::parse_str(&source).unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        let (_, layout) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("ScreenRefraction"))
            .unwrap();
        let naga::TypeInner::Struct { members, span } = &layout.inner else {
            panic!("uniform must be a struct");
        };
        assert_eq!(*span as usize, std::mem::size_of::<super::Uniform>());
        assert_eq!(
            members.iter().map(|m| m.offset).collect::<Vec<_>>(),
            [0, 16, 32, 48]
        );
    }
}
