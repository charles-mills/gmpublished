use super::view_camera::{RasterClip, ViewCamera};
use super::{
    AMBIENT, Camera, FOV_Y, FlyCamera, MapFog, MapPreview, MapSkyCamera, Rectangle, RenderMode,
    RenderScene, ResolvedTexture, SOURCE_UP, Skybox, half_extent, look_at, mat_mul, mid,
    perspective, skybox_eye,
};
use crate::media::preview_model::{ModelPreview, ShadingMode};
use gmpublished_domain::math::Vec3;

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FogMode {
    #[default]
    Disabled = 0,
    Axial = 1,
    Height = 2,
    Immersion = 3,
    Radial = 4,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FogUniform {
    pub start: f32,
    pub end: f32,
    pub max_density: f32,
    pub mode: u32,
}

impl FogUniform {
    pub const DISABLED: Self = Self::new(0.0, 1.0, 0.0, FogMode::Disabled);

    pub const fn new(start: f32, end: f32, max_density: f32, mode: FogMode) -> Self {
        Self {
            start,
            end,
            max_density,
            mode: mode as u32,
        }
    }

    pub fn for_map(fog: Option<MapFog>, scale: f32) -> Self {
        fog.map_or(Self::DISABLED, |fog| {
            Self::new(
                fog.start * scale,
                fog.end * scale,
                fog.max_density,
                if fog.radial {
                    FogMode::Radial
                } else {
                    FogMode::Axial
                },
            )
        })
    }

    pub fn immersion(water: &crate::bridge::materials::WaterMaterial) -> Self {
        Self::new(
            water.fog_start,
            water.fog_end,
            f32::from(water.fog_enabled),
            FogMode::Immersion,
        )
    }

    pub fn height(water: &crate::bridge::materials::WaterMaterial) -> Self {
        Self::new(water.fog_start, water.fog_end, 1.0, FogMode::Height)
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LightmapMode {
    #[default]
    Ldr = 0,
    HdrFallback = 1,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniforms {
    pub view_proj: [[f32; 4]; 4],
    pub light: [f32; 4],
    pub camera_position: [f32; 4],
    pub fog_color: [f32; 4],
    pub fog: FogUniform,
    pub water_time_sky_tint: [f32; 4],
    pub water_depth_params: [f32; 4],
    /// World-space half-space retained by this view; zero disables clipping.
    pub clip_plane: [f32; 4],
    /// Exact optical plane for height fog; independent of clip bias.
    pub medium_plane: [f32; 4],
    pub viewport_size: [f32; 2],
    pub lightmap_mode: u32,
    pub shading_mode: u32,
    /// Physical clip-space bias, retained separately from projected texture UVs.
    pub raster_params: [f32; 4],
    pub raster_clip: RasterClip,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum BumpMode {
    #[default]
    None = 0,
    Tangent = 1,
    SsBump = 2,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PlanarKind {
    #[default]
    None = 0,
    Mirror = 1,
    TexturedWater = 2,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MaterialUniform {
    pub texture_alpha_opaque: u32,
    pub alpha_test: u32,
    pub bump_mode: u32,
    pub planar_kind: u32,
    pub unlit: u32,
    pub vertex_color: u32,
    pub vertex_alpha: u32,
    pub normal_animation_clamp: u32,
    pub water_fog: [f32; 4],
    pub water_optics: [f32; 4],
    pub water_reflect: [f32; 4],
    pub water_refract: [f32; 4],
    pub normal_s: [f32; 4],
    pub normal_t: [f32; 4],
    pub water_scroll: [f32; 4],
    pub water_animation: [f32; 4],
    pub water_state: [f32; 4],
}

impl MaterialUniform {
    pub fn with_surface(mut self, surface: &crate::bridge::materials::SurfaceMaterial) -> Self {
        self.unlit = u32::from(surface.family == crate::bridge::materials::SurfaceFamily::Unlit);
        self.vertex_color = u32::from(surface.effective_vertex_color());
        self.vertex_alpha = u32::from(surface.effective_vertex_alpha());
        self.bump_mode = if surface.normal.is_some() && surface.diffuse_bump {
            if surface.ssbump {
                BumpMode::SsBump
            } else {
                BumpMode::Tangent
            }
        } else {
            BumpMode::None
        } as u32;
        let (transform, _) = surface.texture_transform("$bumptransform", surface.transform);
        self.normal_s = transform[0];
        self.normal_t = transform[1];

        self
    }

    pub fn with_mirror(mut self, mirror: &crate::bridge::materials::MirrorMaterial) -> Self {
        self.planar_kind = PlanarKind::Mirror as u32;
        self.water_reflect = [
            mirror.reflect_tint_linear[0],
            mirror.reflect_tint_linear[1],
            mirror.reflect_tint_linear[2],
            f32::from(mirror.reflect_enabled),
        ];
        self.water_refract = [
            mirror.refract_tint_linear[0],
            mirror.refract_tint_linear[1],
            mirror.refract_tint_linear[2],
            f32::from(mirror.refract_enabled),
        ];
        self.water_optics = [mirror.reflect_amount, mirror.refract_amount, 0.0, 0.0];
        self.normal_s = mirror.bump_transform[0];
        self.normal_t = mirror.bump_transform[1];
        self.water_scroll = [
            mirror.min_reflectivity,
            mirror.max_reflectivity,
            mirror.fresnel_power,
            0.0,
        ];
        self.water_state = [
            mirror
                .normal_map
                .as_ref()
                .map_or(1.0, |normal| normal.frame_count() as f32),
            0.0,
            0.0,
            0.0,
        ];

        self
    }
    pub const fn new(
        render_mode: RenderMode,
        alpha_policy: crate::bridge::materials::TextureAlphaPolicy,
    ) -> Self {
        Self {
            texture_alpha_opaque: alpha_policy.force_opaque() as u32,
            alpha_test: matches!(render_mode, RenderMode::Cutout) as u32,
            bump_mode: BumpMode::None as u32,
            planar_kind: PlanarKind::None as u32,
            unlit: 0,
            vertex_color: 0,
            vertex_alpha: 0,
            normal_animation_clamp: 0,
            water_fog: [0.0; 4],
            water_optics: [0.0; 4],
            water_reflect: [0.0; 4],
            water_refract: [0.0; 4],
            normal_s: [0.0; 4],
            normal_t: [0.0; 4],
            water_scroll: [0.0; 4],
            water_animation: [0.0; 4],
            water_state: [0.0; 4],
        }
    }

    pub fn with_water(mut self, water: &crate::bridge::materials::WaterMaterial) -> Self {
        self.planar_kind = if water.has_base_texture {
            PlanarKind::TexturedWater
        } else {
            PlanarKind::None
        } as u32;
        self.normal_animation_clamp = u32::from(!water.animation_wrap);
        self.water_fog = [
            water.fog_color_linear[0],
            water.fog_color_linear[1],
            water.fog_color_linear[2],
            f32::from(water.fog_enabled),
        ];
        self.water_optics = [
            water.reflect_amount,
            water.refract_amount,
            water.fog_start,
            water.fog_end,
        ];
        self.water_reflect = [
            water.reflect_tint_linear[0],
            water.reflect_tint_linear[1],
            water.reflect_tint_linear[2],
            f32::from(water.reflect_enabled),
        ];
        self.water_refract = [
            water.refract_tint_linear[0],
            water.refract_tint_linear[1],
            water.refract_tint_linear[2],
            f32::from(water.refract_enabled),
        ];
        self.normal_s = water.bump_transform[0];
        self.normal_t = water.bump_transform[1];
        self.water_scroll = [
            water.scroll1[0],
            water.scroll1[1],
            water.scroll2[0],
            water.scroll2[1],
        ];
        self.water_animation = [
            water.proxy_scroll[0],
            water.proxy_scroll[1],
            water.bump_frame,
            water.frame_rate,
        ];
        self.water_state = [
            water
                .normal_map
                .as_ref()
                .map_or(1.0, |map| map.frame_count() as f32),
            f32::from(water.scroll1[0].abs() > 0.0 && !water.has_base_texture),
            f32::from(water.above_water),
            f32::from(water.no_fresnel),
        ];

        self
    }
}

impl Uniforms {
    pub fn apply_model_transform(&mut self, transform: [[f32; 4]; 4]) {
        self.view_proj = mat_mul(self.view_proj, transform);
        let scale_squared = (0..3)
            .map(|axis| transform[0][axis].powi(2))
            .sum::<f32>()
            .max(f32::MIN_POSITIVE);
        let inverse_vector = |vector: [f32; 3]| -> [f32; 3] {
            std::array::from_fn(|axis| {
                (0..3)
                    .map(|channel| transform[axis][channel] * vector[channel])
                    .sum::<f32>()
                    / scale_squared
            })
        };
        let eye = inverse_vector(std::array::from_fn(|axis| {
            self.camera_position[axis] - transform[3][axis]
        }));
        self.camera_position[..3].copy_from_slice(&eye);
        let scale = scale_squared.sqrt();
        self.fog.start /= scale;
        self.fog.end /= scale;
        let light = Vec3::from(inverse_vector([
            self.light[0],
            self.light[1],
            self.light[2],
        ]))
        .normalize()
        .unwrap_or(SOURCE_UP);
        self.light[..3].copy_from_slice(light.as_array());
    }

    pub(super) fn with_clip_camera(mut self, camera: Option<ViewCamera>) -> Self {
        self.raster_clip = camera.map_or_else(
            || RasterClip::world_space(self.clip_plane),
            |camera| {
                RasterClip::new(camera, self.clip_plane).unwrap_or_else(|error| {
                    log::error!(
                        "Source clip transform failed: {error:?}; rejecting clipped geometry"
                    );
                    RasterClip::world_space([0.0, 0.0, 0.0, -1.0])
                })
            },
        );
        self
    }

    pub fn bounds_query(&self) -> super::bounds_query::BoundsQuery {
        super::bounds_query::BoundsQuery::new(self)
    }

    /// Source's half-pixel shift uses physical render-target coordinates.
    pub fn with_raster_offset(mut self, size: (u32, u32), offset: [f32; 2]) -> Self {
        let clip_offset = [
            2.0 * offset[0] / size.0.max(1) as f32,
            -2.0 * offset[1] / size.1.max(1) as f32,
        ];
        for column in &mut self.view_proj {
            column[0] += clip_offset[0] * column[3];
            column[1] += clip_offset[1] * column[3];
        }
        self.viewport_size = [size.0 as f32, size.1 as f32];
        self.raster_params = [offset[0], offset[1], 0.0, 0.0];
        self
    }

    pub fn for_fly(
        scene: &MapPreview,
        camera: &FlyCamera,
        bounds: Rectangle,
        fog: Option<MapFog>,
        water_time: f32,
    ) -> Self {
        let frame = FlyCameraFrame::new(&scene.scene, camera, bounds);
        let view = frame.view_camera.view().expect("finite fly camera pose");
        let liquid = scene
            .water_volumes
            .as_ref()
            .and_then(|volumes| volumes.at(frame.eye));
        let water = scene.immersion_material(frame.eye);
        let (fog_color, fog_uniform) = water.map_or_else(
            || {
                (
                    fog.map_or([0.0; 4], |fog| {
                        [
                            fog.color_linear[0],
                            fog.color_linear[1],
                            fog.color_linear[2],
                            0.0,
                        ]
                    }),
                    FogUniform::for_map(fog, 1.0),
                )
            },
            |water| {
                (
                    [
                        water.scene_fog_color_linear[0],
                        water.scene_fog_color_linear[1],
                        water.scene_fog_color_linear[2],
                        1.0,
                    ],
                    FogUniform::immersion(water),
                )
            },
        );

        Self {
            view_proj: mat_mul(frame.proj, view),
            light: [0.4, 0.6, 0.8, AMBIENT],
            camera_position: [frame.eye[0], frame.eye[1], frame.eye[2], 0.0],
            fog_color,
            fog: fog_uniform,
            water_time_sky_tint: [water_time, 0.0, 0.0, 0.0],
            water_depth_params: [frame.near, frame.far, 0.0, 0.0],
            clip_plane: liquid
                .as_ref()
                .filter(|_| water.is_some_and(|w| w.refract_enabled && !w.force_cheap))
                .map_or([0.0; 4], |sample| {
                    let clip = super::water_clip::world_plane(
                        super::reflections::Plane {
                            normal: Vec3::new(0.0, 0.0, -1.0),
                            distance: -sample.body.surface_z,
                        },
                        super::water_clip::WorldWaterPass::Main,
                    );
                    [
                        clip.normal[0],
                        clip.normal[1],
                        clip.normal[2],
                        -clip.distance,
                    ]
                }),
            medium_plane: liquid.map_or([0.0; 4], |sample| [0.0, 0.0, 1.0, -sample.body.surface_z]),
            viewport_size: [0.0; 2],
            lightmap_mode: if scene.lightmap.as_ref().is_some_and(|atlas| {
                atlas.source == gmpublished_domain::scene::map::LightmapSource::Hdr
            }) {
                LightmapMode::HdrFallback as u32
            } else {
                LightmapMode::Ldr as u32
            },
            shading_mode: scene.shading_mode as u32,
            raster_params: [0.0; 4],
            raster_clip: RasterClip::default(),
        }
        .with_clip_camera(Some(frame.view_camera))
    }

    pub fn for_fly_sky(scene: &MapPreview, camera: &FlyCamera, bounds: Rectangle) -> Self {
        let frame = FlyCameraFrame::new(&scene.scene, camera, bounds);
        let mut view = frame.view_camera.view().expect("finite fly camera pose");
        view[3][0] = 0.0;
        view[3][1] = 0.0;
        view[3][2] = 0.0;

        Self {
            view_proj: mat_mul(frame.proj, view),
            light: [0.0; 4],
            camera_position: [frame.eye[0], frame.eye[1], frame.eye[2], 0.0],
            fog_color: [0.0; 4],
            fog: FogUniform::DISABLED,
            water_time_sky_tint: [camera.water_time, 0.0, 0.0, 0.0],
            water_depth_params: [frame.near, frame.far, 0.0, 0.0],
            clip_plane: [0.0; 4],
            medium_plane: [0.0; 4],
            viewport_size: [0.0; 2],
            lightmap_mode: LightmapMode::Ldr as u32,
            shading_mode: scene.shading_mode as u32,
            raster_params: [0.0; 4],
            raster_clip: RasterClip::default(),
        }
    }

    pub fn for_fly_skybox_composite(
        scene: &MapPreview,
        camera: &FlyCamera,
        bounds: Rectangle,
        sky_camera: MapSkyCamera,
        fog: Option<MapFog>,
    ) -> Self {
        let frame = FlyCameraFrame::new(&scene.scene, camera, bounds);
        let eye = skybox_eye(frame.eye, sky_camera.origin, sky_camera.scale);
        let view_camera = frame
            .view_camera
            .skybox(sky_camera.origin, sky_camera.scale);
        let view = view_camera.view().expect("finite sky camera pose");

        let fog_scale = if sky_camera.scale > 0.0 {
            1.0 / sky_camera.scale
        } else {
            1.0
        };
        Self {
            view_proj: mat_mul(view_camera.raster_projection(), view),
            light: [0.4, 0.6, 0.8, AMBIENT],
            camera_position: [eye[0], eye[1], eye[2], 0.0],
            fog_color: fog.map_or([0.0; 4], |fog| {
                [
                    fog.color_linear[0],
                    fog.color_linear[1],
                    fog.color_linear[2],
                    0.0,
                ]
            }),
            fog: FogUniform::for_map(fog, fog_scale),
            water_time_sky_tint: [camera.water_time, 0.0, 0.0, 0.0],
            water_depth_params: [view_camera.near, view_camera.far, 0.0, 0.0],
            clip_plane: [0.0; 4],
            medium_plane: [0.0; 4],
            viewport_size: [0.0; 2],
            lightmap_mode: if scene.lightmap.as_ref().is_some_and(|atlas| {
                atlas.source == gmpublished_domain::scene::map::LightmapSource::Hdr
            }) {
                LightmapMode::HdrFallback as u32
            } else {
                LightmapMode::Ldr as u32
            },
            shading_mode: scene.shading_mode as u32,
            raster_params: [0.0; 4],
            raster_clip: RasterClip::default(),
        }
    }

    pub fn for_model(
        model: &ModelPreview,
        camera: &Camera,
        bounds: Rectangle,
        framing_bounds: Option<crate::media::preview_model::ModelBounds>,
    ) -> Self {
        let bounds_min = framing_bounds.map_or(model.scene.bounds_min, |b| b.min);
        let bounds_max = framing_bounds.map_or(model.scene.bounds_max, |b| b.max);
        let center = mid(bounds_min, bounds_max);
        let radius = half_extent(bounds_min, bounds_max).max(1.0);
        let distance = radius * 2.2 * camera.orbit.distance();

        let eye = [
            center[0] + distance * camera.orbit.pitch().cos() * camera.orbit.yaw().sin(),
            center[1] + distance * camera.orbit.pitch().cos() * camera.orbit.yaw().cos(),
            center[2] + distance * camera.orbit.pitch().sin(),
        ];
        let view = look_at(Vec3::from(eye), center, SOURCE_UP);
        let aspect = (bounds.width / bounds.height.max(1.0)).max(0.1);
        let proj = perspective(FOV_Y, aspect, radius * 0.01, radius * 20.0 + distance);

        Self {
            view_proj: mat_mul(proj, view),
            light: [0.4, 0.6, 0.8, AMBIENT],
            camera_position: [eye[0], eye[1], eye[2], 0.0],
            fog_color: [0.0; 4],
            fog: FogUniform::DISABLED,
            water_time_sky_tint: [camera.animation_clock.time_seconds, 0.0, 0.0, 0.0],
            water_depth_params: [0.0; 4],
            clip_plane: [0.0; 4],
            medium_plane: [0.0; 4],
            viewport_size: [0.0; 2],
            lightmap_mode: LightmapMode::Ldr as u32,
            shading_mode: ShadingMode::Scene as u32,
            raster_params: [0.0; 4],
            raster_clip: RasterClip::default(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FlyCameraFrame {
    pub eye: Vec3,
    pub(super) view_camera: ViewCamera,
    pub proj: [[f32; 4]; 4],
    pub near: f32,
    pub far: f32,
}

impl FlyCameraFrame {
    pub fn new(scene: &RenderScene, camera: &FlyCamera, bounds: Rectangle) -> Self {
        let radius = half_extent(scene.bounds_min, scene.bounds_max).max(1.0);
        let eye = camera.position.map_or_else(
            || mid(scene.bounds_min, scene.bounds_max),
            |position| {
                position
                    + Vec3::from([
                        0.0,
                        0.0,
                        camera.view_bob_offset() + camera.duck_view_offset(),
                    ])
            },
        );
        let near = 4.0;
        let far = (radius * 6.0).max(30_000.0);
        let view_camera = ViewCamera::fly(camera, bounds, eye, near, far);
        Self {
            eye,
            view_camera,
            proj: view_camera.raster_projection(),
            near,
            far,
        }
    }
}

pub const DEFAULT_SKY_TINT: Vec3 = Vec3::new(0.12, 0.18, 0.24);

pub fn scene_sky_tint(skybox: Option<&Skybox>) -> Vec3 {
    let Some(skybox) = skybox else {
        return DEFAULT_SKY_TINT;
    };
    let mut sum = Vec3::splat(0.0);
    let mut count = 0_u32;
    for color in skybox
        .faces
        .iter()
        .filter_map(Option::as_deref)
        .filter_map(texture_smallest_mip_average)
    {
        for channel in 0..3 {
            sum[channel] += color[channel];
        }
        count += 1;
    }
    if count == 0 {
        DEFAULT_SKY_TINT
    } else {
        let scale = 1.0 / count as f32;
        sum.map(|channel| channel * scale)
    }
}

fn texture_smallest_mip_average(texture: &ResolvedTexture) -> Option<Vec3> {
    if texture.load_outcome().is_unavailable() {
        return None;
    }
    if let Some((format, mips)) = texture.bc_payload() {
        let mip = mips.last()?;
        return average_bc_rgba(
            format,
            mip.dimensions[0],
            mip.dimensions[1],
            mip.bytes.as_slice(),
        );
    }
    let mip = texture.mip_chain().last()?;
    average_srgb_rgba(mip.rgba, mip.width, mip.height)
}

fn average_bc_rgba(
    format: super::super::BcFormat,
    width: u32,
    height: u32,
    bytes: &[u8],
) -> Option<Vec3> {
    let mut sum = Vec3::ZERO;
    let count = super::super::texture::visit_bc_rgba(format, width, height, bytes, |row| {
        for pixel in row.chunks_exact(4) {
            for channel in 0..3 {
                sum[channel] += srgb_channel_to_linear(pixel[channel]);
            }
        }
    })?;
    Some(sum * (1.0 / count as f32))
}

pub fn average_srgb_rgba(rgba: &[u8], width: u32, height: u32) -> Option<Vec3> {
    let pixel_count = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?;
    if pixel_count == 0 || rgba.len() < pixel_count.checked_mul(4)? {
        return None;
    }
    let mut sum = Vec3::splat(0.0);
    for pixel in rgba.chunks_exact(4).take(pixel_count) {
        for channel in 0..3 {
            sum[channel] += srgb_channel_to_linear(pixel[channel]);
        }
    }
    let scale = 1.0 / pixel_count as f32;
    Some(sum.map(|channel| channel * scale))
}

fn srgb_channel_to_linear(channel: u8) -> f32 {
    let value = f32::from(channel) / 255.0;
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod uniform_layout_tests {
    use iced::wgpu::naga::{self, TypeInner};
    use std::mem::{offset_of, size_of};

    use super::{BumpMode, FogUniform, MaterialUniform, PlanarKind, Uniforms};

    fn pixel_position(matrix: [[f32; 4]; 4], point: [f32; 4], size: (u32, u32)) -> [f32; 3] {
        let clip = std::array::from_fn::<_, 4, _>(|row| {
            (0..4)
                .map(|column| matrix[column][row] * point[column])
                .sum::<f32>()
        });
        [
            (clip[0] / clip[3] * 0.5 + 0.5) * size.0 as f32,
            (0.5 - clip[1] / clip[3] * 0.5) * size.1 as f32,
            clip[2] / clip[3],
        ]
    }

    #[test]
    fn source_pixel_center_shift_is_half_a_physical_pixel_at_each_target_size() {
        let mut original = <Uniforms as bytemuck::Zeroable>::zeroed();
        original.view_proj = super::perspective(1.3, 16.0 / 9.0, 4.0, 30_000.0);
        original.lightmap_mode = super::LightmapMode::HdrFallback as u32;
        for size in [(320, 180), (640, 360), (1280, 720), (2560, 1440)] {
            let shifted = original.with_raster_offset(size, [0.5; 2]);
            assert_eq!(shifted.viewport_size, [size.0 as f32, size.1 as f32]);
            assert_eq!(shifted.lightmap_mode, original.lightmap_mode);
            for point in [
                [0.0, 0.0, -16.0, 1.0],
                [1.0, 2.0, -8.0, 1.0],
                [-100.0, -20.0, -512.0, 1.0],
            ] {
                let before = pixel_position(original.view_proj, point, size);
                let after = pixel_position(shifted.view_proj, point, size);
                assert!((after[0] - before[0] - 0.5).abs() < 0.0005);
                assert!((after[1] - before[1] - 0.5).abs() < 0.0005);
                assert_eq!(before[2], after[2]);
            }
        }
        assert_eq!(
            original.with_raster_offset((1280, 720), [0.0; 2]).view_proj,
            original.view_proj
        );
    }

    #[test]
    fn source_pixel_center_shift_applies_after_reflection_without_changing_its_clip_plane() {
        use super::super::reflections::Plane;
        use gmpublished_domain::math::Vec3;
        let mut original = <Uniforms as bytemuck::Zeroable>::zeroed();
        original.view_proj = super::perspective(1.3, 16.0 / 9.0, 4.0, 30_000.0);
        original.camera_position = [0.0, 0.0, 16.0, 0.0];
        let plane = Plane {
            normal: Vec3::new(0.0, 0.0, 1.0),
            distance: 3.0,
        };
        let reflected = plane.reflect_uniforms(original, Some(plane));
        let shifted = reflected.with_raster_offset((512, 288), [0.5; 2]);
        let point = [2.0, 3.0, -32.0, 1.0];
        let before = pixel_position(reflected.view_proj, point, (512, 288));
        let after = pixel_position(shifted.view_proj, point, (512, 288));
        assert!((after[0] - before[0] - 0.5).abs() < 0.0005);
        assert!((after[1] - before[1] - 0.5).abs() < 0.0005);
        assert_eq!(shifted.clip_plane, reflected.clip_plane);
        assert_eq!(shifted.camera_position, reflected.camera_position);
        assert_eq!(after[2], before[2]);
    }

    #[test]
    fn instance_blend_queue_does_not_reinterpret_opaque_texture_alpha() {
        use crate::bridge::materials::{RenderMode, TextureAlphaPolicy};
        let uniform = MaterialUniform::new(RenderMode::Translucent, TextureAlphaPolicy::Opaque);
        assert_eq!([uniform.texture_alpha_opaque, uniform.alpha_test], [1, 0]);
        let cutout = MaterialUniform::new(RenderMode::Cutout, TextureAlphaPolicy::Sampled);
        assert_eq!([cutout.texture_alpha_opaque, cutout.alpha_test], [0, 1]);
    }

    #[test]
    fn water_animation_clamp_has_no_surface_bump_mode() {
        use crate::bridge::materials::{MaterialResolver, RenderMode};
        let resolver = MaterialResolver::prop_lighting_fixture(
            Vec::new(),
            vec![("materials/pool.vmt", b"Water {}".to_vec())],
        );
        let resolved = resolver
            .resolve_primary(&[], "pool")
            .into_material()
            .unwrap();
        let mut water = resolved.shader.water().unwrap().clone();
        water.animation_wrap = false;
        let material = MaterialUniform::new(
            RenderMode::Opaque,
            RenderMode::Opaque.texture_alpha_policy(),
        )
        .with_water(&water);
        assert_eq!(material.bump_mode, BumpMode::None as u32);
        assert_eq!(material.normal_animation_clamp, 1);
        water.animation_wrap = true;
        assert_eq!(
            MaterialUniform::new(
                RenderMode::Opaque,
                RenderMode::Opaque.texture_alpha_policy()
            )
            .with_water(&water)
            .normal_animation_clamp,
            0
        );
    }

    #[test]
    fn unlit_flags_are_separate_from_water_and_bumped_lighting_flags() {
        use crate::bridge::materials::{RenderMode, SurfaceFamily, SurfaceMaterial};
        let unlit = SurfaceMaterial {
            shader: "UnlitGeneric".into(),
            family: SurfaceFamily::Unlit,
            vertex_color: true,
            vertex_alpha: true,
            ..SurfaceMaterial::default()
        };
        let original = MaterialUniform::new(
            RenderMode::Translucent,
            RenderMode::Translucent.texture_alpha_policy(),
        );
        let result = original.with_surface(&unlit);
        assert_eq!(
            [
                result.texture_alpha_opaque,
                result.alpha_test,
                result.bump_mode,
                result.planar_kind
            ],
            [
                original.texture_alpha_opaque,
                original.alpha_test,
                original.bump_mode,
                original.planar_kind
            ]
        );
        assert_eq!(
            [
                result.unlit,
                result.vertex_color,
                result.vertex_alpha,
                result.normal_animation_clamp
            ],
            [1, 1, 1, 0]
        );
        let opaque = MaterialUniform::new(
            RenderMode::Opaque,
            RenderMode::Opaque.texture_alpha_policy(),
        );
        assert_eq!(
            [
                opaque.unlit,
                opaque.vertex_color,
                opaque.vertex_alpha,
                opaque.normal_animation_clamp
            ],
            [0; 4]
        );
    }

    #[test]
    fn surface_mark_fades_have_an_independent_gpu_contract() {
        use crate::media::preview_model::ModelVertex;
        let attribute = super::super::super::MODEL_VERTEX_ATTRIBUTES
            .iter()
            .find(|v| v.shader_location == 14)
            .unwrap();
        assert_eq!(
            attribute.offset as usize,
            offset_of!(ModelVertex, surface_mark)
        );
        assert_eq!(attribute.format, iced::wgpu::VertexFormat::Uint32);
        assert_ne!(
            offset_of!(ModelVertex, surface_mark),
            offset_of!(ModelVertex, lighting_context)
        );
        assert_eq!(size_of::<super::super::marks::GpuSurfaceMark>(), 32);
    }

    #[test]
    fn vertex_opacity_has_its_own_gpu_attribute() {
        use crate::media::preview_model::ModelVertex;
        let attribute = super::super::super::MODEL_VERTEX_ATTRIBUTES
            .iter()
            .find(|v| v.shader_location == 7)
            .unwrap();
        assert_eq!(attribute.offset as usize, offset_of!(ModelVertex, opacity));
        assert_eq!(attribute.format, iced::wgpu::VertexFormat::Float32);
        assert_ne!(
            offset_of!(ModelVertex, opacity),
            offset_of!(ModelVertex, blend_alpha)
        );
        assert_eq!(
            offset_of!(ModelVertex, illumination),
            attribute.offset as usize + size_of::<f32>()
        );
    }

    #[test]
    fn explicit_vertex_illumination_has_independent_gpu_attributes() {
        use crate::media::preview_model::ModelVertex;
        let attributes = &super::super::super::MODEL_VERTEX_ATTRIBUTES;
        let illumination = attributes
            .iter()
            .find(|attribute| attribute.shader_location == 8)
            .unwrap();
        let mode = attributes
            .iter()
            .find(|attribute| attribute.shader_location == 9)
            .unwrap();
        assert_eq!(
            illumination.offset as usize,
            offset_of!(ModelVertex, illumination)
        );
        assert_eq!(illumination.format, iced::wgpu::VertexFormat::Float32x3);
        assert_eq!(mode.offset as usize, offset_of!(ModelVertex, lighting_mode));
        assert_eq!(mode.format, iced::wgpu::VertexFormat::Uint32);
        assert_eq!(mode.offset, illumination.offset + 12);
        assert_eq!(offset_of!(ModelVertex, tangent_s), mode.offset as usize + 4);
        for (location, offset) in [
            (10, offset_of!(ModelVertex, tangent_s)),
            (11, offset_of!(ModelVertex, tangent_t)),
        ] {
            let attribute = attributes
                .iter()
                .find(|a| a.shader_location == location)
                .unwrap();
            assert_eq!(attribute.offset as usize, offset);
            assert_eq!(attribute.format, iced::wgpu::VertexFormat::Float32x3);
        }
        assert_eq!(
            size_of::<ModelVertex>(),
            offset_of!(ModelVertex, lightmap_page) + 4
        );
        let sign = attributes.iter().find(|a| a.shader_location == 12).unwrap();
        assert_eq!(sign.offset as usize, offset_of!(ModelVertex, tangent_sign));
        assert_eq!(sign.offset, offset_of!(ModelVertex, tangent_t) as u64 + 12);
        let context = attributes
            .iter()
            .find(|attribute| attribute.shader_location == 13)
            .unwrap();
        assert_eq!(
            context.offset as usize,
            offset_of!(ModelVertex, lighting_context)
        );
        assert_eq!(context.format, iced::wgpu::VertexFormat::Uint32);
        assert_eq!(context.offset, sign.offset + 4);
        assert_eq!(sign.format, iced::wgpu::VertexFormat::Float32);
    }

    #[test]
    fn shaders_validate_and_match_the_uploaded_uniform_layout() {
        let fields = [
            ("view_proj", offset_of!(Uniforms, view_proj)),
            ("light", offset_of!(Uniforms, light)),
            ("camera_position", offset_of!(Uniforms, camera_position)),
            ("fog_color", offset_of!(Uniforms, fog_color)),
            ("fog", offset_of!(Uniforms, fog)),
            (
                "water_time_sky_tint",
                offset_of!(Uniforms, water_time_sky_tint),
            ),
            (
                "water_depth_params",
                offset_of!(Uniforms, water_depth_params),
            ),
            ("clip_plane", offset_of!(Uniforms, clip_plane)),
            ("medium_plane", offset_of!(Uniforms, medium_plane)),
            ("viewport_size", offset_of!(Uniforms, viewport_size)),
            ("lightmap_mode", offset_of!(Uniforms, lightmap_mode)),
            ("shading_mode", offset_of!(Uniforms, shading_mode)),
            ("raster_params", offset_of!(Uniforms, raster_params)),
            ("raster_clip", offset_of!(Uniforms, raster_clip)),
        ];
        for (name, source) in [
            ("model_viewer", super::super::super::MODEL_SHADER_SOURCE),
            ("water", super::super::super::WATER_SHADER_SOURCE),
            ("detail", super::super::super::DETAIL_SHADER_SOURCE),
            ("sky", super::super::super::SKY_SHADER_SOURCE),
        ] {
            let module = naga::front::wgsl::parse_str(source)
                .unwrap_or_else(|error| panic!("{name}: {}", error.emit_to_string(source)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
            if name == "model_viewer" {
                let output = module
                    .types
                    .iter()
                    .find_map(|(_, ty)| {
                        (ty.name.as_deref() == Some("VertexOutput")).then_some(&ty.inner)
                    })
                    .unwrap();
                let TypeInner::Struct { members, .. } = output else {
                    panic!("vertex output struct");
                };
                let mut variables = 0usize;
                let mut components = 0usize;
                for member in members {
                    if let Some(naga::Binding::Location { location, .. }) = member.binding {
                        assert!(location < 16, "portable inter-stage location budget");
                        variables += 1;
                        components += match module.types[member.ty].inner {
                            TypeInner::Scalar(_) => 1,
                            TypeInner::Vector { size, .. } => size as usize,
                            _ => panic!("unexpected vertex varying type"),
                        };
                    }
                }
                assert!(
                    variables <= 16 && components <= 60,
                    "portable inter-stage budget"
                );
            }
            let uniform = module
                .types
                .iter()
                .find_map(|(_, ty)| (ty.name.as_deref() == Some("Uniforms")).then_some(&ty.inner))
                .expect("Uniforms declaration");
            let TypeInner::Struct { members, span } = uniform else {
                panic!("{name}: Uniforms must be a struct");
            };
            assert_eq!(*span as usize, size_of::<Uniforms>(), "{name}: size");
            assert_eq!(members.len(), fields.len(), "{name}: fields");
            for (member, (field, offset)) in members.iter().zip(fields) {
                assert_eq!(member.name.as_deref(), Some(field), "{name}: field name");
                assert_eq!(member.offset as usize, offset, "{name}: {field} offset");
            }
            let fog = module
                .types
                .iter()
                .find_map(|(_, ty)| (ty.name.as_deref() == Some("FogUniform")).then_some(&ty.inner))
                .unwrap();
            let TypeInner::Struct { members, span } = fog else {
                panic!("FogUniform must be a struct")
            };
            assert_eq!(*span as usize, size_of::<FogUniform>());
            assert_eq!(*span, 16);
            assert_eq!(members.len(), 4);
            for (member, (field, offset)) in members.iter().zip([
                ("start", offset_of!(FogUniform, start)),
                ("end", offset_of!(FogUniform, end)),
                ("max_density", offset_of!(FogUniform, max_density)),
                ("mode", offset_of!(FogUniform, mode)),
            ]) {
                assert_eq!(member.name.as_deref(), Some(field));
                assert_eq!(member.offset as usize, offset);
            }
            let clip = module
                .types
                .iter()
                .find_map(|(_, ty)| (ty.name.as_deref() == Some("RasterClip")).then_some(&ty.inner))
                .unwrap();
            let TypeInner::Struct { members, span } = clip else {
                panic!("RasterClip must be a struct")
            };
            assert_eq!(*span as usize, size_of::<super::RasterClip>());
            assert_eq!(members.len(), 2);
            for (member, (field, offset)) in members.iter().zip([
                (
                    "view_projection",
                    offset_of!(super::RasterClip, view_projection),
                ),
                ("plane", offset_of!(super::RasterClip, plane)),
            ]) {
                assert_eq!(member.name.as_deref(), Some(field));
                assert_eq!(member.offset as usize, offset);
            }
            if matches!(name, "water" | "model_viewer" | "detail") {
                let material = module
                    .types
                    .iter()
                    .find_map(|(_, ty)| {
                        (ty.name.as_deref() == Some("Material")).then_some(&ty.inner)
                    })
                    .unwrap();
                let TypeInner::Struct { members, span } = material else {
                    panic!("Material must be a struct")
                };
                assert_eq!(*span as usize, size_of::<MaterialUniform>());
                for (name, value) in [
                    ("BUMP_NONE", BumpMode::None as u32),
                    ("BUMP_TANGENT", BumpMode::Tangent as u32),
                    ("BUMP_SSBUMP", BumpMode::SsBump as u32),
                    ("PLANAR_NONE", PlanarKind::None as u32),
                    ("PLANAR_MIRROR", PlanarKind::Mirror as u32),
                    ("PLANAR_TEXTURED_WATER", PlanarKind::TexturedWater as u32),
                ] {
                    let constant = module
                        .constants
                        .iter()
                        .find_map(|(_, constant)| {
                            (constant.name.as_deref() == Some(name)).then_some(constant)
                        })
                        .unwrap();
                    assert!(
                        matches!(module.global_expressions[constant.init], naga::Expression::Literal(naga::Literal::U32(actual)) if actual==value),
                        "{name}"
                    );
                }

                assert_eq!(members.len(), 17, "{name}: material field count");
                for (member, (field, offset)) in members.iter().zip([
                    (
                        "texture_alpha_opaque",
                        offset_of!(MaterialUniform, texture_alpha_opaque),
                    ),
                    ("alpha_test", offset_of!(MaterialUniform, alpha_test)),
                    ("bump_mode", offset_of!(MaterialUniform, bump_mode)),
                    ("planar_kind", offset_of!(MaterialUniform, planar_kind)),
                    ("unlit", offset_of!(MaterialUniform, unlit)),
                    ("vertex_color", offset_of!(MaterialUniform, vertex_color)),
                    ("vertex_alpha", offset_of!(MaterialUniform, vertex_alpha)),
                    (
                        "normal_animation_clamp",
                        offset_of!(MaterialUniform, normal_animation_clamp),
                    ),
                    ("water_fog", offset_of!(MaterialUniform, water_fog)),
                    ("water_optics", offset_of!(MaterialUniform, water_optics)),
                    ("water_reflect", offset_of!(MaterialUniform, water_reflect)),
                    ("water_refract", offset_of!(MaterialUniform, water_refract)),
                    ("normal_s", offset_of!(MaterialUniform, normal_s)),
                    ("normal_t", offset_of!(MaterialUniform, normal_t)),
                    ("water_scroll", offset_of!(MaterialUniform, water_scroll)),
                    (
                        "water_animation",
                        offset_of!(MaterialUniform, water_animation),
                    ),
                    ("water_state", offset_of!(MaterialUniform, water_state)),
                ]) {
                    assert_eq!(member.name.as_deref(), Some(field));
                    assert_eq!(member.offset as usize, offset, "{field} offset");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use iced::Point;

    use super::super::super::test_support::empty_preview;
    use super::{
        FlyCamera, FogMode, FogUniform, MapFog, MapSkyCamera, Rectangle, Uniforms, Vec3,
        average_srgb_rgba,
    };

    #[test]
    fn live_uniform_transform_preserves_world_camera_and_light() {
        let scene = empty_preview(Vec3::splat(-128.0), Vec3::splat(128.0));
        let camera = FlyCamera::default();
        let mut uniforms = Uniforms::for_fly(
            &scene,
            &camera,
            Rectangle::new(Point::ORIGIN, iced::Size::new(800.0, 600.0)),
            None,
            0.0,
        );
        let original_projection = uniforms.view_proj;
        uniforms.camera_position = [8.0, 14.0, 20.0, 0.0];
        uniforms.light = [0.0, 1.0, 0.0, 0.35];
        uniforms.fog = FogUniform::new(100.0, 1000.0, 0.5, FogMode::Axial);
        let transform = [
            [0.0, 2.0, 0.0, 0.0],
            [-2.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 2.0, 0.0],
            [10.0, 10.0, 10.0, 1.0],
        ];
        uniforms.apply_model_transform(transform);
        assert_eq!(uniforms.camera_position, [2.0, 1.0, 5.0, 0.0]);
        assert_eq!(uniforms.light, [1.0, 0.0, 0.0, 0.35]);
        assert_eq!(
            uniforms.fog,
            FogUniform::new(50.0, 500.0, 0.5, FogMode::Axial)
        );
        let local_point = [2.0, 1.0, 5.0, 1.0];
        let world_point = [8.0, 14.0, 20.0, 1.0];
        let project = |matrix: [[f32; 4]; 4], point: [f32; 4]| -> [f32; 4] {
            std::array::from_fn(|row| {
                matrix
                    .iter()
                    .zip(point)
                    .map(|(col, value)| col[row] * value)
                    .sum()
            })
        };
        for (local_clip, world_clip) in project(uniforms.view_proj, local_point)
            .into_iter()
            .zip(project(original_projection, world_point))
        {
            assert!((local_clip - world_clip).abs() < 0.0001);
        }
    }

    #[test]
    fn map_shading_mode_is_shared_by_main_and_sky_views() {
        let mut scene = empty_preview(Vec3::splat(-128.0), Vec3::splat(128.0));
        let camera = FlyCamera {
            position: Some(Vec3::ZERO),
            ..FlyCamera::default()
        };
        let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(1280.0, 720.0));
        let sky = MapSkyCamera {
            origin: Vec3::ZERO,
            scale: 16.0,
            fog: None,
        };
        for mode in [super::ShadingMode::Scene, super::ShadingMode::Fullbright] {
            scene.shading_mode = mode;
            assert_eq!(
                Uniforms::for_fly(&scene, &camera, bounds, None, 0.0).shading_mode,
                mode as u32
            );
            assert_eq!(
                Uniforms::for_fly_sky(&scene, &camera, bounds).shading_mode,
                mode as u32
            );
            assert_eq!(
                Uniforms::for_fly_skybox_composite(&scene, &camera, bounds, sky, None).shading_mode,
                mode as u32
            );
        }
    }

    #[test]
    fn bounded_bc_sky_reduction_preserves_full_decode_average_exactly() {
        use vformats::vtf::BcFormat;
        for format in [BcFormat::Bc1, BcFormat::Bc2, BcFormat::Bc3] {
            let [width, height] = [130_u32, 9];
            let bytes = (0..width.div_ceil(4) * height.div_ceil(4) * format.block_bytes())
                .map(|value| value as u8)
                .collect::<Vec<_>>();
            let decoded = vformats::vtf::decode_bc_rgba(
                format,
                width,
                height,
                &bytes,
                &vformats::Limits::default(),
            )
            .unwrap();
            assert_eq!(
                super::average_bc_rgba(format, width, height, &bytes),
                average_srgb_rgba(&decoded.rgba, width, height)
            );
        }
    }

    #[test]
    fn sky_tint_averages_known_2x2_texture() {
        let rgba = [
            0, 0, 0, 255, 255, 255, 255, 255, 255, 0, 0, 255, 0, 0, 255, 255,
        ];

        let tint = average_srgb_rgba(&rgba, 2, 2).expect("valid texture");

        assert!((tint[0] - 0.5).abs() < 1e-6);
        assert!((tint[1] - 0.25).abs() < 1e-6);
        assert!((tint[2] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn immersion_uses_the_containing_body_and_its_underside_fog() {
        use crate::bridge::{gma::PreviewArchive, materials::MaterialResolver};
        use crate::media::preview_model::MaterialSlot;
        use crate::test_support::GmaFixtureBuilder;
        use gmpublished_domain::scene::map::MapWaterVolumes;
        use std::sync::Arc;
        let archive = PreviewArchive::from_gma(GmaFixtureBuilder::new("Liquid")
            .entry("materials/pool.vmt",br#"Water { "$bottommaterial" "pool_beneath" "$fogend" "9999" }"#.to_vec())
            .entry("materials/pool_beneath.vmt",br#"Water { "$abovewater" "0" "$fogenable" "1" "$fogcolor" "[0.2 0.3 0.4]" "$fogstart" "-64" "$fogend" "640" "$refracttexture" "_rt_WaterRefraction" }"#.to_vec())
            .build()).unwrap();
        let resolver = MaterialResolver::new(Arc::new(archive), None);
        let mut scene = empty_preview(Vec3::splat(-128.0), Vec3::splat(128.0));
        scene.water_volumes = Some(MapWaterVolumes::box_for_tests(
            Vec3::splat(-128.0),
            Vec3::splat(128.0),
            "pool".into(),
        ));
        for name in ["pool", "pool_beneath"] {
            let r = resolver.resolve_primary(&[], name).into_material().unwrap();
            Arc::get_mut(&mut scene.scene)
                .unwrap()
                .materials
                .push(MaterialSlot {
                    tint_base: None,
                    resolution: crate::bridge::materials::MaterialResolutionOutcome::Loaded,
                    canonical_name: Some(name.into()),
                    name: name.into(),
                    shader: r.shader,
                    texture: r.texture,
                    texture2: None,
                    render_mode: r.render_mode,
                    alpha_policy: r.render_mode.texture_alpha_policy(),
                });
        }
        let map_fog = MapFog {
            color_linear: Vec3::new(0.8, 0.7, 0.6),
            start: 512.0,
            end: 8192.0,
            max_density: 0.5,
            radial: false,
        };
        let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(800.0, 600.0));
        let mut camera = FlyCamera {
            position: Some(Vec3::new(0.0, 0.0, 160.0)),
            ..FlyCamera::default()
        };
        let above = Uniforms::for_fly(&scene, &camera, bounds, Some(map_fog), 12.0);
        let radial = Uniforms::for_fly(
            &scene,
            &camera,
            bounds,
            Some(MapFog {
                radial: true,
                ..map_fog
            }),
            12.0,
        );
        assert_eq!(
            radial.fog,
            FogUniform::new(512.0, 8192.0, 0.5, FogMode::Radial)
        );
        camera.position = Some(Vec3::ZERO);
        let submerged = Uniforms::for_fly(&scene, &camera, bounds, Some(map_fog), 12.0);
        assert_eq!(above.fog_color, [0.8, 0.7, 0.6, 0.0]);
        assert_eq!(
            above.fog,
            FogUniform::new(512.0, 8192.0, 0.5, FogMode::Axial)
        );
        assert_eq!(
            submerged.fog_color,
            [0.033104762, 0.07227186, 0.13286833, 1.0]
        );
        assert_eq!(
            submerged.fog,
            FogUniform::new(-64.0, 640.0, 1.0, FogMode::Immersion)
        );
        assert_eq!(submerged.medium_plane, [0.0, 0.0, 1.0, -128.0]);
        assert_eq!(submerged.clip_plane, [0.0, 0.0, -1.0, 130.0]);
        camera.position = Some(Vec3::new(256.0, 0.0, 0.0));
        assert_eq!(
            Uniforms::for_fly(&scene, &camera, bounds, Some(map_fog), 12.0).fog,
            above.fog
        );
    }
    #[test]
    fn skybox_fog_distances_follow_the_miniature_view_scale() {
        use super::*;
        let scene = empty_preview(Vec3::splat(-128.0), Vec3::splat(128.0));
        let camera = FlyCamera {
            position: Some(Vec3::ZERO),
            ..FlyCamera::default()
        };
        let fog = MapFog {
            color_linear: Vec3::splat(0.2),
            start: -4000.0,
            end: 320000.0,
            max_density: 0.5,
            radial: true,
        };
        let sky = MapSkyCamera {
            origin: Vec3::new(0.0, 0.0, 1024.0),
            scale: 16.0,
            fog: Some(fog),
        };
        let bounds = Rectangle::new(iced::Point::ORIGIN, iced::Size::new(800.0, 600.0));
        let uniforms = Uniforms::for_fly_skybox_composite(&scene, &camera, bounds, sky, Some(fog));
        assert_eq!(
            uniforms.fog,
            FogUniform::new(-250.0, 20000.0, 0.5, FogMode::Radial)
        );
        assert_eq!(
            Uniforms::for_fly(&scene, &camera, bounds, Some(fog), 0.0).fog,
            FogUniform::new(-4000.0, 320000.0, 0.5, FogMode::Radial)
        );
    }
}
