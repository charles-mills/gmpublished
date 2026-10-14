//! Typed lump accessors: the fixed-stride record arrays of the BSP
//! wire format, decoded on demand from the validated directory.
//!
//! Record counts are the lump length divided by the record stride;
//! trailing partial bytes are tolerated (compilers pad some lumps).
//! Field names follow the engine's `bspfile.h` meanings with
//! idiomatic spelling.

use std::borrow::Cow;

use super::record::{f32_at, i16_at, i32_at, u16_at, vec3_at};
use super::{Bsp, BspError, lump_ids};
use crate::Limits;

/// `LUMP_TEXINFO` flag bits this crate names (the field carries the
/// full engine set; these are the commonly consumed ones).
#[allow(missing_docs)]
pub mod texture_flags {
    pub const LIGHT: i32 = 0x0001;
    pub const SKY2D: i32 = 0x0002;
    pub const SKY: i32 = 0x0004;
    pub const WARP: i32 = 0x0008;
    pub const TRANS: i32 = 0x0010;
    pub const NODRAW: i32 = 0x0080;
    pub const NOLIGHT: i32 = 0x0400;
}

/// Brush contents bits this crate names.
#[allow(missing_docs)]
pub mod contents_flags {
    pub const SOLID: i32 = 0x0000_0001;
    pub const WINDOW: i32 = 0x0000_0002;
    pub const GRATE: i32 = 0x0000_0008;
    pub const SLIME: i32 = 0x0000_0010;
    pub const WATER: i32 = 0x0000_0020;
    pub const PLAYERCLIP: i32 = 0x0001_0000;
}

/// One splitting plane (`LUMP_PLANES`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    /// Unit normal.
    pub normal: [f32; 3],
    /// Distance from origin along the normal.
    pub dist: f32,
    /// Axial classification (engine `type` field).
    pub axis_type: i32,
}

/// One surface (`LUMP_FACES`, 56-byte `dface_t`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Face {
    /// Index into [`Bsp::planes`].
    pub plane: u16,
    /// Which side of the plane the face is on.
    pub side: u8,
    /// Whether the face is on a BSP node.
    pub on_node: u8,
    /// First index into [`Bsp::surfedges`].
    pub first_edge: i32,
    /// Surfedge count.
    pub edge_count: i16,
    /// Index into [`Bsp::texinfos`], -1 for none.
    pub texinfo: i16,
    /// Index into [`Bsp::displacement_infos`], -1 for none.
    pub displacement: i16,
    /// Lightmap styles.
    pub styles: [u8; 4],
    /// Byte offset into the lighting lump, -1 for unlit.
    pub light_offset: i32,
    /// Face area in units².
    pub area: f32,
    /// Lightmap minimums in luxels.
    pub lightmap_mins: [i32; 2],
    /// Lightmap extents in luxels.
    pub lightmap_size: [i32; 2],
    /// The original face this was split from.
    pub original_face: i32,
}

/// One texture reference frame (`LUMP_TEXINFO`, 72 bytes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TexInfo {
    /// s/t texture axes: `[s, t]`, each `[x, y, z, offset]`.
    pub texture_vecs: [[f32; 4]; 2],
    /// s/t lightmap axes, same shape.
    pub lightmap_vecs: [[f32; 4]; 2],
    /// Surface flags (see [`texture_flags`]).
    pub flags: i32,
    /// Index into [`Bsp::texdatas`].
    pub texdata: i32,
}

/// One texture entry (`LUMP_TEXDATA`, 32 bytes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TexData {
    /// Average color.
    pub reflectivity: [f32; 3],
    /// Index into [`Bsp::texdata_strings`].
    pub name_index: i32,
    /// Texture dimensions.
    pub width: i32,
    /// See [`width`](Self::width).
    pub height: i32,
}

/// One brush model (`LUMP_MODELS`, 48 bytes) — model 0 is the world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BspModel {
    /// Bounding box minimum.
    pub mins: [f32; 3],
    /// Bounding box maximum.
    pub maxs: [f32; 3],
    /// Origin for entity models.
    pub origin: [f32; 3],
    /// Root node of the model's BSP subtree.
    pub head_node: i32,
    /// First index into [`Bsp::faces`].
    pub first_face: i32,
    /// Face count.
    pub face_count: i32,
}

/// One brush (`LUMP_BRUSHES`, 12 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Brush {
    /// First index into [`Bsp::brush_sides`].
    pub first_side: i32,
    /// Side count.
    pub side_count: i32,
    /// Contents bits (see [`contents_flags`]).
    pub contents: i32,
}

/// One brush side (`LUMP_BRUSHSIDES`, 8 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrushSide {
    /// Index into [`Bsp::planes`].
    pub plane: u16,
    /// Index into [`Bsp::texinfos`].
    pub texinfo: i16,
    /// Index into [`Bsp::displacement_infos`], -1 for none.
    pub displacement: i16,
    /// Nonzero for bevel planes (collision only).
    pub bevel: i16,
}

/// One BSP tree node (`LUMP_NODES`, 32 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    /// Index into [`Bsp::planes`].
    pub plane: i32,
    /// Children: non-negative = node index, negative = `-(leaf + 1)`.
    pub children: [i32; 2],
    /// Coarse integer bounds.
    pub mins: [i16; 3],
    /// See [`mins`](Self::mins).
    pub maxs: [i16; 3],
    /// First index into [`Bsp::faces`].
    pub first_face: u16,
    /// Face count.
    pub face_count: u16,
    /// Map area index.
    pub area: i16,
}

/// One BSP leaf (`LUMP_LEAFS`; lump version 0 embeds an ambient light
/// cube, version 1 does not — both parse to this).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Leaf {
    /// Contents bits (see [`contents_flags`]).
    pub contents: i32,
    /// Visibility cluster, -1 when outside the PVS.
    pub cluster: i16,
    /// Coarse integer bounds.
    pub mins: [i16; 3],
    /// See [`mins`](Self::mins).
    pub maxs: [i16; 3],
    /// First index into [`Bsp::leaf_faces`].
    pub first_leaf_face: u16,
    /// Leaf-face count.
    pub leaf_face_count: u16,
    /// First index into [`Bsp::leaf_brushes`].
    pub first_leaf_brush: u16,
    /// Leaf-brush count.
    pub leaf_brush_count: u16,
    /// Water data index, -1 for none.
    pub leaf_water_data: i16,
    area_flags: i16,
    /// Embedded ambient cube (lump version 0 only).
    pub ambient: Option<[ColorRgbExp; 6]>,
}

impl Leaf {
    /// Map area (packed field, low 9 bits).
    #[must_use]
    pub fn area(&self) -> i16 {
        self.area_flags & 0x01FF
    }

    /// Leaf flags (packed field, high 7 bits).
    #[must_use]
    pub fn flags(&self) -> i16 {
        self.area_flags >> 9
    }
}

/// One displacement descriptor (`LUMP_DISPINFO`, 176 bytes; the
/// neighbor tables are not currently decoded).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DispInfo {
    /// World position of the first corner.
    pub start_position: [f32; 3],
    /// First index into [`Bsp::displacement_verts`].
    pub vert_start: i32,
    /// First displacement triangle tag.
    pub tri_start: i32,
    /// Subdivision power (2–4; grid side = `2^power + 1`).
    pub power: i32,
    /// Minimum tesselation.
    pub min_tess: i32,
    /// Lighting smoothing angle in radians.
    pub smoothing_angle: f32,
    /// Contents bits.
    pub contents: i32,
    /// The face this displacement replaces.
    pub map_face: u16,
    /// Byte offset into the lightmap alpha data.
    pub lightmap_alpha_start: i32,
    /// Byte offset into the lightmap sample positions.
    pub lightmap_sample_position_start: i32,
}

/// One displacement vertex (`LUMP_DISP_VERTS`, 20 bytes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DispVert {
    /// Offset direction (unit).
    pub vector: [f32; 3],
    /// Offset distance along [`vector`](Self::vector).
    pub dist: f32,
    /// Blend alpha (0–255 stored as float).
    pub alpha: f32,
}

/// One decal overlay (`LUMP_OVERLAYS`, 352-byte `doverlay_t`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Overlay {
    /// Overlay id.
    pub id: i32,
    /// Index into [`Bsp::texinfos`].
    pub texinfo: i16,
    face_count_and_render_order: u16,
    faces: [i32; 64],
    /// Texture-space U extents `[start, end]`.
    pub u: [f32; 2],
    /// Texture-space V extents `[start, end]`.
    pub v: [f32; 2],
    /// The overlay quad's corners in basis space.
    pub uv_points: [[f32; 3]; 4],
    /// Overlay origin.
    pub origin: [f32; 3],
    /// Basis normal (the projection direction).
    pub basis_normal: [f32; 3],
}

impl Overlay {
    /// How many entries of the face table are used (packed field, low
    /// 14 bits).
    #[must_use]
    pub fn face_count(&self) -> usize {
        usize::from(self.face_count_and_render_order & 0x3FFF).min(64)
    }

    /// Render order (packed field, high 2 bits).
    #[must_use]
    pub fn render_order(&self) -> u16 {
        self.face_count_and_render_order >> 14
    }

    /// The faces the overlay projects onto: indices into
    /// [`Bsp::faces`], trimmed to [`face_count`](Self::face_count).
    #[must_use]
    pub fn faces(&self) -> &[i32] {
        &self.faces[..self.face_count()]
    }
}

/// One RGB sample with a shared exponent (`ColorRGBExp32`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorRgbExp {
    /// Red mantissa.
    pub r: u8,
    /// Green mantissa.
    pub g: u8,
    /// Blue mantissa.
    pub b: u8,
    /// Shared power-of-two exponent.
    pub exponent: i8,
}

impl ColorRgbExp {
    /// Decode to linear RGB (`mantissa * 2^exponent`).
    #[must_use]
    pub fn to_linear(self) -> [f32; 3] {
        let scale = exp2_i8(self.exponent);
        [
            f32::from(self.r) * scale,
            f32::from(self.g) * scale,
            f32::from(self.b) * scale,
        ]
    }
}

fn exp2_i8(e: i8) -> f32 {
    // Exponents land in (-127, 128): representable as f32 powers of two.
    let biased =
        u32::try_from(i32::from(e) + 127).expect("ColorRGBExp32 exponent is positive after bias");
    f32::from_bits(biased << 23)
}

/// One per-leaf ambient sample (`LUMP_LEAF_AMBIENT_LIGHTING*`, 28 B).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeafAmbientSample {
    /// Ambient cube: -x, +x, -y, +y, -z, +z.
    pub cube: [ColorRgbExp; 6],
    /// Sample position within the leaf, fixed-point 0–255 per axis.
    pub position: [u8; 3],
}

/// One per-leaf ambient index record (`LUMP_LEAF_AMBIENT_INDEX*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeafAmbientIndex {
    /// Sample count for the leaf.
    pub sample_count: u16,
    /// First index into the ambient sample lump.
    pub first_sample: u16,
}

// -----------------------------------------------------------------
// Record plumbing
// -----------------------------------------------------------------

fn color_at(record: &[u8], at: usize) -> ColorRgbExp {
    ColorRgbExp {
        r: record[at],
        g: record[at + 1],
        b: record[at + 2],
        exponent: i8::from_ne_bytes([record[at + 3]]),
    }
}

impl<'a> Bsp<'a> {
    fn records<T>(
        &self,
        lump: usize,
        stride: usize,
        _part: &'static str,
        limits: &Limits,
        read: impl Fn(&[u8]) -> T,
    ) -> Result<Vec<T>, BspError> {
        // No record-count cap: the count derives from lump bytes that are
        // already bounded (borrowed from the input, or capped by
        // `max_entry_bytes` when decompressed), and decoding is
        // size-proportional. A count cap here only rejects legitimate
        // dense lumps (lighting holds one record per luxel — millions on
        // ordinary maps).
        let bytes = self.lump_data(lump, limits)?;
        Ok(bytes.chunks_exact(stride).map(read).collect())
    }

    /// `LUMP_VERTICES`: world positions.
    pub fn vertices(&self, limits: &Limits) -> Result<Vec<[f32; 3]>, BspError> {
        self.records(lump_ids::VERTICES, 12, "vertices", limits, |r| {
            vec3_at(r, 0)
        })
    }

    /// `LUMP_PLANES`.
    pub fn planes(&self, limits: &Limits) -> Result<Vec<Plane>, BspError> {
        self.records(lump_ids::PLANES, 20, "planes", limits, |r| Plane {
            normal: vec3_at(r, 0),
            dist: f32_at(r, 12),
            axis_type: i32_at(r, 16),
        })
    }

    /// `LUMP_EDGES`: vertex index pairs.
    pub fn edges(&self, limits: &Limits) -> Result<Vec<[u16; 2]>, BspError> {
        self.records(lump_ids::EDGES, 4, "edges", limits, |r| {
            [u16_at(r, 0), u16_at(r, 2)]
        })
    }

    /// `LUMP_SURFEDGES`: signed edge references (negative = reversed).
    pub fn surfedges(&self, limits: &Limits) -> Result<Vec<i32>, BspError> {
        self.records(lump_ids::SURFEDGES, 4, "surfedges", limits, |r| {
            i32_at(r, 0)
        })
    }

    /// `LUMP_FACES`.
    pub fn faces(&self, limits: &Limits) -> Result<Vec<Face>, BspError> {
        self.records(lump_ids::FACES, 56, "faces", limits, |r| Face {
            plane: u16_at(r, 0),
            side: r[2],
            on_node: r[3],
            first_edge: i32_at(r, 4),
            edge_count: i16_at(r, 8),
            texinfo: i16_at(r, 10),
            displacement: i16_at(r, 12),
            styles: [r[16], r[17], r[18], r[19]],
            light_offset: i32_at(r, 20),
            area: f32_at(r, 24),
            lightmap_mins: [i32_at(r, 28), i32_at(r, 32)],
            lightmap_size: [i32_at(r, 36), i32_at(r, 40)],
            original_face: i32_at(r, 44),
        })
    }

    /// `LUMP_TEXINFO`.
    pub fn texinfos(&self, limits: &Limits) -> Result<Vec<TexInfo>, BspError> {
        self.records(lump_ids::TEXINFO, 72, "texinfos", limits, |r| TexInfo {
            texture_vecs: [
                [f32_at(r, 0), f32_at(r, 4), f32_at(r, 8), f32_at(r, 12)],
                [f32_at(r, 16), f32_at(r, 20), f32_at(r, 24), f32_at(r, 28)],
            ],
            lightmap_vecs: [
                [f32_at(r, 32), f32_at(r, 36), f32_at(r, 40), f32_at(r, 44)],
                [f32_at(r, 48), f32_at(r, 52), f32_at(r, 56), f32_at(r, 60)],
            ],
            flags: i32_at(r, 64),
            texdata: i32_at(r, 68),
        })
    }

    /// `LUMP_TEXDATA`.
    pub fn texdatas(&self, limits: &Limits) -> Result<Vec<TexData>, BspError> {
        self.records(lump_ids::TEXDATA, 32, "texdatas", limits, |r| TexData {
            reflectivity: vec3_at(r, 0),
            name_index: i32_at(r, 12),
            width: i32_at(r, 16),
            height: i32_at(r, 20),
        })
    }

    /// Texture names: `LUMP_TEXDATA_STRING_TABLE` offsets resolved into
    /// `LUMP_TEXDATA_STRING_DATA`, lossily decoded, indexed by
    /// [`TexData::name_index`].
    pub fn texdata_strings(&self, limits: &Limits) -> Result<Vec<Cow<'a, str>>, BspError> {
        fn resolve<'d>(data: &'d [u8], record: &[u8]) -> Cow<'d, str> {
            let at = usize::try_from(i32_at(record, 0)).unwrap_or(usize::MAX);
            let Some(rest) = data.get(at..) else {
                return Cow::Owned(String::new());
            };
            let nul = rest
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(rest.len());
            String::from_utf8_lossy(&rest[..nul])
        }
        let table = (lump_ids::TEXDATA_STRING_TABLE, 4, "texdata strings");
        // Strings borrow from a raw data lump; a decompressed one is a
        // temporary, so they must own.
        match self.lump_data(lump_ids::TEXDATA_STRING_DATA, limits)? {
            Cow::Borrowed(data) => {
                self.records(table.0, table.1, table.2, limits, |r| resolve(data, r))
            }
            Cow::Owned(data) => self.records(table.0, table.1, table.2, limits, |r| {
                Cow::Owned(resolve(&data, r).into_owned())
            }),
        }
    }

    /// `LUMP_MODELS`: brush models; index 0 is the worldspawn geometry.
    pub fn models(&self, limits: &Limits) -> Result<Vec<BspModel>, BspError> {
        self.records(lump_ids::MODELS, 48, "models", limits, |r| BspModel {
            mins: vec3_at(r, 0),
            maxs: vec3_at(r, 12),
            origin: vec3_at(r, 24),
            head_node: i32_at(r, 36),
            first_face: i32_at(r, 40),
            face_count: i32_at(r, 44),
        })
    }

    /// `LUMP_BRUSHES`.
    pub fn brushes(&self, limits: &Limits) -> Result<Vec<Brush>, BspError> {
        self.records(lump_ids::BRUSHES, 12, "brushes", limits, |r| Brush {
            first_side: i32_at(r, 0),
            side_count: i32_at(r, 4),
            contents: i32_at(r, 8),
        })
    }

    /// `LUMP_BRUSHSIDES`.
    pub fn brush_sides(&self, limits: &Limits) -> Result<Vec<BrushSide>, BspError> {
        self.records(lump_ids::BRUSHSIDES, 8, "brush sides", limits, |r| {
            BrushSide {
                plane: u16_at(r, 0),
                texinfo: i16_at(r, 2),
                displacement: i16_at(r, 4),
                bevel: i16_at(r, 6),
            }
        })
    }

    /// `LUMP_NODES`.
    pub fn nodes(&self, limits: &Limits) -> Result<Vec<Node>, BspError> {
        self.records(lump_ids::NODES, 32, "nodes", limits, |r| Node {
            plane: i32_at(r, 0),
            children: [i32_at(r, 4), i32_at(r, 8)],
            mins: [i16_at(r, 12), i16_at(r, 14), i16_at(r, 16)],
            maxs: [i16_at(r, 18), i16_at(r, 20), i16_at(r, 22)],
            first_face: u16_at(r, 24),
            face_count: u16_at(r, 26),
            area: i16_at(r, 28),
        })
    }

    /// `LUMP_LEAFS`. Lump version 0 (BSP 19 era) embeds a per-leaf
    /// ambient cube; version 1 does not — [`Leaf::ambient`] carries the
    /// difference.
    pub fn leafs(&self, limits: &Limits) -> Result<Vec<Leaf>, BspError> {
        let version = self.lump_version(lump_ids::LEAFS).unwrap_or(1);
        let (stride, with_ambient) = if version == 0 {
            (56, true)
        } else {
            (32, false)
        };
        self.records(lump_ids::LEAFS, stride, "leafs", limits, move |r| Leaf {
            contents: i32_at(r, 0),
            cluster: i16_at(r, 4),
            area_flags: i16_at(r, 6),
            mins: [i16_at(r, 8), i16_at(r, 10), i16_at(r, 12)],
            maxs: [i16_at(r, 14), i16_at(r, 16), i16_at(r, 18)],
            first_leaf_face: u16_at(r, 20),
            leaf_face_count: u16_at(r, 22),
            first_leaf_brush: u16_at(r, 24),
            leaf_brush_count: u16_at(r, 26),
            leaf_water_data: i16_at(r, 28),
            ambient: with_ambient.then(|| std::array::from_fn(|face| color_at(r, 30 + face * 4))),
        })
    }

    /// `LUMP_LEAF_FACES`: indices into [`Bsp::faces`].
    pub fn leaf_faces(&self, limits: &Limits) -> Result<Vec<u16>, BspError> {
        self.records(lump_ids::LEAF_FACES, 2, "leaf faces", limits, |r| {
            u16_at(r, 0)
        })
    }

    /// `LUMP_LEAF_BRUSHES`: indices into [`Bsp::brushes`].
    pub fn leaf_brushes(&self, limits: &Limits) -> Result<Vec<u16>, BspError> {
        self.records(lump_ids::LEAF_BRUSHES, 2, "leaf brushes", limits, |r| {
            u16_at(r, 0)
        })
    }

    /// `LUMP_DISPINFO`.
    pub fn displacement_infos(&self, limits: &Limits) -> Result<Vec<DispInfo>, BspError> {
        self.records(lump_ids::DISPINFO, 176, "displacements", limits, |r| {
            DispInfo {
                start_position: vec3_at(r, 0),
                vert_start: i32_at(r, 12),
                tri_start: i32_at(r, 16),
                power: i32_at(r, 20),
                min_tess: i32_at(r, 24),
                smoothing_angle: f32_at(r, 28),
                contents: i32_at(r, 32),
                map_face: u16_at(r, 36),
                lightmap_alpha_start: i32_at(r, 40),
                lightmap_sample_position_start: i32_at(r, 44),
            }
        })
    }

    /// `LUMP_DISP_VERTS`.
    pub fn displacement_verts(&self, limits: &Limits) -> Result<Vec<DispVert>, BspError> {
        self.records(
            lump_ids::DISP_VERTS,
            20,
            "displacement verts",
            limits,
            |r| DispVert {
                vector: vec3_at(r, 0),
                dist: f32_at(r, 12),
                alpha: f32_at(r, 16),
            },
        )
    }

    /// `LUMP_OVERLAYS`.
    pub fn overlays(&self, limits: &Limits) -> Result<Vec<Overlay>, BspError> {
        self.records(lump_ids::OVERLAYS, 352, "overlays", limits, |r| Overlay {
            id: i32_at(r, 0),
            texinfo: i16_at(r, 4),
            face_count_and_render_order: u16_at(r, 6),
            faces: std::array::from_fn(|face| i32_at(r, 8 + face * 4)),
            u: [f32_at(r, 264), f32_at(r, 268)],
            v: [f32_at(r, 272), f32_at(r, 276)],
            uv_points: std::array::from_fn(|point| vec3_at(r, 280 + point * 12)),
            origin: vec3_at(r, 328),
            basis_normal: vec3_at(r, 340),
        })
    }

    /// `LUMP_LIGHTING` (LDR) samples; [`Face::light_offset`] is a BYTE
    /// offset into this lump (divide by 4 to index).
    pub fn lighting(&self, limits: &Limits) -> Result<Vec<ColorRgbExp>, BspError> {
        self.records(lump_ids::LIGHTING, 4, "lighting", limits, |r| {
            color_at(r, 0)
        })
    }

    /// `LUMP_LIGHTING_HDR` samples (empty on LDR-only maps).
    pub fn lighting_hdr(&self, limits: &Limits) -> Result<Vec<ColorRgbExp>, BspError> {
        self.records(lump_ids::LIGHTING_HDR, 4, "hdr lighting", limits, |r| {
            color_at(r, 0)
        })
    }

    /// `LUMP_LEAF_AMBIENT_LIGHTING` samples.
    pub fn leaf_ambient_lighting(
        &self,
        limits: &Limits,
    ) -> Result<Vec<LeafAmbientSample>, BspError> {
        self.ambient_samples(lump_ids::LEAF_AMBIENT_LIGHTING, "leaf ambient", limits)
    }

    /// `LUMP_LEAF_AMBIENT_LIGHTING_HDR` samples.
    pub fn leaf_ambient_lighting_hdr(
        &self,
        limits: &Limits,
    ) -> Result<Vec<LeafAmbientSample>, BspError> {
        self.ambient_samples(
            lump_ids::LEAF_AMBIENT_LIGHTING_HDR,
            "hdr leaf ambient",
            limits,
        )
    }

    fn ambient_samples(
        &self,
        lump: usize,
        part: &'static str,
        limits: &Limits,
    ) -> Result<Vec<LeafAmbientSample>, BspError> {
        self.records(lump, 28, part, limits, |r| LeafAmbientSample {
            cube: std::array::from_fn(|face| color_at(r, face * 4)),
            position: [r[24], r[25], r[26]],
        })
    }

    /// `LUMP_LEAF_AMBIENT_INDEX`: per-leaf spans into
    /// [`Bsp::leaf_ambient_lighting`].
    pub fn leaf_ambient_indices(&self, limits: &Limits) -> Result<Vec<LeafAmbientIndex>, BspError> {
        self.ambient_indices(lump_ids::LEAF_AMBIENT_INDEX, "leaf ambient index", limits)
    }

    /// `LUMP_LEAF_AMBIENT_INDEX_HDR`: per-leaf spans into
    /// [`Bsp::leaf_ambient_lighting_hdr`].
    pub fn leaf_ambient_indices_hdr(
        &self,
        limits: &Limits,
    ) -> Result<Vec<LeafAmbientIndex>, BspError> {
        self.ambient_indices(
            lump_ids::LEAF_AMBIENT_INDEX_HDR,
            "hdr leaf ambient index",
            limits,
        )
    }

    fn ambient_indices(
        &self,
        lump: usize,
        part: &'static str,
        limits: &Limits,
    ) -> Result<Vec<LeafAmbientIndex>, BspError> {
        self.records(lump, 4, part, limits, |r| LeafAmbientIndex {
            sample_count: u16_at(r, 0),
            first_sample: u16_at(r, 2),
        })
    }
}
