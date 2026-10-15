use super::uniforms::{MaterialUniform, Uniforms};
use super::{
    BLIT_SHADER_SOURCE, DETAIL_SHADER_SOURCE, DETAIL_VERTEX_ATTRIBUTES, DETAIL_VERTEX_FLOAT_COUNT,
    MATERIAL_ANISOTROPY_CLAMP, MODEL_VERTEX_ATTRIBUTES, ModelVertex, SHADER_SOURCE,
    SKY_SHADER_SOURCE, SKYBOX_FACE_COUNT, SkyboxFace, WATER_SHADER_SOURCE, scene_sample_count,
    wgpu,
};
use gmpublished_domain::math::Vec3;

#[derive(Debug)]
pub struct RenderResources {
    pub sample_count: u32,
    optical: Option<Box<Self>>,
    model_shader: wgpu::ShaderModule,
    model_layout: wgpu::PipelineLayout,
    pub surface_white: wgpu::TextureView,
    pub surface_black_cube: wgpu::TextureView,
    pub sun: super::sun::gpu::Resources,
    pub screen_refraction: super::screen_refraction::Resources,
    pub water_near_intersection_pipeline: wgpu::RenderPipeline,
    pub refracted_opaque_pipeline: wgpu::RenderPipeline,
    pub refracted_translucent_pipeline: wgpu::RenderPipeline,
    pub refracted_additive_pipeline: wgpu::RenderPipeline,
    pub refracted_detail_pipeline: wgpu::RenderPipeline,
    pub refracted_overlay_opaque_pipeline: wgpu::RenderPipeline,
    pub refracted_overlay_translucent_pipeline: wgpu::RenderPipeline,
    pub refracted_overlay_additive_pipeline: wgpu::RenderPipeline,
    pub refracted_planar_pipeline: wgpu::RenderPipeline,

    pub reflected_opaque_pipeline: wgpu::RenderPipeline,
    pub reflected_translucent_pipeline: wgpu::RenderPipeline,
    pub reflected_additive_pipeline: wgpu::RenderPipeline,
    pub uniform_layout: wgpu::BindGroupLayout,
    pub opaque_pipeline: wgpu::RenderPipeline,
    pub water_pipeline: wgpu::RenderPipeline,
    pub reflected_water_pipeline: wgpu::RenderPipeline,
    /// None when shader translation fails; use the single-pass water pipeline instead.
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
    pub material_layout: wgpu::BindGroupLayout,
    pub water_frame_layout: wgpu::BindGroupLayout,
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
        let mut main = Self::new_for_samples(device, queue, format, scene_sample_count(), None);
        if main.sample_count > 1 {
            main.optical = Some(Box::new(Self::new_for_samples(
                device,
                queue,
                format,
                1,
                Some(&main),
            )));
        }
        main
    }

    /// Planar scene attachments are single-sample independently of main AA.
    pub fn optical(&self) -> &Self {
        self.optical.as_deref().unwrap_or(self)
    }

    pub(super) fn for_view(&self, view: super::draw::ViewKind) -> &Self {
        match view {
            super::draw::ViewKind::Main => self,
            super::draw::ViewKind::Reflection | super::draw::ViewKind::Refraction => self.optical(),
        }
    }

    fn new_for_samples(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        sample_count: u32,
        shared: Option<&Self>,
    ) -> Self {
        let target = PipelineTarget {
            format,
            sample_count,
        };
        // These shaders run on iced_wgpu's own device, so they must fit the
        // limits it requests (max_bind_groups: 2, max_non_sampler_bindings:
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

        let uniform_layout = shared.map_or_else(
            || {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("file_preview.model_viewer.uniforms"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: wgpu::BufferSize::new(
                                    std::mem::size_of::<Uniforms>() as u64,
                                ),
                            },
                            count: None,
                        },
                        lightmap_layout_entry(5),
                        lightmap_layout_entry(6),
                        wgpu::BindGroupLayoutEntry {
                            binding: 7,
                            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Storage { read_only: true },
                                has_dynamic_offset: false,
                                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                                    super::model_lighting::GpuModelContext,
                                >(
                                )
                                    as u64),
                            },
                            count: None,
                        },
                    ],
                })
            },
            |resources| resources.uniform_layout.clone(),
        );

        let material_layout = shared.map_or_else(
            || {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
                                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                                    MaterialUniform,
                                >(
                                )
                                    as u64),
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 6,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2Array,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 7,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2Array,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 8,
                            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                                    super::surface::SurfaceUniform,
                                >(
                                )
                                    as u64),
                            },
                            count: None,
                        },
                        surface_texture_layout(9),
                        surface_texture_layout(10),
                        surface_texture_layout(11),
                        wgpu::BindGroupLayoutEntry {
                            binding: 12,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::Cube,
                                multisampled: false,
                            },
                            count: None,
                        },
                        surface_texture_layout(13),
                        wgpu::BindGroupLayoutEntry {
                            binding: 14,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 16,
                            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                                    super::model_lighting::ModelMaterialUniform,
                                >(
                                )
                                    as u64),
                            },
                            count: None,
                        },
                        surface_texture_layout(17),
                        surface_texture_layout(18),
                        surface_texture_layout(19),
                        wgpu::BindGroupLayoutEntry {
                            binding: 20,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                })
            },
            |resources| resources.material_layout.clone(),
        );
        // Uniforms and refraction inputs share one group so the refractive
        // water pipeline fits iced_wgpu's max_bind_groups = 2 alongside the
        // per-material group.
        let water_frame_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("file_preview.model_viewer.water_frame"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(
                                std::mem::size_of::<Uniforms>() as u64,
                            ),
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
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    lightmap_layout_entry(5),
                    lightmap_layout_entry(6),
                ],
            });
        let sky_layout = shared.map_or_else(
            || {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                                    super::sky::SkyUniform,
                                >(
                                )
                                    as u64),
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                })
            },
            |resources| resources.sky_layout.clone(),
        );
        let blit_layout = shared.map_or_else(
            || {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("file_preview.model_viewer.blit_texture"),
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
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                            count: None,
                        },
                    ],
                })
            },
            |resources| resources.blit_layout.clone(),
        );

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("file_preview.model_viewer.layout"),
            bind_group_layouts: &[&uniform_layout, &material_layout],
            push_constant_ranges: &[],
        });
        let refractive_water_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("file_preview.model_viewer.refractive_water_layout"),
                bind_group_layouts: &[&water_frame_layout, &material_layout],
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
            target,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            true,
            "file_preview.model_viewer.opaque_pipeline",
        );
        let translucent_pipeline = create_model_pipeline(
            device,
            &layout,
            &shader,
            target,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            false,
            "file_preview.model_viewer.translucent_pipeline",
        );
        let water_pipeline = create_model_pipeline_with_fragment_entry(
            device,
            &layout,
            &water_shader,
            target,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                ignore_z: false,
                write_enabled: false,
                bias: wgpu::DepthBiasState::default(),
                cull_mode: Some(wgpu::Face::Back),
                front_face: wgpu::FrontFace::Cw,
            },
            PipelineShaderEntry {
                vertex: "vs_main",
                write_mask: wgpu::ColorWrites::ALL,
                fragment: "fs_skybox",
                label: "file_preview.model_viewer.water_pipeline",
            },
        );
        let reflected_water_pipeline = create_model_pipeline_with_fragment_entry(
            device,
            &layout,
            &water_shader,
            target,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                ignore_z: false,
                write_enabled: false,
                bias: wgpu::DepthBiasState::default(),
                cull_mode: Some(wgpu::Face::Back),
                front_face: wgpu::FrontFace::Ccw,
            },
            PipelineShaderEntry {
                vertex: "vs_main",
                write_mask: wgpu::ColorWrites::ALL,
                fragment: "fs_skybox",
                label: "file_preview.reflected.planar_fallback",
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
            target,
            None,
            PipelineRasterMode {
                ignore_z: false,
                write_enabled: true,
                bias: wgpu::DepthBiasState::default(),
                cull_mode: Some(wgpu::Face::Back),
                front_face: wgpu::FrontFace::Cw,
            },
            PipelineShaderEntry {
                vertex: "vs_main",
                write_mask: wgpu::ColorWrites::ALL,
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
        let water_near_intersection_pipeline = create_model_pipeline_with_fragment_entry(
            device,
            &refractive_water_layout,
            &water_shader,
            target,
            None,
            PipelineRasterMode {
                ignore_z: false,
                write_enabled: true,
                bias: wgpu::DepthBiasState::default(),
                cull_mode: Some(wgpu::Face::Back),
                front_face: wgpu::FrontFace::Cw,
            },
            PipelineShaderEntry {
                vertex: "vs_near_main",
                fragment: "fs_near_intersection",
                label: "file_preview.water.near_intersection",
                write_mask: wgpu::ColorWrites::ALL,
            },
        );
        let refractive_water_pipeline = translation_error
            .is_none()
            .then_some(refractive_water_pipeline);
        let additive_pipeline = create_model_pipeline(
            device,
            &layout,
            &shader,
            target,
            Some(additive_blend_state()),
            false,
            "file_preview.model_viewer.additive_pipeline",
        );
        let detail_pipeline = create_detail_pipeline(
            device,
            &layout,
            &detail_shader,
            target,
            "file_preview.model_viewer.detail_pipeline",
            wgpu::ColorWrites::ALL,
        );
        let overlay_opaque_pipeline = create_model_pipeline_with_depth_bias(
            device,
            &layout,
            &shader,
            target,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                ignore_z: false,
                write_enabled: false,
                bias: overlay_depth_bias_state(),
                // Overlays stay two-sided: their quad winding comes from
                // the packed overlay basis, not face winding.
                cull_mode: None,
                front_face: wgpu::FrontFace::Cw,
            },
            "file_preview.model_viewer.overlay_opaque_pipeline",
        );
        let overlay_translucent_pipeline = create_model_pipeline_with_depth_bias(
            device,
            &layout,
            &shader,
            target,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                ignore_z: false,
                write_enabled: false,
                bias: overlay_depth_bias_state(),
                cull_mode: None,
                front_face: wgpu::FrontFace::Cw,
            },
            "file_preview.model_viewer.overlay_translucent_pipeline",
        );
        let overlay_additive_pipeline = create_model_pipeline_with_depth_bias(
            device,
            &layout,
            &shader,
            target,
            Some(additive_blend_state()),
            PipelineRasterMode {
                ignore_z: false,
                write_enabled: false,
                bias: overlay_depth_bias_state(),
                cull_mode: None,
                front_face: wgpu::FrontFace::Cw,
            },
            "file_preview.model_viewer.overlay_additive_pipeline",
        );
        let phy_debug_pipeline = create_model_pipeline_with_depth_bias(
            device,
            &layout,
            &shader,
            target,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            PipelineRasterMode {
                ignore_z: false,
                write_enabled: false,
                bias: overlay_depth_bias_state(),
                cull_mode: None,
                front_face: wgpu::FrontFace::Cw,
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
            multisample: multisample_state(sample_count),
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

        let reflected = |write_enabled, blend, label| {
            create_model_pipeline_with_depth_bias(
                device,
                &layout,
                &shader,
                target,
                blend,
                PipelineRasterMode {
                    ignore_z: false,
                    write_enabled,
                    bias: wgpu::DepthBiasState::default(),
                    cull_mode: Some(wgpu::Face::Back),
                    front_face: wgpu::FrontFace::Ccw,
                },
                label,
            )
        };
        let reflected_opaque_pipeline = reflected(
            true,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            "file_preview.reflected.opaque",
        );
        let reflected_translucent_pipeline = reflected(
            false,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            "file_preview.reflected.translucent",
        );
        let reflected_additive_pipeline = reflected(
            false,
            Some(additive_blend_state()),
            "file_preview.reflected.additive",
        );

        let refraction_pipeline =
            |fragment, write_enabled, blend, overlay, write_mask, label, water| {
                create_model_pipeline_with_fragment_entry(
                    device,
                    &layout,
                    if water { &water_shader } else { &shader },
                    target,
                    blend,
                    PipelineRasterMode {
                        ignore_z: false,
                        write_enabled,
                        bias: if overlay {
                            overlay_depth_bias_state()
                        } else {
                            wgpu::DepthBiasState::default()
                        },
                        cull_mode: if overlay {
                            None
                        } else {
                            Some(wgpu::Face::Back)
                        },
                        front_face: wgpu::FrontFace::Cw,
                    },
                    PipelineShaderEntry {
                        vertex: "vs_main",
                        fragment,
                        label,
                        write_mask,
                    },
                )
            };
        let refracted_opaque_pipeline = refraction_pipeline(
            "fs_refraction",
            true,
            None,
            false,
            wgpu::ColorWrites::ALL,
            "file_preview.refracted.opaque",
            false,
        );
        let refracted_translucent_pipeline = refraction_pipeline(
            "fs_main",
            false,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            false,
            wgpu::ColorWrites::COLOR,
            "file_preview.refracted.translucent",
            false,
        );
        let refracted_additive_pipeline = refraction_pipeline(
            "fs_additive",
            false,
            Some(additive_blend_state()),
            false,
            wgpu::ColorWrites::COLOR,
            "file_preview.refracted.additive",
            false,
        );
        let refracted_overlay_opaque_pipeline = refraction_pipeline(
            "fs_main",
            false,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            true,
            wgpu::ColorWrites::COLOR,
            "file_preview.refracted.overlay_opaque",
            false,
        );
        let refracted_overlay_translucent_pipeline = refraction_pipeline(
            "fs_main",
            false,
            Some(wgpu::BlendState::ALPHA_BLENDING),
            true,
            wgpu::ColorWrites::COLOR,
            "file_preview.refracted.overlay_translucent",
            false,
        );
        let refracted_overlay_additive_pipeline = refraction_pipeline(
            "fs_additive",
            false,
            Some(additive_blend_state()),
            true,
            wgpu::ColorWrites::COLOR,
            "file_preview.refracted.overlay_additive",
            false,
        );
        let refracted_planar_pipeline = refraction_pipeline(
            "fs_skybox",
            true,
            None,
            false,
            wgpu::ColorWrites::ALL,
            "file_preview.refracted.planar_fallback",
            true,
        );
        let refracted_detail_pipeline = create_detail_pipeline(
            device,
            &layout,
            &detail_shader,
            target,
            "file_preview.refracted.detail",
            wgpu::ColorWrites::COLOR,
        );

        let material_sampler = shared.map_or_else(
            || {
                sampler(
                    device,
                    "file_preview.model_viewer.material_sampler",
                    &SamplerSpec {
                        address: wgpu::AddressMode::Repeat,
                        filter: wgpu::FilterMode::Linear,
                        mipmap: wgpu::FilterMode::Linear,
                        anisotropy: MATERIAL_ANISOTROPY_CLAMP,
                    },
                )
            },
            |resources| resources.material_sampler.clone(),
        );
        let simple_sampler = shared.map_or_else(
            || {
                sampler(
                    device,
                    "file_preview.model_viewer.simple_sampler",
                    &SamplerSpec {
                        address: wgpu::AddressMode::Repeat,
                        filter: wgpu::FilterMode::Linear,
                        mipmap: wgpu::FilterMode::Nearest,
                        ..SamplerSpec::default()
                    },
                )
            },
            |resources| resources.simple_sampler.clone(),
        );
        let blit_sampler = shared.map_or_else(
            || {
                sampler(
                    device,
                    "file_preview.model_viewer.blit_sampler",
                    &SamplerSpec {
                        address: wgpu::AddressMode::ClampToEdge,
                        filter: wgpu::FilterMode::Nearest,
                        mipmap: wgpu::FilterMode::Nearest,
                        ..SamplerSpec::default()
                    },
                )
            },
            |resources| resources.blit_sampler.clone(),
        );
        let water_refraction_sampler = shared.map_or_else(
            || {
                sampler(
                    device,
                    "file_preview.model_viewer.water_refraction_sampler",
                    &SamplerSpec {
                        address: wgpu::AddressMode::ClampToEdge,
                        filter: wgpu::FilterMode::Linear,
                        mipmap: wgpu::FilterMode::Nearest,
                        ..SamplerSpec::default()
                    },
                )
            },
            |resources| resources.water_refraction_sampler.clone(),
        );
        let screen_refraction =
            super::screen_refraction::Resources::new(device, format, &blit_layout);
        let sky_vertex_bytes = skybox_vertex_bytes();
        let sky_vertices = shared.map_or_else(
            || {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("file_preview.model_viewer.sky_vertices"),
                    size: sky_vertex_bytes.len() as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            },
            |resources| resources.sky_vertices.clone(),
        );
        queue.write_buffer(&sky_vertices, 0, &sky_vertex_bytes);

        let sun = super::sun::gpu::Resources::new(device, format, &material_layout, sample_count);
        let surface_white = shared.map_or_else(
            || {
                Self::create_texture_frames_view(
                    device,
                    queue,
                    &[vec![crate::bridge::materials::ResolvedTextureMip {
                        rgba: vec![255; 4],
                        width: 1,
                        height: 1,
                    }]],
                    wgpu::TextureFormat::Rgba8Unorm,
                )
            },
            |resources| resources.surface_white.clone(),
        );
        let surface_black_cube = shared.map_or_else(
            || super::cubemap::create_view(device, queue, None),
            |resources| resources.surface_black_cube.clone(),
        );
        Self {
            sample_count,
            optical: None,
            model_shader: shader,
            model_layout: layout,
            surface_white,
            surface_black_cube,
            sun,
            screen_refraction,
            water_near_intersection_pipeline,
            refracted_opaque_pipeline,
            refracted_translucent_pipeline,
            refracted_additive_pipeline,
            refracted_detail_pipeline,
            refracted_overlay_opaque_pipeline,
            refracted_overlay_translucent_pipeline,
            refracted_overlay_additive_pipeline,
            refracted_planar_pipeline,
            reflected_opaque_pipeline,
            reflected_translucent_pipeline,
            reflected_additive_pipeline,
            uniform_layout,
            opaque_pipeline,
            water_pipeline,
            reflected_water_pipeline,
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
            material_layout,
            water_frame_layout,
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

fn surface_texture_layout(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

/// Native wgpu resolves scoped errors synchronously, so one poll is enough.
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
struct PipelineTarget {
    format: wgpu::TextureFormat,
    sample_count: u32,
}

#[derive(Clone, Copy)]
struct PipelineRasterMode {
    ignore_z: bool,
    write_enabled: bool,
    bias: wgpu::DepthBiasState,
    cull_mode: Option<wgpu::Face>,
    front_face: wgpu::FrontFace,
}

impl RenderResources {
    pub(super) fn create_surface_variant(
        &self,
        device: &wgpu::Device,
        key: super::surface_pipelines::SurfacePipelineKey,
    ) -> wgpu::RenderPipeline {
        use super::{draw::ViewKind, surface_pipelines::SurfaceBlend};
        let refracted_opaque =
            key.view == ViewKind::Refraction && !key.overlay && key.blend == SurfaceBlend::Opaque;
        let blend = if refracted_opaque {
            None
        } else {
            Some(match key.blend {
                SurfaceBlend::Additive => additive_blend_state(),
                SurfaceBlend::Opaque | SurfaceBlend::Alpha => wgpu::BlendState::ALPHA_BLENDING,
            })
        };
        create_model_pipeline_with_fragment_entry(
            device,
            &self.model_layout,
            &self.model_shader,
            PipelineTarget {
                format: self.target_format,
                sample_count: self.sample_count,
            },
            blend,
            PipelineRasterMode {
                ignore_z: key.raster.ignore_z,
                write_enabled: key.blend == SurfaceBlend::Opaque && !key.overlay,
                bias: if key.overlay {
                    overlay_depth_bias_state()
                } else {
                    wgpu::DepthBiasState::default()
                },
                cull_mode: if key.overlay || key.raster.no_cull {
                    None
                } else {
                    Some(wgpu::Face::Back)
                },
                front_face: if key.view == ViewKind::Reflection {
                    wgpu::FrontFace::Ccw
                } else {
                    wgpu::FrontFace::Cw
                },
            },
            PipelineShaderEntry {
                vertex: "vs_main",
                fragment: if refracted_opaque {
                    "fs_refraction"
                } else if key.blend == SurfaceBlend::Additive {
                    "fs_additive"
                } else {
                    "fs_main"
                },
                write_mask: if key.view == ViewKind::Refraction && !refracted_opaque {
                    wgpu::ColorWrites::COLOR
                } else {
                    wgpu::ColorWrites::ALL
                },
                label: "file_preview.surface.variant",
            },
        )
    }
}

#[derive(Clone, Copy)]
struct PipelineShaderEntry {
    vertex: &'static str,
    write_mask: wgpu::ColorWrites,
    fragment: &'static str,
    label: &'static str,
}

pub fn uniforms(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    label: &str,
    lighting: &super::lighting::Frame,
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
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(&lighting.lightmap.view),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::TextureView(&lighting.lightmap.raw_view),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: lighting.model_lights.as_entire_binding(),
            },
        ],
    });

    (buffer, bind_group)
}

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

fn multisample_state(sample_count: u32) -> wgpu::MultisampleState {
    wgpu::MultisampleState {
        count: sample_count,
        mask: !0,
        alpha_to_coverage_enabled: false,
    }
}

fn create_model_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    target: PipelineTarget,
    blend: Option<wgpu::BlendState>,
    depth_write_enabled: bool,
    label: &'static str,
) -> wgpu::RenderPipeline {
    create_model_pipeline_with_depth_bias(
        device,
        layout,
        shader,
        target,
        blend,
        PipelineRasterMode {
            ignore_z: false,
            write_enabled: depth_write_enabled,
            bias: wgpu::DepthBiasState::default(),
            cull_mode: Some(wgpu::Face::Back),
            front_face: wgpu::FrontFace::Cw,
        },
        label,
    )
}

fn create_model_pipeline_with_depth_bias(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    target: PipelineTarget,
    blend: Option<wgpu::BlendState>,
    raster: PipelineRasterMode,
    label: &'static str,
) -> wgpu::RenderPipeline {
    create_model_pipeline_with_fragment_entry(
        device,
        layout,
        shader,
        target,
        blend,
        raster,
        PipelineShaderEntry {
            vertex: "vs_main",
            write_mask: wgpu::ColorWrites::ALL,
            fragment: if blend == Some(additive_blend_state()) {
                "fs_additive"
            } else {
                "fs_main"
            },
            label,
        },
    )
}

fn create_model_pipeline_with_fragment_entry(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    target: PipelineTarget,
    blend: Option<wgpu::BlendState>,
    raster: PipelineRasterMode,
    entry: PipelineShaderEntry,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(entry.label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(entry.vertex),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[model_vertex_buffer_layout()],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(entry.fragment),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target.format,
                blend,
                write_mask: entry.write_mask,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // Source renders brush/model geometry one-sided with clockwise
            // front faces; without this, up-facing water planes above the
            // camera paint their undersides across the sky.
            front_face: raster.front_face,
            cull_mode: raster.cull_mode,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: raster.write_enabled && !raster.ignore_z,
            depth_compare: if raster.ignore_z {
                wgpu::CompareFunction::Always
            } else {
                wgpu::CompareFunction::GreaterEqual
            },
            stencil: wgpu::StencilState::default(),
            bias: raster.bias,
        }),
        multisample: multisample_state(target.sample_count),
        multiview: None,
        cache: None,
    })
}

fn create_detail_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    target: PipelineTarget,
    label: &'static str,
    write_mask: wgpu::ColorWrites,
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
                format: target.format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask,
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
        multisample: multisample_state(target.sample_count),
        multiview: None,
        cache: None,
    })
}

fn overlay_depth_bias_state() -> wgpu::DepthBiasState {
    // Reverse-Z needs positive depth bias to pull decals toward the viewer.
    // Slope bias covers interpolation differences at oblique angles.
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
pub fn skybox_face_corners(face: SkyboxFace) -> [Vec3; 4] {
    match face {
        SkyboxFace::Rt => [
            Vec3::new(1.0, 1.0, -1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(1.0, -1.0, -1.0),
        ],
        SkyboxFace::Lf => [
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
            Vec3::new(-1.0, 1.0, -1.0),
        ],
        SkyboxFace::Bk => [
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(-1.0, 1.0, 1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(1.0, 1.0, -1.0),
        ],
        SkyboxFace::Ft => [
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(-1.0, -1.0, -1.0),
        ],
        SkyboxFace::Up => [
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(1.0, -1.0, 1.0),
        ],
        SkyboxFace::Dn => [
            Vec3::new(-1.0, 1.0, -1.0),
            Vec3::new(1.0, 1.0, -1.0),
            Vec3::new(1.0, -1.0, -1.0),
            Vec3::new(-1.0, -1.0, -1.0),
        ],
    }
}

fn lightmap_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{SkyboxFace, Vec3, skybox_face_corners};

    #[test]
    fn skybox_face_corners_match_source_2d_skybox_convention() {
        // Data source: Valve Developer Community "Skybox (2D)" suffixes,
        // with face orientation from noclip.website's SourceEngine SkyboxRenderer.
        assert_eq!(
            SkyboxFace::ALL.map(skybox_face_corners),
            [
                [
                    Vec3::new(1.0, 1.0, -1.0),
                    Vec3::new(1.0, 1.0, 1.0),
                    Vec3::new(1.0, -1.0, 1.0),
                    Vec3::new(1.0, -1.0, -1.0),
                ],
                [
                    Vec3::new(-1.0, -1.0, -1.0),
                    Vec3::new(-1.0, -1.0, 1.0),
                    Vec3::new(-1.0, 1.0, 1.0),
                    Vec3::new(-1.0, 1.0, -1.0),
                ],
                [
                    Vec3::new(-1.0, 1.0, -1.0),
                    Vec3::new(-1.0, 1.0, 1.0),
                    Vec3::new(1.0, 1.0, 1.0),
                    Vec3::new(1.0, 1.0, -1.0),
                ],
                [
                    Vec3::new(1.0, -1.0, -1.0),
                    Vec3::new(1.0, -1.0, 1.0),
                    Vec3::new(-1.0, -1.0, 1.0),
                    Vec3::new(-1.0, -1.0, -1.0),
                ],
                [
                    Vec3::new(1.0, 1.0, 1.0),
                    Vec3::new(-1.0, 1.0, 1.0),
                    Vec3::new(-1.0, -1.0, 1.0),
                    Vec3::new(1.0, -1.0, 1.0),
                ],
                [
                    Vec3::new(-1.0, 1.0, -1.0),
                    Vec3::new(1.0, 1.0, -1.0),
                    Vec3::new(1.0, -1.0, -1.0),
                    Vec3::new(-1.0, -1.0, -1.0),
                ],
            ]
        );
    }
}
