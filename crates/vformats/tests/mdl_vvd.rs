//! VVD differential tests: synthetic files parsed by both this crate
//! and the `vmdl` crate (the implementation being replaced) must agree
//! on geometry — and disagree on exactly one thing, deliberately: raw
//! bone weights, where vmdl's accessor divides by bone count.

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

fn assert_matches_vmdl(bytes: &[u8]) {
    let ours = parse_vvd(bytes, &Limits::default()).expect("our parse");
    let theirs = vmdl::vvd::Vvd::read(bytes).expect("vmdl parse");

    assert_eq!(ours.vertices.len(), theirs.vertices.len(), "vertex count");
    for (index, (a, b)) in ours.vertices.iter().zip(&theirs.vertices).enumerate() {
        assert_eq!(
            a.position,
            [b.position.x, b.position.y, b.position.z],
            "vertex {index} position"
        );
        assert_eq!(
            a.normal,
            [b.normal.x, b.normal.y, b.normal.z],
            "vertex {index} normal"
        );
        assert_eq!(a.uv, b.texture_coordinates, "vertex {index} uv");
        // vmdl keeps the raw slots private; its weights() accessor is the
        // documented divide-by-count bug. Verify our raw weights times the
        // inverse of vmdl's division reproduce its output, proving both
        // read the same stored bytes.
        let vmdl_weights: Vec<(u8, f32)> = b
            .bone_weights
            .weights()
            .map(|w| (w.bone_id, w.weight))
            .collect();
        assert_eq!(
            vmdl_weights.len(),
            usize::from(a.bone_count.min(3)),
            "vertex {index} slots"
        );
        for (slot, (bone_id, divided)) in vmdl_weights.iter().enumerate() {
            assert_eq!(*bone_id, a.bones[slot], "vertex {index} bone {slot}");
            let expected = a.weights[slot] / f32::from(a.bone_count);
            assert_eq!(*divided, expected, "vertex {index} weight {slot}");
        }
    }
    assert_eq!(ours.tangents.len(), theirs.tangents.len(), "tangent count");
    assert_eq!(ours.tangents, theirs.tangents, "tangents");
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
    assert_matches_vmdl(&build_vvd(&spec));
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
    assert_matches_vmdl(&bytes);

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
