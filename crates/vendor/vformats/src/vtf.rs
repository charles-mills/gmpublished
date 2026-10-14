//! VTF texture containers (versions 7.0–7.5).
//!
//! [`parse`] validates the header strictly; pixel data stays borrowed
//! and is bounds-checked per request, so a truncated file still serves
//! the mips it actually contains.
//!
//! Header field layout follows the `vtf` crate (MIT, © icewind1991),
//! with image addressing corrected to VTFLib semantics — the `vtf`
//! crate's `get_offset` strides frames and faces interchangeably
//! (`frame + face`), which mis-addresses cubemap frames; the layout is
//! `mip (smallest first) → frame → face → z-slice`, and envmaps have 7
//! faces (spheremap included) when the version is below 7.5 and
//! `first_frame != 0xFFFF`, else 6.
//!
//! Packed 16-bit formats name their channels from the lowest bits up
//! (`Bgr565`: blue in bits 0–4), matching Valve's byte-order naming for
//! the 8-bit formats (`Bgra8888`: blue in byte 0).

use std::fmt;

use crate::Limits;
use crate::math::half_to_f32;

/// Texture flag bits (the [`Vtf::flags`] word carries the full engine
/// set; these are the commonly consumed ones, per VTFLib).
#[allow(missing_docs)]
pub mod texture_flags {
    pub const POINTSAMPLE: u32 = 0x0000_0001;
    pub const TRILINEAR: u32 = 0x0000_0002;
    pub const CLAMPS: u32 = 0x0000_0004;
    pub const CLAMPT: u32 = 0x0000_0008;
    pub const ANISOTROPIC: u32 = 0x0000_0010;
    pub const HINT_DXT5: u32 = 0x0000_0020;
    pub const NORMAL: u32 = 0x0000_0080;
    pub const NOMIP: u32 = 0x0000_0100;
    pub const NOLOD: u32 = 0x0000_0200;
    pub const PROCEDURAL: u32 = 0x0000_0800;
    pub const ONEBITALPHA: u32 = 0x0000_1000;
    pub const EIGHTBITALPHA: u32 = 0x0000_2000;
    pub const ENVMAP: u32 = 0x0000_4000;
    pub const RENDERTARGET: u32 = 0x0000_8000;
    pub const NODEPTHBUFFER: u32 = 0x0080_0000;
}

const SIGNATURE: &[u8; 4] = b"VTF\0";
/// Fields through `lowres_image_height` (7.0/7.1 layout).
const BASE_HEADER_BYTES: usize = 63;
/// A sane bound on the 7.3+ resource directory (VTFLib caps at 32).
const MAX_RESOURCES: u32 = 4096;

/// A parsed VTF: validated header plus borrowed image data.
#[derive(Clone, Debug, PartialEq)]
pub struct Vtf<'a> {
    width: u32,
    height: u32,
    flags: u32,
    frames: u32,
    faces: u32,
    mip_count: u8,
    format: VtfFormat,
    version_minor: u32,
    reflectivity: [f32; 3],
    bumpmap_scale: f32,
    highres_offset: usize,
    sprite_sheet_offset: Option<usize>,
    bytes: &'a [u8],
}

/// One frame/face/mip decoded to tightly-packed RGBA8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// `width * height * 4` bytes, row-major RGBA.
    pub rgba: Vec<u8>,
}

/// GPU block-compression format of a BC-encoded VTF.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BcFormat {
    /// DXT1 (with or without one-bit alpha).
    Bc1,
    /// DXT3.
    Bc2,
    /// DXT5.
    Bc3,
}

impl BcFormat {
    /// Bytes per 4x4 block.
    #[must_use]
    pub const fn block_bytes(self) -> u32 {
        match self {
            Self::Bc1 => 8,
            Self::Bc2 | Self::Bc3 => 16,
        }
    }
}

/// One mip level of a BC texture, as stored (GPU-uploadable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawBcMip<'a> {
    /// Pixel width of this mip.
    pub width: u32,
    /// Pixel height of this mip.
    pub height: u32,
    /// The raw BC blocks.
    pub data: &'a [u8],
}

/// Borrowed BC block data for one frame/face across mip levels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawBcTexture<'a> {
    /// Block-compression format.
    pub format: BcFormat,
    /// Base (mip 0) width.
    pub width: u32,
    /// Base (mip 0) height.
    pub height: u32,
    /// Whether truncation cut mips the header declares: `mips` holds
    /// fewer levels than [`Vtf::mip_count`].
    pub truncated: bool,
    /// VTF stores high-resolution mips smallest-first. These slices
    /// preserve that file order; upload callers reverse them for wgpu.
    pub mips: Vec<RawBcMip<'a>>,
}

/// One animation frame from a particle sprite sheet resource.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SheetFrame {
    /// Duration in sequence-relative time units.
    pub display_time: f32,
    /// Atlas rectangle as `[u_min, v_min, u_max, v_max]`.
    pub uv: [f32; 4],
}

/// One numbered animation sequence from a particle sprite sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetSequence {
    /// Sequence number referenced by the particle renderer.
    pub number: u32,
    /// Whether playback holds the last frame instead of looping.
    pub clamp: bool,
    /// Sum of the frame durations.
    pub total_time: f32,
    /// Frames in playback order.
    pub frames: Vec<SheetFrame>,
}

/// Particle sprite-sheet metadata stored in a VTF 7.3+ resource block.
#[derive(Clone, Debug, PartialEq)]
pub struct SpriteSheet {
    /// Animation sequences in file order.
    pub sequences: Vec<SheetSequence>,
}

impl SpriteSheet {
    /// Resolve a particle sequence number, using Source's modulo fallback.
    #[must_use]
    pub fn sequence(&self, number: i32) -> Option<&SheetSequence> {
        if self.sequences.is_empty() {
            return None;
        }
        let number = u32::try_from(number.max(0)).expect("clamped sequence number is non-negative");
        self.sequences
            .iter()
            .find(|sequence| sequence.number == number)
            .or_else(|| {
                usize::try_from(number)
                    .ok()
                    .and_then(|number| self.sequences.get(number % self.sequences.len()))
            })
    }
}

impl SheetSequence {
    /// Select the atlas rectangle at `time`, honoring clamp versus loop.
    #[must_use]
    pub fn uv_at(&self, time: f32) -> [f32; 4] {
        let Some(first) = self.frames.first() else {
            return [0.0, 0.0, 1.0, 1.0];
        };
        if self.frames.len() == 1 || self.total_time <= 0.0 || !time.is_finite() {
            return first.uv;
        }
        let mut remaining = if self.clamp {
            time.clamp(0.0, self.total_time)
        } else {
            time.rem_euclid(self.total_time)
        };
        for frame in &self.frames {
            if remaining < frame.display_time {
                return frame.uv;
            }
            remaining -= frame.display_time;
        }
        self.frames.last().map_or(first.uv, |frame| frame.uv)
    }
}

/// VTF pixel formats, by on-disk discriminant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum VtfFormat {
    Rgba8888,
    Abgr8888,
    Rgb888,
    Bgr888,
    Rgb565,
    I8,
    Ia88,
    P8,
    A8,
    Rgb888Bluescreen,
    Bgr888Bluescreen,
    Argb8888,
    Bgra8888,
    Dxt1,
    Dxt3,
    Dxt5,
    Bgrx8888,
    Bgr565,
    Bgrx5551,
    Bgra4444,
    Dxt1Onebitalpha,
    Bgra5551,
    Uv88,
    Uvwq8888,
    Rgba16161616F,
    Rgba16161616,
    Uvlx8888,
}

impl VtfFormat {
    fn from_raw(raw: i32) -> Option<Self> {
        Some(match raw {
            0 => Self::Rgba8888,
            1 => Self::Abgr8888,
            2 => Self::Rgb888,
            3 => Self::Bgr888,
            4 => Self::Rgb565,
            5 => Self::I8,
            6 => Self::Ia88,
            7 => Self::P8,
            8 => Self::A8,
            9 => Self::Rgb888Bluescreen,
            10 => Self::Bgr888Bluescreen,
            11 => Self::Argb8888,
            12 => Self::Bgra8888,
            13 => Self::Dxt1,
            14 => Self::Dxt3,
            15 => Self::Dxt5,
            16 => Self::Bgrx8888,
            17 => Self::Bgr565,
            18 => Self::Bgrx5551,
            19 => Self::Bgra4444,
            20 => Self::Dxt1Onebitalpha,
            21 => Self::Bgra5551,
            22 => Self::Uv88,
            23 => Self::Uvwq8888,
            24 => Self::Rgba16161616F,
            25 => Self::Rgba16161616,
            26 => Self::Uvlx8888,
            _ => return None,
        })
    }

    /// The block-compression format, if this is a BC format.
    #[must_use]
    pub const fn bc(self) -> Option<BcFormat> {
        match self {
            Self::Dxt1 | Self::Dxt1Onebitalpha => Some(BcFormat::Bc1),
            Self::Dxt3 => Some(BcFormat::Bc2),
            Self::Dxt5 => Some(BcFormat::Bc3),
            _ => None,
        }
    }

    /// Bytes for one `width x height` slice in this format. Zero
    /// dimensions yield zero bytes (a real case for absent lowres
    /// images); mip dimensions are clamped to 1 by the caller.
    fn slice_bytes(self, width: u32, height: u32) -> u64 {
        if let Some(bc) = self.bc() {
            let blocks = u64::from(width.div_ceil(4)) * u64::from(height.div_ceil(4));
            return blocks * u64::from(bc.block_bytes());
        }
        let pixels = u64::from(width) * u64::from(height);
        let bytes_per_pixel: u64 = match self {
            Self::I8 | Self::A8 | Self::P8 => 1,
            Self::Rgb565
            | Self::Bgr565
            | Self::Ia88
            | Self::Uv88
            | Self::Bgrx5551
            | Self::Bgra4444
            | Self::Bgra5551 => 2,
            Self::Rgb888 | Self::Bgr888 | Self::Rgb888Bluescreen | Self::Bgr888Bluescreen => 3,
            Self::Rgba8888
            | Self::Abgr8888
            | Self::Argb8888
            | Self::Bgra8888
            | Self::Bgrx8888
            | Self::Uvwq8888
            | Self::Uvlx8888 => 4,
            Self::Rgba16161616F | Self::Rgba16161616 => 8,
            Self::Dxt1 | Self::Dxt1Onebitalpha | Self::Dxt3 | Self::Dxt5 => unreachable!(),
        };
        pixels * bytes_per_pixel
    }
}

/// VTF parse or decode failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum VtfError {
    /// Input exceeds [`Limits::max_input_bytes`].
    InputTooLarge {
        /// Input length in bytes.
        len: u64,
        /// The configured cap.
        max: u64,
    },
    /// Input ends before a required structure.
    Truncated {
        /// Bytes required.
        needed: u64,
        /// Bytes available.
        available: u64,
    },
    /// The file does not start with `VTF\0`.
    BadMagic,
    /// Not a 7.0–7.5 container.
    UnsupportedVersion {
        /// Major version from the header.
        major: u32,
        /// Minor version from the header.
        minor: u32,
    },
    /// Unknown pixel-format discriminant.
    UnknownFormat(i32),
    /// The format is recognized but this crate cannot decode it.
    UnsupportedFormat(VtfFormat),
    /// Volume textures (`depth > 1`) are not supported.
    VolumeTexture {
        /// Depth from the header.
        depth: u16,
    },
    /// Zero width, height, or mip count.
    BadHeader,
    /// The 7.3+ resource directory is implausible or out of bounds.
    CorruptResources,
    /// A requested frame/face/mip does not exist in this texture.
    OutOfRange,
    /// The decoded RGBA image would exceed [`Limits::max_entry_bytes`].
    DecodedTooLarge {
        /// Would-be decoded size in bytes (`width * height * 4`).
        size: u64,
        /// The configured cap.
        max: u64,
    },
}

impl fmt::Display for VtfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge { len, max } => {
                write!(f, "vtf input is {len} bytes, over the {max}-byte limit")
            }
            Self::Truncated { needed, available } => {
                write!(f, "vtf truncated: need {needed} bytes, have {available}")
            }
            Self::BadMagic => write!(f, "not a vtf file (bad magic)"),
            Self::UnsupportedVersion { major, minor } => {
                write!(f, "unsupported vtf version {major}.{minor}")
            }
            Self::UnknownFormat(raw) => write!(f, "unknown vtf pixel format {raw}"),
            Self::UnsupportedFormat(format) => {
                write!(f, "vtf pixel format {format:?} is not decodable")
            }
            Self::VolumeTexture { depth } => {
                write!(f, "vtf volume textures are unsupported (depth {depth})")
            }
            Self::BadHeader => write!(f, "vtf header has zero width, height, or mip count"),
            Self::CorruptResources => write!(f, "vtf resource directory is corrupt"),
            Self::OutOfRange => write!(f, "requested vtf frame/face/mip does not exist"),
            Self::DecodedTooLarge { size, max } => {
                write!(
                    f,
                    "vtf decoded image of {size} bytes exceeds the {max}-byte limit"
                )
            }
        }
    }
}

impl std::error::Error for VtfError {}

impl crate::reader::ReadError for VtfError {
    fn truncated(needed: u64, available: u64) -> Self {
        Self::Truncated { needed, available }
    }
    fn overflow() -> Self {
        Self::BadHeader
    }
}

type Reader<'a> = crate::reader::Reader<'a, VtfError>;

impl Reader<'_> {
    fn u8(&mut self) -> Result<u8, VtfError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, VtfError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn f32(&mut self) -> Result<f32, VtfError> {
        Ok(f32::from_bits(self.u32()?))
    }

    fn u32(&mut self) -> Result<u32, VtfError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32, VtfError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Parse and validate a VTF header. Image data is addressed lazily and
/// bounds-checked per request.
pub fn parse<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Vtf<'a>, VtfError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(VtfError::InputTooLarge {
            len: bytes.len() as u64,
            max: limits.max_input_bytes,
        });
    }
    let mut r = Reader::at(bytes, 0);
    if r.take(4)? != SIGNATURE {
        return Err(VtfError::BadMagic);
    }
    let major = r.u32()?;
    let minor = r.u32()?;
    if major != 7 || minor > 5 {
        return Err(VtfError::UnsupportedVersion { major, minor });
    }
    let header_size = r.u32()? as usize;
    let width = u32::from(r.u16()?);
    let height = u32::from(r.u16()?);
    let flags = r.u32()?;
    let frames = u32::from(r.u16()?).max(1);
    let first_frame = r.u16()?;
    r.take(4)?; // padding
    let reflectivity = [r.f32()?, r.f32()?, r.f32()?];
    r.take(4)?; // padding
    let bumpmap_scale = r.f32()?;
    let format_raw = r.i32()?;
    let mip_count = r.u8()?;
    let lowres_format_raw = r.i32()?;
    let lowres_width = u32::from(r.u8()?);
    let lowres_height = u32::from(r.u8()?);
    debug_assert_eq!(r.pos, BASE_HEADER_BYTES);

    let depth = if minor >= 2 { r.u16()? } else { 1 };
    if depth > 1 {
        return Err(VtfError::VolumeTexture { depth });
    }
    if width == 0 || height == 0 || mip_count == 0 {
        return Err(VtfError::BadHeader);
    }
    let format = VtfFormat::from_raw(format_raw).ok_or(VtfError::UnknownFormat(format_raw))?;

    // Data resources (7.3+): the hires/lowres payload locations.
    let mut lowres_resource = None;
    let mut highres_resource = None;
    let mut sprite_sheet_resource = None;
    if minor >= 3 {
        r.take(3)?; // padding
        let count = r.u32()?;
        if count > MAX_RESOURCES {
            return Err(VtfError::CorruptResources);
        }
        r.take(8)?; // padding
        for _ in 0..count {
            let entry = r.take(8)?;
            let tag = [entry[0], entry[1], entry[2]];
            let has_data = entry[3] & 0x02 == 0;
            let value = u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]);
            if has_data {
                match tag {
                    [0x01, 0x00, 0x00] => lowres_resource = lowres_resource.or(Some(value)),
                    [0x10, 0x00, 0x00] => {
                        sprite_sheet_resource = sprite_sheet_resource.or(Some(value));
                    }
                    [0x30, 0x00, 0x00] => highres_resource = highres_resource.or(Some(value)),
                    _ => {}
                }
            }
        }
    }

    let lowres_offset = lowres_resource.map_or(header_size as u64, |offset| offset as u64);
    let highres_offset = highres_resource.map_or_else(
        || {
            let lowres_bytes = match VtfFormat::from_raw(lowres_format_raw) {
                Some(format) if lowres_format_raw != -1 => {
                    format.slice_bytes(lowres_width, lowres_height)
                }
                _ => 0, // no or unknown lowres image
            };
            lowres_offset + lowres_bytes
        },
        |offset| offset as u64,
    );
    let highres_offset = usize::try_from(highres_offset).map_err(|_| VtfError::CorruptResources)?;
    if highres_offset > bytes.len() {
        return Err(VtfError::Truncated {
            needed: highres_offset as u64,
            available: bytes.len() as u64,
        });
    }

    // VTFLib face rule: envmaps carry a spheremap as a 7th face before
    // 7.5 unless first_frame is 0xFFFF.
    let faces = if flags & texture_flags::ENVMAP != 0 {
        if minor < 5 && first_frame != 0xFFFF {
            7
        } else {
            6
        }
    } else {
        1
    };

    Ok(Vtf {
        width,
        height,
        flags,
        frames,
        faces,
        mip_count,
        format,
        version_minor: minor,
        reflectivity,
        bumpmap_scale,
        highres_offset,
        sprite_sheet_offset: sprite_sheet_resource.and_then(|offset| usize::try_from(offset).ok()),
        bytes,
    })
}

fn mip_dimension(base: u32, level: u8) -> u32 {
    base.checked_shr(u32::from(level)).unwrap_or(0).max(1)
}

impl<'a> Vtf<'a> {
    /// Base (mip 0) width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Base (mip 0) height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Texture flags word (see [`texture_flags`]).
    #[must_use]
    pub fn flags(&self) -> u32 {
        self.flags
    }

    /// The high-resolution pixel format.
    #[must_use]
    pub fn format(&self) -> VtfFormat {
        self.format
    }

    /// Animation frame count (at least 1).
    #[must_use]
    pub fn frame_count(&self) -> u32 {
        self.frames
    }

    /// 1 for ordinary textures; 6 or 7 for envmap cubemaps.
    #[must_use]
    pub fn face_count(&self) -> u32 {
        self.faces
    }

    /// Mip level count (at least 1); level 0 is full resolution.
    #[must_use]
    pub fn mip_count(&self) -> u8 {
        self.mip_count
    }

    /// Average color, from the header (renderers use it for tinting).
    #[must_use]
    pub fn reflectivity(&self) -> [f32; 3] {
        self.reflectivity
    }

    /// Bump-map scale, from the header.
    #[must_use]
    pub fn bumpmap_scale(&self) -> f32 {
        self.bumpmap_scale
    }

    /// Container minor version (7.`minor`).
    #[must_use]
    pub fn version_minor(&self) -> u32 {
        self.version_minor
    }

    /// Byte offset of one frame/face/mip slice within the file, plus its
    /// length. Layout: mip (smallest first) → frame → face → slice.
    fn slice_range(&self, frame: u32, face: u32, mip: u8) -> Result<(usize, usize), VtfError> {
        if frame >= self.frames || face >= self.faces || mip >= self.mip_count {
            return Err(VtfError::OutOfRange);
        }
        let images = u64::from(self.frames) * u64::from(self.faces);
        let mut offset = 0u64;
        for level in (mip + 1)..self.mip_count {
            let w = mip_dimension(self.width, level);
            let h = mip_dimension(self.height, level);
            offset += self.format.slice_bytes(w, h) * images;
        }
        let w = mip_dimension(self.width, mip);
        let h = mip_dimension(self.height, mip);
        let slice = self.format.slice_bytes(w, h);
        offset += slice * (u64::from(frame) * u64::from(self.faces) + u64::from(face));
        let start = (self.highres_offset as u64)
            .checked_add(offset)
            .ok_or(VtfError::BadHeader)?;
        let start = usize::try_from(start).map_err(|_| VtfError::BadHeader)?;
        let len = usize::try_from(slice).map_err(|_| VtfError::BadHeader)?;
        Ok((start, len))
    }

    fn slice_bytes(&self, frame: u32, face: u32, mip: u8) -> Result<&'a [u8], VtfError> {
        let (start, len) = self.slice_range(frame, face, mip)?;
        let end = start.checked_add(len).ok_or(VtfError::BadHeader)?;
        self.bytes.get(start..end).ok_or(VtfError::Truncated {
            needed: end as u64,
            available: self.bytes.len() as u64,
        })
    }

    /// Raw BC blocks for one frame/face across all mips present in the
    /// file, smallest-first (file order); truncated tails are dropped.
    /// `None` if the format is not BC or no complete mip exists.
    #[must_use]
    pub fn raw_bc(&self, frame: u32, face: u32) -> Option<RawBcTexture<'a>> {
        let format = self.format.bc()?;
        if frame >= self.frames || face >= self.faces {
            return None;
        }
        let mut mips = Vec::new();
        for mip in (0..self.mip_count).rev() {
            match self.slice_bytes(frame, face, mip) {
                Ok(data) => mips.push(RawBcMip {
                    width: mip_dimension(self.width, mip),
                    height: mip_dimension(self.height, mip),
                    data,
                }),
                // Smallest mips come first in the file, so the first
                // truncated level ends everything above it too.
                Err(_) => break,
            }
        }
        let truncated = mips.len() < usize::from(self.mip_count);
        (!mips.is_empty()).then_some(RawBcTexture {
            format,
            width: self.width,
            height: self.height,
            truncated,
            mips,
        })
    }

    /// Decode the optional particle sprite-sheet resource.
    #[must_use]
    pub fn sprite_sheet(&self, limits: &Limits) -> Option<SpriteSheet> {
        let offset = self.sprite_sheet_offset?;
        let size_bytes = self.bytes.get(offset..offset.checked_add(4)?)?;
        let size = usize::try_from(u32::from_le_bytes(size_bytes.try_into().ok()?)).ok()?;
        let payload_start = offset.checked_add(4)?;
        let payload = self
            .bytes
            .get(payload_start..payload_start.checked_add(size)?)?;
        parse_sprite_sheet_payload(payload, limits)
    }

    /// Decode one frame/face/mip to RGBA8. Rejects a decode that would
    /// exceed [`Limits::max_entry_bytes`] before allocating it — block
    /// compression amplifies stored bytes up to 8x, so the stored slice
    /// passing the file's own bounds does not bound the decoded size.
    pub fn decode_rgba(
        &self,
        frame: u32,
        face: u32,
        mip: u8,
        limits: &Limits,
    ) -> Result<RgbaImage, VtfError> {
        let data = self.slice_bytes(frame, face, mip)?;
        let width = mip_dimension(self.width, mip);
        let height = mip_dimension(self.height, mip);
        let rgba = decode_slice(self.format, data, width, height, limits)?;
        Ok(RgbaImage {
            width,
            height,
            rgba,
        })
    }
}

fn parse_sprite_sheet_payload(bytes: &[u8], limits: &Limits) -> Option<SpriteSheet> {
    // Structural ceilings on top of the caller's `Limits`: real sprite
    // sheets are small (tens of sequences, dozens of frames), so these
    // stay the effective cap under the crate's generous defaults while
    // still honoring a caller who configures a tighter budget.
    const MAX_SEQUENCES: u32 = 1024;
    const MAX_FRAMES: u32 = 4096;
    let max_entries = u32::try_from(limits.max_entries).unwrap_or(u32::MAX);
    let max_sequences = MAX_SEQUENCES.min(max_entries);
    let max_frames = MAX_FRAMES.min(max_entries);

    let mut position = 0_usize;
    let read_u32 = |position: &mut usize| -> Option<u32> {
        let end = position.checked_add(4)?;
        let slice = bytes.get(*position..end)?;
        *position = end;
        Some(u32::from_le_bytes(slice.try_into().ok()?))
    };
    let version = read_u32(&mut position)?;
    if version > 1 {
        return None;
    }
    let sequence_count = read_u32(&mut position)?;
    if sequence_count > max_sequences {
        return None;
    }
    let mut sequences = Vec::with_capacity(sequence_count as usize);
    for _ in 0..sequence_count {
        let number = read_u32(&mut position)?;
        let clamp = read_u32(&mut position)? != 0;
        let frame_count = read_u32(&mut position)?;
        if frame_count > max_frames {
            return None;
        }
        let total_time = f32::from_bits(read_u32(&mut position)?);
        let mut frames = Vec::with_capacity(frame_count as usize);
        for _ in 0..frame_count {
            let display_time = f32::from_bits(read_u32(&mut position)?);
            let coordinate_sets = if version == 0 { 1 } else { 4 };
            let mut uv = [0.0_f32; 4];
            for set in 0..coordinate_sets {
                let rect = [
                    f32::from_bits(read_u32(&mut position)?),
                    f32::from_bits(read_u32(&mut position)?),
                    f32::from_bits(read_u32(&mut position)?),
                    f32::from_bits(read_u32(&mut position)?),
                ];
                if set == 0 {
                    uv = rect;
                }
            }
            frames.push(SheetFrame { display_time, uv });
        }
        sequences.push(SheetSequence {
            number,
            clamp,
            total_time,
            frames,
        });
    }
    Some(SpriteSheet { sequences })
}

// ---------------------------------------------------------------
// Pixel decoding
// ---------------------------------------------------------------

fn decode_slice(
    format: VtfFormat,
    data: &[u8],
    width: u32,
    height: u32,
    limits: &Limits,
) -> Result<Vec<u8>, VtfError> {
    // Checked: 65535×65535×4 overflows 32-bit usize. (Reachable sizes
    // are file-backed — the slice extent was validated — but the
    // arithmetic must not wrap on the way to that conclusion.)
    let pixels =
        usize::try_from(u64::from(width) * u64::from(height)).map_err(|_| VtfError::BadHeader)?;
    let rgba_len = pixels.checked_mul(4).ok_or(VtfError::BadHeader)?;
    if rgba_len as u64 > limits.max_entry_bytes {
        return Err(VtfError::DecodedTooLarge {
            size: rgba_len as u64,
            max: limits.max_entry_bytes,
        });
    }
    if let Some(bc) = format.bc() {
        return Ok(decode_bc(bc, data, width, height));
    }
    let mut out = Vec::with_capacity(rgba_len);
    match format {
        VtfFormat::Rgba8888 | VtfFormat::Uvwq8888 | VtfFormat::Uvlx8888 => {
            out.extend_from_slice(data);
        }
        VtfFormat::Abgr8888 => {
            for p in data.chunks_exact(4) {
                out.extend_from_slice(&[p[3], p[2], p[1], p[0]]);
            }
        }
        VtfFormat::Argb8888 => {
            for p in data.chunks_exact(4) {
                out.extend_from_slice(&[p[1], p[2], p[3], p[0]]);
            }
        }
        VtfFormat::Bgra8888 => {
            for p in data.chunks_exact(4) {
                out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        }
        VtfFormat::Bgrx8888 => {
            for p in data.chunks_exact(4) {
                out.extend_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
        VtfFormat::Rgb888 => {
            for p in data.chunks_exact(3) {
                out.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        VtfFormat::Bgr888 => {
            for p in data.chunks_exact(3) {
                out.extend_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
        VtfFormat::Rgb888Bluescreen => {
            for p in data.chunks_exact(3) {
                let a = if (p[0], p[1], p[2]) == (0, 0, 255) {
                    0
                } else {
                    255
                };
                out.extend_from_slice(&[p[0], p[1], p[2], a]);
            }
        }
        VtfFormat::Bgr888Bluescreen => {
            for p in data.chunks_exact(3) {
                let a = if (p[2], p[1], p[0]) == (0, 0, 255) {
                    0
                } else {
                    255
                };
                out.extend_from_slice(&[p[2], p[1], p[0], a]);
            }
        }
        VtfFormat::I8 => {
            for &i in data.iter().take(pixels) {
                out.extend_from_slice(&[i, i, i, 255]);
            }
        }
        VtfFormat::A8 => {
            for &a in data.iter().take(pixels) {
                out.extend_from_slice(&[0, 0, 0, a]);
            }
        }
        VtfFormat::Ia88 => {
            for p in data.chunks_exact(2) {
                out.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
        }
        VtfFormat::Uv88 => {
            for p in data.chunks_exact(2) {
                out.extend_from_slice(&[p[0], p[1], 0, 255]);
            }
        }
        // Packed 16-bit: channels named from the lowest bits up.
        VtfFormat::Rgb565 => {
            for p in data.chunks_exact(2) {
                let v = u16::from_le_bytes([p[0], p[1]]);
                out.extend_from_slice(&[
                    expand5((v & 0x1F) as u8),
                    expand6(((v >> 5) & 0x3F) as u8),
                    expand5((v >> 11) as u8),
                    255,
                ]);
            }
        }
        VtfFormat::Bgr565 => {
            for p in data.chunks_exact(2) {
                let v = u16::from_le_bytes([p[0], p[1]]);
                out.extend_from_slice(&[
                    expand5((v >> 11) as u8),
                    expand6(((v >> 5) & 0x3F) as u8),
                    expand5((v & 0x1F) as u8),
                    255,
                ]);
            }
        }
        VtfFormat::Bgra4444 => {
            for p in data.chunks_exact(2) {
                let v = u16::from_le_bytes([p[0], p[1]]);
                out.extend_from_slice(&[
                    expand4(((v >> 8) & 0xF) as u8),
                    expand4(((v >> 4) & 0xF) as u8),
                    expand4((v & 0xF) as u8),
                    expand4((v >> 12) as u8),
                ]);
            }
        }
        VtfFormat::Bgra5551 | VtfFormat::Bgrx5551 => {
            let force_opaque = format == VtfFormat::Bgrx5551;
            for p in data.chunks_exact(2) {
                let v = u16::from_le_bytes([p[0], p[1]]);
                let a = if force_opaque || v & 0x8000 != 0 {
                    255
                } else {
                    0
                };
                out.extend_from_slice(&[
                    expand5(((v >> 10) & 0x1F) as u8),
                    expand5(((v >> 5) & 0x1F) as u8),
                    expand5((v & 0x1F) as u8),
                    a,
                ]);
            }
        }
        VtfFormat::Rgba16161616 => {
            for p in data.chunks_exact(8) {
                for c in 0..4 {
                    let v = u16::from_le_bytes([p[c * 2], p[c * 2 + 1]]);
                    out.push((v >> 8) as u8);
                }
            }
        }
        VtfFormat::Rgba16161616F => {
            for p in data.chunks_exact(8) {
                for c in 0..4 {
                    let v = u16::from_le_bytes([p[c * 2], p[c * 2 + 1]]);
                    let f = half_to_f32(v).clamp(0.0, 1.0);
                    out.push(normalized_float_to_u8(f));
                }
            }
        }
        VtfFormat::P8 => return Err(VtfError::UnsupportedFormat(format)),
        VtfFormat::Dxt1 | VtfFormat::Dxt1Onebitalpha | VtfFormat::Dxt3 | VtfFormat::Dxt5 => {
            unreachable!()
        }
    }
    out.resize(rgba_len, 0);
    Ok(out)
}

fn expand4(v: u8) -> u8 {
    v * 0x11
}

fn normalized_float_to_u8(value: f32) -> u8 {
    let rounded = (value.clamp(0.0, 1.0) * 255.0).round();
    debug_assert!((0.0..=255.0).contains(&rounded));
    rounded as u8
}

fn expand5(v: u8) -> u8 {
    (v << 3) | (v >> 2)
}

fn expand6(v: u8) -> u8 {
    (v << 2) | (v >> 4)
}

// ---------------------------------------------------------------
// BC1/2/3 block decoding
// ---------------------------------------------------------------

fn decode_bc(format: BcFormat, data: &[u8], width: u32, height: u32) -> Vec<u8> {
    // decode_slice pre-validates the checked form of this product.
    let rgba_len = usize::try_from(u64::from(width) * u64::from(height) * 4)
        .expect("output size validated by decode_slice");
    let width = width as usize;
    let height = height as usize;
    let mut out = vec![0u8; rgba_len];
    let block_bytes = format.block_bytes() as usize;
    let blocks_wide = width.max(1).div_ceil(4);
    let blocks_high = height.max(1).div_ceil(4);
    let mut texels = [[0u8; 4]; 16];
    for by in 0..blocks_high {
        for bx in 0..blocks_wide {
            let offset = (by * blocks_wide + bx) * block_bytes;
            let Some(block) = data.get(offset..offset + block_bytes) else {
                continue; // truncated tail: leave transparent black
            };
            match format {
                BcFormat::Bc1 => decode_color_block(&block[..8], &mut texels, false),
                BcFormat::Bc2 => {
                    decode_color_block(&block[8..], &mut texels, true);
                    for (i, texel) in texels.iter_mut().enumerate() {
                        let nibble = (block[i / 2] >> ((i % 2) * 4)) & 0xF;
                        texel[3] = expand4(nibble);
                    }
                }
                BcFormat::Bc3 => {
                    decode_color_block(&block[8..], &mut texels, true);
                    decode_bc3_alpha(&block[..8], &mut texels);
                }
            }
            for ty in 0..4 {
                let y = by * 4 + ty;
                if y >= height {
                    break;
                }
                for tx in 0..4 {
                    let x = bx * 4 + tx;
                    if x >= width {
                        break;
                    }
                    let dst = (y * width + x) * 4;
                    out[dst..dst + 4].copy_from_slice(&texels[ty * 4 + tx]);
                }
            }
        }
    }
    out
}

fn decode_color_block(block: &[u8], texels: &mut [[u8; 4]; 16], opaque: bool) {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let rgb0 = expand565(c0);
    let rgb1 = expand565(c1);
    let mut palette = [[0u8; 4]; 4];
    palette[0] = [rgb0[0], rgb0[1], rgb0[2], 255];
    palette[1] = [rgb1[0], rgb1[1], rgb1[2], 255];
    if c0 > c1 || opaque {
        for c in 0..3 {
            palette[2][c] = u8::try_from((2 * u16::from(rgb0[c]) + u16::from(rgb1[c])) / 3)
                .expect("BC1 interpolation stays in the byte range");
            palette[3][c] = u8::try_from((u16::from(rgb0[c]) + 2 * u16::from(rgb1[c])) / 3)
                .expect("BC1 interpolation stays in the byte range");
        }
        palette[2][3] = 255;
        palette[3][3] = 255;
    } else {
        for c in 0..3 {
            palette[2][c] = u8::try_from((u16::from(rgb0[c]) + u16::from(rgb1[c])) / 2)
                .expect("BC1 interpolation stays in the byte range");
        }
        palette[2][3] = 255;
        palette[3] = [0, 0, 0, 0];
    }
    let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    for (i, texel) in texels.iter_mut().enumerate() {
        *texel = palette[((indices >> (i * 2)) & 0b11) as usize];
    }
}

fn expand565(c: u16) -> [u8; 3] {
    [
        expand5((c >> 11) as u8),
        expand6(((c >> 5) & 0x3F) as u8),
        expand5((c & 0x1F) as u8),
    ]
}

fn decode_bc3_alpha(block: &[u8], texels: &mut [[u8; 4]; 16]) {
    let a0 = block[0];
    let a1 = block[1];
    let mut palette = [0u8; 8];
    palette[0] = a0;
    palette[1] = a1;
    if a0 > a1 {
        for i in 2..8u16 {
            palette[usize::from(i)] =
                u8::try_from(((8 - i) * u16::from(a0) + (i - 1) * u16::from(a1)) / 7)
                    .expect("BC3 interpolation stays in the byte range");
        }
    } else {
        for i in 2..6u16 {
            palette[usize::from(i)] =
                u8::try_from(((6 - i) * u16::from(a0) + (i - 1) * u16::from(a1)) / 5)
                    .expect("BC3 interpolation stays in the byte range");
        }
        palette[6] = 0;
        palette[7] = 255;
    }
    let mut bits = 0u64;
    for (i, &byte) in block[2..8].iter().enumerate() {
        bits |= u64::from(byte) << (i * 8);
    }
    for (i, texel) in texels.iter_mut().enumerate() {
        texel[3] = palette[((bits >> (i * 3)) & 0b111) as usize];
    }
}

#[cfg(test)]
#[path = "vtf_tests.rs"]
mod tests;
