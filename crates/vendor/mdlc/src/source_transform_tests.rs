use crate::{bone_math as math, pipeline, qc, smd};
use std::{
    fmt::Write,
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
        "mdlc_source_transform_{}_{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&directory).unwrap();
    let mut source = smd::parse_smd(include_str!("../tests/fixtures/rig.smd")).unwrap();
    for triangle in &mut source.triangles {
        for vertex in &mut triangle.vertices {
            for (axis, offset) in [10.0, 20.0, 30.0].into_iter().enumerate() {
                vertex.position[axis] += offset;
            }
        }
    }
    let mut text = String::from(
        "version 1\nnodes\n0 \"root\" -1\n1 \"mid\" 0\n2 \"tip\" 1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 0 0 10 0 0 0\n2 0 0 10 0 0 0\nend\ntriangles\n",
    );
    for triangle in &source.triangles {
        writeln!(text, "{}", triangle.material).unwrap();
        for vertex in &triangle.vertices {
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
    fs::write(directory.join("reference.smd"), &text).unwrap();
    fs::write(directory.join("other.smd"), &text).unwrap();
    let animation = "version 1\nnodes\n0 \"root\" -1\n1 \"mid\" 0\n2 \"tip\" 1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 0 0 10 0 0 0\n2 0 0 10 0 0 0\ntime 1\n0 2 3 4 0.1 0.2 0.3\n1 0 0 10 0.4 -0.3 0.2\n2 0 0 10 0.2 0.1 -0.2\nend\n";
    fs::write(directory.join("animation.smd"), animation).unwrap();
    Fixture(directory)
}
fn build(fixture: &Fixture, text: &str) -> pipeline::PipelineOutput {
    let description = qc::parse_qc_str(
        &format!("$modelname models/tests/source_transform.mdl\n{text}"),
        &fixture.0,
    )
    .unwrap();
    pipeline::build(
        &description,
        &fixture.0,
        pipeline::PipelineOptions::default(),
    )
    .unwrap()
}
fn close(a: impl IntoIterator<Item = f32>, b: impl IntoIterator<Item = f32>) {
    let a: Vec<_> = a.into_iter().collect();
    let b: Vec<_> = b.into_iter().collect();
    assert_eq!(a.len(), b.len());
    for (a, b) in a.into_iter().zip(b) {
        assert!((a - b).abs() < 0.0002, "{a} != {b}");
    }
}
fn integer(raw: &[u8], at: usize) -> usize {
    i32::from_le_bytes(raw[at..at + 4].try_into().unwrap()) as usize
}
fn attachment(raw: &[u8], index: usize) -> [f32; 12] {
    let at = integer(raw, 244) + index * 92 + 12;
    std::array::from_fn(|component| {
        f32::from_le_bytes(
            raw[at + component * 4..at + component * 4 + 4]
                .try_into()
                .unwrap(),
        )
    })
}

#[test]
fn autocenter_shifts_all_geometry_lods_and_collision_with_one_placement_origin() {
    let fixture = fixture();
    let text = "$staticprop\n$scale 2\n$origin 5 6 7 25\n$body body reference.smd\n$body extra other.smd\n$sequence idle reference.smd\n$lod 25 { replacemodel reference.smd other.smd }\n$attachment muzzle root 7 8 9 rotate 17 33 -21\n$collisionmodel reference.smd { $mass 42 }\n";
    let original = build(&fixture, text);
    let centered = build(&fixture, &format!("{text}$autocenter\n"));
    let offset = centered.compiled.desc.model.autocenter_offset.unwrap();
    let limits = vformats::Limits::default();
    let before = vformats::mdl::parse_vvd(&original.vvd, &limits).unwrap();
    let after = vformats::mdl::parse_vvd(&centered.vvd, &limits).unwrap();
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for (before, after) in before.vertices.iter().zip(&after.vertices) {
        close(
            after.position,
            std::array::from_fn::<_, 3, _>(|axis| before.position[axis] + offset[axis]),
        );
        close(after.normal, before.normal);
        for axis in 0..3 {
            min[axis] = min[axis].min(after.position[axis]);
            max[axis] = max[axis].max(after.position[axis]);
        }
    }
    close(
        std::array::from_fn::<_, 3, _>(|axis| min[axis] + max[axis]),
        [0.0; 3],
    );
    assert_eq!(integer(&centered.mdl, 240), 2);
    close(attachment(&centered.mdl, 0), attachment(&original.mdl, 0));
    let expected = math::local_transform(offset, [0.0; 3]);
    close(attachment(&centered.mdl, 1), expected);
    for (before, after) in original
        .compiled
        .bodyparts
        .iter()
        .zip(&centered.compiled.bodyparts)
    {
        for (before, after) in before.models.iter().zip(&after.models) {
            for (before, after) in before
                .lods
                .as_ref()
                .unwrap()
                .meshes
                .iter()
                .zip(&after.lods.as_ref().unwrap().meshes)
            {
                for (before, after) in before.vertices.iter().zip(&after.vertices) {
                    close(
                        after.pos,
                        std::array::from_fn::<_, 3, _>(|axis| before.pos[axis] + offset[axis]),
                    );
                }
            }
        }
    }
    let before = vformats::phy::parse(original.phy.as_ref().unwrap(), &limits).unwrap();
    let after = vformats::phy::parse(centered.phy.as_ref().unwrap(), &limits).unwrap();
    let before_points: Vec<_> = before
        .solids
        .iter()
        .flat_map(|solid| &solid.ledges)
        .flat_map(|ledge| &ledge.vertices)
        .collect();
    let after_points: Vec<_> = after
        .solids
        .iter()
        .flat_map(|solid| &solid.ledges)
        .flat_map(|ledge| &ledge.vertices)
        .collect();
    assert_eq!(before_points.len(), after_points.len());
    let ivp_offset = offset;
    for point in before_points {
        assert!(
            after_points
                .iter()
                .any(|other| (0..3)
                    .all(|axis| (other[axis] - point[axis] - ivp_offset[axis]).abs() < 0.0002)),
            "{point:?} {ivp_offset:?}"
        );
    }
}

#[test]
fn autocenter_existing_placement_origin_disables_recentering_and_keeps_final_attachment_untransformed()
 {
    let fixture = fixture();
    let text = "$staticprop\n$body body reference.smd\n$sequence idle reference.smd\n$attachment placementOrigin root 3 4 5\n$attachment final root 6 7 8 rotate 11 22 33\n";
    let original = build(&fixture, text);
    let centered = build(&fixture, &format!("{text}$autocenter\n"));
    assert_eq!(&original.vvd[12..], &centered.vvd[12..]);
    assert_eq!(integer(&centered.mdl, 240), 2);
    assert_eq!(centered.compiled.desc.model.autocenter_offset, None);
    close(attachment(&centered.mdl, 0), attachment(&original.mdl, 0));
    close(
        attachment(&centered.mdl, 1),
        math::local_transform(
            [6.0, 7.0, 8.0],
            [
                33.0f32.to_radians(),
                11.0f32.to_radians(),
                22.0f32.to_radians(),
            ],
        ),
    );
}

#[test]
fn autocenter_rejects_nonstatic_and_unrepresentable_first_sources() {
    let fixture = fixture();
    for text in [
        "$body body reference.smd\n$autocenter\n",
        "$sequence idle animation.smd\n$staticprop\n$body body reference.smd\n$autocenter\n",
    ] {
        let description = qc::parse_qc_str(text, &fixture.0).unwrap();
        let error = pipeline::build(
            &description,
            &fixture.0,
            pipeline::PipelineOptions::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("$autocenter"));
    }
}

#[test]
fn forced_realign_rotates_only_the_selected_world_basis_and_preserves_child_world_pose() {
    let fixture = fixture();
    let text = "$body body reference.smd\n$sequence moving animation.smd\n";
    let original = build(&fixture, text);
    let forced = build(
        &fixture,
        &format!(
            "{text}$renamebone mid middle\n$forcerealign MID rotate 0 0 30\n$forcerealign mid rotate 0 0 60\n"
        ),
    );
    let limits = vformats::Limits::default();
    let before = vformats::mdl::parse_mdl(&original.mdl, &limits).unwrap();
    let after = vformats::mdl::parse_mdl(&forced.mdl, &limits).unwrap();
    let before_world = math::compute_world(
        &before
            .bones
            .iter()
            .map(|bone| bone.position)
            .collect::<Vec<_>>(),
        &before
            .bones
            .iter()
            .map(|bone| bone.rotation)
            .collect::<Vec<_>>(),
        &before
            .bones
            .iter()
            .map(|bone| bone.parent)
            .collect::<Vec<_>>(),
    );
    let after_world = math::compute_world(
        &after
            .bones
            .iter()
            .map(|bone| bone.position)
            .collect::<Vec<_>>(),
        &after
            .bones
            .iter()
            .map(|bone| bone.rotation)
            .collect::<Vec<_>>(),
        &after
            .bones
            .iter()
            .map(|bone| bone.parent)
            .collect::<Vec<_>>(),
    );
    close(after_world[0], before_world[0]);
    close(
        after_world[1],
        math::concat(
            &before_world[1],
            &math::angle_matrix([0.0, 0.0, std::f32::consts::FRAC_PI_2]),
        ),
    );
    close(after_world[2], before_world[2]);
    let before_vertices = vformats::mdl::parse_vvd(&original.vvd, &limits).unwrap();
    let after_vertices = vformats::mdl::parse_vvd(&forced.vvd, &limits).unwrap();
    for (before, after) in before_vertices
        .vertices
        .iter()
        .zip(&after_vertices.vertices)
    {
        close(before.position, after.position);
        close(before.normal, after.normal);
    }
    let realign = forced.compiled.realigned.as_ref().unwrap();
    for (before, after) in original.compiled.animations[0]
        .frames
        .iter()
        .zip(&forced.compiled.animations[0].frames)
    {
        let parents = [-1, 0, 1];
        let before_world = math::compute_world(
            &before.iter().map(|pose| pose.position).collect::<Vec<_>>(),
            &before.iter().map(|pose| pose.rotation).collect::<Vec<_>>(),
            &parents,
        );
        let after_world = math::compute_world(
            &after.iter().map(|pose| pose.position).collect::<Vec<_>>(),
            &after.iter().map(|pose| pose.rotation).collect::<Vec<_>>(),
            &parents,
        );
        for bone in 0..3 {
            close(
                after_world[bone],
                math::concat(&before_world[bone], &realign.src_realign[bone]),
            );
        }
    }
    for text in [
        "$forcerealign missing rotate 0 0 90",
        "$forcerealign mid rotate NaN 0 0",
        "$staticprop\n$forcerealign mid rotate 0 0 90",
    ] {
        let description =
            qc::parse_qc_str(&format!("$body body reference.smd\n{text}\n"), &fixture.0);
        assert!(
            description.is_err()
                || pipeline::build(
                    &description.unwrap(),
                    &fixture.0,
                    pipeline::PipelineOptions::default()
                )
                .is_err()
        );
    }
}

#[test]
fn forced_realign_respects_locked_definebones_and_follows_automatic_realign() {
    let fixture = fixture();
    let text = "$body body reference.smd\n$sequence idle reference.smd\n$definebone root \"\" 0 0 0 0 0 0\n$definebone mid root 0 0 10 0 0 0\n$definebone tip mid 0 0 10 0 0 0\n";
    let original = build(&fixture, text);
    let forced = build(
        &fixture,
        &format!("{text}$forcerealign mid rotate 0 0 90\n"),
    );
    assert_eq!(&original.mdl[12..], &forced.mdl[12..]);
    let auto = build(
        &fixture,
        "$body body reference.smd\n$sequence idle reference.smd\n$realignbones\n",
    );
    let forced = build(
        &fixture,
        "$body body reference.smd\n$sequence idle reference.smd\n$realignbones\n$forcerealign mid rotate 0 0 90\n",
    );
    let auto_poses = auto.compiled.realigned.as_ref().unwrap().poses.clone();
    let forced_poses = forced.compiled.realigned.as_ref().unwrap().poses.clone();
    let world = |poses: &[([f32; 3], [f32; 3])]| {
        math::compute_world(
            &poses.iter().map(|pose| pose.0).collect::<Vec<_>>(),
            &poses.iter().map(|pose| pose.1).collect::<Vec<_>>(),
            &[-1, 0, 1],
        )
    };
    let auto_world = world(&auto_poses);
    let forced_world = world(&forced_poses);
    close(
        forced_world[1],
        math::concat(
            &auto_world[1],
            &math::angle_matrix([0.0, 0.0, std::f32::consts::FRAC_PI_2]),
        ),
    );
    close(forced_world[2], auto_world[2]);
}

#[test]
fn forced_realign_rejects_commands_that_require_an_earlier_reorientation_stage() {
    let fixture = fixture();
    for options in [
        "delta",
        "predelta",
        "subtract base 0",
        "lineardelta",
        "LX",
        "walkframe 1 LX",
        "rotateto 30",
        "counterrotate mid",
        "localhierarchy mid root range 0 0 1 1",
    ] {
        let description = qc::parse_qc_str(
            &format!(
                "$body body reference.smd\n$animation base animation.smd\n$sequence moving animation.smd {{ {options} }}\n$forcerealign mid rotate 0 0 90\n"
            ),
            &fixture.0,
        )
        .unwrap();
        let errors = description.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.path == "model.forced_realign"),
            "{options}: {errors:?}"
        );
    }
}

#[test]
fn root_selects_motion_reference_without_reparenting_the_skeleton() {
    let fixture = fixture();
    let path = fixture.0.join("animation.smd");
    let source = fs::read_to_string(&path)
        .unwrap()
        .replace("1 0 0 10 0.4 -0.3 0.2", "1 8 0 10 0.4 -0.3 0.2");
    fs::write(path, source).unwrap();
    let text =
        "$body body reference.smd\n$sequence moving animation.smd { walkframe 1 LX LY LM }\n";
    let original = build(&fixture, text);
    let selected = build(
        &fixture,
        &format!("{text}$renamebone mid middle\n$root MID\n"),
    );
    close(
        original.compiled.animations[0].movements[0].position,
        [-3.0, 2.0, 0.0],
    );
    close(
        selected.compiled.animations[0].movements[0].position,
        [8.0, 0.0, 0.0],
    );
    let descriptor = integer(&selected.mdl, 184);
    assert_eq!(integer(&selected.mdl, descriptor + 20), 1);
    let movement = descriptor + integer(&selected.mdl, descriptor + 24);
    close(
        (0..3).map(|axis| {
            f32::from_le_bytes(
                selected.mdl[movement + 32 + axis * 4..movement + 36 + axis * 4]
                    .try_into()
                    .unwrap(),
            )
        }),
        [8.0, 0.0, 0.0],
    );
    let limits = vformats::Limits::default();
    let original_model = vformats::mdl::parse_mdl(&original.mdl, &limits).unwrap();
    let selected_model = vformats::mdl::parse_mdl(&selected.mdl, &limits).unwrap();
    for (before, after) in original_model.bones.iter().zip(&selected_model.bones) {
        assert_eq!(before.parent, after.parent);
        close(before.position, after.position);
        close(before.rotation, after.rotation);
    }
    assert_eq!(original.vvd, selected.vvd);
    let missing = build(&fixture, &format!("{text}$root missing\n"));
    assert_eq!(original.mdl, missing.mdl);
}

#[test]
fn root_selects_default_alignment_and_rotateto_reference() {
    let fixture = fixture();
    let text = "$body body reference.smd\n$animation base animation.smd\n$sequence aligned animation.smd { align base X Y Z 0 1 }\n";
    let selected = build(&fixture, &format!("{text}$root mid\n"));
    let frames = &selected.compiled.sequences[0].frames;
    let world = |frame: &[smd::SmdPose]| {
        math::compute_world(
            &frame.iter().map(|pose| pose.position).collect::<Vec<_>>(),
            &frame.iter().map(|pose| pose.rotation).collect::<Vec<_>>(),
            &[-1, 0, 1],
        )
    };
    let reference = world(&selected.compiled.animations[0].frames[0]);
    let aligned = world(&frames[1]);
    close(
        [aligned[1][3], aligned[1][7], aligned[1][11]],
        [reference[1][3], reference[1][7], reference[1][11]],
    );
    let path = fixture.0.join("animation.smd");
    let source = fs::read_to_string(&path)
        .unwrap()
        .replace("1 0 0 10 0.4 -0.3 0.2", "1 8 0 10 0.4 -0.3 0.2");
    fs::write(path, source).unwrap();
    let base = build(
        &fixture,
        "$body body reference.smd\n$sequence moving animation.smd\n",
    );
    let selected = build(
        &fixture,
        "$body body reference.smd\n$sequence moving animation.smd { rotateto 0 }\n$root mid\n",
    );
    for (before, after) in base.compiled.animations[0]
        .frames
        .iter()
        .zip(&selected.compiled.animations[0].frames)
    {
        close(before[0].position, after[0].position);
        close(
            math::angle_matrix(before[0].rotation),
            math::angle_matrix(after[0].rotation),
        );
    }
}

#[test]
fn limitrotation_writes_source_quaternion_alignment_without_changing_geometry_or_physics() {
    let fixture = fixture();
    let mut animation =
        String::from("version 1\nnodes\n0 \"root\" -1\n1 \"mid\" 0\n2 \"tip\" 1\nend\nskeleton\n");
    for frame in 0..4 {
        writeln!(
            animation,
            "time {frame}\n0 0 0 0 0 0 0\n1 0 0 10 0 0 {}\n2 0 0 10 0 0 0",
            (frame as f32 * 30.0).to_radians()
        )
        .unwrap();
    }
    animation.push_str("end\n");
    fs::write(fixture.0.join("animation.smd"), animation).unwrap();
    let text = "$body body reference.smd\n$sequence sweep animation.smd\n$collisionjoints reference.smd { $mass 42 }\n";
    let original = build(&fixture, text);
    let limited = build(&fixture, &format!("{text}$limitrotation MID\n"));
    let bone = integer(&limited.mdl, 160) + 216;
    let quaternion = |offset| {
        std::array::from_fn::<_, 4, _>(|axis| {
            f32::from_le_bytes(
                limited.mdl[bone + offset + axis * 4..bone + offset + axis * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        })
    };
    close(
        quaternion(144),
        math::angle_quaternion([0.0, 0.0, 22.425_f32.to_radians()]),
    );
    assert_ne!(integer(&limited.mdl, bone + 160) & 0x0010_0000, 0);
    close(quaternion(44), [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(original.vvd, limited.vvd);
    assert_eq!(original.phy, limited.phy);
    let limits = vformats::Limits::default();
    let original_model = vformats::mdl::parse_mdl(&original.mdl, &limits).unwrap();
    let limited_model = vformats::mdl::parse_mdl(&limited.mdl, &limits).unwrap();
    for (before, after) in original_model.bones.iter().zip(&limited_model.bones) {
        close(before.position, after.position);
        close(before.rotation, after.rotation);
    }
    let named = build(&fixture, &format!("{text}$limitrotation mid sweep\n"));
    let named_bone = integer(&named.mdl, 160) + 216;
    close(
        (0..4).map(|axis| {
            f32::from_le_bytes(
                named.mdl[named_bone + 144 + axis * 4..named_bone + 148 + axis * 4]
                    .try_into()
                    .unwrap(),
            )
        }),
        [0.0, 0.0, 0.0, 1.0],
    );
    for directive in ["$limitrotation absent", "$staticprop\n$limitrotation mid"] {
        let description = qc::parse_qc_str(&format!("{text}{directive}\n"), &fixture.0).unwrap();
        assert!(
            description
                .validate()
                .unwrap_err()
                .iter()
                .any(|error| error.path.starts_with("model.rotation_limits"))
        );
    }
}

#[test]
fn limitrotation_excludes_short_and_delta_animations_and_accumulates_aligned_sweeps() {
    let fixture = fixture();
    let mut description = qc::parse_qc_str(
        "$body body reference.smd\n$sequence sweep animation.smd\n$limitrotation mid\n",
        &fixture.0,
    )
    .unwrap();
    let original = pipeline::build(
        &description,
        &fixture.0,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    let alignment = crate::bone_alignment::calculate(&original.compiled);
    close(alignment[1].unwrap(), [0.0, 0.0, 0.0, 1.0]);
    description.model.rotation_limits[0].bone = "root".into();
    let original = pipeline::build(
        &description,
        &fixture.0,
        pipeline::PipelineOptions::default(),
    )
    .unwrap();
    let mut compiled = original.compiled;
    let first = compiled.animations[0].frames[0].clone();
    compiled.animations[0].frames.resize(4, first);
    for frame in &mut compiled.animations[0].frames {
        frame[0].rotation = [0.0; 3];
    }
    let alignment = crate::bone_alignment::calculate(&compiled);
    close(
        alignment[0].unwrap(),
        math::angle_quaternion([0.0, 0.0, 44.775_f32.to_radians()]),
    );
    compiled.animations.push(compiled.animations[0].clone());
    let alignment = crate::bone_alignment::calculate(&compiled);
    let sweep = math::angle_quaternion([0.0, 0.0, 89.55_f32.to_radians()]);
    close(
        alignment[0].unwrap(),
        math::quaternion_normalize([0.0, 0.0, 2.0 * sweep[2], 1.0 + 2.0 * sweep[3]]),
    );
    for animation in &mut compiled.animations {
        animation.delta = true;
    }
    let alignment = crate::bone_alignment::calculate(&compiled);
    close(alignment[0].unwrap(), [0.0, 0.0, 0.0, 1.0]);
}
