const FOG_DISABLED:u32=0u;
const FOG_AXIAL:u32=1u;
const FOG_HEIGHT:u32=2u;
const FOG_IMMERSION:u32=3u;
const FOG_RADIAL:u32=4u;

struct FogUniform {
    start:f32,
    end:f32,
    max_density:f32,
    mode:u32,
}

const LIGHTMAP_LDR:u32=0u;
const LIGHTMAP_HDR_FALLBACK:u32=1u;
const SHADING_SCENE:u32=0u;
const SHADING_FULLBRIGHT:u32=1u;
const DIAGNOSTIC_DISABLED:u32=0u;
const DIAGNOSTIC_VERTEX_NORMAL_COLOR:u32=1u;
const DIAGNOSTIC_VERTEX_TANGENT_S_COLOR:u32=2u;
const DIAGNOSTIC_VERTEX_TANGENT_T_COLOR:u32=3u;
const DIAGNOSTIC_VERTEX_NORMAL_LINEAR:u32=4u;
const DIAGNOSTIC_VERTEX_TANGENT_S_LINEAR:u32=5u;
const DIAGNOSTIC_VERTEX_TANGENT_T_LINEAR:u32=6u;
const DIAGNOSTIC_RAW_NORMAL:u32=7u;
const DIAGNOSTIC_RAW_TANGENT_S:u32=8u;
const DIAGNOSTIC_RAW_TANGENT_T:u32=9u;
const DIAGNOSTIC_POSITION:u32=10u;
const DIAGNOSTIC_UNIT_NORMAL:u32=11u;
const DIAGNOSTIC_UNIT_TANGENT_S:u32=12u;
const DIAGNOSTIC_UNIT_EYE:u32=13u;

struct Diagnostic {
    mode:u32, padding0:u32, padding1:u32, padding2:u32,
    transform:vec4<f32>,
}

struct Uniforms {
    view_proj: mat4x4<f32>,
    // Standalone preview direction and ambient strength.
    light: vec4<f32>,
    camera_position: vec4<f32>,
    fog_color: vec4<f32>,
    fog: FogUniform,
    // x: animation time, yzw: sky tint.
    water_time_sky_tint: vec4<f32>,
    // xy: near and far clip distances.
    water_depth_params: vec4<f32>,
    clip_plane: vec4<f32>,
    medium_plane: vec4<f32>,
    viewport_size: vec2<f32>,
    lightmap_mode:u32,
    shading_mode:u32,
    raster_params: vec4<f32>,
    diagnostic:Diagnostic,
}
