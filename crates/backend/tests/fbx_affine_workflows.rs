use std::{fs, path::Path};

use gmpublished_backend::{
    model_compile::{
        ModelCollision, ModelCompileOutput, ModelCompileRequest, ModelOutputKind, compile_model,
    },
    model_decompile::{load_animation_preview, load_model_lod_preview},
};

const INHERITED: &str =
    include_str!("../../vendor/mdlc/tests/fixtures/fbx_import/inherited-scale.fbx");
const SKIN_BIND: &str = include_str!("../../vendor/mdlc/tests/fixtures/fbx_import/skin-bind.fbx");
const ANIMATED_SKIN: &str =
    include_str!("../../vendor/mdlc/tests/fixtures/fbx_import/animated-skin-bind.fbx");
const ANIMATED_SCALE: &str =
    include_str!("../../vendor/mdlc/tests/fixtures/fbx_import/animated-scale.fbx");

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

fn compile(directory: &Path, source: &str, static_prop: bool) -> ModelCompileOutput {
    let path = directory.join("source.fbx");
    fs::write(&path, source).unwrap();
    let mut request = ModelCompileRequest::new(path, directory.join("output"));
    request.static_prop = static_prop;
    request.export_materials = false;
    request.collision = ModelCollision::None;
    compile_model(&request).unwrap()
}

fn assert_points(actual: impl IntoIterator<Item = [f32; 3]>, expected: &[[f32; 3]]) {
    assert_points_with_tolerance(actual, expected, 0.002);
}

fn assert_points_with_tolerance(
    actual: impl IntoIterator<Item = [f32; 3]>,
    expected: &[[f32; 3]],
    tolerance: f32,
) {
    let actual = actual.into_iter().collect::<Vec<_>>();
    for expected in expected {
        assert!(
            actual.iter().any(|point| {
                point
                    .iter()
                    .zip(expected)
                    .all(|(a, b)| (a - b).abs() < tolerance)
            }),
            "expected {expected:?} in {actual:?}"
        );
    }
    for point in actual {
        assert!(
            expected.iter().any(|expected| {
                point
                    .iter()
                    .zip(expected)
                    .all(|(a, b)| (a - b).abs() < tolerance)
            }),
            "unexpected {point:?}, expected {expected:?}"
        );
    }
}

#[test]
fn inherited_idle_keeps_native_geometry_through_compile_and_playback() {
    let root = tempfile::tempdir().unwrap();
    let output = compile(root.path(), INHERITED, false);
    let mdl = bytes(&output, ModelOutputKind::Mdl);
    let vvd = bytes(&output, ModelOutputKind::Vvd);
    let vtx = bytes(&output, ModelOutputKind::Vtx);
    let preview = load_model_lod_preview(&mdl, &vvd, &vtx).unwrap();
    let expected = [
        [std::f32::consts::SQRT_2, std::f32::consts::SQRT_2, 0.0],
        [
            std::f32::consts::SQRT_2 * 2.0,
            std::f32::consts::SQRT_2 * 2.0,
            0.0,
        ],
        [
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2 * 3.0,
            0.0,
        ],
    ];
    assert_points(
        preview.levels[0]
            .meshes
            .iter()
            .flat_map(|mesh| mesh.vertices.iter().map(|vertex| vertex.position)),
        &expected,
    );
    let animation = load_animation_preview(&mdl, &vvd, &vtx, None).unwrap();
    for cycle in [0.0, 0.5, 1.0] {
        let posed = animation.evaluate(0, cycle).unwrap();
        assert_points(
            posed.iter().flatten().map(|vertex| {
                // Compiled animation uses Source's canonical +90 degree Z basis.
                let [x, y, z] = vertex.position;
                [y, -x, z]
            }),
            &expected,
        );
    }
}

#[test]
fn skin_bind_geometry_survives_static_and_skeletal_export() {
    for static_prop in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let output = compile(root.path(), SKIN_BIND, static_prop);
        let preview = load_model_lod_preview(
            &bytes(&output, ModelOutputKind::Mdl),
            &bytes(&output, ModelOutputKind::Vvd),
            &bytes(&output, ModelOutputKind::Vtx),
        )
        .unwrap();
        let positions = preview.levels[0].meshes.iter().flat_map(|mesh| {
            mesh.vertices.iter().map(|vertex| {
                let [x, y, z] = vertex.position;
                if static_prop { [y, -x, z] } else { [x, y, z] }
            })
        });
        assert_points(
            positions,
            &[[5.0, 0.0, 0.0], [6.0, 0.0, 0.0], [5.0, 1.0, 0.0]],
        );
    }
}

#[test]
fn skin_bind_offsets_follow_native_bone_animation_after_roundtrip() {
    let root = tempfile::tempdir().unwrap();
    let output = compile(root.path(), ANIMATED_SKIN, false);
    let animation = load_animation_preview(
        &bytes(&output, ModelOutputKind::Mdl),
        &bytes(&output, ModelOutputKind::Vvd),
        &bytes(&output, ModelOutputKind::Vtx),
        None,
    )
    .unwrap();
    for (cycle, translation) in [(0.0, 0.0), (1.0, 3.0)] {
        let posed = animation.evaluate(0, cycle).unwrap();
        assert_points_with_tolerance(
            posed.iter().flatten().map(|vertex| {
                // Compiled animation uses Source's canonical +90 degree Z basis.
                let [x, y, z] = vertex.position;
                [y, -x, z]
            }),
            &[
                [5.0 + translation, 0.0, 0.0],
                [6.0 + translation, 0.0, 0.0],
                [5.0 + translation, 1.0, 0.0],
            ],
            // Source position channels truncate within one 128/32767 unit step.
            128.0 / 32767.0 + 0.0001,
        );
    }
}

#[test]
fn compile_reports_unrepresentable_scale_animation() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.fbx");
    fs::write(&source, ANIMATED_SCALE).unwrap();
    let mut request = ModelCompileRequest::new(source, root.path().join("output"));
    request.export_materials = false;
    request.collision = ModelCollision::None;
    let error = compile_model(&request).unwrap_err().to_string();
    assert!(error.to_ascii_lowercase().contains("scale"), "{error}");
}
