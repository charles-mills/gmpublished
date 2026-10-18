use std::sync::{Arc, Mutex};

use gmpublished_backend::model_decompile::{AnimatedVertex, PreviewPose};

use super::{ModelPreview, Vec3};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelBounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl ModelBounds {
    fn from_positions(positions: impl Iterator<Item = Option<Vec3>>) -> Option<Option<Self>> {
        let mut bounds = None;
        for position in positions {
            let position = position?;
            if !position.is_finite() {
                return None;
            }
            let current = bounds.get_or_insert(Self {
                min: position,
                max: position,
            });
            for axis in 0..3 {
                current.min[axis] = current.min[axis].min(position[axis]);
                current.max[axis] = current.max[axis].max(position[axis]);
            }
        }
        Some(bounds)
    }

    pub(crate) fn center(self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    fn include(&mut self, other: Self) {
        for axis in 0..3 {
            self.min[axis] = self.min[axis].min(other.min[axis]);
            self.max[axis] = self.max[axis].max(other.max[axis]);
        }
    }
}

#[derive(Debug)]
pub struct EvaluatedGeometry {
    pub vertices: Option<Vec<Vec<AnimatedVertex>>>,
    mesh_bounds: Vec<Option<ModelBounds>>,
}

type CacheEntry = ((usize, usize, Option<PreviewPose>), Arc<EvaluatedGeometry>);

#[derive(Debug, Default)]
pub struct PoseCache(Mutex<Option<CacheEntry>>);

impl ModelPreview {
    pub(crate) fn evaluated_geometry(
        &self,
        pose: Option<&PreviewPose>,
    ) -> Option<Arc<EvaluatedGeometry>> {
        let key = (
            Arc::as_ptr(&self.scene).addr(),
            self.selected_lod,
            pose.cloned(),
        );
        let mut cached = self
            .pose_cache
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((previous, geometry)) = cached.as_ref()
            && *previous == key
        {
            return Some(Arc::clone(geometry));
        }
        let vertices = match pose {
            Some(pose) => Some(self.animation.as_ref()?.as_ref().ok()?.evaluate_lod(
                pose.sequence,
                pose.cycle,
                &pose.parameters,
                self.selected_lod,
            )?),
            None => None,
        };
        let mesh_bounds = self
            .scene
            .meshes
            .iter()
            .enumerate()
            .map(|(index, mesh)| {
                if let Some(vertices) = &vertices {
                    let vertices = vertices.get(index)?;
                    ModelBounds::from_positions(mesh.indices.iter().map(|index| {
                        vertices
                            .get(*index as usize)
                            .map(|vertex| Vec3::from(vertex.position))
                    }))
                } else {
                    ModelBounds::from_positions(mesh.indices.iter().map(|index| {
                        mesh.vertices
                            .get(*index as usize)
                            .map(|vertex| vertex.position)
                    }))
                }
            })
            .collect::<Option<Vec<_>>>()?;
        let geometry = Arc::new(EvaluatedGeometry {
            vertices,
            mesh_bounds,
        });
        *cached = Some((key, Arc::clone(&geometry)));
        drop(cached);
        Some(geometry)
    }

    pub(crate) fn visible_bounds(
        &self,
        pose: Option<&PreviewPose>,
        choices: &[usize],
    ) -> Option<ModelBounds> {
        let geometry = self.evaluated_geometry(pose)?;
        let mut bounds = None;
        for (mesh, mesh_bounds) in self.scene.meshes.iter().zip(&geometry.mesh_bounds) {
            if mesh.bodygroup_choice != choices.get(mesh.bodygroup).copied().unwrap_or(0) {
                continue;
            }
            if let Some(mesh_bounds) = *mesh_bounds {
                match &mut bounds {
                    Some(bounds) => ModelBounds::include(bounds, mesh_bounds),
                    None => bounds = Some(mesh_bounds),
                }
            }
        }
        bounds
    }
}
