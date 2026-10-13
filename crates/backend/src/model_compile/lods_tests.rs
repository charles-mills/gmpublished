use super::*;
use crate::model_compile::{ModelOutputKind, compile_model, load_description};
use crate::model_project::save_model_qc;

pub(in crate::model_compile) fn compiled(
    request: &ModelCompileRequest,
) -> mdlc::model::CompiledModelDesc {
    let source = std::fs::canonicalize(&request.source).unwrap();
    let base = source.parent().unwrap();
    let desc = load_description(request, &source, base).unwrap();
    let mut compiled = mdlc::compile::compile(&desc, base).unwrap();
    crate::model_compile::apply_orientation(&mut compiled, &source, request.orientation).unwrap();
    compiled
}

pub(in crate::model_compile) fn assert_geometry(
    a: &mdlc::model::CompiledModelDesc,
    b: &mdlc::model::CompiledModelDesc,
) {
    assert_eq!(a.bodyparts.len(), b.bodyparts.len());
    for (a, b) in a.bodyparts.iter().zip(&b.bodyparts) {
        assert_eq!(a.models.len(), b.models.len());
        for (a, b) in a.models.iter().zip(&b.models) {
            assert_eq!(a.meshes.len(), b.meshes.len());
            for (index, (am, bm)) in a.meshes.iter().zip(&b.meshes).enumerate() {
                assert_eq!(am.material, bm.material);
                let a = a.lods.as_ref().map_or_else(
                    || mdlc::lod::MeshLods::single(am.vertices.clone(), am.triangles.clone()),
                    |lods| lods.meshes[index].clone(),
                );
                let b = b.lods.as_ref().map_or_else(
                    || mdlc::lod::MeshLods::single(bm.vertices.clone(), bm.triangles.clone()),
                    |lods| lods.meshes[index].clone(),
                );
                assert_eq!(a.triangles.len(), b.triangles.len());
                for (a_triangles, b_triangles) in a.triangles.iter().zip(&b.triangles) {
                    assert_eq!(a_triangles.len(), b_triangles.len());
                    for (a_triangle, b_triangle) in a_triangles.iter().zip(b_triangles) {
                        for (&ai, &bi) in a_triangle.iter().zip(b_triangle) {
                            let av = &a.vertices[ai as usize];
                            let bv = &b.vertices[bi as usize];
                            for (a, b) in av
                                .pos
                                .into_iter()
                                .chain(av.normal)
                                .chain(av.uv)
                                .zip(bv.pos.into_iter().chain(bv.normal).chain(bv.uv))
                            {
                                assert!((a - b).abs() < 0.0001, "{a} != {b}");
                            }
                            assert_eq!(av.bones, bv.bones);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn plain_static_save_preserves_oriented_triangle_attributes() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("cube.smd");
    std::fs::write(
        &source,
        include_str!("../../../vendor/mdlc/tests/fixtures/cube.smd"),
    )
    .unwrap();
    let mut request = ModelCompileRequest::new(source, root.path().join("original"));
    request.static_prop = true;
    request.export_materials = false;
    request.scale = 1.7;
    request.orientation = [17.0, -31.0, 22.0];
    let original = compiled(&request);
    let qc = root.path().join("saved.qc");
    save_model_qc(&request, &qc).unwrap();
    assert_geometry(
        &original,
        &compiled(&ModelCompileRequest::new(qc, root.path().join("restored"))),
    );
}

#[test]
fn external_lod_geometry_survives_scale_rotation_and_portable_qc_roundtrip() {
    for static_prop in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let source = root.path().join("reference.smd");
        let text = include_str!("../../../vendor/mdlc/tests/fixtures/rig.smd");
        std::fs::write(&source, text).unwrap();
        let (header, triangles) = text.split_once("triangles\n").unwrap();
        let low = external.path().join("reference.smd");
        let reduced = triangles.lines().take(16).collect::<Vec<_>>().join("\n");
        std::fs::write(&low, format!("{header}triangles\n{reduced}\nend\n")).unwrap();
        let mut request = ModelCompileRequest::new(source.clone(), root.path().join("original"));
        request.export_materials = false;
        request.static_prop = static_prop;
        request.scale = 1.7;
        if static_prop {
            request.orientation = [17.0, -31.0, 22.0];
        }
        request.lods = vec![ModelLod {
            source: low.clone(),
            switch_point: 25.0,
        }];
        let original = compiled(&request);
        assert_eq!(
            original.bodyparts[0].models[0]
                .lods
                .as_ref()
                .unwrap()
                .switch_points,
            [0.0, 25.0]
        );
        let output = compile_model(&request).unwrap();
        let vtx = std::fs::read(
            &output
                .files
                .iter()
                .find(|file| file.kind == ModelOutputKind::Vtx)
                .unwrap()
                .path,
        )
        .unwrap();
        let topology = vformats::mdl::parse_vtx(&vtx, &vformats::Limits::default()).unwrap();
        assert_eq!(topology.body_parts[0].models[0].lods.len(), 2);
        let offset = |at| u32::from_le_bytes(vtx[at..at + 4].try_into().unwrap()) as usize;
        let body = offset(32);
        let model = body + offset(body + 4);
        let lod = model + offset(model + 4);
        assert_eq!(
            f32::from_le_bytes(vtx[lod + 20..lod + 24].try_into().unwrap()),
            25.0
        );
        let qc = root.path().join("saved.qc");
        save_model_qc(&request, &qc).unwrap();
        let saved = std::fs::read_to_string(&qc).unwrap();
        assert!(saved.contains("$lod 25") && saved.contains("replacemodel"));
        assert!(saved.contains("nofacial"));
        assert!(mdlc::qc::parse_qc_file(&qc).unwrap().bodyparts[0].models[0].lods[0].no_facial);
        std::fs::remove_file(source).unwrap();
        std::fs::remove_file(low).unwrap();
        let mut restored = ModelCompileRequest::new(qc, root.path().join("restored"));
        restored.export_materials = false;
        let restored_compiled = compiled(&restored);
        assert_geometry(&original, &restored_compiled);
        assert_eq!(
            original
                .desc
                .bones
                .iter()
                .map(|bone| &bone.name)
                .collect::<Vec<_>>(),
            restored_compiled
                .desc
                .bones
                .iter()
                .map(|bone| &bone.name)
                .collect::<Vec<_>>()
        );
        compile_model(&restored).unwrap();
    }
}

#[test]
fn authored_external_lods_keep_base_flexes_and_disable_lower_facial_deformation() {
    let root = tempfile::tempdir().unwrap();
    let mut data = Vec::new();
    for value in [
        0.0_f32, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0,
        1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ] {
        data.extend(value.to_le_bytes());
    }
    let source = root.path().join("face.gltf");
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "buffers": [{
            "uri": format!("data:application/octet-stream;base64,{}", base64::encode(&data)),
            "byteLength": data.len()
        }],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 36},
            {"buffer": 0, "byteOffset": 72, "byteLength": 36}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [2,3,0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC3"}
        ],
        "materials": [{"name": "material"}],
        "meshes": [{
            "extras": {"targetNames": ["smile"]},
            "primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1}, "material": 0, "targets": [{"POSITION": 2}]}]
        }],
        "nodes": [{"name": "root", "mesh": 0}],
        "scenes": [{"nodes": [0]}], "scene": 0
    });
    std::fs::write(&source, serde_json::to_vec(&document).unwrap()).unwrap();
    let mut request = ModelCompileRequest::new(source.clone(), root.path().join("output"));
    request.export_materials = false;
    request.lods.push(ModelLod {
        source,
        switch_point: 20.0,
    });
    let model = compiled(&request);
    let model = &model.bodyparts[0].models[0];
    assert!(model.mesh_flexes.iter().any(|flexes| !flexes.is_empty()));
    assert_eq!(model.lods.as_ref().unwrap().no_facial, [false, true]);
    let qc = root.path().join("saved.qc");
    save_model_qc(&request, &qc).unwrap();
    let saved = mdlc::qc::parse_qc_file(&qc).unwrap();
    assert!(saved.bodyparts[0].models[0].lods[0].no_facial);
    let saved_model = compiled(&ModelCompileRequest::new(qc, root.path().join("saved")));
    assert_eq!(
        saved_model.bodyparts[0].models[0].mesh_flexes,
        model.mesh_flexes
    );
    assert_eq!(
        saved_model.bodyparts[0].models[0]
            .lods
            .as_ref()
            .unwrap()
            .no_facial,
        [false, true]
    );
}

#[test]
fn lod_validation_rejects_conflicts_invalid_distances_and_changed_skeletons() {
    let mut lods = vec![ModelLod {
        source: "low.smd".into(),
        switch_point: 20.0,
    }];
    assert!(validate_lods(&lods, true).is_err());
    for distance in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        lods[0].switch_point = distance;
        assert!(validate_lods(&lods, false).is_err());
    }
    lods[0].switch_point = 20.0;
    lods.push(lods[0].clone());
    assert!(validate_lods(&lods, false).is_err());
    let reference = include_str!("../../../vendor/mdlc/tests/fixtures/rig.smd");
    let base = mdlc::smd::parse_smd(reference).unwrap();
    compatible_skeleton(&base, &base).unwrap();
    let renamed = reference.replace("\"root\"", "\"foreign\"");
    assert!(compatible_skeleton(&base, &mdlc::smd::parse_smd(&renamed).unwrap()).is_err());
    let changed = reference.replace("0 0 0 0 0 0 0", "0 1 0 0 0 0 0");
    assert!(compatible_skeleton(&base, &mdlc::smd::parse_smd(&changed).unwrap()).is_err());
    for weight in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
        let mut invalid = base.clone();
        invalid.triangles[0].vertices[0].links[0].weight = weight;
        assert!(compatible_skeleton(&base, &invalid).is_err());
    }
    let mut invalid = base.clone();
    invalid.triangles[0].vertices[0].links.clear();
    assert!(compatible_skeleton(&base, &invalid).is_err());
    invalid = base.clone();
    invalid.triangles[0].vertices[0].links[0].bone = 999;
    assert!(compatible_skeleton(&base, &invalid).is_err());
}

#[test]
fn mixed_gltf_and_smd_lods_share_automatic_axes_and_portable_transform() {
    let root = tempfile::tempdir().unwrap();
    let smd = root.path().join("triangle.smd");
    std::fs::write(&smd,"version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\nmaterial\n0 0 0 0 0 -1 0 0 0\n0 2 0 0 0 -1 0 0 0\n0 0 0 3 0 -1 0 0 0\nend\n").unwrap();
    let mut data = Vec::new();
    for value in [
        0.0_f32, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0,
        1.0,
    ] {
        data.extend(value.to_le_bytes());
    }
    let gltf = root.path().join("triangle.gltf");
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "buffers": [{
            "uri": format!("data:application/octet-stream;base64,{}", base64::encode(&data)),
            "byteLength": data.len()
        }],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 36}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [2,3,0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"}
        ],
        "materials": [{"name": "material"}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1}, "material": 0}]}],
        "nodes": [{"name": "root", "mesh": 0}],
        "scenes": [{"nodes": [0]}], "scene": 0
    });
    std::fs::write(&gltf, serde_json::to_vec(&document).unwrap()).unwrap();
    for (index, (source, low)) in [(&smd, &gltf), (&gltf, &smd)].into_iter().enumerate() {
        let mut request =
            ModelCompileRequest::new(source.clone(), root.path().join(format!("output{index}")));
        request.static_prop = true;
        request.export_materials = false;
        request.scale = 2.0;
        request.lods.push(ModelLod {
            source: low.clone(),
            switch_point: 40.0,
        });
        let original = compiled(&request);
        let lods = original.bodyparts[0].models[0].lods.as_ref().unwrap();
        for (level, triangles) in lods.meshes[0].triangles.iter().enumerate() {
            assert_eq!(triangles.len(), 1, "LOD {level}");
            let mut points = triangles[0]
                .iter()
                .map(|&index| {
                    let vertex = &lods.meshes[0].vertices[index as usize];
                    assert!(
                        (vertex.normal[0] - 1.0).abs() < 0.0001
                            && vertex.normal[1].abs() < 0.0001
                            && vertex.normal[2].abs() < 0.0001,
                        "{:?}",
                        vertex.normal
                    );
                    vertex.pos
                })
                .collect::<Vec<_>>();
            points.sort_by(|a, b| a[1].total_cmp(&b[1]).then(a[2].total_cmp(&b[2])));
            for (actual, expected) in points.iter().flatten().zip(
                [[0.0, 0.0, 0.0], [0.0, 0.0, 6.0], [0.0, 4.0, 0.0]]
                    .iter()
                    .flatten(),
            ) {
                assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
            }
        }
        let qc = root.path().join(format!("mixed{index}.qc"));
        save_model_qc(&request, &qc).unwrap();
        assert_geometry(
            &original,
            &compiled(&ModelCompileRequest::new(qc, root.path().join("restored"))),
        );
    }
}
