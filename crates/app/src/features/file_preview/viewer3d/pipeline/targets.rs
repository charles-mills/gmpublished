//! Render targets sized to the viewport: the MSAA colour and depth
//! attachments, the single-sample resolve texture the final blit reads, and
//! the lazily added refraction copy that water frames sample. Recreated
//! whenever the viewport size or surface format changes.

use super::resources::RenderResources;
use super::{MSAA_SAMPLE_COUNT, wgpu};

#[derive(Debug)]
pub struct RenderTargets {
    pub color: wgpu::TextureView,
    pub resolve_texture: wgpu::Texture,
    pub resolve_view: wgpu::TextureView,
    pub depth: wgpu::TextureView,
    pub refraction: Option<RefractionTarget>,
    pub blit_bind_group: wgpu::BindGroup,
    pub size: (u32, u32),
    pub format: wgpu::TextureFormat,
}

#[derive(Debug)]
pub struct RefractionTarget {
    pub texture: wgpu::Texture,
    pub bind_group: wgpu::BindGroup,
}

impl RenderTargets {
    /// Recreates the attachments when the viewport size or surface format
    /// changes; otherwise reuses them, adding the refraction copy on the
    /// first frame that needs it.
    pub fn ensure(
        slot: &mut Option<Self>,
        device: &wgpu::Device,
        resources: &RenderResources,
        width: u32,
        height: u32,
        needs_refraction: bool,
    ) {
        let size = (width.max(1), height.max(1));
        if slot.as_ref().is_some_and(|targets| {
            targets.size == size && targets.format == resources.target_format
        }) {
            if let Some(targets) = slot.as_mut() {
                if needs_refraction {
                    targets.ensure_refraction(device, resources);
                } else {
                    // Shed it when the scene stops needing it. Keyed on size and
                    // format, this target otherwise survives a switch to a
                    // waterless preview at the same viewport — a full-screen
                    // texture held for a pass that no longer runs.
                    targets.refraction = None;
                }
            }
            return;
        }
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.model_viewer.msaa_color"),
            size: extent,
            mip_level_count: 1,
            sample_count: MSAA_SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: resources.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let resolve = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.model_viewer.msaa_resolve"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: resources.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.model_viewer.depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: MSAA_SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let color = color.create_view(&wgpu::TextureViewDescriptor::default());
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
        let mut targets = Self {
            color,
            resolve_texture: resolve,
            resolve_view,
            depth,
            refraction: None,
            blit_bind_group,
            size,
            format: resources.target_format,
        };
        if needs_refraction {
            targets.ensure_refraction(device, resources);
        }
        *slot = Some(targets);
    }

    fn ensure_refraction(&mut self, device: &wgpu::Device, resources: &RenderResources) {
        if self.refraction.is_some() {
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.model_viewer.refraction"),
            size: wgpu::Extent3d {
                width: self.size.0,
                height: self.size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // The frame group carries the shared uniforms alongside the
        // refraction inputs; see the `water_frame_layout` rationale in
        // `RenderResources::new`.
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.model_viewer.water_frame_bind_group"),
            layout: &resources.water_frame_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: resources.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&resources.water_refraction_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&self.depth),
                },
            ],
        });
        self.refraction = Some(RefractionTarget {
            texture,
            bind_group,
        });
    }
}
