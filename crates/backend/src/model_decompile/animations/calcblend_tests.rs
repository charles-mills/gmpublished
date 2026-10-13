use std::{fmt::Write as _, fs};

use mdlc::{pipeline, qc};

use super::super::Reader;

fn fixture(control: &str, reference: &str, two_dimensions: bool) -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let positions: &[f32] = if control == "XY" {
        &[0.0, 2.0, 9.0, 0.0, 2.0, 9.0, 0.0, 2.0, 9.0]
    } else if two_dimensions {
        &[0.0, 2.0, 9.0, 0.0, 7.0, 8.0]
    } else {
        &[0.0, 0.3, 0.9]
    };
    for (index, value) in positions.iter().enumerate() {
        let (position, rotation) = if control == "X" || control == "XY" {
            (*value, 0.0)
        } else {
            (0.0, *value)
        };
        let y = if control == "XY" {
            [0.0, 3.0, 11.0][index / 3]
        } else {
            0.0
        };
        let smd = format!(
            "version 1\nnodes\n0 \"root\" -1\n1 \"tip\" 0\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 {position} {y} 8 0 0 {rotation}\ntime 1\n0 0 0 0 0 0 0\n1 {position} {y} 8 0 0 {rotation}\nend\ntriangles\nmat\n0 -8 -8 0 0 0 1 0 0 1 0 1\n0 8 -8 0 0 0 1 1 0 1 0 1\n1 0 0 8 0 0 1 0.5 1 1 1 1\nend\n"
        );
        fs::write(project.path().join(format!("a{index}.smd")), smd).unwrap();
    }
    let mut text = String::from(
        "$modelname models/calcblend.mdl\n$poseparameter px -180 180\n$poseparameter py -1 1\n$attachment att tip 0 0 0\n$body body a0.smd\n",
    );
    for index in 0..positions.len() {
        writeln!(
            text,
            "$animation a{index} a{index}.smd{}",
            if reference == "delta" && index != 0 {
                " subtract a0 0"
            } else {
                ""
            }
        )
        .unwrap();
    }
    if reference == "delta" {
        let source = fs::read_to_string(project.path().join("a1.smd"))
            .unwrap()
            .replace("0.3", "0.5");
        fs::write(project.path().join("comp.smd"), source).unwrap();
        text.push_str("$animation companion comp.smd\n");
    }
    text.push_str("$sequence idle {\n");
    for index in 0..positions.len() {
        writeln!(text, "a{index}").unwrap();
    }
    let native_control = if control == "XY" { "X" } else { control };
    let blend_reference = if reference == "delta" {
        "a0"
    } else {
        reference
    };
    write!(
        text,
        "blendwidth 3\ncalcblend px att {native_control}\nblendref {blend_reference}\n"
    )
    .unwrap();
    if reference == "delta" {
        text.push_str("blendcomp companion\n");
    }
    if control == "XY" {
        text.push_str("calcblend py att Y\n");
    } else if two_dimensions {
        text.push_str("blend py 0 1\nblendcenter a0\n");
    }
    text.push_str("}\n");
    let mut desc = qc::parse_qc_str(&text, project.path()).unwrap();
    desc.model.version = Some(48);
    let built =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&built, project.path()).unwrap();
    project
}

fn pose_keys(raw: &[u8]) -> Vec<f32> {
    let reader = Reader(raw);
    let record = reader.table(188, 192, 212).unwrap()[0];
    let count = reader.count(record + 68).unwrap() + reader.count(record + 72).unwrap();
    let keys = reader.relative(record, record + 160).unwrap();
    (0..count)
        .map(|key| reader.f32(keys + key * 4).unwrap())
        .collect()
}

#[test]
fn nonlinear_attachment_blends_roundtrip_positions_angles_reference_and_center() {
    for (control, reference, two_dimensions) in [
        ("ZR", "a0", false),
        ("ZR", "a1", false),
        ("ZR", "delta", false),
        ("X", "a0", true),
        ("XY", "a0", true),
    ] {
        let project = fixture(control, reference, two_dimensions);
        let source = project.path().join("models/calcblend.mdl");
        let original = pose_keys(&fs::read(&source).unwrap());
        let destination = project.path().join("export");
        let report = super::super::super::decompile_model(&source, &destination).unwrap();
        let desc = qc::parse_qc_file(&report.qc_path).unwrap();
        assert_eq!(
            desc.sequences[0]
                .blend_params
                .iter()
                .filter(|axis| axis.attachment.is_some())
                .count(),
            if control == "XY" { 2 } else { 1 }
        );
        let rebuilt =
            pipeline::build(&desc, &destination, pipeline::PipelineOptions::default()).unwrap();
        let rebuilt = pose_keys(&rebuilt.mdl);
        assert_eq!(original.len(), rebuilt.len());
        for (old, new) in original.iter().zip(&rebuilt) {
            assert!(
                (old - new).abs() < 0.001,
                "{control} {reference}: {old} != {new}"
            );
        }
        if two_dimensions && control != "XY" {
            assert!(desc.sequences[0].blend_center.is_some());
        }
    }
}

#[test]
fn nonrepresentable_nonlinear_keys_fail_without_publishing() {
    let project = fixture("ZR", "a0", false);
    let source = project.path().join("models/calcblend.mdl");
    let mut raw = fs::read(&source).unwrap();
    let reader = Reader(&raw);
    let record = reader.table(188, 192, 212).unwrap()[0];
    let keys = reader.relative(record, record + 160).unwrap();
    raw[keys + 4..keys + 8].copy_from_slice(&12345.0_f32.to_le_bytes());
    fs::write(&source, raw).unwrap();
    let destination = project.path().join("invalid");
    assert!(super::super::super::decompile_model(&source, &destination).is_err());
    assert!(!destination.exists());
}
