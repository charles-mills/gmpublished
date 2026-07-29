//! Cluster-visibility state for an uploaded map: the tracker that decides
//! when the camera's visibility cluster changed, and the visible-subset
//! index and vertex buffers rebuilt when it does.

use super::upload::UploadedDetailSprites;
use super::{DETAIL_VERTEX_FLOAT_COUNT, WorldVisibilityPlan, wgpu};

#[derive(Debug)]
pub struct UploadedVisibleIndices {
    pub buffer: Option<wgpu::Buffer>,
    pub index_count: u32,
}

#[derive(Debug)]
pub struct UploadedVisibleVertices {
    pub buffer: Option<wgpu::Buffer>,
    pub vertex_count: u32,
}

#[derive(Debug, Default)]
pub struct UploadedVisibility {
    pub tracker: VisibilityClusterTracker,
    pub plan: Option<WorldVisibilityPlan>,
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
        camera_position: [f32; 3],
        mut cluster_at: impl FnMut([f32; 3]) -> Option<i16>,
    ) -> Option<VisibilityClusterState> {
        if !enabled {
            self.last_camera_position = None;
            return self.set_state(VisibilityClusterState::Disabled);
        }

        let position_key = camera_position.map(f32::to_bits);
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

pub fn upload_visible_detail_sprites(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    sprites: &UploadedDetailSprites,
    plan: &WorldVisibilityPlan,
) -> UploadedVisibleVertices {
    let sprite_bytes = sprites.all_vertices.len() / sprites.sprite_count.max(1);
    let mut bytes = Vec::with_capacity(sprites.all_vertices.len());
    for (sprite_index, visible) in plan.detail_sprite_visible.iter().copied().enumerate() {
        if !visible {
            continue;
        }
        let start = sprite_index.saturating_mul(sprite_bytes);
        let end = start.saturating_add(sprite_bytes);
        if let Some(slice) = sprites.all_vertices.get(start..end) {
            bytes.extend_from_slice(slice);
        }
    }
    let vertex_count = u32::try_from(
        bytes.len() / (DETAIL_VERTEX_FLOAT_COUNT as usize * std::mem::size_of::<f32>()),
    )
    .unwrap_or(u32::MAX);
    if bytes.is_empty() {
        return UploadedVisibleVertices {
            buffer: None,
            vertex_count,
        };
    }
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("file_preview.model_viewer.visible_detail_vertices"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, &bytes);
    UploadedVisibleVertices {
        buffer: Some(buffer),
        vertex_count,
    }
}
