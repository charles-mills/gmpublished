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
    raster_params: vec4<f32>,
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
    @location(8) illumination:vec3<f32>,
    @location(9) lighting_mode:u32,
    @location(13) lighting_context:u32,
    @location(10) tangent_s:vec3<f32>,
    @location(11) tangent_t:vec3<f32>,
    @location(12) tangent_sign:f32,
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
    @location(8) illumination:vec3<f32>,
    @location(9) @interpolate(flat) lighting_state:vec3<u32>,
    @location(10) tangent_s:vec3<f32>,
    @location(11) tangent_t:vec3<f32>,
    @location(12) normal_vertex_unit:vec3<f32>,
    @location(13) tangent_vertex_unit:vec4<f32>,
    @location(14) light_attenuation:vec4<f32>,
    @location(15) eye_vertex_unit:vec3<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = uniforms.view_proj * vec4<f32>(input.position, 1.0);
    out.normal = input.normal;
    // The ordinary VertexLit/Unlit VS normalizes before interpolation.
    // Bumped tangent frames and capture diagnostics retain the raw normal.
    out.normal_vertex_unit = input.normal * inverseSqrt(max(dot(input.normal,input.normal),0.00000001));
    out.tangent_vertex_unit = vec4<f32>(input.tangent_s * inverseSqrt(max(dot(input.tangent_s,input.tangent_s),0.00000001)),input.tangent_sign);
    out.light_attenuation=vec4<f32>(0.0);
    if (input.lighting_mode&4u)!=0u {
        let context=model_lighting_contexts[input.lighting_context];
        for(var i=0u;i<min(context.counts.x,4u);i+=1u) {
            out.light_attenuation[i]=model_light_attenuation(context.lights[i],input.position);
        }
    }
    let eye=uniforms.camera_position.xyz-input.position;
    out.eye_vertex_unit = eye * inverseSqrt(max(dot(eye,eye),0.00000001));
    out.tangent_s = input.tangent_s;
    out.tangent_t = input.tangent_t;
    if model_material.flags.x!=0u {
        out.tangent_t=input.tangent_t*inverseSqrt(max(dot(input.tangent_t,input.tangent_t),0.00000001));
        out.eye_vertex_unit=eye;
    }
    out.uv = input.uv;
    out.lightmap_uv = input.lightmap_uv;
    out.lightmap_bump_step=input.lightmap_bump_step;
    out.color = input.color;
    if (input.lighting_mode&1u)!=0u {
        out.color=surface_modulation(true,input.color,(input.lighting_mode&10u)!=0u).rgb;
    }
    if uniforms.viewport.w>0.5 && uniforms.viewport.w<6.5 {
        let mode=u32(uniforms.viewport.w-1.0)%3u;
        var value=input.normal;
        if mode==1u {value=input.tangent_s;}
        if mode==2u {value=input.tangent_t;}
        // Native DebugTangentSpace exports COLOR0; ToGL's gl_FrontColor clamps
        // before interpolation. Keep this diagnostic separate from raw TBN.
        out.color=clamp(value,vec3<f32>(0.0),vec3<f32>(1.0));
    }
    out.blend_alpha = input.blend_alpha;
    out.opacity = input.opacity;
    out.illumination = input.illumination;
    if (input.lighting_mode&4u)!=0u && !model_pixel_lighting_enabled() {
        out.illumination=model_vertex_lighting(input.position,out.normal_vertex_unit,out.light_attenuation,input.lighting_context);
    }
    out.lighting_state = vec3<u32>(input.lighting_mode,input.lighting_context,bitcast<u32>(input.opacity));
    out.world_position = input.position;
    return out;
}

fn shade(input: VertexOutput) -> vec4<f32> {
    if dot(vec4<f32>(input.world_position,1.0),uniforms.clip_plane)<0.0 {discard;}
    if uniforms.viewport.w>6.5 {
        var frame=input.normal;
        if uniforms.viewport.w>7.5 {frame=input.tangent_s;}
        if uniforms.viewport.w>8.5 {frame=input.tangent_t;}
        if uniforms.viewport.w>9.5 {frame=input.world_position;}
        if uniforms.viewport.w>10.5 {frame=input.normal_vertex_unit;}
        if uniforms.viewport.w>11.5 {frame=input.tangent_vertex_unit.xyz;}
        if uniforms.viewport.w>12.5 {frame=input.eye_vertex_unit;}
        return vec4<f32>(clamp(frame*uniforms.light.w+uniforms.light.xyz,vec3<f32>(0.0),vec3<f32>(1.0)),1.0);
    }
    if uniforms.viewport.w>0.5 {
        return vec4<f32>(surface_diagnostic_color(input.color,uniforms.viewport.w),1.0);
    }
    let height_fog=uniforms.fog_params.w>1.5 && uniforms.fog_params.w<2.5;
    let weight=surface_weight(input.uv,input.blend_alpha,height_fog);
    let uv=surface_uv(input.uv,surface.base_s,surface.base_t);
    var albedo=textureSample(base_texture,base_sampler,uv);
    if surface.alpha_state.x==0u {albedo.a=1.0;}
    if surface.flags.x==2u || (surface.flags.x==0u && material.surface_flags.x==0u) {
        var second=textureSample(base_texture2,base_sampler,uv);
        if surface.alpha_state.y==0u {second.a=1.0;}
        albedo=mix(albedo,second,weight);
    }
    let base_alpha=albedo.a;
    let detail_color=surface_detail(input.uv);
    albedo=surface_combine(albedo,detail_color);
    let unmodulated_albedo=albedo.rgb;
    let explicit_lighting=(input.lighting_state.x&1u)!=0u;
    let static_prop=(input.lighting_state.x&2u)!=0u;
    let model_draw=(input.lighting_state.x&10u)!=0u;
    // Source model alpha is a draw constant. Perspective interpolation of a
    // constant can undershoot one and incorrectly reject exact cutoff values.
    let draw_opacity=clamp(select(input.opacity,bitcast<f32>(input.lighting_state.z),model_draw),0.0,1.0);
    let model=surface.flags.x==3u || explicit_lighting || static_prop || model_draw;
    var modulation=surface_modulation(model,vec3<f32>(1.0),model_draw);
    if explicit_lighting {modulation=vec4<f32>(input.color,modulation.a);}
    let world=surface.flags.x==1u || surface.flags.x==2u;
    if !world {
        if surface.state.z!=0u {
            let tinted=mix(albedo.rgb*modulation.rgb,modulation.rgb,surface.misc.y);
            albedo=vec4<f32>(mix(albedo.rgb,tinted,base_alpha),albedo.a);
        } else {albedo=vec4<f32>(albedo.rgb*modulation.rgb,albedo.a);}
    }
    var opacity=albedo.a;
    if material.flags.x!=0u || (surface.flags.x!=0u && surface.misc.w<0.5) {opacity=1.0;}
    // The lightmapped shader carries material alpha in both vertex
    // modulation and its pixel alpha constant.
    opacity*=select(modulation.a,modulation.a*modulation.a,world);
    if material.surface_flags.z!=0u || explicit_lighting || model_draw {opacity*=draw_opacity;}
    let alpha_test=select(material.flags.y!=0u,surface.state.x!=0u,surface.flags.x!=0u);
    if alpha_test && opacity<surface.misc.x {discard;}
    if material.flags.y!=0u {opacity=1.0;}
    if surface.state.w!=0u && modulation.a==1.0 && (!model_draw || draw_opacity==1.0) {
        opacity=select(1.0,draw_opacity,model_draw);
    }
    var lit:vec3<f32>;
    if material.surface_flags.x!=0u {
        lit=albedo.rgb;
        if material.surface_flags.y!=0u && !explicit_lighting {lit*=input.color;}
    } else if (input.lighting_state.x&4u)!=0u {
        if model_pixel_lighting_enabled() {
            lit=albedo.rgb*model_diffuse_lighting(input,base_alpha);
        } else {lit=albedo.rgb*input.illumination;}
    } else if explicit_lighting {
        lit=albedo.rgb*input.illumination;
    } else if input.lightmap_uv.x!=0.0 || input.lightmap_uv.y!=0.0 {
        var lightmap=textureSample(lightmap_texture,lightmap_sampler,input.lightmap_uv).rgb*lightmap_scale();
        if material.flags.z!=0u && input.lightmap_bump_step>0.0 {
            let bump_uv=surface_uv(input.uv,material.water_bump_s,material.water_bump_t);
            let layer=i32(u32(max(surface.misc.z,0.0))%max(textureNumLayers(normal_texture),1u));
            let first=textureSample(normal_texture,base_sampler,bump_uv,layer).xyz;
            let second=textureSample(normal_texture2,base_sampler,bump_uv,0).xyz;
            let encoded=mix(first,second,weight);
            let normal=select(encoded*2.0-1.0,encoded,material.flags.z==2u);
            lightmap=bumped_lightmap(input.lightmap_uv,input.lightmap_bump_step,normal,material.flags.z==2u,detail_color.rgb);
        }
        lit=albedo.rgb*lightmap;
    // Legacy vertex lighting uses quantized colour as its fallback marker.
    } else if all(abs(input.color-vec3<f32>(1.0))<vec3<f32>(0.5/255.0)) {
        let ambient=uniforms.light.w;
        let diffuse=max(dot(normalize(input.normal),normalize(uniforms.light.xyz)),0.0);
        lit=albedo.rgb*min(ambient+diffuse*(1.0-ambient),1.0);
    } else {lit=albedo.rgb*input.color*2.0;}
    if uniforms.light.w<0.0 && material.surface_flags.x==0u {
        lit=albedo.rgb;

    }
    let skin_emission_order=!world && model_material.flags.x!=0u;
    if world {lit*=modulation.rgb;} else if !skin_emission_order {lit=surface_detail_post(lit,detail_color);}
    let emission_alpha=select(albedo.a,base_alpha,world);
    lit=surface_emission(lit,albedo.rgb,emission_alpha,uv,input.normal_vertex_unit,input.eye_vertex_unit);
    if skin_emission_order {lit=surface_detail_post(lit,detail_color);}
    if (input.lighting_state.x&4u)!=0u {lit+=model_specular_lighting(input,unmodulated_albedo,emission_alpha);}
    lit+=surface_environment_lighting(input,weight,emission_alpha,world);
    lit=mix(lit,uniforms.fog_color.rgb,view_fog_color_factor(input.world_position,1.0/input.clip_position.w));
    if dot(vec4<f32>(input.world_position,1.0),uniforms.clip_plane)<0.0 {discard;}
    return vec4<f32>(lit,opacity);
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
    return vec4<f32>(max(colour.rgb-uniforms.fog_color.rgb*fog,vec3<f32>(0.0))*colour.a,colour.a);
}

fn view_fog_color_factor(position:vec3<f32>,projected_depth:f32) -> f32 {
    let factor = view_fog_factor(position,projected_depth);
    // Source's ps2b+ range-fog blend squares opacity to match hardware fog.
    // Height-fog depth metadata and the water shader's own absorption stay linear.
    let opacity = select(factor*factor, factor, uniforms.fog_params.w>1.5 && uniforms.fog_params.w<2.5);
    return opacity*select(1.0,uniforms.fog_color.a,uniforms.fog_params.w>1.5 && uniforms.fog_params.w<3.5);
}

fn bumped_lightmap(uv:vec2<f32>, step:f32, tangent_normal:vec3<f32>, ssbump:bool, detail_color:vec3<f32>) -> vec3<f32> {
    if step <= 0.0 { return textureSample(lightmap_texture, lightmap_sampler, uv).rgb * lightmap_scale(); }
    var weights = tangent_normal;
    if !ssbump {
        weights = clamp(vec3<f32>(dot(tangent_normal,vec3<f32>(0.81649658,0.0,0.57735027)),
            dot(tangent_normal,vec3<f32>(-0.40824829,0.70710678,0.57735027)),
            dot(tangent_normal,vec3<f32>(-0.40824829,-0.70710678,0.57735027))),vec3<f32>(0.0),vec3<f32>(1.0));
        weights *= weights;
        if surface.flags.y==11u {weights*=2.0*detail_color;}
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

fn surface_environment_lighting(input:VertexOutput,weight:f32,alpha:f32,world:bool)->vec3<f32> {
    if surface.env_flags.x==0u {return vec3<f32>(0.0);}
    if !world && model_material.flags.x!=0u {
        let sampled=model_sampled_normal(input,alpha);
        let normal=sampled.xyz;
        let eye=normalize(input.eye_vertex_unit);
        let invert=(model_material.flags.z&32u)!=0u;
        var mask=alpha;
        if surface.env_flags.y==2u {
            mask=sampled.a;
            if surface.flags.z==3u {mask=select(0.0,1.0,invert);}
        }
        mask=select(mask,1.0-mask,invert);
        let fresnel=source_model_phong_fresnel(normal,eye,model_material.fresnel.xyz);
        let direction=2.0*normal*dot(normal,eye)-eye;
        return textureSample(surface_environment,surface_env_sampler,direction).rgb
            *surface.env_tint.rgb*surface.env_params.z*mask*mix(1.0,fresnel,surface.env_params.w);
    }
    var normal=input.normal;
    if surface.flags.x==3u || (surface.flags.x==4u && (surface.env_flags.w&8u)==0u) {
        normal=input.normal_vertex_unit;
    }
    var normal_alpha=1.0;
    if (surface.env_flags.w&1u)!=0u {
        let uv=surface_uv(input.uv,material.water_bump_s,material.water_bump_t);
        let layer=i32(u32(max(surface.misc.z,0.0))%max(textureNumLayers(normal_texture),1u));
        let first=textureSample(normal_texture,base_sampler,uv,layer);
        var encoded=first.xyz;
        normal_alpha=first.a;
        if (surface.env_flags.w&4u)!=0u {
            let second=textureSample(normal_texture2,base_sampler,uv,0);
            encoded=mix(encoded,second.xyz,weight);
            if surface.flags.w!=0u {normal_alpha=mix(first.a,second.a,weight);}
        }
        var tangent_normal=encoded*2.0-1.0;
        if (surface.env_flags.w&2u)!=0u {
            tangent_normal=normalize(encoded.x*vec3<f32>(0.81649658,0.0,0.57735027)
                +encoded.y*vec3<f32>(-0.40824829,0.70710678,0.57735027)
                +encoded.z*vec3<f32>(-0.40824829,-0.70710678,0.57735027));
        }
        if world {
            normal=input.tangent_s*tangent_normal.x+input.tangent_t*tangent_normal.y+input.normal*tangent_normal.z;
        } else {
            let binormal=cross(input.normal_vertex_unit,input.tangent_vertex_unit.xyz)*input.tangent_vertex_unit.w;
            normal=normalize(input.tangent_vertex_unit.xyz*tangent_normal.x+binormal*tangent_normal.y+input.normal_vertex_unit*tangent_normal.z);
        }
    }
    var mask=vec3<f32>(1.0);
    if surface.env_flags.y==1u {mask=vec3<f32>(1.0-alpha);}
    if surface.env_flags.y==2u {mask=vec3<f32>(normal_alpha);}
    if surface.env_flags.y==3u {
        let layer=i32(surface.env_flags.z%max(textureNumLayers(surface_envmask),1u));
        mask=textureSample(surface_envmask,base_sampler,surface_uv(input.uv,surface.env_mask_s,surface.env_mask_t),layer).rgb;
    }
    var eye=uniforms.camera_position.xyz-input.world_position;
    if !world && (surface.env_flags.w&1u)!=0u {eye=input.eye_vertex_unit;}
    let direction=source_cube_reflection(normal,eye);
    let radiance=textureSample(surface_environment,surface_env_sampler,direction).rgb*surface.env_params.z;
    var result=source_cube_specular(radiance,mask,surface.env_tint.rgb,surface.env_params.x,surface.env_params.y);
    if world {result*=source_world_cube_fresnel(normal,eye,surface.env_params.w);}
    return result;
}

fn model_sampled_normal(input:VertexOutput,base_alpha:f32)->vec4<f32> {
    let uv=surface_uv(input.uv,surface.base_s,surface.base_t);
    var texel=vec4<f32>(0.5,0.5,1.0,1.0);
    if model_material.flags.y!=0u {texel=textureSample(normal_texture,base_sampler,uv,0);}
    let base_mask=model_material.flags.x!=0u && (model_material.flags.z&16u)!=0u;
    let tangent_normal=source_model_phong_tangent_normal(texel.xyz,base_mask);
    var tangent_t=cross(input.normal_vertex_unit,input.tangent_vertex_unit.xyz)*input.tangent_vertex_unit.w;
    if model_material.flags.x!=0u {tangent_t=input.tangent_t;}
    let normal=normalize(input.tangent_vertex_unit.xyz*tangent_normal.x+tangent_t*tangent_normal.y+input.normal_vertex_unit*tangent_normal.z);
    return vec4<f32>(normal,source_model_phong_mask(texel.a,base_alpha,base_mask));
}

fn model_ambient_cube(context:ModelLightingContext)->array<vec3<f32>,6> {
    var cube:array<vec3<f32>,6>;
    for(var i=0u;i<6u;i+=1u) {cube[i]=context.ambient[i].rgb;}
    return cube;
}

fn model_pixel_lighting_enabled()->bool {
    return model_material.flags.x!=0u || model_material.flags.y!=0u || (model_material.flags.z&1u)!=0u;
}

fn model_vertex_lighting(position:vec3<f32>,normal:vec3<f32>,attenuation:vec4<f32>,context_index:u32)->vec3<f32> {
    let context=model_lighting_contexts[context_index];
    var result=source_model_ambient(normal,model_ambient_cube(context));
    for(var i=0u;i<min(context.counts.x,4u);i+=1u) {
        let light=context.lights[i];
        var direction=model_light_direction(light,position);
        if light.position_kind.w<0.5 {direction=-light.direction.xyz;}
        let cosine=dot(normal,direction);
        let half_term=cosine*0.5+0.5;
        let response=select(max(cosine,0.0),half_term*half_term,(model_material.flags.z&8u)!=0u);
        result+=light.color.rgb*response*attenuation[i];
    }
    return result;
}

fn model_diffuse_lighting(input:VertexOutput,base_alpha:f32)->vec3<f32> {
    let context=model_lighting_contexts[input.lighting_state.y];
    let normal=model_sampled_normal(input,base_alpha).xyz;
    var result=source_model_ambient(normal,model_ambient_cube(context));
    let warp=(model_material.flags.z&1u)!=0u;
    for(var i=0u;i<min(context.counts.x,4u);i+=1u) {
        let light=context.lights[i];
        let direction=model_light_direction(light,input.world_position);
        let coordinate=source_model_diffuse_coordinate(normal,direction,(model_material.flags.z&8u)!=0u,warp);
        var texel=vec3<f32>(1.0);
        if warp {texel=textureSample(model_diffuse_warp,model_warp_sampler,source_model_diffuse_warp_uv(coordinate,1.0),0).rgb;}
        result+=source_model_diffuse_term(coordinate,1.0,warp,texel)*light.color.rgb*input.light_attenuation[i];
    }
    return result;
}

fn model_specular_lighting(input:VertexOutput,base_linear:vec3<f32>,base_alpha:f32)->vec3<f32> {
    if model_material.flags.x==0u {return vec3<f32>(0.0);}
    let context=model_lighting_contexts[input.lighting_state.y];
    let sampled=model_sampled_normal(input,base_alpha);
    let normal=sampled.xyz;
    let eye=normalize(input.eye_vertex_unit);
    let fresnel=source_model_phong_fresnel(normal,eye,model_material.fresnel.xyz);
    let uv=surface_uv(input.uv,surface.base_s,surface.base_t);
    let exponent_texel=textureSample(model_exponent,base_sampler,uv,0);
    let exponent=source_model_phong_exponent(model_material.params.x,exponent_texel.r);
    let warp=(model_material.flags.z&2u)!=0u;
    var specular=vec3<f32>(0.0);
    var rim=vec3<f32>(0.0);
    for(var i=0u;i<min(context.counts.x,4u);i+=1u) {
        let light=context.lights[i];
        let direction=model_light_direction(light,input.world_position);
        let lobe=source_model_phong_lobe(normal,direction,eye,exponent);
        var texel=vec3<f32>(1.0);
        if warp {texel=textureSample(model_specular_warp,model_warp_sampler,source_model_phong_warp_uv(lobe,fresnel),0).rgb;}
        specular+=source_model_phong_light(normal,direction,lobe,light.color.rgb,input.light_attenuation[i],1.0,warp,texel);
        if model_material.flags.w!=0u {rim+=source_model_rim_light(normal,direction,eye,model_material.rim.x,light.color.rgb,input.light_attenuation[i]);}
    }
    specular=source_model_phong_finish(specular,sampled.a,model_material.params.z,fresnel,warp);
    if model_material.flags.w!=0u {
        let mask=select(1.0,exponent_texel.a,model_material.rim.z>0.5);
        specular=source_model_rim_finish(specular,rim,source_model_ambient(eye,model_ambient_cube(context)),normal,eye,mask,model_material.rim.y);
    }
    return specular*source_model_phong_tint(model_material.tint.xyz,base_linear,exponent_texel.g);
}
