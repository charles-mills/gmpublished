//! BSP map containers (VBSP versions 19–21), wire-format layer.
//!
//! The container (header + 64-lump directory with a raw escape hatch),
//! the entities lump as a KeyValues dialect, the embedded pakfile via
//! this crate's own minimal ZIP reader, the fixed-stride geometry
//! lumps, the game lump (static and detail props), and visibility
//! decompression are available. Scene assembly — doors, detail
//! sprites, lightmap atlases — is deliberately not this crate's
//! business.
//!
//! Strict on the container: magic, version, and every lump extent are
//! validated up front; lump *contents* are parsed lazily per accessor.
//!
//! Repacked maps LZMA-compress individual lumps (a nonzero directory
//! `fourCC` holds the uncompressed size). Accessors decompress
//! transparently with the `lzma` feature and fail with
//! [`BspError::CompressedLump`] without it; [`Bsp::pakfile`], whose
//! reader borrows, always fails on a compressed pakfile lump (Valve's
//! tools never compress that one).

use std::borrow::Cow;
use std::fmt;

use crate::Limits;
use crate::keyvalues::{self, KvDocument, KvError, Token};

mod game_lump;
pub use game_lump::{
    DetailProp, DetailProps, DetailSprite, GameLump, GameLumpEntry, StaticProp, StaticPropLayout,
    StaticProps, game_lump_ids,
};

mod inflate;

mod record;

mod lumps;
pub use lumps::{
    Brush, BrushSide, BspModel, ColorRgbExp, DispInfo, DispVert, Face, Leaf, LeafAmbientIndex,
    LeafAmbientSample, Node, Overlay, Plane, TexData, TexInfo, contents_flags, texture_flags,
};

mod vis;
pub use vis::Visibility;

mod zip;
pub use zip::{ZipEntry, ZipError, ZipReader};

const BSP_MAGIC: &[u8; 4] = b"VBSP";
const LUMP_COUNT: usize = 64;
const HEADER_BYTES: usize = 4 + 4 + LUMP_COUNT * 16 + 4;

/// Well-known lump indices (the documented Source 1 set this crate
/// currently names; any index 0–63 works with [`Bsp::lump`]).
#[allow(missing_docs)]
pub mod lump_ids {
    pub const ENTITIES: usize = 0;
    pub const PLANES: usize = 1;
    pub const TEXDATA: usize = 2;
    pub const VERTICES: usize = 3;
    pub const VISIBILITY: usize = 4;
    pub const NODES: usize = 5;
    pub const TEXINFO: usize = 6;
    pub const FACES: usize = 7;
    pub const LIGHTING: usize = 8;
    pub const LEAFS: usize = 10;
    pub const EDGES: usize = 12;
    pub const SURFEDGES: usize = 13;
    pub const MODELS: usize = 14;
    pub const LEAF_FACES: usize = 16;
    pub const LEAF_BRUSHES: usize = 17;
    pub const BRUSHES: usize = 18;
    pub const BRUSHSIDES: usize = 19;
    pub const DISPINFO: usize = 26;
    pub const GAME_LUMP: usize = 35;
    pub const LEAF_AMBIENT_INDEX_HDR: usize = 51;
    pub const LEAF_AMBIENT_INDEX: usize = 52;
    pub const LEAF_AMBIENT_LIGHTING_HDR: usize = 55;
    pub const LEAF_AMBIENT_LIGHTING: usize = 56;
    pub const PAKFILE: usize = 40;
    pub const DISP_VERTS: usize = 33;
    pub const TEXDATA_STRING_DATA: usize = 43;
    pub const TEXDATA_STRING_TABLE: usize = 44;
    pub const OVERLAYS: usize = 45;
    pub const LIGHTING_HDR: usize = 53;
}

/// A parsed BSP container: validated lump directory over borrowed bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bsp<'a> {
    version: i32,
    map_revision: i32,
    lumps: Vec<LumpDirectoryEntry>,
    bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LumpDirectoryEntry {
    offset: usize,
    len: usize,
    version: i32,
    /// Uncompressed size when the lump is LZMA-compressed, else 0.
    four_cc: i32,
}

/// BSP container failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BspError {
    /// Input exceeds [`Limits::max_input_bytes`].
    InputTooLarge {
        /// Input length in bytes.
        len: u64,
        /// The configured cap.
        max: u64,
    },
    /// The file does not start with `VBSP`.
    BadMagic,
    /// Not a version 19–21 map.
    UnsupportedVersion(i32),
    /// Input ends before a required structure.
    Truncated {
        /// Bytes required.
        needed: u64,
        /// Bytes available.
        available: u64,
    },
    /// A lump directory entry is malformed or out of bounds.
    CorruptLump {
        /// The lump index.
        index: usize,
    },
    /// The entities lump exceeds [`Limits::max_entry_bytes`] or its
    /// KeyValues structure violates limits.
    Entities(KvError),
    /// A lump's record count exceeds [`Limits::max_entries`].
    TooManyRecords {
        /// Which lump overflowed the cap.
        part: &'static str,
        /// The configured cap.
        max: usize,
    },
    /// The lump is LZMA-compressed and cannot be read here: either the
    /// `lzma` feature is disabled, or the accessor needs the raw bytes
    /// in place (the pakfile reader, the game lump's absolute offsets).
    CompressedLump {
        /// The lump index.
        index: usize,
    },
    /// A compressed payload's LZMA header or stream is invalid, or a
    /// game-lump structure is malformed.
    Decode {
        /// Which structure failed.
        part: &'static str,
    },
    /// A decompressed payload exceeds [`Limits::max_entry_bytes`].
    LumpTooLarge {
        /// Which structure overflowed.
        part: &'static str,
        /// Declared uncompressed size.
        size: u64,
        /// The configured cap.
        max: u64,
    },
    /// The pakfile lump is not a readable ZIP archive.
    Pakfile(ZipError),
}

impl fmt::Display for BspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge { len, max } => {
                write!(f, "bsp input is {len} bytes, over the {max}-byte limit")
            }
            Self::BadMagic => write!(f, "not a bsp file (bad magic)"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported bsp version {version}")
            }
            Self::Truncated { needed, available } => {
                write!(f, "bsp truncated: need {needed} bytes, have {available}")
            }
            Self::CorruptLump { index } => {
                write!(f, "bsp lump {index} directory entry is malformed")
            }
            Self::Entities(error) => write!(f, "bsp entities lump: {error}"),
            Self::TooManyRecords { part, max } => {
                write!(f, "bsp {part} record count exceeds the limit of {max}")
            }
            Self::CompressedLump { index } => {
                write!(
                    f,
                    "bsp lump {index} is lzma-compressed and cannot be read here"
                )
            }
            Self::Decode { part } => write!(f, "bsp {part} is malformed"),
            Self::LumpTooLarge { part, size, max } => {
                write!(f, "bsp {part} of {size} bytes exceeds the {max}-byte limit")
            }
            Self::Pakfile(error) => write!(f, "bsp pakfile lump: {error}"),
        }
    }
}

impl std::error::Error for BspError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Entities(error) => Some(error),
            Self::Pakfile(error) => Some(error),
            _ => None,
        }
    }
}

impl From<KvError> for BspError {
    fn from(error: KvError) -> Self {
        Self::Entities(error)
    }
}

/// Parse and validate a BSP container. Lump contents are accessed
/// lazily; every directory extent is bounds-checked here.
pub fn parse<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Bsp<'a>, BspError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(BspError::InputTooLarge {
            len: bytes.len() as u64,
            max: limits.max_input_bytes,
        });
    }
    if bytes.len() < HEADER_BYTES {
        return Err(if bytes.get(0..4).is_some_and(|magic| magic != BSP_MAGIC) {
            BspError::BadMagic
        } else {
            BspError::Truncated {
                needed: HEADER_BYTES as u64,
                available: bytes.len() as u64,
            }
        });
    }
    if &bytes[0..4] != BSP_MAGIC {
        return Err(BspError::BadMagic);
    }
    let version = i32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes"));
    if !(19..=21).contains(&version) {
        return Err(BspError::UnsupportedVersion(version));
    }

    let mut lumps = Vec::with_capacity(LUMP_COUNT);
    for index in 0..LUMP_COUNT {
        let at = 8 + index * 16;
        let entry = &bytes[at..at + 16];
        let offset = i32::from_le_bytes(entry[0..4].try_into().expect("4 bytes"));
        let len = i32::from_le_bytes(entry[4..8].try_into().expect("4 bytes"));
        let lump_version = i32::from_le_bytes(entry[8..12].try_into().expect("4 bytes"));
        let four_cc = i32::from_le_bytes(entry[12..16].try_into().expect("4 bytes"));
        let (Ok(offset), Ok(len)) = (usize::try_from(offset), usize::try_from(len)) else {
            return Err(BspError::CorruptLump { index });
        };
        // Absent lumps carry arbitrary offsets in real files; normalize
        // so accessors can slice `offset..offset + len` unconditionally.
        let offset = if len == 0 { 0 } else { offset };
        let mut len = len;
        if len > 0 {
            let end = offset
                .checked_add(len)
                .ok_or(BspError::CorruptLump { index })?;
            if offset > bytes.len() {
                return Err(BspError::Truncated {
                    needed: end as u64,
                    available: bytes.len() as u64,
                });
            }
            // Real files overhang: repacking tools declare a length past
            // EOF (a dropped trailing NUL on the entities lump is
            // common). The engine never validates extents — it reads
            // what is there — so clamp rather than reject.
            len = len.min(bytes.len() - offset);
        }
        lumps.push(LumpDirectoryEntry {
            offset,
            len,
            version: lump_version,
            four_cc,
        });
    }
    let map_revision = i32::from_le_bytes(
        bytes[8 + LUMP_COUNT * 16..HEADER_BYTES]
            .try_into()
            .expect("4 bytes"),
    );

    Ok(Bsp {
        version,
        map_revision,
        lumps,
        bytes,
    })
}

impl<'a> Bsp<'a> {
    /// Container version (19–21).
    #[must_use]
    pub fn version(&self) -> i32 {
        self.version
    }

    /// The map's revision counter from the header.
    #[must_use]
    pub fn map_revision(&self) -> i32 {
        self.map_revision
    }

    /// Raw bytes of lump `index` (see [`lump_ids`]); `None` for indices
    /// past 63. Empty lumps yield an empty slice. Raw means raw: on a
    /// repacked map these bytes may be LZMA-compressed — check
    /// [`lump_compression`](Self::lump_compression), or read through
    /// [`lump_data`](Self::lump_data) which decompresses for you.
    #[must_use]
    pub fn lump(&self, index: usize) -> Option<&'a [u8]> {
        let entry = self.lumps.get(index)?;
        // Extents were validated at parse.
        Some(&self.bytes[entry.offset..entry.offset + entry.len])
    }

    /// The declared version of lump `index`.
    #[must_use]
    pub fn lump_version(&self, index: usize) -> Option<i32> {
        self.lumps.get(index).map(|entry| entry.version)
    }

    /// The lump's compression, as its uncompressed size: `Some(n)`
    /// when lump `index` is LZMA-compressed and inflates to `n` bytes,
    /// `None` when stored raw (or past index 63).
    #[must_use]
    pub fn lump_compression(&self, index: usize) -> Option<u32> {
        let entry = self.lumps.get(index)?;
        u32::try_from(entry.four_cc).ok().filter(|size| *size != 0)
    }

    /// Bytes of lump `index`, decompressing LZMA-compressed lumps
    /// (`lzma` feature). Indices past 63 yield an empty slice. This is
    /// what the typed accessors read through; [`Bsp::lump`] stays raw.
    pub fn lump_data(&self, index: usize, limits: &Limits) -> Result<Cow<'a, [u8]>, BspError> {
        let Some(entry) = self.lumps.get(index) else {
            return Ok(Cow::Borrowed(&[]));
        };
        let raw = &self.bytes[entry.offset..entry.offset + entry.len];
        if entry.four_cc == 0 {
            return Ok(Cow::Borrowed(raw));
        }
        #[cfg(feature = "lzma")]
        {
            let data = decompress_valve_lzma(raw, "compressed lump", limits)?;
            // The directory's fourCC is the uncompressed size.
            if u64::try_from(entry.four_cc) != Ok(data.len() as u64) {
                return Err(BspError::CorruptLump { index });
            }
            Ok(Cow::Owned(data))
        }
        #[cfg(not(feature = "lzma"))]
        {
            let _ = limits;
            Err(BspError::CompressedLump { index })
        }
    }

    /// Parse the entities lump: a sequence of keyless `{ ... }` blocks,
    /// one [`KvDocument`] per entity, in lump order. The lump's bytes
    /// are decoded lossily (real maps carry stray bytes) and trailing
    /// NULs are ignored.
    pub fn entities(&self, limits: &Limits) -> Result<Vec<KvDocument<'a>>, BspError> {
        // Documents borrow when the lump is stored raw and valid UTF-8;
        // lossy decodes and decompressed lumps parse from a temporary,
        // so those documents must own. Real entity lumps are ASCII and
        // uncompressed: the borrowed variant is the norm.
        match self.lump_data(lump_ids::ENTITIES, limits)? {
            Cow::Borrowed(raw) => {
                let raw = trim_trailing_nuls(raw);
                std::str::from_utf8(raw).map_or_else(
                    |_| owned_entities(raw, limits),
                    |text| entities_from_text(text, limits),
                )
            }
            Cow::Owned(raw) => owned_entities(trim_trailing_nuls(&raw), limits),
        }
    }

    /// The embedded pakfile as a ZIP central-directory reader. Fails
    /// with [`BspError::CompressedLump`] if the pakfile lump itself is
    /// LZMA-compressed (out of spec — Valve's repack tool compresses
    /// pakfile *entries*, never the lump).
    pub fn pakfile(&self) -> Result<ZipReader<'a>, BspError> {
        if self.lump_compression(lump_ids::PAKFILE).is_some() {
            return Err(BspError::CompressedLump {
                index: lump_ids::PAKFILE,
            });
        }
        ZipReader::parse(self.lump(lump_ids::PAKFILE).unwrap_or_default())
            .map_err(BspError::Pakfile)
    }
}

fn trim_trailing_nuls(raw: &[u8]) -> &[u8] {
    let end = raw
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |index| index + 1);
    &raw[..end]
}

/// Parse entities from a temporary buffer: documents must own.
fn owned_entities(raw: &[u8], limits: &Limits) -> Result<Vec<KvDocument<'static>>, BspError> {
    let text = String::from_utf8_lossy(raw);
    let parsed = entities_from_text(&text, limits)?;
    Ok(parsed.into_iter().map(own_document).collect())
}

/// Decompress Valve's LZMA framing: `LZMA` magic, uncompressed and
/// stream sizes (u32 each), then the 5 raw properties bytes and the
/// stream. Used by compressed lumps and compressed game lumps.
#[cfg(feature = "lzma")]
fn decompress_valve_lzma(
    raw: &[u8],
    part: &'static str,
    limits: &Limits,
) -> Result<Vec<u8>, BspError> {
    use std::io::Read as _;

    const HEADER: usize = 4 + 4 + 4 + 5;
    if raw.len() < HEADER || &raw[0..4] != b"LZMA" {
        return Err(BspError::Decode { part });
    }
    let actual_size = u64::from(u32::from_le_bytes(raw[4..8].try_into().expect("4 bytes")));
    let lzma_size = u32::from_le_bytes(raw[8..12].try_into().expect("4 bytes")) as usize;
    if actual_size > limits.max_entry_bytes {
        return Err(BspError::LumpTooLarge {
            part,
            size: actual_size,
            max: limits.max_entry_bytes,
        });
    }
    let props = raw[12];
    let dict_size = u32::from_le_bytes(raw[13..HEADER].try_into().expect("4 bytes"));
    // The extent may carry trailing alignment padding; the stream size
    // field governs.
    let stream = raw
        .get(
            HEADER
                ..HEADER
                    .checked_add(lzma_size)
                    .ok_or(BspError::Decode { part })?,
        )
        .ok_or(BspError::Decode { part })?;
    let mut reader =
        lzma_rust2::LzmaReader::new_with_props(stream, actual_size, props, dict_size, None)
            .map_err(|_| BspError::Decode { part })?;
    let expected = usize::try_from(actual_size).map_err(|_| BspError::Decode { part })?;
    let mut out = Vec::with_capacity(expected);
    let mut buf = [0u8; 16 * 1024];
    while out.len() < expected {
        let n = reader
            .read(&mut buf)
            .map_err(|_| BspError::Decode { part })?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() > expected {
            return Err(BspError::Decode { part });
        }
    }
    if out.len() != expected {
        return Err(BspError::Decode { part });
    }
    Ok(out)
}

/// Entities: keyless top-level blocks. Words outside blocks are
/// skipped (tolerance for stray bytes between entities).
fn entities_from_text<'a>(text: &'a str, limits: &Limits) -> Result<Vec<KvDocument<'a>>, BspError> {
    if text.len() as u64 > limits.max_input_bytes {
        return Err(BspError::Entities(KvError::InputTooLarge {
            len: text.len() as u64,
            max: limits.max_input_bytes,
        }));
    }
    let tokens = keyvalues::tokenize(text, limits)?;
    let mut parser = keyvalues::Parser::new(&tokens, limits);
    let mut entities = Vec::new();
    while let Some(token) = parser.next_token() {
        if token == Token::Open {
            entities.push(parser.parse_block(1)?);
        }
    }
    Ok(entities)
}

fn own_document(document: KvDocument<'_>) -> KvDocument<'static> {
    KvDocument {
        pairs: document
            .pairs
            .into_iter()
            .map(|pair| keyvalues::KvPair {
                key: Cow::Owned(pair.key.into_owned()),
                value: match pair.value {
                    keyvalues::KvValue::String(value) => {
                        keyvalues::KvValue::String(Cow::Owned(value.into_owned()))
                    }
                    keyvalues::KvValue::Block(block) => {
                        keyvalues::KvValue::Block(own_document(block))
                    }
                },
            })
            .collect(),
    }
}
