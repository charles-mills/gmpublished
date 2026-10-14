struct ScreenRefraction {
    bump_s: vec4<f32>,
    bump_t: vec4<f32>,
    tint_amount: vec4<f32>,
    params: vec4<f32>,
}
@group(1) @binding(0) var screen_normal: texture_2d_array<f32>;
@group(1) @binding(1) var normal_sampler: sampler;
@group(1) @binding(2) var scene_sampler: sampler;
@group(1) @binding(3) var<uniform> overlay: ScreenRefraction;

fn scene_sample(uv: vec2<f32>) -> vec3<f32> {
    let half_texel=vec2<f32>(0.5)/vec2<f32>(textureDimensions(resolved_texture));
    return textureSampleLevel(resolved_texture,scene_sampler,clamp(uv,half_texel,vec2<f32>(1.0)-half_texel),0.0).rgb;
}

@fragment
fn fs_underwater(input: VertexOutput) -> @location(0) vec4<f32> {
    let uv=vec2<f32>(dot(vec4<f32>(input.uv,0.0,1.0),overlay.bump_s),dot(vec4<f32>(input.uv,0.0,1.0),overlay.bump_t));
    let normal=textureSample(screen_normal,normal_sampler,uv,i32(overlay.params.x));
    let warped=input.uv+(normal.xy*2.0-vec2<f32>(1.0))*normal.a*overlay.tint_amount.a;
    var color=scene_sample(warped);
    if overlay.params.y>0.5 {
        // Source Refract's four bilinear fetches reproduce its 3x3 blur.
        let step=1.0/512.0;
        color=scene_sample(warped-vec2<f32>(step*0.5))*4.0/9.0
            +scene_sample(warped+vec2<f32>(step,-step*0.5))*2.0/9.0
            +scene_sample(warped+vec2<f32>(-step*0.5,step))*2.0/9.0
            +scene_sample(warped+vec2<f32>(step))/9.0;
    }
    return vec4<f32>(mix(scene_sample(input.uv),color*overlay.tint_amount.rgb,normal.a),1.0);
}
