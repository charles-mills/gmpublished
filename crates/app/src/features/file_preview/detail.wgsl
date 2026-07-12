struct Uniforms {
    view_proj: mat4x4<f32>,
    light: vec4<f32>,
    camera_position: vec4<f32>,
    fog_color: vec4<f32>,
    fog_params: vec4<f32>,
}

struct Material {
    // x: force opaque, y: alpha test cutout.
    flags: vec4<u32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var base_texture: texture_2d<f32>;
@group(1) @binding(1) var base_texture2: texture_2d<f32>;
@group(1) @binding(2) var lightmap_texture: texture_2d<f32>;
@group(1) @binding(3) var base_sampler: sampler;
@group(1) @binding(4) var lightmap_sampler: sampler;
@group(1) @binding(5) var<uniform> material: Material;

struct VertexInput {
    @location(0) center: vec3<f32>,
    @location(1) corner: vec2<f32>,
    @location(2) uv: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) world_position: vec3<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let to_camera_xy = uniforms.camera_position.xy - input.center.xy;
    var facing = vec2<f32>(1.0, 0.0);
    if dot(to_camera_xy, to_camera_xy) > 0.0001 {
        facing = normalize(to_camera_xy);
    }
    let right = vec3<f32>(-facing.y, facing.x, 0.0);
    let world_position =
        input.center + right * input.corner.x + vec3<f32>(0.0, 0.0, input.corner.y);

    var out: VertexOutput;
    out.clip_position = uniforms.view_proj * vec4<f32>(world_position, 1.0);
    out.uv = input.uv;
    out.world_position = world_position;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let detail_fade_distance = 1024.0;
    let fade_start = detail_fade_distance * 0.75;
    let sprite_distance = distance(input.world_position, uniforms.camera_position.xyz);
    if sprite_distance > detail_fade_distance {
        discard;
    }

    var albedo = textureSample(base_texture, base_sampler, input.uv);
    if material.flags.x != 0u {
        albedo.a = 1.0;
    } else if material.flags.y != 0u && albedo.a < 0.5 {
        discard;
    }

    let fade_alpha = clamp(
        (detail_fade_distance - sprite_distance) / (detail_fade_distance - fade_start),
        0.0,
        1.0,
    );
    albedo.a *= fade_alpha;
    if albedo.a <= 0.0 {
        discard;
    }

    var lit = albedo.rgb;
    if uniforms.fog_params.w > 0.5 && uniforms.fog_params.y > uniforms.fog_params.x {
        let fog_factor = clamp(
            (sprite_distance - uniforms.fog_params.x) / (uniforms.fog_params.y - uniforms.fog_params.x),
            0.0,
            uniforms.fog_params.z,
        );
        lit = mix(lit, uniforms.fog_color.rgb, fog_factor);
    }
    return vec4<f32>(lit, albedo.a);
}
