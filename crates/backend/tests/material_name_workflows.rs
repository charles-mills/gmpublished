use std::fs;

use gmpublished_backend::{
    model_compile::{ModelCompileOutput, ModelCompileRequest, ModelOutputKind, compile_model},
    model_decompile::{decompile_model, export_blender_model},
    model_project::save_model_qc,
};

fn mdl(output: &ModelCompileOutput) -> &std::path::Path {
    &output
        .files
        .iter()
        .find(|file| file.kind == ModelOutputKind::Mdl)
        .unwrap()
        .path
}

#[test]
fn material_names_survive_portable_qc_decompile_and_blender_roundtrips() {
    for name in [
        "02 - Default",
        "Стол основа",
        "123",
        "paint with spaces",
        "end",
        "Mat.001",
    ] {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("cube.smd");
        fs::write(
            &source,
            include_str!("../../vendor/mdlc/tests/fixtures/cube.smd")
                .replace("\nmaterial\n", &format!("\n\"{name}\"\n")),
        )
        .unwrap();
        let mut request = ModelCompileRequest::new(source, root.path().join("original"));
        request.static_prop = true;
        let original = compile_model(&request).unwrap();
        assert_eq!(original.materials, [name]);
        let saved = root.path().join("portable");
        fs::create_dir(&saved).unwrap();
        save_model_qc(&request, &saved.join("prop.qc")).unwrap();
        let decompiled = decompile_model(mdl(&original), &root.path().join("decompiled")).unwrap();
        let blender = export_blender_model(mdl(&original), &root.path().join("blender")).unwrap();
        assert!(
            blender
                .warnings
                .iter()
                .all(|warning| !warning.contains("authoring settings")),
            "{:?}",
            blender.warnings
        );
        for (index, source) in [
            saved.join("prop.qc"),
            decompiled.qc_path,
            blender.model_path,
        ]
        .into_iter()
        .enumerate()
        {
            let compiled = compile_model(&ModelCompileRequest::new(
                source,
                root.path().join(format!("roundtrip{index}")),
            ))
            .unwrap();
            assert_eq!(compiled.materials, original.materials);
            assert_eq!(compiled.triangles, original.triangles);
        }
    }
}

#[test]
fn dotted_and_extensionless_materials_keep_distinct_assets_skins_and_preview_names() {
    use std::collections::BTreeMap;

    use gmpublished_backend::{
        model_compile::inspect_model,
        model_decompile::{load_animation_preview, load_model_lod_preview},
        model_project::{ModelSkin, SkinMaterial},
    };

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("cube.smd");
    let mut triangle = 0;
    let text = include_str!("../../vendor/mdlc/tests/fixtures/cube.smd")
        .lines()
        .map(|line| {
            if line == "material" {
                triangle += 1;
                if triangle % 2 == 1 {
                    "\"Material\""
                } else {
                    "\"Material.002\""
                }
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&source, text).unwrap();
    for (file, color) in [
        ("material.png", [255, 0, 0, 255]),
        ("material.002.png", [0, 0, 255, 255]),
        ("replacement.png", [0, 255, 0, 255]),
    ] {
        image::RgbaImage::from_pixel(2, 2, image::Rgba(color))
            .save(root.path().join(file))
            .unwrap();
    }
    let mut request = ModelCompileRequest::new(source, root.path().join("original"));
    request.material_directory = Some("models/dotted".into());
    request.skins.push(ModelSkin {
        name: "green".into(),
        replacements: BTreeMap::from([(
            "Material.002".into(),
            SkinMaterial::Texture {
                path: "replacement.png".into(),
            },
        )]),
    });
    let original = compile_model(&request).unwrap();
    let original_destination = fs::canonicalize(&request.destination).unwrap();
    for file in original
        .files
        .iter()
        .filter(|file| file.kind == ModelOutputKind::Texture)
    {
        let relative = file
            .path
            .strip_prefix(&original_destination)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let expected = if relative.contains("__skins/") {
            [0, 255, 0, 255]
        } else if relative.ends_with("material.002.vtf") {
            [0, 0, 255, 255]
        } else {
            [255, 0, 0, 255]
        };
        let bytes = fs::read(&file.path).unwrap();
        let limits = vformats::Limits::default();
        let texture = vformats::vtf::parse(&bytes, &limits).unwrap();
        let image = texture.decode_rgba(0, 0, 0, &limits).unwrap();
        assert!(
            image.rgba.chunks_exact(4).all(|pixel| pixel == expected),
            "{relative}"
        );
    }
    let file = |output: &ModelCompileOutput, kind| {
        fs::read(
            &output
                .files
                .iter()
                .find(|file| file.kind == kind)
                .unwrap()
                .path,
        )
        .unwrap()
    };
    let raw_mdl = file(&original, ModelOutputKind::Mdl);
    let raw_vvd = file(&original, ModelOutputKind::Vvd);
    let raw_vtx = file(&original, ModelOutputKind::Vtx);
    let parsed = vformats::mdl::parse_mdl(&raw_mdl, &vformats::Limits::default()).unwrap();
    assert_eq!(
        parsed.textures,
        [
            "Material",
            "Material.002.vmt",
            "__skins/skin_1/Material.002.vmt"
        ]
    );
    let preview = load_model_lod_preview(&raw_mdl, &raw_vvd, &raw_vtx).unwrap();
    assert_eq!(preview.levels[0].material_names, original.materials);
    assert_eq!(preview.levels[0].skin_tables.len(), 2);
    let inspection = inspect_model(mdl(&original)).unwrap();
    assert_eq!(inspection.materials, original.materials);
    assert_eq!(
        inspection.skins,
        preview.levels[0]
            .skin_tables
            .iter()
            .map(|skin| {
                skin.iter()
                    .map(|&index| original.materials[index as usize].clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    );
    load_animation_preview(&raw_mdl, &raw_vvd, &raw_vtx, None).unwrap();
    let saved = root.path().join("portable");
    fs::create_dir(&saved).unwrap();
    save_model_qc(&request, &saved.join("prop.qc")).unwrap();
    let decompiled = decompile_model(mdl(&original), &root.path().join("decompiled")).unwrap();
    let blender = export_blender_model(mdl(&original), &root.path().join("blender")).unwrap();
    for (index, source) in [
        saved.join("prop.qc"),
        decompiled.qc_path,
        blender.model_path,
    ]
    .into_iter()
    .enumerate()
    {
        let compiled = compile_model(&ModelCompileRequest::new(
            source,
            root.path().join(format!("roundtrip{index}")),
        ))
        .unwrap();
        assert_eq!(compiled.materials, original.materials);
        let preview = load_model_lod_preview(
            &file(&compiled, ModelOutputKind::Mdl),
            &file(&compiled, ModelOutputKind::Vvd),
            &file(&compiled, ModelOutputKind::Vtx),
        )
        .unwrap();
        assert_eq!(preview.levels[0].material_names, original.materials);
        assert_eq!(preview.levels[0].skin_tables, parsed_skin_tables(&raw_mdl));
        let destination = root.path().join(format!("roundtrip{index}"));
        for file in original
            .files
            .iter()
            .filter(|file| file.kind == ModelOutputKind::Texture)
        {
            let relative = file.path.strip_prefix(&original_destination).unwrap();
            let path = destination.join(relative);
            assert_eq!(
                fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())),
                fs::read(&file.path).unwrap()
            );
        }
    }
}

fn parsed_skin_tables(raw_mdl: &[u8]) -> Vec<Vec<u16>> {
    let model = vformats::mdl::parse_mdl(raw_mdl, &vformats::Limits::default()).unwrap();
    model
        .skin_table
        .chunks(model.skin_reference_count)
        .map(<[u16]>::to_vec)
        .collect()
}

#[test]
fn dotted_lod_replacement_names_remain_logical_in_preview_and_decompiled_qc() {
    use gmpublished_backend::model_decompile::load_model_lod_preview;

    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("cube.smd"),
        include_str!("../../vendor/mdlc/tests/fixtures/cube.smd"),
    )
    .unwrap();
    let source = root.path().join("input.qc");
    fs::write(&source, "$modelname props/lod.mdl\n$body body cube.smd\n$sequence idle cube.smd\n$lod 20 { replacematerial material material.002 }\n").unwrap();
    let request = ModelCompileRequest::new(source, root.path().join("original"));
    let original = compile_model(&request).unwrap();
    let read = |output: &ModelCompileOutput, kind| {
        fs::read(
            &output
                .files
                .iter()
                .find(|file| file.kind == kind)
                .unwrap()
                .path,
        )
        .unwrap()
    };
    let preview = |output: &ModelCompileOutput| {
        load_model_lod_preview(
            &read(output, ModelOutputKind::Mdl),
            &read(output, ModelOutputKind::Vvd),
            &read(output, ModelOutputKind::Vtx),
        )
        .unwrap()
    };
    let original_preview = preview(&original);
    assert_eq!(original_preview.levels[0].material_names, ["material"]);
    assert_eq!(original_preview.levels[1].material_names, ["material.002"]);
    assert!(
        read(&original, ModelOutputKind::Vtx)
            .windows(b"material.002.vmt\0".len())
            .any(|bytes| bytes == b"material.002.vmt\0")
    );
    let decompiled = decompile_model(mdl(&original), &root.path().join("decompiled")).unwrap();
    let qc = fs::read_to_string(&decompiled.qc_path).unwrap();
    assert!(qc.contains("replacematerial \"material\" \"material.002\""));
    let rebuilt = compile_model(&ModelCompileRequest::new(
        decompiled.qc_path,
        root.path().join("rebuilt"),
    ))
    .unwrap();
    let rebuilt_preview = preview(&rebuilt);
    assert_eq!(rebuilt_preview.levels.len(), original_preview.levels.len());
    for (rebuilt, original) in rebuilt_preview.levels.iter().zip(&original_preview.levels) {
        assert_eq!(rebuilt.material_names, original.material_names);
        assert_eq!(rebuilt.skin_tables, original.skin_tables);
        assert_eq!(rebuilt.triangle_count, original.triangle_count);
    }
}
