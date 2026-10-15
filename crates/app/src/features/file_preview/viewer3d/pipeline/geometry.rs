use std::ops::Deref;
use std::sync::Arc;

use super::super::doors::transform_door_vertices_into;
use super::upload::{UploadedMesh, UploadedModel, UploadedOverlay, mesh_centroid};
use super::{DoorRenderPose, Vec3, initial_door_swing, wgpu};
use crate::media::preview_model::ModelVertex;

#[derive(Debug)]
pub struct ViewerScene {
    pub assets: Arc<UploadedModel>,
    pub meshes: Vec<ViewerMesh>,
    pub overlays: Vec<ViewerOverlay>,
}

#[derive(Debug)]
pub struct ViewerMesh {
    source: Arc<UploadedMesh>,
    deformation: Option<DoorDeformation>,
}

#[derive(Debug)]
struct DoorDeformation {
    vertices: Option<wgpu::Buffer>,
    centroid: Vec3,
    pose: DoorRenderPose,
    scratch: Vec<ModelVertex>,
}

impl DoorDeformation {
    fn new(door: &super::DoorInstance, centroid: Vec3) -> Self {
        Self {
            vertices: None,
            centroid,
            pose: initial_pose(door),
            scratch: Vec::new(),
        }
    }
    fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        door: &super::DoorInstance,
        local: &[ModelVertex],
        pose: DoorRenderPose,
    ) {
        if self.pose == pose {
            return;
        }
        transform_door_vertices_into(door, local, pose, &mut self.scratch);
        let bytes = bytemuck::cast_slice(&self.scratch);
        let vertices = self.vertices.get_or_insert_with(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.viewer.receiver_vertices"),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        queue.write_buffer(vertices, 0, bytes);
        self.centroid = mesh_centroid(&self.scratch);
        self.pose = pose;
    }
}

impl Deref for ViewerMesh {
    type Target = UploadedMesh;

    fn deref(&self) -> &Self::Target {
        &self.source
    }
}

impl ViewerMesh {
    pub fn vertices(&self) -> &wgpu::Buffer {
        self.deformation
            .as_ref()
            .and_then(|door| door.vertices.as_ref())
            .unwrap_or_else(|| self.source.vertices())
    }

    pub fn centroid(&self) -> Vec3 {
        self.deformation
            .as_ref()
            .map_or_else(|| self.source.centroid(), |door| door.centroid)
    }
}

#[derive(Debug)]
pub struct ViewerOverlay {
    source: Arc<UploadedOverlay>,
    deformation: Option<DoorDeformation>,
}
impl Deref for ViewerOverlay {
    type Target = UploadedOverlay;
    fn deref(&self) -> &Self::Target {
        &self.source
    }
}
impl ViewerOverlay {
    pub fn vertices(&self) -> &wgpu::Buffer {
        self.deformation
            .as_ref()
            .and_then(|door| door.vertices.as_ref())
            .unwrap_or_else(|| self.source.vertices())
    }
}

impl ViewerScene {
    pub fn new(assets: Arc<UploadedModel>) -> Self {
        let map = assets.source.map();
        let meshes = assets
            .meshes
            .iter()
            .map(|source| {
                let deformation = source
                    .door_index
                    .and_then(|index| map.and_then(|map| map.doors.get(index)))
                    .zip(source.local_vertices.as_ref())
                    .map(|(door, _)| DoorDeformation::new(door, source.centroid()));
                ViewerMesh {
                    source: Arc::clone(source),
                    deformation,
                }
            })
            .collect();
        let overlays = assets
            .overlays
            .iter()
            .map(|source| {
                let deformation =
                    if let gmpublished_domain::scene::map::MapSurfaceOwner::Door { door_index } =
                        source.owner
                    {
                        map.and_then(|map| map.doors.get(door_index))
                            .map(|door| DoorDeformation::new(door, source.centroid()))
                    } else {
                        None
                    };
                ViewerOverlay {
                    source: Arc::clone(source),
                    deformation,
                }
            })
            .collect();
        Self {
            assets,
            meshes,
            overlays,
        }
    }

    pub fn update_doors(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        poses: &[DoorRenderPose],
    ) {
        let Some(map) = self.assets.source.map() else {
            return;
        };
        for mesh in &mut self.meshes {
            let Some(index) = mesh.source.door_index else {
                continue;
            };
            let Some(deformation) = &mut mesh.deformation else {
                continue;
            };
            let door = &map.doors[index];
            let pose = poses.get(index).copied().unwrap_or_else(|| DoorRenderPose {
                progress: door.initial_progress.clamp(0.0, 1.0),
                swing: initial_door_swing(door.motion),
            });
            deformation.update(
                device,
                queue,
                door,
                mesh.source.local_vertices.as_deref().unwrap(),
                pose,
            );
        }
        for mark in &mut self.overlays {
            let gmpublished_domain::scene::map::MapSurfaceOwner::Door { door_index } = mark.owner
            else {
                continue;
            };
            let Some(deformation) = &mut mark.deformation else {
                continue;
            };
            let door = &map.doors[door_index];
            let pose = poses
                .get(door_index)
                .copied()
                .unwrap_or_else(|| initial_pose(door));
            deformation.update(
                device,
                queue,
                door,
                mark.source.local_vertices.as_deref().unwrap(),
                pose,
            );
        }
    }
}

pub(super) fn initial_pose(door: &super::DoorInstance) -> DoorRenderPose {
    DoorRenderPose {
        progress: door.initial_progress.clamp(0.0, 1.0),
        swing: initial_door_swing(door.motion),
    }
}
