use crate::{
    bone_math as math,
    model::{AnimCmd, Movement},
    smd::SmdPose,
};

const LINEAR: i32 = 0x1000;
const QUADRATIC: i32 = 0x2000;

pub(crate) struct Processor {
    start: usize,
    root: usize,
    pub(crate) movements: Vec<Movement>,
}

impl Processor {
    pub(crate) fn begin(
        frames: &mut [Vec<SmdPose>],
        parents: &[i32],
        motion_type: i32,
        delta: bool,
        root: usize,
    ) -> Result<Self, String> {
        validate(frames, parents)?;
        if root >= parents.len() {
            return Err("Motion reference bone is outside the model skeleton".into());
        }
        validate_type(motion_type)?;
        if !delta {
            convert(frames, parents, true);
        }
        for (bone, &parent) in parents.iter().enumerate() {
            if parent != -1 {
                continue;
            }
            let first = frames[0][bone];
            for frame in &mut *frames {
                for axis in 0..3 {
                    if motion_type & (1 << axis) != 0 {
                        frame[bone].position[axis] = first.position[axis];
                    }
                    if motion_type & (8 << axis) != 0 {
                        frame[bone].rotation[axis] = first.rotation[axis];
                    }
                }
            }
        }
        Ok(Self {
            start: 0,
            root,
            movements: Vec::new(),
        })
    }

    pub(crate) fn command(
        &mut self,
        frames: &mut [Vec<SmdPose>],
        parents: &[i32],
        command: &AnimCmd,
        mut resolve_reference: impl FnMut(&str) -> Option<Vec<Vec<SmdPose>>>,
    ) -> Result<bool, String> {
        match command {
            AnimCmd::Motion {
                motion_type,
                end_frame,
            } => {
                let end = nonnegative(*end_frame, "walkframe end")?;
                let reference = frames[self.start.min(frames.len() - 1)][self.root];
                extract(
                    frames,
                    parents,
                    &mut self.movements,
                    *motion_type,
                    self.start,
                    end,
                    end,
                    reference,
                    self.root,
                )?;
                self.start = end;
            }
            AnimCmd::RefMotion {
                motion_type,
                end_frame,
                src_frame,
                reference,
                ref_frame,
            } => {
                let end = nonnegative(*end_frame, "walkalign end")?;
                let source = nonnegative(*src_frame, "walkalign source")?;
                let reference_frame = nonnegative(*ref_frame, "walkalign reference")?;
                let mut reference = resolve_reference(reference)
                    .ok_or_else(|| format!("Unknown motion reference animation {reference:?}"))?;
                validate(&reference, parents)?;
                if reference_frame >= reference.len() {
                    return Err("Motion reference frame is outside its animation".into());
                }
                convert(&mut reference, parents, true);
                extract(
                    frames,
                    parents,
                    &mut self.movements,
                    *motion_type,
                    self.start,
                    end,
                    source,
                    reference[reference_frame][self.root],
                    self.root,
                )?;
                self.start = end;
            }
            _ => {}
        }

        Ok(matches!(
            command,
            AnimCmd::Motion { .. } | AnimCmd::RefMotion { .. }
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finish(
        mut self,
        frames: &mut [Vec<SmdPose>],
        parents: &[i32],
        motion_type: i32,
        looping: bool,
        fps: f32,
        rollback: f32,
        delta: bool,
        loop_restart: i32,
    ) -> Result<Vec<Movement>, String> {
        if !fps.is_finite() || fps <= 0.0 {
            return Err("Motion extraction requires a finite positive frame rate".into());
        }
        if !rollback.is_finite() || rollback < 0.0 || !(fps * rollback).is_finite() {
            return Err("Motion rollback requires a finite nonnegative duration".into());
        }
        if delta && (motion_type != 0 || !self.movements.is_empty()) {
            return Err("Motion extraction on delta animations is not supported".into());
        }
        if motion_type != 0 {
            let last = frames.len() - 1;
            let end = if looping {
                last
            } else {
                (self.start.saturating_add(1).min(last))
                    .max(last.saturating_sub((fps * rollback) as usize))
            };
            let reference = frames[self.start.min(last)][self.root];
            extract(
                frames,
                parents,
                &mut self.movements,
                motion_type,
                self.start,
                end,
                last,
                reference,
                self.root,
            )?;
        }
        if looping {
            crate::animation_loops::realign(frames, loop_restart)?;
            let last = frames.len() - 1;
            let first = frames[0].clone();
            for (pose, first) in frames[last].iter_mut().zip(first) {
                for axis in 0..3 {
                    if motion_type & (0x40 << axis) == 0 {
                        pose.position[axis] = first.position[axis];
                    }
                    if motion_type & (0x200 << axis) == 0 {
                        pose.rotation[axis] = first.rotation[axis];
                    }
                }
            }
        }
        if !delta {
            convert(frames, parents, false);
        }
        validate(frames, parents)?;
        if self.movements.iter().any(|movement| {
            ![movement.v0, movement.v1, movement.angle]
                .iter()
                .chain(&movement.vector)
                .chain(&movement.position)
                .all(|value| value.is_finite())
        }) {
            return Err("Motion extraction produced nonfinite movement values".into());
        }
        Ok(self.movements)
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn process(
    frames: &mut [Vec<SmdPose>],
    parents: &[i32],
    motion_type: i32,
    looping: bool,
    fps: f32,
    rollback: f32,
    commands: &[AnimCmd],
    mut resolve_reference: impl FnMut(&str) -> Option<Vec<Vec<SmdPose>>>,
) -> Result<Vec<Movement>, String> {
    if commands.iter().any(|command| {
        !matches!(
            command,
            AnimCmd::Motion { .. }
                | AnimCmd::RefMotion { .. }
                | AnimCmd::IkRule { .. }
                | AnimCmd::Weights { .. }
        )
    }) {
        return Err("Motion extraction requires sequential processing of animation edits".into());
    }
    let mut working = frames.to_vec();
    let mut processor = Processor::begin(&mut working, parents, motion_type, false, 0)?;
    for command in commands {
        processor.command(&mut working, parents, command, &mut resolve_reference)?;
    }
    let movements = processor.finish(
        &mut working,
        parents,
        motion_type,
        looping,
        fps,
        rollback,
        false,
        0,
    )?;
    frames.clone_from_slice(&working);
    Ok(movements)
}

fn validate(frames: &[Vec<SmdPose>], parents: &[i32]) -> Result<(), String> {
    if frames.is_empty() || parents.is_empty() || !parents.contains(&-1) {
        return Err("Motion extraction requires animation frames and a root bone".into());
    }
    for frame in frames {
        if frame.len() != parents.len() {
            return Err("Motion animation bone counts do not match".into());
        }
        for (index, pose) in frame.iter().enumerate() {
            if pose.bone != index as i32
                || !pose
                    .position
                    .iter()
                    .chain(&pose.rotation)
                    .all(|v| v.is_finite())
            {
                return Err("Motion extraction requires finite poses ordered by bone index".into());
            }
        }
    }
    Ok(())
}

fn validate_type(value: i32) -> Result<(), String> {
    if value & !0x3fff != 0 {
        return Err(format!("Unsupported motion extraction flags {value:#x}"));
    }
    Ok(())
}

fn nonnegative(value: i32, label: &str) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("{label} frame must not be negative"))
}

fn transform_pose(pose: &mut SmdPose, transform: &math::Matrix3x4) {
    let matrix = math::concat(
        transform,
        &math::local_transform(pose.position, pose.rotation),
    );
    pose.position = [matrix[3], matrix[7], matrix[11]];
    pose.rotation = math::matrix_angles(&matrix);
}

fn convert(frames: &mut [Vec<SmdPose>], parents: &[i32], to_engine: bool) {
    let rotation = math::angle_matrix([
        0.0,
        0.0,
        if to_engine { 1.0 } else { -1.0 } * std::f32::consts::FRAC_PI_2,
    ]);
    for frame in frames {
        for (pose, &parent) in frame.iter_mut().zip(parents) {
            if parent == -1 {
                transform_pose(pose, &rotation);
            }
        }
    }
}

fn length(vector: [f32; 3]) -> f32 {
    vector.iter().map(|v| v * v).sum::<f32>().sqrt()
}

fn normalized(vector: [f32; 3]) -> [f32; 3] {
    let len = length(vector);
    if len == 0.0 {
        [0.0; 3]
    } else {
        vector.map(|v| v / len)
    }
}

#[allow(clippy::too_many_arguments)]
fn extract(
    frames: &mut [Vec<SmdPose>],
    parents: &[i32],
    movements: &mut Vec<Movement>,
    flags: i32,
    start: usize,
    end: usize,
    source: usize,
    reference: SmdPose,
    root: usize,
) -> Result<(), String> {
    validate_type(flags)?;
    if frames.len() <= 1 {
        return Err("Motion extraction requires at least two frames".into());
    }
    let end = end.min(frames.len() - 1);
    let source = source.min(frames.len() - 1);
    if start >= end {
        return Ok(());
    }
    if movements.len() >= 64 {
        return Err("Too many piecewise movement keys".into());
    }
    let middle = (start as f32 + source as f32) * 0.5;
    let index = middle as usize;
    let fraction = middle - index as f32;
    let reference_q = math::angle_quaternion(reference.rotation);
    let delta = math::quaternion_ma(
        math::angle_quaternion(frames[source][root].rotation),
        -1.0,
        reference_q,
    );
    let midpoint_delta = math::quaternion_ma(
        math::angle_quaternion(frames[index][root].rotation),
        -1.0,
        reference_q,
    );
    let mut rotation = [0.0; 3];
    for axis in 0..3 {
        if flags & (0x200 << axis) == 0 {
            continue;
        }
        let mut q = [0.0, 0.0, 0.0, delta[3]];
        q[axis] = delta[axis];
        rotation[axis] = math::quaternion_angles(math::quaternion_normalize(q))[axis];
        if axis == 2 {
            let midpoint = math::quaternion_angles(math::quaternion_normalize([
                0.0,
                0.0,
                midpoint_delta[2],
                midpoint_delta[3],
            ]))[2];
            if midpoint > std::f32::consts::FRAC_PI_4 && rotation[2] < 0.0 {
                rotation[2] += std::f32::consts::TAU;
            }
            if midpoint < -std::f32::consts::FRAC_PI_4 && rotation[2] > 0.0 {
                rotation[2] -= std::f32::consts::TAU;
            }
        }
    }
    let rotated_reference = math::concat(
        &math::angle_matrix(rotation),
        &math::local_transform(reference.position, [0.0; 3]),
    );
    let p0 = [
        rotated_reference[3],
        rotated_reference[7],
        rotated_reference[11],
    ];
    let p2 = std::array::from_fn(|axis| {
        if flags & (0x40 << axis) != 0 {
            frames[source][root].position[axis] - p0[axis]
        } else {
            0.0
        }
    });
    let p1: [f32; 3] = std::array::from_fn(|axis| {
        if flags & (0x40 << axis) != 0 {
            frames[index][root].position[axis] * (1.0 - fraction)
                + frames[(index + 1).min(frames.len() - 1)][root].position[axis] * fraction
                - p0[axis]
        } else {
            0.0
        }
    });
    let d1 = length(p1);
    let d2 = length(p2);
    let (mut v0, mut v1) = (-d2 + 4.0 * d1, 3.0 * d2 - 4.0 * d1);
    if flags & LINEAR != 0 {
        v0 = d2;
        v1 = d2;
    } else if v0 < 0.0 {
        v0 = 0.0;
        v1 = d2 * 2.0;
    } else if v1 < 0.0 {
        v0 = d2 * 2.0;
        v1 = 0.0;
    } else if v0 + v1 > 0.01 && (v0 - v1).abs() / (v0 + v1) < 0.2 {
        v0 = d2;
        v1 = d2;
    }
    let direction = normalized(p2);
    for (frame_index, frame) in frames.iter_mut().enumerate().skip(start) {
        let t = (frame_index.min(end) - start) as f32 / (end - start) as f32;
        let position = if flags & QUADRATIC != 0 {
            std::array::from_fn(|axis| {
                let a = 2.0 * p2[axis] - 4.0 * p1[axis];
                let b = 4.0 * p1[axis] - p2[axis];
                a * t * t + b * t
            })
        } else {
            direction.map(|v| v * (v0 * t + 0.5 * (v1 - v0) * t * t))
        };
        let adjustment = math::invert(&math::local_transform(position, rotation.map(|v| v * t)));
        for (pose, &parent) in frame.iter_mut().zip(parents) {
            if parent == -1 {
                transform_pose(pose, &adjustment);
            }
        }
    }
    let mut cumulative = math::local_transform(p2, rotation);
    let previous = movements.last();
    if let Some(previous) = previous {
        cumulative = math::concat(
            &math::local_transform(previous.position, [0.0, 0.0, previous.angle.to_radians()]),
            &cumulative,
        );
    }
    let position = [cumulative[3], cumulative[7], cumulative[11]];
    let vector = normalized(std::array::from_fn(|axis| {
        position[axis] - previous.map_or(0.0, |p| p.position[axis])
    }));
    let angle = if previous.is_none() {
        rotation[2]
    } else {
        math::matrix_angles(&cumulative)[2]
    };
    movements.push(Movement {
        endframe: end as i32,
        motionflags: flags,
        v0,
        v1,
        angle: angle.to_degrees(),
        vector,
        position,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(bone: i32, position: [f32; 3], yaw: f32) -> SmdPose {
        SmdPose {
            bone,
            position,
            rotation: [0.0, 0.0, yaw],
        }
    }

    fn canonical(mut frames: Vec<Vec<SmdPose>>, parents: &[i32]) -> Vec<Vec<SmdPose>> {
        for frame in &mut frames {
            for (pose, parent) in frame.iter_mut().zip(parents) {
                if *parent == -1 {
                    pose.position = [pose.position[1], -pose.position[0], pose.position[2]];
                    pose.rotation[2] -= std::f32::consts::FRAC_PI_2;
                }
            }
        }
        frames
    }

    fn close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
    }

    fn engine(frames: &[Vec<SmdPose>], parents: &[i32]) -> Vec<Vec<SmdPose>> {
        let mut result = frames.to_vec();
        convert(&mut result, parents, true);
        result
    }

    #[test]
    fn linear_and_accelerated_paths_remove_motion_and_keep_engine_axes() {
        for accelerated in [false, true] {
            let mut frames = canonical(
                (0..5)
                    .map(|i| {
                        let t = i as f32 / 4.0;
                        vec![pose(
                            0,
                            [8.0 * if accelerated { t * t } else { t }, 2.0, 1.0],
                            0.0,
                        )]
                    })
                    .collect(),
                &[-1],
            );
            let movements =
                process(&mut frames, &[-1], 0x40, true, 30.0, 0.2, &[], |_| None).unwrap();
            assert_eq!(movements.len(), 1);
            let movement = &movements[0];
            assert_eq!(movement.endframe, 4);
            close(movement.position[0], 8.0);
            close(movement.vector[0], 1.0);
            close(movement.v0, if accelerated { 0.0 } else { 8.0 });
            close(movement.v1, if accelerated { 16.0 } else { 8.0 });
            for frame in engine(&frames, &[-1]) {
                close(frame[0].position[0], 0.0);
                close(frame[0].position[1], 2.0);
            }
        }
    }

    #[test]
    fn sequential_walkframes_accumulate_paths_and_clamp_the_last_frame() {
        let mut frames = canonical(
            [0.0, 2.0, 4.0, 7.0, 10.0]
                .into_iter()
                .map(|x| vec![pose(0, [x, 0.0, 0.0], 0.0)])
                .collect(),
            &[-1],
        );
        let commands = [
            AnimCmd::Motion {
                motion_type: 0x1040,
                end_frame: 2,
            },
            AnimCmd::Motion {
                motion_type: 0x1040,
                end_frame: 99,
            },
        ];
        let movements =
            process(&mut frames, &[-1], 0, false, 30.0, 0.2, &commands, |_| None).unwrap();
        assert_eq!(
            movements.iter().map(|m| m.endframe).collect::<Vec<_>>(),
            [2, 4]
        );
        close(movements[0].position[0], 4.0);
        close(movements[1].position[0], 10.0);
        close(movements[1].v0, 6.0);
        for frame in engine(&frames, &[-1]) {
            close(frame[0].position[0], 0.0);
        }
    }

    #[test]
    fn turning_motion_adjusts_all_roots_and_leaves_children_local() {
        let parents = [-1, -1, 0];
        let mut frames = canonical(
            (0..5)
                .map(|i| {
                    let t = i as f32 / 4.0;
                    let a = t * std::f32::consts::FRAC_PI_2;
                    let (s, c) = a.sin_cos();
                    vec![
                        pose(0, [2.0 * c + 4.0 * t, 2.0 * s, 0.0], a),
                        pose(1, [c - 3.0 * s + 4.0 * t, s + 3.0 * c, 1.0], a),
                        pose(2, [0.0, 2.0, 1.0], 0.25),
                    ]
                })
                .collect(),
            &parents,
        );
        let commands = [AnimCmd::Motion {
            motion_type: 0x18c0,
            end_frame: 4,
        }];
        let movements = process(
            &mut frames,
            &parents,
            0,
            false,
            30.0,
            0.2,
            &commands,
            |_| None,
        )
        .unwrap();
        close(movements[0].angle, 90.0);
        close(movements[0].position[0], 4.0);
        close(movements[0].position[1], 0.0);
        for frame in engine(&frames, &parents) {
            for (actual, expected) in frame[0].position.iter().zip([2.0, 0.0, 0.0]) {
                close(*actual, expected);
            }
            for (actual, expected) in frame[1].position.iter().zip([1.0, 3.0, 1.0]) {
                close(*actual, expected);
            }
            close(frame[0].rotation[2], 0.0);
            assert_eq!(frame[2], pose(2, [0.0, 2.0, 1.0], 0.25));
        }
    }

    #[test]
    fn midpoint_disambiguates_turns_over_180_degrees() {
        let mut frames = canonical(
            (0..5)
                .map(|i| vec![pose(0, [0.0; 3], (i as f32 / 4.0 * 270.0).to_radians())])
                .collect(),
            &[-1],
        );
        let movements = process(&mut frames, &[-1], 0x800, true, 30.0, 0.2, &[], |_| None).unwrap();
        close(movements[0].angle, 270.0);
        for frame in engine(&frames, &[-1]) {
            close(frame[0].rotation[2], 0.0);
        }
    }

    #[test]
    fn references_freezes_and_nonloop_rollback_follow_source_semantics() {
        let mut frames = canonical(
            (0..11)
                .map(|i| vec![pose(0, [i as f32, i as f32, 0.0], 0.0)])
                .collect(),
            &[-1],
        );
        let movements = process(&mut frames, &[-1], 0x42, false, 10.0, 0.2, &[], |_| None).unwrap();
        assert_eq!(movements[0].endframe, 8);
        close(movements[0].position[0], 10.0);
        let result = engine(&frames, &[-1]);
        close(result[8][0].position[0], -2.0);
        close(result[10][0].position[0], 0.0);
        assert!(result.iter().all(|f| f[0].position[1].abs() < 0.0001));
        let mut frames = canonical(
            (0..4)
                .map(|i| vec![pose(0, [0.0, i as f32, 0.0], 0.0)])
                .collect(),
            &[-1],
        );
        let reference = canonical(vec![vec![pose(0, [0.0, -2.0, 0.0], 0.0)]], &[-1]);
        let commands = [AnimCmd::RefMotion {
            motion_type: 0x1080,
            end_frame: 99,
            src_frame: 99,
            reference: "reference".into(),
            ref_frame: 0,
        }];
        let movements = process(&mut frames, &[-1], 0, false, 30.0, 0.2, &commands, |name| {
            (name == "reference").then(|| reference.clone())
        })
        .unwrap();
        close(movements[0].position[1], 5.0);
        close(engine(&frames, &[-1])[3][0].position[1], -2.0);
    }

    #[test]
    fn loop_closure_preserves_only_components_marked_for_local_motion() {
        let mut frames = canonical(
            vec![
                vec![pose(0, [0.0; 3], 0.0), pose(1, [1.0, 2.0, 3.0], 0.25)],
                vec![
                    pose(0, [2.0, 7.0, 9.0], 0.5),
                    pose(1, [4.0, 5.0, 6.0], 0.75),
                ],
            ],
            &[-1, 0],
        );
        process(&mut frames, &[-1, 0], 0x40, true, 30.0, 0.2, &[], |_| None).unwrap();
        let frames = engine(&frames, &[-1, 0]);
        close(frames[1][0].position[0], 0.0);
        close(frames[1][0].position[1], 0.0);
        close(frames[1][0].position[2], 0.0);
        close(frames[1][1].position[0], 4.0);
        close(frames[1][1].position[1], 2.0);
        close(frames[1][1].position[2], 3.0);
        close(frames[1][1].rotation[2], 0.25);
    }

    #[test]
    fn quadratic_motion_removes_a_curved_path() {
        let mut frames = canonical(
            (0..5)
                .map(|i| {
                    let t = i as f32 / 4.0;
                    vec![pose(0, [4.0 * t, 8.0 * t * t, 0.0], 0.0)]
                })
                .collect(),
            &[-1],
        );
        let movements =
            process(&mut frames, &[-1], 0x20c0, true, 30.0, 0.2, &[], |_| None).unwrap();
        close(movements[0].position[0], 4.0);
        close(movements[0].position[1], 8.0);
        for frame in engine(&frames, &[-1]) {
            for value in frame[0].position {
                close(value, 0.0);
            }
        }
    }

    #[test]
    fn invalid_inputs_and_unordered_commands_fail_without_changing_frames() {
        let original = canonical(
            vec![
                vec![pose(0, [0.0; 3], 0.0)],
                vec![pose(0, [1.0, 0.0, 0.0], 0.0)],
            ],
            &[-1],
        );
        for command in [
            AnimCmd::Motion {
                motion_type: 0x40,
                end_frame: -1,
            },
            AnimCmd::RefMotion {
                motion_type: 0x40,
                end_frame: 1,
                src_frame: 1,
                reference: "missing".into(),
                ref_frame: 0,
            },
            AnimCmd::NoAnimation,
        ] {
            let mut frames = original.clone();
            assert!(
                process(&mut frames, &[-1], 0, false, 30.0, 0.2, &[command], |_| {
                    None
                })
                .is_err()
            );
            assert_eq!(frames, original);
        }
        let mut frames = original[..1].to_vec();
        assert!(process(&mut frames, &[-1], 0x40, false, 30.0, 0.2, &[], |_| None).is_err());
        let mut overflow = original.clone();
        overflow[0][0].position = [f32::MAX, f32::MAX, 0.0];
        overflow[1][0].position = [-f32::MAX, -f32::MAX, 0.0];
        let before = overflow.clone();
        assert!(process(&mut overflow, &[-1], 0xc0, false, 30.0, 0.2, &[], |_| None).is_err());
        assert_eq!(overflow, before);
        let mut frames = original.clone();
        assert!(process(&mut frames, &[-1], 0x4000, false, 30.0, 0.2, &[], |_| None).is_err());
        assert!(
            process(&mut frames, &[-1], 0x40, false, f32::NAN, 0.2, &[], |_| {
                None
            })
            .is_err()
        );
    }
    #[test]
    fn explicit_rollback_changes_extraction_duration_and_zero_axis_motion_is_recorded() {
        for (rollback, end) in [(0.0, 10), (0.3, 7), (0.7, 3)] {
            let mut frames = canonical(
                (0..11)
                    .map(|index| vec![pose(0, [index as f32, 0.0, 0.0], 0.0)])
                    .collect(),
                &[-1],
            );
            let movements = process(
                &mut frames,
                &[-1],
                0x1040,
                false,
                10.0,
                rollback,
                &[],
                |_| None,
            )
            .unwrap();
            assert_eq!(movements[0].endframe, end);
            close(movements[0].position[0], 10.0);
            close(engine(&frames, &[-1])[10][0].position[0], 0.0);
        }
        let mut frames = canonical(
            vec![
                vec![pose(0, [1.0, 2.0, 3.0], 0.0)],
                vec![pose(0, [4.0, 5.0, 6.0], 0.5)],
            ],
            &[-1],
        );
        let movements = process(&mut frames, &[-1], 7, false, 30.0, 0.3, &[], |_| None).unwrap();
        assert_eq!(movements.len(), 1);
        assert_eq!(movements[0].position, [0.0; 3]);
        assert_eq!(movements[0].motionflags, 7);
        for frame in engine(&frames, &[-1]) {
            for (actual, expected) in frame[0].position.into_iter().zip([1.0, 2.0, 3.0]) {
                close(actual, expected);
            }
        }
    }
}
