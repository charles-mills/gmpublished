use super::*;

fn resolver(shader: &str) -> MaterialResolver {
    MaterialResolver::prop_lighting_fixture(
        vec![(
            "materials/test/hidden.vmt",
            format!(
                r#"{shader} {{
                    "$no_Draw" "1" "$basetexture" "test/hidden_base"
                    "$basetexture2" "test/hidden_base2" "$bumpmap" "test/hidden_normal"
                    "$normalmap" "test/hidden_water_normal" "$detail" "test/hidden_detail"
                    "$envmap" "test/hidden_cube" "$translucent" "1" "$additive" "1"
                    "$alpha" "0.25" "$alphatest" "1"
                    "$startexture" "test/hidden_stars" "$starlayers" "1"
                    "$starscale" "1" "$starfade" "1"
                }}"#
            )
            .into_bytes(),
        )],
        Vec::new(),
    )
}

#[test]
fn no_draw_wins_over_blend_flags_without_selecting_or_planning_surface_images() {
    for shader in [
        "VertexLitGeneric",
        "LightmappedGeneric",
        "WorldVertexTransition",
        "UnlitGeneric",
        "DecalModulate",
        "Water",
        "LightmappedReflective",
    ] {
        for on_demand in [false, true] {
            let resolver = resolver(shader);
            let resolver = if on_demand {
                resolver.with_on_demand_textures()
            } else {
                resolver
            };
            let (resolved, plan) = resolver
                .plan_texture_requests(|resolver| resolver.resolve_primary(&[], "test/hidden"));
            let material = resolved
                .material()
                .expect("hidden material remains resolved");
            assert_eq!(resolved.canonical_name(), Some("test/hidden"));
            assert_eq!(material.render_mode, RenderMode::NoDraw, "{shader}");
            assert!(material.texture.is_none());
            assert!(material.texture2.is_none());
            assert_eq!(plan.cost(), TextureRequestCost::default());
            assert!(resolver.selected_texture_paths().is_empty());
            assert!(resolver.selected_reloadable_paths().is_empty());
            assert_eq!(resolver.resident_texture_asset_count(), 0);
            if let Some(surface) = material.shader.surface() {
                assert_eq!(surface.shader, shader);
                assert_eq!(surface.parameters["$no_draw"], "1");
                for alpha in [0.0, 0.5, 1.0] {
                    assert_eq!(
                        surface.draw_render_mode(Some(u32::MAX), alpha),
                        RenderMode::NoDraw
                    );
                }
            } else if shader == "Water" {
                assert!(material.shader.water().is_some());
            } else {
                assert!(material.shader.mirror().is_some());
            }
        }
    }
}

#[test]
fn effective_patch_can_hide_or_restore_a_material_without_using_vrad_nodraw() {
    let resolver = MaterialResolver::prop_lighting_fixture(
        vec![
            (
                "materials/test/base.vmt",
                br#"UnlitGeneric { "$no_draw" "0" "$basetexture" "test/image" }"#.to_vec(),
            ),
            (
                "materials/test/hide.vmt",
                br#"patch { include "materials/test/base.vmt" replace { "$no_draw" "1" } }"#
                    .to_vec(),
            ),
            (
                "materials/test/show.vmt",
                br#"patch { include "materials/test/hide.vmt" replace { "$no_draw" "0" } }"#
                    .to_vec(),
            ),
            (
                "materials/test/vrad.vmt",
                br#"UnlitGeneric { "$nodraw" "1" "$basetexture" "test/image" }"#.to_vec(),
            ),
        ],
        Vec::new(),
    );
    for (name, expected) in [
        ("test/hide", RenderMode::NoDraw),
        ("test/show", RenderMode::Opaque),
        ("test/vrad", RenderMode::Opaque),
    ] {
        let material = resolver.resolve_primary(&[], name).into_material().unwrap();
        assert_eq!(material.render_mode, expected);
    }
}

#[test]
fn hidden_water_preserves_volume_and_overlay_but_does_not_request_surface_textures() {
    let resolver = MaterialResolver::prop_lighting_fixture(vec![
        ("materials/test/water.vmt", br#"Water { "$no_draw" "1" "$normalmap" "test/surface" "$fogcolor" "[0.1 0.2 0.3]" "$fogstart" "12" "$fogend" "345" "$underwateroverlay" "test/overlay" }"#.to_vec()),
        ("materials/test/overlay.vmt", br#"Refract { "$normalmap" "test/volume" }"#.to_vec()),
        ("materials/test/volume.vtf", crate::test_support::fixture_vtf_bytes(1,1,vformats::vtf::VtfFormat::Rgba8888,&[&[128,128,255,255]])),
    ], Vec::new()).with_on_demand_textures();
    let material = resolver
        .resolve_with_base2(&[], "test/water")
        .into_material()
        .unwrap();
    assert_eq!(material.render_mode, RenderMode::NoDraw);
    let water = material.shader.water().unwrap();
    assert_eq!((water.fog_start, water.fog_end), (12.0, 345.0));
    assert_eq!(water.normal_map_path, "test/surface");
    assert!(water.normal_map.is_none());
    assert!(
        water
            .underwater_overlay
            .as_ref()
            .unwrap()
            .normal_map
            .on_demand()
            .is_some()
    );
    assert_eq!(
        resolver.selected_reloadable_paths(),
        ["materials/test/volume.vtf"]
    );
    assert_eq!(resolver.resident_texture_asset_count(), 0);
}

#[test]
fn hidden_sky_skips_face_and_star_sources_without_changing_authored_shader() {
    for shader in ["Sky", "g_sky"] {
        let resolver = resolver(shader);
        let sky = resolver
            .resolve_sky_material_at_path("materials/test/hidden.vmt", None)
            .unwrap();
        assert!(sky.no_draw);
        assert_eq!(sky.shader, shader);
        assert!(sky.texture.is_none());
        assert!(sky.painted.is_none());
        assert!(resolver.selected_texture_paths().is_empty());
    }
}
