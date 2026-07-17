use super::*;
use crate::Limits;

fn limits() -> Limits {
    Limits::default()
}

/// Hand-built VTF bytes: exact header layout control, no encoder in the
/// loop. `minor` selects the 7.0/7.1 (63-byte), 7.2 (+depth), or 7.3+
/// (+resource directory) layout; payload is appended verbatim.
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

// -----------------------------------------------------------------
// Header validation
// -----------------------------------------------------------------

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
    // depth field lives right after the 63-byte base header on 7.2
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
fn face_count_follows_the_vtflib_spheremap_rule() {
    let mut envmap = Fixture::new(4, 4, RGBA8888, 1);
    envmap.flags = texture_flags::ENVMAP;
    envmap.first_frame = 0xFFFF;
    envmap.payload = vec![0; 4 * 4 * 4 * 6];
    assert_eq!(parse(&envmap.bytes(), &limits()).unwrap().face_count(), 6);

    envmap.first_frame = 0; // pre-7.5 with a spheremap
    envmap.payload = vec![0; 4 * 4 * 4 * 7];
    assert_eq!(parse(&envmap.bytes(), &limits()).unwrap().face_count(), 7);

    envmap.minor = 5;
    envmap.payload = vec![0; 4 * 4 * 4 * 6];
    assert_eq!(parse(&envmap.bytes(), &limits()).unwrap().face_count(), 6);

    let plain = Fixture::new(4, 4, RGBA8888, 1);
    assert_eq!(parse(&plain.bytes(), &limits()).unwrap().face_count(), 1);
}

// -----------------------------------------------------------------
// Addressing: mips smallest-first, then frame, then face
// -----------------------------------------------------------------

#[test]
fn frame_and_mip_addressing_matches_the_wire_layout() {
    // 2 frames, 2 mips of a 2x1 RGBA8888 texture.
    // File order: mip1(f0), mip1(f1), mip0(f0), mip0(f1).
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
    // 6-face 1x1 envmap, 1 mip: faces are consecutive slices.
    let mut fx = Fixture::new(1, 1, RGBA8888, 1);
    fx.flags = texture_flags::ENVMAP;
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

// -----------------------------------------------------------------
// Raw BC access
// -----------------------------------------------------------------

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
    // BC1 amplifies stored bytes 8x when decoded: 64x64 stores 2048
    // bytes but decodes to 16384. A `max_entry_bytes` between the two
    // must reject the decode without ever building the RGBA buffer.
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

// -----------------------------------------------------------------
// Pixel format decoding
// -----------------------------------------------------------------

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
// Binary literals below group digits by 5-6-5 channel, not nibbles.
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

    // Bluescreen: pure blue is transparent, all else opaque.
    assert_eq!(decode_one(9, &[0, 0, 255]), [0, 0, 255, 0]);
    assert_eq!(decode_one(9, &[1, 0, 255]), [1, 0, 255, 255]);
    assert_eq!(decode_one(10, &[255, 0, 0]), [0, 0, 255, 0]);

    // Packed 16-bit, channels named from the lowest bits up.
    // RGB565: r=0b00001, g=0b000011, b=0b00111 -> 0b00111_000011_00001
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
    // BGRA4444: b=1, g=2, r=3, a=4 packed low-to-high.
    let bgra4444 = 0x4321u16.to_le_bytes();
    assert_eq!(
        decode_one(19, &bgra4444),
        [expand4(3), expand4(2), expand4(1), expand4(4)]
    );
    // BGRA5551: alpha is the top bit.
    let opaque = (0x8000u16 | (1 << 10) | (2 << 5) | 3).to_le_bytes();
    assert_eq!(
        decode_one(21, &opaque),
        [expand5(1), expand5(2), expand5(3), 255]
    );
    let transparent = ((1u16 << 10) | (2 << 5) | 3).to_le_bytes();
    assert_eq!(decode_one(21, &transparent)[3], 0);
    assert_eq!(decode_one(18, &transparent)[3], 255); // BGRX5551 ignores the bit

    // 16-bit integer and half-float RGBA.
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

    // P8 is recognized but not decodable.
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
    // BC1 4-color mode: c0 > c1, all texels index 0.
    let c0 = 0xF800u16; // pure red
    let c1 = 0x001Fu16; // pure blue
    let mut block = Vec::new();
    block.extend_from_slice(&c0.to_le_bytes());
    block.extend_from_slice(&c1.to_le_bytes());
    block.extend_from_slice(&0u32.to_le_bytes());
    let rgba = decode_bc(BcFormat::Bc1, &block, 4, 4);
    assert_eq!(&rgba[..4], &[255, 0, 0, 255]);

    // BC1 3-color mode: c0 <= c1, index 3 is transparent black.
    let mut block3 = Vec::new();
    block3.extend_from_slice(&c1.to_le_bytes());
    block3.extend_from_slice(&c0.to_le_bytes());
    block3.extend_from_slice(&u32::MAX.to_le_bytes()); // all texels index 3
    let rgba = decode_bc(BcFormat::Bc1, &block3, 4, 4);
    assert_eq!(&rgba[..4], &[0, 0, 0, 0]);

    // A 2x2 image still uses a full block; only the top-left quad lands.
    let rgba = decode_bc(BcFormat::Bc1, &block, 2, 2);
    assert_eq!(rgba.len(), 2 * 2 * 4);
    assert_eq!(&rgba[..4], &[255, 0, 0, 255]);

    // BC2 explicit alpha nibbles, low nibble first.
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

    // BC3 interpolated alpha: a0 > a1 -> 7-step ramp; indices all 0.
    let mut bc3 = vec![200u8, 40];
    bc3.resize(8, 0);
    bc3.extend_from_slice(&c0.to_le_bytes());
    bc3.extend_from_slice(&c1.to_le_bytes());
    bc3.extend_from_slice(&0u32.to_le_bytes());
    let rgba = decode_bc(BcFormat::Bc3, &bc3, 4, 4);
    assert_eq!(rgba[3], 200);
    // a0 <= a1 -> 5-step ramp with 0 and 255 sentinels at 6 and 7.
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

// -----------------------------------------------------------------
// Cross-checks against files written by the `vtf` crate. The
// gradient_*.vtf fixtures under tests/fixtures/ were generated once
// by `vtf` v0.4.1 (its `create` encoder over the gradient below) and are
// committed goldens; gradient_16x8_dxt5.rgba.bin is that crate's
// decode of the DXT5 file (DXT5 is lossy, so the source gradient is
// not the expected output).
// -----------------------------------------------------------------

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
        // Rgba8888 and Rgb888 are lossless: the expected decode is the
        // gradient itself (verified against the vtf crate at generation).
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
    // The vtf crate can only decode mip 0, but our raw_bc walk must agree
    // with the offsets its writer produced for every stored mip.
    let bytes = include_bytes!("../tests/fixtures/gradient_32x16_dxt5.vtf");
    let vtf = parse(bytes, &limits()).expect("parse");
    let texture = vtf.raw_bc(0, 0).expect("bc");
    assert_eq!(texture.mips.len(), usize::from(vtf.mip_count()));
    let largest = texture.mips.last().expect("mips");
    assert_eq!((largest.width, largest.height), (32, 16));
    // The largest mip must end exactly at the end of the file.
    let end = largest.data.as_ptr() as usize + largest.data.len();
    assert_eq!(end, bytes.as_ptr() as usize + bytes.len());
}
