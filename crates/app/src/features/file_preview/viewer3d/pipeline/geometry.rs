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
    pub fn brush_origin(
        &self,
        mesh_index: usize,
        owner: gmpublished_domain::scene::map::MapSurfaceOwner,
    ) -> Vec3 {
        self.brush_point(mesh_index, owner, Vec3::ZERO)
    }

    pub fn brush_point(
        &self,
        mesh_index: usize,
        owner: gmpublished_domain::scene::map::MapSurfaceOwner,
        point: Vec3,
    ) -> Vec3 {
        use gmpublished_domain::scene::map::MapSurfaceOwner;
        match owner {
            MapSurfaceOwner::World => point,
            MapSurfaceOwner::Brush { origin, angles, .. } => point.rotate_source(angles) + origin,
            MapSurfaceOwner::Door { door_index, .. } => {
                let door = &self
                    .assets
                    .map()
                    .expect("brush doors belong to a map")
                    .doors[door_index];
                let pose = self.meshes[mesh_index]
                    .deformation
                    .as_ref()
                    .expect("brush doors have viewer poses")
                    .pose;
                super::super::doors::transform_door_point(door, point, pose.progress, pose.swing)
            }
        }
    }

    pub fn brush_bounds(
        &self,
        mesh_index: usize,
        owner: gmpublished_domain::scene::map::MapSurfaceOwner,
        bounds: gmpublished_domain::scene::map::MapBounds,
    ) -> gmpublished_domain::scene::map::MapBounds {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for x in [bounds.min[0], bounds.max[0]] {
            for y in [bounds.min[1], bounds.max[1]] {
                for z in [bounds.min[2], bounds.max[2]] {
                    let point = self.brush_point(mesh_index, owner, Vec3::new(x, y, z));
                    for axis in 0..3 {
                        min[axis] = min[axis].min(point[axis]);
                        max[axis] = max[axis].max(point[axis]);
                    }
                }
            }
        }
        gmpublished_domain::scene::map::MapBounds { min, max }
    }

    pub fn brush_eye(
        &self,
        mesh_index: usize,
        owner: gmpublished_domain::scene::map::MapSurfaceOwner,
        eye: Vec3,
    ) -> Vec3 {
        use gmpublished_domain::scene::map::MapSurfaceOwner;
        match owner {
            MapSurfaceOwner::World => eye,
            MapSurfaceOwner::Door { .. } => self.receiver_eye(mesh_index, eye),
            MapSurfaceOwner::Brush { origin, angles, .. } => {
                let basis = [
                    Vec3::new(1.0, 0.0, 0.0),
                    Vec3::new(0.0, 1.0, 0.0),
                    Vec3::new(0.0, 0.0, 1.0),
                ]
                .map(|axis| axis.rotate_source(angles));
                let delta = eye - origin;
                Vec3::new(
                    delta.dot(basis[0]),
                    delta.dot(basis[1]),
                    delta.dot(basis[2]),
                )
            }
        }
    }

    pub fn receiver_eye(&self, mesh_index: usize, eye: Vec3) -> Vec3 {
        let mesh = &self.meshes[mesh_index];
        let Some(deformation) = &mesh.deformation else {
            return eye;
        };
        let door = &self
            .assets
            .map()
            .expect("door geometry belongs to a map")
            .doors[mesh.door_index.expect("door geometry has an owner")];
        let pose = deformation.pose;
        let origin =
            super::super::doors::transform_door_point(door, Vec3::ZERO, pose.progress, pose.swing);
        let delta = eye - origin;
        let basis = [
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ]
        .map(|normal| {
            super::super::doors::transform_door_normal(door, normal, pose.progress, pose.swing)
        });
        Vec3::new(
            delta.dot(basis[0]),
            delta.dot(basis[1]),
            delta.dot(basis[2]),
        )
    }

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
                let deformation = if let gmpublished_domain::scene::map::MapSurfaceOwner::Door {
                    door_index,
                    ..
                } = source.owner
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
            let gmpublished_domain::scene::map::MapSurfaceOwner::Door { door_index, .. } =
                mark.owner
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
