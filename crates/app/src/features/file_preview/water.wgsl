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
    clip_plane: vec4<f32>,
    medium_plane: vec4<f32>,
    viewport: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var refraction_texture: texture_2d<f32>;
@group(0) @binding(2) var refraction_sampler: sampler;
@group(0) @binding(3) var scene_depth: texture_depth_multisampled_2d;
@group(0) @binding(4) var reflection_texture: texture_2d<f32>;
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

fn transform_water_vertex(input: VertexInput) -> VertexOutput {
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
    var tangent_normal = vec4<f32>(encoded.xyz * 2.0 - 1.0, encoded.a);
    if material.flags.w != 0u { tangent_normal = vec4<f32>(normalize(tangent_normal.xyz), tangent_normal.a); }
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

fn apply_fog(color:vec3<f32>,position:vec3<f32>,projected_depth:f32) -> vec3<f32> {
    return mix(color,uniforms.fog_color.rgb,view_fog_color_factor(position,projected_depth));
}

// Source height fog stores axial distance through the medium, rather than
// radial distance from the camera. The clipping bias is deliberately separate.
fn view_fog_factor(position:vec3<f32>, projected_depth:f32) -> f32 {
    if uniforms.fog_params.y <= uniforms.fog_params.x { return 0.0; }
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
    return clamp((distance(position,uniforms.camera_position.xyz)-uniforms.fog_params.x)
        /(uniforms.fog_params.y-uniforms.fog_params.x),0.0,uniforms.fog_params.z);
}

@fragment
fn fs_main(input: VertexOutput, @builtin(sample_index) sample_index:u32) -> @location(0) vec4<f32> {
    let lighting = surface_lighting(input);
    let dimensions_u = textureDimensions(refraction_texture, 0);
    let dimensions = vec2<f32>(dimensions_u);
    let half_texel = vec2<f32>(0.5) / dimensions;
    let unperturbed_uv = clamp(input.clip_position.xy / uniforms.viewport.xy, half_texel, vec2<f32>(1.0) - half_texel);
    if material.flags.w != 0u {
        let reflected_uv = clamp(unperturbed_uv + vec2<f32>(-lighting.tangent_normal.x, lighting.tangent_normal.y) * lighting.tangent_normal.a * material.water_optics.x, half_texel, vec2<f32>(1.0) - half_texel);
        let refracted_uv = clamp(unperturbed_uv + lighting.tangent_normal.xy * lighting.tangent_normal.a * material.water_optics.y, half_texel, vec2<f32>(1.0) - half_texel);
        let reflected = textureSampleLevel(reflection_texture, refraction_sampler, reflected_uv, 0.0).rgb * material.water_reflect.rgb;
        let refracted = textureSampleLevel(refraction_texture, refraction_sampler, refracted_uv, 0.0).rgb * material.water_refract.rgb;
        let facing = clamp(dot(lighting.normal, normalize(uniforms.camera_position.xyz-input.world_position)), 0.0, 1.0);
        let fresnel = (material.water_scroll.y-material.water_scroll.x) * pow(1.0-facing, material.water_scroll.z) + material.water_scroll.x;
        var color = mix(refracted, reflected, fresnel);
        if material.water_refract.a < 0.5 { color = reflected; }
        if material.water_reflect.a < 0.5 { color = refracted; }
        return vec4<f32>(apply_fog(color, input.world_position,1.0/input.clip_position.w), 1.0);
    }
    let water_depth = input.clip_position.z;
    let original_sample=textureSampleLevel(refraction_texture,refraction_sampler,unperturbed_uv,0.0);
    let original_fog=select(1.0,original_sample.a,uniforms.water_depth_params.w>0.5 && material.water_state.z>0.5);
    let distortion=lighting.tangent_normal.xy*lighting.tangent_normal.a*material.water_optics.y*original_fog;
    var sample_uv=clamp(unperturbed_uv+distortion,half_texel,vec2<f32>(1.0)-half_texel);
    let sample_pixel=clamp(vec2<i32>(sample_uv*dimensions),vec2<i32>(0),vec2<i32>(dimensions_u)-vec2<i32>(1));
    let sampled_depth=textureLoad(scene_depth,sample_pixel,i32(sample_index));
    // A distorted fetch must not pull geometry in front of this surface into
    // its refraction. The refraction scene has independent, read-only depth.
    if sampled_depth>water_depth {sample_uv=unperturbed_uv;}
    let sampled=textureSampleLevel(refraction_texture,refraction_sampler,sample_uv,0.0);
    let refracted=sampled.rgb*material.water_refract.rgb;
    var absorption=clamp(sampled.a-0.05,0.0,1.0)*material.water_fog.a;
    if material.water_state.z<0.5 {
        absorption=clamp((1.0/input.clip_position.w-material.water_optics.z)
            /max(material.water_optics.w-material.water_optics.z,0.0001),0.0,1.0)*material.water_fog.a;
    }
    let absorbed=mix(refracted,material.water_fog.rgb,absorption);
    let fresnel=lighting.fresnel*clamp((sampled.a-0.05)*20.0,0.0,1.0);
    var reflection=uniforms.water_time_sky_tint.yzw;
    if uniforms.water_depth_params.z>0.5 {
        let reflection_offset=vec2<f32>(lighting.tangent_normal.x,-lighting.tangent_normal.y)
            *lighting.tangent_normal.a*material.water_optics.x*original_fog;
        reflection=textureSampleLevel(reflection_texture,refraction_sampler,
            clamp(unperturbed_uv+reflection_offset,half_texel,vec2<f32>(1.0)-half_texel),0.0).rgb;
    }
    let reflected = reflection * material.water_reflect.rgb;
    var color = material.water_fog.rgb;
    if material.water_refract.a > 0.5 { color = absorbed; }
    if material.water_reflect.a > 0.5 {
        color = mix(color, reflected, select(1.0, fresnel, material.water_refract.a > 0.5));
    }
    return vec4<f32>(apply_fog(color, input.world_position,1.0/input.clip_position.w), 1.0);
}

@fragment
fn fs_skybox(input: VertexOutput) -> @location(0) vec4<f32> {
    let lighting = surface_lighting(input);
    if dot(vec4<f32>(input.world_position, 1.0), uniforms.clip_plane) < 0.0 { discard; }
    var color = mix(material.water_fog.rgb, uniforms.water_time_sky_tint.yzw * material.water_reflect.rgb, lighting.fresnel * material.water_reflect.a);
    if material.flags.w != 0u { color = uniforms.water_time_sky_tint.yzw * material.water_reflect.rgb; }
    return vec4<f32>(apply_fog(color, input.world_position,1.0/input.clip_position.w),select(1.0,view_fog_factor(input.world_position,1.0/input.clip_position.w),uniforms.fog_params.w>1.5));
}

fn view_fog_color_factor(position:vec3<f32>,projected_depth:f32) -> f32 {
    let factor = view_fog_factor(position,projected_depth);
    // Source's ps2b+ range-fog blend squares opacity to match hardware fog.
    // Height-fog depth metadata and the water shader's own absorption stay linear.
    let opacity = select(factor, factor*factor, uniforms.fog_params.w>2.5);
    return opacity*select(1.0,uniforms.fog_color.a,uniforms.fog_params.w>1.5);
}


// Preserve the surface's exact projected footprint when it lies between the
// eye and the near plane. Projecting at near depth keeps the authored screen-space polygon
// boundaries; the fragment test limits coverage to near-plane intersections.
@vertex
fn vs_near_main(input: VertexInput) -> VertexOutput {
    var adjusted=input;
    let eye_distance=dot(vec4<f32>(uniforms.camera_position.xyz,1.0),uniforms.medium_plane);
    // At the exact boundary the projected surface collapses to a line. A
    // representable bias for this mask only keeps its footprint well-defined;
    // actual surface/fog heights and the medium test remain unbiased.
    let epsilon=max(0.001,abs(uniforms.medium_plane.w)*0.000001);
    if eye_distance<epsilon {
        adjusted.position-=uniforms.medium_plane.xyz*(epsilon-eye_distance);
    }
    var out = transform_water_vertex(adjusted);
    out.clip_position.z = out.clip_position.w;
    return out;
}

@fragment
fn fs_near_intersection(input: VertexOutput) -> @location(0) vec4<f32> {
    let x_axis = vec3<f32>(uniforms.view_proj[0][0],uniforms.view_proj[1][0],uniforms.view_proj[2][0]);
    let y_axis = vec3<f32>(uniforms.view_proj[0][1],uniforms.view_proj[1][1],uniforms.view_proj[2][1]);
    let forward = normalize(vec3<f32>(uniforms.view_proj[0][3],uniforms.view_proj[1][3],uniforms.view_proj[2][3]));
    let uv = input.clip_position.xy/uniforms.viewport.xy;
    let ndc = vec2<f32>(uv.x*2.0-1.0,1.0-uv.y*2.0);
    let near_point = uniforms.camera_position.xyz+uniforms.water_depth_params.x*
        (forward+x_axis*ndc.x/dot(x_axis,x_axis)+y_axis*ndc.y/dot(y_axis,y_axis));
    if dot(vec4<f32>(near_point,1.0),uniforms.medium_plane)>=0.0 { discard; }
    let color = textureSampleLevel(refraction_texture,refraction_sampler,uv,0.0).rgb;
    return vec4<f32>(color,1.0);
}


@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    return transform_water_vertex(input);
}
