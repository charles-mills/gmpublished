//! VTX tests: a synthetic file with relative offsets at every tree
//! level. Golden expectations were captured once from the `vmdl` crate
//! v0.2.0 as a one-time differential oracle (trees and strip emission
//! agreed, except vmdl's tri-strip tail overrun).

use vformats::Limits;
use vformats::mdl::{MdlError, VtxStrip, parse_vtx};

const TRI_LIST: u8 = 0x01;
const TRI_STRIP: u8 = 0x02;

/// One strip group: (vertex ids, index pool, strips as (start, count, flags)).
struct GroupSpec {
    vertex_ids: Vec<u16>,
    indices: Vec<u16>,
    strips: Vec<(usize, usize, u8)>,
}

/// Builds a VTX with one body part, one model, one LOD, N meshes of one
/// strip group each — every offset relative to its containing header,
/// exercising the tree's addressing convention.
fn build_vtx(groups: &[GroupSpec]) -> Vec<u8> {
    let mut b = vec![0u8; 36]; // header, filled at the end
    let body_part_offset = b.len();
    b.extend_from_slice(&[0; 8]); // one body part header
    let model_offset = b.len();
    b.extend_from_slice(&[0; 8]); // one model header
    let lod_offset = b.len();
    b.extend_from_slice(&[0; 12]); // one lod header
    let mesh_offset = b.len();
    b.resize(b.len() + groups.len() * 9, 0);

    let mut group_offsets = Vec::new();
    for group in groups {
        let group_base = b.len();
        group_offsets.push(group_base);
        b.resize(b.len() + 25, 0); // strip group header
        let vertex_offset = b.len() - group_base;
        for id in &group.vertex_ids {
            b.extend_from_slice(&[9, 8, 7, 1]); // weight indexes + bone count
            b.extend_from_slice(&id.to_le_bytes());
            b.extend_from_slice(&[4, 5, 6]); // bone ids
        }
        let index_offset = b.len() - group_base;
        for index in &group.indices {
            b.extend_from_slice(&index.to_le_bytes());
        }
        let strip_offset = b.len() - group_base;
        for (start, count, flags) in &group.strips {
            let strip_base = b.len();
            b.resize(b.len() + 27, 0);
            b[strip_base..strip_base + 4].copy_from_slice(&(*count as i32).to_le_bytes());
            b[strip_base + 4..strip_base + 8].copy_from_slice(&(*start as i32).to_le_bytes());
            b[strip_base + 18] = *flags;
        }
        let header = &mut b[group_base..group_base + 25];
        header[0..4].copy_from_slice(&(group.vertex_ids.len() as i32).to_le_bytes());
        header[4..8].copy_from_slice(&(vertex_offset as i32).to_le_bytes());
        header[8..12].copy_from_slice(&(group.indices.len() as i32).to_le_bytes());
        header[12..16].copy_from_slice(&(index_offset as i32).to_le_bytes());
        header[16..20].copy_from_slice(&(group.strips.len() as i32).to_le_bytes());
        header[20..24].copy_from_slice(&(strip_offset as i32).to_le_bytes());
    }

    // Backpatch the tree, each offset relative to its own header.
    for (mesh, group_base) in group_offsets.iter().enumerate() {
        let base = mesh_offset + mesh * 9;
        let relative = (group_base - base) as i32;
        b[base..base + 4].copy_from_slice(&1i32.to_le_bytes());
        b[base + 4..base + 8].copy_from_slice(&relative.to_le_bytes());
    }
    b[lod_offset..lod_offset + 4].copy_from_slice(&(groups.len() as i32).to_le_bytes());
    b[lod_offset + 4..lod_offset + 8]
        .copy_from_slice(&((mesh_offset - lod_offset) as i32).to_le_bytes());
    b[model_offset..model_offset + 4].copy_from_slice(&1i32.to_le_bytes());
    b[model_offset + 4..model_offset + 8]
        .copy_from_slice(&((lod_offset - model_offset) as i32).to_le_bytes());
    b[body_part_offset..body_part_offset + 4].copy_from_slice(&1i32.to_le_bytes());
    b[body_part_offset + 4..body_part_offset + 8]
        .copy_from_slice(&((model_offset - body_part_offset) as i32).to_le_bytes());

    b[0..4].copy_from_slice(&7i32.to_le_bytes()); // version
    b[16..20].copy_from_slice(&0x11223344u32.to_le_bytes()); // checksum
    b[20..24].copy_from_slice(&1i32.to_le_bytes()); // lod count
    b[28..32].copy_from_slice(&1i32.to_le_bytes()); // body part count
    b[32..36].copy_from_slice(&(body_part_offset as i32).to_le_bytes());
    b
}

fn two_group_fixture() -> Vec<u8> {
    build_vtx(&[
        GroupSpec {
            vertex_ids: vec![10, 11, 12, 13],
            indices: vec![0, 1, 2, 2, 1, 3],
            strips: vec![(0, 6, TRI_LIST)],
        },
        GroupSpec {
            vertex_ids: vec![20, 21, 22, 23, 24],
            indices: vec![0, 1, 2, 3, 4, 0, 2, 4],
            strips: vec![(0, 5, TRI_STRIP), (5, 3, TRI_LIST)],
        },
    ])
}

#[test]
fn tree_and_fields_match_vmdl() {
    // Expected values captured from the `vmdl` crate v0.2.0 as a
    // one-time differential oracle; they mirror the GroupSpec inputs.
    let bytes = two_group_fixture();
    let ours = parse_vtx(&bytes, &Limits::default()).expect("our parse");

    assert_eq!(ours.checksum, 0x11223344);
    assert_eq!(ours.body_parts.len(), 1);
    let our_lod = &ours.body_parts[0].models[0].lods[0];
    assert_eq!(our_lod.meshes.len(), 2);

    let expected: [(&[u16], &[u16]); 2] = [
        (&[0, 1, 2, 2, 1, 3], &[10, 11, 12, 13]),
        (&[0, 1, 2, 3, 4, 0, 2, 4], &[20, 21, 22, 23, 24]),
    ];
    for (mesh, (a, (indices, vertex_ids))) in our_lod.meshes.iter().zip(expected).enumerate() {
        assert_eq!(a.strip_groups.len(), 1, "mesh {mesh}");
        let group = &a.strip_groups[0];
        assert_eq!(group.indices, indices, "mesh {mesh} indices");
        assert_eq!(group.vertices.len(), vertex_ids.len());
        for (va, id) in group.vertices.iter().zip(vertex_ids) {
            assert_eq!(va.original_mesh_vertex_id, *id);
            assert_eq!(va.bone_weight_indexes, [9, 8, 7]);
            assert_eq!(va.bone_count, 1);
            assert_eq!(va.bone_ids, [4, 5, 6]);
        }
    }
}

#[test]
fn strip_emission_matches_vmdl_modulo_its_overrun() {
    // Expected emissions captured from the `vmdl` crate v0.2.0 as a
    // one-time differential oracle. Both tri-lists agreed exactly. For
    // the tri-strip vmdl emitted [2,1,0, 2,1,1, 4,3,2, 4,3,3, 6,5,4] —
    // len triangles (two overrun) with degenerate odd triangles; we
    // emit the correct len-2 with proper alternation, agreeing on the
    // even triangles ([2,1,0] and [4,3,2]), proving both read the same
    // ranges.
    let bytes = two_group_fixture();
    let ours = parse_vtx(&bytes, &Limits::default()).expect("our parse");

    let strips: Vec<_> = ours.body_parts[0].models[0].lods[0]
        .meshes
        .iter()
        .flat_map(|m| m.strip_groups.iter())
        .flat_map(|g| g.strips.iter())
        .collect();
    let expected: [&[usize]; 3] = [
        &[5, 4, 3, 2, 1, 0],
        &[2, 1, 0, 2, 3, 1, 4, 3, 2],
        &[7, 6, 5],
    ];
    assert_eq!(strips.len(), expected.len());
    for (strip, (s, want)) in strips.iter().zip(expected).enumerate() {
        let positions: Vec<usize> = s.triangle_index_positions().collect();
        assert_eq!(positions, want, "strip {strip}");
    }
}

#[test]
fn rejects_malformed_vtx() {
    let bytes = two_group_fixture();

    let mut wrong_version = bytes.clone();
    wrong_version[0..4].copy_from_slice(&6i32.to_le_bytes());
    assert!(matches!(
        parse_vtx(&wrong_version, &Limits::default()),
        Err(MdlError::UnsupportedVersion {
            part: "vtx",
            version: 6
        })
    ));

    assert!(matches!(
        parse_vtx(&bytes[..60], &Limits::default()),
        Err(MdlError::Truncated { .. })
    ));

    let cap = Limits {
        max_entries: 3,
        ..Limits::default()
    };
    assert!(matches!(
        parse_vtx(&bytes, &cap),
        Err(MdlError::TooMany { .. })
    ));

    // A strip range past its group's index pool is structural corruption.
    let bad = build_vtx(&[GroupSpec {
        vertex_ids: vec![1, 2, 3],
        indices: vec![0, 1, 2],
        strips: vec![(0, 9, TRI_LIST)],
    }]);
    assert!(matches!(
        parse_vtx(&bad, &Limits::default()),
        Err(MdlError::Corrupt {
            part: "vtx strip range"
        })
    ));
}

#[test]
fn strip_topologies_emit_expected_triangles() {
    // Tri-list of 6: whole sequence reversed.
    let list = VtxStrip {
        index_start: 0,
        index_count: 6,
        flags: TRI_LIST,
    };
    assert_eq!(
        list.triangle_index_positions().collect::<Vec<_>>(),
        [5, 4, 3, 2, 1, 0]
    );
    // Tri-strip of 5: 3 triangles, alternating winding, each reversed.
    let strip = VtxStrip {
        index_start: 0,
        index_count: 5,
        flags: TRI_STRIP,
    };
    assert_eq!(
        strip.triangle_index_positions().collect::<Vec<_>>(),
        [2, 1, 0, 2, 3, 1, 4, 3, 2]
    );
}
