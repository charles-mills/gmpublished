
@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var base_texture: texture_2d<f32>;
@group(1) @binding(3) var base_sampler: sampler;
@group(1) @binding(5) var<uniform> material: Material;

struct VertexInput {
    @location(0) center: vec3<f32>,
    @location(1) corner: vec2<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) orientation:f32,
    @location(4) axis_right:vec3<f32>,
    @location(5) axis_up:vec3<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) world_position: vec3<f32>,
    @location(2) clip_distance:f32,
}

fn transform_detail_vertex(input: VertexInput) -> VertexOutput {
    let to_camera_xy = uniforms.camera_position.xy - input.center.xy;
    var facing = vec2<f32>(1.0, 0.0);
    if dot(to_camera_xy, to_camera_xy) > 0.0001 {
        facing = normalize(to_camera_xy);
    }
    var right = vec3<f32>(-facing.y, facing.x, 0.0);
    var up=vec3<f32>(0.0,0.0,1.0);
    if input.orientation<0.5 {
        right=input.axis_right;up=input.axis_up;
    } else if input.orientation<1.5 {
        let to_camera = uniforms.camera_position.xyz - input.center;
        if dot(to_camera, to_camera) > 0.0001 {
            up = normalize(cross(normalize(to_camera), right));
        }
    }
    let world_position = input.center+right*input.corner.x+up*input.corner.y;

    var out: VertexOutput;
    out.clip_position = uniforms.view_proj * vec4<f32>(world_position, 1.0);
    out.uv = input.uv;
    out.world_position = world_position;
    out.clip_distance=view_clip_distance(world_position);
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
    if material.texture_alpha_opaque != 0u {
        albedo.a = 1.0;
    } else if material.alpha_test != 0u && albedo.a < 0.5 {
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
    lit = mix(lit,uniforms.fog_color.rgb,view_fog_color_factor(input.world_position,1.0/input.clip_position.w));
    if view_fragment_is_clipped(input.clip_distance) { discard; }
    return vec4<f32>(lit, albedo.a);
}
