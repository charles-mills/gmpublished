//! Authored sequence evaluation in model space.

#![allow(missing_docs)]

use super::{
    LocalBonePose, MAT4_IDENTITY, ModelAttachment, array_is_finite, local_pose_matrix,
    mat4_from_pose_to_bone, mat4_mul, matrix_is_finite, stored_quat_to_wxyz,
};
use crate::mdl::studio::{
    BONE_FIXED_ALIGNMENT, BONE_SCREEN_ALIGN_CYLINDER, BONE_SCREEN_ALIGN_SPHERE, Mdl,
    MdlAnimationDescription, MdlBone, STUDIO_AUTOPLAY, STUDIO_CYCLEPOSE, STUDIO_DELTA,
    STUDIO_LOCAL, STUDIO_POST, STUDIO_REALTIME, STUDIO_RESERVED_ENCODING, STUDIO_WORLD,
};

const UNSUPPORTED_SEQUENCE_FLAGS: u32 =
    STUDIO_DELTA | STUDIO_POST | STUDIO_CYCLEPOSE | STUDIO_REALTIME | STUDIO_LOCAL | STUDIO_WORLD;

/// Authored sequence metadata, including model-space animation bounds.
pub use crate::mdl::studio::MdlSequence as ModelSequence;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelAnimationError {
    InvalidSequence { index: usize },
    InvalidCycle,
    Unsupported { feature: &'static str },
    Malformed { part: &'static str },
}

impl std::fmt::Display for ModelAnimationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSequence { index } => write!(f, "model sequence {index} does not exist"),
            Self::InvalidCycle => {
                f.write_str("animation cycle must be finite and between zero and one")
            }
            Self::Unsupported { feature } => write!(f, "unsupported model animation: {feature}"),
            Self::Malformed { part } => write!(f, "invalid model animation {part}"),
        }
    }
}
impl std::error::Error for ModelAnimationError {}

/// Immutable skeleton and authored clips shared by model placements.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnimation {
    bones: Vec<MdlBone>,
    // CStudioHdr uses full-table flags independently of linear clip overrides.
    full_bone_flags: Vec<u32>,
    clips: Vec<MdlAnimationDescription>,
    sequences: Vec<ModelSequence>,
    attachments: Vec<crate::mdl::MdlAttachment>,
    order: Result<Vec<usize>, ModelAnimationError>,
    included_models: usize,
    bone_controllers: usize,
    flex_descriptors: usize,
    has_autoplay: bool,
}

/// Reusable evaluated pose; all 4x4 matrices are column-major.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelPose {
    /// Bone-to-model multiplied by the authored inverse bind matrix.
    pub skin_matrices: Vec<[[f32; 4]; 4]>,
    pub bone_to_model: Vec<[[f32; 4]; 4]>,
    pub attachments: Vec<ModelAttachment>,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    local: Vec<LocalBonePose>,
}

impl ModelAnimation {
    pub(super) fn from_mdl(mdl: &Mdl) -> Result<Self, crate::mdl::MdlError> {
        let bones = mdl.runtime_bones()?.into_owned();
        let order = hierarchy_order(&bones);
        Ok(Self {
            bones,
            full_bone_flags: mdl.bones.iter().map(|bone| bone.flags).collect(),
            clips: mdl.local_animations.clone(),
            sequences: mdl.sequences.clone(),
            attachments: mdl.attachments.clone(),
            order,
            included_models: mdl.included_model_count,
            bone_controllers: mdl.bone_controller_count,
            flex_descriptors: mdl.flex_descriptor_count,
            has_autoplay: mdl.sequences.iter().any(|s| s.flags & STUDIO_AUTOPLAY != 0),
        })
    }

    #[must_use]
    pub fn bone_count(&self) -> usize {
        self.bones.len()
    }

    #[must_use]
    pub fn sequences(&self) -> &[ModelSequence] {
        &self.sequences
    }

    /// Source sequence names are case-insensitive.
    #[must_use]
    pub fn sequence_index(&self, name: &str) -> Option<usize> {
        self.sequences
            .iter()
            .position(|s| s.label.eq_ignore_ascii_case(name))
    }

    /// Playback duration; the last frame is the endpoint, not another interval.
    pub fn duration(&self, sequence: usize) -> Result<f32, ModelAnimationError> {
        let (_, clip) = self.clip(sequence)?;
        Ok(clip.frame_count.saturating_sub(1) as f32 / clip.fps)
    }

    fn clip(
        &self,
        index: usize,
    ) -> Result<(&ModelSequence, &MdlAnimationDescription), ModelAnimationError> {
        use ModelAnimationError::{Malformed, Unsupported};
        let s = self
            .sequences
            .get(index)
            .ok_or(ModelAnimationError::InvalidSequence { index })?;
        if self.bone_controllers != 0 {
            return Err(Unsupported {
                feature: "bone controllers",
            });
        }
        if self.flex_descriptors != 0 {
            return Err(Unsupported {
                feature: "flex animation",
            });
        }
        if self.has_autoplay {
            return Err(Unsupported {
                feature: "autoplay sequences",
            });
        }
        if self.included_models != 0 {
            return Err(Unsupported {
                feature: "included models",
            });
        }
        if s.animations.len() != 1
            || s.blend_grid != [1, 1]
            || s.pose_parameters.iter().any(|p| *p >= 0)
        {
            return Err(Unsupported {
                feature: "pose-parameter sequence blending",
            });
        }
        if s.autolayer_count != 0 {
            return Err(Unsupported {
                feature: "sequence layers",
            });
        }
        if s.ik_rule_count != 0 || s.ik_lock_count != 0 {
            return Err(Unsupported {
                feature: "sequence IK",
            });
        }
        if !valid_bounds(s.bounds_min, s.bounds_max)
            || s.bone_weights.len() != self.bones.len()
            || s.bone_weights
                .iter()
                .any(|w| !w.is_finite() || !(0.0..=1.0).contains(w))
        {
            return Err(Malformed {
                part: "sequence bounds or weights",
            });
        }
        let clip = self.clips.get(s.animations[0]).ok_or(Malformed {
            part: "sequence clip index",
        })?;
        if !clip.fps.is_finite() || clip.fps <= 0.0 || clip.frame_count == 0 {
            return Err(Malformed {
                part: "clip timing",
            });
        }
        if clip.is_all_zeros() {
            return Ok((s, clip));
        }
        if s.flags & UNSUPPORTED_SEQUENCE_FLAGS != 0 {
            return Err(Unsupported {
                feature: "delta, local, world, realtime, or pose-driven sequences",
            });
        }
        if clip.external_block.is_some() || clip.sections.iter().any(|s| s.external_block.is_some())
        {
            return Err(Unsupported {
                feature: "external ANI data",
            });
        }
        if clip.hierarchy_count != 0 {
            return Err(Unsupported {
                feature: "local hierarchy animation",
            });
        }
        if clip.ik_rule_count != 0 {
            return Err(Unsupported {
                feature: "animation IK",
            });
        }
        if clip.flags & (STUDIO_DELTA | STUDIO_RESERVED_ENCODING) != 0 {
            return Err(Unsupported {
                feature: "delta or unsupported clip encoding",
            });
        }
        Ok((s, clip))
    }

    /// Looping cycle one wraps; nonlooping cycle one retains the final frame.
    pub fn evaluate(
        &self,
        sequence: usize,
        cycle: f32,
        out: &mut ModelPose,
    ) -> Result<(), ModelAnimationError> {
        use ModelAnimationError::{Malformed, Unsupported};
        if !cycle.is_finite() || !(0.0..=1.0).contains(&cycle) {
            return Err(ModelAnimationError::InvalidCycle);
        }
        let (sequence, clip) = self.clip(sequence)?;
        let order = self.order.as_ref().map_err(Clone::clone)?;
        for index in 0..self.bones.len() {
            let flags = self.full_bone_flags[index];
            if flags & (BONE_SCREEN_ALIGN_SPHERE | BONE_SCREEN_ALIGN_CYLINDER) != 0 {
                return Err(Unsupported {
                    feature: "screen-aligned bones",
                });
            }
            let weight = sequence.bone_weights[index];
            if flags & BONE_FIXED_ALIGNMENT != 0
                && weight > 0.0
                && weight < 1.0
                && !clip.is_all_zeros()
            {
                return Err(Unsupported {
                    feature: "fixed-alignment sequence bone blending",
                });
            }
        }
        out.local.clear();
        for bone in &self.bones {
            if bone.procedural_kind != 0 {
                return Err(Unsupported {
                    feature: "procedural bones",
                });
            }
            out.local.push(LocalBonePose {
                position: bone.position,
                rotation: stored_quat_to_wxyz(bone.quaternion),
            });
        }
        let cycle = if sequence.looping() && cycle == 1.0 {
            0.0
        } else {
            cycle
        };
        let frame = cycle * clip.frame_count.saturating_sub(1) as f32;
        let integer = frame.floor() as usize;
        let fraction = frame - integer as f32;
        let fraction = if fraction > 0.001 { fraction } else { 0.0 };
        let (channels, local_frame) = clip.channels_at(integer)?;
        for channel in channels {
            let index = usize::from(channel.bone);
            let bone = self.bones.get(index).ok_or(Malformed {
                part: "channel bone",
            })?;
            let (p0, q0) = channel.local_pose(bone, local_frame)?;
            let (p1, q1) = channel.local_pose(bone, local_frame.saturating_add(1))?;
            let weight = sequence.bone_weights[index];
            let p = lerp(p0, p1, fraction);
            let q = normalized_blend(q0, q1, fraction)?;
            out.local[index] = LocalBonePose {
                position: lerp(bone.position, p, weight),
                rotation: slerp(stored_quat_to_wxyz(bone.quaternion), q, weight)?,
            };
        }
        out.bone_to_model.resize(self.bones.len(), MAT4_IDENTITY);
        out.skin_matrices.resize(self.bones.len(), MAT4_IDENTITY);
        for &index in order {
            let bone = &self.bones[index];
            let local = local_pose_matrix(out.local[index]);
            let model = if bone.parent < 0 {
                local
            } else {
                mat4_mul(out.bone_to_model[bone.parent as usize], local)
            };
            let inverse_bind = mat4_from_pose_to_bone(bone.pose_to_bone).ok_or(Malformed {
                part: "inverse bind matrix",
            })?;
            let skin = mat4_mul(model, inverse_bind);
            if !matrix_is_finite(&model) || !matrix_is_finite(&skin) {
                return Err(Malformed {
                    part: "pose matrix",
                });
            }
            out.bone_to_model[index] = model;
            out.skin_matrices[index] = skin;
        }
        out.attachments.truncate(self.attachments.len());
        for (index, source) in self.attachments.iter().enumerate() {
            let bone = out.bone_to_model.get(source.local_bone).ok_or(Malformed {
                part: "attachment bone",
            })?;
            let local = mat4_from_pose_to_bone(source.local).ok_or(Malformed {
                part: "attachment matrix",
            })?;
            let model = mat4_mul(*bone, local);
            let model = std::array::from_fn(|r| std::array::from_fn(|c| model[c][r]));
            if let Some(existing) = out.attachments.get_mut(index) {
                existing.model = model;
            } else {
                out.attachments.push(ModelAttachment {
                    source: source.clone(),
                    model,
                });
            }
        }
        out.bounds_min = sequence.bounds_min;
        out.bounds_max = sequence.bounds_max;
        Ok(())
    }
}

fn hierarchy_order(bones: &[MdlBone]) -> Result<Vec<usize>, ModelAnimationError> {
    let mut states = vec![0u8; bones.len()];
    let mut order = Vec::with_capacity(bones.len());
    let mut chain = Vec::new();
    for start in 0..bones.len() {
        let mut index = start;
        chain.clear();
        loop {
            match states[index] {
                2 => break,
                1 => {
                    return Err(ModelAnimationError::Malformed {
                        part: "cyclic bone hierarchy",
                    });
                }
                _ => {}
            }
            states[index] = 1;
            chain.push(index);
            let bone = &bones[index];
            if !array_is_finite(bone.position)
                || !super::quat_is_valid_unit(stored_quat_to_wxyz(bone.quaternion))
            {
                return Err(ModelAnimationError::Malformed { part: "bind pose" });
            }
            if bone.parent == -1 {
                break;
            }
            index = usize::try_from(bone.parent)
                .ok()
                .filter(|p| *p < bones.len())
                .ok_or(ModelAnimationError::Malformed {
                    part: "bone parent",
                })?;
        }
        while let Some(index) = chain.pop() {
            states[index] = 2;
            order.push(index);
        }
    }
    Ok(order)
}

fn valid_bounds(min: [f32; 3], max: [f32; 3]) -> bool {
    array_is_finite(min) && array_is_finite(max) && (0..3).all(|i| min[i] <= max[i])
}
fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}
fn slerp(a: [f32; 4], mut b: [f32; 4], t: f32) -> Result<[f32; 4], ModelAnimationError> {
    let mut dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    if dot < 0.0 {
        b = b.map(|v| -v);
        dot = -dot;
    }
    let (wa, wb) = if dot > 0.9995 {
        (1.0 - t, t)
    } else {
        let angle = dot.clamp(-1.0, 1.0).acos();
        let denominator = angle.sin();
        (
            ((1.0 - t) * angle).sin() / denominator,
            (t * angle).sin() / denominator,
        )
    };
    let q: [f32; 4] = std::array::from_fn(|i| a[i] * wa + b[i] * wb);
    let norm = q.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return Err(ModelAnimationError::Malformed {
            part: "rotation quaternion",
        });
    }
    Ok(q.map(|v| v / norm))
}

fn normalized_blend(a: [f32; 4], mut b: [f32; 4], t: f32) -> Result<[f32; 4], ModelAnimationError> {
    if a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() < 0.0 {
        b = b.map(|v| -v);
    }
    let q: [f32; 4] = std::array::from_fn(|i| a[i] * (1.0 - t) + b[i] * t);
    let norm = q.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !norm.is_finite() || norm <= f32::EPSILON {
        return Err(ModelAnimationError::Malformed {
            part: "rotation quaternion",
        });
    }
    Ok(q.map(|v| v / norm))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdl::{MdlAnimation, MdlAnimationSection, MdlAttachment};

    fn bone(parent: i32, x: f32) -> MdlBone {
        MdlBone {
            name: String::new(),
            parent,
            position: [x, 0.0, 0.0],
            quaternion: [0.0, 0.0, 0.0, 1.0],
            rotation: [0.0; 3],
            position_scale: [1.0; 3],
            rotation_scale: [1.0; 3],
            quaternion_alignment: [0.0, 0.0, 0.0, 1.0],
            flags: 0,
            procedural_kind: 0,
            pose_to_bone: [
                [1.0, 0.0, 0.0, -x],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
        }
    }
    fn rig() -> ModelAnimation {
        let bones = vec![bone(-1, 10.0)];
        ModelAnimation {
            order: hierarchy_order(&bones),
            bones,
            full_bone_flags: vec![0],
            clips: vec![MdlAnimationDescription {
                name: "@idle".into(),
                fps: 40.0,
                flags: 1,
                frame_count: 3,
                animations: vec![MdlAnimation::positions_for_tests(
                    0,
                    vec![[0.0; 3], [4.0, 0.0, 0.0], [8.0, 0.0, 0.0]],
                )],
                sections: vec![],
                section_frames: 0,
                external_block: None,
                hierarchy_count: 0,
                ik_rule_count: 0,
            }],
            sequences: vec![ModelSequence {
                label: "idle".into(),
                flags: 1,
                bounds_min: [-1.0; 3],
                bounds_max: [20.0; 3],
                animations: vec![0],
                blend_grid: [1, 1],
                pose_parameters: [-1, -1],
                autolayer_count: 0,
                ik_rule_count: 0,
                ik_lock_count: 0,
                bone_weights: vec![1.0],
            }],
            attachments: vec![],
            included_models: 0,
            bone_controllers: 0,
            flex_descriptors: 0,
            has_autoplay: false,
        }
    }
    #[test]
    fn authored_offsets_subframes_bounds_and_original_weights() {
        let animation = rig();
        assert_eq!(animation.duration(0).unwrap(), 0.05);
        assert_eq!(animation.sequence_index("IDLE"), Some(0));
        assert!(animation.sequences()[0].looping());
        let mut pose = ModelPose::default();
        animation.evaluate(0, 0.25, &mut pose).unwrap();
        assert_eq!(pose.bone_to_model[0][3][0], 12.0);
        assert_eq!(pose.skin_matrices[0][3][0], 2.0);
        assert_eq!(pose.bounds_min, [-1.0; 3]);
        assert_eq!(pose.bounds_max, [20.0; 3]);
        let vertex = crate::mdl::VvdVertex {
            position: [1.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0; 2],
            bones: [0; 3],
            weights: [0.3, 0.2, 0.0],
            bone_count: 2,
        };
        let (position, normal) = super::super::skin_model_vertex(
            &pose.skin_matrices,
            &vertex,
            &mut super::super::MdlStats::default(),
        );
        assert!((position[0] - 1.5).abs() < 1e-6);
        assert_eq!(normal, [0.0, 0.0, 0.5]);
    }

    #[test]
    fn screen_alignment_is_rejected_from_full_bone_flags() {
        for flag in [BONE_SCREEN_ALIGN_SPHERE, BONE_SCREEN_ALIGN_CYLINDER] {
            let mut animation = rig();
            animation.full_bone_flags[0] = flag;
            assert_eq!(
                animation.evaluate(0, 0.5, &mut ModelPose::default()),
                Err(ModelAnimationError::Unsupported {
                    feature: "screen-aligned bones"
                })
            );
        }
    }

    #[test]
    fn linear_bone_flags_do_not_override_full_table_sequence_admission() {
        let mut animation = rig();
        animation.bones[0].flags =
            BONE_SCREEN_ALIGN_SPHERE | BONE_SCREEN_ALIGN_CYLINDER | BONE_FIXED_ALIGNMENT;
        animation.sequences[0].bone_weights[0] = 0.25;
        let mut pose = ModelPose::default();
        animation.evaluate(0, 0.5, &mut pose).unwrap();
        assert_eq!(pose.skin_matrices[0][3][0], 1.0);
        animation.full_bone_flags[0] = 0x8000_0400;
        animation.evaluate(0, 0.5, &mut pose).unwrap();
        assert_eq!(pose.skin_matrices[0][3][0], 1.0);
    }

    #[test]
    fn fixed_alignment_admits_end_weights_and_rejects_fractional_blending() {
        for runtime_flag in [0, BONE_FIXED_ALIGNMENT] {
            let mut animation = rig();
            animation.bones[0].flags = runtime_flag;
            animation.full_bone_flags[0] = BONE_FIXED_ALIGNMENT;
            let mut pose = ModelPose::default();
            for (weight, translation) in [(0.0, 0.0), (1.0, 4.0)] {
                animation.sequences[0].bone_weights[0] = weight;
                animation.evaluate(0, 0.5, &mut pose).unwrap();
                assert_eq!(pose.skin_matrices[0][3][0], translation);
            }
            for weight in [0.25, 0.5, 0.75] {
                animation.sequences[0].bone_weights[0] = weight;
                assert_eq!(
                    animation.evaluate(0, 0.5, &mut pose),
                    Err(ModelAnimationError::Unsupported {
                        feature: "fixed-alignment sequence bone blending"
                    })
                );
            }
            animation.clips[0].flags |= crate::mdl::studio::STUDIO_ALLZEROS;
            animation.evaluate(0, 0.5, &mut pose).unwrap();
            assert_eq!(pose.skin_matrices[0], MAT4_IDENTITY);
        }
    }

    #[test]
    fn section_overlap_and_separate_last_frame_are_selected() {
        let mut animation = rig();
        let clip = &mut animation.clips[0];
        clip.frame_count = 5;
        animation.sequences[0].flags = 0;
        clip.section_frames = 2;
        clip.animations.clear();
        clip.sections = [
            vec![[0.0; 3], [1.0; 3], [2.0; 3]],
            vec![[2.0; 3], [3.0; 3], [4.0; 3]],
            vec![[40.0; 3]],
            vec![[4.0; 3]],
        ]
        .into_iter()
        .map(|v| MdlAnimationSection {
            animations: vec![MdlAnimation::positions_for_tests(0, v)],
            external_block: None,
        })
        .collect();
        let mut pose = ModelPose::default();
        for (cycle, expected) in [(0.375, 1.5), (0.5, 2.0), (0.875, 3.5), (1.0, 4.0)] {
            animation.evaluate(0, cycle, &mut pose).unwrap();
            assert_eq!(pose.skin_matrices[0][3][0], expected);
        }
    }
    #[test]
    fn all_zero_clip_suppresses_authored_motion_and_external_payload() {
        let mut animation = rig();
        let mut pose = ModelPose::default();
        animation.evaluate(0, 0.5, &mut pose).unwrap();
        assert_eq!(pose.skin_matrices[0][3][0], 4.0);
        animation.sequences[0].flags |= crate::mdl::studio::STUDIO_ALLZEROS;
        animation.evaluate(0, 0.5, &mut pose).unwrap();
        assert_eq!(pose.skin_matrices[0][3][0], 4.0);
        animation.sequences[0].bone_weights[0] = 0.25;
        animation.evaluate(0, 0.5, &mut pose).unwrap();
        assert_eq!(pose.skin_matrices[0][3][0], 1.0);
        animation.clips[0].flags |= crate::mdl::studio::STUDIO_ALLZEROS;
        for cycle in [0.0, 0.25, 0.5, 1.0] {
            animation.evaluate(0, cycle, &mut pose).unwrap();
            assert_eq!(pose.bone_to_model[0][3][0], 10.0);
            assert_eq!(pose.skin_matrices[0], MAT4_IDENTITY);
        }
        animation.clips[0].external_block = Some(1);
        animation.evaluate(0, 0.5, &mut pose).unwrap();
        assert_eq!(pose.skin_matrices[0], MAT4_IDENTITY);
        animation.clips[0].flags &= !crate::mdl::studio::STUDIO_ALLZEROS;
        assert_eq!(
            animation.evaluate(0, 0.5, &mut pose),
            Err(ModelAnimationError::Unsupported {
                feature: "external ANI data"
            })
        );
    }

    #[test]
    fn looping_endpoint_wraps_but_nonlooping_endpoint_samples_last_frame() {
        let mut animation = rig();
        let mut pose = ModelPose::default();
        animation.evaluate(0, 1.0, &mut pose).unwrap();
        assert_eq!(pose.skin_matrices[0][3][0], 0.0);
        animation.sequences[0].flags = 0;
        animation.evaluate(0, 1.0, &mut pose).unwrap();
        assert_eq!(pose.skin_matrices[0][3][0], 8.0);
    }

    #[test]
    fn hierarchy_does_not_require_parent_first_and_rejects_cycles() {
        let bones = vec![bone(1, 2.0), bone(-1, 3.0)];
        assert_eq!(hierarchy_order(&bones).unwrap(), vec![1, 0]);
        let mut cyclic = bones.clone();
        cyclic[1].parent = 0;
        assert!(hierarchy_order(&cyclic).is_err());
        let mut invalid = bones;
        invalid[0].parent = 5;
        assert!(hierarchy_order(&invalid).is_err());
    }
    #[test]
    fn posed_attachments_and_buffers_are_reused() {
        let mut animation = rig();
        animation.attachments.push(MdlAttachment {
            name: "light".into(),
            flags: 0,
            local_bone: 0,
            local: [
                [1.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
        });
        let mut pose = ModelPose::default();
        animation.evaluate(0, 0.0, &mut pose).unwrap();
        let matrices = pose.skin_matrices.as_ptr();
        let name = pose.attachments[0].source.name.as_ptr();
        animation.evaluate(0, 0.5, &mut pose).unwrap();
        assert_eq!(pose.attachments[0].model[0][3], 15.0);
        assert_eq!(pose.skin_matrices.as_ptr(), matrices);
        assert_eq!(pose.attachments[0].source.name.as_ptr(), name);
    }
    #[test]
    fn unsupported_and_malformed_assets_return_typed_errors() {
        let mut animation = rig();
        let mut pose = ModelPose::default();
        assert_eq!(
            animation.evaluate(0, f32::NAN, &mut pose),
            Err(ModelAnimationError::InvalidCycle)
        );
        assert_eq!(
            animation.duration(1),
            Err(ModelAnimationError::InvalidSequence { index: 1 })
        );
        animation.clips[0].external_block = Some(1);
        assert_eq!(
            animation.duration(0),
            Err(ModelAnimationError::Unsupported {
                feature: "external ANI data"
            })
        );
        animation.clips[0].external_block = None;
        animation.sequences[0].autolayer_count = 1;
        assert_eq!(
            animation.duration(0),
            Err(ModelAnimationError::Unsupported {
                feature: "sequence layers"
            })
        );
        animation.sequences[0].autolayer_count = 0;
        animation.clips[0].fps = 0.0;
        assert_eq!(
            animation.duration(0),
            Err(ModelAnimationError::Malformed {
                part: "clip timing"
            })
        );
    }
    #[test]
    fn quaternion_subframes_use_source_normalized_blend() {
        let a = [1.0, 0.0, 0.0, 0.0];
        let b = [0.0, 0.0, 0.0, 1.0];
        let q = normalized_blend(a, b, 0.25).unwrap();
        assert!((q[3] - 0.31622776).abs() < 1e-6);
        assert!((q[3] - slerp(a, b, 0.25).unwrap()[3]).abs() > 0.05);
        assert_eq!(normalized_blend(a, a.map(|v| -v), 0.5).unwrap(), a);
    }
}
