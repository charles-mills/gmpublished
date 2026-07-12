//! Pose-math tests: bone transform composition, pose-to-bone matrix
//! inversion, and the deliberate bind-fallback divergence on partial
//! rest channels.

use super::*;
use crate::mdl::studio::{MdlAnimation, MdlAnimationDescription, MdlBodyPart, MdlBone};

const QUAT_IDENTITY_WXYZ: Quat = [1.0, 0.0, 0.0, 0.0];

fn wxyz_to_stored(quat: Quat) -> [f32; 4] {
    [quat[1], quat[2], quat[3], quat[0]]
}

fn quat_z(radians: f32) -> Quat {
    let half = radians * 0.5;
    [half.cos(), 0.0, 0.0, half.sin()]
}

fn bone(name: &str, parent: i32, pos: [f32; 3], quat_wxyz: Quat, pose_to_bone: Mat4) -> MdlBone {
    MdlBone {
        name: name.into(),
        parent,
        position: pos,
        quaternion: wxyz_to_stored(quat_wxyz),
        rotation: [0.0; 3],
        position_scale: [0.0; 3],
        rotation_scale: [0.0; 3],
        pose_to_bone: mat4_to_pose_to_bone_rows(pose_to_bone),
    }
}

fn mat4_to_pose_to_bone_rows(matrix: Mat4) -> [[f32; 4]; 3] {
    [
        [matrix[0][0], matrix[1][0], matrix[2][0], matrix[3][0]],
        [matrix[0][1], matrix[1][1], matrix[2][1], matrix[3][1]],
        [matrix[0][2], matrix[1][2], matrix[2][2], matrix[3][2]],
    ]
}

/// Inverse of a rigid (rotation + translation) transform.
fn rigid_inverse(matrix: Mat4) -> Mat4 {
    let rows = mat4_to_pose_to_bone_rows(matrix);
    let mut inverse_rows = [[0.0_f32; 4]; 3];
    for row in 0..3 {
        for column in 0..3 {
            inverse_rows[row][column] = rows[column][row];
        }
        inverse_rows[row][3] = -(inverse_rows[row][0] * rows[0][3]
            + inverse_rows[row][1] * rows[1][3]
            + inverse_rows[row][2] * rows[2][3]);
    }
    [
        [
            inverse_rows[0][0],
            inverse_rows[1][0],
            inverse_rows[2][0],
            0.0,
        ],
        [
            inverse_rows[0][1],
            inverse_rows[1][1],
            inverse_rows[2][1],
            0.0,
        ],
        [
            inverse_rows[0][2],
            inverse_rows[1][2],
            inverse_rows[2][2],
            0.0,
        ],
        [
            inverse_rows[0][3],
            inverse_rows[1][3],
            inverse_rows[2][3],
            1.0,
        ],
    ]
}

fn mdl_with(
    bones: Vec<MdlBone>,
    static_prop: bool,
    animations: Vec<MdlAnimationDescription>,
) -> Mdl {
    Mdl {
        name: "test".into(),
        checksum: 0,
        version: 48,
        flags: if static_prop { FLAG_STATIC_PROP } else { 0 },
        bones,
        textures: Vec::new(),
        texture_paths: Vec::new(),
        skin_reference_count: 0,
        skin_table: Vec::new(),
        body_parts: Vec::<MdlBodyPart>::new(),
        local_animations: animations,
        sequence_count: 0,
    }
}

fn rest_desc(animations: Vec<MdlAnimation>, frame_count: usize) -> MdlAnimationDescription {
    MdlAnimationDescription {
        name: "@idle".into(),
        fps: 30.0,
        frame_count,
        animations,
    }
}

fn vertex(position: [f32; 3], normal: [f32; 3], bone: u8) -> VvdVertex {
    VvdVertex {
        weights: [1.0, 0.0, 0.0],
        bones: [bone, 0, 0],
        bone_count: 1,
        position,
        normal,
        uv: [0.25, 0.75],
    }
}

fn vvd_with(vertices: Vec<VvdVertex>) -> Vvd {
    Vvd {
        checksum: 0,
        tangents: vec![[0.0; 4]; vertices.len()],
        vertices,
    }
}

fn geometry(mdl: &Mdl, vvd: &Vvd) -> (PreparedModelGeometry, MdlStats) {
    let mut stats = MdlStats::default();
    let static_prop = mdl.flags & FLAG_STATIC_PROP != 0;
    let geometry = prepare_model_geometry(mdl, vvd, static_prop, &mut stats);
    (geometry, stats)
}

fn assert_vec3_near(actual: [f32; 3], expected: [f32; 3], tolerance: f32) {
    for axis in 0..3 {
        assert!(
            (actual[axis] - expected[axis]).abs() <= tolerance,
            "axis {axis}: expected {expected:?}, got {actual:?}"
        );
    }
}

#[test]
fn rest_animation_pose_uses_pose_to_bone_and_overrides_bind() {
    let bind_pose = local_pose_matrix(LocalBonePose {
        position: [0.0; 3],
        rotation: quat_z(std::f32::consts::FRAC_PI_4),
    });
    let rest_quat = quat_z(std::f32::consts::FRAC_PI_4 * 3.0);
    let mdl = mdl_with(
        vec![
            bone("root", -1, [0.0; 3], QUAT_IDENTITY_WXYZ, MAT4_IDENTITY),
            bone(
                "door_l",
                -1,
                [0.0; 3],
                quat_z(std::f32::consts::FRAC_PI_4),
                rigid_inverse(bind_pose),
            ),
            bone("door_r", -1, [0.0; 3], QUAT_IDENTITY_WXYZ, MAT4_IDENTITY),
        ],
        false,
        vec![rest_desc(
            vec![MdlAnimation::for_tests(
                1,
                Some(wxyz_to_stored(rest_quat)),
                Some([0.0; 3]),
            )],
            1,
        )],
    );
    let vvd = vvd_with(vec![vertex([1.0, 0.0, 0.0], [1.0, 0.0, 0.0], 1)]);

    let (geometry, _) = geometry(&mdl, &vvd);
    // Bind is 45°, pose_to_bone its inverse; rest pose 135° -> the
    // vertex lands rotated by the 90° delta.
    let expected = [0.0, 1.0, 0.0];
    assert_vec3_near(geometry.vertices[0].position, expected, 1.0e-4);
    assert_vec3_near(geometry.vertices[0].normal, expected, 1.0e-4);

    // Without the override, pose == bind snaps to the exact input.
    let mut stats = MdlStats::default();
    let bind_only = pose_to_bone_skinning_matrices(&mdl, false, &[None, None, None], &mut stats);
    assert_vec3_near(
        mat4_transform_point(bind_only[1], [1.0, 0.0, 0.0]),
        [1.0, 0.0, 0.0],
        1.0e-5,
    );
}

#[test]
fn multi_frame_desc0_uses_bind_pose_and_counts_it() {
    let bind_pose = local_pose_matrix(LocalBonePose {
        position: [0.0; 3],
        rotation: quat_z(std::f32::consts::FRAC_PI_4),
    });
    let mdl = mdl_with(
        vec![bone(
            "flag",
            -1,
            [0.0; 3],
            quat_z(std::f32::consts::FRAC_PI_4),
            rigid_inverse(bind_pose),
        )],
        false,
        vec![rest_desc(
            vec![MdlAnimation::for_tests(
                0,
                Some(wxyz_to_stored(quat_z(std::f32::consts::FRAC_PI_4 * 3.0))),
                Some([0.0; 3]),
            )],
            2, // multi-frame: not a rest pose
        )],
    );
    let vvd = vvd_with(vec![vertex([1.0, 0.0, 0.0], [1.0, 0.0, 0.0], 0)]);

    let (geometry, stats) = geometry(&mdl, &vvd);
    assert_vec3_near(geometry.vertices[0].position, [1.0, 0.0, 0.0], 1.0e-5);
    assert_eq!(
        stats.sanitized.get(&AssemblySkip::MultiFrameRestAnimation),
        Some(&1)
    );
}

#[test]
fn out_of_range_rest_animation_bone_uses_bind_pose() {
    let bind_pose = local_pose_matrix(LocalBonePose {
        position: [0.0; 3],
        rotation: quat_z(std::f32::consts::FRAC_PI_4),
    });
    let mdl = mdl_with(
        vec![bone(
            "flag",
            -1,
            [0.0; 3],
            quat_z(std::f32::consts::FRAC_PI_4),
            rigid_inverse(bind_pose),
        )],
        false,
        vec![rest_desc(
            vec![MdlAnimation::for_tests(
                255,
                Some(wxyz_to_stored(quat_z(std::f32::consts::FRAC_PI_4 * 3.0))),
                Some([0.0; 3]),
            )],
            1,
        )],
    );
    let vvd = vvd_with(vec![vertex([1.0, 0.0, 0.0], [1.0, 0.0, 0.0], 0)]);

    let (geometry, stats) = geometry(&mdl, &vvd);
    assert_vec3_near(geometry.vertices[0].position, [1.0, 0.0, 0.0], 1.0e-5);
    assert_eq!(
        stats.sanitized.get(&AssemblySkip::RestBoneOutOfRange),
        Some(&1)
    );
}

#[test]
fn pose_equal_bind_snaps_to_exact_stored_vertices() {
    let root_pose = local_pose_matrix(LocalBonePose {
        position: [2.0, 0.0, 0.0],
        rotation: quat_z(std::f32::consts::FRAC_PI_4),
    });
    let child_local = local_pose_matrix(LocalBonePose {
        position: [0.0, 3.0, 0.0],
        rotation: quat_z(-std::f32::consts::FRAC_PI_4),
    });
    let child_chain = mat4_mul(root_pose, child_local);
    let mdl = mdl_with(
        vec![
            bone(
                "root",
                -1,
                [2.0, 0.0, 0.0],
                quat_z(std::f32::consts::FRAC_PI_4),
                rigid_inverse(root_pose),
            ),
            bone(
                "child",
                0,
                [0.0, 3.0, 0.0],
                quat_z(-std::f32::consts::FRAC_PI_4),
                rigid_inverse(child_chain),
            ),
        ],
        false,
        vec![],
    );
    let vvd = vvd_with(vec![vertex([12.25, -4.5, 9.0], [0.0, 1.0, 0.0], 1)]);

    let (geometry, stats) = geometry(&mdl, &vvd);
    assert_eq!(geometry.vertices[0].position, [12.25, -4.5, 9.0]);
    assert_vec3_near(geometry.vertices[0].normal, [0.0, 1.0, 0.0], 1.0e-5);
    assert_eq!(stats.total(), 0);
}

#[test]
fn static_props_keep_stored_vertices() {
    let mdl = mdl_with(
        vec![bone(
            "root",
            -1,
            [0.0; 3],
            quat_z(std::f32::consts::FRAC_PI_2),
            MAT4_IDENTITY,
        )],
        true,
        vec![],
    );
    let vvd = vvd_with(vec![vertex([1.0, 0.0, 0.0], [1.0, 0.0, 0.0], 0)]);

    let (geometry, _) = geometry(&mdl, &vvd);
    assert_vec3_near(geometry.vertices[0].position, [1.0, 0.0, 0.0], 1.0e-5);
    assert_vec3_near(geometry.vertices[0].normal, [1.0, 0.0, 0.0], 1.0e-5);
}

#[test]
fn partial_rest_channels_fall_back_to_bind_not_vmdl_defaults() {
    // A channel with position but no rotation must fall back to the bind
    // rotation, not an arbitrary default, or the bone silently reposes.
    let bind_pose = local_pose_matrix(LocalBonePose {
        position: [5.0, 0.0, 0.0],
        rotation: QUAT_IDENTITY_WXYZ,
    });
    let mdl = mdl_with(
        vec![bone(
            "slide",
            -1,
            [5.0, 0.0, 0.0],
            QUAT_IDENTITY_WXYZ,
            rigid_inverse(bind_pose),
        )],
        false,
        vec![rest_desc(
            vec![MdlAnimation::for_tests(0, None, Some([7.0, 0.0, 0.0]))],
            1,
        )],
    );
    let vvd = vvd_with(vec![vertex([6.0, 1.0, 0.0], [0.0, 0.0, 1.0], 0)]);

    let (geometry, stats) = geometry(&mdl, &vvd);
    // Bind at x=5, rest slides to x=7: the vertex translates +2 with no
    // flip; a 180° X flip would negate y and z.
    assert_vec3_near(geometry.vertices[0].position, [8.0, 1.0, 0.0], 1.0e-4);
    assert_vec3_near(geometry.vertices[0].normal, [0.0, 0.0, 1.0], 1.0e-4);
    assert_eq!(
        stats
            .sanitized
            .get(&AssemblySkip::RestChannelUsedBindFallback),
        Some(&1)
    );
}

#[test]
fn winding_bounds_and_skin_table_helpers() {
    let mut indices = vec![0, 1, 2, 3, 4, 5, 6, 7];
    flip_triangle_winding(&mut indices);
    // Full triangles flip; the trailing partial chunk is untouched.
    assert_eq!(indices, [0, 2, 1, 3, 5, 4, 6, 7]);

    assert_eq!(bounds_from_vertices(&[]), ([0.0; 3], [0.0; 3]));
    let vertices = [
        model_vertex_at([2.0, -1.0, 4.0]),
        model_vertex_at([-3.0, 8.0, 1.5]),
        model_vertex_at([0.25, 3.0, -9.0]),
    ];
    assert_eq!(
        bounds_from_vertices(&vertices),
        ([-3.0, -1.0, -9.0], [2.0, 8.0, 4.0])
    );

    // No skin data -> one identity table sized to the materials.
    let mut mdl = mdl_with(vec![], true, vec![]);
    mdl.textures = vec!["a".into(), "b".into(), "c".into()];
    assert_eq!(skin_tables(&mdl), [[0u16, 1, 2]]);
    // Two families; slots past the table fall back to identity.
    mdl.skin_reference_count = 2;
    mdl.skin_table = vec![0, 1, 1, 0];
    assert_eq!(skin_tables(&mdl), [[0u16, 1, 2], [1, 0, 2]]);
}

fn model_vertex_at(position: [f32; 3]) -> ModelVertex {
    ModelVertex {
        position,
        normal: [0.0, 1.0, 0.0],
        uv: [0.0; 2],
    }
}
