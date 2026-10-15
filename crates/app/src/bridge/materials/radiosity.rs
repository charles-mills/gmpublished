use super::{Limits, MaterialResolver, Vec3, material_paths, texture_path};
use gmpublished_domain::scene::map::MapRadiosityMaterialFlags;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedSurfaceLighting {
    pub reflectivity: Vec3,
    pub flags: MapRadiosityMaterialFlags,
}

impl MaterialResolver {
    /// Runtime material reflectivity comes from the loaded base VTF header,
    /// independently of the optional compiler-facing `$reflectivity` parameter.
    pub(crate) fn resolve_surface_lighting(&self, name: &str) -> Option<ResolvedSurfaceLighting> {
        self.find_entry_by_source(&material_paths(&[], name), |path, bytes| {
            let material = self.effective_material(
                &String::from_utf8_lossy(&bytes),
                0,
                &mut vec![path.clone()],
            )?;
            if material.shader.eq_ignore_ascii_case("patch") {
                return None;
            }
            let (reflectivity, base_flags) = if let Some(base) = material.effective_base_texture() {
                let path = texture_path(base)?;
                self.find_entry_by_source(std::slice::from_ref(&path), |_, bytes| {
                    let texture = vformats::vtf::parse(&bytes, &Limits::default()).ok()?;
                    let reflectivity = Vec3::from(texture.reflectivity());
                    reflectivity
                        .is_finite()
                        .then_some((reflectivity, Some(texture.flags())))
                })?
            } else {
                (Vec3::splat(0.2), None)
            };
            let shader =
                self.resolve_shader(&material, material.effective_base_texture().is_some());
            let render_mode = shader.surface().map_or_else(
                || material.render_mode(),
                |surface| surface.draw_render_mode(base_flags, surface.alpha),
            );
            Some(ResolvedSurfaceLighting {
                reflectivity,
                flags: MapRadiosityMaterialFlags {
                    no_draw: super::vmt_bool(material.parameter("$nodraw")),
                    translucent: matches!(
                        render_mode,
                        super::RenderMode::Translucent | super::RenderMode::Additive
                    ),
                },
            })
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
        assert!(resolver.resolve_surface_lighting("test/missing").is_none());
        assert!(resolver.resolve_surface_lighting("test/absent").is_none());
        assert!(resolver.shared.decoded_texture_cache.lock().is_empty());
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
            ("materials/test/model.vmt", "VertexLitGeneric", "", true),
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
            let primary = resolver.resolve_primary(&[], name).unwrap();
            assert_eq!(primary.texture.rgba_bytes().unwrap(), expected);
            let full = resolver.resolve_with_base2(&[], name).unwrap();
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
        }
    }
}
