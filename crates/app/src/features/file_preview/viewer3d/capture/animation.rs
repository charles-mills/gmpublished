use crate::features::file_preview::viewer3d::pipeline::{
    ModelAnimationPhase, ModelAnimationPhaseOverrides,
};
use crate::media::preview_model::{MapPreview, ModelAnimationInstance};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Override {
    pub entity_hammer_id: String,
    pub model: String,
    pub origin: [f32; 3],
    pub sequence: Option<String>,
    pub cycle: f32,
    #[serde(default)]
    pub playback_rate: f32,
}

pub(super) fn resolve(
    scene: &MapPreview,
    overrides: &[Override],
) -> Result<Option<Arc<ModelAnimationPhaseOverrides>>, String> {
    resolve_instances(&scene.model_animations, overrides)
}

fn resolve_instances(
    instances: &[ModelAnimationInstance],
    overrides: &[Override],
) -> Result<Option<Arc<ModelAnimationPhaseOverrides>>, String> {
    if overrides.is_empty() {
        return Ok(None);
    }
    let mut result = ModelAnimationPhaseOverrides::new();
    for control in overrides {
        if !control.cycle.is_finite()
            || !(0.0..=1.0).contains(&control.cycle)
            || control.playback_rate != 0.0
            || !control.origin.iter().all(|value| value.is_finite())
        {
            return Err(format!("invalid fixed animation phase: {control:?}"));
        }
        let hammer_id: u32 = control
            .entity_hammer_id
            .parse()
            .map_err(|_| format!("invalid animation Hammer ID: {control:?}"))?;
        let model = control.model.replace('\\', "/");
        let mut matches = instances.iter().enumerate().filter(|(_, instance)| {
            instance.hammer_id == Some(hammer_id)
                && instance.model_path.eq_ignore_ascii_case(&model)
                && instance.origin.to_array() == control.origin
        });
        let (index, instance) = matches
            .next()
            .ok_or_else(|| format!("animation control has no matching instance: {control:?}"))?;
        if matches.next().is_some() {
            return Err(format!("animation control is ambiguous: {control:?}"));
        }
        let sequence = control
            .sequence
            .as_ref()
            .map(|name| {
                instance.animation.sequence_index(name).ok_or_else(|| {
                    format!(
                        "animation sequence {name:?} is unavailable for {}",
                        control.model
                    )
                })
            })
            .transpose()?;
        instance
            .animation
            .evaluate(
                sequence.unwrap_or(instance.sequence),
                control.cycle,
                &mut Default::default(),
            )
            .map_err(|error| error.to_string())?;
        if result
            .insert(
                index,
                ModelAnimationPhase {
                    sequence,
                    cycle: control.cycle,
                },
            )
            .is_some()
        {
            return Err(format!(
                "animation instance is controlled more than once: {control:?}"
            ));
        }
    }
    Ok(Some(Arc::new(result)))
}

pub(super) fn describe(scene: &MapPreview) -> serde_json::Value {
    use serde_json::json;
    json!({
        "instances": scene.model_animations.iter().enumerate().map(|(index, instance)| json!({
            "index": index,
            "model": instance.model_path,
            "entity_index": instance.entity_index,
            "hammer_id": instance.hammer_id,
            "origin": instance.origin.to_array(),
            "angles": instance.angles.to_array(),
            "scale": instance.scale,
            "sequence": instance.sequence,
            "sequence_name": instance.animation.sequences()[instance.sequence].label,
            "duration": instance.animation.duration(instance.sequence).ok(),
            "looping": instance.animation.sequences()[instance.sequence].looping(),
            "playback": format!("{:?}", instance.playback),
            "bone_count": instance.animation.bone_count(),
            "meshes": instance.bindings.iter().map(|binding| json!({
                "receiver": format!("{:?}", binding.receiver_mesh),
                "vertex_start": binding.vertex_start,
                "vertex_count": binding.vertices.len(),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "issues": scene.model_animation_issues.iter().map(|issue| json!({
            "model": issue.model_path,
            "entity_index": issue.entity_index,
            "error": issue.error.to_string(),
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::preview_model::ModelAnimationPlayback;
    use gmpublished_domain::math::Vec3;

    fn instance() -> ModelAnimationInstance {
        ModelAnimationInstance {
            model_path: "models/control/actor.mdl".into(),
            entity_index: Some(42),
            hammer_id: Some(123),
            animation: gmpublished_domain::scene::model::animation_for_tests(),
            sequence: 0,
            playback: ModelAnimationPlayback::default(),
            origin: Vec3::new(1.0, 2.0, 3.0),
            angles: Vec3::ZERO,
            scale: 1.0,
            bindings: Vec::new(),
            lighting_context: None,
            illumination_attachment_index: None,
            illumination_position: Vec3::ZERO,
            lighting_origin_override: None,
        }
    }

    fn control(instance: &ModelAnimationInstance) -> Override {
        Override {
            entity_hammer_id: instance.hammer_id.unwrap().to_string(),
            model: instance.model_path.to_uppercase().replace('/', "\\"),
            origin: instance.origin.to_array(),
            sequence: Some(instance.animation.sequences()[0].label.to_uppercase()),
            cycle: 0.75,
            playback_rate: 0.0,
        }
    }

    #[test]
    fn fixed_phase_requires_complete_unambiguous_source_identity() {
        let first = instance();
        let mut second = first.clone();
        second.hammer_id = Some(124);
        let control = control(&first);
        let resolved = resolve_instances(&[first.clone(), second], std::slice::from_ref(&control))
            .unwrap()
            .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[&0].sequence, Some(0));
        assert_eq!(resolved[&0].cycle, 0.75);
        assert!(resolve_instances(&[], std::slice::from_ref(&control)).is_err());
        assert!(
            resolve_instances(
                &[first.clone(), first.clone()],
                std::slice::from_ref(&control)
            )
            .is_err()
        );
        assert!(resolve_instances(&[first], &[control.clone(), control]).is_err());
    }

    #[test]
    fn fixed_phase_rejects_invalid_pose_and_nonfixed_playback() {
        let instance = instance();
        let valid = control(&instance);
        for cycle in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
            let mut invalid = valid.clone();
            invalid.cycle = cycle;
            assert!(resolve_instances(std::slice::from_ref(&instance), &[invalid]).is_err());
        }
        let mut invalid = valid.clone();
        invalid.playback_rate = 1.0;
        assert!(resolve_instances(std::slice::from_ref(&instance), &[invalid]).is_err());
        let mut invalid = valid;
        invalid.sequence = Some("missing".into());
        assert!(resolve_instances(&[instance], &[invalid]).is_err());
    }
}
