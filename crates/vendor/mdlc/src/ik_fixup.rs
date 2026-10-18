use crate::{
    bone_math as math,
    model::{CompiledAnimation, IkRule, IkRuleType, ModelDesc},
};

pub(crate) fn apply(
    animation: &mut CompiledAnimation,
    desc: &ModelDesc,
    parents: &[i32],
    rules: &[IkRule],
    base_frame0: &[crate::smd::SmdPose],
) -> Result<(), String> {
    if parents.len() != desc.bones.len()
        || animation
            .frames
            .iter()
            .any(|frame| frame.len() != parents.len())
    {
        return Err("ikfixup frames do not match the model skeleton".into());
    }
    for rule in rules {
        if rule.kind != IkRuleType::Footstep || animation.frames.len() <= 1 {
            continue;
        }
        if !animation.local_hierarchy.is_empty() {
            return Err("ikfixup requires localhierarchy to be baked into animation frames".into());
        }
        if animation.delta && base_frame0.len() != parents.len() {
            return Err("ikfixup delta reference does not match the model skeleton".into());
        }
        let chain = desc
            .ikchains
            .iter()
            .find(|chain| chain.name.eq_ignore_ascii_case(&rule.chain))
            .ok_or_else(|| format!("Unknown ikfixup chain {:?}", rule.chain))?;
        let tip = desc
            .bones
            .iter()
            .position(|bone| bone.name.eq_ignore_ascii_case(&chain.bone))
            .ok_or("ikfixup chain references an absent bone")?;
        let mid = usize::try_from(parents[tip]).map_err(|_| "ikfixup chain needs a knee parent")?;
        let root = usize::try_from(parents[mid]).map_err(|_| "ikfixup chain needs a hip parent")?;
        let last = i32::try_from(animation.frames.len() - 1)
            .map_err(|_| "ikfixup animation is too long")?;
        let mut range = rule
            .range
            .unwrap_or([Some(0); 4])
            .map(|value| value.unwrap_or(-1));
        if range == [0; 4] {
            range[2] = last;
            range[3] = last;
        }
        for phase in 1..4 {
            if range[phase] < range[phase - 1] {
                range[phase] = range[phase]
                    .checked_add(last)
                    .ok_or("ikfixup frame range overflows")?;
            }
        }
        if range.iter().any(|frame| *frame < 0)
            || range.windows(2).any(|window| window[0] > window[1])
            || (!animation.looping && range[3] > last)
        {
            return Err(
                "ikfixup requires an ordered range within the animation or its loop".into(),
            );
        }
        let contact = rule
            .contact
            .filter(|frame| *frame != -1)
            .unwrap_or(range[1]);
        let index = |frame: i32| -> Result<usize, String> {
            if frame < 0 || (!animation.looping && frame > last) {
                return Err("ikfixup contact is outside the animation".into());
            }
            Ok(if animation.looping {
                frame % last
            } else {
                frame
            } as usize)
        };
        let worlds = |animation: &CompiledAnimation, sample: usize| {
            if animation.delta {
                crate::compile::delta_frame_worlds_weighted(
                    desc,
                    parents,
                    base_frame0,
                    &animation.frames[sample],
                    &animation.weights,
                )
            } else {
                crate::compile::frame_worlds(desc, parents, &animation.frames[sample])
            }
        };
        let world = worlds(animation, index(contact)?);
        let footfall = position(&world[tip]);
        for frame in range[0]..=range[3] {
            let sample = index(frame)?;
            let mut world = worlds(animation, sample);
            let movement = crate::anim_writer::ik_movement(
                &animation.movements,
                animation.frames.len(),
                frame,
                contact,
            );
            let target = add(footfall, movement);
            solve(&mut world, [root, mid, tip], target)?;
            for bone in [root, mid, tip] {
                let local = if parents[bone] < 0 {
                    world[bone]
                } else {
                    math::concat(&math::invert(&world[parents[bone] as usize]), &world[bone])
                };
                let local = if parents[bone] < 0 && !desc.model.static_prop {
                    math::concat(
                        &math::angle_matrix([0.0, 0.0, -std::f32::consts::FRAC_PI_2]),
                        &local,
                    )
                } else {
                    local
                };
                let pose = &mut animation.frames[sample][bone];
                if animation.delta {
                    let weight = animation.weights.get(bone).copied().unwrap_or(1.0);
                    let base = base_frame0[bone];
                    if weight <= 0.0 {
                        let expected = math::local_transform(base.position, base.rotation);
                        if local
                            .iter()
                            .zip(expected)
                            .any(|(actual, expected)| (actual - expected).abs() > 0.0001)
                        {
                            return Err(
                                "ikfixup cannot move a bone with zero animation weight".into()
                            );
                        }
                        continue;
                    }
                    pose.position = std::array::from_fn(|axis| {
                        (local[axis * 4 + 3] - base.position[axis]) / weight
                    });
                    let inverse_base =
                        math::quaternion_scale(math::angle_quaternion(base.rotation), -1.0);
                    let delta =
                        math::quaternion_mult(inverse_base, math::matrix_quaternion(&local));
                    pose.rotation =
                        math::quaternion_angles(math::quaternion_scale(delta, 1.0 / weight));
                } else {
                    pose.position = position(&local);
                    pose.rotation = math::matrix_angles(&local);
                }
            }
        }
    }
    if animation.looping && animation.frames.len() > 1 {
        let first = animation.frames[0].clone();
        *animation.frames.last_mut().unwrap() = first;
    }
    Ok(())
}

pub(crate) fn solve(
    world: &mut [math::Matrix3x4],
    [root, mid, tip]: [usize; 3],
    target: [f32; 3],
) -> Result<(), String> {
    let hip = position(&world[root]);
    let knee = sub(position(&world[mid]), hip);
    let foot = sub(position(&world[tip]), hip);
    let upper = length(knee);
    let lower = length(sub(foot, knee));
    let reach = length(foot);
    if upper <= 0.000001 || lower <= 0.000001 || reach <= 0.000001 {
        return Err("ikfixup chain contains a degenerate limb".into());
    }
    if reach > (upper + lower) * 0.9998 {
        return Ok(());
    }
    let knee_direction = normalized(sub(knee, scale(foot, upper / reach)))?;
    let mut desired = sub(target, hip);
    let distance = length(desired);
    if distance > (upper + lower) * 0.9998 {
        desired = scale(normalized(desired)?, (upper + lower) * 0.9998);
    }
    let minimum = ((upper - lower).abs() * 1.15).max(upper.min(lower) * 0.15);
    if length(desired) < minimum {
        desired = scale(normalized(foot)?, minimum);
    }
    let direction = normalized(desired)?;
    let knee_target = add(
        knee,
        scale(
            knee_direction,
            (upper + lower).max(distance - upper.min(lower)) * 100.0,
        ),
    );
    let side = normalized(sub(
        knee_target,
        scale(direction, dot(knee_target, direction)),
    ))?;
    let distance = length(desired);
    let along = (upper * upper - lower * lower + distance * distance) / (2.0 * distance);
    let perpendicular = (upper * upper - along * along).max(0.0).sqrt();
    let knee = add(scale(direction, along), scale(side, perpendicular));
    align(&mut world[root], knee)?;
    align(&mut world[mid], sub(desired, knee))?;
    let knee = add(hip, knee);
    let foot = add(hip, desired);
    for axis in 0..3 {
        world[mid][axis * 4 + 3] = knee[axis];
        world[tip][axis * 4 + 3] = foot[axis];
    }
    Ok(())
}
fn align(matrix: &mut math::Matrix3x4, axis: [f32; 3]) -> Result<(), String> {
    let x = normalized(axis)?;
    let y = cross([matrix[2], matrix[6], matrix[10]], x);
    let y = if length(y) < 0.000001 {
        let previous = [matrix[1], matrix[5], matrix[9]];
        normalized(sub(previous, scale(x, dot(previous, x))))?
    } else {
        normalized(y)?
    };
    let z = cross(x, y);
    for row in 0..3 {
        matrix[row * 4] = x[row];
        matrix[row * 4 + 1] = y[row];
        matrix[row * 4 + 2] = z[row];
    }
    Ok(())
}
fn position(matrix: &math::Matrix3x4) -> [f32; 3] {
    [matrix[3], matrix[7], matrix[11]]
}
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|axis| a[axis] + b[axis])
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|axis| a[axis] - b[axis])
}
fn scale(a: [f32; 3], value: f32) -> [f32; 3] {
    a.map(|a| a * value)
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}
fn length(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalized(a: [f32; 3]) -> Result<[f32; 3], String> {
    let length = length(a);
    if !length.is_finite() || length < 0.000001 {
        return Err("ikfixup cannot determine a stable limb bend direction".into());
    }
    Ok(scale(a, 1.0 / length))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limb() -> Vec<math::Matrix3x4> {
        [[0.0, 0.0, 0.0], [4.0, 3.0, 0.0], [8.0, 0.0, 0.0]]
            .map(|position| math::local_transform(position, [0.0; 3]))
            .to_vec()
    }

    fn fixture(
        delta: bool,
        looping: bool,
    ) -> (
        ModelDesc,
        CompiledAnimation,
        Vec<crate::smd::SmdPose>,
        IkRule,
    ) {
        let desc: ModelDesc = toml::from_str(
            r#"
[model]
name = "limb"
[[bones]]
name = "hip"
[[bones]]
name = "knee"
parent = "hip"
[[bones]]
name = "foot"
parent = "knee"
[[ikchains]]
name = "leg"
bone = "foot"
"#,
        )
        .unwrap();
        let base = [[0.0; 3], [4.0, 3.0, 0.0], [4.0, -3.0, 0.0]]
            .into_iter()
            .enumerate()
            .map(|(bone, position)| crate::smd::SmdPose {
                bone: bone as i32,
                position,
                rotation: [0.0; 3],
            })
            .collect::<Vec<_>>();
        let frames = [0.0, 1.0, 2.0, 1.0, 0.0]
            .into_iter()
            .map(|translation| {
                let mut frame = base.clone();
                frame[0].position[1] = translation;
                if delta {
                    for (pose, base) in frame.iter_mut().zip(&base) {
                        pose.position = std::array::from_fn(|axis| {
                            (pose.position[axis] - base.position[axis]) * 2.0
                        });
                    }
                }
                frame
            })
            .collect();
        let animation = CompiledAnimation {
            no_anim_block: false,
            no_anim_block_stall: false,
            source_frames: None,
            local_hierarchy: Vec::new(),
            weights: vec![if delta { 0.5 } else { 1.0 }; 3],
            loop_restart: 0,
            fudge_loop: false,
            extra_flags: 0,
            movements: Vec::new(),
            name: "walk".into(),
            smd_path: Default::default(),
            fps: 30.0,
            looping,
            frames,
            delta,
            ik_rules: Vec::new(),
            no_auto_ik: false,
            pre_subtract_frames: None,
        };
        let rule: IkRule =
            toml::from_str("chain = \"leg\"\ntype = \"footstep\"\ncontact = 0\n").unwrap();
        (desc, animation, base, rule)
    }

    #[test]
    fn footstep_fixup_preserves_absolute_and_weighted_delta_footfalls() {
        for delta in [false, true] {
            let (desc, mut animation, base, rule) = fixture(delta, false);
            apply(&mut animation, &desc, &[-1, 0, 1], &[rule], &base).unwrap();
            for frame in &animation.frames {
                let world = if delta {
                    crate::compile::delta_frame_worlds_weighted(
                        &desc,
                        &[-1, 0, 1],
                        &base,
                        frame,
                        &animation.weights,
                    )
                } else {
                    crate::compile::frame_worlds(&desc, &[-1, 0, 1], frame)
                };
                for (actual, expected) in position(&world[2]).into_iter().zip([0.0, 8.0, 0.0]) {
                    assert!(
                        (actual - expected).abs() < 0.0001,
                        "delta={delta}: {actual} vs {expected}"
                    );
                }
                assert!(
                    (length(sub(position(&world[1]), position(&world[0]))) - 5.0).abs() < 0.0001
                );
                assert!(
                    (length(sub(position(&world[2]), position(&world[1]))) - 5.0).abs() < 0.0001
                );
            }
        }
    }

    #[test]
    fn looping_footstep_fixup_wraps_the_range_and_preserves_loop_closure() {
        let (desc, mut animation, base, mut rule) = fixture(false, true);
        rule.range = Some([Some(3), Some(3), Some(1), Some(1)]);
        rule.contact = Some(3);
        apply(&mut animation, &desc, &[-1, 0, 1], &[rule], &base).unwrap();
        assert_eq!(animation.frames.first(), animation.frames.last());
        for index in [0, 1, 3] {
            let world = crate::compile::frame_worlds(&desc, &[-1, 0, 1], &animation.frames[index]);
            for (actual, expected) in position(&world[2]).into_iter().zip([-1.0, 8.0, 0.0]) {
                assert!((actual - expected).abs() < 0.0001);
            }
        }
    }

    #[test]
    fn limb_alignment_remains_orthonormal_when_the_new_axis_matches_the_old_up_axis() {
        let mut matrix = math::IDENTITY;
        align(&mut matrix, [0.0, 0.0, 10.0]).unwrap();
        let x = [matrix[0], matrix[4], matrix[8]];
        let y = [matrix[1], matrix[5], matrix[9]];
        let z = [matrix[2], matrix[6], matrix[10]];
        assert_eq!(x, [0.0, 0.0, 1.0]);
        for axis in [x, y, z] {
            assert!((length(axis) - 1.0).abs() < 0.000001);
        }
        assert!(dot(x, y).abs() < 0.000001);
        assert!(dot(x, z).abs() < 0.000001);
        assert!(dot(y, z).abs() < 0.000001);
        assert!((dot(cross(x, y), z) - 1.0).abs() < 0.000001);
    }

    #[test]
    fn two_bone_solver_reaches_target_and_preserves_limb_lengths() {
        let mut world = limb();
        let target = [6.0, 2.0, 0.0];
        solve(&mut world, [0, 1, 2], target).unwrap();
        for (actual, expected) in position(&world[2]).into_iter().zip(target) {
            assert!((actual - expected).abs() < 0.00001);
        }
        assert!((length(sub(position(&world[1]), position(&world[0]))) - 5.0).abs() < 0.00001);
        assert!((length(sub(position(&world[2]), position(&world[1]))) - 5.0).abs() < 0.00001);
        assert!(position(&world[1])[1] > 0.0);
        assert!(position(&world[1])[2].abs() < 0.00001);
    }

    #[test]
    fn unreachable_targets_clamp_and_straight_source_limbs_remain_unchanged() {
        let mut world = limb();
        solve(&mut world, [0, 1, 2], [50.0, 0.0, 0.0]).unwrap();
        assert!((length(position(&world[2])) - 9.998).abs() < 0.00001);
        let mut straight = [[0.0; 3], [5.0, 0.0, 0.0], [10.0, 0.0, 0.0]]
            .map(|position| math::local_transform(position, [0.0; 3]))
            .to_vec();
        let original = straight.clone();
        solve(&mut straight, [0, 1, 2], [6.0, 2.0, 0.0]).unwrap();
        assert_eq!(straight, original);
        world[1] = world[2];
        assert!(solve(&mut world, [0, 1, 2], [6.0, 2.0, 0.0]).is_err());
    }
}
