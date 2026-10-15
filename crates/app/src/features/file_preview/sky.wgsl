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
    water_depth_params: vec4<f32>,
    clip_plane: vec4<f32>,
    medium_plane: vec4<f32>,
    viewport: vec4<f32>,
    raster_params: vec4<f32>,
    // xy: near and far clip distances.
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var sky_texture: texture_2d<f32>;
@group(1) @binding(1) var sky_sampler: sampler;
@group(1) @binding(2) var<uniform> sky: SkyMaterial;
@group(1) @binding(3) var stars_sampler: sampler;

struct SkyMaterial {
    kind: vec4<u32>,
    top: vec4<f32>,
    bottom: vec4<f32>,
    dusk: vec4<f32>,
    sun_direction: vec4<f32>,
    sun_color: vec4<f32>,
    stars: vec4<f32>,
    tint: vec4<f32>,
    texture_s: vec4<f32>,
    texture_t: vec4<f32>,
}

struct VertexInput {
    @location(0) position: vec3<f32>,
    @builtin(vertex_index) vertex_index: u32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) direction: vec3<f32>,
}

fn corner_uv(vertex_index: u32) -> vec2<f32> {
    switch vertex_index % 6u {
        case 0u: {
            return vec2<f32>(0.0, 1.0);
        }
        case 1u: {
            return vec2<f32>(0.0, 0.0);
        }
        case 2u: {
            return vec2<f32>(1.0, 0.0);
        }
        case 3u: {
            return vec2<f32>(0.0, 1.0);
        }
        case 4u: {
            return vec2<f32>(1.0, 0.0);
        }
        default: {
            return vec2<f32>(1.0, 1.0);
        }
    }
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    let clip = uniforms.view_proj * vec4<f32>(input.position, 1.0);
    var out: VertexOutput;
    out.clip_position = vec4<f32>(clip.xy, 0.0, clip.w);
    out.uv = vec2<f32>(1.0/512.0) + corner_uv(input.vertex_index)*(510.0/512.0);
    out.direction = input.position;
    return out;
}

// Constants and arithmetic preserve the installed g_sky shader's asin approximation.
fn painted_asin(t: f32) -> f32 {
    let v = abs(t);
    let p = ((-0.018729299306869507*v + 0.07426100224256516)*v - 0.2121143937110901)*v + 1.5707287788391113;
    let a = p*sqrt(max(0.0,1.0-v));
    return 1.5707963705062866-select(3.1415927410125732-a,a,t>=0.0);
}

fn painted_sky(direction: vec3<f32>) -> vec3<f32> {
    let d = normalize(direction);
    let h = abs(d.z);
    var gradient = 1.0;
    if sky.top.w != 0.0 { gradient = pow(h,sky.top.w); }
    var color = mix(sky.bottom.rgb,sky.top.rgb,gradient);
    var sun_dot = 0.0;
    if length(sky.sun_direction.xyz) >= 0.001 { sun_dot = dot(normalize(sky.sun_direction.xyz),d); }
    let q = (8.0*(1.0-h)+sun_dot)/9.0;
    if q > 0.0 {
        let dusk = pow(abs(q),10.0-5.0*sky.dusk.w)*sky.sun_color.w;
        color = color*(1.0-0.5*dusk)+sky.dusk.rgb*dusk;
    }
    if sun_dot > 0.0 && sky.sun_direction.w > 0.0 {
        color += sky.sun_color.rgb*pow(abs(sun_dot),1.0/(sky.sun_direction.w*0.05));
    }
    color *= sky.bottom.w;
    if d.z > 0.0 && sky.kind.y > 0u {
        let phase = sky.stars.w+sky.stars.z*uniforms.water_time_sky_tint.x;
        var uv = vec2<f32>(painted_asin(d.x)+sin(d.x)*(1.0-h)+phase,painted_asin(d.y)+sin(d.y)*(1.0-h));
        var stars = textureSample(sky_texture,stars_sampler,uv*sky.stars.x).rgb;
        if sky.kind.y > 1u {
            uv.x -= phase*0.01+1.0;
            stars += textureSample(sky_texture,stars_sampler,uv*sky.stars.x).rgb*0.5;
        }
        if sky.kind.y > 2u {
            uv.x -= phase*0.02+1.0;
            stars += textureSample(sky_texture,stars_sampler,uv*sky.stars.x).rgb*0.75;
        }
        color += stars*pow(h,1.0/sky.stars.y);
    }
    return color;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if sky.kind.x == 1u { return vec4<f32>(painted_sky(input.direction),1.0); }
    let uv = vec2<f32>(dot(vec4<f32>(input.uv,0.0,1.0),sky.texture_s),dot(vec4<f32>(input.uv,0.0,1.0),sky.texture_t));
    return vec4<f32>(textureSample(sky_texture, sky_sampler, uv).rgb*sky.tint.rgb,1.0);
}
