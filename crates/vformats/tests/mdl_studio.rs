//! `.mdl` differential tests: a full synthetic studio model parsed by
//! this crate and by `vmdl`. Agreement is asserted wherever vmdl is
//! correct; where it is defective (model record stride, RLE value
//! sign, euler→quaternion conversion), the tests prove both sides read
//! the same bytes and pin OUR output to the studio.h-correct value.

use vformats::Limits;
use vformats::mdl::{FLAG_STATIC_PROP, MdlError, parse_mdl};

const HEADER_BYTES: usize = 408;
const BONE_BYTES: usize = 216;
const TEXTURE_BYTES: usize = 64;
const MODEL_BYTES: usize = 148;
const MESH_BYTES: usize = 116;
const ANIM_DESC_BYTES: usize = 100;

fn put_i32(b: &mut [u8], at: usize, v: i32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_f32(b: &mut [u8], at: usize, v: f32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Encode a quaternion (x, y, z, w) as Quaternion48.
fn quaternion48(q: [f32; 4]) -> [u8; 6] {
    let x = ((q[0] * 32768.0 + 32768.0).round()).clamp(0.0, 65535.0) as u16;
    let y = ((q[1] * 32768.0 + 32768.0).round()).clamp(0.0, 65535.0) as u16;
    let mut z = ((q[2] * 16384.0 + 16384.0).round()).clamp(0.0, 32767.0) as u16;
    if q[3] < 0.0 {
        z |= 0x8000;
    }
    let mut out = [0u8; 6];
    out[0..2].copy_from_slice(&x.to_le_bytes());
    out[2..4].copy_from_slice(&y.to_le_bytes());
    out[4..6].copy_from_slice(&z.to_le_bytes());
    out
}

fn f16_bits(v: f32) -> u16 {
    // Enough for the small test constants: exact halves.
    match v {
        0.0 => 0x0000,
        0.5 => 0x3800,
        1.0 => 0x3C00,
        2.0 => 0x4000,
        -1.0 => 0xBC00,
        _ => panic!("extend f16_bits for {v}"),
    }
}

struct Fixture {
    bytes: Vec<u8>,
}

/// Two bones, two textures + dirs, 2x2 skins, one bodypart with two
/// models (2 meshes then 1), two animation descriptions:
/// `@idle` (RAWROT q48 + RAWPOS v48, bone 1) and `motion` (ANIMROT +
/// ANIMPOS RLE over 3 frames, bone 0, including a negative RLE value).
fn build_mdl() -> Fixture {
    let bone_offset = HEADER_BYTES;
    let texture_offset = bone_offset + 2 * BONE_BYTES;
    let texture_dir_offset = texture_offset + 2 * TEXTURE_BYTES;
    let skin_offset = texture_dir_offset + 8;
    let body_part_offset = skin_offset + 8;
    let model_offset_abs = body_part_offset + 16;
    let mesh_offset_abs = model_offset_abs + 2 * MODEL_BYTES;
    let anim_desc_offset = mesh_offset_abs + 3 * MESH_BYTES;
    let anim_data_offset = anim_desc_offset + 2 * ANIM_DESC_BYTES;
    // Anim A: header(4) + q48(6) + v48(6) = 16 bytes.
    // Anim B: header(4) + rot pointers(6) + pos pointers(6) + rot spans + pos spans.
    let anim_b_offset = anim_data_offset + 16;
    let string_pool = anim_b_offset + 64;

    let mut pool = Vec::new();
    let mut b = vec![0u8; string_pool];
    let intern = |pool: &mut Vec<u8>, s: &str| -> usize {
        let at = string_pool + pool.len();
        pool.extend_from_slice(s.as_bytes());
        pool.push(0);
        at
    };

    // ---- header ----
    b[0..4].copy_from_slice(b"IDST");
    put_i32(&mut b, 4, 48); // version
    put_i32(&mut b, 8, 0x0BADF00Du32 as i32); // checksum
    b[12..12 + 10].copy_from_slice(b"test.mdl\0\0");
    put_i32(&mut b, 152, FLAG_STATIC_PROP as i32); // flags
    put_i32(&mut b, 156, 2); // bones
    put_i32(&mut b, 160, bone_offset as i32);
    put_i32(&mut b, 180, 2); // local animations
    put_i32(&mut b, 184, anim_desc_offset as i32);
    // Sequence count stays 0: vmdl materializes sequence records
    // (we only read the count), so a nonzero count needs real records.
    put_i32(&mut b, 204, 2); // textures
    put_i32(&mut b, 208, texture_offset as i32);
    put_i32(&mut b, 212, 2); // texture dirs
    put_i32(&mut b, 216, texture_dir_offset as i32);
    put_i32(&mut b, 220, 2); // skin references
    put_i32(&mut b, 224, 2); // skin families
    put_i32(&mut b, 228, skin_offset as i32);
    put_i32(&mut b, 232, 1); // body parts
    put_i32(&mut b, 236, body_part_offset as i32);
    // vmdl reads these too; keep them in-bounds and harmless.
    put_i32(&mut b, 308, (string_pool) as i32); // surface prop -> ""
    put_i32(&mut b, 348, (string_pool) as i32); // anim block names -> ""
    pool.push(0); // the "" both point at

    // ---- bones ----
    let bone_quat = [0.0f32, 0.0, 0.382_683_43, 0.923_879_5]; // 45° yaw
    for (index, (parent, name)) in [(-1i32, "root"), (0, "door")].iter().enumerate() {
        let base = bone_offset + index * BONE_BYTES;
        let name_at = intern(&mut pool, name);
        put_i32(&mut b, base, (name_at - base) as i32);
        put_i32(&mut b, base + 4, *parent);
        for slot in 0..6 {
            put_i32(&mut b, base + 8 + slot * 4, -1); // bone controllers
        }
        // pos, quat, rot, pos_scale, rot_scale
        put_f32(&mut b, base + 32, 1.0 + index as f32);
        put_f32(&mut b, base + 36, 2.0);
        put_f32(&mut b, base + 40, 3.0);
        for (slot, v) in bone_quat.iter().enumerate() {
            put_f32(&mut b, base + 44 + slot * 4, *v);
        }
        put_f32(&mut b, base + 60, 0.1);
        put_f32(&mut b, base + 64, 0.2);
        put_f32(&mut b, base + 68, 0.3);
        for slot in 0..3 {
            put_f32(&mut b, base + 72 + slot * 4, 0.25 * (slot as f32 + 1.0)); // pos_scale
            put_f32(&mut b, base + 84 + slot * 4, 0.5 * (slot as f32 + 1.0)); // rot_scale
        }
        // pose_to_bone: identity rows with a translation column.
        for row in 0..3 {
            put_f32(&mut b, base + 96 + (row * 4 + row) * 4, 1.0);
            put_f32(
                &mut b,
                base + 96 + (row * 4 + 3) * 4,
                10.0 * (row as f32 + 1.0),
            );
        }
        put_i32(&mut b, base + 176, (string_pool - base) as i32); // surface prop ""
    }

    // ---- textures + dirs ----
    for (index, name) in ["metal/wall01", "glass\\pane"].iter().enumerate() {
        let base = texture_offset + index * TEXTURE_BYTES;
        let name_at = intern(&mut pool, name);
        put_i32(&mut b, base, (name_at - base) as i32);
    }
    for (index, dir) in ["models\\props\\", "materials/shared/"].iter().enumerate() {
        let at = intern(&mut pool, dir);
        put_i32(&mut b, texture_dir_offset + index * 4, at as i32);
    }

    // ---- skins: family 0 = [0, 1], family 1 = [1, 0] ----
    for (slot, v) in [0u16, 1, 1, 0].iter().enumerate() {
        b[skin_offset + slot * 2..skin_offset + slot * 2 + 2].copy_from_slice(&v.to_le_bytes());
    }

    // ---- bodypart with two models ----
    put_i32(&mut b, body_part_offset + 4, 2); // model count
    put_i32(
        &mut b,
        body_part_offset + 12,
        (model_offset_abs - body_part_offset) as i32,
    );
    for (index, (name, mesh_count, mesh_local, vertex_index)) in
        [("choice_a", 2i32, 0usize, 0i32), ("choice_b", 1, 2, 4 * 48)]
            .iter()
            .enumerate()
    {
        let base = model_offset_abs + index * MODEL_BYTES;
        b[base..base + name.len()].copy_from_slice(name.as_bytes());
        put_i32(&mut b, base + 72, *mesh_count);
        put_i32(
            &mut b,
            base + 76,
            (mesh_offset_abs + mesh_local * MESH_BYTES - base) as i32,
        );
        put_i32(&mut b, base + 84, *vertex_index);
    }
    for (index, (material, vertex_offset)) in [(0i32, 0i32), (1, 2), (0, 0)].iter().enumerate() {
        let base = mesh_offset_abs + index * MESH_BYTES;
        put_i32(&mut b, base, *material);
        put_i32(&mut b, base + 12, *vertex_offset);
    }

    // ---- animation descriptions ----
    let idle_name = intern(&mut pool, "@idle");
    let motion_name = intern(&mut pool, "motion");
    let desc_a = anim_desc_offset;
    put_i32(&mut b, desc_a + 4, (idle_name - desc_a) as i32);
    put_f32(&mut b, desc_a + 8, 30.0);
    put_i32(&mut b, desc_a + 16, 1); // frame count
    put_i32(&mut b, desc_a + 56, (anim_data_offset - desc_a) as i32);
    let desc_b = anim_desc_offset + ANIM_DESC_BYTES;
    put_i32(&mut b, desc_b + 4, (motion_name - desc_b) as i32);
    put_f32(&mut b, desc_b + 8, 24.0);
    put_i32(&mut b, desc_b + 16, 3); // frame count
    put_i32(&mut b, desc_b + 56, (anim_b_offset - desc_b) as i32);

    // Anim A: bone 1, RAWROT | RAWPOS, terminal.
    let a = anim_data_offset;
    b[a] = 1;
    b[a + 1] = 0x01 | 0x02;
    b[a + 4..a + 10].copy_from_slice(&quaternion48([
        0.0,
        0.0,
        std::f32::consts::FRAC_1_SQRT_2,
        std::f32::consts::FRAC_1_SQRT_2,
    ]));
    for (slot, v) in [1.0f32, 0.5, 2.0].iter().enumerate() {
        let bits = f16_bits(*v);
        b[a + 10 + slot * 2..a + 12 + slot * 2].copy_from_slice(&bits.to_le_bytes());
    }

    // Anim B: bone 0, ANIMROT | ANIMPOS, terminal. Pointer structs at
    // +4 (rotation) and +10 (position); spans directly after.
    let bb = anim_b_offset;
    b[bb] = 0;
    b[bb + 1] = 0x04 | 0x08;
    let rot_ptr = bb + 4;
    let pos_ptr = bb + 10;
    let rot_span = bb + 16;
    // Rotation: only component 0 animated: span {valid: 3, total: 3},
    // values 10, -6, 4 (the -6 probes vmdl's unsigned read).
    put_span(&mut b, rot_span, 3, 3, &[10, -6, 4]);
    let pos_span = rot_span + 2 + 6;
    // Position: only component 2 animated: {valid: 1, total: 3}, value 7
    // (frames 1..3 reuse it), followed by a zero-total terminator.
    put_span(&mut b, pos_span, 1, 3, &[7]);
    b[pos_span + 4] = 0; // terminator span: valid 0
    b[pos_span + 5] = 0; // total 0
    for (slot, target) in [(0usize, rot_span), (1, 0), (2, 0)] {
        let rel = if target == 0 { 0 } else { target - rot_ptr } as u16;
        b[rot_ptr + slot * 2..rot_ptr + slot * 2 + 2].copy_from_slice(&rel.to_le_bytes());
    }
    for (slot, target) in [(0usize, 0), (1, 0), (2, pos_span)] {
        let rel = if target == 0 { 0 } else { target - pos_ptr } as u16;
        b[pos_ptr + slot * 2..pos_ptr + slot * 2 + 2].copy_from_slice(&rel.to_le_bytes());
    }

    b.extend_from_slice(&pool);
    Fixture { bytes: b }
}

fn put_span(b: &mut [u8], at: usize, valid: u8, total: u8, values: &[i16]) {
    b[at] = valid;
    b[at + 1] = total;
    for (slot, v) in values.iter().enumerate() {
        b[at + 2 + slot * 2..at + 4 + slot * 2].copy_from_slice(&v.to_le_bytes());
    }
}

#[test]
fn parses_and_matches_vmdl_where_vmdl_is_correct() {
    let fixture = build_mdl();
    let ours = parse_mdl(&fixture.bytes, &Limits::default()).expect("our parse");
    let theirs = vmdl::Mdl::read(&fixture.bytes).expect("vmdl parse");

    assert_eq!(ours.name, "test.mdl");
    assert_eq!(ours.checksum, 0x0BADF00D);
    assert_eq!(ours.version, 48);
    assert_ne!(ours.flags & FLAG_STATIC_PROP, 0);
    assert!(
        theirs
            .header
            .flags
            .contains(vmdl::mdl::ModelFlags::STATIC_PROP)
    );
    assert_eq!(ours.sequence_count, 0);

    // Bones.
    assert_eq!(ours.bones.len(), theirs.bones.len());
    for (a, b) in ours.bones.iter().zip(&theirs.bones) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.parent, b.parent);
        assert_eq!(a.position, [b.pos.x, b.pos.y, b.pos.z]);
        assert_eq!(
            a.quaternion,
            [
                b.quaternion.x,
                b.quaternion.y,
                b.quaternion.z,
                b.quaternion.w
            ]
        );
        assert_eq!(a.rotation, [b.rot.x, b.rot.y, b.rot.z]);
        assert_eq!(
            a.position_scale,
            [b.pos_scale.x, b.pos_scale.y, b.pos_scale.z]
        );
        let theirs_p2b: [[f32; 4]; 3] = bytemuck::cast(b.pose_to_bone);
        assert_eq!(a.pose_to_bone, theirs_p2b);
    }

    // Textures, dirs (both normalize backslashes), skins.
    assert_eq!(ours.textures, ["metal/wall01", "glass/pane"]);
    assert_eq!(
        ours.textures,
        theirs
            .textures
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(ours.texture_paths, ["models/props/", "materials/shared/"]);
    assert_eq!(ours.texture_paths, theirs.texture_paths);
    assert_eq!(ours.skin_table, [0, 1, 1, 0]);
    assert_eq!(ours.skin_table, theirs.skin_table);
    assert_eq!(ours.skin_reference_count, 2);

    // Bodypart tree: model 0 agrees with vmdl...
    assert_eq!(ours.body_parts.len(), 1);
    let our_models = &ours.body_parts[0].models;
    let their_models = &theirs.body_parts[0].models;
    assert_eq!(our_models.len(), their_models.len());
    assert_eq!(our_models[0].name, "choice_a");
    assert_eq!(their_models[0].name.as_str(), "choice_a");
    assert_eq!(our_models[0].vertex_offset, 0);
    assert_eq!(their_models[0].vertex_offset, 0);
    assert_eq!(
        our_models[0]
            .meshes
            .iter()
            .map(|m| (m.material, m.vertex_offset))
            .collect::<Vec<_>>(),
        their_models[0]
            .meshes
            .iter()
            .map(|m| (m.material, m.vertex_offset))
            .collect::<Vec<_>>()
    );

    // ...model 1 is where vmdl's 144-byte stride defect bites: it reads
    // the second record 4 bytes early, landing the name field on the
    // previous record's zero padding. Ours reads studio.h's 148.
    assert_eq!(our_models[1].name, "choice_b");
    assert_eq!(our_models[1].vertex_offset, 4);
    assert_eq!(our_models[1].meshes.len(), 1);
    assert_eq!(
        their_models[1].name.as_str(),
        "",
        "vmdl 0.2.0's model stride defect appears fixed — re-audit this probe"
    );

    // Animation A (RAWROT + RAWPOS): both decode identically.
    let our_idle = &ours.local_animations[0];
    let their_idle = &theirs.local_animations[0];
    assert_eq!(our_idle.name, their_idle.name);
    assert_eq!(our_idle.frame_count, 1);
    assert_eq!(our_idle.animations.len(), 1);
    assert_eq!(our_idle.animations[0].bone, 1);
    let ours_q = our_idle.animations[0].rotation(0);
    let theirs_q = their_idle.animations[0].rotation(0);
    let theirs_q = [theirs_q.x, theirs_q.y, theirs_q.z, theirs_q.w];
    for (a, b) in ours_q.iter().zip(theirs_q) {
        assert!((a - b).abs() < 1e-6, "q48 {ours_q:?} vs {theirs_q:?}");
    }
    let their_pos = their_idle.animations[0].position(0);
    assert_eq!(our_idle.animations[0].position(0), [1.0, 0.5, 2.0]);
    assert_eq!([their_pos.x, their_pos.y, their_pos.z], [1.0, 0.5, 2.0]);
}

#[test]
fn rle_animation_decodes_signed_values_with_scale_and_axis_fixup() {
    let fixture = build_mdl();
    let ours = parse_mdl(&fixture.bytes, &Limits::default()).expect("our parse");
    let motion = &ours.local_animations[1];
    assert_eq!(motion.name, "motion");
    assert_eq!(motion.frame_count, 3);
    let channel = &motion.animations[0];
    assert_eq!(channel.bone, 0);

    // Position: component 2 animated with value 7, pos_scale[2] = 0.75;
    // frames past `valid` reuse the last stored value.
    for frame in 0..3 {
        assert_eq!(channel.position(frame), [0.0, 0.0, 7.0 * 0.75]);
    }

    // Rotation eulers: raw component 0 = [10, -6, 4]; the axis fixup
    // maps final (roll, pitch, yaw) = (v2*s2, v0*s0, v1*s1) with
    // rot_scale = [0.5, 1.0, 1.5], so pitch = v0 * 0.5 and the rest 0.
    // Frame 1's -6 is the signed-read probe: vmdl decodes it as 65530.
    let expected_pitch = [5.0f32, -3.0, 2.0];
    for (frame, pitch) in expected_pitch.iter().enumerate() {
        let quat = channel.rotation(frame);
        let expected = angle_quaternion_reference([0.0, *pitch, 0.0]);
        for (a, b) in quat.iter().zip(expected) {
            assert!(
                (a - b).abs() < 1e-6,
                "frame {frame}: {quat:?} vs {expected:?}"
            );
        }
    }

    // Same bytes through vmdl: frame 0 (positive value) agrees with our
    // decoded euler when converted with vmdl's own defective formula —
    // proving both read identical spans; frame 1 shows the sign defect.
    let theirs = vmdl::Mdl::read(&fixture.bytes).expect("vmdl parse");
    let their_motion = &theirs.local_animations[1];
    let their_q0 = their_motion.animations[0].rotation(0);
    let vmdl_style = vmdl_defective_euler_to_quat([0.0, 5.0, 0.0]);
    for (a, b) in [their_q0.x, their_q0.y, their_q0.z, their_q0.w]
        .iter()
        .zip(vmdl_style)
    {
        assert!((a - b).abs() < 1e-5, "vmdl frame 0 read differs");
    }
    let their_q1 = their_motion.animations[0].rotation(1);
    let unsigned_read = vmdl_defective_euler_to_quat([0.0, 65530.0 * 0.5, 0.0]);
    for (a, b) in [their_q1.x, their_q1.y, their_q1.z, their_q1.w]
        .iter()
        .zip(unsigned_read)
    {
        assert!(
            (a - b).abs() < 1e-5,
            "vmdl 0.2.0's unsigned RLE read appears fixed — re-audit this probe"
        );
    }
}

/// studiomdl AngleQuaternion (the correct conversion), reimplemented
/// independently for the assertion.
fn angle_quaternion_reference(euler: [f32; 3]) -> [f32; 4] {
    let (sr, cr) = (euler[0] * 0.5).sin_cos();
    let (sp, cp) = (euler[1] * 0.5).sin_cos();
    let (sy, cy) = (euler[2] * 0.5).sin_cos();
    [
        sr * cp * cy - cr * sp * sy,
        cr * sp * cy + sr * cp * sy,
        cr * cp * sy - sr * sp * cy,
        cr * cp * cy + sr * sp * sy,
    ]
}

/// vmdl's euler→quaternion: q_y(pitch) * q_x(-roll) * q_z(yaw), with
/// the euler pre-permuted by its scale fixup — reproduced here only to
/// prove byte-level agreement on the decoded values.
fn vmdl_defective_euler_to_quat(euler: [f32; 3]) -> [f32; 4] {
    fn axis(v: f32, x: f32, y: f32, z: f32) -> [f32; 4] {
        let (s, c) = (v * 0.5).sin_cos();
        [x * s, y * s, z * s, c]
    }
    fn mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
        [
            a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
            a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
            a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
            a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
        ]
    }
    mul(
        mul(
            axis(euler[1], 0.0, 1.0, 0.0),
            axis(-euler[0], 1.0, 0.0, 0.0),
        ),
        axis(euler[2], 0.0, 0.0, 1.0),
    )
}

#[test]
fn rejects_malformed_mdl() {
    assert!(matches!(
        parse_mdl(b"TSDI backwards", &Limits::default()),
        Err(MdlError::BadMagic { part: "mdl" })
    ));

    let fixture = build_mdl();
    let mut wrong_version = fixture.bytes.clone();
    put_i32(&mut wrong_version, 4, 40);
    assert!(matches!(
        parse_mdl(&wrong_version, &Limits::default()),
        Err(MdlError::UnsupportedVersion {
            part: "mdl",
            version: 40
        })
    ));

    assert!(matches!(
        parse_mdl(&fixture.bytes[..300], &Limits::default()),
        Err(MdlError::Truncated { .. })
    ));

    let cap = Limits {
        max_entries: 1,
        ..Limits::default()
    };
    assert!(matches!(
        parse_mdl(&fixture.bytes, &cap),
        Err(MdlError::TooMany { .. })
    ));
}
