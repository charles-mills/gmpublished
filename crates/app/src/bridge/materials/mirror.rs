use super::parameters::{scalar, texture_transform, tint};
use super::{
    Arc, BTreeMap, DecodedTextureFrames, EffectiveMaterial, MaterialResolver,
    normalize_texture_name,
};
#[derive(Clone, Debug, PartialEq)]
pub struct MirrorMaterial {
    pub shader: String,
    pub parameters: BTreeMap<String, String>,
    pub normal_map: Option<Arc<DecodedTextureFrames>>,
    pub normal_map_path: String,
    pub bump_transform: [[f32; 4]; 2],
    pub reflect_amount: f32,
    pub refract_amount: f32,
    pub reflect_tint_linear: [f32; 3],
    pub refract_tint_linear: [f32; 3],
    pub reflect_enabled: bool,
    pub refract_enabled: bool,
    pub fresnel_power: f32,
    pub min_reflectivity: f32,
    pub max_reflectivity: f32,
    pub unsupported_features: Vec<String>,
}
pub(super) fn is_mirror_shader(shader: &str) -> bool {
    shader.eq_ignore_ascii_case("LightmappedReflective")
        || shader.eq_ignore_ascii_case("LightmappedReflective_DX90")
}
impl MaterialResolver {
    pub(super) fn resolve_mirror(&self, material: &EffectiveMaterial) -> MirrorMaterial {
        let get = |key| material.parameter(key);
        let normal_map_path = get("$normalmap")
            .and_then(normalize_texture_name)
            .unwrap_or_else(|| "dev/water_normal".into());
        let mut unsupported_features = Vec::new();
        if get("$basetexture").is_some() {
            unsupported_features.push("base-textured mirror lighting/mask".to_owned());
        }
        for proxy in &material.proxies {
            unsupported_features.push(format!("mirror proxy {}", proxy.name));
        }
        for feature in &unsupported_features {
            log::warn!("unsupported reflective glass feature: {feature}");
        }
        MirrorMaterial {
            shader: material.shader.clone(),
            parameters: material.parameters.clone(),
            normal_map: (!material.no_draw())
                .then(|| self.resolve_water_normal(&normal_map_path))
                .flatten(),
            normal_map_path,
            bump_transform: texture_transform(get("$bumptransform")),
            reflect_amount: scalar(get("$reflectamount"), 0.8),
            refract_amount: scalar(get("$refractamount"), 0.0),
            reflect_tint_linear: tint(get("$reflecttint"), [1.0; 3]),
            refract_tint_linear: tint(get("$refracttint"), [1.0; 3]),
            reflect_enabled: get("$reflecttexture").is_none_or(|value| !value.is_empty()),
            refract_enabled: get("$refracttexture").is_none_or(|value| !value.is_empty()),
            fresnel_power: scalar(get("$fresnelpower"), 5.0).max(0.0),
            min_reflectivity: scalar(get("$minreflectivity"), 0.0),
            max_reflectivity: scalar(get("$maxreflectivity"), 1.0),
            unsupported_features,
        }
    }
}
