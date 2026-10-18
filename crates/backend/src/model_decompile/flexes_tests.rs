use std::fs;

use mdlc::{pipeline, qc};

use super::*;

fn fixture(pair: bool, negative_split: bool, eyelid: bool) -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let positions = [
        [-2.0, 0.0, 0.0],
        [-0.5, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.5, 5.0, 0.0],
        [2.0, 5.0, 0.0],
        [0.0, 6.0, 0.0],
    ];
    let mut smd = String::from(
        "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\n",
    );
    for (triangle, vertices) in positions.chunks(3).enumerate() {
        writeln!(smd, "material_{triangle}").unwrap();
        for (index, [x, y, z]) in vertices.iter().enumerate() {
            writeln!(
                smd,
                "0 {x} {y} {z} 0 0 1 {} {} 1 0 1",
                usize::from(index == 1),
                usize::from(index == 2)
            )
            .unwrap();
        }
    }
    smd.push_str("end\n");
    fs::write(project.path().join("face.smd"), smd).unwrap();
    let frame_count = if eyelid { 3 } else { 1 };
    let mut vta = String::from("version 1\nnodes\n0 \"root\" -1\nend\nskeleton\n");
    for frame in 0..=frame_count {
        writeln!(vta, "time {frame}\n0 0 0 0 0 0 0").unwrap();
    }
    vta.push_str("end\nvertexanimation\n");
    for frame in 0..=frame_count {
        writeln!(vta, "time {frame}").unwrap();
        for (index, [x, y, z]) in positions.iter().enumerate() {
            let offset = if frame == 0 {
                0.0
            } else {
                [1.0, 0.5, 0.25, 0.75, 0.125, 0.0][index] * frame as f32
            };
            writeln!(vta, "{index} {x} {y} {} 0 0 1", z + offset).unwrap();
        }
    }
    vta.push_str("end\n");
    fs::write(project.path().join("face.vta"), vta).unwrap();
    let shape = if eyelid {
        "eyelid lid face.vta lowerer 1 -0.5 neutral 2 0.1 raiser 3 0.7\n".to_owned()
    } else if pair {
        format!(
            "flexfile face.vta\nflexpair smile {} frame 1 decay 0.8\n",
            if negative_split { -2 } else { 2 }
        )
    } else {
        "flexfile face.vta\nflex smile frame 1 decay 0.8\n".to_owned()
    };
    let mut options = shape;
    if pair {
        options.push_str("flexcontroller phoneme range -1 1 right_smile left_smile\n%smileR = max(0,right_smile)\n%smileL = min(1,left_smile*0.5)\n");
    } else if !eyelid {
        options.push_str("localvar intermediate\nflexcontroller phoneme range -1 1 smile\n%intermediate = (smile+0.25)*0.5\n%smile = max(0,min(1,%intermediate))\n");
    }
    let qc_path = project.path().join("input.qc");
    fs::write(&qc_path, format!("$modelname models/face.mdl\n$model face face.smd {{\n{options}}}\n$sequence idle face.smd\n")).unwrap();
    let desc = qc::parse_qc_file(&qc_path).unwrap();
    let output =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&output, project.path()).unwrap();
    project
}

fn parsed(raw: &[u8]) -> Mdl {
    vformats::mdl::parse_mdl(raw, &vformats::Limits::default()).unwrap()
}

fn shapes(raw: &[u8]) -> Vec<Shape> {
    let model = parsed(raw);
    let reader = Reader(raw);
    let body = reader.table(232, 236, 16).unwrap()[0];
    let source = reader.relative_table(body, 4, 12, 148).unwrap()[0];
    let meshes = reader.relative_table(source, 72, 76, 116).unwrap();
    let counts = meshes
        .into_iter()
        .map(|mesh| reader.count(mesh + 8).unwrap())
        .collect::<Vec<_>>();
    read_shapes(
        &reader,
        source,
        &model.body_parts[0].models[0],
        &counts,
        reader.count(260).unwrap(),
    )
    .unwrap()
}

type CanonicalDelta = ([u32; 3], u8, u8, [u16; 3], [u16; 3]);
type CanonicalShape = (usize, usize, [u32; 4], Vec<CanonicalDelta>);

fn canonical(mut shapes: Vec<Shape>, raw_vvd: &[u8]) -> Vec<CanonicalShape> {
    let vertices = vformats::mdl::parse_vvd(raw_vvd, &vformats::Limits::default()).unwrap();
    shapes.sort_by_key(|shape| {
        (
            shape.descriptor,
            shape.pair,
            shape.targets.map(f32::to_bits),
        )
    });
    shapes
        .into_iter()
        .map(|shape| {
            let mut deltas = shape
                .deltas
                .into_iter()
                .map(|delta| {
                    (
                        vertices.vertices[delta.vertex].position.map(f32::to_bits),
                        delta.speed,
                        delta.side,
                        delta.delta.map(mdlc::anim_writer::float_to_half_public),
                        delta.normal.map(mdlc::anim_writer::float_to_half_public),
                    )
                })
                .collect::<Vec<_>>();
            deltas.sort();
            (
                shape.descriptor,
                shape.pair,
                shape.targets.map(f32::to_bits),
                deltas,
            )
        })
        .collect()
}

#[test]
fn facial_flexes_roundtrip_sparse_deltas_mesh_indices_split_speed_and_rules() {
    for (pair, negative, eyelid) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (false, false, true),
    ] {
        let project = fixture(pair, negative, eyelid);
        let source = project.path().join("models/face.mdl");
        let raw = fs::read(&source).unwrap();
        let original_vvd = fs::read(source.with_extension("vvd")).unwrap();
        let destination = project.path().join("exported");
        let report = super::super::decompile_model(&source, &destination).unwrap();
        let desc = qc::parse_qc_file(&report.qc_path).unwrap();
        let output =
            pipeline::build(&desc, &destination, pipeline::PipelineOptions::default()).unwrap();
        assert_eq!(
            canonical(shapes(&raw), &original_vvd),
            canonical(shapes(&output.mdl), &output.vvd),
            "pair={pair}, negative={negative}, eyelid={eyelid}"
        );
        let old = Reader(&raw);
        let new = Reader(&output.mdl);
        for (count, offset, stride, name) in [(260, 264, 4, 0), (268, 272, 20, 4)] {
            assert_eq!(
                old.names(count, offset, stride, name).unwrap(),
                new.names(count, offset, stride, name).unwrap()
            );
        }
        let rules = |reader: &Reader<'_>| {
            reader
                .table(276, 280, 12)
                .unwrap()
                .into_iter()
                .map(|record| {
                    let ops = reader
                        .relative_table(record, 4, 8, 8)
                        .unwrap()
                        .into_iter()
                        .map(|op| (reader.signed(op).unwrap(), reader.signed(op + 4).unwrap()))
                        .collect::<Vec<_>>();
                    (reader.signed(record).unwrap(), ops)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(rules(&old), rules(&new));
        assert!(
            !report
                .warnings
                .iter()
                .any(|warning| warning.starts_with("Flexes"))
        );
        let vta =
            mdlc::vta::parse_vta(&fs::read_to_string(destination.join("flexes_0_0.vta")).unwrap())
                .unwrap();
        assert!(vta.frames[1].len() < vta.frames[0].len());
    }
}

#[test]
fn corrupt_flex_offsets_indices_wrinkles_and_rules_fail_atomically() {
    for corruption in 0..5 {
        let project = fixture(true, false, false);
        let source = project.path().join("models/face.mdl");
        let mut raw = fs::read(&source).unwrap();
        let reader = Reader(&raw);
        let body = reader.table(232, 236, 16).unwrap()[0];
        let model = reader.relative_table(body, 4, 12, 148).unwrap()[0];
        let mesh = reader.relative_table(model, 72, 76, 116).unwrap()[0];
        let shape = reader.relative_table(mesh, 16, 20, 60).unwrap()[0];
        let delta = reader.relative_table(shape, 20, 24, 16).unwrap()[0];
        let rule = reader.table(276, 280, 12).unwrap()[0];
        let op = reader.relative_table(rule, 4, 8, 8).unwrap()[0];
        match corruption {
            0 => raw[shape + 24..shape + 28].copy_from_slice(&i32::MAX.to_le_bytes()),
            1 => raw[delta..delta + 2].copy_from_slice(&u16::MAX.to_le_bytes()),
            2 => raw[shape + 32] = 1,
            3 => raw[op..op + 4].copy_from_slice(&99i32.to_le_bytes()),
            _ => raw[delta + 4..delta + 6].copy_from_slice(&0x7c00u16.to_le_bytes()),
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

fn evaluate_rule(raw: &[u8], inputs: &[f32]) -> f32 {
    let reader = Reader(raw);
    let rule = reader.table(276, 280, 12).unwrap()[0];
    let mut stack = Vec::<f32>::new();
    for record in reader.relative_table(rule, 4, 8, 8).unwrap() {
        let opcode = reader.count(record).unwrap();
        match opcode {
            1 => stack.push(reader.float(record + 4).unwrap()),
            2 => stack.push(inputs[reader.count(record + 4).unwrap()]),
            8 => *stack.last_mut().unwrap() *= -1.0,
            4..=7 | 13 | 14 => {
                let right = stack.pop().unwrap();
                let left = stack.pop().unwrap();
                stack.push(match opcode {
                    4 => left + right,
                    5 => left - right,
                    6 => left * right,
                    7 => {
                        if right > 0.0001 {
                            left / right
                        } else {
                            0.0
                        }
                    }
                    13 => left.max(right),
                    14 => left.min(right),
                    _ => unreachable!(),
                });
            }
            _ => panic!("unexpected opcode {opcode}"),
        }
    }
    assert_eq!(stack.len(), 1);
    stack[0]
}

#[test]
fn advanced_facial_rules_recompile_as_standard_qc_with_equivalent_results() {
    let project = fixture(false, false, false);
    let constant = |value: f32| (1i32, value.to_bits() as i32);
    let fetch = |index: i32| (2i32, index);
    let cases = [
        vec![(15, 0)],
        vec![(16, 0)],
        vec![fetch(0), fetch(1), fetch(2), (18, 3)],
        vec![fetch(0), fetch(1), fetch(2), (19, 2)],
        vec![
            constant(-1.0),
            constant(-0.5),
            constant(0.5),
            constant(1.0),
            constant(0.0),
            (17, 1),
        ],
        vec![constant(0.0), constant(1.0), constant(2.0), (20, 1)],
        vec![constant(0.0), constant(1.0), constant(2.0), (21, 1)],
    ];
    let controllers = ["a".to_owned(), "b".to_owned(), "c".to_owned()];
    let ranges = [(-1.0, 1.0), (0.0, 1.0), (-2.0, 2.0)];
    for (case, ops) in cases.into_iter().enumerate() {
        let raw = ops
            .iter()
            .flat_map(|(opcode, value)| opcode.to_le_bytes().into_iter().chain(value.to_le_bytes()))
            .collect::<Vec<_>>();
        let records = (0..ops.len()).map(|index| index * 8).collect::<Vec<_>>();
        let expression = expression(&Reader(&raw), &records, &[], &controllers, &ranges).unwrap();
        let text = format!(
            "$modelname models/rules.mdl\n$model face face.smd {{\nlocalvar smile\nflexcontroller test range -1 1 a\nflexcontroller test range 0 1 b\nflexcontroller test range -2 2 c\n%smile = {expression}\n}}\n$sequence idle face.smd\n"
        );
        let desc = qc::parse_qc_str(&text, project.path()).unwrap();
        let output =
            pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
        for a in [-1.5f32, -1.0, -0.75, -0.5, 0.0, 0.5, 0.75, 1.0, 1.5] {
            for b in [0.0, 0.3, 1.0] {
                for c in [-3.0f32, -1.5, 0.0, 1.5, 3.0] {
                    let expected = match case {
                        0 => (-a).clamp(0.0, 1.0),
                        1 => a.clamp(0.0, 1.0),
                        2 => a * b * c,
                        3 => a * (1.0 - b * c),
                        4 => {
                            if a <= -1.0 || a >= 1.0 {
                                0.0
                            } else if a < -0.5 {
                                (a + 1.0) * 2.0 * b
                            } else if a > 0.5 {
                                (1.0 - a) * 2.0 * b
                            } else {
                                b
                            }
                        }
                        5 => {
                            (1.0 - a.clamp(-1.0, 1.0).max(0.0))
                                * (1.0 - b)
                                * ((c + 2.0) / 4.0).clamp(0.0, 1.0)
                        }
                        6 => {
                            (1.0 + a.clamp(-1.0, 1.0).min(0.0))
                                * b
                                * ((c + 2.0) / 4.0).clamp(0.0, 1.0)
                        }
                        _ => unreachable!(),
                    };
                    let actual = evaluate_rule(&output.mdl, &[a, b, c]);
                    assert!(
                        (actual - expected).abs() < 1e-6,
                        "case {case}, {a},{b},{c}: {actual} != {expected}; {expression}"
                    );
                }
            }
        }
    }
}

#[test]
fn speed_recovery_preserves_half_deltas_when_quantization_changes_the_decay_interval() {
    let mut frame = vec![
        VtaVert {
            index: 0,
            pos: [0.0, 0.0, 0.9301758],
            normal: [0.0, 0.0, 1.0],
        },
        VtaVert {
            index: 1,
            pos: [0.0, 0.0, 1.9931641],
            normal: [0.0, 0.0, 1.0],
        },
    ];
    let base = frame
        .iter()
        .map(|vertex| VtaVert {
            pos: [0.0; 3],
            ..*vertex
        })
        .collect::<Vec<_>>();
    let shape = Shape {
        descriptor: 0,
        pair: 0,
        targets: [0.0, 1.0, 10.0, 11.0],
        deltas: frame
            .iter()
            .zip([118, 255])
            .enumerate()
            .map(|(index, (vertex, speed))| Delta {
                mesh: 0,
                index,
                vertex: index,
                speed,
                side: 0,
                delta: vertex.pos,
                normal: [0.0; 3],
            })
            .collect(),
    };
    let decay = infer_decay(&shape, &base, &mut frame).unwrap();
    let maximum = frame
        .iter()
        .map(|vertex| vertex.pos[2])
        .fold(0.0f32, f32::max);
    for (vertex, delta) in frame.iter().zip(&shape.deltas) {
        assert!(same_half(vertex.pos, delta.delta));
        assert_eq!(
            (255.0 * (vertex.pos[2] / (maximum * decay)).clamp(0.0, 1.0)) as u8,
            delta.speed
        );
    }
}

#[test]
fn eyeballs_and_named_eyelid_control_links_roundtrip_in_bone_space() {
    let project = fixture(false, false, false);
    let input = project.path().join("input.qc");
    let text = fs::read_to_string(&input).unwrap().replace("flexfile face.vta", "eyeball eye0 root 0 -2 1 material_0 2 5 material_0 0.5\neyeball eye1 root 1 -2 1 material_1 2 -5 material_1 0.8\neyelid upper0 face.vta lowerer 1 -0.5 neutral 0 0 raiser 1 0.5 eyeball eye0\neyelid lower0 face.vta lowerer 1 -0.5 neutral 0 0 raiser 1 0.5 eyeball eye0\neyelid upper1 face.vta lowerer 1 -0.5 neutral 0 0 raiser 1 0.5 eyeball eye1\neyelid lower1 face.vta lowerer 1 -0.5 neutral 0 0 raiser 1 0.5 eyeball eye1\nflexfile face.vta");
    fs::write(&input, text).unwrap();
    let desc = qc::parse_qc_file(&input).unwrap();
    let original =
        pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
    pipeline::write_files(&original, project.path()).unwrap();
    let destination = project.path().join("eyes");
    let report =
        super::super::decompile_model(&project.path().join("models/face.mdl"), &destination)
            .unwrap();
    let desc = qc::parse_qc_file(&report.qc_path).unwrap();
    let rebuilt =
        pipeline::build(&desc, &destination, pipeline::PipelineOptions::default()).unwrap();
    let eyes = |raw: &[u8]| {
        let reader = Reader(raw);
        let body = reader.table(232, 236, 16).unwrap()[0];
        let model = reader.relative_table(body, 4, 12, 148).unwrap()[0];
        reader
            .relative_table(model, 100, 104, 172)
            .unwrap()
            .into_iter()
            .map(|eye| {
                let floats = [
                    8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 60, 92, 96, 100, 104, 108, 112,
                ]
                .map(|offset| reader.float(eye + offset).unwrap());
                let links = [4, 68, 72, 76, 80, 84, 88, 116, 120]
                    .map(|offset| reader.signed(eye + offset).unwrap());
                (floats, links)
            })
            .collect::<Vec<_>>()
    };
    for ((old, old_links), (new, new_links)) in
        eyes(&original.mdl).into_iter().zip(eyes(&rebuilt.mdl))
    {
        assert_eq!(old_links, new_links);
        for (old, new) in old.into_iter().zip(new) {
            assert!((old - new).abs() < 0.0001, "{old} != {new}");
        }
    }
    assert_eq!(
        canonical(shapes(&original.mdl), &original.vvd),
        canonical(shapes(&rebuilt.mdl), &rebuilt.vvd)
    );
}

#[test]
fn speed_recovery_selects_a_representable_float_inside_the_byte_interval() {
    use mdlc::vta::VtaVert;
    let positions = [
        [0.134011, -4.655037, 64.764595],
        [0.371213, -4.448651, 64.66576],
    ];
    let deltas = [
        [-0.00006997585, -0.00025701523, 0.01663208],
        [-0.0029506683, 0.044525146, -0.20727539],
    ];
    let base = positions
        .into_iter()
        .enumerate()
        .map(|(index, pos)| VtaVert {
            index: index as u32,
            pos,
            normal: [0.0; 3],
        })
        .collect::<Vec<_>>();
    let mut frame = base
        .iter()
        .zip(deltas)
        .map(|(base, delta)| VtaVert {
            pos: std::array::from_fn(|axis| absolute(base.pos[axis], delta[axis]).unwrap()),
            ..*base
        })
        .collect::<Vec<_>>();
    let shape = Shape {
        descriptor: 0,
        pair: 0,
        targets: [0.0, 1.0, 10.0, 11.0],
        deltas: deltas
            .into_iter()
            .zip([19, 255])
            .enumerate()
            .map(|(index, (delta, speed))| Delta {
                mesh: 0,
                index,
                vertex: index,
                speed,
                side: 0,
                delta,
                normal: [0.0; 3],
            })
            .collect(),
    };
    let decay = infer_decay(&shape, &base, &mut frame).unwrap();
    let lengths = frame
        .iter()
        .zip(&base)
        .map(|(frame, base)| {
            (0..3)
                .map(|axis| (frame.pos[axis] - base.pos[axis]).powi(2))
                .sum::<f32>()
                .sqrt()
        })
        .collect::<Vec<_>>();
    let maximum = lengths.iter().copied().fold(0.0f32, f32::max);
    for ((frame, base), (delta, length)) in frame
        .iter()
        .zip(&base)
        .zip(shape.deltas.iter().zip(lengths))
    {
        assert!(same_half(
            std::array::from_fn(|axis| frame.pos[axis] - base.pos[axis]),
            delta.delta
        ));
        assert_eq!(
            (255.0 * (length / (maximum * decay)).clamp(0.0, 1.0)) as u8,
            delta.speed
        );
    }
}

#[test]
fn consecutive_flex_positions_preserve_piecewise_activation_windows() {
    for pair in [false, true] {
        let project = fixture(pair, false, false);
        let input = project.path().join("input.qc");
        let text = fs::read_to_string(&input).unwrap();
        let command = if pair {
            "flexpair smile 2"
        } else {
            "flex smile"
        };
        let original = format!("{command} frame 1 decay 0.8");
        let replacement = [0.5, 0.8, 1.0]
            .map(|position| format!("{command} frame 1 position {position} decay 0.8"))
            .join("\n");
        assert!(text.contains(&original));
        fs::write(&input, text.replace(&original, &replacement)).unwrap();
        let desc = qc::parse_qc_file(&input).unwrap();
        let output =
            pipeline::build(&desc, project.path(), pipeline::PipelineOptions::default()).unwrap();
        let original_shapes = shapes(&output.mdl);
        let mut targets = original_shapes
            .iter()
            .map(|shape| shape.targets)
            .collect::<Vec<_>>();
        targets.sort_by(|a, b| a[1].total_cmp(&b[1]));
        assert_eq!(
            targets,
            [
                [0.0, 0.5, 0.5, 0.8],
                [0.5, 0.8, 0.8, 1.0],
                [0.8, 1.0, 10.0, 11.0]
            ]
        );
        pipeline::write_files(&output, project.path()).unwrap();
        let destination = project.path().join("positions");
        let report =
            super::super::decompile_model(&project.path().join("models/face.mdl"), &destination)
                .unwrap();
        let rebuilt = qc::parse_qc_file(&report.qc_path).unwrap();
        let rebuilt =
            pipeline::build(&rebuilt, &destination, pipeline::PipelineOptions::default()).unwrap();
        assert_eq!(
            canonical(original_shapes, &output.vvd),
            canonical(shapes(&rebuilt.mdl), &rebuilt.vvd)
        );
    }
}
