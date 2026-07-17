//! VVD tests: synthetic files whose parsed geometry must reproduce the
//! spec they were built from. Golden expectations were captured once
//! from the `vmdl` crate v0.2.0 (the implementation being replaced) as
//! a one-time differential oracle; it agreed on all geometry and
//! disagreed on exactly one thing, deliberately: raw bone weights,
//! where vmdl's accessor divides by bone count.

use vformats::Limits;
use vformats::mdl::{MdlError, parse_vvd};

/// (position, normal, uv, weights, bones, bone_count)
type VertexSpec = ([f32; 3], [f32; 3], [f32; 2], [f32; 3], [u8; 3], u8);

struct VvdSpec {
    vertices: Vec<VertexSpec>,
    fixups: Vec<(i32, i32, i32)>, // (lod, source_vertex_id, count)
}

fn build_vvd(spec: &VvdSpec) -> Vec<u8> {
    let header_len = 64;
    let fixup_len = spec.fixups.len() * 12;
    let vertex_len = spec.vertices.len() * 48;
    let fixup_index = header_len;
    let vertex_index = fixup_index + fixup_len;
    let tangent_index = vertex_index + vertex_len;

    let mut b = Vec::new();
    b.extend_from_slice(b"IDSV");
    b.extend_from_slice(&4i32.to_le_bytes()); // version
    b.extend_from_slice(&0xAABBCCDDu32.to_le_bytes()); // checksum
    b.extend_from_slice(&1i32.to_le_bytes()); // lod count
    b.extend_from_slice(&(spec.vertices.len() as i32).to_le_bytes()); // lod 0
    b.extend_from_slice(&[0; 28]); // lods 1..8
    b.extend_from_slice(&(spec.fixups.len() as i32).to_le_bytes());
    b.extend_from_slice(&(fixup_index as i32).to_le_bytes());
    b.extend_from_slice(&(vertex_index as i32).to_le_bytes());
    b.extend_from_slice(&(tangent_index as i32).to_le_bytes());
    assert_eq!(b.len(), header_len);

    for (lod, from, count) in &spec.fixups {
        b.extend_from_slice(&lod.to_le_bytes());
        b.extend_from_slice(&from.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
    }
    for (position, normal, uv, weights, bones, bone_count) in &spec.vertices {
        for w in weights {
            b.extend_from_slice(&w.to_le_bytes());
        }
        b.extend_from_slice(bones);
        b.push(*bone_count);
        for v in position.iter().chain(normal).chain(uv) {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    for (index, _) in spec.vertices.iter().enumerate() {
        // Distinct per-source-vertex tangents so fixup reordering shows.
        for c in 0..4 {
            b.extend_from_slice(&((index * 4 + c) as f32).to_le_bytes());
        }
    }
    b
}

fn vertex(tag: f32, weights: [f32; 3], bones: [u8; 3], bone_count: u8) -> VertexSpec {
    (
        [tag, tag + 0.25, tag + 0.5],
        [0.0, 1.0, 0.0],
        [tag * 0.1, 1.0 - tag * 0.1],
        weights,
        bones,
        bone_count,
    )
}

/// Asserts the parse reproduces `spec`'s vertices in `order` (output
/// index -> source vertex index), tangents following their vertices.
/// The vmdl v0.2.0 oracle (since removed) confirmed exactly this
/// output — same geometry and tangent reordering, with its weights()
/// equal to our raw weights divided by bone count (its documented
/// divide-by-count bug), proving both read the same stored bytes.
fn assert_matches_vmdl(bytes: &[u8], spec: &VvdSpec, order: &[usize]) {
    let ours = parse_vvd(bytes, &Limits::default()).expect("our parse");

    assert_eq!(ours.vertices.len(), order.len(), "vertex count");
    for (index, (a, &source)) in ours.vertices.iter().zip(order).enumerate() {
        let (position, normal, uv, weights, bones, bone_count) = &spec.vertices[source];
        assert_eq!(a.position, *position, "vertex {index} position");
        assert_eq!(a.normal, *normal, "vertex {index} normal");
        assert_eq!(a.uv, *uv, "vertex {index} uv");
        assert_eq!(a.weights, *weights, "vertex {index} weights");
        assert_eq!(a.bones, *bones, "vertex {index} bones");
        assert_eq!(a.bone_count, *bone_count, "vertex {index} bone count");
    }
    assert_eq!(ours.tangents.len(), order.len(), "tangent count");
    for (index, (tangent, &source)) in ours.tangents.iter().zip(order).enumerate() {
        let base = (source * 4) as f32;
        assert_eq!(
            *tangent,
            [base, base + 1.0, base + 2.0, base + 3.0],
            "tangent {index}"
        );
    }
}

#[test]
fn no_fixup_files_match_vmdl() {
    let spec = VvdSpec {
        vertices: vec![
            vertex(1.0, [1.0, 0.0, 0.0], [0, 0, 0], 1),
            vertex(2.0, [0.7, 0.3, 0.0], [1, 2, 0], 2),
            vertex(3.0, [0.5, 0.25, 0.25], [3, 4, 5], 3),
        ],
        fixups: vec![],
    };
    assert_matches_vmdl(&build_vvd(&spec), &spec, &[0, 1, 2]);
}

#[test]
fn fixup_files_reorder_identically_to_vmdl() {
    // Source order 0..4; fixups emit [2..4) then [0..1): LOD-0 order 2,3,0.
    let spec = VvdSpec {
        vertices: vec![
            vertex(1.0, [1.0, 0.0, 0.0], [0, 0, 0], 1),
            vertex(2.0, [1.0, 0.0, 0.0], [1, 0, 0], 1),
            vertex(3.0, [1.0, 0.0, 0.0], [2, 0, 0], 1),
            vertex(4.0, [1.0, 0.0, 0.0], [3, 0, 0], 1),
        ],
        fixups: vec![(0, 2, 2), (1, 0, 1)],
    };
    let bytes = build_vvd(&spec);
    assert_matches_vmdl(&bytes, &spec, &[2, 3, 0]);

    let ours = parse_vvd(&bytes, &Limits::default()).expect("parse");
    assert_eq!(ours.vertices.len(), 3);
    assert_eq!(ours.vertices[0].bones[0], 2);
    assert_eq!(ours.vertices[1].bones[0], 3);
    assert_eq!(ours.vertices[2].bones[0], 0);
    // Tangents reorder with their vertices.
    assert_eq!(ours.tangents[0][0], 8.0);
    assert_eq!(ours.tangents[2][0], 0.0);
    assert_eq!(ours.checksum, 0xAABBCCDD);
}

#[test]
fn raw_weights_are_exposed_undivided() {
    let spec = VvdSpec {
        vertices: vec![vertex(1.0, [0.6, 0.4, 0.0], [1, 2, 0], 2)],
        fixups: vec![],
    };
    let ours = parse_vvd(&build_vvd(&spec), &Limits::default()).expect("parse");
    // The stored weights, not vmdl's weights()/bone_count division.
    assert_eq!(ours.vertices[0].weights, [0.6, 0.4, 0.0]);
    assert_eq!(ours.vertices[0].bone_count, 2);
}

#[test]
fn rejects_malformed_vvd() {
    assert!(matches!(
        parse_vvd(b"VSDI wrong magic", &Limits::default()),
        Err(MdlError::BadMagic { part: "vvd" })
    ));

    let spec = VvdSpec {
        vertices: vec![vertex(1.0, [1.0, 0.0, 0.0], [0, 0, 0], 1)],
        fixups: vec![],
    };
    let mut wrong_version = build_vvd(&spec);
    wrong_version[4..8].copy_from_slice(&5i32.to_le_bytes());
    assert!(matches!(
        parse_vvd(&wrong_version, &Limits::default()),
        Err(MdlError::UnsupportedVersion {
            part: "vvd",
            version: 5
        })
    ));

    // Fixup range past the source array.
    let bad_fixup = VvdSpec {
        vertices: vec![vertex(1.0, [1.0, 0.0, 0.0], [0, 0, 0], 1)],
        fixups: vec![(0, 0, 5)],
    };
    assert!(matches!(
        parse_vvd(&build_vvd(&bad_fixup), &Limits::default()),
        Err(MdlError::Corrupt { .. })
    ));

    // Truncated vertex data.
    let bytes = build_vvd(&spec);
    assert!(matches!(
        parse_vvd(&bytes[..bytes.len() - 40], &Limits::default()),
        Err(MdlError::Truncated { .. })
    ));

    let cap = Limits {
        max_entries: 0,
        ..Limits::default()
    };
    assert!(matches!(
        parse_vvd(&bytes, &cap),
        Err(MdlError::TooMany {
            part: "vvd vertices",
            ..
        })
    ));
}
