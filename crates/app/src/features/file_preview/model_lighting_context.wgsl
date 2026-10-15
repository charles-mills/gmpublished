struct ModelLocalLight {
    position_kind:vec4<f32>,
    color:vec4<f32>,
    direction:vec4<f32>,
    attenuation:vec4<f32>,
    spot:vec4<f32>,
};
struct ModelLightingContext {
    ambient:array<vec4<f32>,6>,
    lights:array<ModelLocalLight,4>,
    counts:vec4<u32>,
};
@group(0) @binding(7) var<storage,read> model_lighting_contexts:array<ModelLightingContext>;
struct ModelMaterial {
    flags:vec4<u32>,params:vec4<f32>,fresnel:vec4<f32>,tint:vec4<f32>,rim:vec4<f32>,
};
@group(1) @binding(16) var<uniform> model_material:ModelMaterial;
@group(1) @binding(17) var model_exponent:texture_2d_array<f32>;
@group(1) @binding(18) var model_diffuse_warp:texture_2d_array<f32>;
@group(1) @binding(19) var model_specular_warp:texture_2d_array<f32>;
@group(1) @binding(20) var model_warp_sampler:sampler;

fn model_light_direction(light:ModelLocalLight,position:vec3<f32>)->vec3<f32> {
    let delta=light.position_kind.xyz-position;
    return delta*inverseSqrt(max(dot(delta,delta),0.00000001));
}

fn model_light_attenuation(light:ModelLocalLight,position:vec3<f32>)->f32 {
    if light.position_kind.w<0.5 {return 1.0;}
    let delta=light.position_kind.xyz-position;
    let squared=dot(delta,delta);
    let distance=sqrt(max(squared,0.00000001));
    let distance_attenuation=1.0/max(dot(light.attenuation.xyz,vec3<f32>(1.0,distance,squared)),0.00000001);
    if light.position_kind.w<1.5 {return distance_attenuation;}
    let cosine=dot(light.direction.xyz,-delta/distance);
    let spot=pow(max((cosine-light.spot.z)*light.spot.w,0.0001),light.spot.x);
    return distance_attenuation*clamp(spot,0.0,1.0);
}
