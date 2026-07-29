//! Texture and material uploads: mip-chain and block-compressed texture
//! views, material and sky bind groups, and the fallback content (white,
//! checkerboard, collision-debug tint) used when a texture is missing.
//! Everything here is created against the layouts and samplers owned by
//! [`RenderResources`].

use super::resources::RenderResources;
use super::uniforms::MaterialUniform;
use super::{
    BcFormat, CHECKERBOARD_BYTES, CHECKERBOARD_MIP_1X1_BYTES, CHECKERBOARD_MIP_2X2_BYTES,
    CHECKERBOARD_MIP_4X4_BYTES, CHECKERBOARD_MIP_RGBA, CHECKERBOARD_SIZE, CHECKERBOARD_SIZE_USIZE,
    MaterialSlot, PHY_DEBUG_MATERIAL_NAME, PHY_DEBUG_RGBA, RenderMode, ResolvedBcMip,
    ResolvedTexture, TextureUploadLevel, bc_mip_is_valid, bc_texture_format, decode_bc_texture,
    wgpu, write_bc_texture_level, write_texture_level,
};

#[derive(Clone, Copy)]
pub struct MaterialTextureViews<'a> {
    pub base: &'a wgpu::TextureView,
    pub base2: &'a wgpu::TextureView,
    pub lightmap: &'a wgpu::TextureView,
}

#[derive(Clone, Copy)]
pub struct MaterialUploadMode {
    pub force_opaque: bool,
    pub render_mode: RenderMode,
}

impl RenderResources {
    pub fn create_texture_view(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> wgpu::TextureView {
        let level = TextureUploadLevel {
            rgba,
            width: width.max(1),
            height: height.max(1),
        };
        self.create_texture_view_from_levels(device, queue, label, &[level])
    }

    pub fn create_material_texture_view(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        texture: Option<&ResolvedTexture>,
    ) -> wgpu::TextureView {
        let Some(texture) = texture else {
            let levels = checkerboard_mip_levels();
            return self.create_texture_view_from_levels(device, queue, label, &levels);
        };
        if texture.is_water_fallback() {
            return self.create_texture_view(
                device,
                queue,
                label,
                texture.rgba_bytes().unwrap_or(WHITE_RGBA.as_slice()),
                texture.width.max(1),
                texture.height.max(1),
            );
        }
        if let Some((format, mips)) = texture.bc_payload() {
            return self.create_bc_texture_view(device, queue, label, format, mips);
        }

        let levels = texture
            .mip_chain()
            .map(|mip| TextureUploadLevel {
                rgba: mip.rgba,
                width: mip.width,
                height: mip.height,
            })
            .collect::<Vec<_>>();
        self.create_texture_view_from_levels(device, queue, label, &levels)
    }

    pub fn create_slot_texture_view(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        slot: &MaterialSlot,
        texture: Option<&ResolvedTexture>,
    ) -> wgpu::TextureView {
        if slot.name == PHY_DEBUG_MATERIAL_NAME && texture.is_none() {
            return self.create_texture_view(device, queue, label, PHY_DEBUG_RGBA.as_slice(), 1, 1);
        }
        self.create_material_texture_view(device, queue, label, texture)
    }

    pub fn create_texture_view_from_levels(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        levels: &[TextureUploadLevel<'_>],
    ) -> wgpu::TextureView {
        let fallback_level = TextureUploadLevel {
            rgba: WHITE_RGBA.as_slice(),
            width: 1,
            height: 1,
        };
        let supplied_base = levels.first().copied();
        let use_supplied_chain = supplied_base.is_some_and(TextureUploadLevel::is_valid)
            && levels.iter().all(|level| level.is_valid());
        let base = if use_supplied_chain {
            supplied_base.unwrap_or(fallback_level)
        } else {
            supplied_base
                .filter(|level| level.is_valid())
                .unwrap_or(fallback_level)
        };
        let mip_level_count = if use_supplied_chain {
            u32::try_from(levels.len()).unwrap_or(1).max(1)
        } else {
            1
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: base.width.max(1),
                height: base.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        if use_supplied_chain {
            for (mip_level, level) in levels.iter().take(mip_level_count as usize).enumerate() {
                write_texture_level(
                    queue,
                    &texture,
                    u32::try_from(mip_level).unwrap_or(0),
                    *level,
                );
            }
        } else {
            write_texture_level(queue, &texture, 0, base);
        }
        texture.create_view(&wgpu::TextureViewDescriptor::default())
    }

    pub fn create_bc_texture_view(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        format: BcFormat,
        mips: &[ResolvedBcMip],
    ) -> wgpu::TextureView {
        let Some(base) = mips.first() else {
            return self.create_texture_view(device, queue, label, WHITE_RGBA.as_slice(), 1, 1);
        };
        if !device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            if let Some(rgba) = decode_bc_texture(format, base.width, base.height, &base.data) {
                return self.create_texture_view(
                    device,
                    queue,
                    label,
                    &rgba,
                    base.width,
                    base.height,
                );
            }
            return self.create_texture_view(device, queue, label, WHITE_RGBA.as_slice(), 1, 1);
        }
        if !mips.iter().all(|mip| bc_mip_is_valid(format, mip)) {
            if let Some(rgba) = decode_bc_texture(format, base.width, base.height, &base.data) {
                return self.create_texture_view(
                    device,
                    queue,
                    label,
                    &rgba,
                    base.width,
                    base.height,
                );
            }
            return self.create_texture_view(device, queue, label, WHITE_RGBA.as_slice(), 1, 1);
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: base.width.max(1),
                height: base.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: u32::try_from(mips.len()).unwrap_or(1).max(1),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: bc_texture_format(format),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (mip_level, mip) in mips.iter().enumerate() {
            write_bc_texture_level(
                queue,
                &texture,
                u32::try_from(mip_level).unwrap_or(0),
                format,
                mip,
            );
        }
        texture.create_view(&wgpu::TextureViewDescriptor::default())
    }

    pub fn create_material(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        views: MaterialTextureViews<'_>,
        mode: MaterialUploadMode,
    ) -> (wgpu::BindGroup, wgpu::Buffer) {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.model_viewer.material_uniform"),
            size: std::mem::size_of::<MaterialUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let material_uniform = MaterialUniform::new(mode.force_opaque, mode.render_mode);
        // Material uploads are immutable for this content_id; write once while
        // creating the bind group.
        queue.write_buffer(&uniform, 0, bytemuck::bytes_of(&material_uniform));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.model_viewer.material_bind_group"),
            layout: &self.material_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(views.base),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(views.base2),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(views.lightmap),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.material_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.simple_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        (bind_group, uniform)
    }

    pub fn create_sky_material(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.model_viewer.sky_bind_group"),
            layout: &self.sky_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.simple_sampler),
                },
            ],
        })
    }
}

pub static WHITE_RGBA: [u8; 4] = [255, 255, 255, 255];

// 8x8 magenta/black checkerboard for unresolved materials.
pub static CHECKERBOARD_RGBA: [u8; CHECKERBOARD_BYTES] = checkerboard_rgba();
pub static CHECKERBOARD_MIP_4X4_RGBA: [u8; CHECKERBOARD_MIP_4X4_BYTES] =
    solid_rgba(CHECKERBOARD_MIP_RGBA);
pub static CHECKERBOARD_MIP_2X2_RGBA: [u8; CHECKERBOARD_MIP_2X2_BYTES] =
    solid_rgba(CHECKERBOARD_MIP_RGBA);
pub static CHECKERBOARD_MIP_1X1_RGBA: [u8; CHECKERBOARD_MIP_1X1_BYTES] =
    solid_rgba(CHECKERBOARD_MIP_RGBA);

pub fn checkerboard_mip_levels() -> [TextureUploadLevel<'static>; 4] {
    [
        TextureUploadLevel {
            rgba: CHECKERBOARD_RGBA.as_slice(),
            width: CHECKERBOARD_SIZE,
            height: CHECKERBOARD_SIZE,
        },
        TextureUploadLevel {
            rgba: CHECKERBOARD_MIP_4X4_RGBA.as_slice(),
            width: 4,
            height: 4,
        },
        TextureUploadLevel {
            rgba: CHECKERBOARD_MIP_2X2_RGBA.as_slice(),
            width: 2,
            height: 2,
        },
        TextureUploadLevel {
            rgba: CHECKERBOARD_MIP_1X1_RGBA.as_slice(),
            width: 1,
            height: 1,
        },
    ]
}

pub const fn checkerboard_rgba() -> [u8; CHECKERBOARD_BYTES] {
    let mut rgba = [0_u8; CHECKERBOARD_BYTES];
    let mut y = 0;
    while y < CHECKERBOARD_SIZE_USIZE {
        let mut x = 0;
        while x < CHECKERBOARD_SIZE_USIZE {
            let offset = (y * CHECKERBOARD_SIZE_USIZE + x) * 4;
            if (x + y) % 2 == 0 {
                rgba[offset] = 255;
                rgba[offset + 1] = 0;
                rgba[offset + 2] = 255;
                rgba[offset + 3] = 255;
            } else {
                rgba[offset] = 20;
                rgba[offset + 1] = 20;
                rgba[offset + 2] = 20;
                rgba[offset + 3] = 255;
            }
            x += 1;
        }
        y += 1;
    }
    rgba
}

pub const fn solid_rgba<const N: usize>(color: [u8; 4]) -> [u8; N] {
    let mut rgba = [0_u8; N];
    let mut offset = 0;
    while offset < N {
        rgba[offset] = color[0];
        rgba[offset + 1] = color[1];
        rgba[offset + 2] = color[2];
        rgba[offset + 3] = color[3];
        offset += 4;
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::{CHECKERBOARD_MIP_RGBA, checkerboard_mip_levels};

    #[test]
    fn checkerboard_fallback_has_prepared_gamma_correct_mips() {
        let levels = checkerboard_mip_levels();

        assert_eq!(
            levels
                .iter()
                .map(|level| (level.width, level.height))
                .collect::<Vec<_>>(),
            vec![(8, 8), (4, 4), (2, 2), (1, 1)]
        );
        for level in &levels[1..] {
            assert!(
                level
                    .rgba
                    .chunks_exact(4)
                    .all(|pixel| pixel == CHECKERBOARD_MIP_RGBA)
            );
        }
    }
}
