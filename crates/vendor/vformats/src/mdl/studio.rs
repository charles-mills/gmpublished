//! MDL layouts adapted from vmdl (MIT, © icewind1991).

#![allow(missing_docs)]

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
const SEQUENCE_BYTES: usize = 212;
const VVD_VERTEX_BYTES: i32 = 48;

const ANIM_RAWPOS: u8 = 0x01;
const ANIM_RAWROT: u8 = 0x02;
const ANIM_ANIMPOS: u8 = 0x04;
const ANIM_ANIMROT: u8 = 0x08;
const ANIM_DELTA: u8 = 0x10;
const ANIM_RAWROT2: u8 = 0x20;

pub(super) const BONE_SCREEN_ALIGN_SPHERE: u32 = 0x0008;
pub(super) const BONE_SCREEN_ALIGN_CYLINDER: u32 = 0x0010;
pub(super) const BONE_FIXED_ALIGNMENT: u32 = 0x0010_0000;

pub(super) const STUDIO_LOOPING: u32 = 0x0001;
pub(super) const STUDIO_DELTA: u32 = 0x0004;
pub(super) const STUDIO_AUTOPLAY: u32 = 0x0008;
pub(super) const STUDIO_POST: u32 = 0x0010;
pub(super) const STUDIO_ALLZEROS: u32 = 0x0020;
pub(super) const STUDIO_RESERVED_ENCODING: u32 = 0x0040;
pub(super) const STUDIO_CYCLEPOSE: u32 = 0x0080;
pub(super) const STUDIO_REALTIME: u32 = 0x0100;
pub(super) const STUDIO_LOCAL: u32 = 0x0200;
pub(super) const STUDIO_WORLD: u32 = 0x4000;

/// Preposed vertices; bones are ignored.
pub const FLAG_STATIC_PROP: u32 = 0x0000_0010;

#[derive(Clone, Debug, PartialEq)]
pub struct Mdl {
    pub name: String,
    /// Checksum shared with the sibling `.vvd` and `.vtx`.
    pub checksum: u32,
    pub version: i32,
    pub flags: u32,
    /// Model-space lighting origin.
    pub illumination_position: [f32; 3],
    /// Model-space hull bounds.
    pub hull_min: [f32; 3],
    pub hull_max: [f32; 3],
    /// Model-space render bounds.
    pub view_min: [f32; 3],
    pub view_max: [f32; 3],
    pub attachments: Vec<super::MdlAttachment>,
    /// Zero-based; virtual-model attachments may be unavailable.
    pub illumination_attachment_index: Option<usize>,
    pub bones: Vec<MdlBone>,
    /// Optional v48/v49 bone overrides.
    pub linear_bones: Option<super::MdlLinearBones>,
    /// Material names (backslashes normalized to slashes).
    pub textures: Vec<String>,
    /// Normalized `$cdmaterials` paths.
    pub texture_paths: Vec<String>,
    pub skin_reference_count: usize,
    /// Skin table, `skin_reference_count` entries per family.
    pub skin_table: Vec<u16>,
    pub body_parts: Vec<MdlBodyPart>,
    pub local_animations: Vec<MdlAnimationDescription>,
    pub sequences: Vec<MdlSequence>,
    pub included_model_count: usize,
    pub bone_controller_count: usize,
    pub flex_descriptor_count: usize,
    pub sequence_count: u32,
}

impl Mdl {
    pub fn runtime_bones(&self) -> Result<std::borrow::Cow<'_, [MdlBone]>, MdlError> {
        self.linear_bones.as_ref().map_or_else(
            || Ok(std::borrow::Cow::Borrowed(self.bones.as_slice())),
            |linear| linear.apply_to(&self.bones).map(std::borrow::Cow::Owned),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MdlBone {
    pub name: String,
    /// Parent bone index, -1 for roots.
    pub parent: i32,
    /// Bind position (parent-local).
    pub position: [f32; 3],
    /// Bind rotation quaternion in stored order `[x, y, z, w]`.
    pub quaternion: [f32; 4],
    /// Bind rotation as radian euler (roll, pitch, yaw).
    pub rotation: [f32; 3],
    pub position_scale: [f32; 3],
    pub rotation_scale: [f32; 3],
    /// Inverse bind matrix, row-major 3x4 as stored.
    pub pose_to_bone: [[f32; 4]; 3],
    /// Stored XYZW alignment quaternion.
    pub quaternion_alignment: [f32; 4],
    pub flags: u32,
    pub procedural_kind: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdlBodyPart {
    pub models: Vec<MdlModel>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdlModel {
    pub name: String,
    /// Offset into `.vvd`, in vertices.
    pub vertex_offset: i32,
    /// Meshes, `.vtx` order.
    pub meshes: Vec<MdlMesh>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MdlMesh {
    /// Index into [`Mdl::textures`] (pre-skin-table).
    pub material: i32,
    /// Model-local offset, in vertices.
    pub vertex_offset: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MdlAnimationDescription {
    pub name: String,
    pub fps: f32,
    pub flags: u32,
    pub frame_count: usize,
    pub animations: Vec<MdlAnimation>,
    pub sections: Vec<MdlAnimationSection>,
    pub section_frames: usize,
    pub external_block: Option<i32>,
    pub hierarchy_count: usize,
    pub ik_rule_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MdlAnimationSection {
    pub animations: Vec<MdlAnimation>,
    pub external_block: Option<i32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MdlSequence {
    pub label: String,
    pub flags: u32,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub animations: Vec<usize>,
    pub blend_grid: [usize; 2],
    pub pose_parameters: [i32; 2],
    pub autolayer_count: usize,
    pub ik_rule_count: usize,
    pub ik_lock_count: usize,
    pub bone_weights: Vec<f32>,
}

impl MdlSequence {
    #[must_use]
    pub fn looping(&self) -> bool {
        self.flags & STUDIO_LOOPING != 0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MdlAnimation {
    pub bone: u8,
    flags: u8,
    rotation: RotationData,
    position: PositionData,
}

#[derive(Clone, Debug, PartialEq)]
enum RotationData {
    Raw([f32; 4]),
    Animated(Vec<[f32; 3]>),
    None,
}

#[derive(Clone, Debug, PartialEq)]
enum PositionData {
    Raw([f32; 3]),
    Animated(Vec<[f32; 3]>),
    None,
}

impl MdlAnimationDescription {
    pub(crate) fn is_all_zeros(&self) -> bool {
        self.flags & STUDIO_ALLZEROS != 0
    }

    pub(crate) fn channels_at(
        &self,
        frame: usize,
    ) -> Result<(&[MdlAnimation], usize), super::assembly::ModelAnimationError> {
        if self.is_all_zeros() {
            return Ok((&[], 0));
        }
        if self.section_frames == 0 {
            return Ok((&self.animations, frame));
        }
        let (section, local) =
            if self.frame_count > self.section_frames && frame == self.frame_count - 1 {
                (self.frame_count / self.section_frames + 1, 0)
            } else {
                (frame / self.section_frames, frame % self.section_frames)
            };
        let data =
            self.sections
                .get(section)
                .ok_or(super::assembly::ModelAnimationError::Malformed {
                    part: "clip section",
                })?;
        Ok((&data.animations, local))
    }
}

impl MdlAnimation {
    pub(crate) fn local_pose(
        &self,
        bone: &MdlBone,
        frame: usize,
    ) -> Result<([f32; 3], [f32; 4]), super::assembly::ModelAnimationError> {
        use super::assembly::ModelAnimationError;
        if self.flags & ANIM_DELTA != 0 {
            return Err(ModelAnimationError::Unsupported {
                feature: "delta animation channels",
            });
        }
        let position = match &self.position {
            PositionData::Raw(p) => *p,
            PositionData::Animated(values) => {
                let p = values
                    .get(frame)
                    .or_else(|| values.last())
                    .copied()
                    .unwrap_or_default();
                std::array::from_fn(|i| bone.position[i] + p[i])
            }
            PositionData::None => bone.position,
        };
        let rotation = match &self.rotation {
            RotationData::Raw(q) => *q,
            RotationData::Animated(values) => {
                let p = values
                    .get(frame)
                    .or_else(|| values.last())
                    .copied()
                    .unwrap_or_default();
                angle_quaternion([
                    bone.rotation[0] + p[1],
                    bone.rotation[1] + p[2],
                    bone.rotation[2] + p[0],
                ])
            }
            RotationData::None => bone.quaternion,
        };
        let norm = rotation.iter().map(|v| v * v).sum::<f32>().sqrt();
        if position.iter().any(|v| !v.is_finite()) || !norm.is_finite() || norm <= f32::EPSILON {
            return Err(ModelAnimationError::Malformed {
                part: "channel pose",
            });
        }
        Ok((
            position,
            [
                rotation[3] / norm,
                rotation[0] / norm,
                rotation[1] / norm,
                rotation[2] / norm,
            ],
        ))
    }
    #[must_use]
    pub fn has_rotation(&self) -> bool {
        !matches!(self.rotation, RotationData::None)
    }

    #[must_use]
    pub fn has_position(&self) -> bool {
        !matches!(self.position, PositionData::None)
    }

    #[cfg(test)]
    pub(crate) fn positions_for_tests(bone: u8, positions: Vec<[f32; 3]>) -> Self {
        Self {
            bone,
            flags: ANIM_ANIMPOS,
            rotation: RotationData::None,
            position: PositionData::Animated(positions),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests(
        bone: u8,
        rotation: Option<[f32; 4]>,
        position: Option<[f32; 3]>,
    ) -> Self {
        Self {
            bone,
            flags: 0,
            rotation: rotation.map_or(RotationData::None, RotationData::Raw),
            position: position.map_or(PositionData::None, PositionData::Raw),
        }
    }

    /// Returns `[x,y,z,w]`; absent channels use `[1,0,0,0]`.
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

    /// Animated channels return zero past the last frame.
    #[must_use]
    pub fn position(&self, frame: usize) -> [f32; 3] {
        match &self.position {
            PositionData::Raw(position) => *position,
            PositionData::Animated(values) => values.get(frame).copied().unwrap_or_default(),
            PositionData::None => [0.0; 3],
        }
    }
}

/// Radian roll/pitch/yaw to `[x,y,z,w]` (ZYX).
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
    r.take(12)?; // eye position
    let illumination_position = [r.f32()?, r.f32()?, r.f32()?];
    let hull_min = r.vec3()?;
    let hull_max = r.vec3()?;
    let view_min = r.vec3()?;
    let view_max = r.vec3()?;
    debug_assert_eq!(r.pos, 152);
    let flags = r.u32()?;
    let bone_count = r.count("mdl bone count")?;
    let bone_offset = r.count("mdl bone offset")?;
    let bone_controller_count = r.count("mdl bone controllers")?;
    r.take(12)?;
    let anim_count = r.count("mdl animation count")?;
    let anim_offset = r.count("mdl animation offset")?;
    let sequence_count = r.u32()?;
    let sequence_offset = r.count("mdl sequence offset")?;
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
        (bone_controller_count, "mdl bone controllers"),
        (anim_count, "mdl animations"),
        (sequence_count as usize, "mdl sequences"),
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

    let (attachments, illumination_attachment_index) =
        super::attachments::read(bytes, bone_count, limits)?;
    let mut bones = Vec::with_capacity(bone_count);
    for index in 0..bone_count {
        let base = offset_at(bone_offset, index, BONE_BYTES)?;
        bones.push(read_bone(bytes, base)?);
    }
    let linear_bones = if version >= 48 {
        super::linear_bones::read(bytes, bone_count, limits)?
    } else {
        None
    };
    let runtime_bones = if let Some(linear) = &linear_bones {
        std::borrow::Cow::Owned(linear.apply_to(&bones)?)
    } else {
        std::borrow::Cow::Borrowed(bones.as_slice())
    };

    let mut textures = Vec::with_capacity(texture_count);
    for index in 0..texture_count {
        let base = offset_at(texture_offset, index, TEXTURE_BYTES)?;
        let mut tr = Reader::at(bytes, base);
        let name_index = tr.i32()?;
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
    let mut decoded_animation_bytes = 0usize;
    for index in 0..anim_count {
        let base = offset_at(anim_offset, index, ANIM_DESC_BYTES)?;
        local_animations.push(read_animation_description(
            bytes,
            base,
            &runtime_bones,
            limits,
            &mut decoded_animation_bytes,
        )?);
    }
    drop(runtime_bones);
    let mut sequences = Vec::with_capacity(sequence_count as usize);
    for index in 0..sequence_count as usize {
        let max = usize::try_from(limits.max_entry_bytes).unwrap_or(usize::MAX);
        decoded_animation_bytes = bone_count
            .checked_mul(4)
            .and_then(|n| decoded_animation_bytes.checked_add(n))
            .filter(|n| *n <= max)
            .ok_or(MdlError::TooMany {
                part: "decoded animation bytes",
                max,
            })?;
        sequences.push(read_sequence(
            bytes,
            offset_at(sequence_offset, index, SEQUENCE_BYTES)?,
            bone_count,
            limits,
        )?);
    }
    let flex_descriptor_count = Reader::at(bytes, 260).count("mdl flex descriptors")?;
    let included_model_count = if bytes.len() >= 344 {
        Reader::at(bytes, 336).count("mdl included models")?
    } else {
        0
    };

    Ok(Mdl {
        name,
        checksum,
        version,
        flags,
        illumination_position,
        hull_min,
        hull_max,
        view_min,
        view_max,
        attachments,
        illumination_attachment_index,
        bones,
        linear_bones,
        textures,
        texture_paths,
        skin_reference_count,
        skin_table,
        body_parts,
        local_animations,
        sequences,
        included_model_count,
        bone_controller_count,
        flex_descriptor_count,
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
    let quaternion_alignment = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
    let flags = r.u32()?;
    let procedural_kind = r.i32()?;
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
        quaternion_alignment,
        flags,
        procedural_kind,
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
        Reader::at(bytes, mesh_base).take(MESH_BYTES)?;
        meshes.push(MdlMesh {
            material,
            vertex_offset,
        });
    }
    Reader::at(bytes, base).take(MODEL_BYTES)?;
    Ok(MdlModel {
        name,
        vertex_offset: vertex_index / VVD_VERTEX_BYTES,
        meshes,
    })
}

fn read_sequence(
    bytes: &[u8],
    base: usize,
    bones: usize,
    limits: &Limits,
) -> Result<MdlSequence, MdlError> {
    Reader::at(bytes, base).take(SEQUENCE_BYTES)?;
    let label_offset = Reader::at(bytes, base + 4).i32()?;
    let label = relative_string(bytes, base, label_offset).ok_or(MdlError::Corrupt {
        part: "mdl sequence label",
    })?;
    let flags = Reader::at(bytes, base + 12).u32()?;
    let mut bounds = Reader::at(bytes, base + 32);
    let bounds_min = bounds.vec3()?;
    let bounds_max = bounds.vec3()?;
    let mut blends = Reader::at(bytes, base + 56);
    let blend_count = blends.count("mdl sequence blends")?;
    let blend_offset = blends.count("mdl sequence animation offset")?;
    blends.take(4)?;
    let blend_grid = [
        blends.count("mdl sequence grid")?,
        blends.count("mdl sequence grid")?,
    ];
    let count = blend_grid[0]
        .checked_mul(blend_grid[1])
        .ok_or(MdlError::Corrupt {
            part: "mdl sequence grid",
        })?;
    if count != blend_count || count == 0 || count > limits.max_entries {
        return Err(MdlError::Corrupt {
            part: "mdl sequence grid",
        });
    }
    let pose_parameters = [blends.i32()?, blends.i32()?];
    let mut animation_reader = Reader::at(
        bytes,
        base.checked_add(blend_offset).ok_or(MdlError::Corrupt {
            part: "mdl sequence animation offset",
        })?,
    );
    let mut animations = Vec::with_capacity(count);
    for _ in 0..count {
        let index = i16::from_le_bytes(
            animation_reader
                .take(2)?
                .try_into()
                .expect("animation index"),
        );
        animations.push(usize::try_from(index).map_err(|_| MdlError::Corrupt {
            part: "mdl sequence animation index",
        })?);
    }
    let mut extra = Reader::at(bytes, base + 144);
    let ik_rule_count = extra.count("mdl sequence IK rules")?;
    let autolayer_count = extra.count("mdl sequence layers")?;
    extra.take(4)?;
    let weight_offset = extra.count("mdl sequence weight offset")?;
    extra.take(4)?;
    let ik_lock_count = extra.count("mdl sequence IK locks")?;
    let bone_weights = if weight_offset == 0 {
        vec![1.0; bones]
    } else {
        let mut weights = Reader::at(
            bytes,
            base.checked_add(weight_offset).ok_or(MdlError::Corrupt {
                part: "mdl sequence weights",
            })?,
        );
        (0..bones)
            .map(|_| weights.f32())
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(MdlSequence {
        label,
        flags,
        bounds_min,
        bounds_max,
        animations,
        blend_grid,
        pose_parameters,
        autolayer_count,
        ik_rule_count,
        ik_lock_count,
        bone_weights,
    })
}

fn read_animation_description(
    bytes: &[u8],
    base: usize,
    bones: &[MdlBone],
    limits: &Limits,
    decoded_bytes: &mut usize,
) -> Result<MdlAnimationDescription, MdlError> {
    Reader::at(bytes, base).take(ANIM_DESC_BYTES)?;
    let mut r = Reader::at(bytes, base + 4);
    let name_offset = r.i32()?;
    let fps = r.f32()?;
    let flags = r.u32()?;
    let frame_count = r.count("mdl frame count")?.max(1);
    r.take(32)?;
    let animation_block = r.i32()?;
    let animation_index = r.count("mdl animation index")?;
    let ik_rule_count = r.count("mdl animation IK rules")?;
    r.take(8)?;
    let hierarchy_count = r.count("mdl animation hierarchy")?;
    r.take(4)?;
    let section_index = r.count("mdl animation sections")?;
    let section_frames = r.count("mdl animation section frames")?;
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
    let mut sections = Vec::new();
    let decode_channels = flags & (STUDIO_ALLZEROS | STUDIO_RESERVED_ENCODING) == 0;
    if decode_channels && section_frames != 0 {
        let count = frame_count
            .checked_div(section_frames)
            .and_then(|count| count.checked_add(2))
            .ok_or(MdlError::Corrupt {
                part: "mdl animation section count",
            })?;
        if count > limits.max_entries {
            return Err(MdlError::TooMany {
                part: "mdl animation sections",
                max: limits.max_entries,
            });
        }
        let mut descriptors = Reader::at(
            bytes,
            base.checked_add(section_index).ok_or(MdlError::Corrupt {
                part: "mdl animation section offset",
            })?,
        );
        for section in 0..count {
            let block = descriptors.i32()?;
            let offset = descriptors.count("mdl animation section data")?;
            let frames = if section + 1 == count {
                1
            } else {
                section_frames
                    .saturating_add(1)
                    .min(frame_count.saturating_sub(section * section_frames))
                    .max(1)
            };
            let data = if block == 0 && offset != 0 {
                read_animation_chain(
                    bytes,
                    base.checked_add(offset).ok_or(MdlError::Corrupt {
                        part: "mdl animation section data",
                    })?,
                    frames,
                    bones,
                    limits,
                    decoded_bytes,
                )?
            } else {
                Vec::new()
            };
            sections.push(MdlAnimationSection {
                animations: data,
                external_block: (block != 0).then_some(block),
            });
        }
    } else if decode_channels && animation_block == 0 && animation_index != 0 {
        animations = read_animation_chain(
            bytes,
            base.checked_add(animation_index).ok_or(MdlError::Corrupt {
                part: "mdl animation offset",
            })?,
            frame_count,
            bones,
            limits,
            decoded_bytes,
        )?;
    }
    Ok(MdlAnimationDescription {
        name,
        fps,
        flags,
        frame_count,
        animations,
        sections,
        section_frames,
        external_block: (animation_block != 0).then_some(animation_block),
        hierarchy_count,
        ik_rule_count,
    })
}

fn read_animation_chain(
    bytes: &[u8],
    mut offset: usize,
    frames: usize,
    bones: &[MdlBone],
    limits: &Limits,
    decoded_bytes: &mut usize,
) -> Result<Vec<MdlAnimation>, MdlError> {
    let mut animations = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    loop {
        if Reader::at(bytes, offset).take(4)?[0] == 255 {
            break;
        }
        if animations.len() >= bones.len().min(limits.max_entries) {
            return Err(MdlError::Corrupt {
                part: "mdl animation channel count",
            });
        }
        let flags = Reader::at(bytes, offset + 1).u8()?;
        let channels =
            usize::from(flags & ANIM_ANIMROT != 0) + usize::from(flags & ANIM_ANIMPOS != 0);
        let expanded = frames
            .checked_mul(channels)
            .and_then(|n| n.checked_mul(12))
            .and_then(|n| decoded_bytes.checked_add(n));
        let max = usize::try_from(limits.max_entry_bytes).unwrap_or(usize::MAX);
        *decoded_bytes = expanded.filter(|n| *n <= max).ok_or(MdlError::TooMany {
            part: "decoded animation bytes",
            max,
        })?;
        let (animation, next_offset) = read_animation(bytes, offset, frames, bones)?;
        if usize::from(animation.bone) >= bones.len() || !seen.insert(animation.bone) {
            return Err(MdlError::Corrupt {
                part: "mdl animation channel bone",
            });
        }
        animations.push(animation);
        if next_offset == 0 {
            break;
        }
        offset = offset.checked_add(next_offset).ok_or(MdlError::Corrupt {
            part: "mdl animation chain",
        })?;
    }
    Ok(animations)
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
    let next_offset =
        usize::try_from(i16::from_le_bytes([r.u8()?, r.u8()?])).map_err(|_| MdlError::Corrupt {
            part: "mdl animation next offset",
        })?;
    let scales = bones.get(usize::from(bone));

    let offset = base + 4;
    let rotation = if flags & ANIM_RAWROT != 0 {
        let q = read_quaternion48(bytes, offset)?;
        RotationData::Raw(q)
    } else if flags & ANIM_RAWROT2 != 0 {
        let q = read_quaternion64(bytes, offset)?;
        RotationData::Raw(q)
    } else if flags & ANIM_ANIMROT != 0 {
        let scale = scales.map_or([1.0; 3], |bone| bone.rotation_scale);
        let values = read_rle_frames(bytes, offset, frames)?
            .into_iter()
            .map(|value| {
                [
                    value[2] * scale[2],
                    value[0] * scale[0],
                    value[1] * scale[1],
                ]
            })
            .collect();
        RotationData::Animated(values)
    } else {
        RotationData::None
    };

    let position = if flags & ANIM_RAWPOS != 0 {
        PositionData::Raw(r_take3_half(
            bytes,
            offset
                + usize::from(flags & ANIM_RAWROT != 0) * 6
                + usize::from(flags & ANIM_RAWROT2 != 0) * 8,
        )?)
    } else if flags & ANIM_ANIMPOS != 0 {
        let scale = scales.map_or([1.0; 3], |bone| bone.position_scale);
        let values = read_rle_frames(
            bytes,
            offset + usize::from(flags & ANIM_ANIMROT != 0) * 6,
            frames,
        )?
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
            flags,
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

/// Positive signed offsets address `[valid:u8,total:u8,i16...]` spans.
fn read_rle_frames(bytes: &[u8], at: usize, frames: usize) -> Result<Vec<[f32; 3]>, MdlError> {
    let mut r = Reader::at(bytes, at);
    let pointers = [
        i16::from_le_bytes(r.take(2)?.try_into().expect("2 bytes")),
        i16::from_le_bytes(r.take(2)?.try_into().expect("2 bytes")),
        i16::from_le_bytes(r.take(2)?.try_into().expect("2 bytes")),
    ];
    let mut channels: [Vec<f32>; 3] = Default::default();
    for (channel, pointer) in channels.iter_mut().zip(pointers) {
        if pointer > 0 {
            *channel = read_rle_channel(bytes, at + pointer as usize, frames)?;
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

#[cfg(test)]
static SPAN_HEADER_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn read_rle_channel(bytes: &[u8], mut at: usize, frames: usize) -> Result<Vec<f32>, MdlError> {
    let mut out = Vec::with_capacity(frames);
    while out.len() < frames {
        #[cfg(test)]
        SPAN_HEADER_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut r = Reader::at(bytes, at);
        let header = r.take(2)?;
        let (valid, total) = (usize::from(header[0]), usize::from(header[1]));
        if total == 0 || valid == 0 || valid > total {
            return Err(MdlError::Corrupt {
                part: "mdl animation rle span",
            });
        }
        let values = r.take(valid * 2)?;
        for slot in 0..total.min(frames - out.len()) {
            let index = slot.min(valid - 1) * 2;
            out.push(f32::from(i16::from_le_bytes([
                values[index],
                values[index + 1],
            ])));
        }
        at = at.checked_add((valid + 1) * 2).ok_or(MdlError::Corrupt {
            part: "mdl animation rle offset",
        })?;
    }
    Ok(out)
}

fn offset_at(base: usize, index: usize, stride: usize) -> Result<usize, MdlError> {
    index
        .checked_mul(stride)
        .and_then(|delta| base.checked_add(delta))
        .ok_or(MdlError::Corrupt {
            part: "mdl record offset",
        })
}

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

fn fixed_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|byte| *byte == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{SPAN_HEADER_READS, read_rle_channel};

    fn animation_bone() -> super::MdlBone {
        super::MdlBone {
            name: String::new(),
            parent: -1,
            position: [10.0, 20.0, 30.0],
            quaternion: [0.0, 0.0, 0.0, 1.0],
            rotation: [0.0; 3],
            position_scale: [1.0; 3],
            rotation_scale: [1.0; 3],
            quaternion_alignment: [0.0, 0.0, 0.0, 1.0],
            flags: 0,
            pose_to_bone: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            procedural_kind: 0,
        }
    }
    #[test]
    fn rle_repeats_last_valid_sample_and_rejects_malformed_spans() {
        assert_eq!(
            read_rle_channel(&[2, 4, 1, 0, 254, 255], 0, 4).unwrap(),
            vec![1.0, -2.0, -2.0, -2.0]
        );
        for bad in [
            vec![0, 0],
            vec![0, 2],
            vec![3, 2, 0, 0, 0, 0, 0, 0],
            vec![1, 2],
        ] {
            assert!(read_rle_channel(&bad, 0, 2).is_err());
        }
        let signed = [255, 255, 0, 0, 0, 0];
        assert_eq!(
            super::read_rle_frames(&signed, 0, 2).unwrap(),
            vec![[0.0; 3]; 2]
        );
    }
    #[test]
    fn runtime_adds_bind_offsets_and_preserves_stored_accessor_semantics() {
        let bone = animation_bone();
        let channel = super::MdlAnimation::positions_for_tests(0, vec![[1.0, 2.0, 3.0]]);
        assert_eq!(channel.position(0), [1.0, 2.0, 3.0]);
        assert_eq!(channel.local_pose(&bone, 0).unwrap().0, [11.0, 22.0, 33.0]);
        let rotation = super::MdlAnimation {
            bone: 0,
            flags: super::ANIM_ANIMROT,
            position: super::PositionData::None,
            rotation: super::RotationData::Animated(vec![[std::f32::consts::FRAC_PI_2, 0.0, 0.0]]),
        };
        let (_, q) = rotation.local_pose(&bone, 0).unwrap();
        assert!((q[3] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert_eq!(q[1], 0.0);
        assert_eq!(q[2], 0.0);
    }
    #[test]
    fn all_zero_and_reserved_encoding_metadata_do_not_decode_unused_rle_payload() {
        let mut bytes = vec![0u8; 105];
        bytes[4..8].copy_from_slice(&100i32.to_le_bytes());
        bytes[8..12].copy_from_slice(&40.0f32.to_le_bytes());
        bytes[16..20].copy_from_slice(&101i32.to_le_bytes());
        bytes[56..60].copy_from_slice(&90000i32.to_le_bytes());
        bytes[80..84].copy_from_slice(&90000i32.to_le_bytes());
        bytes[84..88].copy_from_slice(&30i32.to_le_bytes());
        bytes[100..].copy_from_slice(b"clip\0");
        for flags in [super::STUDIO_ALLZEROS, super::STUDIO_RESERVED_ENCODING] {
            bytes[12..16].copy_from_slice(&flags.to_le_bytes());
            let clip = super::read_animation_description(
                &bytes,
                0,
                &[animation_bone()],
                &crate::Limits::default(),
                &mut 0,
            )
            .unwrap();
            assert_eq!(clip.flags, flags);
            assert_eq!(clip.frame_count, 101);
            assert!(clip.animations.is_empty());
            assert!(clip.sections.is_empty());
        }
        bytes[12..16].copy_from_slice(&0u32.to_le_bytes());
        assert!(
            super::read_animation_description(
                &bytes,
                0,
                &[animation_bone()],
                &crate::Limits::default(),
                &mut 0
            )
            .is_err()
        );
    }

    #[test]
    fn decoded_animation_budget_is_checked_before_expansion() {
        let bytes = [0, super::ANIM_ANIMPOS, 0, 0, 0, 0, 0, 0, 0, 0];
        let limits = crate::Limits {
            max_entry_bytes: 16,
            ..Default::default()
        };
        let mut total = 0;
        assert!(matches!(
            super::read_animation_chain(&bytes, 0, 2, &[animation_bone()], &limits, &mut total),
            Err(crate::mdl::MdlError::TooMany {
                part: "decoded animation bytes",
                max: 16
            })
        ));
    }
    #[test]
    fn duplicate_channels_and_backward_links_are_rejected() {
        let limits = crate::Limits::default();
        let bones = [animation_bone(), animation_bone()];
        for bytes in [vec![0, 0, 4, 0, 0, 0, 0, 0], vec![0, 0, 255, 255]] {
            assert!(super::read_animation_chain(&bytes, 0, 1, &bones, &limits, &mut 0).is_err());
        }
    }

    #[test]
    fn authored_view_bounds_remain_distinct_from_hull_bounds() {
        let mut bytes = vec![0; 512];
        bytes[..4].copy_from_slice(b"IDST");
        bytes[4..8].copy_from_slice(&48_i32.to_le_bytes());
        let bounds = [
            -1.0_f32, -2.0, -3.0, 1.0, 2.0, 3.0, -4.0, -5.0, -6.0, 7.0, 8.0, 9.0,
        ];
        for (index, value) in bounds.into_iter().enumerate() {
            let offset = 104 + index * 4;
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        let model = super::parse_mdl(&bytes, &crate::Limits::default()).unwrap();
        assert_eq!(model.hull_min, [-1.0, -2.0, -3.0]);
        assert_eq!(model.hull_max, [1.0, 2.0, 3.0]);
        assert_eq!(model.view_min, [-4.0, -5.0, -6.0]);
        assert_eq!(model.view_max, [7.0, 8.0, 9.0]);
    }

    #[test]
    fn rle_channel_walks_a_degenerate_span_chain_linearly() {
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

        assert_eq!(SPAN_HEADER_READS.load(Ordering::Relaxed), FRAMES);
    }
}
