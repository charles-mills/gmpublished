//! Mutation sweep over a fully-populated map: every truncation length
//! and every single-byte flip must produce a typed error or valid data
//! from `parse` and every accessor — never a panic. This class of test
//! catches unchecked-slice bugs like a zero-length lump whose garbage
//! offset panics an accessor.

use vformats::Limits;
use vformats::bsp::{Bsp, game_lump_ids, lump_ids, parse};

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

/// A map exercising every parse path: geometry lumps, entities, vis,
/// a game lump with static and detail props, and a STORE pakfile.
fn populated_map() -> Vec<u8> {
    let entities = b"{\n\"classname\" \"worldspawn\"\n}\n\0".to_vec();

    // Visibility: 2 clusters, one RLE row each.
    let mut vis = Vec::new();
    vis.extend_from_slice(&2i32.to_le_bytes());
    let dir_len = 4 + 2 * 8;
    for cluster in 0..2u32 {
        let offset = (dir_len + cluster as usize) as u32;
        vis.extend_from_slice(&offset.to_le_bytes());
        vis.extend_from_slice(&offset.to_le_bytes());
    }
    vis.extend_from_slice(&[0b0000_0011, 0b0000_0001]);

    // Game lump: sprp (one v4 prop) + dprp (one 48-byte prop), with
    // absolute offsets — the game lump is placed after entities and
    // vis and the four geometry lumps below, so compute its file
    // offset from the build order.
    let sprp = {
        let mut b = Vec::new();
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&[0u8; 128]);
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&7u16.to_le_bytes());
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&[0u8; 56]);
        b
    };
    let dprp = {
        let mut b = Vec::new();
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&[0u8; 128]);
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&[0u8; 32]);
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&[0u8; 48]);
        b
    };

    // Pakfile: one STORE entry.
    let pak = {
        let payload = b"$basetexture";
        let path = b"materials/a.vmt";
        let mut b = Vec::new();
        b.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        b.extend_from_slice(&[20, 0, 0, 0, 0, 0]);
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&vformats::crc32_ieee(payload).to_le_bytes());
        b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        b.extend_from_slice(&(path.len() as u16).to_le_bytes());
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(path);
        b.extend_from_slice(payload);
        let directory = b.len();
        b.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        b.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0]);
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&vformats::crc32_ieee(payload).to_le_bytes());
        b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        b.extend_from_slice(&(path.len() as u16).to_le_bytes());
        b.extend_from_slice(&[0; 12]);
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(path);
        let dir_size = b.len() - directory;
        b.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&(dir_size as u32).to_le_bytes());
        b.extend_from_slice(&(directory as u32).to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b
    };

    // Build once without the game lump to learn its offset, then again
    // with the directory pointing at the real location.
    let geometry: Vec<(usize, Vec<u8>, i32)> = vec![
        (lump_ids::ENTITIES, entities, 0),
        (lump_ids::VERTICES, vec![0u8; 24], 0),
        (lump_ids::PLANES, vec![0u8; 20], 0),
        (lump_ids::FACES, vec![0u8; 56], 0),
        (lump_ids::LEAFS, vec![0u8; 32], 1),
        (lump_ids::TEXDATA_STRING_TABLE, vec![0u8; 4], 0),
        (lump_ids::TEXDATA_STRING_DATA, b"tools/nodraw\0".to_vec(), 0),
        (lump_ids::OVERLAYS, vec![0u8; 352], 0),
        (lump_ids::VISIBILITY, vis, 0),
        (lump_ids::PAKFILE, pak, 0),
    ];
    let probe = build_bsp(&geometry);
    let game_lump_offset = probe.len();
    let mut game = Vec::new();
    game.extend_from_slice(&2i32.to_le_bytes());
    let payloads_at = game_lump_offset + 4 + 2 * 16;
    for (id, data_at, len) in [
        (game_lump_ids::STATIC_PROPS, payloads_at, sprp.len()),
        (
            game_lump_ids::DETAIL_PROPS,
            payloads_at + sprp.len(),
            dprp.len(),
        ),
    ] {
        game.extend_from_slice(&id.to_le_bytes());
        game.extend_from_slice(&0u16.to_le_bytes());
        game.extend_from_slice(&4u16.to_le_bytes());
        game.extend_from_slice(&(data_at as i32).to_le_bytes());
        game.extend_from_slice(&(len as i32).to_le_bytes());
    }
    game.extend_from_slice(&sprp);
    game.extend_from_slice(&dprp);

    let mut lumps = geometry;
    lumps.push((lump_ids::GAME_LUMP, game, 0));
    build_bsp(&lumps)
}

/// Exercise every accessor; results are irrelevant, panics are not.
fn exercise(bsp: &Bsp<'_>, limits: &Limits) {
    let _ = bsp.entities(limits);
    let _ = bsp.vertices(limits);
    let _ = bsp.planes(limits);
    let _ = bsp.edges(limits);
    let _ = bsp.surfedges(limits);
    let _ = bsp.faces(limits);
    let _ = bsp.texinfos(limits);
    let _ = bsp.texdatas(limits);
    let _ = bsp.texdata_strings(limits);
    let _ = bsp.models(limits);
    let _ = bsp.brushes(limits);
    let _ = bsp.brush_sides(limits);
    let _ = bsp.nodes(limits);
    let _ = bsp.leafs(limits);
    let _ = bsp.leaf_faces(limits);
    let _ = bsp.leaf_brushes(limits);
    let _ = bsp.displacement_infos(limits);
    let _ = bsp.displacement_verts(limits);
    let _ = bsp.lighting(limits);
    let _ = bsp.lighting_hdr(limits);
    let _ = bsp.leaf_ambient_lighting(limits);
    let _ = bsp.leaf_ambient_indices(limits);
    let _ = bsp.overlays(limits);
    let _ = bsp.game_lumps(limits);
    let _ = bsp.static_props(limits);
    let _ = bsp.detail_props(limits);
    if let Ok(Some(vis)) = bsp.visibility(limits) {
        for cluster in 0..vis.cluster_count().min(4) {
            let _ = vis.pvs(cluster);
            let _ = vis.pas(cluster);
        }
    }
    if let Ok(pak) = bsp.pakfile() {
        for entry in pak.entries().iter().take(4) {
            let _ = entry.path_is_unsafe();
            let _ = pak.entry_bytes(entry, limits);
        }
    }
    for index in 0..64 {
        let _ = bsp.lump_data(index, limits);
    }
}

#[test]
fn every_truncation_and_byte_flip_is_panic_free() {
    let map = populated_map();
    let limits = Limits::default();

    // The pristine fixture must actually exercise the deep paths.
    let bsp = parse(&map, &limits).expect("fixture parses");
    assert_eq!(
        bsp.static_props(&limits)
            .expect("sprp")
            .expect("present")
            .props
            .len(),
        1
    );
    assert!(bsp.detail_props(&limits).expect("dprp").is_some());
    assert!(bsp.visibility(&limits).expect("vis").is_some());
    assert_eq!(bsp.pakfile().expect("pak").entries().len(), 1);
    exercise(&bsp, &limits);

    for cut in 0..map.len() {
        if let Ok(bsp) = parse(&map[..cut], &limits) {
            exercise(&bsp, &limits);
        }
    }
    for at in 0..map.len() {
        for flip in [0x01, 0x80, 0xFF] {
            let mut mutated = map.clone();
            mutated[at] ^= flip;
            if let Ok(bsp) = parse(&mutated, &limits) {
                exercise(&bsp, &limits);
            }
        }
    }
}
