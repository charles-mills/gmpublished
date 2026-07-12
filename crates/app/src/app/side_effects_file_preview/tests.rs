use std::sync::Arc;

use image::{DynamicImage, RgbaImage};

use super::*;
use crate::backend::{archive::PreviewArchiveSource, gma::PreviewArchive};
use crate::test_support::GmaFixtureBuilder;

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
        request_id: 1,
        archive: PreviewArchiveSource::from_gma(Arc::new(archive)),
        entry_path: path.to_owned(),
        display_name: path.rsplit('/').next().unwrap_or(path).to_owned(),
        size_bytes: bytes.len() as u64,
        crc32: 0,
        bypass_size_limits: false,
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
        request_id: 1,
        archive: PreviewArchiveSource::from_gma(Arc::new(archive)),
        entry_path: path,
        display_name,
        size_bytes,
        crc32,
        bypass_size_limits: false,
    }
}

#[test]
fn classification_table_matches_expected_extensions() {
    assert_eq!(
        classify_entry_path("lua/autorun/init.lua"),
        EntryClass::Code {
            syntax: CodeSyntax::Glua
        }
    );
    assert_eq!(
        classify_entry_path("data/config.JSON"),
        EntryClass::Code {
            syntax: CodeSyntax::Json
        }
    );
    assert_eq!(
        classify_entry_path("materials/icon.vmt"),
        EntryClass::Code {
            syntax: CodeSyntax::Vmt
        }
    );
    assert_eq!(
        classify_entry_path("materials/icon.vtf"),
        EntryClass::Image(ImageClass::Vtf)
    );
    assert_eq!(
        classify_entry_path("models/props_c17/oildrum001.mdl"),
        EntryClass::Model
    );
    assert_eq!(
        classify_entry_path("models/props_c17/oildrum001.vvd"),
        EntryClass::ModelCompanion
    );
    assert_eq!(
        classify_entry_path("models/props_c17/oildrum001.dx90.vtx"),
        EntryClass::ModelCompanion
    );
    assert_eq!(
        classify_entry_path("models/props_c17/oildrum001.dx80.vtx"),
        EntryClass::ModelCompanion
    );
    assert_eq!(
        classify_entry_path("models/props_c17/oildrum001.phy"),
        EntryClass::ModelCompanion
    );
    assert_eq!(
        classify_entry_path("models/props_c17/oildrum001.ani"),
        EntryClass::ModelCompanion
    );
    assert_eq!(
        classify_entry_path("maps/gm_construct.bsp"),
        EntryClass::Map
    );
    assert_eq!(
        classify_entry_path("materials/icon.png"),
        EntryClass::Image(ImageClass::Encoded)
    );
    assert_eq!(classify_entry_path("sound/music.wav"), EntryClass::Audio);
    assert_eq!(classify_entry_path("sound/music.MP3"), EntryClass::Audio);
    assert_eq!(classify_entry_path("sound/music.ogg"), EntryClass::Audio);
    assert_eq!(
        classify_entry_path("models/test/thing.dx90.ctx"),
        EntryClass::Info
    );
    assert_eq!(classify_entry_path("bin/blob.dat"), EntryClass::Info);
}

#[test]
fn vmt_highlighting_tracks_shader_keys_values_groups_and_comments() {
    let tokens = Tokens::classic_source();
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
    let tokens = Tokens::classic_source();
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
    let tokens = Tokens::classic_source();
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
    assert_eq!(redirected.entry_path, parent.path);
    assert_eq!(redirected.display_name, "Thing.MDL");
    assert_eq!(redirected.size_bytes, parent.size);
    assert_eq!(redirected.crc32, parent.crc32);
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

    // "Load anyway" consent skips the size gate and decodes (still
    // truncated for display).
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
fn map_uv_normalization_divides_raw_texels_by_resolved_dimensions() {
    assert_eq!(normalize_map_uv(1024.0, 256.0, 512, 128), [2.0, 2.0]);
    assert_eq!(normalize_map_uv(128.0, 64.0, 0, 0), [128.0, 64.0]);
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

    let skybox = resolve_skybox("sky_test", &resolver).expect("one face should resolve");

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
    assert!(resolve_skybox("sky_missing", &resolver).is_none());
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
            .entry("materials/test/shared.vtf", create_vtf_bytes(&[64; 16]))
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
fn decoded_texture_budget_counts_full_mip_chain_bytes() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Map Texture Budget Fixture")
            .entry(
                "materials/test/first.vmt",
                br#""LightmappedGeneric" { "$basetexture" "test/first" }"#.to_vec(),
            )
            .entry("materials/test/first.vtf", create_vtf_bytes(&[10; 16]))
            .build(),
    )
    .expect("fixture archive should load");
    let budget = Arc::new(DecodedTextureBudget::new(16));
    let resolver = MaterialResolver::new(Arc::new(archive), None)
        .with_decoded_texture_budget(Arc::clone(&budget));

    assert!(resolver.resolve(&[], "test/first").is_none());

    assert_eq!(budget.decoded_bytes(), 0);
    assert_eq!(budget.rejected_textures(), 1);
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
            .entry("materials/test/first.vtf", create_vtf_bytes(&[10; 16]))
            .entry("materials/test/second.vtf", create_vtf_bytes(&[20; 16]))
            .entry("materials/test/third.vtf", create_vtf_bytes(&[30; 16]))
            .build(),
    )
    .expect("fixture archive should load");
    let budget = Arc::new(DecodedTextureBudget::new(20));
    let resolver = MaterialResolver::new(Arc::new(archive), None)
        .with_decoded_texture_budget(Arc::clone(&budget));

    assert!(resolver.resolve(&[], "test/first").is_some());
    assert!(resolver.resolve(&[], "test/second").is_none());
    assert!(resolver.resolve(&[], "test/third").is_none());

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
        material_slot_signatures(&parallel.materials),
        material_slot_signatures(&serial.materials)
    );
    assert_eq!(
        parallel.resolved_material_count,
        serial.resolved_material_count
    );
    assert_eq!(
        parallel.water_fallback_material_count,
        serial.water_fallback_material_count
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
                create_vtf_bytes(&[index; 16]),
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
            .materials
            .iter()
            .filter(|slot| slot.texture.is_some())
            .count(),
        2
    );
    assert_eq!(budget.rejected_textures(), 2);
}

#[test]
fn prop_transform_applies_source_yaw_around_z_before_translation() {
    let placement = prop_placement(
        "models/test/chair.mdl",
        [10.0, 20.0, 30.0],
        [0.0, 90.0, 0.0],
        0,
    );

    assert_vec3_close(
        transform_prop_position([1.0, 0.0, 0.0], &placement),
        [10.0, 21.0, 30.0],
    );
    assert_vec3_close(
        transform_prop_normal([1.0, 0.0, 0.0], &placement),
        [0.0, 1.0, 0.0],
    );
}

#[test]
fn prop_transform_scales_positions_before_rotation_without_scaling_normals() {
    let mut placement = prop_placement(
        "models/test/chair.mdl",
        [10.0, 20.0, 30.0],
        [0.0, 90.0, 0.0],
        0,
    );
    placement.scale = 2.0;

    assert_vec3_close(
        transform_prop_position([1.0, 0.0, 0.0], &placement),
        [10.0, 22.0, 30.0],
    );
    assert_vec3_close(
        transform_prop_normal([1.0, 0.0, 0.0], &placement),
        [0.0, 1.0, 0.0],
    );
}

#[test]
fn prop_transform_composes_roll_before_yaw() {
    // yaw 90 + roll 90: Rx(roll) leaves +X alone, Rz(yaw) then sends it
    // to +Y. The reversed composition would produce +Z instead.
    let placement = prop_placement(
        "models/test/chair.mdl",
        [0.0, 0.0, 0.0],
        [0.0, 90.0, 90.0],
        0,
    );

    assert_vec3_close(
        transform_prop_normal([1.0, 0.0, 0.0], &placement),
        [0.0, 1.0, 0.0],
    );
}

#[test]
fn prop_lighting_adds_visible_sun_in_linear_space() {
    let lighting = PropPlacementLighting {
        ambient_cube: test_ambient_cube([0.1, 0.1, 0.1]),
        sun: Some(PropSunLighting {
            direction_to_sun: [0.0, 0.0, 1.0],
            color_linear: [0.5, 0.25, 0.0],
            visible: true,
        }),
    };

    // Up-facing vertex: ambient + full sun. Down-facing: ambient only —
    // there is deliberately no separate skylight term (the ambient cube
    // already integrates sky bounce).
    assert_vec3_close(lighting.evaluate([0.0, 0.0, 1.0]), [0.6, 0.35, 0.1]);
    assert_vec3_close(lighting.evaluate([0.0, 0.0, -1.0]), [0.1, 0.1, 0.1]);
}

#[test]
fn prop_lighting_keeps_shadowed_vertices_ambient_only() {
    let lighting = PropPlacementLighting {
        ambient_cube: test_ambient_cube([0.25, 0.2, 0.15]),
        sun: Some(PropSunLighting {
            direction_to_sun: [0.0, 0.0, 1.0],
            color_linear: [1.0, 1.0, 1.0],
            visible: false,
        }),
    };

    assert_vec3_close(lighting.evaluate([0.0, 0.0, 1.0]), [0.25, 0.2, 0.15]);
    assert_vec3_close(lighting.evaluate([1.0, 0.0, 0.0]), [0.25, 0.2, 0.15]);
}

#[test]
fn prop_bake_applies_skin_table_to_material_slot() {
    let asset = test_prop_asset(1, vec![10, 11]);
    let placement = prop_placement(
        "models/test/chair.mdl",
        [10.0, 20.0, 30.0],
        [0.0, 90.0, 0.0],
        1,
    );
    let mut meshes = BTreeMap::new();

    assert!(bake_prop_placement(
        &placement,
        &asset,
        PropPlacementLighting {
            ambient_cube: AmbientCube::WHITE,
            sun: None,
        },
        &mut meshes
    ));

    let mesh = meshes.get(&11).expect("skin remapped material");
    assert_eq!(mesh.indices, vec![0, 1, 2]);
    assert_vec3_close(mesh.vertices[0].position, [10.0, 21.0, 30.0]);
    assert_eq!(mesh.vertices[0].lightmap_uv, [0.0; 2]);
    assert_eq!(mesh.vertices[0].blend_alpha, 0.0);
}

#[test]
fn prop_bake_applies_modelscale_to_entity_prop_bounds() {
    let asset = test_prop_asset(1, vec![0, 1]);
    let mut placement = prop_placement("models/test/scaled_entity.mdl", [0.0; 3], [0.0; 3], 0);
    placement.scale = 2.0;
    let mut meshes = BTreeMap::new();

    assert!(bake_prop_placement(
        &placement,
        &asset,
        PropPlacementLighting {
            ambient_cube: AmbientCube::WHITE,
            sun: None,
        },
        &mut meshes
    ));

    let mesh = meshes.get(&0).expect("material mesh");
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in &mesh.vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.position[axis]);
            max[axis] = max[axis].max(vertex.position[axis]);
        }
    }
    assert_vec3_close(min, [0.0, 0.0, 0.0]);
    assert_vec3_close(max, [2.0, 2.0, 2.0]);
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
    let mut materials = vec![MaterialSlot {
        name: "door_skin".to_owned(),
        texture: None,
        texture2: None,
        force_opaque: true,
        render_mode: RenderMode::Opaque,
    }];
    let mut material_indexes = HashMap::from([("door_skin".to_owned(), 0_usize)]);
    let mut resolved_material_count = 0;
    let mut water_fallback_material_count = 0;
    let door = gmpublished_backend::scene::map::MapDoor {
        class: gmpublished_backend::scene::map::MapDoorClass::PropDoorRotating,
        origin: [0.0; 3],
        angles: [0.0; 3],
        local_bounds_min: [0.0; 3],
        local_bounds_max: [0.0; 3],
        visibility: MapVisibilityBucket::Always,
        wait: 0.0,
        initial_progress: 0.0,
        motion: gmpublished_backend::scene::map::MapDoorMotion::Rotating {
            angle_delta: [0.0, 90.0, 0.0],
            degrees: 90.0,
            speed: 100.0,
            open_direction: gmpublished_backend::scene::map::MapDoorOpenDirection::Both,
        },
        sounds: gmpublished_backend::scene::map::MapDoorSounds::default(),
        geometry: MapDoorGeometry::Prop {
            placement: prop_placement("models/test/door.mdl", [0.0; 3], [0.0; 3], 1),
        },
    };

    let result = bake_map_doors_with_prop_model_loader(
        &[door],
        &resolver,
        PropMaterialState {
            materials: &mut materials,
            material_indexes: &mut material_indexes,
            resolved_material_count: &mut resolved_material_count,
            water_fallback_material_count: &mut water_fallback_material_count,
        },
        StaticPropLightingInputs {
            ambient: &MapAmbientLighting::neutral(),
            environment_lighting: None,
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
    assert_eq!(materials[mesh.material_index].name, "door_skin");
    assert!(
        materials[mesh.material_index].texture.is_some(),
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
        prop_placement("models/test/chair.mdl", [0.0; 3], [0.0; 3], 0),
        prop_placement("models/test/chair.mdl", [1.0; 3], [0.0; 3], 0),
        prop_placement("models/test/chair.mdl", [2.0; 3], [0.0; 3], 0),
    ];
    let asset = Arc::new(test_prop_asset(1, vec![0, 1]));

    let result = bake_static_props_with_loader(
        &placements,
        2,
        100,
        StaticPropLightingInputs {
            ambient: &MapAmbientLighting::neutral(),
            environment_lighting: None,
            walk_collision: None,
        },
        |_| Some(Arc::clone(&asset)),
    );

    assert_eq!(result.placed_count, 2);
    assert_eq!(result.skipped_count, 1);
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
        prop_placement("models/test/chair.mdl", [0.0; 3], [0.0; 3], 0),
        prop_placement("models/test/chair.mdl", [1.0; 3], [0.0; 3], 0),
        prop_placement("models/test/chair.mdl", [2.0; 3], [0.0; 3], 0),
    ];
    let asset = Arc::new(test_prop_asset(2, vec![0, 1]));

    let result = bake_static_props_with_loader(
        &placements,
        10,
        3,
        StaticPropLightingInputs {
            ambient: &MapAmbientLighting::neutral(),
            environment_lighting: None,
            walk_collision: None,
        },
        |_| Some(Arc::clone(&asset)),
    );

    assert_eq!(result.placed_count, 1);
    assert_eq!(result.skipped_count, 2);
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
                [index as f32, index as f32 * 2.0, 0.0],
                [0.0, index as f32 * 15.0, 0.0],
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
            [placement_index as f32, placement_index as f32 * 2.0, 0.0],
            [0.0, placement_index as f32 * 15.0, 0.0],
            0,
            visibility,
        )
    }));
    // Two triangles over four shared vertices: vertex count (4) differs
    // from index count (6), so any merge that mixes up vertex and index
    // bases in the visibility ranges breaks the equality below.
    // The first four placements are sprp-owned and the last four are the
    // appended entity-prop range; visibility remains keyed by final index
    // ranges across that ownership boundary.
    let mut asset = test_prop_asset(2, vec![10, 11]);
    {
        let model = Arc::get_mut(&mut asset.model).expect("fresh model arc");
        model.meshes[0].vertices.push(model_vertex([1.0, 1.0, 0.0]));
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
            environment_lighting: None,
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
            environment_lighting: None,
            walk_collision: None,
        },
        |_| Some(Arc::clone(&asset)),
    );

    assert_eq!(parallel.placed_count, serial.placed_count);
    assert_eq!(parallel.skipped_count, serial.skipped_count);
    assert_eq!(parallel.meshes, serial.meshes);
    assert_eq!(parallel.mesh_visibility, serial.mesh_visibility);
    assert_eq!(parallel.skip_stats, serial.skip_stats);
    assert_eq!(parallel.mesh_bytes, serial.mesh_bytes);
    assert_ne!(parallel.meshes[0].vertices[0].color, [1.0; 3]);
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
        prop_placement("models/test/chair.mdl", [0.0; 3], [0.0; 3], 0),
        prop_placement("models/test/chair.mdl", [8.0, 0.0, 0.0], [0.0; 3], 0),
    ];
    let entity_props: Vec<StaticPropPlacement> = Vec::new();
    let mut combined = sprp_props.clone();
    combined.extend(entity_props);
    let asset = Arc::new(test_prop_asset(1, vec![0, 1]));
    let lighting = StaticPropLightingInputs {
        ambient: &MapAmbientLighting::neutral(),
        environment_lighting: None,
        walk_collision: None,
    };

    let sprp_only =
        bake_static_props_with_loader(&sprp_props, 32, 100, lighting, |_| Some(Arc::clone(&asset)));
    let with_empty_entity_range =
        bake_static_props_with_loader(&combined, 32, 100, lighting, |_| Some(Arc::clone(&asset)));

    assert_eq!(with_empty_entity_range.placed_count, sprp_only.placed_count);
    assert_eq!(
        with_empty_entity_range.skipped_count,
        sprp_only.skipped_count
    );
    assert_eq!(with_empty_entity_range.skip_stats, sprp_only.skip_stats);
    assert_eq!(with_empty_entity_range.mesh_bytes, sprp_only.mesh_bytes);
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
        prop_placement("models/test/a.mdl", [0.0; 3], [0.0; 3], 0),
        prop_placement("models/test/b.mdl", [2.0, 0.0, 0.0], [0.0; 3], 0),
        prop_placement("models/test/a.mdl", [4.0, 0.0, 0.0], [0.0; 3], 0),
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
    let mut direct_materials = Vec::new();
    let mut direct_material_indexes = HashMap::new();
    let mut direct_resolved_material_count = 0;
    let mut direct_water_fallback_material_count = 0;
    let direct = bake_static_props_with_loaded_model_cache(
        &placements,
        &direct_resolver,
        PropMaterialState {
            materials: &mut direct_materials,
            material_indexes: &mut direct_material_indexes,
            resolved_material_count: &mut direct_resolved_material_count,
            water_fallback_material_count: &mut direct_water_fallback_material_count,
        },
        &loaded_model_cache,
        false,
        &MapAmbientLighting::neutral(),
    );

    let pre_resolved_resolver = MaterialResolver::new(Arc::new(archive), None);
    let mut pre_resolved_materials = Vec::new();
    let mut pre_resolved_material_indexes = HashMap::new();
    let mut pre_resolved_material_count = 0;
    let mut pre_resolved_water_fallback_material_count = 0;
    let pre_resolved = bake_static_props_with_loaded_model_cache(
        &placements,
        &pre_resolved_resolver,
        PropMaterialState {
            materials: &mut pre_resolved_materials,
            material_indexes: &mut pre_resolved_material_indexes,
            resolved_material_count: &mut pre_resolved_material_count,
            water_fallback_material_count: &mut pre_resolved_water_fallback_material_count,
        },
        &loaded_model_cache,
        true,
        &MapAmbientLighting::neutral(),
    );

    assert_eq!(pre_resolved.placed_count, direct.placed_count);
    assert_eq!(pre_resolved.skipped_count, direct.skipped_count);
    assert_eq!(pre_resolved.meshes, direct.meshes);
    assert_eq!(
        material_slot_signatures(&pre_resolved_materials),
        material_slot_signatures(&direct_materials)
    );
    assert_eq!(pre_resolved_material_indexes, direct_material_indexes);
    assert_eq!(pre_resolved_material_count, direct_resolved_material_count);
    assert_eq!(
        pre_resolved_water_fallback_material_count,
        direct_water_fallback_material_count
    );
    assert_eq!(
        direct_materials
            .iter()
            .find(|material| material.name == "shared")
            .and_then(|material| material.texture.as_ref())
            .and_then(|texture| texture.rgba_bytes().map(Vec::from)),
        Some(vec![
            10, 10, 10, 255, 10, 10, 10, 255, 10, 10, 10, 255, 10, 10, 10, 255
        ])
    );
}

#[test]
fn missing_static_prop_model_is_skipped_and_counted() {
    let archive = PreviewArchive::from_gma(GmaFixtureBuilder::new("Empty").build())
        .expect("empty fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);
    let mut materials = Vec::new();
    let mut material_indexes = HashMap::new();
    let mut resolved_material_count = 0;
    let mut water_fallback_material_count = 0;

    let result = bake_static_props(
        &[prop_placement(
            "models/test/missing.mdl",
            [0.0; 3],
            [0.0; 3],
            0,
        )],
        &resolver,
        &mut materials,
        &mut material_indexes,
        &mut resolved_material_count,
        &mut water_fallback_material_count,
        &MapAmbientLighting::neutral(),
    );

    assert_eq!(result.placed_count, 0);
    assert_eq!(result.skipped_count, 1);
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
fn map_preview_with_panicking_static_prop_model_still_returns_map() {
    let bytes = static_prop_bsp_fixture_bytes();
    let request = request("maps/test.bsp", &bytes);
    let data =
        map_preview_data_with_prop_model_loader(&request, &bytes, None, &mut |_| {}, &|_, _| {
            panic!("fixture model panic")
        });

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
            name: format!("missing_{index:02}"),
            texture: None,
            texture2: None,
            force_opaque: true,
            render_mode: RenderMode::Opaque,
        })
        .collect::<Vec<_>>();
    materials.push(MaterialSlot {
        name: "missing_03".to_owned(),
        texture: None,
        texture2: None,
        force_opaque: true,
        render_mode: RenderMode::Opaque,
    });

    let (names, total) = unresolved_material_names_for_debug(&materials);

    assert_eq!(total, 25);
    assert_eq!(names.len(), 20);
    assert_eq!(names[0], "missing_00");
    assert_eq!(names[19], "missing_19");
}

fn prop_placement(
    model_path: &str,
    origin: [f32; 3],
    angles: [f32; 3],
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
    origin: [f32; 3],
    angles: [f32; 3],
    skin: i32,
    visibility: impl Into<MapPropVisibility>,
) -> StaticPropPlacement {
    StaticPropPlacement {
        model_path: model_path.to_owned(),
        origin,
        angles,
        skin,
        scale: 1.0,
        solid: gmpublished_backend::scene::map::MapPropSolid::None,
        visibility: visibility.into(),
    }
}

fn test_prop_asset(default_triangle_count: usize, material_indices: Vec<usize>) -> PropModelAsset {
    PropModelAsset {
        model: Arc::new(ModelData {
            meshes: vec![MeshData {
                vertices: vec![
                    model_vertex([1.0, 0.0, 0.0]),
                    model_vertex([0.0, 1.0, 0.0]),
                    model_vertex([0.0, 0.0, 1.0]),
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
            bounds_min: [0.0; 3],
            bounds_max: [1.0; 3],
            bone_count: 0,
            sequence_count: 0,
            vertex_count: 3,
            triangle_count: 1,
        }),
        material_indices,
        default_triangle_count,
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
            meshes: vec![MeshData {
                vertices: vec![
                    model_vertex([1.0, 0.0, 0.0]),
                    model_vertex([0.0, 1.0, 0.0]),
                    model_vertex([0.0, 0.0, 1.0]),
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
            bounds_min: [0.0; 3],
            bounds_max: [1.0; 3],
            bone_count: 0,
            sequence_count: 0,
            vertex_count: 3,
            triangle_count: 1,
        }),
        default_triangle_count: 1,
        physics: LoadedPropPhysics::Missing,
        collision: None,
    }
}

fn model_vertex(position: [f32; 3]) -> ModelVertex {
    ModelVertex {
        position,
        normal: [1.0, 0.0, 0.0],
        uv: [0.25, 0.75],
        lightmap_uv: [0.5, 0.5],
        color: [1.0; 3],
        blend_alpha: 1.0,
    }
}

fn test_ambient_cube(color: [f32; 3]) -> AmbientCube {
    AmbientCube { colors: [color; 6] }
}

fn assert_vec3_close(actual: [f32; 3], expected: [f32; 3]) {
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
    push_bsp_vec3(&mut bytes, [0.0, 0.0, 1.0]);
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
    gmpublished_backend::scene::map::load_map(&ambient_static_prop_bsp_fixture_bytes())
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
    push_bsp_vec3(&mut bytes, [10.0, 20.0, 30.0]);
    push_bsp_vec3(&mut bytes, [1.0, 90.0, 3.0]);
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.push(0);
    bytes.push(0);
    bytes.extend_from_slice(&2_i32.to_le_bytes());
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    push_bsp_vec3(&mut bytes, [0.0; 3]);
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes
}

fn push_bsp_vec3(bytes: &mut Vec<u8>, values: [f32; 3]) {
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
                material.force_opaque,
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
    let image = RgbaImage::from_raw(2, 2, rgba.to_vec()).expect("fixture rgba image");
    ::vtf::create(
        DynamicImage::ImageRgba8(image),
        ::vtf::ImageFormat::Rgba8888,
    )
    .expect("fixture vtf should encode")
}
