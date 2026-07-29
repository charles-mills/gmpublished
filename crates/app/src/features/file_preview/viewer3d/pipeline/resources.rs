//! GPU objects created once per device and immutable for the pipeline's
//! life: shader modules compiled into the render pipelines, plus the bind
//! group layouts, samplers, shared uniform buffers, and static vertex data
//! that every later-lived stage borrows.

use super::uniforms::{MaterialUniform, Uniforms};
use super::{
    BLIT_SHADER_SOURCE, DETAIL_SHADER_SOURCE, DETAIL_VERTEX_ATTRIBUTES, DETAIL_VERTEX_FLOAT_COUNT,
    MATERIAL_ANISOTROPY_CLAMP, MODEL_VERTEX_ATTRIBUTES, MSAA_SAMPLE_COUNT, ModelVertex,
    SHADER_SOURCE, SKY_SHADER_SOURCE, SKYBOX_FACE_COUNT, SkyboxFace, WATER_SHADER_SOURCE, wgpu,
};

#[derive(Debug)]
pub struct RenderResources {
    pub opaque_pipeline: wgpu::RenderPipeline,
    pub water_pipeline: wgpu::RenderPipeline,
    /// `None` on backends whose shader translation rejects the refractive
    /// water shader (naga's GLSL backend cannot translate `textureLoad` on a
    /// depth texture); water then renders through `water_pipeline` instead.
    pub refractive_water_pipeline: Option<wgpu::RenderPipeline>,
    pub translucent_pipeline: wgpu::RenderPipeline,
    pub additive_pipeline: wgpu::RenderPipeline,
    pub detail_pipeline: wgpu::RenderPipeline,
    pub overlay_opaque_pipeline: wgpu::RenderPipeline,
    pub overlay_translucent_pipeline: wgpu::RenderPipeline,
    pub overlay_additive_pipeline: wgpu::RenderPipeline,
    pub phy_debug_pipeline: wgpu::RenderPipeline,
    pub sky_pipeline: wgpu::RenderPipeline,
    pub blit_pipeline: wgpu::RenderPipeline,
    pub uniform_buffer: wgpu::Buffer,
    pub uniform_bind_group: wgpu::BindGroup,
    pub sky_uniform_buffer: wgpu::Buffer,
    pub sky_uniform_bind_group: wgpu::BindGroup,
    pub map_skybox_uniform_buffer: wgpu::Buffer,
    pub map_skybox_uniform_bind_group: wgpu::BindGroup,
    pub material_layout: wgpu::BindGroupLayout,
    pub water_refraction_layout: wgpu::BindGroupLayout,
    pub sky_layout: wgpu::BindGroupLayout,
    pub blit_layout: wgpu::BindGroupLayout,
    pub material_sampler: wgpu::Sampler,
    pub simple_sampler: wgpu::Sampler,
    pub blit_sampler: wgpu::Sampler,
    pub water_refraction_sampler: wgpu::Sampler,
    pub sky_vertices: wgpu::Buffer,
    pub target_format: wgpu::TextureFormat,
}

impl RenderResources {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        // These shaders run on iced_wgpu's own device, so they must fit the
        // limits it requests (max_bind_groups: 4, max_non_sampler_bindings:
        // 2048). Exceeding them panics here at preview time, not at build time.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SOURCE.into()),
        });
        let detail_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.detail_shader"),
            source: wgpu::ShaderSource::Wgsl(DETAIL_SHADER_SOURCE.into()),
        });
        let water_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.water_shader"),
            source: wgpu::ShaderSource::Wgsl(WATER_SHADER_SOURCE.into()),
        });
        let sky_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.sky_shader"),
            source: wgpu::ShaderSource::Wgsl(SKY_SHADER_SOURCE.into()),
        });
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.blit_shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER_SOURCE.into()),
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.model_viewer.uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Uniforms>() as u64),
                },
                count: None,
            }],
        });

        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.model_viewer.material"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        // Declared, not `None`: this is wgpu's one hook for
                        // catching a shader whose declaration of this uniform
                        // has drifted from the Rust struct, and `None`
                        // declines it. Binding 0 above already declares its
                        // size; this one was the exception.
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<MaterialUniform>() as u64,
                        ),
                    },
                    count: None,
                },
            ],
        });
        let water_refraction_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("file_preview.model_viewer.water_refraction"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: true,
                        },
                        count: None,
                    },
                ],
            });
        let sky_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.model_viewer.sky_material"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.model_viewer.blit_texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("file_preview.model_viewer.layout"),
            bind_group_layouts: &[&uniform_layout, &material_layout],
            push_constant_ranges: &[],
        });
        let refractive_water_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("file_preview.model_viewer.refractive_water_layout"),
                bind_group_layouts: &[&uniform_layout, &material_layout, &water_refraction_layout],
                push_constant_ranges: &[],
            });
        let sky_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("file_preview.model_viewer.sky_layout"),
            bind_group_layouts: &[&uniform_layout, &sky_layout],
            push_constant_ranges: &[],
        });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("file_preview.model_viewer.blit_layout"),
            bind_group_layouts: &[&blit_layout],
            push_constant_ranges: &[],
        });

        let opaque_pipeline = create_model_pipeline(
            device,
            &layout,
            &shader,
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            true,
            "file_preview.model_viewer.opaque_pipeline",
        );
        let translucent_pipeline = create_model_pipeline(
            device,
            &layout,
            &shader,
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            false,
            "file_preview.model_viewer.translucent_pipeline",
        );
        let water_pipeline = create_model_pipeline_with_fragment_entry(
            device,
            &layout,
            &water_shader,
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                write_enabled: false,
                bias: wgpu::DepthBiasState::default(),
                cull_mode: Some(wgpu::Face::Back),
            },
            PipelineShaderEntry {
                fragment: "fs_skybox",
                label: "file_preview.model_viewer.water_pipeline",
            },
        );
        // Some backends fail shader translation for this pipeline (naga's
        // GLSL backend rejects `textureLoad` on a depth texture); capture
        // the error instead of hitting wgpu's panicking uncaptured-error
        // handler, and fall back to non-refractive water.
        device.push_error_scope(wgpu::ErrorFilter::Internal);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let refractive_water_pipeline = create_model_pipeline_with_fragment_entry(
            device,
            &refractive_water_layout,
            &water_shader,
            format,
            None,
            PipelineRasterMode {
                write_enabled: false,
                bias: wgpu::DepthBiasState::default(),
                cull_mode: Some(wgpu::Face::Back),
            },
            PipelineShaderEntry {
                fragment: "fs_main",
                label: "file_preview.model_viewer.refractive_water_pipeline",
            },
        );
        let validation_error = take_scoped_error(device);
        let internal_error = take_scoped_error(device);
        let translation_error = validation_error.or(internal_error);
        if let Some(error) = &translation_error {
            log::warn!(
                "refractive water pipeline unavailable, using non-refractive water: {error}"
            );
        }
        let refractive_water_pipeline = translation_error
            .is_none()
            .then_some(refractive_water_pipeline);
        let additive_pipeline = create_model_pipeline(
            device,
            &layout,
            &shader,
            format,
            Some(additive_blend_state()),
            false,
            "file_preview.model_viewer.additive_pipeline",
        );
        let detail_pipeline = create_detail_pipeline(
            device,
            &layout,
            &detail_shader,
            format,
            "file_preview.model_viewer.detail_pipeline",
        );
        let overlay_opaque_pipeline = create_model_pipeline_with_depth_bias(
            device,
            &layout,
            &shader,
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                write_enabled: false,
                bias: overlay_depth_bias_state(),
                // Overlays stay two-sided: their quad winding comes from
                // the packed overlay basis, not face winding.
                cull_mode: None,
            },
            "file_preview.model_viewer.overlay_opaque_pipeline",
        );
        let overlay_translucent_pipeline = create_model_pipeline_with_depth_bias(
            device,
            &layout,
            &shader,
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                write_enabled: false,
                bias: overlay_depth_bias_state(),
                cull_mode: None,
            },
            "file_preview.model_viewer.overlay_translucent_pipeline",
        );
        let overlay_additive_pipeline = create_model_pipeline_with_depth_bias(
            device,
            &layout,
            &shader,
            format,
            Some(additive_blend_state()),
            PipelineRasterMode {
                write_enabled: false,
                bias: overlay_depth_bias_state(),
                cull_mode: None,
            },
            "file_preview.model_viewer.overlay_additive_pipeline",
        );
        let phy_debug_pipeline = create_model_pipeline_with_depth_bias(
            device,
            &layout,
            &shader,
            format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                write_enabled: false,
                bias: overlay_depth_bias_state(),
                cull_mode: None,
            },
            "file_preview.model_viewer.phy_debug_pipeline",
        );
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("file_preview.model_viewer.sky_pipeline"),
            layout: Some(&sky_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &sky_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: (3 * std::mem::size_of::<f32>()) as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &sky_shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: multisample_state(),
            multiview: None,
            cache: None,
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("file_preview.model_viewer.blit_pipeline"),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let (uniform_buffer, uniform_bind_group) =
            uniforms(device, &uniform_layout, "file_preview.model_viewer.uniform");
        let (sky_uniform_buffer, sky_uniform_bind_group) = uniforms(
            device,
            &uniform_layout,
            "file_preview.model_viewer.sky_uniform",
        );
        let (map_skybox_uniform_buffer, map_skybox_uniform_bind_group) = uniforms(
            device,
            &uniform_layout,
            "file_preview.model_viewer.map_skybox_uniform",
        );

        // The four samplers differ only in the three axes `sampler` takes;
        // spelled out, the differences were the easiest thing in this function
        // to misread.
        let material_sampler = sampler(
            device,
            "file_preview.model_viewer.material_sampler",
            &SamplerSpec {
                address: wgpu::AddressMode::Repeat,
                filter: wgpu::FilterMode::Linear,
                mipmap: wgpu::FilterMode::Linear,
                anisotropy: MATERIAL_ANISOTROPY_CLAMP,
            },
        );
        let simple_sampler = sampler(
            device,
            "file_preview.model_viewer.simple_sampler",
            &SamplerSpec {
                address: wgpu::AddressMode::Repeat,
                filter: wgpu::FilterMode::Linear,
                mipmap: wgpu::FilterMode::Nearest,
                ..SamplerSpec::default()
            },
        );
        let blit_sampler = sampler(
            device,
            "file_preview.model_viewer.blit_sampler",
            &SamplerSpec {
                address: wgpu::AddressMode::ClampToEdge,
                filter: wgpu::FilterMode::Nearest,
                mipmap: wgpu::FilterMode::Nearest,
                ..SamplerSpec::default()
            },
        );
        let water_refraction_sampler = sampler(
            device,
            "file_preview.model_viewer.water_refraction_sampler",
            &SamplerSpec {
                address: wgpu::AddressMode::ClampToEdge,
                filter: wgpu::FilterMode::Linear,
                mipmap: wgpu::FilterMode::Nearest,
                ..SamplerSpec::default()
            },
        );
        let sky_vertex_bytes = skybox_vertex_bytes();
        let sky_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.model_viewer.sky_vertices"),
            size: sky_vertex_bytes.len() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&sky_vertices, 0, &sky_vertex_bytes);

        Self {
            opaque_pipeline,
            water_pipeline,
            refractive_water_pipeline,
            translucent_pipeline,
            additive_pipeline,
            detail_pipeline,
            overlay_opaque_pipeline,
            overlay_translucent_pipeline,
            overlay_additive_pipeline,
            phy_debug_pipeline,
            sky_pipeline,
            blit_pipeline,
            uniform_buffer,
            uniform_bind_group,
            sky_uniform_buffer,
            sky_uniform_bind_group,
            map_skybox_uniform_buffer,
            map_skybox_uniform_bind_group,
            material_layout,
            water_refraction_layout,
            sky_layout,
            blit_layout,
            material_sampler,
            simple_sampler,
            blit_sampler,
            water_refraction_sampler,
            sky_vertices,
            target_format: format,
        }
    }
}

/// Pops the top error scope off `device`. Native wgpu records scoped errors
/// synchronously, so the returned future is already resolved; a single poll
/// retrieves it without blocking.
fn take_scoped_error(device: &wgpu::Device) -> Option<wgpu::Error> {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    let future = std::pin::pin!(device.pop_error_scope());
    match future.poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(error) => error,
        Poll::Pending => None,
    }
}

#[derive(Clone, Copy)]
struct PipelineRasterMode {
    write_enabled: bool,
    bias: wgpu::DepthBiasState,
    cull_mode: Option<wgpu::Face>,
}

#[derive(Clone, Copy)]
struct PipelineShaderEntry {
    fragment: &'static str,
    label: &'static str,
}

/// A uniform buffer sized for [`Uniforms`] and the bind group that reaches it.
///
/// The three uniform slots (model, sky, map skybox) are the same buffer and
/// the same single-entry bind group; only the label distinguishes them, and
/// labels are what a GPU capture shows you.
fn uniforms(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    label: &str,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(&format!("{label}_buffer")),
        size: std::mem::size_of::<Uniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(&format!("{label}_bind_group")),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });

    (buffer, bind_group)
}

/// The axes this viewer's samplers actually vary on.
#[derive(Clone, Copy)]
struct SamplerSpec {
    address: wgpu::AddressMode,
    filter: wgpu::FilterMode,
    mipmap: wgpu::FilterMode,
    anisotropy: u16,
}

impl Default for SamplerSpec {
    fn default() -> Self {
        Self {
            address: wgpu::AddressMode::ClampToEdge,
            filter: wgpu::FilterMode::Nearest,
            mipmap: wgpu::FilterMode::Nearest,
            anisotropy: 1,
        }
    }
}

fn sampler(device: &wgpu::Device, label: &str, spec: &SamplerSpec) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: spec.address,
        address_mode_v: spec.address,
        mag_filter: spec.filter,
        min_filter: spec.filter,
        mipmap_filter: spec.mipmap,
        anisotropy_clamp: spec.anisotropy,
        ..wgpu::SamplerDescriptor::default()
    })
}

fn multisample_state() -> wgpu::MultisampleState {
    wgpu::MultisampleState {
        count: MSAA_SAMPLE_COUNT,
        mask: !0,
        alpha_to_coverage_enabled: false,
    }
}

fn create_model_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    depth_write_enabled: bool,
    label: &'static str,
) -> wgpu::RenderPipeline {
    create_model_pipeline_with_depth_bias(
        device,
        layout,
        shader,
        format,
        blend,
        PipelineRasterMode {
            write_enabled: depth_write_enabled,
            bias: wgpu::DepthBiasState::default(),
            cull_mode: Some(wgpu::Face::Back),
        },
        label,
    )
}

fn create_model_pipeline_with_depth_bias(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    raster: PipelineRasterMode,
    label: &'static str,
) -> wgpu::RenderPipeline {
    create_model_pipeline_with_fragment_entry(
        device,
        layout,
        shader,
        format,
        blend,
        raster,
        PipelineShaderEntry {
            fragment: "fs_main",
            label,
        },
    )
}

fn create_model_pipeline_with_fragment_entry(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    raster: PipelineRasterMode,
    entry: PipelineShaderEntry,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(entry.label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[model_vertex_buffer_layout()],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(entry.fragment),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // Source renders brush/model geometry one-sided with clockwise
            // front faces; without this, up-facing water planes above the
            // camera paint their undersides across the sky.
            front_face: wgpu::FrontFace::Cw,
            cull_mode: raster.cull_mode,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: raster.write_enabled,
            depth_compare: wgpu::CompareFunction::GreaterEqual,
            stencil: wgpu::StencilState::default(),
            bias: raster.bias,
        }),
        multisample: multisample_state(),
        multiview: None,
        cache: None,
    })
}

fn create_detail_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[detail_vertex_buffer_layout()],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::GreaterEqual,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: multisample_state(),
        multiview: None,
        cache: None,
    })
}

fn overlay_depth_bias_state() -> wgpu::DepthBiasState {
    // Depth bias ADDS to the fragment depth. This viewer is reverse-Z
    // (clear 0.0, GreaterEqual, closer = LARGER depth), so pulling a
    // coplanar decal toward the viewer needs a POSITIVE constant; a
    // negative one pushes it behind its wall and loses the depth test.
    //
    // The constant alone is a couple of float ULPs — nowhere near the
    // interpolation divergence between a decal quad and its wall's
    // triangles at oblique angles, so decals flickered in and out with
    // camera movement. Slope-scaled bias grows with the polygon's depth
    // gradient; polygon offset exists precisely for coplanar decals.
    wgpu::DepthBiasState {
        constant: 2,
        slope_scale: 2.0,
        clamp: 0.0,
    }
}

fn model_vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<ModelVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &MODEL_VERTEX_ATTRIBUTES,
    }
}

fn detail_vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: DETAIL_VERTEX_FLOAT_COUNT * std::mem::size_of::<f32>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &DETAIL_VERTEX_ATTRIBUTES,
    }
}

fn additive_blend_state() -> wgpu::BlendState {
    let component = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    wgpu::BlendState {
        color: component,
        alpha: component,
    }
}

fn skybox_vertex_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SKYBOX_FACE_COUNT * 6 * 3 * std::mem::size_of::<f32>());
    for face in SkyboxFace::ALL {
        let corners = skybox_face_corners(face);
        for position in [
            corners[0], corners[1], corners[2], corners[0], corners[2], corners[3],
        ] {
            for component in position {
                bytes.extend_from_slice(&component.to_le_bytes());
            }
        }
    }
    bytes
}

// Valve's 2D skybox suffixes are documented on the Valve Developer Community
// "Skybox (2D)" page; the Source-space corner data below follows
// noclip.website's SourceEngine SkyboxRenderer vertex table:
// https://github.com/magcius/noclip.website/blob/main/src/SourceEngine/Main.ts
pub fn skybox_face_corners(face: SkyboxFace) -> [[f32; 3]; 4] {
    match face {
        SkyboxFace::Rt => [
            [1.0, 1.0, -1.0],
            [1.0, 1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, -1.0, -1.0],
        ],
        SkyboxFace::Lf => [
            [-1.0, -1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [-1.0, 1.0, 1.0],
            [-1.0, 1.0, -1.0],
        ],
        SkyboxFace::Bk => [
            [-1.0, 1.0, -1.0],
            [-1.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, -1.0],
        ],
        SkyboxFace::Ft => [
            [1.0, -1.0, -1.0],
            [1.0, -1.0, 1.0],
            [-1.0, -1.0, 1.0],
            [-1.0, -1.0, -1.0],
        ],
        SkyboxFace::Up => [
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
        ],
        SkyboxFace::Dn => [
            [-1.0, 1.0, -1.0],
            [1.0, 1.0, -1.0],
            [1.0, -1.0, -1.0],
            [-1.0, -1.0, -1.0],
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::{SkyboxFace, skybox_face_corners};

    #[test]
    fn skybox_face_corners_match_source_2d_skybox_convention() {
        // Data source: Valve Developer Community "Skybox (2D)" suffixes,
        // with face orientation from noclip.website's SourceEngine SkyboxRenderer.
        assert_eq!(
            SkyboxFace::ALL.map(skybox_face_corners),
            [
                [
                    [1.0, 1.0, -1.0],
                    [1.0, 1.0, 1.0],
                    [1.0, -1.0, 1.0],
                    [1.0, -1.0, -1.0],
                ],
                [
                    [-1.0, -1.0, -1.0],
                    [-1.0, -1.0, 1.0],
                    [-1.0, 1.0, 1.0],
                    [-1.0, 1.0, -1.0],
                ],
                [
                    [-1.0, 1.0, -1.0],
                    [-1.0, 1.0, 1.0],
                    [1.0, 1.0, 1.0],
                    [1.0, 1.0, -1.0],
                ],
                [
                    [1.0, -1.0, -1.0],
                    [1.0, -1.0, 1.0],
                    [-1.0, -1.0, 1.0],
                    [-1.0, -1.0, -1.0],
                ],
                [
                    [1.0, 1.0, 1.0],
                    [-1.0, 1.0, 1.0],
                    [-1.0, -1.0, 1.0],
                    [1.0, -1.0, 1.0],
                ],
                [
                    [-1.0, 1.0, -1.0],
                    [1.0, 1.0, -1.0],
                    [1.0, -1.0, -1.0],
                    [-1.0, -1.0, -1.0],
                ],
            ]
        );
    }
}
