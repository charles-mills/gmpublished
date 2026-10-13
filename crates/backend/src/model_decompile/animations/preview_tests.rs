use super::*;
use std::{fmt::Write as _, fs};

fn fixture(options: &str) -> mdlc::pipeline::PipelineOutput {
    let project = tempfile::tempdir().unwrap();
    let reference = "version 1\nnodes\n0 \"root\" -1\n1 \"child\" 0\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 1 0 0 0 0 0\nend\ntriangles\nmaterial\n1 1 0 0 0 0 1 0 0 2 0 .25 1 .75\n1 2 0 0 0 0 1 1 0 1 1 1\n1 1 1 0 0 0 1 0 1 1 1 1\nend\n";
    fs::write(project.path().join("reference.smd"), reference).unwrap();
    fs::write(project.path().join("motion.smd"), "version 1\nnodes\n0 \"root\" -1\n1 \"child\" 0\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 1 0 0 0 0 0\ntime 1\n0 4 0 0 0 0 0\n1 1 0 0 0 0 1.5707963\nend\n").unwrap();
    let desc = mdlc::qc::parse_qc_str(&format!("$modelname models/preview.mdl\n$body body reference.smd\n$sequence move motion.smd fps 2 noautoik {options}\n"), project.path()).unwrap();
    mdlc::pipeline::build(
        &desc,
        project.path(),
        mdlc::pipeline::PipelineOptions::default(),
    )
    .unwrap()
}

#[test]
fn absolute_playback_preserves_bind_weights_source_axes_and_interpolated_rotation() {
    let output = fixture("");
    let preview = load_animation_preview(&output.mdl, &output.vvd, &output.vtx, None).unwrap();
    assert!(
        preview.sequences[0].unavailable.is_none(),
        "{:?}",
        preview.sequences
    );
    assert_eq!(preview.sequences[0].duration, 0.5);
    let first = preview.evaluate(0, 0.0).unwrap();
    let middle = preview.evaluate(0, 0.5).unwrap();
    let last = preview.evaluate(0, 1.0).unwrap();
    let vertex = first[0]
        .iter()
        .position(|vertex| vertex.position[1] > 1.9)
        .unwrap();
    let start = first[0][vertex].position;
    let mid = middle[0][vertex].position;
    let end = last[0][vertex].position;
    assert!(
        start[0].abs() < 0.005 && (start[1] - 2.0).abs() < 0.005,
        "{start:?}"
    );
    assert!(
        (mid[0] + std::f32::consts::FRAC_1_SQRT_2).abs() < 0.005
            && (mid[1] - 3.7071068).abs() < 0.005,
        "{mid:?}"
    );
    assert!(
        (end[0] + 1.0).abs() < 0.005 && (end[1] - 5.0).abs() < 0.005,
        "{end:?}"
    );
    assert!(preview.evaluate(0, f32::NAN).is_none());
    assert!(preview.evaluate(99, 0.0).is_none());
}

#[test]
fn blend_delta_ik_and_malformed_companions_do_not_render_incorrect_poses() {
    let output = fixture("");
    let record = Reader(&output.mdl).table(188, 192, 212).unwrap()[0];
    for (offset, value) in [(56, 2i32), (12, 4), (164, 1)] {
        let mut raw = output.mdl.clone();
        raw[record + offset..record + offset + 4].copy_from_slice(&value.to_le_bytes());
        let preview = load_animation_preview(&raw, &output.vvd, &output.vtx, None).unwrap();
        assert!(preview.sequences[0].unavailable.is_some());
        assert!(preview.evaluate(0, 0.5).is_none());
    }
    let mut slow = output.mdl.clone();
    let animation = Reader(&slow).table(180, 184, 100).unwrap()[0];
    slow[animation + 8..animation + 12].copy_from_slice(&1e-40f32.to_le_bytes());
    let preview = load_animation_preview(&slow, &output.vvd, &output.vtx, None).unwrap();
    assert!(preview.sequences[0].unavailable.is_some());
    let mut vvd = output.vvd.clone();
    vvd[8..12].copy_from_slice(&123u32.to_le_bytes());
    assert!(load_animation_preview(&output.mdl, &vvd, &output.vtx, None).is_err());
    assert!(load_animation_preview(&[], &output.vvd, &output.vtx, None).is_err());
}

#[test]
fn quaternion_frame_interpolation_aligns_shortest_arc_and_uses_normalized_linear_blend() {
    let a = bone_math::angle_quaternion([0.0, 0.0, 170f32.to_radians()]);
    let b = bone_math::angle_quaternion([0.0, 0.0, -170f32.to_radians()]);
    let middle = blend(a, b, 0.5);
    assert!(middle[2].abs() > 0.9999 && middle[3].abs() < 0.0001);
    assert_eq!(
        blend(a, a.map(|value| -value), 0.25),
        bone_math::quaternion_normalize(a)
    );
}

#[test]
fn inline_external_and_sectioned_preview_preserve_weighted_pose_at_every_sample() {
    let mut expected = None;
    for (external, sectioned) in [(false, false), (true, false), (false, true), (true, true)] {
        let project = super::super::tests::fixture(external, sectioned, false);
        let stem = project.path().join("models/tests/animated");
        let raw = fs::read(stem.with_extension("mdl")).unwrap();
        let vvd = fs::read(stem.with_extension("vvd")).unwrap();
        let vtx = fs::read(stem.with_extension("dx90.vtx")).unwrap();
        let ani = fs::read(stem.with_extension("ani")).ok();
        let preview = load_animation_preview(&raw, &vvd, &vtx, ani.as_deref()).unwrap();
        assert!(
            preview.sequences[0].unavailable.is_none(),
            "{:?}",
            preview.sequences
        );
        assert_eq!(preview.sequences[0].weights, [0.5, 0.75, 1.0]);
        let poses = (0..=16)
            .map(|sample| preview.evaluate(0, sample as f32 / 16.0).unwrap())
            .collect::<Vec<_>>();
        if let Some(expected) = &expected {
            let expected: &Vec<Vec<Vec<AnimatedVertex>>> = expected;
            assert_eq!(expected.len(), poses.len());
            for (expected, actual) in expected.iter().zip(&poses) {
                assert_eq!(expected.len(), actual.len());
                for (expected, actual) in expected.iter().zip(actual) {
                    assert_eq!(expected.len(), actual.len());
                }
            }
            for (expected, actual) in expected
                .iter()
                .flatten()
                .flatten()
                .zip(poses.iter().flatten().flatten())
            {
                for (expected, actual) in expected.position.iter().zip(actual.position.iter()) {
                    assert!(
                        (expected - actual).abs() < 0.025,
                        "{external} {sectioned}: {expected} != {actual}"
                    );
                }
                for (expected, actual) in expected.normal.iter().zip(&actual.normal) {
                    assert!(
                        (expected - actual).abs() < 0.001,
                        "normal: {expected} != {actual}"
                    );
                }
            }
        } else {
            expected = Some(poses);
        }
        if external {
            let missing = load_animation_preview(&raw, &vvd, &vtx, None).unwrap();
            assert!(missing.sequences[0].unavailable.is_some());
            let mut wrong = ani.unwrap();
            wrong[8..12].copy_from_slice(&123u32.to_le_bytes());
            assert!(load_animation_preview(&raw, &vvd, &vtx, Some(&wrong)).is_err());
        }
    }
}

#[test]
fn fractional_weightlists_blend_against_bind_pose_and_zero_weights_freeze_bones() {
    let mut output = fixture("");
    let record = Reader(&output.mdl).table(188, 192, 212).unwrap()[0];
    let weights = Reader(&output.mdl).relative(record, record + 156).unwrap();
    let baseline = load_animation_preview(&output.mdl, &output.vvd, &output.vtx, None).unwrap();
    let vertex = baseline.evaluate(0, 0.0).unwrap()[0]
        .iter()
        .position(|vertex| vertex.position[1] > 1.9)
        .unwrap();
    for weight in [0.0f32, 0.5] {
        for bone in 0..2 {
            output.mdl[weights + bone * 4..weights + bone * 4 + 4]
                .copy_from_slice(&weight.to_le_bytes());
        }
        let preview = load_animation_preview(&output.mdl, &output.vvd, &output.vtx, None).unwrap();
        let first = preview.evaluate(0, 0.0).unwrap();
        let last = preview.evaluate(0, 1.0).unwrap();
        let actual = last[0][vertex].position;
        if weight == 0.0 {
            assert_eq!(first, last);
        } else {
            assert!(
                (actual[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.005
                    && (actual[1] - 3.7071068).abs() < 0.005,
                "{actual:?}"
            );
        }
    }
    let q = blend(
        [0.0, 0.0, 0.0, 1.0],
        bone_math::angle_quaternion([0.0, 0.0, std::f32::consts::FRAC_PI_2]),
        0.25,
    );
    assert!((bone_math::quaternion_angles(q)[2] - 0.376959).abs() < 0.00001);
}

fn blend_fixture(grid: [usize; 2], shifts: &[f32]) -> mdlc::pipeline::PipelineOutput {
    blend_rotation_fixture(grid, shifts, &vec![[0.0; 3]; shifts.len()])
}

fn blend_rotation_fixture(
    grid: [usize; 2],
    shifts: &[f32],
    rotations: &[[f32; 3]],
) -> mdlc::pipeline::PipelineOutput {
    let project = tempfile::tempdir().unwrap();
    let reference = "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\nmaterial\n0 0 0 0 0 0 1 0 0\n0 1 0 0 0 0 1 1 0\n0 0 1 0 0 0 1 0 1\nend\n";
    fs::write(project.path().join("reference.smd"), reference).unwrap();
    let mut qc = String::from(
        "$modelname models/blended.mdl\n$body body reference.smd\n$poseparameter aim -90 90\n$poseparameter height 0 1\n",
    );
    for (index, shift) in shifts.iter().enumerate() {
        let [rx, ry, rz] = rotations[index];
        fs::write(project.path().join(format!("motion{index}.smd")), format!("version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 {shift} 0 0 {rx} {ry} {rz}\ntime 1\n0 {} 0 0 {rx} {ry} {rz}\nend\n", shift+4.0)).unwrap();
        writeln!(
            qc,
            "$animation a{index} motion{index}.smd noautoik fps {}",
            2 * (index + 1)
        )
        .unwrap();
    }
    qc.push_str("$sequence blend { ");
    for index in 0..shifts.len() {
        write!(qc, "a{index} ").unwrap();
    }
    write!(qc, "blendwidth {} blend aim -90 90 ", grid[0]).unwrap();
    if grid[1] > 1 {
        qc.push_str("blend height 0 1 ");
    }
    qc.push_str("}\n");
    let desc = mdlc::qc::parse_qc_str(&qc, project.path()).unwrap();
    mdlc::pipeline::build(
        &desc,
        project.path(),
        mdlc::pipeline::PipelineOptions::default(),
    )
    .unwrap()
}

#[test]
fn binary_blend_grid_rotations_follow_source_sequential_quaternion_blending() {
    let half_pi = std::f32::consts::FRAC_PI_2;
    let output = blend_rotation_fixture(
        [2, 2],
        &[0.0; 4],
        &[
            [half_pi, 0.0, 0.0],
            [0.0; 3],
            [0.0, half_pi, 0.0],
            [0.0, 0.0, half_pi],
        ],
    );
    let preview = load_animation_preview(&output.mdl, &output.vvd, &output.vtx, None).unwrap();
    let baseline = preview
        .evaluate_with_parameters(0, 0.0, &[-90.0, 0.0])
        .unwrap();
    let vertex = baseline[0]
        .iter()
        .position(|vertex| vertex.position[1] > 0.99)
        .unwrap();
    let actual = preview
        .evaluate_with_parameters(0, 0.0, &[-45.0, 0.75])
        .unwrap()[0][vertex];
    for (actual, expected) in actual
        .position
        .into_iter()
        .zip([-0.5416353, 0.520316, -0.6602291])
    {
        assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
    }
    for (actual, expected) in actual
        .normal
        .into_iter()
        .zip([0.1365688, 0.8294457, 0.5416353])
    {
        assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
    }
}

#[test]
fn binary_blend_grids_use_source_triangles_and_bilinear_playback_rates() {
    let output = blend_fixture([2, 2], &[0.0, 0.0, 0.0, 100.0]);
    let preview = load_animation_preview(&output.mdl, &output.vvd, &output.vtx, None).unwrap();
    assert!(
        preview.sequences[0].unavailable.is_none(),
        "{:?}",
        preview.sequences
    );
    assert_eq!(preview.sequences[0].parameters, [0, 1]);
    let baseline = preview
        .evaluate_with_parameters(0, 0.0, &[-90.0, 0.0])
        .unwrap();
    let actual = preview
        .evaluate_with_parameters(0, 0.0, &[-45.0, 0.75])
        .unwrap();
    for (baseline, actual) in baseline[0].iter().zip(&actual[0]) {
        assert!((actual.position[1] - baseline.position[1] - 25.0).abs() < 0.01);
    }
    let cps = 2.0 * 0.75 * 0.25 + 4.0 * 0.25 * 0.25 + 6.0 * 0.75 * 0.75 + 8.0 * 0.25 * 0.75;
    assert!((preview.duration(0, &[-45.0, 0.75]).unwrap() - 1.0 / cps).abs() < 0.00001);
    assert!(
        preview
            .evaluate_with_parameters(0, 0.0, &[f32::NAN, 0.0])
            .is_none()
    );
}

#[test]
fn binary_nonlinear_pose_keys_and_looped_parameters_select_correct_cells() {
    let mut output = blend_fixture([3, 1], &[0.0, 10.0, 30.0]);
    let sequence = Reader(&output.mdl).table(188, 192, 212).unwrap()[0];
    let keys = Reader(&output.mdl)
        .relative(sequence, sequence + 160)
        .unwrap();
    output.mdl[keys + 4..keys + 8].copy_from_slice(&(-30.0_f32).to_le_bytes());
    let preview = load_animation_preview(&output.mdl, &output.vvd, &output.vtx, None).unwrap();
    let baseline = preview.evaluate_with_parameters(0, 0.0, &[-90.0]).unwrap();
    let actual = preview.evaluate_with_parameters(0, 0.0, &[30.0]).unwrap();
    for (baseline, actual) in baseline[0].iter().zip(&actual[0]) {
        assert!((actual.position[1] - baseline.position[1] - 20.0).abs() < 0.01);
    }
    let parameter = Reader(&output.mdl).table(300, 304, 20).unwrap()[0];
    output.mdl[parameter + 16..parameter + 20].copy_from_slice(&180.0_f32.to_le_bytes());
    let preview = load_animation_preview(&output.mdl, &output.vvd, &output.vtx, None).unwrap();
    assert_eq!(
        preview.evaluate_with_parameters(0, 0.0, &[120.0]),
        preview.evaluate_with_parameters(0, 0.0, &[-60.0])
    );
    output.mdl[keys + 4..keys + 8].copy_from_slice(&(-90.0_f32).to_le_bytes());
    let invalid = load_animation_preview(&output.mdl, &output.vvd, &output.vtx, None).unwrap();
    assert!(invalid.sequences[0].unavailable.is_some());
    output.mdl[sequence + 76..sequence + 80].copy_from_slice(&(-1_i32).to_le_bytes());
    let inactive = load_animation_preview(&output.mdl, &output.vvd, &output.vtx, None).unwrap();
    assert!(inactive.sequences[0].unavailable.is_none());
    assert!(inactive.sequences[0].parameters.is_empty());
    assert_eq!(
        inactive.evaluate(0, 0.0),
        inactive.evaluate_with_parameters(0, 0.0, &[30.0])
    );
}
