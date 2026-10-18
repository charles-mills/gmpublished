use super::uniforms::{MaterialUniform, Uniforms};
use super::{
    BLIT_SHADER_SOURCE, DETAIL_SHADER_SOURCE, DETAIL_VERTEX_ATTRIBUTES, DETAIL_VERTEX_FLOAT_COUNT,
    MODEL_VERTEX_ATTRIBUTES, ModelVertex, SHADER_SOURCE, SKY_SHADER_SOURCE, SKYBOX_FACE_COUNT,
    SkyboxFace, WATER_SHADER_SOURCE, scene_sample_count, wgpu,
};
use gmpublished_domain::math::Vec3;
use std::{ops::Deref, sync::Arc};

#[derive(Debug)]
pub struct RenderResources {
    shared: Arc<SharedResources>,
    main: ViewPipelines,
    reflection: ViewPipelines,
    refraction: ViewPipelines,
    pub sun: super::sun::gpu::Resources,
    pub reflection_sun: super::sun::gpu::Resources,
    pub refractive_water_pipeline: Option<wgpu::RenderPipeline>,
    pub water_near_intersection_pipeline: wgpu::RenderPipeline,
    pub phy_debug_pipeline: wgpu::RenderPipeline,
}

#[derive(Debug)]
pub struct SharedResources {
    #[cfg(any(test, feature = "map-capture"))]
    pub(super) clip_mode: super::clipping::ClipMode,
    pub(super) texture_uploader: super::texture_upload::SharedUploader,
    pub(super) skinning: std::sync::OnceLock<super::skinning::Resources>,
    model_shader: wgpu::ShaderModule,
    detail_shader: wgpu::ShaderModule,
    water_shader: wgpu::ShaderModule,
    sky_shader: wgpu::ShaderModule,
    model_layout: wgpu::PipelineLayout,
    refractive_water_layout: wgpu::PipelineLayout,
    sky_pipeline_layout: wgpu::PipelineLayout,
    _fallback_allocations: Vec<super::texture_upload::UploadedTexture>,
    pub surface_white: super::texture_upload::UploadedView,
    pub flat_normal: super::texture_upload::UploadedView,
    pub error_texture: super::texture_upload::UploadedView,
    pub phy_debug_texture: super::texture_upload::UploadedView,
    pub sky_black_texture: super::texture_upload::UploadedView,
    pub surface_black_cube: super::texture_upload::UploadedView,
    pub screen_refraction: super::screen_refraction::Resources,
    pub uniform_layout: wgpu::BindGroupLayout,
    pub material_layout: wgpu::BindGroupLayout,
    pub water_material_layout: wgpu::BindGroupLayout,
    pub water_frame_layout: wgpu::BindGroupLayout,
    pub lightmap_layout: wgpu::BindGroupLayout,
    pub(super) default_lightmap: Arc<super::lightmaps::Page>,
    pub sky_layout: wgpu::BindGroupLayout,
    pub blit_layout: wgpu::BindGroupLayout,
    pub(super) samplers: super::samplers::SamplerCache,
    pub(super) sampler_quality: super::samplers::QualityProfile,
    pub blit_sampler: wgpu::Sampler,
    pub water_refraction_sampler: wgpu::Sampler,
    pub sky_vertices: wgpu::Buffer,
    pub blit_pipeline: wgpu::RenderPipeline,
    pub target_format: wgpu::TextureFormat,
}

#[derive(Debug)]
pub struct ViewPipelines {
    kind: super::draw::ViewKind,
    shared: Arc<SharedResources>,
    pub sample_count: u32,
    pub opaque_pipeline: wgpu::RenderPipeline,
    pub translucent_pipeline: wgpu::RenderPipeline,
    pub additive_pipeline: wgpu::RenderPipeline,
    pub modulate_pipeline: wgpu::RenderPipeline,
    pub overlay_opaque_pipeline: wgpu::RenderPipeline,
    pub overlay_translucent_pipeline: wgpu::RenderPipeline,
    pub overlay_additive_pipeline: wgpu::RenderPipeline,
    pub overlay_modulate_pipeline: wgpu::RenderPipeline,
    pub detail_pipeline: wgpu::RenderPipeline,
    pub planar_pipeline: wgpu::RenderPipeline,
    pub sky_pipeline: wgpu::RenderPipeline,
}

impl Deref for RenderResources {
    type Target = SharedResources;
    fn deref(&self) -> &Self::Target {
        &self.shared
    }
}
impl Deref for ViewPipelines {
    type Target = SharedResources;
    fn deref(&self) -> &Self::Target {
        &self.shared
    }
}

impl SharedResources {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        capabilities: Option<&wgpu::DownlevelCapabilities>,
    ) -> Self {
        let clip_mode = super::clipping::ClipMode::for_device(device);
        let shader_source = |source, kind| {
            super::clipping::shader(source, kind, clip_mode)
                .expect("maintained viewer shader interface")
        };
        let texture_uploader = super::texture_upload::TextureUploader::shared(
            device,
            queue,
            super::texture_upload::configured_policy(),
        )
        .expect("validated device texture upload policy");
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.shader"),
            source: wgpu::ShaderSource::Wgsl(
                shader_source(SHADER_SOURCE, super::clipping::ShaderKind::Model).into(),
            ),
        });
        let detail_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.detail_shader"),
            source: wgpu::ShaderSource::Wgsl(
                shader_source(DETAIL_SHADER_SOURCE, super::clipping::ShaderKind::Detail).into(),
            ),
        });
        let water_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("file_preview.model_viewer.water_shader"),
            source: wgpu::ShaderSource::Wgsl(
                shader_source(WATER_SHADER_SOURCE, super::clipping::ShaderKind::Water).into(),
            ),
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
            entries: &uniform_layout_entries(),
        });

        let lightmap_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.lightmap.page"),
            entries: &lightmap_layout_entries(),
        });
        let material_entries = material_layout_entries();
        let water_material_entries = material_entries
            .iter()
            .copied()
            .filter(|entry| water_material_binding(entry.binding))
            .collect::<Vec<_>>();
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("file_preview.model_viewer.material"),
            entries: &material_entries,
        });
        let water_material_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("file_preview.water.material"),
                entries: &water_material_entries,
            });
        let water_frame_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("file_preview.model_viewer.water_frame"),
                entries: &water_frame_layout_entries(),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                            super::sky::SkyUniform,
                        >() as u64),
                    },
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
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("file_preview.model_viewer.layout"),
            bind_group_layouts: &[&uniform_layout, &material_layout, &lightmap_layout],
            push_constant_ranges: &[],
        });
        let refractive_water_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("file_preview.model_viewer.refractive_water_layout"),
                bind_group_layouts: &[
                    &water_frame_layout,
                    &water_material_layout,
                    &lightmap_layout,
                ],
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
        let samplers = super::samplers::SamplerCache::new(
            device,
            capabilities.is_some_and(|caps| {
                caps.flags
                    .contains(wgpu::DownlevelFlags::ANISOTROPIC_FILTERING)
            }),
        );
        let sampler_quality = super::samplers::quality_profile();
        let lightmap_sampler = sampler(
            device,
            "file_preview.model_viewer.lightmap_sampler",
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
        let screen_refraction =
            super::screen_refraction::Resources::new(device, format, &blit_layout);
        let sky_vertex_bytes = skybox_vertex_bytes();
        let sky_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.model_viewer.sky_vertices"),
            size: sky_vertex_bytes.len() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&sky_vertices, 0, &sky_vertex_bytes);

        let (surface_white, surface_white_allocation) = RenderResources::create_texture_frames_view(
            device,
            &texture_uploader,
            &[vec![crate::bridge::materials::ResolvedTextureMip {
                rgba: vec![255; 4],
                width: 1,
                height: 1,
            }]],
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let (flat_normal, flat_normal_allocation) = RenderResources::create_texture_frames_view(
            device,
            &texture_uploader,
            &[vec![crate::bridge::materials::ResolvedTextureMip {
                rgba: vec![128, 128, 255, 255],
                width: 1,
                height: 1,
            }]],
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let (error_texture, error_texture_allocation) =
            RenderResources::create_texture_view_from_levels_format(
                device,
                &texture_uploader,
                "file_preview.error_texture",
                &super::materials::checkerboard_mip_levels(),
                wgpu::TextureFormat::Rgba8UnormSrgb,
            );
        let (sky_black_texture, sky_black_texture_allocation) =
            RenderResources::create_texture_view_from_levels_format(
                device,
                &texture_uploader,
                "file_preview.sky_black",
                &[super::TextureUploadLevel {
                    rgba: &[0, 0, 0, 255],
                    width: 1,
                    height: 1,
                }],
                wgpu::TextureFormat::Rgba8UnormSrgb,
            );
        let (phy_debug_texture, phy_debug_texture_allocation) =
            RenderResources::create_texture_view_from_levels_format(
                device,
                &texture_uploader,
                "file_preview.phy_debug_texture",
                &[super::TextureUploadLevel {
                    rgba: super::PHY_DEBUG_RGBA.as_slice(),
                    width: 1,
                    height: 1,
                }],
                wgpu::TextureFormat::Rgba8UnormSrgb,
            );
        let (surface_black_cube, surface_black_cube_allocation) =
            super::cubemap::create_black_view(device, &texture_uploader);
        let default_lightmap = super::lightmaps::Page::white(
            device,
            &lightmap_layout,
            &texture_uploader,
            &lightmap_sampler,
        )
        .expect("validated default lightmap");
        texture_uploader
            .lock()
            .expect("texture uploader lock")
            .flush();
        Self {
            #[cfg(any(test, feature = "map-capture"))]
            clip_mode,
            texture_uploader,
            skinning: std::sync::OnceLock::new(),
            _fallback_allocations: vec![
                surface_white_allocation,
                flat_normal_allocation,
                error_texture_allocation,
                sky_black_texture_allocation,
                phy_debug_texture_allocation,
                surface_black_cube_allocation,
            ],
            model_shader: shader,
            detail_shader,
            water_shader,
            sky_shader,
            model_layout: layout,
            refractive_water_layout,
            sky_pipeline_layout,
            surface_white,
            flat_normal,
            error_texture,
            phy_debug_texture,
            sky_black_texture,
            surface_black_cube,
            screen_refraction,
            uniform_layout,
            material_layout,
            water_material_layout,
            water_frame_layout,
            lightmap_layout,
            default_lightmap,
            sky_layout,
            blit_layout,
            samplers,
            sampler_quality,
            blit_sampler,
            water_refraction_sampler,
            sky_vertices,
            blit_pipeline,
            target_format: format,
        }
    }
}

impl RenderResources {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        capabilities: Option<&wgpu::DownlevelCapabilities>,
    ) -> Self {
        use super::draw::ViewKind;
        let shared = Arc::new(SharedResources::new(device, queue, format, capabilities));
        let sample_count = scene_sample_count();
        let main = ViewPipelines::new(
            device,
            Arc::clone(&shared),
            ViewKind::Main,
            sample_count,
            None,
        );
        let reflection = ViewPipelines::new(
            device,
            Arc::clone(&shared),
            ViewKind::Reflection,
            1,
            (sample_count == 1).then_some(&main.sky_pipeline),
        );
        let refraction = ViewPipelines::new(
            device,
            Arc::clone(&shared),
            ViewKind::Refraction,
            1,
            Some(&reflection.sky_pipeline),
        );
        let target = PipelineTarget {
            format,
            sample_count,
        };
        let refractive_water_layout = &shared.refractive_water_layout;
        let water_shader = &shared.water_shader;
        // Some backends cannot translate depth texture loads.
        device.push_error_scope(wgpu::ErrorFilter::Internal);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let refractive_water_pipeline = create_model_pipeline_with_fragment_entry(
            device,
            refractive_water_layout,
            water_shader,
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
            refractive_water_layout,
            water_shader,
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
        let sun =
            super::sun::gpu::Resources::new(device, format, &shared.material_layout, sample_count);
        let reflection_sun = sun.with_sample_count(device, 1);
        let phy_debug_pipeline = main.overlay_opaque_pipeline.clone();
        Self {
            shared,
            main,
            reflection,
            refraction,
            sun,
            reflection_sun,
            refractive_water_pipeline,
            water_near_intersection_pipeline,
            phy_debug_pipeline,
        }
    }

    pub(super) fn for_view(&self, view: super::draw::ViewKind) -> &ViewPipelines {
        match view {
            super::draw::ViewKind::Main => &self.main,
            super::draw::ViewKind::Reflection => &self.reflection,
            super::draw::ViewKind::Refraction => &self.refraction,
        }
    }
}

impl ViewPipelines {
    pub(super) fn kind(&self) -> super::draw::ViewKind {
        self.kind
    }

    fn new(
        device: &wgpu::Device,
        shared: Arc<SharedResources>,
        view: super::draw::ViewKind,
        sample_count: u32,
        shared_sky: Option<&wgpu::RenderPipeline>,
    ) -> Self {
        use super::draw::ViewKind;
        let format = shared.target_format;
        let target = PipelineTarget {
            format,
            sample_count,
        };
        let layout = &shared.model_layout;
        let shader = &shared.model_shader;
        let sky_shader = &shared.sky_shader;
        let sky_pipeline_layout = &shared.sky_pipeline_layout;
        let refracted = view == ViewKind::Refraction;
        let front_face = if view == ViewKind::Reflection {
            wgpu::FrontFace::Ccw
        } else {
            wgpu::FrontFace::Cw
        };
        let write_mask = if refracted {
            wgpu::ColorWrites::COLOR
        } else {
            wgpu::ColorWrites::ALL
        };
        let pipeline = |fragment, blend, write_enabled, overlay, mask, water, label| {
            create_model_pipeline_with_fragment_entry(
                device,
                layout,
                if water { &shared.water_shader } else { shader },
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
                    front_face: if overlay {
                        wgpu::FrontFace::Cw
                    } else {
                        front_face
                    },
                },
                PipelineShaderEntry {
                    vertex: "vs_main",
                    fragment,
                    write_mask: mask,
                    label,
                },
            )
        };
        let opaque_pipeline = pipeline(
            if refracted {
                "fs_refraction"
            } else {
                "fs_main"
            },
            (!refracted).then_some(wgpu::BlendState::ALPHA_BLENDING),
            true,
            false,
            wgpu::ColorWrites::ALL,
            false,
            "file_preview.scene.opaque",
        );
        let translucent_pipeline = pipeline(
            "fs_main",
            Some(wgpu::BlendState::ALPHA_BLENDING),
            false,
            false,
            write_mask,
            false,
            "file_preview.scene.translucent",
        );
        let additive_pipeline = pipeline(
            "fs_additive",
            Some(additive_blend_state()),
            false,
            false,
            write_mask,
            false,
            "file_preview.scene.additive",
        );
        let overlay_opaque_pipeline = pipeline(
            "fs_main",
            Some(wgpu::BlendState::ALPHA_BLENDING),
            false,
            true,
            write_mask,
            false,
            "file_preview.scene.overlay",
        );
        let overlay_translucent_pipeline = overlay_opaque_pipeline.clone();
        let overlay_additive_pipeline = pipeline(
            "fs_additive",
            Some(additive_blend_state()),
            false,
            true,
            write_mask,
            false,
            "file_preview.scene.overlay_additive",
        );
        let modulate = |overlay, label| {
            create_model_pipeline_with_fragment_entry(
                device,
                layout,
                shader,
                PipelineTarget {
                    format: super::targets::ColorTransfer::Encoded.format(format),
                    sample_count,
                },
                Some(modulate_blend_state()),
                PipelineRasterMode {
                    ignore_z: false,
                    write_enabled: false,
                    bias: overlay_depth_bias_state(),
                    cull_mode: if overlay {
                        None
                    } else {
                        Some(wgpu::Face::Back)
                    },
                    front_face,
                },
                PipelineShaderEntry {
                    vertex: "vs_main",
                    fragment: "fs_main",
                    write_mask: wgpu::ColorWrites::COLOR,
                    label,
                },
            )
        };
        let modulate_pipeline = modulate(false, "file_preview.scene.modulate");
        let overlay_modulate_pipeline = modulate(true, "file_preview.scene.overlay_modulate");
        let planar_pipeline = pipeline(
            "fs_skybox",
            (!refracted).then_some(wgpu::BlendState::ALPHA_BLENDING),
            refracted,
            false,
            wgpu::ColorWrites::ALL,
            true,
            "file_preview.scene.planar",
        );
        let detail_pipeline = create_detail_pipeline(
            device,
            layout,
            &shared.detail_shader,
            target,
            "file_preview.scene.detail",
            write_mask,
        );
        let sky_pipeline = shared_sky.cloned().unwrap_or_else(|| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("file_preview.model_viewer.sky_pipeline"),
                layout: Some(sky_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: sky_shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: (3 * std::mem::size_of::<f32>()) as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x3],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: sky_shader,
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
            })
        });
        Self {
            kind: view,
            shared,
            sample_count,
            opaque_pipeline,
            translucent_pipeline,
            additive_pipeline,
            modulate_pipeline,
            overlay_opaque_pipeline,
            overlay_translucent_pipeline,
            overlay_additive_pipeline,
            overlay_modulate_pipeline,
            detail_pipeline,
            planar_pipeline,
            sky_pipeline,
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

// Native error scopes resolve on the first poll.
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
                SurfaceBlend::Modulate => modulate_blend_state(),
                SurfaceBlend::Opaque | SurfaceBlend::Alpha => wgpu::BlendState::ALPHA_BLENDING,
            })
        };
        create_model_pipeline_with_fragment_entry(
            device,
            &self.model_layout,
            &self.model_shader,
            PipelineTarget {
                format: key.blend.color_transfer().format(self.target_format),
                sample_count: self.for_view(key.view).sample_count,
            },
            blend,
            PipelineRasterMode {
                ignore_z: key.raster.ignore_z,
                write_enabled: key.blend == SurfaceBlend::Opaque && !key.overlay,
                bias: if key.overlay || key.raster.decal_bias || key.blend == SurfaceBlend::Modulate
                {
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
                write_mask: if key.blend == SurfaceBlend::Modulate
                    || (key.view == ViewKind::Refraction && !refracted_opaque)
                {
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
    surface_marks: &wgpu::Buffer,
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
                binding: 7,
                resource: lighting.model_lights.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: surface_marks.as_entire_binding(),
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
    // Reverse-Z decal bias is positive.
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

fn modulate_blend_state() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Dst,
            dst_factor: wgpu::BlendFactor::Src,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
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

// Face orientation: https://github.com/magcius/noclip.website/blob/main/src/SourceEngine/Main.ts
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

fn lightmap_layout_entries() -> [wgpu::BindGroupLayoutEntry; 3] {
    [
        lightmap_layout_entry(0),
        lightmap_layout_entry(1),
        sampler_layout_entry(2),
    ]
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

pub(super) fn water_material_binding(binding: u32) -> bool {
    matches!(binding, 0 | 3 | 5 | 6 | 22)
}

fn sampler_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn material_layout_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    let mut material_entries = vec![
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
            binding: 5,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(
                    std::mem::size_of::<MaterialUniform>() as u64
                ),
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
                >() as u64),
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
                >() as u64),
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
    ];
    material_entries.extend((21..=29).map(sampler_layout_entry));
    material_entries
}

fn water_frame_layout_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Uniforms>() as u64),
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
    ]
}

fn uniform_layout_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    vec![
        wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Uniforms>() as u64),
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 7,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                    super::model_lighting::GpuModelContext,
                >() as u64),
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 8,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                    super::marks::GpuSurfaceMark,
                >() as u64),
            },
            count: None,
        },
    ]
}

#[cfg(test)]
mod sampler_layout_tests {
    use super::*;

    fn counts(entries: impl Iterator<Item = wgpu::BindGroupLayoutEntry>) -> (u32, u32) {
        let mut result = (0, 0);
        for entry in entries.filter(|entry| entry.visibility.contains(wgpu::ShaderStages::FRAGMENT))
        {
            match entry.ty {
                wgpu::BindingType::Texture { .. } => result.0 += 1,
                wgpu::BindingType::Sampler(_) => result.1 += 1,
                _ => {}
            }
        }
        result
    }

    #[test]
    #[ignore = "requires a Vulkan GPU"]
    fn gpu_role_samplers_and_narrow_water_groups_validate() {
        use super::super::{
            images::ImageCache,
            materials::{
                MaterialTextureViews, MaterialUploadMode, PreparedMaterial, SampledTexture,
            },
            samplers::{SamplerQuality, TextureKind, TextureSamplerPolicy},
        };
        use vformats::vtf::texture_flags as flags;
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let adapter = iced::futures::executor::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        )
        .expect("Vulkan adapter");
        let capabilities = adapter.get_downlevel_capabilities();
        let (device, queue) = iced::futures::executor::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor::default()),
        )
        .unwrap();
        let _completion_scope =
            crate::features::file_preview::viewer3d::pipeline::completion::TestDeviceScope::new(
                &device,
            );
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let resources = RenderResources::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            Some(&capabilities),
        );
        let mut images = ImageCache::new(Some(&capabilities));
        let mut lease = images.lease(&resources.texture_uploader);
        let mut diagnostics = Vec::new();
        let primary = SampledTexture {
            view: &resources.error_texture,
            flags: Some(flags::POINTSAMPLE | flags::CLAMPS),
        };
        let secondary = SampledTexture {
            view: &resources.error_texture,
            flags: Some(flags::TRILINEAR | flags::CLAMPT),
        };
        let normal = SampledTexture {
            view: &resources.flat_normal,
            flags: Some(flags::CLAMPT),
        };
        let (_main, _water, _uniform) = resources.create_material(
            &device,
            &queue,
            MaterialTextureViews {
                base: primary,
                base2: secondary,
                normal,
                normal2: normal,
            },
            MaterialUploadMode {
                frames: None,
                name: "sampler-test",
                material: PreparedMaterial::Fallback,
                base_flags: primary.flags,
                base2_flags: secondary.flags,
                render_mode: super::super::RenderMode::Opaque,
                alpha_policy: crate::bridge::materials::TextureAlphaPolicy::Opaque,
            },
            &mut lease,
            &mut diagnostics,
        );
        let policies = diagnostics
            .iter()
            .map(|diagnostic| serde_json::to_value(diagnostic).unwrap())
            .collect::<Vec<_>>();
        let base = policies
            .iter()
            .find(|policy| policy["role"] == "base")
            .unwrap();
        let second = policies
            .iter()
            .find(|policy| policy["role"] == "base2")
            .unwrap();
        assert_eq!(base["policy"]["descriptor"]["mag_filter"], "Nearest");
        assert_eq!(base["policy"]["descriptor"]["address"][0], "ClampToEdge");
        assert_eq!(second["policy"]["descriptor"]["mag_filter"], "Linear");
        assert_eq!(second["policy"]["descriptor"]["address"][1], "ClampToEdge");
        assert_eq!(diagnostics.len(), 12);
        let requested =
            TextureSamplerPolicy::from_flags(flags::POINTSAMPLE, TextureKind::Texture2d)
                .request(SamplerQuality {
                    anisotropy: 1,
                    force_trilinear: false,
                })
                .unwrap();
        assert_eq!(
            resources.samplers.resolve(requested).sampler,
            resources.samplers.resolve(requested).sampler
        );
        assert!(iced::futures::executor::block_on(device.pop_error_scope()).is_none());
    }

    #[test]
    fn combined_pipeline_bindings_fit_default_limits() {
        let main = counts(
            uniform_layout_entries()
                .into_iter()
                .chain(material_layout_entries())
                .chain(lightmap_layout_entries()),
        );
        let water = counts(
            water_frame_layout_entries()
                .into_iter()
                .chain(
                    material_layout_entries()
                        .into_iter()
                        .filter(|entry| water_material_binding(entry.binding)),
                )
                .chain(lightmap_layout_entries()),
        );
        assert_eq!(main, (14, 13));
        assert_eq!(water, (7, 4));
        let limits = wgpu::Limits::default();
        for (textures, samplers) in [main, water] {
            assert!(textures <= limits.max_sampled_textures_per_shader_stage);
            assert!(samplers <= limits.max_samplers_per_shader_stage);
        }
    }

    #[test]
    fn shader_texture_sampling_pairs_use_their_own_role_samplers() {
        use iced::wgpu::naga::{self, Expression, valid};
        let material_pairs = [
            (0, 3),
            (1, 21),
            (6, 22),
            (7, 23),
            (9, 24),
            (10, 25),
            (11, 26),
            (12, 14),
            (13, 27),
            (17, 28),
            (18, 20),
            (19, 29),
        ];
        let mut seen = std::collections::HashSet::new();
        for source in [super::SHADER_SOURCE, super::WATER_SHADER_SOURCE] {
            let module = naga::front::wgsl::parse_str(source).unwrap();
            valid::Validator::new(valid::ValidationFlags::all(), valid::Capabilities::all())
                .validate(&module)
                .unwrap();
            for function in module
                .functions
                .iter()
                .map(|(_, function)| function)
                .chain(module.entry_points.iter().map(|entry| &entry.function))
            {
                for (_, expression) in function.expressions.iter() {
                    let Expression::ImageSample { image, sampler, .. } = expression else {
                        continue;
                    };
                    let (Expression::GlobalVariable(image), Expression::GlobalVariable(sampler)) = (
                        &function.expressions[*image],
                        &function.expressions[*sampler],
                    ) else {
                        panic!("unexpected indirect sampling binding");
                    };
                    let image = module.global_variables[*image].binding.as_ref().unwrap();
                    let sampler = module.global_variables[*sampler].binding.as_ref().unwrap();
                    if image.group == 1 {
                        let expected = material_pairs
                            .iter()
                            .find(|(texture, _)| *texture == image.binding)
                            .unwrap();
                        assert_eq!(
                            (sampler.group, sampler.binding),
                            (1, expected.1),
                            "texture {}",
                            image.binding
                        );
                        seen.insert(image.binding);
                    }
                }
            }
        }
        assert_eq!(
            seen,
            material_pairs
                .map(|(texture, _)| texture)
                .into_iter()
                .collect()
        );
    }
}
