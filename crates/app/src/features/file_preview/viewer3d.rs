//! Custom wgpu pipeline rendering a static Source model inside the preview
//! modal. Damage-driven: redraws happen only on orbit/zoom input, so an idle
//! open viewer costs zero CPU/GPU.

use std::sync::Arc;

use iced::mouse;
use iced::wgpu;
use iced::widget::shader::{self, Action, Viewport};
use iced::{Event, Point, Rectangle};

use super::model::ModelVertex;
use crate::bridge::materials::{RenderMode, ResolvedBcMip, ResolvedTexture};
use gmpublished_backend::math::{
    self, add, cross, distance, distance_squared, dot, length_squared, normalize_or_zero,
    scale as mul, sub,
};
use gmpublished_backend::scene::map::{MapDoorClass, MapDoorMotion, MapDoorOpenDirection};
use vformats::vtf::BcFormat;

use super::Message;
use super::model::{
    DetailSprite, DoorAudioEvent, DoorAudioEventKind, DoorInstance, DoorSound, MapFog,
    MapSkyCamera, MapSpawn, MapTrace, MapVisibilityBucket, MapWalkCollision, MaterialSlot,
    MeshData, ModelPreview, OverlayPrimitive, PHY_DEBUG_MATERIAL_NAME, SKYBOX_FACE_COUNT, Skybox,
    SkyboxFace, WorldVisibilityPlan,
};
use super::state::{FlyPose, MovementMode, OrbitPose};

mod camera;
mod doors;
mod draw_plan;
mod pipeline;
#[cfg(test)]
mod test_support;
mod texture;

pub(super) use camera::{Camera, FlyCamera, FlyViewer, Viewer3d};
use doors::{
    DOOR_PROGRESS_EPSILON, DOOR_USE_REACH, DoorMotion, DoorRenderPose, DoorRuntime, DoorTarget,
    bounds_intersect, choose_door_swing, door_audio_event, door_progress_step, door_sound_gain,
    door_uses_move_loop, door_world_bounds, endpoint_sound, expand_bounds, initial_door_swing,
    ray_aabb_distance, trace_aabb_against_aabb, transform_door_vertices,
};
use draw_plan::{DrawItem, DrawPlan, DrawPlans, OverlayDrawItem, prepare_draw_plans};
use pipeline::ModelPrimitive;
use pipeline::Uniforms;
use texture::{
    TextureUploadLevel, bc_mip_is_valid, bc_texture_format, decode_bc_texture,
    write_bc_texture_level, write_texture_level,
};

pub(super) const MODEL_SHADER_SOURCE: &str = include_str!("model_viewer.wgsl");
const SHADER_SOURCE: &str = MODEL_SHADER_SOURCE;
pub(super) const WATER_SHADER_SOURCE: &str = include_str!("water.wgsl");
pub(super) const DETAIL_SHADER_SOURCE: &str = include_str!("detail.wgsl");
pub(super) const SKY_SHADER_SOURCE: &str = include_str!("sky.wgsl");
const BLIT_SHADER_SOURCE: &str = r"
var<private> uvs: array<vec2<f32>, 6> = array<vec2<f32>, 6>(
    vec2<f32>(0.0, 0.0),
    vec2<f32>(1.0, 0.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(0.0, 0.0),
    vec2<f32>(0.0, 1.0),
    vec2<f32>(1.0, 1.0)
);

@group(0) @binding(0) var resolved_texture: texture_2d<f32>;
@group(0) @binding(1) var resolved_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let uv = uvs[vertex_index];
    var out: VertexOutput;
    out.uv = uv;
    out.position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(resolved_texture, resolved_sampler, input.uv);
}
";
const CHECKERBOARD_SIZE: u32 = 8;
const CHECKERBOARD_SIZE_USIZE: usize = CHECKERBOARD_SIZE as usize;
const CHECKERBOARD_BYTES: usize = CHECKERBOARD_SIZE_USIZE * CHECKERBOARD_SIZE_USIZE * 4;
const CHECKERBOARD_MIP_RGBA: [u8; 4] = [188, 11, 188, 255];
const CHECKERBOARD_MIP_4X4_BYTES: usize = 4 * 4 * 4;
const CHECKERBOARD_MIP_2X2_BYTES: usize = 2 * 2 * 4;
const CHECKERBOARD_MIP_1X1_BYTES: usize = 4;
const PHY_DEBUG_RGBA: [u8; 4] = [48, 210, 255, 96];
const MSAA_SAMPLE_COUNT: u32 = 4;
const MATERIAL_ANISOTROPY_CLAMP: u16 = 16;
const FOV_Y: f32 = std::f32::consts::FRAC_PI_4;
const AMBIENT: f32 = 0.35;
const DETAIL_VERTEX_FLOAT_COUNT: u64 = 7;
const MODEL_VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    0 => Float32x3,
    1 => Float32x3,
    2 => Float32x2,
    3 => Float32x2,
    4 => Float32x3,
    5 => Float32,
];
const DETAIL_VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
    0 => Float32x3,
    1 => Float32x2,
    2 => Float32x2,
];

fn mid(min: [f32; 3], max: [f32; 3]) -> [f32; 3] {
    [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ]
}

fn half_extent(min: [f32; 3], max: [f32; 3]) -> f32 {
    let dx = (max[0] - min[0]) * 0.5;
    let dy = (max[1] - min[1]) * 0.5;
    let dz = (max[2] - min[2]) * 0.5;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn skybox_eye(world_eye: [f32; 3], sky_origin: [f32; 3], sky_scale: f32) -> [f32; 3] {
    let scale = if sky_scale.is_finite() && sky_scale > 0.0 {
        sky_scale
    } else {
        16.0
    };
    [
        sky_origin[0] + world_eye[0] / scale,
        sky_origin[1] + world_eye[1] / scale,
        sky_origin[2] + world_eye[2] / scale,
    ]
}

/// A direction, falling back to Source's world up.
///
/// The camera-basis convention: a `look_at` needs three usable axes, and a
/// zero vector there produces a NaN view matrix rather than a degenerate one.
fn normalize(v: [f32; 3]) -> [f32; 3] {
    math::normalize(v).unwrap_or(SOURCE_UP)
}

/// Source's world up. The engine is Z-up, so this is +Z rather than the +Y
/// most renderers default to.
pub(super) const SOURCE_UP: [f32; 3] = [0.0, 0.0, 1.0];

/// Right-handed look-at, column-major.
pub(super) fn look_at(eye: [f32; 3], center: [f32; 3], up: [f32; 3]) -> [[f32; 4]; 4] {
    let f = normalize(sub(center, eye));
    let s = normalize(cross(f, up));
    let u = cross(s, f);
    [
        [s[0], u[0], -f[0], 0.0],
        [s[1], u[1], -f[1], 0.0],
        [s[2], u[2], -f[2], 0.0],
        [-dot(s, eye), -dot(u, eye), dot(f, eye), 1.0],
    ]
}

/// Right-handed perspective with reversed wgpu [0, 1] clip-space depth.
pub(super) fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let f = 1.0 / (fov_y * 0.5).tan();
    let range = far - near;
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, near / range, -1.0],
        [0.0, 0.0, (far * near) / range, 0.0],
    ]
}

pub(super) fn mat_mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0_f32; 4]; 4];
    for (column, out_column) in out.iter_mut().enumerate() {
        for row in 0..4 {
            out_column[row] = (0..4).map(|k| a[k][row] * b[column][k]).sum();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perspective_maps_near_to_one_and_far_to_zero() {
        let proj = perspective(FOV_Y, 1.0, 1.0, 100.0);
        assert!((proj[2][2] - 1.0 / 99.0).abs() < 1e-6);
        assert!((proj[3][2] - 100.0 / 99.0).abs() < 1e-6);

        // Clip-space depth of a point at z = -near must be 1 after divide.
        let near_z = -proj[2][2] + proj[3][2];
        let near_w = 1.0;
        assert!((near_z / near_w - 1.0).abs() < 1e-6);
        // ... and z = -far must be 0.
        let far_z = proj[2][2] * -100.0 + proj[3][2];
        let far_w = 100.0;
        assert!((far_z / far_w).abs() < 1e-6);
    }

    #[test]
    fn look_at_puts_eye_at_origin() {
        let view = look_at([5.0, 0.0, 0.0], [0.0, 0.0, 0.0], SOURCE_UP);
        // Transforming the eye position must land on the origin.
        let x = view[0][0] * 5.0 + view[3][0];
        let y = view[0][1] * 5.0 + view[3][1];
        let z = view[0][2] * 5.0 + view[3][2];
        assert!(x.abs() < 1e-6 && y.abs() < 1e-6 && z.abs() < 1e-6);
    }

    #[test]
    fn skybox_eye_moves_world_eye_into_skybox_space() {
        assert_eq!(
            skybox_eye([160.0, -32.0, 48.0], [10.0, 20.0, 30.0], 16.0),
            [20.0, 18.0, 33.0]
        );
        assert_eq!(
            skybox_eye([160.0, -32.0, 48.0], [10.0, 20.0, 30.0], 8.0),
            [30.0, 16.0, 36.0]
        );
    }

    #[test]
    fn skybox_eye_uses_default_scale_for_invalid_input() {
        assert_eq!(
            skybox_eye([160.0, 0.0, 0.0], [1.0, 2.0, 3.0], 0.0),
            [11.0, 2.0, 3.0]
        );
    }
}
