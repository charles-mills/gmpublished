//! Lighting is shared by a frame's scene views, independently of other widgets.
use super::{RenderResources, wgpu};
use gmpublished_domain::scene::map::LightmapAtlas;

#[derive(Debug)]
pub struct Frame {
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    /// Native Water_DX90 filters encoded lightmap bytes; diffuse uses sRGB.
    pub raw_view: wgpu::TextureView,
    values: [f32; 256],
}
impl Frame {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _resources: &RenderResources,
        atlas: Option<&LightmapAtlas>,
    ) -> Self {
        let (width, height, bytes) = atlas.map_or((1, 1, &[255_u8; 4][..]), |a| {
            (a.width, a.height, a.rgba.as_slice())
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.frame.lightmap"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        });
        write(queue, &texture, [0, 0], width, height, bytes);
        let view = texture.create_view(&Default::default());
        let raw_view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            ..Default::default()
        });
        Self {
            texture,
            view,
            raw_view,
            values: atlas.map_or([1.0; 256], |a| a.style_values(0.0)),
        }
    }
    pub fn update(&mut self, queue: &wgpu::Queue, atlas: Option<&LightmapAtlas>, time: f32) {
        let Some(atlas) = atlas else { return };
        let next = atlas.style_values(time);
        if self.values == next {
            return;
        }
        for patch in atlas.updates(&self.values, &next) {
            write(
                queue,
                &self.texture,
                patch.origin,
                patch.width,
                patch.height,
                &patch.rgba,
            );
        }
        self.values = next;
    }
}
fn write(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    origin: [u32; 2],
    width: u32,
    height: u32,
    bytes: &[u8],
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: origin[0],
                y: origin[1],
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
}
