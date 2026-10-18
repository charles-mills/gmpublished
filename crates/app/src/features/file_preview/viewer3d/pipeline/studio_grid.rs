use iced::wgpu;

use crate::media::preview_model::ModelBounds;

use super::super::{MSAA_SAMPLE_COUNT, half_extent};

pub const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.035,
    g: 0.044,
    b: 0.056,
    a: 1.0,
};

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct StudioGrid {
    view_proj: [[f32; 4]; 4],
    origin_spacing: [f32; 4],
    extent: [f32; 4],
}

impl StudioGrid {
    pub fn new(view_proj: [[f32; 4]; 4], bounds: ModelBounds) -> Self {
        let radius = half_extent(bounds.min, bounds.max).max(1.0);
        let spacing = 16.0;
        let center = bounds.center();
        Self {
            view_proj,
            origin_spacing: [
                center[0],
                center[1],
                bounds.min[2] - radius * 0.0005,
                spacing,
            ],
            extent: [radius * 32.0, radius * 12.0, 0.0, 0.0],
        }
    }
}

#[derive(Debug)]
pub struct StudioGridResources {
    pub uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
}

impl StudioGridResources {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.studio_grid.shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("studio_grid.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.studio_grid.uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<StudioGrid>() as u64
                    ),
                },
                count: None,
            }],
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.studio_grid.uniforms"),
            size: std::mem::size_of::<StudioGrid>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.studio_grid.bind_group"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("file_preview.studio_grid.layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("file_preview.studio_grid.pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: MSAA_SAMPLE_COUNT,
                ..wgpu::MultisampleState::default()
            },
            multiview: None,
            cache: None,
        });
        Self {
            uniform_buffer,
            bind_group,
            pipeline,
        }
    }

    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::file_preview::viewer3d::identity_matrix;
    use gmpublished_domain::math::Vec3;
    use iced::wgpu::naga;

    #[test]
    fn ground_follows_model_bottom_and_keeps_source_units() {
        let grid = StudioGrid::new(
            identity_matrix(),
            ModelBounds {
                min: Vec3::new(10.0, -20.0, 37.0),
                max: Vec3::new(110.0, 40.0, 137.0),
            },
        );
        assert_eq!(&grid.origin_spacing[..2], &[60.0, 10.0]);
        assert!(grid.origin_spacing[2] < 37.0 && grid.origin_spacing[2] > 36.9);
        assert_eq!(grid.origin_spacing[3], 16.0);
    }

    #[test]
    fn studio_shader_matches_its_uploaded_uniform_layout() {
        let source = include_str!("studio_grid.wgsl");
        let module = naga::front::wgsl::parse_str(source).expect("studio grid WGSL");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("studio grid shader validates");
        let uniform = module
            .types
            .iter()
            .find_map(|(_, ty)| (ty.name.as_deref() == Some("StudioGrid")).then_some(&ty.inner))
            .expect("StudioGrid declaration");
        let naga::TypeInner::Struct { members, span } = uniform else {
            panic!("StudioGrid must be a struct");
        };
        assert_eq!(*span as usize, std::mem::size_of::<StudioGrid>());
        let fields = [
            ("view_proj", std::mem::offset_of!(StudioGrid, view_proj)),
            (
                "origin_spacing",
                std::mem::offset_of!(StudioGrid, origin_spacing),
            ),
            ("extent", std::mem::offset_of!(StudioGrid, extent)),
        ];
        assert_eq!(members.len(), fields.len());
        for (member, (name, offset)) in members.iter().zip(fields) {
            assert_eq!(member.name.as_deref(), Some(name));
            assert_eq!(member.offset as usize, offset);
        }
    }
}
