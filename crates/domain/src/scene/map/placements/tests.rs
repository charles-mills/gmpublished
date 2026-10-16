use super::*;

fn detail_props(leaves: &[u16]) -> vformats::bsp::DetailProps {
    let mut data = Vec::new();
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&1_u32.to_le_bytes());
    for value in [-8.0_f32, 8.0, 8.0, -8.0, 0.0, 0.0, 1.0, 1.0] {
        data.extend_from_slice(&value.to_le_bytes());
    }
    data.extend_from_slice(&(leaves.len() as u32).to_le_bytes());
    for &leaf in leaves {
        let mut record = [0_u8; 52];
        record[26..28].copy_from_slice(&leaf.to_le_bytes());
        record[44] = DETAIL_PROP_TYPE_SPRITE;
        record[48..52].copy_from_slice(&1.0_f32.to_le_bytes());
        data.extend_from_slice(&record);
    }
    let header_size = 1036;
    let game_size = 20 + data.len();
    let mut bytes = vec![0_u8; header_size];
    bytes[..4].copy_from_slice(b"VBSP");
    bytes[4..8].copy_from_slice(&20_i32.to_le_bytes());
    let entry = 8 + vformats::bsp::lump_ids::GAME_LUMP * 16;
    bytes[entry..entry + 4].copy_from_slice(&(header_size as u32).to_le_bytes());
    bytes[entry + 4..entry + 8].copy_from_slice(&(game_size as u32).to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&vformats::bsp::game_lump_ids::DETAIL_PROPS.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&4_u16.to_le_bytes());
    bytes.extend_from_slice(&((header_size + 20) as u32).to_le_bytes());
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&data);
    let limits = vformats::Limits::default();
    vformats::bsp::parse(&bytes, &limits)
        .unwrap()
        .detail_props(&limits)
        .unwrap()
        .unwrap()
}

#[test]
fn detail_sprites_keep_authored_leaf_identity_when_origins_and_clusters_match() {
    let mut bsp = super::super::tests::brush_collision_bsp("");
    bsp.nodes[0].children = [-1, -1];
    bsp.leaves[0].cluster = 0;
    bsp.leaves[1].cluster = 0;
    bsp.leaves[0].leaf_water_data = -1;
    bsp.leaves[1].leaf_water_data = 0;
    bsp.detail_props = Some(detail_props(&[0, 1]));
    let (sprites, skybox) =
        partitioned_detail_sprite_placements(&bsp, &SkyboxPartition::inactive(false));
    assert!(skybox.is_empty());
    assert_eq!(sprites.len(), 2);
    assert_eq!(sprites[0].origin, sprites[1].origin);
    assert_eq!(sprites[0].visibility, sprites[1].visibility);
    assert_eq!([sprites[0].leaf_index, sprites[1].leaf_index], [0, 1]);
    assert!(!sprites[0].underwater_leaf);
    assert!(sprites[1].underwater_leaf);
    assert_eq!(
        super::super::MapLeafLocator::from_bsp(&bsp).leaf_at(sprites[1].origin),
        Some(0)
    );
}

#[test]
fn detail_fast_eligibility_preserves_raw_source_permutation() {
    let mut baseline = detail_props(&[0]).props[0];
    baseline.orientation = 2;
    assert!(detail_sprite_fast_eligible(&baseline));
    for factor in 0..6 {
        let mut prop = baseline;
        match factor {
            0 => prop.prop_type = 0,
            1 => prop.light_style_count = 1,
            2 => prop.orientation = 1,
            3 => prop.shape_angle = 1,
            4 => prop.shape_size = 1,
            5 => prop.sway_amount = 1,
            _ => unreachable!(),
        }
        assert!(!detail_sprite_fast_eligible(&prop), "factor {factor}");
    }
    let mut unrelated = baseline;
    unrelated.light_styles = 17;
    unrelated.origin = [9.0, 7.0, 5.0];
    unrelated.angles = [10.0, 20.0, 30.0];
    unrelated.scale = 3.0;
    assert!(detail_sprite_fast_eligible(&unrelated));

    let mut props = detail_props(&[0, 0, 0]);
    props.props[0] = baseline;
    props.props[1] = baseline;
    props.props[1].light_style_count = 1;
    props.props[2] = baseline;
    props.props[2].shape_size = 1;
    let mut bsp = super::super::tests::brush_collision_bsp("");
    bsp.detail_props = Some(props);
    let (sprites, skybox) =
        partitioned_detail_sprite_placements(&bsp, &SkyboxPartition::inactive(false));
    assert!(skybox.is_empty());
    assert_eq!(
        sprites
            .iter()
            .map(|sprite| sprite.fast_eligible)
            .collect::<Vec<_>>(),
        [true, false, false]
    );
    assert!(sprites.iter().all(|sprite| sprite.orientation == 2));
}
