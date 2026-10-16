@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var refraction_texture: texture_2d<f32>;
@group(0) @binding(2) var refraction_sampler: sampler;
@group(0) @binding(3) var scene_depth: texture_depth_2d;
@group(0) @binding(4) var reflection_texture: texture_2d<f32>;
@group(1) @binding(0) var base_texture: texture_2d<f32>;
@group(1) @binding(3) var base_sampler: sampler;
@group(1) @binding(22) var normal_sampler:sampler;
@group(1) @binding(4) var lightmap_sampler: sampler;
@group(0) @binding(6) var lightmap_texture:texture_2d<f32>;

@group(1) @binding(5) var<uniform> material: Material;
@group(1) @binding(6) var normal_texture: texture_2d_array<f32>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) lightmap_uv:vec2<f32>,
    @location(6) lightmap_bump_step:f32,
    @location(10) tangent_s:vec3<f32>,
    @location(11) tangent_t:vec3<f32>,
    @location(12) tangent_sign:f32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) lightmap_uv:vec2<f32>,
    @location(4) lightmap_bump_step:f32,
    @location(5) clip_distance:f32,
    @location(10) tangent_s:vec3<f32>,
    @location(11) tangent_t:vec3<f32>,
}

fn transform_water_vertex(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = uniforms.view_proj * vec4<f32>(input.position, 1.0);
    out.world_position = input.position;
    out.clip_distance=view_clip_distance(input.position);
    out.normal = input.normal;
    out.tangent_s = input.tangent_s;
    out.tangent_t = input.tangent_t;
    out.uv = input.uv;
    out.lightmap_uv=input.lightmap_uv;
    out.lightmap_bump_step=input.lightmap_bump_step;
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
    let bump_uv = vec2<f32>(dot(vec4<f32>(uv, 0.0, 1.0), material.normal_s), dot(vec4<f32>(uv, 0.0, 1.0), material.normal_t)) + material.water_animation.xy * time;
    let bump_du = vec2<f32>(dot(du, material.normal_s.xy), dot(du, material.normal_t.xy));
    let bump_dv = vec2<f32>(dot(dv, material.normal_s.xy), dot(dv, material.normal_t.xy));
    let raw_frame = i32(floor(material.water_animation.z + time * material.water_animation.w));
    let frame_count = max(i32(material.water_state.x), 1);
    let frame = select(raw_frame % frame_count, min(raw_frame, frame_count - 1), material.normal_animation_clamp != 0u);
    var encoded = textureSampleGrad(normal_texture, normal_sampler, bump_uv, frame, bump_du, bump_dv);
    if material.water_state.y > 0.5 {
        let extra_a = vec2<f32>(uv.x + uv.y, uv.y - uv.x) * 0.1 + material.water_scroll.xy * time;
        let extra_b = uv.yx * 0.45 + material.water_scroll.zw * time;
        let normal_a = textureSampleGrad(normal_texture, normal_sampler, extra_a, frame, vec2<f32>(du.x + du.y, du.y - du.x) * 0.1, vec2<f32>(dv.x + dv.y, dv.y - dv.x) * 0.1);
        let normal_b = textureSampleGrad(normal_texture, normal_sampler, extra_b, frame, du.yx * 0.45, dv.yx * 0.45);
        encoded = (encoded + normal_a + normal_b) * 0.33;
    }
    var tangent_normal = vec4<f32>(encoded.xyz * 2.0 - 1.0, encoded.a);
    if material.planar_kind == PLANAR_MIRROR { tangent_normal = vec4<f32>(normalize(tangent_normal.xyz), tangent_normal.a); }
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



@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if view_fragment_is_clipped(input.clip_distance) { discard; }
    let lighting = surface_lighting(input);
    let dimensions_u = textureDimensions(refraction_texture, 0);
    let dimensions = vec2<f32>(dimensions_u);
    let half_texel = vec2<f32>(0.5) / dimensions;
    // Project at the interpolated position before raster bias, independent of MSAA sample locations.
    let projected=uniforms.view_proj*vec4<f32>(input.world_position,1.0);
    let projected_uv=projected.xy/projected.w*vec2<f32>(0.5,-0.5)+vec2<f32>(0.5)
        -uniforms.raster_params.xy/uniforms.viewport_size;
    let unperturbed_uv = clamp(projected_uv, half_texel, vec2<f32>(1.0) - half_texel);
    if material.planar_kind == PLANAR_MIRROR {
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
    let original_fog=select(1.0,original_sample.a,uniforms.water_depth_params.w>0.5 && material.water_state.z>0.5 && material.planar_kind!=PLANAR_TEXTURED_WATER);
    let distortion=lighting.tangent_normal.xy*lighting.tangent_normal.a*material.water_optics.y*original_fog;
    var sample_uv=clamp(unperturbed_uv+distortion,half_texel,vec2<f32>(1.0)-half_texel);
    let sample_pixel=clamp(vec2<i32>(sample_uv*dimensions),vec2<i32>(0),vec2<i32>(dimensions_u)-vec2<i32>(1));
    let sampled_depth=textureLoad(scene_depth,sample_pixel,0);
    // Reject distorted refraction samples in front of the surface.
    if sampled_depth>water_depth {sample_uv=unperturbed_uv;}
    let sampled=textureSampleLevel(refraction_texture,refraction_sampler,sample_uv,0.0);
    let refracted=sampled.rgb*material.water_refract.rgb;
    var absorption=clamp(sampled.a-0.05,0.0,1.0)*material.water_fog.a;
    if material.water_state.z<0.5 {
        absorption=clamp((1.0/input.clip_position.w-material.water_optics.z)
            /max(material.water_optics.w-material.water_optics.z,0.0001),0.0,1.0)*material.water_fog.a;
    }
    let absorbed=mix(refracted,material.water_fog.rgb,absorption);
    let fresnel=lighting.fresnel*select(clamp((sampled.a-0.05)*20.0,0.0,1.0),1.0,material.planar_kind==PLANAR_TEXTURED_WATER);
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
    if material.planar_kind==PLANAR_TEXTURED_WATER && material.water_reflect.a>0.5 && material.water_refract.a<0.5 {
        let uv=vec2<f32>(dot(vec4<f32>(input.uv,0.0,1.0),material.normal_s),dot(vec4<f32>(input.uv,0.0,1.0),material.normal_t))+material.water_animation.xy*uniforms.water_time_sky_tint.x;
        let base=textureSample(base_texture,base_sampler,uv);
        let diffuse=bumped_lightmap(input.lightmap_uv,input.lightmap_bump_step,lighting.tangent_normal.xyz,false);
        color=base.rgb*diffuse+reflected*fresnel*base.a;
    }
    return vec4<f32>(apply_fog(color, input.world_position,1.0/input.clip_position.w), 1.0);
}

@fragment
fn fs_skybox(input: VertexOutput) -> @location(0) vec4<f32> {
    let lighting = surface_lighting(input);
    if view_fragment_is_clipped(input.clip_distance) { discard; }
    var color = mix(material.water_fog.rgb, uniforms.water_time_sky_tint.yzw * material.water_reflect.rgb, lighting.fresnel * material.water_reflect.a);
    if material.planar_kind == PLANAR_MIRROR { color = uniforms.water_time_sky_tint.yzw * material.water_reflect.rgb; }
    if material.planar_kind==PLANAR_TEXTURED_WATER && material.water_reflect.a>0.5 && material.water_refract.a<0.5 {
        let uv=vec2<f32>(dot(vec4<f32>(input.uv,0.0,1.0),material.normal_s),dot(vec4<f32>(input.uv,0.0,1.0),material.normal_t))+material.water_animation.xy*uniforms.water_time_sky_tint.x;
        let base=textureSample(base_texture,base_sampler,uv);
        color=base.rgb*bumped_lightmap(input.lightmap_uv,input.lightmap_bump_step,lighting.tangent_normal.xyz,false)
            +uniforms.water_time_sky_tint.yzw*material.water_reflect.rgb*lighting.fresnel*base.a;
    }
    return vec4<f32>(apply_fog(color, input.world_position,1.0/input.clip_position.w),select(1.0,view_fog_factor(input.world_position,1.0/input.clip_position.w),uniforms.fog.mode==FOG_HEIGHT || uniforms.fog.mode==FOG_IMMERSION || uniforms.fog.mode==FOG_RADIAL));
}




// Near-depth projection retains the surface footprint when the near plane clips it.
fn transform_near_water_vertex(input: VertexInput) -> VertexOutput {
    var adjusted=input;
    let eye_distance=dot(vec4<f32>(uniforms.camera_position.xyz,1.0),uniforms.medium_plane);
    // Bias only this mask to retain a footprint at the degenerate eye-plane boundary.
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
    let uv = input.clip_position.xy/uniforms.viewport_size;
    let ndc = vec2<f32>(uv.x*2.0-1.0,1.0-uv.y*2.0);
    let near_point = uniforms.camera_position.xyz+uniforms.water_depth_params.x*
        (forward+x_axis*ndc.x/dot(x_axis,x_axis)+y_axis*ndc.y/dot(y_axis,y_axis));
    if dot(vec4<f32>(near_point,1.0),uniforms.medium_plane)>=0.0 { discard; }
    let color = textureSampleLevel(refraction_texture,refraction_sampler,uv,0.0).rgb;
    return vec4<f32>(color,1.0);
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
