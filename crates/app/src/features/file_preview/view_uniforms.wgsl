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
struct RasterClip {
    view_projection:mat4x4<f32>,
    plane:vec4<f32>,
}

struct Uniforms {
    view_proj: mat4x4<f32>,
    light: vec4<f32>,
    camera_position: vec4<f32>,
    fog_color: vec4<f32>,
    fog: FogUniform,
    water_time_sky_tint: vec4<f32>,
    water_depth_params: vec4<f32>,
    clip_plane: vec4<f32>,
    medium_plane: vec4<f32>,
    viewport_size: vec2<f32>,
    lightmap_mode:u32,
    shading_mode:u32,
    raster_params: vec4<f32>,
    raster_clip:RasterClip,
}

fn view_clip_distance(world_position:vec3<f32>)->f32 {
    return dot(uniforms.raster_clip.view_projection*vec4<f32>(world_position,1.0),uniforms.raster_clip.plane);
}

const HARDWARE_CLIPPING:bool=false;

fn view_fragment_is_clipped(distance:f32)->bool {
    return !HARDWARE_CLIPPING && distance<0.0;
}
