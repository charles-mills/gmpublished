use super::resources::RenderResources;
use super::uniforms::MaterialUniform;
use super::{
    CHECKERBOARD_BYTES, CHECKERBOARD_MIP_1X1_BYTES, CHECKERBOARD_MIP_2X2_BYTES,
    CHECKERBOARD_MIP_4X4_BYTES, CHECKERBOARD_MIP_RGBA, CHECKERBOARD_SIZE, CHECKERBOARD_SIZE_USIZE,
    MaterialSlot, PHY_DEBUG_MATERIAL_NAME, RenderMode, ResolvedTexture, TextureUploadLevel, wgpu,
};

use super::images::{ImageLease, ImageView};
use super::samplers::TextureKind;
use crate::bridge::materials::{TextureReadTransfer, TextureRole};

pub(super) fn bc_upload_enabled(device: &wgpu::Device) -> bool {
    device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
}

fn mip_layout_is_valid(
    dimensions: impl IntoIterator<Item = [u32; 2]>,
    base: [u32; 2],
    count: usize,
    max_dimension: u32,
) -> bool {
    if base.contains(&0)
        || base[0].max(base[1]) > max_dimension
        || count == 0
        || count > (u32::BITS - base[0].max(base[1]).leading_zeros()) as usize
    {
        return false;
    }
    let mut seen = 0;
    for (level, dimensions) in dimensions.into_iter().enumerate() {
        if level >= count || dimensions != base.map(|side| (side >> level).max(1)) {
            return false;
        }
        seen += 1;
    }
    seen == count
}

#[derive(Clone, Copy)]
pub struct SampledTexture<'a> {
    pub view: &'a super::texture_upload::UploadedView,
    pub flags: Option<u32>,
}

pub struct BoundTexture {
    pub view: super::texture_upload::UploadedView,
    pub flags: Option<u32>,
}
impl BoundTexture {
    pub fn sampled(&self) -> SampledTexture<'_> {
        SampledTexture {
            view: &self.view,
            flags: self.flags,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum BaseTextureFrame {
    Base(u32),
    Base2(u32),
}
impl BaseTextureFrame {
    fn frame(self) -> u32 {
        match self {
            Self::Base(frame) | Self::Base2(frame) => frame,
        }
    }
    fn role(self) -> &'static str {
        match self {
            Self::Base(_) => "base",
            Self::Base2(_) => "base2",
        }
    }
}

#[derive(Clone, Copy)]
pub struct MaterialTextureViews<'a> {
    pub base: SampledTexture<'a>,
    pub base2: SampledTexture<'a>,
    pub normal: SampledTexture<'a>,
    pub normal2: SampledTexture<'a>,
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
    pub(super) fn normal(self) -> Option<&'a crate::bridge::materials::DecodedTextureFrames> {
        match self {
            Self::Water(water) => water.normal_map.as_deref(),
            Self::Mirror(mirror) => mirror.normal_map.as_deref(),
            Self::Surface(surface) => surface.normal.as_deref(),
            Self::Fallback => None,
        }
    }
    pub(super) fn normal2(self) -> Option<&'a crate::bridge::materials::DecodedTextureFrames> {
        self.surface()
            .and_then(|surface| surface.normal2.as_deref())
    }

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
    pub name: &'a str,
    pub material: PreparedMaterial<'a>,
    pub base_flags: Option<u32>,
    pub base2_flags: Option<u32>,
    pub render_mode: RenderMode,
    pub alpha_policy: crate::bridge::materials::TextureAlphaPolicy,
    pub(super) frames: Option<&'a super::frame_selection::MaterialFrames>,
}

fn normalize_auxiliary_frames(
    data: &mut super::surface::SurfaceUniform,
    surface: Option<&crate::bridge::materials::SurfaceMaterial>,
    frames: &super::frame_selection::MaterialFrames,
) {
    if let Some(surface) = surface {
        data.alpha_state[2] = if surface
            .normal2
            .as_ref()
            .is_some_and(|t| t.on_demand().is_some())
        {
            0
        } else {
            frames.normal2
        };
    }
    if let Some(surface) = surface
        && let Some(glass) = &surface.refraction
    {
        data.refraction_params[2] = if glass
            .tint_texture
            .as_ref()
            .is_some_and(|t| t.on_demand().is_some())
        {
            0.0
        } else {
            frames.refract_tint as f32
        };
        data.refraction_params[3] = if surface
            .normal2
            .as_ref()
            .is_some_and(|t| t.on_demand().is_some())
        {
            0.0
        } else {
            frames.normal2 as f32
        };
    }
}

#[derive(Debug)]
pub(super) struct MaterialBuffers {
    pub material: wgpu::Buffer,
    pub surface: wgpu::Buffer,
    pub model: wgpu::Buffer,
}
impl MaterialBuffers {
    pub fn write_tinted(
        &self,
        queue: &wgpu::Queue,
        slot: &MaterialSlot,
        frames: &super::frame_selection::MaterialFrames,
        tint: Option<[u8; 3]>,
    ) {
        let prepared = PreparedMaterial::from(&slot.shader);
        let surface = prepared.surface();
        let mut material = match prepared {
            PreparedMaterial::Surface(s) => {
                MaterialUniform::new(slot.render_mode, slot.alpha_policy).with_surface(s)
            }
            PreparedMaterial::Water(s) => {
                MaterialUniform::new(slot.render_mode, slot.alpha_policy).with_water(s)
            }
            PreparedMaterial::Mirror(s) => {
                MaterialUniform::new(slot.render_mode, slot.alpha_policy).with_mirror(s)
            }
            PreparedMaterial::Fallback => MaterialUniform::new(slot.render_mode, slot.alpha_policy),
        };
        if prepared
            .normal()
            .is_some_and(|source| source.on_demand().is_some())
        {
            material.water_animation[2..].fill(0.0);
            material.water_state[0] = 1.0;
        }
        let mut data = super::surface::SurfaceUniform::new(
            surface,
            slot.texture.as_ref().and_then(|t| t.flags()),
            slot.texture2
                .as_ref()
                .or(slot.texture.as_ref())
                .and_then(|t| t.flags()),
        );
        data.detail[1] = if surface
            .and_then(|s| s.detail.as_ref())
            .and_then(|d| d.texture.as_ref())
            .is_some_and(|t| t.on_demand().is_some())
        {
            0.0
        } else {
            frames.detail as f32
        };
        data.env_flags[2] = if surface
            .and_then(|s| s.envmap.as_ref())
            .and_then(|e| e.mask_texture.as_ref())
            .is_some_and(|t| t.on_demand().is_some())
        {
            0
        } else {
            frames.environment_mask
        };
        data.misc[2] = if prepared.normal().is_some_and(|t| t.on_demand().is_some()) {
            0.0
        } else {
            frames.normal as f32
        };
        normalize_auxiliary_frames(&mut data, surface, frames);
        if let Some(tint) = tint {
            let tint = tint.map(|value| f32::from(value) / 255.0);
            data.modulation[..3].copy_from_slice(&tint);
            data.model_modulation[..3].copy_from_slice(&tint);
        }
        queue.write_buffer(&self.material, 0, bytemuck::bytes_of(&material));
        queue.write_buffer(&self.surface, 0, bytemuck::bytes_of(&data));
        queue.write_buffer(
            &self.model,
            0,
            bytemuck::bytes_of(&super::model_lighting::ModelMaterialUniform::new(surface)),
        );
    }
}

impl RenderResources {
    pub(super) fn create_texture_frames_view(
        device: &wgpu::Device,
        uploader: &super::texture_upload::SharedUploader,
        frames: &[Vec<crate::bridge::materials::ResolvedTextureMip>],
        format: wgpu::TextureFormat,
    ) -> (
        super::texture_upload::UploadedView,
        super::texture_upload::UploadedTexture,
    ) {
        let limits = device.limits();
        let valid = frames
            .first()
            .and_then(|mips| mips.first())
            .is_some_and(|base| {
                frames.len() <= limits.max_texture_array_layers as usize
                    && frames.iter().all(|mips| {
                        mips.len() == frames[0].len()
                            && mip_layout_is_valid(
                                mips.iter().map(|mip| [mip.width, mip.height]),
                                [base.width, base.height],
                                mips.len(),
                                limits.max_texture_dimension_2d,
                            )
                            && mips.iter().all(|mip| {
                                TextureUploadLevel {
                                    rgba: &mip.rgba,
                                    width: mip.width,
                                    height: mip.height,
                                }
                                .is_valid()
                            })
                    })
            });
        let fallback;
        let frames = if valid {
            frames
        } else {
            log::warn!("invalid material texture array layout; using fallback texture");
            fallback = [vec![crate::bridge::materials::ResolvedTextureMip {
                rgba: WHITE_RGBA.to_vec(),
                width: 1,
                height: 1,
            }]];
            &fallback
        };
        let base = &frames[0][0];
        let mut uploader = uploader.lock().expect("texture uploader lock");
        let texture = uploader
            .allocate(&wgpu::TextureDescriptor {
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
            })
            .expect("validated generated texture allocation");
        for (layer, mips) in frames.iter().enumerate() {
            for (mip, image) in mips.iter().enumerate() {
                uploader
                    .write(
                        &texture,
                        mip as u32,
                        wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: layer as u32,
                        },
                        super::texture_upload::Input {
                            storage: crate::bridge::materials::TextureStorage::Rgba8,
                            dimensions: [image.width, image.height],
                            bytes: &image.rgba,
                        },
                    )
                    .expect("validated generated texture upload");
            }
        }
        drop(uploader);
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        (view, texture)
    }
    pub(super) fn create_slot_texture(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: &MaterialSlot,
        texture: Option<&ResolvedTexture>,
        images: &mut ImageLease<'_>,
        selection: BaseTextureFrame,
    ) -> BoundTexture {
        let Some(texture) = texture else {
            let view = if slot.name == PHY_DEBUG_MATERIAL_NAME {
                self.phy_debug_texture.clone()
            } else {
                self.error_texture.clone()
            };
            return BoundTexture { view, flags: None };
        };
        if texture.load_outcome().is_unavailable() {
            images.reject(
                &slot.name,
                selection.role(),
                format!("{:?}", texture.load_outcome()),
            );
            return BoundTexture {
                view: self.error_texture.clone(),
                flags: None,
            };
        }
        let surface = slot.shader.surface();
        let transfer = surface.map_or(
            TextureReadTransfer::SrgbToLinear,
            crate::bridge::materials::SurfaceMaterial::base_read_transfer,
        );
        let frame = selection.frame();
        let view = if let Some((_, recipe)) = texture.on_demand() {
            let mut request = recipe.clone();
            request.frame = frame;
            images
                .cached_page_view(&request, transfer, ImageView::D2(0))
                .expect("prepared base page")
        } else {
            let resident = texture.resident().expect("validated scene image residency");
            resident
                .layer_index(frame, 0)
                .ok_or(super::images::UploadError::InvalidView)
                .and_then(|layer| {
                    images.view(device, queue, resident, transfer, ImageView::D2(layer))
                })
        };
        match view {
            Ok(view) => BoundTexture {
                view,
                flags: texture.flags(),
            },
            Err(error) => {
                images.reject_upload(&slot.name, selection.role(), &error);
                BoundTexture {
                    view: self.error_texture.clone(),
                    flags: None,
                }
            }
        }
    }

    pub(super) fn create_data_frame_view(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &crate::bridge::materials::DecodedTextureFrames,
        frame: u32,
        role: TextureRole,
        images: &mut ImageLease<'_>,
        material_name: &str,
    ) -> Option<super::texture_upload::UploadedView> {
        if source.load_outcome().is_unavailable() {
            images.reject(
                material_name,
                &format!("{role:?}"),
                format!("{:?}", source.load_outcome()),
            );
            return None;
        }
        let view = if let Some((_, recipe)) = source.on_demand() {
            let mut request = recipe.clone();
            request.frame = frame;
            images
                .cached_page_view(&request, role.read_transfer(), ImageView::Array)
                .expect("prepared data page")
        } else {
            images.view(
                device,
                queue,
                source.resident().expect("validated scene image residency"),
                role.read_transfer(),
                ImageView::Array,
            )
        };
        match view {
            Ok(view) => Some(view),
            Err(error) => {
                images.reject_upload(material_name, &format!("{role:?}"), &error);
                None
            }
        }
    }

    pub(super) fn create_texture_view_from_levels_format(
        device: &wgpu::Device,
        uploader: &super::texture_upload::SharedUploader,
        label: &'static str,
        levels: &[TextureUploadLevel<'_>],
        format: wgpu::TextureFormat,
    ) -> (
        super::texture_upload::UploadedView,
        super::texture_upload::UploadedTexture,
    ) {
        let fallback_level = TextureUploadLevel {
            rgba: WHITE_RGBA.as_slice(),
            width: 1,
            height: 1,
        };
        let supplied_base = levels.first().copied();
        let use_supplied_chain = supplied_base.is_some_and(|base| {
            mip_layout_is_valid(
                levels.iter().map(|level| [level.width, level.height]),
                [base.width, base.height],
                levels.len(),
                device.limits().max_texture_dimension_2d,
            )
        }) && levels.iter().all(|level| level.is_valid());
        let base = if use_supplied_chain {
            supplied_base.unwrap_or(fallback_level)
        } else {
            log::warn!("invalid RGBA mip chain for {label}; using fallback texture");
            fallback_level
        };
        let mip_level_count = if use_supplied_chain {
            u32::try_from(levels.len())
                .expect("a resident RGBA mip slice cannot exceed u32::MAX elements")
                .max(1)
        } else {
            1
        };
        let mut uploader = uploader.lock().expect("texture uploader lock");
        let texture = uploader
            .allocate(&wgpu::TextureDescriptor {
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
            })
            .expect("validated generated mip allocation");
        if use_supplied_chain {
            let upload_mip_count = usize::try_from(mip_level_count)
                .expect("u32 texture mip counts fit usize on supported targets");
            for (mip_level, level) in levels.iter().take(upload_mip_count).enumerate() {
                uploader
                    .write(
                        &texture,
                        u32::try_from(mip_level).expect("validated texture mip counts fit u32"),
                        wgpu::Origin3d::ZERO,
                        super::texture_upload::Input {
                            storage: crate::bridge::materials::TextureStorage::Rgba8,
                            dimensions: [level.width, level.height],
                            bytes: level.rgba,
                        },
                    )
                    .expect("validated generated mip upload");
            }
        } else {
            uploader
                .write(
                    &texture,
                    0,
                    wgpu::Origin3d::ZERO,
                    super::texture_upload::Input {
                        storage: crate::bridge::materials::TextureStorage::Rgba8,
                        dimensions: [base.width, base.height],
                        bytes: base.rgba,
                    },
                )
                .expect("generated fallback upload");
        }
        drop(uploader);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (view, texture)
    }

    pub(super) fn create_material(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        views: MaterialTextureViews<'_>,
        mode: MaterialUploadMode<'_>,
        images: &mut ImageLease<'_>,
    ) -> (wgpu::BindGroup, wgpu::BindGroup, MaterialBuffers) {
        let frames = mode.frames;
        let material_name = mode.name;
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.model_viewer.material_uniform"),
            size: std::mem::size_of::<MaterialUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let surface = mode.material.surface();
        let mut material_uniform = match mode.material {
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
        if mode
            .material
            .normal()
            .is_some_and(|source| source.on_demand().is_some())
        {
            material_uniform.water_animation[2..].fill(0.0);
            material_uniform.water_state[0] = 1.0;
        }
        queue.write_buffer(&uniform, 0, bytemuck::bytes_of(&material_uniform));
        use wgpu::util::DeviceExt;
        let model_material = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("file_preview.model_material"),
            contents: bytemuck::bytes_of(&super::model_lighting::ModelMaterialUniform::new(
                surface,
            )),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let mut surface_data =
            super::surface::SurfaceUniform::new(surface, mode.base_flags, mode.base2_flags);
        if let Some(frames) = frames {
            normalize_auxiliary_frames(&mut surface_data, surface, frames);
            surface_data.detail[1] = if surface
                .and_then(|surface| surface.detail.as_ref())
                .and_then(|detail| detail.texture.as_ref())
                .is_some_and(|texture| texture.on_demand().is_some())
            {
                0.0
            } else {
                frames.detail as f32
            };
            surface_data.env_flags[2] = if surface
                .and_then(|surface| surface.envmap.as_ref())
                .and_then(|environment| environment.mask_texture.as_ref())
                .is_some_and(|texture| texture.on_demand().is_some())
            {
                0
            } else {
                frames.environment_mask
            };
            surface_data.misc[2] = if mode
                .material
                .normal()
                .is_some_and(|texture| texture.on_demand().is_some())
            {
                0.0
            } else {
                frames.normal as f32
            };
        }
        let surface_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("file_preview.material.surface_uniform"),
            contents: bytemuck::bytes_of(&surface_data),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let mut role_view = |source: Option<&crate::bridge::materials::DecodedTextureFrames>,
                             role: TextureRole,
                             frame: u32| {
            source.and_then(|source| {
                Self::create_data_frame_view(
                    device,
                    queue,
                    source,
                    frame,
                    role,
                    images,
                    material_name,
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
            frames.map_or(0, |frames| frames.detail),
        );
        let refract_tint = surface
            .and_then(|s| s.refraction.as_ref())
            .and_then(|g| g.tint_texture.as_deref());
        let refract_tint_view = role_view(
            refract_tint,
            TextureRole::Base {
                family: crate::bridge::materials::SurfaceFamily::Unlit,
            },
            frames.map_or(0, |f| f.refract_tint),
        );
        let blend_view = role_view(
            surface.and_then(|surface| surface.blend_modulation.as_deref()),
            TextureRole::BlendModulation,
            0,
        );
        let emission_view = role_view(
            surface
                .and_then(|surface| surface.emission.as_ref())
                .and_then(|emission| emission.mask.as_deref()),
            TextureRole::EmissionMask,
            0,
        );
        let envmap = surface.and_then(|surface| surface.envmap.as_deref());
        let cube_source = envmap.and_then(|envmap| match &envmap.source {
            crate::bridge::materials::EnvironmentMapSource::Named(cube) => Some(cube),
            crate::bridge::materials::EnvironmentMapSource::LocalProbe => None,
        });
        let envmap_mask_view = role_view(
            envmap.and_then(|envmap| envmap.mask_texture.as_deref()),
            TextureRole::EnvironmentMask,
            frames.map_or(0, |frames| frames.environment_mask),
        );
        let phong = surface.and_then(|surface| surface.phong.as_deref());
        let exponent_view = role_view(
            phong
                .and_then(|phong| phong.exponent_texture.as_ref())
                .and_then(|texture| texture.texture.as_deref()),
            TextureRole::PhongExponent,
            0,
        );
        let diffuse_warp_view = role_view(
            phong
                .and_then(|phong| phong.diffuse_warp.as_ref())
                .and_then(|texture| texture.texture.as_deref()),
            TextureRole::PhongWarp,
            0,
        );
        let specular_warp_view = role_view(
            phong
                .and_then(|phong| phong.specular_warp.as_ref())
                .and_then(|texture| texture.texture.as_deref()),
            TextureRole::PhongWarp,
            0,
        );
        let cube_view = cube_source.and_then(|cube| {
            if cube.load_outcome().is_unavailable() {
                images.reject(
                    material_name,
                    "environment",
                    format!("{:?}", cube.load_outcome()),
                );
                return None;
            }
            let view = if let Some((_, recipe)) = cube.on_demand() {
                let mut request = recipe.clone();
                request.frame = frames.map_or(cube.frame, |frames| frames.environment);
                images
                    .cached_page_view(
                        &request,
                        TextureReadTransfer::SrgbToLinear,
                        ImageView::Cube(0),
                    )
                    .expect("prepared environment page")
            } else {
                let resident = cube.resident().expect("validated scene image residency");
                let layer = resident
                    .layer_index(frames.map_or(cube.frame, |frames| frames.environment), 0)?;
                images.view(
                    device,
                    queue,
                    resident,
                    TextureReadTransfer::SrgbToLinear,
                    ImageView::Cube(layer),
                )
            };
            match view {
                Ok(view) => Some(view),
                Err(error) => {
                    images.reject_upload(material_name, "environment", &error);
                    None
                }
            }
        });
        let role_flags =
            |view: &Option<super::texture_upload::UploadedView>,
             source: Option<&crate::bridge::materials::DecodedTextureFrames>| {
                view.as_ref().and_then(|_| {
                    source.and_then(|source| {
                        source.source_metadata().and_then(|metadata| metadata.flags)
                    })
                })
            };
        let sampler = |flags, kind| self.samplers.texture(flags, kind);
        let base_sampler = sampler(views.base.flags, TextureKind::Texture2d);
        let base2_sampler = sampler(views.base2.flags, TextureKind::Texture2d);
        let normal_sampler = sampler(views.normal.flags, TextureKind::Texture2d);
        let normal2_sampler = sampler(views.normal2.flags, TextureKind::Texture2d);
        let detail_sampler = sampler(
            role_flags(
                &detail_view,
                detail.and_then(|detail| detail.texture.as_deref()),
            ),
            TextureKind::Texture2d,
        );
        let blend_sampler = sampler(
            role_flags(
                &blend_view,
                surface.and_then(|surface| surface.blend_modulation.as_deref()),
            ),
            TextureKind::Texture2d,
        );
        let emission_sampler = sampler(
            role_flags(
                &emission_view,
                surface
                    .and_then(|surface| surface.emission.as_ref())
                    .and_then(|emission| emission.mask.as_deref()),
            ),
            TextureKind::Texture2d,
        );
        let cube_sampler = sampler(
            cube_view.as_ref().and_then(|_| {
                cube_source
                    .and_then(|cube| cube.source_metadata().and_then(|metadata| metadata.flags))
            }),
            TextureKind::Cubemap,
        );
        let envmask_sampler = sampler(
            role_flags(
                &envmap_mask_view,
                envmap.and_then(|envmap| envmap.mask_texture.as_deref()),
            ),
            TextureKind::Texture2d,
        );
        let exponent_sampler = sampler(
            role_flags(
                &exponent_view,
                phong
                    .and_then(|phong| phong.exponent_texture.as_ref())
                    .and_then(|texture| texture.texture.as_deref()),
            ),
            TextureKind::Texture2d,
        );
        let diffuse_warp_sampler = sampler(
            role_flags(
                &diffuse_warp_view,
                phong
                    .and_then(|phong| phong.diffuse_warp.as_ref())
                    .and_then(|texture| texture.texture.as_deref()),
            ),
            TextureKind::Texture2d,
        );
        let specular_warp_sampler = sampler(
            role_flags(
                &specular_warp_view,
                phong
                    .and_then(|phong| phong.specular_warp.as_ref())
                    .and_then(|texture| texture.texture.as_deref()),
            ),
            TextureKind::Texture2d,
        );
        let refract_tint_sampler = sampler(
            role_flags(&refract_tint_view, refract_tint),
            TextureKind::Texture2d,
        );
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(views.base.view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(views.base2.view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&base_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::TextureView(views.normal.view),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: wgpu::BindingResource::TextureView(views.normal2.view),
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
                resource: wgpu::BindingResource::Sampler(&diffuse_warp_sampler),
            },
        ];
        entries.extend(
            [
                (21, &base2_sampler),
                (22, &normal_sampler),
                (23, &normal2_sampler),
                (24, &detail_sampler),
                (25, &blend_sampler),
                (26, &emission_sampler),
                (27, &envmask_sampler),
                (28, &exponent_sampler),
                (29, &specular_warp_sampler),
            ]
            .map(|(binding, sampler)| wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::Sampler(sampler),
            }),
        );
        entries.push(wgpu::BindGroupEntry {
            binding: 30,
            resource: wgpu::BindingResource::TextureView(
                refract_tint_view.as_ref().unwrap_or(&self.surface_white),
            ),
        });
        entries.push(wgpu::BindGroupEntry {
            binding: 31,
            resource: wgpu::BindingResource::Sampler(&refract_tint_sampler),
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.material"),
            layout: &self.material_layout,
            entries: &entries,
        });
        let water_entries = entries
            .into_iter()
            .filter(|entry| super::resources::water_material_binding(entry.binding))
            .collect::<Vec<_>>();
        let water_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("file_preview.water.material"),
            layout: &self.water_material_layout,
            entries: &water_entries,
        });
        (
            bind_group,
            water_bind_group,
            MaterialBuffers {
                material: uniform,
                surface: surface_uniform,
                model: model_material,
            },
        )
    }

    pub fn create_sky_material(
        &self,
        device: &wgpu::Device,
        view: &super::texture_upload::UploadedView,
        state: super::sky::SkyUniform,
        texture_flags: u32,
    ) -> wgpu::BindGroup {
        use wgpu::util::DeviceExt;
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("file_preview.sky.material_state"),
            contents: bytemuck::bytes_of(&state),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let sampler = self
            .samplers
            .texture(Some(texture_flags), TextureKind::Texture2d);
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
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
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
    fn upload_layout_rejects_misaddressed_and_excessive_levels_before_allocation() {
        use super::mip_layout_is_valid;
        assert!(mip_layout_is_valid(
            [[8, 2], [4, 1], [2, 1], [1, 1]],
            [8, 2],
            4,
            8192
        ));
        assert!(mip_layout_is_valid([[8, 2], [4, 1]], [8, 2], 2, 8192));
        assert!(!mip_layout_is_valid([[8, 2], [4, 2]], [8, 2], 2, 8192));
        assert!(!mip_layout_is_valid([[1, 1], [1, 1]], [1, 1], 2, 8192));
        assert!(!mip_layout_is_valid([[8, 2]], [8, 2], 2, 8192));
        assert!(!mip_layout_is_valid([[8, 2]], [8, 2], 1, 4));
        assert!(!mip_layout_is_valid([[0, 2]], [0, 2], 1, 8192));
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
