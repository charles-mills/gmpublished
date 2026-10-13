use std::{fmt::Write as _, fs};

use mdlc::{pipeline, qc};

use super::*;

pub(super) fn fixture(external: bool, sectioned: bool, delta: bool) -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("rig.smd"),
        include_str!("../../../../vendor/mdlc/tests/fixtures/rig.smd")
            .replace("0 0 0 0 0 0 0\n", "0 3 7 2 0.1 -0.2 0.3\n"),
    )
    .unwrap();
    let mut frames =
        String::from("version 1\nnodes\n0 \"root\" -1\n1 \"mid\" 0\n2 \"tip\" 1\nend\nskeleton\n");
    for frame in 0..9 {
        let t = frame as f32;
        writeln!(
            frames,
            "time {frame}\n0 {} {} {} {} {} {}\n1 0 0 10 {} 0 0\n2 0 {} 10 0 {} 0",
            3.0 + t * 0.3,
            7.0 - t * 0.4,
            2.0 + t * 0.2,
            0.1 + t * 0.03,
            -0.2 + t * 0.02,
            0.3 + t * 0.05,
            t * 0.06,
            t * 0.15,
            t * 0.04
        )
        .unwrap();
    }
    frames.push_str("end\n");
    fs::write(project.path().join("walk.smd"), frames).unwrap();
    let delta_options = if delta { "subtract base 0" } else { "" };
    let sections = if sectioned { "sectionframes 3 3" } else { "" };
    let blocks = if external { "$animblocksize 1\n" } else { "" };
    let source = format!(
        "$modelname models/tests/animated.mdl\n{blocks}$body body rig.smd\n$weightlist torso {{ root 0.5 mid 0.75 tip 1 }}\n$animation base rig.smd noautoik\n$animation walk walk.smd fps 24 noautoik {delta_options}\n$sequence walking {{ walk {sections} weightlist torso activity ACT_WALK 3 fadein 0.15 fadeout 0.35 snap hidden event 1004 4 \"step\" event AE_CL_PLAYSOUND 8 \"footstep\" }}\n"
    );
    let path = project.path().join("source.qc");
    fs::write(&path, source).unwrap();
    let desc = qc::parse_qc_file(&path).unwrap();
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&built, project.path()).unwrap();
    project
}

fn parsed(raw: &[u8]) -> Mdl {
    let mut geometry = raw.to_vec();
    geometry[180..184].fill(0);
    vformats::mdl::parse_mdl(&geometry, &vformats::Limits::default()).unwrap()
}

fn first_animation(raw: &[u8], ani: Option<&[u8]>) -> Animation {
    let model = parsed(raw);
    let reader = Reader(raw);
    let sequence = reader.table(188, 192, 212).unwrap()[0];
    let blend = reader.relative(sequence, sequence + 60).unwrap();
    let animation = reader.i16(blend).unwrap() as usize;
    let record = reader.table(180, 184, 100).unwrap()[animation];
    decode::animation(&reader, ani, &model, record, &AtomicBool::new(false)).unwrap()
}

fn assert_pose(left: &SmdPose, right: &SmdPose, context: &str) {
    assert_eq!(left.bone, right.bone);
    for axis in 0..3 {
        assert!(
            (left.position[axis] - right.position[axis]).abs() < 0.025,
            "{context}, axis {axis}: {left:?} != {right:?}"
        );
    }
    let l = mdlc::bone_math::angle_quaternion(left.rotation);
    let r = mdlc::bone_math::angle_quaternion(right.rotation);
    let dot = l.into_iter().zip(r).map(|(a, b)| a * b).sum::<f32>().abs();
    assert!(
        dot > 0.9999,
        "{context}, rotation {left:?} != {right:?}, dot={dot}"
    );
}

#[test]
fn animation_roundtrip_preserves_frames_fps_events_activity_weights_and_flags() {
    for (external, sectioned, delta) in [
        (false, false, false),
        (false, true, false),
        (true, false, false),
        (true, true, false),
        (false, false, true),
        (true, false, true),
        (true, true, true),
    ] {
        let project = fixture(external, sectioned, delta);
        let source = project.path().join("models/tests/animated.mdl");
        let original = fs::read(&source).unwrap();
        let ani = fs::read(source.with_extension("ani")).ok();
        let expected = first_animation(&original, ani.as_deref());
        let destination = project.path().join("exported");
        let report = super::super::decompile_model(&source, &destination).unwrap();
        assert!(
            report
                .warnings
                .iter()
                .all(|warning| !warning.contains("Animation sequences"))
        );
        let desc = qc::parse_qc_file(&report.qc_path).unwrap();
        let sequence = &desc.sequences[0];
        assert_eq!(sequence.name, "walking");
        assert_eq!(sequence.activity.as_deref(), Some("ACT_WALK"));
        assert_eq!(sequence.activity_weight, 3);
        assert_eq!(sequence.events.len(), 2);
        let built =
            pipeline::build(&desc, &destination, pipeline::PipelineOptions::default()).unwrap();
        let actual = first_animation(&built.mdl, built.ani.as_deref());
        assert_eq!(expected.fps, actual.fps);
        assert_eq!(expected.flags & !0x40, actual.flags & !0x40);
        assert_eq!(expected.frames.len(), 9);
        assert_eq!(expected.frames.len(), actual.frames.len());
        for (frame, (left, right)) in expected.frames.iter().zip(&actual.frames).enumerate() {
            assert_eq!(left.len(), right.len());
            for (left, right) in left.iter().zip(right) {
                assert_pose(
                    left,
                    right,
                    &format!(
                        "external={external} sectioned={sectioned} delta={delta} frame={frame}"
                    ),
                );
            }
        }
        let original_reader = Reader(&original);
        let rebuilt_reader = Reader(&built.mdl);
        let a = original_reader.table(188, 192, 212).unwrap()[0];
        let b = rebuilt_reader.table(188, 192, 212).unwrap()[0];
        for offset in [12, 20, 24, 104, 108] {
            assert_eq!(
                original_reader.i32(a + offset).unwrap(),
                rebuilt_reader.i32(b + offset).unwrap(),
                "sequence field {offset}"
            );
        }
        let a_weights = original_reader.relative(a, a + 156).unwrap();
        let b_weights = rebuilt_reader.relative(b, b + 156).unwrap();
        assert_eq!(
            original_reader.bytes(a_weights, 12).unwrap(),
            rebuilt_reader.bytes(b_weights, 12).unwrap()
        );
    }
}

#[test]
fn external_version48_rle_at_zero_offset_recovers_moving_root() {
    let project = fixture(true, false, false);
    let mut desc = qc::parse_qc_file(&project.path().join("source.qc")).unwrap();
    desc.model.version = Some(48);
    let external =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let reader = Reader(&external.mdl);
    let record = reader.table(180, 184, 100).unwrap()[1];
    assert!(reader.i32(record + 52).unwrap() > 0);
    assert_eq!(reader.count(record + 56).unwrap(), 0);
    assert_eq!(reader.i32(record + 12).unwrap() & 0x40, 0);
    desc.model.anim_block_size = Some(0);
    let inline =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let expected = first_animation(&inline.mdl, None);
    let actual = first_animation(&external.mdl, external.ani.as_deref());
    assert!((expected.frames[0][0].position[0] - expected.frames[8][0].position[0]).abs() > 1.0);
    assert_eq!(expected.frames.len(), actual.frames.len());
    for (frame, (expected, actual)) in expected.frames.iter().zip(&actual.frames).enumerate() {
        assert_eq!(expected.len(), actual.len());
        for (expected, actual) in expected.iter().zip(actual) {
            assert_pose(
                expected,
                actual,
                &format!("external zero offset frame {frame}"),
            );
        }
    }
}

#[test]
fn external_animation_path_has_one_models_prefix_and_forward_slashes() {
    let project = fixture(true, false, false);
    let mut desc = qc::parse_qc_file(&project.path().join("source.qc")).unwrap();
    desc.model.version = Some(48);
    for name in [
        "models/tests/animated.mdl",
        "tests/animated.mdl",
        "models\\tests\\animated.mdl",
    ] {
        desc.model.name = name.into();
        let built =
            pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
        let offset = Reader(&built.mdl).count(348).unwrap();
        let path = built.mdl[offset..].split(|&byte| byte == 0).next().unwrap();
        assert_eq!(path, b"models/tests/animated.ani", "{name}");
    }
}

#[test]
fn malformed_animation_counts_sections_and_external_files_cannot_publish_output() {
    for corruption in [0, 1, 2, 3] {
        let project = fixture(true, true, false);
        let source = project.path().join("models/tests/animated.mdl");
        let mut raw = fs::read(&source).unwrap();
        let record = Reader(&raw).table(180, 184, 100).unwrap()[1];
        match corruption {
            0 => raw[record + 16..record + 20].copy_from_slice(&i32::MAX.to_le_bytes()),
            1 => raw[record + 80..record + 84].copy_from_slice(&i32::MAX.to_le_bytes()),
            2 => {
                let path = source.with_extension("ani");
                let mut ani = fs::read(&path).unwrap();
                ani[8] ^= 1;
                fs::write(path, ani).unwrap();
            }
            3 => {
                fs::remove_file(source.with_extension("ani")).unwrap();
            }
            _ => unreachable!(),
        }
        fs::write(&source, raw).unwrap();
        let destination = project.path().join("bad");
        assert!(
            super::super::decompile_model(&source, &destination).is_err(),
            "corruption {corruption}"
        );
        assert!(!destination.exists());
    }
}

#[test]
fn animation_rle_and_compressed_values_follow_source_storage_rules() {
    let values = [6, 0, 0, 0, 0, 0, 2, 4, 10, 0, 20, 0, 1, 1, 30, 0];
    for (frame, expected) in [(0, 10.0), (1, 20.0), (2, 20.0), (3, 20.0), (4, 30.0)] {
        assert_eq!(
            decode::rle(&Reader(&values), 0, frame).unwrap(),
            [expected, 0.0, 0.0]
        );
    }
    for malformed in [
        vec![6, 0, 0, 0, 0, 0, 0, 4],
        vec![6, 0, 0, 0, 0, 0, 3, 2, 0, 0],
        vec![255, 255, 0, 0, 0, 0],
    ] {
        assert!(decode::rle(&Reader(&malformed), 0, 0).is_err());
    }
}

#[test]
fn gmod_sequence_flags_layers_iklocks_and_keyvalues_survive_recompilation() {
    let project = fixture(false, false, false);
    let path = project.path().join("source.qc");
    let mut source = fs::read_to_string(&path).unwrap();
    source.push_str("$ikchain limb tip knee 0 1 0\n$poseparameter action 0 1\n$sequence gesture base noautoik\n$sequence layered { walk realtime worldspace hidden predelta cyclepose action blendlayer gesture 0 2 6 8 spline xfade local iklock limb 0.25 0.75 keyvalues { sounds { start ping stop pong } } }\n");
    fs::write(&path, source).unwrap();
    let mut desc = qc::parse_qc_file(&path).unwrap();
    desc.model.version = Some(48);
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&built, project.path()).unwrap();
    let reader = Reader(&built.mdl);
    let record = reader.table(188, 192, 212).unwrap()[2];
    let flags = reader.i32(record + 12).unwrap();
    assert_eq!(
        flags & (0x100 | 0x200 | 0x400 | 0x4000),
        0x100 | 0x200 | 0x400 | 0x4000
    );
    assert_eq!(
        flags & 0x14,
        0x04,
        "predelta must preserve pre-multiplication without STUDIO_POST"
    );
    let destination = project.path().join("layered_export");
    let report = super::super::decompile_model(
        &project.path().join("models/tests/animated.mdl"),
        &destination,
    )
    .unwrap();
    let mut restored = qc::parse_qc_file(&report.qc_path).unwrap();
    restored.model.version = Some(48);
    let rebuilt = pipeline::build(
        &restored,
        &destination,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    let other = Reader(&rebuilt.mdl);
    let record_other = other.table(188, 192, 212).unwrap()[2];
    assert_eq!(flags, other.i32(record_other + 12).unwrap());
    for (count_offset, index_offset, stride) in [(148, 152, 24), (164, 168, 32), (176, 172, 1)] {
        let count = reader.count(record + count_offset).unwrap();
        assert!(count > 0);
        assert_eq!(count, other.count(record_other + count_offset).unwrap());
        let start = reader.relative(record, record + index_offset).unwrap();
        let other_start = other
            .relative(record_other, record_other + index_offset)
            .unwrap();
        assert_eq!(
            reader.bytes(start, count * stride).unwrap(),
            other.bytes(other_start, count * stride).unwrap(),
            "sequence subtable {count_offset}"
        );
    }
}

#[test]
fn absent_animation_channels_preserve_the_engine_reference_pose() {
    let project = fixture(false, false, false);
    let source = project.path().join("models/tests/animated.mdl");
    let mut raw = fs::read(&source).unwrap();
    let reader = Reader(&raw);
    let sequence = reader.table(188, 192, 212).unwrap()[0];
    let table = reader.relative(sequence, sequence + 60).unwrap();
    let index = reader.i16(table).unwrap() as usize;
    let record = reader.table(180, 184, 100).unwrap()[index];
    raw[record + 12..record + 16].copy_from_slice(&0x20_i32.to_le_bytes());
    fs::write(&source, &raw).unwrap();
    let expected = first_animation(&raw, None);
    let model = parsed(&raw);
    let reference = &model.bones[0];
    let pose = &expected.frames[0][0];
    assert_eq!(
        pose.position,
        [
            reference.position[1],
            -reference.position[0],
            reference.position[2]
        ]
    );
    let stored = mdlc::ani_writer::quaternion_mul(
        [
            0.0,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ],
        mdlc::bone_math::angle_quaternion(pose.rotation),
    );
    assert!(
        stored
            .into_iter()
            .zip(reference.quaternion)
            .map(|(a, b)| a * b)
            .sum::<f32>()
            .abs()
            > 0.99999
    );
    let destination = project.path().join("absent_channels");
    let report = super::super::decompile_model(&source, &destination).unwrap();
    let desc = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt =
        pipeline::build(&desc, &destination, pipeline::PipelineOptions::default()).unwrap();
    let actual = first_animation(&rebuilt.mdl, rebuilt.ani.as_deref());
    assert_eq!(expected.frames.len(), actual.frames.len());
    for (left, right) in expected.frames.iter().zip(&actual.frames) {
        assert_eq!(left.len(), right.len());
        for (left, right) in left.iter().zip(right) {
            assert_pose(left, right, "absent channel reference pose");
        }
    }
}

#[test]
fn movement_paths_and_in_place_poses_survive_standard_qc_roundtrip() {
    let project = fixture(false, false, false);
    let path = project.path().join("source.qc");
    let mut desc = qc::parse_qc_file(&path).unwrap();
    desc.model.version = Some(48);
    desc.animations[1].smd = "rig.smd".into();
    let reference = fs::read_to_string(project.path().join("rig.smd")).unwrap();
    let skeleton = reference
        .split("skeleton\n")
        .nth(1)
        .unwrap()
        .split("end")
        .next()
        .unwrap();
    let nodes = reference.split("skeleton\n").next().unwrap();
    let pose = skeleton.split('\n').skip(1).collect::<Vec<_>>().join("\n");
    let mut animation = format!("{nodes}skeleton\n");
    for frame in 0..9 {
        writeln!(animation, "time {frame}\n{pose}").unwrap();
    }
    animation.push_str("end\n");
    fs::write(project.path().join("stationary.smd"), animation).unwrap();
    desc.animations[1].smd = "stationary.smd".into();
    desc.sequences[0].movements = vec![
        mdlc::model::Movement {
            endframe: 4,
            motionflags: 64 | 128 | 256 | 2048 | 4096,
            v0: 8.0,
            v1: 8.0,
            angle: 30.0,
            vector: [1.0, 0.0, 0.0],
            position: [8.0, 0.0, 0.0],
        },
        mdlc::model::Movement {
            endframe: 8,
            motionflags: 64 | 128 | 256 | 2048 | 4096,
            v0: 4.0,
            v1: 4.0,
            angle: 90.0,
            vector: [0.0, 1.0, 0.0],
            position: [8.0, 4.0, 0.0],
        },
    ];
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&built, project.path()).unwrap();
    let destination = project.path().join("movement_export");
    let report = super::super::decompile_model(
        &project.path().join("models/tests/animated.mdl"),
        &destination,
    )
    .unwrap();
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("movement extraction"))
    );
    let text = fs::read_to_string(&report.qc_path).unwrap();
    assert!(text.contains("walkframe 4 LX LY LZ LZR LM walkframe 8 LX LY LZ LZR LM"));
    let mut restored = qc::parse_qc_file(&report.qc_path).unwrap();
    restored.model.version = Some(48);
    let rebuilt = pipeline::build(
        &restored,
        &destination,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    let first = Reader(&built.mdl);
    let second = Reader(&rebuilt.mdl);
    let a = first.table(180, 184, 100).unwrap()[1];
    let b = second.table(180, 184, 100).unwrap()[1];
    let orphan = first.table(180, 184, 100).unwrap()[0];
    assert_eq!(
        first.count(orphan + 20).unwrap(),
        0,
        "Unreferenced animations must not inherit sequence movement"
    );
    assert_eq!(first.count(a + 20).unwrap(), 2);
    assert_eq!(second.count(b + 20).unwrap(), 2);
    let at = first.relative(a, a + 24).unwrap();
    let bt = second.relative(b, b + 24).unwrap();
    for key in 0..2 {
        assert_eq!(
            first.i32(at + key * 44).unwrap(),
            second.i32(bt + key * 44).unwrap()
        );
        assert_eq!(
            first.i32(at + key * 44 + 4).unwrap(),
            second.i32(bt + key * 44 + 4).unwrap()
        );
        for field in (8..44).step_by(4) {
            let x = first.f32(at + key * 44 + field).unwrap();
            let y = second.f32(bt + key * 44 + field).unwrap();
            assert!((x - y).abs() < 0.03, "key {key}, field {field}: {x} != {y}");
        }
    }
    let original = decode::animation(
        &first,
        None,
        &parsed(&built.mdl),
        a,
        &AtomicBool::new(false),
    )
    .unwrap();
    let other = decode::animation(
        &second,
        None,
        &parsed(&rebuilt.mdl),
        b,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(original.frames.len(), other.frames.len());
    for (left, right) in original.frames.iter().zip(&other.frames) {
        assert_eq!(left.len(), right.len());
    }
    for (left, right) in original
        .frames
        .iter()
        .flatten()
        .zip(other.frames.iter().flatten())
    {
        for (x, y) in left.position.iter().zip(right.position) {
            assert!((x - y).abs() < 0.03);
        }
        let a = mdlc::bone_math::angle_quaternion(left.rotation);
        let b = mdlc::bone_math::angle_quaternion(right.rotation);
        assert!(a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>().abs() > 0.9999);
    }
    let mut corrupt = built.mdl.clone();
    corrupt[at..at + 4].copy_from_slice(&99i32.to_le_bytes());
    fs::write(project.path().join("models/tests/animated.mdl"), corrupt).unwrap();
    let invalid_destination = project.path().join("invalid_movement_export");
    assert!(
        super::super::decompile_model(
            &project.path().join("models/tests/animated.mdl"),
            &invalid_destination,
        )
        .is_err()
    );
    assert!(!invalid_destination.exists());
}

#[test]
fn qc_walkframe_and_axis_controls_generate_per_animation_movement() {
    let project = fixture(false, false, false);
    let path = project.path().join("source.qc");
    let mut desc = qc::parse_qc_file(&path).unwrap();
    desc.model.version = Some(48);
    desc.animations[1].cmds = vec![
        mdlc::model::AnimCmd::Motion {
            motion_type: 64 | 128 | 4096,
            end_frame: 4,
        },
        mdlc::model::AnimCmd::Motion {
            motion_type: 64 | 128 | 4096,
            end_frame: 8,
        },
    ];
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let reader = Reader(&built.mdl);
    let records = reader.table(180, 184, 100).unwrap();
    assert_eq!(reader.count(records[0] + 20).unwrap(), 0);
    assert_eq!(reader.count(records[1] + 20).unwrap(), 2);
    let movements = reader.relative(records[1], records[1] + 24).unwrap();
    assert!((reader.f32(movements + 8).unwrap() - 2.0).abs() < 0.001);
    assert!((reader.f32(movements + 32).unwrap() - 1.6).abs() < 0.001);
    assert!((reader.f32(movements + 36).unwrap() - 1.2).abs() < 0.001);
    let poses = decode::animation(
        &reader,
        None,
        &parsed(&built.mdl),
        records[1],
        &AtomicBool::new(false),
    )
    .unwrap();
    for frame in &poses.frames {
        for axis in 0..2 {
            assert!((frame[0].position[axis] - poses.frames[0][0].position[axis]).abs() < 0.025);
        }
    }
    let source = fs::read_to_string(&path).unwrap().replace(
        "$animation walk walk.smd fps 24 noautoik ",
        "$animation walk walk.smd fps 24 noautoik X Y ",
    );
    fs::write(&path, source).unwrap();
    let desc = qc::parse_qc_file(&path).unwrap();
    assert_eq!(desc.animations[1].motion_type, 3);
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let reader = Reader(&built.mdl);
    let record = reader.table(180, 184, 100).unwrap()[1];
    let poses = decode::animation(
        &reader,
        None,
        &parsed(&built.mdl),
        record,
        &AtomicBool::new(false),
    )
    .unwrap();
    for frame in &poses.frames {
        for axis in 0..2 {
            assert!((frame[0].position[axis] - poses.frames[0][0].position[axis]).abs() < 0.025);
        }
    }
}

#[test]
fn animation_command_lists_apply_frame_edits_and_never_silently_drop_commands() {
    let project = fixture(false, false, false);
    let path = project.path().join("source.qc");
    let mut desc = qc::parse_qc_file(&path).unwrap();
    desc.model.version = Some(48);
    desc.sequences[0].events.clear();
    desc.animations[1].cmds = vec![
        mdlc::model::AnimCmd::Compress { frames: 2 },
        mdlc::model::AnimCmd::NumFrames { frames: 3 },
    ];
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let reader = Reader(&built.mdl);
    let record = reader.table(180, 184, 100).unwrap()[1];
    assert_eq!(reader.count(record + 16).unwrap(), 3);
    assert_eq!(reader.f32(record + 8).unwrap(), 12.0);
    let poses = decode::animation(
        &reader,
        None,
        &parsed(&built.mdl),
        record,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!((poses.frames[2][0].position[0] - 4.2).abs() < 0.025);
    assert!((poses.frames[2][0].position[1] - 5.4).abs() < 0.025);
    desc.sequences[0].cmds.clear();
    desc.animations[1]
        .cmds
        .push(mdlc::model::AnimCmd::NoAnimation);
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let reader = Reader(&built.mdl);
    let record = reader.table(180, 184, 100).unwrap()[1];
    assert_eq!(reader.count(record + 16).unwrap(), 1);
    assert_eq!(reader.i32(record + 12).unwrap() & 4, 4);
    let sequence = reader.table(188, 192, 212).unwrap()[0];
    let weights = reader.relative(sequence, sequence + 156).unwrap();
    for index in 0..3 {
        assert_eq!(reader.f32(weights + index * 4).unwrap(), 0.0);
    }
    desc.animations[1].cmds = vec![mdlc::model::AnimCmd::FixupLoop { start: 0, end: 8 }];
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let poses = first_animation(&built.mdl, None);
    assert!((poses.frames[0][0].position[0] - 5.4).abs() < 0.025);
    assert!((poses.frames[0][0].position[1] - 3.8).abs() < 0.025);
    assert_pose(
        &poses.frames[0][0],
        &poses.frames[8][0],
        "fixuploop endpoints",
    );
    desc.animations[1].cmds = vec![mdlc::model::AnimCmd::Align {
        reference: "missing".into(),
        motion_type: 3,
        src_frame: 0,
        dest_frame: 0,
        bone: None,
    }];
    let error =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap_err();
    assert!(error.to_string().contains("Unknown alignment reference"));
}

#[test]
fn animation_edits_and_walkframe_motion_follow_command_order() {
    use mdlc::model::AnimCmd;
    let project = fixture(false, false, false);
    let mut desc = qc::parse_qc_file(&project.path().join("source.qc")).unwrap();
    desc.model.version = Some(48);
    desc.sequences[0].events.clear();
    let rotate = AnimCmd::Angle { angle: 0.0 };
    let motion = AnimCmd::Motion {
        motion_type: 0x40,
        end_frame: 8,
    };
    desc.animations[1].cmds = vec![rotate.clone(), motion.clone()];
    let aligned =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let aligned = first_animation(&aligned.mdl, None);
    for frame in &aligned.frames {
        for axis in 0..2 {
            assert!((frame[0].position[axis] - aligned.frames[0][0].position[axis]).abs() < 0.025);
        }
    }
    desc.animations[1].cmds = vec![motion, rotate];
    let unaligned =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let unaligned = first_animation(&unaligned.mdl, None);
    assert!((unaligned.frames[8][0].position[0] - unaligned.frames[0][0].position[0]).abs() > 2.0);
    desc.animations[1].cmds = vec![
        AnimCmd::Compress { frames: 2 },
        AnimCmd::Motion {
            motion_type: 0x40 | 0x80,
            end_frame: 4,
        },
        AnimCmd::NumFrames { frames: 6 },
    ];
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let reader = Reader(&built.mdl);
    let animation = reader.table(180, 184, 100).unwrap()[1];
    assert_eq!(reader.count(animation + 16).unwrap(), 6);
    assert_eq!(reader.f32(animation + 8).unwrap(), 12.0);
    assert_eq!(reader.count(animation + 20).unwrap(), 1);
    let movements = reader.relative(animation, animation + 24).unwrap();
    assert_eq!(reader.count(movements).unwrap(), 4);
    let poses = first_animation(&built.mdl, None);
    for frame in &poses.frames {
        for axis in 0..2 {
            assert!((frame[0].position[axis] - poses.frames[0][0].position[axis]).abs() < 0.025);
        }
    }
}

#[test]
fn direct_animation_options_execute_in_written_order() {
    let project = fixture(false, false, false);
    let path = project.path().join("source.qc");
    let build = |options: &str| {
        fs::write(&path, format!("$modelname tests/ordered.mdl\n$body body rig.smd\n$weightlist zero {{ root 0 }}\n$animation base rig.smd noautoik\n$animation walk walk.smd noautoik {options}\n$sequence walking walk\n")).unwrap();
        let mut desc = qc::parse_qc_file(&path).unwrap();
        desc.model.version = Some(48);
        mdlc::compile::compile(&desc, project.path()).unwrap()
    };
    let before = build("numframes 5 compress 2");
    let after = build("compress 2 numframes 5");
    assert_eq!(before.animations[1].frames.len(), 3);
    assert_eq!(after.animations[1].frames.len(), 5);
    let before = build("weightlist zero subtract base 0").animations[1].clone();
    let after = build("subtract base 0 weightlist zero").animations[1].clone();
    assert!((before.frames[8][0].position[0] + 3.8).abs() < 0.025);
    assert!((after.frames[8][0].position[0] - 3.2).abs() < 0.025);
    assert!((after.frames[8][0].position[1] - 2.4).abs() < 0.025);
    let self_reference = build("rotateto 0 subtract walk 0").animations[1].clone();
    assert!(
        self_reference.frames[0][0]
            .position
            .iter()
            .all(|value| value.abs() < 0.025)
    );
}

#[test]
fn local_hierarchy_roundtrip_preserves_inline_external_and_sectioned_payloads() {
    for (external, sectioned) in [(false, false), (true, false), (true, true)] {
        let project = fixture(external, sectioned, false);
        let path = project.path().join("source.qc");
        let text = fs::read_to_string(&path).unwrap().replace("fps 24 noautoik", "fps 24 noautoik localhierarchy tip root range 0 2 6 8 localhierarchy mid \"\" range 0 0 8 8");
        fs::write(&path, text).unwrap();
        let mut desc = qc::parse_qc_file(&path).unwrap();
        desc.model.version = Some(49);
        let compiled = mdlc::compile::compile(&desc, project.path()).unwrap();
        let rules = &compiled.animations[1].local_hierarchy;
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].bone, 2);
        assert_eq!(rules[0].parent, 0);
        assert_eq!(rules[0].range, [0.0, 0.25, 0.75, 1.0]);
        assert_eq!(rules[1].parent, -1);
        assert!((rules[0].poses[0].0[2] - 20.0).abs() < 0.001);
        let built =
            pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
        pipeline::write_files(&built, project.path()).unwrap();
        let original = Reader(&built.mdl);
        let record = original.table(180, 184, 100).unwrap()[1];
        assert_eq!(original.count(record + 72).unwrap(), 2);
        assert!(original.count(record + 76).unwrap() > 0);
        let destination = project.path().join("hierarchy-export");
        let report = super::super::decompile_model(
            &project.path().join("models/tests/animated.mdl"),
            &destination,
        )
        .unwrap();
        let text = fs::read_to_string(&report.qc_path).unwrap();
        assert!(text.contains("localhierarchy \"tip\" \"root\" range 0 2 6 8"));
        assert!(text.contains("localhierarchy \"mid\" \"\" range 0 0 8 8"));
        let mut restored = qc::parse_qc_file(&report.qc_path).unwrap();
        restored.model.version = Some(49);
        if external {
            restored.model.anim_block_size = Some(1);
        }
        let rebuilt = pipeline::build(
            &restored,
            &destination,
            pipeline::PipelineOptions::default(),
        )
        .unwrap();
        let reader = Reader(&rebuilt.mdl);
        let record = reader.table(180, 184, 100).unwrap()[1];
        let animation = first_animation(&rebuilt.mdl, rebuilt.ani.as_deref());
        let options = hierarchy::recover(
            &reader,
            rebuilt.ani.as_deref(),
            &parsed(&rebuilt.mdl),
            record,
            &animation,
        )
        .unwrap();
        assert!(options.contains("localhierarchy \"tip\" \"root\" range 0 2 6 8"));
    }
}

#[test]
fn local_hierarchy_rejects_unrepresentable_raw_source_overrides_and_bad_payloads() {
    use mdlc::model::AnimCmd;
    let project = fixture(false, false, false);
    let mut desc = qc::parse_qc_file(&project.path().join("source.qc")).unwrap();
    desc.model.version = Some(49);
    desc.animations[1].cmds = vec![
        AnimCmd::CounterRotate {
            bone: "tip".into(),
            target_angle: Some([0.0; 3]),
        },
        AnimCmd::LocalHierarchy {
            bone: "tip".into(),
            parent: "root".into(),
            range: [0, 0, 8, 8],
        },
    ];
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let reader = Reader(&built.mdl);
    let record = reader.table(180, 184, 100).unwrap()[1];
    let animation = first_animation(&built.mdl, None);
    assert!(hierarchy::recover(&reader, None, &parsed(&built.mdl), record, &animation).is_err());
    desc.animations[1].cmds.remove(0);
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let reader = Reader(&built.mdl);
    let table = reader.relative(record, record + 76).unwrap();
    let payload = reader.relative(table, table + 28).unwrap();
    for (offset, value) in [
        (table, 999_i32),
        (table + 4, -2),
        (table + 24, -1),
        (table + 28, i32::MAX),
    ] {
        let mut corrupt = built.mdl.clone();
        corrupt[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        assert!(
            hierarchy::recover(
                &Reader(&corrupt),
                None,
                &parsed(&built.mdl),
                record,
                &first_animation(&built.mdl, None)
            )
            .is_err()
        );
    }
    let mut corrupt = built.mdl.clone();
    corrupt[payload + 24..payload + 26].copy_from_slice(&1_i16.to_le_bytes());
    assert!(
        hierarchy::recover(
            &Reader(&corrupt),
            None,
            &parsed(&built.mdl),
            record,
            &first_animation(&built.mdl, None)
        )
        .is_err()
    );
}

#[test]
fn gmod_local_hierarchy_bakes_channels_without_unsafe_runtime_records() {
    let project = fixture(false, false, false);
    let path = project.path().join("source.qc");
    let text = fs::read_to_string(&path).unwrap().replace(
        "fps 24 noautoik",
        "fps 24 noautoik counterrotateto 0 0 0 tip localhierarchy tip root range 0 0 8 8",
    );
    fs::write(&path, text).unwrap();
    let mut desc = qc::parse_qc_file(&path).unwrap();
    desc.model.version = Some(48);
    let baked = mdlc::compile::compile(&desc, project.path()).unwrap();
    assert!(baked.animations[1].local_hierarchy.is_empty());
    let last = baked.animations[1].frames.last().unwrap();
    assert!((last[2].rotation[1] - 0.32).abs() < 0.0001);
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let reader = Reader(&built.mdl);
    let record = reader.table(180, 184, 100).unwrap()[1];
    assert_eq!(reader.count(record + 72).unwrap(), 0);
    assert_eq!(reader.count(record + 76).unwrap(), 0);
    let mut unedited = desc;
    unedited.animations[1].cmds.clear();
    let reference = pipeline::build(
        &unedited,
        project.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    let reference = first_animation(&reference.mdl, None);
    let actual = first_animation(&built.mdl, None);
    assert!(actual.frames.len() >= reference.frames.len());
    let stride = (actual.frames.len() - 1) / (reference.frames.len() - 1);
    for (index, reference) in reference.frames.iter().enumerate() {
        for (reference, actual) in reference.iter().zip(&actual.frames[index * stride]) {
            assert_pose(
                reference,
                actual,
                "baked hierarchy restores raw-source tip pose",
            );
        }
    }
}

#[test]
fn local_hierarchy_ranges_use_final_frame_count_and_reject_unsafe_bakes() {
    use mdlc::model::AnimCmd;
    let project = fixture(false, false, false);
    let mut desc = qc::parse_qc_file(&project.path().join("source.qc")).unwrap();
    desc.model.version = Some(49);
    desc.animations[1].cmds = vec![
        AnimCmd::LocalHierarchy {
            bone: "tip".into(),
            parent: "root".into(),
            range: [0, 2, 6, 8],
        },
        AnimCmd::NumFrames { frames: 17 },
    ];
    let compiled = mdlc::compile::compile(&desc, project.path()).unwrap();
    assert_eq!(compiled.animations[1].frames.len(), 17);
    assert_eq!(
        compiled.animations[1].local_hierarchy[0].range,
        [0.0, 0.125, 0.375, 0.5]
    );
    desc.animations[1].cmds[1] = AnimCmd::NumFrames { frames: 1 };
    assert!(mdlc::compile::compile(&desc, project.path()).is_err());
    desc.model.version = Some(48);
    desc.animations[1].cmds.pop();
    desc.animations[1].cmds[0] = AnimCmd::LocalHierarchy {
        bone: "tip".into(),
        parent: "root".into(),
        range: [0, 0, 8, 8],
    };
    desc.animations[1].looping = true;
    let errors = mdlc::compile::compile(&desc, project.path()).unwrap_err();
    assert!(format!("{errors:?}").contains("open loop endpoint"));
}

#[test]
fn external_sectioned_single_terminal_frame_keeps_rotation_and_position() {
    let project = fixture(false, false, false);
    let mut desc = qc::parse_qc_file(&project.path().join("source.qc")).unwrap();
    desc.model.version = Some(48);
    let reference =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let reference = first_animation(&reference.mdl, None);
    desc.model.anim_block_size = Some(1);
    desc.sequences[0].section_frames = Some(3);
    desc.sequences[0].section_threshold = Some(3);
    let actual =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let actual = first_animation(&actual.mdl, actual.ani.as_deref());
    for (expected, actual) in reference.frames[8].iter().zip(&actual.frames[8]) {
        assert_pose(expected, actual, "external section terminal pose");
    }
    assert!((actual.frames[8][2].rotation[1] - 0.32).abs() < 0.001);
}

#[test]
fn append_and_prepend_keep_shared_animation_command_order() {
    let project = fixture(false, false, false);
    let path = project.path().join("source.qc");
    let source = fs::read_to_string(&path).unwrap().replace(
        "event 1004 4 \"step\" event AE_CL_PLAYSOUND 8 \"footstep\"",
        "",
    );
    for (options, count) in [
        ("$append walk numframes 5\n$append walking compress 2\n", 3),
        ("$append walking numframes 5\n$prepend walk compress 2\n", 5),
        (
            "$append walk numframes 5\n$prepend walking { compress 2 numframes 7 }\n",
            3,
        ),
    ] {
        fs::write(&path, format!("{source}{options}")).unwrap();
        let desc = qc::parse_qc_file(&path).unwrap();
        let compiled = mdlc::compile::compile(&desc, project.path()).unwrap();
        assert_eq!(compiled.animations[1].frames.len(), count, "{options}");
        assert_eq!(compiled.sequences[0].frames.len(), count, "{options}");
    }
    for command in ["$append absent numframes 3", "$prepend walk fps 30"] {
        fs::write(&path, format!("{source}{command}\n")).unwrap();
        assert!(qc::parse_qc_file(&path).is_err());
    }
}

#[test]
fn declared_animations_write_empty_override_records() {
    let project = fixture(false, false, false);
    let path = project.path().join("source.qc");
    let source = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{source}$declareanimation external_walk\n")).unwrap();
    let desc = qc::parse_qc_file(&path).unwrap();
    assert!(desc.animations[2].forward_declared);
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    let reader = Reader(&built.mdl);
    let record = reader.table(180, 184, 100).unwrap()[2];
    assert_eq!(reader.f32(record + 8).unwrap(), 0.0);
    assert_eq!(reader.i32(record + 12).unwrap(), 0x0800);
    assert_eq!(reader.i32(record + 16).unwrap(), 0);
    pipeline::write_files(&built, project.path()).unwrap();
    let destination = project.path().join("declared-export");
    let report = super::super::decompile_model(
        &project.path().join("models/tests/animated.mdl"),
        &destination,
    )
    .unwrap();
    let text = fs::read_to_string(&report.qc_path).unwrap();
    assert!(text.contains("$declareanimation \"external_walk\""));
    let restored = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt = pipeline::build(
        &restored,
        &destination,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    let reader = Reader(&rebuilt.mdl);
    let record = reader
        .table(180, 184, 100)
        .unwrap()
        .last()
        .copied()
        .unwrap();
    assert_eq!(reader.i32(record + 12).unwrap(), 0x0800);
    assert_eq!(reader.string(record, record + 4).unwrap(), "external_walk");
}

#[test]
fn ikfixup_footstep_locks_contact_after_final_bone_reorientation() {
    let project = fixture(false, false, false);
    let path = project.path().join("source.qc");
    let source = fs::read_to_string(&path).unwrap();
    let mut frames =
        String::from("version 1\nnodes\n0 \"root\" -1\n1 \"mid\" 0\n2 \"tip\" 1\nend\nskeleton\n");
    for frame in 0..9 {
        writeln!(
            frames,
            "time {frame}\n0 3 7 2 0 0 0\n1 0 0 10 {} 0 0\n2 0 0 10 0 0 0",
            frame as f32 * 0.06
        )
        .unwrap();
    }
    frames.push_str("end\n");
    fs::write(project.path().join("walk.smd"), frames).unwrap();
    fs::write(&path, format!("{source}$ikchain leg tip\n")).unwrap();
    let reference =
        mdlc::compile::compile(&qc::parse_qc_file(&path).unwrap(), project.path()).unwrap();
    assert!(reference.realigned.is_some());
    assert_eq!(
        reference.animations[1].frames,
        reference.sequences[0].frames
    );
    fs::write(
        &path,
        format!(
            "{source}$ikchain leg tip\n$append walk ikfixup leg footstep range 2 2 6 6 contact 4\n"
        ),
    )
    .unwrap();
    let fixed = mdlc::compile::compile(&qc::parse_qc_file(&path).unwrap(), project.path()).unwrap();
    let parents = [-1, 0, 1];
    let contact = mdlc::compile::frame_worlds(
        &reference.desc,
        &parents,
        &reference.animations[1].frames[4],
    );
    for frame in 2..=6 {
        let world =
            mdlc::compile::frame_worlds(&fixed.desc, &parents, &fixed.animations[1].frames[frame]);
        for axis in [3, 7, 11] {
            assert!(
                (world[2][axis] - contact[2][axis]).abs() < 0.0001,
                "frame {frame}, axis {axis}"
            );
        }
        for pair in [(0, 1), (1, 2)] {
            let length = [3, 7, 11]
                .iter()
                .map(|axis| (world[pair.1][*axis] - world[pair.0][*axis]).powi(2))
                .sum::<f32>()
                .sqrt();
            assert!((length - 10.0).abs() < 0.0001);
        }
    }
    for frame in [0, 1, 7, 8] {
        assert_eq!(
            fixed.animations[1].frames[frame],
            reference.animations[1].frames[frame]
        );
    }
    assert!(fixed.animations[1].ik_rules.is_empty());
    assert_eq!(fixed.animations[1].frames, fixed.sequences[0].frames);
}
