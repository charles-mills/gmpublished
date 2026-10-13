use std::{fmt::Write as _, fs};

use mdlc::{pipeline, qc};

use super::*;

fn fixture(options: &str, version: i32) -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let smd = "version 1\nnodes\n0 \"root0\" -1\n1 \"root1\" -1\n2 \"child\" 1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 3 2 1 0 0 0\n2 1 0 0 0 0 0\nend\ntriangles\nmaterial\n0 0 0 0 0 0 1 0 0 1 0 1\n1 3 2 1 0 0 1 1 0 1 1 1\n2 4 2 1 0 0 1 0 1 1 2 1\nend\n";
    fs::write(project.path().join("reference.smd"), smd).unwrap();
    let mut motion = String::from(
        "version 1\nnodes\n0 \"root0\" -1\n1 \"root1\" -1\n2 \"child\" 1\nend\nskeleton\n",
    );
    for frame in 0..3 {
        write!(
            motion,
            "time {frame}\n0 {} 0 0 0 0 {}\n1 {} 2 1 0 0 {}\n2 {} 0 0 0 0 0\n",
            frame * 2,
            frame as f32 * 0.2,
            3 + frame,
            frame as f32 * 0.1,
            1 + frame
        )
        .unwrap();
    }
    motion.push_str("end\n");
    fs::write(project.path().join("motion.smd"), motion).unwrap();
    let text = format!(
        "$modelname models/flags.mdl\n$body body reference.smd\n$animation walk motion.smd {options}\n$sequence idle walk\n"
    );
    let mut desc = qc::parse_qc_str(&text, project.path()).unwrap();
    desc.model.version = Some(version);
    let output =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&output, project.path()).unwrap();
    project
}

fn animation_flags(raw: &[u8]) -> Vec<i32> {
    let count = i32::from_le_bytes(raw[180..184].try_into().unwrap()) as usize;
    let offset = i32::from_le_bytes(raw[184..188].try_into().unwrap()) as usize;
    (0..count)
        .map(|index| {
            i32::from_le_bytes(
                raw[offset + index * 100 + 12..offset + index * 100 + 16]
                    .try_into()
                    .unwrap(),
            )
        })
        .collect()
}

#[test]
fn named_animation_flags_have_native_version_bits_and_roundtrip() {
    for version in [48, 49] {
        let project = fixture("snap post realtime", version);
        let source = project.path().join("models/flags.mdl");
        let raw = fs::read(&source).unwrap();
        assert_eq!(
            animation_flags(&raw),
            [2 | 16 | if version == 49 { 0x80 } else { 0x100 }]
        );
        let destination = project.path().join("export");
        let report = super::super::super::decompile_model(&source, &destination).unwrap();
        let mut desc = qc::parse_qc_file(&report.qc_path).unwrap();
        desc.model.version = Some(version);
        let rebuilt =
            pipeline::build(&desc, &destination, pipeline::PipelineOptions::default()).unwrap();
        assert_eq!(animation_flags(&raw), animation_flags(&rebuilt.mdl));
    }
}

#[test]
fn compiled_startloop_and_fudgeloop_roundtrip_without_repeating_authoring_operations() {
    for option in [
        "startloop 1",
        "fudgeloop",
        "fudgeloop startloop 1",
        "fudgeloop LY",
        "fudgeloop startloop 1 LY",
        "loop LX walkframe 99 LY",
    ] {
        let project = fixture(option, 48);
        let source = project.path().join("models/flags.mdl");
        let destination = project.path().join("export");
        let report = super::super::super::decompile_model(&source, &destination).unwrap();
        let desc = qc::parse_qc_file(&report.qc_path).unwrap();
        let rebuilt =
            pipeline::build(&desc, &destination, pipeline::PipelineOptions::default()).unwrap();
        let raw = fs::read(&source).unwrap();
        let model = vformats::mdl::parse_mdl(&raw, &vformats::Limits::default()).unwrap();
        let new_model =
            vformats::mdl::parse_mdl(&rebuilt.mdl, &vformats::Limits::default()).unwrap();
        let reader = super::super::Reader(&raw);
        let new_reader = super::super::Reader(&rebuilt.mdl);
        let old_record = reader.table(180, 184, 100).unwrap()[0];
        let new_record = new_reader.table(180, 184, 100).unwrap()[0];
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let old =
            super::super::decode::animation(&reader, None, &model, old_record, &cancelled).unwrap();
        let new =
            super::super::decode::animation(&new_reader, None, &new_model, new_record, &cancelled)
                .unwrap();
        assert_eq!(
            old.frames.len(),
            if option.contains("fudgeloop") { 4 } else { 3 }
        );
        assert_eq!(old.frames.len(), new.frames.len());
        let shifted = option.contains("startloop 1");
        let extracted = option.contains("LY");
        let expected_root0_x = if shifted && !extracted { 2.0 } else { 0.0 };
        let expected_root1_x = if shifted {
            if extracted { 2.0 } else { 4.0 }
        } else {
            3.0
        };
        assert!(
            (old.frames[0][0].position[0] - expected_root0_x).abs() < 0.005,
            "{option}: {:?} != x={expected_root0_x}",
            old.frames[0]
        );
        assert!((old.frames[0][1].position[0] - expected_root1_x).abs() < 0.005);
        assert!((old.frames[0][2].position[0] - if shifted { 2.0 } else { 1.0 }).abs() < 0.005);
        for (old, new) in old.frames.iter().zip(&new.frames) {
            for (old, new) in old.iter().zip(new) {
                for (old, new) in old
                    .position
                    .iter()
                    .chain(&old.rotation)
                    .zip(new.position.iter().chain(&new.rotation))
                {
                    assert!((old - new).abs() < 0.0002, "{option}: {old} != {new}");
                }
            }
        }
        let exported = fs::read_to_string(&report.qc_path).unwrap();
        assert!(!exported.contains("startloop"));
        assert!(!exported.contains("fudgeloop"));
    }
}

#[test]
fn flag_options_separate_legacy_realtime_from_frame_storage_bits() {
    assert_eq!(options(0x80, 48), "");
    assert_eq!(options(0x80, 49), " realtime");
    assert_eq!(options(0x100, 48), " realtime");
    assert_eq!(options(0x100, 49), "");
}

#[test]
fn malformed_movement_keys_fail_before_exporting() {
    for (offset, bytes) in [
        (0, 99_i32.to_le_bytes()),
        (4, 0x8000_i32.to_le_bytes()),
        (4, 0x200_i32.to_le_bytes()),
        (8, f32::NAN.to_le_bytes()),
        (12, (-1.0_f32).to_le_bytes()),
        (32, 1000.0_f32.to_le_bytes()),
    ] {
        let project = fixture("fudgeloop LY", 48);
        let source = project.path().join("models/flags.mdl");
        let mut raw = fs::read(&source).unwrap();
        let reader = super::super::Reader(&raw);
        let record = reader.table(180, 184, 100).unwrap()[0];
        let movement = reader.relative(record, record + 24).unwrap();
        raw[movement + offset..movement + offset + 4].copy_from_slice(&bytes);
        fs::write(&source, raw).unwrap();
        let destination = project.path().join("invalid");
        assert!(super::super::super::decompile_model(&source, &destination).is_err());
        assert!(!destination.exists());
    }
}

#[test]
fn partial_loop_movement_keeps_its_path_and_nonclosing_endpoint() {
    let project = fixture("fudgeloop LY", 48);
    let source = project.path().join("models/flags.mdl");
    let mut raw = fs::read(&source).unwrap();
    let reader = super::super::Reader(&raw);
    let record = reader.table(180, 184, 100).unwrap()[0];
    let movement = reader.relative(record, record + 24).unwrap();
    raw[movement..movement + 4].copy_from_slice(&2_i32.to_le_bytes());
    fs::write(&source, &raw).unwrap();
    let destination = project.path().join("export");
    let report = super::super::super::decompile_model(&source, &destination).unwrap();
    let desc = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt =
        pipeline::build(&desc, &destination, pipeline::PipelineOptions::default()).unwrap();
    let reader = super::super::Reader(&rebuilt.mdl);
    let record = reader.table(180, 184, 100).unwrap()[0];
    assert_eq!(reader.count(record + 20).unwrap(), 2);
    let rebuilt_movement = reader.relative(record, record + 24).unwrap();
    assert_eq!(reader.i32(rebuilt_movement).unwrap(), 2);
    assert_eq!(reader.i32(rebuilt_movement + 44).unwrap(), 3);
    for offset in (8..44).step_by(4) {
        let original = f32::from_le_bytes(
            raw[movement + offset..movement + offset + 4]
                .try_into()
                .unwrap(),
        );
        assert!((original - reader.f32(rebuilt_movement + offset).unwrap()).abs() < 0.0002);
    }
    assert_eq!(reader.f32(rebuilt_movement + 44 + 8).unwrap(), 0.0);
    assert_eq!(reader.f32(rebuilt_movement + 44 + 12).unwrap(), 0.0);
}
