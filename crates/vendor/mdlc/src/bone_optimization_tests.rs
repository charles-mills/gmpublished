use crate::{bone_math as math, compile, model::ModelDesc, pipeline, qc};
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
    let path = std::env::temp_dir().join(format!(
        "mdlc_bone_optimization_{}_{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    let skeleton = "version 1\nnodes\n0 \"root\" -1\n1 \"helper\" 0\n2 \"animated\" 1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 5 0 0 0 0 0\n2 0 0 3 0 0 0\n";
    fs::write(path.join("mesh.smd"),format!("{skeleton}end\ntriangles\npaint\n1 8 2 0 0 0 1 0 0 1 1 1\n1 12 2 0 0 0 1 1 0 1 1 1\n2 8 6 0 0 0 1 0 1 1 2 1\nend\n")).unwrap();
    fs::write(
        path.join("anim.smd"),
        format!("{skeleton}time 1\n0 2 1 0 0 0 0.2\n1 5 0 0 0 0 0\n2 0 0 7 0.1 0.2 0.3\nend\n"),
    )
    .unwrap();
    Fixture(path)
}
fn desc(fixture: &Fixture) -> ModelDesc {
    qc::parse_qc_str(
        "$modelname tests/bones.mdl\n$body body mesh.smd\n$sequence move anim.smd\n",
        &fixture.0,
    )
    .unwrap()
}
fn worlds(compiled: &crate::model::CompiledModelDesc, frame: usize) -> Vec<math::Matrix3x4> {
    let poses = &compiled.animations[0].frames[frame];
    math::compute_world(
        &poses.iter().map(|p| p.position).collect::<Vec<_>>(),
        &poses.iter().map(|p| p.rotation).collect::<Vec<_>>(),
        &compile::bone_parents(&compiled.desc),
    )
}
fn bind_worlds(compiled: &crate::model::CompiledModelDesc) -> Vec<math::Matrix3x4> {
    let poses: Vec<_> = (0..compiled.desc.bones.len())
        .map(|i| compile::resolve_bone_pose(&compiled.desc, compiled, i))
        .collect();
    math::compute_world(
        &poses.iter().map(|p| p.0).collect::<Vec<_>>(),
        &poses.iter().map(|p| p.1).collect::<Vec<_>>(),
        &compile::bone_parents(&compiled.desc),
    )
}
fn transform(matrix: &math::Matrix3x4, position: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|axis| {
        matrix[axis * 4] * position[0]
            + matrix[axis * 4 + 1] * position[1]
            + matrix[axis * 4 + 2] * position[2]
            + matrix[axis * 4 + 3]
    })
}
fn close(left: impl IntoIterator<Item = f32>, right: impl IntoIterator<Item = f32>) {
    let left: Vec<_> = left.into_iter().collect();
    let right: Vec<_> = right.into_iter().collect();
    assert_eq!(left.len(), right.len());
    for (a, b) in left.into_iter().zip(right) {
        assert!((a - b).abs() < 0.0003, "{a} != {b}");
    }
}
#[test]
fn centered_bind_origins_preserve_reference_vertices_and_animated_skinning() {
    let fixture = fixture();
    let mut desc = desc(&fixture);
    let original = compile::compile(&desc, &fixture.0).unwrap();
    desc.model.center_bones_on_vertices = true;
    let centered = compile::compile(&desc, &fixture.0).unwrap();
    let before = &original.bodyparts[0].models[0].meshes[0];
    let after = &centered.bodyparts[0].models[0].meshes[0];
    assert_eq!(before.vertices, after.vertices);
    let old_bind = bind_worlds(&original);
    let new_bind = bind_worlds(&centered);
    close(
        [new_bind[1][3], new_bind[1][7], new_bind[1][11]],
        [10.0, 2.0, 0.0],
    );
    close(
        [new_bind[2][3], new_bind[2][7], new_bind[2][11]],
        [8.0, 6.0, 0.0],
    );
    for frame in 0..2 {
        let old = worlds(&original, frame);
        let new = worlds(&centered, frame);
        for vertex in &before.vertices {
            let index = vertex.bones[0][0] as usize;
            let old_skin = math::concat(&old[index], &math::invert(&old_bind[index]));
            let new_skin = math::concat(&new[index], &math::invert(&new_bind[index]));
            close(
                transform(&old_skin, vertex.pos),
                transform(&new_skin, vertex.pos),
            );
        }
    }
}
#[test]
fn collapsing_static_helper_preserves_child_world_animation_and_vertex_weights() {
    let fixture = fixture();
    let mut desc = desc(&fixture);
    let original = compile::compile(&desc, &fixture.0).unwrap();
    desc.model.collapse_bones = true;
    let collapsed = compile::compile(&desc, &fixture.0).unwrap();
    assert_eq!(
        collapsed
            .desc
            .bones
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        ["root", "animated"]
    );
    assert_eq!(collapsed.desc.bones[1].parent.as_deref(), Some("root"));
    for frame in 0..2 {
        close(worlds(&original, frame)[2], worlds(&collapsed, frame)[1]);
    }
    assert_eq!(
        collapsed.bodyparts[0].models[0].meshes[0].vertices[0].bones,
        vec![[0.0, 1.0]]
    );
    assert_eq!(
        collapsed.bodyparts[0].models[0].meshes[0].vertices[2].bones,
        vec![[1.0, 1.0]]
    );
    let output = pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
    let limits = vformats::Limits::default();
    let vvd = vformats::mdl::parse_vvd(&output.vvd, &limits).unwrap();
    assert!(
        vvd.vertices
            .iter()
            .all(|v| v.bones.iter().all(|&bone| bone < 2))
    );
}
#[test]
fn lock_lengths_preserves_root_motion_and_reference_local_lengths() {
    let fixture = fixture();
    let mut desc = desc(&fixture);
    desc.model.lock_bone_lengths = true;
    let compiled = compile::compile(&desc, &fixture.0).unwrap();
    assert_ne!(
        compiled.animations[0].frames[1][0].position,
        compiled.animations[0].frames[0][0].position
    );
    for frame in &compiled.animations[0].frames {
        close(frame[1].position, [5.0, 0.0, 0.0]);
        close(frame[2].position, [0.0, 0.0, 3.0]);
    }
}
fn write_large_mesh(fixture: &Fixture, name: &str, triangles: usize) {
    let mut text = String::from(
        "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\n",
    );
    for triangle in 0..triangles {
        text.push_str(if triangle % 2 == 0 {
            "paint\n"
        } else {
            "trim\n"
        });
        for corner in 0..3 {
            let x = triangle * 4 + corner;
            text.push_str(&format!("0 {x} {} 0 0 0 1 0 0\n", corner % 2));
        }
    }
    text.push_str("end\n");
    fs::write(fixture.0.join(name), text).unwrap();
}

#[test]
fn maxverts_slices_all_triangles_and_preserves_material_and_lod_pools() {
    let fixture = fixture();
    write_large_mesh(&fixture, "large.smd", 400);
    let mut desc=qc::parse_qc_str("$modelname tests/maxverts.mdl\n$body body large.smd\n$sequence idle large.smd\n$lod 25 { replacemodel large.smd large.smd }\n",&fixture.0).unwrap();
    desc.model.max_vertices = Some([1024, 1024]);
    let output = pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
    assert_eq!(output.compiled.bodyparts.len(), 2);
    let mut triangles = [0; 2];
    let mut positions = std::collections::BTreeSet::new();
    for part in &output.compiled.bodyparts {
        let model = &part.models[0];
        assert!(
            model
                .meshes
                .iter()
                .map(|mesh| mesh.vertices.len())
                .sum::<usize>()
                <= 1024
        );
        for (mesh, lods) in model
            .meshes
            .iter()
            .zip(&model.lods.as_ref().unwrap().meshes)
        {
            assert!(mesh.material < 2);
            assert_eq!(mesh.triangles.len(), lods.triangles[1].len());
            triangles[mesh.material] += mesh.triangles.len();
            for vertex in &mesh.vertices {
                positions.insert(vertex.pos[0] as i32);
            }
        }
    }
    assert_eq!(triangles, [200, 200]);
    assert_eq!(positions.len(), 1200);
    vformats::mdl::parse_vvd(&output.vvd, &vformats::Limits::default()).unwrap();
    vformats::mdl::parse_vtx(&output.vtx, &vformats::Limits::default()).unwrap();
}

#[test]
fn maxverts_preserves_bodygroup_selection_for_differently_sized_and_blank_variants() {
    let fixture = fixture();
    write_large_mesh(&fixture, "large.smd", 400);
    write_large_mesh(&fixture, "larger.smd", 800);
    let description = qc::parse_qc_str(
        "$modelname tests/bodygroup_split.mdl\n$bodygroup shape {\nblank\nstudio large.smd\nstudio larger.smd\n}\n$bodygroup accessory {\nblank\nstudio mesh.smd\n}\n$maxverts 1024 1024\n",
        &fixture.0,
    ).unwrap();
    let output = pipeline::build(&description, &fixture.0, Default::default()).unwrap();
    let integer = |at: usize| i32::from_le_bytes(output.mdl[at..at + 4].try_into().unwrap());
    let bodyparts = integer(236) as usize;
    assert!(integer(232) > 2);
    for shape in 0..3 {
        for accessory in 0..2 {
            let body = shape + accessory * 3;
            let mut vertices = 0;
            for index in 0..integer(232) as usize {
                let part = bodyparts + index * 16;
                let count = integer(part + 4);
                let base = integer(part + 8);
                let selection = (body / base) % count;
                let model = part + integer(part + 12) as usize + selection as usize * 148;
                vertices += integer(model + 80);
            }
            assert_eq!(vertices, [0, 1200, 2400][shape as usize] + accessory * 3);
        }
    }
    vformats::mdl::parse_vvd(&output.vvd, &vformats::Limits::default()).unwrap();
    vformats::mdl::parse_vtx(&output.vtx, &vformats::Limits::default()).unwrap();
}

#[test]
fn alignment_bits_keep_vertex_usage_and_do_not_spread_to_parents() {
    let fixture = fixture();
    let mut desc = desc(&fixture);
    desc.model.screen_alignments = vec![("helper".into(), 0x10), ("animated".into(), 0x20)];
    let output = pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
    let start = i32::from_le_bytes(output.mdl[160..164].try_into().unwrap()) as usize;
    let flags = |index: usize| {
        let at = start + index * 216 + 160;
        i32::from_le_bytes(output.mdl[at..at + 4].try_into().unwrap())
    };
    assert_eq!(flags(0) & 0x38, 0);
    assert_eq!(flags(1) & 0x38, 0x10);
    assert_eq!(flags(2) & 0x38, 0x20);
    assert_ne!(flags(1) & crate::mdl_writer::BONE_USED_BY_VERTEX_LOD0, 0);
    assert_ne!(flags(2) & crate::mdl_writer::BONE_USED_BY_VERTEX_LOD0, 0);
}

#[test]
fn skinned_lods_keep_authored_lower_detail_weights() {
    let fixture = fixture();
    let source = fs::read_to_string(fixture.0.join("mesh.smd")).unwrap();
    let source = source.replace(" 1 1 1\n", " 1 2 1\n");
    fs::write(fixture.0.join("lower.smd"), source).unwrap();
    let desc = qc::parse_qc_str("$modelname tests/skinned.mdl\n$body body mesh.smd\n$sequence move anim.smd\n$skinnedLODs\n$lod 25 { replacemodel mesh.smd lower.smd }\n", &fixture.0).unwrap();
    let output = pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
    let mesh = &output.compiled.bodyparts[0].models[0]
        .lods
        .as_ref()
        .unwrap()
        .meshes[0];
    assert!(
        mesh.triangles[0]
            .iter()
            .flatten()
            .any(|&index| mesh.vertices[index as usize].bones == vec![[1.0, 1.0]])
    );
    assert!(
        mesh.triangles[1]
            .iter()
            .flatten()
            .all(|&index| mesh.vertices[index as usize].bones == vec![[2.0, 1.0]])
    );
    let vvd = vformats::mdl::parse_vvd(&output.vvd, &vformats::Limits::default()).unwrap();
    assert!(vvd.vertices.iter().any(|vertex| vertex.bones[0] == 2));
}

#[test]
fn centered_delta_animation_keeps_the_same_deformation() {
    let fixture = fixture();
    let mut desc = qc::parse_qc_str("$modelname tests/center_delta.mdl\n$body body mesh.smd\n$animation base mesh.smd\n$sequence move anim.smd subtract base 0 delta\n", &fixture.0).unwrap();
    let original = compile::compile(&desc, &fixture.0).unwrap();
    desc.model.center_bones_on_vertices = true;
    let centered = compile::compile(&desc, &fixture.0).unwrap();
    let deformation = |compiled: &crate::model::CompiledModelDesc, frame: usize| {
        let base = &compiled.animations[0].frames[0];
        let delta = &compiled.animations[1].frames[frame];
        let poses: Vec<_> = base
            .iter()
            .zip(delta)
            .map(|(base, delta)| {
                let position =
                    std::array::from_fn(|axis| base.position[axis] + delta.position[axis]);
                let rotation = math::quaternion_angles(math::quaternion_mult(
                    math::angle_quaternion(base.rotation),
                    math::angle_quaternion(delta.rotation),
                ));
                (position, rotation)
            })
            .collect();
        let worlds = math::compute_world(
            &poses.iter().map(|p| p.0).collect::<Vec<_>>(),
            &poses.iter().map(|p| p.1).collect::<Vec<_>>(),
            &compile::bone_parents(&compiled.desc),
        );
        let bind = bind_worlds(compiled);
        compiled.bodyparts[0].models[0].meshes[0]
            .vertices
            .iter()
            .map(|vertex| {
                let index = vertex.bones[0][0] as usize;
                transform(
                    &math::concat(&worlds[index], &math::invert(&bind[index])),
                    vertex.pos,
                )
            })
            .collect::<Vec<_>>()
    };
    for frame in 0..2 {
        for (before, after) in deformation(&original, frame)
            .into_iter()
            .zip(deformation(&centered, frame))
        {
            close(before, after);
        }
    }
}

#[test]
fn vertex_only_bounds_exclude_hitboxes_and_worldspace_clamps_written_sequence_bounds() {
    let fixture = fixture();
    let mut desc = desc(&fixture);
    desc.hitboxes.set_name = Some("default".into());
    desc.hitboxes.boxes.push(crate::model::Hitbox {
        bone: "root".into(),
        group: None,
        bbmin: [-500.0; 3],
        bbmax: [500.0; 3],
        name: None,
    });
    let original =
        pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
    desc.model.bbox_only_vertices = true;
    let vertex_only =
        pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
    let bounds = |bytes: &[u8]| {
        let sequence = i32::from_le_bytes(bytes[192..196].try_into().unwrap()) as usize;
        let vector = |offset: usize| {
            std::array::from_fn::<_, 3, _>(|axis| {
                f32::from_le_bytes(
                    bytes[sequence + offset + axis * 4..sequence + offset + axis * 4 + 4]
                        .try_into()
                        .unwrap(),
                )
            })
        };
        (vector(32), vector(44))
    };
    let (before_min, before_max) = bounds(&original.mdl);
    let (after_min, after_max) = bounds(&vertex_only.mdl);
    assert!(before_min.iter().all(|&value| value <= -500.0));
    assert!(before_max.iter().all(|&value| value >= 500.0));
    assert!(
        after_min
            .iter()
            .chain(&after_max)
            .all(|value| value.abs() < 50.0)
    );
    desc.model.clamp_worldspace = Some([[-1.0; 3], [1.0; 3]]);
    let clamped = pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
    let (minimum, maximum) = bounds(&clamped.mdl);
    for axis in 0..3 {
        assert_eq!(minimum[axis], after_min[axis].clamp(-1.0, 1.0));
        assert_eq!(maximum[axis], after_max[axis].clamp(-1.0, 1.0));
    }
}

#[test]
fn bone_flex_drivers_write_grouped_controls_and_relative_header_pointers() {
    let fixture = fixture();
    let mut desc = desc(&fixture);
    for name in ["lips", "eyes"] {
        desc.flex_controllers.push(crate::model::FlexController {
            name: name.into(),
            kind: "face".into(),
            min: 0.0,
            max: 1.0,
        });
    }
    desc.model.source_rotation = Some([
        std::f32::consts::FRAC_PI_2,
        0.0,
        std::f32::consts::FRAC_PI_2,
    ]);
    let driver = |bone: &str, component, controller: &str, min, max| crate::model::BoneFlexDriver {
        bone: bone.into(),
        component,
        controller: controller.into(),
        min,
        max,
    };
    desc.model.bone_flex_drivers = vec![
        driver("helper", 0, "lips", -2.0, 4.0),
        driver("root", 0, "eyes", 1.0, 7.0),
        driver("helper", 2, "eyes", 0.0, 3.0),
        driver("helper", 1, "lips", -4.0, 8.0),
    ];
    let output = pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
    let bytes = &output.mdl;
    let integer = |at| i32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let float = |at| f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let header = integer(400) as usize;
    assert_eq!(integer(76) as usize, bytes.len());
    assert_eq!(integer(header + 0x18), 2);
    let first = header + integer(header + 0x1c) as usize;
    assert_eq!((integer(first), integer(first + 4)), (1, 2));
    let controls = first + integer(first + 8) as usize;
    assert_eq!((integer(controls), integer(controls + 4)), (1, 0));
    assert_eq!((float(controls + 8), float(controls + 12)), (-4.0, 8.0));
    assert_eq!((integer(controls + 16), integer(controls + 20)), (2, 1));
    let second = first + 24;
    assert_eq!((integer(second), integer(second + 4)), (0, 1));
    let root_control = second + integer(second + 8) as usize;
    assert_eq!((integer(root_control), integer(root_control + 4)), (1, 1));
    assert_eq!(
        (float(root_control + 8), float(root_control + 12)),
        (1.0, 7.0)
    );
    assert_eq!(root_control + 16, bytes.len());
}

#[test]
fn forced_collapse_rebinds_relative_and_absolute_attachments_to_the_retained_parent() {
    let fixture = fixture();
    let mut desc = qc::parse_qc_str("$modelname tests/collapse_attachment.mdl\n$body body mesh.smd\n$sequence move anim.smd\n$attachment relative helper 1 2 3 rotate 10 20 30\n$attachment absolute helper 8 9 10 absolute\n", &fixture.0).unwrap();
    let original =
        pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
    desc.model.collapse_bones = true;
    desc.model.always_collapse.push("helper".into());
    let collapsed =
        pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
    let attachments = |bytes: &[u8]| {
        let at = i32::from_le_bytes(bytes[244..248].try_into().unwrap()) as usize;
        (0..2)
            .map(|index| {
                let base = at + index * 92;
                let bone =
                    i32::from_le_bytes(bytes[base + 8..base + 12].try_into().unwrap()) as usize;
                let matrix = std::array::from_fn(|component| {
                    f32::from_le_bytes(
                        bytes[base + 12 + component * 4..base + 16 + component * 4]
                            .try_into()
                            .unwrap(),
                    )
                });
                (bone, matrix)
            })
            .collect::<Vec<_>>()
    };
    let old_world = bind_worlds(&original.compiled);
    let new_world = bind_worlds(&collapsed.compiled);
    for ((old_bone, old_matrix), (new_bone, new_matrix)) in attachments(&original.mdl)
        .into_iter()
        .zip(attachments(&collapsed.mdl))
    {
        assert_eq!(new_bone, 0);
        close(
            math::concat(&old_world[old_bone], &old_matrix),
            math::concat(&new_world[new_bone], &new_matrix),
        );
    }
    assert_eq!(collapsed.compiled.desc.attachments[0].bone, "root");
}

#[test]
fn forced_collapse_discards_removed_procedural_targets_and_flex_drivers() {
    let fixture = fixture();
    let mut desc = qc::parse_qc_str("$modelname tests/collapse_procedural.mdl\n$body body mesh.smd\n$sequence move anim.smd\n$jigglebone helper { is_rigid { length 2 tip_mass 1 } }\n",&fixture.0).unwrap();
    desc.flex_controllers.push(crate::model::FlexController {
        name: "lips".into(),
        kind: "face".into(),
        min: 0.0,
        max: 1.0,
    });
    desc.model
        .bone_flex_drivers
        .push(crate::model::BoneFlexDriver {
            bone: "helper".into(),
            component: 0,
            controller: "lips".into(),
            min: 0.0,
            max: 1.0,
        });
    desc.model.collapse_bones = true;
    desc.model.always_collapse.push("helper".into());
    let output = pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
    assert_eq!(
        output
            .compiled
            .desc
            .bones
            .iter()
            .map(|bone| bone.name.as_str())
            .collect::<Vec<_>>(),
        ["root", "animated"]
    );
    assert!(output.compiled.resolved_jiggle_bones.is_empty());
    let header = i32::from_le_bytes(output.mdl[400..404].try_into().unwrap()) as usize;
    assert_eq!(
        i32::from_le_bytes(output.mdl[header + 0x18..header + 0x1c].try_into().unwrap()),
        0
    );
}

#[test]
fn forced_collapse_can_replace_an_ik_middle_bone_when_three_links_remain() {
    let fixture = fixture();
    for name in ["mesh.smd", "anim.smd"] {
        let path = fixture.0.join(name);
        let source = fs::read_to_string(&path)
            .unwrap()
            .replace("2 \"animated\" 1\n", "2 \"animated\" 1\n3 \"foot\" 2\n")
            .replace("2 0 0 3 0 0 0\n", "2 0 0 3 0 0 0\n3 0 0 2 0 0 0\n")
            .replace(
                "2 0 0 7 0.1 0.2 0.3\n",
                "2 0 0 7 0.1 0.2 0.3\n3 0 0 2 0 0 0\n",
            );
        fs::write(path, source).unwrap();
    }
    let mut desc = qc::parse_qc_str("$modelname tests/collapse_ik.mdl\n$body body mesh.smd\n$sequence move anim.smd\n$ikchain leg foot\n",&fixture.0).unwrap();
    let original = compile::compile(&desc, &fixture.0).unwrap();
    desc.model.collapse_bones = true;
    desc.model.always_collapse.push("animated".into());
    let collapsed = compile::compile(&desc, &fixture.0).unwrap();
    assert_eq!(
        collapsed
            .desc
            .bones
            .iter()
            .map(|bone| bone.name.as_str())
            .collect::<Vec<_>>(),
        ["root", "helper", "foot"]
    );
    for frame in 0..2 {
        close(worlds(&original, frame)[3], worlds(&collapsed, frame)[2]);
    }
    pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
}

#[test]
fn forced_root_collapse_promotes_the_animated_descendant_with_world_motion_intact() {
    let fixture = fixture();
    let mut desc = desc(&fixture);
    let original = compile::compile(&desc, &fixture.0).unwrap();
    desc.model.collapse_bones = true;
    desc.model.always_collapse.push("root".into());
    let collapsed = compile::compile(&desc, &fixture.0).unwrap();
    assert_eq!(collapsed.desc.bones.len(), 1);
    assert_eq!(collapsed.desc.bones[0].name, "animated");
    assert!(collapsed.desc.bones[0].parent.is_none());
    for frame in 0..2 {
        close(worlds(&original, frame)[2], worlds(&collapsed, frame)[0]);
    }
    let output = pipeline::build(&desc, &fixture.0, pipeline::PipelineOptions::default()).unwrap();
    let vvd = vformats::mdl::parse_vvd(&output.vvd, &vformats::Limits::default()).unwrap();
    assert!(
        vvd.vertices
            .iter()
            .all(|vertex| vertex.bones[0] == 0 && vertex.bone_count == 1)
    );
}

#[test]
fn zbrush_reaches_single_and_multi_lod_vvd_without_cross_material_smoothing() {
    let fixture = fixture();
    let mut source = String::from(
        "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\n",
    );
    let positions = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
    for (material, uvs) in [
        ("paint", [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
        ("paint", [[0.5, 0.5], [0.5, 1.5], [-0.5, 0.5]]),
        ("trim", [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
    ] {
        source.push_str(material);
        source.push('\n');
        for (position, uv) in positions.into_iter().zip(uvs) {
            source.push_str(&format!(
                "0 {} {} 0 0 0 1 {} {}\n",
                position[0], position[1], uv[0], uv[1]
            ));
        }
    }
    source.push_str("end\n");
    fs::write(fixture.0.join("seam.smd"), source).unwrap();
    for lod in ["", "$lod 25 { replacemodel seam.smd seam.smd }\n"] {
        let build = |smooth: &str| {
            let description = qc::parse_qc_str(&format!("$modelname tests/zbrush.mdl\n$body body seam.smd\n$sequence idle seam.smd\n{lod}{smooth}"),&fixture.0).unwrap();
            pipeline::build(
                &description,
                &fixture.0,
                pipeline::PipelineOptions::default(),
            )
            .unwrap()
        };
        let original = build("");
        let smoothed = build("$zbrush\n");
        let before = vformats::mdl::parse_vvd(&original.vvd, &vformats::Limits::default()).unwrap();
        let after = vformats::mdl::parse_vvd(&smoothed.vvd, &vformats::Limits::default()).unwrap();
        assert_eq!(before.vertices, after.vertices);
        assert_eq!(before.vertices.len(), 9);
        assert_ne!(before.tangents[0], after.tangents[0]);
        assert_eq!(after.tangents[0], after.tangents[3]);
        assert_eq!(before.tangents[6..], after.tangents[6..]);
    }
}

#[test]
fn hitgroup_names_are_validated_and_groups_survive_bone_optimization() {
    let fixture = fixture();
    let mut description = desc(&fixture);
    description.model.hitgroups = vec![("missing".into(), 7)];
    let errors = compile::compile(&description, &fixture.0).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.at() == "model.hitgroups[0]")
    );
    description.model.hitgroups = vec![("helper".into(), 7)];
    description.model.collapse_bones = true;
    let output = pipeline::build(
        &description,
        &fixture.0,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    assert!(
        output
            .compiled
            .desc
            .bones
            .iter()
            .any(|bone| bone.name == "helper")
    );
    let helper_boxes: Vec<_> = output
        .compiled
        .desc
        .hitboxes
        .boxes
        .iter()
        .filter(|hitbox| hitbox.bone == "helper")
        .collect();
    assert!(!helper_boxes.is_empty());
    assert!(helper_boxes.iter().all(|hitbox| hitbox.group == Some(7)));
}

#[test]
fn bone_flex_driver_axis_follows_source_upaxis_rules_without_rotating_ranges() {
    let fixture = fixture();
    for (transform, expected) in [
        ("$upaxis y\n", 1),
        ("$upaxis z\n", 0),
        ("$upaxis y\n$origin 2 3 4 15\n", 0),
    ] {
        for center in ["", "$centerbonesonverts\n"] {
            let text = format!(
                "$modelname tests/driver_axis.mdl\n{transform}$model body mesh.smd {{\nflexcontroller face range 0 1 lips\n}}\n$sequence move anim.smd\n$boneflexdriver root tx lips -3 7\n{center}"
            );
            let description = qc::parse_qc_str(&text, &fixture.0).unwrap();
            let output = pipeline::build(
                &description,
                &fixture.0,
                pipeline::PipelineOptions::default(),
            )
            .unwrap();
            let integer = |at| i32::from_le_bytes(output.mdl[at..at + 4].try_into().unwrap());
            let header = integer(400) as usize;
            let driver = header + integer(header + 0x1c) as usize;
            let control = driver + integer(driver + 8) as usize;
            assert_eq!(integer(control), expected, "{transform}{center}");
            assert_eq!(
                f32::from_le_bytes(output.mdl[control + 8..control + 12].try_into().unwrap()),
                -3.0
            );
            assert_eq!(
                f32::from_le_bytes(output.mdl[control + 12..control + 16].try_into().unwrap()),
                7.0
            );
        }
    }
}
