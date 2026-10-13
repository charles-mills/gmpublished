use crate::{
    bone_math as math,
    model::{Bone, CompiledLocalHierarchy},
    smd::SmdPose,
};

pub(crate) struct Source<'a> {
    pub bones: &'a [Bone],
    pub parents: &'a [i32],
    pub frames: &'a [Vec<SmdPose>],
    pub start: usize,
    pub count: usize,
    pub looping: bool,
}

pub(crate) fn create(
    source: Source<'_>,
    bone: &str,
    parent: &str,
    mut range: [i32; 4],
) -> Result<CompiledLocalHierarchy, String> {
    let Source {
        bones,
        parents,
        frames: source,
        start: source_start,
        count,
        looping,
    } = source;
    if count < 2 || source.is_empty() || source.iter().any(|frame| frame.len() != parents.len()) {
        return Err(
            "localhierarchy requires at least two animation frames and a valid source skeleton"
                .into(),
        );
    }
    let last = i32::try_from(count - 1).map_err(|_| "localhierarchy frame count overflows")?;
    if range.iter().any(|frame| *frame < -1 || *frame > last) {
        return Err("localhierarchy range must use source frame indices or -1".into());
    }
    if range == [0; 4] {
        range[2] = last;
        range[3] = last;
    }
    if range[0] != -1 && range[1] == -1 && range[2] == -1 && range[3] != -1 {
        range[1] = (range[0] + range[3]) / 2;
        range[2] = range[1];
    }
    if range[0] != -1 && range[1] == -1 && range[2] != -1 {
        range[1] = (range[0] + range[2]) / 2;
    }
    if range[1] != -1 && range[2] == -1 && range[3] != -1 {
        range[2] = (range[1] + range[3]) / 2;
    }
    if range[1] == -1 {
        range[0] = 0;
        range[1] = 0;
    }
    if range[2] == -1 {
        range[2] = last;
        range[3] = last;
    }
    for phase in 1..4 {
        if range[phase] < range[phase - 1] {
            range[phase] += last;
        }
    }
    if range[0] < 0 || range.windows(2).any(|window| window[0] > window[1]) {
        return Err("Invalid localhierarchy time range".into());
    }
    let sample_count =
        usize::try_from(range[3] - range[0] + 1 + if range[3] >= count as i32 { 2 } else { 0 })
            .map_err(|_| "Invalid localhierarchy sample count")?;
    if sample_count > 2_000_000 {
        return Err("localhierarchy exceeds animation sample limit".into());
    }
    let target = bones
        .iter()
        .position(|item| item.name.eq_ignore_ascii_case(bone))
        .ok_or_else(|| format!("Unknown localhierarchy bone {bone:?}"))?;
    let new_parent = if parent.is_empty() {
        -1
    } else {
        bones
            .iter()
            .position(|item| item.name.eq_ignore_ascii_case(parent))
            .ok_or_else(|| format!("Unknown localhierarchy parent {parent:?}"))? as i32
    };
    let mut poses = Vec::with_capacity(sample_count);
    for index in 0..sample_count {
        let index = source_start + range[0] as usize + index;
        let index = if looping {
            index % source.len()
        } else {
            index.min(source.len() - 1)
        };
        let frame = &source[index];
        let world = math::compute_world(
            &frame.iter().map(|pose| pose.position).collect::<Vec<_>>(),
            &frame.iter().map(|pose| pose.rotation).collect::<Vec<_>>(),
            parents,
        );
        let local = if new_parent < 0 {
            world[target]
        } else {
            math::concat(&math::invert(&world[new_parent as usize]), &world[target])
        };
        let position = [local[3], local[7], local[11]];
        let rotation = math::matrix_quaternion(&local);
        if position
            .iter()
            .chain(&rotation)
            .any(|value| !value.is_finite())
        {
            return Err("localhierarchy produced a nonfinite pose".into());
        }
        poses.push((position, rotation));
    }
    Ok(CompiledLocalHierarchy {
        bone: target as i32,
        parent: new_parent,
        range: range.map(|frame| frame as f32 / last as f32),
        start_frame: range[0],
        poses,
    })
}

pub(crate) fn write(
    data: &mut Vec<u8>,
    rules: &[CompiledLocalHierarchy],
) -> Result<Option<usize>, String> {
    if rules.is_empty() {
        return Ok(None);
    }
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let start = data.len();
    data.resize(start + rules.len() * 48, 0);
    for (index, rule) in rules.iter().enumerate() {
        let at = start + index * 48;
        data[at..at + 4].copy_from_slice(&rule.bone.to_le_bytes());
        data[at + 4..at + 8].copy_from_slice(&rule.parent.to_le_bytes());
        for (phase, value) in rule.range.iter().enumerate() {
            data[at + 8 + phase * 4..at + 12 + phase * 4].copy_from_slice(&value.to_le_bytes());
        }
        data[at + 24..at + 28].copy_from_slice(&rule.start_frame.to_le_bytes());
        let compressed = crate::anim_writer::compress_ik_errors(&rule.poses)
            .ok_or("localhierarchy contains no samples")?;
        let base = data.len();
        let relative = i32::try_from(base - at).map_err(|_| "localhierarchy offset overflows")?;
        data[at + 28..at + 32].copy_from_slice(&relative.to_le_bytes());
        data.resize(base + 36, 0);
        for (channel, scale) in compressed.scale.iter().enumerate() {
            data[base + channel * 4..base + channel * 4 + 4].copy_from_slice(&scale.to_le_bytes());
        }
        for (channel, values) in compressed.channels.iter().enumerate() {
            let offset = i16::try_from(data.len() - base)
                .map_err(|_| "localhierarchy compressed channel offset exceeds 16 bits")?;
            data[base + 24 + channel * 2..base + 26 + channel * 2]
                .copy_from_slice(&offset.to_le_bytes());
            data.extend_from_slice(values);
        }
        while !data.len().is_multiple_of(4) {
            data.push(0);
        }
    }
    Ok(Some(start))
}

pub(crate) fn bake(
    animation: &mut crate::model::CompiledAnimation,
    parents: &[i32],
) -> Result<(), String> {
    if animation.local_hierarchy.is_empty() {
        return Ok(());
    }
    if animation.delta || !animation.ik_rules.is_empty() || animation.loop_restart != 0 {
        return Err("GMod localhierarchy baking does not support delta, authored IK rules, or startloop combinations".into());
    }
    if animation
        .local_hierarchy
        .iter()
        .any(|rule| rule.range[3] > 1.0)
    {
        return Err("GMod localhierarchy baking cannot preserve wrapped hierarchy windows".into());
    }
    let source = animation.frames.clone();
    let count = source.len();
    if count < 2 {
        return Err("localhierarchy baking requires at least two final animation frames".into());
    }
    let evaluate = |frame: f32| -> Result<Vec<SmdPose>, String> {
        let mut poses = sample(&source, frame);
        crate::anim_commands::convert(std::slice::from_mut(&mut poses), parents, true);
        let cycle = frame / (count - 1) as f32;
        for rule in &animation.local_hierarchy {
            let [start, peak, tail, end] = rule.range;
            let mut weight = 1.0;
            if tail - peak < 1.0 {
                if cycle < start || cycle >= end {
                    continue;
                }
                if cycle < peak && start != peak {
                    weight = (cycle - start) / (peak - start);
                } else if cycle > tail && end != tail {
                    weight = (end - cycle) / (end - tail);
                }
                weight = weight * weight * (3.0 - 2.0 * weight);
            }
            let offset = frame - rule.start_frame as f32;
            if offset < 0.0 || offset > (rule.poses.len() - 1) as f32 + 0.0001 {
                return Err("Local hierarchy sample lies outside its source payload".into());
            }
            let lower = (offset.floor() as usize).min(rule.poses.len() - 1);
            let upper = (lower + 1).min(rule.poses.len() - 1);
            let amount = offset - lower as f32;
            let position = std::array::from_fn(|axis| {
                rule.poses[lower].0[axis] * (1.0 - amount) + rule.poses[upper].0[axis] * amount
            });
            let rotation = blend(rule.poses[lower].1, rule.poses[upper].1, amount, false);
            let world = pose_world(&poses, parents);
            let target = rule.bone as usize;
            let transform = math::quaternion_local_transform(rotation, position);
            let transform = if rule.parent >= 0 {
                math::concat(&world[rule.parent as usize], &transform)
            } else {
                transform
            };
            let transform = if parents[target] >= 0 {
                math::concat(&math::invert(&world[parents[target] as usize]), &transform)
            } else {
                transform
            };
            let rotation = blend(
                math::angle_quaternion(poses[target].rotation),
                math::matrix_quaternion(&transform),
                weight,
                true,
            );
            let position = [transform[3], transform[7], transform[11]];
            poses[target].rotation = math::quaternion_angles(rotation);
            poses[target].position = if weight == 1.0 {
                position
            } else {
                std::array::from_fn(|axis| {
                    position[axis] * (1.0 - weight) + poses[target].position[axis] * weight
                })
            };
        }
        crate::anim_commands::convert(std::slice::from_mut(&mut poses), parents, false);
        Ok(poses)
    };
    let mut subdivision = 1;
    let mut frames = (0..count)
        .map(|frame| evaluate(frame as f32))
        .collect::<Result<Vec<_>, _>>()?;
    loop {
        let next_count = (count - 1)
            .checked_mul(subdivision * 2)
            .and_then(|value| value.checked_add(1))
            .ok_or("Local hierarchy baking frame count overflows")?;
        if next_count
            .checked_mul(parents.len())
            .is_none_or(|poses| poses > 2_000_000)
        {
            return Err("Local hierarchy baking exceeds animation pose limit".into());
        }
        let dense = (0..next_count)
            .map(|frame| evaluate(frame as f32 / (subdivision * 2) as f32))
            .collect::<Result<Vec<_>, _>>()?;
        let mut accurate = true;
        for index in (1..next_count).step_by(2) {
            let interpolated = sample(&frames, index as f32 * 0.5);
            let actual = pose_world(&dense[index], parents);
            let interpolated = pose_world(&interpolated, parents);
            for (actual, interpolated) in actual.iter().zip(&interpolated) {
                if [3, 7, 11]
                    .iter()
                    .any(|axis| (actual[*axis] - interpolated[*axis]).abs() > 0.001)
                    || math::matrix_quaternion(actual)
                        .iter()
                        .zip(math::matrix_quaternion(interpolated))
                        .map(|(a, b)| a * b)
                        .sum::<f32>()
                        .abs()
                        < 0.9999999
                {
                    accurate = false;
                    break;
                }
            }
            if !accurate {
                break;
            }
        }
        if accurate {
            break;
        }
        if subdivision >= 64 {
            return Err(
                "Local hierarchy changes cannot be baked within GMod pose precision".into(),
            );
        }
        frames = dense;
        subdivision *= 2;
    }
    if animation.looping {
        let first = pose_world(&frames[0], parents);
        let last = pose_world(frames.last().unwrap(), parents);
        if first.iter().zip(last).any(|(first, last)| {
            [3, 7, 11]
                .iter()
                .any(|axis| (first[*axis] - last[*axis]).abs() > 0.001)
                || math::matrix_quaternion(first)
                    .iter()
                    .zip(math::matrix_quaternion(&last))
                    .map(|(a, b)| a * b)
                    .sum::<f32>()
                    .abs()
                    < 0.9999999
        }) {
            return Err("GMod localhierarchy baking cannot preserve an open loop endpoint".into());
        }
    }
    animation.frames = frames;
    animation.fps *= subdivision as f32;
    for movement in &mut animation.movements {
        movement.endframe = movement
            .endframe
            .checked_mul(subdivision as i32)
            .ok_or("Local hierarchy movement frame overflows")?;
    }
    if let Some(frames) = &mut animation.pre_subtract_frames {
        let source = frames.clone();
        *frames = (0..animation.frames.len())
            .map(|frame| sample(&source, frame as f32 / subdivision as f32))
            .collect();
    }
    animation.local_hierarchy.clear();
    Ok(())
}

fn sample(frames: &[Vec<SmdPose>], index: f32) -> Vec<SmdPose> {
    let lower = (index.floor() as usize).min(frames.len() - 1);
    let upper = (lower + 1).min(frames.len() - 1);
    let amount = index - lower as f32;
    frames[lower]
        .iter()
        .zip(&frames[upper])
        .map(|(lower, upper)| SmdPose {
            bone: lower.bone,
            position: std::array::from_fn(|axis| {
                lower.position[axis] * (1.0 - amount) + upper.position[axis] * amount
            }),
            rotation: math::quaternion_angles(blend(
                math::angle_quaternion(lower.rotation),
                math::angle_quaternion(upper.rotation),
                amount,
                false,
            )),
        })
        .collect()
}
fn pose_world(poses: &[SmdPose], parents: &[i32]) -> Vec<math::Matrix3x4> {
    math::compute_world(
        &poses.iter().map(|pose| pose.position).collect::<Vec<_>>(),
        &poses.iter().map(|pose| pose.rotation).collect::<Vec<_>>(),
        parents,
    )
}
fn blend(mut a: [f32; 4], mut b: [f32; 4], amount: f32, spherical: bool) -> [f32; 4] {
    let mut dot = a.iter().zip(b).map(|(a, b)| a * b).sum::<f32>();
    if dot < 0.0 {
        b = b.map(|value| -value);
        dot = -dot;
    }
    let (left, right) = if spherical && dot < 0.9995 {
        let angle = dot.clamp(-1.0, 1.0).acos();
        (
            ((1.0 - amount) * angle).sin() / angle.sin(),
            (amount * angle).sin() / angle.sin(),
        )
    } else {
        (1.0 - amount, amount)
    };
    for axis in 0..4 {
        a[axis] = a[axis] * left + b[axis] * right;
    }
    math::quaternion_normalize(a)
}
