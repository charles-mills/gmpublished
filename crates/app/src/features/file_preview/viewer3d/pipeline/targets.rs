use super::resources::{RenderResources, ViewPipelines};
use super::wgpu;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ColorTransfer {
    Linear,
    Encoded,
}

impl ColorTransfer {
    pub(super) fn format(self, format: wgpu::TextureFormat) -> wgpu::TextureFormat {
        match self {
            Self::Linear => format,
            Self::Encoded => format.remove_srgb_suffix(),
        }
    }
}

#[derive(Debug)]
pub struct RenderTargets {
    pub color: wgpu::TextureView,
    pub resolve_view: wgpu::TextureView,
    encoded_color: wgpu::TextureView,
    encoded_resolve: wgpu::TextureView,
    pub depth: wgpu::TextureView,
    pub blit_bind_group: wgpu::BindGroup,
    pub size: (u32, u32),
    pub format: wgpu::TextureFormat,
    pub sample_count: u32,
}

impl RenderTargets {
    pub(super) fn color_attachment(&self, transfer: ColorTransfer) -> &wgpu::TextureView {
        match transfer {
            ColorTransfer::Linear => &self.color,
            ColorTransfer::Encoded => &self.encoded_color,
        }
    }

    pub(super) fn resolve_attachment(&self, transfer: ColorTransfer) -> Option<&wgpu::TextureView> {
        (self.sample_count > 1).then_some(match transfer {
            ColorTransfer::Linear => &self.resolve_view,
            ColorTransfer::Encoded => &self.encoded_resolve,
        })
    }

    pub fn size_for_device(device: &wgpu::Device, size: (u32, u32)) -> (u32, u32) {
        let limit = device.limits().max_texture_dimension_2d;
        (size.0.clamp(1, limit), size.1.clamp(1, limit))
    }

    #[cfg(feature = "map-capture")]
    pub fn estimated_bytes(&self) -> u64 {
        u64::from(self.size.0)
            * u64::from(self.size.1)
            * bytes_per_pixel_for_samples(self.format, self.sample_count)
    }

    pub fn ensure(
        slot: &mut Option<Self>,
        device: &wgpu::Device,
        resources: &ViewPipelines,
        size: (u32, u32),
    ) {
        let size = Self::size_for_device(device, size);
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
        let encoded_format = ColorTransfer::Encoded.format(resources.target_format);
        let encoded_formats = [encoded_format];
        let view_formats = if encoded_format == resources.target_format {
            &[][..]
        } else {
            &encoded_formats[..]
        };
        let resolve = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.model_viewer.msaa_resolve"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: resources.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats,
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
        let color_texture = if resources.sample_count > 1 {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("file_preview.model_viewer.msaa_color"),
                size: extent,
                mip_level_count: 1,
                sample_count: resources.sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: resources.target_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats,
            })
        } else {
            resolve.clone()
        };
        let color = color_texture.create_view(&Default::default());
        let encoded_color = color_texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(encoded_format),
            ..Default::default()
        });
        let resolve_view = resolve.create_view(&Default::default());
        let encoded_resolve = resolve.create_view(&wgpu::TextureViewDescriptor {
            format: Some(encoded_format),
            ..Default::default()
        });
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
            encoded_color,
            encoded_resolve,
            depth,
            blit_bind_group,
            size,
            format: resources.target_format,
            sample_count: resources.sample_count,
        };
        *slot = Some(targets);
    }
}

#[derive(Debug, Eq, PartialEq)]
struct WaterBindingKey {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    refraction: wgpu::TextureView,
    depth: wgpu::TextureView,
    reflection: wgpu::TextureView,
    lightmap: wgpu::TextureView,
    raw_lightmap: wgpu::TextureView,
}

#[derive(Debug)]
pub struct WaterBindings {
    key: WaterBindingKey,
    pub bind_group: wgpu::BindGroup,
}

#[derive(Clone, Copy)]
pub struct WaterBindingInputs<'a> {
    pub uniforms: &'a wgpu::Buffer,
    pub refraction: &'a wgpu::TextureView,
    pub depth: &'a wgpu::TextureView,
    pub reflection: &'a wgpu::TextureView,
    pub lightmap: &'a super::lightmaps::Frame,
}

impl WaterBindings {
    pub fn ensure(
        slot: &mut Option<Self>,
        device: &wgpu::Device,
        resources: &RenderResources,
        inputs: WaterBindingInputs<'_>,
    ) {
        let WaterBindingInputs {
            uniforms,
            refraction,
            depth,
            reflection,
            lightmap,
        } = inputs;
        let key = WaterBindingKey {
            layout: resources.water_frame_layout.clone(),
            sampler: resources.water_refraction_sampler.clone(),
            uniforms: uniforms.clone(),
            refraction: refraction.clone(),
            depth: depth.clone(),
            reflection: reflection.clone(),
            lightmap: lightmap.view.clone(),
            raw_lightmap: lightmap.raw_view.clone(),
        };
        if slot.as_ref().is_some_and(|bindings| bindings.key == key) {
            return;
        }
        let bind_group = create_water_bind_group(
            device, resources, uniforms, refraction, depth, reflection, lightmap,
        );
        *slot = Some(Self { key, bind_group });
    }
}

fn create_water_bind_group(
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
