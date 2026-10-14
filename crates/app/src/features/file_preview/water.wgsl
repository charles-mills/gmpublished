struct Uniforms {
    view_proj: mat4x4<f32>,
    light: vec4<f32>,
    camera_position: vec4<f32>,
    fog_color: vec4<f32>,
    fog_params: vec4<f32>,
    // x: animation time, yzw: sky tint.
    water_time_sky_tint: vec4<f32>,
    // xy: near and far clip distances.
    water_depth_params: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var refraction_texture: texture_2d<f32>;
@group(0) @binding(2) var refraction_sampler: sampler;
@group(0) @binding(3) var scene_depth: texture_depth_multisampled_2d;
@group(1) @binding(0) var base_texture: texture_2d<f32>;
@group(1) @binding(3) var base_sampler: sampler;

struct Material {
    flags: vec4<u32>,
    water_fog: vec4<f32>,
    water_optics: vec4<f32>,
    water_reflect: vec4<f32>,
    water_refract: vec4<f32>,
    water_bump_s: vec4<f32>,
    water_bump_t: vec4<f32>,
    water_scroll: vec4<f32>,
    water_animation: vec4<f32>,
    water_state: vec4<f32>,
}
@group(1) @binding(5) var<uniform> material: Material;
@group(1) @binding(6) var normal_texture: texture_2d_array<f32>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = uniforms.view_proj * vec4<f32>(input.position, 1.0);
    out.world_position = input.position;
    out.normal = input.normal;
    out.uv = input.uv;
    return out;
}

struct SurfaceLighting {
    normal: vec3<f32>,
    tangent_normal: vec4<f32>,
    fresnel: f32,
}

fn surface_lighting(input: VertexOutput) -> SurfaceLighting {
    let time = uniforms.water_time_sky_tint.x;
    let uv = input.uv;
    let du = dpdx(uv);
    let dv = dpdy(uv);
    let bump_uv = vec2<f32>(dot(vec4<f32>(uv, 0.0, 1.0), material.water_bump_s), dot(vec4<f32>(uv, 0.0, 1.0), material.water_bump_t)) + material.water_animation.xy * time;
    let bump_du = vec2<f32>(dot(du, material.water_bump_s.xy), dot(du, material.water_bump_t.xy));
    let bump_dv = vec2<f32>(dot(dv, material.water_bump_s.xy), dot(dv, material.water_bump_t.xy));
    let raw_frame = i32(floor(material.water_animation.z + time * material.water_animation.w));
    let frame_count = max(i32(material.water_state.x), 1);
    let frame = select(raw_frame % frame_count, min(raw_frame, frame_count - 1), material.flags.z != 0u);
    var encoded = textureSampleGrad(normal_texture, base_sampler, bump_uv, frame, bump_du, bump_dv);
    if material.water_state.y > 0.5 {
        let extra_a = vec2<f32>(uv.x + uv.y, uv.y - uv.x) * 0.1 + material.water_scroll.xy * time;
        let extra_b = uv.yx * 0.45 + material.water_scroll.zw * time;
        let normal_a = textureSampleGrad(normal_texture, base_sampler, extra_a, frame, vec2<f32>(du.x + du.y, du.y - du.x) * 0.1, vec2<f32>(dv.x + dv.y, dv.y - dv.x) * 0.1);
        let normal_b = textureSampleGrad(normal_texture, base_sampler, extra_b, frame, du.yx * 0.45, dv.yx * 0.45);
        encoded = (encoded + normal_a + normal_b) * 0.33;
    }
    let tangent_normal = vec4<f32>(encoded.xyz * 2.0 - 1.0, encoded.a);
    let px = dpdx(input.world_position);
    let py = dpdy(input.world_position);
    let determinant = du.x * dv.y - du.y * dv.x;
    let inverse = select(-1.0, 1.0, determinant >= 0.0) / max(abs(determinant), 0.00000001);
    let tangent_s = normalize((px * dv.y - py * du.y) * inverse);
    let tangent_t = normalize((py * du.x - px * dv.x) * inverse);
    let surface_normal = tangent_s * tangent_normal.x + tangent_t * tangent_normal.y + normalize(input.normal) * tangent_normal.z;
    let view_direction = normalize(uniforms.camera_position.xyz - input.world_position);
    let facing = clamp(dot(surface_normal, view_direction), 0.0, 1.0);
    return SurfaceLighting(surface_normal, tangent_normal, pow(1.0 - facing, 5.0));
}

fn apply_fog(color_in: vec3<f32>, world_position: vec3<f32>) -> vec3<f32> {
    var color = color_in;

    if uniforms.fog_params.w > 0.5 && uniforms.fog_params.y > uniforms.fog_params.x {
        let fog_distance = distance(world_position, uniforms.camera_position.xyz);
        let fog_factor = clamp(
            (fog_distance - uniforms.fog_params.x) / (uniforms.fog_params.y - uniforms.fog_params.x),
            0.0,
            uniforms.fog_params.z,
        );
        color = mix(color, uniforms.fog_color.rgb, fog_factor);
    }
    return color;
}

fn linearize_depth(depth: f32) -> f32 {
    let near = uniforms.water_depth_params.x;
    let far = uniforms.water_depth_params.y;
    return near * far / (near + depth * (far - near));
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let lighting = surface_lighting(input);
    let dimensions_u = textureDimensions(refraction_texture, 0);
    let dimensions = vec2<f32>(dimensions_u);
    let half_texel = vec2<f32>(0.5) / dimensions;
    let unperturbed_uv = clamp(input.clip_position.xy / dimensions, half_texel, vec2<f32>(1.0) - half_texel);
    let unperturbed_pixel = clamp(vec2<i32>(unperturbed_uv * dimensions), vec2<i32>(0), vec2<i32>(dimensions_u) - vec2<i32>(1));
    let water_depth = linearize_depth(input.clip_position.z);
    let view_distance = distance(input.world_position, uniforms.camera_position.xyz);
    let ray_scale = view_distance / max(water_depth, 0.0001);
    let original_depth = textureLoad(scene_depth, unperturbed_pixel, 0);
    let original_thickness = max(linearize_depth(original_depth) - water_depth, 0.0) * ray_scale;
    let fog_range = max(material.water_optics.w - material.water_optics.z, 0.0001);
    let original_fog = clamp((original_thickness - material.water_optics.z) / fog_range, 0.0, 1.0);
    let distortion = lighting.tangent_normal.xy * lighting.tangent_normal.a * material.water_optics.y * original_fog;
    var sample_uv = clamp(unperturbed_uv + distortion, half_texel, vec2<f32>(1.0) - half_texel);
    var sample_pixel = clamp(vec2<i32>(sample_uv * dimensions), vec2<i32>(0), vec2<i32>(dimensions_u) - vec2<i32>(1));
    var sampled_depth = textureLoad(scene_depth, sample_pixel, 0);
    if sampled_depth > input.clip_position.z {
        sample_uv = unperturbed_uv;
        sampled_depth = original_depth;
    }
    let refracted = textureSampleLevel(refraction_texture, refraction_sampler, sample_uv, 0.0).rgb * material.water_refract.rgb;
    let thickness = max(linearize_depth(sampled_depth) - water_depth, 0.0) * ray_scale;
    let depth_value = clamp((thickness - material.water_optics.z) / fog_range, 0.0, 1.0);
    let absorption = clamp(depth_value - 0.05, 0.0, 1.0) * material.water_fog.a;
    let absorbed = mix(refracted, material.water_fog.rgb, absorption);
    let fresnel = lighting.fresnel * clamp((depth_value - 0.05) * 20.0, 0.0, 1.0);
    let reflected = uniforms.water_time_sky_tint.yzw * material.water_reflect.rgb;
    var color = material.water_fog.rgb;
    if material.water_refract.a > 0.5 { color = absorbed; }
    if material.water_reflect.a > 0.5 {
        color = mix(color, reflected, select(1.0, fresnel, material.water_refract.a > 0.5));
    }
    return vec4<f32>(apply_fog(color, input.world_position), 1.0);
}

@fragment
fn fs_skybox(input: VertexOutput) -> @location(0) vec4<f32> {
    let lighting = surface_lighting(input);
    let color = mix(material.water_fog.rgb, uniforms.water_time_sky_tint.yzw * material.water_reflect.rgb, lighting.fresnel * material.water_reflect.a);
    return vec4<f32>(apply_fog(color, input.world_position), 1.0);
}
