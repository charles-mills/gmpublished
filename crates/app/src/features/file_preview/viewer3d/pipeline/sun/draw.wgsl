struct Params {
    matrix:mat4x4<f32>,
    positions:array<vec4<f32>,8>,
    colors:array<vec4<f32>,2>,
    world_rect:vec4<f32>,
    sky_rect:vec4<f32>,
    query:vec4<f32>,
    flags:vec4<u32>,
}
struct Material {flags:vec4<u32>,surface_flags:vec4<u32>}
@group(0) @binding(0) var<uniform> params:Params;
@group(0) @binding(1) var<storage,read> state:array<f32,4>;
@group(1) @binding(0) var base_texture:texture_2d<f32>;
@group(1) @binding(3) var base_sampler:sampler;
@group(1) @binding(5) var<uniform> material:Material;

struct Output {
    @builtin(position) clip:vec4<f32>,
    @location(0) uv:vec2<f32>,
    @location(1) color:vec3<f32>,
}
@vertex
fn vs_main(@builtin(vertex_index) vertex:u32,@builtin(instance_index) layer:u32)->Output {
    let corners=array<u32,6>(0u,1u,2u,0u,2u,3u);
    let uv=array<vec2<f32>,4>(vec2<f32>(0.0,1.0),vec2<f32>(1.0,1.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,0.0));
    var out:Output;
    out.clip=params.matrix*params.positions[layer*4u+corners[vertex]];
    out.uv=uv[corners[vertex]];
    let color=params.colors[layer].rgb*state[2]*state[3];
    let vertex_color=vec3<f32>(bitcast<vec3<u32>>(clamp(color,vec3<f32>(0.0),vec3<f32>(1.0))*255.0+8388608.0)&vec3<u32>(255u))/255.0;
    out.color=pow(vertex_color,vec3<f32>(2.2));
    return out;
}
@fragment
fn fs_main(input:Output)->@location(0) vec4<f32> {
    if dot(input.color,input.color)<0.00001 {discard;}
    var color=textureSample(base_texture,base_sampler,input.uv);
    if material.surface_flags.y!=0u {color=vec4<f32>(color.rgb*input.color,color.a);}
    if material.flags.y!=0u && color.a<0.5 {discard;}
    return color;
}
