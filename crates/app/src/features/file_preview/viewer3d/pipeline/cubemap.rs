use super::wgpu;
use crate::bridge::materials::{ResolvedCubemap, ResolvedTextureMip};

fn layout(cube: &ResolvedCubemap, max_dimension: u32) -> Option<(u32, u32)> {
    let first = cube.faces.first()?.first()?;
    let size = first.width;
    let mip_count = u32::try_from(cube.faces[0].len()).ok()?;
    if size == 0
        || size != first.height
        || size > max_dimension
        || mip_count == 0
        || mip_count > u32::BITS - size.leading_zeros()
    {
        return None;
    }
    for face in &cube.faces {
        if face.len() != mip_count as usize {
            return None;
        }
        for (level, image) in face.iter().enumerate() {
            let side = (size >> level).max(1);
            let expected = u64::from(side) * u64::from(side) * 4;
            if image.width != side || image.height != side || image.rgba.len() as u64 != expected {
                return None;
            }
        }
    }
    Some((size, mip_count))
}

pub(super) fn create_view(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    cube: Option<&ResolvedCubemap>,
) -> wgpu::TextureView {
    let fallback = ResolvedCubemap {
        faces: std::array::from_fn(|_| {
            vec![ResolvedTextureMip {
                rgba: vec![0, 0, 0, 255],
                width: 1,
                height: 1,
            }]
        }),
        flags: 0,
        frame: 0,
    };
    let (cube, size, mip_count) = cube
        .and_then(|cube| {
            layout(cube, device.limits().max_texture_dimension_2d)
                .map(|(size, mips)| (cube, size, mips))
        })
        .unwrap_or_else(|| {
            if cube.is_some() {
                log::warn!("invalid cubemap upload layout; using black reflection");
            }
            (&fallback, 1, 1)
        });
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("file_preview.surface.environment_cube"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 6,
        },
        mip_level_count: mip_count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (face, mips) in cube.faces.iter().enumerate() {
        for (level, image) in mips.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: face as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &image.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(image.width * 4),
                    rows_per_image: Some(image.height),
                },
                wgpu::Extent3d {
                    width: image.width,
                    height: image.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::Cube),
        ..Default::default()
    })
}

pub(super) fn create_sampler(device: &wgpu::Device, texture_flags: u32) -> wgpu::Sampler {
    use vformats::vtf::texture_flags as flags;
    let filter = if texture_flags & flags::POINTSAMPLE != 0 {
        wgpu::FilterMode::Nearest
    } else {
        wgpu::FilterMode::Linear
    };
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("file_preview.surface.environment_sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: filter,
        min_filter: filter,
        mipmap_filter: if texture_flags & (flags::TRILINEAR | flags::ANISOTROPIC) != 0 {
            wgpu::FilterMode::Linear
        } else {
            wgpu::FilterMode::Nearest
        },
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> ResolvedCubemap {
        ResolvedCubemap {
            faces: std::array::from_fn(|_| {
                vec![
                    ResolvedTextureMip {
                        rgba: vec![255; 64],
                        width: 4,
                        height: 4,
                    },
                    ResolvedTextureMip {
                        rgba: vec![127; 16],
                        width: 2,
                        height: 2,
                    },
                    ResolvedTextureMip {
                        rgba: vec![0; 4],
                        width: 1,
                        height: 1,
                    },
                ]
            }),
            flags: 0,
            frame: 0,
        }
    }

    #[test]
    fn upload_requires_consistent_six_face_mip_extents_and_device_limits() {
        let source = cube();
        assert_eq!(layout(&source, 4096), Some((4, 3)));
        assert_eq!(layout(&source, 2), None);
        let mut invalid = cube();
        invalid.faces[5][1].rgba.pop();
        assert_eq!(layout(&invalid, 4096), None);
        let mut invalid = cube();
        invalid.faces[3].pop();
        assert_eq!(layout(&invalid, 4096), None);
        let mut invalid = cube();
        invalid.faces[1][2].width = 2;
        assert_eq!(layout(&invalid, 4096), None);
        let mut invalid = cube();
        invalid.faces.iter_mut().for_each(|face| {
            face.push(ResolvedTextureMip {
                rgba: vec![0; 4],
                width: 1,
                height: 1,
            });
        });
        assert_eq!(layout(&invalid, 4096), None);
    }
}
