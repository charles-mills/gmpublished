use super::{
    MaterialResolver, TextureAssetError, TextureSourceError, Vec3, material_paths, texture_path,
};
use gmpublished_domain::scene::map::MapRadiosityMaterialFlags;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedSurfaceLighting {
    pub reflectivity: Vec3,
    pub flags: MapRadiosityMaterialFlags,
    pub source_outcome: super::TextureLoadOutcome,
}

impl MaterialResolver {
    pub(crate) fn resolve_surface_lighting(&self, name: &str) -> Option<ResolvedSurfaceLighting> {
        let paths = material_paths(&[], name);
        let Some(definition) = self.material_definition(&paths) else {
            if self.material_source_failure(&paths)
                != Some(super::definition::MaterialSourceFailure::Missing)
            {
                return None;
            }
            return Some(ResolvedSurfaceLighting {
                reflectivity: Vec3::ZERO,
                source_outcome: super::TextureLoadOutcome::Generated,
                flags: MapRadiosityMaterialFlags {
                    no_draw: false,
                    translucent: false,
                },
            });
        };
        let material = &definition.material;
        if material.shader.eq_ignore_ascii_case("patch") {
            return None;
        }
        let native_error_texture = super::texture_failure_policy(material)
            == super::TextureFailurePolicy::NativeErrorTexture;
        let mut source_outcome = super::TextureLoadOutcome::Loaded;
        let (reflectivity, base_flags, has_base_texture) = if let Some(base) =
            material.effective_base_texture()
        {
            let path = texture_path(base)?;
            match self.selected_texture_source(&path) {
                Ok(selected) => match selected.validate_payload() {
                    Ok(()) => {
                        let metadata = selected.metadata();
                        let reflectivity =
                            Vec3::from(metadata.reflectivity_bits?.map(f32::from_bits));
                        if !reflectivity.is_finite() {
                            return None;
                        }
                        (reflectivity, metadata.flags, true)
                    }
                    Err(error) if native_error_texture && super::native_invalid_texture(&error) => {
                        source_outcome = super::TextureLoadOutcome::InvalidPayload;
                        (
                            Vec3::splat(0.5),
                            Some(super::NATIVE_ERROR_TEXTURE_FLAGS),
                            true,
                        )
                    }
                    Err(error) => {
                        log::warn!("surface lighting texture {path}: {error}");
                        return None;
                    }
                },
                Err(
                    TextureSourceError::EncodedBudget {
                        metadata: Some(metadata),
                        ..
                    }
                    | TextureSourceError::WorkingBudget {
                        metadata: Some(metadata),
                        ..
                    },
                ) => {
                    let reflectivity = Vec3::from(metadata.reflectivity_bits?.map(f32::from_bits));
                    if !reflectivity.is_finite() {
                        return None;
                    }
                    source_outcome = super::TextureLoadOutcome::SourceBudgetRejected;
                    (reflectivity, metadata.flags, true)
                }
                Err(TextureSourceError::Invalid {
                    error: TextureAssetError::Vtf(error),
                    ..
                }) if native_error_texture && super::native_invalid_texture(&error) => {
                    source_outcome = super::TextureLoadOutcome::InvalidSource;
                    (
                        Vec3::splat(0.5),
                        Some(super::NATIVE_ERROR_TEXTURE_FLAGS),
                        true,
                    )
                }
                Err(TextureSourceError::Missing) if native_error_texture => {
                    source_outcome = super::TextureLoadOutcome::MissingSource;
                    (
                        Vec3::splat(0.5),
                        Some(super::NATIVE_ERROR_TEXTURE_FLAGS),
                        true,
                    )
                }
                Err(TextureSourceError::Missing) => return None,
                Err(error) => {
                    log::warn!("surface lighting texture {path}: {error}");
                    return None;
                }
            }
        } else {
            source_outcome = super::TextureLoadOutcome::Generated;
            (Vec3::splat(0.2), None, false)
        };
        let (shader, _) = self.plan_texture_requests(|resolver| {
            resolver.resolve_shader(material, has_base_texture, false)
        });
        let render_mode = shader.surface().map_or_else(
            || material.render_mode(),
            |surface| surface.draw_render_mode(base_flags, surface.alpha),
        );
        Some(ResolvedSurfaceLighting {
            reflectivity,
            source_outcome,
            flags: MapRadiosityMaterialFlags {
                no_draw: super::vmt_bool(material.parameter("$nodraw")),
                translucent: matches!(
                    render_mode,
                    super::RenderMode::Translucent | super::RenderMode::Additive
                ),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_vtf_bytes;
    use vformats::vtf::VtfFormat;

    #[test]
    fn patched_material_uses_base_header_without_decoding_or_authored_override() {
        let mut base = fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[255, 0, 0, 255]]);
        let expected = [0.125_f32, 0.25, 0.75];
        for (index, value) in expected.into_iter().enumerate() {
            base[32 + index * 4..36 + index * 4].copy_from_slice(&value.to_le_bytes());
        }
        let resolver = MaterialResolver::prop_lighting_fixture(vec![
            ("materials/test/base.vtf", base),
            ("materials/test/parent.vmt", br#"LightmappedGeneric { "$basetexture" "test/base" "$reflectivity" "[1 0 0]" }"#.to_vec()),
            ("materials/test/patch.vmt", br#"Patch { "include" "materials/test/parent.vmt" "replace" { "$reflectivity" "[0 1 0]" } }"#.to_vec()),
            ("materials/test/empty.vmt", b"VertexLitGeneric {}".to_vec()),
            ("materials/test/missing.vmt", br#"LightmappedGeneric { "$basetexture" "test/absent" }"#.to_vec()),
        ], Vec::new());
        assert_eq!(
            resolver
                .resolve_surface_lighting("test/patch")
                .unwrap()
                .reflectivity,
            Vec3::from(expected)
        );
        assert_eq!(
            resolver
                .resolve_surface_lighting("test/empty")
                .unwrap()
                .reflectivity,
            Vec3::splat(0.2)
        );
        assert_eq!(
            resolver
                .resolve_surface_lighting("test/missing")
                .unwrap()
                .reflectivity,
            Vec3::splat(0.5)
        );
        assert_eq!(
            resolver
                .resolve_surface_lighting("test/absent")
                .unwrap()
                .reflectivity,
            Vec3::ZERO
        );
        assert!(resolver.shared.texture_assets.resident_count() == 0);
    }

    #[test]
    fn world_albedo_selection_agrees_for_preview_pixels_and_surface_reflectivity() {
        let mut base = fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[255, 0, 0, 255]]);
        let mut albedo = fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[0, 255, 0, 255]]);
        for (texture, rgb) in [
            (&mut base, [0.5_f32, 0.0, 0.0]),
            (&mut albedo, [0.0_f32, 0.25, 0.0]),
        ] {
            for (index, value) in rgb.into_iter().enumerate() {
                texture[32 + index * 4..36 + index * 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        let mut entries = vec![
            ("materials/test/base.vtf", base),
            ("materials/test/albedo.vtf", albedo),
        ];
        let cases = [
            ("materials/test/world.vmt", "LightmappedGeneric", "", true),
            (
                "materials/test/transition.vmt",
                "WorldVertexTransition",
                "",
                true,
            ),
            (
                "materials/test/disabled.vmt",
                "LightmappedGeneric",
                "\"$nodiffusebumplighting\" \"1\"",
                false,
            ),
            (
                "materials/test/world_decal.vmt",
                "LightmappedGeneric_DX9",
                "\"$decal\" \"1\"",
                false,
            ),
            (
                "materials/test/transition_decal.vmt",
                "WorldVertexTransition_DX9",
                "\"$decal\" \"1\"",
                false,
            ),
            (
                "materials/test/world_translucent.vmt",
                "LightmappedGeneric",
                "\"$translucent\" \"1\"",
                true,
            ),
            ("materials/test/model.vmt", "VertexLitGeneric", "", true),
            (
                "materials/test/model_decal.vmt",
                "VertexLitGeneric",
                "\"$decal\" \"1\"",
                true,
            ),
            (
                "materials/test/model_disabled.vmt",
                "VertexLitGeneric",
                "\"$nodiffusebumplighting\" \"1\"",
                true,
            ),
            ("materials/test/unlit.vmt", "UnlitGeneric", "", false),
        ];
        for (path, shader, extra, _) in cases {
            entries.push((path, format!(r#"{shader} {{ "$basetexture" "test/base" "$bumpmap" "test/base" "$albedo" "test/albedo" {extra} }}"#).into_bytes()));
        }
        let resolver = MaterialResolver::prop_lighting_fixture(entries, Vec::new());
        for (path, _, _, selected_albedo) in cases {
            let name = path
                .strip_prefix("materials/")
                .unwrap()
                .strip_suffix(".vmt")
                .unwrap();
            let expected = if selected_albedo {
                [0, 255, 0, 255]
            } else {
                [255, 0, 0, 255]
            };
            let primary = resolver.resolve_primary(&[], name).into_material().unwrap();
            assert_eq!(
                primary.texture.as_ref().unwrap().rgba_bytes().unwrap(),
                expected
            );
            let full = resolver
                .resolve_with_base2(&[], name)
                .into_material()
                .unwrap();
            assert_eq!(
                full.texture.as_ref().unwrap().rgba_bytes().unwrap(),
                expected
            );
            assert_eq!(
                resolver
                    .resolve_surface_lighting(name)
                    .unwrap()
                    .reflectivity,
                if selected_albedo {
                    Vec3::new(0.0, 0.25, 0.0)
                } else {
                    Vec3::new(0.5, 0.0, 0.0)
                }
            );
            assert_eq!(
                primary.shader.surface().unwrap().parameters["$basetexture"],
                "test/base"
            );
            let surface = full.shader.surface().unwrap();
            let excluded = name.ends_with("world_decal") || name.ends_with("transition_decal");
            assert_eq!(surface.normal.is_none(), excluded, "{name}");
            assert_eq!(surface.parameters["$bumpmap"], "test/base");
            assert!(surface.unavailable_textures().is_empty(), "{name}");
        }
    }
}
