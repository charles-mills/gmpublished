//! Typed lump tests: hand-built records with distinct values at every
//! field offset, the leaf lump's version fork, and the caps.

use vformats::Limits;
use vformats::bsp::{ColorRgbExp, lump_ids, parse, texture_flags};

const HEADER_BYTES: usize = 4 + 4 + 64 * 16 + 4;

fn build_bsp(lumps: &[(usize, Vec<u8>, i32)]) -> Vec<u8> {
    let mut b = vec![0u8; HEADER_BYTES];
    b[0..4].copy_from_slice(b"VBSP");
    b[4..8].copy_from_slice(&20i32.to_le_bytes());
    for (index, data, version) in lumps {
        let offset = b.len();
        let entry = 8 + index * 16;
        b[entry..entry + 4].copy_from_slice(&(offset as i32).to_le_bytes());
        b[entry + 4..entry + 8].copy_from_slice(&(data.len() as i32).to_le_bytes());
        b[entry + 8..entry + 12].copy_from_slice(&version.to_le_bytes());
        b.extend_from_slice(data);
    }
    b
}

struct Record(Vec<u8>);

impl Record {
    fn new() -> Self {
        Self(Vec::new())
    }
    fn f32(mut self, v: f32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn i32(mut self, v: i32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u16(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn i16(mut self, v: i16) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(mut self, v: &[u8]) -> Self {
        self.0.extend_from_slice(v);
        self
    }
}

#[test]
fn geometry_lumps_decode_every_field() {
    let plane = Record::new().f32(0.0).f32(0.0).f32(1.0).f32(64.0).i32(2).0;
    let vertices = Record::new()
        .f32(1.0)
        .f32(2.0)
        .f32(3.0)
        .f32(-4.0)
        .f32(-5.0)
        .f32(-6.0)
        .0;
    let edges = Record::new().u16(3).u16(7).u16(7).u16(9).0;
    let surfedges = Record::new().i32(5).i32(-2).0;
    let face = Record::new()
        .u16(11) // plane
        .bytes(&[1, 0]) // side, on_node
        .i32(100) // first edge
        .i16(4) // edge count
        .i16(2) // texinfo
        .i16(-1) // displacement
        .i16(0) // fog volume
        .bytes(&[0, 1, 2, 3]) // styles
        .i32(4096) // light offset
        .f32(512.0) // area
        .i32(-8)
        .i32(-16) // lightmap mins
        .i32(32)
        .i32(64) // lightmap size
        .i32(77) // original face
        .u16(0)
        .u16(0) // prims
        .i32(0) // smoothing groups
        .0;
    assert_eq!(face.len(), 56);
    let texinfo = {
        let mut r = Record::new();
        for v in 0..16 {
            r = r.f32(v as f32 * 0.5);
        }
        r.i32(texture_flags::SKY | texture_flags::NODRAW).i32(1).0
    };
    assert_eq!(texinfo.len(), 72);
    let texdata = Record::new()
        .f32(0.5)
        .f32(0.25)
        .f32(0.125)
        .i32(1) // name index
        .i32(1024)
        .i32(512)
        .i32(1024)
        .i32(512)
        .0;
    let names = b"TOOLS/TOOLSNODRAW\0concrete/wall01\0".to_vec();
    let name_table = Record::new().i32(0).i32(18).i32(9999).0; // last is OOB
    let model = Record::new()
        .f32(-64.0)
        .f32(-64.0)
        .f32(0.0)
        .f32(64.0)
        .f32(64.0)
        .f32(128.0)
        .f32(0.0)
        .f32(0.0)
        .f32(0.0)
        .i32(0)
        .i32(0)
        .i32(1)
        .0;
    assert_eq!(model.len(), 48);
    let disp = {
        let mut r = Record::new()
            .f32(10.0)
            .f32(20.0)
            .f32(30.0) // start position
            .i32(0) // vert start
            .i32(0) // tri start
            .i32(3) // power
            .i32(0) // min tess
            .f32(0.5) // smoothing angle
            .i32(1) // contents
            .u16(6) // map face
            .u16(0) // padding to align
            .i32(1234) // lightmap alpha start
            .i32(5678); // lightmap sample position start
        r.0.resize(176, 0);
        r.0
    };
    let disp_vert = Record::new()
        .f32(0.0)
        .f32(0.0)
        .f32(1.0)
        .f32(24.0)
        .f32(255.0)
        .0;

    let bytes = build_bsp(&[
        (lump_ids::PLANES, plane, 0),
        (lump_ids::VERTICES, vertices, 0),
        (lump_ids::EDGES, edges, 0),
        (lump_ids::SURFEDGES, surfedges, 0),
        (lump_ids::FACES, face, 1),
        (lump_ids::TEXINFO, texinfo, 0),
        (lump_ids::TEXDATA, texdata, 0),
        (lump_ids::TEXDATA_STRING_DATA, names, 0),
        (lump_ids::TEXDATA_STRING_TABLE, name_table, 0),
        (lump_ids::MODELS, model, 0),
        (lump_ids::DISPINFO, disp, 0),
        (lump_ids::DISP_VERTS, disp_vert, 0),
    ]);
    let limits = Limits::default();
    let bsp = parse(&bytes, &limits).expect("parse");

    let planes = bsp.planes(&limits).expect("planes");
    assert_eq!(planes.len(), 1);
    assert_eq!(planes[0].normal, [0.0, 0.0, 1.0]);
    assert_eq!(planes[0].dist, 64.0);
    assert_eq!(planes[0].axis_type, 2);

    assert_eq!(
        bsp.vertices(&limits).expect("vertices"),
        [[1.0, 2.0, 3.0], [-4.0, -5.0, -6.0]]
    );
    assert_eq!(bsp.edges(&limits).expect("edges"), [[3, 7], [7, 9]]);
    assert_eq!(bsp.surfedges(&limits).expect("surfedges"), [5, -2]);

    let faces = bsp.faces(&limits).expect("faces");
    let f = &faces[0];
    assert_eq!(
        (f.plane, f.side, f.on_node, f.first_edge, f.edge_count),
        (11, 1, 0, 100, 4)
    );
    assert_eq!((f.texinfo, f.displacement), (2, -1));
    assert_eq!(f.styles, [0, 1, 2, 3]);
    assert_eq!((f.light_offset, f.area), (4096, 512.0));
    assert_eq!((f.lightmap_mins, f.lightmap_size), ([-8, -16], [32, 64]));
    assert_eq!(f.original_face, 77);

    let ti = &bsp.texinfos(&limits).expect("texinfos")[0];
    assert_eq!(ti.texture_vecs[0], [0.0, 0.5, 1.0, 1.5]);
    assert_eq!(ti.lightmap_vecs[1], [6.0, 6.5, 7.0, 7.5]);
    assert_ne!(ti.flags & texture_flags::SKY, 0);
    assert_eq!(ti.texdata, 1);

    let td = &bsp.texdatas(&limits).expect("texdatas")[0];
    assert_eq!(td.reflectivity, [0.5, 0.25, 0.125]);
    assert_eq!((td.name_index, td.width, td.height), (1, 1024, 512));

    let strings = bsp.texdata_strings(&limits).expect("strings");
    assert_eq!(strings[0], "TOOLS/TOOLSNODRAW");
    assert_eq!(strings[1], "concrete/wall01");
    assert_eq!(strings[2], "", "out-of-range offsets decode as empty");

    let m = &bsp.models(&limits).expect("models")[0];
    assert_eq!((m.mins, m.maxs), ([-64.0, -64.0, 0.0], [64.0, 64.0, 128.0]));
    assert_eq!((m.head_node, m.first_face, m.face_count), (0, 0, 1));

    let d = &bsp.displacement_infos(&limits).expect("disp")[0];
    assert_eq!(d.start_position, [10.0, 20.0, 30.0]);
    assert_eq!((d.power, d.map_face), (3, 6));
    assert_eq!(
        (d.lightmap_alpha_start, d.lightmap_sample_position_start),
        (1234, 5678)
    );

    let dv = &bsp.displacement_verts(&limits).expect("disp verts")[0];
    assert_eq!(
        (dv.vector, dv.dist, dv.alpha),
        ([0.0, 0.0, 1.0], 24.0, 255.0)
    );
}

fn leaf_common() -> Record {
    Record::new()
        .i32(1) // contents = SOLID
        .i16(9) // cluster
        .i16((3 << 9) | 5) // flags 3, area 5
        .i16(-10)
        .i16(-20)
        .i16(-30)
        .i16(10)
        .i16(20)
        .i16(30)
        .u16(2)
        .u16(3) // leaf faces
        .u16(4)
        .u16(5) // leaf brushes
        .i16(-1) // water data
}

#[test]
fn leaf_lump_versions_fork_on_the_directory_version() {
    // Version 1: 32-byte leaves, no ambient.
    let v1 = {
        let mut r = leaf_common();
        r = r.i16(0); // padding
        assert_eq!(r.0.len(), 32);
        r.0
    };
    let bytes = build_bsp(&[(lump_ids::LEAFS, v1, 1)]);
    let limits = Limits::default();
    let leaf = &parse(&bytes, &limits).unwrap().leafs(&limits).expect("v1")[0];
    assert_eq!(leaf.contents, 1);
    assert_eq!(leaf.cluster, 9);
    assert_eq!((leaf.area(), leaf.flags()), (5, 3));
    assert_eq!((leaf.mins, leaf.maxs), ([-10, -20, -30], [10, 20, 30]));
    assert_eq!(
        (
            leaf.first_leaf_face,
            leaf.leaf_face_count,
            leaf.first_leaf_brush,
            leaf.leaf_brush_count
        ),
        (2, 3, 4, 5)
    );
    assert!(leaf.ambient.is_none());

    // Version 0: 56-byte leaves with an embedded ambient cube.
    let v0 = {
        let mut r = leaf_common();
        for face in 0..6u8 {
            r = r.bytes(&[face, face, face, 0]);
        }
        r = r.i16(0); // padding
        assert_eq!(r.0.len(), 56);
        r.0
    };
    let bytes = build_bsp(&[(lump_ids::LEAFS, v0, 0)]);
    let leaf = &parse(&bytes, &limits).unwrap().leafs(&limits).expect("v0")[0];
    let ambient = leaf.ambient.expect("v0 has ambient");
    assert_eq!(ambient[5].r, 5);
    assert_eq!(leaf.leaf_water_data, -1);
}

#[test]
fn lighting_ambient_and_caps() {
    let lighting = Record::new()
        .bytes(&[128, 64, 32, 0])
        .bytes(&[255, 0, 0, 0x80])
        .0;
    let ambient = {
        let mut r = Record::new();
        for face in 0..6u8 {
            r = r.bytes(&[face * 10, 0, 0, 0]);
        }
        r.bytes(&[1, 2, 3, 0]).0 // position + pad
    };
    let index = Record::new().u16(1).u16(0).0;
    let bytes = build_bsp(&[
        (lump_ids::LIGHTING, lighting, 0),
        (lump_ids::LEAF_AMBIENT_LIGHTING, ambient, 0),
        (lump_ids::LEAF_AMBIENT_INDEX, index, 0),
    ]);
    let limits = Limits::default();
    let bsp = parse(&bytes, &limits).expect("parse");

    let samples = bsp.lighting(&limits).expect("lighting");
    assert_eq!(samples.len(), 2);
    assert_eq!(
        samples[0],
        ColorRgbExp {
            r: 128,
            g: 64,
            b: 32,
            exponent: 0
        }
    );
    assert_eq!(samples[0].to_linear(), [128.0, 64.0, 32.0]);
    assert_eq!(samples[1].exponent, -128i8);
    assert!(bsp.lighting_hdr(&limits).expect("hdr").is_empty());

    let ambient = bsp.leaf_ambient_lighting(&limits).expect("ambient");
    assert_eq!(ambient[0].cube[3].r, 30);
    assert_eq!(ambient[0].position, [1, 2, 3]);
    let indices = bsp.leaf_ambient_indices(&limits).expect("indices");
    assert_eq!((indices[0].sample_count, indices[0].first_sample), (1, 0));

    // ColorRgbExp exponent math at the extremes used by real content.
    assert_eq!(
        ColorRgbExp {
            r: 1,
            g: 0,
            b: 0,
            exponent: 4
        }
        .to_linear()[0],
        16.0
    );
    assert_eq!(
        ColorRgbExp {
            r: 128,
            g: 0,
            b: 0,
            exponent: -7
        }
        .to_linear()[0],
        1.0
    );

    // Record counts are not capped by max_entries: the count derives
    // from lump bytes that are already bounded, and dense lumps
    // (lighting) legitimately hold millions of records.
    let two = Limits {
        max_entries: 1,
        ..Limits::default()
    };
    assert_eq!(bsp.lighting(&two).expect("lighting uncapped").len(), 2);

    // Trailing-byte tolerance.
    let ragged = build_bsp(&[(lump_ids::VERTICES, vec![0u8; 25], 0)]);
    let bsp = parse(&ragged, &limits).expect("parse");
    assert_eq!(bsp.vertices(&limits).expect("vertices").len(), 2);
}

#[test]
fn overlays_decode_and_trim_their_face_table() {
    let mut faces = Record::new().i32(11).i32(22);
    for _ in 2..64 {
        faces = faces.i32(-1);
    }
    // Packed face count 2, render order 1.
    let overlay = Record::new()
        .i32(7)
        .i16(3)
        .u16(2 | (1 << 14))
        .bytes(&faces.0)
        .f32(0.25)
        .f32(0.75)
        .f32(0.1)
        .f32(0.9)
        .f32(1.0)
        .f32(2.0)
        .f32(3.0)
        .f32(4.0)
        .f32(5.0)
        .f32(6.0)
        .f32(7.0)
        .f32(8.0)
        .f32(9.0)
        .f32(10.0)
        .f32(11.0)
        .f32(12.0)
        .f32(13.0)
        .f32(14.0)
        .f32(15.0)
        .f32(0.0)
        .f32(0.0)
        .f32(1.0)
        .0;
    assert_eq!(overlay.len(), 352);
    let bytes = build_bsp(&[(lump_ids::OVERLAYS, overlay, 0)]);
    let limits = Limits::default();
    let bsp = parse(&bytes, &limits).expect("parse");

    let overlays = bsp.overlays(&limits).expect("overlays");
    assert_eq!(overlays.len(), 1);
    let overlay = &overlays[0];
    assert_eq!(overlay.id, 7);
    assert_eq!(overlay.texinfo, 3);
    assert_eq!(overlay.face_count(), 2);
    assert_eq!(overlay.render_order(), 1);
    assert_eq!(overlay.faces(), &[11, 22]);
    assert_eq!(overlay.u, [0.25, 0.75]);
    assert_eq!(overlay.v, [0.1, 0.9]);
    assert_eq!(overlay.uv_points[0], [1.0, 2.0, 3.0]);
    assert_eq!(overlay.uv_points[3], [10.0, 11.0, 12.0]);
    assert_eq!(overlay.origin, [13.0, 14.0, 15.0]);
    assert_eq!(overlay.basis_normal, [0.0, 0.0, 1.0]);
}

#[test]
fn nodes_brush_sides_and_leaf_tables_decode() {
    let node = Record::new()
        .i32(3) // plane
        .i32(1)
        .i32(-2) // children
        .i16(-10)
        .i16(-11)
        .i16(-12) // mins
        .i16(10)
        .i16(11)
        .i16(12) // maxs
        .u16(5)
        .u16(2) // faces
        .i16(1) // area
        .i16(0) // padding
        .0;
    let side = Record::new().u16(7).i16(3).i16(-1).i16(1).0;
    let leaf_faces = Record::new().u16(4).u16(9).0;
    let leaf_brushes = Record::new().u16(6).0;
    let bytes = build_bsp(&[
        (lump_ids::NODES, node, 0),
        (lump_ids::BRUSHSIDES, side, 0),
        (lump_ids::LEAF_FACES, leaf_faces, 0),
        (lump_ids::LEAF_BRUSHES, leaf_brushes, 0),
    ]);
    let limits = Limits::default();
    let bsp = parse(&bytes, &limits).expect("parse");

    let nodes = bsp.nodes(&limits).expect("nodes");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].plane, 3);
    assert_eq!(nodes[0].children, [1, -2]);
    assert_eq!(nodes[0].mins, [-10, -11, -12]);
    assert_eq!(nodes[0].maxs, [10, 11, 12]);
    assert_eq!((nodes[0].first_face, nodes[0].face_count), (5, 2));
    assert_eq!(nodes[0].area, 1);

    let sides = bsp.brush_sides(&limits).expect("brush sides");
    assert_eq!(sides.len(), 1);
    assert_eq!(sides[0].plane, 7);
    assert_eq!(sides[0].texinfo, 3);
    assert_eq!(sides[0].displacement, -1);
    assert_eq!(sides[0].bevel, 1);

    assert_eq!(bsp.leaf_faces(&limits).expect("leaf faces"), vec![4, 9]);
    assert_eq!(bsp.leaf_brushes(&limits).expect("leaf brushes"), vec![6]);
}
