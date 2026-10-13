use super::*;
use mdlc::{pipeline, qc};
use std::fs;

fn fixture(joints: bool, concave: bool) -> (tempfile::TempDir, pipeline::PipelineOutput, Mdl) {
    let directory = tempfile::tempdir().unwrap();
    let rig = include_str!("../../../vendor/mdlc/tests/fixtures/rig.smd");
    fs::write(directory.path().join("rig.smd"), rig).unwrap();
    let mut collision = mdlc::smd::parse_smd(rig).unwrap();
    if joints || concave {
        let mut second = collision.triangles.clone();
        for triangle in &mut second {
            for vertex in &mut triangle.vertices {
                vertex.position[0] += if joints { 7.0 } else { 25.0 };
                vertex.position[2] += if joints { 10.0 } else { 0.0 };
                if joints {
                    vertex.parent_bone = 1;
                    vertex.links[0].bone = 1;
                }
            }
        }
        if joints && concave {
            let mut third = second.clone();
            for triangle in &mut third {
                for vertex in &mut triangle.vertices {
                    vertex.position[0] += 10.0;
                }
            }
            collision.triangles.extend(third);
        }
        collision.triangles.extend(second);
    }
    let mut text = String::from(
        "version 1\nnodes\n0 \"root\" -1\n1 \"mid\" 0\n2 \"tip\" 1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 0 0 10 0 0 0\n2 0 0 10 0 0 0\nend\ntriangles\n",
    );
    for triangle in collision.triangles {
        text.push_str("material\n");
        for vertex in triangle.vertices {
            let bone = vertex.parent_bone;
            writeln!(
                text,
                "{bone} {} {} {} 0 0 1 0 0 1 {bone} 1",
                vertex.position[0], vertex.position[1], vertex.position[2]
            )
            .unwrap();
        }
    }
    text.push_str("end\n");
    fs::write(directory.path().join("physics.smd"), text).unwrap();
    let mut text = "$modelname models/tests/physics.mdl\n$surfaceprop metal\n$body body rig.smd\n$bonemerge mid\n$sequence idle rig.smd\n".to_owned();
    writeln!(text, "$collision{} physics.smd\n{{\n$mass 42\n$damping 0.2\n$rotdamping 0.5\n$inertia 2\n$drag 0.1", if joints { "joints" } else { "model" }).unwrap();
    if concave {
        text.push_str("$concave\n");
        if joints {
            text.push_str("$concaveperjoint\n");
        }
    }
    if joints {
        text.push_str("$rootbone root\n$jointdamping mid 0.3\n$jointrotdamping mid 0.7\n$jointinertia mid 3\n$jointmassbias mid 2\n$jointsurfaceprop mid wood\n$jointconstrain mid x limit -35 45 7.5\n$jointconstrain mid y free 0 0 5\n$jointconstrain mid z fixed 0 0 0\n$jointcollide root mid\n$animatedfriction 1 8 2 3 4\n");
    }
    text.push_str("}\n$collisiontext { break { \"model\" \"props/test_piece.mdl\" \"health\" \"10\" } break { \"model\" \"props/test_piece_2.mdl\" \"$include\" \"literal.qci\" } }\n");
    fs::write(directory.path().join("input.qc"), text).unwrap();
    let desc = qc::parse_qc_file(&directory.path().join("input.qc")).unwrap();
    let output = pipeline::build(
        &desc,
        directory.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    let model = vformats::mdl::parse_mdl(&output.mdl, &vformats::Limits::default()).unwrap();
    (directory, output, model)
}

fn assert_shapes_equal(original: &[u8], rebuilt: &[u8]) {
    let limits = vformats::Limits::default();
    let original = phy::parse(original, &limits).unwrap();
    let rebuilt = phy::parse(rebuilt, &limits).unwrap();
    assert_eq!(original.solids.len(), rebuilt.solids.len());
    for (source, target) in original.solids.iter().zip(&rebuilt.solids) {
        assert_eq!(source.ledges.len(), target.ledges.len());
        let mut unmatched: Vec<_> = target.ledges.iter().collect();
        for ledge in &source.ledges {
            let index = unmatched
                .iter()
                .position(|target| {
                    ledge.vertices.len() == target.vertices.len()
                        && ledge.vertices.iter().all(|point| {
                            target.vertices.iter().any(|other| {
                                point.iter().zip(other).all(|(a, b)| (a - b).abs() < 0.001)
                            })
                        })
                })
                .expect("collision hull changed during recompilation");
            unmatched.swap_remove(index);
        }
    }
    let original = original.text.unwrap().keyvalues(&limits).unwrap();
    let rebuilt = rebuilt.text.unwrap().keyvalues(&limits).unwrap();
    let original_solids: Vec<_> = original.blocks("solid").collect();
    let rebuilt_solids: Vec<_> = rebuilt.blocks("solid").collect();
    assert_eq!(original_solids.len(), rebuilt_solids.len());
    for (source, target) in original_solids.iter().zip(&rebuilt_solids) {
        for key in ["mass", "damping", "rotdamping", "inertia", "drag"] {
            assert!(
                (number(source, key).unwrap() - number(target, key).unwrap()).abs() < 0.001,
                "{key}"
            );
        }
        assert_eq!(source.get_str("surfaceprop"), target.get_str("surfaceprop"));
        assert_eq!(source.get_str("parent"), target.get_str("parent"));
    }
    for key in [
        "ragdollconstraint",
        "collisionrules",
        "animatedfriction",
        "break",
    ] {
        assert_eq!(
            original.blocks(key).collect::<Vec<_>>(),
            rebuilt.blocks(key).collect::<Vec<_>>(),
            "{key}"
        );
    }
}

#[test]
fn prop_concave_and_ragdoll_physics_roundtrip_without_losing_convex_pieces_or_joint_parameters() {
    for (joints, concave) in [(false, false), (false, true), (true, false), (true, true)] {
        let (directory, original, model) = fixture(joints, concave);
        let mut text = "$modelname models/tests/rebuilt.mdl\n$body body rig.smd\n$bonemerge mid\n$sequence idle rig.smd\n".to_owned();
        let mut export = ExportFiles {
            directory: directory.path(),
            bytes: 0,
        };
        let frame = &original.compiled.sequences[0].frames[0];
        let report = write_physics(
            &mut text,
            &mut export,
            &original.mdl,
            original.phy.as_deref(),
            &model,
            Some(frame),
        )
        .unwrap();
        assert!(report.preserved);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        fs::write(directory.path().join("rebuilt.qc"), text).unwrap();
        let desc = qc::parse_qc_file(&directory.path().join("rebuilt.qc")).unwrap();
        let rebuilt = pipeline::build(
            &desc,
            directory.path(),
            pipeline::PipelineOptions::default(),
        )
        .unwrap();
        assert_shapes_equal(
            original.phy.as_ref().unwrap(),
            rebuilt.phy.as_ref().unwrap(),
        );
    }
}

#[test]
fn physics_rejects_mismatched_checksums_and_absent_solid_bones_before_writing() {
    let (directory, original, model) = fixture(true, false);
    let mut phy = original.phy.clone().unwrap();
    phy[12] ^= 1;
    let mut text = String::from("untouched");
    let mut export = ExportFiles {
        directory: directory.path(),
        bytes: 0,
    };
    assert!(
        write_physics(
            &mut text,
            &mut export,
            &original.mdl,
            Some(&phy),
            &model,
            None
        )
        .is_err()
    );
    assert_eq!(text, "untouched");
    assert!(!directory.path().join("collision.smd").exists());
    let mut model = model;
    model.bones.clear();
    assert!(
        write_physics(
            &mut text,
            &mut export,
            &original.mdl,
            original.phy.as_deref(),
            &model,
            None
        )
        .is_err()
    );
    assert_eq!(text, "untouched");
}

#[test]
fn collision_text_requires_balanced_blocks_and_complete_pairs() {
    for text in [
        "solid { key value",
        "solid { key }",
        "solid { key value }}",
        "solid { key value } trailing",
        "solid { \"key\" \"unterminated }",
        "solid { key value }\0hidden",
    ] {
        assert!(
            validate_collision_text(text.as_bytes()).is_err(),
            "{text:?}"
        );
    }
    assert!(
        validate_collision_text(b"break { \"model\" \"a.mdl\" } break { model b.mdl }\n\0").is_ok()
    );
}

#[test]
fn collision_piece_limit_roundtrips_more_than_twenty_convex_pieces() {
    let (directory, _, _) = fixture(false, false);
    let original = fs::read_to_string(directory.path().join("physics.smd")).unwrap();
    let prefix = original.split_once("triangles\n").unwrap().0;
    let mesh = mdlc::smd::parse_smd(&original).unwrap();
    let mut geometry = format!("{prefix}triangles\n");
    for piece in 0..21 {
        for triangle in mesh
            .triangles
            .iter()
            .filter(|triangle| triangle.vertices[0].parent_bone == 0)
        {
            geometry.push_str("material\n");
            for vertex in &triangle.vertices {
                writeln!(
                    geometry,
                    "0 {} {} {} 0 0 1 0 0 1 0 1",
                    vertex.position[0] + piece as f32 * 10.0,
                    vertex.position[1],
                    vertex.position[2]
                )
                .unwrap();
            }
        }
    }
    geometry.push_str("end\n");
    fs::write(directory.path().join("physics.smd"), geometry).unwrap();
    let path = directory.path().join("input.qc");
    let source = fs::read_to_string(&path)
        .unwrap()
        .replace("$mass 42", "$mass 42\n$concave\n$maxconvexpieces 32");
    fs::write(&path, source).unwrap();
    let description = qc::parse_qc_file(&path).unwrap();
    assert_eq!(description.physics.max_convex_pieces, Some(32));
    assert!(description.physics.concave);
    let original = pipeline::build(
        &description,
        directory.path(),
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    pipeline::write_files(&original, directory.path()).unwrap();
    let physics = phy::parse(original.phy.as_ref().unwrap(), &vformats::Limits::default()).unwrap();
    assert_eq!(physics.solids[0].ledges.len(), 21, "{:?}", physics.stats);
    let destination = directory.path().join("decompiled_21_pieces");
    let report = super::super::decompile_model(
        &directory.path().join("models/tests/physics.mdl"),
        &destination,
    )
    .unwrap();
    let description = qc::parse_qc_file(&report.qc_path).unwrap();
    assert_eq!(description.physics.max_convex_pieces, Some(21));
    let rebuilt = pipeline::build(
        &description,
        &destination,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    assert_shapes_equal(
        original.phy.as_ref().unwrap(),
        rebuilt.phy.as_ref().unwrap(),
    );
}

#[test]
fn prop_collision_roundtrip_uses_the_first_sequence_pose_and_static_source_basis() {
    for static_prop in [false, true] {
        let (directory, _, _) = fixture(false, false);
        let animation = fs::read_to_string(directory.path().join("rig.smd"))
            .unwrap()
            .replace("0 0 0 0 0 0 0\n", "0 2 -3 5 0.3 -0.2 0.4\n");
        fs::write(directory.path().join("animation.smd"), animation).unwrap();
        let qc_path = directory.path().join("input.qc");
        let mut source = fs::read_to_string(&qc_path)
            .unwrap()
            .replace("$sequence idle rig.smd", "$sequence idle animation.smd");
        if static_prop {
            source.push_str("$staticprop\n");
        }
        fs::write(&qc_path, &source).unwrap();
        let desc = qc::parse_qc_file(&qc_path).unwrap();
        let original = pipeline::build(
            &desc,
            directory.path(),
            pipeline::PipelineOptions::default(),
        )
        .unwrap();
        let model = vformats::mdl::parse_mdl(&original.mdl, &vformats::Limits::default()).unwrap();
        let mut text = format!(
            "$modelname models/tests/rebuilt.mdl\n$body body rig.smd\n$bonemerge mid\n$sequence idle animation.smd\n{}",
            if static_prop { "$staticprop\n" } else { "" }
        );
        let mut export = ExportFiles {
            directory: directory.path(),
            bytes: 0,
        };
        write_physics(
            &mut text,
            &mut export,
            &original.mdl,
            original.phy.as_deref(),
            &model,
            Some(&original.compiled.sequences[0].frames[0]),
        )
        .unwrap();
        fs::write(directory.path().join("rebuilt.qc"), text).unwrap();
        let desc = qc::parse_qc_file(&directory.path().join("rebuilt.qc")).unwrap();
        let rebuilt = pipeline::build(
            &desc,
            directory.path(),
            pipeline::PipelineOptions::default(),
        )
        .unwrap();
        assert_shapes_equal(
            original.phy.as_ref().unwrap(),
            rebuilt.phy.as_ref().unwrap(),
        );
    }
}

#[test]
fn full_model_decompile_recompile_preserves_prop_and_ragdoll_collision() {
    for (joints, concave) in [(false, false), (false, true), (true, false), (true, true)] {
        let (directory, original, _) = fixture(joints, concave);
        pipeline::write_files(&original, directory.path()).unwrap();
        let destination = directory.path().join("decompiled");
        let report = super::super::decompile_model(
            &directory.path().join("models/tests/physics.mdl"),
            &destination,
        )
        .unwrap();
        assert!(
            !report
                .warnings
                .iter()
                .any(|warning| warning.starts_with("Physics and ragdoll"))
        );
        assert!(
            report
                .files
                .iter()
                .any(|file| file.ends_with("collision.smd"))
        );
        let desc = qc::parse_qc_file(&report.qc_path).unwrap();
        let rebuilt =
            pipeline::build(&desc, &destination, pipeline::PipelineOptions::default()).unwrap();
        assert_shapes_equal(
            original.phy.as_ref().unwrap(),
            rebuilt.phy.as_ref().unwrap(),
        );
    }
}
