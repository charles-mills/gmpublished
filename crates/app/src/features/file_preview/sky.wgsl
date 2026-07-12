struct Uniforms {
    view_proj: mat4x4<f32>,
    light: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var sky_texture: texture_2d<f32>;
@group(1) @binding(1) var sky_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @builtin(vertex_index) vertex_index: u32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

fn corner_uv(vertex_index: u32) -> vec2<f32> {
    switch vertex_index % 6u {
        case 0u: {
            return vec2<f32>(0.0, 1.0);
        }
        case 1u: {
            return vec2<f32>(0.0, 0.0);
        }
        case 2u: {
            return vec2<f32>(1.0, 0.0);
        }
        case 3u: {
            return vec2<f32>(0.0, 1.0);
        }
        case 4u: {
            return vec2<f32>(1.0, 0.0);
        }
        default: {
            return vec2<f32>(1.0, 1.0);
        }
    }
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let clip = uniforms.view_proj * vec4<f32>(input.position, 1.0);
    var out: VertexOutput;
    out.clip_position = vec4<f32>(clip.xy, 0.0, clip.w);
    out.uv = corner_uv(input.vertex_index);
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let dims = textureDimensions(sky_texture, 0);
    let size = vec2<f32>(f32(dims.x), f32(dims.y));
    let half_texel = vec2<f32>(0.5, 0.5) / max(size, vec2<f32>(1.0, 1.0));
    let uv = clamp(input.uv, half_texel, vec2<f32>(1.0, 1.0) - half_texel);
    return textureSample(sky_texture, sky_sampler, uv);
}
