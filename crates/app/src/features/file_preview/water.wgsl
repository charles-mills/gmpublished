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
@group(1) @binding(0) var base_texture: texture_2d<f32>;
@group(1) @binding(3) var base_sampler: sampler;
@group(2) @binding(0) var refraction_texture: texture_2d<f32>;
@group(2) @binding(1) var refraction_sampler: sampler;
@group(2) @binding(2) var scene_depth: texture_depth_multisampled_2d;

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

fn wave_gradient(position: vec2<f32>, time: f32) -> vec2<f32> {
    var gradient = vec2<f32>(0.0);
    let direction_a = vec2<f32>(1.0, 0.18);
    let direction_b = vec2<f32>(-0.36, 0.93);
    let direction_c = vec2<f32>(0.68, 0.73);
    let direction_d = vec2<f32>(-0.88, -0.48);
    gradient += direction_a * cos(dot(position, direction_a) * 0.018 + time * 1.1) * 0.030;
    gradient += direction_b * cos(dot(position, direction_b) * 0.031 - time * 0.8) * 0.024;
    gradient += direction_c * cos(dot(position, direction_c) * 0.047 + time * 1.4) * 0.018;
    gradient += direction_d * cos(dot(position, direction_d) * 0.071 - time * 1.7) * 0.012;
    return gradient;
}

struct SurfaceLighting {
    normal: vec3<f32>,
    view_direction: vec3<f32>,
    fresnel: f32,
    deep_color: vec3<f32>,
    glint: vec3<f32>,
}

fn surface_lighting(input: VertexOutput) -> SurfaceLighting {
    let gradient = wave_gradient(input.world_position.xy, uniforms.water_time_sky_tint.x);
    let surface_normal = normalize(input.normal + vec3<f32>(-gradient, 0.0));
    let view_direction = normalize(uniforms.camera_position.xyz - input.world_position);
    let facing = clamp(dot(surface_normal, view_direction), 0.0, 1.0);
    let fresnel = 0.02 + 0.98 * pow(1.0 - facing, 5.0);
    let deep_color = textureSample(base_texture, base_sampler, input.uv).rgb;
    let light_direction = normalize(uniforms.light.xyz);
    let half_vector = normalize(light_direction + view_direction);
    let glint_strength = pow(max(dot(surface_normal, half_vector), 0.0), 256.0);
    return SurfaceLighting(
        surface_normal,
        view_direction,
        fresnel,
        deep_color,
        vec3<f32>(1.0, 0.96, 0.88) * glint_strength * 0.8,
    );
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
    let unperturbed_uv = clamp(
        input.clip_position.xy / dimensions,
        vec2<f32>(0.5) / dimensions,
        (dimensions - vec2<f32>(0.5)) / dimensions,
    );
    let view_distance = distance(input.world_position, uniforms.camera_position.xyz);
    let distance_falloff = clamp(512.0 / max(view_distance, 1.0), 0.1, 1.0);
    var sample_uv = clamp(
        unperturbed_uv + lighting.normal.xy * 0.02 * distance_falloff,
        vec2<f32>(0.5) / dimensions,
        (dimensions - vec2<f32>(0.5)) / dimensions,
    );
    var sample_pixel = clamp(
        vec2<i32>(sample_uv * dimensions),
        vec2<i32>(0),
        vec2<i32>(dimensions_u) - vec2<i32>(1),
    );
    var sampled_depth = textureLoad(scene_depth, sample_pixel, 0);
    if sampled_depth > input.clip_position.z {
        sample_uv = unperturbed_uv;
        sample_pixel = clamp(
            vec2<i32>(sample_uv * dimensions),
            vec2<i32>(0),
            vec2<i32>(dimensions_u) - vec2<i32>(1),
        );
        sampled_depth = textureLoad(scene_depth, sample_pixel, 0);
    }

    let refracted = textureSample(refraction_texture, refraction_sampler, sample_uv).rgb;
    let water_depth = linearize_depth(input.clip_position.z);
    let background_depth = linearize_depth(sampled_depth);
    let thickness = max(background_depth - water_depth, 0.0);
    let absorption = 1.0 - exp(-thickness * 0.0025);
    let absorbed_refraction = mix(refracted, lighting.deep_color, absorption);
    let color = mix(
        absorbed_refraction,
        uniforms.water_time_sky_tint.yzw,
        lighting.fresnel,
    ) + lighting.glint;

    return vec4<f32>(apply_fog(color, input.world_position), 1.0);
}

@fragment
fn fs_skybox(input: VertexOutput) -> @location(0) vec4<f32> {
    let lighting = surface_lighting(input);
    let color = mix(
        lighting.deep_color,
        uniforms.water_time_sky_tint.yzw,
        lighting.fresnel,
    ) + lighting.glint;

    return vec4<f32>(
        apply_fog(color, input.world_position),
        mix(0.75, 1.0, lighting.fresnel),
    );
}
