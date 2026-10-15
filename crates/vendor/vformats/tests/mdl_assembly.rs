//! End-to-end: a minimal but complete .mdl/.vvd/.vtx triple assembled
//! through the public API, with hand-computed expected geometry.

use vformats::Limits;
use vformats::mdl::{
    AssemblySkip, MdlError, ModelData, ModelLightingVertex, VhvError, assemble, assemble_lossy,
    parse_mdl, parse_vhv, parse_vtx, parse_vvd,
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
    for _ in positions {
        for value in [1.0_f32, 0.0, 0.0, 1.0] {
            b.extend_from_slice(&value.to_le_bytes());
        }
    }
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
fn compression_options_preserve_authored_frames_and_default_geometry() {
    use vformats::mdl::{
        ModelAssemblyOptions, VertexCompression, assemble_lossy_with_options, source_ubyte4_vector,
    };
    let limits = Limits::default();
    let mdl = parse_mdl(&build_mdl(), &limits).unwrap();
    let mut vvd = parse_vvd(
        &build_vvd(&[[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]),
        &limits,
    )
    .unwrap();
    for vertex in &mut vvd.vertices {
        vertex.normal = [0.2, 0.3, 0.5];
    }
    for tangent in &mut vvd.tangents {
        *tangent = [0.3, 0.2, 0.5, -1.0];
    }
    let vtx = parse_vtx(&build_vtx(3), &limits).unwrap();
    let raw = assemble(&mdl, &vvd, &vtx).unwrap().model;
    let packed = assemble_lossy_with_options(
        &mdl,
        &vvd,
        &vtx,
        ModelAssemblyOptions {
            vertex_compression: VertexCompression::SourceUbyte4,
        },
    )
    .unwrap()
    .model;
    assert_eq!(raw.vertex_compression, VertexCompression::None);
    assert_eq!(packed.vertex_compression, VertexCompression::SourceUbyte4);
    assert_eq!(raw.source_frames, packed.source_frames);
    assert_eq!(raw.lighting_compatible, packed.lighting_compatible);
    assert_eq!(raw.lighting_meshes, packed.lighting_meshes);
    for (raw_mesh, packed_mesh) in raw.meshes.iter().zip(&packed.meshes) {
        assert_eq!(raw_mesh.indices, packed_mesh.indices);
        assert_eq!(raw_mesh.vertex_lighting, packed_mesh.vertex_lighting);
        for (raw_vertex, packed_vertex) in raw_mesh.vertices.iter().zip(&packed_mesh.vertices) {
            assert_eq!(raw_vertex.position, packed_vertex.position);
            assert_eq!(raw_vertex.uv, packed_vertex.uv);
        }
    }
    assert_eq!(packed.source_frames[0].normal, [0.2, 0.3, 0.5]);
    assert_eq!(packed.source_frames[0].tangent, Some([0.3, 0.2, 0.5, -1.0]));
    assert_eq!(raw.meshes[0].vertices[0].normal, [0.2, 0.3, 0.5]);
    assert_eq!(
        packed.meshes[0].vertices[0].normal,
        source_ubyte4_vector([0.2, 0.3, 0.5]).unwrap()
    );
    assert_eq!(packed.meshes[0].vertices[0].tangent_sign, -1.0);
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

fn lighting_file(model: &ModelData) -> Vec<u8> {
    let directory_end = 40 + model.lighting_meshes.len() * 28;
    let stream_start = directory_end.div_ceil(512) * 512;
    let total: usize = model
        .lighting_meshes
        .iter()
        .map(|mesh| mesh.vertex_count)
        .sum();
    let mut bytes = vec![0; (stream_start + total * 4).div_ceil(512) * 512];
    put_i32(&mut bytes, 0, 2);
    bytes[4..8].copy_from_slice(&model.checksum.to_le_bytes());
    put_i32(&mut bytes, 8, 4);
    put_i32(&mut bytes, 12, 4);
    put_i32(&mut bytes, 16, total as i32);
    put_i32(&mut bytes, 20, model.lighting_meshes.len() as i32);
    let mut stream = stream_start;
    for (index, mesh) in model.lighting_meshes.iter().enumerate() {
        put_i32(&mut bytes, 40 + index * 28, mesh.lod as i32);
        put_i32(&mut bytes, 44 + index * 28, mesh.vertex_count as i32);
        put_i32(&mut bytes, 48 + index * 28, stream as i32);
        for vertex in 0..mesh.vertex_count {
            bytes[stream..stream + 4].copy_from_slice(&[vertex as u8, index as u8, 100, 255]);
            stream += 4;
        }
    }
    bytes
}

#[test]
fn compiled_lighting_matches_hardware_identity_across_lods_models_and_duplicate_vertices() {
    let limits = Limits::default();
    let mut mdl = parse_mdl(&build_mdl(), &limits).unwrap();
    let vvd = parse_vvd(
        &build_vvd(&[[0.0; 3], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]]),
        &limits,
    )
    .unwrap();
    let mut vtx = parse_vtx(&build_vtx(3), &limits).unwrap();
    let second_model = mdl.body_parts[0].models[0].clone();
    mdl.body_parts[0].models.push(second_model);
    let second_hardware_model = vtx.body_parts[0].models[0].clone();
    let lower_lod = vtx.body_parts[0].models[0].lods[0].clone();
    vtx.body_parts[0].models[0].lods.push(lower_lod);
    vtx.body_parts[0].models.push(second_hardware_model);
    let mesh = &mut vtx.body_parts[0].models[0].lods[0].meshes[0];
    let mut second_group = mesh.strip_groups[0].clone();
    second_group.vertices.swap(1, 2);
    let group = &mut mesh.strip_groups[0];
    group.vertices.push(group.vertices[0]);
    group.indices = vec![3, 1, 2, 0, 2, 1];
    group.strips[0].index_count = 6;
    mesh.strip_groups.push(second_group);

    let model = assemble(&mdl, &vvd, &vtx).unwrap().model;
    assert!(model.lighting_compatible);
    assert_eq!(
        model
            .lighting_meshes
            .iter()
            .map(|mesh| (mesh.lod, mesh.vertex_count))
            .collect::<Vec<_>>(),
        [(0, 4), (0, 3), (1, 3), (0, 3)]
    );
    assert_eq!(model.meshes.len(), 2);
    assert_eq!(model.meshes[0].vertices.len(), 7);
    let expected = [(0, 1), (0, 2), (0, 0), (0, 3), (1, 2), (1, 1), (1, 0)];
    assert_eq!(
        model.meshes[0]
            .vertex_lighting
            .iter()
            .map(|vertex| (vertex.mesh, vertex.vertex))
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(model.meshes[0].vertices[2], model.meshes[0].vertices[3]);
    assert!(
        model.meshes[1]
            .vertex_lighting
            .iter()
            .all(|vertex| vertex.mesh == 3)
    );

    let bytes = lighting_file(&model);
    let vhv = parse_vhv(&bytes, &limits).unwrap();
    vhv.validate_model(&model).unwrap();
    let colors: Vec<_> = model.meshes[0]
        .vertex_lighting
        .iter()
        .map(|vertex| vhv.color_rgba(*vertex).unwrap())
        .collect();
    assert_eq!(
        colors,
        expected.map(|(mesh, vertex)| [100, mesh as u8, vertex as u8, 255])
    );
    assert_ne!(
        colors[2], colors[3],
        "equal VVD vertices retain distinct hardware lighting"
    );
}

#[test]
fn compiled_lighting_rejects_stale_checksums_layouts_and_lossy_companions() {
    let limits = Limits::default();
    let mdl = parse_mdl(&build_mdl(), &limits).unwrap();
    let mut vvd = parse_vvd(
        &build_vvd(&[[0.0; 3], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]]),
        &limits,
    )
    .unwrap();
    let mut vtx = parse_vtx(&build_vtx(3), &limits).unwrap();
    let model = assemble(&mdl, &vvd, &vtx).unwrap().model;
    let bytes = lighting_file(&model);
    let vhv = parse_vhv(&bytes, &limits).unwrap();
    vhv.validate_model(&model).unwrap();
    let mut altered = model.clone();
    altered.checksum = 123;
    assert_eq!(
        vhv.validate_model(&altered),
        Err(VhvError::ChecksumMismatch {
            expected: 123,
            actual: 0
        })
    );
    altered = model.clone();
    altered.lighting_meshes[0].lod = 1;
    assert_eq!(
        vhv.validate_model(&altered),
        Err(VhvError::MeshLayoutMismatch { mesh: 0 })
    );
    altered = model.clone();
    altered.lighting_meshes[0].vertex_count += 1;
    assert_eq!(
        vhv.validate_model(&altered),
        Err(VhvError::MeshLayoutMismatch { mesh: 0 })
    );
    altered = model.clone();
    altered.meshes[0].vertex_lighting[0] = ModelLightingVertex { mesh: 0, vertex: 3 };
    assert!(matches!(
        vhv.validate_model(&altered),
        Err(VhvError::Corrupt { .. })
    ));
    altered = model;
    altered.lighting_meshes.clear();
    assert_eq!(
        vhv.validate_model(&altered),
        Err(VhvError::MeshCountMismatch {
            expected: 0,
            actual: 1
        })
    );

    vvd.checksum = 123;
    let lossy = assemble_lossy(&mdl, &vvd, &vtx).unwrap().model;
    assert_eq!(vhv.validate_model(&lossy), Err(VhvError::ModelIncompatible));
    vvd.checksum = 0;
    vtx.body_parts[0].models[0].lods[0].meshes.clear();
    let lossy = assemble_lossy(&mdl, &vvd, &vtx).unwrap().model;
    assert_eq!(vhv.validate_model(&lossy), Err(VhvError::ModelIncompatible));
}

#[test]
fn raw_studio_flags_survive_assembly_without_changing_vertex_identity() {
    let limits = Limits::default();
    let positions = [[0.0f32, 0.0, 0.0], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
    let mut mdl = parse_mdl(&build_mdl(), &limits).unwrap();
    let vvd = parse_vvd(&build_vvd(&positions), &limits).unwrap();
    let vtx = parse_vtx(&build_vtx(3), &limits).unwrap();
    let baseline = assemble(&mdl, &vvd, &vtx).unwrap().model;
    mdl.flags |= 0x8001_0000;
    let flagged = assemble(&mdl, &vvd, &vtx).unwrap().model;
    assert_eq!(flagged.mdl_flags, mdl.flags);
    assert_eq!(flagged.meshes, baseline.meshes);
    assert_eq!(flagged.lighting_meshes, baseline.lighting_meshes);
    assert_eq!(flagged.source_frames, baseline.source_frames);
}
