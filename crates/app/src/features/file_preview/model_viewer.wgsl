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
    flags: vec4<u32>,
    surface_flags: vec4<u32>,
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

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var base_texture: texture_2d<f32>;
@group(1) @binding(1) var base_texture2: texture_2d<f32>;
@group(0) @binding(5) var lightmap_texture: texture_2d<f32>;
@group(1) @binding(3) var base_sampler: sampler;
@group(1) @binding(4) var lightmap_sampler: sampler;
@group(1) @binding(5) var<uniform> material: Material;
@group(1) @binding(6) var normal_texture: texture_2d_array<f32>;
@group(1) @binding(7) var normal_texture2: texture_2d_array<f32>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) lightmap_uv: vec2<f32>,
    @location(4) color: vec3<f32>,
    @location(5) blend_alpha: f32,
    @location(6) lightmap_bump_step:f32,
    @location(7) opacity:f32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) lightmap_uv: vec2<f32>,
    @location(3) color: vec3<f32>,
    @location(4) blend_alpha: f32,
    @location(5) world_position: vec3<f32>,
    @location(6) lightmap_bump_step:f32,
    @location(7) opacity:f32,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = uniforms.view_proj * vec4<f32>(input.position, 1.0);
    out.normal = input.normal;
    out.uv = input.uv;
    out.lightmap_uv = input.lightmap_uv;
    out.lightmap_bump_step=input.lightmap_bump_step;
    out.color = input.color;
    out.blend_alpha = input.blend_alpha;
    out.opacity = input.opacity;
    out.world_position = input.position;
    return out;
}

fn shade(input: VertexOutput) -> vec4<f32> {
    var albedo = textureSample(base_texture, base_sampler, input.uv);
    if material.surface_flags.x == 0u && input.blend_alpha > 0.0 {
        let albedo2 = textureSample(base_texture2, base_sampler, input.uv);
        albedo = mix(albedo, albedo2, clamp(input.blend_alpha, 0.0, 1.0));
    }
    if material.surface_flags.z != 0u { albedo.a *= clamp(input.opacity,0.0,1.0); }
    if material.flags.x != 0u {
        albedo.a = 1.0;
    } else if material.flags.y != 0u && albedo.a < 0.5 {
        discard;
    }
    var lit: vec3<f32>;
    if material.surface_flags.x != 0u {
        lit = albedo.rgb;
        if material.surface_flags.y != 0u { lit *= input.color; }
    } else if input.lightmap_uv.x != 0.0 || input.lightmap_uv.y != 0.0 {
        var lightmap = textureSample(lightmap_texture, lightmap_sampler, input.lightmap_uv).rgb * lightmap_scale();
        if material.flags.z != 0u && input.lightmap_bump_step > 0.0 {
            let uv = vec2<f32>(dot(vec4<f32>(input.uv,0.0,1.0),material.water_bump_s),dot(vec4<f32>(input.uv,0.0,1.0),material.water_bump_t));
            let first=textureSample(normal_texture,base_sampler,uv,0).xyz;
            let second=textureSample(normal_texture2,base_sampler,uv,0).xyz;
            let encoded=mix(first,second,clamp(input.blend_alpha,0.0,1.0));
            let normal=select(encoded*2.0-1.0,encoded,material.flags.z==2u);
            lightmap=bumped_lightmap(input.lightmap_uv,input.lightmap_bump_step,normal,material.flags.z==2u);
        }
        lit = albedo.rgb * lightmap;
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
    if uniforms.light.w < 0.0 && material.surface_flags.x == 0u { lit = albedo.rgb; }
    lit = mix(lit,uniforms.fog_color.rgb,view_fog_color_factor(input.world_position,1.0/input.clip_position.w));
    if dot(vec4<f32>(input.world_position, 1.0), uniforms.clip_plane) < 0.0 { discard; }
    return vec4<f32>(lit, albedo.a);
}

// Source height fog stores axial distance through the medium, rather than
// radial distance from the camera. The clipping bias is deliberately separate.
fn view_fog_factor(position:vec3<f32>, projected_depth:f32) -> f32 {
    if uniforms.fog_params.y <= uniforms.fog_params.x { return 0.0; }
    if uniforms.fog_params.w > 3.5 {
        return clamp((distance(position,uniforms.camera_position.xyz)-uniforms.fog_params.x)
            /(uniforms.fog_params.y-uniforms.fog_params.x),0.0,uniforms.fog_params.z);
    }
    if uniforms.fog_params.w > 2.5 {
        return clamp((projected_depth-uniforms.fog_params.x)
            /(uniforms.fog_params.y-uniforms.fog_params.x),0.0,uniforms.fog_params.z);
    }
    if uniforms.fog_params.w > 1.5 {
        let point_distance=dot(vec4<f32>(position,1.0),uniforms.medium_plane);
        let eye_distance=dot(vec4<f32>(uniforms.camera_position.xyz,1.0),uniforms.medium_plane);
        let fraction=clamp(-point_distance/max(eye_distance-point_distance,0.00001),0.0,1.0);
        return clamp(fraction*projected_depth/(uniforms.fog_params.y-uniforms.fog_params.x),0.0,uniforms.fog_params.z);
    }
    if uniforms.fog_params.w < 0.5 { return 0.0; }
    return clamp((projected_depth-uniforms.fog_params.x)
        /(uniforms.fog_params.y-uniforms.fog_params.x),0.0,uniforms.fog_params.z);
}

@fragment
fn fs_main(input:VertexOutput) -> @location(0) vec4<f32> { return shade(input); }

@fragment
fn fs_refraction(input:VertexOutput) -> @location(0) vec4<f32> {
    let colour=shade(input);
    let fog=select(1.0,view_fog_factor(input.world_position,1.0/input.clip_position.w),uniforms.fog_params.w>1.5 && uniforms.fog_params.w<2.5);
    return vec4<f32>(colour.rgb,fog);
}

@fragment
fn fs_additive(input:VertexOutput) -> @location(0) vec4<f32> {
    let colour=shade(input);
    // Additive light fades to black in a medium; adding its fog colour would
    // brighten the background again for every additive layer.
    let fog=view_fog_color_factor(input.world_position,1.0/input.clip_position.w);
    return vec4<f32>(max(colour.rgb-uniforms.fog_color.rgb*fog,vec3<f32>(0.0)),colour.a);
}

fn view_fog_color_factor(position:vec3<f32>,projected_depth:f32) -> f32 {
    let factor = view_fog_factor(position,projected_depth);
    // Source's ps2b+ range-fog blend squares opacity to match hardware fog.
    // Height-fog depth metadata and the water shader's own absorption stay linear.
    let opacity = select(factor*factor, factor, uniforms.fog_params.w>1.5 && uniforms.fog_params.w<2.5);
    return opacity*select(1.0,uniforms.fog_color.a,uniforms.fog_params.w>1.5 && uniforms.fog_params.w<3.5);
}

fn bumped_lightmap(uv:vec2<f32>, step:f32, tangent_normal:vec3<f32>, ssbump:bool) -> vec3<f32> {
    if step <= 0.0 { return textureSample(lightmap_texture, lightmap_sampler, uv).rgb * lightmap_scale(); }
    var weights = tangent_normal;
    if !ssbump {
        weights = clamp(vec3<f32>(dot(tangent_normal,vec3<f32>(0.81649658,0.0,0.57735027)),
            dot(tangent_normal,vec3<f32>(-0.40824829,0.70710678,0.57735027)),
            dot(tangent_normal,vec3<f32>(-0.40824829,-0.70710678,0.57735027))),vec3<f32>(0.0),vec3<f32>(1.0));
        weights *= weights;
        weights /= max(dot(weights,vec3<f32>(1.0)),0.000001);
    }
    let a=textureSample(lightmap_texture,lightmap_sampler,uv+vec2<f32>(step,0.0)).rgb;
    let b=textureSample(lightmap_texture,lightmap_sampler,uv+vec2<f32>(step*2.0,0.0)).rgb;
    let c=textureSample(lightmap_texture,lightmap_sampler,uv+vec2<f32>(step*3.0,0.0)).rgb;
    return (a*weights.x+b*weights.y+c*weights.z)*lightmap_scale();
}

// LDR lightmap bytes include half-strength gamma encoding before sRGB reads.
// The native 0..4 luxel sweep establishes the inverse scale 2^2.2.
fn lightmap_scale() -> f32 { return select(4.5947934,2.0,uniforms.viewport.z>0.5); }
