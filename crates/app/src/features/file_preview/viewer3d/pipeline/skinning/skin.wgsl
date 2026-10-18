struct SkinVertex {
    position: vec4<f32>,
    normal: vec4<f32>,
    tangent: vec4<f32>,
    weights: vec4<f32>,
    bones: vec4<u32>,
}

struct RenderVertex {
    position: array<f32, 3>,
    normal: array<f32, 3>,
    uv: array<f32, 2>,
    lightmap_uv: array<f32, 2>,
    lightmap_bump_step: f32,
    color: array<f32, 3>,
    blend_alpha: f32,
    opacity: f32,
    illumination: array<f32, 3>,
    lighting_mode: u32,
    tangent_s: array<f32, 3>,
    tangent_t: array<f32, 3>,
    tangent_sign: f32,
    lighting_context: u32,
    surface_mark: u32,
    lightmap_page: u32,
}

struct Parameters {
    placement: mat4x4<f32>,
    direction_x: vec4<f32>,
    direction_y: vec4<f32>,
    direction_z: vec4<f32>,
    source_start: u32,
    target_start: u32,
    vertex_count: u32,
    padding: u32,
}

@group(0) @binding(0) var<storage, read> sources: array<SkinVertex>;
@group(0) @binding(1) var<storage, read> palette: array<mat4x4<f32>>;
@group(0) @binding(2) var<storage, read_write> targets: array<RenderVertex>;
@group(0) @binding(3) var<uniform> parameters: Parameters;

fn placed_direction(value: vec3<f32>) -> vec3<f32> {
    return parameters.direction_x.xyz * value.x
        + parameters.direction_y.xyz * value.y
        + parameters.direction_z.xyz * value.z;
}

@compute @workgroup_size(64)
fn skin(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if invocation.x >= parameters.vertex_count { return; }
    let source = sources[parameters.source_start + invocation.x];
    let target_index = parameters.target_start + invocation.x;
    var position = source.position.xyz;
    var normal = source.normal.xyz;
    var tangent = source.tangent.xyz;
    if source.bones.w > 0u {
        position = vec3<f32>(0.0);
        normal = vec3<f32>(0.0);
        tangent = vec3<f32>(0.0);
        for (var slot = 0u; slot < source.bones.w; slot += 1u) {
            let weight = source.weights[slot];
            if weight <= 0.00000011920929 { continue; }
            let transform = palette[source.bones[slot]];
            position += (transform * vec4<f32>(source.position.xyz, 1.0)).xyz * weight;
            normal += (transform * vec4<f32>(source.normal.xyz, 0.0)).xyz * weight;
            tangent += (transform * vec4<f32>(source.tangent.xyz, 0.0)).xyz * weight;
        }
    }
    position = (parameters.placement * vec4<f32>(position, 1.0)).xyz;
    let model_bitangent = cross(normal, tangent) * source.tangent.w;
    normal = placed_direction(normal);
    tangent = placed_direction(tangent);
    let bitangent = placed_direction(model_bitangent);
    targets[target_index].position = array<f32, 3>(position.x, position.y, position.z);
    targets[target_index].normal = array<f32, 3>(normal.x, normal.y, normal.z);
    targets[target_index].tangent_s = array<f32, 3>(tangent.x, tangent.y, tangent.z);
    targets[target_index].tangent_t = array<f32, 3>(bitangent.x, bitangent.y, bitangent.z);
    targets[target_index].tangent_sign = source.tangent.w;
}
