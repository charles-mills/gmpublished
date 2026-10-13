use super::*;
use crate::{pipeline, qc};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> Fixture {
    static SERIAL: AtomicUsize = AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
        "mdlc_minlod_{}_{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).unwrap();
    let skeleton = "version 1\nnodes\n0 \"root\" -1\n1 \"detail\" 0\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 0 0 2 0 0 0\nend\n";
    let triangle = |material: &str, shift: i32| {
        format!(
            "{material}\n0 {shift} 0 0 0 0 1 0 0 2 0 0.75 1 0.25\n0 {} 0 0 0 0 1 1 0 1 0 1\n1 {shift} 2 0 0 0 1 0 1 1 1 1\n",
            shift + 2
        )
    };
    for (name, offsets) in [("high", vec![0, 4]), ("low", vec![0]), ("last", vec![8])] {
        let mut text = format!("{skeleton}triangles\n");
        for (material, origin) in [("paint", 0), ("trim", 100)] {
            for offset in &offsets {
                text.push_str(&triangle(material, origin + offset));
            }
        }
        text.push_str("end\n");
        fs::write(directory.join(format!("{name}.smd")), text).unwrap();
    }
    fs::write(directory.join("empty.smd"), skeleton).unwrap();
    let mut vta = skeleton.replace(
        "1 0 0 2 0 0 0\nend\n",
        "1 0 0 2 0 0 0\ntime 1\n0 0 0 0 0 0 0\n1 0 0 2 0 0 0\nend\n",
    );
    vta.push_str("vertexanimation\n");
    for frame in 0..2 {
        vta.push_str(&format!("time {frame}\n"));
        let mut index = 0;
        for origin in [0, 100] {
            for shift in [origin, origin + 4] {
                for (x, y) in [(shift, 0), (shift + 2, 0), (shift, 2)] {
                    vta.push_str(&format!("{index} {x} {y} {} 0 0 1\n", frame as f32 * 0.5));
                    index += 1;
                }
            }
        }
    }
    vta.push_str("end\n");
    fs::write(directory.join("shape.vta"), vta).unwrap();
    Fixture(directory)
}

fn build(fixture: &Fixture, min_lod: i32, allowed: i32) -> PipelineOutput {
    let text = format!(
        "$modelname models/tests/minlod.mdl\n$model body high.smd {{ flexfile shape.vta\nflex shape frame 1\n}}\n$bodygroup choices {{\n studio high.smd\n blank\n }}\n$sequence idle high.smd\n$lod 25 {{ replacemodel high.smd low.smd replacematerial paint low_paint }}\n$lod 75 {{ replacemodel high.smd last.smd replacematerial paint last_paint }}\n$minlod {min_lod}\n$allowrootlods {allowed}\n"
    );
    let description = qc::parse_qc_str(&text, &fixture.0).unwrap();
    pipeline::build(
        &description,
        &fixture.0,
        pipeline::PipelineOptions::default(),
    )
    .unwrap()
}

fn materialized(
    vvd: &Vvd,
    root: usize,
) -> (Vec<crate::vvd::VvdVertex>, Vec<crate::vvd::VvdTangent>) {
    if vvd.fixups.is_empty() {
        let count = vvd.header.num_lod_vertexes[root] as usize;
        return (
            vvd.vertices[..count].to_vec(),
            vvd.tangents[..count].to_vec(),
        );
    }
    let mut vertices = Vec::new();
    let mut tangents = Vec::new();
    for fixup in vvd.fixups.iter().filter(|fixup| fixup.lod >= root as i32) {
        let start = fixup.source_vertex_id as usize;
        let end = start + fixup.num_vertexes as usize;
        vertices.extend_from_slice(&vvd.vertices[start..end]);
        tangents.extend_from_slice(&vvd.tangents[start..end]);
    }
    (vertices, tangents)
}

#[test]
fn stripping_preserves_every_retained_lod_in_mdl_vvd_vtx_and_flexes() {
    let fixture = fixture();
    let original = build(&fixture, 0, 0);
    let vvd_before = Vvd::parse(&original.vvd).unwrap();
    assert!(vvd_before.fixups.len() > 3);
    let limits = vformats::Limits::default();
    let topology_before = vformats::mdl::parse_vtx(&original.vtx, &limits).unwrap();
    let bones = count(&original.mdl, 156).unwrap();
    let bone_records = relative(&original.mdl, 0, 160).unwrap();
    for root in [1, 2] {
        let output = build(&fixture, root, 1);
        let vvd_after = Vvd::parse(&output.vvd).unwrap();
        let topology_after = vformats::mdl::parse_vtx(&output.vtx, &limits).unwrap();
        assert_eq!(output.mdl[0x179], root as u8);
        assert_eq!(output.mdl[0x17a], root as u8 + 1);
        assert_eq!(vvd_after.header.num_lods, 3);
        assert!(output.vvd.len() < original.vvd.len());
        assert!(output.vtx.len() < original.vtx.len());
        assert_eq!(count(&output.vtx, 20).unwrap(), 3);
        for level in root as usize..3 {
            assert_eq!(
                materialized(&vvd_before, level),
                materialized(&vvd_after, level)
            );
        }
        for (before, after) in topology_before
            .body_parts
            .iter()
            .zip(&topology_after.body_parts)
        {
            for (before, after) in before.models.iter().zip(&after.models) {
                assert_eq!(after.lods.len(), 3);
                for level in 0..3 {
                    assert_eq!(after.lods[level], before.lods[level.max(root as usize)]);
                }
            }
        }
        for bone in 0..bones {
            assert_eq!(
                i32_at(&output.mdl, bone_records + bone * 216 + 160).unwrap(),
                i32_at(&original.mdl, bone_records + bone * 216 + 160).unwrap()
            );
        }
        let old_parts = relative(&original.mdl, 0, 0xec).unwrap();
        let new_parts = relative(&output.mdl, 0, 0xec).unwrap();
        let mut global_vertices = 0;
        let mut surviving_flex_vertices = 0;
        for part in 0..count(&original.mdl, 0xe8).unwrap() {
            let old_part = old_parts + part * 16;
            let new_part = new_parts + part * 16;
            for model in 0..count(&original.mdl, old_part + 4).unwrap() {
                let old_model = relative(&original.mdl, old_part, 12).unwrap() + model * 148;
                let new_model = relative(&output.mdl, new_part, 12).unwrap() + model * 148;
                assert_eq!(
                    count(&output.mdl, new_model + 84).unwrap(),
                    global_vertices * 48
                );
                assert_eq!(
                    count(&output.mdl, new_model + 88).unwrap(),
                    global_vertices * 16
                );
                let mut model_vertices = 0;
                for mesh in 0..count(&original.mdl, old_model + 72).unwrap() {
                    let old_mesh = relative(&original.mdl, old_model, 76).unwrap() + mesh * 116;
                    let new_mesh = relative(&output.mdl, new_model, 76).unwrap() + mesh * 116;
                    let selected_vertices =
                        count(&original.mdl, old_mesh + 52 + root as usize * 4).unwrap();
                    assert_eq!(count(&output.mdl, new_mesh + 8).unwrap(), selected_vertices);
                    assert_eq!(count(&output.mdl, new_mesh + 12).unwrap(), model_vertices);
                    assert_eq!(
                        &output.mdl[new_mesh + 52..new_mesh + 84],
                        &original.mdl[old_mesh + 52..old_mesh + 84]
                    );
                    model_vertices += selected_vertices;
                    for flex in 0..count(&original.mdl, old_mesh + 16).unwrap() {
                        let old_flex = relative(&original.mdl, old_mesh, 20).unwrap() + flex * 60;
                        let new_flex = relative(&output.mdl, new_mesh, 20).unwrap() + flex * 60;
                        let old_anims = relative(&original.mdl, old_flex, 24).unwrap();
                        let new_anims = relative(&output.mdl, new_flex, 24).unwrap();
                        let expected: Vec<_> = (0..count(&original.mdl, old_flex + 20).unwrap())
                            .map(|index| {
                                &original.mdl[old_anims + index * 16..old_anims + (index + 1) * 16]
                            })
                            .filter(|record| {
                                (u16::from_le_bytes(record[..2].try_into().unwrap()) as usize)
                                    < selected_vertices
                            })
                            .collect();
                        let actual: Vec<_> = (0..count(&output.mdl, new_flex + 20).unwrap())
                            .map(|index| {
                                &output.mdl[new_anims + index * 16..new_anims + (index + 1) * 16]
                            })
                            .collect();
                        assert_eq!(actual, expected);
                        surviving_flex_vertices += actual.len();
                    }
                }
                assert_eq!(count(&output.mdl, new_model + 80).unwrap(), model_vertices);
                global_vertices += model_vertices;
            }
        }
        assert_eq!(global_vertices, vvd_after.vertices.len());
        assert_eq!(surviving_flex_vertices, if root == 1 { 6 } else { 0 });
        let parts = relative(&output.vtx, 0, 32).unwrap();
        let model = relative(&output.vtx, parts, 4).unwrap();
        let lods = relative(&output.vtx, model, 4).unwrap();
        for (level, switch) in [0.0f32, 25.0, 75.0].into_iter().enumerate() {
            assert_eq!(
                &output.vtx[lods + level * 12 + 8..lods + level * 12 + 12],
                &switch.to_le_bytes()
            );
        }
        let lists = relative(&output.vtx, 0, 24).unwrap();
        for level in 0..root as usize {
            assert_eq!(
                relative(&output.vtx, lists + level * 8, 4).unwrap(),
                relative(&output.vtx, lists + root as usize * 8, 4).unwrap()
            );
        }
        assert_eq!(
            count(&output.mdl, 8).unwrap(),
            count(&output.vvd, 8).unwrap()
        );
        assert_eq!(
            count(&output.mdl, 8).unwrap(),
            count(&output.vtx, 16).unwrap()
        );
    }
}

#[test]
fn minimum_lod_clamps_to_available_levels_and_root_restrictions_are_order_independent() {
    let fixture = fixture();
    for requested in [255, 256, i32::MAX] {
        assert_eq!(build(&fixture, requested, 0).mdl[0x179], 2);
    }
    for text in ["$allowrootlods 1\n$minlod 2", "$minlod 2\n$allowrootlods 1"] {
        let description = qc::parse_qc_str(
            &format!("$body body high.smd\n$lod 10 {{ }}\n$lod 20 {{ }}\n{text}\n"),
            &fixture.0,
        )
        .unwrap();
        let output = pipeline::build(
            &description,
            &fixture.0,
            pipeline::PipelineOptions::default(),
        )
        .unwrap();
        assert_eq!(&output.mdl[0x179..0x17b], &[2, 3]);
    }
    let description = qc::parse_qc_str(
        "$body body high.smd\n$minlod 255\n$allowrootlods 1\n",
        &fixture.0,
    )
    .unwrap();
    let output = pipeline::build(
        &description,
        &fixture.0,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    assert_eq!(&output.mdl[0x179..0x17b], &[0, 1]);
    assert!(qc::parse_qc_str("$minlod -1\n", &fixture.0).is_err());
}

#[test]
fn truncated_or_invalid_binary_lod_records_are_rejected() {
    let fixture = fixture();
    for corruption in ["vtx_count", "vtx_offset", "mdl_mesh", "vvd_count"] {
        let mut output = build(&fixture, 0, 0);
        output.compiled.desc.model.minimum_lod = 1;
        match corruption {
            "vtx_count" => output.vtx[20..24].copy_from_slice(&99i32.to_le_bytes()),
            "vtx_offset" => output.vtx[32..36].copy_from_slice(&i32::MAX.to_le_bytes()),
            "mdl_mesh" => output.mdl.truncate(500),
            "vvd_count" => output.vvd[12..16].copy_from_slice(&99i32.to_le_bytes()),
            _ => unreachable!(),
        }
        assert!(apply(&mut output).is_err(), "{corruption}");
    }
}
