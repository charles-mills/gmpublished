// Source SDK common_vertexlitgeneric_dx9.h and skin_ps20b.fxc.
fn source_model_ambient(normal: vec3<f32>, cube: array<vec3<f32>, 6>) -> vec3<f32> {
    let squared = normal * normal;
    return squared.x * select(cube[1], cube[0], normal.x >= 0.0)
        + squared.y * select(cube[3], cube[2], normal.y >= 0.0)
        + squared.z * select(cube[5], cube[4], normal.z >= 0.0);
}

fn source_model_diffuse_coordinate(normal: vec3<f32>, light_direction: vec3<f32>, half_lambert: bool, has_warp: bool) -> f32 {
    let ndotl = dot(normal, light_direction);
    if !half_lambert { return clamp(ndotl, 0.0, 1.0); }
    let half_term = clamp(ndotl * 0.5 + 0.5, 0.0, 1.0);
    return select(half_term * half_term, half_term, has_warp);
}

fn source_model_diffuse_term(coordinate: f32, ambient_occlusion: f32, has_warp: bool, warp_texel: vec3<f32>) -> vec3<f32> {
    return select(vec3<f32>(coordinate * ambient_occlusion), 2.0 * warp_texel, has_warp);
}

fn source_model_diffuse_warp_uv(coordinate: f32, ambient_occlusion: f32) -> vec2<f32> {
    return vec2<f32>(coordinate * ambient_occlusion, 0.5);
}

fn source_model_phong_mask(normal_alpha: f32, base_alpha: f32, base_alpha_mask: bool) -> f32 {
    return select(normal_alpha, base_alpha, base_alpha_mask);
}

fn source_model_phong_tangent_normal(normal_texel: vec3<f32>, base_alpha_mask: bool) -> vec3<f32> {
    return select(2.0 * normal_texel - vec3<f32>(1.0), vec3<f32>(0.0, 0.0, 1.0), base_alpha_mask);
}

fn source_model_fresnel(normal: vec3<f32>, eye_direction: vec3<f32>) -> f32 {
    let fresnel = clamp(1.0 - dot(normal, eye_direction), 0.0, 1.0);
    return fresnel * fresnel;
}

fn source_model_phong_fresnel(normal: vec3<f32>, eye_direction: vec3<f32>, ranges: vec3<f32>) -> f32 {
    let shifted = source_model_fresnel(normal, eye_direction) - 0.5;
    let slope = select(2.0 * (ranges.y - ranges.x), 2.0 * (ranges.z - ranges.y), shifted >= 0.0);
    return ranges.y + slope * shifted;
}

fn source_model_phong_exponent(scalar: f32, exponent_red: f32) -> f32 {
    // The CPU supplies a negative scalar sentinel only after native init.
    return select(1.0 + 149.0 * exponent_red, scalar, scalar >= 0.0);
}

fn source_model_phong_lobe(normal: vec3<f32>, light_direction: vec3<f32>, eye_direction: vec3<f32>, exponent: f32) -> f32 {
    let reflected = 2.0 * normal * dot(normal, eye_direction) - eye_direction;
    return pow(clamp(dot(reflected, light_direction), 0.0, 1.0), exponent);
}

fn source_model_phong_warp_uv(lobe: f32, fresnel: f32) -> vec2<f32> {
    return vec2<f32>(lobe, fresnel);
}

fn source_model_phong_light(normal: vec3<f32>, light_direction: vec3<f32>, lobe: f32, light_radiance: vec3<f32>, attenuation: f32, ambient_occlusion: f32, has_warp: bool, warp_texel: vec3<f32>) -> vec3<f32> {
    return lobe * select(vec3<f32>(1.0), warp_texel, has_warp)
        * clamp(dot(normal, light_direction), 0.0, 1.0) * light_radiance * attenuation * ambient_occlusion;
}

fn source_model_phong_finish(specular: vec3<f32>, raw_mask: f32, boost: f32, fresnel: f32, has_warp: bool) -> vec3<f32> {
    return specular * raw_mask * boost * select(fresnel, 1.0, has_warp);
}

fn source_model_phong_tint(constant_tint: vec3<f32>, base_linear: vec3<f32>, exponent_green: f32) -> vec3<f32> {
    // Native initialization uses a negative red sentinel for exponent-map tint.
    return select(mix(vec3<f32>(1.0), base_linear, exponent_green), constant_tint, constant_tint.x >= 0.0);
}

fn source_model_rim_finish(specular: vec3<f32>, rim_direct: vec3<f32>, ambient_view: vec3<f32>, normal: vec3<f32>, eye_direction: vec3<f32>, mask: f32, boost: f32) -> vec3<f32> {
    let fresnel = source_model_fresnel(normal, eye_direction);
    let multiply = mask * fresnel * fresnel;
    return max(specular, rim_direct * multiply)
        + ambient_view * boost * clamp(multiply * normal.z, 0.0, 1.0);
}

fn source_model_rim_light(normal: vec3<f32>, light_direction: vec3<f32>, eye_direction: vec3<f32>, exponent: f32, light_radiance: vec3<f32>, attenuation: f32) -> vec3<f32> {
    // Source applies AO to specular, but not to this independent direct rim term.
    return source_model_phong_lobe(normal, light_direction, eye_direction, exponent)
        * clamp(dot(normal, light_direction), 0.0, 1.0) * light_radiance * attenuation;
}
