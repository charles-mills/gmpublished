use super::DrawPlans;
use super::geometry::ViewerScene;
use super::targets::RenderTargets;
use super::uniforms::Uniforms;
use super::upload::UploadedModel;
use super::visibility::UploadedVisibility;
use super::{ModelPrimitive, RenderResources, Vec3, mat_mul, wgpu};

/// Aggregate optical attachment budget, excluding main targets and driver overhead.
const TARGET_BUDGET_BYTES: u64 = 128 * 1024 * 1024;
pub fn target_size(size: (u32, u32), count: usize, format: wgpu::TextureFormat) -> (u32, u32) {
    if count == 0 {
        return size;
    }
    let pixels = u64::from(size.0) * u64::from(size.1);
    let budget =
        TARGET_BUDGET_BYTES / super::targets::bytes_per_pixel_for_samples(format, 1) / count as u64;
    let scale = (budget as f64 / pixels.max(1) as f64).sqrt().min(1.0);
    (
        ((f64::from(size.0) * scale).floor() as u32).max(1),
        ((f64::from(size.1) * scale).floor() as u32).max(1),
    )
}

const GLASS_CLIP_OFFSET: f32 = 0.1;

#[derive(Clone, Copy)]
enum ReflectionSpace {
    World,
    Skybox,
}

fn reflection_clip_plane(
    plane: Plane,
    shader: &crate::bridge::materials::MaterialShader,
    space: ReflectionSpace,
) -> Plane {
    if shader.water().is_some() {
        match space {
            ReflectionSpace::World => {
                super::water_clip::world_plane(plane, super::water_clip::WorldWaterPass::Reflection)
            }
            ReflectionSpace::Skybox => plane,
        }
    } else {
        Plane {
            distance: plane.distance + GLASS_CLIP_OFFSET,
            ..plane
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    pub normal: Vec3,
    pub distance: f32,
}
impl Plane {
    pub fn from_triangle(vertices: &[super::ModelVertex], indices: &[u32]) -> Option<Self> {
        let a = vertices.get(*indices.first()? as usize)?;
        let b = vertices.get(*indices.get(1)? as usize)?;
        let c = vertices.get(*indices.get(2)? as usize)?;
        let mut normal = (b.position - a.position)
            .cross(c.position - a.position)
            .normalize()?;
        if normal.dot(a.normal) < 0.0 {
            normal = -normal;
        }
        Some(Self {
            normal,
            distance: normal.dot(a.position),
        })
    }
    pub fn signed_distance(self, point: Vec3) -> f32 {
        self.normal.dot(point) - self.distance
    }
    pub fn intersects_near_plane(self, uniforms: &super::Uniforms) -> bool {
        let matrix = uniforms.view_proj;
        let row = |index| Vec3::new(matrix[0][index], matrix[1][index], matrix[2][index]);
        let x = row(0);
        let y = row(1);
        let forward = row(3).normalize_or_zero();
        let eye = Vec3::new(
            uniforms.camera_position[0],
            uniforms.camera_position[1],
            uniforms.camera_position[2],
        );
        self.signed_distance(eye)
            + uniforms.water_depth_params[0]
                * (self.normal.dot(forward)
                    - self.normal.dot(x).abs() / x.length_squared().max(0.000001)
                    - self.normal.dot(y).abs() / y.length_squared().max(0.000001))
            < 0.0
    }
    pub fn reflect_point(self, point: Vec3) -> Vec3 {
        point - self.normal * (2.0 * self.signed_distance(point))
    }
    pub fn matrix(self) -> [[f32; 4]; 4] {
        let n = self.normal.to_array();
        let mut matrix = [[0.0; 4]; 4];
        for col in 0..3 {
            for row in 0..3 {
                matrix[col][row] = f32::from(col == row) - 2.0 * n[col] * n[row];
            }
        }
        for row in 0..3 {
            matrix[3][row] = 2.0 * self.distance * n[row];
        }
        matrix[3][3] = 1.0;
        matrix
    }
    pub fn same(self, other: Self) -> bool {
        self.normal.dot(other.normal) > 0.99999 && (self.distance - other.distance).abs() < 0.01
    }
    pub fn reflect_uniforms(self, mut uniforms: Uniforms, clip: Option<Self>) -> Uniforms {
        uniforms.view_proj = mat_mul(uniforms.view_proj, self.matrix());
        let eye = self.reflect_point(Vec3::from([
            uniforms.camera_position[0],
            uniforms.camera_position[1],
            uniforms.camera_position[2],
        ]));
        uniforms.camera_position = [eye[0], eye[1], eye[2], 0.0];
        uniforms.clip_plane = clip.map_or([0.0; 4], |plane| {
            [
                plane.normal[0],
                plane.normal[1],
                plane.normal[2],
                -plane.distance,
            ]
        });
        uniforms
    }
}

#[derive(Debug)]
pub struct ViewUniforms {
    pub buffer: wgpu::Buffer,
    pub bind_group: wgpu::BindGroup,
}
impl ViewUniforms {
    pub fn new(
        device: &wgpu::Device,
        resources: &super::resources::SharedResources,
        lighting: &super::lighting::Frame,
        surface_marks: &wgpu::Buffer,
    ) -> Self {
        let (buffer, bind_group) = super::resources::uniforms(
            device,
            &resources.uniform_layout,
            "file_preview.scene_view",
            lighting,
            surface_marks,
        );
        Self { buffer, bind_group }
    }
    pub fn write(&self, queue: &wgpu::Queue, uniforms: &Uniforms) {
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(uniforms));
    }
}
#[derive(Debug)]
pub struct SceneView {
    pub world_groups: super::transparency::WorldDrawGroups,
    pub sun: super::sun::gpu::Frame,
    pub world: ViewUniforms,
    pub sky: ViewUniforms,
    pub skybox: ViewUniforms,
    pub visibility: UploadedVisibility,
    pub skybox_visibility: UploadedVisibility,
    pub plans: Option<DrawPlans>,
    pub(super) plan_cache: super::view_preparation::PlanCache,
    pub(super) queue_cache: super::view_preparation::QueueCache,
    pub targets: Option<RenderTargets>,
    pub(super) planar_indices: super::planar::Selections,
    pub transparency: Vec<super::transparency::Draw>,
    pub skybox_transparency: Vec<super::transparency::Draw>,
    pub(super) transparent_indirect: super::transparent_indirect::Slots,
    pub transparency_diagnostics: super::transparency::TransparencyDiagnostics,
    pub skybox_transparency_diagnostics: super::transparency::TransparencyDiagnostics,
}
impl SceneView {
    pub fn new(
        device: &wgpu::Device,
        resources: &super::resources::SharedResources,
        lighting: &super::lighting::Frame,
        surface_marks: &wgpu::Buffer,
    ) -> Self {
        Self {
            world_groups: super::transparency::WorldDrawGroups::ALL,
            sun: super::sun::gpu::Frame::default(),
            world: ViewUniforms::new(device, resources, lighting, surface_marks),
            sky: ViewUniforms::new(device, resources, lighting, surface_marks),
            skybox: ViewUniforms::new(device, resources, lighting, surface_marks),
            visibility: UploadedVisibility::default(),
            skybox_visibility: UploadedVisibility::default(),
            plans: None,
            plan_cache: super::view_preparation::PlanCache::default(),
            queue_cache: super::view_preparation::QueueCache::default(),
            targets: None,
            planar_indices: super::planar::Selections::default(),
            transparency: Vec::new(),
            skybox_transparency: Vec::new(),
            transparent_indirect: super::transparent_indirect::Slots::default(),
            transparency_diagnostics: super::transparency::TransparencyDiagnostics::default(),
            skybox_transparency_diagnostics: super::transparency::TransparencyDiagnostics::default(
            ),
        }
    }
}
#[derive(Debug)]
pub struct Reflector {
    pub plane: Plane,
    pub mesh_index: usize,
    pub material_slot: usize,
    pub indices: Vec<u32>,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
}

impl Reflector {
    pub fn visible_indices(&self, visible: Option<&[u32]>) -> Vec<u32> {
        let Some(visible) = visible else {
            return self.indices.clone();
        };
        let triangles = visible
            .chunks_exact(3)
            .map(|t| [t[0], t[1], t[2]])
            .collect::<std::collections::HashSet<_>>();
        self.indices
            .chunks_exact(3)
            .filter(|t| triangles.contains(&[t[0], t[1], t[2]]))
            .flatten()
            .copied()
            .collect()
    }

    pub fn intersects_view(&self, matrix: [[f32; 4]; 4]) -> bool {
        bounds_intersect_view(self.bounds_min, self.bounds_max, matrix)
    }
}

pub(super) fn bounds_intersect_view(
    bounds_min: Vec3,
    bounds_max: Vec3,
    matrix: [[f32; 4]; 4],
) -> bool {
    let corners = std::array::from_fn::<_, 8, _>(|bits| {
        let p = [
            if bits & 1 == 0 {
                bounds_min[0]
            } else {
                bounds_max[0]
            },
            if bits & 2 == 0 {
                bounds_min[1]
            } else {
                bounds_max[1]
            },
            if bits & 4 == 0 {
                bounds_min[2]
            } else {
                bounds_max[2]
            },
            1.0,
        ];
        std::array::from_fn::<_, 4, _>(|row| {
            (0..4).map(|col| matrix[col][row] * p[col]).sum::<f32>()
        })
    });
    // Retain near-plane crossings when all AABB corners are offscreen.
    ![(0, 1.0), (0, -1.0), (1, 1.0), (1, -1.0), (2, 1.0)]
        .iter()
        .any(|&(axis, sign)| corners.iter().all(|p| p[axis] * sign > p[3]))
        && !corners.iter().all(|p| p[2] < 0.0)
}

pub fn optical_passes(shader: &crate::bridge::materials::MaterialShader) -> (bool, bool) {
    shader.water().map_or_else(
        || {
            shader.mirror().map_or((false, false), |mirror| {
                (mirror.reflect_enabled, mirror.refract_enabled)
            })
        },
        |water| {
            (
                water.above_water && water.reflect_enabled && !water.force_cheap,
                water.refract_enabled && !water.force_cheap,
            )
        },
    )
}

pub fn collect_reflectors(upload: &UploadedModel, scene: &super::RenderScene) -> Vec<Reflector> {
    let mut surfaces: Vec<Reflector> = Vec::new();
    for (mesh_index, mesh) in upload.meshes.iter().enumerate() {
        if mesh.map_skybox || mesh.door_index.is_some() {
            continue;
        }
        let Some(shader) = scene
            .materials
            .get(mesh.material_index)
            .map(|slot| &slot.shader)
        else {
            continue;
        };
        if optical_passes(shader) == (false, false) {
            continue;
        }
        let Some(source) = scene.meshes.get(mesh.scene_mesh_index) else {
            continue;
        };
        for group in planar_groups(source) {
            surfaces.push(Reflector {
                plane: group.plane,
                mesh_index,
                material_slot: mesh.material_index,
                indices: group.indices,
                bounds_min: group.bounds_min,
                bounds_max: group.bounds_max,
            });
        }
    }
    surfaces
}

#[derive(Debug)]
struct PlanarGroup {
    plane: Plane,
    indices: Vec<u32>,
    bounds_min: Vec3,
    bounds_max: Vec3,
}
fn planar_groups(mesh: &super::MeshData) -> Vec<PlanarGroup> {
    let mut groups: Vec<PlanarGroup> = Vec::new();
    for triangle in mesh.indices.chunks_exact(3) {
        let Some(plane) = Plane::from_triangle(&mesh.vertices, triangle) else {
            continue;
        };
        let index = groups
            .iter()
            .position(|group| group.plane.same(plane))
            .unwrap_or(groups.len());
        if index == groups.len() {
            groups.push(PlanarGroup {
                plane,
                indices: Vec::new(),
                bounds_min: Vec3::splat(f32::INFINITY),
                bounds_max: Vec3::splat(f32::NEG_INFINITY),
            });
        }
        let group = &mut groups[index];
        group.indices.extend_from_slice(triangle);
        for &vertex in triangle {
            let position = mesh.vertices[vertex as usize].position;
            for axis in 0..3 {
                group.bounds_min[axis] = group.bounds_min[axis].min(position[axis]);
                group.bounds_max[axis] = group.bounds_max[axis].max(position[axis]);
            }
        }
    }
    groups
}

#[derive(Debug)]
pub struct PlanarView {
    pub surface_index: usize,
    pub surface_indices: Vec<u32>,
    pub surface_buffer: super::visibility::UploadedVisibleIndices,
    pub reflection: Option<SceneView>,
    pub refraction: Option<SceneView>,
    pub refraction_clear: wgpu::Color,
    pub water_bind_group: Option<super::targets::WaterBindings>,
    pub surface_uniforms: ViewUniforms,
}

impl PlanarView {
    pub(super) fn prepare_reflection(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        primitive: &ModelPrimitive,
        upload: &ViewerScene,
        size: (u32, u32),
    ) {
        let Some(input) = self.prepare_reflection_input(device, queue, primitive, upload, size)
        else {
            return;
        };
        super::optical_planning::prepare_view(
            self.reflection.as_mut().expect("initialized reflection"),
            &input,
            upload,
            super::optical_planning::Common::new(primitive, upload),
            false,
        );
        self.finish_reflection(device, queue, resources, primitive, upload, &input);
    }

    pub(super) fn prepare_reflection_input(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        primitive: &ModelPrimitive,
        upload: &ViewerScene,
        size: (u32, u32),
    ) -> Option<super::optical_planning::Input> {
        let surface = &upload.assets.reflectors[self.surface_index];
        let shader = &primitive.preview.render_scene().materials[surface.material_slot].shader;
        let view = self.reflection.as_mut()?;
        let offset = primitive.raster_offset();
        // Optical target orientation also reverses its half-pixel bias.
        let reflected_offset = if shader.mirror().is_some() {
            [-offset[0], offset[1]]
        } else {
            [offset[0], -offset[1]]
        };
        let water = shader.water();
        let clip_plane = reflection_clip_plane(surface.plane, shader, ReflectionSpace::World);
        let mut main = surface
            .plane
            .reflect_uniforms(primitive.uniforms, Some(clip_plane))
            .with_clip_camera(primitive.view_camera.map(|camera| {
                camera
                    .reflected(surface.plane, water.is_some())
                    .expect("finite reflection camera")
            }))
            .with_raster_offset(size, reflected_offset);
        main.water_time_sky_tint[1..].copy_from_slice(upload.assets.sky_tint.as_array());
        view.world.write(queue, &main);
        if let Some(sky) = primitive.sky_uniforms {
            let direction_plane = Plane {
                distance: 0.0,
                ..surface.plane
            };
            view.sky.write(
                queue,
                &direction_plane
                    .reflect_uniforms(sky, None)
                    .with_raster_offset(size, reflected_offset),
            );
        }
        let skybox = primitive.map_skybox_uniforms.map(|mut uniforms| {
            uniforms.water_time_sky_tint[1..].copy_from_slice(upload.assets.sky_tint.as_array());
            let scale = primitive.skybox_transform.map_or(16.0, |(_, scale)| scale);
            let origin = primitive
                .skybox_transform
                .map_or(Vec3::ZERO, |(origin, _)| origin);
            let plane = Plane {
                normal: surface.plane.normal,
                distance: surface.plane.distance / scale + surface.plane.normal.dot(origin),
            };
            let clip = reflection_clip_plane(plane, shader, ReflectionSpace::Skybox);
            plane
                .reflect_uniforms(uniforms, Some(clip))
                .with_clip_camera(primitive.view_camera.map(|camera| {
                    camera
                        .skybox(origin, scale)
                        .reflected(plane, water.is_some())
                        .expect("finite sky reflection camera")
                }))
                .with_raster_offset(size, reflected_offset)
        });
        if let Some(skybox) = skybox {
            view.skybox.write(queue, &skybox);
        }
        if let Some(map) = primitive.preview.map() {
            // Optical passes retain main-camera PVS while using their own frustum.
            let eye = Vec3::from([
                primitive.uniforms.camera_position[0],
                primitive.uniforms.camera_position[1],
                primitive.uniforms.camera_position[2],
            ]);
            view.visibility.ensure_world_visibility(
                device,
                queue,
                map,
                upload,
                primitive.visibility_culling,
                eye,
            );
        }
        Some(super::optical_planning::Input {
            world: main,
            skybox,
            skybox_visible: primitive.map_skybox_visible
                && water.is_none_or(|water| water.reflect_3d_skybox),
            entities: if water.is_some_and(|water| !water.reflect_entities) {
                super::view_preparation::Entities::WorldOnly
            } else {
                super::view_preparation::Entities::All
            },
            water_side: water.map(|_| super::transparency::WaterSide::Above),
            include_water: water.is_none(),
            kind: super::draw::ViewKind::Reflection,
            excluded_plane: None,
            size,
        })
    }

    pub(super) fn finish_reflection(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        primitive: &ModelPrimitive,
        upload: &ViewerScene,
        input: &super::optical_planning::Input,
    ) {
        let view = self.reflection.as_mut().expect("initialized reflection");
        let sun_skybox = input.skybox.filter(|_| {
            view.plans
                .as_ref()
                .expect("prepared optical plans")
                .map_skybox
                .is_some()
        });
        super::planar::prepare(
            view,
            upload,
            primitive.preview.render_scene(),
            device,
            queue,
            None,
        );
        super::transparent_indirect::prepare(view, upload, device, queue);
        RenderTargets::ensure(
            &mut view.targets,
            device,
            resources.for_view(input.kind),
            input.size,
        );
        view.sun.prepare(
            device,
            queue,
            &resources.reflection_sun,
            &super::sun::gpu::Inputs {
                map: primitive.preview.map(),
                world: input.world,
                skybox: sun_skybox,
                targets: view.targets.as_ref().unwrap(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn water_reflection_keeps_shore_margin_without_moving_the_reflected_eye() {
        use crate::bridge::materials::MaterialResolver;
        let resolver = MaterialResolver::prop_lighting_fixture(
            Vec::new(),
            vec![
                ("materials/pool.vmt", b"Water {}".to_vec()),
                ("materials/glass.vmt", b"LightmappedReflective {}".to_vec()),
            ],
        );
        let plane = Plane {
            normal: Vec3::new(0.0, 0.0, 1.0),
            distance: -160.0,
        };
        let mut uniforms = <Uniforms as bytemuck::Zeroable>::zeroed();
        uniforms.camera_position = [0.0, 3584.0, 64.0, 0.0];
        let water = resolver
            .resolve_primary(&[], "pool")
            .into_material()
            .unwrap();
        let clipped = reflection_clip_plane(plane, &water.shader, ReflectionSpace::World);
        let reflected = plane.reflect_uniforms(uniforms, Some(clipped));
        assert_eq!(reflected.camera_position, [0.0, 3584.0, -384.0, 0.0]);
        assert_eq!(reflected.clip_plane, [0.0, 0.0, 1.0, 162.0]);
        assert!(clipped.signed_distance(Vec3::new(0.0, 0.0, -161.0)) > 0.0);
        assert!(clipped.signed_distance(Vec3::new(0.0, 0.0, -163.0)) < 0.0);
        let skybox_plane = Plane {
            distance: plane.distance / 16.0 + 10991.2,
            ..plane
        };
        let skybox_clip =
            reflection_clip_plane(skybox_plane, &water.shader, ReflectionSpace::Skybox);
        assert_eq!(skybox_clip, skybox_plane);
        assert!(skybox_clip.signed_distance(Vec3::new(0.0, 0.0, 10981.15)) < 0.0);
        let glass = resolver
            .resolve_primary(&[], "glass")
            .into_material()
            .unwrap();
        let clipped = reflection_clip_plane(plane, &glass.shader, ReflectionSpace::World);
        assert_eq!(clipped.distance, -159.9);
        assert_eq!(
            reflection_clip_plane(plane, &glass.shader, ReflectionSpace::Skybox),
            clipped
        );
    }

    #[test]
    fn near_intersection_culling_covers_corners_on_both_sides_of_water() {
        use bytemuck::Zeroable;
        let mut uniforms = super::super::Uniforms::zeroed();
        let scale = 1.0 / (std::f32::consts::FRAC_PI_8).tan();
        uniforms.view_proj = [
            [scale, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, scale, 0.0, 0.0],
            [0.0; 4],
        ];
        uniforms.water_depth_params[0] = 4.0;
        let above = Plane {
            normal: Vec3::new(0.0, 0.0, 1.0),
            distance: 0.0,
        };
        uniforms.camera_position[2] = 1.0;
        assert!(above.intersects_near_plane(&uniforms));
        uniforms.camera_position[2] = 2.0;
        assert!(!above.intersects_near_plane(&uniforms));
        uniforms.camera_position[2] = -1.0;
        let below = Plane {
            normal: -above.normal,
            distance: 0.0,
        };
        assert!(below.intersects_near_plane(&uniforms));
        uniforms.camera_position[2] = -2.0;
        assert!(!below.intersects_near_plane(&uniforms));
    }
    use super::{Plane, ReflectionSpace, Uniforms, Vec3, planar_groups, reflection_clip_plane};
    use crate::media::preview_model::{MeshData, ModelVertex};

    #[test]
    fn refraction_only_and_cheap_water_select_the_correct_passes() {
        use crate::bridge::gma::PreviewArchive;
        use crate::bridge::materials::MaterialResolver;
        use crate::test_support::GmaFixtureBuilder;
        for (parameters, expected) in [
            (
                r#""$reflecttexture" "" "$refracttexture" "_rt_WaterRefraction""#,
                (false, true),
            ),
            (
                r#""$reflecttexture" "_rt_WaterReflection" "$refracttexture" "_rt_WaterRefraction" "$forcecheap" "1""#,
                (false, false),
            ),
        ] {
            let archive = PreviewArchive::from_gma(
                GmaFixtureBuilder::new("Optical passes")
                    .entry(
                        "materials/water.vmt",
                        format!("Water {{ {parameters} }}").into_bytes(),
                    )
                    .build(),
            )
            .unwrap();
            let material = MaterialResolver::new(std::sync::Arc::new(archive), None)
                .resolve_primary(&[], "water")
                .into_material()
                .unwrap();
            assert_eq!(super::optical_passes(&material.shader), expected);
        }
    }

    #[test]
    fn oblique_reflection_is_an_involution_and_fixes_its_plane() {
        let normal = Vec3::new(1.0, 2.0, 3.0).normalize().unwrap();
        let plane = Plane {
            normal,
            distance: 120.0,
        };
        let point = Vec3::new(310.0, -40.0, 90.0);
        let reflected = plane.reflect_point(point);
        assert!((plane.reflect_point(reflected) - point).length_squared() < 1e-8);
        assert!((plane.signed_distance(reflected) + plane.signed_distance(point)).abs() < 1e-4);
        let on_plane = normal * 120.0 + normal.cross(Vec3::new(0.0, 0.0, 1.0)) * 40.0;
        assert!((plane.reflect_point(on_plane) - on_plane).length_squared() < 1e-8);
        let matrix = plane.matrix();
        let p = [point[0], point[1], point[2], 1.0];
        let transformed = Vec3::from(std::array::from_fn(|row| {
            (0..4).map(|col| matrix[col][row] * p[col]).sum()
        }));
        assert!((transformed - reflected).length_squared() < 1e-8);
        let horizontal = Plane {
            normal: Vec3::new(0.0, 0.0, 1.0),
            distance: -144.0,
        }
        .matrix();
        assert_eq!(horizontal[0][0] * horizontal[1][1] * horizontal[2][2], -1.0);
    }

    #[test]
    fn reflection_attachment_budget_preserves_normal_views_and_bounds_large_ones() {
        let format = super::wgpu::TextureFormat::Bgra8UnormSrgb;
        assert_eq!(super::target_size((1280, 720), 2, format), (1280, 720));
        for count in [1, 2, 8, 100] {
            let size = super::target_size((4096, 4096), count, format);
            assert!(size.0 >= 1 && size.1 >= 1);
            assert!(
                u64::from(size.0)
                    * u64::from(size.1)
                    * count as u64
                    * super::super::targets::bytes_per_pixel_for_samples(format, 1)
                    <= super::TARGET_BUDGET_BYTES
            );
        }
        let size = super::target_size((3840, 2160), 4, format);
        assert!((f64::from(size.0) / f64::from(size.1) - 3840.0 / 2160.0).abs() < 0.01);
    }

    #[test]
    fn sky_camera_plane_preserves_the_same_reflected_world_point() {
        let plane = Plane {
            normal: Vec3::new(1.0, 2.0, 3.0).normalize().unwrap(),
            distance: 320.0,
        };
        let origin = Vec3::new(-1000.0, 800.0, -400.0);
        let scale = 16.0;
        let sky_plane = Plane {
            normal: plane.normal,
            distance: plane.distance / scale + plane.normal.dot(origin),
        };
        let point = Vec3::new(400.0, 2000.0, -600.0);
        let expected = origin + plane.reflect_point(point) / scale;
        let actual = sky_plane.reflect_point(origin + point / scale);
        assert!((actual - expected).length_squared() < 1e-6);
    }

    #[test]
    fn planar_surface_selection_preserves_pvs_triangles() {
        let surface = super::Reflector {
            plane: Plane {
                normal: Vec3::new(0.0, 0.0, 1.0),
                distance: 0.0,
            },
            mesh_index: 0,
            material_slot: 0,
            indices: vec![0, 1, 2, 2, 3, 0, 4, 5, 6],
            bounds_min: Vec3::new(-0.5, -0.5, 0.5),
            bounds_max: Vec3::new(0.5, 0.5, 0.5),
        };
        assert_eq!(
            surface.visible_indices(Some(&[4, 5, 6, 7, 8, 9])),
            vec![4, 5, 6]
        );
        assert!(surface.visible_indices(Some(&[])).is_empty());
        assert_eq!(surface.visible_indices(None), surface.indices);
        let identity = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        assert!(surface.intersects_view(identity));
        let outside = super::Reflector {
            bounds_min: Vec3::new(2.0, -0.5, 0.5),
            bounds_max: Vec3::new(3.0, 0.5, 0.5),
            ..surface
        };
        assert!(!outside.intersects_view(identity));
    }

    #[test]
    fn one_material_at_multiple_heights_and_angles_gets_distinct_planes() {
        let vertex = |position, normal| ModelVertex {
            position,
            tangent_s: Vec3::ZERO,
            tangent_t: Vec3::ZERO,
            tangent_sign: 0.0,
            lighting_context: 0,
            surface_mark: 0,
            normal,
            uv: [0.0; 2],
            lightmap_uv: [0.0; 2],
            lightmap_bump_step: 0.0,
            lightmap_page: 0,
            color: Vec3::splat(1.0),
            blend_alpha: 0.0,
            opacity: 1.0,
            illumination: Vec3::splat(1.0),
            lighting_mode: 0,
        };
        let mut vertices = Vec::new();
        for z in [-144.0, 128.0] {
            for position in [
                Vec3::new(0.0, 0.0, z),
                Vec3::new(64.0, 0.0, z),
                Vec3::new(0.0, 64.0, z),
            ] {
                vertices.push(vertex(position, Vec3::new(0.0, 0.0, 1.0)));
            }
        }
        for position in [
            Vec3::new(40.0, 0.0, 0.0),
            Vec3::new(40.0, 64.0, 0.0),
            Vec3::new(40.0, 0.0, 64.0),
        ] {
            vertices.push(vertex(position, Vec3::new(1.0, 0.0, 0.0)));
        }
        let mesh = MeshData {
            geometry: crate::media::preview_model::MeshGeometry::Brush,
            vertices,
            indices: vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 0, 1, 2],
            material_index: 0,
            bodygroup: 0,
            bodygroup_choice: 0,
        };
        let groups = planar_groups(&mesh);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].indices, vec![0, 1, 2, 0, 1, 2]);
        assert_eq!(groups[0].plane.distance, -144.0);
        assert_eq!(groups[1].plane.distance, 128.0);
        assert_eq!(groups[2].plane.normal, Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(groups[2].plane.distance, 40.0);
    }
}
