use crate::{bone_math as math, compile::resolve_bone_pose, model::CompiledModelDesc};

pub(crate) const FIXED_ALIGNMENT: i32 = 0x0010_0000;

pub(crate) fn calculate(compiled: &CompiledModelDesc) -> Vec<Option<[f32; 4]>> {
    let mut result = vec![None; compiled.desc.bones.len()];
    for limit in &compiled.desc.model.rotation_limits {
        let Some(bone) = compiled
            .desc
            .bones
            .iter()
            .position(|bone| bone.name.eq_ignore_ascii_case(&limit.bone))
        else {
            continue;
        };
        let mut base = math::angle_quaternion(resolve_bone_pose(&compiled.desc, compiled, bone).1);
        if limit.sequences.is_empty() {
            for animation in &compiled.animations {
                if animation.delta || animation.frames.len() <= 3 {
                    continue;
                }
                let root = compiled.desc.bones[bone].parent.is_none();
                let sweep = animation_alignment(animation.frames.iter().map(|frame| {
                    let rotation = frame[bone].rotation;
                    if root {
                        math::matrix_angles(&math::concat(
                            &math::angle_matrix([0.0, 0.0, std::f32::consts::FRAC_PI_2]),
                            &math::angle_matrix(rotation),
                        ))
                    } else {
                        rotation
                    }
                }));
                let sweep = align(base, sweep);
                base = std::array::from_fn(|axis| base[axis] + sweep[axis]);
            }
            base = math::quaternion_normalize(base);
        }
        result[bone] = Some(base);
    }
    result
}

fn dot(a: [f32; 4], b: [f32; 4]) -> f32 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}

pub(crate) fn align(reference: [f32; 4], quaternion: [f32; 4]) -> [f32; 4] {
    let forward: f32 = reference
        .into_iter()
        .zip(quaternion)
        .map(|(a, b)| (a - b) * (a - b))
        .sum();
    let backward: f32 = reference
        .into_iter()
        .zip(quaternion)
        .map(|(a, b)| (a + b) * (a + b))
        .sum();
    if forward > backward {
        quaternion.map(|value| -value)
    } else {
        quaternion
    }
}

fn midpoint(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let cosine = dot(a, b).clamp(-1.0, 1.0);
    if 1.0 + cosine > 0.000001 {
        let scale = if 1.0 - cosine > 0.000001 {
            let angle = cosine.acos();
            (angle * 0.5).sin() / angle.sin()
        } else {
            0.5
        };
        std::array::from_fn(|axis| scale * a[axis] + scale * b[axis])
    } else {
        let mut result = [-b[1], b[0], -b[3], b[2]];
        let scale = std::f32::consts::FRAC_PI_4.sin();
        for (component, original) in result[..3].iter_mut().zip(a) {
            *component = scale * original + scale * *component;
        }
        result
    }
}

fn animation_alignment(mut rotations: impl Iterator<Item = [f32; 3]>) -> [f32; 4] {
    let mut base = math::angle_quaternion(rotations.next().unwrap());
    let mut min = base;
    let mut max = base;
    let mut min_dot = 1.0;
    let mut max_dot = 1.0;
    for rotation in rotations {
        let quaternion = align(base, math::angle_quaternion(rotation));
        let base_dot = dot(quaternion, base);
        if dot(quaternion, min) >= base_dot {
            if base_dot < min_dot {
                min = quaternion;
                if max_dot == 1.0 {
                    max = align(base, math::quaternion_ma(base, -0.01, min));
                }
            }
        } else if dot(quaternion, max) >= base_dot && base_dot < max_dot {
            max = quaternion;
        }
        base = midpoint(min, max);
        min_dot = dot(base, min);
        max_dot = dot(base, max);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alignment_preserves_the_long_arc_and_selects_the_bind_quaternion_sign() {
        let start = math::angle_quaternion([0.0; 3]);
        let end = math::angle_quaternion([0.0, 0.0, 240_f32.to_radians()]);
        let middle = midpoint(start, end);
        let expected = math::angle_quaternion([0.0, 0.0, 120_f32.to_radians()]);
        for (actual, expected) in middle.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 0.00001);
        }
        assert_eq!(align([0.0, 0.0, 0.0, -1.0], start), [0.0, 0.0, 0.0, -1.0]);
    }
}
