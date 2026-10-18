use super::{SurfaceFamily, SurfaceMaterial};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceDecalFormat {
    pub normal: bool,
    pub vertex_color: bool,
    pub tangent_space: bool,
    pub texcoord_dimensions: [u8; 8],
    pub user_data_size: u8,
    pub bone_weights: u8,
}

impl SurfaceMaterial {
    pub(crate) fn source_decal_format(&self) -> SourceDecalFormat {
        let defined = |parameter| self.parameters.contains_key(parameter);
        let flag = |parameter| self.authored_flag(parameter);
        let mut format = SourceDecalFormat {
            normal: true,
            vertex_color: false,
            tangent_space: false,
            texcoord_dimensions: [2, 0, 0, 0, 0, 0, 0, 0],
            user_data_size: 0,
            bone_weights: 0,
        };
        match self.family {
            SurfaceFamily::Lightmapped | SurfaceFamily::Transition => {
                let bumped = defined("$bumpmap") && self.family.accepts_primary_bumpmap(self.decal);
                let base2 = defined("$basetexture") && defined("$basetexture2");
                let bump2 = bumped && defined("$bumpmap2");
                format.vertex_color = self.vertex_color || base2 || bump2;
                let envmap = self
                    .parameters
                    .get("$envmap")
                    .is_some_and(|path| !path.eq_ignore_ascii_case("env_cubemap"));
                format.tangent_space = bumped || envmap;
                format.texcoord_dimensions[1] = 2;
                format.texcoord_dimensions[2] = if bumped { 2 } else { 0 };
            }
            SurfaceFamily::Unlit => {
                format.vertex_color =
                    self.vertex_color || self.vertex_alpha || flag("$vertexalphatest");
                format.texcoord_dimensions[1] = if flag("$separatedetailuvs") { 2 } else { 0 };
            }
            SurfaceFamily::VertexLit => {
                let bumped = defined("$bumpmap");
                let warp = defined("$lightwarptexture");
                let skin = flag("$phong") && (bumped || warp || flag("$basemapalphaphongmask"));
                format.vertex_color = skin && (self.vertex_color || self.vertex_alpha);
                format.user_data_size = 4;
                if !skin && flag("$separatedetailuvs") {
                    format.texcoord_dimensions[1] = 2;
                }
            }
            SurfaceFamily::DecalModulate => format.normal = false,
        }
        format
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::materials::{EffectiveMaterial, MaterialResolver};

    fn native_bits(format: SourceDecalFormat) -> u64 {
        let mut bits = 0x101
            | (u64::from(format.normal) << 1)
            | (u64::from(format.vertex_color) << 2)
            | if format.tangent_space { 0x30 } else { 0 }
            | (u64::from(format.bone_weights) << 11)
            | (u64::from(format.user_data_size) << 14);
        for (index, dimension) in format.texcoord_dimensions.into_iter().enumerate() {
            bits |= u64::from(dimension) << (17 + index * 3);
        }
        bits
    }

    #[test]
    fn lightmapped_snapshot_union_matches_native_material_vertex_formats() {
        let resolver = MaterialResolver::prop_lighting_fixture(Vec::new(), Vec::new());
        for (text, expected) in [
            (
                r#"LightmappedGeneric { "$basetexture" "absent" }"#,
                0x240103,
            ),
            (
                r#"LightmappedGeneric { "$basetexture" "absent" "$vertexcolor" "1" }"#,
                0x240107,
            ),
            (
                r#"LightmappedGeneric { "$basetexture" "absent" "$bumpmap" "absent" "$ssbump" "1" }"#,
                0x1240133,
            ),
            (
                r#"LightmappedGeneric { "$basetexture" "absent" "$bumpmap" "absent" "$decal" "1" }"#,
                0x240103,
            ),
        ] {
            let document = vformats::vmt::parse(text, &vformats::Limits::default()).unwrap();
            let effective = EffectiveMaterial::from_document(&document);
            let material = resolver.resolve_surface(
                &effective,
                SurfaceFamily::Lightmapped,
                super::super::SourceSurfaceBaseBlend::Single,
            );
            assert_eq!(
                native_bits(material.source_decal_format()),
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn native_static_order_controls_have_distinct_snapshot_formats() {
        let resolver = MaterialResolver::prop_lighting_fixture(Vec::new(), Vec::new());
        for (text, family, expected) in [
            (
                r#"LightmappedGeneric { "$basetexture" "gmp_decal_order/aba_a" "$decal" "1" "$translucent" "1" }"#,
                SurfaceFamily::Lightmapped,
                0x240103,
            ),
            (
                r#"LightmappedGeneric { "$basetexture" "gmp_decal_order/vertexcolor_b_b" "$decal" "1" "$translucent" "1" "$vertexcolor" "1" }"#,
                SurfaceFamily::Lightmapped,
                0x240107,
            ),
            (
                r#"UnlitGeneric { "$basetexture" "gmp_decal_order/unlit_b_b" "$decal" "1" "$translucent" "1" }"#,
                SurfaceFamily::Unlit,
                0x40103,
            ),
            (
                r#"DecalModulate { "$basetexture" "gmp_decal_order/modulate_b_b" }"#,
                SurfaceFamily::DecalModulate,
                0x40101,
            ),
        ] {
            let document = vformats::vmt::parse(text, &vformats::Limits::default()).unwrap();
            let effective = EffectiveMaterial::from_document(&document);
            let material = resolver.resolve_surface(
                &effective,
                family,
                super::super::SourceSurfaceBaseBlend::Single,
            );
            assert_eq!(
                native_bits(material.source_decal_format()),
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn source_snapshot_contract_is_independent_of_decoded_texture_availability() {
        let resolver = MaterialResolver::prop_lighting_fixture(Vec::new(), Vec::new());
        let document = vformats::vmt::parse(
            r#"LightmappedGeneric { "$basetexture" "absent" "$envmap" "absent_cube" }"#,
            &vformats::Limits::default(),
        )
        .unwrap();
        let effective = EffectiveMaterial::from_document(&document);
        let material = resolver.resolve_surface(
            &effective,
            SurfaceFamily::Lightmapped,
            super::super::SourceSurfaceBaseBlend::Single,
        );
        assert!(material.envmap.is_none());
        assert!(material.source_decal_format().tangent_space);
    }
}
