use super::parameters::{scalar, tint};
use super::{Arc, DecodedTextureFrames, EffectiveMaterial, MaterialResolver, SurfaceMaterial};

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent authored Refract shader switches"
)]
pub struct RefractionMaterial {
    pub tint: [f32; 3],
    pub amount: f32,
    pub blur: bool,
    pub silhouette: bool,
    pub no_write_z: bool,
    pub force_alpha_write: bool,
    pub masked: bool,
    pub vertex_modulate: bool,
    pub base_texture: bool,
    pub tint_texture: Option<Arc<DecodedTextureFrames>>,
    pub tint_frame: f32,
}
pub(super) fn is_refract(shader: &str) -> bool {
    ["Refract", "Refract_DX90"]
        .iter()
        .any(|s| shader.eq_ignore_ascii_case(s))
}
impl MaterialResolver {
    pub(super) fn refract_fallback_texture(
        &self,
        material: &EffectiveMaterial,
    ) -> Option<Arc<super::ResolvedTexture>> {
        is_refract(&material.shader).then(|| {
            Arc::new(super::ResolvedTexture::rgba(vec![255; 4], 1, 1, 1, 1, false).unmapped())
        })
    }
    pub(super) fn resolve_refraction(
        &self,
        material: &EffectiveMaterial,
    ) -> Option<RefractionMaterial> {
        if !is_refract(&material.shader) {
            return None;
        }
        let get = |name| material.parameter(name);
        Some(RefractionMaterial {
            tint: tint(get("$refracttint"), [1.0; 3]),
            amount: scalar(get("$refractamount"), 2.0),
            blur: scalar(get("$bluramount"), 1.0) >= 1.0,
            silhouette: get("$fadeoutonsilhouette").is_none_or(|v| super::vmt_bool(Some(v))),
            no_write_z: super::vmt_bool(get("$nowritez")),
            force_alpha_write: super::vmt_bool(get("$forcealphawrite")),
            masked: super::vmt_bool(get("$masked")),
            vertex_modulate: super::vmt_bool(get("$vertexcolormodulate")),
            base_texture: get("$basetexture").is_some(),
            tint_texture: get("$refracttinttexture")
                .and_then(|name| self.resolve_raw_texture_frames(name)),
            tint_frame: scalar(get("$refracttinttextureframe"), 0.0),
        })
    }
}
impl SurfaceMaterial {
    pub(crate) fn is_refractive(&self) -> bool {
        self.refraction.is_some()
    }
}
