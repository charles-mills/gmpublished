use super::*;
use std::time::Duration;

fn validation_error(device: &wgpu::Device, descriptor: &wgpu::SamplerDescriptor<'_>) {
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _sampler = device.create_sampler(descriptor);
    let error = iced::futures::executor::block_on(device.pop_error_scope());
    assert!(
        matches!(error, Some(wgpu::Error::Validation { .. })),
        "sampler unexpectedly accepted: {error:?}"
    );
}

#[test]
#[ignore = "requires an exclusively reserved Vulkan GPU with hardware anisotropy"]
fn gpu_independent_anisotropic_mip_is_advertised_opt_in_and_sampleable() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter = iced::futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .expect("capability contract untested: no Vulkan adapter");
    assert_eq!(adapter.get_info().backend, wgpu::Backend::Vulkan);
    let hardware = adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::ANISOTROPIC_FILTERING);
    let feature = wgpu::Features::INDEPENDENT_ANISOTROPIC_MIP_FILTER;
    assert_eq!(
        adapter.features().contains(feature),
        hardware,
        "Vulkan feature advertisement must follow hardware anisotropy"
    );
    assert!(
        hardware,
        "capability contract untested: adapter has no hardware anisotropy"
    );
    let (portable, _) = iced::futures::executor::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor::default()),
    )
    .unwrap();
    assert!(!portable.features().contains(feature));
    let requested = TextureSamplerPolicy::from_flags(0, TextureKind::Texture2d)
        .request(SamplerQuality {
            anisotropy: 4,
            force_trilinear: false,
        })
        .unwrap();
    validation_error(&portable, &requested.descriptor().descriptor());
    let lowered = SamplerCache::new(&portable, hardware).resolve(requested);
    assert_eq!(lowered.policy.descriptor.anisotropy, 1);
    assert_eq!(
        lowered.policy.descriptor.mip_filter,
        wgpu::FilterMode::Nearest
    );
    assert_eq!(
        lowered.policy.limitations,
        [SamplerLimitation::IndependentAnisotropicMipUnavailable]
    );
    let (device, queue) =
        iced::futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: feature,
            ..Default::default()
        }))
        .unwrap();
    assert!(device.features().contains(feature));
    let sampled = SamplerCache::new(&device, hardware).resolve(requested);
    assert_eq!(sampled.policy.descriptor, requested.descriptor());
    assert!(sampled.policy.limitations.is_empty());
    for (min_filter, mag_filter) in [
        (wgpu::FilterMode::Nearest, wgpu::FilterMode::Linear),
        (wgpu::FilterMode::Linear, wgpu::FilterMode::Nearest),
        (wgpu::FilterMode::Nearest, wgpu::FilterMode::Nearest),
    ] {
        validation_error(
            &device,
            &wgpu::SamplerDescriptor {
                min_filter,
                mag_filter,
                ..requested.descriptor().descriptor()
            },
        );
    }
    sample_and_readback(&device, &queue, &sampled.sampler);
    println!(
        "native sampler contract passed: {:?}; enabled {:?}; requested/applied {:?}",
        adapter.get_info(),
        device.features(),
        sampled.policy.descriptor
    );
}

fn sample_and_readback(device: &wgpu::Device, queue: &wgpu::Queue, sampler: &wgpu::Sampler) {
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let extent = wgpu::Extent3d {
        width: 8,
        height: 8,
        depth_or_array_layers: 1,
    };
    let source = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sampler contract source"),
        size: extent,
        mip_level_count: 4,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let color = [37_u8, 99, 151, 255];
    for mip in 0..4 {
        let side = 8 >> mip;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source,
                mip_level: mip,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &color.repeat((side * side) as usize),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(side * 4),
                rows_per_image: Some(side),
            },
            wgpu::Extent3d {
                width: side,
                height: side,
                depth_or_array_layers: 1,
            },
        );
    }
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sampler contract shader"),
        source: wgpu::ShaderSource::Wgsl(
            r"
struct Vertex { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@vertex fn vs(@builtin(vertex_index) index: u32) -> Vertex {
    let xy = array(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0))[index];
    return Vertex(vec4(xy,0.0,1.0),xy * vec2(0.5,-0.5) + vec2(0.5));
}
@fragment fn fs(input: Vertex) -> @location(0) vec4<f32> {
    return textureSample(image,image_sampler,input.uv);
}
"
            .into(),
        ),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sampler contract pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview: None,
        cache: None,
    });
    let source_view = source.create_view(&Default::default());
    let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sampler contract bindings"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&source_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sampler contract target"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&Default::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sampler contract readback"),
        size: 256 * 8,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("sampler contract draw"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.draw(0..3, 0..1);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(8),
            },
        },
        extent,
    );
    let submission = queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap();
        });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(Duration::from_secs(30)),
        })
        .unwrap();
    receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    let pixels = readback.slice(..).get_mapped_range();
    for row in pixels.chunks_exact(256) {
        for pixel in row[..32].chunks_exact(4) {
            assert_eq!(pixel, color);
        }
    }
    drop(pixels);
    readback.unmap();
    assert!(iced::futures::executor::block_on(device.pop_error_scope()).is_none());
}

#[test]
#[ignore = "requires an exclusively reserved Vulkan GPU with hardware anisotropy"]
fn gpu_point_anisotropy_is_opt_in_filtering_and_sampleable() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        flags: wgpu::InstanceFlags::VALIDATION,
        ..Default::default()
    });
    let adapter = iced::futures::executor::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
    )
    .expect("point anisotropy contract untested: no Vulkan adapter");
    let hardware = adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::ANISOTROPIC_FILTERING);
    let point = wgpu::Features::NATIVE_ANISOTROPIC_POINT_FILTERING;
    let mip = wgpu::Features::INDEPENDENT_ANISOTROPIC_MIP_FILTER;
    assert_eq!(adapter.features().contains(point), hardware);
    assert!(
        hardware,
        "point anisotropy contract untested: no hardware support"
    );
    let descriptor = wgpu::SamplerDescriptor {
        min_filter: wgpu::FilterMode::Nearest,
        mag_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Nearest,
        anisotropy_clamp: 4,
        ..Default::default()
    };
    let (portable, _) = iced::futures::executor::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor::default()),
    )
    .unwrap();
    validation_error(&portable, &descriptor);
    let (point_only, _) =
        iced::futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: point,
            ..Default::default()
        }))
        .unwrap();
    validation_error(&point_only, &descriptor);
    let linear_mip = wgpu::SamplerDescriptor {
        mipmap_filter: wgpu::FilterMode::Linear,
        ..descriptor.clone()
    };
    point_only.push_error_scope(wgpu::ErrorFilter::Validation);
    let _valid = point_only.create_sampler(&linear_mip);
    assert!(iced::futures::executor::block_on(point_only.pop_error_scope()).is_none());
    let (device, queue) =
        iced::futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: point | mip,
            ..Default::default()
        }))
        .unwrap();
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let sampler = device.create_sampler(&descriptor);
    assert!(iced::futures::executor::block_on(device.pop_error_scope()).is_none());
    sample_and_readback(&device, &queue, &sampler);

    let unfilterable = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("unfilterable float32 contract"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
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
    let view = unfilterable.create_view(&Default::default());
    let entries = [
        wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&view),
        },
        wgpu::BindGroupEntry {
            binding: 1,
            resource: wgpu::BindingResource::Sampler(&sampler),
        },
    ];
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _invalid = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &entries,
    });
    let error = iced::futures::executor::block_on(device.pop_error_scope());
    assert!(
        matches!(error, Some(wgpu::Error::Validation { .. })),
        "anisotropic sampler must reject unfilterable binding: {error:?}"
    );
    let nearest = device.create_sampler(&wgpu::SamplerDescriptor {
        anisotropy_clamp: 1,
        ..descriptor
    });
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _valid = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[
            entries[0].clone(),
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&nearest),
            },
        ],
    });
    assert!(iced::futures::executor::block_on(device.pop_error_scope()).is_none());
    println!(
        "point anisotropy sampler contract passed {:?}, enabled {:?}",
        adapter.get_info(),
        device.features()
    );
}
