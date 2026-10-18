use super::{MeshData, ModelVertex, PreviewScene, wgpu};
use crate::bridge::materials::{DecodedTextureFrames, EnvironmentMapSource, TextureLoadOutcome};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub(super) struct TexturePreparationIssue {
    pub material: String,
    pub role: &'static str,
    pub outcome: TextureLoadOutcome,
}

#[derive(Debug, thiserror::Error)]
#[error("scene preparation rejected {} texture, {} buffer, {} lightmap and {} animation resources", .issues.len(), .buffer_issues.len(), .lightmap_issues.len(), .animation_issues.len())]
pub(super) struct SceneUploadError {
    pub issues: Vec<TexturePreparationIssue>,
    pub buffer_issues: Vec<BufferPreparationIssue>,
    lightmap_issues: Vec<LightmapPreparationIssue>,
    animation_issues: Vec<super::skinning::PreparationIssue>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct LightmapPreparationIssue {
    resource: BufferResource,
    reason: super::lightmap_ranges::RangeError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub(super) enum BufferResource {
    Mesh { skybox: bool, index: usize },
    PhysicsDebug { index: usize },
    Door { door: usize, mesh: usize },
    SurfaceMark { skybox: bool, index: usize },
    Details { skybox: bool },
    Visibility { mesh: usize },
    ModelLighting,
    SurfaceMarkState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub(super) enum BufferKind {
    Vertices,
    Indices,
    Storage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub(super) enum BufferConstraint {
    ByteArithmetic,
    DrawCount,
    HostAllocationSize,
    BufferSize,
    StorageBindingSize,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(super) struct BufferPreparationIssue {
    pub resource: BufferResource,
    pub kind: BufferKind,
    pub constraint: BufferConstraint,
    pub required_bytes: Option<u64>,
    pub count: Option<u64>,
    pub limit: u64,
}

impl SceneUploadError {
    pub fn log(&self) {
        log::error!("scene GPU preparation failed: {self}");
        for issue in &self.issues {
            log::error!(
                "material {} {} residency: {:?}",
                issue.material,
                issue.role,
                issue.outcome
            );
        }
        for issue in &self.buffer_issues {
            log::error!("scene buffer preparation: {issue:?}");
        }
        for issue in &self.animation_issues {
            log::error!("scene animation preparation: {issue:?}");
        }
        for issue in &self.lightmap_issues {
            log::error!("scene lightmap preparation: {issue:?}");
        }
    }
}

struct Validator {
    issues: Vec<TexturePreparationIssue>,
    buffer_issues: Vec<BufferPreparationIssue>,
    lightmap_issues: Vec<LightmapPreparationIssue>,
    animation_issues: Vec<super::skinning::PreparationIssue>,
}

impl Validator {
    fn rows(
        &mut self,
        limits: &wgpu::Limits,
        resource: BufferResource,
        kind: BufferKind,
        mut rows: impl Iterator<Item = u64>,
        stride: u64,
    ) {
        if let Some(count) = rows.try_fold(0_u64, u64::checked_add) {
            self.buffer(limits, resource, kind, count, stride);
        } else {
            self.buffer_issues.push(BufferPreparationIssue {
                resource,
                kind,
                constraint: BufferConstraint::ByteArithmetic,
                required_bytes: None,
                count: None,
                limit: u64::MAX,
            });
        }
    }

    fn instances(
        &mut self,
        limits: &wgpu::Limits,
        resource: BufferResource,
        count: u64,
        vertices: u64,
        stride: u64,
    ) {
        if let Some(count) = count.checked_mul(vertices) {
            self.buffer(limits, resource, BufferKind::Vertices, count, stride);
        } else {
            self.buffer_issues.push(BufferPreparationIssue {
                resource,
                kind: BufferKind::Vertices,
                constraint: BufferConstraint::ByteArithmetic,
                required_bytes: None,
                count: None,
                limit: u64::MAX,
            });
        }
    }

    fn buffer(
        &mut self,
        limits: &wgpu::Limits,
        resource: BufferResource,
        kind: BufferKind,
        count: u64,
        stride: u64,
    ) {
        let required = count.checked_mul(stride);
        let mut reject = |constraint, limit| {
            self.buffer_issues.push(BufferPreparationIssue {
                resource,
                kind,
                constraint,
                required_bytes: required,
                count: Some(count),
                limit,
            });
        };
        let Some(bytes) = required else {
            reject(BufferConstraint::ByteArithmetic, u64::MAX);
            return;
        };
        if kind != BufferKind::Storage && count > u64::from(u32::MAX) {
            reject(BufferConstraint::DrawCount, u64::from(u32::MAX));
        }
        if bytes > isize::MAX as u64 {
            reject(BufferConstraint::HostAllocationSize, isize::MAX as u64);
        }
        if bytes > limits.max_buffer_size {
            reject(BufferConstraint::BufferSize, limits.max_buffer_size);
        }
        if kind == BufferKind::Storage && bytes > u64::from(limits.max_storage_buffer_binding_size)
        {
            reject(
                BufferConstraint::StorageBindingSize,
                u64::from(limits.max_storage_buffer_binding_size),
            );
        }
    }

    fn mesh(&mut self, limits: &wgpu::Limits, resource: BufferResource, mesh: &MeshData) {
        if mesh.indices.is_empty() {
            return;
        }
        if let Err(error) = super::lightmap_ranges::Ranges::new(&mesh.vertices, &mesh.indices) {
            self.lightmap_issues.push(LightmapPreparationIssue {
                resource,
                reason: error,
            });
        }
        self.buffer(
            limits,
            resource,
            BufferKind::Vertices,
            mesh.vertices.len() as u64,
            size_of::<ModelVertex>() as u64,
        );
        self.buffer(
            limits,
            resource,
            BufferKind::Indices,
            mesh.indices.len() as u64,
            size_of::<u32>() as u64,
        );
    }

    fn geometry(&mut self, scene: &PreviewScene, limits: &wgpu::Limits) {
        let render = scene.render_scene();
        for (index, mesh) in render.meshes.iter().enumerate() {
            self.mesh(
                limits,
                BufferResource::Mesh {
                    skybox: false,
                    index,
                },
                mesh,
            );
        }
        for (index, mesh) in render.phy_debug_meshes.iter().enumerate() {
            self.mesh(limits, BufferResource::PhysicsDebug { index }, mesh);
        }
        if let Some(map) = scene.map() {
            for (index, mesh) in map.map_skybox_meshes.iter().enumerate() {
                self.mesh(
                    limits,
                    BufferResource::Mesh {
                        skybox: true,
                        index,
                    },
                    mesh,
                );
            }
            for (door, instance) in map.doors.iter().enumerate() {
                for (mesh, source) in instance.meshes.iter().enumerate() {
                    if !source.vertices.is_empty() {
                        self.mesh(limits, BufferResource::Door { door, mesh }, source);
                    }
                }
            }
            for (skybox, overlays) in [(false, &map.overlays), (true, &map.map_skybox_overlays)] {
                for (index, overlay) in overlays.iter().enumerate() {
                    if let Err(reason) =
                        super::lightmap_ranges::Ranges::new(&overlay.vertices, &overlay.indices)
                    {
                        self.lightmap_issues.push(LightmapPreparationIssue {
                            resource: BufferResource::SurfaceMark { skybox, index },
                            reason,
                        });
                    }
                    self.buffer(
                        limits,
                        BufferResource::SurfaceMark { skybox, index },
                        BufferKind::Vertices,
                        overlay.indices.len() as u64,
                        size_of::<ModelVertex>() as u64,
                    );
                }
            }
            for (skybox, sprites) in [
                (false, &map.detail_sprites),
                (true, &map.map_skybox_detail_sprites),
            ] {
                self.instances(
                    limits,
                    BufferResource::Details { skybox },
                    sprites.len() as u64,
                    6,
                    super::DETAIL_VERTEX_FLOAT_COUNT * size_of::<f32>() as u64,
                );
            }
            for (mesh, visibility) in map.mesh_visibility.iter().enumerate() {
                let Some(source) = render.meshes.get(mesh) else {
                    continue;
                };
                let mut faces = HashMap::<u32, u32>::new();
                for range in visibility.always_visible.iter().chain(
                    visibility
                        .clusters
                        .iter()
                        .flat_map(|cluster| &cluster.ranges),
                ) {
                    if u64::from(range.start) + u64::from(range.count)
                        <= source.indices.len() as u64
                    {
                        faces
                            .entry(range.face)
                            .and_modify(|count| *count = (*count).max(range.count))
                            .or_insert(range.count);
                    }
                }
                self.rows(
                    limits,
                    BufferResource::Visibility { mesh },
                    BufferKind::Indices,
                    faces.values().map(|count| u64::from(*count)),
                    size_of::<u32>() as u64,
                );
            }
        }
        let contexts = scene.map().map_or(0, |map| map.model_lighting.len()) as u64;
        self.rows(
            limits,
            BufferResource::ModelLighting,
            BufferKind::Storage,
            [contexts, 1].into_iter(),
            size_of::<super::model_lighting::GpuModelContext>() as u64,
        );
        let marks = scene.map().map_or([0, 0], |map| {
            [
                map.overlays.len() as u64,
                map.map_skybox_overlays.len() as u64,
            ]
        });
        self.rows(
            limits,
            BufferResource::SurfaceMarkState,
            BufferKind::Storage,
            [marks[0], marks[1], 1].into_iter(),
            size_of::<super::marks::GpuSurfaceMark>() as u64,
        );
    }

    fn check(
        &mut self,
        material: &str,
        role: &'static str,
        resident: bool,
        outcome: TextureLoadOutcome,
    ) {
        if !resident
            || matches!(
                outcome,
                TextureLoadOutcome::Deferred | TextureLoadOutcome::Unplanned
            )
        {
            self.issues.push(TexturePreparationIssue {
                material: material.to_owned(),
                role,
                outcome,
            });
        }
    }

    fn frames(
        &mut self,
        material: &str,
        role: &'static str,
        frames: Option<&DecodedTextureFrames>,
    ) {
        if let Some(frames) = frames {
            self.check(
                material,
                role,
                frames.resident().is_some() || frames.on_demand().is_some(),
                frames.load_outcome(),
            );
        }
    }
}

pub(super) fn validate(
    scene: &PreviewScene,
    limits: &wgpu::Limits,
) -> Result<(), SceneUploadError> {
    let mut validator = Validator {
        issues: Vec::new(),
        buffer_issues: Vec::new(),
        lightmap_issues: Vec::new(),
        animation_issues: super::skinning::validate(scene, limits),
    };
    validator.geometry(scene, limits);
    for slot in &scene.render_scene().materials {
        if !slot.render_mode.draws() {
            continue;
        }
        let name = &slot.name;
        for (role, texture) in [
            ("base", slot.texture.as_deref()),
            ("base2", slot.texture2.as_deref()),
        ] {
            if let Some(texture) = texture {
                validator.check(
                    name,
                    role,
                    texture.resident().is_some() || texture.on_demand().is_some(),
                    texture.load_outcome(),
                );
            }
        }
        if let Some(surface) = slot.shader.surface() {
            validator.frames(name, "normal", surface.normal.as_deref());
            validator.frames(name, "normal2", surface.normal2.as_deref());
            validator.frames(
                name,
                "detail",
                surface
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.texture.as_deref()),
            );
            validator.frames(
                name,
                "blend_modulation",
                surface.blend_modulation.as_deref(),
            );
            validator.frames(
                name,
                "emission",
                surface
                    .emission
                    .as_ref()
                    .and_then(|emission| emission.mask.as_deref()),
            );
            if let Some(environment) = &surface.envmap {
                validator.frames(
                    name,
                    "environment_mask",
                    environment.mask_texture.as_deref(),
                );
                if let EnvironmentMapSource::Named(cube) = &environment.source {
                    validator.check(
                        name,
                        "environment",
                        cube.resident().is_some() || cube.on_demand().is_some(),
                        cube.load_outcome(),
                    );
                }
            }
            if let Some(phong) = &surface.phong {
                for (role, texture) in [
                    ("phong_exponent", &phong.exponent_texture),
                    ("phong_warp", &phong.specular_warp),
                    ("diffuse_warp", &phong.diffuse_warp),
                ] {
                    validator.frames(
                        name,
                        role,
                        texture
                            .as_ref()
                            .and_then(|texture| texture.texture.as_deref()),
                    );
                }
            }
        }
        if let Some(water) = slot.shader.water() {
            validator.frames(name, "normal", water.normal_map.as_deref());
            validator.frames(
                name,
                "underwater_normal",
                water
                    .underwater_overlay
                    .as_ref()
                    .map(|overlay| overlay.normal_map.as_ref()),
            );
        }
        if let Some(mirror) = slot.shader.mirror() {
            validator.frames(name, "normal", mirror.normal_map.as_deref());
        }
    }
    if let Some(sky) = scene.map().and_then(|map| map.skybox.as_ref()) {
        for (index, texture) in sky.faces.iter().enumerate() {
            if sky.materials[index]
                .as_ref()
                .is_some_and(|material| material.no_draw)
            {
                continue;
            }
            let name = format!("sky_face_{index}");
            if let Some(texture) = texture {
                validator.check(
                    &name,
                    "sky",
                    texture.resident().is_some() || texture.on_demand().is_some(),
                    texture.load_outcome(),
                );
            }
            validator.frames(
                &name,
                "stars",
                sky.materials[index]
                    .as_ref()
                    .and_then(|material| material.painted.as_ref())
                    .and_then(|paint| paint.texture.as_deref()),
            );
        }
    }
    if validator.issues.is_empty()
        && validator.buffer_issues.is_empty()
        && validator.lightmap_issues.is_empty()
        && validator.animation_issues.is_empty()
    {
        Ok(())
    } else {
        Err(SceneUploadError {
            issues: validator.issues,
            buffer_issues: validator.buffer_issues,
            lightmap_issues: validator.lightmap_issues,
            animation_issues: validator.animation_issues,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::materials::{
        MaterialResolutionOutcome, MaterialResolver, ResolvedMaterialTextures,
    };
    use crate::media::preview_model::MaterialSlot;
    use crate::test_support::fixture_vtf_bytes;
    use gmpublished_domain::math::Vec3;
    use std::sync::Arc;

    fn scene(material: ResolvedMaterialTextures) -> PreviewScene {
        let mut map =
            super::super::super::test_support::empty_preview(Vec3::ZERO, Vec3::splat(1.0));
        Arc::make_mut(&mut map.scene).materials.push(MaterialSlot {
            tint_base: None,
            resolution: MaterialResolutionOutcome::Loaded,
            canonical_name: Some("test/planned".to_owned()),
            name: "test/planned".to_owned(),
            shader: material.shader,
            texture: material.texture,
            texture2: material.texture2,
            render_mode: material.render_mode,
            alpha_policy: material.render_mode.texture_alpha_policy(),
        });
        PreviewScene::Map(Arc::new(map))
    }

    #[test]
    fn planning_results_cannot_enter_gpu_upload() {
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![
                ("materials/test/planned.vmt", br#"LightmappedGeneric { "$basetexture" "test/image" "$bumpmap" "test/image" "$detail" "test/image" }"#.to_vec()),
                ("materials/test/image.vtf", fixture_vtf_bytes(2, 2, vformats::vtf::VtfFormat::Rgba8888, &[&[255; 16]])),
            ],
            Vec::new(),
        );
        let (planned, requests) = resolver
            .plan_texture_requests(|resolver| resolver.resolve_with_base2(&[], "test/planned"));
        let error = validate(
            &scene(planned.into_material().unwrap()),
            &wgpu::Limits::default(),
        )
        .unwrap_err();
        for role in ["base", "normal", "detail"] {
            assert!(
                error.issues.iter().any(
                    |issue| issue.role == role && issue.outcome == TextureLoadOutcome::Deferred
                ),
                "{role}"
            );
        }
        let admitted = resolver.with_texture_admissions(resolver.admit_texture_requests(requests));
        let material = admitted
            .resolve_with_base2(&[], "test/planned")
            .into_material()
            .unwrap();
        assert!(validate(&scene(material), &wgpu::Limits::default()).is_ok());
    }

    #[test]
    fn no_draw_does_not_admit_retained_surface_pages_for_gpu_residency() {
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![
                (
                    "materials/test/hidden.vmt",
                    br#"UnlitGeneric { "$basetexture" "test/image" }"#.to_vec(),
                ),
                (
                    "materials/test/image.vtf",
                    fixture_vtf_bytes(1, 1, vformats::vtf::VtfFormat::Rgba8888, &[&[1, 2, 3, 255]]),
                ),
            ],
            Vec::new(),
        );
        let (planned, _) = resolver
            .plan_texture_requests(|resolver| resolver.resolve_with_base2(&[], "test/hidden"));
        let mut material = planned.into_material().unwrap();
        assert!(validate(&scene(material.clone()), &wgpu::Limits::default()).is_err());
        material.render_mode = crate::bridge::materials::RenderMode::NoDraw;
        assert!(validate(&scene(material), &wgpu::Limits::default()).is_ok());
    }
    fn validator() -> Validator {
        Validator {
            issues: Vec::new(),
            buffer_issues: Vec::new(),
            lightmap_issues: Vec::new(),
            animation_issues: Vec::new(),
        }
    }

    #[test]
    fn host_allocation_limit_is_checked_independently_of_device_limits() {
        let limits = wgpu::Limits {
            max_buffer_size: u64::MAX,
            ..Default::default()
        };
        let mut check = validator();
        check.buffer(
            &limits,
            BufferResource::Details { skybox: false },
            BufferKind::Vertices,
            1,
            isize::MAX as u64,
        );
        assert!(check.buffer_issues.is_empty());
        check.buffer(
            &limits,
            BufferResource::Details { skybox: false },
            BufferKind::Vertices,
            1,
            isize::MAX as u64 + 1,
        );
        assert_eq!(check.buffer_issues.len(), 1);
        assert_eq!(
            check.buffer_issues[0].constraint,
            BufferConstraint::HostAllocationSize
        );
    }

    #[test]
    fn device_buffer_and_storage_binding_limits_are_independent_and_exact() {
        let limits = wgpu::Limits {
            max_buffer_size: 1024,
            max_storage_buffer_binding_size: 512,
            ..Default::default()
        };
        let mut check = validator();
        check.buffer(
            &limits,
            BufferResource::ModelLighting,
            BufferKind::Storage,
            8,
            64,
        );
        check.buffer(
            &limits,
            BufferResource::Mesh {
                skybox: false,
                index: 0,
            },
            BufferKind::Vertices,
            16,
            64,
        );
        assert!(check.buffer_issues.is_empty());
        check.buffer(
            &limits,
            BufferResource::ModelLighting,
            BufferKind::Storage,
            9,
            64,
        );
        assert_eq!(
            check.buffer_issues[0].constraint,
            BufferConstraint::StorageBindingSize
        );
        assert_eq!(check.buffer_issues[0].required_bytes, Some(576));
        check.buffer(
            &limits,
            BufferResource::Mesh {
                skybox: false,
                index: 0,
            },
            BufferKind::Indices,
            257,
            4,
        );
        assert_eq!(
            check.buffer_issues[1].constraint,
            BufferConstraint::BufferSize
        );
        assert_eq!(check.buffer_issues[1].required_bytes, Some(1028));
    }

    #[test]
    fn byte_row_and_instance_overflow_are_rejected_without_allocations() {
        let mut check = validator();
        let limits = wgpu::Limits::default();
        check.buffer(
            &limits,
            BufferResource::ModelLighting,
            BufferKind::Storage,
            u64::MAX,
            2,
        );
        check.rows(
            &limits,
            BufferResource::SurfaceMarkState,
            BufferKind::Storage,
            [u64::MAX, 1].into_iter(),
            32,
        );
        check.instances(
            &limits,
            BufferResource::Details { skybox: false },
            u64::MAX,
            6,
            56,
        );
        assert_eq!(check.buffer_issues.len(), 3);
        assert!(
            check
                .buffer_issues
                .iter()
                .all(|issue| issue.constraint == BufferConstraint::ByteArithmetic
                    && issue.required_bytes.is_none())
        );
    }

    fn mesh(vertices: usize, indices: usize) -> MeshData {
        use bytemuck::Zeroable;
        MeshData {
            geometry: Default::default(),
            vertices: vec![ModelVertex::zeroed(); vertices],
            indices: vec![0; indices],
            material_index: 0,
            bodygroup: 0,
            bodygroup_choice: 0,
        }
    }

    #[test]
    fn lazy_debug_sky_and_world_mesh_buffers_are_preflighted() {
        let mut map =
            super::super::super::test_support::empty_preview(Vec3::ZERO, Vec3::splat(1.0));
        Arc::make_mut(&mut map.scene).meshes.push(mesh(9, 3));
        Arc::make_mut(&mut map.scene)
            .phy_debug_meshes
            .push(mesh(3, 300));
        map.map_skybox_meshes.push(mesh(9, 3));
        let errors = validate(
            &PreviewScene::Map(Arc::new(map)),
            &wgpu::Limits {
                max_buffer_size: 1024,
                ..Default::default()
            },
        )
        .unwrap_err();
        for resource in [
            BufferResource::Mesh {
                skybox: false,
                index: 0,
            },
            BufferResource::Mesh {
                skybox: true,
                index: 0,
            },
            BufferResource::PhysicsDebug { index: 0 },
        ] {
            assert!(
                errors
                    .buffer_issues
                    .iter()
                    .any(|issue| issue.resource == resource
                        && issue.constraint == BufferConstraint::BufferSize)
            );
        }
    }

    #[test]
    fn mark_expansion_and_detail_six_vertex_recipe_match_gpu_writers() {
        use gmpublished_domain::scene::map::{
            MapMeshIndexRange, MapReceiverMesh, MapSurfaceMarkSource, MapSurfaceOwner,
        };
        let mut map =
            super::super::super::test_support::empty_preview(Vec3::ZERO, Vec3::splat(1.0));
        map.overlays
            .push(crate::media::preview_model::OverlayPrimitive {
                source: MapSurfaceMarkSource::Overlay { index: 0, id: 0 },
                vertices: mesh(3, 3).vertices,
                indices: vec![0; 12],
                material_index: 0,
                visibility: gmpublished_domain::scene::map::MapPropVisibility::Always,
                owner: MapSurfaceOwner::World,
                receiver_mesh: MapReceiverMesh::World(0),
                receiver_range: MapMeshIndexRange {
                    face: 0,
                    start: 0,
                    count: 3,
                },
                receiver_material_index: 0,
                render_order: 0,
                fade: None,
                fade_origin: Vec3::ZERO,
                decal_sort: None,
            });
        let sprite = crate::media::preview_model::DetailSprite {
            fast_eligible: true,
            leaf_index: 0,
            origin: Vec3::ZERO,
            angles: Vec3::ZERO,
            orientation: 0,
            underwater_leaf: false,
            upper_left: [0.0; 2],
            lower_right: [1.0; 2],
            tex_upper_left: [0.0; 2],
            tex_lower_right: [1.0; 2],
            material_index: 0,
            visibility: gmpublished_domain::scene::map::MapVisibilityBucket::Always,
        };
        map.detail_sprites = vec![sprite; 2];
        map.map_skybox_detail_sprites = vec![sprite; 2];
        let errors = validate(
            &PreviewScene::Map(Arc::new(map)),
            &wgpu::Limits {
                max_buffer_size: 512,
                max_storage_buffer_binding_size: 32,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(errors.buffer_issues.iter().any(|issue| issue.resource
            == BufferResource::SurfaceMark {
                skybox: false,
                index: 0
            }
            && issue.required_bytes == Some(12 * size_of::<ModelVertex>() as u64)));
        assert!(errors.buffer_issues.iter().any(|issue| issue.resource
            == BufferResource::SurfaceMarkState
            && issue.count == Some(2)
            && issue.required_bytes
                == Some(2 * size_of::<super::super::marks::GpuSurfaceMark>() as u64)
            && issue.constraint == BufferConstraint::StorageBindingSize));
        for skybox in [false, true] {
            assert!(
                errors
                    .buffer_issues
                    .iter()
                    .any(|issue| issue.resource == BufferResource::Details { skybox }
                        && issue.count == Some(12)
                        && issue.required_bytes == Some(672))
            );
        }
    }

    #[test]
    fn distinct_face_visibility_ranges_bound_dynamic_buffers_and_dedup_clusters() {
        use gmpublished_domain::scene::map::{
            MapMeshClusterRanges, MapMeshIndexRange, MapMeshVisibility,
        };
        let mut map =
            super::super::super::test_support::empty_preview(Vec3::ZERO, Vec3::splat(1.0));
        Arc::make_mut(&mut map.scene).meshes.push(mesh(3, 240));
        let range = MapMeshIndexRange {
            face: 0,
            start: 0,
            count: 240,
        };
        map.mesh_visibility.push(MapMeshVisibility {
            always_visible: vec![range],
            clusters: vec![MapMeshClusterRanges {
                cluster: 1,
                ranges: vec![range],
            }],
        });
        let limits = wgpu::Limits {
            max_buffer_size: 1024,
            ..Default::default()
        };
        assert!(validate(&PreviewScene::Map(Arc::new(map.clone())), &limits).is_ok());
        map.mesh_visibility[0]
            .always_visible
            .push(MapMeshIndexRange { face: 1, ..range });
        let errors = validate(&PreviewScene::Map(Arc::new(map)), &limits).unwrap_err();
        assert!(errors.buffer_issues.iter().any(|issue| issue.resource
            == BufferResource::Visibility { mesh: 0 }
            && issue.required_bytes == Some(1920)));
    }
    #[test]
    fn model_storage_requirement_matches_encoded_rows_including_default_row() {
        let data = super::super::model_lighting::encode(&[], &[1.0; 128], 0.0, None, None);
        let expected = size_of_val(data.as_slice()) as u64;
        let scene = PreviewScene::Map(Arc::new(super::super::super::test_support::empty_preview(
            Vec3::ZERO,
            Vec3::splat(1.0),
        )));
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: expected as u32 - 1,
            ..Default::default()
        };
        let error = validate(&scene, &limits).unwrap_err();
        let issue = error
            .buffer_issues
            .iter()
            .find(|issue| issue.resource == BufferResource::ModelLighting)
            .unwrap();
        assert_eq!(issue.required_bytes, Some(expected));
        assert_eq!(issue.count, Some(data.len() as u64));
        assert_eq!(issue.constraint, BufferConstraint::StorageBindingSize);
    }

    #[test]
    fn door_pose_buffers_use_the_original_local_vertex_count() {
        use gmpublished_domain::scene::map::{MapDoorClass, MapDoorMotion, MapDoorOpenDirection};
        let mut map =
            super::super::super::test_support::empty_preview(Vec3::ZERO, Vec3::splat(1.0));
        map.doors.push(super::super::DoorInstance {
            source_index: 0,
            brush_collision: None,
            class: MapDoorClass::PropDoorRotating,
            origin: Vec3::ZERO,
            angles: Vec3::ZERO,
            local_bounds_min: Vec3::ZERO,
            local_bounds_max: Vec3::splat(1.0),
            visibility: gmpublished_domain::scene::map::MapVisibilityBucket::Always,
            initial_progress: 0.5,
            auto_close_after: None,
            motion: MapDoorMotion::Rotating {
                angle_delta: Vec3::new(0.0, 90.0, 0.0),
                degrees: 90.0,
                speed: 100.0,
                open_direction: MapDoorOpenDirection::Both,
            },
            sounds: Default::default(),
            meshes: vec![mesh(9, 3)],
        });
        let posed = super::super::transform_door_vertices(
            &map.doors[0],
            &map.doors[0].meshes[0].vertices,
            super::super::geometry::initial_pose(&map.doors[0]),
        );
        let expected = size_of_val(posed.as_slice()) as u64;
        let error = validate(
            &PreviewScene::Map(Arc::new(map)),
            &wgpu::Limits {
                max_buffer_size: 1024,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.buffer_issues.iter().any(|issue| issue.resource
            == BufferResource::Door { door: 0, mesh: 0 }
            && issue.required_bytes == Some(expected)));
    }
}
