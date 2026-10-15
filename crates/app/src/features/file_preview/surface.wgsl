const SURFACE_NONE:u32=0u;
const SURFACE_LIGHTMAPPED:u32=1u;
const SURFACE_TRANSITION:u32=2u;
const SURFACE_VERTEX_LIT:u32=3u;
const SURFACE_UNLIT:u32=4u;
const SURFACE_DECAL_MODULATE:u32=5u;
const EMISSION_NONE:u32=0u;
const EMISSION_BASE_ALPHA:u32=1u;
const EMISSION_TEXTURE:u32=2u;
const EMISSION_FRESNEL:u32=3u;
const EMISSION_ENVIRONMENT_MASK:u32=4u;
const ENV_MASK_NONE:u32=0u;
const ENV_MASK_BASE_ALPHA:u32=1u;
const ENV_MASK_NORMAL_ALPHA:u32=2u;
const ENV_MASK_TEXTURE:u32=3u;
const ENV_BUMP:u32=1u;
const ENV_SSBUMP:u32=2u;
const ENV_SECOND_NORMAL:u32=4u;
const ENV_VERTEX_MODULATION:u32=8u;

struct Surface {
    flags:vec4<u32>,
    modulation:vec4<f32>,
    model_modulation:vec4<f32>,
    base_s:vec4<f32>,
    base_t:vec4<f32>,
    detail_s:vec4<f32>,
    detail_t:vec4<f32>,
    detail:vec4<f32>,
    detail_tint:vec4<f32>,
    emission:vec4<f32>,
    emission_fresnel:vec4<f32>,
    blend_s:vec4<f32>,
    blend_t:vec4<f32>,
    state:vec4<u32>,
    misc:vec4<f32>,
    env_flags:vec4<u32>,
    env_tint:vec4<f32>,
    env_params:vec4<f32>,
    env_mask_s:vec4<f32>,
    env_mask_t:vec4<f32>,
    alpha_state:vec4<u32>,
}
@group(1) @binding(8) var<uniform> surface:Surface;
@group(1) @binding(9) var surface_detail_texture:texture_2d_array<f32>;
@group(1) @binding(10) var surface_blend_texture:texture_2d_array<f32>;
@group(1) @binding(11) var surface_emission_texture:texture_2d_array<f32>;
@group(1) @binding(12) var surface_environment:texture_cube<f32>;
@group(1) @binding(13) var surface_envmask:texture_2d_array<f32>;
@group(1) @binding(14) var surface_env_sampler:sampler;

fn surface_uv(uv:vec2<f32>,s:vec4<f32>,t:vec4<f32>)->vec2<f32> {
    let value=vec4<f32>(uv,0.0,1.0);
    return vec2<f32>(dot(value,s),dot(value,t));
}

fn surface_gamma(color:vec3<f32>)->vec3<f32> {
    let scaled=clamp(color,vec3<f32>(0.0),vec3<f32>(1.0))*255.0;
    let lower=floor(scaled);
    let fraction=scaled-lower;
    let odd=(vec3<u32>(lower)&vec3<u32>(1u))!=vec3<u32>(0u);
    let up=select(fraction>vec3<f32>(0.5),odd,fraction==vec3<f32>(0.5));
    let quantized=(lower+select(vec3<f32>(0.0),vec3<f32>(1.0),up))/255.0;
    let bounded=select(pow(quantized,vec3<f32>(2.2)),vec3<f32>(1.0),color>=vec3<f32>(0.95));
    return select(bounded,color,color>vec3<f32>(1.0));
}


fn surface_modulation(model:bool,instance:vec3<f32>,model_draw:bool)->vec4<f32> {
    var value=select(surface.modulation,surface.model_modulation,model);
    value=vec4<f32>(value.rgb*instance,value.a);
    if model_draw {value.a=1.0;}
    if model || surface.flags.x==SURFACE_UNLIT {
        value=vec4<f32>(surface_gamma(value.rgb),value.a);
    }
    return value;
}

fn surface_weight(uv:vec2<f32>,blend:f32,height_fog:bool)->f32 {
    let weight=clamp(blend,0.0,1.0);
    if surface.flags.w==0u || height_fog || surface.flags.z!=EMISSION_NONE {return weight;}
    let mask=textureSample(surface_blend_texture,base_sampler,surface_uv(uv,surface.blend_s,surface.blend_t),0);
    if surface.state.y!=0u {return mask.g;}
    let low=clamp(mask.g-mask.r,0.0,1.0);
    let high=clamp(mask.g+mask.r,0.0,1.0);
    let t=clamp((weight-low)/max(high-low,0.000001),0.0,1.0);
    return t*t*(3.0-2.0*t);
}

fn surface_detail(uv:vec2<f32>)->vec4<f32> {
    if surface.flags.y==0u {return vec4<f32>(1.0);}
    let layer=i32(u32(max(surface.detail.y,0.0))%max(textureNumLayers(surface_detail_texture),1u));
    let sample=textureSample(surface_detail_texture,base_sampler,surface_uv(uv,surface.detail_s,surface.detail_t),layer);
    return vec4<f32>(sample.rgb*surface.detail_tint.rgb,sample.a);
}

fn surface_combine(base:vec4<f32>,detail:vec4<f32>)->vec4<f32> {
    if surface.flags.y==0u {return base;}
    let mode=surface.flags.y-1u;
    let f=surface.detail.x;
    var value=base;
    switch mode {
        case 0u:{value=vec4<f32>(base.rgb*mix(vec3<f32>(1.0),2.0*detail.rgb,f),base.a);}
        case 1u:{value=vec4<f32>(base.rgb+f*detail.rgb,base.a);}
        case 2u:{value=vec4<f32>(mix(base.rgb,detail.rgb,f*detail.a),base.a);}
        case 3u:{value=mix(base,detail,f);}
        case 4u:{value=vec4<f32>(mix(base.rgb,detail.rgb,f*(1.0-base.a)),detail.a);}
        case 7u:{let pattern=mix(detail.r,detail.a,base.a);value=vec4<f32>(base.rgb*mix(vec3<f32>(1.0),vec3<f32>(2.0*pattern),f),base.a);}
        case 8u:{value=mix(base,base*detail,f);}
        case 9u:{value=vec4<f32>(base.rgb,mix(base.a,base.a*detail.a,f));}
        case 11u:{value=vec4<f32>(base.rgb*dot(detail.rgb,vec3<f32>(2.0/3.0)),base.a);}
        default:{}
    }
    return value;
}

fn surface_detail_post(lit:vec3<f32>,detail:vec4<f32>)->vec3<f32> {
    if surface.flags.y==6u {return lit+surface.detail.x*detail.rgb;}
    if surface.flags.y==7u {
        let f=surface.detail.x;
        var multiplier=4.0*f;
        var addition=-0.5*multiplier;
        if f>=0.5 {multiplier=1.0/f;addition=1.0-multiplier;}
        return lit+clamp(multiplier*detail.rgb+vec3<f32>(addition),vec3<f32>(0.0),vec3<f32>(1.0));
    }
    return lit;
}

fn surface_emission(lit:vec3<f32>,albedo:vec3<f32>,alpha:f32,uv:vec2<f32>,vertex_normal:vec3<f32>,eye:vec3<f32>)->vec3<f32> {
    if surface.flags.z==EMISSION_NONE {return lit;}
    var mask=vec3<f32>(alpha);
    if surface.flags.z==EMISSION_TEXTURE {mask=textureSample(surface_emission_texture,base_sampler,uv,0).rgb;}
    if surface.flags.z==EMISSION_FRESNEL {
        let fresnel=clamp(pow(clamp(dot(normalize(vertex_normal),normalize(eye)),0.0,1.0),surface.emission_fresnel.z)
            *surface.emission_fresnel.x+surface.emission_fresnel.y,0.0,1.0);
        return max(mix(lit,albedo*surface.emission.rgb*surface.emission_fresnel.w,vec3<f32>(alpha*fresnel)),vec3<f32>(0.0));
    }
    if surface.flags.z==EMISSION_ENVIRONMENT_MASK {
        let layer=i32(surface.env_flags.z%max(textureNumLayers(surface_envmask),1u));
        let glow=8.0*textureSample(surface_envmask,base_sampler,surface_uv(uv,surface.env_mask_s,surface.env_mask_t),layer).a;
        return max(0.0,1.0-glow)*lit+glow*albedo*surface.emission.rgb;
    }
    return mix(lit,albedo*surface.emission.rgb,mask);
}
