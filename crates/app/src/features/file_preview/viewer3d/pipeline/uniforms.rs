use super::{
    AMBIENT, Camera, FOV_Y, FlyCamera, MapFog, MapPreview, MapSkyCamera, Rectangle, RenderMode,
    RenderScene, ResolvedTexture, SOURCE_UP, Skybox, decode_bc_texture, half_extent, look_at,
    mat_mul, mid, perspective, skybox_eye,
};
use crate::media::preview_model::ModelPreview;
use gmpublished_domain::math::Vec3;

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniforms {
    pub view_proj: [[f32; 4]; 4],
    pub light: [f32; 4],
    pub camera_position: [f32; 4],
    pub fog_color: [f32; 4],
    pub fog_params: [f32; 4],
    pub water_time_sky_tint: [f32; 4],
    pub water_depth_params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MaterialUniform {
    pub flags: [u32; 4],
    pub tint: [f32; 4],
}

impl MaterialUniform {
    pub fn with_tint(mut self, color: [u8; 3]) -> Self {
        self.tint[..3].copy_from_slice(&color.map(srgb_channel_to_linear));
        self
    }

    pub const fn new(force_opaque: bool, render_mode: RenderMode) -> Self {
        Self {
            tint: [1.0; 4],
            flags: [
                (force_opaque && matches!(render_mode, RenderMode::Opaque)) as u32,
                matches!(render_mode, RenderMode::Cutout) as u32,
                0,
                0,
            ],
        }
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
        self.fog_params[0] /= scale;
        self.fog_params[1] /= scale;
        let light = Vec3::from(inverse_vector([
            self.light[0],
            self.light[1],
            self.light[2],
        ]))
        .normalize()
        .unwrap_or(SOURCE_UP);
        self.light[..3].copy_from_slice(light.as_array());
    }

    pub fn for_fly(
        scene: &MapPreview,
        camera: &FlyCamera,
        bounds: Rectangle,
        fog: Option<MapFog>,
        water_time: f32,
        submerged: bool,
    ) -> Self {
        let frame = FlyCameraFrame::new(&scene.scene, camera, bounds);
        let target = frame.eye + frame.forward;
        let view = look_at(frame.eye, target, SOURCE_UP);
        let (fog_color, fog_params) = if submerged {
            let color = scene_water_fog_color(&scene.scene);
            ([color[0], color[1], color[2], 0.0], [0.0, 2048.0, 1.0, 1.0])
        } else {
            (
                fog.map_or([0.0; 4], |fog| {
                    [
                        fog.color_linear[0],
                        fog.color_linear[1],
                        fog.color_linear[2],
                        0.0,
                    ]
                }),
                fog.map_or([0.0, 1.0, 0.0, 0.0], |fog| {
                    [fog.start, fog.end, fog.max_density, 1.0]
                }),
            )
        };

        Self {
            view_proj: mat_mul(frame.proj, view),
            light: [0.4, 0.6, 0.8, AMBIENT],
            camera_position: [frame.eye[0], frame.eye[1], frame.eye[2], 0.0],
            fog_color,
            fog_params,
            water_time_sky_tint: [water_time, 0.0, 0.0, 0.0],
            water_depth_params: [frame.near, frame.far, 0.0, 0.0],
        }
    }

    pub fn for_fly_sky(scene: &MapPreview, camera: &FlyCamera, bounds: Rectangle) -> Self {
        let frame = FlyCameraFrame::new(&scene.scene, camera, bounds);
        let target = frame.eye + frame.forward;
        let mut view = look_at(frame.eye, target, SOURCE_UP);
        view[3][0] = 0.0;
        view[3][1] = 0.0;
        view[3][2] = 0.0;

        Self {
            view_proj: mat_mul(frame.proj, view),
            light: [0.0; 4],
            camera_position: [frame.eye[0], frame.eye[1], frame.eye[2], 0.0],
            fog_color: [0.0; 4],
            fog_params: [0.0, 1.0, 0.0, 0.0],
            water_time_sky_tint: [0.0; 4],
            water_depth_params: [frame.near, frame.far, 0.0, 0.0],
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
        let view = look_at(eye, eye + frame.forward, SOURCE_UP);

        Self {
            view_proj: mat_mul(frame.proj, view),
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
            fog_params: fog.map_or([0.0, 1.0, 0.0, 0.0], |fog| {
                [fog.start, fog.end, fog.max_density, 1.0]
            }),
            water_time_sky_tint: [0.0; 4],
            water_depth_params: [frame.near, frame.far, 0.0, 0.0],
        }
    }

    pub fn for_model(
        model: &ModelPreview,
        camera: &Camera,
        bounds: Rectangle,
        framing: Option<crate::media::preview_model::ModelBounds>,
    ) -> Self {
        let framing = framing.unwrap_or(crate::media::preview_model::ModelBounds {
            min: model.scene.bounds_min,
            max: model.scene.bounds_max,
        });
        let center = framing.center();
        let radius = half_extent(framing.min, framing.max).max(1.0);
        let distance = radius * 2.2 * camera.orbit.distance();

        let eye = [
            center[0] + distance * camera.orbit.pitch().cos() * camera.orbit.yaw().sin(),
            center[1] + distance * camera.orbit.pitch().cos() * camera.orbit.yaw().cos(),
            center[2] + distance * camera.orbit.pitch().sin(),
        ];
        // Source models are Z-up.
        let view = look_at(Vec3::from(eye), center, SOURCE_UP);
        let aspect = (bounds.width / bounds.height.max(1.0)).max(0.1);
        let proj = perspective(FOV_Y, aspect, radius * 0.01, radius * 20.0 + distance);

        Self {
            view_proj: mat_mul(proj, view),
            light: [0.4, 0.6, 0.8, AMBIENT],
            camera_position: [eye[0], eye[1], eye[2], 0.0],
            fog_color: [0.0; 4],
            fog_params: [0.0, 1.0, 0.0, 0.0],
            water_time_sky_tint: [0.0; 4],
            water_depth_params: [0.0; 4],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FlyCameraFrame {
    pub eye: Vec3,
    pub forward: Vec3,
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
        let aspect = (bounds.width / bounds.height.max(1.0)).max(0.1);
        let near = 4.0;
        let far = (radius * 6.0).max(30_000.0);
        Self {
            eye,
            forward: camera.forward(),
            proj: perspective(FOV_Y, aspect, near, far),
            near,
            far,
        }
    }
}

pub const DEFAULT_SKY_TINT: Vec3 = Vec3::new(0.12, 0.18, 0.24);

fn scene_water_fog_color(scene: &RenderScene) -> Vec3 {
    scene
        .materials
        .iter()
        .filter_map(|slot| slot.texture.as_deref())
        .find(|texture| texture.is_water_fallback())
        .and_then(texture_smallest_mip_average)
        .unwrap_or(crate::bridge::materials::DEFAULT_WATER_FOG_LINEAR)
}

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
    if let Some((format, mips)) = texture.bc_payload() {
        let mip = mips.last()?;
        let rgba = decode_bc_texture(format, mip.width, mip.height, &mip.data)?;
        return average_srgb_rgba(&rgba, mip.width, mip.height);
    }
    let mip = texture.mip_chain().last()?;
    average_srgb_rgba(mip.rgba, mip.width, mip.height)
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

    use super::Uniforms;

    #[test]
    fn shaders_validate_and_match_the_uploaded_uniform_layout() {
        let fields = [
            ("view_proj", offset_of!(Uniforms, view_proj)),
            ("light", offset_of!(Uniforms, light)),
            ("camera_position", offset_of!(Uniforms, camera_position)),
            ("fog_color", offset_of!(Uniforms, fog_color)),
            ("fog_params", offset_of!(Uniforms, fog_params)),
            (
                "water_time_sky_tint",
                offset_of!(Uniforms, water_time_sky_tint),
            ),
            (
                "water_depth_params",
                offset_of!(Uniforms, water_depth_params),
            ),
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
            if let Some(naga::TypeInner::Struct { members, span }) = module
                .types
                .iter()
                .find_map(|(_, ty)| (ty.name.as_deref() == Some("Material")).then_some(&ty.inner))
            {
                assert_eq!(
                    *span as usize,
                    size_of::<super::MaterialUniform>(),
                    "{name}: material size"
                );
                assert_eq!(members.len(), 2);
                assert_eq!(
                    members[0].offset as usize,
                    offset_of!(super::MaterialUniform, flags)
                );
                assert_eq!(
                    members[1].offset as usize,
                    offset_of!(super::MaterialUniform, tint)
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
        }
    }
}

#[cfg(test)]
mod tests {
    use iced::Point;

    use super::super::super::test_support::empty_preview;
    use super::{FlyCamera, MapFog, Rectangle, Uniforms, Vec3, average_srgb_rgba};

    #[test]
    fn material_tint_matches_srgb_texture_sampling_and_preserves_render_flags() {
        let base = super::MaterialUniform::new(false, super::RenderMode::Cutout);
        let tinted = base.with_tint([128, 0, 255]);
        assert_eq!(tinted.flags, base.flags);
        assert!((tinted.tint[0] - 0.215_860_5).abs() < 0.000_001);
        assert_eq!(&tinted.tint[1..], &[0.0, 1.0, 1.0]);
        assert_eq!(base.tint, [1.0; 4]);
    }

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
            false,
        );
        let original_projection = uniforms.view_proj;
        uniforms.camera_position = [8.0, 14.0, 20.0, 0.0];
        uniforms.light = [0.0, 1.0, 0.0, 0.35];
        uniforms.fog_params = [100.0, 1000.0, 0.5, 1.0];
        let transform = [
            [0.0, 2.0, 0.0, 0.0],
            [-2.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 2.0, 0.0],
            [10.0, 10.0, 10.0, 1.0],
        ];
        uniforms.apply_model_transform(transform);
        assert_eq!(uniforms.camera_position, [2.0, 1.0, 5.0, 0.0]);
        assert_eq!(uniforms.light, [1.0, 0.0, 0.0, 0.35]);
        assert_eq!(uniforms.fog_params, [50.0, 500.0, 0.5, 1.0]);
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
    fn submerged_fly_uniforms_override_map_fog() {
        let scene = empty_preview(Vec3::splat(-128.0), Vec3::splat(128.0));
        let camera = FlyCamera::default();
        let map_fog = MapFog {
            color_linear: Vec3::new(0.8, 0.7, 0.6),
            start: 512.0,
            end: 8192.0,
            max_density: 0.5,
        };

        let above = Uniforms::for_fly(
            &scene,
            &camera,
            Rectangle::new(Point::ORIGIN, iced::Size::new(800.0, 600.0)),
            Some(map_fog),
            12.0,
            false,
        );
        let submerged = Uniforms::for_fly(
            &scene,
            &camera,
            Rectangle::new(Point::ORIGIN, iced::Size::new(800.0, 600.0)),
            Some(map_fog),
            12.0,
            true,
        );

        assert_eq!(above.fog_color, [0.8, 0.7, 0.6, 0.0]);
        assert_eq!(above.fog_params, [512.0, 8192.0, 0.5, 1.0]);
        assert_eq!(above.water_time_sky_tint[0], 12.0);
        assert_eq!(submerged.fog_color, [0.03, 0.10, 0.10, 0.0]);
        assert_eq!(submerged.fog_params, [0.0, 2048.0, 1.0, 1.0]);
    }
}
