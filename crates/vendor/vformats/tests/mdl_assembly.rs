//! End-to-end: a minimal but complete .mdl/.vvd/.vtx triple assembled
//! through the public API, with hand-computed expected geometry.

use vformats::Limits;
use vformats::mdl::{
    AssemblySkip, MdlError, assemble, assemble_lossy, parse_mdl, parse_vtx, parse_vvd,
};

fn put_i32(b: &mut [u8], at: usize, v: i32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Static prop, 1 bone, 1 texture + dir, 1 bodypart/model/mesh.
fn build_mdl() -> Vec<u8> {
    let bone_offset = 408;
    let texture_offset = bone_offset + 216;
    let texture_dir_offset = texture_offset + 64;
    let body_part_offset = texture_dir_offset + 4;
    let model_offset = body_part_offset + 16;
    let mesh_offset = model_offset + 148;
    let pool = mesh_offset + 116;

    let mut b = vec![0u8; pool];
    b[0..4].copy_from_slice(b"IDST");
    put_i32(&mut b, 4, 48);
    b[12..17].copy_from_slice(b"cube\0");
    put_i32(&mut b, 152, 0x10); // STATIC_PROP
    put_i32(&mut b, 156, 1); // bones
    put_i32(&mut b, 160, bone_offset as i32);
    put_i32(&mut b, 204, 1); // textures
    put_i32(&mut b, 208, texture_offset as i32);
    put_i32(&mut b, 212, 1); // texture dirs
    put_i32(&mut b, 216, texture_dir_offset as i32);
    put_i32(&mut b, 232, 1); // body parts
    put_i32(&mut b, 236, body_part_offset as i32);

    let mut pool_bytes = Vec::new();
    let intern = |pool_bytes: &mut Vec<u8>, s: &str| -> usize {
        let at = pool + pool_bytes.len();
        pool_bytes.extend_from_slice(s.as_bytes());
        pool_bytes.push(0);
        at
    };
    let bone_name = intern(&mut pool_bytes, "static_prop");
    put_i32(&mut b, bone_offset, (bone_name - bone_offset) as i32);
    put_i32(&mut b, bone_offset + 4, -1); // no parent
    // Identity bind quaternion in stored [x, y, z, w] order.
    b[bone_offset + 56..bone_offset + 60].copy_from_slice(&1.0f32.to_le_bytes());

    let texture_name = intern(&mut pool_bytes, "props/crate01");
    put_i32(
        &mut b,
        texture_offset,
        (texture_name - texture_offset) as i32,
    );
    let dir = intern(&mut pool_bytes, "models/props/");
    put_i32(&mut b, texture_dir_offset, dir as i32);

    put_i32(&mut b, body_part_offset + 4, 1); // one model
    put_i32(
        &mut b,
        body_part_offset + 12,
        (model_offset - body_part_offset) as i32,
    );
    b[model_offset..model_offset + 4].copy_from_slice(b"lod\0");
    put_i32(&mut b, model_offset + 72, 1); // one mesh
    put_i32(
        &mut b,
        model_offset + 76,
        (mesh_offset - model_offset) as i32,
    );
    // material 0, vertex offsets 0: mesh header stays zeroed.

    b.extend_from_slice(&pool_bytes);
    b
}

fn build_vvd(positions: &[[f32; 3]]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"IDSV");
    b.extend_from_slice(&4i32.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&1i32.to_le_bytes()); // lod count
    b.extend_from_slice(&(positions.len() as i32).to_le_bytes());
    b.extend_from_slice(&[0; 28]);
    b.extend_from_slice(&0i32.to_le_bytes()); // fixups
    b.extend_from_slice(&64i32.to_le_bytes()); // fixup offset
    b.extend_from_slice(&64i32.to_le_bytes()); // vertex offset
    b.extend_from_slice(&((64 + positions.len() * 48) as i32).to_le_bytes());
    for position in positions {
        b.extend_from_slice(&1.0f32.to_le_bytes()); // weight 0
        b.extend_from_slice(&[0; 8]); // weights 1-2
        b.extend_from_slice(&[0, 0, 0, 1]); // bones + count
        for v in position.iter().chain(&[0.0, 0.0, 1.0]) {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&0.5f32.to_le_bytes()); // u
        b.extend_from_slice(&0.25f32.to_le_bytes()); // v
    }
    b.extend_from_slice(&vec![0u8; positions.len() * 16]); // tangents
    b
}

/// One bodypart/model/lod/mesh/group: `vertex_count` records mapping
/// 1:1, a tri-list over all indices.
fn build_vtx(vertex_count: u16) -> Vec<u8> {
    let mut b = vec![0u8; 36];
    put_i32(&mut b, 0, 7);
    put_i32(&mut b, 20, 1); // lod count
    put_i32(&mut b, 28, 1); // body parts
    put_i32(&mut b, 32, 36);
    let bp = 36;
    b.extend_from_slice(&[0; 8]);
    let model = b.len();
    b.extend_from_slice(&[0; 8]);
    let lod = b.len();
    b.extend_from_slice(&[0; 12]);
    let mesh = b.len();
    b.extend_from_slice(&[0; 9]);
    let group = b.len();
    b.extend_from_slice(&[0; 25]);
    let vertex_data = b.len();
    for id in 0..vertex_count {
        let mut record = [0u8; 9];
        record[3] = 1;
        record[4..6].copy_from_slice(&id.to_le_bytes());
        b.extend_from_slice(&record);
    }
    let index_data = b.len();
    for id in 0..vertex_count {
        b.extend_from_slice(&id.to_le_bytes());
    }
    let strip = b.len();
    let mut record = [0u8; 27];
    record[0..4].copy_from_slice(&i32::from(vertex_count).to_le_bytes());
    record[18] = 0x01; // tri list
    b.extend_from_slice(&record);

    put_i32(&mut b, bp, 1);
    put_i32(&mut b, bp + 4, (model - bp) as i32);
    put_i32(&mut b, model, 1);
    put_i32(&mut b, model + 4, (lod - model) as i32);
    put_i32(&mut b, lod, 1);
    put_i32(&mut b, lod + 4, (mesh - lod) as i32);
    put_i32(&mut b, mesh, 1);
    put_i32(&mut b, mesh + 4, (group - mesh) as i32);
    put_i32(&mut b, group, i32::from(vertex_count));
    put_i32(&mut b, group + 4, (vertex_data - group) as i32);
    put_i32(&mut b, group + 8, i32::from(vertex_count));
    put_i32(&mut b, group + 12, (index_data - group) as i32);
    put_i32(&mut b, group + 16, 1);
    put_i32(&mut b, group + 20, (strip - group) as i32);
    b
}

#[test]
fn assembles_a_static_prop_triangle_end_to_end() {
    let limits = Limits::default();
    let positions = [[0.0f32, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
    let mdl = parse_mdl(&build_mdl(), &limits).expect("mdl");
    let vvd = parse_vvd(&build_vvd(&positions), &limits).expect("vvd");
    let vtx = parse_vtx(&build_vtx(3), &limits).expect("vtx");

    let read = assemble(&mdl, &vvd, &vtx).expect("assemble");
    assert_eq!(read.stats.total(), 0, "clean input degrades nothing");

    let model = &read.model;
    assert_eq!(model.material_names, ["props/crate01"]);
    assert_eq!(model.material_dirs, ["models/props/"]);
    assert_eq!(model.skin_tables, [[0u16]]);
    assert_eq!(model.bodygroups, [1]);
    assert_eq!(model.bone_count, 1);
    assert_eq!(model.vertex_count, 3);
    assert_eq!(model.triangle_count, 1);
    assert_eq!(model.bounds_min, [0.0, 0.0, 0.0]);
    assert_eq!(model.bounds_max, [4.0, 4.0, 0.0]);

    let mesh = &model.meshes[0];
    assert_eq!((mesh.bodygroup, mesh.bodygroup_choice), (0, 0));
    assert_eq!(mesh.material_index, 0);
    // Static prop: stored vertices untouched.
    assert_eq!(mesh.vertices.len(), 3);
    for vertex in &mesh.vertices {
        assert_eq!(vertex.normal, [0.0, 0.0, 1.0]);
        assert_eq!(vertex.uv, [0.5, 0.25]);
    }
    // Tri-list emission reverses the pool ([2,1,0] -> vertices in that
    // first-use order), then the winding flip swaps the tail pair.
    assert_eq!(mesh.vertices[0].position, positions[2]);
    assert_eq!(mesh.vertices[1].position, positions[1]);
    assert_eq!(mesh.vertices[2].position, positions[0]);
    assert_eq!(mesh.indices, [0, 2, 1]);
}

#[test]
fn assembly_rejects_bad_material_and_vertex_references() {
    let limits = Limits::default();
    let positions = [[0.0f32, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
    let mdl = parse_mdl(&build_mdl(), &limits).expect("mdl");
    let vvd = parse_vvd(&build_vvd(&positions), &limits).expect("vvd");

    // Strip vertex ids past the vvd pool must fail loudly.
    let vtx = parse_vtx(&build_vtx(5), &limits).expect("vtx parses");
    assert!(matches!(
        assemble(&mdl, &vvd, &vtx),
        Err(vformats::mdl::MdlError::VertexIndex { .. })
    ));

    // A mesh material outside the texture table must fail loudly.
    let mut bad_material = build_mdl();
    let mesh_offset = 408 + 216 + 64 + 4 + 16 + 148;
    put_i32(&mut bad_material, mesh_offset, 3);
    let mdl = parse_mdl(&bad_material, &limits).expect("mdl");
    let vtx = parse_vtx(&build_vtx(3), &limits).expect("vtx");
    assert!(matches!(
        assemble(&mdl, &vvd, &vtx),
        Err(vformats::mdl::MdlError::MaterialIndex { index: 3 })
    ));
}

/// `Vtx` is freely constructible, so `assemble` cannot trust the
/// parse-time strip-range validation: a caller-corrupted strip must
/// error, not index out of bounds.
#[test]
fn assembly_rejects_caller_corrupted_strip_ranges() {
    let limits = Limits::default();
    let positions = [[0.0f32, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
    let mdl = parse_mdl(&build_mdl(), &limits).expect("mdl");
    let vvd = parse_vvd(&build_vvd(&positions), &limits).expect("vvd");
    let mut vtx = parse_vtx(&build_vtx(3), &limits).expect("vtx");

    let strip = &mut vtx.body_parts[0].models[0].lods[0].meshes[0].strip_groups[0].strips[0];
    strip.index_count += 1000;

    assert!(matches!(
        assemble(&mdl, &vvd, &vtx),
        Err(vformats::mdl::MdlError::Corrupt { .. })
    ));
}

#[test]
fn checksum_mismatch_errors_strict_and_is_recorded_lossy() {
    let limits = Limits::default();
    let positions = [[0.0f32, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
    let mdl = parse_mdl(&build_mdl(), &limits).expect("mdl");
    let mut vvd_bytes = build_vvd(&positions);
    vvd_bytes[8..12].copy_from_slice(&0xDEAD_BEEF_u32.to_le_bytes());
    let vvd = parse_vvd(&vvd_bytes, &limits).expect("vvd");
    let vtx = parse_vtx(&build_vtx(3), &limits).expect("vtx");

    assert_eq!(
        assemble(&mdl, &vvd, &vtx),
        Err(MdlError::ChecksumMismatch {
            mdl: 0,
            vvd: 0xDEAD_BEEF,
            vtx: 0,
        })
    );

    let read = assemble_lossy(&mdl, &vvd, &vtx).expect("lossy assembly still succeeds");
    assert_eq!(
        read.stats.sanitized.get(&AssemblySkip::ChecksumMismatch),
        Some(&1)
    );
    assert_eq!(
        read.model.meshes[0].vertices.len(),
        3,
        "geometry still assembled despite the mismatch"
    );
}

#[test]
fn mesh_count_mismatch_errors_strict_and_truncates_lossy() {
    let limits = Limits::default();
    let positions = [[0.0f32, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
    let mdl = parse_mdl(&build_mdl(), &limits).expect("mdl");
    let vvd = parse_vvd(&build_vvd(&positions), &limits).expect("vvd");
    let mut vtx = parse_vtx(&build_vtx(3), &limits).expect("vtx");
    vtx.body_parts[0].models[0].lods[0].meshes.clear();

    assert_eq!(
        assemble(&mdl, &vvd, &vtx),
        Err(MdlError::MeshCountMismatch { mdl: 1, vtx: 0 })
    );

    let read = assemble_lossy(&mdl, &vvd, &vtx).expect("lossy assembly still succeeds");
    assert_eq!(
        read.stats.sanitized.get(&AssemblySkip::MeshCountMismatch),
        Some(&1)
    );
    assert!(
        read.model.meshes.is_empty(),
        "zip-truncated to the shorter (empty) mesh list"
    );
}
