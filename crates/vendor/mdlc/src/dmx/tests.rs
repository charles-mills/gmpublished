use super::*;

fn element(
    class: &str,
    name: &str,
    attributes: impl IntoIterator<Item = (&'static str, Value)>,
) -> Node {
    Node {
        id: None,
        class: class.into(),
        name: name.into(),
        attributes: attributes.into_iter().map(|(k, v)| (k.into(), v)).collect(),
    }
}

fn fixture() -> Document {
    Document {
        root: 0,
        format: "model".into(),
        version: 18,
        nodes: vec![
            element(
                "DmElement",
                "root",
                [
                    ("model", Value::Element(Some(1))),
                    ("skeleton", Value::Element(Some(1))),
                    ("combinationOperator", Value::Element(Some(10))),
                ],
            ),
            element(
                "DmeModel",
                "model",
                [
                    ("children", Value::ElementArray(vec![Some(2)])),
                    (
                        "jointTransforms",
                        Value::ElementArray(vec![Some(3), Some(5)]),
                    ),
                ],
            ),
            element(
                "DmeJoint",
                "root",
                [
                    ("transform", Value::Element(Some(3))),
                    ("children", Value::ElementArray(vec![Some(4)])),
                ],
            ),
            element(
                "DmeTransform",
                "root",
                [
                    ("position", Value::Vector3([10., 0., 0.])),
                    ("orientation", Value::Quaternion([0., 0., 0., 1.])),
                ],
            ),
            element(
                "DmeDag",
                "mesh",
                [
                    ("transform", Value::Element(Some(5))),
                    ("shape", Value::Element(Some(6))),
                ],
            ),
            element(
                "DmeTransform",
                "mesh",
                [("position", Value::Vector3([0., 2., 0.]))],
            ),
            element(
                "DmeMesh",
                "mesh",
                [
                    ("bindState", Value::Element(Some(7))),
                    ("faceSets", Value::ElementArray(vec![Some(8)])),
                    ("deltaStates", Value::ElementArray(vec![Some(12)])),
                ],
            ),
            element(
                "DmeVertexData",
                "bind",
                [
                    (
                        "positions",
                        Value::Vector3Array(vec![
                            [0., 0., 0.],
                            [2., 0., 0.],
                            [2., 2., 0.],
                            [0., 2., 0.],
                        ]),
                    ),
                    (
                        "normals",
                        Value::Vector3Array(vec![[0., 0., 1.], [0., 1., 0.]]),
                    ),
                    (
                        "textureCoordinates",
                        Value::Vector2Array(vec![
                            [0., 0.],
                            [1., 0.],
                            [1., 1.],
                            [0., 1.],
                            [0.5, 0.5],
                        ]),
                    ),
                    ("positionsIndices", Value::IntArray(vec![0, 1, 2, 0, 2, 3])),
                    ("normalsIndices", Value::IntArray(vec![0, 0, 0, 1, 0, 0])),
                    (
                        "textureCoordinatesIndices",
                        Value::IntArray(vec![0, 1, 2, 4, 2, 3]),
                    ),
                    ("flipVCoordinates", Value::Bool(true)),
                    ("jointCount", Value::Int(2)),
                    (
                        "jointWeights",
                        Value::FloatArray(vec![0.25, 0.75, 1., 0., 1., 0., 1., 0.]),
                    ),
                    (
                        "jointIndices",
                        Value::IntArray(vec![0, 1, 0, 1, 0, 1, 0, 1]),
                    ),
                    ("balance", Value::FloatArray(vec![0.2, 0.4, 0.6, 0.8])),
                    ("balanceIndices", Value::IntArray(vec![0, 1, 2, 0, 2, 3])),
                ],
            ),
            element(
                "DmeFaceSet",
                "paint",
                [
                    ("material", Value::Element(Some(9))),
                    ("faces", Value::IntArray(vec![0, 1, 2, -1, 3, 4, 5, -1])),
                ],
            ),
            element(
                "DmeMaterial",
                "paint",
                [("mtlName", Value::String("models/test/paint".into()))],
            ),
            element(
                "DmeCombinationOperator",
                "combination",
                [
                    ("controls", Value::ElementArray(vec![Some(11)])),
                    ("targets", Value::ElementArray(vec![Some(6)])),
                ],
            ),
            element(
                "DmeCombinationInputControl",
                "smile",
                [
                    ("rawControlNames", Value::StringArray(vec!["smile".into()])),
                    ("stereo", Value::Bool(true)),
                    ("wrinkleScales", Value::FloatArray(vec![0.5])),
                ],
            ),
            element(
                "DmeVertexDeltaData",
                "smile",
                [
                    ("positions", Value::Vector3Array(vec![[0., 0., 2.]])),
                    ("positionsIndices", Value::IntArray(vec![0])),
                    ("normals", Value::Vector3Array(vec![[0.2, 0., -0.1]])),
                    ("normalsIndices", Value::IntArray(vec![0])),
                ],
            ),
        ],
    }
}

#[test]
fn indexed_geometry_skinning_and_sparse_morph_streams_survive_dmx_import() {
    let geometry = from_document(&fixture(), &FbxOpts::default()).unwrap();
    assert_eq!(
        geometry
            .smd
            .nodes
            .iter()
            .map(|n| (&*n.name, n.parent))
            .collect::<Vec<_>>(),
        [("root", -1), ("mesh", 0), ("defaultRoot", -1)]
    );
    assert_eq!(geometry.smd.frames[0].poses[0].position, [10., 0., 0.]);
    assert_eq!(geometry.smd.triangles.len(), 2);
    let first = &geometry.smd.triangles[0].vertices[0];
    assert_eq!(first.position, [10., 2., 0.]);
    assert_eq!(first.uv, [0., 1.]);
    assert_eq!(
        first.links,
        [
            SmdBoneLink {
                bone: 1,
                weight: 0.75
            },
            SmdBoneLink {
                bone: 0,
                weight: 0.25
            }
        ]
    );
    let seam = &geometry.smd.triangles[1].vertices[0];
    assert_ne!(first.src_index, seam.src_index);
    assert_eq!(seam.normal, [0., 1., 0.]);
    let shape = &geometry.metadata.shapes[0];
    assert!(shape.wrinkle);
    let a = shape
        .vertices
        .iter()
        .find(|v| v.vertex == first.src_index)
        .unwrap();
    let b = shape
        .vertices
        .iter()
        .find(|v| v.vertex == seam.src_index)
        .unwrap();
    assert_eq!(a.position, [0., 0., 2.]);
    assert_eq!(b.position, [0., 0., 2.]);
    assert_eq!(a.normal, [0.2, 0., -0.1]);
    assert_eq!(b.normal, [0.; 3]);
    assert_eq!(a.wrinkle, 0.5);
    assert_eq!(b.wrinkle, 0.5);
    assert_eq!(a.balance, 0.2);
    assert!(
        shape
            .vertices
            .iter()
            .any(|v| v.position == [0.; 3] && v.normal != [0.; 3])
    );
}

#[test]
fn mesh_dag_and_root_axis_transform_affect_positions_normals_and_deltas_consistently() {
    let geometry = from_document(
        &fixture(),
        &FbxOpts {
            axis: Some(ForcedAxis::Y),
            scale: 2.,
            ..FbxOpts::default()
        },
    )
    .unwrap();
    let vertex = &geometry.smd.triangles[0].vertices[0];
    for (actual, expected) in vertex.position.into_iter().zip([20., 0., 4.]) {
        assert!((actual - expected).abs() < 1e-5);
    }
    for (actual, expected) in vertex.normal.into_iter().zip([0., -1., 0.]) {
        assert!((actual - expected).abs() < 1e-5);
    }
    let delta = &geometry.metadata.shapes[0].vertices[0];
    for (actual, expected) in delta.position.into_iter().zip([0., -4., 0.]) {
        assert!((actual - expected).abs() < 1e-5);
    }
}

#[test]
fn malformed_mesh_attributes_indices_and_cycles_return_errors() {
    let mut document = fixture();
    document.nodes[7]
        .attributes
        .insert("positionsIndices".into(), Value::IntArray(vec![99]));
    assert!(
        from_document(&document, &FbxOpts::default())
            .unwrap_err()
            .contains("position index")
    );
    let mut document = fixture();
    document.nodes[2]
        .attributes
        .insert("children".into(), Value::ElementArray(vec![Some(2)]));
    assert!(
        from_document(&document, &FbxOpts::default())
            .unwrap_err()
            .contains("cycle")
    );
    let mut document = fixture();
    document.nodes[7]
        .attributes
        .insert("jointIndices".into(), Value::IntArray(vec![99; 8]));
    assert!(
        from_document(&document, &FbxOpts::default())
            .unwrap_err()
            .contains("skin joint")
    );
    let mut document = fixture();
    document.nodes[6].attributes.remove("bindState");
    document.nodes[6]
        .attributes
        .insert("baseStates".into(), Value::String("bind".into()));
    assert!(
        from_document(&document, &FbxOpts::default())
            .unwrap_err()
            .contains("baseStates must be an element array")
    );
    let mut document = fixture();
    document.nodes[8]
        .attributes
        .insert("faces".into(), Value::IntArray(vec![99; 4097]));
    assert!(
        from_document(&document, &FbxOpts::default())
            .unwrap_err()
            .contains("polygon exceeds 4096 corners")
    );
}

#[test]
fn concave_polygons_triangulate_without_covering_the_notch() {
    let mut document = fixture();
    document.nodes[7].attributes.insert(
        "positions".into(),
        Value::Vector3Array(vec![
            [0., 0., 0.],
            [3., 0., 0.],
            [3., 3., 0.],
            [1.5, 1., 0.],
            [0., 3., 0.],
        ]),
    );
    for key in [
        "positionsIndices",
        "normalsIndices",
        "textureCoordinatesIndices",
        "balanceIndices",
        "jointIndices",
        "jointWeights",
    ] {
        document.nodes[7].attributes.remove(key);
    }
    document.nodes[7]
        .attributes
        .insert("jointCount".into(), Value::Int(0));
    document.nodes[7]
        .attributes
        .insert("normals".into(), Value::Vector3Array(vec![[0., 0., 1.]; 5]));
    document.nodes[7].attributes.remove("balance");
    document.nodes[8]
        .attributes
        .insert("faces".into(), Value::IntArray(vec![0, 1, 2, 3, 4, -1]));
    document.nodes[6].attributes.remove("deltaStates");
    let geometry = from_document(&document, &FbxOpts::default()).unwrap();
    assert_eq!(geometry.smd.triangles.len(), 3);
    let area: f32 = geometry
        .smd
        .triangles
        .iter()
        .map(|t| {
            let a = t.vertices[0].position;
            let b = t.vertices[1].position;
            let c = t.vertices[2].position;
            ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() / 2.
        })
        .sum();
    assert_eq!(area, 6.);
}

fn keyvalues(document: &Document) -> String {
    fn id(i: usize) -> String {
        format!("00000000-0000-0000-0000-{i:012x}")
    }
    fn array(values: impl IntoIterator<Item = String>) -> String {
        format!(
            "[ {} ]",
            values
                .into_iter()
                .map(|v| format!("\"{v}\""))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
    let mut text = "<!-- dmx encoding keyvalues2 1 format model 18 -->\n".to_string();
    for (i, node) in document.nodes.iter().enumerate() {
        text.push_str(&format!(
            "\"{}\" {{\n\"id\" \"elementid\" \"{}\"\n\"name\" \"string\" \"{}\"\n",
            node.class,
            id(i),
            node.name
        ));
        for (name, value) in &node.attributes {
            let (kind, value) = match value {
                Value::Element(Some(i)) => ("element", format!("\"{}\"", id(*i))),
                Value::ElementArray(values) => (
                    "element_array",
                    format!(
                        "[ {} ]",
                        values
                            .iter()
                            .map(|i| format!("\"element\" \"{}\"", i.map(id).unwrap_or_default()))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                ),
                Value::String(value) => ("string", format!("\"{value}\"")),
                Value::StringArray(values) => ("string_array", array(values.iter().cloned())),
                Value::Bool(value) => ("bool", format!("\"{}\"", u8::from(*value))),
                Value::Int(value) => ("int", format!("\"{value}\"")),
                Value::Float(value) => ("float", format!("\"{value}\"")),
                Value::Vector3(value) => (
                    "vector3",
                    format!("\"{} {} {}\"", value[0], value[1], value[2]),
                ),
                Value::Quaternion(value) => (
                    "quaternion",
                    format!("\"{} {} {} {}\"", value[0], value[1], value[2], value[3]),
                ),
                Value::IntArray(values) => {
                    ("int_array", array(values.iter().map(ToString::to_string)))
                }
                Value::FloatArray(values) => {
                    ("float_array", array(values.iter().map(ToString::to_string)))
                }
                Value::Vector2Array(values) => (
                    "vector2_array",
                    array(values.iter().map(|v| format!("{} {}", v[0], v[1]))),
                ),
                Value::Vector3Array(values) => (
                    "vector3_array",
                    array(values.iter().map(|v| format!("{} {} {}", v[0], v[1], v[2]))),
                ),
                _ => panic!("Unexpected fixture attribute {name}"),
            };
            text.push_str(&format!("\"{name}\" \"{kind}\" {value}\n"));
        }
        text.push_str("}\n");
    }
    text
}

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn directory() -> Fixture {
    static SERIAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "mdlc_dmx_{}_{}",
        std::process::id(),
        SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    Fixture(path)
}

#[test]
fn qc_dmx_compilation_writes_stereo_controllers_normal_deltas_and_wrinkle_payload() {
    let dir = directory();
    std::fs::write(dir.0.join("mesh.dmx"), keyvalues(&fixture())).unwrap();
    let desc = crate::qc::parse_qc_str(
        "$modelname tests/dmx.mdl\n$model body mesh.dmx\n$sequence idle mesh.dmx\n",
        &dir.0,
    )
    .unwrap();
    let output =
        crate::pipeline::build(&desc, &dir.0, crate::pipeline::PipelineOptions::default()).unwrap();
    let compiled = &output.compiled;
    assert_eq!(
        compiled
            .desc
            .flex_controllers
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["right_smile", "left_smile"]
    );
    assert_eq!(
        compiled
            .desc
            .flex_descriptors
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["smileL", "smileR"]
    );
    assert_eq!(compiled.resolved_flex_rules.len(), 2);
    assert!(
        compiled
            .resolved_flex_rules
            .iter()
            .all(|r| r.ops.len() == 1 && r.ops[0].op == 2)
    );
    let int = |at| i32::from_le_bytes(output.mdl[at..at + 4].try_into().unwrap());
    let body = int(236) as usize;
    let model = body + int(body + 12) as usize;
    let mesh = model + int(model + 76) as usize;
    assert_eq!(int(mesh + 16), 1);
    let flex = mesh + int(mesh + 20) as usize;
    assert_eq!(int(flex + 28), 1);
    assert_eq!(output.mdl[flex + 32], 1);
    let anim = flex + int(flex + 24) as usize;
    let count = int(flex + 20) as usize;
    assert!(count >= 4);
    assert!((0..count).any(|i| output.mdl[anim + i * 18 + 10..anim + i * 18 + 16] != [0; 6]));
    assert!((0..count).any(|i| output.mdl[anim + i * 18 + 16..anim + i * 18 + 18] == [0, 8]));
    let ui = int(388) as usize;
    assert_eq!(output.mdl[ui + 17], 1);
    assert_eq!(output.mdl[ui + 16], 0);
}

#[test]
fn appended_dmx_preserves_coincident_vertices_with_distinct_morph_deltas() {
    let mut document = fixture();
    let bind = &mut document.nodes[7].attributes;
    bind.insert(
        "positions".into(),
        Value::Vector3Array(vec![[0., 0., 0.], [2., 0., 0.], [0., 2., 0.], [0., 0., 0.]]),
    );
    bind.insert(
        "positionsIndices".into(),
        Value::IntArray(vec![0, 1, 2, 3, 1, 2]),
    );
    bind.insert("normalsIndices".into(), Value::IntArray(vec![0; 6]));
    bind.insert(
        "textureCoordinatesIndices".into(),
        Value::IntArray(vec![0, 1, 2, 0, 1, 2]),
    );
    bind.insert(
        "balanceIndices".into(),
        Value::IntArray(vec![0, 1, 2, 0, 1, 2]),
    );
    bind.insert("jointCount".into(), Value::Int(0));
    bind.remove("jointWeights");
    bind.remove("jointIndices");
    let dir = directory();
    std::fs::write(dir.0.join("mesh.dmx"), keyvalues(&document)).unwrap();
    std::fs::write(
        dir.0.join("base.obj"),
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nusemtl base\nf 1 2 3\n",
    )
    .unwrap();
    let desc = crate::qc::parse_qc_str(
        "$modelname tests/append_dmx.mdl\n$body body base.obj\n$appendsource mesh.dmx\n",
        &dir.0,
    )
    .unwrap();
    let output =
        crate::pipeline::build(&desc, &dir.0, crate::pipeline::PipelineOptions::default()).unwrap();
    let model = &output.compiled.bodyparts[0].models[0];
    let mesh = model
        .meshes
        .iter()
        .position(|mesh| mesh.triangles.len() == 2)
        .unwrap();
    let vertices = &model.meshes[mesh].vertices;
    assert_eq!(vertices.len(), 4);
    let pair = (0..vertices.len())
        .find_map(|a| {
            (a + 1..vertices.len())
                .find(|&b| vertices[a] == vertices[b])
                .map(|b| [a, b])
        })
        .unwrap();
    let flex = &model.mesh_flexes[mesh][0];
    let mut deltas = pair.map(|vertex| {
        flex.vertanims
            .iter()
            .find(|delta| delta.index as usize == vertex)
            .map(|delta| delta.delta.iter().map(|v| v.abs()).sum::<f32>())
            .unwrap_or(0.)
    });
    deltas.sort_by(f32::total_cmp);
    assert!((deltas[0]).abs() < 1e-5);
    assert!((deltas[1] - 2.).abs() < 1e-5);
}

#[test]
fn compiled_two_way_and_n_way_combination_rules_keep_operand_indices() {
    for raw in [vec!["smile", "frown"], vec!["smile", "neutral", "frown"]] {
        let mut document = fixture();
        document.nodes[11]
            .attributes
            .insert("stereo".into(), Value::Bool(false));
        document.nodes[11].attributes.insert(
            "rawControlNames".into(),
            Value::StringArray(raw.iter().map(|s| s.to_string()).collect()),
        );
        document.nodes[11].attributes.insert(
            "wrinkleScales".into(),
            Value::FloatArray(vec![0.; raw.len()]),
        );
        let dir = directory();
        std::fs::write(dir.0.join("mesh.dmx"), keyvalues(&document)).unwrap();
        let desc = crate::qc::parse_qc_str(
            "$modelname tests/dmx.mdl\n$body body mesh.dmx\n$sequence idle mesh.dmx\n",
            &dir.0,
        )
        .unwrap();
        let output =
            crate::pipeline::build(&desc, &dir.0, crate::pipeline::PipelineOptions::default())
                .unwrap();
        let rules = &output.compiled.resolved_flex_rules;
        assert_eq!(rules.len(), 1);
        let last = rules[0].ops.last().unwrap();
        assert_eq!(last.op, if raw.len() == 2 { 15 } else { 17 });
        assert_eq!(last.d, crate::model::FlexOpData::Index(0));
        let ui = &output.compiled.resolved_flex_controller_ui[0];
        assert_eq!(ui.remap_type, if raw.len() == 2 { 1 } else { 2 });
        if raw.len() == 3 {
            assert_eq!(ui.fc2, Some(1));
            assert_eq!(rules[0].ops[4].d, crate::model::FlexOpData::Value(1.));
        }
    }
}

#[test]
fn qc_imports_dmx_attachments_preserves_their_bones_and_allows_redefinition() {
    for (rigid, override_qc, expected_bone, translation, flags) in [
        (false, "", "mount", [0., 0., 0.], 0x10000),
        (true, "", "root", [1., 2., 3.], 0x10000),
        (
            false,
            "$redefineattachment muzzle root 5 6 7 rotate 0 0 0\n",
            "root",
            [5., 6., 7.],
            0,
        ),
    ] {
        let mut document = fixture();
        document.nodes[2].attributes.insert(
            "children".into(),
            Value::ElementArray(vec![Some(4), Some(13)]),
        );
        document.nodes[1].attributes.insert(
            "jointTransforms".into(),
            Value::ElementArray(vec![Some(3), Some(5), Some(14)]),
        );
        document.nodes.extend([
            element(
                "DmeDag",
                "mount",
                [
                    ("transform", Value::Element(Some(14))),
                    ("shape", Value::Element(Some(15))),
                ],
            ),
            element(
                "DmeTransform",
                "mount",
                [("position", Value::Vector3([1., 2., 3.]))],
            ),
            element(
                "DmeAttachment",
                "muzzle",
                [
                    ("isRigid", Value::Bool(rigid)),
                    ("isWorldAligned", Value::Bool(true)),
                ],
            ),
        ]);
        let dir = directory();
        std::fs::write(dir.0.join("mesh.dmx"), keyvalues(&document)).unwrap();
        let text = format!(
            "$modelname tests/dmx_attachment.mdl\n$model body mesh.dmx\n{override_qc}$sequence idle mesh.dmx\n"
        );
        let desc = crate::qc::parse_qc_str(&text, &dir.0).unwrap();
        assert_eq!(desc.attachments.len(), 1);
        let output =
            crate::pipeline::build(&desc, &dir.0, crate::pipeline::PipelineOptions::default())
                .unwrap();
        let int = |at| i32::from_le_bytes(output.mdl[at..at + 4].try_into().unwrap());
        assert_eq!(int(240), 1);
        let record = int(244) as usize;
        assert_eq!(int(record + 4), flags);
        let bone = int(record + 8) as usize;
        assert_eq!(output.compiled.desc.bones[bone].name, expected_bone);
        for (axis, expected) in translation.into_iter().enumerate() {
            let at = record + 12 + (axis * 4 + 3) * 4;
            let actual = f32::from_le_bytes(output.mdl[at..at + 4].try_into().unwrap());
            assert!(
                (actual - expected).abs() < 1e-5,
                "rigid={rigid}, override={override_qc:?}, axis={axis}: {actual} != {expected}"
            );
        }
    }
}
