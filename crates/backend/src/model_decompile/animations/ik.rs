use std::fmt::Write as _;

use mdlc::{bone_math as math, model::Movement};
use vformats::mdl::Mdl;

use super::{Animation, DecompileError, Reader, invalid, quoted};

pub(super) fn recover(
    reader: &Reader<'_>,
    model: &Mdl,
    record: usize,
    animation: &Animation,
) -> Result<String, DecompileError> {
    let count = reader.count(record + 60)?;
    if count == 0 {
        return Ok(String::new());
    }
    if count > 4096 || animation.frames.is_empty() {
        return Err(invalid("Invalid animation IK rule count"));
    }
    if reader.i32(record + 64)? == 0 {
        return Err(invalid(
            "Animation IK rules require unavailable external data",
        ));
    }
    let base = reader.relative(record, record + 64)?;
    reader.bytes(base, count * 152)?;
    let chains = reader.table(284, 288, 16)?;
    let movements = movements(reader, record)?;
    let worlds = worlds(model, animation)?;
    let mut output = String::new();
    for index in 0..count {
        let at = base + index * 152;
        let kind = reader.i32(at + 4)?;
        if animation.flags & 4 != 0 && !matches!(kind, 4 | 6) {
            return Err(invalid(
                "IK rules on delta animations need their original reference poses",
            ));
        }
        let chain = reader.count(at + 8)?;
        let chain_record = *chains
            .get(chain)
            .ok_or_else(|| invalid("IK rule references an absent chain"))?;
        if reader.count(chain_record + 8)? != 3 {
            return Err(invalid("IK rule requires a three-bone chain"));
        }
        let links = reader.relative(chain_record, chain_record + 12)?;
        reader.bytes(links, 3 * 28)?;
        let tip = reader.count(links + 56)?;
        if tip >= model.bones.len() {
            return Err(invalid("IK chain references an absent bone"));
        }
        let bone = reader.i32(at + 12)?;
        if bone < -1 || bone >= model.bones.len() as i32 {
            return Err(invalid("IK rule references an absent bone"));
        }
        let slot = reader.i32(at + 16)?;
        if slot < 0 {
            return Err(invalid("IK rule has a negative target slot"));
        }
        let height = reader.f32(at + 20)?;
        let radius = reader.f32(at + 24)?;
        let floor = reader.f32(at + 28)?;
        let position = vector(reader, at + 32)?;
        let quaternion = quaternion(reader, at + 44)?;
        let frames = animation.frames.len();
        let mut times = [0; 5];
        for (target, offset) in times.iter_mut().zip([76, 80, 84, 88, 96]) {
            *target = frame(reader, at + offset, frames)?;
        }
        let [start, peak, tail, end, contact] = times;
        if frames == 1 {
            for (offset, expected) in [(76, 0.0), (80, 0.0), (84, 1.0), (88, 1.0), (96, 0.0)] {
                if reader.f32(at + offset)? != expected {
                    return Err(invalid("Unsupported single-frame IK timing"));
                }
            }
        }
        if start < 0
            || start > peak
            || peak > tail
            || tail > end
            || contact < 0
            || end > (frames.saturating_sub(1) * 3) as i32
            || reader.i32(at + 68)? != start
        {
            return Err(invalid("Unsupported animation IK timing"));
        }
        if reader.f32(at + 100)? != 0.0 || reader.f32(at + 104)? != 0.0 {
            return Err(invalid(
                "Authored IK drop/top corrections cannot be reconstructed",
            ));
        }
        let options = match kind {
            1 => format!(
                "touch {}",
                quoted(if bone < 0 {
                    ""
                } else {
                    &model.bones[bone as usize].name
                })?
            ),
            3 if bone == 0 => "footstep".into(),
            4 | 6 if bone == 0 => if kind == 4 { "release" } else { "unlatch" }.into(),
            5 if bone == tip as i32 || bone == -1 => {
                let name = reader.string(at, at + 120)?;
                if name.is_empty() {
                    return Err(invalid("IK attachment rule has no attachment name"));
                }
                format!("attachment {}", quoted(name)?)
            }
            _ => {
                return Err(invalid(
                    "Animation IK rule cannot be represented by standard QC",
                ));
            }
        };
        write!(output, " ikrule {} {options} target {slot} height {height} radius {radius} floor {floor} range {start} {peak} {tail} {end} contact {contact}", quoted(reader.string(chain_record, chain_record)?)?).unwrap();
        if bone == -1 {
            if kind == 5 || position != [0.0; 3] {
                write!(
                    output,
                    " fakeorigin {} {} {}",
                    position[0], position[1], position[2]
                )
                .unwrap();
            }
            if quaternion != [0.0; 4] {
                let rotation = math::quaternion_angles(quaternion).map(f32::to_degrees);
                write!(
                    output,
                    " fakerotate {} {} {}",
                    rotation[0], rotation[1], rotation[2]
                )
                .unwrap();
            }
        }
        let contact_world = sample(&worlds, animation.flags, contact)?;
        if kind == 5 && bone >= 0 {
            let expected = contact_world[bone as usize];
            compare_transform(
                &math::quaternion_local_transform(quaternion, position),
                &expected,
                0.025,
            )?;
        } else if kind == 3 {
            let expected = [contact_world[tip][3], contact_world[tip][7], floor];
            if position
                .iter()
                .zip(expected)
                .any(|(a, b)| (a - b).abs() > 0.025)
                || math::quaternion_angles(quaternion)
                    .iter()
                    .any(|v| v.abs() > 0.001)
            {
                return Err(invalid(
                    "IK footstep uses an authored chain center that cannot be inferred safely",
                ));
            }
        } else if bone >= 0 && (position != [0.0; 3] || quaternion != [0.0; 4]) {
            return Err(invalid("IK rule has unrepresentable authored anchor data"));
        }
        let samples = (end - start + 1 + if end >= frames as i32 { 2 } else { 0 }) as usize;
        if samples > 65_536 {
            return Err(DecompileError::SizeLimit);
        }
        let errors = errors(reader, at, samples)?;
        if errors.is_none() && kind != 4 {
            return Err(invalid("IK rule has no error samples"));
        }
        for offset in 0..samples {
            let t = start + offset as i32;
            let expected = match kind {
                4 | 6 => math::identity(),
                _ => {
                    let world = sample(&worlds, animation.flags, t)?;
                    if kind == 1 {
                        if bone < 0 {
                            world[tip]
                        } else {
                            math::concat(&math::invert(&world[bone as usize]), &world[tip])
                        }
                    } else {
                        let movement = displacement(&movements, frames, t, contact);
                        let mut anchor_position =
                            std::array::from_fn(|axis| position[axis] + movement[axis]);
                        if kind == 3 {
                            let current = [world[tip][3], world[tip][7], anchor_position[2]];
                            let blend = if t >= end {
                                1.0
                            } else if t < peak {
                                (peak - t) as f32 / (peak - start) as f32
                            } else if t > tail {
                                (t - tail) as f32 / (end - tail) as f32
                            } else {
                                0.0
                            };
                            let blend = 3.0 * blend * blend - 2.0 * blend * blend * blend;
                            anchor_position = std::array::from_fn(|axis| {
                                anchor_position[axis] * (1.0 - blend) + current[axis] * blend
                            });
                        }
                        let anchor = math::quaternion_local_transform(quaternion, anchor_position);
                        math::concat(&math::invert(&anchor), &world[tip])
                    }
                }
            };
            if let Some(errors) = &errors {
                let (values, scales) = errors[offset];
                let stored = math::local_transform(
                    [values[0], values[1], values[2]],
                    [values[3], values[4], values[5]],
                );
                let tolerance = scales[..3].iter().copied().fold(0.025, f32::max) * 2.0;
                compare_transform(&stored, &expected, tolerance)?;
            }
        }
    }
    Ok(output)
}

fn vector(reader: &Reader<'_>, at: usize) -> Result<[f32; 3], DecompileError> {
    Ok([reader.f32(at)?, reader.f32(at + 4)?, reader.f32(at + 8)?])
}

fn quaternion(reader: &Reader<'_>, at: usize) -> Result<[f32; 4], DecompileError> {
    let q = [
        reader.f32(at)?,
        reader.f32(at + 4)?,
        reader.f32(at + 8)?,
        reader.f32(at + 12)?,
    ];
    let length = q.iter().map(|v| v * v).sum::<f32>();
    if length != 0.0 && (length - 1.0).abs() > 0.002 {
        return Err(invalid("IK rule contains an invalid quaternion"));
    }
    Ok(q)
}

fn frame(reader: &Reader<'_>, at: usize, frames: usize) -> Result<i32, DecompileError> {
    let cycle = reader.f32(at)?;
    if frames == 1 {
        return Ok(0);
    }
    let value = cycle * (frames - 1) as f32;
    if (value - value.round()).abs() > 0.0001 || value.abs() > 2_000_000.0 {
        return Err(invalid(
            "IK timing cannot be represented as an integral QC frame",
        ));
    }
    Ok(value.round() as i32)
}

fn worlds(model: &Mdl, animation: &Animation) -> Result<Vec<Vec<math::Matrix3x4>>, DecompileError> {
    let root = math::angle_matrix([0.0, 0.0, std::f32::consts::FRAC_PI_2]);
    animation
        .frames
        .iter()
        .map(|frame| {
            if frame.len() != model.bones.len() {
                return Err(invalid("IK animation has an incomplete skeleton"));
            }
            let mut world = Vec::with_capacity(frame.len());
            for (pose, bone) in frame.iter().zip(&model.bones) {
                let local = math::local_transform(pose.position, pose.rotation);
                world.push(if bone.parent < 0 {
                    math::concat(&root, &local)
                } else {
                    let parent = world
                        .get(bone.parent as usize)
                        .ok_or_else(|| invalid("IK skeleton is not parent ordered"))?;
                    math::concat(parent, &local)
                });
            }
            Ok(world)
        })
        .collect()
}

fn sample(
    worlds: &[Vec<math::Matrix3x4>],
    flags: u32,
    frame: i32,
) -> Result<&[math::Matrix3x4], DecompileError> {
    let frame = if flags & 1 != 0 && worlds.len() > 1 {
        frame.rem_euclid((worlds.len() - 1) as i32)
    } else {
        frame
    };
    worlds
        .get(frame as usize)
        .map(Vec::as_slice)
        .ok_or_else(|| invalid("IK rule samples outside its animation"))
}

fn compare_transform(
    stored: &math::Matrix3x4,
    expected: &math::Matrix3x4,
    tolerance: f32,
) -> Result<(), DecompileError> {
    if (0..12).any(|axis| {
        (stored[axis] - expected[axis]).abs() > if axis % 4 == 3 { tolerance } else { 0.003 }
    }) {
        return Err(invalid(
            "Authored IK errors differ from the recovered animation; standard QC would change the result",
        ));
    }
    Ok(())
}

type Errors = Vec<([f32; 6], [f32; 6])>;

fn errors(reader: &Reader<'_>, at: usize, count: usize) -> Result<Option<Errors>, DecompileError> {
    if reader.i32(at + 72)? != 0 {
        return Err(invalid("Uncompressed authored IK errors are not supported"));
    }
    if reader.i32(at + 60)? == 0 {
        return Ok(None);
    }
    let base = reader.relative(at, at + 60)?;
    reader.bytes(base, 36)?;
    let mut result = vec![([0.0; 6], [0.0; 6]); count];
    for channel in 0..6 {
        let scale = reader.f32(base + channel * 4)?;
        if scale < 0.0 {
            return Err(invalid("Negative IK error scale"));
        }
        let offset = reader.i16(base + 24 + channel * 2)?;
        if offset < 0 {
            return Err(invalid("Negative IK error channel offset"));
        }
        if offset == 0 {
            continue;
        }
        let mut cursor = base + offset as usize;
        let mut frame = 0;
        while frame < count {
            let valid = reader.u8(cursor)? as usize;
            let total = reader.u8(cursor + 1)? as usize;
            if valid == 0 || total < valid {
                return Err(invalid("Malformed IK error run"));
            }
            reader.bytes(cursor + 2, valid * 2)?;
            for local in 0..total.min(count - frame) {
                let value = f32::from(reader.i16(cursor + 2 + local.min(valid - 1) * 2)?) * scale;
                if !value.is_finite() {
                    return Err(invalid("IK error sample overflow"));
                }
                result[frame + local].0[channel] = value;
                result[frame + local].1[channel] = scale;
            }
            frame += total;
            cursor += 2 + valid * 2;
        }
    }
    Ok(Some(result))
}

fn movements(reader: &Reader<'_>, record: usize) -> Result<Vec<Movement>, DecompileError> {
    let count = reader.count(record + 20)?;
    if count > 128 {
        return Err(DecompileError::SizeLimit);
    }
    if count == 0 {
        return Ok(vec![]);
    }
    let base = reader.relative(record, record + 24)?;
    reader.bytes(base, count * 44)?;
    let mut output = Vec::with_capacity(count);
    let mut previous = 0;
    for index in 0..count {
        let at = base + index * 44;
        let endframe = reader.i32(at)?;
        if endframe <= previous {
            return Err(invalid("Invalid IK movement interval"));
        }
        output.push(Movement {
            endframe,
            motionflags: reader.i32(at + 4)?,
            v0: reader.f32(at + 8)?,
            v1: reader.f32(at + 12)?,
            angle: reader.f32(at + 16)?,
            vector: vector(reader, at + 20)?,
            position: vector(reader, at + 32)?,
        });
        previous = endframe;
    }
    Ok(output)
}

fn displacement(movements: &[Movement], frames: usize, from: i32, to: i32) -> [f32; 3] {
    let position = |frame: i32| {
        if movements.is_empty() || frames <= 1 {
            return [0.0; 3];
        }
        let period = (frames - 1) as i32;
        let loops = frame.div_euclid(period);
        let frame = frame.rem_euclid(period);
        let mut start = 0;
        let mut base = [0.0; 3];
        for movement in movements {
            if frame <= movement.endframe {
                let t = (frame - start) as f32 / (movement.endframe - start) as f32;
                let d = movement.v0 * t + 0.5 * (movement.v1 - movement.v0) * t * t;
                return std::array::from_fn(|axis| {
                    base[axis]
                        + d * movement.vector[axis]
                        + loops as f32 * movements.last().unwrap().position[axis]
                });
            }
            start = movement.endframe;
            base = movement.position;
        }
        base
    };
    let a = position(from);
    let b = position(to);
    std::array::from_fn(|axis| b[axis] - a[axis])
}

#[cfg(test)]
#[path = "ik_tests.rs"]
mod tests;
