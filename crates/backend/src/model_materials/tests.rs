use super::*;

fn png(path: &Path, color: [u8; 4]) {
    RgbaImage::from_pixel(4, 2, image::Rgba(color))
        .save(path)
        .unwrap();
}

#[test]
fn gltf_matching_texture_coordinates_keep_normals_and_mismatched_sets_are_reported() {
    let root = tempfile::tempdir().unwrap();
    png(&root.path().join("color.png"), [40, 120, 220, 255]);
    png(&root.path().join("normal.png"), [128, 128, 255, 255]);
    let coordinates = serde_json::json!({"texCoord": 1, "extensions": {"KHR_texture_transform": {"offset": [0.1, 0.2], "scale": [2.0, 3.0], "rotation": 0.25}}});
    let mut base = coordinates.clone();
    base["index"] = 0.into();
    let mut normal = coordinates;
    normal["index"] = 1.into();
    let mut document = serde_json::json!({
        "asset": {"version": "2.0"},
        "images": [{"uri": "color.png"}, {"uri": "normal.png"}],
        "textures": [{"source": 0, "sampler": 0}, {"source": 1, "sampler": 1}],
        "samplers": [{"wrapS": 33071, "wrapT": 10497}, {"wrapS": 10497, "wrapT": 33071}],
        "materials": [{"name": "paint", "normalTexture": normal, "pbrMetallicRoughness": {"baseColorTexture": base, "metallicFactor": 0.0}}]
    });
    let path = root.path().join("paint.gltf");
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let materials = prepare_materials_for_source(&["paint".into()], &[], &path).unwrap();
    assert!(materials.warnings.is_empty());
    for (suffix, expected) in [("paint.vtf", 4), ("paint_normal.vtf", 8)] {
        let texture = materials
            .assets
            .iter()
            .find(|asset| asset.relative_path.ends_with(suffix))
            .unwrap();
        assert_eq!(
            u32::from_le_bytes(texture.bytes[20..24].try_into().unwrap()) & 12,
            expected
        );
    }
    document["materials"][0]["normalTexture"]["texCoord"] = 2.into();
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let materials = prepare_materials_for_source(&["paint".into()], &[], &path).unwrap();
    assert_eq!(materials.warnings.len(), 1);
    assert!(materials.warnings[0].contains("normal texture uses different UV coordinates"));
    assert!(
        !materials
            .assets
            .iter()
            .any(|asset| asset.relative_path.to_string_lossy().contains("normal"))
    );
    document["materials"][0]["normalTexture"]["texCoord"] = 1.into();
    document["samplers"][0]["wrapS"] = 33648.into();
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let materials = prepare_materials_for_source(&["paint".into()], &[], &path).unwrap();
    assert_eq!(materials.warnings.len(), 1);
    assert!(materials.warnings[0].contains("mirrored texture wrapping"));
    document["samplers"][0]["wrapS"] = 99.into();
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    assert!(prepare_materials_for_source(&["paint".into()], &[], &path).is_err());
}

#[test]
fn image_export_produces_source_material_and_complete_mip_chain() {
    let source = tempfile::tempdir().unwrap();
    png(&source.path().join("Skin.PNG"), [40, 120, 220, 255]);
    let materials = prepare_materials(
        &["Skin".into()],
        &["models/character".into()],
        source.path(),
    )
    .unwrap();
    assert!(materials.warnings.is_empty());
    let destination = tempfile::tempdir().unwrap();
    materials.write_to(destination.path()).unwrap();
    let material = fs::read_to_string(
        destination
            .path()
            .join("materials/models/character/skin.vmt"),
    )
    .unwrap();
    assert_eq!(
        vformats::vmt::basetexture(&material, &vformats::Limits::default()),
        Some("models/character/skin".into())
    );
    let bytes = fs::read(
        destination
            .path()
            .join("materials/models/character/skin.vtf"),
    )
    .unwrap();
    let texture = vformats::vtf::parse(&bytes, &vformats::Limits::default()).unwrap();
    assert_eq!(
        (texture.width(), texture.height(), texture.mip_count()),
        (4, 2, 3)
    );
    for mip in 0..texture.mip_count() {
        let image = texture
            .decode_rgba(0, 0, mip, &vformats::Limits::default())
            .unwrap();
        assert!(
            image
                .rgba
                .chunks_exact(4)
                .all(|pixel| pixel == [40, 120, 220, 255])
        );
    }
}

#[test]
fn mip_filter_averages_colors_in_linear_space_and_normalizes_normal_vectors() {
    let image = RgbaImage::from_fn(2, 1, |x, _| {
        image::Rgba(if x == 0 {
            [0, 0, 0, 255]
        } else {
            [255, 255, 255, 255]
        })
    });
    let bytes = encode_vtf(&image, false).unwrap();
    let texture = vformats::vtf::parse(&bytes, &vformats::Limits::default()).unwrap();
    assert_eq!(
        texture
            .decode_rgba(0, 0, 1, &vformats::Limits::default())
            .unwrap()
            .rgba,
        [188, 188, 188, 255]
    );
    let normals = RgbaImage::from_fn(2, 1, |x, _| {
        image::Rgba(if x == 0 {
            [255, 128, 128, 255]
        } else {
            [128, 255, 128, 255]
        })
    });
    let reduced = downsample(&normals, true);
    assert!((216..=219).contains(&reduced.get_pixel(0, 0)[0]));
    assert!((216..=219).contains(&reduced.get_pixel(0, 0)[1]));
}

#[test]
fn existing_materials_preserve_shader_parameters_and_include_dependencies() {
    let source = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("materials/models/test")).unwrap();
    fs::write(source.path().join("materials/models/test/skin.vmt"), b"patch { include \"models/test/base.vmt\" insert { \"$bumpmap\" \"models/test/normal\" } }").unwrap();
    fs::write(
        source.path().join("materials/models/test/base.vmt"),
        b"VertexLitGeneric { \"$basetexture\" \"models/test/diffuse\" \"$phong\" \"1\" }",
    )
    .unwrap();
    png(&source.path().join("diffuse.png"), [0, 20, 40, 255]);
    png(&source.path().join("normal.png"), [128, 128, 255, 255]);
    let materials =
        prepare_materials(&["skin".into()], &["models/test/".into()], source.path()).unwrap();
    assert!(materials.warnings.is_empty());
    assert_eq!(materials.assets.len(), 4);
    let normal = materials
        .assets
        .iter()
        .find(|asset| asset.relative_path.ends_with("normal.vtf"))
        .unwrap();
    assert_ne!(
        vformats::vtf::parse(&normal.bytes, &vformats::Limits::default())
            .unwrap()
            .flags()
            & 0x80,
        0
    );
    let output = tempfile::tempdir().unwrap();
    materials.write_decompiled_to(output.path()).unwrap();
    assert!(
        output
            .path()
            .join("materials/models/test/diffuse.png")
            .exists()
    );
    assert!(
        fs::read_to_string(output.path().join("materials/models/test/base.vmt"))
            .unwrap()
            .contains("$phong")
    );
}

#[test]
fn unsafe_material_paths_cannot_escape_the_export_directory() {
    let source = tempfile::tempdir().unwrap();
    for name in [
        "../escape",
        "/absolute",
        "C:/absolute",
        "foo/../../escape",
        "quote\"name",
        "foo\nbar",
        "foo//bar",
    ] {
        assert!(
            matches!(
                prepare_materials(&[name.into()], &[], source.path()),
                Err(MaterialExportError::UnsafePath(_))
            ),
            "{name}"
        );
    }
    assert!(matches!(
        prepare_materials(&["skin".into()], &["../outside".into()], source.path()),
        Err(MaterialExportError::UnsafePath(_))
    ));
}

#[test]
fn existing_export_files_are_rejected_before_any_write() {
    let source = tempfile::tempdir().unwrap();
    png(&source.path().join("skin.png"), [0, 20, 40, 100]);
    let materials = prepare_materials(&["skin".into()], &[], source.path()).unwrap();
    let destination = tempfile::tempdir().unwrap();
    fs::create_dir(destination.path().join("materials")).unwrap();
    fs::write(destination.path().join("materials/skin.vtf"), b"existing").unwrap();
    assert!(matches!(
        materials.write_to(destination.path()),
        Err(MaterialExportError::AlreadyExists(_))
    ));
    assert!(!destination.path().join("materials/skin.vmt").exists());
    assert_eq!(
        fs::read(destination.path().join("materials/skin.vtf")).unwrap(),
        b"existing"
    );
}

#[test]
fn missing_materials_are_reported_and_non_power_of_two_images_are_resized() {
    let source = tempfile::tempdir().unwrap();
    RgbaImage::from_pixel(3, 5, image::Rgba([255, 0, 0, 255]))
        .save(source.path().join("skin.png"))
        .unwrap();
    let materials =
        prepare_materials(&["skin".into(), "missing".into()], &[], source.path()).unwrap();
    assert_eq!(materials.warnings.len(), 2);
    let texture = materials
        .assets
        .iter()
        .find(|asset| asset.relative_path.ends_with("skin.vtf"))
        .unwrap();
    let texture = vformats::vtf::parse(&texture.bytes, &vformats::Limits::default()).unwrap();
    assert_eq!((texture.width(), texture.height()), (4, 8));
}

#[cfg(unix)]
#[test]
fn symlinked_sources_and_export_folders_cannot_escape() {
    use std::os::unix::fs::symlink;
    let source = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    png(&outside.path().join("skin.png"), [0, 20, 40, 255]);
    symlink(
        outside.path().join("skin.png"),
        source.path().join("skin.png"),
    )
    .unwrap();
    assert!(matches!(
        prepare_materials(&["skin".into()], &[], source.path()),
        Err(MaterialExportError::UnsafePath(_))
    ));
    fs::remove_file(source.path().join("skin.png")).unwrap();
    png(&source.path().join("skin.png"), [0, 20, 40, 255]);
    let materials = prepare_materials(&["skin".into()], &[], source.path()).unwrap();
    let destination = tempfile::tempdir().unwrap();
    symlink(outside.path(), destination.path().join("materials")).unwrap();
    assert!(matches!(
        materials.write_to(destination.path()),
        Err(MaterialExportError::UnsafePath(_))
    ));
    assert!(!outside.path().join("skin.vmt").exists());
}

#[test]
fn gltf_materials_export_embedded_images_with_factors_and_alpha_mode() {
    let source = tempfile::tempdir().unwrap();
    let mut png = io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, image::Rgba([255, 128, 0, 128])))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let document = serde_json::json!({
        "asset": {"version":"2.0"},
        "images": [{"uri": format!("data:image/png;base64,{}", base64::encode(png.into_inner()))}],
        "textures": [{"source":0}],
        "materials": [{"name":"paint", "alphaMode":"MASK", "alphaCutoff":0.25, "doubleSided":true,
            "pbrMetallicRoughness":{"baseColorTexture":{"index":0}, "baseColorFactor":[0.5,1.0,1.0,0.5], "metallicFactor":0.0}}]
    });
    let path = source.path().join("model.gltf");
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    let prepared =
        prepare_materials_for_source(&["paint".into()], &["models/test".into()], &path).unwrap();
    assert!(prepared.warnings.is_empty());
    let bytes = &prepared
        .assets
        .iter()
        .find(|asset| asset.relative_path.ends_with("paint.vtf"))
        .unwrap()
        .bytes;
    let texture = vformats::vtf::parse(bytes, &vformats::Limits::default()).unwrap();
    assert_eq!(
        texture
            .decode_rgba(0, 0, 0, &vformats::Limits::default())
            .unwrap()
            .rgba[0..4],
        [188, 128, 0, 64]
    );
    let vmt = prepared
        .assets
        .iter()
        .find(|asset| asset.relative_path.ends_with("paint.vmt"))
        .unwrap();
    let text = std::str::from_utf8(&vmt.bytes).unwrap();
    let parsed = vformats::vmt::parse(text, &vformats::Limits::default()).unwrap();
    assert_eq!(parsed.value("$alphatestreference"), Some("0.25"));
    assert_eq!(parsed.value("$nocull"), Some("1"));
}

#[test]
fn glb_buffer_view_textures_are_exported_and_out_of_range_views_fail() {
    let source = tempfile::tempdir().unwrap();
    let mut png = io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, image::Rgba([0, 200, 120, 255])))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let binary = png.into_inner();
    let mut document = serde_json::json!({"asset":{"version":"2.0"},
        "buffers":[{"byteLength":binary.len()}],
        "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":binary.len()}],
        "images":[{"bufferView":0,"mimeType":"image/png"}],
        "textures":[{"source":0}],
        "materials":[{"name":"paint","pbrMetallicRoughness":{"baseColorTexture":{"index":0},"metallicFactor":0.0}}]});
    let path = source.path().join("model.glb");
    let write_glb = |document: &serde_json::Value| {
        let mut json = serde_json::to_vec(document).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut data = binary.clone();
        while !data.len().is_multiple_of(4) {
            data.push(0);
        }
        let mut glb = Vec::new();
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2_u32.to_le_bytes());
        glb.extend_from_slice(&(28_u32 + json.len() as u32 + data.len() as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(data.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&data);
        fs::write(&path, glb).unwrap();
    };
    write_glb(&document);
    let prepared = prepare_materials_for_source(&["paint".into()], &[], &path).unwrap();
    assert_eq!(prepared.assets.len(), 2);
    assert!(prepared.warnings.is_empty());
    document["bufferViews"][0]["byteOffset"] = serde_json::json!(usize::MAX);
    write_glb(&document);
    assert!(matches!(
        prepare_materials_for_source(&["paint".into()], &[], &path),
        Err(MaterialExportError::Decode { .. })
    ));
}

#[test]
fn gltf_external_textures_resolve_case_insensitively_and_cannot_escape() {
    let source = tempfile::tempdir().unwrap();
    fs::create_dir(source.path().join("Textures")).unwrap();
    png(
        &source.path().join("Textures/Paint Color.png"),
        [60, 120, 240, 255],
    );
    let path = source.path().join("model.gltf");
    let mut document = serde_json::json!({"asset":{"version":"2.0"},
        "images":[{"uri":"textures/paint%20color.png"}],"textures":[{"source":0}],
        "materials":[{"name":"surface","pbrMetallicRoughness":{"baseColorTexture":{"index":0},"metallicFactor":0.0}}]});
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    assert_eq!(
        prepare_materials_for_source(&["surface".into()], &[], &path)
            .unwrap()
            .assets
            .len(),
        2
    );
    document["images"][0]["uri"] = serde_json::json!("%2e%2e/paint.png");
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    assert!(matches!(
        prepare_materials_for_source(&["surface".into()], &[], &path),
        Err(MaterialExportError::UnsafePath(_))
    ));
}

#[test]
fn fbx_solid_material_colors_are_preserved() {
    let source = tempfile::tempdir().unwrap();
    let path = source.path().join("model.fbx");
    fs::write(
        &path,
        br#"; FBX 7.4.0 project file
FBXHeaderExtension: { FBXHeaderVersion: 1003
    FBXVersion: 7400
}

Objects: {
    Material: 100, "Material::paint", "" {
        Version: 102
        ShadingModel: "lambert"
        MultiLayer: 0
        Properties70: {
            P: "DiffuseColor", "Color", "", "A", 1.0, 0.0, 0.0
            P: "DiffuseFactor", "Number", "", "A", 1.0
        }
    }
}
Connections: { }
"#,
    )
    .unwrap();
    let prepared = prepare_materials_for_source(&["paint".into()], &[], &path).unwrap();
    assert_eq!(prepared.assets.len(), 2);
    assert!(prepared.warnings.is_empty());
    let bytes = &prepared
        .assets
        .iter()
        .find(|asset| asset.relative_path.ends_with("paint.vtf"))
        .unwrap()
        .bytes;
    let texture = vformats::vtf::parse(bytes, &vformats::Limits::default()).unwrap();
    assert_eq!(
        texture
            .decode_rgba(0, 0, 0, &vformats::Limits::default())
            .unwrap()
            .rgba,
        [255, 0, 0, 255]
    );
}

#[test]
fn fbx_scene_reuse_does_not_hide_edits_after_an_operation_or_panic() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("paint.fbx");
    let write_color = |color: &str| {
        fs::write(
            &path,
            format!(
                r#"; FBX 7.4.0 project file
FBXHeaderExtension: {{ FBXHeaderVersion: 1003
    FBXVersion: 7400
}}
Objects: {{
    Material: 100, "Material::paint", "" {{
        Properties70: {{
            P: "DiffuseColor", "Color", "", "A", {color}
        }}
    }}
}}
"#
            ),
        )
        .unwrap();
    };
    let pixels = || {
        let prepared = prepare_materials_for_source(&["paint".into()], &[], &path).unwrap();
        let asset = prepared
            .assets
            .iter()
            .find(|asset| {
                asset
                    .relative_path
                    .extension()
                    .is_some_and(|ext| ext == "vtf")
            })
            .unwrap();
        vformats::vtf::parse(&asset.bytes, &vformats::Limits::default())
            .unwrap()
            .decode_rgba(0, 0, 0, &vformats::Limits::default())
            .unwrap()
            .rgba
    };
    write_color("1.0, 0.0, 0.0");
    mdlc::fbx::with_scene_cache(|| {
        assert_eq!(pixels(), [255, 0, 0, 255]);
        mdlc::fbx::with_scene_cache(|| assert_eq!(pixels(), [255, 0, 0, 255]));
    });
    write_color("0.0, 1.0, 0.0");
    let interrupted = std::panic::catch_unwind(|| {
        mdlc::fbx::with_scene_cache(|| {
            assert_eq!(pixels(), [0, 255, 0, 255]);
            panic!("interrupted material operation");
        })
    });
    assert!(interrupted.is_err());
    write_color("0.0, 0.0, 1.0");
    mdlc::fbx::with_scene_cache(|| assert_eq!(pixels(), [0, 0, 255, 255]));
}

#[test]
fn referenced_sources_merge_materials_and_reject_conflicting_definitions() {
    let source = tempfile::tempdir().unwrap();
    let write = |file: &str, name: &str, color: [f32; 4]| {
        let path = source.path().join(file);
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "asset":{"version":"2.0"}, "materials":[{"name":name,
                    "pbrMetallicRoughness":{"baseColorFactor":color,"metallicFactor":0.0}}]
            }))
            .unwrap(),
        )
        .unwrap();
        path
    };
    let first = write("first.gltf", "red", [1.0, 0.0, 0.0, 1.0]);
    let second = write("second.gltf", "blue", [0.0, 0.0, 1.0, 1.0]);
    let names = ["red".into(), "blue".into()];
    let prepared =
        prepare_materials_for_sources(&names, &[], source.path(), &[first.clone(), second])
            .unwrap();
    assert_eq!(prepared.assets.len(), 4);
    assert!(prepared.warnings.is_empty());
    let conflict = write("conflict.gltf", "red", [0.0, 1.0, 0.0, 1.0]);
    assert!(matches!(
        prepare_materials_for_sources(&names, &[], source.path(), &[first, conflict]),
        Err(MaterialExportError::Conflict(_))
    ));
}

#[test]
fn public_prepared_assets_cannot_write_duplicate_or_absolute_paths() {
    let output = tempfile::tempdir().unwrap();
    for path in ["../escape.vmt", "/tmp/escape.vmt", "C:/escape.vmt"] {
        let prepared = PreparedMaterials {
            assets: vec![MaterialAsset {
                relative_path: path.into(),
                bytes: Vec::new(),
            }],
            warnings: Vec::new(),
        };
        assert!(matches!(
            prepared.write_to(output.path()),
            Err(MaterialExportError::UnsafePath(_))
        ));
    }
    let prepared = PreparedMaterials {
        assets: vec![
            MaterialAsset {
                relative_path: "materials/skin.vmt".into(),
                bytes: Vec::new()
            };
            2
        ],
        warnings: Vec::new(),
    };
    assert!(matches!(
        prepared.write_to(output.path()),
        Err(MaterialExportError::Conflict(_))
    ));
    assert!(!output.path().join("materials").exists());
}

#[test]
fn export_paths_match_compiler_prefix_and_extension_normalization() {
    let source = tempfile::tempdir().unwrap();
    png(&source.path().join("skin.png"), [60, 120, 240, 255]);
    let prepared = prepare_materials(
        &["models/test/skin.png".into()],
        &["models/test".into()],
        source.path(),
    )
    .unwrap();
    assert!(prepared.warnings.is_empty());
    assert!(
        prepared
            .assets
            .iter()
            .any(|asset| asset.relative_path == Path::new("materials/models/test/skin.png.vmt"))
    );
    assert!(
        prepared
            .assets
            .iter()
            .any(|asset| asset.relative_path == Path::new("materials/models/test/skin.png.vtf"))
    );
    assert_eq!(
        mdlc::mdl_writer::normalize_texture_name("models/test/skin.png", &["models/test".into()]),
        "skin.png"
    );
}

#[test]
fn gltf_unnamed_and_duplicate_material_names_match_native_compiler() {
    let source = tempfile::tempdir().unwrap();
    let path = source.path().join("model.gltf");
    fs::write(&path, serde_json::to_vec(&serde_json::json!({
        "asset":{"version":"2.0"},
        "materials":[
            {"pbrMetallicRoughness":{"baseColorFactor":[1.0,0.0,0.0,1.0],"metallicFactor":0.0}},
            {"name":"material_0", "pbrMetallicRoughness":{"baseColorFactor":[0.0,1.0,0.0,1.0],"metallicFactor":0.0}},
            {"name":"material_0", "pbrMetallicRoughness":{"baseColorFactor":[0.0,0.0,1.0,1.0],"metallicFactor":0.0}}
        ]
    })).unwrap()).unwrap();
    let names = mdlc::gltf::stable_source_names(
        [None, Some("material_0"), Some("material_0")].into_iter(),
        "material",
    );
    let prepared = prepare_materials_for_source(&names, &[], &path).unwrap();
    assert_eq!(prepared.assets.len(), 6);
    assert!(prepared.warnings.is_empty());
    for (name, color) in names
        .iter()
        .zip([[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]])
    {
        let texture = prepared
            .assets
            .iter()
            .find(|asset| asset.relative_path == Path::new(&format!("materials/{name}.vtf")))
            .unwrap();
        let texture = vformats::vtf::parse(&texture.bytes, &vformats::Limits::default()).unwrap();
        assert_eq!(
            texture
                .decode_rgba(0, 0, 0, &vformats::Limits::default())
                .unwrap()
                .rgba,
            color
        );
    }
}

#[test]
fn source_names_that_normalize_to_one_engine_material_are_rejected() {
    let source = tempfile::tempdir().unwrap();
    let path = source.path().join("model.gltf");
    fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "asset":{"version":"2.0"}, "materials":[
                {"name":"paint.vmt", "pbrMetallicRoughness":{"baseColorFactor":[1.0,0.0,0.0,1.0]}},
                {"name":"paint", "pbrMetallicRoughness":{"baseColorFactor":[0.0,1.0,0.0,1.0]}}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        prepare_materials_for_source(&["paint.vmt".into(), "paint".into()], &[], &path),
        Err(MaterialExportError::Conflict(_))
    ));
}

fn textured_fbx(directory: &Path, color: &str, normal: &str) -> PathBuf {
    let path = directory.join("model.fbx");
    let text = r#"; FBX 7.4.0 project file
FBXHeaderExtension: {
    FBXHeaderVersion: 1003
    FBXVersion: 7400
}
Objects: {
    Material: 100, "Material::paint", "" {
        Version: 102
        ShadingModel: "lambert"
        Properties70: {
            P: "DiffuseColor", "Color", "", "A", 1.0, 0.0, 0.0
            P: "DiffuseFactor", "Number", "", "A", 1.0
        }
    }
    Material: 101, "Material::unused", "" {
        Version: 102
        ShadingModel: "lambert"
    }
    Texture: 200, "Texture::color", "" {
        Type: "TextureVideoClip"
        FileName: "COLOR_PATH"
        RelativeFilename: "COLOR_PATH"
    }
    Texture: 201, "Texture::normal", "" {
        Type: "TextureVideoClip"
        FileName: "NORMAL_PATH"
        RelativeFilename: "NORMAL_PATH"
    }
    Texture: 202, "Texture::unused", "" {
        Type: "TextureVideoClip"
        FileName: "../outside.png"
        RelativeFilename: "../outside.png"
    }
}
Connections: {
    C: "OP", 200, 100, "DiffuseColor"
    C: "OP", 201, 100, "NormalMap"
    C: "OP", 202, 101, "DiffuseColor"
}
"#;
    fs::write(
        &path,
        text.replace("COLOR_PATH", color)
            .replace("NORMAL_PATH", normal),
    )
    .unwrap();
    path
}

#[test]
fn missing_fbx_color_and_normal_textures_warn_and_export_diffuse_color() {
    let source = tempfile::tempdir().unwrap();
    let path = textured_fbx(source.path(), "model/color.png", "model/normal.png");
    let prepared = prepare_materials_for_source(&["paint".into()], &[], &path).unwrap();
    assert_eq!(prepared.assets.len(), 2);
    assert_eq!(prepared.warnings.len(), 2);
    assert!(
        prepared
            .warnings
            .iter()
            .any(|warning| warning.contains("diffuse color was exported"))
    );
    assert!(
        prepared
            .warnings
            .iter()
            .any(|warning| warning.contains("normal map was omitted"))
    );
    let texture = prepared
        .assets
        .iter()
        .find(|asset| asset.relative_path.ends_with("paint.vtf"))
        .unwrap();
    let texture = vformats::vtf::parse(&texture.bytes, &vformats::Limits::default()).unwrap();
    assert_eq!(
        texture
            .decode_rgba(0, 0, 0, &vformats::Limits::default())
            .unwrap()
            .rgba,
        [255, 0, 0, 255]
    );
    assert!(
        !std::str::from_utf8(
            &prepared
                .assets
                .iter()
                .find(|asset| asset.relative_path.ends_with("paint.vmt"))
                .unwrap()
                .bytes
        )
        .unwrap()
        .contains("$bumpmap")
    );
}

#[test]
fn fbx_texture_folders_are_resolved_and_corrupt_or_unsafe_images_are_rejected() {
    let source = tempfile::tempdir().unwrap();
    let path = textured_fbx(source.path(), "original/color.png", "original/normal.png");
    fs::create_dir(source.path().join("model.fbm")).unwrap();
    fs::create_dir(source.path().join("textures")).unwrap();
    png(
        &source.path().join("model.fbm/color.png"),
        [255, 255, 255, 255],
    );
    png(
        &source.path().join("textures/normal.png"),
        [128, 128, 255, 255],
    );
    let prepared = prepare_materials_for_source(&["paint".into()], &[], &path).unwrap();
    assert_eq!(prepared.assets.len(), 3);
    assert!(prepared.warnings.is_empty());
    fs::write(source.path().join("model.fbm/color.png"), b"not an image").unwrap();
    assert!(matches!(
        prepare_materials_for_source(&["paint".into()], &[], &path),
        Err(MaterialExportError::Decode { .. })
    ));
    let path = textured_fbx(source.path(), "../outside.png", "original/normal.png");
    assert!(matches!(
        prepare_materials_for_source(&["paint".into()], &[], &path),
        Err(MaterialExportError::UnsafePath(_))
    ));
}

#[test]
fn blender_source_materials_keep_shader_proxies_and_edited_textures_over_loose_assets() {
    let source = tempfile::tempdir().unwrap();
    fs::create_dir_all(source.path().join("materials/models/test")).unwrap();
    fs::write(
        source.path().join("materials/models/test/paint.vmt"),
        "\"VertexLitGeneric\" { \"$basetexture\" \"models/test/paint\" \"$bumpmap\" \"models/test/paint_normal\" }",
    )
    .unwrap();
    fs::write(
        source.path().join("materials/models/test/paint.vtf"),
        encode_vtf(
            &RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255])),
            false,
        )
        .unwrap(),
    )
    .unwrap();
    fs::write(
        source.path().join("materials/models/test/paint_normal.vtf"),
        encode_vtf(
            &RgbaImage::from_pixel(1, 1, image::Rgba([128, 128, 255, 255])),
            true,
        )
        .unwrap(),
    )
    .unwrap();
    let mut png = io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, image::Rgba([0, 200, 120, 64])))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let parameters = serde_json::json!({
        "$basetexture":"original/paint", "$bumpmap":"original/normal",
        "$phong":"1", "$phongexponent":"32", "$basealphaenvmapmask":"1",
        "$envmap":"env_cubemap", "$detail":"original/detail"
    });
    let blocks = serde_json::json!({"Proxies":{"TextureScroll":{
        "textureScrollVar":"$basetexturetransform", "textureScrollRate":"0.125"
    }}});
    let mut document = serde_json::json!({
        "asset":{"version":"2.0"},
        "images":[{"uri":format!("data:image/png;base64,{}",base64::encode(png.into_inner()))}],
        "textures":[{"source":0}],
        "materials":[{"name":"paint", "normalTexture":{"index":0}, "pbrMetallicRoughness":{
            "baseColorTexture":{"index":0}, "metallicFactor":0.0
        }, "extras":{"sourceShader":"UnlitGeneric", "sourceParameters":parameters, "sourceBlocks":blocks}}]
    });
    let path = source.path().join("edited.gltf");
    for strings in [false, true] {
        if strings {
            for key in ["sourceParameters", "sourceBlocks"] {
                document["materials"][0]["extras"][key] =
                    serde_json::Value::String(document["materials"][0]["extras"][key].to_string());
            }
        }
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let prepared = prepare_materials_for_sources(
            &["paint".into()],
            &["models/test".into()],
            source.path(),
            std::slice::from_ref(&path),
        )
        .unwrap();
        let vmt = prepared
            .assets
            .iter()
            .find(|asset| asset.relative_path.ends_with("paint.vmt"))
            .unwrap();
        let text = std::str::from_utf8(&vmt.bytes).unwrap();
        let parsed = vformats::vmt::parse(text, &vformats::Limits::default()).unwrap();
        assert_eq!(parsed.shader, "UnlitGeneric");
        assert_eq!(parsed.value("$basetexture"), Some("models/test/paint"));
        assert_eq!(parsed.value("$bumpmap"), Some("models/test/paint_normal"));
        assert_eq!(parsed.value("$phongexponent"), Some("32"));
        assert_eq!(parsed.value("$basealphaenvmapmask"), Some("1"));
        assert_eq!(parsed.value("$translucent"), None);
        let proxies = parsed.kv.blocks("Proxies").next().unwrap();
        let scroll = proxies.blocks("TextureScroll").next().unwrap();
        assert!(scroll.pairs.iter().any(|pair| pair.key == "textureScrollRate"
            && matches!(&pair.value, vformats::keyvalues::KvValue::String(value) if value == "0.125")));
        let texture = prepared
            .assets
            .iter()
            .find(|asset| asset.relative_path.ends_with("paint.vtf"))
            .unwrap();
        assert_eq!(
            vformats::vtf::parse(&texture.bytes, &vformats::Limits::default())
                .unwrap()
                .decode_rgba(0, 0, 0, &vformats::Limits::default())
                .unwrap()
                .rgba,
            [0, 200, 120, 64]
        );
        let normal = prepared
            .assets
            .iter()
            .find(|asset| asset.relative_path.ends_with("paint_normal.vtf"))
            .unwrap();
        let normal = vformats::vtf::parse(&normal.bytes, &vformats::Limits::default()).unwrap();
        assert_eq!(normal.flags() & 0x80, 0x80);
        assert_eq!(
            normal
                .decode_rgba(0, 0, 0, &vformats::Limits::default())
                .unwrap()
                .rgba,
            [0, 200, 120, 64]
        );
        assert!(prepared.warnings.iter().any(|warning| warning.contains("original/detail") && warning.contains("not found")));
        assert!(
            !prepared
                .warnings
                .iter()
                .any(|warning| warning.contains("env_cubemap")
                    || warning.contains("original/paint")
                    || warning.contains("original/normal"))
        );
    }
}

#[test]
fn blender_source_tint_is_applied_once_and_color_edits_replace_original_tint() {
    let source = tempfile::tempdir().unwrap();
    let mut png = io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([255, 255, 255, 128]),
    ))
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    let mut document = serde_json::json!({
        "asset":{"version":"2.0"},
        "images":[{"uri":format!("data:image/png;base64,{}",base64::encode(png.into_inner()))}],
        "textures":[{"source":0}],
        "materials":[{"name":"paint", "alphaMode":"BLEND", "pbrMetallicRoughness":{
            "baseColorTexture":{"index":0},"baseColorFactor":[0.5,0.25,1.0,0.5],"metallicFactor":0.0
        }, "extras":{"sourceShader":"VertexLitGeneric", "sourceParameters":{
            "$color":"[0.5 0.25 1]", "$alpha":"0.5", "$phong":"1"
        }}}]
    });
    let path = source.path().join("edited.gltf");
    for edited in [false, true] {
        if edited {
            document["materials"][0]["pbrMetallicRoughness"]["baseColorFactor"] =
                serde_json::json!([1.0, 0.25, 1.0, 1.0]);
        }
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let prepared = prepare_materials_for_source(&["paint".into()], &[], &path).unwrap();
        let vmt = prepared
            .assets
            .iter()
            .find(|asset| asset.relative_path.ends_with("paint.vmt"))
            .unwrap();
        let parsed = vformats::vmt::parse(
            std::str::from_utf8(&vmt.bytes).unwrap(),
            &vformats::Limits::default(),
        )
        .unwrap();
        assert_eq!(parsed.value("$phong"), Some("1"));
        assert_eq!(parsed.value("$color"), (!edited).then_some("[0.5 0.25 1]"));
        assert_eq!(parsed.value("$alpha"), (!edited).then_some("0.5"));
        let texture = prepared
            .assets
            .iter()
            .find(|asset| asset.relative_path.ends_with("paint.vtf"))
            .unwrap();
        let rgba = vformats::vtf::parse(&texture.bytes, &vformats::Limits::default())
            .unwrap()
            .decode_rgba(0, 0, 0, &vformats::Limits::default())
            .unwrap()
            .rgba;
        assert_eq!(
            rgba,
            if edited {
                [255, 137, 255, 128]
            } else {
                [255, 255, 255, 128]
            }
        );
    }
}
