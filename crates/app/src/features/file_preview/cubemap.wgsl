// Source's reflection vector supports interpolated, non-unit normals.
fn source_cube_reflection(normal: vec3<f32>, vertex_to_eye: vec3<f32>) -> vec3<f32> {
    return 2.0 * normal * dot(normal, vertex_to_eye)
        - vertex_to_eye * dot(normal, normal);
}

// The caller supplies radiance with the native environment scale applied.
fn source_cube_specular(
    radiance: vec3<f32>,
    mask: vec3<f32>,
    tint: vec3<f32>,
    contrast: f32,
    saturation: f32,
) -> vec3<f32> {
    var specular = radiance * mask * tint;
    specular = mix(specular, specular * specular, contrast);
    let grey = dot(specular, vec3<f32>(0.299, 0.587, 0.114));
    return mix(vec3<f32>(grey), specular, saturation);
}

fn source_world_cube_fresnel(normal: vec3<f32>, vertex_to_eye: vec3<f32>, reflection: f32) -> f32 {
    let facing = 1.0 - dot(normal, normalize(vertex_to_eye));
    return pow(facing, 5.0) * (1.0 - reflection) + reflection;
}
