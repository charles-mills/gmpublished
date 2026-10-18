use vformats::{
    Limits,
    bsp::{self, BspError, lump_ids},
};

fn fixture(payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 4 + 4 + 64 * 16 + 4];
    bytes[..4].copy_from_slice(b"VBSP");
    bytes[4..8].copy_from_slice(&20_i32.to_le_bytes());
    let offset = bytes.len();
    let entry = 8 + lump_ids::CUBEMAPS * 16;
    bytes[entry..entry + 4].copy_from_slice(&(offset as u32).to_le_bytes());
    bytes[entry + 4..entry + 8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

#[test]
fn cubemap_samples_preserve_integer_origins_order_size_and_padding() {
    let mut payload = Vec::new();
    for (origin, size) in [([-832, 448, -96], 0), ([i32::MAX, i32::MIN, 17], 255)] {
        for coordinate in origin {
            payload.extend_from_slice(&coordinate.to_le_bytes());
        }
        payload.extend_from_slice(&[size, 23, 41, 97]);
    }
    let bytes = fixture(&payload);
    let samples = bsp::parse(&bytes, &Limits::default())
        .unwrap()
        .cubemap_samples(&Limits::default())
        .unwrap();
    assert_eq!(samples[0].origin, [-832, 448, -96]);
    assert_eq!(samples[0].size, 0);
    assert_eq!(samples[1].origin, [i32::MAX, i32::MIN, 17]);
    assert_eq!(samples[1].size, 255);
}

#[test]
fn cubemap_samples_reject_partial_records_and_obey_count_limits() {
    for count in [1, 15, 17, 31] {
        let bytes = fixture(&vec![0; count]);
        assert!(matches!(
            bsp::parse(&bytes, &Limits::default())
                .unwrap()
                .cubemap_samples(&Limits::default()),
            Err(BspError::Decode {
                part: "cubemap samples"
            })
        ));
    }
    let bytes = fixture(&[0; 32]);
    let limits = Limits {
        max_entries: 1,
        ..Limits::default()
    };
    assert!(matches!(
        bsp::parse(&bytes, &limits)
            .unwrap()
            .cubemap_samples(&limits),
        Err(BspError::TooManyRecords {
            part: "cubemap samples",
            max: 1
        })
    ));
    let bytes = fixture(&[]);
    assert!(
        bsp::parse(&bytes, &limits)
            .unwrap()
            .cubemap_samples(&limits)
            .unwrap()
            .is_empty()
    );
}
