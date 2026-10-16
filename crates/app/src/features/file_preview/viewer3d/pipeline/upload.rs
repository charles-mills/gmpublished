use super::{PreviewScene, SceneKey};
use gmpublished_domain::math::Vec3;
use gmpublished_domain::scene::map::{
    MapMeshIndexRange, MapOverlayFade, MapReceiverMesh, MapSurfaceMarkSource, MapSurfaceOwner,
    SourceLightmapAllocation, SourceLightmapPage,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use super::images::{Image, ImageCache, ImageResidency, ImageStats, ImageUploadIssue, ImageView};
use super::materials::{
    MaterialTextureViews, MaterialUploadMode, PreparedMaterial, SampledTexture,
};
use super::resources::RenderResources;
use super::uniforms::scene_sky_tint;
use crate::bridge::materials::{TextureReadTransfer, TextureRole};

use super::{
    DETAIL_VERTEX_FLOAT_COUNT, DetailSprite, DoorInstance, DoorRenderPose, MapVisibilityBucket,
    MeshData, ModelVertex, OverlayPrimitive, RenderMode, RenderScene, SKYBOX_FACE_COUNT,
    initial_door_swing, transform_door_vertices, wgpu,
};

#[derive(Debug, Default)]
pub struct UploadCache {
    uploads: HashMap<SceneKey, Arc<UploadedModel>>,
    live: HashSet<SceneKey>,
    rejected: HashMap<SceneKey, RejectedUpload>,
    images: ImageCache,
}

#[derive(Debug)]
struct RejectedUpload {
    source: PreviewScene,
    error: Arc<super::residency::SceneUploadError>,
}

impl UploadCache {
    pub fn new(capabilities: Option<&wgpu::DownlevelCapabilities>) -> Self {
        Self {
            images: ImageCache::new(capabilities),
            ..Self::default()
        }
    }

    pub(super) fn image_stats(&self) -> ImageStats {
        self.images.stats()
    }

    pub fn get(&self, key: SceneKey) -> Option<&Arc<UploadedModel>> {
        self.uploads.get(&key)
    }

    pub fn touch(&mut self, key: SceneKey) {
        self.live.insert(key);
    }

    pub fn trim(&mut self) {
        self.uploads.retain(|id, _| self.live.contains(id));
        self.rejected.retain(|id, _| self.live.contains(id));
        self.live.clear();
        self.images.trim();
        log::trace!("GPU image cache: {:?}", self.image_stats());
    }

    fn admit_scene(
        &mut self,
        key: SceneKey,
        source: &PreviewScene,
        limits: &wgpu::Limits,
    ) -> Result<(), Arc<super::residency::SceneUploadError>> {
        debug_assert_eq!(key, source.key());
        if let Some(rejected) = self.rejected.get(&key) {
            debug_assert_eq!(key, rejected.source.key());
            let error = Arc::clone(&rejected.error);
            self.touch(key);
            return Err(error);
        }
        if let Err(error) = super::residency::validate(source, limits) {
            error.log();
            let error = Arc::new(error);
            self.rejected.insert(
                key,
                RejectedUpload {
                    source: source.clone(),
                    error: Arc::clone(&error),
                },
            );
            self.touch(key);
            return Err(error);
        }
        Ok(())
    }

    pub fn ensure_upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        key: SceneKey,
        source: &PreviewScene,
    ) -> Result<(), Arc<super::residency::SceneUploadError>> {
        let scene = source.render_scene();
        let map = source.map();
        if self.uploads.contains_key(&key) {
            return Ok(());
        }

        self.admit_scene(key, source, &device.limits())?;

        let mut images = self.images.lease(&resources.texture_uploader);
        let flat_normal = &resources.flat_normal;
        let flat_sample = SampledTexture {
            view: flat_normal,
            flags: None,
        };
        let mut sampler_diagnostics = Vec::new();
        let material_uploads = scene
            .materials
            .iter()
            .map(|slot| {
                let base_view = resources.create_slot_texture(
                    device,
                    queue,
                    slot,
                    slot.texture.as_deref(),
                    &mut images,
                    false,
                );
                let base2_view = slot.texture2.as_deref().map(|texture| {
                    resources.create_slot_texture(
                        device,
                        queue,
                        slot,
                        Some(texture),
                        &mut images,
                        true,
                    )
                });
                let prepared = PreparedMaterial::from(&slot.shader);
                let normal_source = prepared.normal();
                let normal2_source = prepared.normal2();
                let normal = normal_source.and_then(|normal| {
                    RenderResources::create_data_view(
                        device,
                        queue,
                        normal,
                        TextureRole::Normal,
                        &mut images,
                        &slot.name,
                    )
                });
                let normal2 = normal2_source.and_then(|normal| {
                    RenderResources::create_data_view(
                        device,
                        queue,
                        normal,
                        TextureRole::Normal,
                        &mut images,
                        &slot.name,
                    )
                });
                resources.create_material(
                    device,
                    queue,
                    MaterialTextureViews {
                        base: base_view.sampled(),
                        base2: base2_view.as_ref().unwrap_or(&base_view).sampled(),
                        normal: normal.as_ref().map_or(flat_sample, |view| SampledTexture {
                            view,
                            flags: normal_source
                                .map(crate::bridge::materials::DecodedTextureFrames::flags),
                        }),
                        normal2: normal2
                            .as_ref()
                            .map(|view| SampledTexture {
                                view,
                                flags: normal2_source
                                    .map(crate::bridge::materials::DecodedTextureFrames::flags),
                            })
                            .or_else(|| {
                                normal.as_ref().map(|view| SampledTexture {
                                    view,
                                    flags: normal_source
                                        .map(crate::bridge::materials::DecodedTextureFrames::flags),
                                })
                            })
                            .unwrap_or(flat_sample),
                    },
                    MaterialUploadMode {
                        name: &slot.name,
                        material: PreparedMaterial::from(&slot.shader),
                        base_flags: slot.texture.as_ref().and_then(|texture| texture.flags()),
                        base2_flags: slot
                            .texture2
                            .as_ref()
                            .or(slot.texture.as_ref())
                            .and_then(|texture| texture.flags()),
                        render_mode: slot.render_mode,
                        alpha_policy: slot.alpha_policy,
                    },
                    &mut images,
                    &mut sampler_diagnostics,
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
        let mut material_uploads = material_uploads;
        if material_uploads.is_empty() {
            let base_view = &resources.error_texture;
            let upload = resources.create_material(
                device,
                queue,
                MaterialTextureViews {
                    base: SampledTexture {
                        view: base_view,
                        flags: None,
                    },
                    base2: SampledTexture {
                        view: base_view,
                        flags: None,
                    },
                    normal: flat_sample,
                    normal2: flat_sample,
                },
                MaterialUploadMode {
                    name: "<missing material>",
                    material: PreparedMaterial::Fallback,
                    base_flags: None,
                    base2_flags: None,
                    render_mode: RenderMode::Opaque,
                    alpha_policy: crate::bridge::materials::TextureAlphaPolicy::Opaque,
                },
                &mut images,
                &mut sampler_diagnostics,
            );
            material_uploads.push(upload);
        }
        let mut material_bind_groups = Vec::with_capacity(material_uploads.len());
        let mut water_material_bind_groups = Vec::with_capacity(material_uploads.len());
        let mut material_uniforms = Vec::with_capacity(material_uploads.len());
        for (material, water, uniform) in material_uploads {
            material_bind_groups.push(material);
            water_material_bind_groups.push(water);
            material_uniforms.push(uniform);
        }

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
        let renderables = map.map_or_else(Vec::new, |map| {
            [
                (&map.renderables, false),
                (&map.map_skybox_renderables, true),
            ]
            .into_iter()
            .flat_map(|(sources, map_skybox)| {
                sources
                    .iter()
                    .enumerate()
                    .map(move |(source_index, source)| (source_index, source, map_skybox))
            })
            .map(|(source_index, source, map_skybox)| {
                let ranges = source
                    .ranges
                    .iter()
                    .filter_map(|range| {
                        let receiver = if map_skybox {
                            MapReceiverMesh::Skybox(range.mesh_index)
                        } else {
                            MapReceiverMesh::World(range.mesh_index)
                        };
                        let &(mesh_index, _) = receivers.get(&receiver)?;
                        let Some(end) = range
                            .start
                            .checked_add(range.count)
                            .filter(|&end| end <= meshes[mesh_index].index_count)
                        else {
                            log::warn!("map renderable {:?}: invalid mesh range", source.source);
                            return None;
                        };
                        Some(UploadedRenderableRange {
                            mesh_index,
                            indices: range.start..end,
                        })
                    })
                    .collect();
                if let Err(error) = source.bounds {
                    log::debug!("map renderable {:?}: {error}", source.source);
                }
                UploadedMapRenderable {
                    source_index,
                    map_skybox,
                    ranges,
                }
            })
            .collect()
        });
        let mut receiver_faces = HashMap::<u32, Vec<UploadedReceiver>>::new();
        let mut brush_receiver_groups = std::collections::BTreeMap::<_, Vec<(usize, usize)>>::new();
        if let Some(map) = map {
            for (source_index, receiver) in map.surface_receivers.iter().enumerate() {
                if let Some(&(mesh_index, _)) = receivers.get(&receiver.receiver_mesh) {
                    let owner = receiver.owner.id();
                    if owner != gmpublished_domain::scene::map::MapSurfaceOwnerId::World {
                        brush_receiver_groups
                            .entry(owner)
                            .or_default()
                            .push((mesh_index, source_index));
                    }
                    let lightmap =
                        receiver_lightmap(map, receiver.range.face, receiver.material_index);
                    receiver_faces
                        .entry(receiver.range.face)
                        .or_default()
                        .push(UploadedReceiver {
                            source_index,
                            mesh_index,
                            material_group: lightmap.material_group,
                            lightmap_page: lightmap.page,
                        });
                }
            }
        }
        let overlays = map.map_or_else(Vec::new, |map| {
            let mut overlays = upload_overlays(
                device,
                queue,
                map.overlays.as_slice(),
                false,
                map,
                0,
                &receivers,
            );
            overlays.extend(upload_overlays(
                device,
                queue,
                map.map_skybox_overlays.as_slice(),
                true,
                map,
                overlays.len(),
                &receivers,
            ));
            overlays
        });
        let sky_tint = scene_sky_tint(map.and_then(|map| map.skybox.as_ref()));
        let skybox = map.and_then(|map| map.skybox.as_ref()).map(|skybox| {
            let face_bind_groups = std::array::from_fn(|index| {
                skybox.materials[index].as_ref().map(|material| {
                    let source = if material.is_painted() {
                        material
                            .painted
                            .as_ref()
                            .and_then(|paint| paint.texture.as_ref())
                            .and_then(|texture| texture.resident())
                    } else {
                        skybox.faces[index]
                            .as_ref()
                            .and_then(|texture| texture.resident())
                    };
                    let transfer = if material.is_painted() {
                        TextureReadTransfer::Unmodified
                    } else {
                        TextureReadTransfer::SrgbToLinear
                    };
                    let view = source
                        .and_then(|source| {
                            match images.view(device, queue, source, transfer, ImageView::D2(0)) {
                                Ok(view) => Some(view),
                                Err(error) => {
                                    images.reject("<sky>", "face", error);
                                    None
                                }
                            }
                        })
                        .unwrap_or_else(|| resources.sky_black_texture.clone());
                    resources.create_sky_material(
                        device,
                        &view,
                        super::sky::SkyUniform::new(material),
                        material
                            .painted
                            .as_ref()
                            .and_then(|paint| paint.texture.as_ref())
                            .map_or(0, |texture| texture.flags()),
                        &mut sampler_diagnostics,
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
                    let view = RenderResources::create_data_view(
                        device,
                        queue,
                        &effect.normal_map,
                        TextureRole::Normal,
                        &mut images,
                        &effect.name,
                    );
                    let flags = view.as_ref().and_then(|_| {
                        effect
                            .normal_map
                            .source_metadata()
                            .and_then(|metadata| metadata.flags)
                    });
                    let sampler = resources.samplers.texture(
                        &effect.name,
                        "underwater_normal",
                        flags,
                        super::samplers::TextureKind::Texture2d,
                        resources.sampler_quality,
                        &mut sampler_diagnostics,
                    );
                    Arc::new(super::screen_refraction::Texture {
                        view: view.unwrap_or_else(|| flat_normal.clone()),
                        sampler,
                    })
                });
        }
        super::transparency::build_parts(&mut meshes, &material_render_modes, scene, map);
        let (images, image_upload_issues) = images.finish();
        let mut upload = UploadedModel {
            images,
            image_upload_issues,
            sampler_diagnostics,
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
            receiver_faces,
            renderables,
            brush_receiver_groups: brush_receiver_groups
                .into_iter()
                .map(|(owner, receivers)| {
                    let model = map.and_then(|map| {
                        let &(_, source) = receivers.first()?;
                        let index = map.surface_receivers[source].owner.model_index()?;
                        map.visibility.as_ref()?.brush_model(index)
                    });
                    UploadedBrushReceiverGroup {
                        owner,
                        receivers,
                        model,
                    }
                })
                .collect(),
            phy_debug_meshes: OnceLock::new(),
            material_bind_groups,
            water_material_bind_groups,
            material_render_modes,
            material_planar_materials,
            _material_uniforms: material_uniforms,
            skybox,
            sky_tint,
            reflectors: Vec::new(),
        };
        upload.reflectors = super::reflections::collect_reflectors(&upload, scene);
        log::debug!(
            "Material samplers: {} roles, {} limitations",
            upload.sampler_diagnostics.len(),
            upload
                .sampler_diagnostics
                .iter()
                .filter(|policy| policy.has_limitations())
                .count()
        );
        log::debug!(
            "Scene GPU images: {:?}; {} upload issues",
            upload.image_stats(),
            upload.image_upload_issues.len()
        );
        self.uploads.insert(key, Arc::new(upload));
        Ok(())
    }
}

#[derive(Debug)]
pub struct UploadedReceiver {
    pub source_index: usize,
    pub lightmap_page: SourceLightmapPage,
    pub mesh_index: usize,
    pub material_group: usize,
}

#[derive(Debug)]
pub struct UploadedBrushReceiverGroup {
    pub owner: gmpublished_domain::scene::map::MapSurfaceOwnerId,
    pub receivers: Vec<(usize, usize)>,
    pub model: Option<gmpublished_domain::scene::map::MapBrushModelRendering>,
}

#[derive(Debug)]
pub struct UploadedMapRenderable {
    pub source_index: usize,
    pub map_skybox: bool,
    pub ranges: Vec<UploadedRenderableRange>,
}

#[derive(Debug)]
pub struct UploadedRenderableRange {
    pub mesh_index: usize,
    pub indices: std::ops::Range<u32>,
}

#[derive(Debug)]
pub struct UploadedModel {
    images: Vec<Arc<Image>>,
    pub(super) image_upload_issues: Vec<ImageUploadIssue>,
    pub(super) sampler_diagnostics: Vec<super::samplers::SamplerDiagnostic>,
    pub(super) source: PreviewScene,
    pub(super) surface_pipelines: super::surface_pipelines::SurfacePipelineVariants,
    pub screen_refraction_normals: HashMap<String, Arc<super::screen_refraction::Texture>>,
    pub meshes: Vec<Arc<UploadedMesh>>,
    pub detail_sprites: Option<UploadedDetailSprites>,
    pub map_skybox_detail_sprites: Option<UploadedDetailSprites>,
    pub overlays: Vec<Arc<UploadedOverlay>>,
    pub receiver_faces: HashMap<u32, Vec<UploadedReceiver>>,
    pub brush_receiver_groups: Vec<UploadedBrushReceiverGroup>,
    pub renderables: Vec<UploadedMapRenderable>,
    pub phy_debug_meshes: OnceLock<Vec<UploadedMesh>>,
    pub material_bind_groups: Vec<wgpu::BindGroup>,
    pub water_material_bind_groups: Vec<wgpu::BindGroup>,
    pub material_render_modes: Vec<RenderMode>,
    pub material_planar_materials: Vec<bool>,
    pub _material_uniforms: Vec<wgpu::Buffer>,
    pub skybox: Option<UploadedSkybox>,
    pub sky_tint: Vec3,
    pub reflectors: Vec<super::reflections::Reflector>,
}

impl UploadedModel {
    pub(super) fn image_stats(&self) -> ImageResidency {
        super::images::lease_stats(&self.images)
    }
    pub fn map(&self) -> Option<&super::MapPreview> {
        self.source.map()
    }
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
    pub fast_eligible: Vec<bool>,
    pub leaf_indices: Vec<u16>,
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
    pub decal_sort: Option<crate::media::preview_model::StaticDecalSort>,
    pub receiver_mesh_index: Option<usize>,
    pub receiver_range: MapMeshIndexRange,
    pub receiver_material_index: usize,
    pub receiver_material_group: usize,
    pub receiver_lightmap_page: SourceLightmapPage,
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
        fast_eligible: sprites.iter().map(|sprite| sprite.fast_eligible).collect(),
        leaf_indices: sprites.iter().map(|sprite| sprite.leaf_index).collect(),
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
    map: &super::MapPreview,
    offset: usize,
    receivers: &HashMap<MapReceiverMesh, (usize, usize)>,
) -> Vec<Arc<UploadedOverlay>> {
    overlays
        .iter()
        .enumerate()
        .map(|(index, overlay)| {
            let lightmap = receiver_lightmap(
                map,
                overlay.receiver_range.face,
                overlay.receiver_material_index,
            );
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
            let (posed, local_vertices) = if let MapSurfaceOwner::Door { door_index, .. } =
                overlay.owner
            {
                let door = &map.doors[door_index];
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
                decal_sort: overlay.decal_sort,
                receiver_mesh_index: receivers
                    .get(&overlay.receiver_mesh)
                    .map(|&(index, _)| index),
                receiver_range: overlay.receiver_range,
                receiver_lightmap_page: lightmap.page,
                receiver_material_group: lightmap.material_group,
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

fn receiver_lightmap(
    map: &super::MapPreview,
    face: u32,
    material_group: usize,
) -> SourceLightmapAllocation {
    map.surface_lightmaps
        .as_ref()
        .and_then(|layout| layout.surfaces.get(face as usize))
        .copied()
        .unwrap_or(SourceLightmapAllocation {
            page: SourceLightmapPage::White,
            offset: [0; 2],
            material_group,
        })
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

#[cfg(test)]
mod admission_tests {
    use super::*;

    #[test]
    fn rejected_scene_identity_is_retained_touched_and_trimmed_without_image_allocations() {
        let source = Arc::new(super::super::super::test_support::empty_preview(
            Vec3::ZERO,
            Vec3::splat(1.0),
        ));
        let weak = Arc::downgrade(&source);
        let scene = PreviewScene::Map(source);
        let key = scene.key();
        let limits = wgpu::Limits {
            max_buffer_size: 0,
            ..Default::default()
        };
        let mut cache = UploadCache::default();
        let first = cache.admit_scene(key, &scene, &limits).unwrap_err();
        cache.trim();
        let second = cache.admit_scene(key, &scene, &limits).unwrap_err();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(cache.rejected.len(), 1);
        assert!(cache.uploads.is_empty());
        assert_eq!(cache.image_stats().allocations, 0);
        drop(scene);
        assert!(weak.upgrade().is_some());
        cache.trim();
        assert!(weak.upgrade().is_some());
        cache.trim();
        assert!(weak.upgrade().is_none());
        assert!(cache.rejected.is_empty());
    }
}
