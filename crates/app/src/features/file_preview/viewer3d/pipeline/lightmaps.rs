use super::{
    texture_upload::{Input, SharedUploader, UploadedTexture},
    wgpu,
};
use crate::bridge::materials::TextureStorage;
use gmpublished_domain::scene::map::LightmapAtlas;

#[derive(Debug)]
pub struct Frame {
    texture: UploadedTexture,
    uploader: SharedUploader,
    pub view: wgpu::TextureView,
    /// Water filters encoded lightmap bytes; diffuse uses sRGB.
    pub raw_view: wgpu::TextureView,
    values: [f32; 256],
}
impl Frame {
    pub(super) fn new(uploader: &SharedUploader, atlas: Option<&LightmapAtlas>) -> Self {
        let (width, height, bytes) = atlas.map_or((1, 1, &[255_u8; 4][..]), |a| {
            (a.width, a.height, a.rgba.as_slice())
        });
        let input = Input {
            storage: TextureStorage::Rgba8,
            dimensions: [width, height],
            bytes,
        };
        let mut uploads = uploader.lock().expect("texture uploader lock");
        let plan = uploads
            .plan(input, wgpu::TextureFormat::Rgba8UnormSrgb)
            .expect("validated lightmap copy plan");
        let texture = uploads
            .allocate(&wgpu::TextureDescriptor {
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
            })
            .expect("validated lightmap allocation");
        uploads
            .write_planned(&texture, 0, wgpu::Origin3d::ZERO, input, plan)
            .expect("validated lightmap upload");
        uploads.flush();
        drop(uploads);
        let view = texture.create_view(&Default::default());
        let raw_view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            ..Default::default()
        });
        Self {
            texture,
            uploader: std::sync::Arc::clone(uploader),
            view,
            raw_view,
            values: atlas.map_or([1.0; 256], |a| a.style_values(0.0)),
        }
    }
    pub fn update(&mut self, atlas: Option<&LightmapAtlas>, time: f32) {
        let Some(atlas) = atlas else { return };
        let next = atlas.style_values(time);
        if self.values == next {
            return;
        }
        let mut uploader = self.uploader.lock().expect("texture uploader lock");
        for patch in atlas.updates(&self.values, &next) {
            uploader
                .write(
                    &self.texture,
                    0,
                    wgpu::Origin3d {
                        x: patch.origin[0],
                        y: patch.origin[1],
                        z: 0,
                    },
                    Input {
                        storage: TextureStorage::Rgba8,
                        dimensions: [patch.width, patch.height],
                        bytes: &patch.rgba,
                    },
                )
                .expect("validated lightmap patch upload");
        }
        uploader.flush();
        drop(uploader);
        self.values = next;
    }
}
