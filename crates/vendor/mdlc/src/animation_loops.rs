use crate::{model::CompiledAnimation, smd::SmdPose};

pub(crate) fn prepare(animation: &mut CompiledAnimation) -> Result<(), String> {
    if animation.fudge_loop {
        missing_frame(&mut animation.frames)?;
        if let Some(frames) = &mut animation.pre_subtract_frames {
            missing_frame(frames)?;
        }
    }
    Ok(())
}

fn missing_frame(frames: &mut Vec<Vec<SmdPose>>) -> Result<(), String> {
    if frames.len() < 2 {
        return Err("fudgeloop requires at least two source frames".into());
    }
    let first = &frames[0];
    let last = frames.last().unwrap();
    if first.len() != last.len() {
        return Err("fudgeloop source frames have different bone counts".into());
    }
    let scale = 1.0 / (frames.len() - 1) as f32;
    let mut predicted = first.clone();
    for ((predicted, first), last) in predicted.iter_mut().zip(first).zip(last) {
        if first.bone != last.bone {
            return Err("fudgeloop source bones do not match".into());
        }
        predicted.position = std::array::from_fn(|axis| {
            last.position[axis] + scale * (last.position[axis] - first.position[axis])
        });
        if !predicted
            .position
            .iter()
            .chain(&predicted.rotation)
            .all(|value| value.is_finite())
        {
            return Err("fudgeloop produced a nonfinite predicted pose".into());
        }
    }
    frames.push(predicted);
    Ok(())
}

pub(crate) fn realign(frames: &mut [Vec<SmdPose>], restart: i32) -> Result<(), String> {
    let restart = usize::try_from(restart)
        .map_err(|_| "startloop requires a nonnegative frame".to_string())?;
    if restart == 0 {
        return Ok(());
    }
    if frames.len() <= 1 || restart >= frames.len() {
        return Err("startloop frame is outside its animation".into());
    }
    let last = frames[restart].clone();
    let count = frames.len() - 1;
    frames[..count].rotate_left(restart % count);
    frames[count] = last;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames() -> Vec<Vec<SmdPose>> {
        (0..3)
            .map(|frame| {
                (0..3)
                    .map(|bone| SmdPose {
                        bone,
                        position: [bone as f32 + frame as f32 * 2.0, frame as f32 * -3.0, 1.0],
                        rotation: [frame as f32 * 0.5, bone as f32, 0.0],
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn missing_frame_extrapolates_all_bones_and_copies_first_rotations() {
        let mut frames = frames();
        missing_frame(&mut frames).unwrap();
        assert_eq!(frames.len(), 4);
        for (bone, (last, first)) in frames[3].iter().zip(&frames[0]).enumerate() {
            assert_eq!(last.position, [bone as f32 + 6.0, -9.0, 1.0]);
            assert_eq!(last.rotation, first.rotation);
        }
    }

    #[test]
    fn restart_rotates_period_and_retains_the_requested_last_frame() {
        let original = frames();
        for restart in [0, 1, 2] {
            let mut frames = original.clone();
            realign(&mut frames, restart).unwrap();
            assert_eq!(frames[0], original[restart as usize % 2]);
            assert_eq!(
                frames[2],
                original[if restart == 0 { 2 } else { restart as usize }]
            );
        }
        for restart in [-1, 3] {
            let mut frames = original.clone();
            assert!(realign(&mut frames, restart).is_err());
            assert_eq!(frames, original);
        }
    }

    #[test]
    fn invalid_fudge_inputs_fail_before_appending_a_pose() {
        let mut single = vec![frames()[0].clone()];
        assert!(missing_frame(&mut single).is_err());
        assert_eq!(single.len(), 1);
        let mut frames = frames();
        frames[0][0].position[0] = -f32::MAX;
        frames[2][0].position[0] = f32::MAX;
        assert!(missing_frame(&mut frames).is_err());
        assert_eq!(frames.len(), 3);
    }
}
