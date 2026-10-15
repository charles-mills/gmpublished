use super::resources::RenderResources;
use super::wgpu;

#[derive(Debug)]
pub struct RenderTargets {
    pub color: wgpu::TextureView,
    pub resolve_view: wgpu::TextureView,
    pub depth: wgpu::TextureView,
    pub blit_bind_group: wgpu::BindGroup,
    pub size: (u32, u32),
    pub format: wgpu::TextureFormat,
    pub sample_count: u32,
}

impl RenderTargets {
    #[cfg(feature = "map-capture")]
    pub fn estimated_bytes(&self) -> u64 {
        u64::from(self.size.0)
            * u64::from(self.size.1)
            * bytes_per_pixel_for_samples(self.format, self.sample_count)
    }

    pub fn ensure(
        slot: &mut Option<Self>,
        device: &wgpu::Device,
        resources: &RenderResources,
        size: (u32, u32),
    ) {
        let size = (size.0.max(1), size.1.max(1));
        if slot.as_ref().is_some_and(|targets| {
            targets.size == size
                && targets.format == resources.target_format
                && targets.sample_count == resources.sample_count
        }) {
            return;
        }
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };
        let resolve = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.model_viewer.msaa_resolve"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: resources.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.model_viewer.depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: resources.sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let color = if resources.sample_count > 1 {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("file_preview.model_viewer.msaa_color"),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: resources.sample_count,
                    dimension: wgpu::TextureDimension::D2,
                    format: resources.target_format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        } else {
            // A single-sample pass writes the sampled colour texture itself.
            // Resolving a one-sample attachment is invalid in WebGPU.
            resolve.create_view(&wgpu::TextureViewDescriptor::default())
        };
        let resolve_view = resolve.create_view(&wgpu::TextureViewDescriptor::default());
        let depth = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let blit_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.model_viewer.blit_bind_group"),
            layout: &resources.blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&resolve_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&resources.blit_sampler),
                },
            ],
        });
        let targets = Self {
            color,
            resolve_view,
            depth,
            blit_bind_group,
            size,
            format: resources.target_format,
            sample_count: resources.sample_count,
        };
        *slot = Some(targets);
    }
}

pub fn water_bind_group(
    device: &wgpu::Device,
    resources: &RenderResources,
    uniforms: &wgpu::Buffer,
    refraction: &wgpu::TextureView,
    depth: &wgpu::TextureView,
    reflection: &wgpu::TextureView,
    lightmap: &super::lightmaps::Frame,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("file_preview.planar_frame"),
        layout: &resources.water_frame_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(refraction),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&resources.water_refraction_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(depth),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(reflection),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(&lightmap.view),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::TextureView(&lightmap.raw_view),
            },
        ],
    })
}

/// Colour MSAA + resolved colour + MSAA depth, excluding driver overhead.
pub fn bytes_per_pixel_for_samples(format: wgpu::TextureFormat, sample_count: u32) -> u64 {
    let count = u64::from(sample_count);
    u64::from(format.block_copy_size(None).unwrap_or(4)) * (count + u64::from(count > 1))
        + 4 * count
}
