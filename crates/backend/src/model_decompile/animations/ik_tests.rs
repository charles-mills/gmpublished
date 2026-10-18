use std::{fs, sync::atomic::AtomicBool};

use mdlc::{pipeline, qc};

use super::*;
use crate::model_decompile::decompile_model;

fn fixture(rules: &str, movement: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("rig.smd"),
        include_str!("../../../../vendor/mdlc/tests/fixtures/rig.smd"),
    )
    .unwrap();
    let mut smd =
        String::from("version 1\nnodes\n0 \"root\" -1\n1 \"mid\" 0\n2 \"tip\" 1\nend\nskeleton\n");
    for frame in 0..5 {
        writeln!(
            smd,
            "time {frame}\n0 {} 2 1 0 0 {}\n1 0 0 10 0.1 0 0\n2 0 0 10 0 0.2 0",
            frame as f32 * 0.5,
            frame as f32 * 0.03
        )
        .unwrap();
    }
    smd.push_str("end\n");
    fs::write(directory.path().join("walk.smd"), smd).unwrap();
    let qc = format!(
        "$modelname models/tests/ik.mdl\n$body body rig.smd\n$ikchain leg tip knee 0 1 0 height 7 floor -2 pad 4\n$animation walk walk.smd noautoik {movement} {rules}\n$sequence idle walk\n"
    );
    fs::write(directory.path().join("source.qc"), qc).unwrap();
    build(directory.path(), "source.qc");
    directory
}

fn build(directory: &std::path::Path, path: &str) {
    let desc = qc::parse_qc_file(&directory.join(path)).unwrap();
    let built = pipeline::build(&desc, directory, pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&built, directory).unwrap();
}

fn model(raw: &[u8]) -> Mdl {
    let mut copy = raw.to_vec();
    copy[180..184].fill(0);
    vformats::mdl::parse_mdl(&copy, &vformats::Limits::default()).unwrap()
}

fn animation_record(reader: &Reader<'_>) -> usize {
    reader.table(180, 184, 100).unwrap()[0]
}

fn recover_raw(raw: &[u8]) -> Result<String, DecompileError> {
    let reader = Reader(raw);
    let model = model(raw);
    let record = animation_record(&reader);
    let animation =
        super::super::decode::animation(&reader, None, &model, record, &AtomicBool::new(false))?;
    recover(&reader, &model, record, &animation)
}

#[test]
fn standard_ik_rules_roundtrip_binary_metadata_and_error_samples() {
    for movement in ["", "walkframe 4 LX LY LQ"] {
        let rules = "ikrule leg touch root range 0 1 3 4 contact 2 ikrule leg touch \"\" range 0 1 3 4 contact 2 ikrule leg attachment grip range 0 1 3 4 contact 2 ikrule leg footstep range 0 1 3 4 contact 2 height 7 radius 2 floor -2 ikrule leg release range 0 1 3 4 contact 2 ikrule leg unlatch range 0 1 3 4 contact 2";
        let directory = fixture(rules, movement);
        let raw = fs::read(directory.path().join("models/tests/ik.mdl")).unwrap();
        recover_raw(&raw).unwrap();
        let destination = directory.path().join("exported");
        let report =
            decompile_model(&directory.path().join("models/tests/ik.mdl"), &destination).unwrap();
        let path = fs::read_dir(&destination)
            .unwrap()
            .map(|v| v.unwrap().path())
            .find(|path| path.extension().is_some_and(|v| v == "qc"))
            .unwrap();
        build(&destination, path.file_name().unwrap().to_str().unwrap());
        let desc = qc::parse_qc_file(&report.qc_path).unwrap();
        let rebuilt = fs::read(destination.join(&desc.model.name)).unwrap();
        let original = Reader(&raw);
        let rebuilt = Reader(&rebuilt);
        let a = animation_record(&original);
        let b = animation_record(&rebuilt);
        assert_eq!(original.count(a + 60).unwrap(), 6);
        assert_eq!(rebuilt.count(b + 60).unwrap(), 6);
        let a = original.relative(a, a + 64).unwrap();
        let b = rebuilt.relative(b, b + 64).unwrap();
        for index in 0..6 {
            let a = a + index * 152;
            let b = b + index * 152;
            for offset in [4, 8, 12, 16, 68] {
                assert_eq!(
                    original.i32(a + offset).unwrap(),
                    rebuilt.i32(b + offset).unwrap(),
                    "rule {index}, field {offset}"
                );
            }
            for offset in (20..60).step_by(4).chain([76, 80, 84, 88, 96, 100, 104]) {
                assert!(
                    (original.f32(a + offset).unwrap() - rebuilt.f32(b + offset).unwrap()).abs()
                        < 0.025,
                    "rule {index}, field {offset}"
                );
            }
            let original = errors(&original, a, 5).unwrap().unwrap();
            let rebuilt = errors(&rebuilt, b, 5).unwrap().unwrap();
            for (a, b) in original.iter().zip(rebuilt) {
                for channel in 0..6 {
                    assert!(
                        (a.0[channel] - b.0[channel]).abs()
                            < if channel < 3 { 0.025 } else { 0.001 },
                        "rule {index}, channel {channel}: {:?} vs {:?}",
                        a.0,
                        b.0
                    );
                }
            }
        }
    }
}

#[test]
fn ik_rejects_malformed_references_and_authored_errors() {
    let directory = fixture("ikrule leg touch root range 0 1 3 4 contact 2", "");
    let raw = fs::read(directory.path().join("models/tests/ik.mdl")).unwrap();
    let reader = Reader(&raw);
    let record = animation_record(&reader);
    let rule = reader.relative(record, record + 64).unwrap();
    for (at, value) in [
        (record + 60, 4097),
        (record + 64, i32::MAX),
        (rule + 8, 99),
        (rule + 12, 99),
        (rule + 60, i32::MAX),
        (rule + 68, -1),
    ] {
        let mut corrupt = raw.clone();
        corrupt[at..at + 4].copy_from_slice(&value.to_le_bytes());
        assert!(
            recover_raw(&corrupt).is_err(),
            "accepted invalid field at {at}"
        );
    }
    let compressed = reader.relative(rule, rule + 60).unwrap();
    let channel = compressed + reader.i16(compressed + 24).unwrap() as usize;
    let mut corrupt = raw.clone();
    corrupt[channel] = 0;
    assert!(recover_raw(&corrupt).is_err());
    let mut authored = raw.clone();
    authored[compressed..compressed + 4].copy_from_slice(&1.0f32.to_le_bytes());
    authored[channel + 2..channel + 4].copy_from_slice(&32000i16.to_le_bytes());
    assert!(recover_raw(&authored).is_err());
}

#[test]
fn footstep_inherits_chain_settings_and_release_without_payload_is_recoverable() {
    let directory = fixture("ikrule leg footstep range 0 1 3 4 contact 2", "");
    let raw = fs::read(directory.path().join("models/tests/ik.mdl")).unwrap();
    let reader = Reader(&raw);
    let record = animation_record(&reader);
    let rule = reader.relative(record, record + 64).unwrap();
    assert_eq!(reader.f32(rule + 20).unwrap(), 7.0);
    assert_eq!(reader.f32(rule + 24).unwrap(), 2.0);
    assert_eq!(reader.f32(rule + 28).unwrap(), -2.0);
    recover_raw(&raw).unwrap();
    let directory = fixture("ikrule leg release range 0 0 4 4 contact 0", "");
    let mut raw = fs::read(directory.path().join("models/tests/ik.mdl")).unwrap();
    let reader = Reader(&raw);
    let record = animation_record(&reader);
    let rule = reader.relative(record, record + 64).unwrap();
    raw[rule + 60..rule + 64].fill(0);
    assert!(recover_raw(&raw).unwrap().contains("release"));
}

#[test]
fn authored_chain_center_is_compiled_and_reported_when_not_recoverable() {
    let directory = fixture("ikrule leg footstep range 0 1 3 4 contact 2", "");
    let path = directory.path().join("source.qc");
    let source = fs::read_to_string(&path).unwrap().replace(
        "height 7 floor -2 pad 4",
        "height 7 floor -2 pad 4 center 3 4 5",
    );
    fs::write(&path, source).unwrap();
    let desc = qc::parse_qc_file(&path).unwrap();
    assert_eq!(desc.ikchains[0].center, Some([3.0, 4.0, 5.0]));
    build(directory.path(), "source.qc");
    let raw = fs::read(directory.path().join("models/tests/ik.mdl")).unwrap();
    let error = recover_raw(&raw).unwrap_err().to_string();
    assert!(error.contains("authored chain center"), "{error}");
}

#[test]
fn release_rules_preserve_single_frame_and_delta_animations() {
    for delta in [false, true] {
        let directory = fixture("ikrule leg release", "");
        let path = directory.path().join("source.qc");
        let source = fs::read_to_string(&path).unwrap();
        let source = if delta {
            source
                .replace(
                    "$animation walk",
                    "$animation base rig.smd noautoik\n$animation walk",
                )
                .replace("walk.smd noautoik", "walk.smd noautoik subtract base 0")
        } else {
            source.replace("walk.smd noautoik", "rig.smd noautoik")
        };
        fs::write(&path, source).unwrap();
        build(directory.path(), "source.qc");
        let raw = fs::read(directory.path().join("models/tests/ik.mdl")).unwrap();
        let reader = Reader(&raw);
        let model = model(&raw);
        let record = reader.table(180, 184, 100).unwrap()[usize::from(delta)];
        let animation =
            super::super::decode::animation(&reader, None, &model, record, &AtomicBool::new(false))
                .unwrap();
        assert!(
            recover(&reader, &model, record, &animation)
                .unwrap()
                .contains("release")
        );
    }
}
