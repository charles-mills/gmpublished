use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use image::{DynamicImage, RgbaImage};
use lzma_rust2::{LzmaOptions, LzmaWriter};

use super::*;
use crate::{
    backend::gma::PreviewArchive,
    test_support::{GmaFixtureBuilder, write_gma_fixture},
};

const VPK_SIGNATURE: u32 = 0x55aa1234;
const VPK_DIR_ARCHIVE_INDEX: u16 = 0x7fff;
const VPK_ENTRY_TERMINATOR: u16 = 0xffff;

#[test]
fn resolves_addon_material_vmt_to_decoded_vtf() {
    let rgba = vec![
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
    ];
    let archive = archive_with_texture(
        "materials/models/test/thing.vmt",
        r#""VertexlitGeneric" { "$basetexture" "models/test/thing_color" }"#,
        "materials/models/test/thing_color.vtf",
        &rgba,
    );
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let texture = resolver
        .resolve(&["models/test".to_owned()], "thing")
        .expect("addon material should resolve");

    assert_eq!(texture.width, 2);
    assert_eq!(texture.height, 2);
    assert_eq!(texture_rgba(&texture), rgba);
    assert_eq!(texture.mip_level_count(), 2);
}

#[test]
fn mip_chain_dimensions_round_up_odd_edges_to_one() {
    let five_by_three = vec![128_u8; 5 * 3 * 4];
    let levels = generate_srgb_mip_chain(&five_by_three, 5, 3).expect("valid 5x3 mip chain");
    assert_eq!(
        levels
            .iter()
            .map(|level| (level.width, level.height))
            .collect::<Vec<_>>(),
        vec![(3, 2), (2, 1), (1, 1)]
    );

    let one_by_five = vec![128_u8; 5 * 4];
    let levels = generate_srgb_mip_chain(&one_by_five, 1, 5).expect("valid 1x5 mip chain");
    assert_eq!(
        levels
            .iter()
            .map(|level| (level.width, level.height))
            .collect::<Vec<_>>(),
        vec![(1, 3), (1, 2), (1, 1)]
    );
}

#[test]
fn mip_chain_averages_srgb_bytes_in_linear_space() {
    let rgba = vec![0, 0, 0, 255, 255, 255, 255, 255];
    let levels = generate_srgb_mip_chain(&rgba, 2, 1).expect("valid 2x1 mip chain");

    assert_eq!((levels[0].width, levels[0].height), (1, 1));
    assert_eq!(&levels[0].rgba[..3], &[188, 188, 188]);
    assert_eq!(levels[0].rgba[3], 255);
}

#[test]
fn addon_material_wins_over_game_vpk_material() {
    let addon_rgba = vec![
        255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    ];
    let vpk_rgba = vec![
        0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255,
    ];
    let archive = archive_with_texture(
        "materials/models/test/thing.vmt",
        r#""VertexlitGeneric" { "$basetexture" "models/test/addon_color" }"#,
        "materials/models/test/addon_color.vtf",
        &addon_rgba,
    );
    let gmod_dir = tempfile::TempDir::new().expect("temp gmod dir");
    write_vpk_fixture(
        &gmod_dir.path().join("garrysmod/pak01_dir.vpk"),
        vec![
            (
                "materials/models/test/thing.vmt",
                r#""VertexlitGeneric" { "$basetexture" "models/test/vpk_color" }"#
                    .as_bytes()
                    .to_vec(),
            ),
            (
                "materials/models/test/vpk_color.vtf",
                create_vtf_bytes(&vpk_rgba),
            ),
        ],
    );
    let resolver = MaterialResolver::new(Arc::new(archive), Some(gmod_dir.path().to_owned()));

    let texture = resolver
        .resolve(&["models/test".to_owned()], "thing")
        .expect("material should resolve");

    assert_eq!(texture_rgba(&texture), addon_rgba);
}

#[test]
fn prepended_material_wins_over_addon_material() {
    let addon_rgba = vec![
        255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    ];
    let prepended_rgba = vec![
        0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255,
    ];
    let archive = archive_with_texture(
        "materials/models/test/thing.vmt",
        r#""VertexlitGeneric" { "$basetexture" "models/test/addon_color" }"#,
        "materials/models/test/addon_color.vtf",
        &addon_rgba,
    );
    let resolver = MaterialResolver::with_prepended_source(
        Arc::new(archive),
        None,
        [
            (
                "materials/models/test/thing.vmt".to_owned(),
                r#""VertexlitGeneric" { "$basetexture" "models/test/prepended_color" }"#
                    .as_bytes()
                    .to_vec(),
            ),
            (
                "materials/models/test/prepended_color.vtf".to_owned(),
                create_vtf_bytes(&prepended_rgba),
            ),
        ],
    );

    let texture = resolver
        .resolve(&["models/test".to_owned()], "thing")
        .expect("prepended material should resolve");

    assert_eq!(texture_rgba(&texture), prepended_rgba);
}

#[test]
fn pak_source_lookup_is_case_insensitive_and_decodes_lzma_entries() {
    let stored_bytes = br#""LightmappedGeneric" {}"#.to_vec();
    let lzma_bytes = b"method 14 bytes".to_vec();
    let pakfile = MapPakFile::from_pak_bytes(zip_fixture([
        zip_stored_entry("Materials/Test/Thing.VMT", stored_bytes.clone()),
        zip_lzma_entry("materials/test/thing.vtf", lzma_bytes.clone()),
    ]))
    .expect("pak fixture should parse");
    let source = PakSource::new(pakfile).expect("pak source should index");

    assert_eq!(
        source
            .entry_bytes("materials/test/thing.vmt")
            .expect("stored vmt"),
        stored_bytes
    );
    assert_eq!(
        source
            .entry_bytes("MATERIALS/TEST/THING.VTF")
            .expect("lzma vtf"),
        lzma_bytes
    );
    assert!(source.entry_bytes("materials/test/missing.vmt").is_none());
}

#[test]
fn malformed_pak_source_degrades_to_absent_source() {
    let pakfile = MapPakFile::from_pak_bytes(b"not a zip".to_vec())
        .expect("malformed central directory is tolerated");

    assert!(PakSource::new(pakfile).is_none());
}

#[test]
fn resolves_patch_include_with_overrides_across_sources() {
    let rgba = vec![
        40, 80, 120, 255, 40, 80, 120, 255, 40, 80, 120, 255, 40, 80, 120, 255,
    ];
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Patch Fixture")
            .entry(
                "materials/models/test/thing.vmt",
                br#"
                patch
                {
                    include "materials/models/test/base.vmt"
                    insert { "$surfaceprop" "metal" }
                    replace { "$surfaceprop" "concrete" }
                }
                "#
                .to_vec(),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::with_prepended_source(
        Arc::new(archive),
        None,
        [
            (
                "materials/models/test/base.vmt".to_owned(),
                br#""VertexlitGeneric" { "$basetexture" "models/test/base_color" }"#.to_vec(),
            ),
            (
                "materials/models/test/base_color.vtf".to_owned(),
                create_vtf_bytes(&rgba),
            ),
        ],
    );

    let texture = resolver
        .resolve(&["models/test".to_owned()], "thing")
        .expect("patch material should resolve through included base");

    assert_eq!(texture_rgba(&texture), rgba);
}

#[test]
fn patch_basetexture_override_wins_over_include() {
    let base_rgba = vec![
        30, 30, 30, 255, 30, 30, 30, 255, 30, 30, 30, 255, 30, 30, 30, 255,
    ];
    let override_rgba = vec![
        220, 180, 40, 255, 220, 180, 40, 255, 220, 180, 40, 255, 220, 180, 40, 255,
    ];
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Patch Override Fixture")
            .entry(
                "materials/models/test/thing.vmt",
                br#"
                patch
                {
                    include "materials/models/test/base.vmt"
                    insert { "$basetexture" "models/test/base_color" }
                    replace { "$basetexture" "models/test/override_color" }
                }
                "#
                .to_vec(),
            )
            .entry(
                "materials/models/test/base.vmt",
                br#""VertexlitGeneric" { "$basetexture" "models/test/base_color" }"#.to_vec(),
            )
            .entry(
                "materials/models/test/base_color.vtf",
                create_vtf_bytes(&base_rgba),
            )
            .entry(
                "materials/models/test/override_color.vtf",
                create_vtf_bytes(&override_rgba),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let texture = resolver
        .resolve(&["models/test".to_owned()], "thing")
        .expect("patch material should resolve with override basetexture");

    assert_eq!(texture_rgba(&texture), override_rgba);
}

#[test]
fn resolves_basetexture2_for_world_vertex_transition() {
    let base_rgba = vec![
        10, 40, 70, 255, 10, 40, 70, 255, 10, 40, 70, 255, 10, 40, 70, 255,
    ];
    let base2_rgba = vec![
        200, 160, 120, 255, 200, 160, 120, 255, 200, 160, 120, 255, 200, 160, 120, 255,
    ];
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Base2 Fixture")
            .entry(
                "materials/nature/blend.vmt",
                br#"
                WorldVertexTransition
                {
                    "$basetexture" "nature/base_color"
                    "$basetexture2" "nature/base2_color"
                }
                "#
                .to_vec(),
            )
            .entry(
                "materials/nature/base_color.vtf",
                create_vtf_bytes(&base_rgba),
            )
            .entry(
                "materials/nature/base2_color.vtf",
                create_vtf_bytes(&base2_rgba),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let textures = resolver
        .resolve_with_base2(&[], "nature/blend")
        .expect("base2 material should resolve");

    assert_eq!(texture_rgba(&textures.texture.expect("base")), base_rgba);
    assert_eq!(texture_rgba(&textures.texture2.expect("base2")), base2_rgba);
}

#[test]
fn decoded_texture_cache_reuses_shared_vtf_path() {
    let rgba = vec![
        25, 50, 75, 255, 25, 50, 75, 255, 25, 50, 75, 255, 25, 50, 75, 255,
    ];
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Shared Texture Fixture")
            .entry(
                "materials/models/test/first.vmt",
                br#""VertexlitGeneric" { "$basetexture" "models/test/shared_color" }"#.to_vec(),
            )
            .entry(
                "materials/models/test/second.vmt",
                br#""VertexlitGeneric" { "$basetexture" "models/test/shared_color" }"#.to_vec(),
            )
            .entry(
                "materials/models/test/shared_color.vtf",
                create_vtf_bytes(&rgba),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let first = resolver
        .resolve(&["models/test".to_owned()], "first")
        .expect("first material should resolve");
    let second = resolver
        .resolve(&["models/test".to_owned()], "second")
        .expect("second material should resolve");

    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn alphatest_preserves_alpha_and_separates_decoded_cache_key() {
    let rgba = vec![
        25, 50, 75, 0, 25, 50, 75, 64, 25, 50, 75, 128, 25, 50, 75, 255,
    ];
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Alpha Texture Fixture")
            .entry(
                "materials/models/test/cutout.vmt",
                br#""VertexlitGeneric" { "$basetexture" "models/test/shared_alpha" "$alphatest" "1" }"#.to_vec(),
            )
            .entry(
                "materials/models/test/opaque.vmt",
                br#""VertexlitGeneric" { "$basetexture" "models/test/shared_alpha" }"#.to_vec(),
            )
            .entry(
                "materials/models/test/shared_alpha.vtf",
                create_vtf_bytes(&rgba),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let cutout = resolver
        .resolve(&["models/test".to_owned()], "cutout")
        .expect("alphatest material should resolve");
    let opaque = resolver
        .resolve(&["models/test".to_owned()], "opaque")
        .expect("opaque material should resolve");

    assert!(!Arc::ptr_eq(&cutout, &opaque));
    assert_eq!(texture_rgba(&cutout), rgba);
    assert!(
        opaque
            .rgba_bytes()
            .expect("opaque rgba")
            .iter()
            .skip(3)
            .step_by(4)
            .all(|alpha| *alpha == 255)
    );
    assert_eq!(
        opaque
            .rgba_bytes()
            .expect("opaque rgba")
            .chunks_exact(4)
            .map(|pixel| &pixel[..3])
            .collect::<Vec<_>>(),
        rgba.chunks_exact(4)
            .map(|pixel| &pixel[..3])
            .collect::<Vec<_>>()
    );
}

#[test]
fn render_mode_precedence_follows_vmt_flags() {
    let rgba = vec![
        25, 50, 75, 128, 25, 50, 75, 255, 25, 50, 75, 64, 25, 50, 75, 0,
    ];
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Render Mode Fixture")
            .entry(
                "materials/test/opaque.vmt",
                br#""VertexlitGeneric" { "$basetexture" "test/shared" }"#.to_vec(),
            )
            .entry(
                "materials/test/cutout.vmt",
                br#""VertexlitGeneric" { "$basetexture" "test/shared" "$alphatest" "1" }"#.to_vec(),
            )
            .entry(
                "materials/test/translucent.vmt",
                br#""VertexlitGeneric" { "$basetexture" "test/shared" "$translucent" "1" "$alphatest" "1" }"#.to_vec(),
            )
            .entry(
                "materials/test/additive.vmt",
                br#""VertexlitGeneric" { "$basetexture" "test/shared" "$additive" "1" "$translucent" "1" "$alphatest" "1" }"#.to_vec(),
            )
            .entry("materials/test/shared.vtf", create_vtf_bytes(&rgba))
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let opaque = resolver
        .resolve_primary(&[], "test/opaque")
        .expect("opaque");
    let cutout = resolver
        .resolve_primary(&[], "test/cutout")
        .expect("cutout");
    let translucent = resolver
        .resolve_primary(&[], "test/translucent")
        .expect("translucent");
    let additive = resolver
        .resolve_primary(&[], "test/additive")
        .expect("additive");

    assert_eq!(opaque.render_mode, RenderMode::Opaque);
    assert_eq!(cutout.render_mode, RenderMode::Cutout);
    assert_eq!(translucent.render_mode, RenderMode::Translucent);
    assert_eq!(additive.render_mode, RenderMode::Additive);
    assert!(opaque.force_opaque);
    assert!(!cutout.force_opaque);
    assert!(!translucent.force_opaque);
    assert!(!additive.force_opaque);
    assert!(
        opaque
            .texture
            .rgba_bytes()
            .expect("opaque rgba")
            .iter()
            .skip(3)
            .step_by(4)
            .all(|alpha| *alpha == 255)
    );
    assert_eq!(texture_rgba(&translucent.texture), rgba);
    assert_eq!(texture_rgba(&additive.texture), rgba);
}

#[test]
fn non_alphatest_material_forces_decoded_alpha_opaque() {
    let rgba = vec![
        240, 10, 20, 0, 30, 220, 40, 16, 50, 60, 210, 128, 70, 80, 90, 254,
    ];
    let archive = archive_with_texture(
        "materials/models/test/thing.vmt",
        r#""VertexlitGeneric" { "$basetexture" "models/test/thing_color" }"#,
        "materials/models/test/thing_color.vtf",
        &rgba,
    );
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let texture = resolver
        .resolve(&["models/test".to_owned()], "thing")
        .expect("material should resolve");

    assert!(
        texture
            .rgba_bytes()
            .expect("rgba texture")
            .iter()
            .skip(3)
            .step_by(4)
            .all(|alpha| *alpha == 255)
    );
}

#[test]
fn decoded_texture_downscale_caps_edges_preserves_aspect_and_never_upscales() {
    assert_eq!(
        downscaled_texture_dimensions(2048, 1024, 512),
        Some((512, 256))
    );
    assert_eq!(
        downscaled_texture_dimensions(1024, 2048, 512),
        Some((256, 512))
    );
    assert_eq!(downscaled_texture_dimensions(128, 64, 512), Some((128, 64)));
}

#[test]
fn decoded_texture_cap_downscales_resolved_vtf() {
    let rgba = vec![128_u8; 4 * 2 * 4];
    let archive = archive_with_texture_with_dimensions(
        "materials/models/test/thing.vmt",
        r#""VertexlitGeneric" { "$basetexture" "models/test/thing_color" }"#,
        "materials/models/test/thing_color.vtf",
        4,
        2,
        &rgba,
    );
    let base_resolver = MaterialResolver::new(Arc::new(archive), None);
    let resolver = base_resolver.with_decoded_texture_max_dimension(2);

    let texture = resolver
        .resolve(&["models/test".to_owned()], "thing")
        .expect("material should resolve");

    assert_eq!((texture.width, texture.height), (2, 1));
    assert_eq!(texture.rgba_bytes().expect("rgba texture").len(), 2 * 4);
    // BSP texel UVs must keep normalizing against the source size, or
    // downscaled map textures tile too often (the six-lane road bug).
    assert_eq!(texture.original_dimensions(), (4, 2));
}

#[test]
fn bc_mip_drop_caps_to_max_dimension_and_clamps_at_last_mip() {
    let dropped = drop_bc_mips_to_max_dimension(
        vec![
            resolved_bc_mip(1024, 1024, 1),
            resolved_bc_mip(512, 512, 2),
            resolved_bc_mip(256, 256, 3),
        ],
        Some(512),
    );
    assert_eq!(
        dropped
            .iter()
            .map(|mip| (mip.width, mip.height, mip.data[0]))
            .collect::<Vec<_>>(),
        vec![(512, 512, 2), (256, 256, 3)]
    );

    let clamped = drop_bc_mips_to_max_dimension(vec![resolved_bc_mip(1024, 1024, 9)], Some(512));
    assert_eq!((clamped[0].width, clamped[0].height), (1024, 1024));
}

#[test]
fn bc_resolution_reorders_vtf_mips_largest_first() {
    let smallest = vec![1_u8; 8];
    let middle = vec![2_u8; 8];
    let largest = vec![3_u8; 16];
    let bytes = create_bc_vtf_bytes(
        8,
        4,
        ::vtf::ImageFormat::Dxt1,
        &[smallest.as_slice(), middle.as_slice(), largest.as_slice()],
    );

    let texture = resolved_bc_texture(&bytes, None).expect("BC texture");
    let (format, mips) = texture.bc_payload().expect("BC payload");

    assert_eq!(format, BcFormat::Bc1);
    assert_eq!(
        mips.iter()
            .map(|mip| (mip.width, mip.height, mip.data[0]))
            .collect::<Vec<_>>(),
        vec![(8, 4, 3), (4, 2, 2), (2, 1, 1)]
    );
    assert_eq!(texture.original_dimensions(), (8, 4));
}

#[test]
fn bc_texture_budget_counts_compressed_mip_bytes() {
    let block = solid_bc1_red_block();
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("BC Budget Fixture")
            .entry(
                "materials/test/first.vmt",
                br#""LightmappedGeneric" { "$basetexture" "test/first" }"#.to_vec(),
            )
            .entry(
                "materials/test/first.vtf",
                create_bc_vtf_bytes(4, 4, ::vtf::ImageFormat::Dxt1, &[block.as_slice()]),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let budget = Arc::new(DecodedTextureBudget::new(8));
    let resolver = MaterialResolver::new(Arc::new(archive), None)
        .with_bc_texture_support(true)
        .with_decoded_texture_budget(Arc::clone(&budget));

    let texture = resolver
        .resolve(&[], "test/first")
        .expect("BC material should resolve");

    assert!(texture.is_bc());
    assert_eq!(budget.decoded_bytes(), 8);
    assert_eq!(budget.rejected_textures(), 0);
}

#[test]
fn bc_texture_cache_reuses_path_across_alpha_modes() {
    let block = solid_bc1_red_block();
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("BC Alpha Fixture")
            .entry(
                "materials/models/test/cutout.vmt",
                br#""VertexlitGeneric" { "$basetexture" "models/test/shared_dxt" "$alphatest" "1" }"#.to_vec(),
            )
            .entry(
                "materials/models/test/opaque.vmt",
                br#""VertexlitGeneric" { "$basetexture" "models/test/shared_dxt" }"#.to_vec(),
            )
            .entry(
                "materials/models/test/shared_dxt.vtf",
                create_bc_vtf_bytes(4, 4, ::vtf::ImageFormat::Dxt1, &[block.as_slice()]),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None).with_bc_texture_support(true);

    let cutout = resolver
        .resolve_primary(&["models/test".to_owned()], "cutout")
        .expect("cutout material");
    let opaque = resolver
        .resolve_primary(&["models/test".to_owned()], "opaque")
        .expect("opaque material");

    assert!(!cutout.force_opaque);
    assert!(opaque.force_opaque);
    assert!(Arc::ptr_eq(&cutout.texture, &opaque.texture));
    assert!(cutout.texture.is_bc());
}

#[test]
fn dxt1_material_resolves_to_bc_and_rgb888_stays_rgba() {
    let block = solid_bc1_red_block();
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("BC Material Fixture")
            .entry(
                "materials/test/dxt.vmt",
                br#""LightmappedGeneric" { "$basetexture" "test/dxt" }"#.to_vec(),
            )
            .entry(
                "materials/test/rgb.vmt",
                br#""LightmappedGeneric" { "$basetexture" "test/rgb" }"#.to_vec(),
            )
            .entry(
                "materials/test/dxt.vtf",
                create_bc_vtf_bytes(4, 4, ::vtf::ImageFormat::Dxt1, &[block.as_slice()]),
            )
            .entry(
                "materials/test/rgb.vtf",
                create_rgb888_vtf_bytes(2, 2, &[10, 20, 30, 40, 50, 60, 70, 80, 90, 1, 2, 3]),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None).with_bc_texture_support(true);

    let dxt = resolver.resolve(&[], "test/dxt").expect("DXT material");
    let rgb = resolver.resolve(&[], "test/rgb").expect("RGB material");

    assert!(dxt.is_bc());
    assert_eq!(dxt.original_dimensions(), (4, 4));
    assert!(!rgb.is_bc());
    assert_eq!(rgb.original_dimensions(), (2, 2));
    assert_eq!(rgb.rgba_bytes().expect("RGB888 decoded to RGBA").len(), 16);
}

#[test]
fn patch_basetexture2_override_wins_over_include() {
    let base_rgba = vec![
        30, 60, 90, 255, 30, 60, 90, 255, 30, 60, 90, 255, 30, 60, 90, 255,
    ];
    let include_base2_rgba = vec![
        80, 80, 80, 255, 80, 80, 80, 255, 80, 80, 80, 255, 80, 80, 80, 255,
    ];
    let override_base2_rgba = vec![
        210, 170, 130, 255, 210, 170, 130, 255, 210, 170, 130, 255, 210, 170, 130, 255,
    ];
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Patch Base2 Override Fixture")
            .entry(
                "materials/nature/blend.vmt",
                br#"
                patch
                {
                    include "materials/nature/base.vmt"
                    replace { "$basetexture2" "nature/override_base2" }
                }
                "#
                .to_vec(),
            )
            .entry(
                "materials/nature/base.vmt",
                br#"
                WorldVertexTransition
                {
                    "$basetexture" "nature/base_color"
                    "$basetexture2" "nature/include_base2"
                }
                "#
                .to_vec(),
            )
            .entry(
                "materials/nature/base_color.vtf",
                create_vtf_bytes(&base_rgba),
            )
            .entry(
                "materials/nature/include_base2.vtf",
                create_vtf_bytes(&include_base2_rgba),
            )
            .entry(
                "materials/nature/override_base2.vtf",
                create_vtf_bytes(&override_base2_rgba),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let textures = resolver
        .resolve_with_base2(&[], "nature/blend")
        .expect("patch base2 material should resolve");

    assert_eq!(texture_rgba(&textures.texture.expect("base")), base_rgba);
    assert_eq!(
        texture_rgba(&textures.texture2.expect("base2")),
        override_base2_rgba
    );
}

#[test]
fn water_fogcolor_parsing_handles_brackets_braces_and_garbage() {
    assert_eq!(
        water_fog_rgba(Some("[0.05 0.08 0.07]")),
        [
            linear_to_srgb_byte(0.05),
            linear_to_srgb_byte(0.08),
            linear_to_srgb_byte(0.07),
            255
        ]
    );
    assert_eq!(
        water_fog_rgba(Some("{13 20 18}")),
        [
            linear_to_srgb_byte(13.0 / 255.0),
            linear_to_srgb_byte(20.0 / 255.0),
            linear_to_srgb_byte(18.0 / 255.0),
            255
        ]
    );
    assert_eq!(
        water_fog_rgba(Some("garbage")),
        [
            linear_to_srgb_byte(DEFAULT_WATER_FOG_LINEAR[0]),
            linear_to_srgb_byte(DEFAULT_WATER_FOG_LINEAR[1]),
            linear_to_srgb_byte(DEFAULT_WATER_FOG_LINEAR[2]),
            255
        ]
    );
}

#[test]
fn water_shader_without_resolvable_basetexture_returns_tinted_slot() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Water Fixture")
            .entry(
                "materials/water/canal.vmt",
                br#"
                Water
                {
                    "$fogcolor" "[0.05 0.08 0.07]"
                }
                "#
                .to_vec(),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let texture = resolver
        .resolve(&[], "water/canal")
        .expect("water fallback should resolve");

    assert_eq!(texture.width, 1);
    assert_eq!(texture.height, 1);
    assert!(texture.is_water_fallback());
    assert_eq!(
        texture_rgba(&texture),
        water_fog_rgba(Some("[0.05 0.08 0.07]"))
    );
    assert_eq!(texture.mip_level_count(), 1);
}

#[test]
fn patch_include_water_shader_returns_tinted_slot() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Water Patch Fixture")
            .entry(
                "materials/water/patch.vmt",
                br#"
                patch
                {
                    include "materials/water/base.vmt"
                    replace { "$fogcolor" "{13 20 18}" }
                }
                "#
                .to_vec(),
            )
            .entry(
                "materials/water/base.vmt",
                br#"
                Water_DX90
                {
                    "$basetexture" "water/missing_texture"
                    "$fogcolor" "[0.40 0.40 0.40]"
                }
                "#
                .to_vec(),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let texture = resolver
        .resolve(&[], "water/patch")
        .expect("water patch fallback should resolve");

    assert!(texture.is_water_fallback());
    assert_eq!(texture_rgba(&texture), water_fog_rgba(Some("{13 20 18}")));
}

#[test]
fn patch_include_cycles_do_not_recurse_forever() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Patch Cycle Fixture")
            .entry(
                "materials/models/test/a.vmt",
                br#"patch { include "materials/models/test/b.vmt" }"#.to_vec(),
            )
            .entry(
                "materials/models/test/b.vmt",
                br#"patch { include "materials/models/test/a.vmt" }"#.to_vec(),
            )
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    assert!(resolver.resolve(&["models/test".to_owned()], "a").is_none());
}

#[test]
fn cubemap_depatch_fallback_resolves_original_material() {
    let rgba = vec![
        10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255,
    ];
    let archive = archive_with_texture(
        "materials/brick/wall.vmt",
        r#""LightmappedGeneric" { "$basetexture" "brick/wall_color" }"#,
        "materials/brick/wall_color.vtf",
        &rgba,
    );
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let texture = resolver
        .resolve(&[], "maps/gm_flatgrass/brick/wall_1_-2_3")
        .expect("cubemap-patched material should fall back to original");

    assert_eq!(texture_rgba(&texture), rgba);
}

#[test]
fn material_paths_only_depatch_map_coord_suffixes() {
    assert_eq!(
        material_paths(&[], "maps/gm_flatgrass/brick/wall_1_-2_3"),
        vec![
            "materials/maps/gm_flatgrass/brick/wall_1_-2_3.vmt".to_owned(),
            "materials/brick/wall.vmt".to_owned(),
        ]
    );
    assert_eq!(
        material_paths(&[], "maps/gm_flatgrass/brick/wall_2024"),
        vec!["materials/maps/gm_flatgrass/brick/wall_2024.vmt".to_owned()]
    );
    assert_eq!(
        material_paths(&[], "brick/wall_1_2_3"),
        vec!["materials/brick/wall_1_2_3.vmt".to_owned()]
    );
}

#[test]
fn resolves_loose_game_material_dirs() {
    let rgba = vec![
        200, 100, 50, 255, 200, 100, 50, 255, 200, 100, 50, 255, 200, 100, 50, 255,
    ];
    let gmod_dir = tempfile::TempDir::new().expect("temp gmod dir");
    let material_dir = gmod_dir.path().join("garrysmod/materials/models/test");
    fs::create_dir_all(&material_dir).expect("material dir");
    fs::write(
        material_dir.join("thing.vmt"),
        br#""VertexlitGeneric" { "$basetexture" "models/test/loose_color" }"#,
    )
    .expect("loose vmt");
    fs::write(
        material_dir.join("loose_color.vtf"),
        create_vtf_bytes(&rgba),
    )
    .expect("loose vtf");
    let resolver =
        MaterialResolver::new(Arc::new(empty_archive()), Some(gmod_dir.path().to_owned()));

    let texture = resolver
        .resolve(&["models/test".to_owned()], "thing")
        .expect("loose material should resolve");

    assert_eq!(texture_rgba(&texture), rgba);
}

#[test]
fn entry_bytes_reads_arbitrary_loose_game_content() {
    let gmod_dir = tempfile::TempDir::new().expect("temp gmod dir");
    let model_dir = gmod_dir.path().join("garrysmod/models/test");
    fs::create_dir_all(&model_dir).expect("model dir");
    fs::write(model_dir.join("chair.mdl"), b"mdl bytes").expect("loose mdl");
    let resolver =
        MaterialResolver::new(Arc::new(empty_archive()), Some(gmod_dir.path().to_owned()));

    assert_eq!(
        resolver
            .entry_bytes(r"\Models\Test\Chair.MDL")
            .expect("loose model should resolve"),
        b"mdl bytes"
    );
}

#[test]
fn resolves_raw_sound_wave_prefixes_case_insensitively() {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Sound Fixture")
            .entry("sound/doors/door1_move.wav", b"move bytes".to_vec())
            .build(),
    )
    .expect("fixture archive should load");
    let resolver = MaterialResolver::new(Arc::new(archive), None);

    let resolved = resolver
        .resolve_sound_reference(r"*#@Doors\Door1_Move.WAV")
        .expect("raw wave should resolve");

    assert_eq!(resolved.reference, r"*#@Doors\Door1_Move.WAV");
    assert_eq!(resolved.sound_level, soundscript::DEFAULT_SOUND_LEVEL_DB);
    assert_eq!(resolved.volume, 1.0);
    assert_eq!(resolved.waves.len(), 1);
    assert_eq!(resolved.waves[0].path, "sound/doors/door1_move.wav");
    assert_eq!(resolved.waves[0].source_tier, ContentSourceTier::Addon);
    assert_eq!(resolved.waves[0].bytes.as_slice(), b"move bytes");
}

#[test]
fn resolves_manifest_and_globbed_soundscripts_to_waves() {
    let resolver = MaterialResolver::with_prepended_source(
        Arc::new(empty_archive()),
        None,
        [
            (
                "scripts/game_sounds_manifest.txt".to_owned(),
                br"
                game_sounds_manifest
                {
                    precache_file scripts/game_sounds_doors.txt
                }
                "
                .to_vec(),
            ),
            (
                "scripts/game_sounds_doors.txt".to_owned(),
                br"
                DoorSound.DefaultMove
                {
                    volume 0.5
                    soundlevel SNDLVL_75dB
                    wave doors/default_move.wav
                }
                "
                .to_vec(),
            ),
            (
                "scripts/game_sounds_extra.txt".to_owned(),
                br"
                DoorSound.DefaultOpen
                {
                    rndwave
                    {
                        wave doors/default_open1.wav
                        wave doors/default_open2.wav
                    }
                }
                "
                .to_vec(),
            ),
            (
                "sound/doors/default_move.wav".to_owned(),
                b"default move".to_vec(),
            ),
            (
                "sound/doors/default_open1.wav".to_owned(),
                b"default open one".to_vec(),
            ),
            (
                "sound/doors/default_open2.wav".to_owned(),
                b"default open two".to_vec(),
            ),
        ],
    );

    let files = resolver.sound_script_files();
    assert!(
        files
            .iter()
            .any(|file| file == "scripts/game_sounds_doors.txt")
    );
    assert!(
        files
            .iter()
            .any(|file| file == "scripts/game_sounds_extra.txt")
    );

    let move_sound = resolver
        .resolve_sound_reference("DoorSound.DefaultMove")
        .expect("manifest soundscript should resolve");
    assert_eq!(move_sound.volume, 0.5);
    assert_eq!(move_sound.sound_level, 75.0);
    assert_eq!(move_sound.waves.len(), 1);
    assert_eq!(
        move_sound.waves[0].source_tier,
        ContentSourceTier::Prepended
    );
    assert_eq!(move_sound.waves[0].bytes.as_slice(), b"default move");

    let open_sound = resolver
        .resolve_sound_reference("doorsound.defaultopen")
        .expect("globbed soundscript should resolve case-insensitively");
    assert_eq!(
        open_sound
            .waves
            .iter()
            .map(|wave| wave.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "sound/doors/default_open1.wav",
            "sound/doors/default_open2.wav"
        ]
    );
}

#[test]
fn resolves_sibling_workshop_gma_materials() {
    let rgba = vec![
        90, 140, 190, 255, 90, 140, 190, 255, 90, 140, 190, 255, 90, 140, 190, 255,
    ];
    let steam = tempfile::TempDir::new().expect("temp steam dir");
    let gmod_dir = steam.path().join("steamapps/common/GarrysMod");
    let workshop_item = steam.path().join("steamapps/workshop/content/4000/12345");
    fs::create_dir_all(&gmod_dir).expect("gmod dir");
    fs::create_dir_all(&workshop_item).expect("workshop item dir");
    fs::write(workshop_item.join("bad.gma"), b"not a gma").expect("bad gma fixture");
    let sibling_gma = GmaFixtureBuilder::new("Sibling")
        .entry(
            "materials/models/test/thing.vmt",
            br#""VertexlitGeneric" { "$basetexture" "models/test/sibling_color" }"#.to_vec(),
        )
        .entry(
            "materials/models/test/sibling_color.vtf",
            create_vtf_bytes(&rgba),
        )
        .build();
    write_gma_fixture(workshop_item.join("sibling.gma"), &sibling_gma);
    let resolver = MaterialResolver::new(Arc::new(empty_archive()), Some(gmod_dir));

    let texture = resolver
        .resolve(&["models/test".to_owned()], "thing")
        .expect("sibling GMA material should resolve");

    assert_eq!(texture_rgba(&texture), rgba);
}

#[test]
fn sibling_workshop_plain_gma_beats_legacy_bin() {
    let steam = tempfile::TempDir::new().expect("temp steam dir");
    let gmod_dir = steam.path().join("steamapps/common/GarrysMod");
    let workshop_item = steam.path().join("steamapps/workshop/content/4000/12345");
    fs::create_dir_all(&gmod_dir).expect("gmod dir");
    fs::create_dir_all(&workshop_item).expect("workshop item dir");
    fs::write(workshop_item.join("12345_legacy.bin"), b"not lzma").expect("bin fixture");
    write_gma_fixture(
        workshop_item.join("plain.gma"),
        &GmaFixtureBuilder::new("Plain").build(),
    );

    let paths = discover_sibling_gma_paths(&gmod_dir);

    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0].path,
        fs::canonicalize(workshop_item.join("plain.gma")).expect("canonical plain gma")
    );
    assert_eq!(paths[0].kind, SiblingGmaPathKind::Plain);
}

#[test]
fn sibling_legacy_bin_index_reads_lzma_gma_fixture() {
    let rgba = vec![
        12, 34, 56, 255, 12, 34, 56, 255, 12, 34, 56, 255, 12, 34, 56, 255,
    ];
    let steam = tempfile::TempDir::new().expect("temp steam dir");
    let gmod_dir = steam.path().join("steamapps/common/GarrysMod");
    let workshop_item = steam.path().join("steamapps/workshop/content/4000/67890");
    fs::create_dir_all(&gmod_dir).expect("gmod dir");
    fs::create_dir_all(&workshop_item).expect("workshop item dir");
    let raw_gma_path = steam.path().join("legacy-source.gma");
    write_gma_fixture(
        &raw_gma_path,
        &GmaFixtureBuilder::new("Legacy Bin")
            .entry(
                "materials/models/test/thing.vmt",
                br#""VertexlitGeneric" { "$basetexture" "models/test/bin_color" }"#.to_vec(),
            )
            .entry(
                "materials/models/test/bin_color.vtf",
                create_vtf_bytes(&rgba),
            )
            .build(),
    );
    fs::write(
        workshop_item.join("67890_legacy.bin"),
        compress_lzma(&fs::read(&raw_gma_path).expect("raw gma bytes")),
    )
    .expect("legacy bin fixture");
    let paths = discover_sibling_gma_paths(&gmod_dir);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].kind, SiblingGmaPathKind::LegacyBin);
    let index = build_sibling_gma_index(&paths);

    assert_eq!(
        index
            .entry_bytes("materials/models/test/thing.vmt")
            .expect("indexed vmt"),
        br#""VertexlitGeneric" { "$basetexture" "models/test/bin_color" }"#
    );
    assert_eq!(
        decode_vtf_rgba(
            &index
                .entry_bytes("materials/models/test/bin_color.vtf")
                .expect("indexed vtf")
        )
        .expect("decoded vtf")
        .rgba,
        rgba
    );
}

#[test]
fn discovers_download_plain_gmas_to_depth_three() {
    let gmod_dir = tempfile::TempDir::new().expect("temp gmod dir");
    let download = gmod_dir.path().join("garrysmod/download/a/b/c");
    fs::create_dir_all(&download).expect("download dir");
    write_gma_fixture(
        download.join("server-pushed.gma"),
        &GmaFixtureBuilder::new("Downloaded").build(),
    );

    let paths = discover_sibling_gma_paths(gmod_dir.path());

    assert!(paths.iter().any(|path| {
        path.path
            .ends_with(Path::new("garrysmod/download/a/b/c/server-pushed.gma"))
    }));
}

fn archive_with_texture(
    vmt_path: &str,
    vmt_text: &str,
    vtf_path: &str,
    rgba: &[u8],
) -> PreviewArchive {
    archive_with_texture_with_dimensions(vmt_path, vmt_text, vtf_path, 2, 2, rgba)
}

fn archive_with_texture_with_dimensions(
    vmt_path: &str,
    vmt_text: &str,
    vtf_path: &str,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> PreviewArchive {
    PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Material Fixture")
            .entry(vmt_path, vmt_text.as_bytes().to_vec())
            .entry(
                vtf_path,
                create_vtf_bytes_with_dimensions(width, height, rgba),
            )
            .build(),
    )
    .expect("fixture archive should load")
}

fn empty_archive() -> PreviewArchive {
    PreviewArchive::from_gma(GmaFixtureBuilder::new("Empty").build())
        .expect("empty fixture archive should load")
}

fn create_vtf_bytes(rgba: &[u8]) -> Vec<u8> {
    create_vtf_bytes_with_dimensions(2, 2, rgba)
}

fn texture_rgba(texture: &ResolvedTexture) -> Vec<u8> {
    texture.rgba_bytes().expect("RGBA texture").to_vec()
}

fn resolved_bc_mip(width: u32, height: u32, marker: u8) -> ResolvedBcMip {
    ResolvedBcMip {
        data: vec![marker; 8],
        width,
        height,
    }
}

fn create_vtf_bytes_with_dimensions(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let image = RgbaImage::from_raw(width, height, rgba.to_vec()).expect("fixture rgba image");
    ::vtf::create(
        DynamicImage::ImageRgba8(image),
        ::vtf::ImageFormat::Rgba8888,
    )
    .expect("fixture vtf should encode")
}

fn solid_bc1_red_block() -> [u8; 8] {
    let mut block = [0_u8; 8];
    block[0..2].copy_from_slice(&0xf800_u16.to_le_bytes());
    block
}

fn create_bc_vtf_bytes(
    width: u16,
    height: u16,
    format: ::vtf::ImageFormat,
    stored_mips: &[&[u8]],
) -> Vec<u8> {
    let header = ::vtf::header::VTFHeader {
        signature: ::vtf::header::VTFHeader::SIGNATURE,
        version: [7, 1],
        header_size: 64,
        width,
        height,
        flags: 0,
        frames: 1,
        first_frame: 0,
        reflectivity: [0.0; 3],
        bumpmap_scale: 1.0,
        highres_image_format: format,
        mipmap_count: u8::try_from(stored_mips.len()).expect("fixture mip count"),
        lowres_image_format: ::vtf::ImageFormat::None,
        lowres_image_width: 0,
        lowres_image_height: 0,
        depth: 1,
        resources: ::vtf::resources::ResourceList::empty(),
    };
    let mut bytes = Vec::new();
    header.write(&mut bytes).expect("fixture header");
    bytes.resize(header.header_size as usize, 0);
    for mip in stored_mips {
        bytes.extend_from_slice(mip);
    }
    bytes
}

fn create_rgb888_vtf_bytes(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let image =
        ::image::RgbImage::from_raw(width, height, rgb.to_vec()).expect("fixture rgb image");
    ::vtf::create(DynamicImage::ImageRgb8(image), ::vtf::ImageFormat::Rgb888)
        .expect("fixture vtf should encode")
}

fn compress_lzma(input: &[u8]) -> Vec<u8> {
    let options = LzmaOptions::with_preset(1);
    let mut encoder =
        LzmaWriter::new_use_header(Vec::new(), &options, None).expect("fixture lzma encoder");
    encoder.write_all(input).expect("fixture lzma input");
    encoder.finish().expect("fixture lzma finish")
}

#[derive(Debug)]
struct ZipFixtureEntry {
    path: String,
    method: u16,
    uncompressed: Vec<u8>,
    compressed: Vec<u8>,
}

fn zip_stored_entry(path: &str, bytes: Vec<u8>) -> ZipFixtureEntry {
    ZipFixtureEntry {
        path: path.to_owned(),
        method: 0,
        uncompressed: bytes.clone(),
        compressed: bytes,
    }
}

fn zip_lzma_entry(path: &str, bytes: Vec<u8>) -> ZipFixtureEntry {
    let encoded = compress_lzma(&bytes);
    let lzma_header = encoded
        .get(..13)
        .expect("lzma-alone fixture header should be present");
    let mut compressed = Vec::new();
    compressed.extend_from_slice(&9_u16.to_le_bytes());
    compressed.extend_from_slice(&5_u16.to_le_bytes());
    compressed.extend_from_slice(&lzma_header[..5]);
    compressed.extend_from_slice(&encoded[13..]);
    ZipFixtureEntry {
        path: path.to_owned(),
        method: 14,
        uncompressed: bytes,
        compressed,
    }
}

fn zip_fixture(entries: impl IntoIterator<Item = ZipFixtureEntry>) -> Vec<u8> {
    let mut file = Vec::new();
    let mut central_entries = Vec::new();
    for entry in entries {
        let name = entry.path.as_bytes();
        let offset = u32::try_from(file.len()).expect("zip local offset");
        let crc = crc32fast::hash(&entry.uncompressed);
        let compressed_size = u32::try_from(entry.compressed.len()).expect("zip compressed size");
        let uncompressed_size =
            u32::try_from(entry.uncompressed.len()).expect("zip uncompressed size");
        let name_len = u16::try_from(name.len()).expect("zip name length");

        file.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        file.extend_from_slice(&20_u16.to_le_bytes());
        file.extend_from_slice(&0_u16.to_le_bytes());
        file.extend_from_slice(&entry.method.to_le_bytes());
        file.extend_from_slice(&0_u16.to_le_bytes());
        file.extend_from_slice(&0_u16.to_le_bytes());
        file.extend_from_slice(&crc.to_le_bytes());
        file.extend_from_slice(&compressed_size.to_le_bytes());
        file.extend_from_slice(&uncompressed_size.to_le_bytes());
        file.extend_from_slice(&name_len.to_le_bytes());
        file.extend_from_slice(&0_u16.to_le_bytes());
        file.extend_from_slice(name);
        file.extend_from_slice(&entry.compressed);

        central_entries.push((
            name.to_vec(),
            entry.method,
            crc,
            compressed_size,
            uncompressed_size,
            offset,
        ));
    }

    let central_offset = u32::try_from(file.len()).expect("zip central offset");
    for (name, method, crc, compressed_size, uncompressed_size, offset) in &central_entries {
        let name_len = u16::try_from(name.len()).expect("zip central name length");
        file.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        file.extend_from_slice(&20_u16.to_le_bytes());
        file.extend_from_slice(&20_u16.to_le_bytes());
        file.extend_from_slice(&0_u16.to_le_bytes());
        file.extend_from_slice(&method.to_le_bytes());
        file.extend_from_slice(&0_u16.to_le_bytes());
        file.extend_from_slice(&0_u16.to_le_bytes());
        file.extend_from_slice(&crc.to_le_bytes());
        file.extend_from_slice(&compressed_size.to_le_bytes());
        file.extend_from_slice(&uncompressed_size.to_le_bytes());
        file.extend_from_slice(&name_len.to_le_bytes());
        file.extend_from_slice(&0_u16.to_le_bytes());
        file.extend_from_slice(&0_u16.to_le_bytes());
        file.extend_from_slice(&0_u16.to_le_bytes());
        file.extend_from_slice(&0_u16.to_le_bytes());
        file.extend_from_slice(&0_u32.to_le_bytes());
        file.extend_from_slice(&offset.to_le_bytes());
        file.extend_from_slice(name);
    }
    let central_size = u32::try_from(file.len()).expect("zip end offset") - central_offset;
    let entry_count = u16::try_from(central_entries.len()).expect("zip entry count");

    file.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    file.extend_from_slice(&0_u16.to_le_bytes());
    file.extend_from_slice(&0_u16.to_le_bytes());
    file.extend_from_slice(&entry_count.to_le_bytes());
    file.extend_from_slice(&entry_count.to_le_bytes());
    file.extend_from_slice(&central_size.to_le_bytes());
    file.extend_from_slice(&central_offset.to_le_bytes());
    file.extend_from_slice(&0_u16.to_le_bytes());
    file
}

fn write_vpk_fixture(path: &Path, entries: Vec<(&str, Vec<u8>)>) -> PathBuf {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("vpk fixture parent");
    }

    let mut embedded_data = Vec::new();
    let prepared = entries
        .into_iter()
        .map(|(path, bytes)| {
            let (directory, filename, extension) = split_entry_path(path);
            let entry_offset = u32::try_from(embedded_data.len()).expect("fixture offset");
            let entry_length = u32::try_from(bytes.len()).expect("fixture len");
            embedded_data.extend_from_slice(&bytes);
            PreparedEntry {
                extension,
                directory,
                filename,
                crc: crc32fast::hash(&bytes),
                entry_offset,
                entry_length,
            }
        })
        .collect::<Vec<_>>();
    let tree = build_tree(prepared);

    let mut file = Vec::new();
    file.extend_from_slice(&VPK_SIGNATURE.to_le_bytes());
    file.extend_from_slice(&1_u32.to_le_bytes());
    file.extend_from_slice(&(u32::try_from(tree.len()).unwrap()).to_le_bytes());
    file.extend_from_slice(&tree);
    file.extend_from_slice(&embedded_data);

    fs::write(path, file).expect("vpk fixture file");
    path.to_owned()
}

#[derive(Debug)]
struct PreparedEntry {
    extension: String,
    directory: String,
    filename: String,
    crc: u32,
    entry_offset: u32,
    entry_length: u32,
}

fn split_entry_path(path: &str) -> (String, String, String) {
    let (directory, file_name) = path.rsplit_once('/').unwrap_or((" ", path));
    let directory = if directory.is_empty() { " " } else { directory }.to_owned();
    let (filename, extension) = file_name
        .rsplit_once('.')
        .map_or((file_name, " "), |(filename, extension)| {
            (filename, extension)
        });
    (directory, filename.to_owned(), extension.to_owned())
}

fn build_tree(entries: Vec<PreparedEntry>) -> Vec<u8> {
    let mut grouped = BTreeMap::<String, BTreeMap<String, Vec<PreparedEntry>>>::new();
    for entry in entries {
        grouped
            .entry(entry.extension.clone())
            .or_default()
            .entry(entry.directory.clone())
            .or_default()
            .push(entry);
    }

    let mut tree = Vec::new();
    for (extension, paths) in grouped {
        write_c_string(&mut tree, &extension);
        for (directory, entries) in paths {
            write_c_string(&mut tree, &directory);
            for entry in entries {
                write_c_string(&mut tree, &entry.filename);
                tree.extend_from_slice(&entry.crc.to_le_bytes());
                tree.extend_from_slice(&0_u16.to_le_bytes());
                tree.extend_from_slice(&VPK_DIR_ARCHIVE_INDEX.to_le_bytes());
                tree.extend_from_slice(&entry.entry_offset.to_le_bytes());
                tree.extend_from_slice(&entry.entry_length.to_le_bytes());
                tree.extend_from_slice(&VPK_ENTRY_TERMINATOR.to_le_bytes());
            }
            tree.push(0);
        }
        tree.push(0);
    }
    tree.push(0);
    tree
}

fn write_c_string(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(value.as_bytes());
    bytes.push(0);
}
