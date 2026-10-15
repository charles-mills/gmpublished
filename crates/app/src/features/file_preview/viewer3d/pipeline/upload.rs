use super::{PreviewScene, SceneKey};
use gmpublished_domain::math::Vec3;
use gmpublished_domain::scene::map::{
    MapMeshIndexRange, MapOverlayFade, MapReceiverMesh, MapSurfaceMarkSource, MapSurfaceOwner,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use super::TextureUploadLevel;
use super::materials::{MaterialTextureViews, MaterialUploadMode, PreparedMaterial};
use super::resources::RenderResources;
use super::uniforms::scene_sky_tint;

use super::{
    DETAIL_VERTEX_FLOAT_COUNT, DetailSprite, DoorInstance, DoorRenderPose, MapVisibilityBucket,
    MeshData, ModelVertex, OverlayPrimitive, RenderMode, RenderScene, SKYBOX_FACE_COUNT,
    initial_door_swing, transform_door_vertices, wgpu,
};

#[derive(Debug, Default)]
pub struct UploadCache {
    uploads: HashMap<SceneKey, Arc<UploadedModel>>,
    live: HashSet<SceneKey>,
}

impl UploadCache {
    pub fn get(&self, key: SceneKey) -> Option<&Arc<UploadedModel>> {
        self.uploads.get(&key)
    }

    pub fn touch(&mut self, key: SceneKey) {
        self.live.insert(key);
    }

    pub fn trim(&mut self) {
        self.uploads.retain(|id, _| self.live.contains(id));
        self.live.clear();
    }

    pub fn ensure_upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        key: SceneKey,
        source: &PreviewScene,
    ) {
        let scene = source.render_scene();
        let map = source.map();
        if self.uploads.contains_key(&key) {
            return;
        }

        let flat_normal = &resources.flat_normal;
        let material_uploads = scene
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
                let base2_view = slot.texture2.as_deref().map(|texture| {
                    resources.create_slot_texture_view(
                        device,
                        queue,
                        "file_preview.model_viewer.texture2",
                        slot,
                        Some(texture),
                    )
                });
                let surface = slot.shader.surface();
                let water = slot.shader.water();
                let mirror = slot.shader.mirror();
                let normal = water
                    .and_then(|water| water.normal_map.as_deref())
                    .or_else(|| mirror.and_then(|mirror| mirror.normal_map.as_deref()))
                    .or_else(|| surface.and_then(|surface| surface.normal.as_deref()))
                    .map(|normal| RenderResources::create_normal_view(device, queue, normal));
                let normal2 = surface
                    .and_then(|surface| surface.normal2.as_deref())
                    .map(|normal| RenderResources::create_normal_view(device, queue, normal));
                resources.create_material(
                    device,
                    queue,
                    MaterialTextureViews {
                        base: &base_view,
                        base2: base2_view.as_ref().unwrap_or(&base_view),
                        normal: normal.as_ref().unwrap_or(flat_normal),
                        normal2: normal2.as_ref().or(normal.as_ref()).unwrap_or(flat_normal),
                    },
                    MaterialUploadMode {
                        material: PreparedMaterial::from(&slot.shader),
                        base_flags: slot.texture.as_ref().and_then(|texture| texture.flags),
                        base2_flags: slot
                            .texture2
                            .as_ref()
                            .or(slot.texture.as_ref())
                            .and_then(|texture| texture.flags),
                        render_mode: slot.render_mode,
                        alpha_policy: slot.alpha_policy,
                    },
                )
            })
            .collect::<Vec<_>>();
        let material_render_modes = if scene.materials.is_empty() {
            vec![RenderMode::Opaque]
        } else {
            scene
                .materials
                .iter()
                .map(|slot| slot.render_mode)
                .collect::<Vec<_>>()
        };
        let material_planar_materials = if scene.materials.is_empty() {
            vec![false]
        } else {
            scene
                .materials
                .iter()
                .map(|slot| slot.shader.is_planar())
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
                        normal: flat_normal,
                        normal2: flat_normal,
                    },
                    MaterialUploadMode {
                        material: PreparedMaterial::Fallback,
                        base_flags: None,
                        base2_flags: None,
                        render_mode: RenderMode::Opaque,
                        alpha_policy: crate::bridge::materials::TextureAlphaPolicy::Opaque,
                    },
                );
                (vec![bind_group], vec![uniform])
            } else {
                material_uploads.into_iter().unzip()
            };

        let mut meshes = upload_meshes(
            device,
            queue,
            scene.meshes.as_slice(),
            false,
            map.map(|map| map.entity_meshes.as_slice()),
        );
        let detail_sprites = map.and_then(|map| {
            meshes.extend(upload_meshes(
                device,
                queue,
                map.map_skybox_meshes.as_slice(),
                true,
                Some(&map.skybox_entity_meshes),
            ));
            meshes.extend(upload_door_meshes(device, queue, map.doors.as_slice()));
            upload_detail_sprites(device, queue, map.detail_sprites.as_slice())
        });
        let map_skybox_detail_sprites = map.and_then(|map| {
            upload_detail_sprites(device, queue, map.map_skybox_detail_sprites.as_slice())
        });
        let receivers = meshes
            .iter()
            .enumerate()
            .map(|(index, mesh)| (mesh.receiver_source(), (index, mesh.material_index)))
            .collect::<HashMap<_, _>>();
        let overlays = map.map_or_else(Vec::new, |map| {
            let mut overlays = upload_overlays(
                device,
                queue,
                map.overlays.as_slice(),
                false,
                &map.doors,
                0,
                &receivers,
            );
            overlays.extend(upload_overlays(
                device,
                queue,
                map.map_skybox_overlays.as_slice(),
                true,
                &map.doors,
                overlays.len(),
                &receivers,
            ));
            overlays
        });
        let sky_tint = scene_sky_tint(map.and_then(|map| map.skybox.as_ref()));
        let skybox = map.and_then(|map| map.skybox.as_ref()).map(|skybox| {
            let face_bind_groups = std::array::from_fn(|index| {
                skybox.materials[index].as_ref().map(|material| {
                    let fallback = TextureUploadLevel {
                        rgba: &[0, 0, 0, 255],
                        width: 1,
                        height: 1,
                    };
                    let levels = if material.is_painted() {
                        material
                            .painted
                            .as_ref()
                            .and_then(|paint| paint.texture.as_ref())
                            .and_then(|texture| texture.frames.first())
                            .map(|mips| {
                                mips.iter()
                                    .map(|mip| TextureUploadLevel {
                                        rgba: &mip.rgba,
                                        width: mip.width,
                                        height: mip.height,
                                    })
                                    .collect::<Vec<_>>()
                            })
                    } else {
                        skybox.faces[index].as_ref().map(|texture| {
                            texture
                                .mip_chain()
                                .map(|mip| TextureUploadLevel {
                                    rgba: mip.rgba,
                                    width: mip.width,
                                    height: mip.height,
                                })
                                .collect::<Vec<_>>()
                        })
                    }
                    .unwrap_or_else(|| vec![fallback]);
                    let view = resources.create_texture_view_from_levels_format(
                        device,
                        queue,
                        "file_preview.sky.texture",
                        &levels,
                        if material.is_painted() {
                            wgpu::TextureFormat::Rgba8Unorm
                        } else {
                            wgpu::TextureFormat::Rgba8UnormSrgb
                        },
                    );
                    resources.create_sky_material(
                        device,
                        &view,
                        super::sky::SkyUniform::new(material),
                        material
                            .painted
                            .as_ref()
                            .and_then(|paint| paint.texture.as_ref())
                            .map_or(0, |texture| texture.flags),
                    )
                })
            });
            UploadedSkybox { face_bind_groups }
        });

        let mut screen_refraction_normals = HashMap::new();
        for effect in scene
            .materials
            .iter()
            .filter_map(|slot| slot.shader.water())
            .filter_map(|water| water.underwater_overlay.as_deref())
        {
            screen_refraction_normals
                .entry(effect.name.clone())
                .or_insert_with(|| {
                    RenderResources::create_normal_view(device, queue, &effect.normal_map)
                });
        }
        super::transparency::build_parts(&mut meshes, &material_render_modes, scene, map);
        let mut upload = UploadedModel {
            source: source.clone(),
            surface_pipelines: super::surface_pipelines::SurfacePipelineVariants::new(
                device,
                resources,
                &scene.materials,
            ),
            screen_refraction_normals,
            meshes: meshes.into_iter().map(Arc::new).collect(),
            detail_sprites,
            map_skybox_detail_sprites,
            overlays,
            phy_debug_meshes: OnceLock::new(),
            material_bind_groups,
            material_render_modes,
            material_planar_materials,
            _material_uniforms: material_uniforms,
            skybox,
            sky_tint,
            reflectors: Vec::new(),
        };
        upload.reflectors = super::reflections::collect_reflectors(&upload, scene);
        self.uploads.insert(key, Arc::new(upload));
    }
}

#[derive(Debug)]
pub struct UploadedModel {
    pub(super) source: PreviewScene,
    pub(super) surface_pipelines: super::surface_pipelines::SurfacePipelineVariants,
    pub screen_refraction_normals: HashMap<String, wgpu::TextureView>,
    pub meshes: Vec<Arc<UploadedMesh>>,
    pub detail_sprites: Option<UploadedDetailSprites>,
    pub map_skybox_detail_sprites: Option<UploadedDetailSprites>,
    pub overlays: Vec<Arc<UploadedOverlay>>,
    pub phy_debug_meshes: OnceLock<Vec<UploadedMesh>>,
    pub material_bind_groups: Vec<wgpu::BindGroup>,
    pub material_render_modes: Vec<RenderMode>,
    pub material_planar_materials: Vec<bool>,
    pub _material_uniforms: Vec<wgpu::Buffer>,
    pub skybox: Option<UploadedSkybox>,
    pub sky_tint: Vec3,
    pub reflectors: Vec<super::reflections::Reflector>,
}

impl UploadedModel {
    pub fn has_map_skybox_content(&self) -> bool {
        self.meshes.iter().any(|mesh| mesh.map_skybox)
            || self.map_skybox_detail_sprites.is_some()
            || self.overlays.iter().any(|overlay| overlay.map_skybox)
    }

    pub fn ensure_phy_debug_meshes(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &RenderScene,
    ) {
        self.phy_debug_meshes
            .get_or_init(|| upload_meshes(device, queue, &scene.phy_debug_meshes, false, None));
    }
}

#[derive(Debug)]
pub struct UploadedSkybox {
    pub face_bind_groups: [Option<wgpu::BindGroup>; SKYBOX_FACE_COUNT],
}

#[derive(Debug)]
pub struct UploadedMesh {
    pub geometry: crate::media::preview_model::MeshGeometry,
    pub entity: bool,
    pub transparent_parts: Vec<super::transparency::Part>,
    vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    pub index_count: u32,
    pub scene_mesh_index: usize,
    centroid: Vec3,
    pub material_index: usize,
    pub bodygroup: usize,
    pub bodygroup_choice: usize,
    pub map_skybox: bool,
    pub door_index: Option<usize>,
    pub door_mesh_index: Option<usize>,
    pub door_visibility: Option<MapVisibilityBucket>,
    pub local_vertices: Option<Vec<ModelVertex>>,
}

impl UploadedMesh {
    pub fn receiver_source(&self) -> MapReceiverMesh {
        if let (Some(door_index), Some(mesh_index)) = (self.door_index, self.door_mesh_index) {
            MapReceiverMesh::Door {
                door_index,
                mesh_index,
            }
        } else if self.map_skybox {
            MapReceiverMesh::Skybox(self.scene_mesh_index)
        } else {
            MapReceiverMesh::World(self.scene_mesh_index)
        }
    }

    pub fn vertices(&self) -> &wgpu::Buffer {
        &self.vertices
    }
    pub fn centroid(&self) -> Vec3 {
        self.centroid
    }
}

#[derive(Debug)]
pub struct UploadedDetailSprites {
    pub vertices: wgpu::Buffer,
    pub all_vertices: Vec<u8>,
    pub material_index: usize,
    pub underwater_leaves: Vec<bool>,
}

#[derive(Debug)]
pub struct UploadedOverlay {
    vertices: wgpu::Buffer,
    pub vertex_count: u32,
    centroid: Vec3,
    pub material_index: usize,
    pub map_skybox: bool,
    pub owner: MapSurfaceOwner,
    pub source: MapSurfaceMarkSource,
    pub receiver_mesh_index: Option<usize>,
    pub receiver_range: MapMeshIndexRange,
    pub receiver_material_index: usize,
    pub render_order: u16,
    pub fade: Option<MapOverlayFade>,
    pub fade_origin: Vec3,
    pub local_vertices: Option<Vec<ModelVertex>>,
}

impl UploadedOverlay {
    pub fn vertices(&self) -> &wgpu::Buffer {
        &self.vertices
    }
    pub fn centroid(&self) -> Vec3 {
        self.centroid
    }
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
            * usize::try_from(DETAIL_VERTEX_FLOAT_COUNT).unwrap_or(14)
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
        all_vertices: vertex_bytes,
        material_index: first.material_index,
        underwater_leaves: sprites
            .iter()
            .map(|sprite| sprite.underwater_leaf)
            .collect(),
    })
}

pub fn upload_meshes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    meshes: &[MeshData],
    map_skybox: bool,
    entity_flags: Option<&[bool]>,
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
                geometry: mesh.geometry,
                transparent_parts: Vec::new(),
                entity: entity_flags
                    .and_then(|flags| flags.get(scene_mesh_index))
                    .copied()
                    .unwrap_or(false),
                vertices,
                indices,
                index_count: mesh.indices.len() as u32,
                scene_mesh_index,
                centroid,
                material_index: mesh.material_index,
                bodygroup: mesh.bodygroup,
                bodygroup_choice: mesh.bodygroup_choice,
                map_skybox,
                door_index: None,
                door_mesh_index: None,
                door_visibility: None,
                local_vertices: None,
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
        for (door_mesh_index, mesh) in door.meshes.iter().enumerate() {
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
                geometry: mesh.geometry,
                transparent_parts: Vec::new(),
                entity: true,
                vertices,
                indices,
                index_count: mesh.indices.len() as u32,
                scene_mesh_index: usize::MAX,
                centroid: mesh_centroid(&transformed_vertices),
                material_index: mesh.material_index,
                bodygroup: 0,
                bodygroup_choice: 0,
                map_skybox: false,
                door_index: Some(door_index),
                door_mesh_index: Some(door_mesh_index),
                door_visibility: Some(door.visibility),
                local_vertices: Some(mesh.vertices.clone()),
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
        push_f32s(bytes, sprite.origin.as_array());
        push_f32s(bytes, &corner);
        push_f32s(bytes, &uv);
        push_f32s(bytes, &[f32::from(sprite.orientation)]);
        push_f32s(
            bytes,
            Vec3::new(0.0, -1.0, 0.0)
                .rotate_source(sprite.angles)
                .as_array(),
        );
        push_f32s(
            bytes,
            Vec3::new(0.0, 0.0, 1.0)
                .rotate_source(sprite.angles)
                .as_array(),
        );
    }
}

pub fn upload_overlays(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    overlays: &[OverlayPrimitive],
    map_skybox: bool,
    doors: &[DoorInstance],
    offset: usize,
    receivers: &HashMap<MapReceiverMesh, (usize, usize)>,
) -> Vec<Arc<UploadedOverlay>> {
    overlays
        .iter()
        .enumerate()
        .map(|(index, overlay)| {
            let mark_id = u32::try_from(offset + index + 1).expect("surface mark index fits u32");
            let expanded = overlay
                .indices
                .iter()
                .map(|&index| {
                    let mut vertex = overlay.vertices[index as usize];
                    vertex.surface_mark = mark_id;
                    vertex
                })
                .collect::<Vec<_>>();
            let (posed, local_vertices) = if let MapSurfaceOwner::Door { door_index } =
                overlay.owner
            {
                let door = &doors[door_index];
                (
                    transform_door_vertices(door, &expanded, super::geometry::initial_pose(door)),
                    Some(expanded),
                )
            } else {
                (expanded, None)
            };
            let vertices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("file_preview.surface_mark.vertices"),
                size: std::mem::size_of_val(posed.as_slice()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&vertices, 0, bytemuck::cast_slice(&posed));
            Arc::new(UploadedOverlay {
                vertices,
                vertex_count: posed.len() as u32,
                centroid: mesh_centroid(&posed),
                material_index: overlay.material_index,
                map_skybox,
                owner: overlay.owner,
                source: overlay.source,
                receiver_mesh_index: receivers
                    .get(&overlay.receiver_mesh)
                    .map(|&(index, _)| index),
                receiver_range: overlay.receiver_range,
                receiver_material_index: receivers
                    .get(&overlay.receiver_mesh)
                    .map_or(overlay.receiver_material_index, |&(_, material)| material),
                render_order: overlay.render_order,
                fade: overlay.fade,
                fade_origin: overlay.fade_origin,
                local_vertices,
            })
        })
        .collect()
}

pub fn push_f32s(bytes: &mut Vec<u8>, values: &[f32]) {
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

pub fn mesh_centroid(vertices: &[ModelVertex]) -> Vec3 {
    if vertices.is_empty() {
        return Vec3::splat(0.0);
    }
    let mut sum = Vec3::ZERO;
    for vertex in vertices {
        sum[0] += vertex.position[0];
        sum[1] += vertex.position[1];
        sum[2] += vertex.position[2];
    }
    let scale = 1.0 / vertices.len() as f32;
    Vec3::new(sum[0] * scale, sum[1] * scale, sum[2] * scale)
}
