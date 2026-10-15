use super::resources::RenderResources;
use super::uniforms::MaterialUniform;
use super::{
    BcFormat, CHECKERBOARD_BYTES, CHECKERBOARD_MIP_1X1_BYTES, CHECKERBOARD_MIP_2X2_BYTES,
    CHECKERBOARD_MIP_4X4_BYTES, CHECKERBOARD_MIP_RGBA, CHECKERBOARD_SIZE, CHECKERBOARD_SIZE_USIZE,
    MaterialSlot, PHY_DEBUG_MATERIAL_NAME, PHY_DEBUG_RGBA, RenderMode, ResolvedBcMip,
    ResolvedTexture, TextureUploadLevel, bc_mip_is_valid, bc_texture_format, decode_bc_texture,
    wgpu, write_bc_texture_level, write_texture_level,
};

use crate::bridge::materials::{TextureReadTransfer, TextureRole};

fn transfer_format(transfer: TextureReadTransfer) -> wgpu::TextureFormat {
    match transfer {
        TextureReadTransfer::Unmodified => wgpu::TextureFormat::Rgba8Unorm,
        TextureReadTransfer::SrgbToLinear => wgpu::TextureFormat::Rgba8UnormSrgb,
    }
}

fn texture_format(role: TextureRole) -> wgpu::TextureFormat {
    transfer_format(role.read_transfer())
}

fn bc_transfer_format(format: BcFormat, transfer: TextureReadTransfer) -> wgpu::TextureFormat {
    match transfer {
        TextureReadTransfer::SrgbToLinear => bc_texture_format(format),
        TextureReadTransfer::Unmodified => match format {
            BcFormat::Bc1 => wgpu::TextureFormat::Bc1RgbaUnorm,
            BcFormat::Bc2 => wgpu::TextureFormat::Bc2RgbaUnorm,
            BcFormat::Bc3 => wgpu::TextureFormat::Bc3RgbaUnorm,
        },
    }
}

#[derive(Clone, Copy)]
pub struct MaterialTextureViews<'a> {
    pub base: &'a wgpu::TextureView,
    pub base2: &'a wgpu::TextureView,
    pub normal: &'a wgpu::TextureView,
    pub normal2: &'a wgpu::TextureView,
}

#[derive(Clone, Copy)]
pub enum PreparedMaterial<'a> {
    Fallback,
    Surface(&'a crate::bridge::materials::SurfaceMaterial),
    Water(&'a crate::bridge::materials::WaterMaterial),
    Mirror(&'a crate::bridge::materials::MirrorMaterial),
}

impl<'a> From<&'a crate::bridge::materials::MaterialShader> for PreparedMaterial<'a> {
    fn from(shader: &'a crate::bridge::materials::MaterialShader) -> Self {
        use crate::bridge::materials::MaterialShader;
        match shader {
            MaterialShader::Surface(surface) => Self::Surface(surface),
            MaterialShader::Water(water) => Self::Water(water),
            MaterialShader::Mirror(mirror) => Self::Mirror(mirror),
            MaterialShader::Unknown | MaterialShader::Unsupported(_) => Self::Fallback,
        }
    }
}

impl<'a> PreparedMaterial<'a> {
    fn surface(self) -> Option<&'a crate::bridge::materials::SurfaceMaterial> {
        if let Self::Surface(surface) = self {
            Some(surface)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy)]
pub struct MaterialUploadMode<'a> {
    pub material: PreparedMaterial<'a>,
    pub base_flags: Option<u32>,
    pub base2_flags: Option<u32>,
    pub render_mode: RenderMode,
    pub alpha_policy: crate::bridge::materials::TextureAlphaPolicy,
}

impl RenderResources {
    pub fn create_normal_view(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        normal: &crate::bridge::materials::DecodedTextureFrames,
    ) -> wgpu::TextureView {
        Self::create_texture_frames_view(
            device,
            queue,
            &normal.frames,
            texture_format(TextureRole::Normal),
        )
    }

    pub(super) fn create_texture_frames_view(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frames: &[Vec<crate::bridge::materials::ResolvedTextureMip>],
        format: wgpu::TextureFormat,
    ) -> wgpu::TextureView {
        let base = &frames[0][0];
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("file_preview.material.texture_frames"),
            size: wgpu::Extent3d {
                width: base.width,
                height: base.height,
                depth_or_array_layers: frames.len() as u32,
            },
            mip_level_count: frames[0].len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (layer, mips) in frames.iter().enumerate() {
            for (mip, image) in mips.iter().enumerate() {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: mip as u32,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: layer as u32,
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
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        })
    }
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
        self.create_material_texture_view_transfer(
            device,
            queue,
            label,
            texture,
            TextureReadTransfer::SrgbToLinear,
        )
    }

    fn create_material_texture_view_transfer(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        texture: Option<&ResolvedTexture>,
        transfer: TextureReadTransfer,
    ) -> wgpu::TextureView {
        let format = transfer_format(transfer);
        let Some(texture) = texture else {
            let levels = checkerboard_mip_levels();
            return self
                .create_texture_view_from_levels_format(device, queue, label, &levels, format);
        };
        if texture.is_water_fallback() {
            let level = TextureUploadLevel {
                rgba: texture.rgba_bytes().unwrap_or(WHITE_RGBA.as_slice()),
                width: texture.width.max(1),
                height: texture.height.max(1),
            };
            return self.create_texture_view_from_levels_format(
                device,
                queue,
                label,
                &[level],
                format,
            );
        }
        if let Some((format, mips)) = texture.bc_payload() {
            return self.create_bc_texture_view(device, queue, label, (format, mips), transfer);
        }

        let levels = texture
            .mip_chain()
            .map(|mip| TextureUploadLevel {
                rgba: mip.rgba,
                width: mip.width,
                height: mip.height,
            })
            .collect::<Vec<_>>();
        self.create_texture_view_from_levels_format(device, queue, label, &levels, format)
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
        let transfer = slot
            .shader
            .surface()
            .map_or(TextureReadTransfer::SrgbToLinear, |surface| {
                surface.base_read_transfer()
            });
        self.create_material_texture_view_transfer(device, queue, label, texture, transfer)
    }

    pub fn create_texture_view_from_levels(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        levels: &[TextureUploadLevel<'_>],
    ) -> wgpu::TextureView {
        self.create_texture_view_from_levels_format(
            device,
            queue,
            label,
            levels,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        )
    }

    pub fn create_texture_view_from_levels_format(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        levels: &[TextureUploadLevel<'_>],
        format: wgpu::TextureFormat,
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
            u32::try_from(levels.len())
                .expect("a resident RGBA mip slice cannot exceed u32::MAX elements")
                .max(1)
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
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        if use_supplied_chain {
            let upload_mip_count = usize::try_from(mip_level_count)
                .expect("u32 texture mip counts fit usize on supported targets");
            for (mip_level, level) in levels.iter().take(upload_mip_count).enumerate() {
                write_texture_level(
                    queue,
                    &texture,
                    u32::try_from(mip_level).expect("validated texture mip counts fit u32"),
                    *level,
                );
            }
        } else {
            write_texture_level(queue, &texture, 0, base);
        }
        texture.create_view(&wgpu::TextureViewDescriptor::default())
    }

    fn create_bc_texture_view(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        payload: (BcFormat, &[ResolvedBcMip]),
        transfer: TextureReadTransfer,
    ) -> wgpu::TextureView {
        let (format, mips) = payload;
        let format_rgba = transfer_format(transfer);
        let rgba_view = |rgba: &[u8], width: u32, height: u32| {
            self.create_texture_view_from_levels_format(
                device,
                queue,
                label,
                &[TextureUploadLevel {
                    rgba,
                    width,
                    height,
                }],
                format_rgba,
            )
        };
        let Some(base) = mips.first() else {
            return rgba_view(WHITE_RGBA.as_slice(), 1, 1);
        };
        if !device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            if let Some(rgba) = decode_bc_texture(format, base.width, base.height, &base.data) {
                return rgba_view(&rgba, base.width, base.height);
            }
            return rgba_view(WHITE_RGBA.as_slice(), 1, 1);
        }
        if !mips.iter().all(|mip| bc_mip_is_valid(format, mip)) {
            if let Some(rgba) = decode_bc_texture(format, base.width, base.height, &base.data) {
                return rgba_view(&rgba, base.width, base.height);
            }
            return rgba_view(WHITE_RGBA.as_slice(), 1, 1);
        }

        let mip_level_count = u32::try_from(mips.len())
            .expect("a resident BC mip slice cannot exceed u32::MAX elements")
            .max(1);
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
            format: bc_transfer_format(format, transfer),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let upload_mip_count = usize::try_from(mip_level_count)
            .expect("u32 texture mip counts fit usize on supported targets");
        for (mip_level, mip) in mips.iter().take(upload_mip_count).enumerate() {
            write_bc_texture_level(
                queue,
                &texture,
                u32::try_from(mip_level).expect("validated texture mip counts fit u32"),
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
        mode: MaterialUploadMode<'_>,
    ) -> (wgpu::BindGroup, wgpu::Buffer) {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.model_viewer.material_uniform"),
            size: std::mem::size_of::<MaterialUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let surface = mode.material.surface();
        let material_uniform = match mode.material {
            PreparedMaterial::Fallback => MaterialUniform::new(mode.render_mode, mode.alpha_policy),
            PreparedMaterial::Surface(surface) => {
                MaterialUniform::new(mode.render_mode, mode.alpha_policy).with_surface(surface)
            }
            PreparedMaterial::Water(water) => {
                MaterialUniform::new(mode.render_mode, mode.alpha_policy).with_water(water)
            }
            PreparedMaterial::Mirror(mirror) => {
                MaterialUniform::new(mode.render_mode, mode.alpha_policy).with_mirror(mirror)
            }
        };
        queue.write_buffer(&uniform, 0, bytemuck::bytes_of(&material_uniform));
        use wgpu::util::DeviceExt;
        let model_material = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("file_preview.model_material"),
            contents: bytemuck::bytes_of(&super::model_lighting::ModelMaterialUniform::new(
                surface,
            )),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let surface_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("file_preview.material.surface_uniform"),
            contents: bytemuck::bytes_of(&super::surface::SurfaceUniform::new(
                surface,
                mode.base_flags,
                mode.base2_flags,
            )),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let role_view = |source: Option<&crate::bridge::materials::DecodedTextureFrames>,
                         role: TextureRole| {
            source.map(|source| {
                Self::create_texture_frames_view(
                    device,
                    queue,
                    source.frames.as_slice(),
                    texture_format(role),
                )
            })
        };
        let detail = surface.and_then(|surface| surface.detail.as_ref());
        let detail_role = TextureRole::Detail {
            family: surface.map_or(
                crate::bridge::materials::SurfaceFamily::Lightmapped,
                |surface| surface.family,
            ),
            mode: surface
                .and_then(crate::bridge::materials::SurfaceMaterial::effective_detail_mode),
        };
        let detail_view = role_view(
            detail.and_then(|detail| detail.texture.as_deref()),
            detail_role,
        );
        let blend_view = role_view(
            surface.and_then(|surface| surface.blend_modulation.as_deref()),
            TextureRole::BlendModulation,
        );
        let emission_view = role_view(
            surface
                .and_then(|surface| surface.emission.as_ref())
                .and_then(|emission| emission.mask.as_deref()),
            TextureRole::EmissionMask,
        );
        let envmap = surface.and_then(|surface| surface.envmap.as_deref());
        let cube_view = envmap.and_then(|envmap| match &envmap.source {
            crate::bridge::materials::EnvironmentMapSource::Named(cube) => {
                Some(super::cubemap::create_view(device, queue, Some(cube)))
            }
            crate::bridge::materials::EnvironmentMapSource::LocalProbe => None,
        });
        let cube_sampler = super::cubemap::create_sampler(
            device,
            envmap
                .and_then(|envmap| match &envmap.source {
                    crate::bridge::materials::EnvironmentMapSource::Named(cube) => Some(cube.flags),
                    crate::bridge::materials::EnvironmentMapSource::LocalProbe => None,
                })
                .unwrap_or(0),
        );
        let envmap_mask_view = role_view(
            envmap.and_then(|envmap| envmap.mask_texture.as_deref()),
            TextureRole::EnvironmentMask,
        );
        let phong = surface.and_then(|surface| surface.phong.as_deref());
        let exponent_view = role_view(
            phong
                .and_then(|phong| phong.exponent_texture.as_ref())
                .and_then(|texture| texture.texture.as_deref()),
            TextureRole::PhongExponent,
        );
        let diffuse_warp_view = role_view(
            phong
                .and_then(|phong| phong.diffuse_warp.as_ref())
                .and_then(|texture| texture.texture.as_deref()),
            TextureRole::PhongWarp,
        );
        let specular_warp_view = role_view(
            phong
                .and_then(|phong| phong.specular_warp.as_ref())
                .and_then(|texture| texture.texture.as_deref()),
            TextureRole::PhongWarp,
        );
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
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(views.normal),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(views.normal2),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: surface_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: wgpu::BindingResource::TextureView(
                        detail_view.as_ref().unwrap_or(&self.surface_white),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(
                        blend_view.as_ref().unwrap_or(&self.surface_white),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::TextureView(
                        emission_view.as_ref().unwrap_or(&self.surface_white),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: wgpu::BindingResource::TextureView(
                        cube_view.as_ref().unwrap_or(&self.surface_black_cube),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 13,
                    resource: wgpu::BindingResource::TextureView(
                        envmap_mask_view.as_ref().unwrap_or(&self.surface_white),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 14,
                    resource: wgpu::BindingResource::Sampler(&cube_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 16,
                    resource: model_material.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 17,
                    resource: wgpu::BindingResource::TextureView(
                        exponent_view.as_ref().unwrap_or(&self.surface_white),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 18,
                    resource: wgpu::BindingResource::TextureView(
                        diffuse_warp_view.as_ref().unwrap_or(&self.surface_white),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 19,
                    resource: wgpu::BindingResource::TextureView(
                        specular_warp_view.as_ref().unwrap_or(&self.surface_white),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 20,
                    resource: wgpu::BindingResource::Sampler(&self.simple_sampler),
                },
            ],
        });
        (bind_group, uniform)
    }

    pub fn create_sky_material(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        state: super::sky::SkyUniform,
        texture_flags: u32,
    ) -> wgpu::BindGroup {
        use wgpu::util::DeviceExt;
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("file_preview.sky.material_state"),
            contents: bytemuck::bytes_of(&state),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        use vformats::vtf::texture_flags as flags;
        let address = |bit| {
            if texture_flags & bit != 0 {
                wgpu::AddressMode::ClampToEdge
            } else {
                wgpu::AddressMode::Repeat
            }
        };
        let filter = if texture_flags & flags::POINTSAMPLE != 0 {
            wgpu::FilterMode::Nearest
        } else {
            wgpu::FilterMode::Linear
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("file_preview.sky.stars_sampler"),
            address_mode_u: address(flags::CLAMPS),
            address_mode_v: address(flags::CLAMPT),
            mag_filter: filter,
            min_filter: filter,
            // ANISOTROPIC requests linear mip blending even at one anisotropic sample.
            mipmap_filter: if texture_flags & (flags::TRILINEAR | flags::ANISOTROPIC) != 0 {
                wgpu::FilterMode::Linear
            } else {
                wgpu::FilterMode::Nearest
            },
            ..Default::default()
        });
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
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        })
    }
}

pub static WHITE_RGBA: [u8; 4] = [255, 255, 255, 255];

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
    fn decal_modulate_keeps_encoded_channels_for_rgba_and_bc_textures() {
        use super::*;
        let role = TextureRole::Base {
            family: crate::bridge::materials::SurfaceFamily::DecalModulate,
        };
        let transfer = role.read_transfer();
        assert_eq!(texture_format(role), wgpu::TextureFormat::Rgba8Unorm);
        for format in [BcFormat::Bc1, BcFormat::Bc2, BcFormat::Bc3] {
            assert!(!bc_transfer_format(format, transfer).is_srgb());
            assert!(bc_transfer_format(format, TextureReadTransfer::SrgbToLinear).is_srgb());
        }
    }

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
