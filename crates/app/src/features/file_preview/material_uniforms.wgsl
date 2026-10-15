struct Material {
    texture_alpha_opaque: u32,
    alpha_test: u32,
    bump_mode: u32,
    planar_kind: u32,
    unlit: u32,
    vertex_color: u32,
    vertex_alpha: u32,
    normal_animation_clamp: u32,
    water_fog: vec4<f32>,
    water_optics: vec4<f32>,
    water_reflect: vec4<f32>,
    water_refract: vec4<f32>,
    normal_s: vec4<f32>,
    normal_t: vec4<f32>,
    water_scroll: vec4<f32>,
    water_animation: vec4<f32>,
    water_state: vec4<f32>,
}


const BUMP_NONE:u32=0u;
const BUMP_TANGENT:u32=1u;
const BUMP_SSBUMP:u32=2u;
const PLANAR_NONE:u32=0u;
const PLANAR_MIRROR:u32=1u;
const PLANAR_TEXTURED_WATER:u32=2u;
