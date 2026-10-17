use super::*;
use crate::Limits;

fn limits() -> Limits {
    Limits::default()
}

struct Fixture {
    minor: u32,
    width: u16,
    height: u16,
    flags: u32,
    frames: u16,
    first_frame: u16,
    format: i32,
    mip_count: u8,
    payload: Vec<u8>,
}

impl Fixture {
    fn new(width: u16, height: u16, format: i32, mip_count: u8) -> Self {
        Self {
            minor: 2,
            width,
            height,
            flags: 0,
            frames: 1,
            first_frame: 0,
            format,
            mip_count,
            payload: Vec::new(),
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let header_size: u32 = match self.minor {
            0 | 1 => 64,
            2 => 80,
            _ => 80, // 65 fields + padding; no resources appended
        };
        let mut b = Vec::new();
        b.extend_from_slice(b"VTF\0");
        b.extend_from_slice(&7u32.to_le_bytes());
        b.extend_from_slice(&self.minor.to_le_bytes());
        b.extend_from_slice(&header_size.to_le_bytes());
        b.extend_from_slice(&self.width.to_le_bytes());
        b.extend_from_slice(&self.height.to_le_bytes());
        b.extend_from_slice(&self.flags.to_le_bytes());
        b.extend_from_slice(&self.frames.to_le_bytes());
        b.extend_from_slice(&self.first_frame.to_le_bytes());
        b.extend_from_slice(&[0; 4]); // padding
        b.extend_from_slice(&[0; 12]); // reflectivity
        b.extend_from_slice(&[0; 4]); // padding
        b.extend_from_slice(&1.0f32.to_le_bytes()); // bumpmap scale
        b.extend_from_slice(&(self.format as u32).to_le_bytes());
        b.push(self.mip_count);
        b.extend_from_slice(&(-1i32 as u32).to_le_bytes()); // no lowres image
        b.push(0); // lowres width
        b.push(0); // lowres height
        if self.minor >= 2 {
            b.extend_from_slice(&1u16.to_le_bytes()); // depth
        }
        if self.minor >= 3 {
            b.extend_from_slice(&[0; 3]); // padding
            b.extend_from_slice(&0u32.to_le_bytes()); // no resources
            b.extend_from_slice(&[0; 8]); // padding
        }
        b.resize(header_size as usize, 0);
        b.extend_from_slice(&self.payload);
        b
    }
}

const RGBA8888: i32 = 0;
const DXT1: i32 = 13;
const DXT3: i32 = 14;
const DXT5: i32 = 15;

#[test]
fn particle_sprite_sheet_selects_and_loops_frames() {
    let mut payload = Vec::new();
    let values = [
        1_u32,
        1,
        3,
        0,
        2,
        1.0_f32.to_bits(),
        0.5_f32.to_bits(),
        0.0_f32.to_bits(),
        0.0_f32.to_bits(),
        0.5_f32.to_bits(),
        1.0_f32.to_bits(),
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0.5_f32.to_bits(),
        0.5_f32.to_bits(),
        0.0_f32.to_bits(),
        1.0_f32.to_bits(),
        1.0_f32.to_bits(),
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    for value in values {
        payload.extend_from_slice(&value.to_le_bytes());
    }

    let sheet = parse_sprite_sheet_payload(&payload, &limits()).expect("sprite sheet should parse");
    let sequence = sheet.sequence(3).expect("sequence 3 should exist");
    assert_eq!(sequence.uv_at(0.1), [0.0, 0.0, 0.5, 1.0]);
    assert_eq!(sequence.uv_at(0.7), [0.5, 0.0, 1.0, 1.0]);
    assert_eq!(sequence.uv_at(1.2), [0.0, 0.0, 0.5, 1.0]);
}

#[test]
fn sprite_sheet_resource_bounds_exclude_headers_and_truncated_blocks() {
    let mut fixture = Fixture::new(1, 1, RGBA8888, 1);
    fixture.minor = 3;
    fixture.payload = vec![255; 4];
    let bytes = fixture.bytes();
    assert!(
        parse(&bytes, &limits())
            .unwrap()
            .sprite_sheet_descriptor()
            .unwrap()
            .is_none()
    );
    let mut bytes = bytes[..80].to_vec();
    bytes[12..16].copy_from_slice(&96_u32.to_le_bytes());
    bytes[68..72].copy_from_slice(&2_u32.to_le_bytes());
    bytes.extend_from_slice(&[0x30, 0, 0, 0]);
    bytes.extend_from_slice(&96_u32.to_le_bytes());
    bytes.extend_from_slice(&[0x10, 0, 0, 0]);
    bytes.extend_from_slice(&100_u32.to_le_bytes());
    bytes.extend_from_slice(&[255; 4]);
    bytes.extend_from_slice(&8_u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 8]);
    let vtf = parse(&bytes, &limits()).unwrap();
    assert_eq!(
        vtf.sprite_sheet_descriptor()
            .unwrap()
            .unwrap()
            .cost()
            .frames,
        0
    );
    bytes[92..96].copy_from_slice(&80_u32.to_le_bytes());
    assert!(matches!(
        parse(&bytes, &limits()).unwrap().sprite_sheet_descriptor(),
        Err(SpriteSheetError::InvalidResource)
    ));
    bytes[92..96].copy_from_slice(&100_u32.to_le_bytes());
    bytes.pop();
    assert!(matches!(
        parse(&bytes, &limits()).unwrap().sprite_sheet_descriptor(),
        Err(SpriteSheetError::InvalidResource)
    ));
}

#[test]
fn rejects_bad_magic_version_format_and_volume() {
    assert!(matches!(
        parse(b"GMAD not a vtf", &limits()),
        Err(VtfError::BadMagic)
    ));

    let mut fx = Fixture::new(4, 4, RGBA8888, 1);
    fx.payload = vec![0; 64];
    let mut v8 = fx.bytes();
    v8[4..8].copy_from_slice(&8u32.to_le_bytes());
    assert!(matches!(
        parse(&v8, &limits()),
        Err(VtfError::UnsupportedVersion { major: 8, .. })
    ));

    let mut unknown = Fixture::new(4, 4, 99, 1);
    unknown.payload = vec![0; 64];
    assert!(matches!(
        parse(&unknown.bytes(), &limits()),
        Err(VtfError::UnknownFormat(99))
    ));

    let mut volume = Fixture::new(4, 4, RGBA8888, 1);
    volume.payload = vec![0; 64];
    let mut bytes = volume.bytes();
    bytes[63..65].copy_from_slice(&4u16.to_le_bytes());
    assert!(matches!(
        parse(&bytes, &limits()),
        Err(VtfError::VolumeTexture { depth: 4 })
    ));

    let tiny = Limits {
        max_input_bytes: 8,
        ..Limits::default()
    };
    assert!(matches!(
        parse(&Fixture::new(4, 4, RGBA8888, 1).bytes(), &tiny),
        Err(VtfError::InputTooLarge { .. })
    ));

    assert!(matches!(
        parse(b"VTF\0\x07", &limits()),
        Err(VtfError::Truncated { .. })
    ));
}

#[test]
fn stored_face_count_follows_source_version_and_preserves_first_frame() {
    for minor in 0..=5 {
        for first_frame in [0, u16::MAX] {
            let mut envmap = Fixture::new(4, 4, RGBA8888, 1);
            envmap.minor = minor;
            envmap.flags = texture_flags::ENVMAP;
            envmap.first_frame = first_frame;
            let expected = match minor {
                1..=4 => 7,
                _ => 6,
            };
            envmap.payload = vec![0; 4 * 4 * 4 * expected];
            let bytes = envmap.bytes();
            let parsed = parse(&bytes, &limits()).unwrap();
            assert_eq!(parsed.face_count(), expected as u32);
            assert_eq!(parsed.first_frame(), first_frame);
            assert!(inspect_header(&bytes, bytes.len() as u64, &limits()).is_ok());
        }
    }
    let plain = Fixture::new(4, 4, RGBA8888, 1);
    assert_eq!(parse(&plain.bytes(), &limits()).unwrap().face_count(), 1);
}

#[test]
fn frame_and_mip_addressing_matches_the_wire_layout() {
    let mut fx = Fixture::new(2, 1, RGBA8888, 2);
    fx.frames = 2;
    let tag = |v: u8| vec![v; 4];
    let mip1_f0 = tag(1); // 1x1
    let mip1_f1 = tag(2);
    let mip0_f0 = [tag(3), tag(4)].concat(); // 2x1
    let mip0_f1 = [tag(5), tag(6)].concat();
    fx.payload = [mip1_f0, mip1_f1, mip0_f0.clone(), mip0_f1.clone()].concat();
    let bytes = fx.bytes();
    let vtf = parse(&bytes, &limits()).unwrap();

    assert_eq!(vtf.decode_rgba(0, 0, 1, &limits()).unwrap().rgba, tag(1));
    assert_eq!(vtf.decode_rgba(1, 0, 1, &limits()).unwrap().rgba, tag(2));
    assert_eq!(vtf.decode_rgba(0, 0, 0, &limits()).unwrap().rgba, mip0_f0);
    assert_eq!(vtf.decode_rgba(1, 0, 0, &limits()).unwrap().rgba, mip0_f1);
    assert!(matches!(
        vtf.decode_rgba(2, 0, 0, &limits()),
        Err(VtfError::OutOfRange)
    ));
    assert!(matches!(
        vtf.decode_rgba(0, 1, 0, &limits()),
        Err(VtfError::OutOfRange)
    ));
    assert!(matches!(
        vtf.decode_rgba(0, 0, 2, &limits()),
        Err(VtfError::OutOfRange)
    ));
}

#[test]
fn cubemap_faces_address_independently() {
    let mut fx = Fixture::new(1, 1, RGBA8888, 1);
    fx.flags = texture_flags::ENVMAP;
    fx.minor = 5;
    fx.first_frame = 0xFFFF;
    fx.payload = (0..6u8).flat_map(|f| [f, f, f, 255]).collect();
    let bytes = fx.bytes();
    let vtf = parse(&bytes, &limits()).unwrap();
    for face in 0..6 {
        assert_eq!(
            vtf.decode_rgba(0, face, 0, &limits()).unwrap().rgba,
            vec![face as u8, face as u8, face as u8, 255],
            "face {face}"
        );
    }
}

#[test]
fn raw_bc_exposes_dxt1_mips_in_file_order() {
    let mut fx = Fixture::new(8, 4, DXT1, 3);
    fx.minor = 1;
    fx.payload = [vec![1u8; 8], vec![2u8; 8], vec![3u8; 16]].concat();
    let bytes = fx.bytes();
    let vtf = parse(&bytes, &limits()).unwrap();
    let texture = vtf.raw_bc(0, 0).expect("BC eligible");

    assert_eq!(texture.format, BcFormat::Bc1);
    assert_eq!((texture.width, texture.height), (8, 4));
    assert_eq!(
        texture
            .mips
            .iter()
            .map(|mip| (mip.width, mip.height, mip.data[0]))
            .collect::<Vec<_>>(),
        vec![(2, 1, 1), (4, 2, 2), (8, 4, 3)]
    );
}

#[test]
fn raw_bc_stops_before_truncated_dxt5_mip() {
    let mut fx = Fixture::new(4, 4, DXT5, 2);
    fx.payload = [vec![4u8; 16], vec![5u8; 11]].concat();
    let bytes = fx.bytes();
    let vtf = parse(&bytes, &limits()).unwrap();
    let texture = vtf.raw_bc(0, 0).expect("first mip is complete");

    assert_eq!(texture.format, BcFormat::Bc3);
    assert_eq!(texture.mips.len(), 1);
    assert_eq!((texture.mips[0].width, texture.mips[0].height), (2, 2));
    assert_eq!(texture.mips[0].data, vec![4u8; 16]);
}

#[test]
fn decode_rejects_a_crafted_bc1_header_before_allocating() {
    let mut fx = Fixture::new(64, 64, DXT1, 1);
    fx.payload = vec![0u8; 2048];
    let bytes = fx.bytes();
    let vtf = parse(&bytes, &limits()).unwrap();
    let tight = Limits {
        max_entry_bytes: 1024,
        ..Limits::default()
    };
    assert_eq!(
        vtf.decode_rgba(0, 0, 0, &tight),
        Err(VtfError::DecodedTooLarge {
            size: 64 * 64 * 4,
            max: 1024,
        })
    );
}

#[test]
fn raw_bc_with_no_complete_mips_is_none() {
    let mut fx = Fixture::new(4, 4, DXT1, 1);
    fx.payload = vec![7u8; 6];
    let bytes = fx.bytes();
    let vtf = parse(&bytes, &limits()).unwrap();
    assert!(vtf.raw_bc(0, 0).is_none());
    assert!(vtf.raw_bc(1, 0).is_none(), "out-of-range frame");
}

#[test]
fn standalone_bc_decode_matches_container_pixels_and_endpoint_expansion() {
    let colour = [0x00, 0x18, 0x00, 0x00, 0, 0, 0, 0];
    for (format, encoded_format, alpha, expected_alpha) in [
        (BcFormat::Bc1, DXT1, Vec::new(), 255),
        (BcFormat::Bc2, DXT3, vec![0xaa; 8], 170),
        (BcFormat::Bc3, DXT5, vec![123, 0, 0, 0, 0, 0, 0, 0], 123),
    ] {
        let mut payload = alpha;
        payload.extend_from_slice(&colour);
        let mut fixture = Fixture::new(3, 2, encoded_format, 1);
        fixture.payload = payload.clone();
        let bytes = fixture.bytes();
        let expected = parse(&bytes, &limits())
            .unwrap()
            .decode_rgba(0, 0, 0, &limits())
            .unwrap();
        let decoded = decode_bc_rgba(format, 3, 2, &payload, &limits()).unwrap();
        assert_eq!(decoded, expected);
        assert_eq!(decoded.rgba, [24, 0, 0, expected_alpha].repeat(6));
    }
}

#[test]
fn standalone_bc_decode_requires_exact_block_payload() {
    assert_eq!(
        decode_bc_rgba(BcFormat::Bc1, 5, 3, &[0; 8], &limits()),
        Err(VtfError::Truncated {
            needed: 16,
            available: 8,
        })
    );
    assert_eq!(
        decode_bc_rgba(BcFormat::Bc1, 1, 1, &[0; 9], &limits()),
        Err(VtfError::ImageLengthMismatch {
            expected: 8,
            actual: 9,
        })
    );
    assert_eq!(
        decode_bc_rgba(BcFormat::Bc3, 0, 1, &[], &limits()),
        Err(VtfError::BadHeader)
    );
    assert_eq!(
        decode_bc_rgba(BcFormat::Bc3, u32::MAX, u32::MAX, &[], &limits()),
        Err(VtfError::BadHeader)
    );
}

#[test]
fn standalone_bc_decode_honours_encoded_and_decoded_limits() {
    let input_limit = Limits {
        max_input_bytes: 7,
        ..limits()
    };
    assert_eq!(
        decode_bc_rgba(BcFormat::Bc1, 1, 1, &[0; 8], &input_limit),
        Err(VtfError::InputTooLarge { len: 8, max: 7 })
    );
    let decoded_limit = Limits {
        max_entry_bytes: 15,
        ..limits()
    };
    assert_eq!(
        decode_bc_rgba(BcFormat::Bc1, 2, 2, &[0; 8], &decoded_limit),
        Err(VtfError::DecodedTooLarge { size: 16, max: 15 })
    );
}

fn decode_one(format: i32, payload: &[u8]) -> Vec<u8> {
    let mut fx = Fixture::new(1, 1, format, 1);
    fx.payload = payload.to_vec();
    let bytes = fx.bytes();
    parse(&bytes, &limits())
        .unwrap()
        .decode_rgba(0, 0, 0, &limits())
        .unwrap()
        .rgba
}

#[test]
#[allow(clippy::unusual_byte_groupings)]
fn swizzle_and_packed_formats_decode_single_pixels() {
    assert_eq!(decode_one(0, &[1, 2, 3, 4]), [1, 2, 3, 4]); // RGBA8888
    assert_eq!(decode_one(1, &[4, 3, 2, 1]), [1, 2, 3, 4]); // ABGR8888
    assert_eq!(decode_one(11, &[4, 1, 2, 3]), [1, 2, 3, 4]); // ARGB8888
    assert_eq!(decode_one(12, &[3, 2, 1, 4]), [1, 2, 3, 4]); // BGRA8888
    assert_eq!(decode_one(16, &[3, 2, 1, 9]), [1, 2, 3, 255]); // BGRX8888
    assert_eq!(decode_one(2, &[1, 2, 3]), [1, 2, 3, 255]); // RGB888
    assert_eq!(decode_one(3, &[3, 2, 1]), [1, 2, 3, 255]); // BGR888
    assert_eq!(decode_one(5, &[7]), [7, 7, 7, 255]); // I8
    assert_eq!(decode_one(8, &[9]), [0, 0, 0, 9]); // A8
    assert_eq!(decode_one(6, &[7, 9]), [7, 7, 7, 9]); // IA88
    assert_eq!(decode_one(22, &[7, 9]), [7, 9, 0, 255]); // UV88

    assert_eq!(decode_one(9, &[0, 0, 255]), [0, 0, 255, 0]);
    assert_eq!(decode_one(9, &[1, 0, 255]), [1, 0, 255, 255]);
    assert_eq!(decode_one(10, &[255, 0, 0]), [0, 0, 255, 0]);

    let rgb565 = 0b00111_000011_00001u16.to_le_bytes();
    assert_eq!(
        decode_one(4, &rgb565),
        [expand5(0b00001), expand6(0b000011), expand5(0b00111), 255]
    );
    let bgr565 = 0b00111_000011_00001u16.to_le_bytes();
    assert_eq!(
        decode_one(17, &bgr565),
        [expand5(0b00111), expand6(0b000011), expand5(0b00001), 255]
    );
    let bgra4444 = 0x4321u16.to_le_bytes();
    assert_eq!(
        decode_one(19, &bgra4444),
        [expand4(3), expand4(2), expand4(1), expand4(4)]
    );
    let opaque = (0x8000u16 | (1 << 10) | (2 << 5) | 3).to_le_bytes();
    assert_eq!(
        decode_one(21, &opaque),
        [expand5(1), expand5(2), expand5(3), 255]
    );
    let transparent = ((1u16 << 10) | (2 << 5) | 3).to_le_bytes();
    assert_eq!(decode_one(21, &transparent)[3], 0);
    assert_eq!(decode_one(18, &transparent)[3], 255); // BGRX5551 ignores the bit

    let deep: Vec<u8> = [0x0100u16, 0x8000, 0xFF00, 0xFFFF]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    assert_eq!(decode_one(25, &deep), [1, 128, 255, 255]);
    let halves: Vec<u8> = [0x3C00u16, 0x0000, 0x3800, 0x4000] // 1.0, 0.0, 0.5, 2.0
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    assert_eq!(decode_one(24, &halves), [255, 0, 128, 255]);

    let mut fx = Fixture::new(1, 1, 7, 1);
    fx.payload = vec![0];
    let p8_bytes = fx.bytes();
    assert!(matches!(
        parse(&p8_bytes, &limits())
            .unwrap()
            .decode_rgba(0, 0, 0, &limits()),
        Err(VtfError::UnsupportedFormat(VtfFormat::P8))
    ));
}

#[test]
fn bc_blocks_decode_including_edge_clamping_and_alpha_modes() {
    let c0 = 0xF800u16; // pure red
    let c1 = 0x001Fu16; // pure blue
    let mut block = Vec::new();
    block.extend_from_slice(&c0.to_le_bytes());
    block.extend_from_slice(&c1.to_le_bytes());
    block.extend_from_slice(&0u32.to_le_bytes());
    let rgba = decode_bc(BcFormat::Bc1, &block, 4, 4);
    assert_eq!(&rgba[..4], &[255, 0, 0, 255]);

    let mut block3 = Vec::new();
    block3.extend_from_slice(&c1.to_le_bytes());
    block3.extend_from_slice(&c0.to_le_bytes());
    block3.extend_from_slice(&u32::MAX.to_le_bytes()); // all texels index 3
    let rgba = decode_bc(BcFormat::Bc1, &block3, 4, 4);
    assert_eq!(&rgba[..4], &[0, 0, 0, 0]);

    let rgba = decode_bc(BcFormat::Bc1, &block, 2, 2);
    assert_eq!(rgba.len(), 2 * 2 * 4);
    assert_eq!(&rgba[..4], &[255, 0, 0, 255]);

    let mut bc2 = vec![0x21, 0xF0]; // texels: a=0x2, 0x1, 0x0, 0xF ...
    bc2.resize(8, 0);
    bc2.extend_from_slice(&c0.to_le_bytes());
    bc2.extend_from_slice(&c1.to_le_bytes());
    bc2.extend_from_slice(&0u32.to_le_bytes());
    let rgba = decode_bc(BcFormat::Bc2, &bc2, 4, 4);
    assert_eq!(rgba[3], expand4(1)); // texel 0 alpha = low nibble of byte 0
    assert_eq!(rgba[7], expand4(2));
    assert_eq!(rgba[11], expand4(0));
    assert_eq!(rgba[15], expand4(0xF));

    let mut bc3 = vec![200u8, 40];
    bc3.resize(8, 0);
    bc3.extend_from_slice(&c0.to_le_bytes());
    bc3.extend_from_slice(&c1.to_le_bytes());
    bc3.extend_from_slice(&0u32.to_le_bytes());
    let rgba = decode_bc(BcFormat::Bc3, &bc3, 4, 4);
    assert_eq!(rgba[3], 200);
    let mut bits = 0u64;
    for i in 0..16 {
        bits |= 0b111 << (i * 3);
    }
    let mut bc3s = vec![40u8, 200];
    bc3s.extend_from_slice(&bits.to_le_bytes()[..6]);
    bc3s.extend_from_slice(&c0.to_le_bytes());
    bc3s.extend_from_slice(&c1.to_le_bytes());
    bc3s.extend_from_slice(&0u32.to_le_bytes());
    let rgba = decode_bc(BcFormat::Bc3, &bc3s, 4, 4);
    assert_eq!(rgba[3], 255);
}

fn gradient(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[
                (x * 37 % 256) as u8,
                (y * 91 % 256) as u8,
                ((x + y) * 53 % 256) as u8,
                255,
            ]);
        }
    }
    rgba
}

#[test]
fn decode_matches_the_vtf_crate_on_its_supported_formats() {
    let dxt5_golden = include_bytes!("../tests/fixtures/gradient_16x8_dxt5.rgba.bin");
    for (bytes, expected, label) in [
        (
            &include_bytes!("../tests/fixtures/gradient_16x8_rgba8888.vtf")[..],
            gradient(16, 8),
            "rgba8888",
        ),
        (
            &include_bytes!("../tests/fixtures/gradient_16x8_rgb888.vtf")[..],
            gradient(16, 8),
            "rgb888",
        ),
        (
            &include_bytes!("../tests/fixtures/gradient_16x8_dxt5.vtf")[..],
            dxt5_golden.to_vec(),
            "dxt5",
        ),
    ] {
        let ours = parse(bytes, &limits())
            .expect("our parse")
            .decode_rgba(0, 0, 0, &limits())
            .expect("our decode");
        assert_eq!(ours.width, 16, "{label} width");
        assert_eq!(ours.height, 8, "{label} height");
        assert_eq!(ours.rgba, expected, "{label} pixels");
    }
}

#[test]
fn full_mip_chain_matches_the_vtf_crate_frame_size_walk() {
    let bytes = include_bytes!("../tests/fixtures/gradient_32x16_dxt5.vtf");
    let vtf = parse(bytes, &limits()).expect("parse");
    let texture = vtf.raw_bc(0, 0).expect("bc");
    assert_eq!(texture.mips.len(), usize::from(vtf.mip_count()));
    let largest = texture.mips.last().expect("mips");
    assert_eq!((largest.width, largest.height), (32, 16));
    let end = largest.data.as_ptr() as usize + largest.data.len();
    assert_eq!(end, bytes.as_ptr() as usize + bytes.len());
}

#[test]
fn checked_descriptors_preserve_versions_frames_faces_and_authored_levels() {
    for minor in 0..=5 {
        for sentinel in [false, true] {
            let mut fixture = Fixture::new(4, 2, RGBA8888, 3);
            fixture.minor = minor;
            fixture.frames = 2;
            fixture.flags = texture_flags::ENVMAP;
            fixture.first_frame = if sentinel { u16::MAX } else { 0 };
            let faces = if (1..=4).contains(&minor) { 7 } else { 6 };
            for mip in (0..3).rev() {
                for frame in 0..2 {
                    for face in 0..faces {
                        let tag = [mip, frame, face, 127];
                        fixture.payload.extend(
                            tag.repeat(((4_u32 >> mip).max(1) * (2_u32 >> mip).max(1)) as usize),
                        );
                    }
                }
            }
            let bytes = fixture.bytes();
            let parsed = parse(&bytes, &limits()).unwrap();
            assert_eq!(parsed.first_frame(), fixture.first_frame);
            for frame in 0..2 {
                for face in 0..u32::from(faces) {
                    let images = parsed.stored_mip_chain(frame, face, 1).unwrap();
                    assert_eq!(
                        images
                            .iter()
                            .map(|image| image.source_mip)
                            .collect::<Vec<_>>(),
                        vec![1, 2]
                    );
                    for image in images {
                        assert_eq!(
                            &bytes[image.byte_range][..4],
                            &[image.source_mip, frame as u8, face as u8, 127]
                        );
                    }
                    let decoded = parsed.decode_mip_chain(frame, face, 0, &limits()).unwrap();
                    assert_eq!(
                        decoded
                            .iter()
                            .map(|image| image.source_mip)
                            .collect::<Vec<_>>(),
                        vec![0, 1, 2]
                    );
                    for mip in decoded {
                        assert_eq!(
                            mip.image.rgba[..4],
                            [mip.source_mip, frame as u8, face as u8, 127]
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn checked_chain_rejects_truncation_without_discarding_complete_lower_levels() {
    let mut fixture = Fixture::new(4, 4, RGBA8888, 3);
    fixture.payload = vec![42; 4 + 16 + 64 - 1];
    let bytes = fixture.bytes();
    let parsed = parse(&bytes, &limits()).unwrap();
    assert!(matches!(
        parsed.stored_image(0, 0, 0),
        Err(VtfError::Truncated {
            needed: 164,
            available: 163
        })
    ));
    assert_eq!(parsed.stored_mip_chain(0, 0, 1).unwrap().len(), 2);
    assert!(matches!(
        parsed.decode_mip_chain(0, 0, 0, &limits()),
        Err(VtfError::Truncated { .. })
    ));
    assert!(matches!(
        parsed.stored_mip_chain(0, 0, 3),
        Err(VtfError::OutOfRange)
    ));
    assert!(matches!(
        parsed.stored_mip_chain(1, 0, 1),
        Err(VtfError::OutOfRange)
    ));
    let small = Limits {
        max_entry_bytes: 19,
        ..limits()
    };
    assert_eq!(
        parsed.decode_mip_chain(0, 0, 1, &small),
        Err(VtfError::DecodedTooLarge { size: 20, max: 19 })
    );
}

#[test]
fn stored_hdr_descriptors_preserve_raw_half_float_payload() {
    let payload = [0x00, 0x3c, 0x00, 0x40, 0x00, 0x42, 0x00, 0x3c];
    let mut fixture = Fixture::new(1, 1, 24, 1);
    fixture.payload = payload.to_vec();
    let bytes = fixture.bytes();
    let parsed = parse(&bytes, &limits()).unwrap();
    assert_eq!(parsed.format(), VtfFormat::Rgba16161616F);
    let image = parsed.stored_image(0, 0, 0).unwrap();
    assert_eq!(&bytes[image.byte_range], &payload);
}

#[test]
fn stored_images_use_resource_offsets_without_treating_headers_as_pixels() {
    for minor in 3..=5 {
        let mut fixture = Fixture::new(2, 1, RGBA8888, 1);
        fixture.minor = minor;
        fixture.payload = vec![17; 8];
        let mut bytes = fixture.bytes();
        bytes.splice(80..80, [0x30, 0, 0, 0, 104, 0, 0, 0]);
        bytes.splice(88..88, [0; 16]);
        bytes[12..16].copy_from_slice(&88_u32.to_le_bytes());
        bytes[68..72].copy_from_slice(&1_u32.to_le_bytes());
        let parsed = parse(&bytes, &limits()).unwrap();
        assert_eq!(parsed.stored_image(0, 0, 0).unwrap().byte_range, 104..112);
        bytes[84..88].copy_from_slice(&8_u32.to_le_bytes());
        let parsed = parse(&bytes, &limits()).unwrap();
        assert_eq!(parsed.stored_image(0, 0, 0), Err(VtfError::BadHeader));
    }
}

#[test]
fn header_inspection_uses_declared_container_bounds_without_payload_allocation() {
    for minor in 0..=5 {
        let mut fixture = Fixture::new(8, 4, RGBA8888, 2);
        fixture.minor = minor;
        fixture.frames = 2;
        fixture.payload = vec![1; (8 * 4 + 4 * 2) * 4 * 2];
        let bytes = fixture.bytes();
        let header_len = if minor < 2 { 64 } else { 80 };
        let header = inspect_header(&bytes[..header_len], bytes.len() as u64, &limits()).unwrap();
        assert_eq!(header, parse(&bytes, &limits()).unwrap().header());
        assert!(matches!(
            inspect_header(&bytes[..header_len], bytes.len() as u64 - 1, &limits()),
            Err(VtfError::Truncated { .. })
        ));
        assert!(
            parse(&bytes[..header_len], &limits())
                .unwrap()
                .stored_image(0, 0, 0)
                .is_err()
        );
    }
}

#[test]
fn header_inspection_preserves_hdr_and_legacy_cube_layout_and_resource_offsets() {
    let mut fixture = Fixture::new(1, 1, 24, 1);
    fixture.minor = 4;
    fixture.flags = texture_flags::ENVMAP;
    fixture.first_frame = u16::MAX;
    fixture.payload = vec![0; 8 * 7];
    let bytes = fixture.bytes();
    let header = inspect_header(&bytes[..80], bytes.len() as u64, &limits()).unwrap();
    assert_eq!(header.format, VtfFormat::Rgba16161616F);
    assert_eq!(header.faces, 7);
    let mut fixture = Fixture::new(2, 1, RGBA8888, 1);
    fixture.minor = 5;
    fixture.payload = vec![0; 8];
    let mut bytes = fixture.bytes();
    bytes.splice(80..80, [0x30, 0, 0, 0, 104, 0, 0, 0]);
    bytes.splice(88..88, [0; 16]);
    bytes[12..16].copy_from_slice(&88u32.to_le_bytes());
    bytes[68..72].copy_from_slice(&1u32.to_le_bytes());
    assert!(inspect_header(&bytes[..88], 112, &limits()).is_ok());
    assert!(matches!(
        inspect_header(&bytes[..88], 111, &limits()),
        Err(VtfError::Truncated { .. })
    ));
}

#[test]
fn region_decode_matches_full_images_without_touching_padding_or_neighbors() {
    for format in [BcFormat::Bc1, BcFormat::Bc2, BcFormat::Bc3] {
        let dimensions = [7, 9];
        let bytes = (0..2 * 3 * format.block_bytes())
            .map(|index| index as u8)
            .collect::<Vec<_>>();
        let full = decode_bc_rgba(format, dimensions[0], dimensions[1], &bytes, &limits()).unwrap();
        for region in [[0, 0, 7, 9], [1, 3, 5, 5], [6, 8, 1, 1]] {
            let stride = region[2] as usize * 4 + 13;
            let mut output = vec![177; stride * region[3] as usize + 9];
            decode_bc_rgba_region_into(format, dimensions, region, &bytes, &mut output, stride)
                .unwrap();
            for row in 0..region[3] as usize {
                let source =
                    ((row + region[1] as usize) * dimensions[0] as usize + region[0] as usize) * 4;
                let count = region[2] as usize * 4;
                assert_eq!(
                    &output[row * stride..row * stride + count],
                    &full.rgba[source..source + count]
                );
                assert!(
                    output[row * stride + count..(row + 1) * stride]
                        .iter()
                        .all(|&byte| byte == 177)
                );
            }
            assert_eq!(&output[stride * region[3] as usize..], &[177; 9]);
        }
    }
}

#[test]
fn region_decode_validates_all_ranges_before_writing() {
    for (region, stride, bytes) in [
        ([0, 0, 1, 1], 3, vec![0; 8]),
        ([1, 0, 1, 1], 4, vec![0; 8]),
        ([0, 0, 1, 1], 4, vec![0; 7]),
        ([u32::MAX, 0, 2, 1], 4, vec![0; 8]),
    ] {
        let mut output = [191; 16];
        assert!(
            decode_bc_rgba_region_into(BcFormat::Bc1, [1, 1], region, &bytes, &mut output, stride)
                .is_err()
        );
        assert_eq!(output, [191; 16]);
    }
}

#[test]
fn source_legacy_sentinel_cubemap_requires_the_seventh_face_at_every_mip() {
    let mut fixture = Fixture::new(32, 32, RGBA8888, 6);
    fixture.flags = texture_flags::ENVMAP;
    fixture.first_frame = u16::MAX;
    for mip in (0..6_u8).rev() {
        for face in 0..6_u8 {
            fixture
                .payload
                .extend([mip, 0, face, 255].repeat((32_usize >> mip).pow(2)));
        }
    }
    let six = fixture.bytes();
    assert_eq!(six.len(), 32_840);
    assert_eq!(
        inspect_header(&six[..80], six.len() as u64, &limits()),
        Err(VtfError::Truncated {
            needed: 38_300,
            available: 32_840
        })
    );
    fixture.payload.clear();
    fixture.frames = 2;
    for mip in (0..6_u8).rev() {
        for frame in 0..2_u8 {
            for face in 0..7_u8 {
                fixture
                    .payload
                    .extend([mip, frame, face, 255].repeat((32_usize >> mip).pow(2)));
            }
        }
    }
    let seven = fixture.bytes();
    let parsed = parse(&seven, &limits()).unwrap();
    assert_eq!(
        inspect_header(&seven[..80], seven.len() as u64, &limits())
            .unwrap()
            .faces,
        7
    );
    for frame in 0..2 {
        for face in 0..7 {
            for image in parsed.stored_mip_chain(frame, face, 0).unwrap() {
                assert_eq!(
                    &seven[image.byte_range][..4],
                    &[image.source_mip, frame as u8, face as u8, 255]
                );
            }
        }
    }
    assert_eq!(
        parsed.stored_image(0, 0, 0).unwrap().byte_range.start,
        19_176
    );
    assert_eq!(
        parsed.stored_image(1, 5, 0).unwrap().byte_range.start,
        68_328
    );
}
