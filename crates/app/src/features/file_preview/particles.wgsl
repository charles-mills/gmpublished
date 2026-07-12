// Particle preview: instanced quads, either camera-facing billboards or
// axis-stretched (trails, rope segments). No depth buffer — translucents are
// CPU-sorted back-to-front and additives are order-independent.

struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_right: vec4<f32>,
    camera_up: vec4<f32>,
    camera_eye: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var sprite_texture: texture_2d<f32>;
@group(1) @binding(1) var sprite_sampler: sampler;

struct Instance {
    // xyz = world position, w = roll rotation in radians
    @location(0) position_rotation: vec4<f32>,
    // xyz = stretch axis (unnormalized ok), w = 0 billboard / 1 axis quad
    @location(1) axis_mode: vec4<f32>,
    // linear rgb, straight alpha
    @location(2) color: vec4<f32>,
    // x = half width, y = half length, z = mirror u, w = unused
    @location(3) size: vec4<f32>,
    // sprite sheet frame: xy = uv offset, zw = uv scale
    @location(4) uv_rect: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

var<private> corners: array<vec2<f32>, 6> = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(1.0, -1.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(1.0, 1.0),
    vec2<f32>(-1.0, 1.0)
);

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32, instance: Instance) -> VertexOutput {
    let corner = corners[vertex_index];
    let center = instance.position_rotation.xyz;
    let rotation = instance.position_rotation.w;
    let half_width = instance.size.x;
    let half_length = instance.size.y;

    var world_offset: vec3<f32>;
    if instance.axis_mode.w < 0.5 {
        // Billboard: rotate the corner in the camera plane.
        let cr = cos(rotation);
        let sr = sin(rotation);
        let local = vec2<f32>(
            corner.x * cr - corner.y * sr,
            corner.x * sr + corner.y * cr
        );
        world_offset = uniforms.camera_right.xyz * (local.x * half_width)
            + uniforms.camera_up.xyz * (local.y * half_width);
    } else {
        // Axis quad: length along the axis, width perpendicular in view.
        let axis = normalize(instance.axis_mode.xyz);
        let to_eye = normalize(uniforms.camera_eye.xyz - center);
        var side = cross(axis, to_eye);
        let side_length = length(side);
        if side_length < 1e-4 {
            side = uniforms.camera_right.xyz;
        } else {
            side = side / side_length;
        }
        world_offset = axis * (corner.y * half_length) + side * (corner.x * half_width);
    }

    var uv = corner * 0.5 + vec2<f32>(0.5, 0.5);
    if instance.size.z > 0.5 {
        uv.x = 1.0 - uv.x;
    }
    uv = instance.uv_rect.xy + uv * instance.uv_rect.zw;

    var out: VertexOutput;
    out.position = uniforms.view_proj * vec4<f32>(center + world_offset, 1.0);
    out.uv = uv;
    out.color = instance.color;
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let sampled = textureSample(sprite_texture, sprite_sampler, input.uv);
    return sampled * input.color;
}
