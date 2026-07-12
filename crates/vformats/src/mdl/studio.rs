//! `.mdl` studio model files: skeleton, materials, bodygroups, and the
//! local animation table. This parser reads the subset that geometry
//! assembly and material resolution consume; layouts follow studio.h
//! via the `vmdl` crate (MIT, © icewind1991) with several upstream
//! defects corrected at the source:
//!
//! - RLE animation values are signed shorts (`i16`); vmdl reads them
//!   unsigned, mis-decoding every negative delta.
//! - Animated rotations convert euler → quaternion with studiomdl's
//!   `AngleQuaternion` (ZYX); vmdl's conversion is axis-scrambled and
//!   flips 180° on some bones.
//! - Bodypart model records stride 148 bytes (`mstudiomodel_t`); vmdl
//!   strides 144, mis-addressing every bodygroup choice after the
//!   first.
//! - Demand-loaded animation blocks (`animation_block != 0`) parse as
//!   an empty channel list; vmdl panics (`todo!`).
//!
//! Where vmdl's behavior is quirky but load-bearing, it is kept:
//! Quaternion48 values are re-normalized but Quaternion64 are not, and
//! a missing rotation channel yields the stored-order default
//! `[1, 0, 0, 0]` (x, y, z, w) exactly as vmdl's `Quaternion::default`
//! does — the assembly layer decides what to do with it.

use std::borrow::Cow;

use super::{MdlError, Reader};
use crate::Limits;
use crate::math::{cos_f32, sin_f32, sqrt_f32};

const MDL_MAGIC: &[u8; 4] = b"IDST";
const BONE_BYTES: usize = 216;
const TEXTURE_BYTES: usize = 64;
const BODY_PART_BYTES: usize = 16;
const MODEL_BYTES: usize = 148;
const MESH_BYTES: usize = 116;
const ANIM_DESC_BYTES: usize = 100;
const VVD_VERTEX_BYTES: i32 = 48;

const ANIM_RAWPOS: u8 = 0x01;
const ANIM_RAWROT: u8 = 0x02;
const ANIM_ANIMPOS: u8 = 0x04;
const ANIM_ANIMROT: u8 = 0x08;
const ANIM_RAWROT2: u8 = 0x20;

/// Model flag: vertices are stored pre-posed; bones are ignored.
pub const FLAG_STATIC_PROP: u32 = 0x0000_0010;

/// Parsed `.mdl` content (the geometry/material subset).
#[derive(Clone, Debug, PartialEq)]
pub struct Mdl {
    /// Internal model name from the header.
    pub name: String,
    /// Checksum shared with the sibling `.vvd` and `.vtx`.
    pub checksum: u32,
    /// Studio format version (44–49).
    pub version: i32,
    /// Header flags word (see [`FLAG_STATIC_PROP`]).
    pub flags: u32,
    /// Skeleton bones, file order.
    pub bones: Vec<MdlBone>,
    /// Material names (backslashes normalized to slashes).
    pub textures: Vec<String>,
    /// Material search directories (`$cdmaterials`), normalized.
    pub texture_paths: Vec<String>,
    /// Materials per skin family row.
    pub skin_reference_count: usize,
    /// Skin table, `skin_reference_count` entries per family.
    pub skin_table: Vec<u16>,
    /// Body parts with their bodygroup choice models.
    pub body_parts: Vec<MdlBodyPart>,
    /// Local animation descriptions (rest poses live here).
    pub local_animations: Vec<MdlAnimationDescription>,
    /// Declared sequence count (sequences themselves are not parsed).
    pub sequence_count: u32,
}

/// One skeleton bone, fields as stored.
#[derive(Clone, Debug, PartialEq)]
pub struct MdlBone {
    /// Bone name.
    pub name: String,
    /// Parent bone index, -1 for roots.
    pub parent: i32,
    /// Bind position (parent-local).
    pub position: [f32; 3],
    /// Bind rotation quaternion in stored order `[x, y, z, w]`.
    pub quaternion: [f32; 4],
    /// Bind rotation as radian euler (roll, pitch, yaw).
    pub rotation: [f32; 3],
    /// Animation position channel scale.
    pub position_scale: [f32; 3],
    /// Animation rotation channel scale.
    pub rotation_scale: [f32; 3],
    /// Inverse bind matrix, row-major 3x4 as stored.
    pub pose_to_bone: [[f32; 4]; 3],
}

/// One body part (bodygroup).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdlBodyPart {
    /// Choice models.
    pub models: Vec<MdlModel>,
}

/// One bodygroup choice model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdlModel {
    /// Model name.
    pub name: String,
    /// Base offset into the `.vvd` vertices, in vertex elements
    /// (converted from the stored byte offset).
    pub vertex_offset: i32,
    /// Meshes, `.vtx` order.
    pub meshes: Vec<MdlMesh>,
}

/// One mesh of a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MdlMesh {
    /// Index into [`Mdl::textures`] (pre-skin-table).
    pub material: i32,
    /// Vertex offset within the model, in vertex elements.
    pub vertex_offset: i32,
}

/// One local animation description.
#[derive(Clone, Debug, PartialEq)]
pub struct MdlAnimationDescription {
    /// Animation name (e.g. `@idle`).
    pub name: String,
    /// Frames per second.
    pub fps: f32,
    /// Frame count.
    pub frame_count: usize,
    /// Per-bone channels.
    pub animations: Vec<MdlAnimation>,
}

/// Per-bone animation channel data.
#[derive(Clone, Debug, PartialEq)]
pub struct MdlAnimation {
    /// Bone index.
    pub bone: u8,
    rotation: RotationData,
    position: PositionData,
}

#[derive(Clone, Debug, PartialEq)]
enum RotationData {
    /// Decoded (and re-normalized) Quaternion48, `[x, y, z, w]`.
    Raw([f32; 4]),
    /// Animated euler per frame (scales and axis fixup applied).
    Animated(Vec<[f32; 3]>),
    None,
}

#[derive(Clone, Debug, PartialEq)]
enum PositionData {
    /// Decoded Vector48.
    Raw([f32; 3]),
    /// Animated position per frame (scales applied).
    Animated(Vec<[f32; 3]>),
    None,
}

impl MdlAnimation {
    /// Whether this channel carries rotation data.
    #[must_use]
    pub fn has_rotation(&self) -> bool {
        !matches!(self.rotation, RotationData::None)
    }

    /// Whether this channel carries position data.
    #[must_use]
    pub fn has_position(&self) -> bool {
        !matches!(self.position, PositionData::None)
    }

    /// Test-only channel constructor (quaternions in stored
    /// `[x, y, z, w]` order).
    #[cfg(test)]
    pub(crate) fn for_tests(
        bone: u8,
        rotation: Option<[f32; 4]>,
        position: Option<[f32; 3]>,
    ) -> Self {
        Self {
            bone,
            rotation: rotation.map_or(RotationData::None, RotationData::Raw),
            position: position.map_or(PositionData::None, PositionData::Raw),
        }
    }

    /// Rotation at `frame` as `[x, y, z, w]`. Animated channels clamp
    /// past the last frame; a missing channel yields the stored-order
    /// default `[1, 0, 0, 0]` (vmdl-faithful).
    #[must_use]
    pub fn rotation(&self, frame: usize) -> [f32; 4] {
        match &self.rotation {
            RotationData::Raw(quat) => *quat,
            RotationData::Animated(values) => {
                let euler = values
                    .get(frame)
                    .or_else(|| values.last())
                    .copied()
                    .unwrap_or_default();
                angle_quaternion(euler)
            }
            RotationData::None => [1.0, 0.0, 0.0, 0.0],
        }
    }

    /// Position at `frame`; animated channels past the end yield the
    /// origin (vmdl-faithful).
    #[must_use]
    pub fn position(&self, frame: usize) -> [f32; 3] {
        match &self.position {
            PositionData::Raw(position) => *position,
            PositionData::Animated(values) => values.get(frame).copied().unwrap_or_default(),
            PositionData::None => [0.0; 3],
        }
    }
}

/// studiomdl's `AngleQuaternion`: radian euler (roll, pitch, yaw) to
/// `[x, y, z, w]`, ZYX intrinsic. This replaces vmdl's axis-scrambled
/// conversion.
fn angle_quaternion(euler: [f32; 3]) -> [f32; 4] {
    let (sr, cr) = (sin_f32(euler[0] * 0.5), cos_f32(euler[0] * 0.5));
    let (sp, cp) = (sin_f32(euler[1] * 0.5), cos_f32(euler[1] * 0.5));
    let (sy, cy) = (sin_f32(euler[2] * 0.5), cos_f32(euler[2] * 0.5));
    [
        sr * cp * cy - cr * sp * sy,
        cr * sp * cy + sr * cp * sy,
        cr * cp * sy - sr * sp * cy,
        cr * cp * cy + sr * sp * sy,
    ]
}

/// Parse a `.mdl` file.
pub fn parse_mdl(bytes: &[u8], limits: &Limits) -> Result<Mdl, MdlError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(MdlError::InputTooLarge {
            len: bytes.len() as u64,
            max: limits.max_input_bytes,
        });
    }
    let mut r = Reader::at(bytes, 0);
    if r.take(4).map_err(|_| MdlError::BadMagic { part: "mdl" })? != MDL_MAGIC {
        return Err(MdlError::BadMagic { part: "mdl" });
    }
    let version = r.i32()?;
    if !(44..=49).contains(&version) {
        return Err(MdlError::UnsupportedVersion {
            part: "mdl",
            version,
        });
    }
    let checksum = r.u32()?;
    let name = fixed_string(r.take(64)?);
    r.take(4)?; // data length
    r.take(72)?; // eye/illumination positions, bounding boxes
    debug_assert_eq!(r.pos, 152);
    let flags = r.u32()?;
    let bone_count = r.count("mdl bone count")?;
    let bone_offset = r.count("mdl bone offset")?;
    r.take(16)?; // bone controllers, hitbox sets
    let anim_count = r.count("mdl animation count")?;
    let anim_offset = r.count("mdl animation offset")?;
    let sequence_count = r.u32()?;
    r.take(4)?; // sequence offset
    r.take(8)?; // activity list version, events indexed
    let texture_count = r.count("mdl texture count")?;
    let texture_offset = r.count("mdl texture offset")?;
    let texture_dir_count = r.count("mdl texture dir count")?;
    let texture_dir_offset = r.count("mdl texture dir offset")?;
    let skin_reference_count = r.count("mdl skin reference count")?;
    let skin_family_count = r.count("mdl skin family count")?;
    let skin_offset = r.count("mdl skin offset")?;
    let body_part_count = r.count("mdl body part count")?;
    let body_part_offset = r.count("mdl body part offset")?;
    debug_assert_eq!(r.pos, 240);

    for (count, part) in [
        (bone_count, "mdl bones"),
        (anim_count, "mdl animations"),
        (texture_count, "mdl textures"),
        (texture_dir_count, "mdl texture dirs"),
        (
            skin_family_count.saturating_mul(skin_reference_count),
            "mdl skin table",
        ),
        (body_part_count, "mdl body parts"),
    ] {
        if count > limits.max_entries {
            return Err(MdlError::TooMany {
                part,
                max: limits.max_entries,
            });
        }
    }

    let mut bones = Vec::with_capacity(bone_count);
    for index in 0..bone_count {
        let base = offset_at(bone_offset, index, BONE_BYTES)?;
        bones.push(read_bone(bytes, base)?);
    }

    let mut textures = Vec::with_capacity(texture_count);
    for index in 0..texture_count {
        let base = offset_at(texture_offset, index, TEXTURE_BYTES)?;
        let mut tr = Reader::at(bytes, base);
        let name_index = tr.i32()?;
        // vmdl tolerates out-of-range texture names as empty; keep that.
        let name = relative_string(bytes, base, name_index).unwrap_or_default();
        textures.push(name.replace('\\', "/"));
    }

    let mut texture_paths = Vec::with_capacity(texture_dir_count);
    let mut dr = Reader::at(bytes, texture_dir_offset);
    for _ in 0..texture_dir_count {
        let at = dr.u32()? as usize;
        let path = c_string_at(bytes, at).ok_or(MdlError::Corrupt {
            part: "mdl texture dir",
        })?;
        texture_paths.push(path.replace('\\', "/"));
    }

    let mut skin_table = Vec::with_capacity(skin_reference_count * skin_family_count);
    let mut sr = Reader::at(bytes, skin_offset);
    for _ in 0..skin_reference_count * skin_family_count {
        let s = sr.take(2)?;
        skin_table.push(u16::from_le_bytes([s[0], s[1]]));
    }

    let mut body_parts = Vec::with_capacity(body_part_count);
    for index in 0..body_part_count {
        let base = offset_at(body_part_offset, index, BODY_PART_BYTES)?;
        body_parts.push(read_body_part(bytes, base, limits)?);
    }

    let mut local_animations = Vec::with_capacity(anim_count);
    for index in 0..anim_count {
        let base = offset_at(anim_offset, index, ANIM_DESC_BYTES)?;
        local_animations.push(read_animation_description(bytes, base, &bones, limits)?);
    }

    Ok(Mdl {
        name,
        checksum,
        version,
        flags,
        bones,
        textures,
        texture_paths,
        skin_reference_count,
        skin_table,
        body_parts,
        local_animations,
        sequence_count,
    })
}

fn read_bone(bytes: &[u8], base: usize) -> Result<MdlBone, MdlError> {
    let mut r = Reader::at(bytes, base);
    let name_index = r.i32()?;
    let parent = r.i32()?;
    r.take(24)?; // bone controllers
    let position = r.vec3()?;
    let quaternion = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
    let rotation = r.vec3()?;
    let position_scale = r.vec3()?;
    let rotation_scale = r.vec3()?;
    let mut pose_to_bone = [[0.0f32; 4]; 3];
    for row in &mut pose_to_bone {
        for cell in row.iter_mut() {
            *cell = r.f32()?;
        }
    }
    // q_alignment, flags, procedural, physics, surfaceprop, contents,
    // reserved: unused by assembly.
    let name = relative_string(bytes, base, name_index).ok_or(MdlError::Corrupt {
        part: "mdl bone name",
    })?;
    Ok(MdlBone {
        name,
        parent,
        position,
        quaternion,
        rotation,
        position_scale,
        rotation_scale,
        pose_to_bone,
    })
}

fn read_body_part(bytes: &[u8], base: usize, limits: &Limits) -> Result<MdlBodyPart, MdlError> {
    let mut r = Reader::at(bytes, base);
    r.take(4)?; // name index
    let model_count = r.count("mdl model count")?;
    r.take(4)?; // base
    let model_offset = r.count("mdl model offset")?;
    if model_count > limits.max_entries {
        return Err(MdlError::TooMany {
            part: "mdl models",
            max: limits.max_entries,
        });
    }
    let mut models = Vec::with_capacity(model_count);
    for index in 0..model_count {
        // mstudiomodel_t is 148 bytes; vmdl strides 144 here and
        // mis-addresses every choice after the first.
        let model_base = base
            .checked_add(model_offset)
            .and_then(|start| start.checked_add(index * MODEL_BYTES))
            .ok_or(MdlError::Corrupt {
                part: "mdl model offset",
            })?;
        models.push(read_model(bytes, model_base, limits)?);
    }
    Ok(MdlBodyPart { models })
}

fn read_model(bytes: &[u8], base: usize, limits: &Limits) -> Result<MdlModel, MdlError> {
    let mut r = Reader::at(bytes, base);
    let name = fixed_string(r.take(64)?);
    r.take(8)?; // type, bounding radius
    let mesh_count = r.count("mdl mesh count")?;
    let mesh_offset = r.count("mdl mesh offset")?;
    r.take(4)?; // vertex count
    let vertex_index = r.i32()?;
    if vertex_index % VVD_VERTEX_BYTES != 0 {
        return Err(MdlError::Corrupt {
            part: "mdl model vertex index",
        });
    }
    if mesh_count > limits.max_entries {
        return Err(MdlError::TooMany {
            part: "mdl meshes",
            max: limits.max_entries,
        });
    }
    let mut meshes = Vec::with_capacity(mesh_count);
    for index in 0..mesh_count {
        let mesh_base = base
            .checked_add(mesh_offset)
            .and_then(|start| start.checked_add(index * MESH_BYTES))
            .ok_or(MdlError::Corrupt {
                part: "mdl mesh offset",
            })?;
        let mut mr = Reader::at(bytes, mesh_base);
        let material = mr.i32()?;
        mr.take(8)?; // model index, vertex count
        let vertex_offset = mr.i32()?;
        // Bounds-probe the whole record so truncation fails loudly.
        Reader::at(bytes, mesh_base).take(MESH_BYTES)?;
        meshes.push(MdlMesh {
            material,
            vertex_offset,
        });
    }
    // Bounds-probe the whole model record too.
    Reader::at(bytes, base).take(MODEL_BYTES)?;
    Ok(MdlModel {
        name,
        vertex_offset: vertex_index / VVD_VERTEX_BYTES,
        meshes,
    })
}

fn read_animation_description(
    bytes: &[u8],
    base: usize,
    bones: &[MdlBone],
    limits: &Limits,
) -> Result<MdlAnimationDescription, MdlError> {
    let mut r = Reader::at(bytes, base);
    r.take(4)?; // base pointer
    let name_offset = r.i32()?;
    let fps = r.f32()?;
    r.take(4)?; // flags
    let frame_count = r.count("mdl frame count")?.max(1);
    r.take(32)?; // movements, padding
    let animation_block = r.i32()?;
    let animation_index = r.count("mdl animation index")?;
    // Bounds-probe the full 100-byte description record.
    Reader::at(bytes, base).take(ANIM_DESC_BYTES)?;

    if frame_count > limits.max_entries {
        return Err(MdlError::TooMany {
            part: "mdl animation frames",
            max: limits.max_entries,
        });
    }

    let name = relative_string(bytes, base, name_offset).ok_or(MdlError::Corrupt {
        part: "mdl animation name",
    })?;

    let mut animations = Vec::new();
    if animation_block == 0 {
        // Demand-loaded blocks (animation_block != 0) stay empty: the
        // data lives in external .ani files (vmdl panics here).
        let mut offset = base.checked_add(animation_index).ok_or(MdlError::Corrupt {
            part: "mdl animation offset",
        })?;
        loop {
            if animations.len() >= limits.max_entries {
                return Err(MdlError::TooMany {
                    part: "mdl animation channels",
                    max: limits.max_entries,
                });
            }
            let (animation, next_offset) = read_animation(bytes, offset, frame_count, bones)?;
            animations.push(animation);
            if next_offset == 0 {
                break;
            }
            offset = offset.checked_add(next_offset).ok_or(MdlError::Corrupt {
                part: "mdl animation chain",
            })?;
        }
    }

    Ok(MdlAnimationDescription {
        name,
        fps,
        frame_count,
        animations,
    })
}

fn read_animation(
    bytes: &[u8],
    base: usize,
    frames: usize,
    bones: &[MdlBone],
) -> Result<(MdlAnimation, usize), MdlError> {
    let mut r = Reader::at(bytes, base);
    let bone = r.u8()?;
    let flags = r.u8()?;
    let next_offset = usize::from(u16::from_le_bytes([r.u8()?, r.u8()?]));
    let scales = bones.get(usize::from(bone));

    let mut offset = base + 4;
    let rotation = if flags & ANIM_RAWROT != 0 {
        let q = read_quaternion48(bytes, offset)?;
        offset += 6;
        RotationData::Raw(q)
    } else if flags & ANIM_RAWROT2 != 0 {
        let q = read_quaternion64(bytes, offset)?;
        offset += 8;
        RotationData::Raw(q)
    } else if flags & ANIM_ANIMROT != 0 {
        let scale = scales.map_or([1.0; 3], |bone| bone.rotation_scale);
        let values = read_rle_frames(bytes, offset, frames)?
            .into_iter()
            // vmdl's axis fixup applied with the bone's rotation scale:
            // final (roll, pitch, yaw) = (v2*s2, v0*s0, v1*s1).
            .map(|value| {
                [
                    value[2] * scale[2],
                    value[0] * scale[0],
                    value[1] * scale[1],
                ]
            })
            .collect();
        offset += 6;
        RotationData::Animated(values)
    } else {
        RotationData::None
    };

    let position = if flags & ANIM_RAWPOS != 0 {
        PositionData::Raw(r_take3_half(bytes, offset)?)
    } else if flags & ANIM_ANIMPOS != 0 {
        let scale = scales.map_or([1.0; 3], |bone| bone.position_scale);
        let values = read_rle_frames(bytes, offset, frames)?
            .into_iter()
            .map(|value| {
                [
                    value[0] * scale[0],
                    value[1] * scale[1],
                    value[2] * scale[2],
                ]
            })
            .collect();
        PositionData::Animated(values)
    } else {
        PositionData::None
    };

    Ok((
        MdlAnimation {
            bone,
            rotation,
            position,
        },
        next_offset,
    ))
}

fn read_quaternion48(bytes: &[u8], at: usize) -> Result<[f32; 4], MdlError> {
    let mut r = Reader::at(bytes, at);
    let b = r.take(6)?;
    let raw_x = u16::from_le_bytes([b[0], b[1]]);
    let raw_y = u16::from_le_bytes([b[2], b[3]]);
    let raw_z = u16::from_le_bytes([b[4], b[5]]);
    let x = (f32::from(raw_x) - 32768.0) / 32768.0;
    let y = (f32::from(raw_y) - 32768.0) / 32768.0;
    let z = (f32::from(raw_z & 0x7FFF) - 16384.0) / 16384.0;
    let w_sign = if raw_z & 0x8000 != 0 { -1.0 } else { 1.0 };
    let w = sqrt_f32(1.0 - x * x - y * y - z * z) * w_sign;
    // vmdl re-normalizes Quaternion48 (only); keep that behavior.
    let len = sqrt_f32(x * x + y * y + z * z + w * w);
    if len > 0.0 && len.is_finite() {
        Ok([x / len, y / len, z / len, w / len])
    } else {
        Ok([x, y, z, w])
    }
}

fn read_quaternion64(bytes: &[u8], at: usize) -> Result<[f32; 4], MdlError> {
    let mut r = Reader::at(bytes, at);
    let b = r.take(8)?;
    let raw = u64::from_le_bytes(b.try_into().expect("8 bytes"));
    let component = |offset: u32| {
        let bits = (raw >> offset) & 0x1F_FFFF;
        (bits as f32 - 1_048_576.0) / 1_048_576.5
    };
    let (x, y, z) = (component(0), component(21), component(42));
    let w_sign = if raw & 0x8000_0000_0000_0000 != 0 {
        -1.0
    } else {
        1.0
    };
    // vmdl does NOT re-normalize Quaternion64; keep that behavior.
    Ok([x, y, z, sqrt_f32(1.0 - x * x - y * y - z * z) * w_sign])
}

fn r_take3_half(bytes: &[u8], at: usize) -> Result<[f32; 3], MdlError> {
    let mut r = Reader::at(bytes, at);
    let b = r.take(6)?;
    Ok([
        crate::math::half_to_f32(u16::from_le_bytes([b[0], b[1]])),
        crate::math::half_to_f32(u16::from_le_bytes([b[2], b[3]])),
        crate::math::half_to_f32(u16::from_le_bytes([b[4], b[5]])),
    ])
}

/// Materialize `frames` frames of a three-component RLE channel. The
/// pointer struct at `at` holds three u16 offsets (relative to itself);
/// each leads to spans of `{valid: u8, total: u8}` headers followed by
/// `valid` signed shorts, `total` frames per span (frames past `valid`
/// reuse the last value). Values are `i16` — studiomdl signs them;
/// vmdl reads them unsigned.
fn read_rle_frames(bytes: &[u8], at: usize, frames: usize) -> Result<Vec<[f32; 3]>, MdlError> {
    let mut r = Reader::at(bytes, at);
    let pointers = [
        u16::from_le_bytes(r.take(2)?.try_into().expect("2 bytes")),
        u16::from_le_bytes(r.take(2)?.try_into().expect("2 bytes")),
        u16::from_le_bytes(r.take(2)?.try_into().expect("2 bytes")),
    ];
    let mut channels: [Vec<f32>; 3] = Default::default();
    for (channel, pointer) in channels.iter_mut().zip(pointers) {
        if pointer != 0 {
            *channel = read_rle_channel(bytes, at + usize::from(pointer), frames)?;
        }
    }
    Ok((0..frames)
        .map(|frame| {
            channels
                .each_ref()
                .map(|channel| channel.get(frame).copied().unwrap_or(0.0))
        })
        .collect())
}

/// Span headers walked by [`read_rle_channel`], test builds only —
/// proves the walk stays linear in `frames` instead of re-scanning the
/// chain per frame.
#[cfg(test)]
static SPAN_HEADER_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Walks one RLE span chain once, emitting exactly `frames` values in
/// order — a single forward pass rather than re-walking the chain from
/// its start for every frame, which turns quadratic on long channels.
/// Values past a span's `valid` count repeat its last stored value; a
/// chain that runs out of spans zero-fills the remainder.
fn read_rle_channel(bytes: &[u8], mut at: usize, frames: usize) -> Result<Vec<f32>, MdlError> {
    let mut out = Vec::with_capacity(frames);
    // Each iteration either consumes a real span (advancing `out` by at
    // least one frame) or is the empty-span half of the chain
    // terminator, so this bound stays a small multiple of `frames`
    // regardless of how a crafted chain interleaves empty spans.
    let max_spans = frames.saturating_mul(2).saturating_add(4);
    for _ in 0..max_spans {
        if out.len() >= frames {
            return Ok(out);
        }
        #[cfg(test)]
        SPAN_HEADER_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut r = Reader::at(bytes, at);
        let header = r.take(2)?;
        let (valid, total) = (usize::from(header[0]), usize::from(header[1]));
        if total == 0 {
            // Empty span: skip it, then check the next header — two
            // consecutive empty spans is the wire format's chain
            // terminator (vmdl-faithful), so the remainder is zero.
            at += (valid + 1) * 2;
            let mut peek = Reader::at(bytes, at);
            if peek.take(2)?[1] == 0 {
                out.resize(frames, 0.0);
                return Ok(out);
            }
            continue;
        }
        for slot in 0..total {
            if out.len() >= frames {
                return Ok(out);
            }
            let index = if valid > slot { slot + 1 } else { valid };
            let mut vr = Reader::at(bytes, at + index * 2);
            let b = vr.take(2)?;
            out.push(f32::from(i16::from_le_bytes([b[0], b[1]])));
        }
        at += (valid + 1) * 2;
    }
    Err(MdlError::Corrupt {
        part: "mdl animation rle",
    })
}

fn offset_at(base: usize, index: usize, stride: usize) -> Result<usize, MdlError> {
    index
        .checked_mul(stride)
        .and_then(|delta| base.checked_add(delta))
        .ok_or(MdlError::Corrupt {
            part: "mdl record offset",
        })
}

/// NUL-terminated string at `base + relative`, lossily decoded.
fn relative_string(bytes: &[u8], base: usize, relative: i32) -> Option<String> {
    let at = usize::try_from(relative).ok()?.checked_add(base)?;
    c_string_at(bytes, at)
}

fn c_string_at(bytes: &[u8], at: usize) -> Option<String> {
    let rest = bytes.get(at..)?;
    let nul = rest.iter().position(|byte| *byte == 0)?;
    Some(match String::from_utf8_lossy(&rest[..nul]) {
        Cow::Borrowed(s) => s.to_string(),
        Cow::Owned(s) => s,
    })
}

/// Fixed 64-byte name field, NUL-trimmed, lossily decoded.
fn fixed_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|byte| *byte == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{SPAN_HEADER_READS, read_rle_channel};

    #[test]
    fn rle_channel_walks_a_degenerate_span_chain_linearly() {
        // Worst case for a naive "restart from the chain's start per
        // frame" implementation: one span per frame, so it re-scans
        // an ever-longer prefix on every one of `FRAMES` calls.
        const FRAMES: usize = 4000;
        let mut bytes = Vec::with_capacity(FRAMES * 4);
        for i in 0..FRAMES {
            bytes.push(1); // valid
            bytes.push(1); // total
            bytes.extend_from_slice(&i16::try_from(i % 100).unwrap().to_le_bytes());
        }

        SPAN_HEADER_READS.store(0, Ordering::Relaxed);
        let values = read_rle_channel(&bytes, 0, FRAMES).expect("well-formed chain");
        assert_eq!(values.len(), FRAMES);

        // A single forward walk reads exactly one header per frame
        // here; re-walking from the start per frame would read on the
        // order of FRAMES*(FRAMES + 1)/2 headers instead.
        assert_eq!(SPAN_HEADER_READS.load(Ordering::Relaxed), FRAMES);
    }
}
