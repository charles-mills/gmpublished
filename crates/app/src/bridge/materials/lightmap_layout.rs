use super::{EffectiveMaterial, MaterialResolver, SurfaceFamily, material_paths, vmt_bool};
use gmpublished_domain::scene::map::SourceLightmapRequirement;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedLightmapLayoutMaterial {
    pub canonical_name: String,
    pub requirement: SourceLightmapRequirement,
}

impl MaterialResolver {
    pub(crate) fn resolve_lightmap_layout_material(
        &self,
        name: &str,
    ) -> super::ResolvedLightmapLayoutMaterial {
        self.material_definition(&material_paths(&[], name))
            .and_then(|definition| {
                let material = &definition.material;
                if material.shader.eq_ignore_ascii_case("patch") {
                    return None;
                }
                Some(ResolvedLightmapLayoutMaterial {
                    canonical_name: definition
                        .path
                        .as_str()
                        .strip_prefix("materials/")?
                        .strip_suffix(".vmt")?
                        .to_owned(),
                    requirement: lightmap_requirement(material),
                })
            })
            .unwrap_or_else(|| ResolvedLightmapLayoutMaterial {
                canonical_name: "___error".to_owned(),
                requirement: SourceLightmapRequirement::None,
            })
    }
}

fn lightmap_requirement(material: &EffectiveMaterial) -> SourceLightmapRequirement {
    let parameter = |key| material.parameter(key);
    let normal = if super::is_water_shader(&material.shader) {
        parameter("$normalmap").is_some()
    } else if super::mirror::is_mirror_shader(&material.shader) {
        if parameter("$basetexture").is_none() {
            return SourceLightmapRequirement::None;
        }
        parameter("$normalmap").is_some()
    } else {
        match super::shader::surface_family(&material.shader) {
            Some(SurfaceFamily::Lightmapped | SurfaceFamily::Transition) => {
                material.effective_bumpmap().is_some()
                    && !vmt_bool(parameter("$nodiffusebumplighting"))
            }
            Some(
                SurfaceFamily::VertexLit | SurfaceFamily::Unlit | SurfaceFamily::DecalModulate,
            )
            | None => return SourceLightmapRequirement::None,
        }
    };
    if normal {
        SourceLightmapRequirement::Bumped
    } else {
        SourceLightmapRequirement::Lightmapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_resolution_uses_effective_parameters_without_texture_reads() {
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![
                ("materials/test/lit.vmt", br#"LightmappedGeneric { "$basetexture" "test/absent" "$bumpmap" "test/absent_normal" "$nodiffusebumplighting" "0" }"#.to_vec()),
                ("materials/test/patch.vmt", br#"Patch { "include" "materials/test/lit.vmt" "replace" { "$nodiffusebumplighting" "1" } }"#.to_vec()),
                ("materials/test/profile.vmt", br#"WorldVertexTransition { "$bumpmap" "test/absent_normal" "ldr" { "$nodiffusebumplighting" "1" } "hdr" { "$nodiffusebumplighting" "0" } }"#.to_vec()),
                ("materials/test/decal.vmt", br#"LightmappedGeneric { "$bumpmap" "test/absent_normal" "$decal" "1" }"#.to_vec()),
                ("materials/test/nondecal.vmt", br#"Patch { "include" "materials/test/decal.vmt" "replace" { "$decal" "0" } }"#.to_vec()),
            ],
            Vec::new(),
        );
        assert_eq!(
            resolver.resolve_lightmap_layout_material("TEST/LIT.VMT"),
            ResolvedLightmapLayoutMaterial {
                canonical_name: "test/lit".to_owned(),
                requirement: SourceLightmapRequirement::Bumped,
            }
        );
        for name in ["test/patch", "test/profile", "test/decal"] {
            assert_eq!(
                resolver.resolve_lightmap_layout_material(name).requirement,
                SourceLightmapRequirement::Lightmapped,
                "{name}"
            );
        }
        assert_eq!(
            resolver
                .resolve_lightmap_layout_material("test/nondecal")
                .requirement,
            SourceLightmapRequirement::Bumped
        );
        assert_eq!(resolver.shared.texture_assets.resident_count(), 0);
    }

    #[test]
    fn shader_families_define_layout_requirements_independently_of_images() {
        for (text, expected) in [
            (
                "LightmappedGeneric {}",
                SourceLightmapRequirement::Lightmapped,
            ),
            (
                "WorldVertexTransition {}",
                SourceLightmapRequirement::Lightmapped,
            ),
            (
                r#"WorldVertexTransition { "$bumpmap" "absent" "$decal" "1" }"#,
                SourceLightmapRequirement::Lightmapped,
            ),
            (
                r#"LightmappedGeneric { "$bumpmap" "absent" "$translucent" "1" }"#,
                SourceLightmapRequirement::Bumped,
            ),
            ("VertexLitGeneric {}", SourceLightmapRequirement::None),
            ("UnlitGeneric {}", SourceLightmapRequirement::None),
            ("DecalModulate {}", SourceLightmapRequirement::None),
            (
                "LightmappedGeneric_Decal {}",
                SourceLightmapRequirement::None,
            ),
            ("Water {}", SourceLightmapRequirement::Lightmapped),
            (
                r#"Water { "$normalmap" "absent" }"#,
                SourceLightmapRequirement::Bumped,
            ),
            ("LightmappedReflective {}", SourceLightmapRequirement::None),
            (
                r#"LightmappedReflective { "$normalmap" "absent" }"#,
                SourceLightmapRequirement::None,
            ),
            (
                r#"LightmappedReflective { "$basetexture" "absent" }"#,
                SourceLightmapRequirement::Lightmapped,
            ),
            (
                r#"LightmappedReflective { "$basetexture" "absent" "$normalmap" "absent" }"#,
                SourceLightmapRequirement::Bumped,
            ),
        ] {
            let resolver = MaterialResolver::prop_lighting_fixture(
                vec![("materials/test/input.vmt", text.as_bytes().to_vec())],
                Vec::new(),
            );
            assert_eq!(
                resolver
                    .resolve_lightmap_layout_material("test/input")
                    .requirement,
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn missing_materials_share_the_error_identity_while_missing_images_do_not() {
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![(
                "materials/test/valid.vmt",
                br#"LightmappedGeneric { "$basetexture" "missing" }"#.to_vec(),
            )],
            Vec::new(),
        );
        let first = resolver.resolve_lightmap_layout_material("missing/first");
        assert_eq!(
            first,
            resolver.resolve_lightmap_layout_material("missing/second")
        );
        assert_eq!(first.canonical_name, "___error");
        assert_eq!(first.requirement, SourceLightmapRequirement::None);
        assert_eq!(
            resolver
                .resolve_lightmap_layout_material("test/valid")
                .canonical_name,
            "test/valid"
        );
    }
}
