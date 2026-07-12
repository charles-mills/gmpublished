//! Game lump (static/detail props across the version zoo), visibility
//! RLE, and per-lump LZMA compression tests, all over hand-built maps.

use std::borrow::Cow;

use vformats::Limits;
use vformats::bsp::{BspError, StaticPropLayout, game_lump_ids, lump_ids, parse};

const HEADER_BYTES: usize = 4 + 4 + 64 * 16 + 4;

fn build_bsp(lumps: &[(usize, Vec<u8>, i32, i32)]) -> Vec<u8> {
    let mut b = vec![0u8; HEADER_BYTES];
    b[0..4].copy_from_slice(b"VBSP");
    b[4..8].copy_from_slice(&20i32.to_le_bytes());
    for (index, data, version, four_cc) in lumps {
        let offset = b.len();
        let entry = 8 + index * 16;
        b[entry..entry + 4].copy_from_slice(&(offset as i32).to_le_bytes());
        b[entry + 4..entry + 8].copy_from_slice(&(data.len() as i32).to_le_bytes());
        b[entry + 8..entry + 12].copy_from_slice(&version.to_le_bytes());
        b[entry + 12..entry + 16].copy_from_slice(&four_cc.to_le_bytes());
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
    fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u16(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u8(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    fn vec3(self, v: [f32; 3]) -> Self {
        self.f32(v[0]).f32(v[1]).f32(v[2])
    }
    fn bytes_n(mut self, n: usize) -> Self {
        self.0.extend(std::iter::repeat_n(0u8, n));
        self
    }
}

/// A game lump's lump bytes: directory (with absolute file offsets
/// based at `file_base`) then payloads. `declared_len` is the entry's
/// `len` field (the uncompressed size for compressed sub-lumps).
fn game_lump_bytes(file_base: usize, subs: &[(i32, u16, u16, &[u8], usize)]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(subs.len() as i32).to_le_bytes());
    let mut at = file_base + 4 + subs.len() * 16;
    for (id, flags, version, stored, declared_len) in subs {
        b.extend_from_slice(&id.to_le_bytes());
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(&version.to_le_bytes());
        b.extend_from_slice(&(at as i32).to_le_bytes());
        b.extend_from_slice(&(*declared_len as i32).to_le_bytes());
        at += stored.len();
    }
    for (_, _, _, stored, _) in subs {
        b.extend_from_slice(stored);
    }
    b
}

fn sprp_payload(models: &[&str], leaves: &[u16], props: &[&[u8]]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(models.len() as i32).to_le_bytes());
    for model in models {
        let mut name = [0u8; 128];
        name[..model.len()].copy_from_slice(model.as_bytes());
        b.extend_from_slice(&name);
    }
    b.extend_from_slice(&(leaves.len() as i32).to_le_bytes());
    for leaf in leaves {
        b.extend_from_slice(&leaf.to_le_bytes());
    }
    b.extend_from_slice(&(props.len() as i32).to_le_bytes());
    for prop in props {
        b.extend_from_slice(prop);
    }
    b
}

/// The 56 core bytes every static prop version shares.
fn prop_core() -> Record {
    Record::new()
        .vec3([1.0, 2.0, 3.0]) // origin
        .vec3([10.0, 20.0, 30.0]) // angles
        .u16(1) // model_index
        .u16(2) // first_leaf
        .u16(3) // leaf_count
        .u8(6) // solid
        .u8(4) // flags
        .i32(7) // skin
        .f32(100.0) // fade min
        .f32(200.0) // fade max
        .vec3([4.0, 5.0, 6.0]) // lighting origin
}

fn sprp_bsp(version: u16, payload: &[u8]) -> Vec<u8> {
    let lump = game_lump_bytes(
        HEADER_BYTES,
        &[(
            game_lump_ids::STATIC_PROPS,
            0,
            version,
            payload,
            payload.len(),
        )],
    );
    build_bsp(&[(lump_ids::GAME_LUMP, lump, 0, 0)])
}

#[test]
fn static_props_v4_decode_core_fields() {
    let record = prop_core().0;
    let payload = sprp_payload(
        &["models/props/oildrum.mdl", "models/props/tree.mdl"],
        &[9, 8, 7],
        &[&record, &record],
    );
    let bytes = sprp_bsp(4, &payload);
    let limits = Limits::default();
    let bsp = parse(&bytes, &limits).expect("parse");

    let props = bsp.static_props(&limits).expect("sprp").expect("present");
    assert_eq!(props.version, 4);
    assert_eq!(props.layout, StaticPropLayout::Standard);
    assert_eq!(props.stride, 56);
    assert_eq!(
        props.models,
        vec!["models/props/oildrum.mdl", "models/props/tree.mdl"]
    );
    assert_eq!(props.leaves, vec![9, 8, 7]);
    assert_eq!(props.props.len(), 2);
    let prop = &props.props[0];
    assert_eq!(prop.origin, [1.0, 2.0, 3.0]);
    assert_eq!(prop.angles, [10.0, 20.0, 30.0]);
    assert_eq!(prop.model_index, 1);
    assert_eq!(prop.first_leaf, 2);
    assert_eq!(prop.leaf_count, 3);
    assert_eq!(prop.solid, 6);
    assert_eq!(prop.flags, 4);
    assert_eq!(prop.skin, 7);
    assert_eq!(prop.fade_min_distance, 100.0);
    assert_eq!(prop.fade_max_distance, 200.0);
    assert_eq!(prop.lighting_origin, [4.0, 5.0, 6.0]);
    assert_eq!(prop.forced_fade_scale, None);
    assert_eq!(prop.dx_levels, None);
    assert_eq!(prop.cpu_gpu_levels, None);
    assert_eq!(prop.diffuse_modulation, None);
    assert_eq!(prop.disable_x360, None);
    assert_eq!(prop.flags_ex, None);
    assert_eq!(prop.uniform_scale, None);
    assert_eq!(prop.extra_flags, None);
    assert_eq!(prop.lightmap_resolution, None);
}

#[test]
fn static_props_standard_versions_decode_their_tails() {
    let limits = Limits::default();

    // v6: fade scale + DirectX levels.
    let record = prop_core().f32(0.5).u16(80).u16(95).0;
    let bytes = sprp_bsp(6, &sprp_payload(&["m"], &[], &[&record]));
    let props = parse(&bytes, &limits)
        .expect("parse")
        .static_props(&limits)
        .expect("sprp")
        .expect("present");
    assert_eq!(
        (props.layout, props.stride),
        (StaticPropLayout::Standard, 64)
    );
    assert_eq!(props.props[0].forced_fade_scale, Some(0.5));
    assert_eq!(props.props[0].dx_levels, Some([80, 95]));
    assert_eq!(props.props[0].cpu_gpu_levels, None);

    // Standard v10 (76 bytes): cpu/gpu levels, diffuse modulation,
    // x360 toggle, extra flags word.
    let record = prop_core()
        .f32(0.5)
        .u8(1)
        .u8(2)
        .u8(3)
        .u8(4)
        .u8(10)
        .u8(20)
        .u8(30)
        .u8(40)
        .u32(1)
        .u32(0xAABB_CCDD)
        .0;
    let bytes = sprp_bsp(10, &sprp_payload(&["m"], &[], &[&record]));
    let props = parse(&bytes, &limits)
        .expect("parse")
        .static_props(&limits)
        .expect("sprp")
        .expect("present");
    assert_eq!(
        (props.layout, props.stride),
        (StaticPropLayout::Standard, 76)
    );
    let prop = &props.props[0];
    assert_eq!(prop.cpu_gpu_levels, Some([1, 2, 3, 4]));
    assert_eq!(prop.diffuse_modulation, Some([10, 20, 30, 40]));
    assert_eq!(prop.disable_x360, Some(true));
    assert_eq!(prop.flags_ex, Some(0xAABB_CCDD));
    assert_eq!(prop.dx_levels, None);
    assert_eq!(prop.lightmap_resolution, None);

    // v11 appends the uniform scale.
    let record = prop_core().f32(0.5).u32(0).u32(0).u32(0).u32(0).f32(1.5).0;
    let bytes = sprp_bsp(11, &sprp_payload(&["m"], &[], &[&record]));
    let props = parse(&bytes, &limits)
        .expect("parse")
        .static_props(&limits)
        .expect("sprp")
        .expect("present");
    assert_eq!(
        (props.layout, props.stride),
        (StaticPropLayout::Standard, 80)
    );
    assert_eq!(props.props[0].uniform_scale, Some(1.5));
}

#[test]
fn static_props_v10_multiplayer_branch_resolves_by_record_size() {
    // 72-byte version 10: the 2013 MP branch's layout (DirectX levels,
    // second flags word, lightmap resolution).
    let record = prop_core()
        .f32(0.5)
        .u16(80)
        .u16(95)
        .u32(0x0000_0180)
        .u16(32)
        .u16(64)
        .0;
    assert_eq!(record.len(), 72);
    let bytes = sprp_bsp(10, &sprp_payload(&["m"], &[], &[&record, &record]));
    let limits = Limits::default();
    let props = parse(&bytes, &limits)
        .expect("parse")
        .static_props(&limits)
        .expect("sprp")
        .expect("present");
    assert_eq!(
        (props.layout, props.stride),
        (StaticPropLayout::Multiplayer2013, 72)
    );
    let prop = &props.props[1];
    assert_eq!(prop.dx_levels, Some([80, 95]));
    assert_eq!(prop.extra_flags, Some(0x0000_0180));
    assert_eq!(prop.lightmap_resolution, Some([32, 64]));
    assert_eq!(prop.flags_ex, None);
    assert_eq!(prop.disable_x360, None);
}

#[test]
fn static_props_unknown_version_fall_back_to_core() {
    // An unrecognized future version with an 84-byte record: core
    // fields and the fade scale decode; the tail is ignored.
    let record = prop_core().f32(0.5).bytes_n(24).0;
    let bytes = sprp_bsp(12, &sprp_payload(&["m"], &[5], &[&record]));
    let limits = Limits::default();
    let props = parse(&bytes, &limits)
        .expect("parse")
        .static_props(&limits)
        .expect("sprp")
        .expect("present");
    assert_eq!((props.layout, props.stride), (StaticPropLayout::Core, 84));
    assert_eq!(props.props[0].origin, [1.0, 2.0, 3.0]);
    assert_eq!(props.props[0].forced_fade_scale, Some(0.5));
    assert_eq!(props.props[0].dx_levels, None);
}

#[test]
fn static_props_truncated_dictionary_error() {
    // Declares two model names but carries one.
    let mut payload = Vec::new();
    payload.extend_from_slice(&2i32.to_le_bytes());
    payload.extend_from_slice(&[0u8; 128]);
    let bytes = sprp_bsp(4, &payload);
    let limits = Limits::default();
    assert!(matches!(
        parse(&bytes, &limits).expect("parse").static_props(&limits),
        Err(BspError::Decode { .. })
    ));
}

#[test]
fn missing_and_empty_game_lumps_read_as_absent() {
    let limits = Limits::default();

    // No game lump at all.
    let bytes = build_bsp(&[]);
    let bsp = parse(&bytes, &limits).expect("parse");
    assert!(bsp.game_lumps(&limits).expect("directory").is_empty());
    assert_eq!(bsp.static_props(&limits).expect("sprp"), None);
    assert_eq!(bsp.detail_props(&limits).expect("dprp"), None);

    // A directory whose sprp entry is empty.
    let lump = game_lump_bytes(HEADER_BYTES, &[(game_lump_ids::STATIC_PROPS, 0, 4, &[], 0)]);
    let bytes = build_bsp(&[(lump_ids::GAME_LUMP, lump, 0, 0)]);
    let bsp = parse(&bytes, &limits).expect("parse");
    assert_eq!(bsp.game_lumps(&limits).expect("directory").len(), 1);
    assert_eq!(bsp.static_props(&limits).expect("sprp"), None);
}

fn dprp_payload(models: &[&str], sprites: &[&[u8]], props: &[&[u8]]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(models.len() as i32).to_le_bytes());
    for model in models {
        let mut name = [0u8; 128];
        name[..model.len()].copy_from_slice(model.as_bytes());
        b.extend_from_slice(&name);
    }
    b.extend_from_slice(&(sprites.len() as i32).to_le_bytes());
    for sprite in sprites {
        b.extend_from_slice(sprite);
    }
    b.extend_from_slice(&(props.len() as i32).to_le_bytes());
    for prop in props {
        b.extend_from_slice(prop);
    }
    b
}

/// The 40 bytes every detail prop record starts with.
fn detail_prop_head() -> Record {
    Record::new()
        .vec3([10.0, 20.0, 30.0]) // origin
        .vec3([1.0, 90.0, 3.0]) // angles
        .u16(1) // model_index
        .u16(5) // leaf
        .u8(64)
        .u8(128)
        .u8(255)
        .u8(2) // lighting rgb + exponent
        .u32(77) // light_styles
        .u8(3) // light_style_count
        .u8(9) // sway
        .u8(45) // shape_angle
        .u8(200) // shape_size
}

fn dprp_bsp(payload: &[u8]) -> Vec<u8> {
    let lump = game_lump_bytes(
        HEADER_BYTES,
        &[(game_lump_ids::DETAIL_PROPS, 0, 4, payload, payload.len())],
    );
    build_bsp(&[(lump_ids::GAME_LUMP, lump, 0, 0)])
}

#[test]
fn detail_props_decode_both_record_sizes() {
    let sprite = Record::new()
        .f32(-2.0)
        .f32(12.0)
        .f32(2.0)
        .f32(0.0)
        .f32(0.0)
        .f32(0.0)
        .f32(0.25)
        .f32(0.5)
        .0;
    let limits = Limits::default();

    // The SDK's 48-byte record: prop type at 44.
    let record = detail_prop_head().u8(3).bytes_n(3).u8(1).bytes_n(3).0;
    assert_eq!(record.len(), 48);
    let bytes = dprp_bsp(&dprp_payload(&["models/grass.mdl"], &[&sprite], &[&record]));
    let props = parse(&bytes, &limits)
        .expect("parse")
        .detail_props(&limits)
        .expect("dprp")
        .expect("present");
    assert_eq!(props.models, vec!["models/grass.mdl"]);
    assert_eq!(props.sprites.len(), 1);
    assert_eq!(props.sprites[0].upper_left, [-2.0, 12.0]);
    assert_eq!(props.sprites[0].tex_lower_right, [0.25, 0.5]);
    let prop = &props.props[0];
    assert_eq!(prop.origin, [10.0, 20.0, 30.0]);
    assert_eq!(prop.angles, [1.0, 90.0, 3.0]);
    assert_eq!(prop.model_index, 1);
    assert_eq!(prop.leaf, 5);
    assert_eq!((prop.lighting.r, prop.lighting.exponent), (64, 2));
    assert_eq!(prop.light_styles, 77);
    assert_eq!(prop.light_style_count, 3);
    assert_eq!(prop.sway_amount, 9);
    assert_eq!(prop.shape_angle, 45);
    assert_eq!(prop.shape_size, 200);
    assert_eq!(prop.orientation, 3);
    assert_eq!(prop.prop_type, 1);

    // The older 44-byte record: prop type at 41.
    let record = detail_prop_head().u8(3).u8(1).bytes_n(2).0;
    assert_eq!(record.len(), 44);
    let bytes = dprp_bsp(&dprp_payload(
        &["models/grass.mdl"],
        &[],
        &[&record, &record],
    ));
    let props = parse(&bytes, &limits)
        .expect("parse")
        .detail_props(&limits)
        .expect("dprp")
        .expect("present");
    assert_eq!(props.props.len(), 2);
    assert_eq!(props.props[1].orientation, 3);
    assert_eq!(props.props[1].prop_type, 1);
}

#[test]
fn detail_props_keep_complete_sections_when_truncated() {
    // One complete model name, then a sprite section declaring two
    // records but carrying half of one.
    let mut payload = Vec::new();
    payload.extend_from_slice(&1i32.to_le_bytes());
    let mut name = [0u8; 128];
    name[..12].copy_from_slice(b"models/a.mdl");
    payload.extend_from_slice(&name);
    payload.extend_from_slice(&2i32.to_le_bytes());
    payload.extend_from_slice(&[0u8; 16]);

    let bytes = dprp_bsp(&payload);
    let limits = Limits::default();
    let props = parse(&bytes, &limits)
        .expect("parse")
        .detail_props(&limits)
        .expect("dprp")
        .expect("present");
    assert_eq!(props.models, vec!["models/a.mdl"]);
    assert!(props.sprites.is_empty());
    assert!(props.props.is_empty());
}

#[cfg(feature = "lzma")]
fn valve_lzma(payload: &[u8]) -> Vec<u8> {
    let options = lzma_rust2::LzmaOptions::with_preset(6);
    let mut encoder =
        lzma_rust2::LzmaWriter::new_use_header(Vec::new(), &options, Some(payload.len() as u64))
            .expect("encoder");
    std::io::Write::write_all(&mut encoder, payload).expect("compress");
    let alone = encoder.finish().expect("finish");
    // Valve framing: magic, uncompressed and stream sizes, raw props.
    let mut out = Vec::new();
    out.extend_from_slice(b"LZMA");
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&((alone.len() - 13) as u32).to_le_bytes());
    out.extend_from_slice(&alone[0..5]);
    out.extend_from_slice(&alone[13..]);
    out
}

#[cfg(feature = "lzma")]
#[test]
fn compressed_game_lumps_decompress() {
    let record = prop_core().0;
    let payload = sprp_payload(&["models/props/oildrum.mdl"], &[1, 2], &[&record]);
    let compressed = valve_lzma(&payload);
    // A compressed sub-lump's extent runs to the next entry's offset;
    // the zero sentinel entry closes the list.
    let sentinel_offset = HEADER_BYTES + 4 + 2 * 16 + compressed.len();
    let mut lump = game_lump_bytes(
        HEADER_BYTES,
        &[
            (
                game_lump_ids::STATIC_PROPS,
                1,
                4,
                &compressed,
                payload.len(),
            ),
            (0, 0, 0, &[], 0),
        ],
    );
    // The builder bases every offset past the directory; the sentinel's
    // is already the end of the compressed payload.
    assert_eq!(
        &lump[4 + 16 + 8..4 + 16 + 12],
        (sentinel_offset as i32).to_le_bytes().as_slice()
    );
    let bytes = build_bsp(&[(lump_ids::GAME_LUMP, lump.clone(), 0, 0)]);
    let limits = Limits::default();
    let bsp = parse(&bytes, &limits).expect("parse");

    let entry = bsp.game_lumps(&limits).expect("directory")[0];
    assert!(entry.is_compressed());
    assert_eq!(entry.len, payload.len());

    let props = bsp.static_props(&limits).expect("sprp").expect("present");
    assert_eq!(props.models, vec!["models/props/oildrum.mdl"]);
    assert_eq!(props.leaves, vec![1, 2]);
    assert_eq!(props.props.len(), 1);
    assert_eq!(props.props[0].origin, [1.0, 2.0, 3.0]);

    // Some writers give the sentinel a zero offset instead of the data's
    // end; the game lump's own extent bounds the entry instead.
    let mut zero_sentinel = lump.clone();
    zero_sentinel[4 + 16 + 8..4 + 16 + 12].copy_from_slice(&0i32.to_le_bytes());
    let bytes = build_bsp(&[(lump_ids::GAME_LUMP, zero_sentinel, 0, 0)]);
    let bsp = parse(&bytes, &limits).expect("parse");
    let props = bsp.static_props(&limits).expect("sprp").expect("present");
    assert_eq!(props.models, vec!["models/props/oildrum.mdl"]);

    // No sentinel at all: the same game-lump-end fallback bounds it.
    lump[0..4].copy_from_slice(&1i32.to_le_bytes());
    lump.truncate(4 + 16);
    lump.extend_from_slice(&compressed);
    // Re-point the entry's offset field (the directory is 16 bytes
    // shorter now; the field sits past id, flags, and version).
    lump[4 + 8..4 + 12].copy_from_slice(&((HEADER_BYTES + 4 + 16) as i32).to_le_bytes());
    let bytes = build_bsp(&[(lump_ids::GAME_LUMP, lump, 0, 0)]);
    let bsp = parse(&bytes, &limits).expect("parse");
    let props = bsp.static_props(&limits).expect("sprp").expect("present");
    assert_eq!(props.leaves, vec![1, 2]);
}

#[cfg(feature = "lzma")]
#[test]
fn compressed_lumps_decompress_through_typed_accessors() {
    let vertices = Record::new()
        .vec3([1.0, 2.0, 3.0])
        .vec3([-4.0, -5.0, -6.0])
        .0;
    let compressed = valve_lzma(&vertices);
    let entities = b"{\n\"classname\" \"worldspawn\"\n}\n\0";
    let bytes = build_bsp(&[
        (
            lump_ids::VERTICES,
            compressed.clone(),
            0,
            vertices.len() as i32,
        ),
        (
            lump_ids::ENTITIES,
            valve_lzma(entities),
            0,
            entities.len() as i32,
        ),
        (lump_ids::PAKFILE, valve_lzma(b"junk"), 0, 4),
    ]);
    let limits = Limits::default();
    let bsp = parse(&bytes, &limits).expect("parse");

    // The raw escape hatch stays compressed; lump_data decompresses.
    assert_eq!(bsp.lump(lump_ids::VERTICES), Some(compressed.as_slice()));
    assert_eq!(
        bsp.lump_compression(lump_ids::VERTICES),
        Some(vertices.len() as u32)
    );
    assert!(matches!(
        bsp.lump_data(lump_ids::VERTICES, &limits),
        Ok(Cow::Owned(_))
    ));

    assert_eq!(
        bsp.vertices(&limits).expect("vertices"),
        vec![[1.0, 2.0, 3.0], [-4.0, -5.0, -6.0]]
    );
    let docs = bsp.entities(&limits).expect("entities");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("classname"), Some("worldspawn"));

    // The pakfile reader borrows: a compressed pakfile lump is a loud
    // error, not garbage.
    assert!(matches!(
        bsp.pakfile(),
        Err(BspError::CompressedLump { index }) if index == lump_ids::PAKFILE
    ));
}

#[cfg(not(feature = "lzma"))]
#[test]
fn compressed_lumps_error_without_the_lzma_feature() {
    let bytes = build_bsp(&[(lump_ids::VERTICES, vec![0u8; 24], 0, 12)]);
    let limits = Limits::default();
    let bsp = parse(&bytes, &limits).expect("parse");
    assert!(matches!(
        bsp.vertices(&limits),
        Err(BspError::CompressedLump { index }) if index == lump_ids::VERTICES
    ));
}

fn vis_lump(rows: &[&[u8]], clusters: &[(usize, usize)]) -> Vec<u8> {
    let directory_bytes = 4 + clusters.len() * 8;
    let mut row_offsets = Vec::new();
    let mut at = directory_bytes;
    for row in rows {
        row_offsets.push(at as u32);
        at += row.len();
    }
    let mut b = Vec::new();
    b.extend_from_slice(&(clusters.len() as i32).to_le_bytes());
    for (pvs, pas) in clusters {
        b.extend_from_slice(&row_offsets[*pvs].to_le_bytes());
        b.extend_from_slice(&row_offsets[*pas].to_le_bytes());
    }
    for row in rows {
        b.extend_from_slice(row);
    }
    b
}

#[test]
fn visibility_rows_decompress() {
    // Row 0: clusters 0 and 2. Row 1: skip 8, then cluster 8.
    // Row 2: a zero run length (malformed) terminates cleanly.
    let rows: &[&[u8]] = &[&[0b0000_0101], &[0x00, 0x01, 0b0000_0001], &[0x00, 0x00]];
    let mut clusters = vec![(0usize, 1usize), (1, 0), (2, 2)];
    clusters.resize(10, (0, 0));
    let lump = vis_lump(rows, &clusters);
    let bytes = build_bsp(&[(lump_ids::VISIBILITY, lump, 0, 0)]);
    let limits = Limits::default();
    let bsp = parse(&bytes, &limits).expect("parse");

    let vis = bsp.visibility(&limits).expect("vis").expect("present");
    assert_eq!(vis.cluster_count(), 10);

    let mut expected = vec![false; 10];
    expected[0] = true;
    expected[2] = true;
    // Row 0 is one byte: clusters past its 8 bits stay invisible.
    assert_eq!(vis.pvs(0), Some(expected.clone()));
    assert_eq!(vis.pas(1), Some(expected));

    let mut expected = vec![false; 10];
    expected[8] = true;
    assert_eq!(vis.pvs(1), Some(expected.clone()));
    assert_eq!(vis.pas(0), Some(expected));

    // The zero-run row decodes as nothing visible (and terminates).
    assert_eq!(vis.pvs(2), Some(vec![false; 10]));

    // Out-of-range cluster.
    assert_eq!(vis.pvs(10), None);

    // A row offset past the lump: None, not a panic.
    let mut lump = vis_lump(rows, &clusters);
    lump[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    let bytes = build_bsp(&[(lump_ids::VISIBILITY, lump, 0, 0)]);
    let bsp = parse(&bytes, &limits).expect("parse");
    let vis = bsp.visibility(&limits).expect("vis").expect("present");
    assert_eq!(vis.pvs(0), None);
    assert!(vis.pas(0).is_some());
}

#[test]
fn visibility_absent_and_malformed() {
    let limits = Limits::default();
    let bytes = build_bsp(&[]);
    assert_eq!(
        parse(&bytes, &limits).expect("parse").visibility(&limits),
        Ok(None)
    );

    // A directory that declares more clusters than the lump holds.
    let bytes = build_bsp(&[(lump_ids::VISIBILITY, 64i32.to_le_bytes().to_vec(), 0, 0)]);
    assert!(matches!(
        parse(&bytes, &limits).expect("parse").visibility(&limits),
        Err(BspError::Decode { .. })
    ));
}

#[test]
fn visibility_into_owned_detaches_from_the_source_bytes() {
    let rows: &[&[u8]] = &[&[0b0000_0011]];
    let clusters = vec![(0usize, 0usize), (0, 0)];
    let lump = vis_lump(rows, &clusters);
    let lump_len = lump.len();
    let limits = Limits::default();

    let owned = {
        let bytes = build_bsp(&[(lump_ids::VISIBILITY, lump, 0, 0)]);
        let bsp = parse(&bytes, &limits).expect("parse");
        let vis = bsp.visibility(&limits).expect("vis").expect("present");
        vis.into_owned()
        // `bytes` and `bsp` drop here; `owned` must not borrow from them.
    };

    assert_eq!(owned.cluster_count(), 2);
    assert_eq!(owned.pvs(0), Some(vec![true, true]));
    assert_eq!(owned.lump_len(), lump_len);
}
