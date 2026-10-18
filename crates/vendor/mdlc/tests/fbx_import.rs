use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use mdlc::{
    bone_math,
    fbx::{self, FbxOpts, ForcedAxis},
    smd::Smd,
};

struct Source(PathBuf);

impl Source {
    fn new(contents: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "mdlc-fbx-import-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("source.fbx");
        fs::write(&path, contents).unwrap();
        Self(path)
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().unwrap());
    }
}

const INHERITED: &str = include_str!("fixtures/fbx_import/inherited-scale.fbx");
const SKIN_BIND: &str = include_str!("fixtures/fbx_import/skin-bind.fbx");
const ANIMATED_SCALE: &str = include_str!("fixtures/fbx_import/animated-scale.fbx");

fn options() -> FbxOpts {
    FbxOpts {
        axis: Some(ForcedAxis::Auto),
        ..Default::default()
    }
}

fn world_poses(source: &Smd, frame: usize) -> Vec<bone_math::Matrix3x4> {
    let poses = &source.frames[frame].poses;
    bone_math::compute_world(
        &poses.iter().map(|pose| pose.position).collect::<Vec<_>>(),
        &poses.iter().map(|pose| pose.rotation).collect::<Vec<_>>(),
        &source
            .nodes
            .iter()
            .map(|node| node.parent)
            .collect::<Vec<_>>(),
    )
}

fn assert_point(actual: [f32; 3], expected: [f64; 3]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!(
            (f64::from(actual) - expected).abs() < 0.0001,
            "{actual} != {expected}"
        );
    }
}

#[test]
fn idle_animation_keeps_evaluated_inheritance_and_bind_origins() {
    for scale in [1.0, 0.5] {
        let source = Source::new(INHERITED);
        let options = FbxOpts { scale, ..options() };
        let geometry = fbx::read(&source.0, "inherited scale", &options).unwrap();
        let animation = fbx::read_frames(&source.0, "idle", None, 1.0, &options).unwrap();
        assert_eq!(animation.nodes, geometry.smd.nodes);
        let scene = fbx::load_scene(&source.0, "expected world transforms").unwrap();
        for frame in 0..animation.frames.len() {
            let evaluated = ufbx::evaluate_scene(
                &scene,
                &scene.anim_stacks[0].anim,
                frame as f64,
                Default::default(),
            )
            .unwrap();
            let worlds = world_poses(&animation, frame);
            let bind = world_poses(&geometry.smd, 0);
            for (index, node) in animation.nodes.iter().enumerate() {
                let expected = evaluated
                    .nodes
                    .iter()
                    .find(|source| source.element.name.as_ref() == node.name)
                    .unwrap()
                    .node_to_world;
                let expected = [expected.m03, expected.m13, expected.m23]
                    .map(|value| value * f64::from(scale));
                assert_point(
                    [worlds[index][3], worlds[index][7], worlds[index][11]],
                    expected,
                );
                assert_point([bind[index][3], bind[index][7], bind[index][11]], expected);
            }
        }
    }
}

#[test]
fn skinned_geometry_uses_cluster_bind_space_instead_of_current_mesh_transform() {
    let source = Source::new(SKIN_BIND);
    let geometry = fbx::read(&source.0, "skin bind", &options()).unwrap();
    let scene = fbx::load_scene(&source.0, "expected bind geometry").unwrap();
    let mesh = &scene.meshes[0];
    let cluster = &mesh.skin_deformers[0].clusters[0];
    let bind = ufbx::matrix_mul(&cluster.bind_to_world, &cluster.geometry_to_bone);
    assert_eq!(bind.m03, 5.0);
    assert_eq!(
        scene
            .nodes
            .iter()
            .find(|node| node.mesh.is_some())
            .unwrap()
            .geometry_to_world
            .m03,
        20.0
    );
    for (vertex, local) in geometry.smd.triangles[0].vertices.iter().zip([
        ufbx::Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
        ufbx::Vec3 {
            x: 1.0,
            y: 0.0,
            z: 0.0,
        },
        ufbx::Vec3 {
            x: 0.0,
            y: 1.0,
            z: 0.0,
        },
    ]) {
        let expected = ufbx::transform_position(&bind, local);
        assert_point(vertex.position, [expected.x, expected.y, expected.z]);
    }
}

#[test]
fn changing_scale_is_rejected_instead_of_becoming_a_motionless_animation() {
    let source = Source::new(ANIMATED_SCALE);
    let error = fbx::read_frames(&source.0, "animated scale", None, 1.0, &options())
        .unwrap_err()
        .to_string();
    assert!(error.to_ascii_lowercase().contains("scale"), "{error}");
    assert!(error.contains("parent"), "{error}");
}

#[test]
fn constant_scale_animation_channels_remain_supported() {
    let source = Source::new(&ANIMATED_SCALE.replace(
        "KeyValueFloat: *2 { a: 1,2 }",
        "KeyValueFloat: *2 { a: 1,1 }",
    ));
    let animation = fbx::read_frames(&source.0, "constant scale", None, 1.0, &options()).unwrap();
    assert_eq!(
        animation.frames[0].poses,
        animation.frames.last().unwrap().poses
    );
}

#[test]
fn legacy_explicit_axis_keeps_existing_mesh_transform_behavior() {
    let source = Source::new(SKIN_BIND);
    let geometry = fbx::read(
        &source.0,
        "legacy",
        &FbxOpts {
            axis: Some(ForcedAxis::Z),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        geometry.smd.triangles[0].vertices[0].position,
        [20.0, 0.0, 0.0]
    );
}

#[test]
fn missing_normals_are_generated_from_geometry() {
    let source = Source::new(&SKIN_BIND.replace(
        "Vertices: *9 { a: 0,0,0,1,0,0,0,1,0 }",
        "Vertices: *9 { a: 0,0,0,0,1,0,0,0,1 }",
    ));
    let geometry = fbx::read(&source.0, "missing normals", &options()).unwrap();
    for vertex in &geometry.smd.triangles[0].vertices {
        assert_point(vertex.normal, [1.0, 0.0, 0.0]);
    }
}

#[test]
fn changing_inherited_shear_is_rejected_instead_of_distorting_the_rig() {
    let source = Source::new(include_str!("fixtures/fbx_import/animated-shear.fbx"));
    let error = fbx::read_frames(&source.0, "animated shear", None, 2.0, &options())
        .unwrap_err()
        .to_string();
    assert!(
        error.to_ascii_lowercase().contains("shear")
            || error.to_ascii_lowercase().contains("scale"),
        "{error}"
    );
    assert!(error.contains("rotated"), "{error}");
}

#[test]
fn transient_scale_and_shear_between_output_frames_are_rejected() {
    for (source, values) in [
        (ANIMATED_SCALE, ("1,2", "1,2,1")),
        (
            include_str!("fixtures/fbx_import/animated-shear.fbx"),
            ("45,90", "45,90,45"),
        ),
    ] {
        let source = Source::new(
            &source
                .replace(
                    "KeyTime: *2 { a: 0,46186158000 }",
                    "KeyTime: *3 { a: 0,23093079000,46186158000 }",
                )
                .replace(
                    &format!("KeyValueFloat: *2 {{ a: {} }}", values.0),
                    &format!("KeyValueFloat: *3 {{ a: {} }}", values.1),
                )
                .replace(
                    "KeyAttrRefCount: *1 { a: 2 }",
                    "KeyAttrRefCount: *1 { a: 3 }",
                ),
        );
        let error = fbx::read_frames(&source.0, "transient deformation", None, 1.0, &options())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("scale") && error.contains("shear"),
            "{error}"
        );
        assert!(error.contains("Bake"), "{error}");
    }
}
