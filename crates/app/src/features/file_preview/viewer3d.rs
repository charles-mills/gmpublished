//! Custom wgpu pipeline rendering a static Source model inside the preview
//! modal. Damage-driven: redraws happen only on orbit/zoom input, so an idle
//! open viewer costs zero CPU/GPU.

use std::sync::Arc;

use iced::mouse;
use iced::wgpu;
use iced::widget::shader::{self, Action, Viewport};
use iced::{Event, Point, Rectangle};

use super::model::ModelVertex;
use crate::backend::materials::{RenderMode, ResolvedBcMip, ResolvedTexture};
use gmpublished_backend::scene::map::{MapDoorClass, MapDoorMotion, MapDoorOpenDirection};
use vformats::vtf::BcFormat;

use super::Message;
#[cfg(test)]
use super::model::DoorSounds;
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
mod texture;

pub(super) use camera::{Camera, FlyCamera, FlyViewer, Viewer3d};
#[cfg(test)]
use camera::{
    LAND_BOB_AMPLITUDE, PLAYER_START_EYE_NUDGE, WALK_DUCK_EYE_HEIGHT, WALK_HULL_HALF_EXTENTS,
    WalkHull,
};
#[cfg(test)]
use doors::source_sound_gain;
use doors::{
    DOOR_PROGRESS_EPSILON, DOOR_USE_REACH, DoorMoveLoopRuntime, DoorRenderPose, DoorRuntime,
    DoorTarget, bounds_intersect, choose_door_open_sign, door_audio_event, door_progress_step,
    door_sound_gain, door_uses_move_loop, door_world_bounds, endpoint_sound, expand_bounds,
    initial_door_open_sign, ray_aabb_distance, trace_aabb_against_aabb, transform_door_vertices,
};
use draw_plan::{DrawItem, DrawPlan, DrawPlans, OverlayDrawItem, prepare_draw_plans};
#[cfg(test)]
use draw_plan::{
    DrawPlanMaterials, DrawPlanRequest, DrawPlanSelection, DrawPlanSourceSlices, MeshPlanSource,
    OverlayPlanSource, prepare_draw_plans_from_sources,
};
use pipeline::ModelPrimitive;
use pipeline::Uniforms;
#[cfg(test)]
use pipeline::{
    VisibilityClusterState, VisibilityClusterTracker, average_srgb_rgba, checkerboard_mip_levels,
    skybox_face_corners,
};
use texture::{
    TextureUploadLevel, bc_mip_is_valid, bc_texture_format, decode_bc_texture,
    write_bc_texture_level, write_texture_level,
};

const SHADER_SOURCE: &str = include_str!("model_viewer.wgsl");
const WATER_SHADER_SOURCE: &str = include_str!("water.wgsl");
const DETAIL_SHADER_SOURCE: &str = include_str!("detail.wgsl");
const SKY_SHADER_SOURCE: &str = include_str!("sky.wgsl");
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
const ORBIT_SENSITIVITY: f32 = 0.008;
const ZOOM_STEP: f32 = 0.9;
const MIN_PITCH: f32 = -1.55;
const MAX_PITCH: f32 = 1.55;
const FOV_Y: f32 = std::f32::consts::FRAC_PI_4;
const AMBIENT: f32 = 0.35;
const MODEL_VERTEX_FLOAT_COUNT: u64 = 14;
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

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn mul(vector: [f32; 3], scalar: f32) -> [f32; 3] {
    [vector[0] * scalar, vector[1] * scalar, vector[2] * scalar]
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

fn distance_squared(a: [f32; 3], b: [f32; 3]) -> f32 {
    let delta = sub(a, b);
    dot(delta, delta)
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    distance_squared(a, b).sqrt()
}

fn length_squared(vector: [f32; 3]) -> f32 {
    dot(vector, vector)
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = dot(v, v).sqrt();
    if len <= f32::EPSILON {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

fn normalize_or_zero(v: [f32; 3]) -> [f32; 3] {
    let len = dot(v, v).sqrt();
    if len <= f32::EPSILON {
        [0.0; 3]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

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
    use super::super::model::ModelStats;
    use super::*;

    fn empty_preview(bounds_min: [f32; 3], bounds_max: [f32; 3]) -> ModelPreview {
        ModelPreview {
            meshes: Vec::new(),
            mesh_visibility: Vec::new(),
            map_skybox_meshes: Vec::new(),
            materials: Vec::new(),
            lightmap: None,
            skybox: None,
            detail_sprites: Vec::new(),
            map_skybox_detail_sprites: Vec::new(),
            overlays: Vec::new(),
            map_skybox_overlays: Vec::new(),
            doors: Vec::new(),
            phy_debug_meshes: Vec::new(),
            skin_tables: vec![Vec::new()],
            bodygroups: Vec::new(),
            stats: ModelStats {
                bone_count: 0,
                sequence_count: 0,
                vertex_count: 0,
                triangle_count: 0,
                mesh_count: 0,
                material_count: 0,
                resolved_material_count: 0,
            },
            bounds_min,
            bounds_max,
            visibility: None,
            walk_collision: None,
        }
    }

    fn floor_scene() -> ModelPreview {
        let mut scene = empty_preview([0.0; 3], [1024.0; 3]);
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            [-4096.0, -4096.0, -64.0],
            [4096.0, 4096.0, 0.0],
        ));
        scene
    }

    fn deep_water_scene() -> ModelPreview {
        let mut scene = empty_preview([-512.0, -512.0, -320.0], [512.0, 512.0, 256.0]);
        scene.walk_collision = Some(
            MapWalkCollision::solid_box_for_tests(
                [-4096.0, -4096.0, -320.0],
                [4096.0, 4096.0, -256.0],
            )
            .with_water_box_for_tests([-4096.0, -4096.0, -256.0], [4096.0, 4096.0, 100.0]),
        );
        scene
    }

    fn walk_camera(position: [f32; 3], grounded: bool) -> FlyCamera {
        FlyCamera {
            content_id: Some(1),
            position: Some(position),
            mode: MovementMode::Walk,
            grounded,
            ..FlyCamera::default()
        }
    }

    fn horizontal_distance_from(position: [f32; 3], origin: [f32; 3]) -> f32 {
        ((position[0] - origin[0]).powi(2) + (position[1] - origin[1]).powi(2)).sqrt()
    }

    #[test]
    fn walk_standing_on_the_floor_can_move_and_jump() {
        let scene = floor_scene();

        // Resting contact: hull bottom a hair above the floor plane — the
        // state every landing converges to (hit traces back the mover off
        // by the plane epsilon, so a grounded player rests at that
        // separation, never at mathematically exact contact).
        let mut camera = walk_camera([512.0, 512.0, 64.1], true);

        camera.held.forward = true;
        for _ in 0..30 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
        }
        let after_walk = camera.position.expect("position retained");
        let walked = horizontal_distance_from(after_walk, [512.0, 512.0, 64.1]);
        assert!(
            walked > 30.0,
            "half a second of held-forward must actually move the player, moved {walked}"
        );
        assert!(camera.grounded, "walking on flat ground must stay grounded");

        camera.held.forward = false;
        camera.request_jump();
        let ground_z = after_walk[2];
        let mut apex = ground_z;
        let mut left_ground = false;
        for _ in 0..120 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            let z = camera.position.expect("position retained")[2];
            apex = apex.max(z);
            left_ground |= !camera.grounded;
        }
        assert!(left_ground, "jump must leave the ground");
        assert!(
            apex > ground_z + 20.0,
            "jump apex should clear ~45 units, got {}",
            apex - ground_z
        );
        assert!(camera.grounded, "jump must land again within two seconds");
    }

    #[test]
    fn walk_falling_into_deep_water_stops_falling() {
        let scene = deep_water_scene();
        let mut camera = walk_camera([0.0, 0.0, 180.0], false);

        for _ in 0..180 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
        }

        assert!(camera.swimming);
        assert!(!camera.grounded);
        assert!(camera.position.expect("swimmer position")[2] > 40.0);
        assert!(camera.walk_velocity[2].abs() < 0.1);
    }

    #[test]
    fn walk_swimming_forward_uses_view_pitch() {
        let scene = deep_water_scene();
        let mut camera = walk_camera([0.0, 0.0, 64.0], false);
        camera.pitch = -0.6;
        camera.held.forward = true;

        for _ in 0..60 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
        }

        let position = camera.position.expect("swimmer position");
        assert!(
            position[0] > 50.0,
            "forward swim should advance: {position:?}"
        );
        assert!(
            position[2] < 20.0,
            "downward pitch should dive: {position:?}"
        );
        assert!(camera.submerged());
    }

    #[test]
    fn walk_motionless_floating_swimmer_goes_idle_within_two_seconds() {
        let scene = deep_water_scene();
        let mut camera = walk_camera([0.0, 0.0, 64.0], false);
        camera.walk_velocity = [120.0, 0.0, -30.0];

        for _ in 0..120 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            if !camera.needs_movement_tick() {
                break;
            }
        }

        assert!(camera.swimming);
        assert_eq!(camera.walk_velocity, [0.0; 3]);
        assert!(!camera.needs_movement_tick());
    }

    #[test]
    fn walk_swimming_exit_assist_climbs_pool_ledge() {
        let mut scene = empty_preview([-256.0, -256.0, -128.0], [256.0, 256.0, 160.0]);
        scene.walk_collision = Some(
            MapWalkCollision::solid_box_for_tests(
                [-4096.0, -4096.0, -128.0],
                [4096.0, 4096.0, -64.0],
            )
            .with_solid_box_for_tests([48.0, -128.0, -64.0], [256.0, 128.0, 82.0])
            .with_water_box_for_tests([-256.0, -128.0, -64.0], [48.0, 128.0, 64.0]),
        );
        let mut camera = walk_camera([0.0, 0.0, 68.0], false);
        camera.held.forward = true;

        for _ in 0..240 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            if camera.grounded && camera.position.is_some_and(|position| position[2] > 145.0) {
                break;
            }
        }

        let position = camera.position.expect("walker position");
        assert!(
            camera.grounded,
            "exit assist should finish grounded: {camera:?}"
        );
        assert!(
            position[0] > 32.0,
            "exit assist should clear the ledge: {position:?}"
        );
        assert!(
            position[2] > 145.0,
            "hull should stand on the ledge: {position:?}"
        );
    }

    #[test]
    fn walk_entering_water_suppresses_land_bob() {
        let scene = deep_water_scene();
        let mut camera = walk_camera([0.0, 0.0, 64.0], false);
        camera.walk_velocity[2] = -240.0;
        camera.land_bob_elapsed = 0.05;
        camera.land_bob_amplitude = LAND_BOB_AMPLITUDE;

        let _ = camera.integrate(&scene, 1, 1.0 / 60.0);

        assert!(camera.swimming);
        assert_eq!(camera.land_bob_amplitude, 0.0);
        assert_eq!(camera.view_bob_offset(), 0.0);
    }

    #[test]
    fn walk_sprint_covers_more_ground_than_walking() {
        let scene = floor_scene();
        let run = |sprint: bool| {
            let mut camera = walk_camera([512.0, 512.0, 64.1], true);
            camera.held.forward = true;
            camera.held.fast = sprint;
            for _ in 0..60 {
                let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            }
            let position = camera.position.expect("position retained");
            horizontal_distance_from(position, [512.0, 512.0, 64.1])
        };
        let walked = run(false);
        let sprinted = run(true);
        assert!(
            sprinted > walked * 1.4,
            "shift must sprint: walked {walked}, sprinted {sprinted}"
        );
    }

    #[test]
    fn walk_toggle_at_exact_floor_contact_unsticks_and_walks() {
        let scene = floor_scene();

        // Mappers place info_player_start exactly on the floor, so the
        // hull starts at mathematically exact contact — the trace calls
        // that solid even though the embed check does not. Toggling walk
        // here must unstick and produce a mover that actually moves.
        let mut camera = FlyCamera {
            content_id: Some(1),
            position: Some([512.0, 512.0, 64.0]),
            ..FlyCamera::default()
        };
        camera.toggle_walk(&scene);
        assert_eq!(camera.mode, MovementMode::Walk, "toggle must engage walk");

        camera.held.forward = true;
        for _ in 0..90 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
        }
        assert!(camera.grounded, "must settle onto the floor");
        let position = camera.position.expect("position retained");
        let walked = horizontal_distance_from(position, [512.0, 512.0, 64.0]);
        assert!(
            walked > 30.0,
            "held-forward from an exact-contact spawn must move, moved {walked}"
        );
    }

    #[test]
    fn walk_crouch_enters_low_gap_and_refuses_blocked_unduck() {
        let mut scene = empty_preview([-128.0, -128.0, 0.0], [256.0, 128.0, 128.0]);
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            [80.0, -64.0, 40.0],
            [160.0, 64.0, 128.0],
        ));
        let collision = scene.walk_collision.as_ref().expect("collision fixture");

        let mut standing = walk_camera([0.0, 0.0, PLAYER_START_EYE_NUDGE], true);
        standing.move_walk_delta(collision, [140.0, 0.0, 0.0], true);
        assert!(
            standing.position.expect("standing position")[0] < 70.0,
            "standing hull must not enter a 40-unit gap"
        );

        let mut camera = walk_camera([0.0, 0.0, PLAYER_START_EYE_NUDGE], true);
        camera.held.duck = true;
        camera.reconcile_duck_state(collision);
        assert_eq!(camera.walk_hull, WalkHull::Ducked);
        assert_eq!(
            camera.position.expect("ducked position")[2],
            WALK_DUCK_EYE_HEIGHT
        );

        camera.move_walk_delta(collision, [140.0, 0.0, 0.0], true);
        let under_ceiling = camera.position.expect("under ceiling");
        assert!(
            under_ceiling[0] > 120.0,
            "ducked hull must pass under the 40-unit ceiling"
        );

        camera.held.duck = false;
        camera.reconcile_duck_state(collision);
        assert_eq!(camera.walk_hull, WalkHull::Ducked);
        assert_eq!(
            camera.position.expect("blocked unduck keeps eye")[2],
            under_ceiling[2],
            "blocked unduck must leave the physics eye low"
        );

        camera.move_walk_delta(collision, [100.0, 0.0, 0.0], true);
        camera.reconcile_duck_state(collision);
        assert_eq!(camera.walk_hull, WalkHull::Standing);
        assert!(
            (camera.position.expect("standing again")[2] - PLAYER_START_EYE_NUDGE).abs() < 1.0e-4,
            "unduck outside the ceiling must restore standing eye height"
        );
    }

    #[test]
    fn walk_step_rejects_zero_horizontal_progress_at_backed_off_wall_contact() {
        let collision =
            MapWalkCollision::solid_box_for_tests([80.0, -64.0, 0.0], [120.0, 64.0, 128.0]);
        let mut camera = walk_camera(
            [
                80.0 - WALK_HULL_HALF_EXTENTS[0] - 0.03125,
                0.0,
                PLAYER_START_EYE_NUDGE,
            ],
            true,
        );
        let start = camera.position.expect("walk position");

        assert!(
            !camera.try_step(&collision, start, [120.0, 0.0, 0.0]),
            "a step attempt that cannot move forward must fall back to slide/clip handling"
        );
        assert_eq!(camera.position, Some(start));
    }

    #[test]
    fn walk_crouch_jump_pulls_feet_up_to_clear_obstacle() {
        let mut scene = empty_preview([-128.0, -128.0, 0.0], [256.0, 128.0, 128.0]);
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            [60.0, -32.0, 0.0],
            [90.0, 32.0, 64.0],
        ));
        let collision = scene.walk_collision.as_ref().expect("collision fixture");

        let mut jumper = walk_camera([0.0, 0.0, PLAYER_START_EYE_NUDGE], true);
        jumper.request_jump();
        for _ in 0..24 {
            jumper.integrate_walk_step(collision, 1.0 / 60.0);
            if jumper.walk_velocity[2] <= 0.0 {
                break;
            }
        }
        let apex = jumper.position.expect("jump apex");
        assert!(apex[2] > 100.0, "jump fixture should reach obstacle height");

        let mut standing = walk_camera(apex, false);
        standing.move_walk_delta(collision, [140.0, 0.0, 0.0], false);
        assert!(
            standing.position.expect("standing air move")[0] < 50.0,
            "standing jump must hit the obstacle"
        );

        let mut ducked = walk_camera(apex, false);
        ducked.held.duck = true;
        ducked.reconcile_duck_state(collision);
        assert_eq!(
            ducked.position.expect("air duck keeps eye"),
            apex,
            "air duck must shrink toward the eye, not lower it"
        );
        assert_eq!(ducked.walk_hull, WalkHull::Ducked);

        ducked.move_walk_delta(collision, [140.0, 0.0, 0.0], false);
        assert!(
            ducked.position.expect("ducked air move")[0] > 120.0,
            "ducked jump must pull feet above the obstacle"
        );
    }

    #[test]
    fn walk_ducked_speed_is_one_third_and_overrides_sprint() {
        let scene = floor_scene();
        let run = |duck: bool, sprint: bool| {
            let mut camera = walk_camera([512.0, 512.0, 64.1], true);
            camera.held.forward = true;
            camera.held.duck = duck;
            camera.held.fast = sprint;
            for _ in 0..60 {
                let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            }
            horizontal_distance_from(
                camera.position.expect("position retained"),
                [512.0, 512.0, 64.1],
            )
        };

        let walked = run(false, false);
        let ducked = run(true, false);
        let duck_sprinted = run(true, true);
        assert!(
            ((ducked / walked) - (1.0 / 3.0)).abs() < 0.03,
            "ducked speed must be one third of walk: walked {walked}, ducked {ducked}"
        );
        assert!(
            (duck_sprinted - ducked).abs() < 0.5,
            "duck must override sprint: ducked {ducked}, duck+sprint {duck_sprinted}"
        );
    }

    #[test]
    fn walk_duck_view_animation_terminates_and_goes_idle() {
        let scene = floor_scene();
        let mut camera = walk_camera([512.0, 512.0, 64.1], true);
        camera.held.duck = true;
        camera.duck_reconcile_requested = true;

        assert!(camera.needs_movement_tick());
        for _ in 0..20 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
        }

        assert_eq!(camera.walk_hull, WalkHull::Ducked);
        assert!(
            !camera.duck_view_transition_active(),
            "duck view interpolation must finish after >0.2s"
        );
        assert!(
            !camera.needs_movement_tick(),
            "settled crouch with no movement must not keep the tick loop alive"
        );
    }

    #[test]
    fn default_walk_entry_settles_grounded_and_goes_idle() {
        let scene = floor_scene();
        let spawn = MapSpawn {
            origin: [512.0, 512.0, 0.0],
            angles: [0.0, 90.0, 0.0],
        };
        let mut camera = FlyCamera::default();

        camera.ensure_spawn(&scene, Some(spawn), 7, None, None);

        assert_eq!(camera.mode, MovementMode::Walk);
        assert!(!camera.grounded, "default walk entry starts airborne");
        for _ in 0..240 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            if !camera.needs_movement_tick() {
                break;
            }
        }
        assert_eq!(camera.mode, MovementMode::Walk);
        assert!(camera.grounded, "spawned walker must settle to ground");
        assert!(
            !camera.needs_movement_tick(),
            "settled default-walk spawn must reach idle"
        );
    }

    #[test]
    fn restored_walk_mode_reenters_walk_from_pose() {
        let scene = floor_scene();
        let pose = FlyPose {
            position: [512.0, 512.0, 128.0],
            yaw: 0.5,
            pitch: -0.25,
            speed: 1.75,
        };
        let mut camera = FlyCamera::default();

        camera.ensure_spawn(&scene, None, 7, Some(pose), Some(MovementMode::Walk));

        assert_eq!(camera.pose(), Some(pose));
        assert_eq!(camera.mode, MovementMode::Walk);
        assert!(
            !camera.grounded,
            "walk restore must resume gravity from pose"
        );
    }

    #[test]
    fn restored_fly_mode_keeps_fly_pose() {
        let scene = floor_scene();
        let pose = FlyPose {
            position: [512.0, 512.0, 128.0],
            yaw: 0.5,
            pitch: -0.25,
            speed: 1.75,
        };
        let mut camera = FlyCamera::default();

        camera.ensure_spawn(&scene, None, 7, Some(pose), Some(MovementMode::Fly));

        assert_eq!(camera.pose(), Some(pose));
        assert_eq!(camera.mode, MovementMode::Fly);
    }

    #[test]
    fn absent_mode_with_legacy_pose_defaults_to_walk_at_spawn() {
        let scene = floor_scene();
        let legacy_pose = FlyPose {
            position: [128.0, 256.0, 384.0],
            yaw: 0.5,
            pitch: -0.25,
            speed: 2.0,
        };
        let spawn = MapSpawn {
            origin: [512.0, 512.0, 0.0],
            angles: [0.0, 90.0, 0.0],
        };
        let mut camera = FlyCamera::default();

        camera.ensure_spawn(&scene, Some(spawn), 7, Some(legacy_pose), None);

        let pose = camera.pose().expect("spawn should initialize camera");
        assert_eq!(camera.mode, MovementMode::Walk);
        assert_eq!(
            [pose.position[0], pose.position[1]],
            [512.0, 512.0],
            "legacy pose-only state must not suppress spawn walk default"
        );
        assert_ne!(pose.position, legacy_pose.position);
        assert!((pose.yaw - 90.0_f32.to_radians()).abs() < 1.0e-6);
    }

    #[test]
    fn direct_mode_selection_matches_v_toggle_selector() {
        let scene = floor_scene();
        let pose = FlyPose {
            position: [512.0, 512.0, 128.0],
            yaw: 0.5,
            pitch: -0.25,
            speed: 1.75,
        };

        let mut via_toggle = FlyCamera::default();
        via_toggle.ensure_spawn(&scene, None, 7, Some(pose), Some(MovementMode::Fly));
        assert!(via_toggle.toggle_walk(&scene));

        let mut via_select = FlyCamera::default();
        via_select.ensure_spawn(&scene, None, 7, Some(pose), Some(MovementMode::Fly));
        assert!(via_select.select_mode(&scene, MovementMode::Walk));

        assert_eq!(via_toggle.mode, via_select.mode);
        assert_eq!(via_toggle.pose(), via_select.pose());
        assert_eq!(via_toggle.grounded, via_select.grounded);
        assert_eq!(
            via_toggle.needs_movement_tick(),
            via_select.needs_movement_tick()
        );
    }

    #[test]
    fn default_walk_entry_falls_back_without_spawn_or_collision() {
        let scene = floor_scene();
        let mut camera = FlyCamera::default();
        camera.ensure_spawn(&scene, None, 7, None, None);
        assert_eq!(camera.mode, MovementMode::Fly);

        let no_collision = empty_preview([0.0; 3], [1024.0; 3]);
        let spawn = MapSpawn {
            origin: [512.0, 512.0, 0.0],
            angles: [0.0; 3],
        };
        let mut camera = FlyCamera::default();
        camera.ensure_spawn(&no_collision, Some(spawn), 7, None, None);
        assert_eq!(camera.mode, MovementMode::Fly);
    }

    #[test]
    fn default_walk_entry_falls_back_when_spawn_remains_solid() {
        let mut scene = empty_preview([-1024.0; 3], [1024.0; 3]);
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            [-1024.0, -1024.0, -1024.0],
            [1024.0, 1024.0, 1024.0],
        ));
        let spawn = MapSpawn {
            origin: [0.0; 3],
            angles: [0.0; 3],
        };
        let mut camera = FlyCamera::default();

        camera.ensure_spawn(&scene, Some(spawn), 7, None, None);

        assert_eq!(camera.mode, MovementMode::Fly);
        assert_eq!(
            camera.position.expect("fly fallback position"),
            [0.0, 0.0, PLAYER_START_EYE_NUDGE]
        );
    }

    #[test]
    fn walk_falling_into_the_void_reverts_to_fly_and_goes_idle() {
        let mut scene = empty_preview([0.0; 3], [1024.0; 3]);
        // Non-empty collision (walk mode refuses to engage otherwise), but
        // nothing anywhere near the camera — an endless fall.
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            [4000.0, 4000.0, 0.0],
            [4100.0, 4100.0, 100.0],
        ));

        let mut camera = FlyCamera {
            content_id: Some(1),
            position: Some([512.0, 512.0, 2048.0]),
            mode: MovementMode::Walk,
            ..FlyCamera::default()
        };

        assert!(camera.needs_movement_tick(), "airborne walker must tick");
        for _ in 0..600 {
            let _ = camera.integrate(&scene, 1, 1.0 / 60.0);
            if camera.mode == MovementMode::Fly {
                break;
            }
        }
        assert_eq!(
            camera.mode,
            MovementMode::Fly,
            "endless fall must hand the camera back to fly"
        );
        assert!(
            !camera.needs_movement_tick(),
            "after the void failsafe the redraw loop must go idle"
        );
        let position = camera.position.expect("position retained");
        assert!(position[2].is_finite());
    }

    #[test]
    fn door_toggle_reverses_mid_transition_and_then_goes_idle() {
        let scene = door_scene(vec![test_linear_door([40.0, 0.0, 64.0], 100.0)]);
        let mut camera = walk_camera_for_scene(&scene, [0.0, 0.0, 64.0], 0.0);

        assert!(matches!(
            camera.toggle_nearest_door(&scene, 1),
            Some(DoorAudioEvent {
                kind: DoorAudioEventKind::MoveStarted,
                ..
            })
        ));
        assert!(camera.doors[0].move_loop.is_some());
        let events = camera.integrate_doors(&scene, 1, 0.25);
        assert!(events.iter().any(|event| {
            matches!(
                event.kind,
                DoorAudioEventKind::MoveLoopVolumeChanged | DoorAudioEventKind::MotionEnded { .. }
            )
        }));
        assert!(camera.doors[0].progress > 0.24 && camera.doors[0].progress < 0.26);
        assert_eq!(camera.doors[0].target, DoorTarget::Open);

        assert!(camera.toggle_nearest_door(&scene, 1).is_some());
        assert_eq!(camera.doors[0].target, DoorTarget::Closed);
        let _ = camera.integrate_doors(&scene, 1, 0.10);
        assert!(
            camera.doors[0].progress < 0.25,
            "closing after a mid-transition toggle must reverse from the current pose"
        );
        for _ in 0..60 {
            let _ = camera.integrate_doors(&scene, 1, 1.0 / 60.0);
        }

        assert_eq!(camera.doors[0].progress, 0.0);
        assert!(!camera.doors[0].moving);
        assert!(camera.doors[0].move_loop.is_none());
        assert!(
            !camera.needs_movement_tick(),
            "a settled door must not keep the redraw loop alive"
        );
    }

    #[test]
    fn door_endpoint_clears_move_loop_marker_and_emits_stop_event() {
        let mut door = test_linear_door([40.0, 0.0, 64.0], 100.0);
        door.sounds.stop_sound = Some(test_door_sound("doors/door1_stop.wav"));
        let scene = door_scene(vec![door]);
        let mut camera = walk_camera_for_scene(&scene, [0.0, 0.0, 64.0], 0.0);

        assert!(camera.toggle_nearest_door(&scene, 1).is_some());
        assert!(camera.doors[0].move_loop.is_some());

        let events = camera.integrate_doors(&scene, 1, 2.0);

        assert_eq!(camera.doors[0].progress, 1.0);
        assert!(!camera.doors[0].moving);
        assert!(
            camera.doors[0].move_loop.is_none(),
            "endpoint must drop its move-loop marker"
        );
        assert!(events.iter().any(|event| {
            event.door_index == 0
                && event.gain > 0.0
                && event.kind == (DoorAudioEventKind::MotionEnded { open: true })
        }));
    }

    #[test]
    fn blocked_closing_door_parks_and_clears_move_loop_marker() {
        let scene = door_scene(vec![test_linear_door([40.0, 0.0, 64.0], 100.0)]);
        let mut camera = walk_camera_for_scene(&scene, [50.0, 0.0, 64.0], 0.0);
        camera.doors[0].progress = 0.2;
        camera.doors[0].target = DoorTarget::Closed;
        camera.doors[0].moving = true;
        camera.doors[0].move_loop = Some(DoorMoveLoopRuntime);
        (camera.doors[0].bounds_min, camera.doors[0].bounds_max) =
            door_world_bounds(&scene.doors[0], 0.2, 1.0);

        let events = camera.integrate_doors(&scene, 1, 1.0 / 60.0);

        assert!(!camera.doors[0].moving);
        assert!(camera.doors[0].blocked_closing);
        assert!(
            camera.doors[0].move_loop.is_none(),
            "parked door must drop its move-loop marker"
        );
        assert!(
            events
                .iter()
                .any(|event| { event.door_index == 0 && event.kind == DoorAudioEventKind::Parked })
        );
    }

    #[test]
    fn use_ray_picks_nearest_door_and_ignores_doors_beyond_reach() {
        let scene = door_scene(vec![
            test_linear_door([70.0, 0.0, 64.0], 32.0),
            test_linear_door([40.0, 0.0, 64.0], 32.0),
        ]);
        let mut camera = walk_camera_for_scene(&scene, [0.0, 0.0, 64.0], 0.0);

        assert!(camera.toggle_nearest_door(&scene, 1).is_some());
        assert_eq!(camera.doors[0].target, DoorTarget::Closed);
        assert_eq!(camera.doors[1].target, DoorTarget::Open);
        assert!(camera.doors[1].moving);

        let far_scene = door_scene(vec![test_linear_door([90.0, 0.0, 64.0], 32.0)]);
        let mut far_camera = walk_camera_for_scene(&far_scene, [0.0, 0.0, 64.0], 0.0);
        assert!(
            far_camera.toggle_nearest_door(&far_scene, 1).is_none(),
            "use reach is capped at 80 Source units"
        );
        assert_eq!(far_camera.doors[0].target, DoorTarget::Closed);
    }

    #[test]
    fn walk_trace_hits_door_at_current_mid_swing_pose() {
        let scene = door_scene(vec![test_linear_door([40.0, 0.0, 64.0], 40.0)]);
        let mut camera = walk_camera_for_scene(&scene, [0.0, 0.0, 64.0], 0.0);
        camera.doors[0].progress = 0.5;
        (camera.doors[0].bounds_min, camera.doors[0].bounds_max) =
            door_world_bounds(&scene.doors[0], 0.5, 1.0);
        let collision = scene.walk_collision.as_ref().expect("collision fixture");

        let hit = camera.trace_aabb(collision, [50.0, 0.0, 64.0], [80.0, 0.0, 64.0], [1.0; 3]);

        assert!(!hit.start_solid);
        assert!(hit.fraction > 0.29 && hit.fraction < 0.31, "{hit:?}");
        assert_eq!(hit.normal, [-1.0, 0.0, 0.0]);
        assert!((hit.end_position[0] - 59.0).abs() < 1.0e-4, "{hit:?}");
    }

    #[test]
    fn source_sound_gain_matches_documented_three_point_falloff() {
        let near = source_sound_gain(64.0, 75.0);
        let mid = source_sound_gain(750.0, 75.0);
        let far = source_sound_gain(1500.0, 75.0);

        assert_eq!(near, 1.0);
        assert!(mid > 0.0 && mid < near);
        assert_eq!(far, 0.0);
    }

    fn mesh_plan_source(mesh_index: usize, map_skybox: bool, centroid: [f32; 3]) -> MeshPlanSource {
        MeshPlanSource {
            mesh_index,
            scene_mesh_index: mesh_index,
            material_index: 0,
            bodygroup: 0,
            bodygroup_choice: 0,
            centroid,
            map_skybox,
            door_index: None,
            door_visibility: None,
        }
    }

    fn door_scene(doors: Vec<DoorInstance>) -> ModelPreview {
        let mut scene = empty_preview([-128.0, -128.0, -128.0], [256.0, 128.0, 128.0]);
        scene.walk_collision = Some(MapWalkCollision::solid_box_for_tests(
            [1000.0, 1000.0, 1000.0],
            [1100.0, 1100.0, 1100.0],
        ));
        scene.doors = doors;
        scene
    }

    fn test_linear_door(origin: [f32; 3], distance: f32) -> DoorInstance {
        DoorInstance {
            class: MapDoorClass::FuncDoor,
            origin,
            angles: [0.0; 3],
            local_bounds_min: [0.0, -16.0, -32.0],
            local_bounds_max: [8.0, 16.0, 32.0],
            visibility: MapVisibilityBucket::Always,
            initial_progress: 0.0,
            motion: MapDoorMotion::Linear {
                direction: [1.0, 0.0, 0.0],
                distance,
                speed: 100.0,
            },
            sounds: DoorSounds::default(),
            meshes: Vec::new(),
        }
    }

    fn test_door_sound(reference: &str) -> DoorSound {
        DoorSound {
            reference: reference.to_owned(),
            sound_level: 75.0,
            volume: 1.0,
            waves: Vec::new(),
        }
    }

    fn walk_camera_for_scene(scene: &ModelPreview, position: [f32; 3], yaw: f32) -> FlyCamera {
        let mut camera = FlyCamera::default();
        camera.ensure_spawn(
            scene,
            None,
            1,
            Some(FlyPose {
                position,
                yaw,
                pitch: 0.0,
                speed: 1.0,
            }),
            Some(MovementMode::Fly),
        );
        camera.mode = MovementMode::Walk;
        camera.grounded = true;
        camera
    }

    fn overlay_plan_source(
        overlay_index: usize,
        map_skybox: bool,
        centroid: [f32; 3],
    ) -> OverlayPlanSource {
        OverlayPlanSource {
            overlay_index,
            material_index: 0,
            centroid,
            map_skybox,
        }
    }

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
        let view = look_at([5.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
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
        let scene = empty_preview([-128.0; 3], [128.0; 3]);
        let camera = FlyCamera::default();
        let map_fog = MapFog {
            color_linear: [0.8, 0.7, 0.6],
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
        assert_eq!(submerged.fog_color, [0.03, 0.10, 0.10, 1.0]);
        assert_eq!(submerged.fog_params, [0.0, 2048.0, 1.0, 1.0]);
    }

    #[test]
    fn water_meshes_are_partitioned_and_sorted_back_to_front() {
        let meshes = [
            mesh_plan_source(0, false, [1.0, 0.0, 0.0]),
            MeshPlanSource {
                mesh_index: 1,
                material_index: 1,
                centroid: [2.0, 0.0, 0.0],
                ..mesh_plan_source(1, false, [2.0, 0.0, 0.0])
            },
            MeshPlanSource {
                mesh_index: 2,
                material_index: 1,
                centroid: [4.0, 0.0, 0.0],
                ..mesh_plan_source(2, false, [4.0, 0.0, 0.0])
            },
            MeshPlanSource {
                mesh_index: 3,
                material_index: 2,
                centroid: [3.0, 0.0, 0.0],
                ..mesh_plan_source(3, false, [3.0, 0.0, 0.0])
            },
        ];
        let render_modes = [
            RenderMode::Opaque,
            RenderMode::Translucent,
            RenderMode::Translucent,
        ];
        let water_fallbacks = [false, true, false];

        let plans = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    water_fallbacks: &water_fallbacks,
                    material_count: 3,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0; 4],
                map_skybox_visible: false,
                map_skybox_content_present: false,
                map_skybox_camera_position: None,
                visibility_plan: None,
            },
        );

        assert_eq!(
            plans
                .world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            plans
                .world
                .water
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(
            plans
                .world
                .translucent
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[test]
    fn map_skybox_disabled_omits_skybox_sets_from_draw_plans() {
        let meshes = [
            mesh_plan_source(0, false, [0.0, 0.0, 0.0]),
            mesh_plan_source(1, true, [10.0, 0.0, 0.0]),
        ];
        let overlays = [overlay_plan_source(0, true, [12.0, 0.0, 0.0])];
        let render_modes = [RenderMode::Opaque];
        let plans = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &overlays,
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    water_fallbacks: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: false,
                map_skybox_content_present: true,
                map_skybox_camera_position: Some([1.0, 0.0, 0.0, 0.0]),
                visibility_plan: None,
            },
        );

        assert_eq!(plans.content_id, 7);
        assert_eq!(
            plans
                .world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert!(plans.world.overlay_opaque.is_empty());
        assert!(plans.map_skybox.is_none());
    }

    #[test]
    fn missing_sky_camera_omits_composite_plan() {
        let meshes = [mesh_plan_source(0, true, [10.0, 0.0, 0.0])];
        let render_modes = [RenderMode::Opaque];
        let plans = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    water_fallbacks: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: true,
                map_skybox_content_present: true,
                map_skybox_camera_position: None,
                visibility_plan: None,
            },
        );

        assert!(plans.world.opaque.is_empty());
        assert!(plans.map_skybox.is_none());
    }

    #[test]
    fn empty_skybox_partition_omits_composite_plan() {
        let meshes = [mesh_plan_source(0, false, [0.0, 0.0, 0.0])];
        let render_modes = [RenderMode::Opaque];
        let plans = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    water_fallbacks: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: true,
                map_skybox_content_present: false,
                map_skybox_camera_position: Some([1.0, 0.0, 0.0, 0.0]),
                visibility_plan: None,
            },
        );

        assert_eq!(plans.world.opaque.len(), 1);
        assert!(plans.map_skybox.is_none());
    }

    #[test]
    fn visibility_plan_filters_world_meshes_but_off_path_draws_everything() {
        let meshes = [
            mesh_plan_source(0, false, [0.0, 0.0, 0.0]),
            mesh_plan_source(1, false, [10.0, 0.0, 0.0]),
        ];
        let render_modes = [RenderMode::Opaque];
        let visibility = WorldVisibilityPlan {
            mesh_indices: vec![vec![0, 1, 2], Vec::new()],
            overlay_visible: Vec::new(),
            detail_sprite_visible: Vec::new(),
            visible_clusters: vec![true],
            visible_cluster_count: 1,
        };

        let off = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    water_fallbacks: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: false,
                map_skybox_content_present: false,
                map_skybox_camera_position: None,
                visibility_plan: None,
            },
        );
        let on = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    water_fallbacks: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: false,
                map_skybox_content_present: false,
                map_skybox_camera_position: None,
                visibility_plan: Some(&visibility),
            },
        );

        assert_eq!(
            off.world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(
            on.world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0]
        );
    }

    #[test]
    fn visibility_plan_lookup_uses_scene_index_not_upload_index() {
        // Empty-index meshes are dropped at upload, so upload position and
        // scene position diverge; the plan is keyed by scene position.
        // Scene mesh 1 was dropped: the surviving upload slot 1 holds scene
        // mesh 2, whose plan entry is visible while scene mesh 1's is not.
        let meshes = [
            mesh_plan_source(0, false, [0.0, 0.0, 0.0]),
            MeshPlanSource {
                mesh_index: 1,
                scene_mesh_index: 2,
                material_index: 0,
                bodygroup: 0,
                bodygroup_choice: 0,
                centroid: [10.0, 0.0, 0.0],
                map_skybox: false,
                door_index: None,
                door_visibility: None,
            },
        ];
        let render_modes = [RenderMode::Opaque];
        let visibility = WorldVisibilityPlan {
            mesh_indices: vec![vec![0, 1, 2], Vec::new(), vec![3, 4, 5]],
            overlay_visible: Vec::new(),
            detail_sprite_visible: Vec::new(),
            visible_clusters: vec![true],
            visible_cluster_count: 1,
        };

        let on = prepare_draw_plans_from_sources(
            DrawPlanSourceSlices {
                meshes: &meshes,
                overlays: &[],
                materials: DrawPlanMaterials {
                    render_modes: &render_modes,
                    water_fallbacks: &[],
                    material_count: 1,
                },
            },
            DrawPlanRequest {
                content_id: 7,
                selection: DrawPlanSelection {
                    skin_remap: &[],
                    bodygroup_choices: &[],
                },
                camera_position: [0.0, 0.0, 0.0, 0.0],
                map_skybox_visible: false,
                map_skybox_content_present: false,
                map_skybox_camera_position: None,
                visibility_plan: Some(&visibility),
            },
        );

        assert_eq!(
            on.world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn visibility_plan_filters_door_meshes_by_bucket_not_scene_index() {
        let meshes = [
            mesh_plan_source(0, false, [0.0, 0.0, 0.0]),
            MeshPlanSource {
                mesh_index: 1,
                scene_mesh_index: usize::MAX,
                material_index: 0,
                bodygroup: 0,
                bodygroup_choice: 0,
                centroid: [10.0, 0.0, 0.0],
                map_skybox: false,
                door_index: Some(0),
                door_visibility: Some(MapVisibilityBucket::Cluster(1)),
            },
        ];
        let render_modes = [RenderMode::Opaque];
        let hidden = WorldVisibilityPlan {
            mesh_indices: vec![vec![0, 1, 2]],
            overlay_visible: Vec::new(),
            detail_sprite_visible: Vec::new(),
            visible_clusters: vec![true, false],
            visible_cluster_count: 1,
        };
        let visible = WorldVisibilityPlan {
            visible_clusters: vec![true, true],
            visible_cluster_count: 2,
            ..hidden.clone()
        };

        let plan = |visibility_plan| {
            prepare_draw_plans_from_sources(
                DrawPlanSourceSlices {
                    meshes: &meshes,
                    overlays: &[],
                    materials: DrawPlanMaterials {
                        render_modes: &render_modes,
                        water_fallbacks: &[],
                        material_count: 1,
                    },
                },
                DrawPlanRequest {
                    content_id: 7,
                    selection: DrawPlanSelection {
                        skin_remap: &[],
                        bodygroup_choices: &[],
                    },
                    camera_position: [0.0, 0.0, 0.0, 0.0],
                    map_skybox_visible: false,
                    map_skybox_content_present: false,
                    map_skybox_camera_position: None,
                    visibility_plan: Some(visibility_plan),
                },
            )
        };

        assert_eq!(
            plan(&hidden)
                .world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            plan(&visible)
                .world
                .opaque
                .iter()
                .map(|item| item.mesh_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn visibility_tracker_rebuilds_only_when_cluster_changes() {
        let mut tracker = VisibilityClusterTracker::default();
        let mut probes = 0_u32;

        assert_eq!(
            tracker.update(true, [0.0, 0.0, 0.0], |_| {
                probes = probes.saturating_add(1);
                Some(3)
            }),
            Some(VisibilityClusterState::Cluster(3))
        );
        assert_eq!(tracker.rebuild_count, 1);
        assert_eq!(
            tracker.update(true, [1.0, 0.0, 0.0], |_| {
                probes = probes.saturating_add(1);
                Some(3)
            }),
            None
        );
        assert_eq!(tracker.rebuild_count, 1);
        assert_eq!(
            tracker.update(true, [1.0, 0.0, 0.0], |_| {
                panic!("leaf lookup should not run without movement")
            }),
            None
        );
        assert_eq!(
            tracker.update(true, [2.0, 0.0, 0.0], |_| {
                probes = probes.saturating_add(1);
                Some(4)
            }),
            Some(VisibilityClusterState::Cluster(4))
        );
        assert_eq!(tracker.rebuild_count, 2);
        assert_eq!(probes, 3);
    }

    #[test]
    fn orbit_camera_fresh_state_seeds_from_pose() {
        let mut camera = Camera::default();
        let pose = OrbitPose {
            yaw: 1.25,
            pitch: -0.75,
            distance: 3.5,
        };

        camera.ensure_spawn(7, Some(pose));

        assert_eq!(camera.content_id, Some(7));
        assert_eq!(camera.pose(), pose);
    }

    #[test]
    fn orbit_camera_without_pose_uses_default_framing() {
        let mut camera = Camera {
            content_id: None,
            yaw: 9.0,
            pitch: -9.0,
            distance: 4.0,
            drag_from: Some(Point::new(1.0, 2.0)),
        };

        camera.ensure_spawn(7, None);

        assert_eq!(camera.content_id, Some(7));
        assert_eq!(camera.pose(), OrbitPose::default());
        assert_eq!(camera.drag_from, None);
    }

    #[test]
    fn fly_camera_fresh_state_seeds_from_pose() {
        let scene = empty_preview([-10.0, -10.0, -10.0], [10.0, 10.0, 10.0]);
        let mut camera = FlyCamera::default();
        let pose = FlyPose {
            position: [3.0, 4.0, 5.0],
            yaw: 1.25,
            pitch: -0.75,
            speed: 3.5,
        };

        camera.ensure_spawn(&scene, None, 7, Some(pose), Some(MovementMode::Fly));

        assert_eq!(camera.content_id, Some(7));
        assert_eq!(camera.pose(), Some(pose));
    }

    #[test]
    fn fly_camera_without_pose_uses_map_spawn() {
        let scene = empty_preview([-10.0, -10.0, -10.0], [10.0, 10.0, 10.0]);
        let mut camera = FlyCamera::default();
        let spawn = MapSpawn {
            origin: [1.0, 2.0, 3.0],
            angles: [10.0, 90.0, 0.0],
        };

        camera.ensure_spawn(&scene, Some(spawn), 7, None, None);

        let pose = camera.pose().expect("spawn should initialize fly pose");
        assert_eq!(camera.content_id, Some(7));
        assert_eq!(pose.position, [1.0, 2.0, 3.0 + PLAYER_START_EYE_NUDGE]);
        assert!((pose.yaw - 90.0_f32.to_radians()).abs() < 1e-6);
        assert!((pose.pitch - -10.0_f32.to_radians()).abs() < 1e-6);
        assert_eq!(pose.speed, 1.0);
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

    #[test]
    fn software_bc_decoder_matches_vtf_decode_for_solid_blocks() {
        let fixtures = [
            (
                BcFormat::Bc1,
                ::vtf::ImageFormat::Dxt1,
                solid_bc1_color_block(0xf800),
            ),
            (
                BcFormat::Bc2,
                ::vtf::ImageFormat::Dxt3,
                solid_bc2_color_block(0x07e0, 0x0f),
            ),
            (
                BcFormat::Bc3,
                ::vtf::ImageFormat::Dxt5,
                solid_bc3_color_block(0x001f, 255),
            ),
        ];

        for (format, image_format, block) in fixtures {
            let bytes = bc_vtf_bytes(4, 4, image_format, &block);
            let decoded = crate::backend::materials::decode_vtf_rgba(&bytes).expect("vtf decode");
            let software = decode_bc_texture(format, decoded.width, decoded.height, &block)
                .expect("BC decode");

            assert_eq!(software.len(), decoded.rgba.len());
            for (actual, expected) in software.iter().zip(decoded.rgba.iter()) {
                assert!(
                    actual.abs_diff(*expected) <= 8,
                    "format {format:?}: {actual} != {expected}"
                );
            }
        }
    }

    /// Fragment shading must not flicker per pixel on meshes with a constant
    /// white vertex color. Regression test for the `all(input.color ==
    /// vec3(1.0))` exact float compare in `model_viewer.wgsl`: attribute
    /// interpolation is not required to reproduce 1.0 exactly per fragment,
    /// and on NVIDIA hardware the compare flickered pixel-by-pixel between
    /// the ambient/diffuse and vertex-color-modulate branches, rendering
    /// every textured surface as salt-and-pepper noise. Renders an angled
    /// quad with constant color/UV through the real ModelPipeline and
    /// asserts the interior shades uniformly. (Only catches the regression
    /// on GPUs with inexact constant interpolation; exact GPUs pass either
    /// way.)
    #[test]
    fn constant_white_vertex_color_shades_uniformly() {
        const WIDTH: u32 = 512;
        const HEIGHT: u32 = 384;

        // GL only: naga's GLSL backend cannot translate the refractive water
        // shader's `textureLoad` on a depth texture, so ModelPipeline::new
        // panics on driverless machines (CI) that fall back to GL. Restrict
        // to the primary backends and take the skip path instead.
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::default()
        });
        let Ok(adapter) = futures::executor::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        ) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let (device, queue) =
            futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("test.uniform_shading"),
                ..wgpu::DeviceDescriptor::default()
            }))
            .expect("device");

        // Desk-top-like angled quad: constant white color, constant UV, so
        // every fragment must shade identically.
        let vertex = |x: f32, y: f32| ModelVertex {
            position: [x, y, 20.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.25, 0.25],
            lightmap_uv: [0.0; 2],
            color: [1.0; 3],
            blend_alpha: 0.0,
        };
        let mesh = MeshData {
            vertices: vec![
                vertex(-48.0, -24.0),
                vertex(48.0, -24.0),
                vertex(48.0, 24.0),
                vertex(-48.0, 24.0),
            ],
            // Both windings so the quad is visible regardless of which side
            // the default orbit camera ends up on.
            indices: vec![0, 1, 2, 0, 2, 3, 2, 1, 0, 3, 2, 0],
            material_index: 0,
            bodygroup: 0,
            bodygroup_choice: 0,
        };
        let mut preview = empty_preview([-48.0, -24.0, 0.0], [48.0, 24.0, 38.0]);
        preview.meshes = vec![mesh];
        preview.materials = vec![MaterialSlot {
            name: "test".to_owned(),
            texture: None,
            texture2: None,
            force_opaque: true,
            render_mode: RenderMode::Opaque,
        }];
        let preview = Arc::new(preview);

        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(WIDTH as f32, HEIGHT as f32));
        let clip_bounds = Rectangle::<u32> {
            x: 0,
            y: 0,
            width: WIDTH,
            height: HEIGHT,
        };
        let viewport = Viewport::with_physical_size(iced::Size::new(WIDTH, HEIGHT), 1.0);
        let mut camera = Camera::default();
        camera.ensure_spawn(1, None);
        let primitive = ModelPrimitive {
            skin_remap: vec![0],
            bodygroup_choices: Vec::new(),
            map_skybox_visible: false,
            visibility_culling: false,
            phy_debug_visible: false,
            uniforms: Uniforms::for_model(&preview, &camera, bounds),
            map_skybox_uniforms: None,
            sky_uniforms: None,
            door_poses: Vec::new(),
            model: preview,
            content_id: 1,
        };
        let mut pipeline_state =
            <pipeline::ModelPipeline as shader::Pipeline>::new(&device, &queue, format);
        shader::Primitive::prepare(
            &primitive,
            &mut pipeline_state,
            &device,
            &queue,
            &bounds,
            &viewport,
        );

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test.uniform_shading.target"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("test.uniform_shading.clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLUE),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        shader::Primitive::render(
            &primitive,
            &pipeline_state,
            &mut encoder,
            &target_view,
            &clip_bounds,
        );
        let padded_row = (WIDTH * 4).div_ceil(256) * 256;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test.uniform_shading.readback"),
            size: u64::from(padded_row) * u64::from(HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        receiver
            .recv()
            .expect("map_async callback")
            .expect("map readback");
        let mapped = readback.slice(..).get_mapped_range();
        let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
        for y in 0..HEIGHT {
            let start = (y * padded_row) as usize;
            pixels.extend_from_slice(&mapped[start..start + (WIDTH * 4) as usize]);
        }
        drop(mapped);
        readback.unmap();

        // Quad pixels are whatever isn't the blue clear color; erode by
        // requiring the 4-neighborhood to also be quad so edge antialiasing
        // and silhouette pixels don't count.
        let is_quad = |x: u32, y: u32| {
            let offset = ((y * WIDTH + x) * 4) as usize;
            let pixel = &pixels[offset..offset + 4];
            !(pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 255)
        };
        let mut min_rgb = [u8::MAX; 3];
        let mut max_rgb = [u8::MIN; 3];
        let mut interior = 0_usize;
        for y in 1..HEIGHT - 1 {
            for x in 1..WIDTH - 1 {
                if !(is_quad(x, y)
                    && is_quad(x - 1, y)
                    && is_quad(x + 1, y)
                    && is_quad(x, y - 1)
                    && is_quad(x, y + 1))
                {
                    continue;
                }
                interior += 1;
                let offset = ((y * WIDTH + x) * 4) as usize;
                for channel in 0..3 {
                    let value = pixels[offset + channel];
                    min_rgb[channel] = min_rgb[channel].min(value);
                    max_rgb[channel] = max_rgb[channel].max(value);
                }
            }
        }
        assert!(
            interior > 1000,
            "quad did not render (interior={interior}); harness is broken"
        );
        let spread: Vec<u8> = (0..3)
            .map(|channel| max_rgb[channel].saturating_sub(min_rgb[channel]))
            .collect();
        assert!(
            spread.iter().all(|&value| value <= 2),
            "shading is not uniform across the quad: rgb spread {spread:?} over {interior} pixels \
             (min {min_rgb:?}, max {max_rgb:?}) — fragment branches are flickering per pixel"
        );
    }

    #[test]
    fn skybox_face_corners_match_source_2d_skybox_convention() {
        // Data source: Valve Developer Community "Skybox (2D)" suffixes,
        // with face orientation from noclip.website's SourceEngine SkyboxRenderer.
        assert_eq!(
            SkyboxFace::ALL.map(skybox_face_corners),
            [
                [
                    [1.0, 1.0, -1.0],
                    [1.0, 1.0, 1.0],
                    [1.0, -1.0, 1.0],
                    [1.0, -1.0, -1.0],
                ],
                [
                    [-1.0, -1.0, -1.0],
                    [-1.0, -1.0, 1.0],
                    [-1.0, 1.0, 1.0],
                    [-1.0, 1.0, -1.0],
                ],
                [
                    [-1.0, 1.0, -1.0],
                    [-1.0, 1.0, 1.0],
                    [1.0, 1.0, 1.0],
                    [1.0, 1.0, -1.0],
                ],
                [
                    [1.0, -1.0, -1.0],
                    [1.0, -1.0, 1.0],
                    [-1.0, -1.0, 1.0],
                    [-1.0, -1.0, -1.0],
                ],
                [
                    [1.0, 1.0, 1.0],
                    [-1.0, 1.0, 1.0],
                    [-1.0, -1.0, 1.0],
                    [1.0, -1.0, 1.0],
                ],
                [
                    [-1.0, 1.0, -1.0],
                    [1.0, 1.0, -1.0],
                    [1.0, -1.0, -1.0],
                    [-1.0, -1.0, -1.0],
                ],
            ]
        );
    }

    fn solid_bc1_color_block(color: u16) -> Vec<u8> {
        let mut block = vec![0_u8; 8];
        block[0..2].copy_from_slice(&color.to_le_bytes());
        block
    }

    fn solid_bc2_color_block(color: u16, alpha_nibble: u8) -> Vec<u8> {
        let mut block = vec![0_u8; 16];
        let alpha_byte = (alpha_nibble & 0x0f) | ((alpha_nibble & 0x0f) << 4);
        block[0..8].fill(alpha_byte);
        block[8..10].copy_from_slice(&color.to_le_bytes());
        block
    }

    fn solid_bc3_color_block(color: u16, alpha: u8) -> Vec<u8> {
        let mut block = vec![0_u8; 16];
        block[0] = alpha;
        block[1] = 0;
        block[8..10].copy_from_slice(&color.to_le_bytes());
        block
    }

    fn bc_vtf_bytes(width: u16, height: u16, format: ::vtf::ImageFormat, block: &[u8]) -> Vec<u8> {
        let header = ::vtf::header::VTFHeader {
            signature: ::vtf::header::VTFHeader::SIGNATURE,
            version: [7, 1],
            header_size: 64,
            width,
            height,
            flags: 0,
            frames: 1,
            first_frame: 0,
            reflectivity: [0.0; 3],
            bumpmap_scale: 1.0,
            highres_image_format: format,
            mipmap_count: 1,
            lowres_image_format: ::vtf::ImageFormat::None,
            lowres_image_width: 0,
            lowres_image_height: 0,
            depth: 1,
            resources: ::vtf::resources::ResourceList::empty(),
        };
        let mut bytes = Vec::new();
        header.write(&mut bytes).expect("fixture header");
        bytes.resize(header.header_size as usize, 0);
        bytes.extend_from_slice(block);
        bytes
    }
}
