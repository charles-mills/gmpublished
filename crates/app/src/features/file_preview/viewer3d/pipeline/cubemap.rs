use super::wgpu;

pub(super) fn create_black_view(
    _device: &wgpu::Device,
    uploader: &super::texture_upload::SharedUploader,
) -> (wgpu::TextureView, super::texture_upload::UploadedTexture) {
    let mut uploader = uploader.lock().expect("texture uploader lock");
    let texture = uploader
        .allocate(&wgpu::TextureDescriptor {
            label: Some("file_preview.environment_fallback"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 6,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
        .expect("generated cube allocation");
    for layer in 0..6 {
        uploader
            .write(
                &texture,
                0,
                wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                super::texture_upload::Input {
                    storage: crate::bridge::materials::TextureStorage::Rgba8,
                    dimensions: [1, 1],
                    bytes: &[0, 0, 0, 255],
                },
            )
            .expect("generated cube upload");
    }
    drop(uploader);
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::Cube),
        ..Default::default()
    });
    (view, texture)
}
