use super::*;
use crate::model_compile::{ModelCompileOutput, compile_model};

fn fixture() -> (tempfile::TempDir, ModelCompileRequest) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("cube.smd");
    fs::write(
        &source,
        include_str!("../../../vendor/mdlc/tests/fixtures/cube.smd")
            .replace("0 0 1 0 1\n", "0.2 0.7 1 0 1\n"),
    )
    .unwrap();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([200, 100, 50, 255]))
        .save(root.path().join("material.png"))
        .unwrap();
    let mut request = ModelCompileRequest::new(source, root.path().join("original"));
    request.static_prop = true;
    request.model_name = Some("props/qc_saved.mdl".into());
    request.material_directory = Some("models/qc_saved".into());
    (root, request)
}

fn bytes(output: &ModelCompileOutput, kind: ModelOutputKind) -> Vec<u8> {
    fs::read(
        &output
            .files
            .iter()
            .find(|file| file.kind == kind)
            .unwrap()
            .path,
    )
    .unwrap()
}

fn vertices(output: &ModelCompileOutput) -> Vec<[f32; 8]> {
    let vvd = vformats::mdl::parse_vvd(
        &bytes(output, ModelOutputKind::Vvd),
        &vformats::Limits::default(),
    )
    .unwrap();
    let mut vertices = vvd
        .vertices
        .iter()
        .map(|vertex| {
            [
                vertex.position[0],
                vertex.position[1],
                vertex.position[2],
                vertex.normal[0],
                vertex.normal[1],
                vertex.normal[2],
                vertex.uv[0],
                vertex.uv[1],
            ]
        })
        .collect::<Vec<_>>();
    vertices.sort_by(|a, b| {
        a.iter()
            .zip(b)
            .find_map(|(a, b)| (a != b).then(|| a.total_cmp(b)))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    vertices
}

fn collision_bounds(output: &ModelCompileOutput) -> ([f32; 3], [f32; 3]) {
    let bytes = bytes(output, ModelOutputKind::Phy);
    let phy = vformats::phy::parse(&bytes, &vformats::Limits::default()).unwrap();
    let mut minimum = [f32::INFINITY; 3];
    let mut maximum = [f32::NEG_INFINITY; 3];
    for point in phy
        .solids
        .iter()
        .flat_map(|solid| &solid.ledges)
        .flat_map(|ledge| &ledge.vertices)
    {
        for axis in 0..3 {
            minimum[axis] = minimum[axis].min(point[axis]);
            maximum[axis] = maximum[axis].max(point[axis]);
        }
    }
    (minimum, maximum)
}

#[test]
fn custom_collision_survives_portable_qc_with_scale_rotation_and_multiple_pieces() {
    let (root, mut request) = fixture();
    let source = fs::read_to_string(&request.source).unwrap();
    let (header, geometry) = source.split_once("triangles\n").unwrap();
    let geometry = geometry.trim_end().strip_suffix("end").unwrap();
    let shifted = geometry
        .lines()
        .map(|line| {
            let mut fields = line
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if fields.len() >= 9 {
                fields[1] = (fields[1].parse::<f32>().unwrap() + 8.0).to_string();
                fields.join(" ")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let custom = root.path().join("authored.smd");
    fs::write(
        &custom,
        format!("{header}triangles\n{geometry}{shifted}\nend\n"),
    )
    .unwrap();
    request.collision = ModelCollision::Custom {
        source: custom.clone(),
        parts: Vec::new(),
        repair: None,
    };
    request.scale = 1.75;
    request.orientation = [17.0, 31.0, -43.0];
    request.export_materials = false;
    let original = compile_model(&request).unwrap();
    let saved = root.path().join("authored.qc");
    save_model_qc(&request, &saved).unwrap();
    fs::remove_file(custom).unwrap();
    let rebuilt = compile_model(&ModelCompileRequest::new(
        saved,
        root.path().join("rebuilt"),
    ))
    .unwrap();
    for output in [&original, &rebuilt] {
        let raw = bytes(output, ModelOutputKind::Phy);
        let physics = vformats::phy::parse(&raw, &vformats::Limits::default()).unwrap();
        assert_eq!(physics.solids.len(), 1);
        assert_eq!(physics.solids[0].ledges.len(), 2);
    }
    let original_bounds = collision_bounds(&original);
    let rebuilt_bounds = collision_bounds(&rebuilt);
    for (a, b) in original_bounds
        .0
        .into_iter()
        .chain(original_bounds.1)
        .zip(rebuilt_bounds.0.into_iter().chain(rebuilt_bounds.1))
    {
        assert!(
            (a - b).abs() < 0.001,
            "custom collision moved during QC relocation: {a} != {b}"
        );
    }
}

#[test]
fn disabling_authored_collision_survives_saving_and_reimporting_qc() {
    let (root, mut request) = fixture();
    request.collision = ModelCollision::Automatic(model_compile::CollisionDetail::Simple);
    request.export_materials = false;
    let authored = root.path().join("authored.qc");
    save_model_qc(&request, &authored).unwrap();
    let mut request = ModelCompileRequest::new(authored, root.path().join("disabled"));
    request.collision = ModelCollision::None;
    request.export_materials = false;
    let direct = compile_model(&request).unwrap();
    assert!(
        !direct
            .files
            .iter()
            .any(|file| file.kind == ModelOutputKind::Phy)
    );
    let saved = root.path().join("disabled.qc");
    save_model_qc(&request, &saved).unwrap();
    let reimported = compile_model(&ModelCompileRequest::new(
        saved,
        root.path().join("reimported"),
    ))
    .unwrap();
    assert!(
        !reimported
            .files
            .iter()
            .any(|file| file.kind == ModelOutputKind::Phy)
    );
    assert_eq!(vertices(&direct), vertices(&reimported));
}

#[test]
fn saved_qc_recompiles_skins_rotation_scale_and_collision() {
    let (root, mut request) = fixture();
    request.scale = 1.75;
    request.orientation = [17.0, 31.0, -43.0];
    request.collision =
        model_compile::ModelCollision::Automatic(model_compile::CollisionDetail::Simple);
    request.collision_mass = 1250.5;
    request.surface_property = Some("metal".into());
    request.skins.push(ModelSkin {
        name: "Blue".into(),
        replacements: BTreeMap::from([(
            "material".into(),
            SkinMaterial::Tint {
                color: [10, 30, 200],
            },
        )]),
    });
    let original = compile_model(&request).unwrap();
    let saved = root.path().join("cube.qc");
    save_model_qc(&request, &saved).unwrap();
    let text = fs::read_to_string(&saved).unwrap();
    assert!(text.starts_with("$modelname"));
    assert!(text.contains("$texturegroup"));
    assert!(text.contains("$collisionmodel"));
    let desc = mdlc::qc::parse_qc_file(&saved).unwrap();
    assert_eq!(desc.physics.mass, Some(1250.5));
    assert_eq!(desc.model.surface_prop.as_deref(), Some("metal"));
    assert!(
        root.path()
            .join("materials/models/qc_saved/__skins/skin_1/material.vmt")
            .exists()
    );
    let recompiled = compile_model(&ModelCompileRequest::new(
        saved,
        root.path().join("recompiled"),
    ))
    .unwrap();
    assert_eq!(original.vertices, recompiled.vertices);
    assert_eq!(original.triangles, recompiled.triangles);
    for (a, b) in vertices(&original)
        .iter()
        .flatten()
        .zip(vertices(&recompiled).iter().flatten())
    {
        assert!((a - b).abs() < 0.001, "vertex changed: {a} != {b}");
    }
    let a = collision_bounds(&original);
    let b = collision_bounds(&recompiled);
    for (a, b) in a.0.into_iter().chain(a.1).zip(b.0.into_iter().chain(b.1)) {
        assert!((a - b).abs() < 0.001, "collision changed: {a} != {b}");
    }
    let mdl = vformats::mdl::parse_mdl(
        &bytes(&recompiled, ModelOutputKind::Mdl),
        &vformats::Limits::default(),
    )
    .unwrap();
    assert_eq!(mdl.skin_table.len(), mdl.skin_reference_count * 2);
    let mdl = bytes(&recompiled, ModelOutputKind::Mdl);
    assert_eq!(
        f32::from_le_bytes(mdl[328..332].try_into().unwrap()),
        1250.5
    );
    let phy = bytes(&recompiled, ModelOutputKind::Phy);
    let phy = vformats::phy::parse(&phy, &vformats::Limits::default()).unwrap();
    let text = std::str::from_utf8(phy.text.unwrap().bytes).unwrap();
    assert!(text.contains("\"mass\" \"1250.500000\""));
    assert!(text.contains("\"surfaceprop\" \"metal\""));
}

#[test]
fn animated_smd_qc_preserves_source_frames_after_moving() {
    let (root, mut request) = fixture();
    request.static_prop = false;
    request.scale = 1.75;
    let text = fs::read_to_string(&request.source).unwrap();
    let text = text.replace(
        "end\ntriangles\n",
        "time 1\n0 1 2 3 0.1 0.2 0.3\nend\ntriangles\n",
    );
    fs::write(&request.source, &text).unwrap();
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    save_model_qc(&request, &saved.join("animated.qc")).unwrap();
    assert_eq!(
        fs::read_to_string(saved.join("animated_sources/mesh.smd")).unwrap(),
        text
    );
    fs::remove_file(&request.source).unwrap();
    fs::remove_file(root.path().join("material.png")).unwrap();
    let moved = root.path().join("moved");
    fs::rename(saved, &moved).unwrap();
    let desc = mdlc::qc::parse_qc_file(&moved.join("animated.qc")).unwrap();
    let compiled = mdlc::compile::compile(&desc, &moved).unwrap();
    assert_eq!(compiled.sequences[0].frames.len(), 2);
    let pose = &compiled.sequences[0].frames[1][0];
    for (actual, expected) in pose.position.iter().zip([1.75, 3.5, 5.25]) {
        assert!((actual - expected).abs() < 0.0001);
    }
    compile_model(&ModelCompileRequest::new(
        moved.join("animated.qc"),
        root.path().join("recompiled"),
    ))
    .unwrap();
}

#[test]
fn rigid_gltf_qc_is_portable_with_axes_uvs_skins_and_collision() {
    for static_prop in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input");
        let saved = root.path().join("saved");
        fs::create_dir(&input).unwrap();
        fs::create_dir(&saved).unwrap();
        let mut geometry = Vec::new();
        for value in [
            0.0_f32, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 7.0, 0.0, 0.0, 0.0, 3.0, 0.2, 0.7, 0.4, 0.6,
            0.3, 0.8, 0.5, 0.9,
        ] {
            geometry.extend_from_slice(&value.to_le_bytes());
        }
        for index in [0_u16, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3] {
            geometry.extend_from_slice(&index.to_le_bytes());
        }
        fs::write(input.join("geometry.bin"), &geometry).unwrap();
        let document = serde_json::json!({
            "asset": {"version": "2.0"},
            "buffers": [{"uri": "geometry.bin", "byteLength": geometry.len()}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 48},
                {"buffer": 0, "byteOffset": 48, "byteLength": 32},
                {"buffer": 0, "byteOffset": 80, "byteLength": 24}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3", "min": [0,0,0], "max": [2,7,3]},
                {"bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC2"},
                {"bufferView": 2, "componentType": 5123, "count": 12, "type": "SCALAR"}
            ],
            "materials": [{"name": "material", "pbrMetallicRoughness": {"baseColorFactor": [0.8,0.4,0.2,1.0]}}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "TEXCOORD_0": 1}, "indices": 2, "material": 0}]}],
            "nodes": [{"mesh": 0}], "scenes": [{"nodes": [0]}], "scene": 0
        });
        let source = input.join("model.gltf");
        fs::write(&source, serde_json::to_vec(&document).unwrap()).unwrap();
        let mut request = ModelCompileRequest::new(source, root.path().join("original"));
        request.static_prop = static_prop;
        request.scale = 1.75;
        request.model_name = Some("props/portable.mdl".into());
        request.material_directory = Some("models/portable".into());
        request.collision =
            model_compile::ModelCollision::Automatic(model_compile::CollisionDetail::Simple);
        request.surface_property = Some("metal".into());
        request.collision_mass = 125.0;
        request.skins.push(ModelSkin {
            name: "Blue".into(),
            replacements: BTreeMap::from([(
                "material".into(),
                SkinMaterial::Tint {
                    color: [10, 30, 200],
                },
            )]),
        });
        let original = compile_model(&request).unwrap();
        let qc = saved.join("model.qc");
        save_model_qc(&request, &qc).unwrap();
        let text = fs::read_to_string(&qc).unwrap();
        assert!(!text.contains("srcaxis"));
        assert!(!text.contains(".gltf"));
        assert!(!text.contains(&input.to_string_lossy().to_string()));
        assert!(text.contains("$surfaceprop \"metal\""));
        assert!(text.contains("$mass 125"));
        fs::remove_dir_all(input).unwrap();
        let moved = root.path().join("moved");
        fs::rename(saved, &moved).unwrap();
        let recompiled = compile_model(&ModelCompileRequest::new(
            moved.join("model.qc"),
            root.path().join("recompiled"),
        ))
        .unwrap();
        assert_eq!(original.vertices, recompiled.vertices);
        assert_eq!(original.triangles, recompiled.triangles);
        for (a, b) in vertices(&original)
            .iter()
            .flatten()
            .zip(vertices(&recompiled).iter().flatten())
        {
            assert!(
                (a - b).abs() < 0.001,
                "vertex changed: {a} != {b}; static={static_prop}"
            );
        }
        let a = collision_bounds(&original);
        let b = collision_bounds(&recompiled);
        for (a, b) in a.0.into_iter().chain(a.1).zip(b.0.into_iter().chain(b.1)) {
            assert!(
                (a - b).abs() < 0.001,
                "collision changed: {a} != {b}; static={static_prop}"
            );
        }
        let mdl = vformats::mdl::parse_mdl(
            &bytes(&recompiled, ModelOutputKind::Mdl),
            &vformats::Limits::default(),
        )
        .unwrap();
        assert_eq!(mdl.skin_table.len(), mdl.skin_reference_count * 2);
    }
}

#[test]
fn authored_qc_preserves_comments_includes_sequences_and_custom_commands() {
    let (root, mut request) = fixture();
    fs::create_dir(root.path().join("includes")).unwrap();
    fs::write(
        root.path().join("includes/nested.qci"),
        "$sequence second cube.smd fps 12\n",
    )
    .unwrap();
    fs::write(root.path().join("includes/props.qci"), "// collision settings\n$surfaceprop metal\n$include \"includes/nested.qci\"\n$cdmaterials \"old/path\"\n").unwrap();
    let original = "// hand authored QC\n$cd \".\"\n$modelname props/original.mdl\n$staticprop\n$body body cube.smd\n$sequence idle cube.smd fps 24\n$include \"includes/props.qci\"\n$texturegroup skins { { material } { material } }\n$keyvalues { custom { answer 42 \"$texturegroup\" \"$cdmaterials\" } }\n";
    let source = root.path().join("original.qc");
    fs::write(&source, original).unwrap();
    request.source = source.clone();
    request.skins.push(ModelSkin {
        name: "Tint".into(),
        replacements: BTreeMap::from([(
            "material".into(),
            SkinMaterial::Tint {
                color: [20, 20, 20],
            },
        )]),
    });
    let saved_dir = root.path().join("saved");
    fs::create_dir(&saved_dir).unwrap();
    let saved = saved_dir.join("car.qc");
    save_model_qc(&request, &saved).unwrap();
    assert_eq!(fs::read_to_string(source).unwrap(), original);
    let entry = fs::read_to_string(&saved).unwrap();
    assert!(entry.contains("$include \"car_sources/original.qc\""));
    let text = fs::read_to_string(saved_dir.join("car_sources/original.qc")).unwrap();
    assert!(text.contains("// hand authored QC"));
    assert!(
        text.contains("$keyvalues { custom { answer 42 \"$texturegroup\" \"$cdmaterials\" } }")
    );
    assert!(text.contains("$sequence idle cube.smd fps 24"));
    assert!(text.contains("$include \"car_sources/includes/props.qci\""));
    assert!(
        fs::read_to_string(saved_dir.join("car_sources/includes/props.qci"))
            .unwrap()
            .contains("// collision settings")
    );
    let desc = mdlc::qc::parse_qc_file(&saved).unwrap();
    assert_eq!(desc.model.name, "models/props/qc_saved.mdl");
    assert_eq!(desc.materials.search_paths, ["models/qc_saved/"]);
    assert_eq!(desc.sequences.len(), 2);
    assert_eq!(desc.materials.skin_families.len(), 3);
    assert_eq!(desc.model.surface_prop.as_deref(), Some("metal"));
    compile_model(&ModelCompileRequest::new(
        saved,
        root.path().join("recompiled"),
    ))
    .unwrap();
}

#[test]
fn failed_save_keeps_existing_qc_and_materials_intact() {
    let (root, mut request) = fixture();
    let saved = root.path().join("cube.qc");
    save_model_qc(&request, &saved).unwrap();
    let original = fs::read(&saved).unwrap();
    let vmt = root.path().join("materials/models/qc_saved/material.vmt");
    let material = fs::read(&vmt).unwrap();
    request.skins.push(ModelSkin {
        name: "Broken".into(),
        replacements: BTreeMap::from([(
            "material".into(),
            SkinMaterial::Texture {
                path: root.path().join("missing.png"),
            },
        )]),
    });
    assert!(save_model_qc(&request, &saved).is_err());
    assert_eq!(fs::read(&saved).unwrap(), original);
    assert_eq!(fs::read(vmt).unwrap(), material);
    request.scale = f32::NAN;
    assert!(save_model_qc(&request, &saved).is_err());
    assert_eq!(fs::read(saved).unwrap(), original);
}

#[test]
fn saving_toml_as_qc_is_rejected_without_overwriting() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.toml");
    fs::write(&source, "[model]\nname = 'model.mdl'").unwrap();
    let saved = root.path().join("saved.qc");
    fs::write(&saved, "original").unwrap();
    assert!(
        save_model_qc(
            &ModelCompileRequest::new(source, root.path().join("out")),
            &saved
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(saved).unwrap(), "original");
}

#[test]
fn generated_collision_keeps_alignment_with_a_rotated_source_root() {
    let (root, mut request) = fixture();
    let text = fs::read_to_string(&request.source).unwrap();
    let original_pose = text
        .lines()
        .find(|line| line.trim().starts_with("0 0 0 0"))
        .unwrap();
    let modified = text.replacen(original_pose, "0 0 0 0 1.5707964 0 0", 1);
    fs::write(&request.source, modified).unwrap();
    request.collision =
        model_compile::ModelCollision::Automatic(model_compile::CollisionDetail::Simple);
    let original = compile_model(&request).unwrap();
    let saved = root.path().join("rotated.qc");
    save_model_qc(&request, &saved).unwrap();
    let recompiled = compile_model(&ModelCompileRequest::new(
        saved,
        root.path().join("recompiled"),
    ))
    .unwrap();
    let a = collision_bounds(&original);
    let b = collision_bounds(&recompiled);
    for (a, b) in a.0.into_iter().chain(a.1).zip(b.0.into_iter().chain(b.1)) {
        assert!(
            (a - b).abs() < 0.001,
            "rotated root shifted collision: {a} != {b}"
        );
    }
}

fn animated_gltf_project(root: &Path) -> ModelCompileRequest {
    let input = root.join("input");
    fs::create_dir_all(input.join("buffers")).unwrap();
    fs::create_dir_all(input.join("Textures")).unwrap();
    let mut bytes = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut add = |data: Vec<u8>, component: u32, count: usize, kind: &str| {
        while !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
        let offset = bytes.len();
        bytes.extend_from_slice(&data);
        views.push(serde_json::json!({"buffer":0,"byteOffset":offset,"byteLength":data.len()}));
        let accessor = accessors.len();
        accessors.push(serde_json::json!({"bufferView":views.len()-1,"componentType":component,"count":count,"type":kind}));
        accessor
    };
    let floats = |values: &[f32]| {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>()
    };
    let position = add(
        floats(&[0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 2.0, 0.0]),
        5126,
        3,
        "VEC3",
    );
    let normal = add(
        floats(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]),
        5126,
        3,
        "VEC3",
    );
    let uv = add(floats(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0]), 5126, 3, "VEC2");
    let joints = add(vec![0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0], 5121, 3, "VEC4");
    let weights = add(
        floats(&[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
        5126,
        3,
        "VEC4",
    );
    let morph = add(
        floats(&[0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        5126,
        3,
        "VEC3",
    );
    let time = add(floats(&[0.0, 1.0]), 5126, 2, "SCALAR");
    let walking = add(floats(&[0.0, 0.0, 0.0, 2.0, 0.0, 0.0]), 5126, 2, "VEC3");
    let running = add(floats(&[0.0, 0.0, 0.0, 4.0, 0.0, 0.0]), 5126, 2, "VEC3");
    let indices = add(vec![0, 0, 1, 0, 2, 0], 5123, 3, "SCALAR");
    fs::write(input.join("buffers/geometry.bin"), &bytes).unwrap();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([100, 80, 60, 255]))
        .save(input.join("Textures/Face Color.PNG"))
        .unwrap();
    let animation = |name, output| serde_json::json!({"name":name,"samplers":[{"input":time,"output":output}],"channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]});
    let json = serde_json::json!({
        "asset":{"version":"2.0"},
        "buffers":[{"uri":"buffers/geometry.bin","byteLength":bytes.len()}],
        "bufferViews":views,"accessors":accessors,
        "images":[{"uri":"textures/face%20color.png"}],"textures":[{"source":0}],
        "materials":[{"name":"face","pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}],
        "meshes":[{"extras":{"targetNames":["smile"]},"primitives":[{"attributes":{"POSITION":position,"NORMAL":normal,"TEXCOORD_0":uv,"JOINTS_0":joints,"WEIGHTS_0":weights},"indices":indices,"material":0,"targets":[{"POSITION":morph}]}]}],
        "skins":[{"joints":[0,1]}],
        "nodes":[{"name":"root","children":[1]},{"name":"tip","translation":[0,0,1]},{"name":"mesh","mesh":0,"skin":0}],
        "scenes":[{"nodes":[0,2]}],"scene":0,
        "animations":[animation("walking",walking),animation("running",running)]
    });
    let source = input.join("character.gltf");
    fs::write(&source, serde_json::to_vec(&json).unwrap()).unwrap();
    ModelCompileRequest::new(source, root.join("compiled"))
}

#[test]
fn animated_facial_gltf_qc_remains_portable_with_external_buffers_and_images() {
    for variant in ["gltf", "glb", "one_frame"] {
        let root = tempfile::tempdir().unwrap();
        let mut request = animated_gltf_project(root.path());
        if variant == "glb" {
            let mut json = fs::read(&request.source).unwrap();
            while !json.len().is_multiple_of(4) {
                json.push(b' ');
            }
            let mut glb = b"glTF".to_vec();
            glb.extend_from_slice(&2_u32.to_le_bytes());
            glb.extend_from_slice(&(20_u32 + json.len() as u32).to_le_bytes());
            glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
            glb.extend_from_slice(b"JSON");
            glb.extend_from_slice(&json);
            request.source.set_extension("glb");
            fs::write(&request.source, glb).unwrap();
        } else if variant == "one_frame" {
            let mut document: serde_json::Value =
                serde_json::from_slice(&fs::read(&request.source).unwrap()).unwrap();
            document.as_object_mut().unwrap().remove("skins");
            let mesh = &mut document["meshes"][0];
            mesh.as_object_mut().unwrap().remove("extras");
            mesh["primitives"][0]
                .as_object_mut()
                .unwrap()
                .remove("targets");
            mesh["primitives"][0]["attributes"]
                .as_object_mut()
                .unwrap()
                .remove("JOINTS_0");
            mesh["primitives"][0]["attributes"]
                .as_object_mut()
                .unwrap()
                .remove("WEIGHTS_0");
            document["nodes"] = serde_json::json!([{"name":"mesh","mesh":0}]);
            document["scenes"][0]["nodes"] = serde_json::json!([0]);
            for accessor in [6, 7, 8] {
                document["accessors"][accessor]["count"] = serde_json::json!(1);
            }
            fs::write(&request.source, serde_json::to_vec(&document).unwrap()).unwrap();
        }
        let source_bytes = fs::read(&request.source).unwrap();
        let source_desc = model_compile::load_description(
            &request,
            &request.source,
            request.source.parent().unwrap(),
        )
        .unwrap();
        let original =
            mdlc::compile::compile(&source_desc, request.source.parent().unwrap()).unwrap();
        let saved = root.path().join("saved");
        fs::create_dir(&saved).unwrap();
        save_model_qc(&request, &saved.join("character.qc")).unwrap();
        assert_eq!(
            fs::read(
                saved
                    .join("character_sources")
                    .join(request.source.file_name().unwrap())
            )
            .unwrap(),
            source_bytes
        );
        assert!(
            saved
                .join("character_sources/buffers/geometry.bin")
                .is_file()
        );
        assert!(
            saved
                .join("character_sources/textures/face color.png")
                .is_file()
        );
        let qc = fs::read_to_string(saved.join("character.qc")).unwrap();
        assert!(!qc.contains(request.source.to_str().unwrap()));
        fs::remove_dir_all(root.path().join("input")).unwrap();
        let moved = root.path().join("moved");
        fs::rename(saved, &moved).unwrap();
        let desc = mdlc::qc::parse_qc_file(&moved.join("character.qc")).unwrap();
        let rebuilt = mdlc::compile::compile(&desc, &moved).unwrap();
        assert_eq!(original.sequences.len(), 2);
        assert!(original.desc.bones.len() >= if variant == "one_frame" { 1 } else { 2 });
        assert_eq!(original.desc.bones.len(), rebuilt.desc.bones.len());
        assert_eq!(original.sequences.len(), rebuilt.sequences.len());
        for (old, new) in original.sequences.iter().zip(&rebuilt.sequences) {
            assert_eq!(old.name, new.name);
            assert_eq!(old.frames, new.frames);
            if variant == "one_frame" {
                assert_eq!(old.frames.len(), 1);
            } else {
                assert!(old.frames.len() > 1);
            }
        }
        let old_flexes = &original.bodyparts[0].models[0].mesh_flexes;
        let new_flexes = &rebuilt.bodyparts[0].models[0].mesh_flexes;
        if variant != "one_frame" {
            assert!(old_flexes.iter().any(|mesh| !mesh.is_empty()));
        }
        assert_eq!(format!("{old_flexes:?}"), format!("{new_flexes:?}"));
        compile_model(&ModelCompileRequest::new(
            moved.join("character.qc"),
            root.path().join("recompiled"),
        ))
        .unwrap();
    }
}

#[test]
fn portable_source_copy_rejects_escaped_dependencies_without_publishing() {
    let root = tempfile::tempdir().unwrap();
    let request = animated_gltf_project(root.path());
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&request.source).unwrap()).unwrap();
    document["images"] = serde_json::json!([{"uri":"../outside.png"}]);
    let stage = root.path().join("stage");
    fs::create_dir(&stage).unwrap();
    fs::write(&request.source, serde_json::to_vec(&document).unwrap()).unwrap();
    let mut writes = Vec::new();
    let mut copier = SourceCopier::new(
        request.source.parent().unwrap(),
        &stage,
        "sources",
        &mut writes,
    );
    assert!(matches!(
        copier.copy(&request.source),
        Err(ModelCompileError::UnsafePath(_))
    ));
    for uri in [
        "%2e%2e/outside.png",
        "file:///etc/passwd",
        "https://example.com/a.png",
        "%xx",
        "a%00b",
    ] {
        let relative = decode_source_uri(uri);
        assert!(relative.is_err() || copier.find(&relative.unwrap()).is_err());
    }
}

#[cfg(unix)]
#[test]
fn portable_source_copy_cannot_follow_symlinks_outside_the_source_root() {
    let root = tempfile::tempdir().unwrap();
    let request = animated_gltf_project(root.path());
    let outside = root.path().join("outside.bin");
    fs::write(&outside, b"private").unwrap();
    std::os::unix::fs::symlink(outside, request.source.parent().unwrap().join("escape.bin"))
        .unwrap();
    let stage = root.path().join("stage");
    fs::create_dir(&stage).unwrap();
    let mut writes = Vec::new();
    let copier = SourceCopier::new(
        request.source.parent().unwrap(),
        &stage,
        "sources",
        &mut writes,
    );
    assert!(matches!(
        copier.find(Path::new("escape.bin")),
        Err(ModelCompileError::UnsafePath(_))
    ));
}

#[test]
fn fbx_source_copy_retains_bytes_and_resolved_texture_sidecars() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input");
    let stage = root.path().join("stage");
    fs::create_dir_all(input.join("Textures")).unwrap();
    fs::create_dir(&stage).unwrap();
    let source = input.join("character.fbx");
    let fbx = br#"; FBX 7.4.0 project file
FBXHeaderExtension: {
    FBXHeaderVersion: 1003
    FBXVersion: 7400
}
Objects: {
    Texture: 1, "Texture::face", "" {
        Type: "TextureVideoClip"
        Version: 202
        TextureName: "Texture::face"
        FileName: "C:/artist/model/texture.png"
        RelativeFilename: "model/texture.png"
    }
}
"#;
    fs::write(&source, fbx).unwrap();
    fs::write(input.join("Textures/Texture.PNG"), b"texture payload").unwrap();
    fs::write(input.join("unrelated.bin"), b"unrelated").unwrap();
    let scene = ufbx::load_memory(fbx, ufbx::LoadOpts::default()).unwrap();
    assert_eq!(scene.textures.len(), 1);
    let mut writes = Vec::new();
    let mut copier = SourceCopier::new(&input, &stage, "sources", &mut writes);
    assert_eq!(
        copier.copy(&source).unwrap(),
        Path::new("sources/character.fbx")
    );
    assert_eq!(fs::read(stage.join("sources/character.fbx")).unwrap(), fbx);
    assert_eq!(
        fs::read(stage.join("sources/textures/texture.png")).unwrap(),
        b"texture payload"
    );
    assert!(!stage.join("sources/unrelated.bin").exists());
}

#[test]
fn authored_animation_selection_playback_and_looping_survive_saved_qc() {
    let root = tempfile::tempdir().unwrap();
    let mut request = animated_gltf_project(root.path());
    let inspection = model_compile::inspect_model(&request.source).unwrap();
    assert_eq!(inspection.animations.len(), 2);
    request.animation_settings = inspection.animations;
    request.animation_settings[0].included = false;
    request.animation_settings[1].name = "run_fast".into();
    request.animation_settings[1].fps = 60.0;
    let base = request.source.parent().unwrap();
    let desc = model_compile::load_description(&request, &request.source, base).unwrap();
    let compiled = mdlc::compile::compile(&desc, base).unwrap();
    assert_eq!(compiled.sequences.len(), 1);
    assert_eq!(compiled.sequences[0].name, "run_fast");
    assert_eq!(compiled.sequences[0].fps, 60.0);
    assert_eq!(compiled.sequences[0].frames.len(), 31);
    assert_ne!(
        compiled.sequences[0].frames[0],
        compiled.sequences[0].frames[30]
    );
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    save_model_qc(&request, &saved.join("character.qc")).unwrap();
    let parsed = mdlc::qc::parse_qc_file(&saved.join("character.qc")).unwrap();
    assert_eq!(parsed.sequences.len(), 1);
    assert_eq!(parsed.sequences[0].src_stack.as_deref(), Some("running"));
    assert_eq!(parsed.sequences[0].fps, Some(60.0));
    assert!(!parsed.sequences[0].looping);
    let rebuilt = mdlc::compile::compile(&parsed, &saved).unwrap();
    assert_eq!(compiled.sequences[0].frames, rebuilt.sequences[0].frames);
    request.animation_settings[1].looping = true;
    let desc = model_compile::load_description(&request, &request.source, base).unwrap();
    let looped = mdlc::compile::compile(&desc, base).unwrap();
    assert_eq!(looped.sequences[0].frames.len(), 31);
    assert_eq!(
        looped.sequences[0].frames[0],
        looped.sequences[0].frames[30]
    );
    save_model_qc(&request, &saved.join("character.qc")).unwrap();
    let parsed = mdlc::qc::parse_qc_file(&saved.join("character.qc")).unwrap();
    assert!(parsed.sequences[0].looping);
    assert!(mdlc::compile::compile(&parsed, &saved).unwrap().sequences[0].looping);
}

#[test]
fn invalid_or_stale_animation_settings_cannot_publish_qc() {
    let root = tempfile::tempdir().unwrap();
    let mut request = animated_gltf_project(root.path());
    request.animation_settings = model_compile::inspect_model(&request.source)
        .unwrap()
        .animations;
    let original = request.animation_settings.clone();
    let saved = root.path().join("saved.qc");
    fs::write(&saved, b"existing QC").unwrap();
    for bad_fps in [0.0, -1.0, f32::NAN, f32::INFINITY, 1000.1] {
        request.animation_settings.clone_from(&original);
        request.animation_settings[0].fps = bad_fps;
        assert!(save_model_qc(&request, &saved).is_err());
        assert_eq!(fs::read(&saved).unwrap(), b"existing QC");
    }
    for name in ["", "bad name", "bad\"name", "☃", &"a".repeat(64)] {
        request.animation_settings.clone_from(&original);
        request.animation_settings[0].name = name.into();
        assert!(save_model_qc(&request, &saved).is_err());
    }
    request.animation_settings.clone_from(&original);
    request.animation_settings[0].name = request.animation_settings[1].name.to_uppercase();
    assert!(save_model_qc(&request, &saved).is_err());
    request.animation_settings.clone_from(&original);
    for setting in &mut request.animation_settings {
        setting.included = false;
    }
    assert!(save_model_qc(&request, &saved).is_err());
    request.animation_settings.clone_from(&original);
    request.animation_settings[0].source = Some("changed source action".into());
    assert!(save_model_qc(&request, &saved).is_err());
    request.animation_settings.clone_from(&original);
    request.animation_settings.pop();
    assert!(save_model_qc(&request, &saved).is_err());
    assert_eq!(fs::read(&saved).unwrap(), b"existing QC");
    request.animation_settings.clone_from(&original);
    request.animation_settings[0].name = "a".repeat(63);
    request.animation_settings[0].fps = 1000.0;
    assert!(
        model_compile::load_description(
            &request,
            &request.source,
            request.source.parent().unwrap()
        )
        .is_ok()
    );
}

#[test]
fn reserved_animation_source_and_saved_project_names_recompile() {
    let root = tempfile::tempdir().unwrap();
    let mut request = animated_gltf_project(root.path());
    let source = request.source.with_file_name("weights.gltf");
    fs::rename(&request.source, &source).unwrap();
    request.source = source;
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    let qc = saved.join("weights.qc");
    save_model_qc(&request, &qc).unwrap();
    let desc = mdlc::qc::parse_qc_file(&qc).unwrap();
    assert_eq!(desc.sequences.len(), 2);
    let compiled = mdlc::compile::compile(&desc, &saved).unwrap();
    assert_eq!(compiled.sequences.len(), 2);
    assert!(
        fs::read_to_string(&qc)
            .unwrap()
            .contains("\"./weights_sources/weights.gltf\"")
    );
}

#[test]
fn activity_and_numeric_named_events_survive_portable_qc_and_native_output() {
    use crate::model_compile::AnimationEvent;
    let root = tempfile::tempdir().unwrap();
    let mut request = animated_gltf_project(root.path());
    request.animation_settings = model_compile::inspect_model(&request.source)
        .unwrap()
        .animations;
    assert!(
        request
            .animation_settings
            .iter()
            .all(|animation| animation.frame_count == 31)
    );
    request.animation_settings[0].included = false;
    let action = &mut request.animation_settings[1];
    action.fps = 60.0;
    action.activity = Some("ACT_VM_RELOAD".into());
    action.activity_weight = 7;
    action.events = vec![
        AnimationEvent {
            frame: 15,
            event: "5004".into(),
            options: "Weapon.Reload".into(),
        },
        AnimationEvent {
            frame: 30,
            event: "AE_CL_PLAYSOUND".into(),
            options: "finish sound".into(),
        },
    ];
    let original = model_compile::load_description(
        &request,
        &request.source,
        request.source.parent().unwrap(),
    )
    .unwrap();
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    save_model_qc(&request, &saved.join("character.qc")).unwrap();
    fs::remove_dir_all(root.path().join("input")).unwrap();
    let parsed = mdlc::qc::parse_qc_file(&saved.join("character.qc")).unwrap();
    assert_eq!(parsed.sequences[0].events, original.sequences[0].events);
    assert_eq!(
        parsed.sequences[0].activity.as_deref(),
        Some("ACT_VM_RELOAD")
    );
    assert_eq!(parsed.sequences[0].activity_weight, 7);
    let output =
        mdlc::pipeline::build(&parsed, &saved, mdlc::pipeline::PipelineOptions::default()).unwrap();
    let raw = &output.mdl;
    let integer = |offset: usize| i32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap());
    let string = |offset: usize| {
        let bytes = &raw[offset..];
        std::str::from_utf8(&bytes[..bytes.iter().position(|&byte| byte == 0).unwrap()]).unwrap()
    };
    assert_eq!(integer(188), 1);
    let sequence = integer(192) as usize;
    assert_eq!(
        string(sequence + integer(sequence + 8) as usize),
        "ACT_VM_RELOAD"
    );
    assert_eq!(integer(sequence + 20), 7);
    assert_eq!(integer(sequence + 24), 2);
    let events = sequence + integer(sequence + 28) as usize;
    for (index, (cycle, code, kind, options, name)) in [
        (0.5, 5004, 0, "Weapon.Reload", ""),
        (1.0, 0, 1024, "finish sound", "AE_CL_PLAYSOUND"),
    ]
    .into_iter()
    .enumerate()
    {
        let record = events + index * 80;
        assert_eq!(
            f32::from_le_bytes(raw[record..record + 4].try_into().unwrap()),
            cycle
        );
        assert_eq!(integer(record + 4), code);
        assert_eq!(integer(record + 8), kind);
        assert_eq!(string(record + 12), options);
        let name_offset = integer(record + 76);
        if name.is_empty() {
            assert_eq!(name_offset, 0);
        } else {
            assert_eq!(string(record + name_offset as usize), name);
        }
    }
}

#[test]
fn activity_event_validation_rejects_unrepresentable_or_stale_source_values() {
    use crate::model_compile::AnimationEvent;
    let root = tempfile::tempdir().unwrap();
    let mut request = animated_gltf_project(root.path());
    request.animation_settings = model_compile::inspect_model(&request.source)
        .unwrap()
        .animations;
    let original = request.animation_settings[0].clone();
    let saved = root.path().join("invalid.qc");
    for activity in [
        "idle",
        "ACT_",
        "ACT_BAD\"",
        "ACT_é",
        &format!("ACT_{}", "x".repeat(60)),
    ] {
        request.animation_settings[0].activity = Some(activity.into());
        assert!(save_model_qc(&request, &saved).is_err());
        assert!(!saved.exists());
    }
    request.animation_settings[0] = original.clone();
    for (frame, event, options) in [
        (31, "5004", ""),
        (u32::MAX, "5004", ""),
        (0, "", ""),
        (0, "5004bad", ""),
        (0, "2147483648", ""),
        (0, "-1", ""),
        (0, "AE_BAD NAME", ""),
        (0, "5004", "quote\""),
        (0, "5004", "line\nbreak"),
        (0, "5004", "}end"),
        (0, "5004", "\0"),
        (0, "5004", "éééééééééééééééééééééééééééééééé"),
    ] {
        request.animation_settings[0].events = vec![AnimationEvent {
            frame,
            event: event.into(),
            options: options.into(),
        }];
        assert!(
            save_model_qc(&request, &saved).is_err(),
            "{frame} {event:?} {options:?}"
        );
        assert!(!saved.exists());
    }
    request.animation_settings[0] = original;
    request.animation_settings[0].frame_count = 100;
    request.animation_settings[0].events = vec![AnimationEvent {
        frame: 99,
        event: "5004".into(),
        options: String::new(),
    }];
    assert!(save_model_qc(&request, &saved).is_err());
    assert!(!saved.exists());
}

#[test]
fn event_frames_use_the_selected_source_stack_sampling_rate() {
    let root = tempfile::tempdir().unwrap();
    let request = animated_gltf_project(root.path());
    let source = quote_path(&request.source).unwrap();
    for source_option in [
        format!(
            "$sequence action {source} srcstack running srcfps 10 fps 60 {{\n event 5004 5 \"sound\"\n}}\n"
        ),
        format!(
            "$animation shared {source} srcstack running srcfps 10 fps 60\n$sequence action shared {{\n event 5004 5 \"sound\"\n}}\n"
        ),
    ] {
        let text = format!("$body body {source}\n{source_option}");
        let desc = mdlc::qc::parse_qc_str(&text, root.path()).unwrap();
        let compiled = mdlc::compile::compile(&desc, root.path()).unwrap();
        assert_eq!(compiled.sequences[0].frames.len(), 11);
        assert_eq!(compiled.sequences[0].events[0].cycle, 0.5);
    }
}

#[test]
fn edited_qc_with_spaced_filename_relocates_shared_macros_animation_and_procedural_bones() {
    let root = tempfile::tempdir().unwrap();
    let mut request = animated_gltf_project(root.path());
    let input = request.source.parent().unwrap().to_path_buf();
    fs::create_dir(input.join("qc")).unwrap();
    fs::create_dir(input.join("includes")).unwrap();
    fs::write(
        input.join("pose.vrd"),
        "bone tip root\nbasepos 0 0 1\ntrigger 45 0 0 0 0 0 20 0 0 1\n",
    )
    .unwrap();
    fs::write(input.join("includes/body.qci"), "// preserve this include\n$cd \".\"\n$definemacro bind source \\\\\n$body body $source$ srcaxis y\n$bind \"../character.gltf\"\n$sequence run \"../character.gltf\" srcstack running fps 47 noautoik { activity ACT_RUN 4 event 5004 15 \"step\" }\n$keyvalues { custom { $cd \"metadata-directory\" } }\n").unwrap();
    let source = input.join("qc/original.qc");
    let original = "$unsupported_original_command\n";
    fs::write(&source, original).unwrap();
    let edited = "\u{feff}// edited in the app\n$modelname props/edited.mdl\n$definevariable folder \"../includes\"\n$include $folder$/body.qci\n$proceduralbones \"../pose.vrd\"\n";
    request.source = source.clone();
    request.qc_text = Some(edited.into());
    let inspection = model_compile::inspect_qc_text(&source, edited).unwrap();
    assert_eq!(inspection.animations[0].name, "run");
    let desc =
        model_compile::load_description(&request, &source, source.parent().unwrap()).unwrap();
    assert_eq!(desc.quat_interp_bones.len(), 1);
    let expected = mdlc::compile::compile(&desc, source.parent().unwrap()).unwrap();
    let original_output = compile_model(&request).unwrap();
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    save_model_qc(&request, &saved.join("My prop.qc")).unwrap();
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    let retained = fs::read_to_string(saved.join("My~20prop_sources/qc/original.qc")).unwrap();
    assert!(retained.contains("$definevariable folder \"../includes\""));
    let retained = fs::read_to_string(saved.join("My~20prop_sources/includes/body.qci")).unwrap();
    assert!(retained.contains("$definemacro bind source"));
    assert!(retained.contains("$keyvalues { custom { $cd \"metadata-directory\" } }"));
    fs::remove_dir_all(&input).unwrap();
    let moved = root.path().join("moved");
    fs::rename(saved, &moved).unwrap();
    let parsed = mdlc::qc::parse_qc_file(&moved.join("My prop.qc")).unwrap();
    assert_eq!(parsed.quat_interp_bones, desc.quat_interp_bones);
    assert_eq!(parsed.model.source_dependencies.len(), 1);
    let actual = mdlc::compile::compile(&parsed, &moved).unwrap();
    assert_eq!(actual.sequences[0].frames.len(), 31);
    assert_eq!(actual.sequences[0].fps, 47.0);
    assert_eq!(actual.sequences[0].activity_name, "ACT_RUN");
    assert_eq!(actual.sequences[0].events, expected.sequences[0].events);
    for (expected, actual) in expected.sequences[0]
        .frames
        .iter()
        .flatten()
        .zip(actual.sequences[0].frames.iter().flatten())
    {
        assert_eq!(expected.bone, actual.bone);
        for (expected, actual) in expected
            .position
            .iter()
            .chain(&expected.rotation)
            .zip(actual.position.iter().chain(&actual.rotation))
        {
            assert!(
                (expected - actual).abs() < 0.00001,
                "pose changed: {expected} != {actual}"
            );
        }
    }
    let recompiled = compile_model(&ModelCompileRequest::new(
        moved.join("My prop.qc"),
        root.path().join("recompiled"),
    ))
    .unwrap();
    assert_eq!(vertices(&original_output), vertices(&recompiled));
    assert_eq!(
        bytes(&original_output, ModelOutputKind::Mdl),
        bytes(&recompiled, ModelOutputKind::Mdl)
    );
}

#[test]
fn portable_qc_namespace_escapes_token_delimiters_without_filename_collisions() {
    assert_eq!(
        asset_namespace(Path::new("ordinary_name-2.1.qc")),
        "ordinary_name-2.1_sources"
    );
    assert_eq!(
        asset_namespace(Path::new("My prop.qc")),
        "My~20prop_sources"
    );
    assert_eq!(
        asset_namespace(Path::new("My~20prop.qc")),
        "My~7E20prop_sources"
    );
    assert_eq!(
        asset_namespace(Path::new("é;$\"{}.qc")),
        "~C3~A9~3B~24~22~7B~7D_sources"
    );
    let names = ["My prop.qc", "My~20prop.qc", "My_prop.qc", "My~7E20prop.qc"];
    let namespaces: std::collections::BTreeSet<_> = names
        .iter()
        .map(|name| asset_namespace(Path::new(name)))
        .collect();
    assert_eq!(namespaces.len(), names.len());
}

#[test]
fn edited_qc_reports_original_locations_and_rejects_unsafe_or_non_qc_overrides() {
    let (root, mut request) = fixture();
    request.qc_text = Some("$body body cube.smd\n".into());
    assert!(
        compile_model(&request)
            .unwrap_err()
            .to_string()
            .contains("requires a QC source")
    );
    let source = root.path().join("source.qc");
    fs::write(&source, "$body body cube.smd\n").unwrap();
    request.source = source.clone();
    request.qc_text = Some("\n$unknown\n".into());
    let error = compile_model(&request).unwrap_err().to_string();
    assert!(
        error.contains(&format!("{}:2:", source.display())),
        "{error}"
    );
    request.qc_text = Some(format!(
        "$modelname props/source.mdl\n$body body {}\n$sequence idle {}\n",
        quote_path(&root.path().join("cube.smd")).unwrap(),
        quote_path(&root.path().join("cube.smd")).unwrap()
    ));
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    assert!(save_model_qc(&request, &saved.join("source.qc")).is_err());
    assert!(!saved.join("source.qc").exists());
    assert!(
        save_model_qc(&request, &source)
            .unwrap_err()
            .to_string()
            .contains("preserve the imported source")
    );
    assert_eq!(
        fs::read_to_string(&source).unwrap(),
        "$body body cube.smd\n"
    );
}

#[test]
fn portable_qc_namespace_never_overwrites_an_imported_source() {
    let (root, mut request) = fixture();
    let input = root.path().join("saved_sources");
    fs::create_dir(&input).unwrap();
    fs::copy(&request.source, input.join("cube.smd")).unwrap();
    let original = "$modelname props/original.mdl\n$staticprop\n$body body cube.smd\n$sequence idle cube.smd\n";
    request.source = input.join("original.qc");
    request.export_materials = false;
    fs::write(&request.source, original).unwrap();
    let error = save_model_qc(&request, &root.path().join("saved.qc")).unwrap_err();
    assert!(
        error.to_string().contains("overwrite an imported source"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(&request.source).unwrap(), original);
    assert!(!root.path().join("saved.qc").exists());
}

#[test]
fn portable_qc_preserves_native_comments_macro_continuations_and_scale_order() {
    let (root, mut request) = fixture();
    let source = root.path().join("original.qc");
    let original = "; { this brace is a comment\n# { so is this brace\n$cd \".\"\n$modelname props/original.mdl\n$staticprop\n$definevariable directory old\n$definemacro configure \\\\\n$cdmaterials $directory$/materials \\\\\n$scale 2\n$body first cube.smd\n$configure\n$body second cube.smd\n$sequence idle cube.smd\n";
    fs::write(&source, original).unwrap();
    request.source = source.clone();
    let expected = compile_model(&request).unwrap();
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    save_model_qc(&request, &saved.join("portable.qc")).unwrap();
    let retained = fs::read_to_string(saved.join("portable_sources/original.qc")).unwrap();
    assert!(retained.starts_with("; { this brace is a comment\n# { so is this brace\n"));
    assert!(!retained.contains("$cdmaterials"));
    fs::remove_file(&source).unwrap();
    fs::remove_file(root.path().join("cube.smd")).unwrap();
    fs::remove_file(root.path().join("material.png")).unwrap();
    let actual = compile_model(&ModelCompileRequest::new(
        saved.join("portable.qc"),
        root.path().join("recompiled"),
    ))
    .unwrap();
    assert_eq!(vertices(&expected), vertices(&actual));
    assert_eq!(
        bytes(&expected, ModelOutputKind::Mdl),
        bytes(&actual, ModelOutputKind::Mdl)
    );
    let parsed = mdlc::qc::parse_qc_file(&saved.join("portable.qc")).unwrap();
    assert_eq!(parsed.bodyparts[0].models[0].input_scale, Some(1.0));
    assert_eq!(parsed.bodyparts[1].models[0].input_scale, Some(2.0));
}

#[test]
fn portable_qc_preserves_virtual_vta_appended_obj_and_wavefront_dependencies() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input");
    fs::create_dir_all(input.join("models")).unwrap();
    fs::create_dir(root.path().join("textures")).unwrap();
    let obj =
        "mtllib paint.mtl\nv 1 2 3\nv 4 2 3\nv 1 5 3\nvn 0 0 1\nusemtl body\nf 1//1 2//1 3//1\n";
    fs::write(input.join("models/base.obj"), obj).unwrap();
    fs::write(
        input.join("models/shape.obj"),
        obj.replacen("v 1 2 3", "v 1 2 4", 1),
    )
    .unwrap();
    fs::write(input.join("models/extra.obj"), obj).unwrap();
    fs::write(
        input.join("models/paint.mtl"),
        "newmtl body\nmap_Kd ../../textures/paint.png\n",
    )
    .unwrap();
    let texture = root.path().join("textures/paint.png");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([100, 150, 200, 255]))
        .save(&texture)
        .unwrap();
    let texture_bytes = fs::read(&texture).unwrap();
    fs::write(input.join("mesh.smd"), "version 1\nnodes\n0 \"default\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\npaint\n0 1 -3 2 0 -1 0 0 0 1 0 1\n0 1 -3 5 0 -1 0 0 0 1 0 1\n0 4 -3 2 0 -1 0 0 0 1 0 1\nend\n").unwrap();
    let source = input.join("original.qc");
    let original = "$modelname props/source_support.mdl\n$fakevta virtual {\n appendvta models/base.obj 0\n appendvta models/shape.obj 1\n}\n$model body mesh.smd {\n flexfile virtual\n flex moved frame 1\n}\n$appendsource models/extra.obj \"offset pos[ 10 20 30 ] angle[ 0 0 0 ] scale[ 1 ]\"\n";
    fs::write(&source, original).unwrap();
    let mut request = ModelCompileRequest::new(source.clone(), root.path().join("original_output"));
    request.static_prop = false;
    request.export_materials = false;
    let expected = compile_model(&request).unwrap();
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    save_model_qc(&request, &saved.join("portable.qc")).unwrap();
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    assert_eq!(
        fs::read(saved.join("portable_sources/textures/paint.png")).unwrap(),
        texture_bytes
    );
    assert_eq!(
        fs::read_to_string(saved.join("portable_sources/input/models/extra.obj")).unwrap(),
        obj
    );
    assert!(!saved.join("portable_sources/input/virtual.vta").exists());
    for (dependency, portable, absolute) in [
        (
            input.join("models/base.obj"),
            obj.to_owned(),
            obj.replace(
                "mtllib paint.mtl",
                &format!("mtllib {}", input.join("models/paint.mtl").display()),
            ),
        ),
        (
            input.join("models/paint.mtl"),
            "newmtl body\nmap_Kd ../../textures/paint.png\n".to_owned(),
            format!("newmtl body\nmap_Kd {}\n", texture.display()),
        ),
    ] {
        fs::write(&dependency, absolute).unwrap();
        let error = save_model_qc(&request, &saved.join("portable.qc")).unwrap_err();
        assert!(error.to_string().contains("relative Wavefront"));
        fs::write(dependency, portable).unwrap();
    }
    fs::remove_dir_all(&input).unwrap();
    fs::remove_dir_all(root.path().join("textures")).unwrap();
    let moved = root.path().join("moved");
    fs::rename(saved, &moved).unwrap();
    let mut request =
        ModelCompileRequest::new(moved.join("portable.qc"), root.path().join("recompiled"));
    request.static_prop = false;
    request.export_materials = false;
    let actual = compile_model(&request).unwrap();
    assert_eq!(vertices(&expected), vertices(&actual));
    assert_eq!(
        bytes(&expected, ModelOutputKind::Mdl),
        bytes(&actual, ModelOutputKind::Mdl)
    );
}

#[test]
fn portable_qc_relocates_search_directories_and_their_includes() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input");
    let shared = root.path().join("shared");
    fs::create_dir(&input).unwrap();
    fs::create_dir(&shared).unwrap();
    fs::write(
        shared.join("mesh.smd"),
        include_str!("../../../vendor/mdlc/tests/fixtures/cube.smd"),
    )
    .unwrap();
    fs::write(
        shared.join("body.qci"),
        "$body body mesh.smd\n$surfaceprop metal\n",
    )
    .unwrap();
    let source = input.join("original.qc");
    fs::write(
        &source,
        format!(
            "$modelname props/search.mdl\n$addsearchdir \"{}\"\n$include body.qci\n",
            shared.display()
        ),
    )
    .unwrap();
    let mut request = ModelCompileRequest::new(source, root.path().join("original_output"));
    request.export_materials = false;
    let expected = compile_model(&request).unwrap();
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    save_model_qc(&request, &saved.join("portable.qc")).unwrap();
    fs::remove_dir_all(&input).unwrap();
    fs::remove_dir_all(&shared).unwrap();
    let moved = root.path().join("moved");
    fs::rename(saved, &moved).unwrap();
    let mut request =
        ModelCompileRequest::new(moved.join("portable.qc"), root.path().join("recompiled"));
    request.export_materials = false;
    let actual = compile_model(&request).unwrap();
    assert_eq!(
        bytes(&expected, ModelOutputKind::Mdl),
        bytes(&actual, ModelOutputKind::Mdl)
    );
    assert_eq!(vertices(&expected), vertices(&actual));
}

#[test]
fn direct_dmx_import_and_portable_qc_preserve_mesh_parts_and_morph_output() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("triangle.dmx");
    fs::write(
        &source,
        include_bytes!("../../../vendor/mdlc/tests/fixtures/dmx.dmx"),
    )
    .unwrap();
    let inspection = model_compile::inspect_model(&source).unwrap();
    assert_eq!(
        inspection
            .parts
            .iter()
            .map(|part| part.name.as_str())
            .collect::<Vec<_>>(),
        ["body"]
    );
    assert_eq!(inspection.materials, ["paint"]);
    assert_eq!(inspection.animations[0].frame_count, 1);
    let mut request = ModelCompileRequest::new(source.clone(), root.path().join("original_output"));
    request.model_name = Some("props/dmx.mdl".into());
    request.static_prop = false;
    request.export_materials = false;
    let expected = compile_model(&request).unwrap();
    let mdl = bytes(&expected, ModelOutputKind::Mdl);
    assert_eq!(i32::from_le_bytes(mdl[268..272].try_into().unwrap()), 1);
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    save_model_qc(&request, &saved.join("portable.qc")).unwrap();
    fs::remove_file(source).unwrap();
    let moved = root.path().join("moved");
    fs::rename(saved, &moved).unwrap();
    let mut request =
        ModelCompileRequest::new(moved.join("portable.qc"), root.path().join("recompiled"));
    request.static_prop = false;
    request.export_materials = false;
    let actual = compile_model(&request).unwrap();
    assert_eq!(vertices(&expected), vertices(&actual));
    assert_eq!(
        bytes(&expected, ModelOutputKind::Mdl),
        bytes(&actual, ModelOutputKind::Mdl)
    );
}

#[test]
fn blender_authoring_saved_qc_remains_portable_and_preserves_edited_geometry_and_settings() {
    let (root, mut request) = fixture();
    let original = root.path().join("original.qc");
    fs::write(&original, "$modelname models/authored.mdl\n$cdmaterials models/qc_saved\n$surfaceprop metal\n$body primary cube.smd\n$bodygroup options {\n studio cube.smd\n blank\n}\n$attachment muzzle root 1 2 3 rotate 10 20 30\n$hboxset shots\n$hbox 2 root -2 -3 -4 5 6 7\n$sequence idle cube.smd fps 24 loop activity ACT_IDLE 2 { event 5004 0 fire }\n$texturegroup skins { { material } { alternate } }\n$lod 25 { replacemodel cube.smd cube.smd }\n$collisionmodel cube.smd { $mass 31 }\n").unwrap();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([60, 150, 220, 255]))
        .save(root.path().join("alternate.png"))
        .unwrap();
    request.source = original;
    request.static_prop = false;
    let initial = compile_model(&request).unwrap();
    let mdl = initial
        .files
        .iter()
        .find(|file| file.kind == ModelOutputKind::Mdl)
        .unwrap();
    let exported =
        crate::model_decompile::export_blender_model(&mdl.path, &root.path().join("blender"))
            .unwrap();
    let mut glb = fs::read(&exported.model_path).unwrap();
    let json_length = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
    let document: serde_json::Value = serde_json::from_slice(&glb[20..20 + json_length]).unwrap();
    let accessor = &document["accessors"]
        [document["meshes"][0]["primitives"][0]["attributes"]["POSITION"]
            .as_u64()
            .unwrap() as usize];
    let view = &document["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let offset = 28
        + json_length
        + view["byteOffset"].as_u64().unwrap_or(0) as usize
        + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
    let position = f32::from_le_bytes(glb[offset..offset + 4].try_into().unwrap()) + 0.0254;
    glb[offset..offset + 4].copy_from_slice(&position.to_le_bytes());
    fs::write(&exported.model_path, glb).unwrap();
    let request = ModelCompileRequest::new(exported.model_path, root.path().join("edited"));
    let expected = compile_model(&request).unwrap();
    assert_ne!(vertices(&initial), vertices(&expected));
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    save_model_qc(&request, &saved.join("prop.qc")).unwrap();
    let desc = mdlc::qc::parse_qc_file(&saved.join("prop.qc")).unwrap();
    assert_eq!(desc.bodyparts.len(), 2);
    assert_eq!(desc.bodyparts[1].models.len(), 2);
    assert!(desc.bodyparts[1].models[1].smd.is_empty());
    assert_eq!(desc.bodyparts[0].models[0].lods[0].switch_point, Some(25.0));
    assert_eq!(desc.materials.skin_families.len(), 2);
    assert_eq!(desc.attachments[0].name, "muzzle");
    assert_eq!(desc.attachments[0].bone, "root");
    assert_eq!(desc.hitboxes.set_name.as_deref(), Some("shots"));
    assert_eq!(desc.hitboxes.boxes[0].group, Some(2));
    assert_eq!(desc.model.surface_prop.as_deref(), Some("metal"));
    assert_eq!(desc.physics.mass, Some(31.0));
    let sequence = &desc.sequences[0];
    let animation = desc.animations.iter().find(|animation| {
        sequence.smd == animation.name
            || sequence
                .blends
                .first()
                .is_some_and(|name| name == &animation.name)
    });
    assert_eq!(
        sequence
            .fps
            .or_else(|| animation.and_then(|animation| animation.fps)),
        Some(24.0)
    );
    assert!(desc.sequences[0].looping);
    assert_eq!(desc.sequences[0].activity.as_deref(), Some("ACT_IDLE"));
    assert_eq!(desc.sequences[0].events[0].event, 5004);
    assert_eq!(desc.sequences[0].events[0].options, "fire");
    let moved = root.path().join("moved");
    fs::rename(saved, &moved).unwrap();
    fs::remove_dir_all(root.path().join("blender")).unwrap();
    for filename in ["original.qc", "cube.smd", "material.png", "alternate.png"] {
        fs::remove_file(root.path().join(filename)).unwrap();
    }
    let actual = compile_model(&ModelCompileRequest::new(
        moved.join("prop.qc"),
        root.path().join("rebuilt"),
    ))
    .unwrap();
    let expected_vertices = vertices(&expected);
    let actual_vertices = vertices(&actual);
    assert_eq!(expected_vertices.len(), actual_vertices.len());
    for (expected, actual) in expected_vertices.iter().zip(&actual_vertices) {
        for (expected, actual) in expected.iter().zip(actual) {
            assert!((expected - actual).abs() < 0.002, "{expected} != {actual}");
        }
    }
    let expected_bounds = collision_bounds(&expected);
    let actual_bounds = collision_bounds(&actual);
    for (expected, actual) in [expected_bounds.0, expected_bounds.1]
        .iter()
        .zip([actual_bounds.0, actual_bounds.1])
    {
        for (expected, actual) in expected.iter().zip(actual) {
            assert!((expected - actual).abs() < 0.002);
        }
    }
    assert!(actual.files.iter().any(|file| {
        file.path
            .ends_with("materials/models/qc_saved/material.vmt")
    }));
}

#[test]
fn saving_a_direct_source_never_replaces_the_imported_mesh_with_baked_output() {
    let (root, mut request) = fixture();
    let input = root.path().join("saved_sources");
    fs::create_dir(&input).unwrap();
    request.source = input.join("mesh.smd");
    let original = include_bytes!("../../../vendor/mdlc/tests/fixtures/cube.smd");
    fs::write(&request.source, original).unwrap();
    request.static_prop = true;
    request.scale = 2.0;
    request.export_materials = false;
    request.overwrite = true;
    let error = save_model_qc(&request, &root.path().join("saved.qc")).unwrap_err();
    assert!(
        error.to_string().contains("overwrite an imported source"),
        "{error}"
    );
    assert_eq!(fs::read(&request.source).unwrap(), original);
    assert!(!root.path().join("saved.qc").exists());
    assert!(!root.path().join(".gmpublished-qc-projects.json").exists());

    let expected = compile_model(&request).unwrap();
    let saved = root.path().join("safe.qc");
    save_model_qc(&request, &saved).unwrap();
    let reopened = ModelCompileRequest::new(saved, root.path().join("reopened"));
    assert_eq!(
        vertices(&compile_model(&reopened).unwrap()),
        vertices(&expected)
    );
    assert_eq!(fs::read(&request.source).unwrap(), original);
}

#[test]
fn saved_projects_cannot_change_each_others_materials_and_owned_resaves_remain_safe() {
    let (root, mut first) = fixture();
    first.material_directory = None;
    let second_source = root.path().join("blue");
    fs::create_dir(&second_source).unwrap();
    fs::copy(&first.source, second_source.join("cube.smd")).unwrap();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 255, 255]))
        .save(second_source.join("material.png"))
        .unwrap();
    let mut second = first.clone();
    second.source = second_source.join("cube.smd");
    second.destination = root.path().join("second-output");
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    let first_qc = saved.join("one.qc");
    save_model_qc(&first, &first_qc).unwrap();
    let original_qc = fs::read(&first_qc).unwrap();
    let original_texture = fs::read(saved.join("materials/material.vtf")).unwrap();
    let registry = fs::read(saved.join(".gmpublished-qc-projects.json")).unwrap();
    let first_input = fs::read(&first.source).unwrap();
    let first_image = fs::read(root.path().join("material.png")).unwrap();
    let second_image = fs::read(second_source.join("material.png")).unwrap();

    second.overwrite = true;
    assert!(save_model_qc(&second, &saved.join("two.qc")).is_err());
    assert_eq!(fs::read(&first_qc).unwrap(), original_qc);
    assert_eq!(
        fs::read(saved.join("materials/material.vtf")).unwrap(),
        original_texture
    );
    assert_eq!(
        fs::read(saved.join(".gmpublished-qc-projects.json")).unwrap(),
        registry
    );
    assert!(!saved.join("two.qc").exists());

    // A deliberate update to this project's own unchanged artifacts is allowed.
    first.scale = 2.0;
    save_model_qc(&first, &first_qc).unwrap();
    let expected = compile_model(&first).unwrap();
    let reopened = ModelCompileRequest::new(first_qc, root.path().join("reopened-one"));
    let actual = compile_model(&reopened).unwrap();
    assert_eq!(vertices(&expected), vertices(&actual));
    assert_eq!(bytes(&actual, ModelOutputKind::Texture), original_texture);

    // A distinct material directory makes the second project independent.
    second.material_directory = Some("blue".into());
    save_model_qc(&second, &saved.join("two.qc")).unwrap();
    let expected = compile_model(&second).unwrap();
    let reopened = ModelCompileRequest::new(saved.join("two.qc"), root.path().join("reopened-two"));
    let actual = compile_model(&reopened).unwrap();
    assert_eq!(vertices(&expected), vertices(&actual));
    assert_eq!(
        bytes(&expected, ModelOutputKind::Texture),
        bytes(&actual, ModelOutputKind::Texture)
    );
    assert_eq!(
        fs::read(saved.join("materials/material.vtf")).unwrap(),
        original_texture
    );
    assert_eq!(fs::read(&first.source).unwrap(), first_input);
    assert_eq!(
        fs::read(root.path().join("material.png")).unwrap(),
        first_image
    );
    assert_eq!(
        fs::read(second_source.join("material.png")).unwrap(),
        second_image
    );
}

#[test]
fn identical_shared_materials_are_reused_but_their_owner_cannot_change_them() {
    let (root, mut request) = fixture();
    request.material_directory = None;
    let saved = root.path().join("saved");
    fs::create_dir(&saved).unwrap();
    save_model_qc(&request, &saved.join("one.qc")).unwrap();
    save_model_qc(&request, &saved.join("two.qc")).unwrap();
    let texture = fs::read(saved.join("materials/material.vtf")).unwrap();
    let qc = fs::read(saved.join("one.qc")).unwrap();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 255, 255]))
        .save(root.path().join("material.png"))
        .unwrap();
    let error = save_model_qc(&request, &saved.join("one.qc")).unwrap_err();
    assert!(
        error.to_string().contains("used by another project"),
        "{error}"
    );
    assert_eq!(
        fs::read(saved.join("materials/material.vtf")).unwrap(),
        texture
    );
    assert_eq!(fs::read(saved.join("one.qc")).unwrap(), qc);
}

#[test]
fn protection_collects_gltf_buffers_images_and_material_resolver_sources() {
    let root = tempfile::tempdir().unwrap();
    let request = animated_gltf_project(root.path());
    let source = canonical(&request.source).unwrap();
    let description =
        model_compile::load_description(&request, &source, source.parent().unwrap()).unwrap();
    let inputs = portable_qc::input_files(&request, &description, &source).unwrap();
    for file in [
        "input/character.gltf",
        "input/buffers/geometry.bin",
        "input/Textures/Face Color.PNG",
    ] {
        assert!(
            inputs.contains(&canonical(&root.path().join(file)).unwrap()),
            "{file}"
        );
    }

    let (root, mut request) = fixture();
    request.material_directory = None;
    let output = compile_model(&request).unwrap();
    // Treat an exported VMT/VTF pair as material inputs to another direct mesh.
    let files = model_materials::prepare_materials(&["material".into()], &[], &request.destination)
        .unwrap()
        .source_paths;
    for file in output.files.iter().filter(|file| {
        matches!(
            file.kind,
            ModelOutputKind::Material | ModelOutputKind::Texture
        )
    }) {
        assert!(
            files.contains(&canonical(&file.path).unwrap()),
            "{}",
            file.path.display()
        );
    }
    let originals: BTreeMap<_, _> = files
        .iter()
        .map(|file| (file.clone(), fs::read(file).unwrap()))
        .collect();
    let source = root.path().join("original/cube.smd");
    fs::copy(&request.source, &source).unwrap();
    request.source = source;
    save_model_qc(&request, &root.path().join("original/safe.qc")).unwrap();
    for (path, bytes) in originals {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn relative_skin_texture_inputs_use_the_model_directory_and_survive_saving() {
    let (root, mut request) = fixture();
    request.skins.push(ModelSkin {
        name: "replacement".into(),
        replacements: BTreeMap::from([(
            "material".into(),
            SkinMaterial::Texture {
                path: "material.png".into(),
            },
        )]),
    });
    let source = canonical(&request.source).unwrap();
    let description =
        model_compile::load_description(&request, &source, source.parent().unwrap()).unwrap();
    let inputs = portable_qc::input_files(&request, &description, &source).unwrap();
    let texture = root.path().join("material.png");
    assert!(inputs.contains(&canonical(&texture).unwrap()));
    let original = fs::read(&texture).unwrap();
    save_model_qc(&request, &root.path().join("relative-skin.qc")).unwrap();
    let expected = compile_model(&request).unwrap();
    let reopened = ModelCompileRequest::new(
        root.path().join("relative-skin.qc"),
        root.path().join("reopened-skin"),
    );
    let actual = compile_model(&reopened).unwrap();
    assert_eq!(vertices(&expected), vertices(&actual));
    assert_eq!(fs::read(texture).unwrap(), original);
}
