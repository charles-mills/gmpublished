//! `.vtx` optimized mesh data: the triangle indices that turn `.vvd`
//! vertices into render geometry, organized bodypart → model → LOD →
//! mesh → strip group → strip.
//!
//! All offsets in the tree are relative to the header they appear in
//! (the wire format's convention). Strip decoding matches the `vmdl`
//! crate's emission order — per-triangle index reversal for lists and
//! strips alike, whole-sequence reversal for tri-lists — because the
//! downstream winding flip was pixel-verified against that order (on
//! tri-lists, the only topology dx90 content uses). The tri-strip path
//! deliberately diverges: vmdl iterates `0..len` (two trailing
//! triangles index out of bounds) and its `idx + 1 - cw` alternation
//! makes every odd triangle degenerate (`[i, i, i+1]`); this parser
//! emits the correct `len - 2` triangles with D3D winding alternation.

use super::{MdlError, Reader};
use crate::Limits;

const VTX_VERSION: i32 = 7;

/// Parsed `.vtx` content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vtx {
    /// Checksum shared with the sibling `.mdl` and `.vvd`.
    pub checksum: u32,
    /// Body parts, matching the `.mdl`'s bodypart order.
    pub body_parts: Vec<VtxBodyPart>,
}

/// One body part: its models (bodygroup choices).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VtxBodyPart {
    /// Models in choice order.
    pub models: Vec<VtxModel>,
}

/// One model: its LODs, coarsest last; LOD 0 is full detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VtxModel {
    /// Level-of-detail meshes.
    pub lods: Vec<VtxLod>,
}

/// One LOD: its meshes, matching the `.mdl` mesh order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VtxLod {
    /// Meshes in `.mdl` order.
    pub meshes: Vec<VtxMesh>,
}

/// One mesh: its strip groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VtxMesh {
    /// Strip groups.
    pub strip_groups: Vec<VtxStripGroup>,
}

/// A strip group: shared vertex/index pools plus the strips that
/// consume ranges of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VtxStripGroup {
    /// Vertex records mapping into the mesh's `.vvd` vertices.
    pub vertices: Vec<VtxVertex>,
    /// Index pool; values index [`vertices`](Self::vertices).
    pub indices: Vec<u16>,
    /// Strips consuming ranges of the pools.
    pub strips: Vec<VtxStrip>,
}

/// One strip-group vertex record (9 bytes on the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VtxVertex {
    /// Per-strip bone weight slot indices.
    pub bone_weight_indexes: [u8; 3],
    /// Used bone slots.
    pub bone_count: u8,
    /// Index into the mesh's `.vvd` vertices (pre-offset).
    pub original_mesh_vertex_id: u16,
    /// Hardware bone ids.
    pub bone_ids: [u8; 3],
}

/// One strip: a range of the group's index pool plus its topology flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VtxStrip {
    /// First element in the group's index pool.
    pub index_start: usize,
    /// Element count.
    pub index_count: usize,
    /// Topology: bit 0 = triangle list, bit 1 = triangle strip.
    pub flags: u8,
}

const STRIP_IS_TRI_STRIP: u8 = 0x02;

impl VtxStrip {
    /// Element positions into the group's index pool, in triangle
    /// emission order (matches vmdl: reversed per triangle; tri-lists
    /// reversed as a whole sequence).
    pub fn triangle_index_positions(&self) -> impl Iterator<Item = usize> + 'static {
        let start = self.index_start;
        let count = self.index_count;
        if self.flags & STRIP_IS_TRI_STRIP != 0 {
            Iterate::Strip((0..count.saturating_sub(2)).flat_map(move |i| {
                let idx = start + i;
                if i & 1 == 0 {
                    [idx + 2, idx + 1, idx]
                } else {
                    [idx + 1, idx + 2, idx]
                }
            }))
        } else {
            Iterate::List((0..count).rev().map(move |i| start + i))
        }
    }
}

/// Two-armed iterator without an itertools dependency.
enum Iterate<A, B> {
    Strip(A),
    List(B),
}

impl<A: Iterator<Item = usize>, B: Iterator<Item = usize>> Iterator for Iterate<A, B> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        match self {
            Self::Strip(inner) => inner.next(),
            Self::List(inner) => inner.next(),
        }
    }
}

/// Parse a `.vtx` file.
pub fn parse_vtx(bytes: &[u8], limits: &Limits) -> Result<Vtx, MdlError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(MdlError::InputTooLarge {
            len: bytes.len() as u64,
            max: limits.max_input_bytes,
        });
    }
    let mut r = Reader::at(bytes, 0);
    let version = r.i32()?;
    if version != VTX_VERSION {
        return Err(MdlError::UnsupportedVersion {
            part: "vtx",
            version,
        });
    }
    r.take(4 + 2 + 2 + 4)?; // cache size, bones per strip/tri/vertex
    let checksum = r.u32()?;
    r.take(4 + 4)?; // lod count, material replacement list offset
    let body_part_count = r.count("vtx body part count")?;
    let body_part_offset = r.count("vtx body part offset")?;

    if body_part_count > limits.max_entries {
        return Err(MdlError::TooMany {
            part: "vtx body parts",
            max: limits.max_entries,
        });
    }
    let mut body_parts = Vec::with_capacity(body_part_count);
    for index in 0..body_part_count {
        let base = body_part_offset
            .checked_add(index * 8)
            .ok_or(MdlError::Corrupt {
                part: "vtx body part offset",
            })?;
        body_parts.push(read_body_part(bytes, base, limits)?);
    }

    Ok(Vtx {
        checksum,
        body_parts,
    })
}

fn read_body_part(bytes: &[u8], base: usize, limits: &Limits) -> Result<VtxBodyPart, MdlError> {
    let mut r = Reader::at(bytes, base);
    let count = r.count("vtx model count")?;
    let offset = r.count("vtx model offset")?;
    let models = read_children(
        bytes,
        base,
        offset,
        count,
        8,
        limits,
        "vtx models",
        read_model,
    )?;
    Ok(VtxBodyPart { models })
}

fn read_model(bytes: &[u8], base: usize, limits: &Limits) -> Result<VtxModel, MdlError> {
    let mut r = Reader::at(bytes, base);
    let count = r.count("vtx lod count")?;
    let offset = r.count("vtx lod offset")?;
    let lods = read_children(bytes, base, offset, count, 12, limits, "vtx lods", read_lod)?;
    Ok(VtxModel { lods })
}

fn read_lod(bytes: &[u8], base: usize, limits: &Limits) -> Result<VtxLod, MdlError> {
    let mut r = Reader::at(bytes, base);
    let count = r.count("vtx mesh count")?;
    let offset = r.count("vtx mesh offset")?;
    // switch_point f32 follows; unused here.
    let meshes = read_children(
        bytes,
        base,
        offset,
        count,
        9,
        limits,
        "vtx meshes",
        read_mesh,
    )?;
    Ok(VtxLod { meshes })
}

fn read_mesh(bytes: &[u8], base: usize, limits: &Limits) -> Result<VtxMesh, MdlError> {
    let mut r = Reader::at(bytes, base);
    let count = r.count("vtx strip group count")?;
    let offset = r.count("vtx strip group offset")?;
    // flags u8 follows; unused here. Strip-group headers are 25 bytes.
    let strip_groups = read_children(
        bytes,
        base,
        offset,
        count,
        25,
        limits,
        "vtx strip groups",
        read_strip_group,
    )?;
    Ok(VtxMesh { strip_groups })
}

fn read_strip_group(bytes: &[u8], base: usize, limits: &Limits) -> Result<VtxStripGroup, MdlError> {
    let mut r = Reader::at(bytes, base);
    let vertex_count = r.count("vtx group vertex count")?;
    let vertex_offset = r.count("vtx group vertex offset")?;
    let index_count = r.count("vtx group index count")?;
    let index_offset = r.count("vtx group index offset")?;
    let strip_count = r.count("vtx group strip count")?;
    let strip_offset = r.count("vtx group strip offset")?;

    for (count, part) in [
        (vertex_count, "vtx group vertices"),
        (index_count, "vtx group indices"),
        (strip_count, "vtx group strips"),
    ] {
        if count > limits.max_entries {
            return Err(MdlError::TooMany {
                part,
                max: limits.max_entries,
            });
        }
    }

    let mut vertices = Vec::with_capacity(vertex_count);
    let vertex_base = base.checked_add(vertex_offset).ok_or(MdlError::Corrupt {
        part: "vtx group vertex offset",
    })?;
    let mut vr = Reader::at(bytes, vertex_base);
    for _ in 0..vertex_count {
        let v = vr.take(9)?;
        vertices.push(VtxVertex {
            bone_weight_indexes: [v[0], v[1], v[2]],
            bone_count: v[3],
            original_mesh_vertex_id: u16::from_le_bytes([v[4], v[5]]),
            bone_ids: [v[6], v[7], v[8]],
        });
    }

    let mut indices = Vec::with_capacity(index_count);
    let index_base = base.checked_add(index_offset).ok_or(MdlError::Corrupt {
        part: "vtx group index offset",
    })?;
    let mut ir = Reader::at(bytes, index_base);
    for _ in 0..index_count {
        let i = ir.take(2)?;
        indices.push(u16::from_le_bytes([i[0], i[1]]));
    }

    let mut strips = Vec::with_capacity(strip_count);
    let strip_base = base.checked_add(strip_offset).ok_or(MdlError::Corrupt {
        part: "vtx group strip offset",
    })?;
    for index in 0..strip_count {
        // Strip headers are 27 bytes: index count/offset, vertex
        // count/offset, bone count u16, flags u8, bone state changes.
        let strip_at = strip_base
            .checked_add(index * 27)
            .ok_or(MdlError::Corrupt {
                part: "vtx strip offset",
            })?;
        let mut sr = Reader::at(bytes, strip_at);
        let strip_index_count = sr.count("vtx strip index count")?;
        let strip_index_start = sr.count("vtx strip index start")?;
        sr.take(4 + 4 + 2)?; // vertex count/offset, bone count
        let flags = sr.u8()?;
        // Strip index ranges address the group's pool by element.
        if strip_index_start
            .checked_add(strip_index_count)
            .is_none_or(|end| end > indices.len())
        {
            return Err(MdlError::Corrupt {
                part: "vtx strip range",
            });
        }
        strips.push(VtxStrip {
            index_start: strip_index_start,
            index_count: strip_index_count,
            flags,
        });
    }

    Ok(VtxStripGroup {
        vertices,
        indices,
        strips,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the helper mirrors the repeated count/offset/header fields of VTX child tables"
)]
fn read_children<T>(
    bytes: &[u8],
    base: usize,
    offset: usize,
    count: usize,
    header_bytes: usize,
    limits: &Limits,
    part: &'static str,
    read: impl Fn(&[u8], usize, &Limits) -> Result<T, MdlError>,
) -> Result<Vec<T>, MdlError> {
    if count > limits.max_entries {
        return Err(MdlError::TooMany {
            part,
            max: limits.max_entries,
        });
    }
    let mut children = Vec::with_capacity(count);
    for index in 0..count {
        let child = base
            .checked_add(offset)
            .and_then(|start| start.checked_add(index * header_bytes))
            .ok_or(MdlError::Corrupt { part })?;
        children.push(read(bytes, child, limits)?);
    }
    Ok(children)
}
