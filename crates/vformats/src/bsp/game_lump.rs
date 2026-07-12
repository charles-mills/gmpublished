//! The game lump (`LUMP_GAME_LUMP`): a directory of engine-specific
//! sub-lumps addressed by absolute file offsets, most importantly the
//! static props (`sprp`) and detail props (`dprp`).
//!
//! Static prop records are a version zoo: the record layout varies not
//! just by declared version but by engine branch (the 2013 multiplayer
//! branch's version 10 is 72 bytes with lightmap fields; other
//! branches' version 10 is 76 bytes with `FlagsEx`). Layouts are
//! resolved from the (version, record size) pair against the known
//! table; unknown pairs fall back to the version-4 core fields every
//! branch shares, with the outcome reported in
//! [`StaticProps::layout`]. Consumers needing branch-specific tails
//! can re-read [`Bsp::game_lump`] raw.

use std::borrow::Cow;

use super::record::{f32_at, i32_at, u16_at, u32_at, vec3_at};
use super::{Bsp, BspError, lumps::ColorRgbExp};
use crate::Limits;

/// Well-known game lump ids (stored big-endian so the byte order in
/// the file spells the tag).
#[allow(missing_docs)]
pub mod game_lump_ids {
    pub const STATIC_PROPS: i32 = i32::from_be_bytes(*b"sprp");
    pub const DETAIL_PROPS: i32 = i32::from_be_bytes(*b"dprp");
}

const FLAG_COMPRESSED: u16 = 0x0001;
const ENTRY_BYTES: usize = 16;

/// One game lump directory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameLumpEntry {
    /// Sub-lump id (see [`game_lump_ids`]).
    pub id: i32,
    /// Flag bits (bit 0 = LZMA-compressed).
    pub flags: u16,
    /// Sub-lump version.
    pub version: u16,
    /// Absolute file offset of the data.
    pub offset: usize,
    /// Uncompressed data length.
    pub len: usize,
}

impl GameLumpEntry {
    /// Whether the sub-lump's data is LZMA-compressed.
    #[must_use]
    pub fn is_compressed(&self) -> bool {
        self.flags & FLAG_COMPRESSED != 0
    }
}

/// One game lump's directory entry and data, from [`Bsp::game_lump`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameLump<'a> {
    /// The directory entry.
    pub entry: GameLumpEntry,
    /// The sub-lump's bytes (owned when decompressed).
    pub data: Cow<'a, [u8]>,
}

/// The parsed `sprp` game lump.
#[derive(Clone, Debug, PartialEq)]
pub struct StaticProps {
    /// The sub-lump's declared version.
    pub version: u16,
    /// Which record layout the (version, record size) pair resolved to.
    pub layout: StaticPropLayout,
    /// Bytes per prop record (0 when there are no props).
    pub stride: usize,
    /// Model dictionary: prop records index into this by
    /// [`StaticProp::model_index`].
    pub models: Vec<String>,
    /// Leaf table: prop records span it via
    /// [`StaticProp::first_leaf`] / [`StaticProp::leaf_count`].
    pub leaves: Vec<u16>,
    /// The prop placements.
    pub props: Vec<StaticProp>,
}

/// Which static prop record layout was decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StaticPropLayout {
    /// The `bspfile.h` layout for the declared version (4–11).
    Standard,
    /// The Source SDK 2013 multiplayer branch's 72-byte version-10
    /// layout: DirectX levels, a second flags word, and a lightmap
    /// resolution instead of `DisableX360`/`FlagsEx`.
    Multiplayer2013,
    /// Unrecognized (version, record size) pair: only the core fields
    /// every branch shares were decoded (plus the fade scale when the
    /// record is large enough to carry it).
    Core,
}

/// One static prop placement. Core fields are present in every layout;
/// the optional tail fields depend on [`StaticProps::layout`] and
/// version.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct StaticProp {
    /// World position.
    pub origin: [f32; 3],
    /// Orientation as `[pitch, yaw, roll]` degrees (`QAngle`).
    pub angles: [f32; 3],
    /// Index into [`StaticProps::models`].
    pub model_index: u16,
    /// First index into [`StaticProps::leaves`].
    pub first_leaf: u16,
    /// Leaf span length.
    pub leaf_count: u16,
    /// Collision mode (0 none, 1 BSP, 2 bounding box, 6 vphysics).
    pub solid: u8,
    /// Legacy flag byte.
    pub flags: u8,
    /// Skin index.
    pub skin: i32,
    /// Fade distances in units.
    pub fade_min_distance: f32,
    /// See [`fade_min_distance`](Self::fade_min_distance).
    pub fade_max_distance: f32,
    /// Lighting sample position (when the flag is set).
    pub lighting_origin: [f32; 3],
    /// Forced fade scale (version 5+).
    pub forced_fade_scale: Option<f32>,
    /// Min/max DirectX level (standard versions 6–7, 2013 MP).
    pub dx_levels: Option<[u16; 2]>,
    /// Min/max CPU then min/max GPU level (standard version 8+).
    pub cpu_gpu_levels: Option<[u8; 4]>,
    /// RGBA diffuse modulation (standard version 7+).
    pub diffuse_modulation: Option<[u8; 4]>,
    /// Whether the prop is disabled on Xbox 360 (standard versions
    /// 9–10).
    pub disable_x360: Option<bool>,
    /// Extra engine flags (standard version 10+).
    pub flags_ex: Option<u32>,
    /// Uniform scale factor (standard version 11).
    pub uniform_scale: Option<f32>,
    /// Second flags word (2013 MP layout).
    pub extra_flags: Option<u32>,
    /// Lightmap resolution (2013 MP layout).
    pub lightmap_resolution: Option<[u16; 2]>,
}

/// The parsed `dprp` game lump.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DetailProps {
    /// Model dictionary (`.mdl` paths).
    pub models: Vec<String>,
    /// Sprite dictionary.
    pub sprites: Vec<DetailSprite>,
    /// The detail placements.
    pub props: Vec<DetailProp>,
    /// Whether a section was cut short: everything before the ragged
    /// section is intact, the rest is missing (real maps carry ragged
    /// detail prop lumps, so this is tolerance, not failure).
    pub truncated: bool,
}

/// One detail sprite dictionary entry (world and texture extents).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetailSprite {
    /// World-space upper left.
    pub upper_left: [f32; 2],
    /// World-space lower right.
    pub lower_right: [f32; 2],
    /// Texture-space upper left.
    pub tex_upper_left: [f32; 2],
    /// Texture-space lower right.
    pub tex_lower_right: [f32; 2],
}

/// One detail prop placement.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct DetailProp {
    /// World position.
    pub origin: [f32; 3],
    /// Orientation as `[pitch, yaw, roll]` degrees.
    pub angles: [f32; 3],
    /// Index into [`DetailProps::models`] (model type) or the sprite
    /// dictionary, per [`prop_type`](Self::prop_type).
    pub model_index: u16,
    /// The leaf containing the prop.
    pub leaf: u16,
    /// Baked lighting.
    pub lighting: ColorRgbExp,
    /// Light style index data.
    pub light_styles: u32,
    /// Light style count.
    pub light_style_count: u8,
    /// Wind sway amount.
    pub sway_amount: u8,
    /// Shape angle (procedural sprites).
    pub shape_angle: u8,
    /// Shape size (procedural sprites).
    pub shape_size: u8,
    /// Orientation mode (0 normal, 1 screen-aligned, 2 vertical).
    pub orientation: u8,
    /// Prop type (0 model, 1 sprite, 2+ procedural shapes).
    pub prop_type: u8,
}

impl<'a> Bsp<'a> {
    /// The game lump directory. Empty when the map has no game lump.
    pub fn game_lumps(&self, limits: &Limits) -> Result<Vec<GameLumpEntry>, BspError> {
        // Entries hold absolute file offsets, so a fourCC-compressed
        // game lump (never produced by Valve's tools) is unreadable.
        if self.lump_compression(super::lump_ids::GAME_LUMP).is_some() {
            return Err(BspError::CompressedLump {
                index: super::lump_ids::GAME_LUMP,
            });
        }
        let bytes = self.lump(super::lump_ids::GAME_LUMP).unwrap_or_default();
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let malformed = BspError::Decode {
            part: "game lump directory",
        };
        if bytes.len() < 4 {
            return Err(malformed);
        }
        let count = i32::from_le_bytes(bytes[0..4].try_into().expect("4 bytes"));
        let count = usize::try_from(count).map_err(|_| malformed.clone())?;
        if count > limits.max_entries {
            return Err(BspError::TooManyRecords {
                part: "game lumps",
                max: limits.max_entries,
            });
        }
        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            let at = 4 + index * ENTRY_BYTES;
            let entry = bytes
                .get(at..at + ENTRY_BYTES)
                .ok_or_else(|| malformed.clone())?;
            let offset = i32::from_le_bytes(entry[8..12].try_into().expect("4 bytes"));
            let len = i32::from_le_bytes(entry[12..16].try_into().expect("4 bytes"));
            let (Ok(offset), Ok(len)) = (usize::try_from(offset), usize::try_from(len)) else {
                return Err(malformed);
            };
            entries.push(GameLumpEntry {
                id: i32::from_le_bytes(entry[0..4].try_into().expect("4 bytes")),
                flags: u16::from_le_bytes(entry[4..6].try_into().expect("2 bytes")),
                version: u16::from_le_bytes(entry[6..8].try_into().expect("2 bytes")),
                offset,
                len,
            });
        }
        Ok(entries)
    }

    /// One game lump's directory entry and data by id, decompressing
    /// compressed sub-lumps (`lzma` feature). `None` when the id is
    /// absent.
    pub fn game_lump(&self, id: i32, limits: &Limits) -> Result<Option<GameLump<'a>>, BspError> {
        let entries = self.game_lumps(limits)?;
        let Some(at) = entries.iter().position(|entry| entry.id == id) else {
            return Ok(None);
        };
        let entry = entries[at];
        let malformed = BspError::Decode { part: "game lump" };
        if entry.len == 0 {
            return Ok(Some(GameLump {
                entry,
                data: Cow::Borrowed(&[]),
            }));
        }
        if !entry.is_compressed() {
            let end = entry
                .offset
                .checked_add(entry.len)
                .ok_or_else(|| malformed.clone())?;
            let data = self.bytes.get(entry.offset..end).ok_or(malformed)?;
            return Ok(Some(GameLump {
                entry,
                data: Cow::Borrowed(data),
            }));
        }
        // A compressed sub-lump's extent runs to the next entry's
        // offset (`len` is the uncompressed size); writers close the
        // list with a sentinel entry. Some writers give the sentinel a
        // zero offset instead of the data's end, so when the next
        // offset can't bound this entry, fall back to the game lump's
        // own end (sub-lump data sits inside its extent). The LZMA
        // header's stream-size field trims any overshoot.
        let game_lump_end = self
            .lumps
            .get(super::lump_ids::GAME_LUMP)
            .map(|lump| lump.offset.saturating_add(lump.len))
            .unwrap_or_default();
        let end = entries
            .get(at + 1)
            .map(|next| next.offset)
            .filter(|&next_offset| next_offset > entry.offset)
            .unwrap_or(game_lump_end);
        #[cfg(feature = "lzma")]
        {
            let raw = self.bytes.get(entry.offset..end).ok_or(malformed)?;
            let data = super::decompress_valve_lzma(raw, "game lump", limits)?;
            Ok(Some(GameLump {
                entry,
                data: Cow::Owned(data),
            }))
        }
        #[cfg(not(feature = "lzma"))]
        {
            let _ = end;
            Err(BspError::CompressedLump {
                index: super::lump_ids::GAME_LUMP,
            })
        }
    }

    /// The static props (`sprp`) game lump; `None` when absent or
    /// present but empty. Structurally strict (truncated dictionaries
    /// fail); tolerant of unknown record layouts (see
    /// [`StaticPropLayout::Core`]).
    pub fn static_props(&self, limits: &Limits) -> Result<Option<StaticProps>, BspError> {
        let Some(lump) = self.game_lump(game_lump_ids::STATIC_PROPS, limits)? else {
            return Ok(None);
        };
        if lump.data.is_empty() {
            return Ok(None);
        }
        parse_static_props(lump.entry.version, &lump.data, limits).map(Some)
    }

    /// The detail props (`dprp`) game lump; `None` when absent or
    /// present but empty. Tolerant like the reference implementations:
    /// a truncated section keeps everything complete before it, and
    /// [`DetailProps::truncated`] says whether that happened.
    pub fn detail_props(&self, limits: &Limits) -> Result<Option<DetailProps>, BspError> {
        let Some(lump) = self.game_lump(game_lump_ids::DETAIL_PROPS, limits)? else {
            return Ok(None);
        };
        if lump.data.is_empty() {
            return Ok(None);
        }
        Ok(Some(parse_detail_props(&lump.data, limits)))
    }
}

const CORE_BYTES: usize = 56;
const NAME_BYTES: usize = 128;

fn read_count(
    data: &[u8],
    at: &mut usize,
    part: &'static str,
    limits: &Limits,
) -> Result<usize, BspError> {
    let malformed = BspError::Decode { part };
    let bytes = data.get(*at..*at + 4).ok_or_else(|| malformed.clone())?;
    *at += 4;
    let count = i32::from_le_bytes(bytes.try_into().expect("4 bytes"));
    let count = usize::try_from(count).map_err(|_| malformed)?;
    if count > limits.max_entries {
        return Err(BspError::TooManyRecords {
            part,
            max: limits.max_entries,
        });
    }
    Ok(count)
}

fn nul_string(record: &[u8]) -> String {
    let end = record
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(record.len());
    String::from_utf8_lossy(&record[..end]).into_owned()
}

fn parse_static_props(version: u16, data: &[u8], limits: &Limits) -> Result<StaticProps, BspError> {
    let malformed = BspError::Decode {
        part: "static props",
    };
    let mut at = 0;
    let model_count = read_count(data, &mut at, "static prop models", limits)?;
    let mut models = Vec::with_capacity(model_count.min(1024));
    for _ in 0..model_count {
        let name = data
            .get(at..at + NAME_BYTES)
            .ok_or_else(|| malformed.clone())?;
        models.push(nul_string(name));
        at += NAME_BYTES;
    }
    let leaf_count = read_count(data, &mut at, "static prop leaves", limits)?;
    let leaf_bytes = data
        .get(at..at + leaf_count * 2)
        .ok_or_else(|| malformed.clone())?;
    let leaves = leaf_bytes
        .chunks_exact(2)
        .map(|pair| u16_at(pair, 0))
        .collect();
    at += leaf_count * 2;
    let prop_count = read_count(data, &mut at, "static props", limits)?;

    if prop_count == 0 {
        return Ok(StaticProps {
            version,
            layout: StaticPropLayout::Standard,
            stride: 0,
            models,
            leaves,
            props: Vec::new(),
        });
    }
    let remaining = data.len() - at;
    let (stride, layout) =
        resolve_static_prop_layout(version, prop_count, remaining).ok_or(malformed)?;
    let mut props = Vec::with_capacity(prop_count.min(4096));
    for index in 0..prop_count {
        let record = &data[at + index * stride..at + (index + 1) * stride];
        props.push(read_static_prop(version, layout, record));
    }
    Ok(StaticProps {
        version,
        layout,
        stride,
        models,
        leaves,
        props,
    })
}

/// The known (version, record size) table. Trailing slack under one
/// record (alignment padding) is tolerated; anything larger means the
/// declared version's layout does not match and the core fallback
/// applies.
fn resolve_static_prop_layout(
    version: u16,
    count: usize,
    remaining: usize,
) -> Option<(usize, StaticPropLayout)> {
    let fits = |stride: usize| {
        count
            .checked_mul(stride)
            .is_some_and(|need| need <= remaining && remaining - need < stride)
    };
    let standard = match version {
        4 => Some(56),
        5 => Some(60),
        6 => Some(64),
        7 => Some(68),
        8 => Some(68),
        9 => Some(72),
        10 => Some(76),
        11 => Some(80),
        _ => None,
    };
    if let Some(stride) = standard.filter(|stride| fits(*stride)) {
        return Some((stride, StaticPropLayout::Standard));
    }
    if version == 10 && fits(72) {
        return Some((72, StaticPropLayout::Multiplayer2013));
    }
    let derived = remaining / count;
    (derived >= CORE_BYTES).then_some((derived, StaticPropLayout::Core))
}

fn read_static_prop(version: u16, layout: StaticPropLayout, record: &[u8]) -> StaticProp {
    let mut prop = StaticProp {
        origin: vec3_at(record, 0),
        angles: vec3_at(record, 12),
        model_index: u16_at(record, 24),
        first_leaf: u16_at(record, 26),
        leaf_count: u16_at(record, 28),
        solid: record[30],
        flags: record[31],
        skin: i32_at(record, 32),
        fade_min_distance: f32_at(record, 36),
        fade_max_distance: f32_at(record, 40),
        lighting_origin: vec3_at(record, 44),
        forced_fade_scale: None,
        dx_levels: None,
        cpu_gpu_levels: None,
        diffuse_modulation: None,
        disable_x360: None,
        flags_ex: None,
        uniform_scale: None,
        extra_flags: None,
        lightmap_resolution: None,
    };
    if record.len() >= 60 && (version >= 5 || layout == StaticPropLayout::Core) {
        prop.forced_fade_scale = Some(f32_at(record, 56));
    }
    match layout {
        StaticPropLayout::Standard => {
            match version {
                6 | 7 => prop.dx_levels = Some([u16_at(record, 60), u16_at(record, 62)]),
                8.. => {
                    prop.cpu_gpu_levels = Some([record[60], record[61], record[62], record[63]]);
                }
                _ => {}
            }
            if version >= 7 {
                prop.diffuse_modulation = Some([record[64], record[65], record[66], record[67]]);
            }
            if version >= 9 {
                prop.disable_x360 = Some(u32_at(record, 68) != 0);
            }
            if version >= 10 {
                prop.flags_ex = Some(u32_at(record, 72));
            }
            if version >= 11 {
                prop.uniform_scale = Some(f32_at(record, 76));
            }
        }
        StaticPropLayout::Multiplayer2013 => {
            prop.dx_levels = Some([u16_at(record, 60), u16_at(record, 62)]);
            prop.extra_flags = Some(u32_at(record, 64));
            prop.lightmap_resolution = Some([u16_at(record, 68), u16_at(record, 70)]);
        }
        StaticPropLayout::Core => {}
    }
    prop
}

/// The largest whole-record prefix of a section, and whether the whole
/// requested section was present.
fn complete_records(data: &[u8], at: usize, count: usize, stride: usize) -> (&[u8], bool) {
    let requested = count.saturating_mul(stride);
    let available = data.len().saturating_sub(at);
    let complete = available.min(requested) / stride * stride;
    let end = at.saturating_add(complete).min(data.len());
    (data.get(at..end).unwrap_or_default(), complete == requested)
}

fn parse_detail_props(data: &[u8], limits: &Limits) -> DetailProps {
    const SPRITE_BYTES: usize = 32;
    let mut out = DetailProps::default();
    let mut at = 0;

    let Ok(model_count) = read_count(data, &mut at, "detail prop models", limits) else {
        out.truncated = true;
        return out;
    };
    let (names, complete) = complete_records(data, at, model_count, NAME_BYTES);
    out.models = names.chunks_exact(NAME_BYTES).map(nul_string).collect();
    if !complete {
        out.truncated = true;
        return out;
    }
    at += model_count * NAME_BYTES;

    let Ok(sprite_count) = read_count(data, &mut at, "detail prop sprites", limits) else {
        out.truncated = true;
        return out;
    };
    let (sprites, complete) = complete_records(data, at, sprite_count, SPRITE_BYTES);
    out.sprites = sprites
        .chunks_exact(SPRITE_BYTES)
        .map(|r| DetailSprite {
            upper_left: [f32_at(r, 0), f32_at(r, 4)],
            lower_right: [f32_at(r, 8), f32_at(r, 12)],
            tex_upper_left: [f32_at(r, 16), f32_at(r, 20)],
            tex_lower_right: [f32_at(r, 24), f32_at(r, 28)],
        })
        .collect();
    if !complete {
        out.truncated = true;
        return out;
    }
    at += sprite_count * SPRITE_BYTES;

    let Ok(prop_count) = read_count(data, &mut at, "detail props", limits) else {
        out.truncated = true;
        return out;
    };
    if prop_count == 0 {
        return out;
    }
    // Two record sizes exist in the wild: the SDK's 48 bytes (prop
    // type at 44) and an older 44 (prop type at 41). Prefer 48 when
    // the section fits it exactly-ish (slack under one record).
    let remaining = data.len().saturating_sub(at);
    let stride = if prop_count
        .checked_mul(48)
        .is_some_and(|need| need <= remaining && remaining - need < 48)
    {
        48
    } else {
        44
    };
    let type_at = if stride >= 48 { 44 } else { 41 };
    let (props, complete) = complete_records(data, at, prop_count, stride);
    out.truncated = !complete;
    out.props = props
        .chunks_exact(stride)
        .map(|r| DetailProp {
            origin: vec3_at(r, 0),
            angles: vec3_at(r, 12),
            model_index: u16_at(r, 24),
            leaf: u16_at(r, 26),
            lighting: ColorRgbExp {
                r: r[28],
                g: r[29],
                b: r[30],
                exponent: i8::from_ne_bytes([r[31]]),
            },
            light_styles: u32_at(r, 32),
            light_style_count: r[36],
            sway_amount: r[37],
            shape_angle: r[38],
            shape_size: r[39],
            orientation: r[40],
            prop_type: r[type_at],
        })
        .collect();
    out
}
