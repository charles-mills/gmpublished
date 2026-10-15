use vformats::{
    Limits,
    bsp::{self, WorldLightError, WorldLightKind, lump_ids},
};

fn fixture(ldr: &[u8], hdr: &[u8], version: i32) -> Vec<u8> {
    let mut bytes = vec![0; 1036];
    bytes[..4].copy_from_slice(b"VBSP");
    bytes[4..8].copy_from_slice(&20_i32.to_le_bytes());
    for (lump, data) in [
        (lump_ids::WORLDLIGHTS, ldr),
        (lump_ids::WORLDLIGHTS_HDR, hdr),
    ] {
        let at = 8 + lump * 16;
        let offset = bytes.len() as u32;
        bytes[at..at + 4].copy_from_slice(&offset.to_le_bytes());
        bytes[at + 4..at + 8].copy_from_slice(&(data.len() as u32).to_le_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&version.to_le_bytes());
        bytes.extend_from_slice(data);
    }
    bytes
}

fn record(kind: i32) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [-17.0_f32, 23.0, 31.0, 0.25, 1.5, 4.0, 0.0, -1.0, 0.0] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for value in [-1_i32, kind, 33] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for value in [0.9_f32, 0.4, 2.0, 512.0, 1.0, 0.02, 0.003] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for value in [i32::MIN + 1, -1, 77] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    assert_eq!(bytes.len(), 88);
    bytes
}

#[test]
fn all_emitter_types_fields_channel_order_and_unknown_bits_are_preserved() {
    let ldr = (0..7).map(record).collect::<Vec<_>>().concat();
    let bytes = fixture(&ldr, &record(-9), 0);
    let bsp = bsp::parse(&bytes, &Limits::default()).unwrap();
    let rows = bsp.world_lights(&Limits::default()).unwrap();
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.original_index, index);
        assert_eq!(row.kind.raw(), index as i32);
        assert_eq!(row.origin, [-17.0, 23.0, 31.0]);
        assert_eq!(row.intensity, [0.25, 1.5, 4.0]);
        assert_eq!(row.normal, [0.0, -1.0, 0.0]);
        assert_eq!(
            (row.cluster, row.style, row.texinfo, row.owner),
            (-1, 33, -1, 77)
        );
        assert_eq!(row.flags, 0x8000_0001);
        assert_eq!(
            [
                row.stopdot,
                row.stopdot2,
                row.exponent,
                row.radius,
                row.constant_attenuation,
                row.linear_attenuation,
                row.quadratic_attenuation
            ],
            [0.9, 0.4, 2.0, 512.0, 1.0, 0.02, 0.003]
        );
    }
    assert_eq!(rows[6].kind, WorldLightKind::Unknown(6));
    assert_eq!(
        bsp.world_lights_hdr(&Limits::default()).unwrap()[0].kind,
        WorldLightKind::Unknown(-9)
    );
}

#[test]
fn partial_records_limits_and_unknown_layouts_are_rejected_before_allocation() {
    for length in [1, 87, 89, 175] {
        let bytes = fixture(&vec![0; length], &[], 0);
        assert!(
            matches!(bsp::parse(&bytes,&Limits::default()).unwrap().world_lights(&Limits::default()),Err(WorldLightError::PartialRecord(n)) if n==length)
        );
    }
    let bytes = fixture(&[record(1), record(2)].concat(), &[], 0);
    let limits = Limits {
        max_entries: 1,
        ..Limits::default()
    };
    assert!(matches!(
        bsp::parse(&bytes, &limits).unwrap().world_lights(&limits),
        Err(WorldLightError::TooManyRecords { count: 2, max: 1 })
    ));
    let limits = Limits {
        max_entry_bytes: 87,
        ..Limits::default()
    };
    assert!(matches!(
        bsp::parse(&bytes, &limits).unwrap().world_lights(&limits),
        Err(WorldLightError::Bsp(bsp::BspError::LumpTooLarge { .. }))
    ));
    let bytes = fixture(&record(1), &[], 1);
    assert_eq!(
        bsp::parse(&bytes, &Limits::default())
            .unwrap()
            .world_lights(&Limits::default()),
        Err(WorldLightError::UnsupportedVersion(1))
    );
}

#[test]
fn every_nonfinite_float_is_diagnosed_with_original_row_and_field() {
    for offset in (0..36).step_by(4).chain((48..76).step_by(4)) {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut payload = [record(1), record(2)].concat();
            payload[88 + offset..88 + offset + 4].copy_from_slice(&value.to_le_bytes());
            let bytes = fixture(&payload, &[], 0);
            assert!(matches!(
                bsp::parse(&bytes, &Limits::default())
                    .unwrap()
                    .world_lights(&Limits::default()),
                Err(WorldLightError::NonFinite { index: 1, .. })
            ));
        }
    }
    let bytes = fixture(&[], &[], 0);
    assert!(
        bsp::parse(&bytes, &Limits::default())
            .unwrap()
            .world_lights(&Limits::default())
            .unwrap()
            .is_empty()
    );
}
