struct Uniforms {
    view_proj: mat4x4<f32>,
    // Directional light in world space (xyz) + ambient strength (w).
    light: vec4<f32>,
    camera_position: vec4<f32>,
    fog_color: vec4<f32>,
    // start, end, max density, enabled flag.
    fog_params: vec4<f32>,
    // x: animation time, yzw: sky tint.
    water_time_sky_tint: vec4<f32>,
    // xy: near and far clip distances.
    water_depth_params: vec4<f32>,
    clip_plane: vec4<f32>,
    medium_plane: vec4<f32>,
    viewport: vec4<f32>,
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
    @location(3) orientation:f32,
    @location(4) axis_right:vec3<f32>,
    @location(5) axis_up:vec3<f32>,
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
    var right = vec3<f32>(-facing.y, facing.x, 0.0);
    var up=vec3<f32>(0.0,0.0,1.0);
    if input.orientation<0.5 {
        right=input.axis_right;up=input.axis_up;
    } else if input.orientation<1.5 {
        // Source computes angles from each sprite to the current eye, rather
        // than using one camera-wide basis for every screen-aligned sprite.
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
    lit = mix(lit,uniforms.fog_color.rgb,view_fog_color_factor(input.world_position,1.0/input.clip_position.w));
    if dot(vec4<f32>(input.world_position, 1.0), uniforms.clip_plane) < 0.0 { discard; }
    return vec4<f32>(lit, albedo.a);
}

// Source height fog stores axial distance through the medium, rather than
// radial distance from the camera. The clipping bias is deliberately separate.
fn view_fog_factor(position:vec3<f32>, projected_depth:f32) -> f32 {
    if uniforms.fog_params.y <= uniforms.fog_params.x { return 0.0; }
    if uniforms.fog_params.w > 1.5 {
        let point_distance=dot(vec4<f32>(position,1.0),uniforms.medium_plane);
        let eye_distance=dot(vec4<f32>(uniforms.camera_position.xyz,1.0),uniforms.medium_plane);
        let fraction=clamp(-point_distance/max(eye_distance-point_distance,0.00001),0.0,1.0);
        return clamp(fraction*projected_depth/(uniforms.fog_params.y-uniforms.fog_params.x),0.0,uniforms.fog_params.z);
    }
    if uniforms.fog_params.w < 0.5 { return 0.0; }
    return clamp((distance(position,uniforms.camera_position.xyz)-uniforms.fog_params.x)
        /(uniforms.fog_params.y-uniforms.fog_params.x),0.0,uniforms.fog_params.z);
}

fn view_fog_color_factor(position:vec3<f32>,projected_depth:f32) -> f32 {
    return view_fog_factor(position,projected_depth)*select(1.0,uniforms.fog_color.a,uniforms.fog_params.w>1.5);
}
