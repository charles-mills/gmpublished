use super::super::doors::transform_door_point;
use super::{DoorRenderPose, ViewerScene, wgpu};
use gmpublished_domain::scene::map::MapSurfaceOwner;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GpuSurfaceMark {
    origin: [f32; 3],
    fade_min_sq: f32,
    fade_max_sq: f32,
    fade_enabled: u32,
    padding: [u32; 2],
}

#[derive(Debug)]
pub(super) struct Frame {
    pub buffer: wgpu::Buffer,
    data: Vec<GpuSurfaceMark>,
}

impl Frame {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, scene: &ViewerScene) -> Self {
        let data = Self::data(scene, &[]);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("file_preview.surface_marks"),
            size: std::mem::size_of_val(data.as_slice()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&data));
        Self { buffer, data }
    }

    fn data(scene: &ViewerScene, poses: &[DoorRenderPose]) -> Vec<GpuSurfaceMark> {
        let map = scene.assets.source.map();
        std::iter::once(GpuSurfaceMark::default())
            .chain(scene.assets.overlays.iter().map(|mark| {
                let mut origin = mark.fade_origin;
                if let MapSurfaceOwner::Door { door_index } = mark.owner
                    && let Some(door) = map.and_then(|map| map.doors.get(door_index))
                {
                    let pose = poses
                        .get(door_index)
                        .copied()
                        .unwrap_or_else(|| super::geometry::initial_pose(door));
                    origin = transform_door_point(door, origin, pose.progress, pose.swing);
                }
                let fade = mark.fade.filter(|fade| {
                    fade.min_distance_squared >= 0.0
                        && fade.max_distance_squared > fade.min_distance_squared
                });
                GpuSurfaceMark {
                    origin: origin.to_array(),
                    fade_min_sq: fade.map_or(0.0, |fade| fade.min_distance_squared),
                    fade_max_sq: fade.map_or(0.0, |fade| fade.max_distance_squared),
                    fade_enabled: u32::from(fade.is_some()),
                    padding: [0; 2],
                }
            }))
            .collect()
    }

    pub fn update(&mut self, queue: &wgpu::Queue, scene: &ViewerScene, poses: &[DoorRenderPose]) {
        let Some(map) = scene.assets.source.map() else {
            return;
        };
        for (index, mark) in scene.assets.overlays.iter().enumerate() {
            let MapSurfaceOwner::Door { door_index } = mark.owner else {
                continue;
            };
            let door = &map.doors[door_index];
            let pose = poses
                .get(door_index)
                .copied()
                .unwrap_or_else(|| super::geometry::initial_pose(door));
            let origin =
                transform_door_point(door, mark.fade_origin, pose.progress, pose.swing).to_array();
            let data = &mut self.data[index + 1];
            if origin != data.origin {
                data.origin = origin;
                queue.write_buffer(
                    &self.buffer,
                    ((index + 1) * std::mem::size_of::<GpuSurfaceMark>()) as u64,
                    bytemuck::bytes_of(data),
                );
            }
        }
    }
}
