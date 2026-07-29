//! CPU-side frame parameters: the uniform block every pass reads, the
//! camera frames that fill it, and the scene-colour analysis (water fog,
//! sky tint) feeding those values. Owns no GPU objects.

use super::{
    AMBIENT, Camera, FOV_Y, FlyCamera, MapFog, MapSkyCamera, ModelPreview, Rectangle, RenderMode,
    ResolvedTexture, SOURCE_UP, Skybox, decode_bc_texture, half_extent, look_at, mat_mul, mid,
    perspective, skybox_eye,
};
use gmpublished_backend::math::Vec3;

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
}

impl MaterialUniform {
    pub const fn new(force_opaque: bool, render_mode: RenderMode) -> Self {
        Self {
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
    pub fn for_fly(
        scene: &ModelPreview,
        camera: &FlyCamera,
        bounds: Rectangle,
        fog: Option<MapFog>,
        water_time: f32,
        submerged: bool,
    ) -> Self {
        let frame = FlyCameraFrame::new(scene, camera, bounds);
        let target = frame.eye + frame.forward;
        let view = look_at(frame.eye, target, SOURCE_UP);
        let (fog_color, fog_params) = if submerged {
            let color = scene_water_fog_color(scene);
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

    pub fn for_fly_sky(scene: &ModelPreview, camera: &FlyCamera, bounds: Rectangle) -> Self {
        let frame = FlyCameraFrame::new(scene, camera, bounds);
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
        scene: &ModelPreview,
        camera: &FlyCamera,
        bounds: Rectangle,
        sky_camera: MapSkyCamera,
        fog: Option<MapFog>,
    ) -> Self {
        let frame = FlyCameraFrame::new(scene, camera, bounds);
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

    pub fn for_model(model: &ModelPreview, camera: &Camera, bounds: Rectangle) -> Self {
        let center = mid(model.bounds_min, model.bounds_max);
        let radius = half_extent(model.bounds_min, model.bounds_max).max(1.0);
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
    pub fn new(scene: &ModelPreview, camera: &FlyCamera, bounds: Rectangle) -> Self {
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

fn scene_water_fog_color(scene: &ModelPreview) -> Vec3 {
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
    use super::Uniforms;

    /// Every shader that binds the shared uniform buffer declares its own copy
    /// of the struct, and WGSL only requires the buffer to be *at least* as
    /// large as the declaration. A shader carrying a prefix therefore compiles
    /// and runs, reading the wrong fields the moment one is inserted anywhere
    /// but the end — silently, on the GPU, with no Rust-side symptom.
    ///
    /// `min_binding_size` catches a buffer that is too small; nothing catches
    /// a declaration that is too short. This does.
    #[test]
    fn every_shader_declares_the_whole_uniform_struct() {
        for (name, source) in [
            (
                "model_viewer.wgsl",
                super::super::super::MODEL_SHADER_SOURCE,
            ),
            ("water.wgsl", super::super::super::WATER_SHADER_SOURCE),
            ("detail.wgsl", super::super::super::DETAIL_SHADER_SOURCE),
            ("sky.wgsl", super::super::super::SKY_SHADER_SOURCE),
        ] {
            assert_eq!(
                declared_uniform_size(source),
                Some(std::mem::size_of::<Uniforms>()),
                "{name} does not declare the whole `Uniforms` struct"
            );
        }
    }

    /// Every `.wgsl` in this feature declares `Uniforms` with these seven
    /// members at these offsets, and the bind-group layout pins the buffer to
    /// this size. Nothing can check the WGSL side from Rust, so this pins the
    /// Rust side exactly: a reorder or an inserted member fails here, and the
    /// fix is to mirror it in `model_viewer.wgsl`, `detail.wgsl`, `sky.wgsl`
    /// and `water.wgsl`.
    #[test]
    fn uniforms_layout_matches_the_shaders() {
        assert_eq!(size_of::<Uniforms>(), 160);
        assert_eq!(align_of::<Uniforms>(), 4);
        assert_eq!(std::mem::offset_of!(Uniforms, view_proj), 0);
        assert_eq!(std::mem::offset_of!(Uniforms, light), 64);
        assert_eq!(std::mem::offset_of!(Uniforms, camera_position), 80);
        assert_eq!(std::mem::offset_of!(Uniforms, fog_color), 96);
        assert_eq!(std::mem::offset_of!(Uniforms, fog_params), 112);
        assert_eq!(std::mem::offset_of!(Uniforms, water_time_sky_tint), 128);
        assert_eq!(std::mem::offset_of!(Uniforms, water_depth_params), 144);
    }

    /// Byte size of the `Uniforms` struct a WGSL source declares, by summing
    /// its members. `None` if it declares none, or uses a member type this
    /// does not know — either is a reason to look rather than pass.
    fn declared_uniform_size(source: &str) -> Option<usize> {
        let body = source.split_once("struct Uniforms {")?.1.split_once('}')?.0;
        let mut total = 0;
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let ty = line.split_once(':')?.1.trim().trim_end_matches(',');
            total += match ty {
                "mat4x4<f32>" => 64,
                "vec4<f32>" => 16,
                _ => return None,
            };
        }
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use iced::Point;

    use super::super::super::test_support::empty_preview;
    use super::{FlyCamera, MapFog, Rectangle, Uniforms, Vec3, average_srgb_rgba};

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
