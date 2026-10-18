use super::{resources::SharedResources, targets::RenderTargets, wgpu};
#[derive(Debug)]
pub(super) struct Snapshot {
    texture: wgpu::Texture,
    pub bind_group: wgpu::BindGroup,
}
impl Snapshot {
    pub(super) fn new(resources: &SharedResources, targets: &RenderTargets) -> Self {
        let texture = resources.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.glass.scene_snapshot"),
            size: wgpu::Extent3d {
                width: targets.size.0,
                height: targets.size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: targets.format,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let sampler = resources.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("file_preview.glass.scene_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let view = texture.create_view(&Default::default());
        let bind_group = resources
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("file_preview.glass.scene_bind_group"),
                layout: &resources.lightmap_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
        Self {
            texture,
            bind_group,
        }
    }
    pub(super) fn copy(&self, encoder: &mut wgpu::CommandEncoder, targets: &RenderTargets) {
        encoder.copy_texture_to_texture(
            targets.resolve_texture.as_image_copy(),
            self.texture.as_image_copy(),
            wgpu::Extent3d {
                width: targets.size.0,
                height: targets.size.1,
                depth_or_array_layers: 1,
            },
        );
    }
}
