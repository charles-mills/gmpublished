//! Planar views share geometry, but own all camera, visibility and target state.
use super::super::draw_plan::{DrawPlanRequest, DrawPlanSelection, prepare_draw_plans};
use super::DrawPlans;
use super::targets::RenderTargets;
use super::uniforms::Uniforms;
use super::upload::UploadedModel;
use super::visibility::UploadedVisibility;
use super::{ModelPrimitive, RenderResources, Vec3, mat_mul, wgpu};

/// Bound aggregate reflection attachments while preserving aspect ratio. The
/// normalized projection/sample coordinates are independent of RT resolution.
/// Main-view attachments and driver allocation overhead are separate.
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
    pub fn intersects_near_plane(self, uniforms: super::Uniforms) -> bool {
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
        // Minimum signed distance over the four perspective near corners.
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
    pub fn reflect_uniforms(self, mut uniforms: Uniforms, clip: bool) -> Uniforms {
        uniforms.view_proj = mat_mul(uniforms.view_proj, self.matrix());
        let eye = self.reflect_point(Vec3::from([
            uniforms.camera_position[0],
            uniforms.camera_position[1],
            uniforms.camera_position[2],
        ]));
        uniforms.camera_position = [eye[0], eye[1], eye[2], 0.0];
        uniforms.clip_plane = if clip {
            [
                self.normal[0],
                self.normal[1],
                self.normal[2],
                -self.distance - 0.1,
            ]
        } else {
            [0.0; 4]
        };
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
        resources: &RenderResources,
        lighting: &super::lighting::Frame,
    ) -> Self {
        let (buffer, bind_group) = super::resources::uniforms(
            device,
            &resources.uniform_layout,
            "file_preview.scene_view",
            lighting,
        );
        Self { buffer, bind_group }
    }
    pub fn write(&self, queue: &wgpu::Queue, uniforms: &Uniforms) {
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(uniforms));
    }
}
#[derive(Debug)]
pub struct SceneView {
    pub sun: super::sun::gpu::Frame,
    pub world: ViewUniforms,
    pub sky: ViewUniforms,
    pub skybox: ViewUniforms,
    pub visibility: UploadedVisibility,
    pub skybox_visibility: UploadedVisibility,
    pub plans: Option<DrawPlans>,
    pub targets: Option<RenderTargets>,
    pub planar_exclusion_generation: Option<u64>,
    pub transparency: Vec<super::transparency::Draw>,
    pub skybox_transparency: Vec<super::transparency::Draw>,
}
impl SceneView {
    pub fn new(
        device: &wgpu::Device,
        resources: &RenderResources,
        lighting: &super::lighting::Frame,
    ) -> Self {
        Self {
            sun: super::sun::gpu::Frame::default(),
            world: ViewUniforms::new(device, resources, lighting),
            sky: ViewUniforms::new(device, resources, lighting),
            skybox: ViewUniforms::new(device, resources, lighting),
            visibility: UploadedVisibility::default(),
            skybox_visibility: UploadedVisibility::default(),
            plans: None,
            targets: None,
            planar_exclusion_generation: None,
            transparency: Vec::new(),
            skybox_transparency: Vec::new(),
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
        let corners = std::array::from_fn::<_, 8, _>(|bits| {
            let p = [
                if bits & 1 == 0 {
                    self.bounds_min[0]
                } else {
                    self.bounds_max[0]
                },
                if bits & 2 == 0 {
                    self.bounds_min[1]
                } else {
                    self.bounds_max[1]
                },
                if bits & 4 == 0 {
                    self.bounds_min[2]
                } else {
                    self.bounds_max[2]
                },
                1.0,
            ];
            std::array::from_fn::<_, 4, _>(|row| {
                (0..4).map(|col| matrix[col][row] * p[col]).sum::<f32>()
            })
        });
        // Conservative homogeneous AABB/frustum rejection retains near-plane
        // crossings instead of dropping triangles whose corners are offscreen.
        ![(0, 1.0), (0, -1.0), (1, 1.0), (1, -1.0), (2, 1.0)]
            .iter()
            .any(|&(axis, sign)| corners.iter().all(|p| p[axis] * sign > p[3]))
            && !corners.iter().all(|p| p[2] < 0.0)
    }
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
    pub view: SceneView,
    pub reflect_enabled: bool,
    pub refraction: Option<SceneView>,
    pub refraction_clear: wgpu::Color,
    pub water_bind_group: Option<wgpu::BindGroup>,
    pub surface_uniforms: ViewUniforms,
}

impl PlanarView {
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        primitive: &ModelPrimitive,
        upload: &UploadedModel,
        size: (u32, u32),
    ) {
        let surface = &upload.reflectors[self.surface_index];
        let shader = &primitive.preview.render_scene().materials[surface.material_slot].shader;
        let offset = primitive.raster_offset();
        // Stored optical targets use the main-camera reflection matrix. Source
        // uses a proper reflected camera, with a different axis reversal for
        // water and reflective glass; transform its raster bias accordingly.
        let reflected_offset = if shader.mirror().is_some() {
            [-offset[0], offset[1]]
        } else {
            [offset[0], -offset[1]]
        };
        let mut main = surface
            .plane
            .reflect_uniforms(primitive.uniforms, true)
            .with_raster_offset(size, reflected_offset);
        let water = primitive.preview.render_scene().materials[surface.material_slot]
            .shader
            .water();
        main.water_time_sky_tint[1..].copy_from_slice(upload.sky_tint.as_array());
        self.view.world.write(queue, &main);
        if let Some(sky) = primitive.sky_uniforms {
            let direction_plane = Plane {
                distance: 0.0,
                ..surface.plane
            };
            self.view.sky.write(
                queue,
                &direction_plane
                    .reflect_uniforms(sky, false)
                    .with_raster_offset(size, reflected_offset),
            );
        }
        let skybox = primitive.map_skybox_uniforms.map(|mut uniforms| {
            uniforms.water_time_sky_tint[1..].copy_from_slice(upload.sky_tint.as_array());
            // sky_camera translates and uniformly scales world coordinates.
            let scale = primitive.skybox_transform.map_or(16.0, |(_, scale)| scale);
            let origin = primitive
                .skybox_transform
                .map_or(Vec3::ZERO, |(origin, _)| origin);
            Plane {
                normal: surface.plane.normal,
                distance: surface.plane.distance / scale + surface.plane.normal.dot(origin),
            }
            .reflect_uniforms(uniforms, true)
            .with_raster_offset(size, reflected_offset)
        });
        if let Some(skybox) = skybox {
            self.view.skybox.write(queue, &skybox);
        }
        if let Some(map) = primitive.preview.map() {
            // Main SetupVis remains active while Source builds optical world
            // lists. Keep the reflected frustum and clipping view-local;
            // projecting the eye onto water admits unrelated world clusters.
            let eye = Vec3::from([
                primitive.uniforms.camera_position[0],
                primitive.uniforms.camera_position[1],
                primitive.uniforms.camera_position[2],
            ]);
            self.view.visibility.ensure_world_visibility(
                device,
                queue,
                map,
                upload,
                primitive.visibility_culling,
                eye,
            );
        }
        let mut plans = prepare_draw_plans(
            upload,
            DrawPlanRequest {
                content_id: primitive.content_id,
                selection: DrawPlanSelection {
                    skin_remap: &primitive.skin_remap,
                    bodygroup_choices: &primitive.bodygroup_choices,
                },
                camera_position: main.camera_position,
                map_skybox_visible: primitive.map_skybox_visible,
                map_skybox_content_present: upload.has_map_skybox_content(),
                map_skybox_camera_position: skybox.map(|uniforms| uniforms.camera_position),
                visibility_plan: self.view.visibility.plan.as_ref(),
            },
        );
        // Secondary planar surfaces use the non-recursive material fallback.
        // They still cover geometry and obey this view's clip plane; skipping
        // them would leave holes. No reflected pass samples its own target.
        if let Some(water) = primitive
            .preview
            .render_scene()
            .materials
            .get(surface.material_slot)
            .and_then(|slot| slot.shader.water())
        {
            if !water.reflect_entities {
                for items in [
                    &mut plans.world.opaque,
                    &mut plans.world.translucent,
                    &mut plans.world.additive,
                    &mut plans.world.planar,
                ] {
                    items.retain(|item| !upload.meshes[item.mesh_index].entity);
                }
            }
            if !water.reflect_3d_skybox {
                plans.map_skybox = None;
            }
        }
        let sun_skybox = skybox.filter(|_| plans.map_skybox.is_some());
        self.view.plans = Some(plans);
        super::transparency::prepare(&mut self.view, upload, main, skybox, water.map(|_| false));
        RenderTargets::ensure(&mut self.view.targets, device, resources, size);
        // Legacy env_sun overlays are part of GMod's reflected sky scene. Keep
        // their visibility history and depth queries local to this view, using
        // its reflected camera and the skybox plan actually rendered here.
        self.view.sun.prepare(
            device,
            queue,
            &resources.sun,
            &super::sun::gpu::Inputs {
                map: primitive.preview.map(),
                world: main,
                skybox: sun_skybox,
                targets: self.view.targets.as_ref().unwrap(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
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
        assert!(above.intersects_near_plane(uniforms));
        uniforms.camera_position[2] = 2.0;
        assert!(!above.intersects_near_plane(uniforms));
        uniforms.camera_position[2] = -1.0;
        let below = Plane {
            normal: -above.normal,
            distance: 0.0,
        };
        assert!(below.intersects_near_plane(uniforms));
        uniforms.camera_position[2] = -2.0;
        assert!(!below.intersects_near_plane(uniforms));
    }
    use super::{Plane, Vec3, planar_groups};
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
        // A reflection reverses winding: exactly one basis axis changes sign
        // for this axis-aligned case, so reflected views must reverse culling.
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
            normal,
            uv: [0.0; 2],
            lightmap_uv: [0.0; 2],
            lightmap_bump_step: 0.0,
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
