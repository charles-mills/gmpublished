use crate::{
    bone_math as math,
    model::{AnimCmd, Bone, CompiledAnimation},
    smd::SmdPose,
};

fn apply_basic(
    animation: &mut CompiledAnimation,
    weights: &mut [f32],
    command: &AnimCmd,
) -> Result<bool, String> {
    match command {
        AnimCmd::NoAnimation => {
            let frame: Vec<_> = weights
                .iter_mut()
                .enumerate()
                .map(|(bone, weight)| {
                    *weight = 0.0;
                    SmdPose {
                        bone: bone as i32,
                        position: [0.0; 3],
                        rotation: [0.0; 3],
                    }
                })
                .collect();
            animation.frames = vec![frame];
            animation.delta = true;
            animation.pre_subtract_frames = None;
        }
        AnimCmd::NumFrames { frames } => {
            let count = usize::try_from(*frames)
                .ok()
                .filter(|count| *count > 0)
                .ok_or("numframes requires a positive frame count")?;
            if count
                .checked_mul(weights.len().max(1))
                .is_none_or(|poses| poses > 2_000_000)
            {
                return Err("numframes exceeds the animation pose limit".into());
            }
            let last = animation
                .frames
                .last()
                .ok_or("Animation contains no frames")?
                .clone();
            animation.frames.resize(count, last);
        }
        AnimCmd::Compress { frames } => {
            let step = usize::try_from(*frames)
                .ok()
                .filter(|step| *step > 0)
                .ok_or("compress requires a positive frame step")?;
            animation.frames = animation.frames.iter().step_by(step).cloned().collect();
            animation.fps /= step as f32;
            if !animation.fps.is_finite() || animation.fps <= 0.0 {
                return Err("compress produced an invalid frame rate".into());
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

pub(crate) struct Context<'a> {
    pub base_frame0: Option<&'a [SmdPose]>,
    pub reference_weights: &'a [f32],
    pub reference_engine_space: bool,
    pub bones: &'a [Bone],
    pub parents: &'a [i32],
    pub motion_type: i32,
    pub motion_root: usize,
    pub reference: Option<&'a [Vec<SmdPose>]>,
    pub engine_space: bool,
    pub reference_is_delta: bool,
    pub root_rotation: [f32; 3],
    pub bind: &'a [SmdPose],
    pub pos_weights: &'a [f32],
}

pub(crate) fn apply(
    animation: &mut CompiledAnimation,
    weights: &mut [f32],
    command: &AnimCmd,
    context: &Context<'_>,
) -> Result<bool, String> {
    if apply_basic(animation, weights, command)? {
        return Ok(true);
    }
    if animation.frames.is_empty()
        || animation
            .frames
            .iter()
            .any(|frame| frame.len() != context.parents.len())
    {
        return Err("Animation commands require frames matching the model skeleton".into());
    }
    if context
        .base_frame0
        .is_some_and(|base| base.len() != context.parents.len())
        || (!context.reference_weights.is_empty()
            && context.reference_weights.len() != context.parents.len())
        || weights.len() != context.parents.len()
        || context.motion_root >= context.parents.len()
        || context.pos_weights.len() != context.parents.len()
        || context.reference.is_some_and(|frames| {
            frames.is_empty()
                || frames
                    .iter()
                    .any(|frame| frame.len() != context.parents.len())
        })
    {
        return Err(
            "Animation command weights or reference do not match the model skeleton".into(),
        );
    }
    let world_command = matches!(
        command,
        AnimCmd::Align { .. } | AnimCmd::CounterRotate { .. } | AnimCmd::WorldSpaceBlend { .. }
    );
    let reconstruct_delta = animation.delta && world_command;
    let mut frames = if reconstruct_delta {
        absolute_delta_frames(&animation.frames, weights, context, context.engine_space)
    } else {
        animation.frames.clone()
    };
    let absolute = !animation.delta || reconstruct_delta;
    if absolute && !context.engine_space && !reconstruct_delta {
        convert(&mut frames, context.parents, true);
    }
    let reference = context.reference.map(|source| {
        if context.reference_is_delta && world_command {
            absolute_delta_frames(
                source,
                context.reference_weights,
                context,
                context.reference_engine_space,
            )
        } else {
            let mut frames = source.to_vec();
            if !context.reference_is_delta {
                convert(&mut frames, context.parents, true);
            } else if !context.reference_engine_space {
                crate::motion::convert_delta_positions(&mut frames, context.parents, true);
            }
            frames
        }
    });
    match command {
        AnimCmd::Match { .. } => {
            let source = reference
                .as_ref()
                .ok_or("Unknown match reference animation")?;
            let source = source.first().ok_or("Match reference contains no frames")?;
            let first = frames[0].clone();
            for frame in &mut frames {
                for (bone, pose) in frame.iter_mut().enumerate() {
                    if weights[bone] > 0.0 {
                        let delta = sm(
                            -1.0,
                            quat(first[bone].rotation),
                            quat(source[bone].rotation),
                        );
                        pose.rotation = math::quaternion_angles(math::quaternion_ma(
                            quat(pose.rotation),
                            1.0,
                            delta,
                        ));
                    }
                }
            }
        }
        AnimCmd::FixupLoop { start, end } => {
            if *start > 0 || *end < 0 || start >= end {
                return Err(
                    "fixuploop requires a negative start, nonnegative end, and a nonempty interval"
                        .into(),
                );
            }
            let last = frames.len() - 1;
            if last > 0 {
                let (delta_pos, delta_q) = deltas(&frames[last], &frames[0], context);
                let mut start = i64::from(*start);
                let mut end = i64::from(*end);
                if end - start > frames.len() as i64 {
                    end = frames.len() as i64 + start;
                    if end < 0 {
                        end = 0;
                        start = -(last as i64);
                    }
                }
                let span = (end - start) as f32;
                for offset in start + 1..=0 {
                    let amount = spline((offset - start) as f32 / span);
                    let index = last as i64 + offset;
                    if index < 0 {
                        return Err("fixuploop interval exceeds the animation".into());
                    }
                    add_deltas(
                        &mut frames[index as usize],
                        -amount,
                        &delta_pos,
                        &delta_q,
                        weights,
                    );
                }
                for index in 0..end {
                    let amount = spline((end - index) as f32 / span);
                    add_deltas(
                        &mut frames[index as usize],
                        amount,
                        &delta_pos,
                        &delta_q,
                        weights,
                    );
                }
            }
        }
        AnimCmd::Angle { angle } => {
            if !angle.is_finite() {
                return Err("rotateto requires a finite angle".into());
            }
            let root = context.motion_root;
            let movement = animation
                .movements
                .last()
                .map(|key| key.position)
                .unwrap_or_else(|| {
                    std::array::from_fn(|axis| {
                        frames.last().unwrap()[root].position[axis] - frames[0][root].position[axis]
                    })
                });
            let change = if movement[0] != 0.0 || movement[1] != 0.0 {
                angle.to_radians() - movement[1].atan2(movement[0])
            } else {
                0.0
            };
            let rotation = math::angle_matrix([0.0, 0.0, change]);
            transform_roots(&mut frames, context.parents, &rotation);
            for key in &mut animation.movements {
                key.position = rotate(key.position, &rotation);
                key.vector = rotate(key.vector, &rotation);
            }
        }
        AnimCmd::Derivative { scale } => {
            if !scale.is_finite() {
                return Err("derivative requires a finite scale".into());
            }
            let original = frames.clone();
            let wrap = original.len() - 1 - usize::from(animation.looping && original.len() > 1);
            for (index, frame) in frames.iter_mut().enumerate() {
                let previous = &original[if index == 0 { wrap } else { index - 1 }];
                for (bone, pose) in frame.iter_mut().enumerate() {
                    if weights[bone] > 0.0 {
                        let delta = sm(-1.0, quat(previous[bone].rotation), quat(pose.rotation));
                        pose.rotation =
                            math::quaternion_angles(math::quaternion_scale(delta, *scale));
                        pose.position = std::array::from_fn(|axis| {
                            (pose.position[axis] - previous[bone].position[axis]) * scale
                        });
                    }
                }
            }
        }
        AnimCmd::LinearDelta { flags } => {
            if flags & !(0x10 | 0x40) != 0 {
                return Err("lineardelta contains unsupported flags".into());
            }
            animation.pre_subtract_frames = Some(original_frames(animation, context));
            let first = frames[0].clone();
            let last = frames.last().unwrap().clone();
            let denominator = frames.len().saturating_sub(1);
            for (index, frame) in frames.iter_mut().enumerate() {
                let amount = if denominator == 0 {
                    1.0
                } else {
                    index as f32 / denominator as f32
                };
                let amount = if flags & 0x40 != 0 {
                    spline(amount)
                } else {
                    amount
                };
                for (bone, pose) in frame.iter_mut().enumerate() {
                    if weights[bone] > 0.0 {
                        let base = slerp(
                            quat(first[bone].rotation),
                            quat(last[bone].rotation),
                            amount,
                        );
                        pose.rotation = math::quaternion_angles(if flags & 0x10 != 0 {
                            sm(-1.0, base, quat(pose.rotation))
                        } else {
                            math::quaternion_ma(quat(pose.rotation), -1.0, base)
                        });
                        pose.position = std::array::from_fn(|axis| {
                            let base = first[bone].position[axis] * (1.0 - amount)
                                + last[bone].position[axis] * amount;
                            if flags & 0x10 != 0 {
                                pose.position[axis] - base
                            } else {
                                base - pose.position[axis]
                            }
                        });
                    }
                }
            }
            animation.delta = true;
        }
        AnimCmd::Align {
            motion_type,
            src_frame,
            dest_frame,
            bone,
            ..
        } => {
            let source = reference
                .as_ref()
                .ok_or("Unknown alignment reference animation")?;
            let src = frame(source, *src_frame, "align source")?;
            let dest = frame(&frames, *dest_frame, "align destination")?;
            let bone = match bone {
                Some(name) => context
                    .bones
                    .iter()
                    .position(|bone| bone.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| format!("Unknown alignment bone {name:?}"))?,
                None => context.motion_root,
            };
            let sw = world(src, context.parents);
            let dw = world(dest, context.parents);
            let delta = math::quaternion_ma(
                math::matrix_quaternion(&dw[bone]),
                -1.0,
                math::matrix_quaternion(&sw[bone]),
            );
            let mut angles = [0.0; 3];
            for axis in 0..3 {
                if motion_type & ((8 << axis) | (0x200 << axis)) != 0 {
                    let mut projection = [0.0, 0.0, 0.0, delta[3]];
                    projection[axis] = delta[axis];
                    angles[axis] =
                        math::quaternion_angles(math::quaternion_normalize(projection))[axis];
                }
            }
            if motion_type & 0x38 == 0x38 {
                angles = math::quaternion_angles(delta);
            }
            let mut adjustment = math::invert(&math::angle_matrix(angles));
            let rotated = rotate(position(&dw[bone]), &adjustment);
            for axis in 0..3 {
                adjustment[axis * 4 + 3] = if motion_type & ((1 << axis) | (0x40 << axis)) != 0 {
                    sw[bone][axis * 4 + 3] - rotated[axis]
                } else {
                    0.0
                };
            }
            transform_roots(&mut frames, context.parents, &adjustment);
        }
        AnimCmd::CounterRotate { bone, target_angle } => {
            let bone = context
                .bones
                .iter()
                .position(|candidate| candidate.name.eq_ignore_ascii_case(bone))
                .ok_or_else(|| format!("Unknown counterrotate bone {bone:?}"))?;
            let target = if let Some(angle) = target_angle {
                if !angle.iter().all(|value| value.is_finite()) {
                    return Err("counterrotateto requires finite angles".into());
                }
                math::angle_matrix([
                    angle[2].to_radians(),
                    angle[0].to_radians(),
                    angle[1].to_radians(),
                ])
            } else {
                let mut bind = vec![context.bind.to_vec()];
                transform_roots(
                    &mut bind,
                    context.parents,
                    &math::angle_matrix(context.root_rotation),
                );
                world(&bind[0], context.parents)[bone]
            };
            for poses in &mut frames {
                let matrices = world(poses, context.parents);
                let mut target = target;
                for axis in 0..3 {
                    target[axis * 4 + 3] = matrices[bone][axis * 4 + 3];
                }
                let local = if context.parents[bone] < 0 {
                    target
                } else {
                    math::concat(
                        &math::invert(&matrices[context.parents[bone] as usize]),
                        &target,
                    )
                };
                poses[bone].rotation = math::matrix_angles(&local);
                poses[bone].position = position(&local);
            }
        }
        AnimCmd::MatchBlend {
            src_frame,
            dest_frame,
            dest_pre,
            dest_post,
            ..
        } => {
            let source = reference
                .as_ref()
                .ok_or("Unknown matchblend reference animation")?;
            let src = frame(source, *src_frame, "matchblend source")?;
            let dest = frame(&frames, *dest_frame, "matchblend destination")?;
            let last = frames.len() - 1;
            if last == 0 {
                return Err("matchblend requires at least two frames".into());
            }
            if *dest_pre > 0 || *dest_post < 0 {
                return Err(
                    "matchblend requires nonpositive pre and nonnegative post intervals".into(),
                );
            }
            let (delta_pos, delta_q) = deltas(src, dest, context);
            let pre = (*dest_pre).max(if animation.looping {
                -(frames.len() as i32)
            } else {
                -*dest_frame
            });
            let post = (*dest_post).min(if animation.looping {
                frames.len() as i32
            } else {
                frames.len() as i32 - *dest_frame
            });
            for offset in pre..=post {
                let amount = 1.0
                    - if offset < 0 {
                        offset as f32 / (pre as f32 - 1.0)
                    } else {
                        offset as f32 / (post as f32 + 1.0)
                    };
                let index = (*dest_frame + offset).rem_euclid(last as i32) as usize;
                add_deltas(
                    &mut frames[index],
                    spline(amount),
                    &delta_pos,
                    &delta_q,
                    weights,
                );
                if animation.looping && index == 0 {
                    add_deltas(
                        &mut frames[last],
                        spline(amount),
                        &delta_pos,
                        &delta_q,
                        weights,
                    );
                }
            }
        }
        AnimCmd::WorldSpaceBlend {
            start_frame, loops, ..
        } => {
            let source = reference
                .as_ref()
                .ok_or("Unknown worldspaceblend reference animation")?;
            frame(source, *start_frame, "worldspaceblend source")?;
            let last = frames.len() - 1;
            for (index, poses) in frames.iter_mut().enumerate() {
                let src = if *loops {
                    let cycle = if last == 0 {
                        0.0
                    } else {
                        index as f32 / last as f32
                    } + if source.len() < 2 {
                        0.0
                    } else {
                        *start_frame as f32 / (source.len() - 1) as f32
                    };
                    sample_cycle(source, cycle)
                } else {
                    source[*start_frame as usize].clone()
                };
                let sw = world(&src, context.parents);
                let mut dw = world(poses, context.parents);
                for bone in 0..poses.len() {
                    if weights[bone] > 0.0 {
                        let rotation = slerp(
                            math::matrix_quaternion(&dw[bone]),
                            math::matrix_quaternion(&sw[bone]),
                            weights[bone],
                        );
                        let position = position(&dw[bone]);
                        dw[bone] = math::quaternion_local_transform(rotation, position);
                    }
                    let local = if context.parents[bone] < 0 {
                        dw[bone]
                    } else {
                        math::concat(
                            &math::invert(&dw[context.parents[bone] as usize]),
                            &dw[bone],
                        )
                    };
                    poses[bone].rotation = math::matrix_angles(&local);
                    if context.parents[bone] >= 0 {
                        let amount = context.pos_weights[bone];
                        poses[bone].position = std::array::from_fn(|axis| {
                            poses[bone].position[axis] * (1.0 - amount)
                                + src[bone].position[axis] * amount
                        });
                    }
                }
            }
        }
        AnimCmd::Subtract {
            reference: _,
            frame: base_frame,
            post,
        } => {
            let source = reference
                .as_ref()
                .ok_or("Unknown subtract reference animation")?;
            let src = frame(source, *base_frame, "subtract source")?;
            animation.pre_subtract_frames = Some(original_frames(animation, context));
            for poses in &mut frames {
                for (bone, pose) in poses.iter_mut().enumerate() {
                    if weights[bone] > 0.0 {
                        pose.rotation = math::quaternion_angles(if *post {
                            sm(-1.0, quat(src[bone].rotation), quat(pose.rotation))
                        } else {
                            math::quaternion_ma(quat(pose.rotation), -1.0, quat(src[bone].rotation))
                        });
                        pose.position = std::array::from_fn(|axis| {
                            if *post {
                                pose.position[axis] - src[bone].position[axis]
                            } else {
                                src[bone].position[axis] - pose.position[axis]
                            }
                        });
                    }
                }
            }
            animation.delta = true;
        }
        _ => return Ok(false),
    }
    if reconstruct_delta {
        let mut absolute_frames = frames.clone();
        convert(&mut absolute_frames, context.parents, false);
        animation.pre_subtract_frames = Some(absolute_frames);
        restore_delta_frames(&mut frames, &animation.frames, weights, context)?;
    }
    if absolute && animation.delta && !context.engine_space && !reconstruct_delta {
        crate::motion::convert_delta_positions(&mut frames, context.parents, false);
    }
    if absolute && !animation.delta && !context.engine_space {
        convert(&mut frames, context.parents, false);
    }
    if frames.iter().flatten().any(|pose| {
        !pose
            .position
            .iter()
            .chain(&pose.rotation)
            .all(|value| value.is_finite())
    }) {
        return Err("Animation command produced nonfinite poses".into());
    }
    animation.frames = frames;
    Ok(true)
}

fn absolute_delta_frames(
    source: &[Vec<SmdPose>],
    weights: &[f32],
    context: &Context<'_>,
    engine_positions: bool,
) -> Vec<Vec<SmdPose>> {
    let mut canonical = source.to_vec();
    if engine_positions {
        crate::motion::convert_delta_positions(&mut canonical, context.parents, false);
    }
    let source = canonical.as_slice();
    let base = context.base_frame0.unwrap_or(context.bind);
    let mut frames: Vec<_> = source
        .iter()
        .map(|frame| {
            crate::compile::delta_local_matrices_weighted(base, frame, weights)
                .into_iter()
                .enumerate()
                .map(|(bone, matrix)| SmdPose {
                    bone: bone as i32,
                    position: position(&matrix),
                    rotation: math::matrix_angles(&matrix),
                })
                .collect()
        })
        .collect();
    convert(&mut frames, context.parents, true);
    frames
}

fn restore_delta_frames(
    frames: &mut [Vec<SmdPose>],
    original: &[Vec<SmdPose>],
    weights: &[f32],
    context: &Context<'_>,
) -> Result<(), String> {
    convert(frames, context.parents, false);
    let base = context.base_frame0.unwrap_or(context.bind);
    if base.len() != context.parents.len() {
        return Err("Delta reference does not match the model skeleton".into());
    }
    for (frame, original) in frames.iter_mut().zip(original) {
        for (bone, pose) in frame.iter_mut().enumerate() {
            let weight = weights[bone];
            if weight <= 0.0 {
                *pose = original[bone];
                continue;
            }
            pose.position = std::array::from_fn(|axis| {
                (pose.position[axis] - base[bone].position[axis]) / weight
            });
            let inverse = math::quaternion_scale(quat(base[bone].rotation), -1.0);
            pose.rotation = math::quaternion_angles(math::quaternion_scale(
                math::quaternion_mult(inverse, quat(pose.rotation)),
                1.0 / weight,
            ));
        }
    }
    if context.engine_space {
        crate::motion::convert_delta_positions(frames, context.parents, true);
    }
    Ok(())
}

fn original_frames(animation: &CompiledAnimation, context: &Context<'_>) -> Vec<Vec<SmdPose>> {
    let mut frames = animation.frames.clone();
    if context.engine_space && !animation.delta {
        convert(&mut frames, context.parents, false);
    }
    frames
}

fn quat(angles: [f32; 3]) -> [f32; 4] {
    math::angle_quaternion(angles)
}
fn sm(amount: f32, source: [f32; 4], dest: [f32; 4]) -> [f32; 4] {
    math::quaternion_normalize(math::quaternion_mult(
        math::quaternion_scale(source, amount),
        dest,
    ))
}
fn spline(amount: f32) -> f32 {
    amount * amount * (3.0 - 2.0 * amount)
}
fn slerp(mut a: [f32; 4], mut b: [f32; 4], amount: f32) -> [f32; 4] {
    let mut dot = a.iter().zip(b).map(|(a, b)| a * b).sum::<f32>();
    if dot < 0.0 {
        b = b.map(|v| -v);
        dot = -dot;
    }
    let (left, right) = if dot > 0.9995 {
        (1.0 - amount, amount)
    } else {
        let angle = dot.clamp(-1.0, 1.0).acos();
        (
            ((1.0 - amount) * angle).sin() / angle.sin(),
            (amount * angle).sin() / angle.sin(),
        )
    };
    for i in 0..4 {
        a[i] = a[i] * left + b[i] * right;
    }
    math::quaternion_normalize(a)
}
fn frame<'a>(frames: &'a [Vec<SmdPose>], index: i32, label: &str) -> Result<&'a [SmdPose], String> {
    usize::try_from(index)
        .ok()
        .and_then(|index| frames.get(index))
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{label} frame {index} is out of range"))
}
fn world(poses: &[SmdPose], parents: &[i32]) -> Vec<math::Matrix3x4> {
    math::compute_world(
        &poses.iter().map(|pose| pose.position).collect::<Vec<_>>(),
        &poses.iter().map(|pose| pose.rotation).collect::<Vec<_>>(),
        parents,
    )
}
fn position(matrix: &math::Matrix3x4) -> [f32; 3] {
    [matrix[3], matrix[7], matrix[11]]
}
fn rotate(vector: [f32; 3], matrix: &math::Matrix3x4) -> [f32; 3] {
    std::array::from_fn(|axis| {
        (0..3)
            .map(|column| matrix[axis * 4 + column] * vector[column])
            .sum()
    })
}
fn transform_roots(frames: &mut [Vec<SmdPose>], parents: &[i32], transform: &math::Matrix3x4) {
    for frame in frames {
        for (pose, parent) in frame.iter_mut().zip(parents) {
            if *parent == -1 {
                let matrix = math::concat(
                    transform,
                    &math::local_transform(pose.position, pose.rotation),
                );
                pose.position = position(&matrix);
                pose.rotation = math::matrix_angles(&matrix);
            }
        }
    }
}
pub(crate) fn convert(frames: &mut [Vec<SmdPose>], parents: &[i32], engine: bool) {
    transform_roots(
        frames,
        parents,
        &math::angle_matrix([
            0.0,
            0.0,
            if engine {
                std::f32::consts::FRAC_PI_2
            } else {
                -std::f32::consts::FRAC_PI_2
            },
        ]),
    )
}
fn deltas(
    source: &[SmdPose],
    dest: &[SmdPose],
    context: &Context<'_>,
) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    let positions = source
        .iter()
        .zip(dest)
        .enumerate()
        .map(|(bone, (src, dest))| {
            std::array::from_fn(|axis| {
                if context.parents[bone] == -1 && context.motion_type & (0x40 << axis) != 0 {
                    0.0
                } else {
                    src.position[axis] - dest.position[axis]
                }
            })
        })
        .collect();
    let rotations = source
        .iter()
        .zip(dest)
        .map(|(src, dest)| math::quaternion_ma(quat(src.rotation), -1.0, quat(dest.rotation)))
        .collect();
    (positions, rotations)
}
fn add_deltas(
    poses: &mut [SmdPose],
    amount: f32,
    positions: &[[f32; 3]],
    rotations: &[[f32; 4]],
    weights: &[f32],
) {
    for (bone, pose) in poses.iter_mut().enumerate() {
        if weights[bone] > 0.0 {
            pose.position =
                std::array::from_fn(|axis| pose.position[axis] + amount * positions[bone][axis]);
            pose.rotation =
                math::quaternion_angles(sm(amount, rotations[bone], quat(pose.rotation)));
        }
    }
}
fn sample_cycle(source: &[Vec<SmdPose>], cycle: f32) -> Vec<SmdPose> {
    let coordinate = cycle.rem_euclid(1.0) * source.len().saturating_sub(1) as f32;
    let first = coordinate.floor() as usize;
    let second = if source.len() > 1 {
        (first + 1) % (source.len() - 1)
    } else {
        0
    };
    let amount = coordinate.fract();
    source[first]
        .iter()
        .zip(&source[second])
        .map(|(a, b)| SmdPose {
            bone: a.bone,
            position: std::array::from_fn(|axis| {
                a.position[axis] * (1.0 - amount) + b.position[axis] * amount
            }),
            rotation: math::quaternion_angles(slerp(quat(a.rotation), quat(b.rotation), amount)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn animation() -> CompiledAnimation {
        CompiledAnimation {
            no_anim_block: false,
            no_anim_block_stall: false,
            source_frames: None,
            local_hierarchy: Vec::new(),
            weights: Vec::new(),
            loop_restart: 0,
            fudge_loop: false,
            extra_flags: 0,
            movements: vec![],
            name: "test".into(),
            smd_path: "test.smd".into(),
            fps: 24.0,
            looping: false,
            delta: false,
            ik_rules: vec![],
            no_auto_ik: true,
            pre_subtract_frames: None,
            frames: (0..7)
                .map(|frame| {
                    vec![SmdPose {
                        bone: 0,
                        position: [frame as f32, 0.0, 0.0],
                        rotation: [0.0; 3],
                    }]
                })
                .collect(),
        }
    }

    #[test]
    fn frame_commands_preserve_order_duration_and_endpoint_padding() {
        let mut animation = animation();
        let mut weights = [1.0];
        apply_basic(
            &mut animation,
            &mut weights,
            &AnimCmd::Compress { frames: 2 },
        )
        .unwrap();
        assert_eq!(animation.fps, 12.0);
        assert_eq!(
            animation
                .frames
                .iter()
                .map(|frame| frame[0].position[0])
                .collect::<Vec<_>>(),
            [0.0, 2.0, 4.0, 6.0]
        );
        apply_basic(
            &mut animation,
            &mut weights,
            &AnimCmd::NumFrames { frames: 6 },
        )
        .unwrap();
        assert_eq!(
            animation
                .frames
                .iter()
                .map(|frame| frame[0].position[0])
                .collect::<Vec<_>>(),
            [0.0, 2.0, 4.0, 6.0, 6.0, 6.0]
        );
        apply_basic(
            &mut animation,
            &mut weights,
            &AnimCmd::NumFrames { frames: 2 },
        )
        .unwrap();
        assert_eq!(animation.frames.len(), 2);
        apply_basic(&mut animation, &mut weights, &AnimCmd::NoAnimation).unwrap();
        assert_eq!(animation.frames.len(), 1);
        assert_eq!(animation.frames[0][0].position, [0.0; 3]);
        assert!(animation.delta);
        assert_eq!(weights, [0.0]);
    }

    #[test]
    fn malformed_frame_commands_fail_before_allocating() {
        let mut animation = animation();
        for command in [
            AnimCmd::Compress { frames: 0 },
            AnimCmd::Compress { frames: -1 },
            AnimCmd::NumFrames { frames: 0 },
            AnimCmd::NumFrames { frames: i32::MAX },
        ] {
            assert!(apply_basic(&mut animation, &mut [1.0], &command).is_err());
        }
    }

    fn run(
        animation: &mut CompiledAnimation,
        weights: &mut [f32],
        command: AnimCmd,
        reference: Option<&[Vec<SmdPose>]>,
    ) {
        let bones: Vec<Bone> = (0..weights.len())
            .map(|index| toml::from_str(&format!("name = 'bone{index}'")).unwrap())
            .collect();
        let parents: Vec<_> = (0..weights.len())
            .map(|index| if index == 0 { -1 } else { 0 })
            .collect();
        let bind = animation.frames[0].clone();
        let position_weights = weights.to_vec();
        let context = Context {
            base_frame0: None,
            reference_weights: &[],
            reference_engine_space: false,
            motion_root: 0,
            bones: &bones,
            parents: &parents,
            motion_type: 0,
            reference,
            pos_weights: &position_weights,
            engine_space: false,
            reference_is_delta: false,
            root_rotation: crate::ani_writer::ROOT_REFERENCE_ANGLES,
            bind: &bind,
        };
        assert!(apply(animation, weights, &command, &context).unwrap());
    }

    fn close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
    }

    #[test]
    fn fixup_loop_spreads_endpoint_error_with_source_spline_weights() {
        let mut animation = animation();
        for frame in &mut animation.frames {
            frame.push(SmdPose {
                bone: 1,
                ..frame[0]
            });
        }
        run(
            &mut animation,
            &mut [1.0, 0.0],
            AnimCmd::FixupLoop { start: -2, end: 2 },
            None,
        );
        close(animation.frames[0][0].position[0], 3.0);
        close(animation.frames[6][0].position[0], 3.0);
        close(animation.frames[1][0].position[0], 1.9375);
        close(animation.frames[5][0].position[0], 4.0625);
        close(animation.frames[0][1].position[0], 0.0);
        close(animation.frames[6][1].position[0], 6.0);
    }

    #[test]
    fn derivative_uses_previous_frame_loop_wrap_and_quaternion_scaling() {
        let mut animation = animation();
        for (index, frame) in animation.frames.iter_mut().enumerate() {
            frame.push(SmdPose {
                bone: 1,
                position: [index as f32, 0.0, 0.0],
                rotation: [index as f32 * 0.1, 0.0, 0.0],
            });
        }
        animation.looping = true;
        run(
            &mut animation,
            &mut [0.0, 1.0],
            AnimCmd::Derivative { scale: 2.0 },
            None,
        );
        close(animation.frames[0][1].position[0], -10.0);
        close(animation.frames[1][1].position[0], 2.0);
        close(animation.frames[0][1].rotation[0], -1.0);
        close(animation.frames[1][1].rotation[0], 0.2);
    }

    #[test]
    fn linear_and_spline_delta_remove_interpolated_endpoints_in_canonical_axes() {
        for flags in [0x10, 0x50] {
            let mut animation = animation();
            animation.frames.truncate(3);
            for (frame, pos) in animation.frames.iter_mut().zip([1.0, 4.0, 5.0]) {
                frame[0].position[0] = pos;
            }
            run(
                &mut animation,
                &mut [1.0],
                AnimCmd::LinearDelta { flags },
                None,
            );
            assert!(animation.delta);
            close(animation.frames[0][0].position[0], 0.0);
            close(animation.frames[1][0].position[0], 1.0);
            close(animation.frames[2][0].position[0], 0.0);
            assert!(
                animation
                    .frames
                    .iter()
                    .all(|frame| frame[0].rotation.iter().all(|angle| angle.abs() < 0.0001))
            );
            close(
                animation.pre_subtract_frames.as_ref().unwrap()[1][0].position[0],
                4.0,
            );
        }
    }

    #[test]
    fn align_matches_selected_root_axes_and_keeps_the_animation_path() {
        let mut animation = animation();
        let mut reference = animation.frames.clone();
        reference[0][0].position = [10.0, 20.0, 30.0];
        reference[0][0].rotation = [0.0, 0.0, 0.5];
        run(
            &mut animation,
            &mut [1.0],
            AnimCmd::Align {
                reference: "reference".into(),
                motion_type: 1 | 2 | 32,
                src_frame: 0,
                dest_frame: 0,
                bone: None,
            },
            Some(&reference),
        );
        close(animation.frames[0][0].position[0], 10.0);
        close(animation.frames[0][0].position[1], 20.0);
        close(animation.frames[0][0].position[2], 0.0);
        close(animation.frames[0][0].rotation[2], 0.5);
        close(
            animation.frames[6][0].position[0],
            10.0 + 6.0 * 0.5f32.cos(),
        );
        close(
            animation.frames[6][0].position[1],
            20.0 + 6.0 * 0.5f32.sin(),
        );
    }

    #[test]
    fn match_changes_rotation_without_translating_and_rotateto_changes_roots_only() {
        let mut animation = animation();
        for (index, frame) in animation.frames.iter_mut().enumerate() {
            frame.push(SmdPose {
                bone: 1,
                position: [0.0, 0.0, 10.0],
                rotation: [index as f32 * 0.1, 0.0, 0.0],
            });
        }
        let mut reference = animation.frames.clone();
        reference[0][1].rotation[0] = 0.5;
        run(
            &mut animation,
            &mut [0.0, 1.0],
            AnimCmd::Match {
                reference: "reference".into(),
            },
            Some(&reference),
        );
        close(animation.frames[0][1].rotation[0], 0.5);
        close(animation.frames[6][1].rotation[0], 1.1);
        run(
            &mut animation,
            &mut [1.0, 1.0],
            AnimCmd::Angle { angle: 0.0 },
            None,
        );
        close(animation.frames[6][0].position[0], 0.0);
        close(animation.frames[6][0].position[1], -6.0);
        close(animation.frames[6][1].rotation[0], 1.1);
        close(animation.frames[6][1].position[2], 10.0);
    }

    #[test]
    fn world_commands_preserve_weighted_delta_representation_and_delta_references() {
        let base = vec![
            SmdPose {
                bone: 0,
                position: [2.0, 1.0, 0.0],
                rotation: [0.2, 0.1, 0.3],
            },
            SmdPose {
                bone: 1,
                position: [1.0, 2.0, 3.0],
                rotation: [0.1, 0.4, 0.2],
            },
        ];
        let bones: Vec<Bone> = vec![
            toml::from_str("name='root'").unwrap(),
            toml::from_str("name='tip'\nparent='root'").unwrap(),
        ];
        let encode = |frames: &[Vec<SmdPose>], weights: &[f32]| {
            frames
                .iter()
                .map(|frame| {
                    frame
                        .iter()
                        .enumerate()
                        .map(|(bone, pose)| SmdPose {
                            bone: bone as i32,
                            position: std::array::from_fn(|axis| {
                                (pose.position[axis] - base[bone].position[axis]) / weights[bone]
                            }),
                            rotation: math::quaternion_angles(math::quaternion_scale(
                                math::quaternion_mult(
                                    math::quaternion_scale(quat(base[bone].rotation), -1.0),
                                    quat(pose.rotation),
                                ),
                                1.0 / weights[bone],
                            )),
                        })
                        .collect()
                })
                .collect::<Vec<Vec<SmdPose>>>()
        };
        for command in [
            AnimCmd::Align {
                reference: "ref".into(),
                motion_type: 0x3F,
                src_frame: 0,
                dest_frame: 0,
                bone: Some("tip".into()),
            },
            AnimCmd::CounterRotate {
                bone: "tip".into(),
                target_angle: Some([20.0, 45.0, 10.0]),
            },
            AnimCmd::WorldSpaceBlend {
                reference: "ref".into(),
                start_frame: 0,
                loops: false,
            },
        ] {
            for delta_reference in [false, true] {
                let mut absolute = animation();
                absolute.frames = (0..3)
                    .map(|frame| {
                        let mut poses = base.clone();
                        poses[0].position[0] += frame as f32;
                        poses[1].rotation[2] += frame as f32 * 0.1;
                        poses
                    })
                    .collect();
                let mut reference = vec![base.clone()];
                reference[0][0].rotation[2] = 0.8;
                reference[0][1].position[2] = 10.0;
                let mut delta = absolute.clone();
                delta.delta = true;
                delta.frames = encode(&absolute.frames, &[0.5, 0.75]);
                let delta_source = encode(&reference, &[0.6, 0.8]);
                let context = Context {
                    base_frame0: Some(&base),
                    reference_weights: &[0.6, 0.8],
                    reference_engine_space: false,
                    bones: &bones,
                    parents: &[-1, 0],
                    motion_type: 0,
                    motion_root: 0,
                    reference: Some(&reference),
                    engine_space: false,
                    reference_is_delta: false,
                    root_rotation: [0.0, 0.0, std::f32::consts::FRAC_PI_2],
                    bind: &base,
                    pos_weights: &[0.5, 0.75],
                };
                apply(&mut absolute, &mut [0.5, 0.75], &command, &context).unwrap();
                let context = Context {
                    reference: Some(if delta_reference {
                        &delta_source
                    } else {
                        &reference
                    }),
                    reference_is_delta: delta_reference,
                    ..context
                };
                apply(&mut delta, &mut [0.5, 0.75], &command, &context).unwrap();
                assert!(delta.delta);
                for (actual, expected) in delta.frames.iter().zip(&absolute.frames) {
                    for bone in 0..2 {
                        for axis in 0..3 {
                            close(
                                base[bone].position[axis]
                                    + actual[bone].position[axis] * [0.5, 0.75][bone],
                                expected[bone].position[axis],
                            );
                        }
                        let rotation = math::quaternion_ma(
                            quat(base[bone].rotation),
                            [0.5, 0.75][bone],
                            quat(actual[bone].rotation),
                        );
                        for (actual, expected) in math::quaternion_matrix(rotation)
                            .into_iter()
                            .zip(math::angle_matrix(expected[bone].rotation))
                        {
                            close(actual, expected);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn counterrotate_keeps_child_orientation_fixed_in_world_space() {
        let mut animation = animation();
        for (index, frame) in animation.frames.iter_mut().enumerate() {
            frame[0].rotation[2] = index as f32 * 0.1;
            frame.push(SmdPose {
                bone: 1,
                position: [0.0, 0.0, 10.0],
                rotation: [0.0; 3],
            });
        }
        run(
            &mut animation,
            &mut [1.0, 1.0],
            AnimCmd::CounterRotate {
                bone: "bone1".into(),
                target_angle: Some([0.0, 45.0, 0.0]),
            },
            None,
        );
        let mut frames = animation.frames.clone();
        convert(&mut frames, &[-1, 0], true);
        for frame in frames {
            close(
                math::matrix_angles(&world(&frame, &[-1, 0])[1])[2],
                std::f32::consts::FRAC_PI_4,
            );
        }
    }

    #[test]
    fn world_space_blend_honors_rotation_and_position_weights_independently() {
        let mut animation = animation();
        animation.frames.truncate(1);
        animation.frames[0].push(SmdPose {
            bone: 1,
            position: [0.0, 0.0, 10.0],
            rotation: [0.0; 3],
        });
        let mut source = animation.frames.clone();
        source[0][0].rotation[2] = 0.5;
        source[0][1].rotation[2] = 0.5;
        source[0][1].position = [0.0, 0.0, 20.0];
        let bones: Vec<Bone> = vec![
            toml::from_str("name='root'").unwrap(),
            toml::from_str("name='tip'").unwrap(),
        ];
        let bind = animation.frames[0].clone();
        let context = Context {
            base_frame0: None,
            reference_weights: &[],
            reference_engine_space: false,
            motion_root: 0,
            bones: &bones,
            parents: &[-1, 0],
            motion_type: 0,
            reference: Some(&source),
            pos_weights: &[0.0, 0.25],
            bind: &bind,
            engine_space: false,
            reference_is_delta: false,
            root_rotation: crate::ani_writer::ROOT_REFERENCE_ANGLES,
        };
        apply(
            &mut animation,
            &mut [1.0, 0.5],
            &AnimCmd::WorldSpaceBlend {
                reference: "reference".into(),
                start_frame: 0,
                loops: false,
            },
            &context,
        )
        .unwrap();
        close(animation.frames[0][0].rotation[2], 0.5);
        close(animation.frames[0][1].rotation[2], 0.0);
        close(animation.frames[0][1].position[2], 12.5);
    }

    #[test]
    fn matchblend_uses_a_tapered_window_and_closes_loop_seams() {
        let mut animation = animation();
        let mut source = animation.frames.clone();
        source[0][0].position[0] = 12.0;
        run(
            &mut animation,
            &mut [1.0],
            AnimCmd::MatchBlend {
                reference: "ref".into(),
                src_frame: 0,
                dest_frame: 3,
                dest_pre: -1,
                dest_post: 1,
            },
            Some(&source),
        );
        close(animation.frames[3][0].position[0], 12.0);
        close(animation.frames[2][0].position[0], 6.5);
        close(animation.frames[4][0].position[0], 8.5);
        close(animation.frames[1][0].position[0], 1.0);
        let mut looping = self::animation();
        looping.looping = true;
        let mut source = looping.frames.clone();
        source[0][0].position[0] = 4.0;
        run(
            &mut looping,
            &mut [1.0],
            AnimCmd::MatchBlend {
                reference: "ref".into(),
                src_frame: 0,
                dest_frame: 0,
                dest_pre: -1,
                dest_post: 1,
            },
            Some(&source),
        );
        close(looping.frames[0][0].position[0], 4.0);
        close(looping.frames[6][0].position[0], 10.0);
        close(looping.frames[5][0].position[0], 7.0);
        close(looping.frames[1][0].position[0], 3.0);
    }

    #[test]
    fn world_space_loop_interpolates_last_interval_back_to_frame_zero() {
        let mut animation = animation();
        animation.frames.truncate(5);
        for frame in &mut animation.frames {
            frame[0].rotation = [0.0; 3];
        }
        let mut source = animation.frames[..3].to_vec();
        for (frame, angle) in source.iter_mut().zip([0.0, 0.4, 2.0]) {
            frame[0].rotation[2] = angle;
        }
        run(
            &mut animation,
            &mut [1.0],
            AnimCmd::WorldSpaceBlend {
                reference: "reference".into(),
                start_frame: 0,
                loops: true,
            },
            Some(&source),
        );
        close(animation.frames[0][0].rotation[2], 0.0);
        close(animation.frames[1][0].rotation[2], 0.2);
        close(animation.frames[2][0].rotation[2], 0.4);
        close(animation.frames[3][0].rotation[2], 0.2);
        close(animation.frames[4][0].rotation[2], 0.0);
    }
}
