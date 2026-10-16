use super::{Arc, DecodedTextureFrames, EffectiveMaterial, MaterialResolver};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhongTexture {
    pub path: String,
    /// Exponent and warp channels are raw UNORM data, including their alpha.
    pub texture: Option<Arc<DecodedTextureFrames>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PhongRim {
    pub enabled: Option<bool>,
    pub exponent: Option<f32>,
    pub boost: Option<f32>,
    pub exponent_alpha_mask: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhongMaterial {
    pub enabled: Option<bool>,
    pub exponent: Option<f32>,
    pub exponent_factor: Option<f32>,
    pub boost: Option<f32>,
    pub fresnel_ranges: Option<[f32; 3]>,
    pub tint: Option<[f32; 3]>,
    pub albedo_tint: Option<bool>,
    pub base_alpha_mask: Option<bool>,
    pub invert_mask: Option<bool>,
    pub half_lambert: Option<bool>,
    pub exponent_texture: Option<PhongTexture>,
    pub specular_warp: Option<PhongTexture>,
    pub diffuse_warp: Option<PhongTexture>,
    pub rim: PhongRim,
}

const PARAMETERS: [&str; 17] = [
    "$phong",
    "$phongexponent",
    "$phongexponentfactor",
    "$phongboost",
    "$phongfresnelranges",
    "$phongtint",
    "$phongalbedotint",
    "$basemapalphaphongmask",
    "$invertphongmask",
    "$halflambert",
    "$phongexponenttexture",
    "$phongwarptexture",
    "$lightwarptexture",
    "$rimlight",
    "$rimlightexponent",
    "$rimlightboost",
    "$rimmask",
];

fn finite_scalar(material: &EffectiveMaterial, name: &str) -> Option<f32> {
    let authored = material.parameter(name)?;
    let parsed = authored
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite());
    if parsed.is_none() {
        log::debug!("invalid model lighting scalar {name}: {authored}");
    }
    parsed
}

fn finite_vector(material: &EffectiveMaterial, name: &str) -> Option<[f32; 3]> {
    let authored = material.parameter(name)?;
    let parsed = super::parse_material_color(authored)
        .map(super::Vec3::to_array)
        .or_else(|| finite_scalar(material, name).map(|v| [v; 3]));
    if parsed.is_none() {
        log::debug!("invalid model lighting vector {name}: {authored}");
    }
    parsed
}

impl MaterialResolver {
    pub(super) fn resolve_phong(&self, material: &EffectiveMaterial) -> Option<Arc<PhongMaterial>> {
        if !PARAMETERS
            .iter()
            .any(|name| material.parameter(name).is_some())
        {
            return None;
        }
        let flag = |name| {
            material
                .parameter(name)
                .map(|value| super::vmt_bool(Some(value)))
        };
        let texture = |name| {
            material.parameter(name).map(|path| PhongTexture {
                path: path.to_owned(),
                texture: self.resolve_raw_texture_frames(path),
            })
        };
        Some(Arc::new(PhongMaterial {
            enabled: flag("$phong"),
            exponent: finite_scalar(material, "$phongexponent"),
            exponent_factor: finite_scalar(material, "$phongexponentfactor"),
            boost: finite_scalar(material, "$phongboost"),
            fresnel_ranges: finite_vector(material, "$phongfresnelranges"),
            tint: finite_vector(material, "$phongtint"),
            albedo_tint: flag("$phongalbedotint"),
            base_alpha_mask: flag("$basemapalphaphongmask"),
            invert_mask: flag("$invertphongmask"),
            half_lambert: flag("$halflambert"),
            exponent_texture: texture("$phongexponenttexture"),
            specular_warp: texture("$phongwarptexture"),
            diffuse_warp: texture("$lightwarptexture"),
            rim: PhongRim {
                enabled: flag("$rimlight"),
                exponent: finite_scalar(material, "$rimlightexponent"),
                boost: finite_scalar(material, "$rimlightboost"),
                exponent_alpha_mask: flag("$rimmask"),
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vformats::Limits;

    fn state(text: &str) -> Option<Arc<PhongMaterial>> {
        let resolver = MaterialResolver::prop_lighting_fixture(Vec::new(), Vec::new());
        let document = vformats::vmt::parse(text, &Limits::default()).unwrap();
        resolver.resolve_phong(&EffectiveMaterial::from_document(&document))
    }

    #[test]
    fn omitted_parameters_remain_distinct_from_authored_zero_and_false() {
        assert!(state("VertexLitGeneric {}").is_none());
        let value = state(r#"VertexLitGeneric { "$phong" "0" "$phongexponent" "0" "$phongboost" "0" "$basemapalphaphongmask" "0" }"#).unwrap();
        assert_eq!(value.enabled, Some(false));
        assert_eq!(value.exponent, Some(0.0));
        assert_eq!(value.boost, Some(0.0));
        assert_eq!(value.base_alpha_mask, Some(false));
        assert_eq!(value.tint, None);
        assert_eq!(value.fresnel_ranges, None);
        assert_eq!(value.albedo_tint, None);
    }

    #[test]
    fn authored_doll_phong_terms_preserve_linear_values_and_independent_tint_masks() {
        let value = state(r#"VertexLitGeneric { "$phong" "1" "$phongexponent" "12" "$phongboost" ".8" "$phongfresnelranges" "[.1 .3 .9]" "$blendtintbybasealpha" "1" "$blendtintcoloroverbase" ".05" }"#).unwrap();
        assert_eq!(value.enabled, Some(true));
        assert_eq!(value.exponent, Some(12.0));
        assert_eq!(value.boost, Some(0.8));
        assert_eq!(value.fresnel_ranges, Some([0.1, 0.3, 0.9]));
        assert_eq!(value.base_alpha_mask, None);
        assert_eq!(value.tint, None);
    }

    #[test]
    fn missing_warp_resources_preserve_authored_identity_and_all_mask_switches() {
        let value = state(r#"VertexLitGeneric { "$phongexponenttexture" "custom/exponent" "$phongwarptexture" "custom/specular" "$lightwarptexture" "custom/diffuse" "$phongexponentfactor" "-2" "$phongtint" "[.5 1 2]" "$phongalbedotint" "1" "$basemapalphaphongmask" "1" "$invertphongmask" "1" "$halflambert" "1" "$rimlight" "1" "$rimlightexponent" "4" "$rimlightboost" "2" "$rimmask" "1" }"#).unwrap();
        assert_eq!(
            value.exponent_texture.as_ref().unwrap().path,
            "custom/exponent"
        );
        assert!(value.exponent_texture.as_ref().unwrap().texture.is_none());
        assert_eq!(
            value.specular_warp.as_ref().unwrap().path,
            "custom/specular"
        );
        assert_eq!(value.diffuse_warp.as_ref().unwrap().path, "custom/diffuse");
        assert_eq!(value.exponent_factor, Some(-2.0));
        assert_eq!(value.tint, Some([0.5, 1.0, 2.0]));
        assert_eq!(value.albedo_tint, Some(true));
        assert_eq!(value.base_alpha_mask, Some(true));
        assert_eq!(value.invert_mask, Some(true));
        assert_eq!(value.half_lambert, Some(true));
        assert_eq!(
            value.rim,
            PhongRim {
                enabled: Some(true),
                exponent: Some(4.0),
                boost: Some(2.0),
                exponent_alpha_mask: Some(true)
            }
        );
    }

    #[test]
    fn exponent_and_warp_samples_retain_raw_rgba_even_without_alpha_flags() {
        let mut bytes = vec![0_u8; 80];
        bytes[..4].copy_from_slice(b"VTF\0");
        for (at, value) in [(4, 7_u32), (8, 2), (12, 80), (57, u32::MAX)] {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        for at in [16, 18, 24, 63] {
            bytes[at..at + 2].copy_from_slice(&1_u16.to_le_bytes());
        }
        bytes[56] = 1;
        bytes.extend_from_slice(&[32, 64, 128, 17]);
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![("materials/custom/raw.vtf", bytes)],
            Vec::new(),
        );
        let document = vformats::vmt::parse(r#"VertexLitGeneric { "$phong" "0" "$phongexponenttexture" "custom/raw" "$phongwarptexture" "custom/raw" "$lightwarptexture" "custom/raw" }"#, &Limits::default()).unwrap();
        let value = resolver
            .resolve_phong(&EffectiveMaterial::from_document(&document))
            .unwrap();
        assert_eq!(value.enabled, Some(false));
        for role in [
            &value.exponent_texture,
            &value.specular_warp,
            &value.diffuse_warp,
        ] {
            let texture = role.as_ref().unwrap().texture.as_ref().unwrap();
            assert_eq!(texture.flags(), 0);
            assert_eq!(
                texture.frame(0).unwrap()[0].bytes.as_slice(),
                [32, 64, 128, 17]
            );
        }
        assert!(Arc::ptr_eq(
            value
                .exponent_texture
                .as_ref()
                .unwrap()
                .texture
                .as_ref()
                .unwrap()
                .resident()
                .unwrap(),
            value
                .diffuse_warp
                .as_ref()
                .unwrap()
                .texture
                .as_ref()
                .unwrap()
                .resident()
                .unwrap()
        ));
    }

    #[test]
    fn nonfinite_values_are_not_uploaded_or_replaced_by_native_defaults() {
        let value = state(r#"VertexLitGeneric { "$phongexponent" "NaN" "$phongboost" "inf" "$phongfresnelranges" "[0 inf 1]" "$phongtint" "[-1 .5 2]" }"#).unwrap();
        assert_eq!(value.exponent, None);
        assert_eq!(value.boost, None);
        assert_eq!(value.fresnel_ranges, None);
        assert_eq!(value.tint, Some([-1.0, 0.5, 2.0]));
    }
}
