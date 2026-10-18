use std::fs;
use std::sync::atomic::AtomicBool;

use gmpublished_backend::model_compile::{
    CollisionDetail, ModelCollision, ModelCompileError, ModelCompileRequest, ModelOutputKind,
    compile_model, compile_model_with_cancel,
};

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("cube.smd");
    fs::write(
        &source,
        include_bytes!("../../vendor/mdlc/tests/fixtures/cube.smd"),
    )
    .unwrap();
    (root, source)
}

#[test]
fn direct_mesh_compiles_matching_source_files_for_gmod() {
    let (root, source) = fixture();
    let mut request = ModelCompileRequest::new(source, root.path().join("addon"));
    request.model_name = Some("props/my_cube.mdl".into());
    request.export_materials = false;
    request.static_prop = true;
    let output = compile_model(&request).unwrap();
    assert_eq!(output.model_name, "models/props/my_cube.mdl");
    assert_eq!(output.files.len(), 3);
    assert!(output.triangles > 0);
    assert!(output.vertices > 0);
    assert_eq!(output.materials, ["material"]);
    let model_directory = fs::canonicalize(root.path().join("addon/models/props")).unwrap();
    for file in &output.files {
        let bytes = fs::read(&file.path).unwrap();
        assert_eq!(bytes.len() as u64, file.bytes);
        assert!(file.path.starts_with(&model_directory));
        let checksum_offset = match file.kind {
            ModelOutputKind::Mdl => {
                assert_eq!(&bytes[..4], b"IDST");
                assert_eq!(i32::from_le_bytes(bytes[4..8].try_into().unwrap()), 48);
                8
            }
            ModelOutputKind::Vvd => {
                assert_eq!(&bytes[..4], b"IDSV");
                8
            }
            ModelOutputKind::Vtx => 16,
            other => panic!("unexpected output {other:?}"),
        };
        assert_eq!(
            i32::from_le_bytes(
                bytes[checksum_offset..checksum_offset + 4]
                    .try_into()
                    .unwrap()
            ),
            output.checksum
        );
    }
    assert!(matches!(
        compile_model(&request),
        Err(ModelCompileError::AlreadyExists(_))
    ));
    request.overwrite = true;
    fs::write(
        root.path().join("addon/models/props/my_cube.phy"),
        "stale physics",
    )
    .unwrap();
    compile_model(&request).unwrap();
    assert!(!root.path().join("addon/models/props/my_cube.phy").exists());
}

#[test]
fn direct_gltf_converts_metres_corrects_y_up_and_keeps_collision_aligned() {
    let root = tempfile::tempdir().unwrap();
    let mut geometry = Vec::new();
    for value in [
        0.0_f32, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 7.0, 0.0, 0.0, 0.0, 3.0,
    ] {
        geometry.extend_from_slice(&value.to_le_bytes());
    }
    for index in [0_u16, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3] {
        geometry.extend_from_slice(&index.to_le_bytes());
    }
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "buffers": [{"uri": format!("data:application/octet-stream;base64,{}", base64::encode(&geometry)), "byteLength": geometry.len()}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 48},
            {"buffer": 0, "byteOffset": 48, "byteLength": 24}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3", "min": [0,0,0], "max": [2,7,3]},
            {"bufferView": 1, "componentType": 5123, "count": 12, "type": "SCALAR"}
        ],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "indices": 1}]}],
        "nodes": [{"mesh": 0}], "scenes": [{"nodes": [0]}], "scene": 0
    });
    let source = root.path().join("upright.gltf");
    fs::write(&source, serde_json::to_vec(&document).unwrap()).unwrap();
    let mut request = ModelCompileRequest::new(source, root.path().join("auto"));
    request.static_prop = true;
    request.export_materials = false;
    request.collision = ModelCollision::Automatic(CollisionDetail::Simple);
    let output = compile_model(&request).unwrap();
    let bytes = |kind| {
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
    let vvd = vformats::mdl::parse_vvd(&bytes(ModelOutputKind::Vvd), &vformats::Limits::default())
        .unwrap();
    let bounds = |points: Vec<[f32; 3]>| {
        let mut minimum = [f32::INFINITY; 3];
        let mut maximum = [f32::NEG_INFINITY; 3];
        for point in points {
            for axis in 0..3 {
                minimum[axis] = minimum[axis].min(point[axis]);
                maximum[axis] = maximum[axis].max(point[axis]);
            }
        }
        (minimum, maximum)
    };
    let visual = bounds(vvd.vertices.iter().map(|vertex| vertex.position).collect());
    for (axis, extent) in [3.0, 2.0, 7.0].into_iter().enumerate() {
        assert!((visual.1[axis] - visual.0[axis] - extent / 0.0254).abs() < 0.001);
    }
    let phy_bytes = bytes(ModelOutputKind::Phy);
    let phy = vformats::phy::parse(&phy_bytes, &vformats::Limits::default()).unwrap();
    let physical = bounds(
        phy.solids
            .iter()
            .flat_map(|solid| &solid.ledges)
            .flat_map(|ledge| ledge.vertices.iter().copied())
            .collect(),
    );
    for (actual, expected) in physical
        .0
        .into_iter()
        .chain(physical.1)
        .zip(visual.0.into_iter().chain(visual.1))
    {
        assert!((actual - expected).abs() < 0.001);
    }
    request.automatic_axes = false;
    request.destination = root.path().join("raw");
    request.collision = ModelCollision::Existing;
    let output = compile_model(&request).unwrap();
    let vvd = vformats::mdl::parse_vvd(
        &fs::read(
            &output
                .files
                .iter()
                .find(|file| file.kind == ModelOutputKind::Vvd)
                .unwrap()
                .path,
        )
        .unwrap(),
        &vformats::Limits::default(),
    )
    .unwrap();
    let raw = bounds(vvd.vertices.iter().map(|vertex| vertex.position).collect());
    assert!((raw.1[2] - raw.0[2] - 3.0).abs() < 0.001);
}

#[test]
fn qc_compiles_relative_sources_and_rejects_unsupported_directives_without_output() {
    let (root, _) = fixture();
    let qc = root.path().join("model.qc");
    fs::write(
        &qc,
        "$modelname props/qc_cube.mdl\n$body body cube.smd\n$sequence idle cube.smd\n",
    )
    .unwrap();
    let mut request = ModelCompileRequest::new(qc.clone(), root.path().join("qc-output"));
    request.export_materials = false;
    let output = compile_model(&request).unwrap();
    assert!(
        output
            .files
            .iter()
            .any(|file| file.path.ends_with("models/props/qc_cube.mdl"))
    );
    fs::write(
        qc,
        "$modelname props/bad.mdl\n$body body cube.smd\n$assumeworldspace\n",
    )
    .unwrap();
    request.destination = root.path().join("invalid-output");
    assert!(matches!(
        compile_model(&request),
        Err(ModelCompileError::Description(_))
    ));
    assert!(!request.destination.exists());
}

#[test]
fn cancellation_and_unsafe_output_never_publish() {
    let (root, source) = fixture();
    let mut request = ModelCompileRequest::new(source, root.path().join("output"));
    let cancelled = AtomicBool::new(true);
    assert!(matches!(
        compile_model_with_cancel(&request, &cancelled),
        Err(ModelCompileError::Cancelled)
    ));
    assert!(!request.destination.exists());
    request.model_name = Some("../escaped.mdl".into());
    assert!(matches!(
        compile_model(&request),
        Err(ModelCompileError::UnsafePath(_))
    ));
    assert!(!root.path().join("escaped.mdl").exists());
}

#[cfg(unix)]
#[test]
fn export_rejects_symlinks_inside_destination() {
    use std::os::unix::fs::symlink;
    let (root, source) = fixture();
    let destination = root.path().join("output");
    let outside = root.path().join("outside");
    fs::create_dir_all(&destination).unwrap();
    fs::create_dir(&outside).unwrap();
    symlink(&outside, destination.join("models")).unwrap();
    let mut request = ModelCompileRequest::new(source, destination);
    request.export_materials = false;
    assert!(matches!(
        compile_model(&request),
        Err(ModelCompileError::UnsafePath(_))
    ));
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn mesh_and_generated_materials_are_exported_together() {
    let (root, source) = fixture();
    let texture = image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 100, 50, 255]));
    texture.save(root.path().join("material.png")).unwrap();
    let mut request = ModelCompileRequest::new(source, root.path().join("addon"));
    request.material_directory = Some("models/test".into());
    let output = compile_model(&request).unwrap();
    assert!(
        output
            .files
            .iter()
            .any(|file| file.kind == ModelOutputKind::Material)
    );
    assert!(
        output
            .files
            .iter()
            .any(|file| file.kind == ModelOutputKind::Texture)
    );
    assert!(
        root.path()
            .join("addon/materials/models/test/material.vmt")
            .exists()
    );
    assert!(
        root.path()
            .join("addon/materials/models/test/material.vtf")
            .exists()
    );
}

#[test]
fn existing_material_blocks_all_model_publication_when_overwrite_is_disabled() {
    let (root, source) = fixture();
    let texture = image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 100, 50, 255]));
    texture.save(root.path().join("material.png")).unwrap();
    let destination = root.path().join("addon");
    fs::create_dir_all(destination.join("materials/models/test")).unwrap();
    let existing = destination.join("materials/models/test/material.vmt");
    fs::write(&existing, "existing material").unwrap();
    let mut request = ModelCompileRequest::new(source, destination.clone());
    request.material_directory = Some("models/test".into());
    assert!(matches!(
        compile_model(&request),
        Err(ModelCompileError::AlreadyExists(_))
    ));
    assert!(!destination.join("models/cube.mdl").exists());
    assert_eq!(fs::read_to_string(existing).unwrap(), "existing material");
}

#[test]
fn static_mesh_decompile_and_recompile_preserves_geometry_and_materials() {
    use gmpublished_backend::model_decompile::decompile_model;
    use std::collections::BTreeSet;

    let (root, source) = fixture();
    image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 100, 50, 255]))
        .save(root.path().join("material.png"))
        .unwrap();
    let mut original_request = ModelCompileRequest::new(source, root.path().join("original"));
    original_request.static_prop = true;
    original_request.material_directory = Some("models/test".into());
    let original = compile_model(&original_request).unwrap();
    let original_model = original
        .files
        .iter()
        .find(|file| file.kind == ModelOutputKind::Mdl)
        .unwrap();
    let exported = decompile_model(&original_model.path, &root.path().join("decompiled")).unwrap();
    let recompiled = compile_model(&ModelCompileRequest::new(
        exported.qc_path,
        root.path().join("recompiled"),
    ))
    .unwrap();
    assert_eq!(original.triangles, recompiled.triangles);
    assert_eq!(original.materials, recompiled.materials);
    let points = |output: &gmpublished_backend::model_compile::ModelCompileOutput| {
        let file = output
            .files
            .iter()
            .find(|file| file.kind == ModelOutputKind::Vvd)
            .unwrap();
        let bytes = fs::read(&file.path).unwrap();
        let vvd = vformats::mdl::parse_vvd(&bytes, &vformats::Limits::default()).unwrap();
        vvd.vertices
            .iter()
            .map(|vertex| {
                let mut attributes = [0_i32; 8];
                for (out, value) in attributes.iter_mut().zip(
                    vertex
                        .position
                        .iter()
                        .chain(&vertex.normal)
                        .chain(&vertex.uv),
                ) {
                    *out = (*value * 10000.0).round() as i32;
                }
                attributes
            })
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(points(&original), points(&recompiled));
    assert!(
        recompiled
            .files
            .iter()
            .any(|file| file.kind == ModelOutputKind::Texture)
    );
}

#[test]
fn rig_decompile_and_recompile_preserves_reference_bones_and_geometry() {
    use gmpublished_backend::model_decompile::decompile_model;
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("rig.smd"),
        include_bytes!("../../vendor/mdlc/tests/fixtures/rig.smd"),
    )
    .unwrap();
    fs::write(
        root.path().join("animation.smd"),
        include_bytes!("../../vendor/mdlc/tests/fixtures/animation.smd"),
    )
    .unwrap();
    let qc = root.path().join("rig.qc");
    fs::write(
        &qc,
        "$modelname models/rig.mdl\n$body body rig.smd\n$sequence move animation.smd\n",
    )
    .unwrap();
    let mut request = ModelCompileRequest::new(qc, root.path().join("original"));
    request.export_materials = false;
    let original = compile_model(&request).unwrap();
    let model = original
        .files
        .iter()
        .find(|file| file.kind == ModelOutputKind::Mdl)
        .unwrap();
    let decompiled = decompile_model(&model.path, &root.path().join("decompiled")).unwrap();
    let mut request = ModelCompileRequest::new(decompiled.qc_path, root.path().join("recompiled"));
    request.export_materials = false;
    let recompiled = compile_model(&request).unwrap();
    assert_eq!(original.triangles, recompiled.triangles);
    assert_eq!(original.vertices, recompiled.vertices);
    assert_eq!(original.bones, recompiled.bones);
    let read_model = |output: &gmpublished_backend::model_compile::ModelCompileOutput| {
        let file = output
            .files
            .iter()
            .find(|file| file.kind == ModelOutputKind::Mdl)
            .unwrap();
        let mut bytes = fs::read(&file.path).unwrap();
        bytes[180..184].fill(0);
        vformats::mdl::parse_mdl(&bytes, &vformats::Limits::default()).unwrap()
    };
    let original = read_model(&original);
    let recompiled = read_model(&recompiled);
    for (first, second) in original.bones.iter().zip(&recompiled.bones) {
        assert_eq!(first.name, second.name);
        assert_eq!(first.parent, second.parent);
        for (a, b) in first
            .position
            .iter()
            .chain(&first.rotation)
            .zip(second.position.iter().chain(&second.rotation))
        {
            assert!(
                (a - b).abs() < 1e-4,
                "bone {} differs: {a} != {b}",
                first.name
            );
        }
    }
}

#[test]
fn qc_exports_unnamed_gltf_nodes_and_material_textures() {
    let root = tempfile::tempdir().unwrap();
    let mut geometry = Vec::new();
    for value in [
        0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0,
        1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0,
    ] {
        geometry.extend_from_slice(&value.to_le_bytes());
    }
    for index in [0_u16, 1, 2] {
        geometry.extend_from_slice(&index.to_le_bytes());
    }
    let mut image = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        4,
        4,
        image::Rgba([25, 50, 100, 255]),
    ))
    .write_to(&mut image, image::ImageFormat::Png)
    .unwrap();
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "buffers": [{"uri": format!("data:application/octet-stream;base64,{}", base64::encode(&geometry)), "byteLength": geometry.len()}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 36},
            {"buffer": 0, "byteOffset": 72, "byteLength": 24},
            {"buffer": 0, "byteOffset": 96, "byteLength": 6}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"},
            {"bufferView": 3, "componentType": 5123, "count": 3, "type": "SCALAR"}
        ],
        "images": [{"uri": format!("data:image/png;base64,{}", base64::encode(image.into_inner()))}],
        "textures": [{"source": 0}],
        "materials": [{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}, "indices": 3, "material": 0}]}],
        "nodes": [{"mesh": 0}], "scenes": [{"nodes": [0]}], "scene": 0
    });
    fs::write(
        root.path().join("scene.gltf"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    let qc = root.path().join("model.qc");
    fs::write(&qc, "$modelname models/embedded.mdl\n$cdmaterials models/test\n$body body scene.gltf\n$sequence idle scene.gltf\n").unwrap();
    let output = compile_model(&ModelCompileRequest::new(qc, root.path().join("addon"))).unwrap();
    assert_eq!(output.triangles, 1);
    assert!(
        output
            .files
            .iter()
            .any(|file| file.path.ends_with("materials/models/test/material_0.vtf"))
    );
    assert!(
        !output
            .warnings
            .iter()
            .any(|warning| warning.contains("No material"))
    );
}

#[test]
fn model_inspection_reads_skins_without_decoding_animation_payloads() {
    use gmpublished_backend::model_compile::inspect_model;

    let (root, source) = fixture();
    let mut request = ModelCompileRequest::new(source, root.path().join("output"));
    request.export_materials = false;
    let output = compile_model(&request).unwrap();
    let path = &output
        .files
        .iter()
        .find(|file| file.kind == ModelOutputKind::Mdl)
        .unwrap()
        .path;
    let expected = inspect_model(path).unwrap();
    let mut bytes = fs::read(path).unwrap();
    bytes[180..184].copy_from_slice(&i32::MAX.to_le_bytes());
    bytes[184..188].copy_from_slice(&i32::MAX.to_le_bytes());
    fs::write(path, &bytes).unwrap();
    assert_eq!(inspect_model(path).unwrap(), expected);
    assert_eq!(expected.skins, [vec!["material".to_owned()]]);

    fs::write(path, &bytes[..180]).unwrap();
    assert!(matches!(
        inspect_model(path),
        Err(ModelCompileError::Description(_))
    ));
}

#[test]
fn authored_skins_export_tints_textures_and_stable_material_remaps_without_duplicate_geometry() {
    use gmpublished_backend::model_compile::inspect_model;
    use gmpublished_backend::model_project::{ModelSkin, SkinMaterial};
    use std::collections::BTreeMap;
    let (root, source) = fixture();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 255, 255, 255]))
        .save(root.path().join("material.png"))
        .unwrap();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([20, 40, 180, 255]))
        .save(root.path().join("blue.png"))
        .unwrap();
    let inspection = inspect_model(&source).unwrap();
    assert_eq!(inspection.materials, ["material"]);
    assert_eq!(inspection.skins, [vec!["material".to_owned()]]);
    let mut request = ModelCompileRequest::new(source, root.path().join("skins"));
    request.skins = vec![
        ModelSkin {
            name: "Red".into(),
            replacements: BTreeMap::from([(
                "material".into(),
                SkinMaterial::Tint { color: [255, 0, 0] },
            )]),
        },
        ModelSkin {
            name: "Blue".into(),
            replacements: BTreeMap::from([(
                "material".into(),
                SkinMaterial::Texture {
                    path: root.path().join("blue.png"),
                },
            )]),
        },
        ModelSkin {
            name: "Unchanged".into(),
            replacements: BTreeMap::new(),
        },
    ];
    let output = compile_model(&request).unwrap();
    assert!(output.warnings.is_empty(), "{:?}", output.warnings);
    let bytes = fs::read(
        output
            .files
            .iter()
            .find(|file| file.kind == ModelOutputKind::Mdl)
            .unwrap()
            .path
            .clone(),
    )
    .unwrap();
    let mdl = vformats::mdl::parse_mdl(&bytes, &vformats::Limits::default()).unwrap();
    assert_eq!(mdl.skin_reference_count, 3);
    assert_eq!(mdl.skin_table, [0, 1, 2, 1, 1, 2, 2, 1, 2, 0, 1, 2]);
    assert_eq!(mdl.body_parts[0].models.len(), 1);
    let tint = fs::read_to_string(
        root.path()
            .join("skins/materials/__skins/skin_1/material.vmt"),
    )
    .unwrap();
    assert!(tint.contains("\"include\" \"materials/material.vmt\""));
    assert!(tint.contains("[1 0 0]"));
    let texture_bytes = fs::read(
        root.path()
            .join("skins/materials/__skins/skin_2/material.vtf"),
    )
    .unwrap();
    let texture = vformats::vtf::parse(&texture_bytes, &vformats::Limits::default()).unwrap();
    let rgba = texture
        .decode_rgba(0, 0, 0, &vformats::Limits::default())
        .unwrap();
    assert_eq!(&rgba.rgba[..4], &[20, 40, 180, 255]);
    let exported_inspection = inspect_model(&root.path().join("skins/models/cube.mdl")).unwrap();
    assert_eq!(exported_inspection.skins.len(), 4);
}

#[test]
fn authored_skin_appends_after_existing_qc_families_and_preserves_their_indices() {
    use gmpublished_backend::model_project::{ModelSkin, SkinMaterial};
    use std::collections::BTreeMap;
    let (root, _) = fixture();
    fs::write(root.path().join("model.qc"), "$modelname skins.mdl\n$body body cube.smd\n$sequence idle cube.smd\n$texturegroup skins { { material } { alternate } }\n").unwrap();
    for name in ["material", "alternate"] {
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255; 4]))
            .save(root.path().join(format!("{name}.png")))
            .unwrap();
    }
    let mut request =
        ModelCompileRequest::new(root.path().join("model.qc"), root.path().join("output"));
    request.skins.push(ModelSkin {
        name: "New".into(),
        replacements: BTreeMap::from([(
            "material".into(),
            SkinMaterial::Material {
                name: "alternate".into(),
            },
        )]),
    });
    let output = compile_model(&request).unwrap();
    let mdl = vformats::mdl::parse_mdl(
        &fs::read(&output.files[0].path).unwrap(),
        &vformats::Limits::default(),
    )
    .unwrap();
    assert_eq!(mdl.skin_table, [0, 1, 1, 1, 1, 1]);
    assert_eq!(mdl.textures, ["material", "alternate"]);
}

#[test]
fn reimported_missing_material_references_are_reported_before_publication() {
    use gmpublished_backend::model_project::{ModelSkin, SkinMaterial};
    use std::collections::BTreeMap;
    let (root, source) = fixture();
    let mut request = ModelCompileRequest::new(source, root.path().join("output"));
    request.skins.push(ModelSkin {
        name: "Red".into(),
        replacements: BTreeMap::from([(
            "old_paint".into(),
            SkinMaterial::Tint { color: [255, 0, 0] },
        )]),
    });
    let error = compile_model(&request).unwrap_err();
    assert!(error.to_string().contains("reconcile"));
    assert!(!request.destination.exists());
}

#[test]
fn static_mesh_orientation_rotates_exported_vertices_normals_and_bounds() {
    let (root, source) = fixture();
    let mut request = ModelCompileRequest::new(source, root.path().join("original"));
    request.static_prop = true;
    request.export_materials = false;
    let original = compile_model(&request).unwrap();
    request.destination = root.path().join("rotated");
    request.orientation = [90.0, 0.0, 0.0];
    let rotated = compile_model(&request).unwrap();
    let vertices = |output: &gmpublished_backend::model_compile::ModelCompileOutput| {
        let path = &output
            .files
            .iter()
            .find(|file| file.kind == ModelOutputKind::Vvd)
            .unwrap()
            .path;
        vformats::mdl::parse_vvd(&fs::read(path).unwrap(), &vformats::Limits::default())
            .unwrap()
            .vertices
    };
    let original_vertices = vertices(&original);
    let rotated_vertices = vertices(&rotated);
    assert_eq!(original_vertices.len(), rotated_vertices.len());
    let mut min = [0.0f32; 3];
    let mut max = [0.0f32; 3];
    for (before, after) in original_vertices.iter().zip(&rotated_vertices) {
        for (actual, expected) in
            after
                .position
                .iter()
                .zip([before.position[0], -before.position[2], before.position[1]])
        {
            assert!((actual - expected).abs() < 0.0001);
        }
        for (actual, expected) in
            after
                .normal
                .iter()
                .zip([before.normal[0], -before.normal[2], before.normal[1]])
        {
            assert!((actual - expected).abs() < 0.0001);
        }
        for axis in 0..3 {
            min[axis] = min[axis].min(after.position[axis]);
            max[axis] = max[axis].max(after.position[axis]);
        }
    }
    let bytes = fs::read(
        &rotated
            .files
            .iter()
            .find(|file| file.kind == ModelOutputKind::Mdl)
            .unwrap()
            .path,
    )
    .unwrap();
    for (index, expected) in min.into_iter().chain(max).enumerate() {
        let offset = 104 + index * 4;
        let actual = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        assert!(
            (actual - expected).abs() < 0.0001,
            "bounds {index}: {actual} != {expected}"
        );
    }
}

#[test]
fn nonstatic_orientation_and_missing_replacement_materials_do_not_publish() {
    use gmpublished_backend::model_project::{ModelSkin, SkinMaterial};
    use std::collections::BTreeMap;
    let (root, source) = fixture();
    let mut request = ModelCompileRequest::new(source, root.path().join("output"));
    request.orientation = [0.0, 90.0, 0.0];
    assert!(matches!(
        compile_model(&request),
        Err(ModelCompileError::InvalidOption(_))
    ));
    request.orientation = [0.0; 3];
    request.skins.push(ModelSkin {
        name: "Missing".into(),
        replacements: BTreeMap::from([(
            "material".into(),
            SkinMaterial::Material {
                name: "missing_skin".into(),
            },
        )]),
    });
    assert!(matches!(
        compile_model(&request),
        Err(ModelCompileError::Materials(_))
    ));
    assert!(!request.destination.exists());
}

#[test]
fn authored_qc_collision_is_valid_and_matches_the_model() {
    let (root, _) = fixture();
    let source = root.path().join("collision.qc");
    fs::write(
        &source,
        "$modelname props/collision_cube.mdl\n$body body cube.smd\n$sequence idle cube.smd\n$collisionmodel cube.smd { $mass 10 }\n",
    )
    .unwrap();
    let mut request = ModelCompileRequest::new(source, root.path().join("collision"));
    request.export_materials = false;
    let output = compile_model(&request).unwrap();
    let bytes = fs::read(
        &output
            .files
            .iter()
            .find(|file| file.kind == ModelOutputKind::Phy)
            .unwrap()
            .path,
    )
    .unwrap();
    let phy = vformats::phy::parse(&bytes, &vformats::Limits::default()).unwrap();
    assert_eq!(phy.header.mdl_checksum, output.checksum as u32);
    assert_eq!(phy.solids.len(), 1);
    assert_eq!(phy.solids[0].ledges.len(), 1);
    assert!(String::from_utf8_lossy(&bytes).contains("\"mass\" \"10.000000\""));
}

#[test]
fn generated_convex_collision_matches_final_rotated_mesh_and_model_checksum() {
    let (root, source) = fixture();
    let mut request = ModelCompileRequest::new(source, root.path().join("collision"));
    request.static_prop = true;
    request.orientation = [90.0, 0.0, 0.0];
    request.collision = ModelCollision::Automatic(CollisionDetail::Simple);
    request.export_materials = false;
    let output = compile_model(&request).unwrap();
    let phy_bytes = fs::read(
        &output
            .files
            .iter()
            .find(|file| file.kind == ModelOutputKind::Phy)
            .unwrap()
            .path,
    )
    .unwrap();
    let phy = vformats::phy::parse(&phy_bytes, &vformats::Limits::default()).unwrap();
    assert_eq!(phy.header.mdl_checksum, output.checksum as u32);
    assert_eq!(phy.solids.len(), 1);
    assert_eq!(phy.solids[0].ledges.len(), 1);
    let vvd = vformats::mdl::parse_vvd(
        &fs::read(
            &output
                .files
                .iter()
                .find(|file| file.kind == ModelOutputKind::Vvd)
                .unwrap()
                .path,
        )
        .unwrap(),
        &vformats::Limits::default(),
    )
    .unwrap();
    let bounds = |points: Vec<[f32; 3]>| {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for point in points {
            for axis in 0..3 {
                min[axis] = min[axis].min(point[axis]);
                max[axis] = max[axis].max(point[axis]);
            }
        }
        (min, max)
    };
    let visual = bounds(vvd.vertices.iter().map(|vertex| vertex.position).collect());
    let physical = bounds(phy.solids[0].ledges[0].vertices.clone());
    for (actual, expected) in physical
        .0
        .into_iter()
        .chain(physical.1)
        .zip(visual.0.into_iter().chain(visual.1))
    {
        assert!(
            (actual - expected).abs() < 0.001,
            "collision extent {actual} != visual extent {expected}"
        );
    }
    let mdl = fs::read(
        &output
            .files
            .iter()
            .find(|file| file.kind == ModelOutputKind::Mdl)
            .unwrap()
            .path,
    )
    .unwrap();
    assert_eq!(f32::from_le_bytes(mdl[328..332].try_into().unwrap()), 1.0);
    assert!(
        phy.text
            .unwrap()
            .bytes
            .windows(b"\"mass\" \"1.000000\"".len())
            .any(|value| value == b"\"mass\" \"1.000000\"")
    );
}

#[test]
fn prop_mass_and_surface_are_consistent_in_model_and_collision_files() {
    let (root, source) = fixture();
    let mut request = ModelCompileRequest::new(source, root.path().join("physics"));
    request.export_materials = false;
    request.collision = ModelCollision::Automatic(CollisionDetail::Simple);
    request.collision_mass = 37.5;
    request.surface_property = Some("painted_metal".into());
    let output = compile_model(&request).unwrap();
    let bytes = |kind| {
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
    let mdl = bytes(ModelOutputKind::Mdl);
    assert_eq!(f32::from_le_bytes(mdl[328..332].try_into().unwrap()), 37.5);
    let offset = u32::from_le_bytes(mdl[308..312].try_into().unwrap()) as usize;
    let end = mdl[offset..].iter().position(|byte| *byte == 0).unwrap();
    assert_eq!(&mdl[offset..offset + end], b"painted_metal");
    let phy_bytes = bytes(ModelOutputKind::Phy);
    let phy = vformats::phy::parse(&phy_bytes, &vformats::Limits::default()).unwrap();
    assert_eq!(phy.header.mdl_checksum, output.checksum as u32);
    let text = std::str::from_utf8(phy.text.unwrap().bytes).unwrap();
    assert!(text.contains("\"mass\" \"37.500000\""));
    assert!(text.contains("\"totalmass\" \"37.500000\""));
    assert!(text.contains("\"surfaceprop\" \"painted_metal\""));

    request.destination = root.path().join("no_collision");
    request.collision = ModelCollision::Existing;
    request.collision_mass = f32::NAN;
    let output = compile_model(&request).unwrap();
    assert!(
        !output
            .files
            .iter()
            .any(|file| file.kind == ModelOutputKind::Phy)
    );
    let mdl = fs::read(
        &output
            .files
            .iter()
            .find(|file| file.kind == ModelOutputKind::Mdl)
            .unwrap()
            .path,
    )
    .unwrap();
    assert_eq!(f32::from_le_bytes(mdl[328..332].try_into().unwrap()), 1.0);
    let offset = u32::from_le_bytes(mdl[308..312].try_into().unwrap()) as usize;
    let end = mdl[offset..].iter().position(|byte| *byte == 0).unwrap();
    assert_eq!(&mdl[offset..offset + end], b"painted_metal");
}

#[test]
fn tiny_total_mass_keeps_precision_and_studiomdl_solid_mass_floor() {
    let (root, source) = fixture();
    let mut request = ModelCompileRequest::new(source, root.path().join("tiny_mass"));
    request.export_materials = false;
    request.collision = ModelCollision::Automatic(CollisionDetail::Simple);
    request.collision_mass = 1e-7;
    let output = compile_model(&request).unwrap();
    let bytes = |kind| {
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
    let mdl = bytes(ModelOutputKind::Mdl);
    assert_eq!(
        f32::from_le_bytes(mdl[328..332].try_into().unwrap()).to_bits(),
        request.collision_mass.to_bits()
    );
    let phy_bytes = bytes(ModelOutputKind::Phy);
    let phy = vformats::phy::parse(&phy_bytes, &vformats::Limits::default()).unwrap();
    let text = std::str::from_utf8(phy.text.unwrap().bytes).unwrap();
    assert!(text.contains("\"totalmass\" \"0.0000001\""));
    assert!(text.contains("\"mass\" \"1.000000\""));
}

#[test]
fn invalid_prop_physics_options_publish_nothing() {
    let (root, source) = fixture();
    let mut request = ModelCompileRequest::new(source, root.path().join("invalid_physics"));
    request.export_materials = false;
    request.collision = ModelCollision::Automatic(CollisionDetail::Simple);
    for mass in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        request.collision_mass = mass;
        assert!(matches!(
            compile_model(&request),
            Err(ModelCompileError::InvalidOption(_))
        ));
        assert!(!request.destination.exists());
    }
    request.collision_mass = 1.0;
    for property in [
        "",
        "metal wood",
        "metal\n",
        "\"metal\"",
        "metal{",
        "metal}",
        "metal//",
        "métal",
        "metal\\wood",
    ] {
        request.surface_property = Some(property.into());
        assert!(matches!(
            compile_model(&request),
            Err(ModelCompileError::InvalidOption(_))
        ));
        assert!(!request.destination.exists());
    }
}

#[test]
fn authored_descriptions_reject_automatic_physics_overrides() {
    let (root, _) = fixture();
    for extension in ["qc", "toml"] {
        let source = root.path().join(format!("authored.{extension}"));
        fs::write(
            &source,
            if extension == "qc" {
                "$modelname props/authored.mdl\n$body body cube.smd\n$sequence idle cube.smd\n"
            } else {
                "[model]\nname = \"props/authored.mdl\"\n"
            },
        )
        .unwrap();
        let mut request = ModelCompileRequest::new(source, root.path().join("override"));
        request.export_materials = false;
        request.collision = ModelCollision::Automatic(CollisionDetail::Simple);
        request.collision_mass = 10.0;
        assert!(matches!(
            compile_model(&request),
            Err(ModelCompileError::InvalidOption(_))
        ));
        assert!(!request.destination.exists());
        request.collision = ModelCollision::Existing;
        request.surface_property = Some("metal".into());
        assert!(matches!(
            compile_model(&request),
            Err(ModelCompileError::InvalidOption(_))
        ));
        assert!(!request.destination.exists());
    }
}

#[test]
fn native_vtf_skin_texture_preserves_the_original_file_and_mipmaps() {
    use gmpublished_backend::model_project::{ModelSkin, SkinMaterial};
    use std::collections::BTreeMap;
    let (root, source) = fixture();
    image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 100, 50, 255]))
        .save(root.path().join("material.png"))
        .unwrap();
    let mut request = ModelCompileRequest::new(source, root.path().join("original"));
    let first = compile_model(&request).unwrap();
    let texture = first
        .files
        .iter()
        .find(|file| file.kind == ModelOutputKind::Texture)
        .unwrap()
        .path
        .clone();
    let original = fs::read(&texture).unwrap();
    request.destination = root.path().join("variant");
    request.skins.push(ModelSkin {
        name: "VTF".into(),
        replacements: BTreeMap::from([(
            "material".into(),
            SkinMaterial::Texture { path: texture },
        )]),
    });
    compile_model(&request).unwrap();
    assert_eq!(
        fs::read(
            request
                .destination
                .join("materials/__skins/skin_1/material.vtf")
        )
        .unwrap(),
        original
    );
}

#[test]
fn multi_bone_prop_collision_matches_playback_and_survives_qc_export() {
    use gmpublished_backend::model_decompile::load_animation_preview;
    use gmpublished_backend::model_project::save_model_qc;

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("rig.smd");
    fs::write(
        &source,
        include_bytes!("../../vendor/mdlc/tests/fixtures/rig.smd"),
    )
    .unwrap();
    let mut request = ModelCompileRequest::new(source, root.path().join("rigid"));
    request.collision = ModelCollision::Automatic(CollisionDetail::Simple);
    request.export_materials = false;
    let original = compile_model(&request).unwrap();
    let qc = root.path().join("prop.qc");
    save_model_qc(&request, &qc).unwrap();
    let mut saved = ModelCompileRequest::new(qc, root.path().join("saved"));
    saved.export_materials = false;
    let rebuilt = compile_model(&saved).unwrap();
    for output in [&original, &rebuilt] {
        let bytes = |kind| {
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
        let mdl = bytes(ModelOutputKind::Mdl);
        let vvd = bytes(ModelOutputKind::Vvd);
        let vtx = bytes(ModelOutputKind::Vtx);
        let phy = bytes(ModelOutputKind::Phy);
        let limits = vformats::Limits::default();
        let model = vformats::mdl::parse_mdl(&mdl, &limits).unwrap();
        assert_eq!(model.bones.len(), 3);
        let playback = load_animation_preview(&mdl, &vvd, &vtx, None).unwrap();
        let visual = playback.evaluate(0, 0.0).unwrap();
        let physics = vformats::phy::parse(&phy, &limits).unwrap();
        assert_eq!(physics.solids.len(), 1);
        assert_eq!(physics.solids[0].ledges.len(), 1);
        for axis in 0..3 {
            for reduce in [f32::min, f32::max] {
                let visual_bound = visual
                    .iter()
                    .flatten()
                    .map(|vertex| vertex.position[axis])
                    .reduce(reduce)
                    .unwrap();
                let physical_bound = physics.solids[0].ledges[0]
                    .vertices
                    .iter()
                    .map(|point| point[axis])
                    .reduce(reduce)
                    .unwrap();
                assert!(
                    (visual_bound - physical_bound).abs() < 0.001,
                    "collision {physical_bound} does not match playback {visual_bound}"
                );
            }
        }
    }
}

#[test]
fn qc_automatic_mass_uses_game_density_in_both_model_and_collision_outputs() {
    let (root, _) = fixture();
    let source = root.path().join("mass.qc");
    fs::write(&source, "$modelname props/automatic.mdl\n$body body cube.smd\n$surfaceprop metal\n$collisionmodel cube.smd { $automass }\n").unwrap();
    let mut request = ModelCompileRequest::new(source, root.path().join("compiled"));
    request.export_materials = false;
    gmpublished_backend::model_compile::load_surface_properties(&mut request, |path| {
        (path == "scripts/surfaceproperties.txt")
            .then(|| br#""metal" { "density" "7800" "thickness" "0" }"#.to_vec())
    })
    .unwrap();
    let output = compile_model(&request).unwrap();
    let mdl = fs::read(
        &output
            .files
            .iter()
            .find(|file| file.kind == ModelOutputKind::Mdl)
            .unwrap()
            .path,
    )
    .unwrap();
    let mass = f32::from_le_bytes(mdl[328..332].try_into().unwrap());
    let expected = 4.0_f32 * 6.0 * 8.0 * 0.0254_f32.powi(3) * 7800.0;
    assert!((mass - expected).abs() < 0.001);
    let phy_bytes = fs::read(
        &output
            .files
            .iter()
            .find(|file| file.kind == ModelOutputKind::Phy)
            .unwrap()
            .path,
    )
    .unwrap();
    let phy = vformats::phy::parse(&phy_bytes, &vformats::Limits::default()).unwrap();
    let text = std::str::from_utf8(phy.text.unwrap().bytes).unwrap();
    assert!(text.contains(&format!("\"totalmass\" \"{mass:.6}\"")));
}

#[test]
fn broken_game_surface_manifest_only_blocks_automatic_mass() {
    let (root, _) = fixture();
    let source = root.path().join("mass.qc");
    for (mass_option, automatic) in [("$mass 20", false), ("$automass", true)] {
        fs::write(
            &source,
            format!("$modelname props/mass.mdl\n$body body cube.smd\n$collisionmodel cube.smd {{ {mass_option} }}\n"),
        )
        .unwrap();
        let mut request = ModelCompileRequest::new(source.clone(), root.path().join("compiled"));
        request.export_materials = false;
        let result =
            gmpublished_backend::model_compile::load_surface_properties(&mut request, |_| {
                Some(b"invalid manifest".to_vec())
            });
        if automatic {
            assert!(result.is_err());
        } else {
            result.unwrap();
            assert!(compile_model(&request).is_ok());
        }
    }
}

#[test]
fn compiler_warnings_are_returned_to_the_app_without_leaking_between_exports() {
    let (root, _) = fixture();
    let source = root.path().join("warnings.qc");
    fs::write(
        &source,
        "$modelname props/warnings.mdl\n$body body cube.smd\n$externaltextures\n",
    )
    .unwrap();
    let mut request = ModelCompileRequest::new(source.clone(), root.path().join("first"));
    request.export_materials = false;
    let output = compile_model(&request).unwrap();
    assert!(
        output
            .warnings
            .iter()
            .any(|warning| warning.contains("$externaltextures"))
    );
    fs::write(
        &source,
        "$modelname props/warnings.mdl\n$body body cube.smd\n",
    )
    .unwrap();
    request.destination = root.path().join("second");
    assert!(compile_model(&request).unwrap().warnings.is_empty());
}

#[test]
fn qc_event_outside_the_animation_reports_a_warning_and_writes_cycle_zero() {
    let (root, _) = fixture();
    let source = root.path().join("events.qc");
    fs::write(&source, "$modelname models/tests/events.mdl\n$body body cube.smd\n$sequence idle cube.smd { event 5004 30 \"sound\" }\n").unwrap();
    let mut request = ModelCompileRequest::new(source, root.path().join("addon"));
    request.export_materials = false;
    let output = compile_model(&request).unwrap();
    assert!(
        output
            .warnings
            .iter()
            .any(|warning| warning.contains("Event 5004")
                && warning.contains("idle")
                && warning.contains("30"))
    );
    let model = output
        .files
        .iter()
        .find(|file| file.kind == ModelOutputKind::Mdl)
        .unwrap();
    let bytes = fs::read(&model.path).unwrap();
    let integer = |at| i32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let sequence = integer(0xC0);
    assert_eq!(integer(sequence + 24), 1);
    let event = sequence + integer(sequence + 28);
    assert_eq!(
        f32::from_le_bytes(bytes[event..event + 4].try_into().unwrap()),
        0.0
    );
    assert_eq!(integer(event + 4), 5004);
}
