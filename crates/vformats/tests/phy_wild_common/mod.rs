#![allow(dead_code)]

use std::collections::BTreeMap;

use vformats::phy::{ConvexLedge, IVP_METERS_TO_SOURCE_INCHES, PhyFile, SkipReason};

const PHY_HEADER_BYTES: usize = 16;
const COMPACT_SURFACE_HEADER_BYTES: usize = 32;
const LEGACY_SURFACE_HEADER_BYTES: usize = 48;
const LEGACY_LEDGETREE_ROOT_OFFSET: usize = 32;
const COMPACT_LEDGE_HEADER_BYTES: usize = 16;
const COMPACT_TRIANGLE_BYTES: usize = 16;
const COMPACT_POINT_BYTES: usize = 16;
const COMPACT_LEDGETREE_NODE_BYTES: usize = 28;
const COMPACT_LEDGE_IS_COMPACT_FLAG: u32 = 1;

const TETRA_TRIANGLES: [[i16; 3]; 4] = [[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]];
const TETRA_POINTS: [[f32; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.0, 1.0, 0.0],
];
const FLAT_TRIANGLE_PAIR_TRIANGLES: [[i16; 3]; 2] = [[0, 1, 2], [1, 0, 2]];
const FLAT_TRIANGLE_PAIR_POINTS: [[f32; 3]; 3] =
    [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];

pub fn compact_phy() -> Vec<u8> {
    phy_with_sections([compact_section(1)].into_iter(), &[])
}

/// Two solids with different ledge counts (1 then 2), so a bug that swaps or
/// duplicates solid contents — not just one that drops the second solid
/// outright — would also fail this fixture's assertions.
pub fn two_compact_solids_phy() -> Vec<u8> {
    phy_with_sections([compact_section(1), compact_section(2)].into_iter(), &[])
}

pub fn compact_phy_with_text(text: &[u8]) -> Vec<u8> {
    phy_with_sections([compact_section(1)].into_iter(), text)
}

pub fn flat_triangle_pair_phy() -> Vec<u8> {
    phy_with_sections([flat_triangle_pair_section()].into_iter(), &[])
}

pub fn ledgetree_phy() -> Vec<u8> {
    phy_with_sections([ledgetree_section()].into_iter(), &[])
}

pub fn convex_header_phy() -> Vec<u8> {
    phy_with_sections([convex_header_section()].into_iter(), &[])
}

pub fn unsupported_then_compact_phy() -> Vec<u8> {
    phy_with_sections(
        [unsupported_section(), compact_section(1)].into_iter(),
        b"surfaceprop\0",
    )
}

pub fn phy_with_truncated_second_section() -> Vec<u8> {
    let mut bytes = compact_phy();
    bytes[8..12].copy_from_slice(&2_i32.to_le_bytes());
    bytes.extend_from_slice(&4_i32.to_le_bytes());
    bytes
}

pub fn skip_count(model: &PhyFile, reason: SkipReason) -> usize {
    model
        .stats
        .skip_reasons
        .get(&reason)
        .copied()
        .unwrap_or_default()
}

pub fn phy_bounds(ledges: &[ConvexLedge]) -> Option<([f32; 3], [f32; 3])> {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut found = false;
    for ledge in ledges {
        for vertex in &ledge.vertices {
            found = true;
            for axis in 0..3 {
                min[axis] = min[axis].min(vertex[axis]);
                max[axis] = max[axis].max(vertex[axis]);
            }
        }
    }
    found.then_some((min, max))
}

pub fn extents(bounds: ([f32; 3], [f32; 3])) -> [f32; 3] {
    [
        bounds.1[0] - bounds.0[0],
        bounds.1[1] - bounds.0[1],
        bounds.1[2] - bounds.0[2],
    ]
}

pub fn all_synthetic_seeds() -> BTreeMap<&'static str, Vec<u8>> {
    BTreeMap::from([
        ("empty", Vec::new()),
        ("compact", compact_phy()),
        ("flat-triangle-pair", flat_triangle_pair_phy()),
        ("two-compact-solids", two_compact_solids_phy()),
        ("ledgetree", ledgetree_phy()),
        ("convex-header", convex_header_phy()),
        (
            "truncated-second-section",
            phy_with_truncated_second_section(),
        ),
        ("unsupported-then-compact", unsupported_then_compact_phy()),
    ])
}

fn phy_with_sections(sections: impl Iterator<Item = Vec<u8>>, text: &[u8]) -> Vec<u8> {
    let sections = sections.collect::<Vec<_>>();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(PHY_HEADER_BYTES as i32).to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&(sections.len() as i32).to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    for section in sections {
        bytes.extend_from_slice(&section);
    }
    bytes.extend_from_slice(text);
    bytes
}

fn unsupported_section() -> Vec<u8> {
    let mut section = compact_section(1);
    section[4..8].copy_from_slice(b"NOPE");
    section
}

fn compact_section(ledge_count: usize) -> Vec<u8> {
    let mut section = vphy_prefix(None);
    append_packed_compact_ledges(&mut section, ledge_count);
    finish_section(section)
}

fn flat_triangle_pair_section() -> Vec<u8> {
    let mut section = vphy_prefix(None);
    append_flat_triangle_pair_ledge(&mut section);
    finish_section(section)
}

fn ledgetree_section() -> Vec<u8> {
    let mut section = vphy_prefix(None);
    let ledge_start = section.len();
    append_packed_compact_ledges(&mut section, 3);

    let tree_root = section.len();
    let legacy_start = COMPACT_SURFACE_HEADER_BYTES;
    let root_offset = i32::try_from(tree_root - legacy_start).expect("tree root offset");
    let root_field = COMPACT_SURFACE_HEADER_BYTES + LEGACY_LEDGETREE_ROOT_OFFSET;
    section[root_field..root_field + 4].copy_from_slice(&root_offset.to_le_bytes());

    append_tree_node(&mut section, (COMPACT_LEDGETREE_NODE_BYTES * 2) as i32, 0);
    let left_node = tree_root + COMPACT_LEDGETREE_NODE_BYTES;
    let right_node = tree_root + COMPACT_LEDGETREE_NODE_BYTES * 2;
    append_tree_node(
        &mut section,
        0,
        i32::try_from(ledge_start).expect("ledge offset")
            - i32::try_from(left_node).expect("left node offset"),
    );
    append_tree_node(
        &mut section,
        0,
        i32::try_from(ledge_start + 2 * compact_ledge_record_len()).expect("ledge offset")
            - i32::try_from(right_node).expect("right node offset"),
    );

    finish_section(section)
}

fn convex_header_section() -> Vec<u8> {
    let mut section = vphy_prefix(None);
    let convex_start = section.len();
    let vertices_offset =
        COMPACT_LEDGE_HEADER_BYTES + TETRA_TRIANGLES.len() * COMPACT_TRIANGLE_BYTES;
    section.extend_from_slice(&(vertices_offset as i32).to_le_bytes());
    section.extend_from_slice(&0_i32.to_le_bytes());
    section.extend_from_slice(&0_i32.to_le_bytes());
    section.extend_from_slice(&(TETRA_TRIANGLES.len() as i32).to_le_bytes());
    assert_eq!(section.len() - convex_start, COMPACT_LEDGE_HEADER_BYTES);
    append_triangles(&mut section);
    assert_eq!(section.len() - convex_start, vertices_offset);
    append_points(&mut section);
    finish_section(section)
}

fn vphy_prefix(root_offset: Option<i32>) -> Vec<u8> {
    let mut section = Vec::new();
    section.extend_from_slice(&0_i32.to_le_bytes());
    section.extend_from_slice(b"VPHY");
    section.extend_from_slice(&0_i16.to_le_bytes());
    section.extend_from_slice(&0_i16.to_le_bytes());
    section.extend_from_slice(&0_i32.to_le_bytes());
    section.extend_from_slice(&[0; 12]);
    section.extend_from_slice(&0_i32.to_le_bytes());
    assert_eq!(section.len(), COMPACT_SURFACE_HEADER_BYTES);

    let legacy_start = section.len();
    section.extend_from_slice(&[0; LEGACY_SURFACE_HEADER_BYTES]);
    if let Some(root_offset) = root_offset {
        let root_field = legacy_start + LEGACY_LEDGETREE_ROOT_OFFSET;
        section[root_field..root_field + 4].copy_from_slice(&root_offset.to_le_bytes());
    }
    section
}

fn append_packed_compact_ledges(section: &mut Vec<u8>, ledge_count: usize) {
    let ledge_start = section.len();
    let point_start = ledge_start + ledge_count * compact_ledge_record_len();
    for ledge_index in 0..ledge_count {
        let ledge_offset = section.len();
        let point_offset = point_start - ledge_offset;
        section.extend_from_slice(&(point_offset as i32).to_le_bytes());
        section.extend_from_slice(&0_i32.to_le_bytes());
        section.extend_from_slice(&0x0000_0904_u32.to_le_bytes());
        section.extend_from_slice(&(TETRA_TRIANGLES.len() as i16).to_le_bytes());
        section.extend_from_slice(&0_i16.to_le_bytes());
        append_triangles(section);
        assert_eq!(
            section.len() - ledge_start,
            (ledge_index + 1) * compact_ledge_record_len()
        );
    }
    append_points(section);
}

fn append_flat_triangle_pair_ledge(section: &mut Vec<u8>) {
    let ledge_offset = section.len();
    let point_offset =
        COMPACT_LEDGE_HEADER_BYTES + FLAT_TRIANGLE_PAIR_TRIANGLES.len() * COMPACT_TRIANGLE_BYTES;
    let size_div_16 = point_offset / 16;
    let flags = ((size_div_16 as u32) << 8) | COMPACT_LEDGE_IS_COMPACT_FLAG << 2;

    section.extend_from_slice(&(point_offset as i32).to_le_bytes());
    section.extend_from_slice(&0_i32.to_le_bytes());
    section.extend_from_slice(&flags.to_le_bytes());
    section.extend_from_slice(&(FLAT_TRIANGLE_PAIR_TRIANGLES.len() as i16).to_le_bytes());
    section.extend_from_slice(&0_i16.to_le_bytes());
    for triangle in FLAT_TRIANGLE_PAIR_TRIANGLES {
        section.extend_from_slice(&0_i32.to_le_bytes());
        for index in triangle {
            section.extend_from_slice(&index.to_le_bytes());
            section.extend_from_slice(&0_i16.to_le_bytes());
        }
    }
    assert_eq!(section.len() - ledge_offset, point_offset);
    for point in FLAT_TRIANGLE_PAIR_POINTS {
        for component in point {
            section.extend_from_slice(&component.to_le_bytes());
        }
        section.extend_from_slice(&0.0_f32.to_le_bytes());
    }
}

fn append_triangles(bytes: &mut Vec<u8>) {
    for triangle in TETRA_TRIANGLES {
        bytes.extend_from_slice(&0_i32.to_le_bytes());
        for index in triangle {
            bytes.extend_from_slice(&index.to_le_bytes());
            bytes.extend_from_slice(&0_i16.to_le_bytes());
        }
    }
}

fn append_points(bytes: &mut Vec<u8>) {
    for point in TETRA_POINTS {
        for component in point {
            bytes.extend_from_slice(&component.to_le_bytes());
        }
        bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    }
    assert_eq!(TETRA_POINTS.len() * COMPACT_POINT_BYTES, 64);
}

fn append_tree_node(bytes: &mut Vec<u8>, right_node_offset: i32, compact_ledge_offset: i32) {
    let before = bytes.len();
    bytes.extend_from_slice(&right_node_offset.to_le_bytes());
    bytes.extend_from_slice(&compact_ledge_offset.to_le_bytes());
    bytes.extend_from_slice(&[0; 20]);
    assert_eq!(bytes.len() - before, COMPACT_LEDGETREE_NODE_BYTES);
}

fn compact_ledge_record_len() -> usize {
    COMPACT_LEDGE_HEADER_BYTES + TETRA_TRIANGLES.len() * COMPACT_TRIANGLE_BYTES
}

fn finish_section(mut section: Vec<u8>) -> Vec<u8> {
    let section_size = section
        .len()
        .checked_sub(4)
        .expect("section has size field");
    let section_size = i32::try_from(section_size).expect("section size fits i32");
    section[0..4].copy_from_slice(&section_size.to_le_bytes());
    section
}

pub fn expected_tetra_source_vertex(index: usize) -> [f32; 3] {
    match index {
        0 => [0.0, 0.0, 0.0],
        1 => [IVP_METERS_TO_SOURCE_INCHES, 0.0, 0.0],
        2 => [0.0, IVP_METERS_TO_SOURCE_INCHES, 0.0],
        3 => [0.0, 0.0, -IVP_METERS_TO_SOURCE_INCHES],
        _ => panic!("unexpected tetra index"),
    }
}
