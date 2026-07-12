struct Uniforms {
    view_proj: mat4x4<f32>,
    // Directional light in world space (xyz) + ambient strength (w).
    light: vec4<f32>,
    camera_position: vec4<f32>,
    fog_color: vec4<f32>,
    // start, end, max density, enabled flag.
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
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) lightmap_uv: vec2<f32>,
    @location(4) color: vec3<f32>,
    @location(5) blend_alpha: f32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) lightmap_uv: vec2<f32>,
    @location(3) color: vec3<f32>,
    @location(4) blend_alpha: f32,
    @location(5) world_position: vec3<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = uniforms.view_proj * vec4<f32>(input.position, 1.0);
    out.normal = input.normal;
    out.uv = input.uv;
    out.lightmap_uv = input.lightmap_uv;
    out.color = input.color;
    out.blend_alpha = input.blend_alpha;
    out.world_position = input.position;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    var albedo = textureSample(base_texture, base_sampler, input.uv);
    if input.blend_alpha > 0.0 {
        let albedo2 = textureSample(base_texture2, base_sampler, input.uv);
        albedo = mix(albedo, albedo2, clamp(input.blend_alpha, 0.0, 1.0));
    }
    if material.flags.x != 0u {
        albedo.a = 1.0;
    } else if material.flags.y != 0u && albedo.a < 0.5 {
        discard;
    }
    var lit: vec3<f32>;
    if input.lightmap_uv.x != 0.0 || input.lightmap_uv.y != 0.0 {
        let lightmap = textureSample(lightmap_texture, lightmap_sampler, input.lightmap_uv).rgb;
        // Source applies a 2x overbright factor to baked lightmaps.
        lit = albedo.rgb * lightmap * 2.0;
    // Vertex colors are byte-quantized (1/255 steps), and attribute
    // interpolation is not exact on every GPU: an exact `== 1.0` compare
    // flickers per fragment on NVIDIA, dithering every textured surface.
    // Half a quantization step of tolerance keeps "no vertex color" stable
    // without absorbing real vertex colors.
    } else if all(abs(input.color - vec3<f32>(1.0)) < vec3<f32>(0.5 / 255.0)) {
        let ambient = uniforms.light.w;
        let diffuse = max(dot(normalize(input.normal), normalize(uniforms.light.xyz)), 0.0);
        lit = albedo.rgb * min(ambient + diffuse * (1.0 - ambient), 1.0);
    } else {
        lit = albedo.rgb * input.color * 2.0;
    }
    if uniforms.fog_params.w > 0.5 && uniforms.fog_params.y > uniforms.fog_params.x {
        let fog_distance = distance(input.world_position, uniforms.camera_position.xyz);
        let fog_factor = clamp(
            (fog_distance - uniforms.fog_params.x) / (uniforms.fog_params.y - uniforms.fog_params.x),
            0.0,
            uniforms.fog_params.z,
        );
        lit = mix(lit, uniforms.fog_color.rgb, fog_factor);
    }
    return vec4<f32>(lit, albedo.a);
}
