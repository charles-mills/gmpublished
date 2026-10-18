use std::{fmt::Write as _, fs, path::Path};

use gmpublished_backend::{
    model_compile::{ModelCompileOutput, ModelCompileRequest, ModelOutputKind, compile_model},
    model_decompile::load_model_lod_preview,
};
use mdlc::fbx::{FbxOpts, ForcedAxis};

const EPSILON: f32 = 0.002;

fn scene(directory: &Path) -> std::path::PathBuf {
    let source = directory.join("transforms.fbx");
    let mut text = String::from(
        r#"; FBX 7.4.0 project file
FBXHeaderExtension: { FBXHeaderVersion: 1003
    FBXVersion: 7400
}
GlobalSettings: {
    Version: 1000
    Properties70: {
        P: "UpAxis", "int", "Integer", "", 2
        P: "UpAxisSign", "int", "Integer", "", 1
        P: "FrontAxis", "int", "Integer", "", 1
        P: "FrontAxisSign", "int", "Integer", "", -1
        P: "CoordAxis", "int", "Integer", "", 0
        P: "CoordAxisSign", "int", "Integer", "", 1
        P: "UnitScaleFactor", "double", "Number", "", 2.54
    }
}
Objects: {
    Model: 900, "Model::parent", "Null" {
        Version: 232
        Properties70: {
            P: "Lcl Translation", "Lcl Translation", "", "A", 10,20,30
            P: "Lcl Rotation", "Lcl Rotation", "", "A", 0,0,90
            P: "Lcl Scaling", "Lcl Scaling", "", "A", 2,3,4
        }
    }
"#,
    );
    for (index, name, translation, scale) in [
        (0, "large", "3,4,5", "5,1,0.5"),
        (1, "small", "-1,2,0", "0.5,2,3"),
        (2, "mirrored", "3,4,5", "-5,1,0.5"),
    ] {
        let geometry = 100 + index;
        let node = 200 + index;
        let material = 500 + index;
        write!(text,
            r#"    Geometry: {geometry}, "Geometry::{name}", "Mesh" {{
        Vertices: *9 {{ a: 0,0,0,2,0,1,0,2,1 }}
        PolygonVertexIndex: *3 {{ a: 0,1,-3 }}
        LayerElementNormal: 0 {{
            Version: 101
            MappingInformationType: "ByPolygonVertex"
            ReferenceInformationType: "Direct"
            Normals: *9 {{ a: -0.408248290463863,-0.408248290463863,0.816496580927726,-0.408248290463863,-0.408248290463863,0.816496580927726,-0.408248290463863,-0.408248290463863,0.816496580927726 }}
        }}
        LayerElementUV: 0 {{
            Version: 101
            MappingInformationType: "ByPolygonVertex"
            ReferenceInformationType: "Direct"
            UV: *6 {{ a: 0,0,1,0,0,1 }}
        }}
        LayerElementMaterial: 0 {{
            Version: 101
            MappingInformationType: "AllSame"
            ReferenceInformationType: "IndexToDirect"
            Materials: *1 {{ a: 0 }}
        }}
        Layer: 0 {{
            Version: 100
            LayerElement: {{ Type: "LayerElementNormal"
                TypedIndex: 0
            }}
            LayerElement: {{ Type: "LayerElementUV"
                TypedIndex: 0
            }}
            LayerElement: {{ Type: "LayerElementMaterial"
                TypedIndex: 0
            }}
        }}
    }}
    Model: {node}, "Model::{name}", "Mesh" {{
        Version: 232
        Properties70: {{
            P: "Lcl Translation", "Lcl Translation", "", "A", {translation}
            P: "Lcl Rotation", "Lcl Rotation", "", "A", 0,0,0
            P: "Lcl Scaling", "Lcl Scaling", "", "A", {scale}
        }}
    }}
    Material: {material}, "Material::{name}", "" {{
        Version: 102
        ShadingModel: "lambert"
    }}
"#
        ).unwrap();
    }
    text.push_str("}\nConnections: {\n    C: \"OO\", 900, 0\n");
    for index in 0..3 {
        write!(
            text,
            "    C: \"OO\", {}, {}\n    C: \"OO\", {}, 900\n    C: \"OO\", {}, {}\n",
            100 + index,
            200 + index,
            200 + index,
            500 + index,
            200 + index
        )
        .unwrap();
    }
    text.push_str("}\n");
    fs::write(&source, text).unwrap();
    source
}

fn bounds(points: impl IntoIterator<Item = [f32; 3]>) -> ([f32; 3], [f32; 3]) {
    let mut minimum = [f32::INFINITY; 3];
    let mut maximum = [f32::NEG_INFINITY; 3];
    for point in points {
        for axis in 0..3 {
            minimum[axis] = minimum[axis].min(point[axis]);
            maximum[axis] = maximum[axis].max(point[axis]);
        }
    }
    (minimum, maximum)
}

fn assert_vector(actual: [f32; 3], expected: [f32; 3]) {
    for axis in 0..3 {
        assert!(
            (actual[axis] - expected[axis]).abs() < EPSILON,
            "expected {expected:?}, got {actual:?}"
        );
    }
}

fn assert_bounds(actual: ([f32; 3], [f32; 3]), expected: ([f32; 3], [f32; 3])) {
    assert_vector(actual.0, expected.0);
    assert_vector(actual.1, expected.1);
}

fn subtract(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|axis| a[axis] - b[axis])
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[test]
fn automatic_axes_preserve_parent_transforms_and_each_mesh_scale() {
    let root = tempfile::tempdir().unwrap();
    let source = scene(root.path());
    let model = mdlc::fbx::read(
        &source,
        "transform regression",
        &FbxOpts {
            axis: Some(ForcedAxis::Auto),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(model.smd.triangles.len(), 3);
    // Parent rotation maps the scaled X axis onto +Y and the scaled Y axis onto -X.
    for (material, minimum, maximum) in [
        ("large", [-8.0, 26.0, 50.0], [-2.0, 46.0, 52.0]),
        ("small", [-8.0, 18.0, 30.0], [4.0, 20.0, 42.0]),
        ("mirrored", [-8.0, 6.0, 50.0], [-2.0, 26.0, 52.0]),
    ] {
        let triangle = model
            .smd
            .triangles
            .iter()
            .find(|triangle| triangle.material == material)
            .unwrap();
        assert_bounds(
            bounds(triangle.vertices.iter().map(|vertex| vertex.position)),
            (minimum, maximum),
        );
    }
    // Explicit legacy mode retains its existing normalization of authored node scale.
    let legacy = mdlc::fbx::read(&source, "legacy regression", &FbxOpts::default()).unwrap();
    for triangle in legacy.smd.triangles {
        let (minimum, maximum) = bounds(triangle.vertices.iter().map(|vertex| vertex.position));
        assert_vector(subtract(maximum, minimum), [2.0, 2.0, 1.0]);
    }
}

#[test]
fn automatic_axes_preserve_front_faces_and_normals_under_nonuniform_and_mirrored_scale() {
    let root = tempfile::tempdir().unwrap();
    let source = scene(root.path());
    let model = mdlc::fbx::read(
        &source,
        "normal regression",
        &FbxOpts {
            axis: Some(ForcedAxis::Auto),
            ..Default::default()
        },
    )
    .unwrap();
    for triangle in model.smd.triangles {
        let vertices = &triangle.vertices;
        let edge_a = subtract(vertices[1].position, vertices[0].position);
        let edge_b = subtract(vertices[2].position, vertices[0].position);
        let area = cross(edge_a, edge_b);
        for vertex in vertices {
            assert!((dot(vertex.normal, vertex.normal) - 1.0).abs() < EPSILON);
            assert!(dot(vertex.normal, edge_a).abs() < EPSILON);
            assert!(dot(vertex.normal, edge_b).abs() < EPSILON);
            assert!(dot(vertex.normal, area) > 0.0, "{}", triangle.material);
        }
    }
}

fn model_bytes(output: &ModelCompileOutput, kind: ModelOutputKind) -> Vec<u8> {
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

#[test]
fn direct_fbx_static_and_rigid_skeletal_previews_keep_the_same_scaled_scene() {
    let root = tempfile::tempdir().unwrap();
    let source = scene(root.path());
    let mut previews = Vec::new();
    for static_prop in [true, false] {
        let mut request = ModelCompileRequest::new(
            source.clone(),
            root.path()
                .join(if static_prop { "static" } else { "skeletal" }),
        );
        request.static_prop = static_prop;
        request.export_materials = false;
        let output = compile_model(&request).unwrap();
        let preview = load_model_lod_preview(
            &model_bytes(&output, ModelOutputKind::Mdl),
            &model_bytes(&output, ModelOutputKind::Vvd),
            &model_bytes(&output, ModelOutputKind::Vtx),
        )
        .unwrap();
        let model = &preview.levels[0];
        assert_eq!(model.triangle_count, 3);
        let mut material_bounds = std::collections::BTreeMap::new();
        let mut front_face_sign = None;
        for mesh in &model.meshes {
            for indices in mesh.indices.chunks_exact(3) {
                let vertices = [indices[0], indices[1], indices[2]]
                    .map(|index| &mesh.vertices[index as usize]);
                let edge_a = subtract(vertices[1].position, vertices[0].position);
                let edge_b = subtract(vertices[2].position, vertices[0].position);
                let area = cross(edge_a, edge_b);
                for vertex in vertices {
                    assert!(dot(vertex.normal, edge_a).abs() < EPSILON);
                    assert!(dot(vertex.normal, edge_b).abs() < EPSILON);
                    let facing = dot(vertex.normal, area);
                    assert!(facing.abs() > EPSILON);
                    // Source's index convention must be consistent for every mesh,
                    // including the instance with a negative determinant.
                    let expected = front_face_sign.get_or_insert_with(|| facing.signum());
                    assert_eq!(facing.signum(), *expected);
                }
            }
            material_bounds.insert(
                model.material_names[mesh.material_index].clone(),
                bounds(mesh.vertices.iter().map(|vertex| {
                    let [x, y, z] = vertex.position;
                    // Static props bake the compiler's +90 degree Z rotation;
                    // rigid skeletal preview assembly restores the reference pose.
                    if static_prop { [y, -x, z] } else { [x, y, z] }
                })),
            );
        }
        // Both paths preserve the authored layout after the static-prop basis conversion.
        for (material, minimum, maximum) in [
            ("large", [-8.0, 26.0, 50.0], [-2.0, 46.0, 52.0]),
            ("small", [-8.0, 18.0, 30.0], [4.0, 20.0, 42.0]),
            ("mirrored", [-8.0, 6.0, 50.0], [-2.0, 26.0, 52.0]),
        ] {
            assert_bounds(material_bounds[material], (minimum, maximum));
        }
        previews.push(material_bounds);
    }
    for (name, static_bounds) in &previews[0] {
        assert_bounds(previews[1][name], *static_bounds);
    }
}
