//! `.vvd` vertex data: the raw vertices `.vtx` strips index into.
//!
//! Wire layout (64-byte header, all little-endian):
//! `id "IDSV"` i32, `version` i32 (4), `checksum` u32, `lod_count` i32,
//! `lod_vertex_count` [i32; 8], `fixup_count` i32, `fixup_index` i32,
//! `vertex_index` i32, `tangent_index` i32. Vertices are 48 bytes:
//! bone weights (3xf32 + 3xu8 bone indices + u8 count), position,
//! normal, UV. Fixups (12 bytes: lod, source vertex id, count) remap
//! the source vertex array into per-LOD order; this parser materializes
//! LOD 0 (every fixup applies, since fixups apply to their LOD and all
//! below).

use super::{MdlError, Reader};
use crate::Limits;

const VVD_MAGIC: i32 = i32::from_le_bytes(*b"IDSV");
const VVD_VERSION: i32 = 4;
const HEADER_BYTES: usize = 64;
const FIXUP_BYTES: usize = 12;
const TANGENT_BYTES: usize = 16;

/// Parsed `.vvd` content, materialized for LOD 0.
#[derive(Clone, Debug, PartialEq)]
pub struct Vvd {
    /// Checksum shared with the sibling `.mdl` and `.vtx`.
    pub checksum: u32,
    /// LOD-0 vertices, fixups applied.
    pub vertices: Vec<VvdVertex>,
    /// LOD-0 tangents (xyzw), fixups applied; same length as
    /// [`vertices`](Self::vertices) in well-formed files.
    pub tangents: Vec<[f32; 4]>,
}

/// One vertex as stored. `weights`/`bones`/`bone_count` are the raw
/// VVD-authored skinning slots — deliberately NOT normalized or divided
/// (the `vmdl` crate's accessor divides by bone count, which is wrong;
/// the engine blends the stored weights as-is).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VvdVertex {
    /// Raw bone weights, slot-per-bone.
    pub weights: [f32; 3],
    /// Bone indices for each weight slot.
    pub bones: [u8; 3],
    /// Number of used slots (engine range 0–3).
    pub bone_count: u8,
    /// Model-space position.
    pub position: [f32; 3],
    /// Model-space normal.
    pub normal: [f32; 3],
    /// Texture coordinates.
    pub uv: [f32; 2],
}

/// Parse a `.vvd` and materialize LOD 0.
pub fn parse_vvd(bytes: &[u8], limits: &Limits) -> Result<Vvd, MdlError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(MdlError::InputTooLarge {
            len: bytes.len() as u64,
            max: limits.max_input_bytes,
        });
    }
    let mut r = Reader::at(bytes, 0);
    if r.i32().map_err(|_| MdlError::BadMagic { part: "vvd" })? != VVD_MAGIC {
        return Err(MdlError::BadMagic { part: "vvd" });
    }
    let version = r.i32()?;
    if version != VVD_VERSION {
        return Err(MdlError::UnsupportedVersion {
            part: "vvd",
            version,
        });
    }
    let checksum = r.u32()?;
    let lod_count = r.i32()?;
    let mut lod_vertex_count = [0i32; 8];
    for slot in &mut lod_vertex_count {
        *slot = r.i32()?;
    }
    let fixup_count = r.count("vvd fixup count")?;
    let fixup_index = r.count("vvd fixup offset")?;
    let vertex_index = r.count("vvd vertex offset")?;
    let tangent_index = r.count("vvd tangent offset")?;
    debug_assert_eq!(r.pos, HEADER_BYTES);

    if lod_count <= 0 {
        // No LODs: an empty but valid vertex file.
        return Ok(Vvd {
            checksum,
            vertices: Vec::new(),
            tangents: Vec::new(),
        });
    }
    let source_count = usize::try_from(lod_vertex_count[0]).map_err(|_| MdlError::Corrupt {
        part: "vvd lod vertex count",
    })?;
    if source_count > limits.max_entries {
        return Err(MdlError::TooMany {
            part: "vvd vertices",
            max: limits.max_entries,
        });
    }

    let mut source_vertices = Vec::with_capacity(source_count);
    let mut r = Reader::at(bytes, vertex_index);
    for _ in 0..source_count {
        source_vertices.push(read_vertex(&mut r)?);
    }
    let mut source_tangents = Vec::with_capacity(source_count);
    let mut r = Reader::at(bytes, tangent_index);
    for _ in 0..source_count {
        let t = r.take(TANGENT_BYTES)?;
        source_tangents.push([
            f32::from_le_bytes(t[0..4].try_into().expect("4 bytes")),
            f32::from_le_bytes(t[4..8].try_into().expect("4 bytes")),
            f32::from_le_bytes(t[8..12].try_into().expect("4 bytes")),
            f32::from_le_bytes(t[12..16].try_into().expect("4 bytes")),
        ]);
    }

    if fixup_count == 0 {
        return Ok(Vvd {
            checksum,
            vertices: source_vertices,
            tangents: source_tangents,
        });
    }

    if fixup_count > limits.max_entries {
        return Err(MdlError::TooMany {
            part: "vvd fixups",
            max: limits.max_entries,
        });
    }
    let mut vertices = Vec::new();
    let mut tangents = Vec::new();
    let mut r = Reader::at(bytes, fixup_index);
    for _ in 0..fixup_count {
        let fixup = r.take(FIXUP_BYTES)?;
        // lod (bytes 0..4) is not filtered: LOD-0 output includes every
        // fixup (a fixup applies to its LOD and all finer ones).
        let from = i32::from_le_bytes(fixup[4..8].try_into().expect("4 bytes"));
        let count = i32::from_le_bytes(fixup[8..12].try_into().expect("4 bytes"));
        let from = usize::try_from(from).map_err(|_| MdlError::Corrupt { part: "vvd fixup" })?;
        let to = from
            .checked_add(
                usize::try_from(count).map_err(|_| MdlError::Corrupt { part: "vvd fixup" })?,
            )
            .ok_or(MdlError::Corrupt { part: "vvd fixup" })?;
        let vertex_run = source_vertices.get(from..to).ok_or(MdlError::Corrupt {
            part: "vvd fixup range",
        })?;
        let tangent_run = source_tangents.get(from..to).ok_or(MdlError::Corrupt {
            part: "vvd fixup range",
        })?;
        if vertices.len() + vertex_run.len() > limits.max_entries {
            return Err(MdlError::TooMany {
                part: "vvd vertices",
                max: limits.max_entries,
            });
        }
        vertices.extend_from_slice(vertex_run);
        tangents.extend_from_slice(tangent_run);
    }

    Ok(Vvd {
        checksum,
        vertices,
        tangents,
    })
}

fn read_vertex(r: &mut Reader<'_>) -> Result<VvdVertex, MdlError> {
    let weights = [r.f32()?, r.f32()?, r.f32()?];
    let bones = [r.u8()?, r.u8()?, r.u8()?];
    let bone_count = r.u8()?;
    let position = r.vec3()?;
    let normal = r.vec3()?;
    let uv = [r.f32()?, r.f32()?];
    Ok(VvdVertex {
        weights,
        bones,
        bone_count,
        position,
        normal,
        uv,
    })
}
