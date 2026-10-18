use vformats::{
    Limits,
    bsp::{self, lump_ids},
};

fn fixture(lumps: &[(usize, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = vec![0; 4 + 4 + 64 * 16 + 4];
    bytes[..4].copy_from_slice(b"VBSP");
    bytes[4..8].copy_from_slice(&20_i32.to_le_bytes());
    for (id, payload) in lumps {
        let entry = 8 + id * 16;
        let offset = bytes.len();
        bytes[entry..entry + 4].copy_from_slice(&(offset as u32).to_le_bytes());
        bytes[entry + 4..entry + 8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend(payload);
    }
    bytes
}
#[test]
fn displacement_neighbor_spans_corners_and_allowed_vertices_keep_wire_offsets() {
    let mut disp = vec![0; 176];
    disp[20..24].copy_from_slice(&4_i32.to_le_bytes());
    for edge in 0..4 {
        for sub in 0..2 {
            let o = 48 + edge * 12 + sub * 6;
            disp[o..o + 2].copy_from_slice(&((edge * 2 + sub + 1) as u16).to_le_bytes());
            disp[o + 2] = edge as u8;
            disp[o + 3] = sub as u8;
            disp[o + 4] = 2;
            disp[o + 5] = 0xfe;
        }
    }
    disp[96..98].copy_from_slice(&123_u16.to_le_bytes());
    disp[104] = 1;
    for i in 0..10 {
        disp[136 + i * 4..140 + i * 4].copy_from_slice(&(1_u32 << i).to_le_bytes());
    }
    let bytes = fixture(&[(lump_ids::DISPINFO, disp)]);
    let limits = Limits::default();
    let bsp = bsp::parse(&bytes, &limits).unwrap();
    let records = bsp.displacement_infos(&limits).unwrap();
    let d = records[0];
    assert_eq!(d.power, 4);
    assert_eq!(d.edge_neighbors[3][1].neighbor, 8);
    assert_eq!(d.edge_neighbors[3][1].orientation, 3);
    assert_eq!(d.edge_neighbors[3][1].span, 1);
    assert_eq!(d.edge_neighbors[3][1].neighbor_span, 2);
    assert_eq!(d.corner_neighbors[0].neighbors[0], 123);
    assert_eq!(d.corner_neighbors[0].count, 1);
    assert_eq!(d.allowed_vertices[9], 512);
}
#[test]
fn compiled_corner_normal_arrays_preserve_signed_vectors_and_u16_indices() {
    let mut normals = Vec::new();
    for value in [0.0_f32, -0.6, 0.8] {
        normals.extend(value.to_le_bytes());
    }
    let bytes = fixture(&[
        (lump_ids::VERT_NORMALS, normals),
        (lump_ids::VERT_NORMAL_INDICES, vec![255, 255, 0, 0]),
    ]);
    let limits = Limits::default();
    let bsp = bsp::parse(&bytes, &limits).unwrap();
    assert_eq!(bsp.vertex_normals(&limits).unwrap(), [[0.0, -0.6, 0.8]]);
    assert_eq!(bsp.vertex_normal_indices(&limits).unwrap(), [u16::MAX, 0]);
}
