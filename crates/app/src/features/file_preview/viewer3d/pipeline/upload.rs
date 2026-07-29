//! Per-preview GPU uploads keyed by content id: vertex, index, sprite,
//! overlay, and skybox buffers plus material bind groups, and the cache
//! that keeps an upload alive only while its preview still draws.

use std::collections::HashMap;

use super::materials::{MaterialTextureViews, MaterialUploadMode, WHITE_RGBA};
use super::resources::RenderResources;
use super::uniforms::scene_sky_tint;
use super::visibility::{
    UploadedVisibility, UploadedVisibleIndices, UploadedVisibleVertices, VisibilityClusterState,
    upload_visible_detail_sprites, upload_visible_indices,
};
use super::{
    DETAIL_VERTEX_FLOAT_COUNT, DetailSprite, DoorInstance, DoorRenderPose, MapVisibilityBucket,
    MeshData, ModelPreview, ModelVertex, OverlayPrimitive, RenderMode, ResolvedTexture,
    SKYBOX_FACE_COUNT, WorldVisibilityPlan, initial_door_swing, transform_door_vertices, wgpu,
};

/// Uploads keyed by preview content id, kept only while their preview still
/// draws.
#[derive(Debug, Default)]
pub struct UploadCache {
    uploads: HashMap<u64, UploadedModel>,
    /// Content ids drawn since the last [`Self::trim`].
    live: Vec<u64>,
}

impl UploadCache {
    pub fn get(&self, content_id: u64) -> Option<&UploadedModel> {
        self.uploads.get(&content_id)
    }

    pub fn get_mut(&mut self, content_id: u64) -> Option<&mut UploadedModel> {
        self.uploads.get_mut(&content_id)
    }

    pub fn touch(&mut self, content_id: u64) {
        if !self.live.contains(&content_id) {
            self.live.push(content_id);
        }
    }

    pub fn trim(&mut self) {
        // Keep only uploads drawn since the last trim; a closed/replaced
        // preview drops its GPU buffers on the next frame.
        let live = std::mem::take(&mut self.live);
        self.uploads.retain(|id, _| live.contains(id));
    }

    pub fn ensure_upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        content_id: u64,
        model: &ModelPreview,
    ) {
        if self.uploads.contains_key(&content_id) {
            return;
        }

        let (lightmap_rgba, lightmap_width, lightmap_height) =
            model
                .lightmap
                .as_ref()
                .map_or((WHITE_RGBA.as_slice(), 1, 1), |lightmap| {
                    (
                        lightmap.rgba.as_slice(),
                        lightmap.width.max(1),
                        lightmap.height.max(1),
                    )
                });
        let lightmap_view = resources.create_texture_view(
            device,
            queue,
            "file_preview.model_viewer.lightmap",
            lightmap_rgba,
            lightmap_width,
            lightmap_height,
        );

        let material_uploads = model
            .materials
            .iter()
            .map(|slot| {
                let base_view = resources.create_slot_texture_view(
                    device,
                    queue,
                    "file_preview.model_viewer.texture",
                    slot,
                    slot.texture.as_deref(),
                );
                let base2_view = resources.create_slot_texture_view(
                    device,
                    queue,
                    "file_preview.model_viewer.texture2",
                    slot,
                    slot.texture2.as_deref().or(slot.texture.as_deref()),
                );
                resources.create_material(
                    device,
                    queue,
                    MaterialTextureViews {
                        base: &base_view,
                        base2: &base2_view,
                        lightmap: &lightmap_view,
                    },
                    MaterialUploadMode {
                        force_opaque: slot.force_opaque,
                        render_mode: slot.render_mode,
                    },
                )
            })
            .collect::<Vec<_>>();
        let material_render_modes = if model.materials.is_empty() {
            vec![RenderMode::Opaque]
        } else {
            model
                .materials
                .iter()
                .map(|slot| slot.render_mode)
                .collect::<Vec<_>>()
        };
        let material_water_fallbacks = if model.materials.is_empty() {
            vec![false]
        } else {
            model
                .materials
                .iter()
                .map(|slot| {
                    slot.texture
                        .as_deref()
                        .is_some_and(ResolvedTexture::is_water_fallback)
                })
                .collect::<Vec<_>>()
        };
        let (material_bind_groups, material_uniforms): (Vec<_>, Vec<_>) =
            if material_uploads.is_empty() {
                let base_view = resources.create_material_texture_view(
                    device,
                    queue,
                    "file_preview.model_viewer.texture",
                    None,
                );
                let (bind_group, uniform) = resources.create_material(
                    device,
                    queue,
                    MaterialTextureViews {
                        base: &base_view,
                        base2: &base_view,
                        lightmap: &lightmap_view,
                    },
                    MaterialUploadMode {
                        force_opaque: true,
                        render_mode: RenderMode::Opaque,
                    },
                );
                (vec![bind_group], vec![uniform])
            } else {
                material_uploads.into_iter().unzip()
            };

        let mut meshes = upload_meshes(device, queue, model.meshes.as_slice(), false);
        meshes.extend(upload_meshes(
            device,
            queue,
            model.map_skybox_meshes.as_slice(),
            true,
        ));
        meshes.extend(upload_door_meshes(device, queue, model.doors.as_slice()));
        let detail_sprites = upload_detail_sprites(device, queue, model.detail_sprites.as_slice());
        let map_skybox_detail_sprites =
            upload_detail_sprites(device, queue, model.map_skybox_detail_sprites.as_slice());
        let mut overlays = upload_overlays(device, queue, model.overlays.as_slice(), false);
        overlays.extend(upload_overlays(
            device,
            queue,
            model.map_skybox_overlays.as_slice(),
            true,
        ));
        let sky_tint = scene_sky_tint(model.skybox.as_ref());
        let skybox = model.skybox.as_ref().map(|skybox| {
            let face_bind_groups = std::array::from_fn(|index| {
                skybox.faces[index].as_ref().map(|texture| {
                    let view = resources.create_texture_view(
                        device,
                        queue,
                        "file_preview.model_viewer.sky_texture",
                        texture.rgba_bytes().unwrap_or(WHITE_RGBA.as_slice()),
                        texture.width.max(1),
                        texture.height.max(1),
                    );
                    resources.create_sky_material(device, &view)
                })
            });
            UploadedSkybox { face_bind_groups }
        });

        self.uploads.insert(
            content_id,
            UploadedModel {
                meshes,
                detail_sprites,
                map_skybox_detail_sprites,
                overlays,
                phy_debug_meshes: None,
                material_bind_groups,
                material_render_modes,
                material_water_fallbacks,
                _material_uniforms: material_uniforms,
                skybox,
                sky_tint,
                visibility: UploadedVisibility::default(),
            },
        );
    }
}

#[derive(Debug)]
pub struct UploadedModel {
    pub meshes: Vec<UploadedMesh>,
    pub detail_sprites: Option<UploadedDetailSprites>,
    pub map_skybox_detail_sprites: Option<UploadedDetailSprites>,
    pub overlays: Vec<UploadedOverlay>,
    pub phy_debug_meshes: Option<Vec<UploadedMesh>>,
    pub material_bind_groups: Vec<wgpu::BindGroup>,
    pub material_render_modes: Vec<RenderMode>,
    pub material_water_fallbacks: Vec<bool>,
    pub _material_uniforms: Vec<wgpu::Buffer>,
    pub skybox: Option<UploadedSkybox>,
    pub sky_tint: [f32; 3],
    pub visibility: UploadedVisibility,
}

impl UploadedModel {
    pub fn has_map_skybox_content(&self) -> bool {
        self.meshes.iter().any(|mesh| mesh.map_skybox)
            || self.map_skybox_detail_sprites.is_some()
            || self.overlays.iter().any(|overlay| overlay.map_skybox)
    }

    pub fn ensure_phy_debug_meshes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &ModelPreview,
    ) {
        if self.phy_debug_meshes.is_some() {
            return;
        }
        self.phy_debug_meshes = Some(upload_meshes(
            device,
            queue,
            scene.phy_debug_meshes.as_slice(),
            false,
        ));
    }

    pub fn ensure_world_visibility(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &ModelPreview,
        enabled: bool,
        camera_position: [f32; 3],
    ) {
        let Some(visibility) = scene.visibility.as_ref() else {
            if self
                .visibility
                .tracker
                .set_state(VisibilityClusterState::Disabled)
                .is_some()
            {
                self.apply_world_visibility_plan(device, queue, None);
            }
            return;
        };
        let Some(state) = self
            .visibility
            .tracker
            .update(enabled, camera_position, |point| {
                visibility.cluster_at(point)
            })
        else {
            return;
        };

        match state {
            VisibilityClusterState::Disabled | VisibilityClusterState::StandDown => {
                self.apply_world_visibility_plan(device, queue, None);
            }
            VisibilityClusterState::Cluster(cluster) => {
                if let Some(visible_clusters) = visibility.visible_clusters(cluster) {
                    let plan = WorldVisibilityPlan::from_visible_clusters(scene, &visible_clusters);
                    log::debug!(
                        "map preview visibility rebuild cluster {cluster}: {} visible clusters",
                        plan.visible_cluster_count
                    );
                    self.apply_world_visibility_plan(device, queue, Some(plan));
                } else {
                    self.apply_world_visibility_plan(device, queue, None);
                }
            }
        }
    }

    pub fn apply_world_visibility_plan(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: Option<WorldVisibilityPlan>,
    ) {
        for mesh in self.meshes.iter_mut() {
            if mesh.map_skybox || mesh.door_index.is_some() {
                continue;
            }
            mesh.visible_indices = plan.as_ref().map(|plan| {
                let visible = plan
                    .mesh_indices
                    .get(mesh.scene_mesh_index)
                    .map_or(&[][..], Vec::as_slice);
                upload_visible_indices(device, queue, visible)
            });
        }

        if let Some(detail_sprites) = self.detail_sprites.as_mut() {
            detail_sprites.visible_vertices = plan
                .as_ref()
                .map(|plan| upload_visible_detail_sprites(device, queue, detail_sprites, plan));
        }

        self.visibility.plan = plan;
    }

    pub fn update_door_vertices(
        &mut self,
        queue: &wgpu::Queue,
        scene: &ModelPreview,
        poses: &[DoorRenderPose],
    ) {
        for mesh in self.meshes.iter_mut() {
            let Some(door_index) = mesh.door_index else {
                continue;
            };
            let Some(door) = scene.doors.get(door_index) else {
                continue;
            };
            let pose = poses
                .get(door_index)
                .copied()
                .unwrap_or_else(|| DoorRenderPose {
                    progress: door.initial_progress.clamp(0.0, 1.0),
                    swing: initial_door_swing(door.motion),
                });
            if mesh.last_door_pose == Some(pose) {
                continue;
            }
            let Some(local_vertices) = mesh.local_vertices.as_ref() else {
                continue;
            };
            let transformed = transform_door_vertices(door, local_vertices.as_slice(), pose);
            queue.write_buffer(&mesh.vertices, 0, bytemuck::cast_slice(&transformed));
            mesh.centroid = mesh_centroid(transformed.as_slice());
            mesh.last_door_pose = Some(pose);
        }
    }
}

#[derive(Debug)]
pub struct UploadedSkybox {
    pub face_bind_groups: [Option<wgpu::BindGroup>; SKYBOX_FACE_COUNT],
}

#[derive(Debug)]
pub struct UploadedMesh {
    pub vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    pub index_count: u32,
    pub visible_indices: Option<UploadedVisibleIndices>,
    // Position in the source scene's mesh list, NOT this upload list:
    // empty-index meshes are dropped at upload, and WorldVisibilityPlan is
    // keyed by the unfiltered scene order.
    pub scene_mesh_index: usize,
    pub centroid: [f32; 3],
    pub material_index: usize,
    pub bodygroup: usize,
    pub bodygroup_choice: usize,
    pub map_skybox: bool,
    pub door_index: Option<usize>,
    pub door_visibility: Option<MapVisibilityBucket>,
    pub local_vertices: Option<Vec<ModelVertex>>,
    pub last_door_pose: Option<DoorRenderPose>,
}

#[derive(Debug)]
pub struct UploadedDetailSprites {
    pub vertices: wgpu::Buffer,
    pub vertex_count: u32,
    pub all_vertices: Vec<u8>,
    pub sprite_count: usize,
    pub visible_vertices: Option<UploadedVisibleVertices>,
    pub material_index: usize,
}

#[derive(Debug)]
pub struct UploadedOverlay {
    pub vertices: wgpu::Buffer,
    pub vertex_count: u32,
    pub centroid: [f32; 3],
    pub material_index: usize,
    pub map_skybox: bool,
}

pub fn upload_detail_sprites(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    sprites: &[DetailSprite],
) -> Option<UploadedDetailSprites> {
    let first = sprites.first()?;
    let mut vertex_bytes = Vec::with_capacity(
        sprites.len()
            * 6
            * usize::try_from(DETAIL_VERTEX_FLOAT_COUNT).unwrap_or(7)
            * std::mem::size_of::<f32>(),
    );
    for sprite in sprites {
        push_detail_sprite_vertices(&mut vertex_bytes, sprite);
    }
    let vertices = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("file_preview.model_viewer.detail_vertices"),
        size: vertex_bytes.len() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&vertices, 0, &vertex_bytes);

    Some(UploadedDetailSprites {
        vertices,
        vertex_count: u32::try_from(sprites.len().saturating_mul(6)).unwrap_or(u32::MAX),
        all_vertices: vertex_bytes,
        sprite_count: sprites.len(),
        visible_vertices: None,
        material_index: first.material_index,
    })
}

pub fn upload_meshes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    meshes: &[MeshData],
    map_skybox: bool,
) -> Vec<UploadedMesh> {
    meshes
        .iter()
        .enumerate()
        .filter(|(_, mesh)| !mesh.indices.is_empty())
        .map(|(scene_mesh_index, mesh)| {
            let vertex_bytes: &[u8] = bytemuck::cast_slice(&mesh.vertices);
            let centroid = mesh_centroid(&mesh.vertices);

            let vertices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.model_viewer.vertices"),
                size: vertex_bytes.len() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&vertices, 0, vertex_bytes);

            let indices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.model_viewer.indices"),
                size: (mesh.indices.len() * std::mem::size_of::<u32>()) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&indices, 0, bytemuck::cast_slice(&mesh.indices));

            UploadedMesh {
                vertices,
                indices,
                index_count: mesh.indices.len() as u32,
                visible_indices: None,
                scene_mesh_index,
                centroid,
                material_index: mesh.material_index,
                bodygroup: mesh.bodygroup,
                bodygroup_choice: mesh.bodygroup_choice,
                map_skybox,
                door_index: None,
                door_visibility: None,
                local_vertices: None,
                last_door_pose: None,
            }
        })
        .collect()
}

pub fn upload_door_meshes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    doors: &[DoorInstance],
) -> Vec<UploadedMesh> {
    let mut uploaded = Vec::new();
    for (door_index, door) in doors.iter().enumerate() {
        let pose = DoorRenderPose {
            progress: door.initial_progress.clamp(0.0, 1.0),
            swing: initial_door_swing(door.motion),
        };
        for mesh in &door.meshes {
            if mesh.vertices.is_empty() || mesh.indices.is_empty() {
                continue;
            }
            let transformed_vertices =
                transform_door_vertices(door, mesh.vertices.as_slice(), pose);
            let vertex_bytes: &[u8] = bytemuck::cast_slice(&transformed_vertices);
            let vertices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.model_viewer.door_vertices"),
                size: vertex_bytes.len() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&vertices, 0, vertex_bytes);

            let indices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.model_viewer.door_indices"),
                size: (mesh.indices.len() * std::mem::size_of::<u32>()) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&indices, 0, bytemuck::cast_slice(&mesh.indices));

            uploaded.push(UploadedMesh {
                vertices,
                indices,
                index_count: mesh.indices.len() as u32,
                visible_indices: None,
                scene_mesh_index: usize::MAX,
                centroid: mesh_centroid(&transformed_vertices),
                material_index: mesh.material_index,
                bodygroup: 0,
                bodygroup_choice: 0,
                map_skybox: false,
                door_index: Some(door_index),
                door_visibility: Some(door.visibility),
                local_vertices: Some(mesh.vertices.clone()),
                last_door_pose: Some(pose),
            });
        }
    }
    uploaded
}

pub fn push_detail_sprite_vertices(bytes: &mut Vec<u8>, sprite: &DetailSprite) {
    let left = sprite.upper_left[0];
    let right = sprite.lower_right[0];
    let top = sprite.upper_left[1];
    let bottom = sprite.lower_right[1];
    let tex_left = sprite.tex_upper_left[0];
    let tex_top = sprite.tex_upper_left[1];
    let tex_right = sprite.tex_lower_right[0];
    let tex_bottom = sprite.tex_lower_right[1];
    for (corner, uv) in [
        ([left, top], [tex_left, tex_top]),
        ([right, top], [tex_right, tex_top]),
        ([right, bottom], [tex_right, tex_bottom]),
        ([left, top], [tex_left, tex_top]),
        ([right, bottom], [tex_right, tex_bottom]),
        ([left, bottom], [tex_left, tex_bottom]),
    ] {
        push_f32s(bytes, &sprite.origin);
        push_f32s(bytes, &corner);
        push_f32s(bytes, &uv);
    }
}

pub fn upload_overlays(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    overlays: &[OverlayPrimitive],
    map_skybox: bool,
) -> Vec<UploadedOverlay> {
    overlays
        .iter()
        .map(|overlay| {
            let vertices = overlay_vertices(overlay);
            let vertex_bytes: &[u8] = bytemuck::cast_slice(&vertices);
            let vertices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.model_viewer.overlay_vertices"),
                size: vertex_bytes.len() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&vertices, 0, vertex_bytes);
            UploadedOverlay {
                vertices,
                vertex_count: 6,
                centroid: overlay_centroid(overlay),
                material_index: overlay.material_index,
                map_skybox,
            }
        })
        .collect()
}

/// The overlay quad as two triangles. Overlays carry no lightmap, vertex
/// colour, or blend weight, so those lanes take their neutral values.
pub fn overlay_vertices(overlay: &OverlayPrimitive) -> [ModelVertex; 6] {
    [0_usize, 1, 2, 0, 2, 3].map(|index| {
        let vertex = overlay.vertices[index];
        ModelVertex {
            position: vertex.position,
            normal: vertex.normal,
            uv: vertex.uv,
            lightmap_uv: [0.0, 0.0],
            color: [1.0, 1.0, 1.0],
            blend_alpha: 0.0,
        }
    })
}

pub fn overlay_centroid(overlay: &OverlayPrimitive) -> [f32; 3] {
    let mut centroid = [0.0_f32; 3];
    for vertex in overlay.vertices {
        for (axis, component) in vertex.position.into_iter().enumerate() {
            centroid[axis] += component;
        }
    }
    centroid.map(|component| component / overlay.vertices.len() as f32)
}

pub fn push_f32s(bytes: &mut Vec<u8>, values: &[f32]) {
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

pub fn mesh_centroid(vertices: &[ModelVertex]) -> [f32; 3] {
    if vertices.is_empty() {
        return [0.0; 3];
    }
    let mut sum = [0.0_f32; 3];
    for vertex in vertices {
        sum[0] += vertex.position[0];
        sum[1] += vertex.position[1];
        sum[2] += vertex.position[2];
    }
    let scale = 1.0 / vertices.len() as f32;
    [sum[0] * scale, sum[1] * scale, sum[2] * scale]
}
