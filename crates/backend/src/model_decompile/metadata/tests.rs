use super::*;
use mdlc::{pipeline, qc};
use std::fs;

fn fixture(static_prop: bool) -> (tempfile::TempDir, Vec<u8>, Mdl) {
    let directory = tempfile::tempdir().unwrap();
    let source = include_str!("../../../../vendor/mdlc/tests/fixtures/rig.smd");
    let source = if static_prop {
        source.replace("\"root\"", "\"static_prop\"")
    } else {
        source.to_owned()
    };
    fs::write(directory.path().join("rig.smd"), source).unwrap();
    let text = format!(
        "$modelname models/tests/metadata.mdl\n{}$body rig rig.smd\n$sequence idle rig.smd\n$hboxset damage\n$hbox 2 \"root\" -2 -3 -4 5 6 7 \"torso\"\n$attachment muzzle \"root\" 3 -4 5 rotate 37 62 -24 world_align\n$poseparameter aim_yaw -120 120 wrap\n$poseparameter lean -1 1 loop 0.5\n$poseparameter pitch -30 50\n$includemodel shared/animations.mdl\n$keyvalues {{ prop_data {{ \"base\" \"Metal.Medium\" \"health\" \"30\" \"$include\" \"literal.qci\" \"$unbound$\" \"value\" }} }}\n",
        if static_prop { "$staticprop\n" } else { "" }
    );
    let text = if static_prop {
        text.replace("\"root\"", "\"static_prop\"")
    } else {
        text
    };
    fs::write(directory.path().join("input.qc"), text).unwrap();
    let description = qc::parse_qc_file(&directory.path().join("input.qc")).unwrap();
    let output = pipeline::build(
        &description,
        directory.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    pipeline::write_files(&output, directory.path()).unwrap();
    let model = mdl::parse_mdl(&output.mdl, &vformats::Limits::default()).unwrap();
    (directory, output.mdl, model)
}

fn int(raw: &[u8], at: usize) -> usize {
    u32::from_le_bytes(raw[at..at + 4].try_into().unwrap()) as usize
}

fn float(raw: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(raw[at..at + 4].try_into().unwrap())
}

#[test]
fn model_and_joint_contents_masks_survive_full_decompile_recompile() {
    for static_prop in [false, true] {
        let (directory, original, model) = fixture(static_prop);
        for combination in 0..16 {
            let global = [1i32, 8, 0x0200_0000, 0x2000_0000]
                .into_iter()
                .enumerate()
                .filter(|(index, _)| combination & (1 << index) != 0)
                .fold(0, |mask, (_, flag)| mask | flag);
            let mut raw = original.clone();
            raw[332..336].copy_from_slice(&global.to_le_bytes());
            let bones = int(&raw, 160);
            for (index, bone) in model.bones.iter().enumerate() {
                let mask = if bone.name == "mid" && global & 9 != 9 {
                    global ^ 0x2000_0000
                } else if bone.name == "tip" {
                    global & !1
                } else if static_prop && global & 9 != 9 {
                    global ^ 0x0200_0000
                } else {
                    global
                };
                let at = bones + index * 216 + 180;
                raw[at..at + 4].copy_from_slice(&mask.to_le_bytes());
            }
            let source = directory.path().join("models/tests/metadata.mdl");
            fs::write(&source, &raw).unwrap();
            let destination = directory.path().join(format!("contents_{combination}"));
            let report = super::super::decompile_model(&source, &destination).unwrap();
            assert!(
                !report
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("contents")),
                "static={static_prop}, mask={global:#x}: {:?}",
                report.warnings
            );
            let description = qc::parse_qc_file(&report.qc_path).unwrap();
            let rebuilt = pipeline::build(
                &description,
                &destination,
                pipeline::PipelineOptions::default(),
            )
            .unwrap();
            assert_eq!(int(&rebuilt.mdl, 332), global as usize);
            let rebuilt_model = mdl::parse_mdl(&rebuilt.mdl, &vformats::Limits::default()).unwrap();
            let rebuilt_bones = int(&rebuilt.mdl, 160);
            assert_eq!(model.bones.len(), rebuilt_model.bones.len());
            for (index, bone) in model.bones.iter().enumerate() {
                let rebuilt_index = rebuilt_model
                    .bones
                    .iter()
                    .position(|candidate| candidate.name == bone.name)
                    .unwrap();
                assert_eq!(
                    int(&raw, bones + index * 216 + 180),
                    int(&rebuilt.mdl, rebuilt_bones + rebuilt_index * 216 + 180),
                    "static={static_prop}, mask={global:#x}, bone={:?}",
                    bone.name
                );
            }
        }
    }
}

#[test]
fn unrepresentable_contents_report_original_joint_masks_and_actual_fallback() {
    let (directory, mut raw, model) = fixture(false);
    raw[332..336].copy_from_slice(&0x11i32.to_le_bytes());
    let bones = int(&raw, 160);
    for (index, bone) in model.bones.iter().enumerate() {
        let mask: i32 = match bone.name.as_str() {
            "mid" => 9,
            "tip" => 0x18,
            _ => 1,
        };
        let at = bones + index * 216 + 180;
        raw[at..at + 4].copy_from_slice(&mask.to_le_bytes());
    }
    let source = directory.path().join("models/tests/metadata.mdl");
    fs::write(&source, raw).unwrap();
    let destination = directory.path().join("contents_fallback");
    let report = super::super::decompile_model(&source, &destination).unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("Model contents 0x00000011")
                && warning.contains("uses 0x00000001"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("\"mid\" contents 0x00000009")
                && warning.contains("override was omitted")
                && warning.contains("uses 0x00000001"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("\"tip\" contents 0x00000018")
                && warning.contains("flags unavailable")
                && warning.contains("uses 0x00000008"))
    );
    let description = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt = pipeline::build(
        &description,
        &destination,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    assert_eq!(int(&rebuilt.mdl, 332), 1);
    let rebuilt_bones = int(&rebuilt.mdl, 160);
    for (index, bone) in rebuilt.compiled.desc.bones.iter().enumerate() {
        assert_eq!(
            int(&rebuilt.mdl, rebuilt_bones + index * 216 + 180),
            if bone.name == "tip" { 8 } else { 1 }
        );
    }
}

#[test]
fn attachment_axes_and_inherited_surface_properties_survive_full_roundtrip() {
    for static_prop in [false, true] {
        let (directory, _, _) = fixture(static_prop);
        let root = if static_prop { "static_prop" } else { "root" };
        let text = format!(
            "$modelname models/tests/metadata.mdl\n{}$body body rig.smd\n$sequence idle rig.smd\n$surfaceprop wood\n$jointsurfaceprop {root} metal\n$jointsurfaceprop mid flesh\n$jointsurfaceprop tip \"\"\n$attachment muzzle {root} 3 -4 5 rotate 13 27 41 x_and_z_axes 2 -3 4 -1 5 2 world_align\n",
            if static_prop { "$staticprop\n" } else { "" }
        );
        let description = qc::parse_qc_str(&text, directory.path()).unwrap();
        let original = pipeline::build(
            &description,
            directory.path(),
            pipeline::PipelineOptions::default(),
        )
        .unwrap();
        pipeline::write_files(&original, directory.path()).unwrap();
        let source = directory.path().join("models/tests/metadata.mdl");
        let destination = directory.path().join("axes_surface");
        let report = super::super::decompile_model(&source, &destination).unwrap();
        assert!(
            !report
                .warnings
                .iter()
                .any(|warning| warning.contains("surface properties")),
            "{:?}",
            report.warnings
        );
        let description = qc::parse_qc_file(&report.qc_path).unwrap();
        let rebuilt = pipeline::build(
            &description,
            &destination,
            pipeline::PipelineOptions::default(),
        )
        .unwrap();
        let a = int(&original.mdl, 244);
        let b = int(&rebuilt.mdl, 244);
        assert_eq!(int(&original.mdl, a + 4), 0x10000);
        assert_eq!(int(&rebuilt.mdl, b + 4), 0x10000);
        let x = [2.0, -3.0, 4.0].map(|value| value / 29f32.sqrt());
        let z = [-11.0, 118.0, 94.0].map(|value| value / 22_881f32.sqrt());
        for (row, expected_x, expected_z) in [(0, x[0], z[0]), (1, x[1], z[1]), (2, x[2], z[2])] {
            let (expected_x, expected_z) = if static_prop {
                match row {
                    0 => (-x[1], -z[1]),
                    1 => (x[0], z[0]),
                    _ => (x[2], z[2]),
                }
            } else {
                (expected_x, expected_z)
            };
            assert!((float(&original.mdl, a + 12 + row * 16) - expected_x).abs() < 0.00001);
            assert!((float(&original.mdl, a + 20 + row * 16) - expected_z).abs() < 0.00001);
        }
        for field in (12..60).step_by(4) {
            assert!(
                (float(&original.mdl, a + field) - float(&rebuilt.mdl, b + field)).abs() < 0.00001,
                "static={static_prop}, field={field}"
            );
        }
        let source_bones = int(&original.mdl, 160);
        let target_bones = int(&rebuilt.mdl, 160);
        assert_eq!(int(&original.mdl, 156), int(&rebuilt.mdl, 156));
        for index in 0..int(&original.mdl, 156) {
            let read_surface = |raw: &[u8], bone: usize| {
                let start = bone + int(raw, bone + 176);
                let end = start + raw[start..].iter().position(|&byte| byte == 0).unwrap();
                std::str::from_utf8(&raw[start..end]).unwrap().to_owned()
            };
            let expected = if static_prop || index == 0 {
                "metal"
            } else if original.compiled.desc.bones[index].name == "tip" {
                ""
            } else {
                "flesh"
            };
            assert_eq!(
                read_surface(&original.mdl, source_bones + index * 216),
                expected
            );
            assert_eq!(
                read_surface(&rebuilt.mdl, target_bones + index * 216),
                expected
            );
        }
    }
}

#[test]
fn metadata_recompiles_with_equivalent_attachment_hitbox_pose_and_keyvalue_data() {
    for static_prop in [false, true] {
        let (directory, raw, model) = fixture(static_prop);
        let mut text = format!(
            "$modelname models/tests/rebuilt.mdl\n{}$body rig rig.smd\n$sequence idle rig.smd\n",
            if static_prop { "$staticprop\n" } else { "" }
        );
        let report = write_metadata(&mut text, &raw, &model).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        fs::write(directory.path().join("rebuilt.qc"), text).unwrap();
        let description = qc::parse_qc_file(&directory.path().join("rebuilt.qc")).unwrap();
        let output = pipeline::build(
            &description,
            directory.path(),
            pipeline::PipelineOptions::default(),
        )
        .unwrap();
        let rebuilt = &output.mdl;
        let attachment = int(&raw, 244);
        let rebuilt_attachment = int(rebuilt, 244);
        assert_eq!(
            int(&raw, attachment + 4),
            int(rebuilt, rebuilt_attachment + 4)
        );
        assert_eq!(
            int(&raw, attachment + 8),
            int(rebuilt, rebuilt_attachment + 8)
        );
        for field in (12..60).step_by(4) {
            assert!(
                (float(&raw, attachment + field) - float(rebuilt, rebuilt_attachment + field))
                    .abs()
                    < 0.00001,
                "attachment field {field}, static={static_prop}"
            );
        }
        let set = int(&raw, 176);
        let rebuilt_set = int(rebuilt, 176);
        let hitbox = set + int(&raw, set + 8);
        let rebuilt_hitbox = rebuilt_set + int(rebuilt, rebuilt_set + 8);
        assert_eq!(
            &raw[hitbox..hitbox + 32],
            &rebuilt[rebuilt_hitbox..rebuilt_hitbox + 32]
        );
        let pose = int(&raw, 304);
        let rebuilt_pose = int(rebuilt, 304);
        assert_eq!(int(&raw, 300), 3);
        for index in 0..3 {
            assert_eq!(
                &raw[pose + index * 20 + 4..pose + index * 20 + 20],
                &rebuilt[rebuilt_pose + index * 20 + 4..rebuilt_pose + index * 20 + 20]
            );
        }
        assert_eq!(description.include_models, ["models/shared/animations.mdl"]);
        let keyvalues = int(&raw, 312);
        let rebuilt_keyvalues = int(rebuilt, 312);
        let size = int(&raw, 316);
        assert_eq!(size, int(rebuilt, 316));
        assert_eq!(
            &raw[keyvalues..keyvalues + size],
            &rebuilt[rebuilt_keyvalues..rebuilt_keyvalues + size]
        );
    }
}

#[test]
fn metadata_rejects_truncated_tables_invalid_bones_and_nonfinite_matrices_atomically() {
    let (_, raw, model) = fixture(false);
    let attachment = int(&raw, 244);
    let bone = int(&raw, 160);
    for (at, bytes) in [
        (bone + 4, 999i32.to_le_bytes()),
        (bone + 4, (-2i32).to_le_bytes()),
        (240, (-1i32).to_le_bytes()),
        (240, 65_537i32.to_le_bytes()),
        (244, (raw.len() as i32 - 10).to_le_bytes()),
        (attachment + 8, 999i32.to_le_bytes()),
        (attachment + 12, f32::NAN.to_le_bytes()),
        (attachment + 12, 2f32.to_le_bytes()),
        (attachment, (-1i32).to_le_bytes()),
        (300, i32::MAX.to_le_bytes()),
        (316, 2_000_000i32.to_le_bytes()),
    ] {
        let mut broken = raw.clone();
        broken[at..at + 4].copy_from_slice(&bytes);
        let mut qc = String::from("untouched");
        assert!(
            write_metadata(&mut qc, &broken, &model).is_err(),
            "offset {at}"
        );
        assert_eq!(qc, "untouched");
    }
}

#[test]
fn metadata_rejects_structurally_unsafe_keyvalues() {
    assert_eq!(quoted("$include").unwrap(), "\"$include\"");
    for data in [
        "mdlkeyvalue { } $modelname evil.mdl }",
        "mdlkeyvalue { prop_data { key value }",
        "mdlkeyvalue { prop_data { key \"unterminated } }",
        "mdlkeyvalue { prop_data { key \"}\" } }",
    ] {
        let mut output = String::from("untouched");
        assert!(write_keyvalues(&mut output, data).is_err(), "{data}");
        assert_eq!(output, "untouched");
    }
}

#[test]
fn multiple_hitbox_sets_empty_sets_and_bone_controllers_roundtrip() {
    let (directory, _, _) = fixture(false);
    let path = directory.path().join("input.qc");
    let mut source = fs::read_to_string(&path).unwrap();
    source.push_str("$hboxset alternate\n$hbox 3 root -9 -8 -7 6 5 4 alternate_box\n$hboxset damage\n$hbox 4 root -1 -2 -3 3 2 1 second_box\n$hboxset empty\n$controller 0 root x -3 5\n$controller 1 root yr 0 360\n$controller mouth root zr -20 30\n");
    fs::write(&path, source).unwrap();
    let description = qc::parse_qc_file(&path).unwrap();
    let original = pipeline::build(
        &description,
        directory.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    let raw = original.mdl;
    let model = mdl::parse_mdl(&raw, &vformats::Limits::default()).unwrap();
    let mut text =
        "$modelname models/tests/rebuilt.mdl\n$body body rig.smd\n$sequence idle rig.smd\n"
            .to_owned();
    let report = write_metadata(&mut text, &raw, &model).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(report.preserved.contains(&172));
    assert!(report.preserved.contains(&164));
    fs::write(directory.path().join("rebuilt.qc"), text).unwrap();
    let description = qc::parse_qc_file(&directory.path().join("rebuilt.qc")).unwrap();
    let rebuilt = pipeline::build(
        &description,
        directory.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap()
    .mdl;
    assert_eq!(int(&raw, 172), 3);
    assert_eq!(int(&rebuilt, 172), 3);
    let reader = MetadataReader(&raw);
    let rebuilt_reader = MetadataReader(&rebuilt);
    for (a, b) in reader
        .table(172, 176, 12)
        .unwrap()
        .iter()
        .zip(rebuilt_reader.table(172, 176, 12).unwrap())
    {
        assert_eq!(
            reader.relative_string(*a, 0).unwrap(),
            rebuilt_reader.relative_string(b, 0).unwrap()
        );
        let boxes = reader.relative_table(*a, 4, 8, 68).unwrap();
        let rebuilt_boxes = rebuilt_reader.relative_table(b, 4, 8, 68).unwrap();
        assert_eq!(boxes.len(), rebuilt_boxes.len());
        for (a, b) in boxes.into_iter().zip(rebuilt_boxes) {
            assert_eq!(&raw[a..a + 32], &rebuilt[b..b + 32]);
            assert_eq!(
                reader.optional_relative_string(a, 32).unwrap(),
                rebuilt_reader.optional_relative_string(b, 32).unwrap()
            );
        }
    }
    let a = int(&raw, 168);
    let b = int(&rebuilt, 168);
    assert_eq!(int(&raw, 164), 3);
    assert_eq!(int(&rebuilt, 164), 3);
    assert_eq!(&raw[a..a + 3 * 56], &rebuilt[b..b + 3 * 56]);
    let a = int(&raw, 160);
    let b = int(&rebuilt, 160);
    assert_eq!(&raw[a + 8..a + 32], &rebuilt[b + 8..b + 32]);
    let controller = int(&raw, 168);
    for (offset, value) in [
        (controller, 999i32.to_le_bytes()),
        (controller + 4, 3i32.to_le_bytes()),
        (controller + 8, f32::NAN.to_le_bytes()),
        (controller + 16, 1i32.to_le_bytes()),
        (controller + 20, 5i32.to_le_bytes()),
        (168, i32::MAX.to_le_bytes()),
    ] {
        let mut malformed = raw.clone();
        malformed[offset..offset + 4].copy_from_slice(&value);
        let mut output = "untouched".to_owned();
        assert!(write_metadata(&mut output, &malformed, &model).is_err());
        assert_eq!(output, "untouched");
    }
}

#[test]
fn full_decompile_recompile_preserves_metadata_on_static_and_rigged_models() {
    for static_prop in [false, true] {
        let (directory, raw, _) = fixture(static_prop);
        let destination = directory.path().join("decompiled");
        let report = super::super::decompile_model(
            &directory.path().join("models/tests/metadata.mdl"),
            &destination,
        )
        .unwrap();
        for label in [
            "Hitbox sets",
            "Attachments",
            "Pose parameters",
            "Included models",
        ] {
            assert!(
                !report
                    .warnings
                    .iter()
                    .any(|warning| warning.contains(label))
            );
        }
        let description = qc::parse_qc_file(&report.qc_path).unwrap();
        let output = pipeline::build(
            &description,
            &destination,
            pipeline::PipelineOptions::default(),
        )
        .unwrap();
        let source_attachment = int(&raw, 244);
        let destination_attachment = int(&output.mdl, 244);
        for field in (12..60).step_by(4) {
            assert!(
                (float(&raw, source_attachment + field)
                    - float(&output.mdl, destination_attachment + field))
                .abs()
                    < 0.00001
            );
        }
        assert_eq!(int(&raw, 172), int(&output.mdl, 172));
        assert_eq!(int(&raw, 300), int(&output.mdl, 300));
        assert_eq!(description.include_models, ["models/shared/animations.mdl"]);
        let start = int(&raw, 312);
        let rebuilt_start = int(&output.mdl, 312);
        let size = int(&raw, 316);
        assert_eq!(size, int(&output.mdl, 316));
        assert_eq!(
            &raw[start..start + size],
            &output.mdl[rebuilt_start..rebuilt_start + size]
        );
    }
}

#[test]
fn ik_chain_autoplay_lock_and_mouth_metadata_recompile_without_changing_bone_links() {
    let (directory, _, _) = fixture(false);
    let source = directory.path().join("input.qc");
    let mut text = fs::read_to_string(&source).unwrap();
    text.push_str("$ikchain leg tip knee 1 0 0\n$ikautoplaylock leg 0.5 0.75\n$model face rig.smd {\n mouth 0 open mid 1 0.5 -0.25\n}\n");
    fs::write(&source, text).unwrap();
    let desc = qc::parse_qc_file(&source).unwrap();
    let original = pipeline::build(
        &desc,
        directory.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    let model = mdl::parse_mdl(&original.mdl, &vformats::Limits::default()).unwrap();
    let mut text =
        "$modelname models/tests/metadata_rebuilt.mdl\n$body rig rig.smd\n$sequence idle rig.smd\n"
            .to_owned();
    let report = write_metadata(&mut text, &original.mdl, &model).unwrap();
    assert!(report.preserved.contains(&284));
    assert!(report.preserved.contains(&320));
    text.push_str("$model face rig.smd {\n");
    text.push_str(&mouth_options(&original.mdl, &model).unwrap());
    text.push_str("}\n");
    fs::write(directory.path().join("rebuilt.qc"), text).unwrap();
    let desc = qc::parse_qc_file(&directory.path().join("rebuilt.qc")).unwrap();
    let rebuilt = pipeline::build(
        &desc,
        directory.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    for (count, offset, stride) in [(284, 288, 16), (320, 324, 32), (292, 296, 20)] {
        assert_eq!(int(&original.mdl, count), int(&rebuilt.mdl, count));
        let original_base = int(&original.mdl, offset);
        let rebuilt_base = int(&rebuilt.mdl, offset);
        let skip = if count == 284 { 4 } else { 0 };
        assert_eq!(
            &original.mdl[original_base + skip..original_base + stride],
            &rebuilt.mdl[rebuilt_base + skip..rebuilt_base + stride]
        );
        if count == 284 {
            let source_link = original_base + int(&original.mdl, original_base + 12);
            let rebuilt_link = rebuilt_base + int(&rebuilt.mdl, rebuilt_base + 12);
            assert_eq!(
                &original.mdl[source_link..source_link + 84],
                &rebuilt.mdl[rebuilt_link..rebuilt_link + 84]
            );
        }
    }
}

#[test]
fn version_44_unnamed_hitboxes_accept_legacy_absolute_empty_string_offsets() {
    let (_, mut raw, model) = fixture(false);
    let set = int(&raw, 176);
    let hitbox = set + int(&raw, set + 8);
    raw[4..8].copy_from_slice(&44i32.to_le_bytes());
    let empty = raw.len();
    raw.push(0);
    raw[hitbox + 32..hitbox + 36].copy_from_slice(&(empty as i32).to_le_bytes());
    let mut text = String::new();
    write_metadata(&mut text, &raw, &model).unwrap();
    assert!(text.contains("$hbox 2 \"root\" -2 -3 -4 5 6 7 \"\""));
    raw[4..8].copy_from_slice(&49i32.to_le_bytes());
    assert!(write_metadata(&mut String::new(), &raw, &model).is_err());
}

#[test]
fn explicit_bones_preserve_unused_roots_and_reference_transforms() {
    let (directory, _, _) = fixture(false);
    let path = directory.path().join("input.qc");
    let mut source = fs::read_to_string(&path).unwrap();
    source.push_str("$definebone unused \"\" 3 -5 7 13 27 -41\n");
    fs::write(&path, source).unwrap();
    let description = qc::parse_qc_file(&path).unwrap();
    let original = pipeline::build(
        &description,
        directory.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    pipeline::write_files(&original, directory.path()).unwrap();
    let destination = directory.path().join("decompiled_unused");
    let report = super::super::decompile_model(
        &directory.path().join("models/tests/metadata.mdl"),
        &destination,
    )
    .unwrap();
    let description = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt = pipeline::build(
        &description,
        &destination,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    let before = mdl::parse_mdl(&original.mdl, &vformats::Limits::default()).unwrap();
    let after = mdl::parse_mdl(&rebuilt.mdl, &vformats::Limits::default()).unwrap();
    assert_eq!(before.bones.len(), after.bones.len());
    let a = int(&original.mdl, 160);
    let b = int(&rebuilt.mdl, 160);
    for (index, (source, target)) in before.bones.iter().zip(&after.bones).enumerate() {
        assert_eq!(source.name, target.name);
        assert_eq!(source.parent, target.parent);
        for field in (32..144)
            .step_by(4)
            .filter(|field| !(72..96).contains(field))
        {
            assert!(
                (float(&original.mdl, a + index * 216 + field)
                    - float(&rebuilt.mdl, b + index * 216 + field))
                .abs()
                    < 0.0001,
                "bone {} field {field}",
                source.name
            );
        }
        assert_eq!(
            int(&original.mdl, a + index * 216 + 160) & 0x40000,
            int(&rebuilt.mdl, b + index * 216 + 160) & 0x40000
        );
    }
    assert!(before.bones.iter().any(|bone| bone.name == "unused"));
}

#[test]
fn illumination_position_remains_linked_to_its_original_bone() {
    let (directory, _, _) = fixture(false);
    let path = directory.path().join("input.qc");
    let mut source = fs::read_to_string(&path).unwrap();
    source.push_str("$illumposition 2 -3 5 root\n");
    fs::write(&path, source).unwrap();
    let description = qc::parse_qc_file(&path).unwrap();
    let original = pipeline::build(
        &description,
        directory.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    pipeline::write_files(&original, directory.path()).unwrap();
    let destination = directory.path().join("decompiled_illum");
    let report = super::super::decompile_model(
        &directory.path().join("models/tests/metadata.mdl"),
        &destination,
    )
    .unwrap();
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("illumination")),
        "{:?}",
        report.warnings
    );
    let description = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt = pipeline::build(
        &description,
        &destination,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    let original_header2 = int(&original.mdl, 400);
    let rebuilt_header2 = int(&rebuilt.mdl, 400);
    assert_eq!(
        int(&original.mdl, original_header2 + 8),
        int(&rebuilt.mdl, rebuilt_header2 + 8)
    );
    let source_attachments = int(&original.mdl, 244);
    let target_attachments = int(&rebuilt.mdl, 244);
    assert_eq!(int(&original.mdl, 240), int(&rebuilt.mdl, 240));
    for index in 0..int(&original.mdl, 240) {
        for field in (12..60).step_by(4) {
            assert!(
                (float(&original.mdl, source_attachments + index * 92 + field)
                    - float(&rebuilt.mdl, target_attachments + index * 92 + field))
                .abs()
                    < 0.0001
            );
        }
    }
}

#[test]
fn procedural_jiggle_and_quaternion_helpers_roundtrip_through_standard_qc_and_vrd() {
    let (directory, _, _) = fixture(false);
    let path = directory.path().join("input.qc");
    let mut source = fs::read_to_string(&path).unwrap();
    source.push_str("$proceduralbones helpers.vrd\n$jigglebone tip { is_rigid { length 13 tip_mass 3 angle_constraint 45 yaw_constraint -20 30 pitch_constraint -40 50 yaw_friction 0.2 pitch_bounce 0.1 } is_flexible { yaw_stiffness 32 pitch_stiffness 44 along_stiffness 67 yaw_damping 2 pitch_damping 3 along_damping 4 allow_length_flex } has_base_spring { base_mass 5 stiffness 12 damping 4 left_constraint -2 3 up_constraint -4 5 forward_constraint -6 7 left_friction 0.1 up_friction 0.2 forward_friction 0.3 } }\n");
    fs::write(&path, source).unwrap();
    fs::write(directory.path().join("helpers.vrd"), "<helper> \"mid\" \"root\" \"\" \"root\"\n<display> 1 2 3\n<basepos> 1 -2 3\n<trigger> 90 20 35 -40 400 20 -30 5 -6 7\n<trigger> 45 -10 50 30 -20 40 70 -2 3 4\n").unwrap();
    let description = qc::parse_qc_file(&path).unwrap();
    assert_eq!(description.quat_interp_bones.len(), 1);
    let original = pipeline::build(
        &description,
        directory.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    pipeline::write_files(&original, directory.path()).unwrap();
    let destination = directory.path().join("decompiled_procedural");
    let report = super::super::decompile_model(
        &directory.path().join("models/tests/metadata.mdl"),
        &destination,
    )
    .unwrap();
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("Procedural")),
        "{:?}",
        report.warnings
    );
    assert!(
        fs::read_to_string(destination.join("procedural.vrd"))
            .unwrap()
            .starts_with("<helper>")
    );
    let description = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt = pipeline::build(
        &description,
        &destination,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    let a = MetadataReader(&original.mdl);
    let b = MetadataReader(&rebuilt.mdl);
    let source_bones = a.table(156, 160, 216).unwrap();
    let target_bones = b.table(156, 160, 216).unwrap();
    for (source, target) in source_bones.into_iter().zip(target_bones) {
        let kind = a.i32(source + 164).unwrap();
        assert_eq!(kind, b.i32(target + 164).unwrap());
        if kind == 0 {
            continue;
        }
        let source = a.relative(source, 168).unwrap();
        let target = b.relative(target, 168).unwrap();
        if kind == 5 {
            assert_eq!(a.i32(source).unwrap(), b.i32(target).unwrap());
            for field in (4..120).step_by(4) {
                assert!(
                    (a.float(source + field).unwrap() - b.float(target + field).unwrap()).abs()
                        < 0.00001,
                    "jiggle field {field}"
                );
            }
        } else {
            assert_eq!(a.i32(source).unwrap(), b.i32(target).unwrap());
            let source = a.relative_table(source, 4, 8, 48).unwrap();
            let target = b.relative_table(target, 4, 8, 48).unwrap();
            assert_eq!(source.len(), target.len());
            for (source, target) in source.into_iter().zip(target) {
                for field in (0..48).step_by(4) {
                    assert!(
                        (a.float(source + field).unwrap() - b.float(target + field).unwrap()).abs()
                            < 0.00001,
                        "trigger field {field}"
                    );
                }
            }
        }
    }
}

#[test]
fn malformed_procedural_offsets_counts_and_quaternions_cannot_write_output() {
    let (_, mut raw, model) = fixture(false);
    let bone = int(&raw, 160);
    let procedure = raw.len();
    raw.extend([0; 60]);
    raw[bone + 164..bone + 168].copy_from_slice(&2i32.to_le_bytes());
    raw[bone + 168..bone + 172].copy_from_slice(&((procedure - bone) as i32).to_le_bytes());
    raw[procedure + 4..procedure + 8].copy_from_slice(&1i32.to_le_bytes());
    raw[procedure + 8..procedure + 12].copy_from_slice(&12i32.to_le_bytes());
    for field in [12, 28, 56] {
        raw[procedure + field..procedure + field + 4].copy_from_slice(&1.0f32.to_le_bytes());
    }
    for (at, value) in [
        (bone + 168, 0i32.to_le_bytes()),
        (procedure, 999i32.to_le_bytes()),
        (procedure + 4, 65_537i32.to_le_bytes()),
        (procedure + 8, i32::MAX.to_le_bytes()),
        (procedure + 12, 0f32.to_le_bytes()),
        (procedure + 16, f32::NAN.to_le_bytes()),
        (procedure + 28, 2f32.to_le_bytes()),
    ] {
        let mut malformed = raw.clone();
        malformed[at..at + 4].copy_from_slice(&value);
        let directory = tempfile::tempdir().unwrap();
        let mut export = ExportFiles {
            directory: directory.path(),
            bytes: 0,
        };
        let mut qc = "untouched".to_owned();
        assert!(write_procedural(&mut qc, &mut export, &malformed, &model).is_err());
        assert_eq!(qc, "untouched");
        assert!(!directory.path().join("procedural.vrd").exists());
    }
    raw[bone + 164..bone + 168].copy_from_slice(&99i32.to_le_bytes());
    let directory = tempfile::tempdir().unwrap();
    let mut export = ExportFiles {
        directory: directory.path(),
        bytes: 0,
    };
    let warnings = write_procedural(&mut String::new(), &mut export, &raw, &model).unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("unsupported procedure type 99"));
}

#[test]
fn allowed_root_lods_header_recompiles_with_the_same_restriction() {
    let (directory, mut raw, model) = fixture(false);
    raw[0x17a] = 2;
    let mut text = "$modelname models/tests/metadata_rebuilt.mdl\n$body body rig.smd\n$sequence idle rig.smd\n".to_owned();
    write_metadata(&mut text, &raw, &model).unwrap();
    let description = qc::parse_qc_str(&text, directory.path()).unwrap();
    let rebuilt = pipeline::build(
        &description,
        directory.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    assert_eq!(rebuilt.mdl[0x17a], 2);
}

#[test]
fn stripped_root_lod_is_reported_and_invalid_root_lod_is_rejected() {
    let (_, mut raw, model) = fixture(false);
    raw[0x179] = 1;
    let report = write_metadata(&mut String::new(), &raw, &model).unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("higher-detail geometry was stripped"))
    );
    raw[0x179] = 8;
    assert!(write_metadata(&mut String::new(), &raw, &model).is_err());
}

#[test]
fn fixed_quaternion_alignment_roundtrips_with_honest_recalculation_warnings() {
    let (directory, _, _) = fixture(false);
    let mut animation =
        "version 1\nnodes\n0 \"root\" -1\n1 \"mid\" 0\n2 \"tip\" 1\nend\nskeleton\n".to_owned();
    for frame in 0..4 {
        writeln!(
            animation,
            "time {frame}\n0 0 0 0 0 0 0\n1 0 0 10 0 0 {}\n2 0 0 10 0 0 0",
            (frame as f32 * 30.0).to_radians()
        )
        .unwrap();
    }
    animation.push_str("end\n");
    fs::write(directory.path().join("sweep.smd"), animation).unwrap();
    for (label, selection) in [("calculated", ""), ("bind", " sweep")] {
        let description = qc::parse_qc_str(
            &format!("$modelname models/tests/metadata.mdl\n$body body rig.smd\n$sequence sweep sweep.smd\n$limitrotation mid{selection}\n"),
            directory.path(),
        ).unwrap();
        let original = pipeline::build(
            &description,
            directory.path(),
            pipeline::PipelineOptions::default(),
        )
        .unwrap();
        pipeline::write_files(&original, directory.path()).unwrap();
        let destination = directory.path().join(format!("alignment_{label}"));
        let report = super::super::decompile_model(
            &directory.path().join("models/tests/metadata.mdl"),
            &destination,
        )
        .unwrap();
        assert_eq!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("fixed quaternion alignment")),
            selection.is_empty(),
            "{:?}",
            report.warnings
        );
        let description = qc::parse_qc_file(&report.qc_path).unwrap();
        let rebuilt = pipeline::build(
            &description,
            &destination,
            pipeline::PipelineOptions::default(),
        )
        .unwrap();
        let source_bone = int(&original.mdl, 160) + 216;
        let target_bone = int(&rebuilt.mdl, 160) + 216;
        assert_ne!(int(&rebuilt.mdl, target_bone + 160) & 0x0010_0000, 0);
        for component in 0..4 {
            let source = float(&original.mdl, source_bone + 144 + component * 4);
            let target = float(&rebuilt.mdl, target_bone + 144 + component * 4);
            assert!(
                (source - target).abs() < 0.0002,
                "{label}: {source} != {target}"
            );
        }
    }
}

#[test]
fn malformed_fixed_quaternion_alignment_is_rejected_atomically() {
    let (_, raw, model) = fixture(false);
    let bone = int(&raw, 160) + 216;
    for alignment in [[0.0_f32; 4], [f32::NAN, 0.0, 0.0, 1.0]] {
        let mut broken = raw.clone();
        let flags = int(&raw, bone + 160) as u32 | 0x0010_0000;
        broken[bone + 160..bone + 164].copy_from_slice(&flags.to_le_bytes());
        for (component, value) in alignment.into_iter().enumerate() {
            broken[bone + 144 + component * 4..bone + 148 + component * 4]
                .copy_from_slice(&value.to_le_bytes());
        }
        let mut text = "untouched".to_owned();
        assert!(write_metadata(&mut text, &broken, &model).is_err());
        assert_eq!(text, "untouched");
    }
}

#[test]
fn stripped_minimum_lod_decompiles_and_recompiles_with_retained_geometry() {
    use super::super::{lods, reference_smd};
    use std::sync::atomic::AtomicBool;

    let directory = tempfile::tempdir().unwrap();
    let skeleton = "version 1\nnodes\n0 \"root\" -1\n1 \"detail\" 0\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 0 0 2 0 0 0\nend\ntriangles\n";
    let triangle = |material: &str, shift: i32| {
        format!(
            "{material}\n0 {shift} 0 0 0 0 1 0 0 2 0 0.75 1 0.25\n0 {} 0 0 0 0 1 1 0 1 0 1\n1 {shift} 2 0 0 0 1 0 1 1 1 1\n",
            shift + 2
        )
    };
    for (name, offsets) in [("high", vec![0, 4]), ("low", vec![0]), ("last", vec![8])] {
        let mut text = skeleton.to_owned();
        for (material, origin) in [("paint", 0), ("trim", 100)] {
            for shift in &offsets {
                text.push_str(&triangle(material, origin + shift));
            }
        }
        text.push_str("end\n");
        fs::write(directory.path().join(format!("{name}.smd")), text).unwrap();
    }
    let text = "$modelname models/tests/minlod.mdl\n$bodygroup choices {\n studio high.smd\n blank\n}\n$body extra high.smd\n$sequence idle high.smd\n$lod 25 { replacemodel high.smd low.smd replacematerial paint low_paint }\n$lod 75 { replacemodel high.smd last.smd replacematerial paint last_paint }\n$minlod 1\n$allowrootlods 3\n";
    let description = qc::parse_qc_str(text, directory.path()).unwrap();
    let original = pipeline::build(
        &description,
        directory.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    pipeline::write_files(&original, directory.path()).unwrap();
    let destination = directory.path().join("decompiled");
    let report = super::super::decompile_model(
        &directory.path().join("models/tests/minlod.mdl"),
        &destination,
    )
    .unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("higher-detail geometry was stripped"))
    );
    let text = fs::read_to_string(&report.qc_path).unwrap();
    assert!(text.contains("$minlod 1\n"));
    assert!(text.contains("$allowrootlods 3\n"));
    let description = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt = pipeline::build(
        &description,
        &destination,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    assert_eq!(&rebuilt.mdl[0x179..0x17b], &[1, 3]);
    let read_geometry = |output: &pipeline::PipelineOutput| {
        let limits = vformats::Limits::default();
        let model = vformats::mdl::parse_mdl(&output.mdl, &limits).unwrap();
        let vertices = vformats::mdl::parse_vvd(&output.vvd, &limits).unwrap();
        let topology = vformats::mdl::parse_vtx(&output.vtx, &limits).unwrap();
        let metadata = lods::Metadata::read(
            &output.mdl,
            &output.vvd,
            &output.vtx,
            &model,
            &vertices,
            &topology,
        )
        .unwrap();
        assert_eq!(metadata.switch_points, [0.0, 25.0, 75.0]);
        let mut geometry = Vec::new();
        for (group, (part, vtx_part)) in model
            .body_parts
            .iter()
            .zip(&topology.body_parts)
            .enumerate()
        {
            for (choice, (source, vtx_model)) in
                part.models.iter().zip(&vtx_part.models).enumerate()
            {
                for lod in &vtx_model.lods {
                    let smd = reference_smd(
                        &model,
                        source,
                        lod,
                        &vertices,
                        &metadata.mesh_vertices[group][choice],
                        &AtomicBool::new(false),
                    )
                    .unwrap();
                    let smd = mdlc::smd::parse_smd(&smd).unwrap();
                    let mut faces: Vec<_> = smd
                        .triangles
                        .iter()
                        .map(|triangle| {
                            let corners = triangle
                                .vertices
                                .each_ref()
                                .map(|vertex| format!("{vertex:?}"));
                            let face = (0..3)
                                .map(|start| {
                                    format!(
                                        "{}|{}|{}",
                                        corners[start],
                                        corners[(start + 1) % 3],
                                        corners[(start + 2) % 3]
                                    )
                                })
                                .min()
                                .unwrap();
                            format!("{}:{face}", triangle.material)
                        })
                        .collect();
                    faces.sort();
                    geometry.push(faces);
                }
            }
        }
        (metadata.material_replacements, geometry)
    };
    assert_eq!(read_geometry(&original), read_geometry(&rebuilt));
}

#[test]
fn lighting_flags_and_every_directional_light_byte_survive_metadata_reconstruction() {
    let (directory, mut raw, mut model) = fixture(true);
    let flags =
        (1 << 2) | (1 << 3) | (1 << 11) | (1 << 12) | (1 << 13) | (1 << 16) | (1 << 17) | (1 << 18);
    model.flags |= flags;
    for value in 0..=255 {
        raw[0x178] = value;
        let mut text = String::from(
            "$modelname tests/lighting.mdl\n$staticprop\n$body body rig.smd\n$sequence idle rig.smd\n",
        );
        write_metadata(&mut text, &raw, &model).unwrap();
        let description = qc::parse_qc_str(&text, directory.path()).unwrap();
        assert_eq!(
            description.model.extra_flags.unwrap() & flags as i32,
            flags as i32
        );
        assert_eq!(
            description.model.constant_directional_light_dot,
            Some(value)
        );
        if [0, 127, 255].contains(&value) {
            let output = pipeline::build(
                &description,
                directory.path(),
                pipeline::PipelineOptions::default(),
            )
            .unwrap();
            assert_eq!(output.mdl[0x178], value);
            assert_eq!(int(&output.mdl, 152) & flags as usize, flags as usize);
        }
    }
}
