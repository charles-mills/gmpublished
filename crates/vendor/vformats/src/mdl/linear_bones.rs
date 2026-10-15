//! Optional v48/v49 studio-header linear bone arrays.
use super::studio::MdlBone;
use super::{MdlError, Reader};
use crate::Limits;

/// Optimized bone arrays referenced by `studiohdr2_t::linearboneindex`.
///
/// Keep these separate from the full bone records: native bone setup and the
/// optimized renderer prefer these arrays when present, while diagnostic APIs
/// may expose the full records. Layout: Valve `studio.h::mstudiolinearbone_t`.
#[derive(Clone, Debug, PartialEq)]
pub struct MdlLinearBones {
    /// Authored bone flag words.
    pub flags: Vec<u32>,
    /// Parent indices, with -1 for roots.
    pub parents: Vec<i32>,
    /// Parent-local bind positions.
    pub positions: Vec<[f32; 3]>,
    /// Stored XYZW bind quaternions.
    pub quaternions: Vec<[f32; 4]>,
    /// Bind Euler rotations.
    pub rotations: Vec<[f32; 3]>,
    /// Row-major inverse-bind matrices.
    pub pose_to_bone: Vec<[[f32; 4]; 3]>,
    /// Animation position channel scales.
    pub position_scales: Vec<[f32; 3]>,
    /// Animation rotation channel scales.
    pub rotation_scales: Vec<[f32; 3]>,
    /// Authored quaternion alignment values.
    pub quaternion_alignment: Vec<[f32; 4]>,
}

impl MdlLinearBones {
    pub(super) fn apply_to(&self, full: &[MdlBone]) -> Result<Vec<MdlBone>, MdlError> {
        let count = full.len();
        if [
            self.flags.len(),
            self.parents.len(),
            self.positions.len(),
            self.quaternions.len(),
            self.rotations.len(),
            self.pose_to_bone.len(),
            self.position_scales.len(),
            self.rotation_scales.len(),
            self.quaternion_alignment.len(),
        ]
        .into_iter()
        .any(|n| n != count)
        {
            return Err(MdlError::Corrupt {
                part: "mdl linear/full bone count mismatch",
            });
        }
        Ok(full
            .iter()
            .enumerate()
            .map(|(i, bone)| MdlBone {
                name: bone.name.clone(),
                parent: self.parents[i],
                position: self.positions[i],
                quaternion: self.quaternions[i],
                rotation: self.rotations[i],
                position_scale: self.position_scales[i],
                rotation_scale: self.rotation_scales[i],
                pose_to_bone: self.pose_to_bone[i],
            })
            .collect())
    }
}

pub(super) fn read(
    bytes: &[u8],
    bone_count: usize,
    limits: &Limits,
) -> Result<Option<MdlLinearBones>, MdlError> {
    let mut header = Reader::at(bytes, 400);
    let header2 = header.count("mdl secondary header offset")?;
    if header2 == 0 {
        return Ok(None);
    }
    if header2 < 408 {
        return Err(MdlError::Corrupt {
            part: "mdl secondary header overlaps primary header",
        });
    }
    let mut secondary = Reader::at(bytes, header2);
    secondary.take(16)?;
    let offset = secondary.count("mdl linear bone offset")?;
    if offset == 0 {
        return Ok(None);
    }
    let base = header2.checked_add(offset).ok_or(MdlError::Corrupt {
        part: "mdl linear bone offset",
    })?;
    let mut table = Reader::at(bytes, base);
    let count = table.count("mdl linear bone count")?;
    if count > limits.max_entries {
        return Err(MdlError::TooMany {
            part: "mdl linear bones",
            max: limits.max_entries,
        });
    }
    if count != bone_count {
        return Err(MdlError::Corrupt {
            part: "mdl linear/full bone count mismatch",
        });
    }
    let mut offsets = [0usize; 9];
    for at in &mut offsets {
        *at = table.count("mdl linear bone array offset")?;
    }
    table.take(24)?; // reserved words complete the 64-byte wire header
    let reader = |index: usize, stride: usize| -> Result<Reader<'_>, MdlError> {
        if count > 0 && offsets[index] < 64 {
            return Err(MdlError::Corrupt {
                part: "mdl linear bone array overlaps header",
            });
        }
        let at = base.checked_add(offsets[index]).ok_or(MdlError::Corrupt {
            part: "mdl linear bone array offset",
        })?;
        let length = count.checked_mul(stride).ok_or(MdlError::Corrupt {
            part: "mdl linear bone array length",
        })?;
        Reader::at(bytes, at).take(length)?;
        Ok(Reader::at(bytes, at))
    };
    let mut flags = reader(0, 4)?;
    let mut parents = reader(1, 4)?;
    let matrices = read_vectors::<12>(reader(5, 48)?, count)?
        .into_iter()
        .map(|v| {
            [
                [v[0], v[1], v[2], v[3]],
                [v[4], v[5], v[6], v[7]],
                [v[8], v[9], v[10], v[11]],
            ]
        })
        .collect();
    Ok(Some(MdlLinearBones {
        flags: (0..count).map(|_| flags.u32()).collect::<Result<_, _>>()?,
        parents: (0..count)
            .map(|_| parents.i32())
            .collect::<Result<_, _>>()?,
        positions: read_vectors::<3>(reader(2, 12)?, count)?,
        quaternions: read_vectors::<4>(reader(3, 16)?, count)?,
        rotations: read_vectors::<3>(reader(4, 12)?, count)?,
        pose_to_bone: matrices,
        position_scales: read_vectors::<3>(reader(6, 12)?, count)?,
        rotation_scales: read_vectors::<3>(reader(7, 12)?, count)?,
        quaternion_alignment: read_vectors::<4>(reader(8, 16)?, count)?,
    }))
}

fn read_vectors<const N: usize>(
    mut input: Reader<'_>,
    count: usize,
) -> Result<Vec<[f32; N]>, MdlError> {
    (0..count)
        .map(|_| {
            let mut vector = [0.0; N];
            for value in &mut vector {
                *value = input.f32()?;
            }
            Ok(vector)
        })
        .collect()
}
