use mdlc::{
    pipeline::{self, PipelineOptions, PipelineOutput},
    qc,
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use vformats::{Limits, mdl};

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mdlc-output-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        for (name, text) in [
            ("cube.smd", include_str!("fixtures/cube.smd")),
            ("rig.smd", include_str!("fixtures/rig.smd")),
            ("rig_lod.smd", include_str!("fixtures/rig_lod.smd")),
            ("animation.smd", include_str!("fixtures/animation.smd")),
        ] {
            fs::write(dir.join(name), text).unwrap();
        }
        let cube = include_str!("fixtures/cube.smd");
        let (skeleton, faces) = cube.split_once("triangles\n").unwrap();
        let triangle = faces.lines().take(4).collect::<Vec<_>>().join("\n");
        fs::write(
            dir.join("cube_lod.smd"),
            format!("{skeleton}triangles\n{triangle}\nend\n"),
        )
        .unwrap();
        Self(dir)
    }

    fn desc(&self, text: &str) -> mdlc::model::ModelDesc {
        let path = self.0.join("model.qc");
        fs::write(&path, text).unwrap();
        qc::parse_qc_file(&path).unwrap_or_else(|errors| panic!("{errors:?}"))
    }

    fn build(&self, text: &str) -> PipelineOutput {
        pipeline::build(&self.desc(text), &self.0, PipelineOptions::default()).unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

const BASE: &str = "$modelname models/tests/fixture.mdl\n$cdmaterials models/tests\n$body body rig.smd\n$sequence idle rig.smd\n";

#[test]
fn default_fades_capture_declaration_order_and_allow_sequence_overrides() {
    let project = Project::new();
    let output = project.build("$modelname tests/fades.mdl\n$body body rig.smd\n$sequence first rig.smd\n$defaultfadein 0.4\n$defaultfadeout 0.8\n$sequence second rig.smd\n$sequence third rig.smd fadein 0.1 fadeout 0\n");
    let sequences = i32::from_le_bytes(output.mdl[192..196].try_into().unwrap()) as usize;
    for (index, expected) in [[0.2, 0.2], [0.4, 0.8], [0.1, 0.0]].iter().enumerate() {
        let sequence = sequences + index * 212;
        assert_eq!(f32_at(&output.mdl, sequence + 104), expected[0]);
        assert_eq!(f32_at(&output.mdl, sequence + 108), expected[1]);
    }
    for invalid in ["$defaultfadein -0.1", "$defaultfadeout NaN"] {
        assert!(qc::parse_qc_str(&format!("{BASE}{invalid}\n"), &project.0).is_err());
    }
}

#[test]
fn default_weight_list_changes_implicit_sequences_and_seeds_named_children() {
    let project = Project::new();
    let text = format!(
        "{BASE}$defaultweightlist {{ root 0 mid 0.5 posweight 0.25 }}\n$weightlist upper {{ tip 0.75 }}\n$sequence upper rig.smd weightlist upper\n"
    );
    let output = project.build(&text);
    let sequences = i32::from_le_bytes(output.mdl[192..196].try_into().unwrap()) as usize;
    for (index, expected) in [[0.0, 0.5, 0.5], [0.0, 0.5, 0.75]].iter().enumerate() {
        let sequence = sequences + index * 212;
        let weights = sequence
            + i32::from_le_bytes(
                output.mdl[sequence + 156..sequence + 160]
                    .try_into()
                    .unwrap(),
            ) as usize;
        for (bone, expected) in expected.iter().enumerate() {
            assert_eq!(f32_at(&output.mdl, weights + bone * 4), *expected);
        }
    }

    let replaced = project.desc(&format!(
        "{BASE}$defaultweightlist {{ mid 0.5 }}\n$defaultweightlist {{ root 0.25 }}\n"
    ));
    assert_eq!(
        mdlc::compile::resolve_default_weight_list(&replaced),
        [0.25; 3]
    );
    for invalid in [
        "$defaultweightlist { posweight 0.5 }",
        "$defaultweightlist { root 0 posweight 0.5 }",
        "$defaultweightlist { root 1",
    ] {
        assert!(qc::parse_qc_str(&format!("{BASE}{invalid}\n"), &project.0).is_err());
    }
}

fn f32_at(bytes: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn i16_at(bytes: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn half_at(bytes: &[u8], offset: usize) -> f32 {
    let bits = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = i32::from((bits >> 10) & 31);
    let mantissa = f32::from(bits & 1023);
    assert!(exponent < 31, "nonfinite position sample");
    sign * if exponent == 0 {
        mantissa * 2.0f32.powi(-24)
    } else {
        (1.0 + mantissa / 1024.0) * 2.0f32.powi(exponent - 15)
    }
}

fn rle_sample(bytes: &[u8], mut offset: usize, mut frame: usize) -> f32 {
    loop {
        let valid = usize::from(bytes[offset]);
        let total = usize::from(bytes[offset + 1]);
        assert!(valid > 0 && valid <= total);
        if frame < total {
            return f32::from(i16_at(bytes, offset + 2 + frame.min(valid - 1) * 2));
        }
        frame -= total;
        offset += 2 + valid * 2;
    }
}

// Section selection follows SDK studio.cpp; position samples follow bone_setup.cpp.
fn animation_position(out: &PipelineOutput, anim: usize, bone: usize, frame: usize) -> [f32; 3] {
    let bytes = &out.mdl;
    assert!(anim < u32_at(bytes, 180));
    let desc = u32_at(bytes, 184) + anim * 100;
    let frames = u32_at(bytes, desc + 16);
    assert!(frame < frames);
    assert_eq!(u32_at(bytes, desc + 52), 0, "fixture must be inline");
    let bone_at = u32_at(bytes, 160) + bone * 216;
    let bind = std::array::from_fn(|axis| f32_at(bytes, bone_at + 32 + axis * 4));
    let mut local_frame = frame;
    let mut record = desc + u32_at(bytes, desc + 56);
    let section_frames = u32_at(bytes, desc + 84);
    if section_frames > 0 {
        let section = if frames > section_frames && frame == frames - 1 {
            local_frame = 0;
            frames / section_frames + 1
        } else {
            local_frame = frame % section_frames;
            frame / section_frames
        };
        let entry = desc + u32_at(bytes, desc + 80) + section * 8;
        assert_eq!(u32_at(bytes, entry), 0);
        record = desc + u32_at(bytes, entry + 4);
    }
    let delta = u32_at(bytes, desc + 12) & 4 != 0;
    while bytes[record] < 255 {
        let flags = bytes[record + 1];
        if usize::from(bytes[record]) == bone {
            let data = record + 4;
            if flags & 1 != 0 {
                let position =
                    data + if flags & 2 != 0 { 6 } else { 0 } + if flags & 32 != 0 { 8 } else { 0 };
                return std::array::from_fn(|axis| half_at(bytes, position + axis * 2));
            }
            if flags & 4 != 0 {
                let ptr = data + if flags & 8 != 0 { 6 } else { 0 };
                return std::array::from_fn(|axis| {
                    let relative = i16_at(bytes, ptr + axis * 2);
                    assert!(relative >= 0);
                    let sample = if relative == 0 {
                        0.0
                    } else {
                        rle_sample(bytes, ptr + relative as usize, local_frame)
                    };
                    sample * f32_at(bytes, bone_at + 72 + axis * 4)
                        + if delta { 0.0 } else { bind[axis] }
                });
            }
            break;
        }
        let next = i16_at(bytes, record + 2);
        if next == 0 {
            break;
        }
        assert!(next > 0);
        record += next as usize;
    }
    if delta { [0.0; 3] } else { bind }
}

fn assert_position(actual: [f32; 3], expected: [f32; 3]) {
    for axis in 0..3 {
        assert!(
            (actual[axis] - expected[axis]).abs() < 0.02,
            "{actual:?} != {expected:?}"
        );
    }
}

#[test]
fn segmented_animation_preserves_every_frame_including_exact_section_multiples() {
    let project = Project::new();
    for count in [120, 121, 300, 301] {
        let mut source = String::from(
            "version 1\nnodes\n0 \"root\" -1\n1 \"mid\" 0\n2 \"tip\" 1\nend\nskeleton\n",
        );
        for frame in 0..count {
            source.push_str(&format!(
                "time {frame}\n0 0 0 0 0 0 0\n1 0 0 {} 0 0 0\n2 0 0 10 0 0 0\n",
                10.0 + frame as f32 * 0.25
            ));
        }
        source.push_str("end\n");
        fs::write(project.0.join("long.smd"), source).unwrap();
        let out = project.build("$modelname models/tests/frames.mdl\n$body body rig.smd\n$sectionframes 30 120\n$sequence sample long.smd\n");
        for frame in 0..count {
            assert_position(
                animation_position(&out, 0, 1, frame),
                [0.0, 0.0, 10.0 + frame as f32 * 0.25],
            );
        }
    }
}

#[test]
fn source_frame_ranges_and_named_animation_scale_reach_serialized_channels() {
    let project = Project::new();
    let prefix = "$modelname models/tests/frames.mdl\n$body body rig.smd\n";
    for sequence in [
        "$sequence sample animation.smd frames 1 2 scale 2\n",
        "$animation shared animation.smd\n$sequence sample shared frames 1 2 scale 2\n",
        "$animation shared animation.smd\n$sequence before shared\n$sequence sample shared frames 1 2 scale 2\n",
        "$animation shared animation.smd\n$scale 3\n$sequence sample shared frames 1 2 scale 2\n",
    ] {
        let out = project.build(&format!("{prefix}{sequence}"));
        for anim in 0..u32_at(&out.mdl, 180) {
            let desc = u32_at(&out.mdl, 184) + anim * 100;
            assert_eq!(u32_at(&out.mdl, desc + 16), 2);
            for (frame, expected) in [22.0, 24.0].into_iter().enumerate() {
                for bone in [1, 2] {
                    assert_position(
                        animation_position(&out, anim, bone, frame),
                        [0.0, 0.0, expected],
                    );
                }
            }
        }
    }
}

#[test]
fn empty_static_animation_terminator_preserves_the_bind_pose() {
    let project = Project::new();
    let out = project.build(&format!("{BASE}$staticprop\n"));
    let desc = u32_at(&out.mdl, 184);
    let chain = desc + u32_at(&out.mdl, desc + 56);
    assert_eq!(&out.mdl[chain..chain + 4], &[255, 0, 0, 0]);
    let bone = u32_at(&out.mdl, 160);
    assert_position(
        animation_position(&out, 0, 0, 0),
        std::array::from_fn(|axis| f32_at(&out.mdl, bone + 32 + axis * 4)),
    );
}

fn u32_at(bytes: &[u8], offset: usize) -> usize {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
}

fn lod_positions(bytes: &[u8], lod: usize) -> Vec<[f32; 3]> {
    let count = u32_at(bytes, 16 + lod * 4);
    let fixups = u32_at(bytes, 48);
    let fixup_offset = u32_at(bytes, 52);
    let vertex_offset = u32_at(bytes, 56);
    let mut source_indices = Vec::new();
    if fixups == 0 {
        source_indices.extend(0..count);
    } else {
        for i in 0..fixups {
            let fixup = fixup_offset + i * 12;
            if u32_at(bytes, fixup) >= lod {
                let start = u32_at(bytes, fixup + 4);
                source_indices.extend(start..start + u32_at(bytes, fixup + 8));
            }
        }
    }
    assert_eq!(source_indices.len(), count);
    source_indices
        .into_iter()
        .map(|index| {
            let pos = vertex_offset + index * 48 + 16;
            std::array::from_fn(|axis| {
                f32::from_le_bytes(
                    bytes[pos + axis * 4..pos + axis * 4 + 4]
                        .try_into()
                        .unwrap(),
                )
            })
        })
        .collect()
}

// Studio_SetRootLOD recomputes model/mesh offsets from numLODVertexes.
fn check_lod_pools(out: &PipelineOutput, indices: &mdl::Vtx, full: &mdl::Vvd) {
    for lod in 0..u32_at(&out.vvd, 12) {
        let pool = lod_positions(&out.vvd, lod);
        let mut vertex_base = 0;
        let mut full_vertex_base = 0;
        let bodyparts = u32_at(&out.mdl, 236);
        for (bi, bp) in indices.body_parts.iter().enumerate() {
            let bodypart = bodyparts + bi * 16;
            let models = bodypart + u32_at(&out.mdl, bodypart + 12);
            for (mi, model) in bp.models.iter().enumerate() {
                let model_at = models + mi * 148;
                let meshes = model_at + u32_at(&out.mdl, model_at + 76);
                for (ki, mesh) in model.lods[lod].meshes.iter().enumerate() {
                    let mesh_at = meshes + ki * 116;
                    let count = u32_at(&out.mdl, mesh_at + 52 + lod * 4);
                    let full_count = u32_at(&out.mdl, mesh_at + 52);
                    for group in &mesh.strip_groups {
                        for vertex in &group.vertices {
                            let local = usize::from(vertex.original_mesh_vertex_id);
                            assert!(
                                local < count,
                                "LOD {lod} references a vertex removed by its fixups"
                            );
                            assert_eq!(
                                pool[vertex_base + local],
                                full.vertices[full_vertex_base + local].position
                            );
                        }
                    }
                    vertex_base += count;
                    full_vertex_base += full_count;
                }
            }
        }
        assert_eq!(vertex_base, pool.len());
    }
}

fn decode(out: &PipelineOutput) -> (mdl::Mdl, mdl::Vvd, Vec<Vec<Vec<usize>>>) {
    let limits = Limits::default();
    let model = mdl::parse_mdl(&out.mdl, &limits).unwrap();
    let vertices = mdl::parse_vvd(&out.vvd, &limits).unwrap();
    let indices = mdl::parse_vtx(&out.vtx, &limits).unwrap();
    check_lod_pools(out, &indices, &vertices);
    mdl::assemble(&model, &vertices, &indices).unwrap();
    assert_eq!(model.checksum, vertices.checksum);
    assert_eq!(model.checksum, indices.checksum);
    assert_eq!(model.body_parts.len(), indices.body_parts.len());
    let counts = model
        .body_parts
        .iter()
        .zip(&indices.body_parts)
        .map(|(mbp, vbp)| {
            assert_eq!(mbp.models.len(), vbp.models.len());
            mbp.models
                .iter()
                .zip(&vbp.models)
                .map(|(mm, vm)| {
                    assert!(!vm.lods.is_empty());
                    vm.lods
                        .iter()
                        .map(|lod| {
                            assert_eq!(mm.meshes.len(), lod.meshes.len());
                            mm.meshes
                                .iter()
                                .zip(&lod.meshes)
                                .map(|(mesh, vmesh)| {
                                    let mut index_count = 0;
                                    for group in &vmesh.strip_groups {
                                        for strip in &group.strips {
                                            for slot in strip.triangle_index_positions() {
                                                let vertex = &group.vertices
                                                    [usize::from(group.indices[slot])];
                                                let offset = mm.vertex_offset
                                                    + mesh.vertex_offset
                                                    + i32::from(vertex.original_mesh_vertex_id);
                                                assert!(offset >= 0);
                                                assert!(
                                                    (offset as usize) < vertices.vertices.len()
                                                );
                                                index_count += 1;
                                            }
                                        }
                                    }
                                    assert_eq!(index_count % 3, 0);
                                    index_count / 3
                                })
                                .sum()
                        })
                        .collect()
                })
                .collect()
        })
        .collect();
    (model, vertices, counts)
}

#[test]
fn bodygroup_choices_and_skin_slots_survive_serialization() {
    let project = Project::new();
    let out = project.build(&format!("{BASE}$bodygroup accessory {{\nstudio rig.smd\nstudio rig.smd\n}}\n$texturegroup skins {{\n{{ material }}\n{{ replacement }}\n}}\n"));
    let (model, _, counts) = decode(&out);
    assert_eq!(counts, vec![vec![vec![36]], vec![vec![36], vec![36]]]);
    assert_eq!(model.skin_reference_count, 2);
    assert_eq!(model.skin_table, [0, 1, 1, 1]);
    assert_eq!(model.textures, ["material", "replacement"]);
}

#[test]
fn blank_bodygroup_choice_preserves_choice_order_and_has_no_geometry() {
    let project = Project::new();
    let out = project.build(&format!("{BASE}$bodygroup accessory {{\nblank\nstudio cube.smd\n}}\n$lod 20 {{\nreplacemodel rig.smd rig_lod.smd\n}}\n"));
    let (model, _, counts) = decode(&out);
    assert_eq!(
        counts,
        vec![vec![vec![36, 6]], vec![vec![0, 0], vec![12, 12]]]
    );
    assert_eq!(model.body_parts[1].models[0].name, "blank");
}

#[test]
fn global_lods_replace_matching_sources_and_keep_unmatched_models() {
    let project = Project::new();
    let out = project.build(&format!("{BASE}$bodygroup accessory {{\nstudio rig.smd\nstudio cube.smd\n}}\n$lod 20 {{\nreplacemodel rig rig_lod.smd\n}}\n$lod 40 {{\nreplacemodel cube.smd cube_lod.smd\n}}\n"));
    let (_, _, counts) = decode(&out);
    assert_eq!(
        counts,
        vec![
            vec![vec![36, 6, 36]],
            vec![vec![36, 6, 36], vec![12, 12, 1]]
        ]
    );
}

#[test]
fn multiple_replacements_in_one_lod_are_all_applied() {
    let project = Project::new();
    let out = project.build(&format!("{BASE}$bodygroup accessory {{\nstudio cube.smd\n}}\n$lod 20 {{\nreplacemodel rig.smd rig_lod.smd\nreplacemodel cube.smd cube_lod.smd\n}}\n"));
    let (_, _, counts) = decode(&out);
    assert_eq!(counts, vec![vec![vec![36, 6]], vec![vec![12, 1]]]);
}

#[test]
fn uneven_library_lods_keep_models_visible_at_every_global_level() {
    let project = Project::new();
    let mut desc = project.desc(&format!("{BASE}$bodygroup accessory {{\nstudio cube.smd\n}}\n$lod 20 {{\nreplacemodel rig.smd rig_lod.smd\n}}\n"));
    desc.bodyparts[1].models[0].lods.clear();
    let out = pipeline::build(&desc, &project.0, PipelineOptions::default()).unwrap();
    let (_, _, counts) = decode(&out);
    assert_eq!(counts, vec![vec![vec![36, 6]], vec![vec![12, 12]]]);
}

#[test]
fn zero_lod_bodygroup_record_fails_the_compiler_self_check() {
    let project = Project::new();
    let out = project.build(&format!(
        "{BASE}$bodygroup accessory {{\nstudio cube.smd\nstudio cube.smd\n}}\n"
    ));
    let mut vtx = mdlc::vtx_writer::write_vtx(&out.compiled).unwrap();
    let read = |off| i32::from_le_bytes(vtx.bytes[off..off + 4].try_into().unwrap()) as usize;
    let bodypart = read(32) + 8;
    let second_model = bodypart + read(bodypart + 4) + 8;
    vtx.bytes[second_model..second_model + 4].fill(0);
    assert!(mdlc::vtx_writer::check_invariants(&vtx, &out.compiled).is_err());
}

#[test]
fn malformed_vtx_offsets_return_errors_without_panicking() {
    let project = Project::new();
    let out = project.build(BASE);
    let valid = mdlc::vtx_writer::write_vtx(&out.compiled).unwrap();
    for length in [0, 4, 35, valid.bytes.len() - 1] {
        let mut truncated = valid.clone();
        truncated.bytes.truncate(length);
        assert!(mdlc::vtx_writer::check_invariants(&truncated, &out.compiled).is_err());
    }
    let mut invalid = valid;
    invalid.bytes[32..36].copy_from_slice(&i32::MAX.to_le_bytes());
    assert!(mdlc::vtx_writer::check_invariants(&invalid, &out.compiled).is_err());
}

#[test]
fn unsafe_output_paths_are_rejected_before_export() {
    let project = Project::new();
    let mut desc = project.desc(BASE);
    for name in [
        "../escape.mdl",
        "models/../../escape.mdl",
        "/tmp/escape.mdl",
        "C:\\escape.mdl",
        "\\\\server\\share\\escape.mdl",
        "models\\..\\escape.mdl",
    ] {
        desc.model.name = name.into();
        assert!(
            pipeline::build(&desc, &project.0, PipelineOptions::default()).is_err(),
            "{name}"
        );
    }
    let mut out = project.build(BASE);
    out.stem = "../escaped".into();
    assert!(pipeline::write_files(&out, &project.0.join("selected")).is_err());
    assert!(!project.0.join("escaped.mdl").exists());
}

#[cfg(unix)]
#[test]
fn output_symlinks_cannot_redirect_any_generated_file() {
    use std::os::unix::fs::symlink;
    let project = Project::new();
    let mut out = project.build(BASE);
    let selected = project.0.join("selected");
    let outside = project.0.join("outside");
    fs::create_dir_all(&selected).unwrap();
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, selected.join("models")).unwrap();
    assert!(pipeline::write_files(&out, &selected).is_err());
    assert!(!outside.join("tests").exists());
    out.stem = "fixture".into();
    fs::write(outside.join("unchanged.mdl"), "unchanged").unwrap();
    symlink(outside.join("unchanged.mdl"), selected.join("fixture.mdl")).unwrap();
    assert!(pipeline::write_files(&out, &selected).is_err());
    assert_eq!(
        fs::read_to_string(outside.join("unchanged.mdl")).unwrap(),
        "unchanged"
    );
    assert!(!selected.join("fixture.vvd").exists());
}

#[test]
fn transformed_root_positions_survive_constant_and_sampled_animation_encoding() {
    let project = Project::new();
    let reference = include_str!("fixtures/cube.smd")
        .replace("time 0\n0 0 0 0 0 0 0", "time 0\n0 10 20 30 0 0 0");
    fs::write(project.0.join("reference.smd"), reference).unwrap();
    for sampled in [false, true] {
        let mut animation = String::from(
            "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 13 25 37 0 0 0\n",
        );
        if sampled {
            animation.push_str("time 1\n0 14 26 38 0 0 0\n");
        }
        animation.push_str("end\n");
        fs::write(project.0.join("root.smd"), animation).unwrap();
        for (transform, expected) in [
            ("", [-25.0, 13.0, 37.0]),
            ("$origin 2 4 6 90\n", [-11.0, -21.0, 31.0]),
            ("$origin 2 4 6\n$upaxis y\n", [31.0, 11.0, 21.0]),
        ] {
            let out = project.build(&format!("$modelname models/tests/root.mdl\n$body body reference.smd\n{transform}$sequence sample root.smd\n"));
            assert_position(animation_position(&out, 0, 0, 0), expected);
        }
    }
}

#[test]
fn static_qc_transform_keeps_collision_aligned_with_render_geometry() {
    let project = Project::new();
    let out = project.build("$modelname models/tests/static_transform.mdl\n$staticprop\n$origin 1 2 3\n$upaxis y\n$body body cube.smd\n$sequence idle cube.smd\n$collisionmodel cube.smd { $mass 10 }\n");
    let phy = vformats::phy::parse(out.phy.as_ref().unwrap(), &Limits::default()).unwrap();
    let mut minimum = [f32::INFINITY; 3];
    let mut maximum = [f32::NEG_INFINITY; 3];
    for point in phy
        .solids
        .iter()
        .flat_map(|solid| &solid.ledges)
        .flat_map(|ledge| &ledge.vertices)
    {
        for axis in 0..3 {
            minimum[axis] = minimum[axis].min(point[axis]);
            maximum[axis] = maximum[axis].max(point[axis]);
        }
    }
    assert_position(minimum, [-3.0, 0.0, 0.0]);
    assert_position(maximum, [5.0, 4.0, 6.0]);
}

#[test]
fn constant_directional_light_writes_quantized_byte_without_overwriting_lod_settings() {
    let project = Project::new();
    for (value, expected) in [(0.0, 0), (0.1, 25), (0.5, 127), (1.0, 255)] {
        let output = project.build(&format!(
            "{BASE}$staticprop\n$constantdirectionallight {value}\n$allowrootlods 3\n"
        ));
        assert_eq!(output.mdl[0x178], expected);
        assert_eq!(&output.mdl[0x179..0x17c], &[0, 3, 0]);
        let flags = i32::from_le_bytes(output.mdl[152..156].try_into().unwrap());
        assert_ne!(flags & (1 << 13), 0);
    }
    for value in ["NaN", "inf", "-0.1", "1.1"] {
        assert!(
            qc::parse_qc_str(
                &format!("{BASE}$constantdirectionallight {value}\n"),
                &project.0,
            )
            .is_err()
        );
    }
}
