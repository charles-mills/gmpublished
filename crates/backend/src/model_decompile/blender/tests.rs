use std::fs;

use mdlc::{bone_math, fbx::FbxOpts, pipeline, qc};
use serde_json::Value;

use super::*;

const RIG: &str = "version 1\nnodes\n0 \"root\" -1\n1 \"child\" 0\nend\nskeleton\ntime 0\n0 1 2 3 0.2 0.3 0.4\n1 0 0 5 0.1 0 -0.2\ntime 1\n0 4 5 6 0.2 0.3 0.8\n1 0 0 8 0.3 0 -0.2\nend\ntriangles\npaint\n0 2 3 4 0 0 1 0.1 0.2 1 0 1\n1 6 3 4 0 0 1 0.8 0.2 1 1 1\n0 2 7 4 0 0 1 0.1 0.9 2 0 0.25 1 0.75\nend\n";

fn fixture(qc: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("rig.smd"), RIG).unwrap();
    fs::write(
        root.path().join("model.qc"),
        format!("$modelname models/test.mdl\n{qc}"),
    )
    .unwrap();
    let description = qc::parse_qc_file(&root.path().join("model.qc")).unwrap();
    let output = pipeline::build(
        &description,
        root.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    pipeline::write_files(&output, root.path()).unwrap();
    root
}

fn document(path: &Path) -> (Value, Vec<u8>) {
    let bytes = fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"glTF");
    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize,
        bytes.len()
    );
    let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let json = serde_json::from_slice(&bytes[20..20 + length]).unwrap();
    (json, bytes[28 + length..].to_vec())
}

fn floats(document: &Value, bin: &[u8], accessor: usize) -> Vec<f32> {
    let accessor = &document["accessors"][accessor];
    assert_eq!(accessor["componentType"], 5126);
    let view = &document["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let start = view["byteOffset"].as_u64().unwrap_or(0) as usize;
    let length = view["byteLength"].as_u64().unwrap() as usize;
    bin[start..start + length]
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect()
}

fn near(a: f32, b: f32) {
    assert!((a - b).abs() < 0.00002, "{a} != {b}");
}

fn indices(document: &Value, bin: &[u8], accessor: usize) -> Vec<u32> {
    let accessor = &document["accessors"][accessor];
    assert_eq!(accessor["componentType"], 5125);
    let view = &document["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let start = view["byteOffset"].as_u64().unwrap_or(0) as usize;
    let length = view["byteLength"].as_u64().unwrap() as usize;
    bin[start..start + length]
        .chunks_exact(4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
        .collect()
}

fn add_reversed_face(compiled: &mut pipeline::PipelineOutput) {
    let mesh = &mut compiled.compiled.bodyparts[0].models[0].meshes[0];
    let [a, b, c] = mesh.triangles[0];
    mesh.triangles.push([a, c, b]);
    let topology = mdlc::vtx_writer::write_vtx(&compiled.compiled).unwrap();
    assert_eq!(topology.checksum, compiled.checksum);
    compiled.vtx = topology.bytes;
}

#[test]
fn opposite_faces_keep_both_windings_and_all_corner_attributes_in_blender() {
    let root = fixture("$body body rig.smd\n$sequence walk rig.smd\n");
    let description = qc::parse_qc_file(&root.path().join("model.qc")).unwrap();
    let mut compiled = pipeline::build(
        &description,
        root.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    add_reversed_face(&mut compiled);
    pipeline::write_files(&compiled, root.path()).unwrap();
    let report = export_blender_model(
        &root.path().join("models/test.mdl"),
        &root.path().join("blender"),
    )
    .unwrap();
    let (document, bin) = document(&report.model_path);
    let primitive = &document["meshes"][0]["primitives"][0];
    let positions =
        &document["accessors"][primitive["attributes"]["POSITION"].as_u64().unwrap() as usize];
    assert_eq!(positions["count"], 6);
    let indices = indices(
        &document,
        &bin,
        primitive["indices"].as_u64().unwrap() as usize,
    );
    let mut faces = std::collections::HashSet::new();
    for triangle in indices.chunks_exact(3) {
        let mut key = [triangle[0], triangle[1], triangle[2]];
        key.sort_unstable();
        assert!(
            faces.insert(key),
            "Blender would discard a repeated triangle"
        );
    }
    assert_eq!(faces.len(), 2);
    let geometry = mdlc::gltf::read(&report.model_path, "export", &FbxOpts::default()).unwrap();
    assert_eq!(geometry.smd.triangles.len(), 2);
    let [first, second] = geometry.smd.triangles.as_slice() else {
        unreachable!()
    };
    for vertex in &first.vertices {
        let other = second
            .vertices
            .iter()
            .find(|other| other.position == vertex.position)
            .unwrap();
        assert_eq!(vertex.normal, other.normal);
        assert_eq!(vertex.uv, other.uv);
        assert_eq!(vertex.links, other.links);
    }
    let normal = |face: &mdlc::smd::SmdTriangle| {
        let [a, b, c] = face.vertices.each_ref().map(|vertex| vertex.position);
        let u: [f32; 3] = std::array::from_fn(|axis| b[axis] - a[axis]);
        let v: [f32; 3] = std::array::from_fn(|axis| c[axis] - a[axis]);
        [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ]
    };
    assert!(
        normal(first)
            .iter()
            .zip(normal(second))
            .map(|(a, b)| a * b)
            .sum::<f32>()
            < 0.0
    );
}

#[test]
fn blender_file_preserves_geometry_uv_winding_and_joint_weights_through_gltf_reader() {
    let root = fixture("$body body rig.smd\n$sequence walk rig.smd fps 30\n");
    let report = export_blender_model(
        &root.path().join("models/test.mdl"),
        &root.path().join("blender"),
    )
    .unwrap();
    let geometry = mdlc::gltf::read(
        &report.model_path,
        "export",
        &FbxOpts {
            scale: 1.0 / INCHES_TO_METERS,
            ..FbxOpts::default()
        },
    )
    .unwrap();
    let mut original = mdlc::smd::parse_smd(RIG).unwrap();
    for vertex in &mut original.triangles[0].vertices {
        vertex.position = [vertex.position[1], vertex.position[2], vertex.position[0]];
        vertex.normal = [vertex.normal[1], vertex.normal[2], vertex.normal[0]];
    }
    assert_eq!(geometry.smd.triangles.len(), 1);
    let triangle = &geometry.smd.triangles[0];
    let [a, b, c] = triangle.vertices.each_ref().map(|vertex| vertex.position);
    let u: [f32; 3] = std::array::from_fn(|axis| b[axis] - a[axis]);
    let v: [f32; 3] = std::array::from_fn(|axis| c[axis] - a[axis]);
    let cross = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    assert!(
        cross
            .iter()
            .zip(triangle.vertices[0].normal)
            .map(|(a, b)| a * b)
            .sum::<f32>()
            > 0.0,
        "GLB triangle is back-facing relative to its normal"
    );
    let original = &original.triangles[0];
    let start = (0..3)
        .find(|&start| {
            (0..3).all(|corner| {
                (0..3).all(|axis| {
                    (triangle.vertices[corner].position[axis]
                        - original.vertices[(corner + start) % 3].position[axis])
                        .abs()
                        < 0.001
                })
            })
        })
        .unwrap_or_else(|| {
            panic!(
                "GLB changed triangle winding or position: actual={:?}, original={:?}",
                triangle
                    .vertices
                    .iter()
                    .map(|vertex| vertex.position)
                    .collect::<Vec<_>>(),
                original
                    .vertices
                    .iter()
                    .map(|vertex| vertex.position)
                    .collect::<Vec<_>>()
            )
        });
    for (corner, vertex) in triangle.vertices.iter().enumerate() {
        let expected = &original.vertices[(corner + start) % 3];
        for axis in 0..3 {
            near(vertex.normal[axis], expected.normal[axis]);
        }
        for axis in 0..2 {
            near(vertex.uv[axis], expected.uv[axis]);
        }
        assert_eq!(vertex.links.len(), expected.links.len());
        for link in &vertex.links {
            let name = &geometry.smd.nodes[link.bone as usize].name;
            let expected_link = expected
                .links
                .iter()
                .find(|link| {
                    if link.bone == 0 {
                        name == "root"
                    } else {
                        name == "child"
                    }
                })
                .unwrap();
            near(link.weight, expected_link.weight);
        }
    }
    assert!(
        geometry
            .anim_stacks
            .iter()
            .any(|name| name.contains("walk"))
    );
}

#[test]
fn converted_bind_matrices_cancel_nontrivial_hierarchical_joint_transforms() {
    let root = fixture("$body body rig.smd\n$sequence walk rig.smd fps 30\n");
    let report = export_blender_model(
        &root.path().join("models/test.mdl"),
        &root.path().join("blender"),
    )
    .unwrap();
    let (document, bin) = document(&report.model_path);
    let skin = &document["skins"][0];
    let inverse = floats(
        &document,
        &bin,
        skin["inverseBindMatrices"].as_u64().unwrap() as usize,
    );
    let mut parents = vec![-1; document["nodes"].as_array().unwrap().len()];
    for (index, node) in document["nodes"].as_array().unwrap().iter().enumerate() {
        if let Some(children) = node["children"].as_array() {
            for child in children {
                parents[child.as_u64().unwrap() as usize] = index as i32;
            }
        }
    }
    let local = document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| {
            let q = std::array::from_fn(|axis| {
                node["rotation"][axis]
                    .as_f64()
                    .map_or(if axis == 3 { 1.0 } else { 0.0 }, |value| value as f32)
            });
            let p = std::array::from_fn(|axis| {
                node["translation"][axis].as_f64().unwrap_or(0.0) as f32
            });
            bone_math::quaternion_local_transform(q, p)
        })
        .collect::<Vec<_>>();
    let mut world = local.clone();
    for index in 0..world.len() {
        if parents[index] >= 0 {
            world[index] = bone_math::concat(&world[parents[index] as usize], &local[index]);
        }
    }
    for (joint, node) in skin["joints"].as_array().unwrap().iter().enumerate() {
        let matrix = &inverse[joint * 16..joint * 16 + 16];
        let inverse = std::array::from_fn(|index| matrix[(index % 4) * 4 + index / 4]);
        let combined = bone_math::concat(&world[node.as_u64().unwrap() as usize], &inverse);
        for (index, &value) in combined.iter().enumerate() {
            near(value, if index / 4 == index % 4 { 1.0 } else { 0.0 });
        }
    }
    let root_node = document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .position(|node| node["name"] == "root")
        .unwrap();
    let channel = document["animations"][0]["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|channel| {
            channel["target"]["node"] == root_node && channel["target"]["path"] == "translation"
        })
        .unwrap();
    let sampler =
        &document["animations"][0]["samplers"][channel["sampler"].as_u64().unwrap() as usize];
    let translations = floats(
        &document,
        &bin,
        sampler["output"].as_u64().unwrap() as usize,
    );
    for (actual, expected) in translations[translations.len() - 3..]
        .iter()
        .zip([4.0, 6.0, -5.0])
    {
        assert!(
            (*actual - expected * INCHES_TO_METERS).abs() < 0.0002,
            "{actual} != {} after Source animation compression",
            expected * INCHES_TO_METERS
        );
    }
}

#[test]
fn animated_blender_model_recompiles_with_original_geometry_and_deformation() {
    let root = fixture("$body body rig.smd\n$sequence walk rig.smd fps 30\n");
    let source = root.path().join("models/test.mdl");
    let report = export_blender_model(&source, &root.path().join("blender")).unwrap();
    let mut request = crate::model_compile::ModelCompileRequest::new(
        report.model_path,
        root.path().join("rebuilt"),
    );
    request.export_materials = false;
    let rebuilt = crate::model_compile::compile_model(&request).unwrap();
    let rebuilt_path = &rebuilt
        .files
        .iter()
        .find(|file| file.kind == crate::model_compile::ModelOutputKind::Mdl)
        .unwrap()
        .path;
    let rebuilt_mdl = fs::read(rebuilt_path).unwrap();
    let rebuilt_vvd = fs::read(rebuilt_path.with_extension("vvd")).unwrap();
    let rebuilt_vtx = fs::read(rebuilt_path.with_extension("dx90.vtx")).unwrap();
    let rebuilt_ani = fs::read(rebuilt_path.with_extension("ani")).ok();
    let original_mdl = fs::read(&source).unwrap();
    let original_vvd = fs::read(source.with_extension("vvd")).unwrap();
    let original_vtx = fs::read(source.with_extension("dx90.vtx")).unwrap();
    let original = super::super::animations::preview::load_animation_preview(
        &original_mdl,
        &original_vvd,
        &original_vtx,
        None,
    )
    .unwrap();
    let compiled = super::super::animations::preview::load_animation_preview(
        &rebuilt_mdl,
        &rebuilt_vvd,
        &rebuilt_vtx,
        rebuilt_ani.as_deref(),
    )
    .unwrap();
    let mut differences = Vec::new();
    let bound =
        |offset: usize| f32::from_le_bytes(rebuilt_mdl[offset..offset + 4].try_into().unwrap());
    let minimum: [f32; 3] = std::array::from_fn(|axis| bound(104 + axis * 4));
    let maximum: [f32; 3] = std::array::from_fn(|axis| bound(116 + axis * 4));
    for cycle in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let expected = original.evaluate(0, cycle).unwrap();
        let actual = compiled.evaluate(0, cycle).unwrap();
        let expected = expected
            .iter()
            .flatten()
            .map(|vertex| vertex.position)
            .collect::<Vec<_>>();
        let actual = actual
            .iter()
            .flatten()
            .map(|vertex| vertex.position)
            .collect::<Vec<_>>();
        for point in &actual {
            for axis in 0..3 {
                assert!(
                    point[axis] >= minimum[axis] - 0.03 && point[axis] <= maximum[axis] + 0.03,
                    "Recompiled model bounds exclude animated geometry at cycle {cycle}: {point:?} outside {minimum:?}..{maximum:?}"
                );
            }
        }
        assert_eq!(actual.len(), expected.len());
        let error = expected
            .iter()
            .map(|a| {
                actual
                    .iter()
                    .map(|b| {
                        a.iter()
                            .zip(b)
                            .map(|(a, b)| (a - b).powi(2))
                            .sum::<f32>()
                            .sqrt()
                    })
                    .fold(f32::INFINITY, f32::min)
            })
            .fold(0.0f32, f32::max);
        if error > 0.03 {
            differences.push((cycle, error, expected, actual));
        }
    }
    assert!(
        differences.is_empty(),
        "Animated model changed during Blender-file recompile: {differences:?}"
    );
}

#[test]
fn bodygroup_choices_lods_and_skin_variants_are_separate_and_editable() {
    let root = fixture(
        "$bodygroup body {\n studio rig.smd\n blank\n studio rig.smd\n}\n$sequence walk rig.smd fps 30\n$texturegroup skins { { paint } { red } }\n$lod 25 { replacemodel rig.smd rig.smd }\n",
    );
    let report = export_blender_model(
        &root.path().join("models/test.mdl"),
        &root.path().join("blender"),
    )
    .unwrap();
    let (document, _) = document(&report.model_path);
    assert_eq!(document["scenes"].as_array().unwrap().len(), 6);
    for scene in document["scenes"].as_array().unwrap() {
        let meshes = scene["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| {
                document["nodes"][node.as_u64().unwrap() as usize]
                    .get("mesh")
                    .is_some()
            })
            .count();
        assert_eq!(meshes, usize::from(scene["extras"]["sourceChoice"] != 1));
    }
    assert_eq!(
        document["extensions"]["KHR_materials_variants"]["variants"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let mappings =
        &document["meshes"][0]["primitives"][0]["extensions"]["KHR_materials_variants"]["mappings"];
    assert_ne!(mappings[0]["material"], mappings[1]["material"]);
    assert_eq!(
        document["materials"][mappings[1]["material"].as_u64().unwrap() as usize]["name"],
        "red"
    );
}

#[test]
fn lower_lods_do_not_contain_loose_vertices_from_the_unified_source_pool() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("rig.smd"), RIG).unwrap();
    fs::write(root.path().join("low.smd"), RIG.replace("6 3 4", "4 3 4")).unwrap();
    let description = qc::parse_qc_str("$modelname models/test.mdl\n$body body rig.smd\n$sequence walk rig.smd\n$lod 25 { replacemodel rig.smd low.smd }\n",root.path()).unwrap();
    let compiled = pipeline::build(
        &description,
        root.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    pipeline::write_files(&compiled, root.path()).unwrap();
    let source = vformats::mdl::parse_vvd(&compiled.vvd, &vformats::Limits::default()).unwrap();
    assert!(source.vertices.len() > 3);
    let report = export_blender_model(
        &root.path().join("models/test.mdl"),
        &root.path().join("blender"),
    )
    .unwrap();
    let (document, bin) = document(&report.model_path);
    for mesh in document["meshes"].as_array().unwrap() {
        let primitive = &mesh["primitives"][0];
        let positions =
            &document["accessors"][primitive["attributes"]["POSITION"].as_u64().unwrap() as usize];
        assert_eq!(positions["count"], 3);
        let referenced = indices(
            &document,
            &bin,
            primitive["indices"].as_u64().unwrap() as usize,
        )
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(referenced, [0, 1, 2].into_iter().collect());
    }
}

#[test]
fn source_authoring_metadata_is_kept_on_a_blender_importable_object() {
    let root = fixture(
        "$body body rig.smd\n$sequence walk rig.smd\n$attachment muzzle root 1 2 3\n$hbox 1 child -1 -2 -3 1 2 3\n",
    );
    let report = export_blender_model(
        &root.path().join("models/test.mdl"),
        &root.path().join("blender"),
    )
    .unwrap();
    let (document, _) = document(&report.model_path);
    let metadata: Value = serde_json::from_str(
        document["nodes"][0]["extras"]["gmpublishedSourceMetadata"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["sourceUnits"], "inches");
    assert_eq!(metadata["sourceAttachments"][0]["name"], "muzzle");
    assert_eq!(
        metadata["sourceHitboxSets"][0]["boxes"][0]["minimum"],
        serde_json::json!([-1.0, -2.0, -3.0])
    );
    assert_eq!(metadata["sourceHitboxSets"][0]["boxes"][0]["group"], 1);
}

#[test]
fn source_44_absolute_empty_hitbox_names_keep_metadata_without_spurious_warnings() {
    let root =
        fixture("$body body rig.smd\n$sequence walk rig.smd\n$hbox 1 child -1 -2 -3 1 2 3\n");
    let source = root.path().join("models/test.mdl");
    let mut bytes = fs::read(&source).unwrap();
    let number =
        |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    let set = number(176);
    let hitbox = set + number(set + 8);
    let empty = bytes.iter().rposition(|byte| *byte == 0).unwrap();
    assert!(hitbox + empty >= bytes.len());
    bytes[4..8].copy_from_slice(&44i32.to_le_bytes());
    bytes[hitbox + 32..hitbox + 36].copy_from_slice(&(empty as i32).to_le_bytes());
    fs::write(&source, bytes).unwrap();
    let report = export_blender_model(&source, &root.path().join("blender")).unwrap();
    assert!(
        report
            .warnings
            .iter()
            .all(|warning| !warning.contains("authoring settings could not be preserved")),
        "{:?}",
        report.warnings
    );
    let (document, _) = document(&report.model_path);
    let metadata: Value = serde_json::from_str(
        document["nodes"][0]["extras"]["gmpublishedSourceMetadata"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["sourceHitboxSets"][0]["boxes"][0]["name"], "");
    assert_eq!(metadata["sourceHitboxSets"][0]["boxes"][0]["group"], 1);
}

#[test]
fn wrinkle_flexes_export_as_native_shape_keys_with_auxiliary_values() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("mesh.dmx"),
        include_bytes!("../../../../vendor/mdlc/tests/fixtures/dmx.dmx"),
    )
    .unwrap();
    let description = qc::parse_qc_str(
        "$modelname models/test.mdl\n$model body mesh.dmx\n$sequence idle mesh.dmx\n",
        root.path(),
    )
    .unwrap();
    let mut compiled = pipeline::build(
        &description,
        root.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    add_reversed_face(&mut compiled);
    pipeline::write_files(&compiled, root.path()).unwrap();
    let report = export_blender_model(
        &root.path().join("models/test.mdl"),
        &root.path().join("blender"),
    )
    .unwrap();
    let (document, bin) = document(&report.model_path);
    let mesh = &document["meshes"][0];
    assert_eq!(mesh["extras"]["targetNames"][0], "smile");
    assert_eq!(mesh["extras"]["sourceFlexes"][0]["wrinkles"][0][2], 0.5);
    let target = &mesh["primitives"][0]["targets"][0];
    let deltas = floats(
        &document,
        &bin,
        target["POSITION"].as_u64().unwrap() as usize,
    );
    assert_eq!(
        deltas.iter().filter(|value| value.abs() > 0.00001).count(),
        2
    );
    near(
        *deltas.iter().find(|value| value.abs() > 0.00001).unwrap(),
        INCHES_TO_METERS,
    );
    let geometry = mdlc::gltf::read(&report.model_path, "export", &FbxOpts::default()).unwrap();
    assert_eq!(geometry.shape_keys.len(), 1);
    assert_eq!(geometry.shape_keys[0].name, "smile");
}

#[test]
fn failed_or_cancelled_export_leaves_no_partial_destination_and_never_overwrites() {
    let root = fixture("$body body rig.smd\n$sequence walk rig.smd\n");
    let source = root.path().join("models/test.mdl");
    let destination = root.path().join("blender");
    assert_eq!(
        export_blender_model_with_cancel(&source, &destination, &AtomicBool::new(true), |_| None)
            .unwrap_err(),
        DecompileError::Cancelled
    );
    assert!(!destination.exists());
    fs::write(source.with_extension("vvd"), b"truncated").unwrap();
    assert!(export_blender_model(&source, &destination).is_err());
    assert!(!destination.exists());
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("keep"), b"original").unwrap();
    assert!(matches!(
        export_blender_model(&source, &destination),
        Err(DecompileError::DestinationExists(_))
    ));
    assert_eq!(fs::read(destination.join("keep")).unwrap(), b"original");
}

fn collision_tetra(offset: f32) -> mdlc::phy::PhyHull {
    mdlc::phy::PhyHull {
        vertices: vec![
            [offset, 0.0, 0.0],
            [offset + 4.0, 0.0, 0.0],
            [offset, 7.0, 0.0],
            [offset, 0.0, 9.0],
        ],
        faces: vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]],
    }
}

#[test]
fn collision_export_keeps_each_convex_piece_in_a_separate_hidden_scene_with_engine_axes() {
    let root = fixture("$body body rig.smd\n$sequence walk rig.smd\n");
    let source = root.path().join("models/test.mdl");
    let raw_mdl = fs::read(&source).unwrap();
    let model = vformats::mdl::parse_mdl(&raw_mdl, &vformats::Limits::default()).unwrap();
    let raw_phy = mdlc::phy::write_phy_multi(
        &[vec![collision_tetra(0.0), collision_tetra(20.0)]],
        &[mdlc::phy::PhySolid::prop("collision")],
        &mdlc::phy::PhyParams::new("collision", model.checksum),
    )
    .unwrap();
    fs::write(source.with_extension("phy"), &raw_phy).unwrap();
    let report = export_blender_model(&source, &root.path().join("blender")).unwrap();
    let (document, bin) = document(&report.model_path);
    let collision = document["scenes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scene| scene["name"] == "Collision")
        .unwrap();
    assert_ne!(collision, &document["scenes"][0]);
    let collision_root = collision["nodes"][1].as_u64().unwrap() as usize;
    assert!(
        !document["scenes"][0]["nodes"]
            .as_array()
            .unwrap()
            .contains(&json!(collision_root))
    );
    let pieces = document["nodes"][collision_root]["children"]
        .as_array()
        .unwrap();
    assert_eq!(pieces.len(), 2);
    for (piece, node) in pieces.iter().enumerate() {
        let node = &document["nodes"][node.as_u64().unwrap() as usize];
        assert_eq!(node["extras"]["sourceCollisionPiece"], piece);
        assert_eq!(node["extras"]["gmpublishedCollision"], true);
        assert!(node.get("skin").is_none());
        let mesh = &document["meshes"][node["mesh"].as_u64().unwrap() as usize];
        let positions = floats(
            &document,
            &bin,
            mesh["primitives"][0]["attributes"]["POSITION"]
                .as_u64()
                .unwrap() as usize,
        );
        let offset = piece as f32 * 20.0;
        let expected = [
            [offset, 0.0, 0.0],
            [offset + 4.0, 0.0, 0.0],
            [offset, 0.0, -7.0],
            [offset, 9.0, 0.0],
        ]
        .map(|point| point.map(|value| value * 0.0254));
        for point in positions.chunks_exact(3) {
            assert!(
                expected.iter().any(|other| point
                    .iter()
                    .zip(other)
                    .all(|(a, b)| (a - b).abs() < 0.00002)),
                "{point:?}"
            );
        }
        assert_eq!(positions.len(), 12);
    }
    let parts = mdlc::gltf::source_parts(&report.model_path, "collision").unwrap();
    assert_eq!(
        parts
            .iter()
            .filter(|part| part.name.starts_with("collision / "))
            .count(),
        2
    );
    let metadata: Value = serde_json::from_str(
        document["nodes"][collision_root]["extras"]["gmpublishedSourceMetadata"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(
        metadata["sourcePhysicsKeyvalues"]
            .as_str()
            .unwrap()
            .contains("editparams")
    );
    assert!(
        !report.warnings.iter().any(
            |warning| warning.contains("physics mesh and ragdoll constraints are not included")
        )
    );
}

#[test]
fn ragdoll_collision_uses_bone_local_engine_axes_and_a_matching_blender_skin() {
    let root = fixture("$body body rig.smd\n$sequence walk rig.smd\n");
    let source = root.path().join("models/test.mdl");
    let raw_mdl = fs::read(&source).unwrap();
    let model = vformats::mdl::parse_mdl(&raw_mdl, &vformats::Limits::default()).unwrap();
    let mut hull = collision_tetra(0.0);
    for point in &mut hull.vertices {
        *point = [point[1], -point[0], point[2]];
    }
    let raw_phy = mdlc::phy::write_phy(
        &[hull.clone(), hull],
        &[
            mdlc::phy::PhySolid::ragdoll(0, "root", None),
            mdlc::phy::PhySolid::ragdoll(1, "child", Some("root".into())),
        ],
        &mdlc::phy::PhyParams::new("collision", model.checksum),
    )
    .unwrap();
    fs::write(source.with_extension("phy"), &raw_phy).unwrap();
    let pieces = super::super::load_collision_meshes(&raw_mdl, &raw_phy).unwrap();
    let root_world =
        bone_math::quaternion_local_transform(model.bones[0].quaternion, model.bones[0].position);
    let child_world = bone_math::concat(
        &root_world,
        &bone_math::quaternion_local_transform(model.bones[1].quaternion, model.bones[1].position),
    );
    let transform = |matrix: &bone_math::Matrix3x4, point: [f32; 3]| {
        std::array::from_fn::<_, 3, _>(|axis| {
            matrix[axis * 4] * point[0]
                + matrix[axis * 4 + 1] * point[1]
                + matrix[axis * 4 + 2] * point[2]
                + matrix[axis * 4 + 3]
        })
    };
    for (piece, world) in pieces.iter().zip([root_world, child_world]) {
        let expected = [
            [0.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            [0.0, 7.0, 0.0],
            [0.0, 0.0, 9.0],
        ]
        .map(|point| transform(&world, point));
        for point in &piece.vertices {
            assert!(
                expected
                    .iter()
                    .any(|other| point.iter().zip(other).all(|(a, b)| (a - b).abs() < 0.0001)),
                "{point:?}"
            );
        }
        assert_eq!(piece.vertices.len(), 4);
    }
    let report = export_blender_model(&source, &root.path().join("blender")).unwrap();
    let (document, bin) = document(&report.model_path);
    let skin = document["skins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|skin| skin["name"] == "Collision")
        .unwrap();
    let inverse = floats(
        &document,
        &bin,
        skin["inverseBindMatrices"].as_u64().unwrap() as usize,
    );
    for (index, world) in [root_world, child_world].iter().enumerate() {
        let matrix = &inverse[index * 16..(index + 1) * 16];
        for point in [[0.0, 0.0, 0.0], [4.0, 7.0, 9.0]] {
            let world = position(transform(world, point));
            let local: [f32; 3] = std::array::from_fn(|axis| {
                matrix[axis] * world[0]
                    + matrix[4 + axis] * world[1]
                    + matrix[8 + axis] * world[2]
                    + matrix[12 + axis]
            });
            for (actual, expected) in local.into_iter().zip(position(point)) {
                near(actual, expected);
            }
        }
    }
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("ragdoll constraints are preserved"))
    );
    let metadata: Value = serde_json::from_str(
        document["nodes"][0]["extras"]["gmpublishedSourceMetadata"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(
        metadata["sourcePhysics"]["sourcePhysicsKeyvalues"]
            .as_str()
            .unwrap()
            .contains("ragdollconstraint")
    );
}

#[test]
fn blender_collision_rejects_wrong_companions_before_creating_output() {
    let root = fixture("$body body rig.smd\n$sequence walk rig.smd\n");
    let source = root.path().join("models/test.mdl");
    let raw_mdl = fs::read(&source).unwrap();
    let model = vformats::mdl::parse_mdl(&raw_mdl, &vformats::Limits::default()).unwrap();
    let mut raw_phy = mdlc::phy::write_phy(
        &[collision_tetra(0.0)],
        &[mdlc::phy::PhySolid::prop("collision")],
        &mdlc::phy::PhyParams::new("collision", model.checksum),
    )
    .unwrap();
    raw_phy[12] ^= 1;
    fs::write(source.with_extension("phy"), &raw_phy).unwrap();
    let output = root.path().join("blender");
    assert!(
        matches!(export_blender_model(&source,&output),Err(DecompileError::InvalidModel(error)) if error.contains("checksums"))
    );
    assert!(!output.exists());
    raw_phy[12] ^= 1;
    raw_phy.truncate(80);
    fs::write(source.with_extension("phy"), &raw_phy).unwrap();
    assert!(export_blender_model(&source, &output).is_err());
    assert!(!output.exists());
}
