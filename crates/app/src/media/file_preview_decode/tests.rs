use gmpublished_domain::math::Vec3;
use gmpublished_domain::scene::map::{
    AmbientCube, MapAmbientLighting, MapDoorGeometry, MapPropVisibility, StaticPropPlacement,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use vformats::vtf::VtfFormat;

use super::*;
use crate::bridge::materials::{DecodedTextureBudget, TextureLoadOutcome};
use crate::bridge::{archive::PreviewArchiveSource, gma::PreviewArchive};
use crate::generation::Generation;
use crate::media::preview_model::{MapStats, ModelVertex};
use crate::test_support::{GmaFixtureBuilder, fixture_vtf_bytes};
use crate::theme::ThemeVariant;

fn test_tokens() -> Tokens {
    Tokens::dark()
}

fn request(path: &str, bytes: &[u8]) -> PreviewRequest {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Fixture")
            .entry(path, bytes.to_vec())
            .build(),
    )
    .expect("fixture archive should load");

    PreviewRequest {
        cancellation: Default::default(),
        request_id: Generation::from_raw(1),
        archive: PreviewArchiveSource::from_gma(Arc::new(archive)),
        entry_path: path.to_owned(),
        display_name: path.rsplit('/').next().unwrap_or(path).to_owned(),
        size_bytes: bytes.len() as u64,
        crc32: 0,
        bypass_size_limits: false,
        map_texture_quality: Default::default(),
    }
}

fn request_from_archive(
    archive: PreviewArchive,
    path: String,
    size_bytes: u64,
    crc32: u32,
) -> PreviewRequest {
    let display_name = path.rsplit('/').next().unwrap_or(&path).to_owned();
    PreviewRequest {
        cancellation: Default::default(),
        request_id: Generation::from_raw(1),
        archive: PreviewArchiveSource::from_gma(Arc::new(archive)),
        entry_path: path,
        display_name,
        size_bytes,
        crc32,
        bypass_size_limits: false,
        map_texture_quality: Default::default(),
    }
}

#[test]
fn classification_table_matches_expected_extensions() {
    let previewable = |path: &str| match classify_entry_path(path) {
        EntryClass::Previewable(class) => Some(class),
        EntryClass::ModelCompanion => None,
    };

    assert_eq!(
        previewable("lua/autorun/init.lua"),
        Some(PreviewClass::Code {
            syntax: CodeSyntax::Glua
        })
    );
    assert_eq!(
        previewable("data/config.JSON"),
        Some(PreviewClass::Code {
            syntax: CodeSyntax::Json
        })
    );
    assert_eq!(
        previewable("materials/icon.vmt"),
        Some(PreviewClass::Code {
            syntax: CodeSyntax::Vmt
        })
    );
    assert_eq!(
        previewable("materials/icon.vtf"),
        Some(PreviewClass::Image(ImageClass::Vtf))
    );
    assert_eq!(
        previewable("models/props_c17/oildrum001.mdl"),
        Some(PreviewClass::Model)
    );
    for companion in [
        "models/props_c17/oildrum001.vvd",
        "models/props_c17/oildrum001.dx90.vtx",
        "models/props_c17/oildrum001.dx80.vtx",
        "models/props_c17/oildrum001.phy",
        "models/props_c17/oildrum001.ani",
    ] {
        assert_eq!(
            classify_entry_path(companion),
            EntryClass::ModelCompanion,
            "{companion} must route through its parent .mdl"
        );
    }
    assert_eq!(
        previewable("maps/gm_construct.bsp"),
        Some(PreviewClass::Map)
    );
    assert_eq!(
        previewable("materials/icon.png"),
        Some(PreviewClass::Image(ImageClass::Encoded))
    );
    assert_eq!(previewable("sound/music.wav"), Some(PreviewClass::Audio));
    assert_eq!(previewable("sound/music.MP3"), Some(PreviewClass::Audio));
    assert_eq!(previewable("sound/music.ogg"), Some(PreviewClass::Audio));
    assert_eq!(
        previewable("resource/fonts/menu.TTF"),
        Some(PreviewClass::Font)
    );
    assert_eq!(
        previewable("models/test/thing.dx90.ctx"),
        Some(PreviewClass::Info)
    );
    assert_eq!(previewable("bin/blob.dat"), Some(PreviewClass::Info));
}

#[test]
fn vmt_highlighting_tracks_shader_keys_values_groups_and_comments() {
    let tokens = Tokens::for_variant(ThemeVariant::ClassicSource);
    let palette = VmtHighlightPalette::from_tokens(&tokens);
    assert_ne!(palette.shader, palette.key);
    let source_lines = vec![
        "// leading comment".to_owned(),
        "Patch".to_owned(),
        "{".to_owned(),
        "    include \"materials/base.vmt\" // trailing".to_owned(),
        "    Insert".to_owned(),
        "    {".to_owned(),
        "        surfaceprop metal".to_owned(),
        "        \"$basetexture\" \"brick/wall01\"".to_owned(),
        "    }".to_owned(),
        "}".to_owned(),
    ];

    let highlighted = vmt_highlighted_lines(&source_lines, &tokens);

    assert_eq!(highlighted.len(), source_lines.len());
    for (line, source) in highlighted.iter().zip(&source_lines) {
        assert_eq!(code_line_text(line), *source);
    }
    assert_eq!(
        exact_span_color(&highlighted[0], "// leading comment"),
        Some(Some(palette.comment))
    );
    assert_eq!(
        exact_span_color(&highlighted[1], "Patch"),
        Some(Some(palette.shader))
    );
    assert_eq!(
        exact_span_color(&highlighted[2], "{"),
        Some(Some(palette.punctuation))
    );
    assert_eq!(
        exact_span_color(&highlighted[3], "include"),
        Some(Some(palette.key))
    );
    assert_eq!(
        exact_span_color(&highlighted[3], "\"materials/base.vmt\""),
        Some(Some(palette.value))
    );
    assert_eq!(
        exact_span_color(&highlighted[3], "// trailing"),
        Some(Some(palette.comment))
    );
    assert_eq!(
        exact_span_color(&highlighted[4], "Insert"),
        Some(Some(palette.group))
    );
    assert_eq!(
        exact_span_color(&highlighted[6], "surfaceprop"),
        Some(Some(palette.key))
    );
    assert_eq!(
        exact_span_color(&highlighted[6], "metal"),
        Some(Some(palette.value))
    );
    assert_eq!(
        exact_span_color(&highlighted[7], "\"$basetexture\""),
        Some(Some(palette.key))
    );
    assert_eq!(
        exact_span_color(&highlighted[7], "\"brick/wall01\""),
        Some(Some(palette.value))
    );
}

#[test]
fn vmt_highlighting_keeps_unterminated_quotes_line_local() {
    let tokens = test_tokens();
    let palette = VmtHighlightPalette::from_tokens(&tokens);
    let source_lines = vec![
        "VertexLitGeneric".to_owned(),
        "{".to_owned(),
        "    $basetexture \"unfinished".to_owned(),
        "    surfaceprop metal".to_owned(),
    ];

    let highlighted = vmt_highlighted_lines(&source_lines, &tokens);

    assert_eq!(
        exact_span_color(&highlighted[2], "$basetexture"),
        Some(Some(palette.key))
    );
    assert_eq!(
        exact_span_color(&highlighted[2], "\"unfinished"),
        Some(Some(palette.value))
    );
    assert_eq!(
        exact_span_color(&highlighted[3], "surfaceprop"),
        Some(Some(palette.key))
    );
    assert_eq!(
        exact_span_color(&highlighted[3], "metal"),
        Some(Some(palette.value))
    );
}

#[test]
fn glua_highlighting_tracks_multiline_constructs_and_language_tokens() {
    let tokens = Tokens::for_variant(ThemeVariant::ClassicSource);
    let palette = CodeHighlightPalette::from_tokens(&tokens);
    let source_lines = vec![
        "local answer = 0x2A + 1.5e2".to_owned(),
        "local text = [=[hello".to_owned(),
        "world]=] .. \"done -- still string\" -- trailing".to_owned(),
        "--[==[block".to_owned(),
        "not closed ]=]".to_owned(),
        "closed]==] return true, nil, false".to_owned(),
        "function render(self, café) end".to_owned(),
    ];

    let highlighted = glua_highlighted_lines(&source_lines, &tokens);

    assert_eq!(highlighted.len(), source_lines.len());
    for (line, source) in highlighted.iter().zip(&source_lines) {
        assert_eq!(code_line_text(line), *source);
    }
    assert_eq!(
        exact_span_color(&highlighted[0], "local"),
        Some(Some(palette.keyword))
    );
    assert_eq!(
        exact_span_color(&highlighted[0], "0x2A"),
        Some(Some(palette.number))
    );
    assert_eq!(
        exact_span_color(&highlighted[0], "1.5e2"),
        Some(Some(palette.number))
    );
    assert_eq!(
        exact_span_color(&highlighted[1], "[=[hello"),
        Some(Some(palette.string))
    );
    assert_eq!(
        exact_span_color(&highlighted[2], "world]=]"),
        Some(Some(palette.string))
    );
    assert_eq!(
        exact_span_color(&highlighted[2], "-- trailing"),
        Some(Some(palette.comment))
    );
    assert_eq!(
        exact_span_color(&highlighted[4], "not closed ]=]"),
        Some(Some(palette.comment))
    );
    assert_eq!(
        exact_span_color(&highlighted[5], "return"),
        Some(Some(palette.keyword))
    );
    assert_eq!(
        exact_span_color(&highlighted[5], "true"),
        Some(Some(palette.number))
    );
    assert_eq!(
        exact_span_color(&highlighted[6], "render"),
        Some(Some(palette.function))
    );
}

#[test]
fn json_highlighting_preserves_utf8_numbers_literals_and_line_local_strings() {
    let tokens = Tokens::for_variant(ThemeVariant::ClassicSource);
    let palette = CodeHighlightPalette::from_tokens(&tokens);
    let source_lines = vec![
        "{\"title\":\"café\",\"count\":-12.5e+2,\"enabled\":true,\"missing\":null}".to_owned(),
        "{\"broken\":\"value".to_owned(),
        "{\"next\":false}".to_owned(),
    ];

    let highlighted = json_highlighted_lines(&source_lines, &tokens);

    for (line, source) in highlighted.iter().zip(&source_lines) {
        assert_eq!(code_line_text(line), *source);
    }
    assert_eq!(
        exact_span_color(&highlighted[0], "title"),
        Some(Some(palette.string))
    );
    assert_eq!(
        exact_span_color(&highlighted[0], "café"),
        Some(Some(palette.string))
    );
    assert_eq!(
        exact_span_color(&highlighted[0], "-12.5e+2"),
        Some(Some(palette.number))
    );
    assert_eq!(
        exact_span_color(&highlighted[0], "true"),
        Some(Some(palette.number))
    );
    assert_eq!(
        exact_span_color(&highlighted[0], "null"),
        Some(Some(palette.number))
    );
    assert_eq!(
        exact_span_color(&highlighted[1], "value"),
        Some(Some(palette.string))
    );
    assert_eq!(
        exact_span_color(&highlighted[2], "false"),
        Some(Some(palette.number))
    );
}

fn code_line_text(line: &CodeLine) -> String {
    line.iter()
        .map(|span| span.text.as_str())
        .collect::<String>()
}

fn exact_span_color(line: &CodeLine, text: &str) -> Option<Option<[u8; 4]>> {
    line.iter()
        .find(|span| span.text == text)
        .map(|span| span.color)
}

#[test]
fn model_companion_parent_path_strips_model_sidecar_suffixes() {
    assert_eq!(
        model_companion_parent_path("models/test/thing.vvd").as_deref(),
        Some("models/test/thing.mdl")
    );
    assert_eq!(
        model_companion_parent_path("models/test/thing.dx90.vtx").as_deref(),
        Some("models/test/thing.mdl")
    );
    assert_eq!(
        model_companion_parent_path("models/test/thing.dx80.vtx").as_deref(),
        Some("models/test/thing.mdl")
    );
    assert_eq!(
        model_companion_parent_path("models/test/thing.vtx").as_deref(),
        Some("models/test/thing.mdl")
    );
    assert_eq!(
        model_companion_parent_path("models/test/thing.PHY").as_deref(),
        Some("models/test/thing.mdl")
    );
    assert_eq!(model_companion_parent_path("models/test/thing.ctx"), None);
}

#[test]
fn model_companion_request_redirects_to_parent_mdl_entry() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Model Fixture")
            .entry("Models/Test/Thing.MDL", b"mdl bytes".to_vec())
            .entry("Models/Test/Thing.VVD", b"vvd bytes".to_vec())
            .build(),
    )
    .expect("fixture archive should load");
    let parent = archive
        .entry("Models/Test/Thing.MDL")
        .expect("parent model entry")
        .clone();
    let request = request_from_archive(archive, "Models/Test/Thing.VVD".to_owned(), 9, 123);

    let redirected = model_companion_preview_request(&request).expect("companion should redirect");

    assert_eq!(redirected.request_id, request.request_id);
    assert_eq!(redirected.entry_path, parent.path.as_str());
    assert_eq!(redirected.display_name, "Thing.MDL");
    assert_eq!(redirected.size_bytes, parent.size);
    assert_eq!(redirected.crc32, parent.crc32);
}

#[test]
fn model_companion_load_previews_the_parent_entry() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Model Fixture")
            .entry("Models/Test/Thing.MDL", b"mdl bytes".to_vec())
            .entry("Models/Test/Thing.VVD", b"vvd bytes".to_vec())
            .build(),
    )
    .expect("fixture archive should load");
    let request = request_from_archive(archive, "Models/Test/Thing.VVD".to_owned(), 9, 123);
    let data = load_preview_data(request, &test_tokens(), None, None, &mut |_stage| {})
        .expect("companion load should resolve through the parent");

    assert_eq!(data.entry_path, "Models/Test/Thing.MDL");
    assert_eq!(data.display_name, "Thing.MDL");
}

#[test]
fn model_companion_load_without_parent_reports_the_companion_itself() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Model Fixture")
            .entry("models/test/orphan.dx80.vtx", b"vtx bytes".to_vec())
            .build(),
    )
    .expect("fixture archive should load");
    let request = request_from_archive(archive, "models/test/orphan.dx80.vtx".to_owned(), 9, 123);
    let data = load_preview_data(request, &test_tokens(), None, None, &mut |_stage| {})
        .expect("orphaned companion should still produce an info preview");

    assert_eq!(data.entry_path, "models/test/orphan.dx80.vtx");
    assert!(matches!(
        data.content,
        PreviewContent::Info {
            reason: InfoReason::DecodeFailed,
            ..
        }
    ));
}

#[test]
fn model_companion_request_without_parent_is_not_redirected() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Model Fixture")
            .entry("models/test/missing_parent.dx80.vtx", b"vtx bytes".to_vec())
            .build(),
    )
    .expect("fixture archive should load");
    let request = request_from_archive(
        archive,
        "models/test/missing_parent.dx80.vtx".to_owned(),
        9,
        123,
    );

    assert!(model_companion_preview_request(&request).is_none());
}

#[test]
fn vtf_preview_links_to_same_stem_vmt_when_present() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Material Fixture")
            .entry("materials/test/thing.vtf", b"not a real vtf".to_vec())
            .entry(
                "materials/test/thing.vmt",
                br#""VertexlitGeneric" { "$basetexture" "test/thing" }"#.to_vec(),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let request = request_from_archive(archive, "materials/test/thing.vtf".to_owned(), 14, 123);

    let target =
        related_preview_target(&request, b"not a real vtf").expect("vtf should link to vmt");

    assert_eq!(
        target,
        RelatedPreviewTarget {
            entry_path: "materials/test/thing.vmt".to_owned(),
            kind: RelatedPreviewKind::Material,
        }
    );
}

#[test]
fn vmt_preview_links_to_primary_basetexture_when_present() {
    let vmt_bytes = br#""VertexlitGeneric" { "$basetexture" "models/test/thing_color" }"#;
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Material Fixture")
            .entry("materials/models/test/thing.vmt", vmt_bytes.to_vec())
            .entry(
                "materials/models/test/thing_color.vtf",
                b"vtf bytes".to_vec(),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let request = request_from_archive(
        archive,
        "materials/models/test/thing.vmt".to_owned(),
        vmt_bytes.len() as u64,
        123,
    );

    let data = preview_data_from_bytes(&request, vmt_bytes, &test_tokens(), None);

    assert_eq!(
        data.related_preview,
        Some(RelatedPreviewTarget {
            entry_path: "materials/models/test/thing_color.vtf".to_owned(),
            kind: RelatedPreviewKind::Texture,
        })
    );
}

#[test]
fn vmt_preview_without_resolved_basetexture_has_no_related_target() {
    let vmt_bytes = br#""VertexlitGeneric" { "$basetexture" "models/test/missing" }"#;
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Material Fixture")
            .entry("materials/models/test/thing.vmt", vmt_bytes.to_vec())
            .build(),
    )
    .expect("fixture archive should load");
    let request = request_from_archive(
        archive,
        "materials/models/test/thing.vmt".to_owned(),
        vmt_bytes.len() as u64,
        123,
    );

    assert!(related_preview_target(&request, vmt_bytes).is_none());
}

#[test]
fn audio_preview_probes_tiny_wav_duration() {
    let bytes = tiny_wav_bytes();
    let request = request("sound/ui/tone.wav", &bytes);

    let data = preview_data_from_bytes(&request, &bytes, &test_tokens(), None);

    assert!(matches!(
        data.content,
        PreviewContent::Audio {
            duration_secs: Some(duration),
            ..
        } if (duration - 0.1).abs() < 0.01
    ));
}

#[test]
fn audio_decode_failure_becomes_info() {
    let bytes = b"not audio";
    let request = request("sound/broken.ogg", bytes);

    let data = preview_data_from_bytes(&request, bytes, &test_tokens(), None);

    assert!(matches!(
        data.content,
        PreviewContent::Info {
            reason: InfoReason::DecodeFailed
        }
    ));
}

#[test]
fn text_preview_truncates_large_line_sets() {
    let lines = (0..MAX_PREVIEW_LINES + 8)
        .map(|index| format!("print({index})"))
        .collect::<Vec<_>>()
        .join("\n");
    let request = request("lua/autorun/many.lua", lines.as_bytes());

    let data = preview_data_from_bytes(&request, lines.as_bytes(), &test_tokens(), None);

    assert!(matches!(
        data.content,
        PreviewContent::Code {
            ref lines,
            truncated: true,
        } if lines.len() == MAX_PREVIEW_LINES
    ));
}

#[test]
fn text_preview_over_hard_cap_becomes_too_large_info() {
    let bytes = vec![b'a'; TEXT_TOO_LARGE_BYTES + 1];
    let request = request("lua/autorun/huge.lua", &bytes);

    let data = preview_data_from_bytes(&request, &bytes, &test_tokens(), None);

    assert!(matches!(
        data.content,
        PreviewContent::Info {
            reason: InfoReason::TooLarge
        }
    ));

    let mut request = request;
    request.bypass_size_limits = true;
    let data = preview_data_from_bytes(&request, &bytes, &test_tokens(), None);
    assert!(matches!(
        data.content,
        PreviewContent::Code {
            truncated: true,
            ..
        }
    ));
}

#[test]
fn ttf_preview_renders_the_font_family() {
    let bytes = crate::assets::fonts::inter_regular_bytes();
    let request = request("resource/fonts/inter.ttf", bytes);

    let data = preview_data_from_bytes(&request, bytes, &test_tokens(), None);

    assert!(matches!(
        data.content,
        PreviewContent::Font {
            ref family,
            ..
        } if family == "Inter"
    ));
}

#[test]
fn invalid_ttf_becomes_decode_failed_info() {
    let bytes = b"not a font";
    let request = request("resource/fonts/broken.ttf", bytes);

    let data = preview_data_from_bytes(&request, bytes, &test_tokens(), None);

    assert!(matches!(
        data.content,
        PreviewContent::Info {
            reason: InfoReason::DecodeFailed
        }
    ));
}

#[test]
fn oversized_ttf_requires_load_anyway() {
    let bytes = vec![0; FONT_TOO_LARGE_BYTES + 1];
    let request = request("resource/fonts/huge.ttf", &bytes);

    let data = preview_data_from_bytes(&request, &bytes, &test_tokens(), None);

    assert!(matches!(
        data.content,
        PreviewContent::Info {
            reason: InfoReason::TooLarge
        }
    ));
}

#[test]
fn map_uv_normalization_divides_raw_texels_by_resolved_dimensions() {
    assert_eq!(normalize_map_uv(1024.0, 256.0, 512, 128), [2.0, 2.0]);
    assert_eq!(normalize_map_uv(128.0, 64.0, 0, 0), [128.0, 64.0]);
}

#[test]
fn brush_uvs_use_effective_mapping_dimensions_with_compiled_fallback() {
    use gmpublished_domain::scene::map::{MapMesh, MapMeshVisibility, MapVertex};
    let mesh = MapMesh {
        entity: false,
        vertices: vec![MapVertex {
            position: Vec3::ZERO,
            base_position: Vec3::ZERO,
            tangent_s: Vec3::ZERO,
            tangent_t: Vec3::ZERO,
            normal: Vec3::new(0.0, 0.0, 1.0),
            tex_s: 256.0,
            tex_t: 128.0,
            texture_size: [128, 64],
            lightmap_uv: [0.0; 2],
            lightmap_page: None,
            lightmap_bump_step: 0.0,
            blend_alpha: 0.0,
            opacity: 1.0,
        }],
        indices: vec![0],
        material_index: 0,
        visibility: MapMeshVisibility::default(),
        surface_owners: Vec::new(),
    };
    let converted = map_preview::map_mesh_to_model_mesh(&mesh, &[]);
    assert_eq!(converted.vertices[0].uv, [2.0, 2.0]);
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Water UV")
            .entry("materials/water.vmt", b"Water { }".to_vec())
            .build(),
    )
    .unwrap();
    let resolved = MaterialResolver::new(Arc::new(archive), None)
        .resolve_primary(&[], "water")
        .into_material()
        .unwrap();
    assert_eq!(
        [
            resolved.texture.as_ref().unwrap().width(),
            resolved.texture.as_ref().unwrap().height()
        ],
        [1, 1]
    );
    let material = MaterialSlot {
        resolution: Default::default(),
        canonical_name: None,
        name: "water".into(),
        shader: resolved.shader,
        texture: resolved.texture,
        texture2: None,
        render_mode: RenderMode::Opaque,
        alpha_policy: RenderMode::Opaque.texture_alpha_policy(),
    };
    assert_eq!(
        map_preview::map_mesh_to_model_mesh(&mesh, &[material]).vertices[0].uv,
        [8.0, 4.0]
    );

    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Runtime mapping dimensions")
            .entry(
                "materials/replacement.vmt",
                b"LightmappedGeneric { $basetexture replacement }".to_vec(),
            )
            .entry(
                "materials/replacement.vtf",
                fixture_vtf_bytes(64, 128, VtfFormat::Rgba8888, &[&vec![255; 64 * 128 * 4]]),
            )
            .entry(
                "materials/invalid.vmt",
                b"LightmappedGeneric { $basetexture invalid }".to_vec(),
            )
            .entry("materials/invalid.vtf", b"invalid".to_vec())
            .build(),
    )
    .unwrap();
    let resolver = MaterialResolver::new(Arc::new(archive), None);
    for (name, expected) in [("replacement", [4.0, 1.0]), ("invalid", [8.0, 4.0])] {
        let resolved = resolver.resolve_primary(&[], name).into_material().unwrap();
        let material = MaterialSlot {
            resolution: Default::default(),
            canonical_name: None,
            name: name.into(),
            shader: resolved.shader,
            texture: resolved.texture,
            texture2: None,
            render_mode: RenderMode::Opaque,
            alpha_policy: RenderMode::Opaque.texture_alpha_policy(),
        };
        assert_eq!(
            map_preview::map_mesh_to_model_mesh(&mesh, &[material]).vertices[0].uv,
            expected
        );
    }
}

#[test]
fn skybox_face_paths_follow_source_suffixes() {
    assert_eq!(
        SkyboxFace::ALL.map(|face| skybox_face_material_path("sky_day01_01", face)),
        [
            "materials/skybox/sky_day01_01rt.vmt",
            "materials/skybox/sky_day01_01lf.vmt",
            "materials/skybox/sky_day01_01bk.vmt",
            "materials/skybox/sky_day01_01ft.vmt",
            "materials/skybox/sky_day01_01up.vmt",
            "materials/skybox/sky_day01_01dn.vmt",
        ]
    );
}

#[test]
fn skybox_resolution_degrades_to_available_faces() {
    let rgba = vec![
        80, 120, 200, 255, 80, 120, 200, 255, 80, 120, 200, 255, 80, 120, 200, 255,
    ];
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Skybox Fixture")
            .entry(
                "materials/skybox/sky_testft.vmt",
                br#"UnlitGeneric { "$basetexture" "skybox/sky_testft" }"#.to_vec(),
            )
            .entry("materials/skybox/sky_testft.vtf", create_vtf_bytes(&rgba))
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let skybox = resolve_skybox("sky_test", &resolver, None).expect("one face should resolve");

    let face = skybox.faces[SkyboxFace::Ft.index()]
        .as_ref()
        .expect("front face");
    assert_eq!(face.mip_level_count(), 1);
    for face in [
        SkyboxFace::Rt,
        SkyboxFace::Lf,
        SkyboxFace::Bk,
        SkyboxFace::Up,
        SkyboxFace::Dn,
    ] {
        assert!(skybox.faces[face.index()].is_none(), "{face:?}");
    }
    assert!(resolve_skybox("sky_missing", &resolver, None).is_none());
}

#[test]
fn painted_sky_retains_authored_state_and_loads_cloud_vtf_without_a_material() {
    let pixels = [32, 96, 160, 255].repeat(4);
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Painted Sky Fixture")
            .entry(
                "materials/skybox/paintedup.vmt",
                br"g_sky { Proxies { SkyPaint {} } }".to_vec(),
            )
            .entry("materials/skybox/clouds.vtf", create_vtf_bytes(&pixels))
            .build(),
    )
    .unwrap();
    let resolver = MaterialResolver::new(Arc::new(archive), None);
    let paint = gmpublished_domain::scene::map::MapSkyPaint {
        top_color: Vec3::new(0.12, 0.34, 0.56),
        fade_bias: 0.1,
        star_texture: "skybox/clouds".into(),
        star_scale: 2.0,
        star_speed: 0.03,
        ..Default::default()
    };
    let skybox = resolve_skybox("painted", &resolver, Some(&paint)).unwrap();
    assert_eq!(skybox.paint.as_ref(), Some(&paint));
    assert!(skybox.faces.iter().all(Option::is_none));
    assert!(
        skybox.materials[SkyboxFace::Up.index()]
            .as_ref()
            .unwrap()
            .is_painted()
    );
    let clouds = skybox.materials[SkyboxFace::Up.index()]
        .as_ref()
        .unwrap()
        .painted
        .as_ref()
        .unwrap()
        .texture
        .as_ref()
        .unwrap();
    assert_eq!(clouds.frame(0).unwrap()[0].bytes.as_slice(), pixels);
}

#[test]
fn static_sky_preserves_patched_material_transform_and_linear_tint() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Static Sky Fixture")
            .entry("materials/skybox/parent.vmt", br#"Sky { "$basetexture" "skybox/face" "$color" "[0.25 0.5 0.75]" }"#.to_vec())
            .entry("materials/skybox/patchedup.vmt", br#"Patch { "include" "materials/skybox/parent.vmt" "insert" { "$basetexturetransform" "center 0 0 scale 1 2 rotate 0 translate 0 0" } }"#.to_vec())
            .entry("materials/skybox/face.vtf", create_vtf_bytes(&[128; 16]))
            .build(),
    )
    .unwrap();
    let resolver = MaterialResolver::new(Arc::new(archive), None);
    let skybox = resolve_skybox("patched", &resolver, None).unwrap();
    let material = skybox.materials[SkyboxFace::Up.index()].as_ref().unwrap();
    assert_eq!(material.shader, "Sky");
    assert_eq!(material.color, Vec3::new(0.25, 0.5, 0.75));
    assert_eq!(
        material.transform,
        [[1.0, 0.0, 0.0, 0.0], [0.0, 2.0, 0.0, 0.0]]
    );
    assert!(skybox.paint.is_none());
    assert!(material.painted.is_none());
}

#[test]
fn painted_material_defaults_and_parameters_are_independent_of_lua_controller_defaults() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Painted Material State")
            .entry(
                "materials/skybox/independentup.vmt",
                br#"g_sky {
                "$topcolor" "[0.25 0.5 0.75]" "$bottomcolor" "[0.1 0.2 0.3]"
                "$fadebias" "0.5" "$startexture" "skybox/stars"
                "$starlayers" "2" "$starscale" "-2" "$starfade" "0.5" "$starpos" "0.75"
            }"#
                .to_vec(),
            )
            .entry(
                "materials/skybox/controlledft.vmt",
                br"g_sky { Proxies { SkyPaint {} } }".to_vec(),
            )
            .entry("materials/skybox/stars.vtf", create_vtf_bytes(&[128; 16]))
            .build(),
    )
    .unwrap();
    let resolver = MaterialResolver::new(Arc::new(archive), None);
    let controller = gmpublished_domain::scene::map::MapSkyPaint::default();
    let independent = resolve_skybox("independent", &resolver, Some(&controller)).unwrap();
    let paint = independent.materials[SkyboxFace::Up.index()]
        .as_ref()
        .unwrap()
        .painted
        .as_ref()
        .unwrap();
    assert_eq!(paint.top_color, Vec3::new(0.25, 0.5, 0.75));
    assert_eq!(paint.star_scale, -2.0);
    assert_eq!(paint.star_fade, 0.5);
    assert_eq!(paint.star_position, 0.75);
    assert_eq!(paint.star_speed, 0.0);
    assert!(paint.texture.is_some());

    let no_controller = resolve_skybox("controlled", &resolver, None).unwrap();
    assert!(no_controller.paint.is_none());
    let paint = no_controller.materials[SkyboxFace::Ft.index()]
        .as_ref()
        .unwrap()
        .painted
        .as_ref()
        .unwrap();
    assert_eq!(paint.top_color, Vec3::ZERO);
    assert_eq!(paint.bottom_color, Vec3::ZERO);
    assert_eq!(paint.star_layers, 0);
    assert!(paint.texture.is_none());

    let controlled = resolve_skybox("controlled", &resolver, Some(&controller)).unwrap();
    let paint = controlled.materials[SkyboxFace::Ft.index()]
        .as_ref()
        .unwrap()
        .painted
        .as_ref()
        .unwrap();
    assert_eq!(paint.top_color, controller.top_color);
    assert_eq!(paint.star_scale, controller.client_star_scale());
    assert_eq!(paint.star_speed, controller.client_star_speed());
}

#[test]
fn decoded_texture_budget_counts_deduped_bytes_once() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Map Texture Budget Fixture")
            .entry(
                "materials/test/first.vmt",
                br#""LightmappedGeneric" { "$basetexture" "test/shared" }"#.to_vec(),
            )
            .entry(
                "materials/test/second.vmt",
                br#""LightmappedGeneric" { "$basetexture" "test/shared" }"#.to_vec(),
            )
            .entry(
                "materials/test/shared.vtf",
                fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[64; 4], &[64; 16]]),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let budget = Arc::new(DecodedTextureBudget::new(20));
    let resolver = MaterialResolver::new(Arc::new(archive), None)
        .with_decoded_texture_budget(Arc::clone(&budget));

    assert!(resolver.resolve(&[], "test/first").is_some());
    assert!(resolver.resolve(&[], "test/second").is_some());

    assert_eq!(budget.decoded_bytes(), 20);
    assert_eq!(budget.rejected_textures(), 0);
}

#[test]
fn material_slots_preserve_missing_identity_and_canonical_error_state() {
    use crate::bridge::materials::{MaterialResolutionOutcome, SurfaceFamily};
    let resolver = MaterialResolver::prop_lighting_fixture(
        vec![
            (
                "materials/test/lit.vmt",
                br#"LightmappedGeneric { "$basetexture" "test/absent" }"#.to_vec(),
            ),
            ("materials/test/malformed.vmt", Vec::new()),
        ],
        Vec::new(),
    );
    let names = vec![
        "test/lit".to_owned(),
        "TEST/MISSING_A.VMT".to_owned(),
        "test/missing_b".to_owned(),
        "test/malformed".to_owned(),
    ];
    let table = resolve_map_material_slots_serial(&names, &resolver);
    assert_eq!(table.resolved_count(), 1);
    assert_eq!(
        table.slots()[0].resolution,
        MaterialResolutionOutcome::Loaded
    );
    for index in [1, 2] {
        let slot = &table.slots()[index];
        assert_eq!(slot.name, names[index]);
        assert_eq!(slot.resolution, MaterialResolutionOutcome::Missing);
        assert_eq!(slot.canonical_name.as_deref(), Some("___error"));
        assert_eq!(slot.shader.surface().unwrap().family, SurfaceFamily::Unlit);
        assert_eq!(
            slot.texture.as_ref().unwrap().original_dimensions(),
            (32, 32)
        );
    }
    assert!(Arc::ptr_eq(
        table.slots()[1].texture.as_ref().unwrap(),
        table.slots()[2].texture.as_ref().unwrap()
    ));
    assert_eq!(
        table.slots()[3].resolution,
        MaterialResolutionOutcome::Malformed
    );
    assert!(table.slots()[3].canonical_name.is_none());
    assert!(table.slots()[3].texture.is_none());
    assert!(
        unresolved_material_names_for_debug(table.slots())
            .0
            .contains(&names[1])
    );
}

#[test]
fn decoded_texture_budget_counts_full_mip_chain_bytes() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Map Texture Budget Fixture")
            .entry(
                "materials/test/first.vmt",
                br#""LightmappedGeneric" { "$basetexture" "test/first" }"#.to_vec(),
            )
            .entry(
                "materials/test/first.vtf",
                fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[10; 4], &[10; 16]]),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let budget = Arc::new(DecodedTextureBudget::new(16));
    let resolver = MaterialResolver::new(Arc::new(archive), None)
        .with_decoded_texture_budget(Arc::clone(&budget));

    assert_eq!(
        resolver.resolve(&[], "test/first").unwrap().load_outcome(),
        TextureLoadOutcome::QualityRejected
    );

    assert_eq!(budget.decoded_bytes(), 0);
    assert_eq!(budget.rejected_textures(), 1);
    let table = resolve_map_material_slots_serial(&["test/first".to_owned()], &resolver);
    assert_eq!(table.resolved_count(), 0);
    assert!(!table.slots()[0].has_resolved_base());
}

#[test]
fn decoded_texture_budget_stops_and_counts_rejected_textures() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Map Texture Budget Fixture")
            .entry(
                "materials/test/first.vmt",
                br#""LightmappedGeneric" { "$basetexture" "test/first" }"#.to_vec(),
            )
            .entry(
                "materials/test/second.vmt",
                br#""LightmappedGeneric" { "$basetexture" "test/second" }"#.to_vec(),
            )
            .entry(
                "materials/test/third.vmt",
                br#""LightmappedGeneric" { "$basetexture" "test/third" }"#.to_vec(),
            )
            .entry(
                "materials/test/first.vtf",
                fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[10; 4], &[10; 16]]),
            )
            .entry(
                "materials/test/second.vtf",
                fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[20; 4], &[20; 16]]),
            )
            .entry(
                "materials/test/third.vtf",
                fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[30; 4], &[30; 16]]),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let budget = Arc::new(DecodedTextureBudget::new(20));
    let resolver = MaterialResolver::new(Arc::new(archive), None)
        .with_decoded_texture_budget(Arc::clone(&budget));

    assert!(resolver.resolve(&[], "test/first").is_some());
    assert_eq!(
        resolver.resolve(&[], "test/second").unwrap().load_outcome(),
        TextureLoadOutcome::QualityRejected
    );
    assert_eq!(
        resolver.resolve(&[], "test/third").unwrap().load_outcome(),
        TextureLoadOutcome::QualityRejected
    );

    assert_eq!(budget.decoded_bytes(), 20);
    assert_eq!(budget.rejected_textures(), 2);
}

#[test]
fn parallel_material_resolution_matches_serial_slots_in_order() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Parallel Material Fixture")
            .entry(
                "materials/test/first.vmt",
                br#""LightmappedGeneric" { "$basetexture" "test/first" }"#.to_vec(),
            )
            .entry(
                "materials/test/blend.vmt",
                br#"
                WorldVertexTransition
                {
                    "$basetexture" "test/base"
                    "$basetexture2" "test/base2"
                }
                "#
                .to_vec(),
            )
            .entry(
                "materials/test/water.vmt",
                br#"Water { "$fogcolor" "[0.05 0.08 0.07]" }"#.to_vec(),
            )
            .entry("materials/test/first.vtf", create_vtf_bytes(&[10; 16]))
            .entry("materials/test/base.vtf", create_vtf_bytes(&[20; 16]))
            .entry("materials/test/base2.vtf", create_vtf_bytes(&[30; 16]))
            .build(),
    )
    .expect("fixture archive should load");
    let names = vec![
        "test/first".to_owned(),
        "test/blend".to_owned(),
        "test/missing".to_owned(),
        "test/water".to_owned(),
    ];
    let serial_resolver = MaterialResolver::new(Arc::new(archive.clone()), None);
    let parallel_resolver = MaterialResolver::new(Arc::new(archive), None);

    let serial = resolve_map_material_slots_serial(&names, &serial_resolver);
    let parallel = resolve_map_material_slots_parallel(&names, &parallel_resolver);

    assert_eq!(
        material_slot_signatures(parallel.slots()),
        material_slot_signatures(serial.slots())
    );
    assert_eq!(parallel.resolved_count(), serial.resolved_count());
    assert_eq!(
        parallel.water_material_count(),
        serial.water_material_count()
    );
}

#[test]
fn parallel_material_resolution_respects_atomic_texture_budget() {
    let mut builder = GmaFixtureBuilder::new("Parallel Budget Fixture");
    for index in 0..4 {
        builder = builder
            .entry(
                format!("materials/test/{index}.vmt"),
                format!(r#""LightmappedGeneric" {{ "$basetexture" "test/{index}" }}"#).into_bytes(),
            )
            .entry(
                format!("materials/test/{index}.vtf"),
                fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[&[index; 4], &[index; 16]]),
            );
    }
    let archive = PreviewArchive::from_gma(builder.build()).expect("fixture archive should load");
    let budget = Arc::new(DecodedTextureBudget::new(40));
    let resolver = MaterialResolver::new(Arc::new(archive), None)
        .with_decoded_texture_budget(Arc::clone(&budget));
    let names = (0..4)
        .map(|index| format!("test/{index}"))
        .collect::<Vec<_>>();

    let resolved = resolve_map_material_slots_parallel(&names, &resolver);

    assert!(budget.decoded_bytes() <= 40);
    assert_eq!(
        resolved
            .slots()
            .iter()
            .filter(|slot| slot
                .texture
                .as_ref()
                .is_some_and(|texture| texture.load_outcome() == TextureLoadOutcome::Loaded))
            .count(),
        2
    );
    assert_eq!(
        resolved
            .slots()
            .iter()
            .filter(|slot| slot.texture.as_ref().is_some_and(
                |texture| texture.load_outcome() == TextureLoadOutcome::QualityRejected
            ))
            .count(),
        2
    );
    assert!(resolved.slots().iter().all(|slot| {
        slot.texture
            .as_ref()
            .is_some_and(|texture| texture.has_effective_base_texture())
    }));
    assert_eq!(budget.rejected_textures(), 2);
}

#[test]
fn prop_transform_applies_source_yaw_around_z_before_translation() {
    let placement = prop_placement(
        "models/test/chair.mdl",
        Vec3::new(10.0, 20.0, 30.0),
        Vec3::new(0.0, 90.0, 0.0),
        0,
    );

    assert_vec3_close(
        transform_prop_position(Vec3::new(1.0, 0.0, 0.0), &placement),
        Vec3::new(10.0, 21.0, 30.0),
    );
    assert_vec3_close(
        transform_prop_normal(Vec3::new(1.0, 0.0, 0.0), &placement),
        Vec3::new(0.0, 1.0, 0.0),
    );
}

#[test]
fn prop_transform_scales_positions_before_rotation_without_scaling_normals() {
    let mut placement = prop_placement(
        "models/test/chair.mdl",
        Vec3::new(10.0, 20.0, 30.0),
        Vec3::new(0.0, 90.0, 0.0),
        0,
    );
    placement.scale = 2.0;

    assert_vec3_close(
        transform_prop_position(Vec3::new(1.0, 0.0, 0.0), &placement),
        Vec3::new(10.0, 22.0, 30.0),
    );
    assert_vec3_close(
        transform_prop_normal(Vec3::new(1.0, 0.0, 0.0), &placement),
        Vec3::new(0.0, 1.0, 0.0),
    );
}

#[test]
fn prop_transform_composes_roll_before_yaw() {
    let placement = prop_placement(
        "models/test/chair.mdl",
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(0.0, 90.0, 90.0),
        0,
    );

    assert_vec3_close(
        transform_prop_normal(Vec3::new(1.0, 0.0, 0.0), &placement),
        Vec3::new(0.0, 1.0, 0.0),
    );
}

#[test]
fn prop_lighting_adds_visible_sun_in_linear_space() {
    let lighting = LegacyPropLightingFallback {
        ambient_cube: test_ambient_cube(Vec3::new(0.1, 0.1, 0.1)),
        sun: Some(PropSunLighting {
            direction_to_sun: Vec3::new(0.0, 0.0, 1.0),
            color_linear: Vec3::new(0.5, 0.25, 0.0),
            visible: true,
        }),
    };

    assert_vec3_close(
        lighting.evaluate(Vec3::new(0.0, 0.0, 1.0)),
        Vec3::new(0.6, 0.35, 0.1),
    );
    assert_vec3_close(
        lighting.evaluate(Vec3::new(0.0, 0.0, -1.0)),
        Vec3::new(0.1, 0.1, 0.1),
    );
}

#[test]
fn prop_lighting_keeps_shadowed_vertices_ambient_only() {
    let lighting = LegacyPropLightingFallback {
        ambient_cube: test_ambient_cube(Vec3::new(0.25, 0.2, 0.15)),
        sun: Some(PropSunLighting {
            direction_to_sun: Vec3::new(0.0, 0.0, 1.0),
            color_linear: Vec3::new(1.0, 1.0, 1.0),
            visible: false,
        }),
    };

    assert_vec3_close(
        lighting.evaluate(Vec3::new(0.0, 0.0, 1.0)),
        Vec3::new(0.25, 0.2, 0.15),
    );
    assert_vec3_close(
        lighting.evaluate(Vec3::new(1.0, 0.0, 0.0)),
        Vec3::new(0.25, 0.2, 0.15),
    );
}

#[test]
fn prop_bake_applies_skin_table_to_material_slot() {
    let asset = test_prop_asset(vec![10, 11]);
    let placement = prop_placement(
        "models/test/chair.mdl",
        Vec3::new(10.0, 20.0, 30.0),
        Vec3::new(0.0, 90.0, 0.0),
        1,
    );
    let mut meshes = BTreeMap::new();

    assert!(bake_prop_placement(
        &placement,
        &asset,
        LegacyPropLightingFallback {
            ambient_cube: AmbientCube::WHITE,
            sun: None,
        },
        &mut meshes
    ));

    let mesh = meshes
        .get(&map_preview::PropMeshDestination {
            material_index: 11,
            entity: false,
            animation_instance: None,
        })
        .expect("skin remapped material");
    assert_eq!(mesh.indices, vec![0, 1, 2]);
    assert_vec3_close(mesh.vertices[0].position, Vec3::new(10.0, 21.0, 30.0));
    assert_eq!(mesh.vertices[0].lightmap_uv, [0.0; 2]);
    assert_eq!(mesh.vertices[0].blend_alpha, 0.0);
    assert_eq!(mesh.vertices[0].opacity, 1.0);
}

#[test]
fn prop_bake_applies_modelscale_to_entity_prop_bounds() {
    let asset = test_prop_asset(vec![0, 1]);
    let mut placement = prop_placement(
        "models/test/scaled_entity.mdl",
        Vec3::splat(0.0),
        Vec3::splat(0.0),
        0,
    );
    placement.scale = 2.0;
    let mut meshes = BTreeMap::new();

    assert!(bake_prop_placement(
        &placement,
        &asset,
        LegacyPropLightingFallback {
            ambient_cube: AmbientCube::WHITE,
            sun: None,
        },
        &mut meshes
    ));

    let mesh = meshes
        .get(&map_preview::PropMeshDestination {
            material_index: 0,
            entity: false,
            animation_instance: None,
        })
        .expect("material mesh");
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in &mesh.vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.position[axis]);
            max[axis] = max[axis].max(vertex.position[axis]);
        }
    }
    assert_vec3_close(Vec3::from(min), Vec3::new(0.0, 0.0, 0.0));
    assert_vec3_close(Vec3::from(max), Vec3::new(2.0, 2.0, 2.0));
}

#[test]
fn door_bake_resolves_skin_remapped_slot_with_model_material_dirs() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Door Material Fixture")
            .entry(
                "materials/models/test/door_base.vmt",
                br#""VertexLitGeneric" { "$basetexture" "models/test/door_base" }"#.to_vec(),
            )
            .entry(
                "materials/models/test/door_skin.vmt",
                br#""VertexLitGeneric" { "$basetexture" "models/test/door_skin" }"#.to_vec(),
            )
            .entry(
                "materials/models/test/door_base.vtf",
                create_vtf_bytes(&[10; 16]),
            )
            .entry(
                "materials/models/test/door_skin.vtf",
                create_vtf_bytes(&[90; 16]),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);
    let mut materials = MaterialTable::default();
    materials.push(
        "door_skin",
        MaterialSlot {
            resolution: Default::default(),
            canonical_name: None,
            shader: Default::default(),
            name: "door_skin".to_owned(),
            texture: None,
            texture2: None,
            render_mode: RenderMode::Opaque,
            alpha_policy: RenderMode::Opaque.texture_alpha_policy(),
        },
    );
    let door = gmpublished_domain::scene::map::MapDoor {
        entity_index: 7,
        brush_collision: None,
        class: gmpublished_domain::scene::map::MapDoorClass::PropDoorRotating,
        origin: Vec3::splat(0.0),
        angles: Vec3::splat(0.0),
        local_bounds_min: Vec3::splat(0.0),
        local_bounds_max: Vec3::splat(0.0),
        visibility: MapVisibilityBucket::Always,
        auto_close_after: Some(0.0),
        initial_progress: 0.0,
        motion: gmpublished_domain::scene::map::MapDoorMotion::Rotating {
            angle_delta: Vec3::new(0.0, 90.0, 0.0),
            degrees: 90.0,
            speed: 100.0,
            open_direction: gmpublished_domain::scene::map::MapDoorOpenDirection::Both,
        },
        sounds: gmpublished_domain::scene::map::MapDoorSounds::default(),
        geometry: MapDoorGeometry::Prop {
            placement: prop_placement(
                "models/test/door.mdl",
                Vec3::splat(0.0),
                Vec3::splat(0.0),
                1,
            )
            .into(),
        },
    };

    let result = bake_map_doors_with_prop_model_loader(
        &[door],
        &resolver,
        &mut materials,
        StaticPropLightingInputs {
            ambient: &MapAmbientLighting::neutral(),
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
        &|_, _| {
            let mut loaded =
                test_loaded_prop_model(vec!["models/test"], vec!["door_base", "door_skin"], 0);
            Arc::get_mut(&mut loaded.model)
                .expect("fresh model")
                .skin_tables = vec![vec![0, 1], vec![1, 1]];
            Some(loaded)
        },
    );

    let mesh = &result.doors[0].meshes[0];
    let slot = materials
        .get(mesh.material_index)
        .expect("door mesh must reference a material slot");
    assert_eq!(slot.name, "door_skin");
    assert!(
        slot.texture.is_some(),
        "skin-remapped door material must not reuse the unresolved map slot"
    );
    let resolution = &result.prop_material_resolutions["models/test/door.mdl"];
    assert_eq!(resolution.used_material_slots, BTreeSet::from([1]));
    assert_eq!(resolution.resolved_used_material_slots, BTreeSet::from([1]));
    assert!(resolution.unresolved_used_material_slots.is_empty());
}

#[test]
fn prop_bake_counts_placement_cap_overflow_as_skipped() {
    let placements = vec![
        prop_placement(
            "models/test/chair.mdl",
            Vec3::splat(0.0),
            Vec3::splat(0.0),
            0,
        ),
        prop_placement(
            "models/test/chair.mdl",
            Vec3::splat(1.0),
            Vec3::splat(0.0),
            0,
        ),
        prop_placement(
            "models/test/chair.mdl",
            Vec3::splat(2.0),
            Vec3::splat(0.0),
            0,
        ),
    ];
    let asset = Arc::new(test_prop_asset(vec![0, 1]));

    let result = bake_static_props_with_loader(
        &placements,
        2,
        100,
        StaticPropLightingInputs {
            ambient: &MapAmbientLighting::neutral(),
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
        |_| Some(Arc::clone(&asset)),
    );

    assert_eq!(result.placed_count, 2);
    assert_eq!(result.skipped_count(), 1);
    assert_eq!(
        result.skip_stats,
        PropBakeSkipStats {
            placement_cap: 1,
            ..PropBakeSkipStats::default()
        }
    );
    assert_eq!(result.meshes[0].indices.len(), 6);
}

#[test]
fn prop_bake_counts_triangle_cap_remaining_as_skipped() {
    let placements = vec![
        prop_placement(
            "models/test/chair.mdl",
            Vec3::splat(0.0),
            Vec3::splat(0.0),
            0,
        ),
        prop_placement(
            "models/test/chair.mdl",
            Vec3::splat(1.0),
            Vec3::splat(0.0),
            0,
        ),
        prop_placement(
            "models/test/chair.mdl",
            Vec3::splat(2.0),
            Vec3::splat(0.0),
            0,
        ),
    ];
    let mut asset = test_prop_asset(vec![0, 1]);
    Arc::make_mut(&mut asset.model).meshes[0].indices = vec![0, 1, 2, 0, 1, 2];
    let asset = Arc::new(asset);

    let result = bake_static_props_with_loader(
        &placements,
        10,
        3,
        StaticPropLightingInputs {
            ambient: &MapAmbientLighting::neutral(),
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
        |_| Some(Arc::clone(&asset)),
    );

    assert_eq!(result.placed_count, 1);
    assert_eq!(result.skipped_count(), 2);
    assert_eq!(
        result.skip_stats,
        PropBakeSkipStats {
            triangle_cap: 2,
            ..PropBakeSkipStats::default()
        }
    );
}

#[test]
fn parallel_prop_bake_matches_serial_output_for_mixed_sprp_and_entity_ranges() {
    let sprp_count = 4;
    let mut placements = (0..sprp_count)
        .map(|index| {
            prop_placement_with_visibility(
                "models/test/chair.mdl",
                Vec3::new(index as f32, index as f32 * 2.0, 0.0),
                Vec3::new(0.0, index as f32 * 15.0, 0.0),
                0,
                MapVisibilityBucket::Always,
            )
        })
        .collect::<Vec<_>>();
    placements.extend((0..4).map(|index| {
        let placement_index = sprp_count + index;
        let visibility = if index == 0 {
            MapPropVisibility::Clusters(vec![3, 7])
        } else {
            MapVisibilityBucket::Cluster(7).into()
        };
        prop_placement_with_visibility(
            "models/test/entity_chair.mdl",
            Vec3::new(placement_index as f32, placement_index as f32 * 2.0, 0.0),
            Vec3::new(0.0, placement_index as f32 * 15.0, 0.0),
            0,
            visibility,
        )
    }));
    let mut asset = test_prop_asset(vec![10, 11]);
    {
        let model = Arc::get_mut(&mut asset.model).expect("fresh model arc");
        model.meshes[0]
            .vertices
            .push(model_vertex(Vec3::new(1.0, 1.0, 0.0)));
        model.meshes[0].indices = vec![0, 1, 2, 1, 3, 2];
        model.vertex_count = 4;
        model.triangle_count = 2;
    }
    let asset = Arc::new(asset);
    let ambient = test_ambient_from_bsp_fixture();

    let serial = bake_static_props_with_loader_serial(
        &placements,
        32,
        100,
        StaticPropLightingInputs {
            ambient: &ambient,
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
        |_| Some(Arc::clone(&asset)),
    );
    let parallel = bake_static_props_with_loader(
        &placements,
        32,
        100,
        StaticPropLightingInputs {
            ambient: &ambient,
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
        |_| Some(Arc::clone(&asset)),
    );

    assert_eq!(parallel.placed_count, serial.placed_count);
    assert_eq!(parallel.skipped_count(), serial.skipped_count());
    assert_eq!(parallel.meshes, serial.meshes);
    assert_eq!(parallel.mesh_visibility, serial.mesh_visibility);
    assert_eq!(parallel.skip_stats, serial.skip_stats);
    assert_eq!(
        parallel.mesh_bytes(),
        parallel
            .meshes
            .iter()
            .map(|mesh| mesh.vertices.len() * size_of::<ModelVertex>()
                + mesh.indices.len() * size_of::<u32>())
            .sum::<usize>()
    );
    assert_ne!(parallel.meshes[0].vertices[0].color, Vec3::splat(1.0));
    assert!(parallel.mesh_visibility[0].clusters.iter().any(
        |cluster| cluster.cluster == 7 && cluster.ranges.iter().all(|range| range.start >= 24)
    ));
    assert!(parallel.mesh_visibility[0].clusters.iter().any(
        |cluster| cluster.cluster == 3 && cluster.ranges.iter().any(|range| range.start == 24)
    ));
}

#[test]
fn sprp_only_prop_bake_is_unchanged_when_entity_prop_list_is_empty() {
    let sprp_props = vec![
        prop_placement(
            "models/test/chair.mdl",
            Vec3::splat(0.0),
            Vec3::splat(0.0),
            0,
        ),
        prop_placement(
            "models/test/chair.mdl",
            Vec3::new(8.0, 0.0, 0.0),
            Vec3::splat(0.0),
            0,
        ),
    ];
    let entity_props: Vec<StaticPropPlacement> = Vec::new();
    let mut combined = sprp_props.clone();
    combined.extend(entity_props);
    let asset = Arc::new(test_prop_asset(vec![0, 1]));
    let lighting = StaticPropLightingInputs {
        ambient: &MapAmbientLighting::neutral(),
        environment_maps: None,
        environment_lighting: None,
        lightmap_source: None,
        walk_collision: None,
    };

    let sprp_only =
        bake_static_props_with_loader(&sprp_props, 32, 100, lighting, |_| Some(Arc::clone(&asset)));
    let with_empty_entity_range =
        bake_static_props_with_loader(&combined, 32, 100, lighting, |_| Some(Arc::clone(&asset)));

    assert_eq!(with_empty_entity_range.placed_count, sprp_only.placed_count);
    assert_eq!(
        with_empty_entity_range.skipped_count(),
        sprp_only.skipped_count()
    );
    assert_eq!(with_empty_entity_range.skip_stats, sprp_only.skip_stats);
    assert_eq!(with_empty_entity_range.mesh_bytes(), sprp_only.mesh_bytes());
    assert_eq!(with_empty_entity_range.meshes, sprp_only.meshes);
    assert_eq!(
        with_empty_entity_range.mesh_visibility,
        sprp_only.mesh_visibility
    );
}

#[test]
fn pre_resolved_prop_materials_match_direct_prop_resolution() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Prop Material Fixture")
            .entry(
                "materials/props/a/shared.vmt",
                br#""VertexLitGeneric" { "$basetexture" "props/a/shared" }"#.to_vec(),
            )
            .entry(
                "materials/props/a/first.vmt",
                br#""VertexLitGeneric" { "$basetexture" "props/a/first" }"#.to_vec(),
            )
            .entry(
                "materials/props/b/shared.vmt",
                br#""VertexLitGeneric" { "$basetexture" "props/b/shared" }"#.to_vec(),
            )
            .entry(
                "materials/props/b/second.vmt",
                br#""VertexLitGeneric" { "$basetexture" "props/b/second" }"#.to_vec(),
            )
            .entry(
                "materials/props/b/water.vmt",
                br#"Water { "$fogcolor" "[0.05 0.08 0.07]" }"#.to_vec(),
            )
            .entry("materials/props/a/shared.vtf", create_vtf_bytes(&[10; 16]))
            .entry("materials/props/a/first.vtf", create_vtf_bytes(&[20; 16]))
            .entry("materials/props/b/shared.vtf", create_vtf_bytes(&[90; 16]))
            .entry("materials/props/b/second.vtf", create_vtf_bytes(&[30; 16]))
            .build(),
    )
    .expect("fixture archive should load");
    let placements = vec![
        prop_placement("models/test/a.mdl", Vec3::splat(0.0), Vec3::splat(0.0), 0),
        prop_placement(
            "models/test/b.mdl",
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::splat(0.0),
            0,
        ),
        prop_placement(
            "models/test/a.mdl",
            Vec3::new(4.0, 0.0, 0.0),
            Vec3::splat(0.0),
            0,
        ),
    ];
    let loaded_model_cache = HashMap::from([
        (
            "models/test/a.mdl".to_owned(),
            Some(Arc::new(test_loaded_prop_model(
                vec!["props/a"],
                vec!["shared", "first", "missing"],
                0,
            ))),
        ),
        (
            "models/test/b.mdl".to_owned(),
            Some(Arc::new(test_loaded_prop_model(
                vec!["props/b"],
                vec!["second", "shared", "water"],
                0,
            ))),
        ),
    ]);

    let direct_resolver = MaterialResolver::new(Arc::new(archive.clone()), None);
    let mut direct_materials = MaterialTable::default();
    let direct = bake_static_props_with_loaded_model_cache(
        &placements,
        &direct_resolver,
        &mut direct_materials,
        &loaded_model_cache,
        false,
        &MapAmbientLighting::neutral(),
    );

    let pre_resolved_resolver = MaterialResolver::new(Arc::new(archive), None);
    let mut pre_resolved_materials = MaterialTable::default();
    let pre_resolved = bake_static_props_with_loaded_model_cache(
        &placements,
        &pre_resolved_resolver,
        &mut pre_resolved_materials,
        &loaded_model_cache,
        true,
        &MapAmbientLighting::neutral(),
    );

    assert_eq!(pre_resolved.placed_count, direct.placed_count);
    assert_eq!(pre_resolved.skipped_count(), direct.skipped_count());
    assert_eq!(pre_resolved.meshes, direct.meshes);
    assert_eq!(
        material_slot_signatures(pre_resolved_materials.slots()),
        material_slot_signatures(direct_materials.slots())
    );
    assert_eq!(
        pre_resolved_materials.name_indexes(),
        direct_materials.name_indexes()
    );
    assert_eq!(
        pre_resolved_materials.resolved_count(),
        direct_materials.resolved_count()
    );
    assert_eq!(
        pre_resolved_materials.water_material_count(),
        direct_materials.water_material_count()
    );
    assert_eq!(
        direct_materials
            .slots()
            .iter()
            .find(|material| material.name == "shared")
            .and_then(|material| material.texture.as_ref())
            .and_then(|texture| texture.rgba_bytes().map(Vec::from)),
        Some(vec![10; 16])
    );
}

#[test]
fn prop_materials_share_one_slot_per_directory_qualified_name() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Shared Prop Material Fixture")
            .entry(
                "materials/props/a/shared.vmt",
                br#""VertexLitGeneric" { "$basetexture" "props/a/shared" }"#.to_vec(),
            )
            .entry(
                "materials/props/b/shared.vmt",
                br#""VertexLitGeneric" { "$basetexture" "props/b/shared" }"#.to_vec(),
            )
            .entry("materials/props/a/shared.vtf", create_vtf_bytes(&[10; 16]))
            .entry("materials/props/b/shared.vtf", create_vtf_bytes(&[90; 16]))
            .build(),
    )
    .expect("fixture archive should load");
    let placements = vec![
        prop_placement("models/test/a.mdl", Vec3::splat(0.0), Vec3::splat(0.0), 0),
        prop_placement(
            "models/test/b.mdl",
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::splat(0.0),
            0,
        ),
        prop_placement(
            "models/test/c.mdl",
            Vec3::new(4.0, 0.0, 0.0),
            Vec3::splat(0.0),
            0,
        ),
    ];
    let loaded_model_cache = HashMap::from([
        (
            "models/test/a.mdl".to_owned(),
            Some(Arc::new(test_loaded_prop_model(
                vec!["props/a"],
                vec!["shared"],
                0,
            ))),
        ),
        (
            "models/test/b.mdl".to_owned(),
            Some(Arc::new(test_loaded_prop_model(
                vec!["props/b"],
                vec!["shared"],
                0,
            ))),
        ),
        (
            "models/test/c.mdl".to_owned(),
            Some(Arc::new(test_loaded_prop_model(
                vec!["props/a"],
                vec!["shared"],
                0,
            ))),
        ),
    ]);

    let resolver = MaterialResolver::new(Arc::new(archive), None);
    let mut materials = MaterialTable::default();
    let result = bake_static_props_with_loaded_model_cache(
        &placements,
        &resolver,
        &mut materials,
        &loaded_model_cache,
        false,
        &MapAmbientLighting::neutral(),
    );

    assert_eq!(result.placed_count, 3);
    let indices = result
        .meshes
        .iter()
        .map(|mesh| mesh.material_index)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        indices.len(),
        2,
        "`a.mdl` and `c.mdl` must share the one `props/a/shared` slot, and `props/b/shared` \
         must not collide with it despite the identical material name"
    );
    assert_eq!(materials.slots().len(), 2);
    assert!(
        indices.iter().all(|index| materials
            .get(*index)
            .is_some_and(|slot| slot.texture.is_some())),
        "both shared materials resolve in the fixture archive"
    );
}

#[test]
fn missing_static_prop_model_is_skipped_and_counted() {
    let archive = PreviewArchive::from_gma(GmaFixtureBuilder::new("Empty").build())
        .expect("empty fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);
    let mut materials = MaterialTable::default();

    let result = bake_static_props(
        &[prop_placement(
            "models/test/missing.mdl",
            Vec3::splat(0.0),
            Vec3::splat(0.0),
            0,
        )],
        &resolver,
        &mut materials,
        &MapAmbientLighting::neutral(),
    );

    assert_eq!(result.placed_count, 0);
    assert_eq!(result.skipped_count(), 1);
    assert_eq!(
        result.skip_stats,
        PropBakeSkipStats {
            load_failure: 1,
            ..PropBakeSkipStats::default()
        }
    );
    assert!(result.meshes.is_empty());
}

#[test]
fn map_without_baked_lighting_uses_fullbright_independently_of_atlas_availability() {
    use crate::media::preview_model::ShadingMode;
    for (has_channel, expected) in [(false, ShadingMode::Fullbright), (true, ShadingMode::Scene)] {
        let mut bytes = static_prop_bsp_fixture_bytes();
        if has_channel {
            let offset = bytes.len();
            bytes.extend_from_slice(&[128, 128, 128, 0]);
            write_bsp_lump_entry(&mut bytes, 8, offset, 4, 0);
        }
        let request = request("maps/test.bsp", &bytes);
        let data = map_preview_data_with_prop_model_loader(
            &request,
            &bytes,
            None,
            None,
            &mut |_| {},
            &|_, _| None,
        );
        let PreviewContent::Map { scene, .. } = data.content else {
            panic!("expected map preview");
        };
        assert!(scene.lightmap.is_none());
        assert_eq!(scene.shading_mode, expected);
    }
}

#[test]
fn map_preview_with_panicking_static_prop_model_still_returns_map() {
    let bytes = static_prop_bsp_fixture_bytes();
    let request = request("maps/test.bsp", &bytes);
    let data = map_preview_data_with_prop_model_loader(
        &request,
        &bytes,
        None,
        None,
        &mut |_| {},
        &|_, _| panic!("fixture model panic"),
    );

    assert!(matches!(
        data.content,
        PreviewContent::Map {
            stats: MapStats {
                static_prop_count: 1,
                placed_prop_count: 0,
                skipped_prop_count: 1,
                ..
            },
            ..
        }
    ));
}

#[test]
fn unresolved_material_names_for_debug_are_sorted_deduped_and_capped() {
    let mut materials = (0..25)
        .rev()
        .map(|index| MaterialSlot {
            resolution: Default::default(),
            canonical_name: None,
            shader: Default::default(),
            name: format!("missing_{index:02}"),
            texture: None,
            texture2: None,
            render_mode: RenderMode::Opaque,
            alpha_policy: RenderMode::Opaque.texture_alpha_policy(),
        })
        .collect::<Vec<_>>();
    materials.push(MaterialSlot {
        resolution: Default::default(),
        canonical_name: None,
        shader: Default::default(),
        name: "missing_03".to_owned(),
        texture: None,
        texture2: None,
        render_mode: RenderMode::Opaque,
        alpha_policy: RenderMode::Opaque.texture_alpha_policy(),
    });

    let (names, total) = unresolved_material_names_for_debug(&materials);

    assert_eq!(total, 25);
    assert_eq!(names.len(), 20);
    assert_eq!(names[0], "missing_00");
    assert_eq!(names[19], "missing_19");
}

pub(super) fn prop_placement(
    model_path: &str,
    origin: Vec3,
    angles: Vec3,
    skin: i32,
) -> StaticPropPlacement {
    prop_placement_with_visibility(
        model_path,
        origin,
        angles,
        skin,
        MapVisibilityBucket::Always,
    )
}

fn prop_placement_with_visibility(
    model_path: &str,
    origin: Vec3,
    angles: Vec3,
    skin: i32,
    visibility: impl Into<MapPropVisibility>,
) -> StaticPropPlacement {
    StaticPropPlacement {
        entity: false,
        initially_visible: true,
        animation: None,
        leaf_membership: gmpublished_domain::scene::map::MapPropLeafMembership::SpatialBounds,
        static_prop_index: None,
        lighting_origin: None,
        lighting_origin_reference: None,
        diffuse_modulation: [255; 4],
        render_mode: 0,
        flags: 0,
        flags_ex: None,
        extra_flags: None,
        model_path: model_path.to_owned(),
        origin,
        angles,
        skin,
        scale: 1.0,
        solid: gmpublished_domain::scene::map::MapPropSolid::None,
        visibility: visibility.into(),
    }
}

pub(super) fn test_prop_asset(material_indices: Vec<usize>) -> PropModelAsset {
    PropModelAsset {
        model: Arc::new(ModelData {
            animation: None,
            skin_vertices: Vec::new(),
            mdl_flags: 0,
            vertex_compression: vformats::mdl::VertexCompression::None,
            illumination_position: Vec3::ZERO,
            hull_min: Vec3::ZERO,
            hull_max: Vec3::splat(1.0),
            view_min: Vec3::ZERO,
            view_max: Vec3::ZERO,
            illumination_attachment_index: None,
            illumination_attachment_position: None,
            lighting: None,
            meshes: vec![MeshData {
                geometry: crate::media::preview_model::MeshGeometry::Model,
                vertices: vec![
                    model_vertex(Vec3::new(1.0, 0.0, 0.0)),
                    model_vertex(Vec3::new(0.0, 1.0, 0.0)),
                    model_vertex(Vec3::new(0.0, 0.0, 1.0)),
                ],
                indices: vec![0, 1, 2],
                material_index: 0,
                bodygroup: 0,
                bodygroup_choice: 0,
            }],
            material_names: vec!["mat0".to_owned(), "mat1".to_owned()],
            material_dirs: Vec::new(),
            skin_tables: vec![vec![0, 1], vec![1, 0]],
            bodygroups: vec![1],
            bounds_min: Vec3::splat(0.0),
            bounds_max: Vec3::splat(1.0),
            bone_count: 0,
            sequence_count: 0,
            vertex_count: 3,
            triangle_count: 1,
        }),
        material_indices: material_indices.into_iter().enumerate().collect(),
    }
}

fn test_loaded_prop_model(
    material_dirs: Vec<&str>,
    material_names: Vec<&str>,
    mesh_material_index: usize,
) -> LoadedPropModel {
    let material_count = material_names.len();
    LoadedPropModel {
        model: Arc::new(ModelData {
            animation: None,
            skin_vertices: Vec::new(),
            mdl_flags: 0,
            vertex_compression: vformats::mdl::VertexCompression::None,
            illumination_position: Vec3::ZERO,
            hull_min: Vec3::ZERO,
            hull_max: Vec3::splat(1.0),
            view_min: Vec3::ZERO,
            view_max: Vec3::ZERO,
            illumination_attachment_index: None,
            illumination_attachment_position: None,
            lighting: None,
            meshes: vec![MeshData {
                geometry: crate::media::preview_model::MeshGeometry::Model,
                vertices: vec![
                    model_vertex(Vec3::new(1.0, 0.0, 0.0)),
                    model_vertex(Vec3::new(0.0, 1.0, 0.0)),
                    model_vertex(Vec3::new(0.0, 0.0, 1.0)),
                ],
                indices: vec![0, 1, 2],
                material_index: mesh_material_index,
                bodygroup: 0,
                bodygroup_choice: 0,
            }],
            material_names: material_names
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
            material_dirs: material_dirs
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
            skin_tables: vec![
                (0..material_count)
                    .map(|index| u16::try_from(index).unwrap_or(u16::MAX))
                    .collect(),
            ],
            bodygroups: vec![1],
            bounds_min: Vec3::splat(0.0),
            bounds_max: Vec3::splat(1.0),
            bone_count: 0,
            sequence_count: 0,
            vertex_count: 3,
            triangle_count: 1,
        }),
        physics: LoadedPropPhysics::Missing,
        collision: None,
    }
}

fn model_vertex(position: Vec3) -> ModelVertex {
    ModelVertex {
        position,
        tangent_s: Vec3::ZERO,
        tangent_t: Vec3::ZERO,
        tangent_sign: 0.0,
        lighting_context: 0,
        surface_mark: 0,
        normal: Vec3::new(1.0, 0.0, 0.0),
        uv: [0.25, 0.75],
        lightmap_uv: [0.5, 0.5],
        lightmap_bump_step: 0.0,
        lightmap_page: 0,
        color: Vec3::splat(1.0),
        blend_alpha: 1.0,
        opacity: 1.0,
        illumination: Vec3::splat(1.0),
        lighting_mode: 0,
    }
}

fn test_ambient_cube(color: Vec3) -> AmbientCube {
    AmbientCube { colors: [color; 6] }
}

fn assert_vec3_close(actual: Vec3, expected: Vec3) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!(
            (actual - expected).abs() < 1e-4,
            "actual {actual} expected {expected}"
        );
    }
}

fn static_prop_bsp_fixture_bytes() -> Vec<u8> {
    const HEADER_LEN: usize = 8;
    const LUMP_COUNT: usize = 64;
    const LUMP_ENTRY_LEN: usize = 16;
    const PLANES_LUMP_INDEX: usize = 1;
    const NODES_LUMP_INDEX: usize = 5;
    const LEAVES_LUMP_INDEX: usize = 10;
    const GAME_LUMP_INDEX: usize = 35;
    const PAKFILE_LUMP_INDEX: usize = 40;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"VBSP");
    bytes.extend_from_slice(&20_u32.to_le_bytes());
    bytes.resize(HEADER_LEN + LUMP_COUNT * LUMP_ENTRY_LEN, 0);

    let planes_offset = bytes.len();
    push_bsp_vec3(&mut bytes, Vec3::new(0.0, 0.0, 1.0));
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    write_bsp_lump_entry(&mut bytes, PLANES_LUMP_INDEX, planes_offset, 20, 0);

    let nodes_offset = bytes.len();
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&(-1_i32).to_le_bytes());
    bytes.extend_from_slice(&(-1_i32).to_le_bytes());
    bytes.extend_from_slice(&[0_u8; 20]);
    write_bsp_lump_entry(&mut bytes, NODES_LUMP_INDEX, nodes_offset, 32, 0);

    let leaves_offset = bytes.len();
    bytes.extend_from_slice(&[0_u8; 56]);
    write_bsp_lump_entry(&mut bytes, LEAVES_LUMP_INDEX, leaves_offset, 56, 0);

    let prop_data = static_prop_game_lump_data();
    let game_lump_offset = bytes.len();
    let prop_offset = game_lump_offset + 20;
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    bytes.extend_from_slice(&i32::from_be_bytes(*b"sprp").to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&10_u16.to_le_bytes());
    bytes.extend_from_slice(&(prop_offset as i32).to_le_bytes());
    bytes.extend_from_slice(&(prop_data.len() as i32).to_le_bytes());
    bytes.extend_from_slice(&prop_data);
    write_bsp_lump_entry(&mut bytes, GAME_LUMP_INDEX, game_lump_offset, 20, 0);

    let pakfile_offset = bytes.len();
    let empty_zip = empty_zip_bytes();
    bytes.extend_from_slice(&empty_zip);
    write_bsp_lump_entry(
        &mut bytes,
        PAKFILE_LUMP_INDEX,
        pakfile_offset,
        empty_zip.len(),
        0,
    );

    bytes
}

fn test_ambient_from_bsp_fixture() -> MapAmbientLighting {
    gmpublished_domain::scene::map::load_map(&ambient_static_prop_bsp_fixture_bytes())
        .expect("ambient fixture bsp should load")
        .ambient
}

fn ambient_static_prop_bsp_fixture_bytes() -> Vec<u8> {
    const LEAF_AMBIENT_INDEX_LUMP_INDEX: usize = 52;
    const LEAF_AMBIENT_LIGHTING_LUMP_INDEX: usize = 56;

    let mut bytes = static_prop_bsp_fixture_bytes();
    let mut index = Vec::new();
    index.extend_from_slice(&1_u16.to_le_bytes());
    index.extend_from_slice(&0_u16.to_le_bytes());
    let index_offset = bytes.len();
    bytes.extend_from_slice(&index);
    write_bsp_lump_entry(
        &mut bytes,
        LEAF_AMBIENT_INDEX_LUMP_INDEX,
        index_offset,
        index.len(),
        0,
    );

    let mut samples = Vec::new();
    for _ in 0..6 {
        samples.extend_from_slice(&[64, 128, 255, 0]);
    }
    samples.extend_from_slice(&[0, 0, 0, 0]);
    let samples_offset = bytes.len();
    bytes.extend_from_slice(&samples);
    write_bsp_lump_entry(
        &mut bytes,
        LEAF_AMBIENT_LIGHTING_LUMP_INDEX,
        samples_offset,
        samples.len(),
        0,
    );

    bytes
}

fn static_prop_game_lump_data() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    let mut name = [0_u8; 128];
    let source_name = br"\Models\Props_C17\Oildrum001.MDL";
    name[..source_name.len()].copy_from_slice(source_name);
    bytes.extend_from_slice(&name);
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    push_bsp_vec3(&mut bytes, Vec3::new(10.0, 20.0, 30.0));
    push_bsp_vec3(&mut bytes, Vec3::new(1.0, 90.0, 3.0));
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.push(0);
    bytes.push(0);
    bytes.extend_from_slice(&2_i32.to_le_bytes());
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    push_bsp_vec3(&mut bytes, Vec3::splat(0.0));
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes
}

fn push_bsp_vec3(bytes: &mut Vec<u8>, values: Vec3) {
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

fn write_bsp_lump_entry(
    bytes: &mut [u8],
    lump_index: usize,
    offset: usize,
    length: usize,
    version: u32,
) {
    let start = 8 + lump_index * 16;
    bytes[start..start + 4].copy_from_slice(&(offset as u32).to_le_bytes());
    bytes[start + 4..start + 8].copy_from_slice(&(length as u32).to_le_bytes());
    bytes[start + 8..start + 12].copy_from_slice(&version.to_le_bytes());
}

fn empty_zip_bytes() -> [u8; 22] {
    [
        0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]
}

type MaterialSlotSignature = (
    String,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    bool,
    RenderMode,
    bool,
);

fn material_slot_signatures(materials: &[MaterialSlot]) -> Vec<MaterialSlotSignature> {
    materials
        .iter()
        .map(|material| {
            (
                material.name.clone(),
                material
                    .texture
                    .as_ref()
                    .and_then(|texture| texture.rgba_bytes().map(Vec::from)),
                material
                    .texture2
                    .as_ref()
                    .and_then(|texture| texture.rgba_bytes().map(Vec::from)),
                material.alpha_policy.force_opaque(),
                material.render_mode,
                material
                    .texture
                    .as_ref()
                    .is_some_and(|texture| texture.is_water_fallback()),
            )
        })
        .collect()
}

fn tiny_wav_bytes() -> Vec<u8> {
    let channels = 1_u16;
    let sample_rate = 8_000_u32;
    let bits_per_sample = 16_u16;
    let sample_count = 800_u32;
    let block_align = channels * bits_per_sample / 8;
    let byte_rate = sample_rate * u32::from(block_align);
    let data_len = sample_count * u32::from(block_align);
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(44 + data_len as usize, 0);
    bytes
}

fn create_vtf_bytes(rgba: &[u8]) -> Vec<u8> {
    fixture_vtf_bytes(2, 2, VtfFormat::Rgba8888, &[rgba])
}

#[test]
fn explicit_lightmap_source_survives_zero_uv_coordinates() {
    use gmpublished_domain::scene::map::{MapMesh, MapMeshVisibility, MapVertex};
    let mesh = MapMesh {
        entity: false,
        vertices: vec![MapVertex {
            position: Vec3::ZERO,
            base_position: Vec3::ZERO,
            normal: Vec3::new(0.0, 0.0, 1.0),
            tangent_s: Vec3::ZERO,
            tangent_t: Vec3::ZERO,
            tex_s: 0.0,
            tex_t: 0.0,
            texture_size: [1, 1],
            lightmap_uv: [0.0; 2],
            lightmap_page: Some(gmpublished_domain::scene::map::LightmapPageId(0)),
            lightmap_bump_step: 0.0,
            blend_alpha: 0.0,
            opacity: 1.0,
        }],
        indices: vec![0],
        material_index: 0,
        visibility: MapMeshVisibility::default(),
        surface_owners: Vec::new(),
    };
    let vertex = map_preview::map_mesh_to_model_mesh(&mesh, &[]).vertices[0];
    assert_eq!(vertex.lightmap_uv, [0.0; 2]);
    assert_ne!(
        vertex.lighting_mode & crate::media::preview_model::vertex_lighting::LIGHTMAP,
        0
    );
    assert_eq!(
        vertex.lighting_mode & crate::media::preview_model::vertex_lighting::PREVIEW,
        0
    );
}

#[test]
fn compiled_surfaces_without_samples_use_constant_light() {
    let mut receiver = surface_mark_receiver();
    for vertex in &mut receiver.vertices {
        vertex.lightmap_page = None;
    }
    for entity in [false, true] {
        receiver.entity = entity;
        let mesh = map_preview::map_mesh_to_model_mesh(&receiver, &[]);
        assert!(mesh.vertices.iter().all(|vertex| {
            vertex.lighting_mode == crate::media::preview_model::vertex_lighting::WORLD_CONSTANT
                && vertex.illumination == Vec3::splat(1.0)
        }));
    }
}

fn surface_mark_receiver() -> gmpublished_domain::scene::map::MapMesh {
    use gmpublished_domain::scene::map::{
        MapMesh, MapMeshIndexRange, MapMeshSurfaceOwner, MapMeshVisibility, MapSurfaceOwner,
        MapVertex,
    };
    MapMesh {
        entity: false,
        vertices: [[-64.0, -64.0], [-64.0, 64.0], [64.0, 64.0], [64.0, -64.0]]
            .map(|[x, y]| MapVertex {
                position: Vec3::new(x, y, 0.0),
                base_position: Vec3::new(x, y, 0.0),
                normal: Vec3::new(0.0, 0.0, 1.0),
                tangent_s: Vec3::new(1.0, 0.0, 0.0),
                tangent_t: Vec3::new(0.0, 1.0, 0.0),
                tex_s: x,
                tex_t: y,
                texture_size: [128; 2],
                lightmap_uv: [0.25 + x / 512.0, 0.25 + y / 512.0],
                lightmap_page: Some(gmpublished_domain::scene::map::LightmapPageId(0)),
                lightmap_bump_step: 0.125,
                blend_alpha: 0.0,
                opacity: 1.0,
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material_index: 0,
        visibility: MapMeshVisibility {
            always_visible: vec![MapMeshIndexRange {
                face: 7,
                start: 0,
                count: 6,
            }],
            clusters: Vec::new(),
        },
        surface_owners: vec![MapMeshSurfaceOwner {
            index_start: 0,
            owner: MapSurfaceOwner::World,
            solid: true,
            allows_decals: true,
            plane_normal: Vec3::new(0.0, 0.0, 1.0),
            plane_distance: 0.0,
            displacement: None,
            texture_projection: Some(
                gmpublished_domain::scene::map::MapSurfaceTextureProjection {
                    vectors: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
                    minimum: [-64; 2],
                    extent: [128; 2],
                },
            ),
        }],
    }
}

fn surface_mark() -> gmpublished_domain::scene::map::MapOverlay {
    use gmpublished_domain::scene::map::{MapDecal, MapSurfaceOwner, MapSurfaceProjector};
    let world = [surface_mark_receiver()];
    MapSurfaceProjector::new(&world, &[], &[])
        .project_decal(
            &MapDecal {
                id: 3,
                material_name: "test/decal".to_owned(),
                position: gmpublished_domain::scene::map::MapDecalPosition {
                    authored_world: Vec3::ZERO,
                    receiver_local: Vec3::ZERO,
                },
                owner: MapSurfaceOwner::World,
                permanence: Default::default(),
                projection_order: None,
            },
            gmpublished_domain::scene::map::MapDecalDimensions {
                extent: [32.0, 16.0],
                radius: 16.0,
            },
            &[true],
        )
        .unwrap()
        .pop()
        .unwrap()
}

#[test]
fn surface_marks_retain_receiver_geometry_lighting_and_draw_attachment() {
    let mark = surface_mark();
    let primitive = materials::overlay_primitive(&mark, 5);
    assert_eq!(primitive.indices, mark.indices);
    assert_eq!(primitive.source, mark.source);
    assert_eq!(primitive.receiver_mesh, mark.receiver_mesh);
    assert_eq!(primitive.receiver_range, mark.receiver_range);
    assert_eq!(
        primitive.receiver_material_index,
        mark.receiver_material_index
    );
    assert_eq!(primitive.material_index, 5);
    for (actual, expected) in primitive.vertices.iter().zip(&mark.vertices) {
        assert_eq!(actual.position, expected.surface.position);
        assert_eq!(actual.lightmap_uv, expected.surface.lightmap_uv);
        assert_eq!(
            actual.lightmap_bump_step,
            expected.surface.lightmap_bump_step
        );
        assert_eq!(actual.tangent_s, expected.surface.tangent_s);
        assert_eq!(actual.tangent_t, expected.surface.tangent_t);
        assert_eq!(actual.uv, expected.uv);
        assert_eq!(
            actual.lighting_mode,
            crate::media::preview_model::vertex_lighting::RECEIVER_LIGHTMAP
                | crate::media::preview_model::vertex_lighting::BATCHED_STATIC_DECAL
        );
    }
    let mut compiled = mark;
    compiled.source =
        gmpublished_domain::scene::map::MapSurfaceMarkSource::Overlay { index: 0, id: 3 };
    let primitive = materials::overlay_primitive(&compiled, 5);
    assert!(primitive.vertices.iter().all(|vertex| vertex.lighting_mode
        == crate::media::preview_model::vertex_lighting::RECEIVER_LIGHTMAP));
}

#[test]
fn static_decal_mapping_uses_original_dimensions_after_stored_mip_selection() {
    use gmpublished_domain::scene::map::{MapDecal, MapSurfaceOwner, MapSurfaceProjector};
    let mips = (0..7)
        .rev()
        .map(|mip| {
            let pixels = (64_u32 >> mip).max(1) * (32_u32 >> mip).max(1);
            [17, 31, 47, 255].repeat(pixels as usize)
        })
        .collect::<Vec<_>>();
    let stored = mips.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let archive = PreviewArchive::from_gma(GmaFixtureBuilder::new("DecalDimensions")
        .entry("materials/test/decal.vmt",br#"LightmappedGeneric { "$basetexture" "test/decal" "$decal" "1" "$decalscale" "0.5" }"#.to_vec())
        .entry("materials/test/decal.vtf",fixture_vtf_bytes(64,32,VtfFormat::Rgba8888,&stored))
        .build()).unwrap();
    let resolver =
        MaterialResolver::new(Arc::new(archive), None).with_decoded_texture_max_dimension(1);
    let world = [surface_mark_receiver()];
    let projector = MapSurfaceProjector::new(&world, &[], &[]);
    let mut table = MaterialTable::default();
    let decal = MapDecal {
        id: 3,
        material_name: "test/decal".to_owned(),
        position: gmpublished_domain::scene::map::MapDecalPosition {
            authored_world: Vec3::ZERO,
            receiver_local: Vec3::ZERO,
        },
        owner: MapSurfaceOwner::World,
        permanence: Default::default(),
        projection_order: None,
    };
    let (world, sky) =
        materials::resolve_map_decals(&[decal], &projector, &[true], &resolver, &mut table, None);
    assert!(sky.overlays.is_empty());
    assert_eq!(world.skipped_count, 0);
    assert_eq!(world.overlays.len(), 1);
    let texture = table.slots()[0].texture.as_ref().unwrap();
    assert_eq!([texture.width(), texture.height()], [1, 1]);
    assert_eq!(texture.original_dimensions(), (64, 32));
    assert_eq!(texture.load_outcome(), TextureLoadOutcome::Loaded);
    assert_eq!(texture.resident().unwrap().id().first_source_mip, 6);
    assert_eq!(texture.rgba_bytes(), Some(&[17, 31, 47, 255][..]));
    let vertices = &world.overlays[0].vertices;
    let max = vertices
        .iter()
        .map(|vertex| vertex.position[0])
        .max_by(f32::total_cmp)
        .unwrap();
    let min = vertices
        .iter()
        .map(|vertex| vertex.position[0])
        .min_by(f32::total_cmp)
        .unwrap();
    assert!((max - min - 32.0).abs() < 1e-6);
}

#[test]
fn surface_mark_statistics_count_authored_sources_instead_of_receiver_fragments() {
    use gmpublished_domain::scene::map::MapSurfaceMarkSource;
    let mut first = materials::overlay_primitive(&surface_mark(), 0);
    first.source = MapSurfaceMarkSource::Overlay { index: 0, id: 11 };
    let mut second = first.clone();
    second.receiver_range.face = 8;
    let mut third = first.clone();
    third.source = MapSurfaceMarkSource::Decal {
        entity_index: 11,
        permanence: Default::default(),
    };
    let mut fourth = first.clone();
    fourth.source = MapSurfaceMarkSource::Overlay { index: 1, id: 11 };
    assert_eq!(
        materials::authored_surface_mark_count(&[first, second, third, fourth]),
        3
    );
}

#[test]
fn unavailable_surface_mark_material_is_counted_once_across_fragments() {
    let first = surface_mark();
    let resolver = MaterialResolver::new(
        Arc::new(
            PreviewArchive::from_gma(
                GmaFixtureBuilder::new("UnavailableDecal")
                    .entry(format!("materials/{}.vmt", first.material_name), Vec::new())
                    .build(),
            )
            .unwrap(),
        ),
        None,
    );
    let mut second = first.clone();
    second.receiver_range.face = 8;
    let baked =
        materials::resolve_map_overlays(&[first, second], &resolver, &mut MaterialTable::default());
    assert_eq!(baked.skipped_count, 1);
    assert!(baked.overlays.is_empty());
}

#[test]
fn compacted_door_indices_remap_surface_mark_geometry_and_omit_unavailable_owners() {
    use crate::media::preview_model::{DoorInstance, DoorSounds};
    use gmpublished_domain::scene::map::{
        MapDoorClass, MapDoorMotion, MapReceiverMesh, MapSurfaceMarkSource, MapSurfaceOwner,
    };
    let door = DoorInstance {
        source_index: 5,
        brush_collision: None,
        class: MapDoorClass::FuncDoor,
        origin: Vec3::ZERO,
        angles: Vec3::ZERO,
        local_bounds_min: Vec3::ZERO,
        local_bounds_max: Vec3::ZERO,
        visibility: MapVisibilityBucket::Always,
        initial_progress: 0.0,
        auto_close_after: None,
        motion: MapDoorMotion::Linear {
            direction: Vec3::new(1.0, 0.0, 0.0),
            distance: 64.0,
            speed: 64.0,
        },
        sounds: DoorSounds::default(),
        meshes: Vec::new(),
    };
    let mut available = materials::overlay_primitive(&surface_mark(), 0);
    available.owner = MapSurfaceOwner::Door {
        door_index: 5,
        model_index: gmpublished_domain::scene::map::ModelIndex::new(1),
    };
    available.receiver_mesh = MapReceiverMesh::Door {
        door_index: 5,
        mesh_index: 2,
    };
    let mut missing = available.clone();
    missing.owner = MapSurfaceOwner::Door {
        door_index: 1,
        model_index: gmpublished_domain::scene::map::ModelIndex::new(1),
    };
    missing.receiver_mesh = MapReceiverMesh::Door {
        door_index: 1,
        mesh_index: 0,
    };
    missing.source = MapSurfaceMarkSource::Decal {
        entity_index: 20,
        permanence: Default::default(),
    };
    let mut world = vec![available, missing.clone(), missing];
    assert_eq!(
        map_preview::remap_surface_mark_doors(&mut world, &mut Vec::new(), &[door]),
        1
    );
    assert_eq!(world.len(), 1);
    assert_eq!(
        world[0].owner,
        MapSurfaceOwner::Door {
            door_index: 0,
            model_index: gmpublished_domain::scene::map::ModelIndex::new(1)
        }
    );
    assert_eq!(
        world[0].receiver_mesh,
        MapReceiverMesh::Door {
            door_index: 0,
            mesh_index: 2
        }
    );
}

#[test]
fn cancelled_preview_routes_stop_before_archive_or_content_decoding() {
    for path in [
        "maps/missing.bsp",
        "models/missing.mdl",
        "particles/missing.pcf",
    ] {
        let request = request(path, &[]);
        request.cancellation.cancel();
        let cpu = gmpublished_backend::CpuExecutor::build(2).unwrap();
        let result = load_preview(&request, &Tokens::dark(), None, &cpu, &mut |_| {
            panic!("cancelled load emitted a stage")
        });
        assert!(
            matches!(result, Err(PreviewLoadError::Cancelled(_))),
            "{path}: {result:?}"
        );
    }
}

#[test]
fn particle_materials_keep_unknown_source_failures_and_blocked_texture_outcomes() {
    use crate::bridge::materials::MaterialResolutionOutcome;
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    std::fs::create_dir_all(game.join("garrysmod")).unwrap();
    std::fs::write(game.join("garrysmod/addons"), []).unwrap();
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Particle sources")
            .entry(
                "materials/particle/authored.vmt",
                br#"UnlitGeneric { "$basetexture" "particle/unavailable" }"#.to_vec(),
            )
            .build(),
    )
    .unwrap();
    let resolver = MaterialResolver::new(Arc::new(archive), Some(game));
    let material = resolve_particle_material(&resolver, "particle/unknown".to_owned());
    assert_eq!(
        material.resolution,
        MaterialResolutionOutcome::SourceUnavailable
    );
    assert!(material.texture.is_none());
    assert!(material.is_unavailable());
    let authored = resolve_particle_material(&resolver, "particle/authored".to_owned());
    assert_eq!(authored.resolution, MaterialResolutionOutcome::Loaded);
    assert_eq!(
        authored.texture.as_ref().unwrap().load_outcome(),
        TextureLoadOutcome::SourceUnavailable
    );
    assert!(authored.is_unavailable());
    let preview = ParticlePreview {
        file: Default::default(),
        systems: Vec::new(),
        materials: vec![material, authored],
        sheet_stats: Default::default(),
        source_discovery: resolver.source_discovery_stats().clone(),
    };
    assert!(!preview.source_discovery.failures.is_empty());
    assert!(preview.has_unavailable_content());
}

#[test]
fn particle_native_missing_and_invalid_base_images_remain_sampleable() {
    use crate::bridge::materials::MaterialResolutionOutcome;
    let resolver = MaterialResolver::prop_lighting_fixture(
        vec![
            (
                "materials/particle/missing.vmt",
                br#"UnlitGeneric { "$basetexture" "particle/absent" }"#.to_vec(),
            ),
            (
                "materials/particle/invalid.vmt",
                br#"UnlitGeneric { "$basetexture" "particle/bad" }"#.to_vec(),
            ),
            (
                "materials/particle/bad.vtf",
                b"invalid selected payload".to_vec(),
            ),
        ],
        Vec::new(),
    );
    for (name, outcome) in [
        ("missing", TextureLoadOutcome::MissingSource),
        ("invalid", TextureLoadOutcome::InvalidSource),
    ] {
        let slot = resolve_particle_material(&resolver, format!("particle/{name}"));
        assert_eq!(slot.resolution, MaterialResolutionOutcome::Loaded);
        let texture = slot.texture.unwrap();
        assert_eq!(texture.load_outcome(), outcome);
        assert!(!texture.load_outcome().is_unavailable());
        assert_eq!(texture.mip_chain().next().unwrap().width, 32);
    }
}
