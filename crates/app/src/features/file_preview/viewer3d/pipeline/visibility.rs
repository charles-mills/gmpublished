use super::MapPreview;
use super::geometry::ViewerScene;
use super::{WorldVisibilityPlan, wgpu};
use gmpublished_domain::math::Vec3;
use std::collections::HashMap;

#[derive(Debug)]
pub struct UploadedVisibleIndices {
    pub buffer: Option<wgpu::Buffer>,
    pub index_count: u32,
}

#[derive(Debug, Default)]
pub struct UploadedVisibility {
    pub tracker: VisibilityClusterTracker,
    pub plan: Option<WorldVisibilityPlan>,
    pub mesh_indices: HashMap<usize, UploadedVisibleIndices>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VisibilityClusterState {
    #[default]
    Disabled,
    StandDown,
    Cluster(i16),
}

#[derive(Debug, Default)]
pub struct VisibilityClusterTracker {
    pub last_camera_position: Option<[u32; 3]>,
    pub state: VisibilityClusterState,
    pub rebuild_count: u64,
}

impl VisibilityClusterTracker {
    pub fn update(
        &mut self,
        enabled: bool,
        camera_position: Vec3,
        mut cluster_at: impl FnMut(Vec3) -> Option<i16>,
    ) -> Option<VisibilityClusterState> {
        if !enabled {
            self.last_camera_position = None;
            return self.set_state(VisibilityClusterState::Disabled);
        }

        let position_key = camera_position.to_array().map(f32::to_bits);
        if self.last_camera_position == Some(position_key) {
            return None;
        }
        self.last_camera_position = Some(position_key);
        let state = cluster_at(camera_position).map_or(
            VisibilityClusterState::StandDown,
            VisibilityClusterState::Cluster,
        );
        self.set_state(state)
    }

    pub fn set_state(&mut self, state: VisibilityClusterState) -> Option<VisibilityClusterState> {
        if self.state == state {
            return None;
        }
        self.state = state;
        self.rebuild_count = self.rebuild_count.saturating_add(1);
        Some(state)
    }
}

pub fn upload_visible_indices(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    indices: &[u32],
) -> UploadedVisibleIndices {
    let index_count = u32::try_from(indices.len()).unwrap_or(u32::MAX);
    if indices.is_empty() {
        return UploadedVisibleIndices {
            buffer: None,
            index_count,
        };
    }
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("file_preview.model_viewer.visible_indices"),
        size: std::mem::size_of_val(indices) as u64,
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, bytemuck::cast_slice(indices));
    UploadedVisibleIndices {
        buffer: Some(buffer),
        index_count,
    }
}

impl UploadedVisibility {
    pub fn ensure_world_visibility(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &MapPreview,
        upload: &ViewerScene,
        enabled: bool,
        camera_position: Vec3,
    ) {
        let Some(visibility) = scene.visibility.as_ref() else {
            if self
                .tracker
                .set_state(VisibilityClusterState::Disabled)
                .is_some()
            {
                self.apply_world_visibility_plan(device, queue, upload, None);
            }
            return;
        };
        let Some(state) = self.tracker.update(enabled, camera_position, |point| {
            visibility.cluster_at(point)
        }) else {
            return;
        };

        match state {
            VisibilityClusterState::Disabled | VisibilityClusterState::StandDown => {
                self.apply_world_visibility_plan(device, queue, upload, None);
            }
            VisibilityClusterState::Cluster(cluster) => {
                if let Some(visible_clusters) = visibility.visible_clusters(cluster) {
                    let plan = WorldVisibilityPlan::from_visible_clusters(scene, &visible_clusters);
                    log::debug!(
                        "map preview visibility rebuild cluster {cluster}: {} visible clusters",
                        plan.visible_cluster_count
                    );
                    self.apply_world_visibility_plan(device, queue, upload, Some(plan));
                } else {
                    self.apply_world_visibility_plan(device, queue, upload, None);
                }
            }
        }
    }

    pub fn apply_world_visibility_plan(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        upload: &ViewerScene,
        plan: Option<WorldVisibilityPlan>,
    ) {
        self.mesh_indices.clear();
        if let Some(plan) = plan.as_ref() {
            for (index, mesh) in upload.meshes.iter().enumerate() {
                if mesh.map_skybox || mesh.door_index.is_some() {
                    continue;
                }
                let visible = plan
                    .mesh_indices
                    .get(mesh.scene_mesh_index)
                    .map_or(&[][..], Vec::as_slice);
                self.mesh_indices
                    .insert(index, upload_visible_indices(device, queue, visible));
            }
        }
        self.plan = plan;
    }
}

#[cfg(test)]
mod tests {
    use super::{Vec3, VisibilityClusterState, VisibilityClusterTracker};

    #[test]
    fn visibility_tracker_rebuilds_only_when_cluster_changes() {
        let mut tracker = VisibilityClusterTracker::default();
        let mut probes = 0_u32;

        assert_eq!(
            tracker.update(true, Vec3::new(0.0, 0.0, 0.0), |_| {
                probes = probes.saturating_add(1);
                Some(3)
            }),
            Some(VisibilityClusterState::Cluster(3))
        );
        assert_eq!(tracker.rebuild_count, 1);
        assert_eq!(
            tracker.update(true, Vec3::new(1.0, 0.0, 0.0), |_| {
                probes = probes.saturating_add(1);
                Some(3)
            }),
            None
        );
        assert_eq!(tracker.rebuild_count, 1);
        assert_eq!(
            tracker.update(true, Vec3::new(1.0, 0.0, 0.0), |_| {
                panic!("leaf lookup should not run without movement")
            }),
            None
        );
        assert_eq!(
            tracker.update(true, Vec3::new(2.0, 0.0, 0.0), |_| {
                probes = probes.saturating_add(1);
                Some(4)
            }),
            Some(VisibilityClusterState::Cluster(4))
        );
        assert_eq!(tracker.rebuild_count, 2);
        assert_eq!(probes, 3);
    }
}
