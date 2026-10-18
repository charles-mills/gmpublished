use super::*;
use std::fmt::Write;

fn arch() -> MeshGeometry {
    let outline = [
        [-3.0, 0.0],
        [-2.0, 0.0],
        [-2.0, 4.0],
        [2.0, 4.0],
        [2.0, 0.0],
        [3.0, 0.0],
        [3.0, 5.0],
        [-3.0, 5.0],
    ];
    let points = [-1.0, 1.0]
        .into_iter()
        .flat_map(|y| outline.map(|[x, z]| [x, y, z]))
        .collect();
    let mut faces = Vec::new();
    for face in [
        [0, 1, 2],
        [0, 2, 7],
        [2, 3, 6],
        [2, 6, 7],
        [3, 4, 5],
        [3, 5, 6],
    ] {
        faces.push(face);
        faces.push(face.map(|index| index + 8));
    }
    for i in 0..8 {
        let next = (i + 1) % 8;
        faces.extend([[i, next, next + 8], [i, next + 8, i + 8]]);
    }
    (points, faces)
}

fn smd_text(geometry: &MeshGeometry) -> String {
    let mut text =
        "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\n"
            .to_owned();
    for face in &geometry.1 {
        text.push_str("material\n");
        for index in face {
            let [x, y, z] = geometry.0[*index as usize];
            writeln!(text, "0 {x} {y} {z} 0 0 1 0 0").unwrap();
        }
    }
    text.push_str("end\n");
    text
}

fn write_smd(path: &Path, geometry: &MeshGeometry) {
    std::fs::write(path, smd_text(geometry)).unwrap();
}

fn contains(hull: &PhyHull, point: [f32; 3]) -> bool {
    assert!(
        has_volume(&hull.vertices, &hull.faces),
        "collision hull must have volume"
    );
    let center: [f32; 3] = std::array::from_fn(|axis| {
        hull.vertices.iter().map(|v| v[axis]).sum::<f32>() / hull.vertices.len() as f32
    });
    hull.faces.iter().all(|face| {
        let [a, b, c] = face.map(|index| hull.vertices[index as usize]);
        let u: [f32; 3] = std::array::from_fn(|axis| b[axis] - a[axis]);
        let v: [f32; 3] = std::array::from_fn(|axis| c[axis] - a[axis]);
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let distance = |p: [f32; 3]| {
            (0..3)
                .map(|axis| n[axis] * (p[axis] - a[axis]))
                .sum::<f32>()
        };
        distance(point) * distance(center) >= -1.0e-5
    })
}

#[test]
fn decomposition_preserves_an_arch_opening_that_a_single_hull_blocks() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("arch.smd");
    write_smd(&source, &arch());
    let mut request = ModelCompileRequest::new(source.clone(), root.path().join("output"));
    request.static_prop = true;
    request.export_materials = false;
    request.collision = ModelCollision::Automatic(CollisionDetail::Simple);
    let desc = super::super::load_description(&request, &source, root.path()).unwrap();
    let compiled = mdlc::compile::compile(&desc, root.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let simple = collision_hulls(&compiled, &request, &cancel).unwrap();
    assert!(simple.iter().any(|hull| contains(hull, [0.0, 0.0, 2.0])));
    request.collision = ModelCollision::Automatic(CollisionDetail::Balanced);
    let generated = collision_hulls(&compiled, &request, &cancel).unwrap();
    assert!(generated.len() > 1 && generated.len() <= 16);
    assert!(
        !generated.iter().any(|hull| contains(hull, [0.0, 0.0, 2.0])),
        "opening blocked by {generated:?}"
    );
    for point in [[-2.5, 0.0, 2.0], [2.5, 0.0, 2.0], [0.0, 0.0, 4.5]] {
        assert!(
            generated.iter().any(|hull| contains(hull, point)),
            "collision lost solid region {point:?}"
        );
    }
}

#[test]
fn custom_collision_requires_explicit_repair_for_concave_meshes() {
    let (vertices, faces) = arch();
    assert_eq!(
        validate_convex(&vertices, &faces).unwrap_err(),
        "is concave"
    );
    assert_eq!(
        validate_convex(&vertices, &faces[..faces.len() - 1]).unwrap_err(),
        "is open or non-manifold"
    );
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("collision.smd");
    write_smd(&source, &(vertices, faces));
    let mut request = ModelCompileRequest::new(source.clone(), root.path().join("output"));
    request.static_prop = true;
    request.collision = ModelCollision::Custom {
        source: source.clone(),
        parts: Vec::new(),
        repair: None,
    };
    let desc = super::super::load_description(&request, &source, root.path()).unwrap();
    let compiled = mdlc::compile::compile(&desc, root.path()).unwrap();
    let error = collision_hulls(&compiled, &request, &AtomicBool::new(false))
        .unwrap_err()
        .to_string();
    assert!(error.contains("piece 1 is concave"), "{error}");
    assert!(!request.destination.exists());
    let original = std::fs::read(&source).unwrap();
    let ModelCollision::Custom { repair, .. } = &mut request.collision else {
        unreachable!()
    };
    *repair = Some(CollisionDetail::Balanced);
    let repaired = collision_hulls(&compiled, &request, &AtomicBool::new(false)).unwrap();
    assert!(repaired.len() > 1);
    assert!(!repaired.iter().any(|hull| contains(hull, [0.0, 0.0, 2.0])));
    for point in [[-2.5, 0.0, 2.0], [2.5, 0.0, 2.0], [0.0, 0.0, 4.5]] {
        assert!(repaired.iter().any(|hull| contains(hull, point)));
    }
    assert_eq!(std::fs::read(&source).unwrap(), original);
}

#[test]
fn custom_collision_shares_scale_rotation_and_origin_with_render_geometry() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("cube.smd");
    std::fs::write(
        &source,
        include_bytes!("../../../vendor/mdlc/tests/fixtures/cube.smd"),
    )
    .unwrap();
    let mut request = ModelCompileRequest::new(source.clone(), root.path().join("output"));
    request.static_prop = true;
    request.scale = 2.5;
    request.orientation = [15.0, 35.0, -20.0];
    request.collision = ModelCollision::Custom {
        source: source.clone(),
        parts: Vec::new(),
        repair: None,
    };
    let desc = super::super::load_description(&request, &source, root.path()).unwrap();
    let mut compiled = mdlc::compile::compile(&desc, root.path()).unwrap();
    super::super::apply_orientation(&mut compiled, &source, request.orientation).unwrap();
    let hulls = collision_hulls(&compiled, &request, &AtomicBool::new(false)).unwrap();
    assert_eq!(hulls.len(), 1);
    let (points, _) = mesh_geometry(&compiled, &AtomicBool::new(false)).unwrap();
    for axis in 0..3 {
        for minimum in [true, false] {
            let bound = |points: &[[f32; 3]]| {
                points
                    .iter()
                    .map(|point| point[axis])
                    .reduce(if minimum { f32::min } else { f32::max })
                    .unwrap()
            };
            assert!((bound(&points) - bound(&hulls[0].vertices)).abs() < 0.001);
        }
    }
}

#[test]
fn tagged_touching_collision_objects_stay_separate_and_keep_gltf_units() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("prop.gltf");
    let mut geometry = Vec::new();
    for point in [
        [0.0_f32, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 2.0, 0.0],
        [0.0, 0.0, 3.0],
    ] {
        for value in point {
            geometry.extend_from_slice(&value.to_le_bytes());
        }
    }
    for index in [0_u16, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3] {
        geometry.extend_from_slice(&index.to_le_bytes());
    }
    let document = serde_json::json!({
        "asset":{"version":"2.0"},
        "buffers":[{"uri":format!("data:application/octet-stream;base64,{}",base64::encode(&geometry)),"byteLength":geometry.len()}],
        "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":48},{"buffer":0,"byteOffset":48,"byteLength":24}],
        "accessors":[{"bufferView":0,"componentType":5126,"count":4,"type":"VEC3","min":[0,0,0],"max":[1,2,3]},{"bufferView":1,"componentType":5123,"count":12,"type":"SCALAR"}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0},"indices":1}]}],
        "nodes":[{"name":"visible","mesh":0},{"name":"physics","mesh":0,"translation":[5,0,0],"extras":{"gmpublishedCollision":true}},{"name":"touching","mesh":0,"translation":[6,0,0],"extras":{"sourceCollision":true}}],
        "scenes":[{"nodes":[0]},{"nodes":[1,2]}],"scene":0
    });
    std::fs::write(&source, serde_json::to_vec(&document).unwrap()).unwrap();
    let collision = collision_parts(&source).unwrap();
    assert_eq!(collision.len(), 2);
    let mut request = ModelCompileRequest::new(source.clone(), root.path().join("output"));
    request.static_prop = true;
    request.scale = 2.0;
    request.collision = ModelCollision::Custom {
        source: source.clone(),
        parts: collision,
        repair: None,
    };
    let desc = super::super::load_description(&request, &source, root.path()).unwrap();
    let compiled = mdlc::compile::compile(&desc, root.path()).unwrap();
    assert_eq!(
        compiled.bodyparts[0].models[0]
            .meshes
            .iter()
            .map(|mesh| mesh.triangles.len())
            .sum::<usize>(),
        4
    );
    let hulls = collision_hulls(&compiled, &request, &AtomicBool::new(false)).unwrap();
    assert_eq!(hulls.len(), 2);
    let bounds = |points: &[[f32; 3]]| {
        std::array::from_fn::<_, 3, _>(|axis| {
            [
                points.iter().map(|p| p[axis]).fold(f32::INFINITY, f32::min),
                points
                    .iter()
                    .map(|p| p[axis])
                    .fold(f32::NEG_INFINITY, f32::max),
            ]
        })
    };
    let actual = bounds(&hulls[0].vertices);
    let extents: Vec<_> = actual
        .iter()
        .map(|[minimum, maximum]| maximum - minimum)
        .collect();
    let mut sorted = extents;
    sorted.sort_by(f32::total_cmp);
    for (actual, expected) in sorted.into_iter().zip([2.0, 4.0, 6.0]) {
        assert!((actual * 0.0254 - expected).abs() < 0.001);
    }
    let (visual, _) = mesh_geometry(&compiled, &AtomicBool::new(false)).unwrap();
    assert!(
        actual
            .into_iter()
            .zip(bounds(&visual))
            .any(|(custom, render)| (custom[0] - render[0]).abs() > 9.0 / 0.0254)
    );
    request.collision = ModelCollision::None;
    let desc = super::super::load_description(&request, &source, root.path()).unwrap();
    assert_eq!(desc.bodyparts[0].models[0].src_parts.len(), 1);
}

#[test]
fn touching_smd_convex_pieces_keep_their_authored_smoothing_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("touching.smd");
    let points = vec![
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
    ];
    let faces = vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]];
    let first = smd_text(&(points.clone(), faces.clone()));
    let next = smd_text(&(
        points
            .into_iter()
            .map(|[x, y, z]| [x + 1.0, y, z])
            .collect(),
        faces,
    ))
    .replace(" 0 0 1 0 0\n", " 0 1 0 0 0\n");
    let text = format!(
        "{}{}",
        first.strip_suffix("end\n").unwrap(),
        next.split_once("triangles\n").unwrap().1
    );
    std::fs::write(&source, text).unwrap();
    let mut request = ModelCompileRequest::new(source.clone(), root.path().join("output"));
    request.static_prop = true;
    request.collision = ModelCollision::Custom {
        source: source.clone(),
        parts: Vec::new(),
        repair: None,
    };
    let desc = super::super::load_description(&request, &source, root.path()).unwrap();
    let compiled = mdlc::compile::compile(&desc, root.path()).unwrap();
    let hulls = collision_hulls(&compiled, &request, &AtomicBool::new(false)).unwrap();
    assert_eq!(hulls.len(), 2);
    assert!(!hulls.iter().any(|hull| contains(hull, [0.75, 0.5, 0.0])));
    for point in [[0.1, 0.1, 0.1], [1.1, 0.1, 0.1]] {
        assert!(hulls.iter().any(|hull| contains(hull, point)));
    }
}

#[test]
fn blender_collision_inspection_recovers_mass_and_surface_from_the_original_phy() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("cube.smd");
    std::fs::write(
        &source,
        include_bytes!("../../../vendor/mdlc/tests/fixtures/cube.smd"),
    )
    .unwrap();
    let mut request = ModelCompileRequest::new(source, root.path().join("compiled"));
    request.static_prop = true;
    request.export_materials = false;
    request.collision = ModelCollision::Automatic(CollisionDetail::Simple);
    request.collision_mass = 37.5;
    request.surface_property = Some("wood_solid".into());
    let output = super::super::compile_model(&request).unwrap();
    let model = &output
        .files
        .iter()
        .find(|file| file.kind == super::super::ModelOutputKind::Mdl)
        .unwrap()
        .path;
    let blender =
        crate::model_decompile::export_blender_model(model, &root.path().join("blender")).unwrap();
    let inspection = super::super::inspect_model(&blender.model_path).unwrap();
    assert_eq!(
        inspection.collision_physics,
        Some(CollisionPhysics {
            mass: 37.5,
            surface_property: Some("wood_solid".into())
        })
    );
    assert_eq!(inspection.collision_parts.len(), 1);
    for metadata in [
        serde_json::json!({"sourcePhysicsKeyvalues":"solid { surfaceprop metal } editparams { totalmass NaN }"}),
        serde_json::json!({"sourcePhysics":{"sourcePhysicsKeyvalues":"solid { surfaceprop metal } editparams { totalmass -10 }"}}),
    ] {
        assert!(parse_collision_physics(&metadata.to_string()).is_none());
    }
}
