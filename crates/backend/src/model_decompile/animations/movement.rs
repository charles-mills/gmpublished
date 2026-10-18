use std::fmt::Write as _;

use mdlc::bone_math::{angle_matrix, concat, invert, local_transform, matrix_angles};
use mdlc::model::Movement;
use vformats::mdl::Mdl;

use super::{Animation, DecompileError, Reader, invalid};

pub(super) fn restore(
    reader: &Reader<'_>,
    model: &Mdl,
    record: usize,
    animation: &mut Animation,
) -> Result<String, DecompileError> {
    let count = reader.count(record + 20)?;
    if count == 0 {
        return Ok(String::new());
    }
    if count > 64 || animation.frames.len() < 2 {
        return Err(invalid("Invalid animation movement count"));
    }
    if animation.flags & 4 != 0 || model.flags & vformats::mdl::FLAG_STATIC_PROP != 0 {
        return Err(invalid(
            "Movement on delta or static animations cannot be reconstructed",
        ));
    }
    let start = reader.relative(record, record + 24)?;
    reader.bytes(start, count * 44)?;
    let mut movements = Vec::with_capacity(count);
    let mut previous = Movement::default();
    let loop_mask = if animation.flags & 1 != 0 {
        endpoint_mask(model, animation)?
    } else {
        0
    };
    let mut options = String::new();
    for (bit, name) in [(64, "LX"), (128, "LY"), (256, "LZ"), (2048, "LZR")] {
        if loop_mask & bit != 0 {
            write!(options, " {name}").unwrap();
        }
    }
    for index in 0..count {
        let at = start + index * 44;
        let movement = Movement {
            endframe: reader.i32(at)?,
            motionflags: reader.i32(at + 4)?,
            v0: reader.f32(at + 8)?,
            v1: reader.f32(at + 12)?,
            angle: reader.f32(at + 16)?,
            vector: [
                reader.f32(at + 20)?,
                reader.f32(at + 24)?,
                reader.f32(at + 28)?,
            ],
            position: [
                reader.f32(at + 32)?,
                reader.f32(at + 36)?,
                reader.f32(at + 40)?,
            ],
        };
        if movement.endframe <= previous.endframe
            || movement.endframe as usize >= animation.frames.len()
            || movement.motionflags & !0x3fff != 0
            || movement.motionflags & 0x600 != 0
            || movement.v0 < 0.0
            || movement.v1 < 0.0
        {
            return Err(invalid("Invalid or unsupported animation movement key"));
        }
        let distance = (movement.v0 + movement.v1) * 0.5;
        let length = movement
            .vector
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        if !distance.is_finite()
            || !length.is_finite()
            || (distance > 0.001 && (length - 1.0).abs() > 0.001)
            || length > 1.001
            || (0..3).any(|axis| {
                (previous.position[axis] + movement.vector[axis] * distance
                    - movement.position[axis])
                    .abs()
                    > 0.01 + distance * 0.0001
            })
        {
            return Err(invalid("Animation movement path is inconsistent"));
        }
        write!(options, " walkframe {}", movement.endframe).unwrap();
        for (bit, name) in [
            (1, "X"),
            (2, "Y"),
            (4, "Z"),
            (8, "XR"),
            (16, "YR"),
            (32, "ZR"),
            (64, "LX"),
            (128, "LY"),
            (256, "LZ"),
            (2048, "LZR"),
            (4096, "LM"),
            (8192, "LQ"),
        ] {
            if movement.motionflags & bit != 0 {
                write!(options, " {name}").unwrap();
            }
        }
        previous = movement.clone();
        movements.push(movement);
    }
    if loop_mask != 0 && previous.endframe as usize + 1 < animation.frames.len() {
        if movements.len() == 64 {
            return Err(invalid(
                "Loop movement needs an additional endpoint key beyond the engine limit",
            ));
        }
        write!(options, " walkframe {}", animation.frames.len() - 1).unwrap();
    }
    let root_rotation = angle_matrix([0.0, 0.0, std::f32::consts::FRAC_PI_2]);
    let inverse_root = invert(&root_rotation);
    let mut segment = 0;
    let mut previous = Movement::default();
    for (frame, poses) in animation.frames.iter_mut().enumerate() {
        while segment + 1 < movements.len() && frame > movements[segment].endframe as usize {
            previous = movements[segment].clone();
            segment += 1;
        }
        let movement = &movements[segment];
        let time = ((frame as f32 - previous.endframe as f32)
            / (movement.endframe - previous.endframe) as f32)
            .clamp(0.0, 1.0);
        let distance = movement.v0 * time + 0.5 * (movement.v1 - movement.v0) * time * time;
        let position =
            std::array::from_fn(|axis| previous.position[axis] + movement.vector[axis] * distance);
        let yaw = (previous.angle + (movement.angle - previous.angle) * time).to_radians();
        let movement_transform = local_transform(position, [0.0, 0.0, yaw]);
        for (pose, bone) in poses.iter_mut().zip(&model.bones) {
            if bone.parent >= 0 {
                continue;
            }
            let source = local_transform(pose.position, pose.rotation);
            let restored = concat(
                &inverse_root,
                &concat(&movement_transform, &concat(&root_rotation, &source)),
            );
            pose.position = [restored[3], restored[7], restored[11]];
            pose.rotation = matrix_angles(&restored);
            if !pose
                .position
                .iter()
                .chain(&pose.rotation)
                .all(|value| value.is_finite())
            {
                return Err(invalid("Animation movement reconstruction overflowed"));
            }
        }
    }
    Ok(options)
}

fn endpoint_mask(model: &Mdl, animation: &Animation) -> Result<i32, DecompileError> {
    let first = &animation.frames[0];
    let last = animation.frames.last().unwrap();
    if first.len() != model.bones.len() || last.len() != first.len() {
        return Err(invalid("Loop animation bone counts do not match"));
    }
    let root = angle_matrix([0.0, 0.0, std::f32::consts::FRAC_PI_2]);
    let mut mask = 0;
    for ((first, last), bone) in first.iter().zip(last).zip(&model.bones) {
        let mut first = local_transform(first.position, first.rotation);
        let mut last = local_transform(last.position, last.rotation);
        if bone.parent < 0 {
            first = concat(&root, &first);
            last = concat(&root, &last);
        }
        for (offset, bit) in [(3, 64), (7, 128), (11, 256)] {
            if (first[offset] - last[offset]).abs() > 0.0002 {
                mask |= bit;
            }
        }
        let first = matrix_angles(&first);
        let last = matrix_angles(&last);
        for axis in 0..3 {
            let difference = (first[axis] - last[axis])
                .sin()
                .atan2((first[axis] - last[axis]).cos());
            if difference.abs() > 0.0002 {
                if axis != 2 {
                    return Err(invalid("Loop pitch or roll motion cannot be reconstructed"));
                }
                mask |= 2048;
            }
        }
    }
    Ok(mask)
}
