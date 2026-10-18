use super::{ModelData, ModelLightingContextId};
use gmpublished_domain::{math::Vec3, scene::map::MapReceiverMesh};
use std::sync::Arc;
use vformats::mdl::{ModelAnimation, ModelAnimationError, ModelSkinVertex};

#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnimationBinding {
    pub receiver_mesh: MapReceiverMesh,
    pub vertex_start: u32,
    pub vertices: Arc<[ModelSkinVertex]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelAnimationInstance {
    pub model_path: String,
    pub entity_index: Option<usize>,
    pub hammer_id: Option<u32>,
    pub animation: Arc<ModelAnimation>,
    pub sequence: usize,
    pub playback: ModelAnimationPlayback,
    pub origin: Vec3,
    pub angles: Vec3,
    pub scale: f32,
    pub bindings: Vec<ModelAnimationBinding>,
    pub lighting_context: Option<ModelLightingContextId>,
    pub illumination_attachment_index: Option<usize>,
    pub illumination_position: Vec3,
    pub lighting_origin_override: Option<Vec3>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelAnimationPlayback {
    pub initial_cycle: f32,
    pub playback_rate: f32,
    pub mode: ModelAnimationPlaybackMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelAnimationPlaybackMode {
    Sequence,
    Default,
}

impl Default for ModelAnimationPlayback {
    fn default() -> Self {
        Self {
            initial_cycle: 0.0,
            playback_rate: 1.0,
            mode: ModelAnimationPlaybackMode::Sequence,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ModelAnimationPlaybackError {
    #[error("model animation time, rate or starting cycle is invalid")]
    InvalidTime,
    #[error(transparent)]
    Animation(#[from] ModelAnimationError),
}

impl ModelAnimationPlayback {
    pub fn cycle(
        &self,
        animation: &ModelAnimation,
        sequence: usize,
        time: f32,
    ) -> Result<f32, ModelAnimationPlaybackError> {
        let duration = animation.duration(sequence)?;
        let looping = animation.sequences()[sequence].looping();
        self.cycle_for_duration(duration, looping, time)
    }

    pub fn is_animated(&self, animation: &ModelAnimation, sequence: usize) -> bool {
        self.playback_rate != 0.0
            && animation
                .duration(sequence)
                .is_ok_and(|duration| duration.is_finite() && duration > 0.0)
    }

    fn cycle_for_duration(
        &self,
        duration: f32,
        looping: bool,
        time: f32,
    ) -> Result<f32, ModelAnimationPlaybackError> {
        if !time.is_finite()
            || !duration.is_finite()
            || duration < 0.0
            || !self.initial_cycle.is_finite()
            || !(0.0..=1.0).contains(&self.initial_cycle)
            || !self.playback_rate.is_finite()
        {
            return Err(ModelAnimationPlaybackError::InvalidTime);
        }
        if duration == 0.0 || self.playback_rate == 0.0 {
            return Ok(self.initial_cycle);
        }
        let elapsed = f64::from(time.max(0.0)) * f64::from(self.playback_rate);
        let duration = f64::from(duration);
        let cycle = if looping {
            (f64::from(self.initial_cycle) + elapsed / duration).rem_euclid(1.0)
        } else if self.mode == ModelAnimationPlaybackMode::Default {
            // Native Think times additionally quantize to server ticks.
            const THINK_INTERVAL: f64 = 0.1;
            let restart = ((0.999 * duration / THINK_INTERVAL).ceil() + 1.0) * THINK_INTERVAL;
            ((f64::from(self.initial_cycle) * duration + elapsed).rem_euclid(restart) / duration)
                .clamp(0.0, 1.0)
        } else {
            (f64::from(self.initial_cycle) + elapsed / duration).clamp(0.0, 1.0)
        };
        Ok(cycle as f32)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ModelAnimationPreparationError {
    #[error("authored animation has no decoded skeleton and sequence table")]
    MissingAnimation,
    #[error("authored sequence {name:?} is unavailable")]
    MissingSequence { name: String },
    #[error("random activity selection requires entity animation state")]
    RandomActivity,
    #[error("animated mesh {mesh} has {actual} influences for {expected} vertices")]
    InvalidVertexCount {
        mesh: usize,
        expected: usize,
        actual: usize,
    },
    #[error(transparent)]
    Playback(#[from] ModelAnimationPlaybackError),
    #[error(transparent)]
    Animation(#[from] ModelAnimationError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelAnimationIssue {
    pub model_path: String,
    pub entity_index: Option<usize>,
    pub error: ModelAnimationPreparationError,
}

pub fn animation_local_bounds(model: &ModelData) -> Option<(Vec3, Vec3)> {
    let animation = model.animation.as_ref()?;
    let boxes = std::iter::once((model.bounds_min, model.bounds_max)).chain(
        animation.sequences().iter().map(|sequence| {
            (
                Vec3::from(sequence.bounds_min),
                Vec3::from(sequence.bounds_max),
            )
        }),
    );
    boxes
        .filter(|(min, max)| {
            min.is_finite() && max.is_finite() && (0..3).all(|axis| min[axis] <= max[axis])
        })
        .reduce(|(min, max), (lower, upper)| {
            (
                Vec3::new(
                    min[0].min(lower[0]),
                    min[1].min(lower[1]),
                    min[2].min(lower[2]),
                ),
                Vec3::new(
                    max[0].max(upper[0]),
                    max[1].max(upper[1]),
                    max[2].max(upper[2]),
                ),
            )
        })
}

pub fn model_preview_animations(
    model: &ModelData,
    model_path: &str,
) -> (Vec<ModelAnimationInstance>, Vec<ModelAnimationIssue>) {
    if model.mdl_flags & vformats::mdl::FLAG_STATIC_PROP != 0 || model.animation.is_none() {
        return (Vec::new(), Vec::new());
    }
    let prepare = || {
        let animation = model
            .animation
            .as_ref()
            .ok_or(ModelAnimationPreparationError::MissingAnimation)?;
        let playback = ModelAnimationPlayback::default();
        animation.evaluate(
            0,
            playback.cycle(animation, 0, 0.0)?,
            &mut Default::default(),
        )?;
        let bindings = model
            .meshes
            .iter()
            .enumerate()
            .filter(|(_, mesh)| !mesh.vertices.is_empty() && !mesh.indices.is_empty())
            .map(|(index, mesh)| {
                let vertices = model.skin_vertices.get(index).ok_or(
                    ModelAnimationPreparationError::InvalidVertexCount {
                        mesh: index,
                        expected: mesh.vertices.len(),
                        actual: 0,
                    },
                )?;
                if vertices.len() != mesh.vertices.len() {
                    return Err(ModelAnimationPreparationError::InvalidVertexCount {
                        mesh: index,
                        expected: mesh.vertices.len(),
                        actual: vertices.len(),
                    });
                }
                Ok(ModelAnimationBinding {
                    receiver_mesh: MapReceiverMesh::World(index),
                    vertex_start: 0,
                    vertices: Arc::clone(vertices),
                })
            })
            .collect::<Result<Vec<_>, ModelAnimationPreparationError>>()?;
        Ok::<_, ModelAnimationPreparationError>(ModelAnimationInstance {
            model_path: model_path.to_owned(),
            entity_index: None,
            hammer_id: None,
            animation: Arc::clone(animation),
            sequence: 0,
            playback,
            origin: Vec3::ZERO,
            angles: Vec3::ZERO,
            scale: 1.0,
            bindings,
            lighting_context: None,
            illumination_attachment_index: model.illumination_attachment_index,
            illumination_position: model.illumination_position,
            lighting_origin_override: None,
        })
    };
    match prepare() {
        Ok(instance) => (vec![instance], Vec::new()),
        Err(error) => (
            Vec::new(),
            vec![ModelAnimationIssue {
                model_path: model_path.to_owned(),
                entity_index: None,
                error,
            }],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn animated_model() -> ModelData {
        let vertex = vformats::mdl::ModelVertex {
            position: [1.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            tangent_s: [1.0, 0.0, 0.0],
            tangent_t: [0.0, 1.0, 0.0],
            tangent_sign: 1.0,
            uv: [0.0; 2],
        };
        ModelData {
            animation: Some(gmpublished_domain::scene::model::animation_for_tests()),
            skin_vertices: vec![Arc::from([ModelSkinVertex {
                vertex,
                bones: [0; 3],
                weights: [1.0, 0.0, 0.0],
                bone_count: 1,
            }])],
            mdl_flags: 0,
            vertex_compression: vformats::mdl::VertexCompression::None,
            illumination_position: Vec3::ZERO,
            hull_min: Vec3::ZERO,
            hull_max: Vec3::ZERO,
            view_min: Vec3::ZERO,
            view_max: Vec3::ZERO,
            illumination_attachment_index: Some(0),
            illumination_attachment_position: None,
            lighting: None,
            meshes: vec![super::super::MeshData {
                geometry: super::super::MeshGeometry::Model,
                vertices: vec![super::super::ModelVertex::from(&vertex)],
                indices: vec![0, 0, 0],
                material_index: 0,
                bodygroup: 0,
                bodygroup_choice: 0,
            }],
            material_names: Vec::new(),
            material_dirs: Vec::new(),
            skin_tables: Vec::new(),
            bodygroups: vec![1],
            bounds_min: Vec3::new(-40.0, -1.0, -1.0),
            bounds_max: Vec3::new(1.0, 40.0, 1.0),
            bone_count: 2,
            sequence_count: 1,
            vertex_count: 1,
            triangle_count: 1,
        }
    }

    #[test]
    fn motion_bounds_union_sequence_and_rest_without_header_view_bounds() {
        let mut model = animated_model();
        assert_eq!(
            animation_local_bounds(&model),
            Some((Vec3::new(-40.0, -32.0, -32.0), Vec3::new(32.0, 40.0, 32.0)))
        );
        model.bounds_min[0] = f32::NAN;
        assert_eq!(
            animation_local_bounds(&model),
            Some((Vec3::splat(-32.0), Vec3::splat(32.0)))
        );
        model.bounds_min = Vec3::splat(2.0);
        model.bounds_max = Vec3::splat(1.0);
        assert_eq!(
            animation_local_bounds(&model),
            Some((Vec3::splat(-32.0), Vec3::splat(32.0)))
        );
        model.animation = None;
        assert_eq!(animation_local_bounds(&model), None);
    }

    #[test]
    fn standalone_factory_shares_rig_and_influences_and_rejects_invalid_binding() {
        let mut model = animated_model();
        let (instances, issues) = model_preview_animations(&model, "models/turn.mdl");
        assert!(issues.is_empty());
        assert_eq!(instances.len(), 1);
        assert!(Arc::ptr_eq(
            &instances[0].animation,
            model.animation.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            &instances[0].bindings[0].vertices,
            &model.skin_vertices[0]
        ));
        assert_eq!(
            instances[0].bindings[0].receiver_mesh,
            MapReceiverMesh::World(0)
        );
        assert_eq!(instances[0].illumination_attachment_index, Some(0));
        assert_eq!(
            instances[0].playback.mode,
            ModelAnimationPlaybackMode::Sequence
        );
        model.skin_vertices.clear();
        let (instances, issues) = model_preview_animations(&model, "models/turn.mdl");
        assert!(instances.is_empty());
        assert_eq!(
            issues[0].error,
            ModelAnimationPreparationError::InvalidVertexCount {
                mesh: 0,
                expected: 1,
                actual: 0
            }
        );
    }

    #[test]
    fn standalone_empty_meshes_preserve_later_receiver_identity() {
        let mut model = animated_model();
        let mut empty = model.meshes[0].clone();
        empty.vertices.clear();
        empty.indices.clear();
        model.meshes.insert(0, empty);
        model.skin_vertices.insert(0, Arc::from([]));
        let mut unreferenced = model.meshes[1].clone();
        unreferenced.indices.clear();
        model.meshes.push(unreferenced);
        let (instances, issues) = model_preview_animations(&model, "models/turn.mdl");
        assert!(issues.is_empty());
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].bindings.len(), 1);
        assert_eq!(
            instances[0].bindings[0].receiver_mesh,
            MapReceiverMesh::World(1)
        );
        assert!(Arc::ptr_eq(
            &instances[0].bindings[0].vertices,
            &model.skin_vertices[1]
        ));
    }

    #[test]
    fn default_nonloop_playback_uses_nominal_think_restart_boundary() {
        let playback = ModelAnimationPlayback {
            mode: ModelAnimationPlaybackMode::Default,
            ..Default::default()
        };
        assert_eq!(playback.cycle_for_duration(2.0, false, 2.0), Ok(1.0));
        assert_eq!(playback.cycle_for_duration(2.0, false, 2.09999), Ok(1.0));
        assert!(playback.cycle_for_duration(2.0, false, 2.10001).unwrap() < 1e-5);
        assert!((playback.cycle_for_duration(2.0, false, 2.2).unwrap() - 0.05).abs() < 1e-6);
        assert_eq!(playback.cycle_for_duration(0.0, false, 20.0), Ok(0.0));
    }

    #[test]
    fn loop_uses_duration_with_independent_initial_phase_and_rate() {
        let playback = ModelAnimationPlayback {
            initial_cycle: 0.25,
            playback_rate: 2.0,
            ..Default::default()
        };
        assert_eq!(playback.cycle_for_duration(2.5, true, 0.0), Ok(0.25));
        assert_eq!(playback.cycle_for_duration(2.5, true, 0.625), Ok(0.75));
        assert_eq!(playback.cycle_for_duration(2.5, true, 1.25), Ok(0.25));
        assert_eq!(playback.cycle_for_duration(2.5, true, 1001.25), Ok(0.25));
    }

    #[test]
    fn one_shot_and_paused_playback_hold_their_pose() {
        let mut playback = ModelAnimationPlayback::default();
        assert_eq!(playback.cycle_for_duration(2.0, false, 20.0), Ok(1.0));
        playback.initial_cycle = 0.75;
        playback.playback_rate = 0.0;
        assert_eq!(playback.cycle_for_duration(2.0, true, 20.0), Ok(0.75));
        playback.playback_rate = -1.0;
        assert_eq!(playback.cycle_for_duration(2.0, true, 0.5), Ok(0.5));
        assert_eq!(playback.cycle_for_duration(2.0, false, 20.0), Ok(0.0));
    }

    #[test]
    fn invalid_playback_cannot_produce_nan_palette_keys() {
        let playback = ModelAnimationPlayback::default();
        for time in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                playback.cycle_for_duration(2.0, true, time),
                Err(ModelAnimationPlaybackError::InvalidTime)
            );
        }
    }
}
