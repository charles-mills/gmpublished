use crate::{bone_math as math, model::CompiledModelDesc, smd::SmdPose};

pub(super) fn worlds(
    compiled: &CompiledModelDesc,
    parents: &[i32],
    sequence: usize,
    frame: i32,
) -> Result<Vec<math::Matrix3x4>, String> {
    if frame < 0 {
        return Err("usesequence requires a nonnegative frame".into());
    }
    let mut positions = Vec::with_capacity(parents.len());
    let mut rotations = Vec::with_capacity(parents.len());
    for bone in 0..parents.len() {
        let (position, rotation) =
            crate::compile::resolve_bone_pose(&compiled.desc, compiled, bone);
        positions.push(position);
        rotations.push(math::angle_quaternion(rotation));
    }
    accumulate(
        compiled,
        sequence,
        frame as f32,
        1.0,
        &mut positions,
        &mut rotations,
        &mut Vec::new(),
    )?;
    let frames: Vec<_> = positions
        .into_iter()
        .zip(rotations)
        .enumerate()
        .map(|(bone, (position, rotation))| SmdPose {
            bone: bone as i32,
            position,
            rotation: math::quaternion_angles(rotation),
        })
        .collect();
    Ok(crate::compile::frame_worlds(
        &compiled.desc,
        parents,
        &frames,
    ))
}

fn frames(compiled: &CompiledModelDesc, sequence: usize) -> Result<&[Vec<SmdPose>], String> {
    compiled
        .sequences
        .get(sequence)
        .and_then(|sequence| sequence.cells.first())
        .and_then(|index| compiled.animations.get(*index))
        .map(|animation| animation.frames.as_slice())
        .filter(|frames| !frames.is_empty())
        .ok_or_else(|| format!("usesequence layer {sequence} has no local animation"))
}

#[allow(clippy::too_many_arguments)]
fn accumulate(
    compiled: &CompiledModelDesc,
    sequence: usize,
    frame: f32,
    amount: f32,
    positions: &mut [[f32; 3]],
    rotations: &mut [[f32; 4]],
    active: &mut Vec<usize>,
) -> Result<(), String> {
    if !frame.is_finite() || frame < 0.0 || !amount.is_finite() {
        return Err("usesequence layer produced an invalid frame or weight".into());
    }
    if active.contains(&sequence) {
        return Err("usesequence contains a cyclic sequence layer".into());
    }
    active.push(sequence);
    let source = frames(compiled, sequence)?;
    let row = &source[(frame as usize) % source.len()];
    if row.len() != positions.len() {
        return Err("usesequence frames do not match the model skeleton".into());
    }
    let description = &compiled.sequences[sequence];
    let amount = amount.clamp(0.0, 1.0);
    for (bone, pose) in row.iter().enumerate() {
        let weight = amount * description.weights.get(bone).copied().unwrap_or(1.0);
        if weight <= 0.0 {
            continue;
        }
        let rotation = math::angle_quaternion(pose.rotation);
        if description.delta || description.extra_flags.unwrap_or(0) & 0x4 != 0 {
            rotations[bone] =
                if description.delta || description.extra_flags.unwrap_or(0) & 0x10 != 0 {
                    math::quaternion_ma(rotations[bone], weight, rotation)
                } else {
                    math::quaternion_normalize(math::quaternion_mult(
                        math::quaternion_scale(rotation, weight),
                        rotations[bone],
                    ))
                };
            for (position, delta) in positions[bone].iter_mut().zip(pose.position) {
                *position += weight * delta;
            }
        } else {
            let align = compiled.desc.bones[bone].flags.unwrap_or(0) & 0x100000 == 0;
            rotations[bone] = interpolate(rotation, rotations[bone], 1.0 - weight, align);
            for (position, target) in positions[bone].iter_mut().zip(pose.position) {
                *position = *position * (1.0 - weight) + target * weight;
            }
        }
    }
    for layer in &description.auto_layers {
        let target = usize::try_from(layer.sequence)
            .map_err(|_| "usesequence has a negative layer index")?;
        let target_frames = frames(compiled, target)?.len();
        let mut layer_frame = frame;
        let mut layer_weight = amount;
        if layer.start != layer.end {
            let pose_layer = layer.flags & 0x4000 != 0;
            // Compiled layers store cycle values; studiomdl evaluates frame values.
            let scale = if pose_layer {
                1.0
            } else {
                source.len().saturating_sub(1) as f32
            };
            let [start, peak, tail, end] =
                [layer.start, layer.peak, layer.tail, layer.end].map(|value| value * scale);
            let index = if pose_layer { 0.0 } else { frame };
            if index < start || index >= end {
                continue;
            }
            let mut envelope = if index < peak && start != peak {
                (index - start) / (peak - start)
            } else if index > tail && end != tail {
                (end - index) / (end - tail)
            } else {
                1.0
            };
            if layer.flags & 0x40 != 0 {
                envelope = envelope * envelope * (3.0 - 2.0 * envelope);
            }
            layer_weight = if layer.flags & 0x80 != 0 && frame > tail {
                envelope * amount / (1.0 - amount + envelope * amount)
            } else if layer.flags & 0x200 != 0 {
                envelope
            } else {
                amount * envelope
            };
            layer_frame = (index - start) / (end - start) * target_frames.saturating_sub(1) as f32;
        }
        accumulate(
            compiled,
            target,
            layer_frame,
            layer_weight,
            positions,
            rotations,
            active,
        )?;
    }
    active.pop();
    Ok(())
}

fn interpolate(a: [f32; 4], mut b: [f32; 4], amount: f32, align: bool) -> [f32; 4] {
    let mut dot: f32 = a.iter().zip(b).map(|(a, b)| a * b).sum();
    if align && dot < 0.0 {
        b = b.map(|value| -value);
        dot = -dot;
    }
    if dot < -0.999999 {
        let perpendicular = [-a[1], a[0], -a[3], a[2]];
        return math::quaternion_normalize(std::array::from_fn(|axis| {
            a[axis] * (std::f32::consts::PI * amount).cos()
                + perpendicular[axis] * (std::f32::consts::PI * amount).sin()
        }));
    }
    let (left, right) = if dot > 0.999999 {
        (1.0 - amount, amount)
    } else {
        let angle = dot.clamp(-1.0, 1.0).acos();
        (
            ((1.0 - amount) * angle).sin() / angle.sin(),
            (amount * angle).sin() / angle.sin(),
        )
    };
    math::quaternion_normalize(std::array::from_fn(|axis| a[axis] * left + b[axis] * right))
}
