use std::sync::atomic::AtomicBool;

use mdlc::smd::SmdPose;
use vformats::mdl::Mdl;

use super::{Animation, DecompileError, Reader, check_cancelled, invalid};

pub(super) fn animation(
    reader: &Reader<'_>,
    ani: Option<&[u8]>,
    model: &Mdl,
    record: usize,
    cancelled: &AtomicBool,
) -> Result<Animation, DecompileError> {
    let fps = reader.f32(record + 8)?;
    if fps <= 0.0 {
        return Err(invalid("Animation frame rate must be positive"));
    }
    let flags = reader.i32(record + 12)? as u32;
    let count = reader.count(record + 16)?;
    if count == 0 {
        return Err(invalid("Animation contains no frames"));
    }
    let section_frames = reader.count(record + 84)?;
    let sections = if section_frames > 0 {
        Some(reader.relative(record, record + 80)?)
    } else {
        None
    };
    let mut frames = Vec::with_capacity(count);
    for frame in 0..count {
        if frame.is_multiple_of(64) {
            check_cancelled(cancelled)?;
        }
        let (block, index, local_frame) = if let Some(sections) = sections {
            let (section, local) = if count > section_frames && frame == count - 1 {
                (count / section_frames + 1, 0)
            } else {
                (frame / section_frames, frame % section_frames)
            };
            let section = sections
                .checked_add(section.checked_mul(8).ok_or(DecompileError::SizeLimit)?)
                .ok_or(DecompileError::SizeLimit)?;
            (reader.i32(section)?, reader.count(section + 4)?, local)
        } else {
            (reader.i32(record + 52)?, reader.count(record + 56)?, frame)
        };
        let (data, start) = if block == 0 {
            (
                reader.0,
                record.checked_add(index).ok_or(DecompileError::SizeLimit)?,
            )
        } else if block > 0 {
            let ani = ani.ok_or_else(|| {
                invalid("The matching .ani file is required to recover external animations")
            })?;
            let blocks = reader.count(352)?;
            let block = block as usize;
            if block >= blocks {
                return Err(invalid("Animation references an absent external block"));
            }
            let table = reader.count(356)?;
            let entry = table
                .checked_add(block.checked_mul(8).ok_or(DecompileError::SizeLimit)?)
                .ok_or(DecompileError::SizeLimit)?;
            let begin = reader.count(entry)?;
            let end = reader.count(entry + 4)?;
            let data = ani
                .get(begin..end)
                .ok_or_else(|| invalid("External animation block is outside its file"))?;
            (data, index)
        } else {
            return Err(invalid("Animation contains an unavailable data block"));
        };
        let data = Reader(data);
        let delta = flags & 4 != 0;
        let mut poses = model
            .bones
            .iter()
            .enumerate()
            .map(|(index, bone)| {
                let root =
                    bone.parent < 0 && !delta && model.flags & vformats::mdl::FLAG_STATIC_PROP == 0;
                SmdPose {
                    bone: index as i32,
                    position: if delta {
                        [0.0; 3]
                    } else if root {
                        inverse_root(bone.position)
                    } else {
                        bone.position
                    },
                    rotation: if delta {
                        [0.0; 3]
                    } else if root {
                        mdlc::bone_math::quaternion_angles(undo_root_rotation(bone.quaternion))
                    } else {
                        bone.rotation
                    },
                }
            })
            .collect::<Vec<_>>();
        if block > 0 && flags & 0x40 != 0 {
            raw_frame(&data, start, local_frame, model, delta, &mut poses)?;
        } else if (block > 0 || index != 0) && flags & 0x20 == 0 {
            chain_frame(&data, start, local_frame, model, delta, &mut poses)?;
        }
        frames.push(poses);
    }
    Ok(Animation { fps, flags, frames })
}

fn chain_frame(
    data: &Reader<'_>,
    start: usize,
    frame: usize,
    model: &Mdl,
    delta: bool,
    poses: &mut [SmdPose],
) -> Result<(), DecompileError> {
    let mut current = start;
    let mut seen = vec![false; model.bones.len()];
    loop {
        let bone_index = usize::from(data.u8(current)?);
        if bone_index == 255 {
            break;
        }
        let bone = model
            .bones
            .get(bone_index)
            .ok_or_else(|| invalid("Animation references an absent bone"))?;
        if std::mem::replace(&mut seen[bone_index], true) {
            return Err(invalid("Animation has duplicate bone channels"));
        }
        let flags = data.u8(current + 1)?;
        if flags & 0xc0 != 0 || (flags & 2 != 0 && flags & 0x20 != 0) {
            return Err(invalid("Unsupported animation channel encoding"));
        }
        let channel_delta = flags & 0x10 != 0;
        if channel_delta != delta {
            return Err(invalid(
                "Animation delta flags do not match its bone channels",
            ));
        }
        let raw_rotation = flags & (2 | 0x20) != 0;
        let animated_rotation = flags & 8 != 0;
        let raw_position = flags & 1 != 0;
        let animated_position = flags & 4 != 0;
        if (raw_rotation || raw_position) && (animated_rotation || animated_position) {
            return Err(invalid("Animation channel payloads overlap"));
        }
        let payload = current.checked_add(4).ok_or(DecompileError::SizeLimit)?;
        let rotation = if flags & 2 != 0 {
            quaternion48(data, payload)?
        } else if flags & 0x20 != 0 {
            quaternion64(data, payload)?
        } else if animated_rotation {
            let values = rle(data, payload, frame)?;
            let angles = std::array::from_fn(|axis| {
                values[axis] * bone.rotation_scale[axis]
                    + if delta { 0.0 } else { bone.rotation[axis] }
            });
            mdlc::bone_math::angle_quaternion(angles)
        } else if delta {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            bone.quaternion
        };
        let position_pointer = payload + if animated_rotation { 6 } else { 0 };
        let raw_pointer =
            payload + if flags & 2 != 0 { 6 } else { 0 } + if flags & 0x20 != 0 { 8 } else { 0 };
        let mut position = if raw_position {
            vector48(data, raw_pointer)?
        } else if animated_position {
            let values = rle(data, position_pointer, frame)?;
            std::array::from_fn(|axis| {
                values[axis] * bone.position_scale[axis]
                    + if delta { 0.0 } else { bone.position[axis] }
            })
        } else if delta {
            [0.0; 3]
        } else {
            bone.position
        };
        let mut rotation = rotation;
        if bone.parent < 0 && !delta && model.flags & vformats::mdl::FLAG_STATIC_PROP == 0 {
            rotation = undo_root_rotation(rotation);
            position = inverse_root(position);
        }
        let rotation = mdlc::bone_math::quaternion_angles(rotation);
        if !position
            .iter()
            .chain(&rotation)
            .all(|value| value.is_finite())
        {
            return Err(invalid("Decoded animation pose is nonfinite"));
        }
        poses[bone_index].position = position;
        poses[bone_index].rotation = rotation;
        let next = data.i16(current + 2)?;
        if next == 0 {
            break;
        }
        if next < 4 {
            return Err(invalid("Animation chain offset is invalid"));
        }
        current = current
            .checked_add(next as usize)
            .ok_or(DecompileError::SizeLimit)?;
    }
    Ok(())
}

fn raw_frame(
    data: &Reader<'_>,
    start: usize,
    frame: usize,
    model: &Mdl,
    delta: bool,
    poses: &mut [SmdPose],
) -> Result<(), DecompileError> {
    if delta {
        return Err(invalid("External raw delta animations are not supported"));
    }
    let constants = data.count(start)?;
    let samples = data.count(start + 4)?;
    let stride = data.count(start + 8)?;
    if constants < 24 + poses.len() || samples < constants {
        return Err(invalid("Invalid raw animation header"));
    }
    let mut constant = start
        .checked_add(constants)
        .ok_or(DecompileError::SizeLimit)?;
    let mut sample = start
        .checked_add(samples)
        .and_then(|offset| {
            frame
                .checked_mul(stride)
                .and_then(|frame| offset.checked_add(frame))
        })
        .ok_or(DecompileError::SizeLimit)?;
    let sample_begin = sample;
    for (index, (pose, bone)) in poses.iter_mut().zip(&model.bones).enumerate() {
        let flags = data.u8(start + 24 + index)?;
        if flags & !0xc5 != 0 || flags & 0xc0 == 0xc0 || flags & 5 == 5 {
            return Err(invalid("Invalid raw animation bone flags"));
        }
        let rotation = if flags & 0x40 != 0 {
            let rotation = quaternion48s(data, constant)?;
            constant += 6;
            Some(rotation)
        } else if flags & 0x80 != 0 {
            let rotation = quaternion48s(data, sample)?;
            sample += 6;
            Some(rotation)
        } else {
            None
        };
        let position = if flags & 1 != 0 {
            let position = vector48(data, constant)?;
            constant += 6;
            Some(position)
        } else if flags & 4 != 0 {
            let position = vector48(data, sample)?;
            sample += 6;
            Some(position)
        } else {
            None
        };
        if let Some(rotation) = rotation {
            let rotation = if bone.parent < 0 {
                undo_root_rotation(rotation)
            } else {
                rotation
            };
            pose.rotation = mdlc::bone_math::quaternion_angles(rotation);
        }
        if let Some(position) = position {
            pose.position = if bone.parent < 0 {
                inverse_root(position)
            } else {
                position
            };
        }
    }
    if constant > start + samples || sample - sample_begin != stride {
        return Err(invalid("Raw animation stride does not match its channels"));
    }
    Ok(())
}

fn inverse_root(value: [f32; 3]) -> [f32; 3] {
    [value[1], -value[0], value[2]]
}

fn undo_root_rotation(quaternion: [f32; 4]) -> [f32; 4] {
    mdlc::ani_writer::quaternion_mul(
        [
            0.0,
            0.0,
            -std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ],
        quaternion,
    )
}

pub(super) fn rle(data: &Reader<'_>, at: usize, frame: usize) -> Result<[f32; 3], DecompileError> {
    let mut values = [0.0; 3];
    for (axis, value) in values.iter_mut().enumerate() {
        let offset = data.i16(at + axis * 2)?;
        if offset == 0 {
            continue;
        }
        if offset < 6 {
            return Err(invalid("Invalid animation value pointer"));
        }
        let mut current = at
            .checked_add(offset as usize)
            .ok_or(DecompileError::SizeLimit)?;
        let mut remaining = frame;
        loop {
            let valid = usize::from(data.u8(current)?);
            let total = usize::from(data.u8(current + 1)?);
            if valid == 0 || total == 0 || valid > total {
                return Err(invalid("Invalid animation RLE span"));
            }
            data.bytes(current + 2, valid * 2)?;
            if remaining < total {
                *value = f32::from(data.i16(current + 2 + remaining.min(valid - 1) * 2)?);
                break;
            }
            remaining -= total;
            current = current
                .checked_add(2 + valid * 2)
                .ok_or(DecompileError::SizeLimit)?;
        }
    }
    Ok(values)
}

fn quaternion64(data: &Reader<'_>, at: usize) -> Result<[f32; 4], DecompileError> {
    let bits = u64::from_le_bytes(data.bytes(at, 8)?.try_into().unwrap());
    let component =
        |shift: u32| (((bits >> shift) & 0x1f_ffff_u64) as f32 - 1_048_576.0) / 1_048_576.5;
    quaternion(
        [component(0), component(21), component(42)],
        bits >> 63 != 0,
    )
}

fn quaternion48(data: &Reader<'_>, at: usize) -> Result<[f32; 4], DecompileError> {
    let x = (f32::from(data.u16(at)?) - 32768.0) / 32768.0;
    let y = (f32::from(data.u16(at + 2)?) - 32768.0) / 32768.0;
    let z = data.u16(at + 4)?;
    quaternion(
        [x, y, (f32::from(z & 0x7fff) - 16384.0) / 16384.0],
        z & 0x8000 != 0,
    )
}

fn quaternion48s(data: &Reader<'_>, at: usize) -> Result<[f32; 4], DecompileError> {
    let packed = [data.u16(at)?, data.u16(at + 2)?, data.u16(at + 4)?];
    let first = usize::from(((packed[0] >> 15) << 1) | (packed[1] >> 15));
    let mut quaternion = [0.0; 4];
    for axis in 0..3 {
        quaternion[(first + axis) & 3] = (f32::from(packed[axis] & 0x7fff) - 16384.0) / 23168.0;
    }
    let remaining = (1.0 - quaternion.iter().map(|value| value * value).sum::<f32>())
        .max(0.0)
        .sqrt();
    quaternion[(first + 3) & 3] = if packed[2] & 0x8000 != 0 {
        -remaining
    } else {
        remaining
    };
    Ok(quaternion)
}

fn quaternion(xyz: [f32; 3], negative: bool) -> Result<[f32; 4], DecompileError> {
    let squared = xyz.iter().map(|value| value * value).sum::<f32>();
    if squared > 1.001 {
        return Err(invalid("Invalid compressed animation quaternion"));
    }
    let w = (1.0 - squared).max(0.0).sqrt();
    Ok([xyz[0], xyz[1], xyz[2], if negative { -w } else { w }])
}

fn vector48(data: &Reader<'_>, at: usize) -> Result<[f32; 3], DecompileError> {
    let mut vector = [0.0; 3];
    for (axis, value) in vector.iter_mut().enumerate() {
        let half = data.u16(at + axis * 2)?;
        let mantissa = half & 0x3ff;
        let exponent = (half >> 10) & 31;
        if exponent == 31 {
            return Err(invalid("Nonfinite compressed animation position"));
        }
        let sign = if half & 0x8000 != 0 { -1.0 } else { 1.0 };
        *value = if exponent == 0 {
            sign * f32::from(mantissa) * 2.0_f32.powi(-24)
        } else {
            sign * (1.0 + f32::from(mantissa) / 1024.0) * 2.0_f32.powi(i32::from(exponent) - 15)
        };
    }
    Ok(vector)
}
