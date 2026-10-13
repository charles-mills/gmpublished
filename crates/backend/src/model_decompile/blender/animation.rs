use std::sync::atomic::AtomicBool;

use mdlc::bone_math;
use serde_json::json;

use super::super::{LoadedModel, animations};
use super::{DecompileError, Document, check_cancelled, position, rotation};

pub(super) fn write(
    output: &mut Document,
    loaded: &LoadedModel,
    cancelled: &AtomicBool,
) -> Result<(), DecompileError> {
    if loaded.model.bones.is_empty() {
        return Ok(());
    }
    let (clips, warnings) = animations::read_for_blender(
        &loaded.raw_mdl,
        loaded.ani.as_deref(),
        &loaded.model,
        cancelled,
    )?;
    output.warnings.extend(warnings);
    let mut has_delta = false;
    for clip in clips {
        check_cancelled(cancelled)?;
        if clip.frames.is_empty() {
            continue;
        }
        let times = (0..clip.frames.len())
            .map(|frame| [frame as f32 / clip.fps])
            .collect::<Vec<_>>();
        let input = output.floats(&times, "SCALAR", true, None)?;
        let mut samplers = Vec::new();
        let mut channels = Vec::new();
        let delta = clip.flags & 4 != 0;
        has_delta |= delta;
        for (index, bone) in loaded.model.bones.iter().enumerate() {
            check_cancelled(cancelled)?;
            let mut translations = Vec::with_capacity(clip.frames.len());
            let mut rotations = Vec::with_capacity(clip.frames.len());
            for frame in &clip.frames {
                let pose = frame
                    .get(index)
                    .filter(|pose| pose.bone == index as i32)
                    .ok_or_else(|| super::invalid("An animation is missing a bone pose"))?;
                let mut translation = pose.position;
                let mut quaternion = bone_math::angle_quaternion(pose.rotation);
                if delta {
                    translation =
                        std::array::from_fn(|axis| bone.position[axis] + translation[axis]);
                    quaternion = if clip.flags & 0x10 != 0 {
                        bone_math::quaternion_mult(bone.quaternion, quaternion)
                    } else {
                        bone_math::quaternion_mult(quaternion, bone.quaternion)
                    };
                }
                translations.push(position(translation));
                let mut quaternion = rotation(bone_math::quaternion_normalize(quaternion));
                if let Some(previous) = rotations.last()
                    && quaternion
                        .iter()
                        .zip(previous)
                        .map(|(a, b)| a * b)
                        .sum::<f32>()
                        < 0.0
                {
                    quaternion = quaternion.map(|value| -value);
                }
                rotations.push(quaternion);
            }
            let translation = output.floats(&translations, "VEC3", false, None)?;
            let quaternion = output.floats(&rotations, "VEC4", false, None)?;
            for (path, accessor) in [("translation", translation), ("rotation", quaternion)] {
                let sampler = samplers.len();
                samplers.push(json!({"input":input,"output":accessor,"interpolation":"LINEAR"}));
                channels.push(json!({"sampler":sampler,"target":{"node":index+1,"path":path}}));
            }
        }
        output.animations.push(json!({"name":clip.name,"samplers":samplers,"channels":channels,"extras":{"sourceAnimationFlags":clip.flags,"sourceFPS":clip.fps,"sourceDeltaBakedOnBindPose":delta}}));
    }
    if has_delta {
        output.warnings.push("Additive animation clips are baked onto the bind pose. Source sequence layering and pose parameters are not Blender animation constraints.".into());
    }
    Ok(())
}
