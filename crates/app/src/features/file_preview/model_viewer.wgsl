

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var base_texture: texture_2d<f32>;
@group(1) @binding(1) var base_texture2: texture_2d<f32>;
@group(0) @binding(5) var lightmap_texture: texture_2d<f32>;
@group(1) @binding(3) var base_sampler: sampler;
@group(1) @binding(4) var lightmap_sampler: sampler;
@group(1) @binding(5) var<uniform> material: Material;
@group(1) @binding(6) var normal_texture: texture_2d_array<f32>;
@group(1) @binding(7) var normal_texture2: texture_2d_array<f32>;

const MODEL_DIFFUSE_WARP:u32=1u;
const MODEL_SPECULAR_WARP:u32=2u;
const MODEL_EXPONENT_MAP:u32=4u;
const MODEL_HALF_LAMBERT:u32=8u;
const MODEL_BASE_ALPHA_MASK:u32=16u;
const MODEL_INVERT_MASK:u32=32u;
const MODEL_MATERIAL_RIM:u32=1u;
const MODEL_MATERIAL_PIXEL_LIGHTING:u32=2u;
const LIGHTING_LINEAR_ILLUMINATION:u32=1u;
const LIGHTING_STATIC_PROP:u32=2u;
const LIGHTING_RUNTIME_CONTEXT:u32=4u;
const LIGHTING_MODEL_DRAW:u32=8u;
const LIGHTING_LIGHTMAP:u32=16u;
const LIGHTING_PREVIEW:u32=32u;
const LIGHTING_LEGACY_VERTEX_COLOR:u32=64u;

struct SurfaceMark {
    origin:vec3<f32>, fade_min_sq:f32,
    fade_max_sq:f32, fade_enabled:u32, padding:vec2<u32>,
}
@group(0) @binding(8) var<storage,read> surface_marks:array<SurfaceMark>;

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
    @location(14) surface_mark:u32,
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
    @location(9) @interpolate(flat) vertex_state:vec4<u32>,
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
    out.normal_vertex_unit = input.normal * inverseSqrt(max(dot(input.normal,input.normal),0.00000001));
    out.tangent_vertex_unit = vec4<f32>(input.tangent_s * inverseSqrt(max(dot(input.tangent_s,input.tangent_s),0.00000001)),input.tangent_sign);
    out.light_attenuation=vec4<f32>(0.0);
    if (input.lighting_mode&LIGHTING_RUNTIME_CONTEXT)!=0u {
        let context=model_lighting_contexts[input.lighting_context];
        for(var i=0u;i<min(context.counts.x,4u);i+=1u) {
            out.light_attenuation[i]=model_light_attenuation(context.lights[i],input.position);
        }
    }
    let eye=uniforms.camera_position.xyz-input.position;
    out.eye_vertex_unit = eye * inverseSqrt(max(dot(eye,eye),0.00000001));
    out.tangent_s = input.tangent_s;
    out.tangent_t = input.tangent_t;
    if model_material.skin!=0u {
        out.tangent_t=input.tangent_t*inverseSqrt(max(dot(input.tangent_t,input.tangent_t),0.00000001));
        out.eye_vertex_unit=eye;
    }
    out.uv = input.uv;
    out.lightmap_uv = input.lightmap_uv;
    out.lightmap_bump_step=input.lightmap_bump_step;
    out.color = input.color;
    if (input.lighting_mode&LIGHTING_LINEAR_ILLUMINATION)!=0u {
        out.color=surface_modulation(true,input.color,(input.lighting_mode&(LIGHTING_STATIC_PROP|LIGHTING_MODEL_DRAW))!=0u).rgb;
    }
    switch uniforms.diagnostic.mode {
        case DIAGNOSTIC_VERTEX_NORMAL_COLOR, DIAGNOSTIC_VERTEX_NORMAL_LINEAR: {
            out.color=clamp(input.normal,vec3<f32>(0.0),vec3<f32>(1.0));
        }
        case DIAGNOSTIC_VERTEX_TANGENT_S_COLOR, DIAGNOSTIC_VERTEX_TANGENT_S_LINEAR: {
            out.color=clamp(input.tangent_s,vec3<f32>(0.0),vec3<f32>(1.0));
        }
        case DIAGNOSTIC_VERTEX_TANGENT_T_COLOR, DIAGNOSTIC_VERTEX_TANGENT_T_LINEAR: {
            out.color=clamp(input.tangent_t,vec3<f32>(0.0),vec3<f32>(1.0));
        }
        default: {}
    }
    out.blend_alpha = input.blend_alpha;
    out.opacity = input.opacity;
    var surface_mark_fade = 1.0;
    if input.surface_mark!=0u {
        let mark=surface_marks[input.surface_mark];
        if mark.fade_enabled!=0u {
            let delta=uniforms.camera_position.xyz-mark.origin;
            surface_mark_fade=clamp((mark.fade_max_sq-dot(delta,delta))/(mark.fade_max_sq-mark.fade_min_sq),0.0,1.0);
        }
    }
    out.illumination = input.illumination;
    if (input.lighting_mode&LIGHTING_RUNTIME_CONTEXT)!=0u && !model_pixel_lighting_enabled() {
        out.illumination=model_vertex_lighting(input.position,out.normal_vertex_unit,out.light_attenuation,input.lighting_context);
    }
    out.vertex_state = vec4<u32>(input.lighting_mode,input.lighting_context,bitcast<u32>(input.opacity),bitcast<u32>(surface_mark_fade));
    out.world_position = input.position;
    return out;
}

fn vertex_lighting_flags(input:VertexOutput)->u32 {return input.vertex_state.x;}
fn vertex_lighting_context(input:VertexOutput)->u32 {return input.vertex_state.y;}
fn vertex_draw_opacity(input:VertexOutput)->f32 {return bitcast<f32>(input.vertex_state.z);}
fn vertex_surface_mark_fade(input:VertexOutput)->f32 {return bitcast<f32>(input.vertex_state.w);}

fn shade(input: VertexOutput) -> vec4<f32> {
    if dot(vec4<f32>(input.world_position,1.0),uniforms.clip_plane)<0.0 {discard;}
    var diagnostic_value=vec3<f32>(0.0);
    switch uniforms.diagnostic.mode {
        case DIAGNOSTIC_RAW_NORMAL: {diagnostic_value=input.normal;}
        case DIAGNOSTIC_RAW_TANGENT_S: {diagnostic_value=input.tangent_s;}
        case DIAGNOSTIC_RAW_TANGENT_T: {diagnostic_value=input.tangent_t;}
        case DIAGNOSTIC_POSITION: {diagnostic_value=input.world_position;}
        case DIAGNOSTIC_UNIT_NORMAL: {diagnostic_value=input.normal_vertex_unit;}
        case DIAGNOSTIC_UNIT_TANGENT_S: {diagnostic_value=input.tangent_vertex_unit.xyz;}
        case DIAGNOSTIC_UNIT_EYE: {diagnostic_value=input.eye_vertex_unit;}
        case DIAGNOSTIC_VERTEX_NORMAL_COLOR, DIAGNOSTIC_VERTEX_TANGENT_S_COLOR, DIAGNOSTIC_VERTEX_TANGENT_T_COLOR: {
            let color=clamp(input.color,vec3<f32>(0.0),vec3<f32>(1.0));
            return vec4<f32>(select(pow((color+vec3<f32>(0.055))/1.055,vec3<f32>(2.4)),color/12.92,color<=vec3<f32>(0.04045)),1.0);
        }
        case DIAGNOSTIC_VERTEX_NORMAL_LINEAR, DIAGNOSTIC_VERTEX_TANGENT_S_LINEAR, DIAGNOSTIC_VERTEX_TANGENT_T_LINEAR: {
            return vec4<f32>(clamp(input.color,vec3<f32>(0.0),vec3<f32>(1.0)),1.0);
        }
        default: {}
    }
    if uniforms.diagnostic.mode!=DIAGNOSTIC_DISABLED {
        return vec4<f32>(clamp(diagnostic_value*uniforms.diagnostic.transform.w+uniforms.diagnostic.transform.xyz,vec3<f32>(0.0),vec3<f32>(1.0)),1.0);
    }
    let height_fog=uniforms.fog.mode==FOG_HEIGHT;
    let weight=surface_weight(input.uv,input.blend_alpha,height_fog);
    let uv=surface_uv(input.uv,surface.base_s,surface.base_t);
    var albedo=textureSample(base_texture,base_sampler,uv);
    if surface.flags.x==SURFACE_DECAL_MODULATE {
        if albedo.a<=0.0 || vertex_surface_mark_fade(input)<=0.0 {discard;}
        let factor=pow(view_fog_factor(input.world_position,1.0/input.clip_position.w),0.4);
        let opacity=select(factor*factor,factor,uniforms.fog.mode==FOG_HEIGHT);
        return vec4<f32>(mix(albedo.rgb,vec3<f32>(0.5),opacity),albedo.a);
    }
    if surface.alpha_state.x==0u {albedo.a=1.0;}
    if surface.flags.x==SURFACE_TRANSITION || (surface.flags.x==SURFACE_NONE && material.unlit==0u) {
        var second=textureSample(base_texture2,base_sampler,uv);
        if surface.alpha_state.y==0u {second.a=1.0;}
        albedo=mix(albedo,second,weight);
    }
    let base_alpha=albedo.a;
    let detail_color=surface_detail(input.uv);
    albedo=surface_combine(albedo,detail_color);
    let unmodulated_albedo=albedo.rgb;
    let explicit_lighting=(vertex_lighting_flags(input)&LIGHTING_LINEAR_ILLUMINATION)!=0u;
    let static_prop=(vertex_lighting_flags(input)&LIGHTING_STATIC_PROP)!=0u;
    let model_draw=(vertex_lighting_flags(input)&(LIGHTING_STATIC_PROP|LIGHTING_MODEL_DRAW))!=0u;
    // Flat alpha preserves byte cutoffs.
    let draw_opacity=clamp(select(input.opacity,vertex_draw_opacity(input),model_draw),0.0,1.0);
    let model=surface.flags.x==SURFACE_VERTEX_LIT || explicit_lighting || static_prop || model_draw;
    var modulation=surface_modulation(model,vec3<f32>(1.0),model_draw);
    if explicit_lighting {modulation=vec4<f32>(input.color,modulation.a);}
    let world=surface.flags.x==SURFACE_LIGHTMAPPED || surface.flags.x==SURFACE_TRANSITION;
    if !world {
        if surface.state.z!=0u {
            let tinted=mix(albedo.rgb*modulation.rgb,modulation.rgb,surface.misc.y);
            albedo=vec4<f32>(mix(albedo.rgb,tinted,base_alpha),albedo.a);
        } else {albedo=vec4<f32>(albedo.rgb*modulation.rgb,albedo.a);}
    }
    var opacity=albedo.a;
    if material.texture_alpha_opaque!=0u || (surface.flags.x!=SURFACE_NONE && surface.misc.w<0.5) {opacity=1.0;}
    // Lightmapped material alpha is squared.
    opacity*=select(modulation.a,modulation.a*modulation.a,world);
    if material.vertex_alpha!=0u || explicit_lighting || model_draw {opacity*=draw_opacity;}
    let alpha_test=select(material.alpha_test!=0u,surface.state.x!=0u,surface.flags.x!=SURFACE_NONE);
    if alpha_test && opacity<surface.misc.x {discard;}
    if material.alpha_test!=0u {opacity=1.0;}
    if surface.state.w!=0u && modulation.a==1.0 && (!model_draw || draw_opacity==1.0) {
        opacity=select(1.0,draw_opacity,model_draw);
    }
    opacity*=vertex_surface_mark_fade(input);
    var lit:vec3<f32>;
    if material.unlit!=0u {
        lit=albedo.rgb;
        if material.vertex_color!=0u && !explicit_lighting {lit*=input.color;}
    } else if (vertex_lighting_flags(input)&LIGHTING_RUNTIME_CONTEXT)!=0u {
        if model_pixel_lighting_enabled() {
            lit=albedo.rgb*model_diffuse_lighting(input,base_alpha);
        } else {lit=albedo.rgb*input.illumination;}
    } else if explicit_lighting {
        lit=albedo.rgb*input.illumination;
    } else if (vertex_lighting_flags(input)&LIGHTING_LIGHTMAP)!=0u {
        var lightmap=textureSample(lightmap_texture,lightmap_sampler,input.lightmap_uv).rgb*lightmap_scale();
        if material.bump_mode!=BUMP_NONE && input.lightmap_bump_step>0.0 {
            let bump_uv=surface_uv(input.uv,material.normal_s,material.normal_t);
            let layer=i32(u32(max(surface.misc.z,0.0))%max(textureNumLayers(normal_texture),1u));
            let first=textureSample(normal_texture,base_sampler,bump_uv,layer).xyz;
            let second=textureSample(normal_texture2,base_sampler,bump_uv,0).xyz;
            let encoded=mix(first,second,weight);
            let normal=select(encoded*2.0-1.0,encoded,material.bump_mode==BUMP_SSBUMP);
            lightmap=bumped_lightmap(input.lightmap_uv,input.lightmap_bump_step,normal,material.bump_mode==BUMP_SSBUMP,detail_color.rgb);
        }
        lit=albedo.rgb*lightmap;
    } else if (vertex_lighting_flags(input)&LIGHTING_PREVIEW)!=0u {
        let ambient=uniforms.light.w;
        let diffuse=max(dot(normalize(input.normal),normalize(uniforms.light.xyz)),0.0);
        lit=albedo.rgb*min(ambient+diffuse*(1.0-ambient),1.0);
    } else if (vertex_lighting_flags(input)&LIGHTING_LEGACY_VERTEX_COLOR)!=0u {
        lit=albedo.rgb*input.color*2.0;
    } else {lit=albedo.rgb;}
    if uniforms.shading_mode==SHADING_FULLBRIGHT && material.unlit==0u {
        lit=albedo.rgb;

    }
    let skin_emission_order=!world && model_material.skin!=0u;
    if world {
        lit*=modulation.rgb;
        if material.vertex_color!=0u {lit*=input.color;}
    } else if !skin_emission_order {lit=surface_detail_post(lit,detail_color);}
    let emission_alpha=select(albedo.a,base_alpha,world);
    lit=surface_emission(lit,albedo.rgb,emission_alpha,uv,input.normal_vertex_unit,input.eye_vertex_unit);
    if skin_emission_order {lit=surface_detail_post(lit,detail_color);}
    if (vertex_lighting_flags(input)&LIGHTING_RUNTIME_CONTEXT)!=0u {lit+=model_specular_lighting(input,unmodulated_albedo,emission_alpha);}
    lit+=surface_environment_lighting(input,weight,emission_alpha,world);
    lit=mix(lit,uniforms.fog_color.rgb,view_fog_color_factor(input.world_position,1.0/input.clip_position.w));
    if dot(vec4<f32>(input.world_position,1.0),uniforms.clip_plane)<0.0 {discard;}
    return vec4<f32>(lit,opacity);
}

@fragment
fn fs_main(input:VertexOutput) -> @location(0) vec4<f32> { return shade(input); }

@fragment
fn fs_refraction(input:VertexOutput) -> @location(0) vec4<f32> {
    let colour=shade(input);
    let fog=select(1.0,view_fog_factor(input.world_position,1.0/input.clip_position.w),uniforms.fog.mode==FOG_HEIGHT);
    return vec4<f32>(colour.rgb,fog);
}

@fragment
fn fs_additive(input:VertexOutput) -> @location(0) vec4<f32> {
    let colour=shade(input);
    // Additive fog attenuates toward black.
    let fog=view_fog_color_factor(input.world_position,1.0/input.clip_position.w);
    return vec4<f32>(max(colour.rgb-uniforms.fog_color.rgb*fog,vec3<f32>(0.0))*colour.a,colour.a);
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

fn surface_environment_lighting(input:VertexOutput,weight:f32,alpha:f32,world:bool)->vec3<f32> {
    if surface.env_flags.x==0u {return vec3<f32>(0.0);}
    if !world && model_material.skin!=0u {
        let sampled=model_sampled_normal(input,alpha);
        let normal=sampled.xyz;
        let eye=normalize(input.eye_vertex_unit);
        let invert=(model_material.features&MODEL_INVERT_MASK)!=0u;
        var mask=alpha;
        if surface.env_flags.y==ENV_MASK_NORMAL_ALPHA {
            mask=sampled.a;
            if surface.flags.z==EMISSION_FRESNEL {mask=select(0.0,1.0,invert);}
        }
        mask=select(mask,1.0-mask,invert);
        let fresnel=source_model_phong_fresnel(normal,eye,model_material.fresnel.xyz);
        let direction=2.0*normal*dot(normal,eye)-eye;
        return textureSample(surface_environment,surface_env_sampler,direction).rgb
            *surface.env_tint.rgb*surface.env_params.z*mask*mix(1.0,fresnel,surface.env_params.w);
    }
    var normal=input.normal;
    if surface.flags.x==SURFACE_VERTEX_LIT || (surface.flags.x==SURFACE_UNLIT && (surface.env_flags.w&ENV_VERTEX_MODULATION)==0u) {
        normal=input.normal_vertex_unit;
    }
    var normal_alpha=1.0;
    if (surface.env_flags.w&ENV_BUMP)!=0u {
        let uv=surface_uv(input.uv,material.normal_s,material.normal_t);
        let layer=i32(u32(max(surface.misc.z,0.0))%max(textureNumLayers(normal_texture),1u));
        let first=textureSample(normal_texture,base_sampler,uv,layer);
        var encoded=first.xyz;
        normal_alpha=first.a;
        if (surface.env_flags.w&ENV_SECOND_NORMAL)!=0u {
            let second=textureSample(normal_texture2,base_sampler,uv,0);
            encoded=mix(encoded,second.xyz,weight);
            if surface.flags.w!=0u {normal_alpha=mix(first.a,second.a,weight);}
        }
        var tangent_normal=encoded*2.0-1.0;
        if (surface.env_flags.w&ENV_SSBUMP)!=0u {
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
    if surface.env_flags.y==ENV_MASK_BASE_ALPHA {mask=vec3<f32>(1.0-alpha);}
    if surface.env_flags.y==ENV_MASK_NORMAL_ALPHA {mask=vec3<f32>(normal_alpha);}
    if surface.env_flags.y==ENV_MASK_TEXTURE {
        let layer=i32(surface.env_flags.z%max(textureNumLayers(surface_envmask),1u));
        mask=textureSample(surface_envmask,base_sampler,surface_uv(input.uv,surface.env_mask_s,surface.env_mask_t),layer).rgb;
    }
    var eye=uniforms.camera_position.xyz-input.world_position;
    if !world && (surface.env_flags.w&ENV_BUMP)!=0u {eye=input.eye_vertex_unit;}
    let direction=source_cube_reflection(normal,eye);
    let radiance=textureSample(surface_environment,surface_env_sampler,direction).rgb*surface.env_params.z;
    var result=source_cube_specular(radiance,mask,surface.env_tint.rgb,surface.env_params.x,surface.env_params.y);
    if world {result*=source_world_cube_fresnel(normal,eye,surface.env_params.w);}
    return result;
}

fn model_sampled_normal(input:VertexOutput,base_alpha:f32)->vec4<f32> {
    let uv=surface_uv(input.uv,surface.base_s,surface.base_t);
    var texel=vec4<f32>(0.5,0.5,1.0,1.0);
    if model_material.normal!=0u {texel=textureSample(normal_texture,base_sampler,uv,0);}
    let base_mask=model_material.skin!=0u && (model_material.features&MODEL_BASE_ALPHA_MASK)!=0u;
    let tangent_normal=source_model_phong_tangent_normal(texel.xyz,base_mask);
    var tangent_t=cross(input.normal_vertex_unit,input.tangent_vertex_unit.xyz)*input.tangent_vertex_unit.w;
    if model_material.skin!=0u {tangent_t=input.tangent_t;}
    let normal=normalize(input.tangent_vertex_unit.xyz*tangent_normal.x+tangent_t*tangent_normal.y+input.normal_vertex_unit*tangent_normal.z);
    return vec4<f32>(normal,source_model_phong_mask(texel.a,base_alpha,base_mask));
}

fn model_ambient_cube(context:ModelLightingContext)->array<vec3<f32>,6> {
    var cube:array<vec3<f32>,6>;
    for(var i=0u;i<6u;i+=1u) {cube[i]=context.ambient[i].rgb;}
    return cube;
}

fn model_pixel_lighting_enabled()->bool {
    return (model_material.lighting&MODEL_MATERIAL_PIXEL_LIGHTING)!=0u;
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
        let response=select(max(cosine,0.0),half_term*half_term,(model_material.features&MODEL_HALF_LAMBERT)!=0u);
        result+=light.color.rgb*response*attenuation[i];
    }
    return result;
}

fn model_diffuse_lighting(input:VertexOutput,base_alpha:f32)->vec3<f32> {
    let context=model_lighting_contexts[vertex_lighting_context(input)];
    let normal=model_sampled_normal(input,base_alpha).xyz;
    var result=source_model_ambient(normal,model_ambient_cube(context));
    let warp=(model_material.features&MODEL_DIFFUSE_WARP)!=0u;
    for(var i=0u;i<min(context.counts.x,4u);i+=1u) {
        let light=context.lights[i];
        let direction=model_light_direction(light,input.world_position);
        let coordinate=source_model_diffuse_coordinate(normal,direction,(model_material.features&MODEL_HALF_LAMBERT)!=0u,warp);
        var texel=vec3<f32>(1.0);
        if warp {texel=textureSample(model_diffuse_warp,model_warp_sampler,source_model_diffuse_warp_uv(coordinate,1.0),0).rgb;}
        result+=source_model_diffuse_term(coordinate,1.0,warp,texel)*light.color.rgb*input.light_attenuation[i];
    }
    return result;
}

fn model_specular_lighting(input:VertexOutput,base_linear:vec3<f32>,base_alpha:f32)->vec3<f32> {
    if model_material.skin==0u {return vec3<f32>(0.0);}
    let context=model_lighting_contexts[vertex_lighting_context(input)];
    let sampled=model_sampled_normal(input,base_alpha);
    let normal=sampled.xyz;
    let eye=normalize(input.eye_vertex_unit);
    let fresnel=source_model_phong_fresnel(normal,eye,model_material.fresnel.xyz);
    let uv=surface_uv(input.uv,surface.base_s,surface.base_t);
    let exponent_texel=textureSample(model_exponent,base_sampler,uv,0);
    let exponent=source_model_phong_exponent(model_material.exponent,exponent_texel.r);
    let warp=(model_material.features&MODEL_SPECULAR_WARP)!=0u;
    var specular=vec3<f32>(0.0);
    var rim=vec3<f32>(0.0);
    for(var i=0u;i<min(context.counts.x,4u);i+=1u) {
        let light=context.lights[i];
        let direction=model_light_direction(light,input.world_position);
        let lobe=source_model_phong_lobe(normal,direction,eye,exponent);
        var texel=vec3<f32>(1.0);
        if warp {texel=textureSample(model_specular_warp,model_warp_sampler,source_model_phong_warp_uv(lobe,fresnel),0).rgb;}
        specular+=source_model_phong_light(normal,direction,lobe,light.color.rgb,input.light_attenuation[i],1.0,warp,texel);
        if (model_material.lighting&MODEL_MATERIAL_RIM)!=0u {rim+=source_model_rim_light(normal,direction,eye,model_material.rim.x,light.color.rgb,input.light_attenuation[i]);}
    }
    specular=source_model_phong_finish(specular,sampled.a,model_material.boost,fresnel,warp);
    if (model_material.lighting&MODEL_MATERIAL_RIM)!=0u {
        let mask=select(1.0,exponent_texel.a,model_material.rim.z>0.5);
        specular=source_model_rim_finish(specular,rim,source_model_ambient(eye,model_ambient_cube(context)),normal,eye,mask,model_material.rim.y);
    }
    return specular*source_model_phong_tint(model_material.tint.xyz,base_linear,exponent_texel.g);
}
