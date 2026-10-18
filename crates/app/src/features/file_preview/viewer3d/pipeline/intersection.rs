use super::reflections::{Plane, SceneView};
use super::transparency::WaterSide;
use super::uniforms::{FogUniform, Uniforms};
use super::{ModelPrimitive, PreparedFrame, RenderResources, Vec3, wgpu};
use gmpublished_domain::scene::map::MapBounds;

#[derive(Debug)]
pub(super) struct IntersectionView {
    pub view: SceneView,
    pub selection: Option<Selection>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Selection {
    pub surface_index: usize,
    pub body_index: usize,
}

const NEAR_VOLUME_MARGIN: f32 = 7.0;

fn near_bounds(uniforms: &Uniforms) -> MapBounds {
    let row = |index| {
        Vec3::new(
            uniforms.view_proj[0][index],
            uniforms.view_proj[1][index],
            uniforms.view_proj[2][index],
        )
    };
    let x = row(0);
    let y = row(1);
    let forward = row(3).normalize_or_zero();
    let eye = Vec3::new(
        uniforms.camera_position[0],
        uniforms.camera_position[1],
        uniforms.camera_position[2],
    );
    let center = eye + forward * uniforms.water_depth_params[0];
    let extent = (Vec3::from(x.to_array().map(f32::abs)) / x.length_squared()
        + Vec3::from(y.to_array().map(f32::abs)) / y.length_squared())
        * uniforms.water_depth_params[0]
        + Vec3::splat(NEAR_VOLUME_MARGIN);
    MapBounds {
        min: center - extent,
        max: center + extent,
    }
}

struct SurfaceCandidate<'a> {
    surface_index: usize,
    plane: Plane,
    material_name: &'a str,
    water_side: Option<WaterSide>,
    refracts: bool,
}

fn select_surface<'a>(
    candidates: impl Iterator<Item = SurfaceCandidate<'a>>,
    samples: &[gmpublished_domain::scene::map::MapWaterSample<'_>],
    bounds: MapBounds,
) -> Option<Selection> {
    candidates
        .filter(|candidate| candidate.refracts && candidate.water_side == Some(WaterSide::Above))
        .find_map(|candidate| {
            samples
                .iter()
                .find(|sample| {
                    sample.body.material_name == candidate.material_name
                        && sample.body.surface_z > bounds.min[2]
                        && sample.body.surface_z < bounds.max[2]
                        && candidate
                            .plane
                            .signed_distance(Vec3::new(0.0, 0.0, sample.body.surface_z))
                            .abs()
                            < 0.01
                })
                .map(|sample| Selection {
                    surface_index: candidate.surface_index,
                    body_index: sample.body_index,
                })
        })
}

fn select(frame: &PreparedFrame, primitive: &ModelPrimitive) -> Option<Selection> {
    if primitive.submerged {
        return None;
    }
    let map = primitive.preview.map()?;
    let visibility = map.visibility.as_ref()?;
    let volumes = map.water_volumes.as_ref()?;
    let bounds = near_bounds(&primitive.uniforms);
    let leaves = visibility.leaves_for_aabb(bounds)?;
    let samples = leaves
        .iter()
        .filter_map(|&leaf| volumes.at_leaf(leaf))
        .collect::<Vec<_>>();
    select_surface(
        frame.planar_views.iter().map(|view| {
            let surface = &frame.scene.assets.reflectors[view.surface_index];
            let material = &primitive.preview.render_scene().materials[surface.material_slot];
            SurfaceCandidate {
                surface_index: view.surface_index,
                plane: surface.plane,
                material_name: &material.name,
                water_side: material
                    .shader
                    .water()
                    .map(|water| WaterSide::from_submerged(!water.above_water)),
                refracts: view.refraction.is_some(),
            }
        }),
        &samples,
        bounds,
    )
}

pub(super) fn prepare(
    frame: &mut PreparedFrame,
    primitive: &ModelPrimitive,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    resources: &RenderResources,
    size: (u32, u32),
) -> Option<Plane> {
    let Some(selected) = select(frame, primitive) else {
        if let Some(intersection) = &mut frame.water_intersection {
            intersection.selection = None;
            intersection.view.plans = None;
            intersection.view.transparency.clear();
        }
        return None;
    };
    let upload = &frame.scene;
    let source = &upload.assets.reflectors[selected.surface_index];
    let plane = source.plane;
    let material = &primitive.preview.render_scene().materials[source.material_slot];
    let water = material.shader.water().expect("selected water material");
    let intersection = frame
        .water_intersection
        .get_or_insert_with(|| IntersectionView {
            view: SceneView::new(device, resources, &frame.lighting, &frame.marks.buffer),
            selection: Some(selected),
        });
    intersection.selection = Some(selected);
    let view = &mut intersection.view;
    let mut uniforms = primitive
        .uniforms
        .with_raster_offset(size, primitive.raster_offset());
    uniforms.water_time_sky_tint[1..].copy_from_slice(upload.assets.sky_tint.as_array());
    let clip =
        super::water_clip::world_plane(plane, super::water_clip::WorldWaterPass::Intersection);
    uniforms.clip_plane = [
        clip.normal[0],
        clip.normal[1],
        clip.normal[2],
        -clip.distance,
    ];
    uniforms.medium_plane = [
        plane.normal[0],
        plane.normal[1],
        plane.normal[2],
        -plane.distance,
    ];
    uniforms.fog = FogUniform::height(water);
    uniforms.fog_color = [
        water.scene_fog_color_linear[0],
        water.scene_fog_color_linear[1],
        water.scene_fog_color_linear[2],
        f32::from(water.fog_enabled),
    ];
    uniforms = uniforms.with_clip_camera(primitive.view_camera);
    view.world.write(queue, &uniforms);
    let plans = frame.main.plans.as_ref().expect("prepared main plans");
    view.plans = Some(super::DrawPlans {
        content_id: plans.content_id,
        world: plans.world.clone(),
        map_skybox: None,
    });
    if let Some(map) = primitive.preview.map() {
        view.visibility.ensure_world_visibility(
            device,
            queue,
            map,
            upload,
            primitive.visibility_culling,
            Vec3::new(
                uniforms.camera_position[0],
                uniforms.camera_position[1],
                uniforms.camera_position[2],
            ),
        );
    }
    super::transparency::prepare(view, upload, &uniforms, None, Some(WaterSide::Under), false);
    super::planar::prepare(
        view,
        upload,
        primitive.preview.render_scene(),
        device,
        queue,
        Some(plane),
    );
    super::transparent_indirect::prepare(view, upload, device, queue);
    Some(super::water_clip::world_plane(
        plane,
        super::water_clip::WorldWaterPass::Main,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use gmpublished_domain::scene::map::{MapWaterBody, MapWaterSample};

    fn body(height: f32, material: &str) -> MapWaterBody {
        MapWaterBody {
            surface_z: height,
            min_z: height - 64.0,
            material_name: material.to_owned(),
        }
    }

    fn sample(body_index: usize, body: &MapWaterBody) -> MapWaterSample<'_> {
        MapWaterSample {
            body_index,
            body,
            contents: 32,
        }
    }

    fn water(surface_index: usize, height: f32, material_name: &str) -> SurfaceCandidate<'_> {
        SurfaceCandidate {
            surface_index,
            plane: Plane {
                normal: Vec3::new(0.0, 0.0, 1.0),
                distance: height,
            },
            material_name,
            water_side: Some(WaterSide::Above),
            refracts: true,
        }
    }

    fn bounds() -> MapBounds {
        MapBounds {
            min: Vec3::splat(-7.0),
            max: Vec3::splat(7.0),
        }
    }

    #[test]
    fn coplanar_mirror_order_does_not_replace_selected_water_identity() {
        let body = body(0.0, "water");
        let mirror = || SurfaceCandidate {
            water_side: None,
            ..water(2, 0.0, "mirror")
        };
        for candidates in [
            vec![mirror(), water(9, 0.0, "water")],
            vec![water(9, 0.0, "water"), mirror()],
        ] {
            assert_eq!(
                select_surface(candidates.into_iter(), &[sample(41, &body)], bounds()),
                Some(Selection {
                    surface_index: 9,
                    body_index: 41,
                })
            );
        }
    }

    #[test]
    fn coplanar_water_materials_bind_the_matching_compiled_body() {
        let body = body(0.0, "pool_b");
        assert_eq!(
            select_surface(
                [water(3, 0.0, "pool_a"), water(8, 0.0, "pool_b")].into_iter(),
                &[sample(27, &body)],
                bounds(),
            ),
            Some(Selection {
                surface_index: 8,
                body_index: 27,
            })
        );
    }

    #[test]
    fn unrelated_non_refracting_water_does_not_disable_local_intersection() {
        let body = body(0.0, "pool");
        let selected = select_surface(
            [
                SurfaceCandidate {
                    refracts: false,
                    ..water(3, 0.0, "pool")
                },
                water(8, 0.0, "pool"),
            ]
            .into_iter(),
            &[sample(27, &body)],
            bounds(),
        );
        assert_eq!(
            selected,
            select_surface(
                [water(8, 0.0, "pool")].into_iter(),
                &[sample(27, &body)],
                bounds(),
            )
        );
        assert!(selected.is_some());
    }

    #[test]
    fn distant_body_with_the_same_material_does_not_select_its_plane() {
        let distant = body(48.0, "pool");
        let local = body(0.0, "pool");
        assert_eq!(
            select_surface(
                [water(3, 48.0, "pool"), water(8, 0.0, "pool")].into_iter(),
                &[sample(27, &distant), sample(41, &local)],
                bounds(),
            ),
            Some(Selection {
                surface_index: 8,
                body_index: 41,
            })
        );
    }

    #[test]
    fn underwater_surface_material_does_not_select_an_above_water_pass() {
        let body = body(0.0, "pool");
        assert_eq!(
            select_surface(
                [SurfaceCandidate {
                    water_side: Some(WaterSide::Under),
                    ..water(3, 0.0, "pool")
                }]
                .into_iter(),
                &[sample(27, &body)],
                bounds(),
            ),
            None
        );
    }

    #[test]
    fn seven_unit_near_volume_margin_has_strict_crossing_boundaries() {
        let mut uniforms = Uniforms::zeroed();
        uniforms.view_proj = super::super::mat_mul(
            super::super::perspective(0.7, 1.0, 4.0, 1024.0),
            super::super::look_to(
                Vec3::ZERO,
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(0.0, 1.0, 0.0),
            ),
        );
        uniforms.water_depth_params[0] = 4.0;
        let bounds = near_bounds(&uniforms);
        assert_eq!((bounds.min[2], bounds.max[2]), (-3.0, 11.0));
        for (height, intersects) in [(-3.0, false), (-2.999, true), (10.999, true), (11.0, false)] {
            let body = body(height, "pool");
            assert_eq!(
                select_surface(
                    [water(8, height, "pool")].into_iter(),
                    &[sample(41, &body)],
                    bounds,
                )
                .is_some(),
                intersects,
                "height {height}"
            );
        }
    }

    fn inverse(matrix: [[f32; 4]; 4]) -> [[f64; 4]; 4] {
        let mut rows: [[f64; 8]; 4] = std::array::from_fn(|row| {
            std::array::from_fn(|col| {
                if col < 4 {
                    f64::from(matrix[col][row])
                } else {
                    f64::from(col - 4 == row)
                }
            })
        });
        for col in 0..4 {
            let pivot = (col..4)
                .max_by(|&a, &b| rows[a][col].abs().total_cmp(&rows[b][col].abs()))
                .unwrap();
            rows.swap(col, pivot);
            let divisor = rows[col][col];
            assert!(divisor.abs() > 1e-12);
            rows[col].iter_mut().for_each(|value| *value /= divisor);
            let pivot_values = rows[col];
            for (row_index, row) in rows.iter_mut().enumerate() {
                if row_index != col {
                    let factor = row[col];
                    for (value, pivot) in row.iter_mut().zip(pivot_values) {
                        *value -= factor * pivot;
                    }
                }
            }
        }
        std::array::from_fn(|row| std::array::from_fn(|col| rows[row][col + 4]))
    }

    #[test]
    fn near_bounds_match_unprojected_corners_across_pitch_roll_and_near_distance() {
        for (pitch, roll) in [(0.0_f32, 0.0_f32), (37.0, 23.0), (-61.0, 109.0)] {
            let (sin_pitch, cos_pitch) = pitch.to_radians().sin_cos();
            let direction = Vec3::new(cos_pitch, 0.0, sin_pitch);
            let right = direction
                .cross(Vec3::new(0.0, 0.0, 1.0))
                .normalize()
                .unwrap();
            let up = right.cross(direction);
            let (sin_roll, cos_roll) = roll.to_radians().sin_cos();
            let up = up * cos_roll + right * sin_roll;
            let eye = Vec3::new(1024.0, -2304.0, 160.0);
            for near in [0.125_f32, 4.0, 128.0] {
                let mut uniforms = Uniforms::zeroed();
                uniforms.camera_position = [eye[0], eye[1], eye[2], 0.0];
                uniforms.water_depth_params[0] = near;
                uniforms.view_proj = super::super::mat_mul(
                    super::super::perspective(1.1, 16.0 / 9.0, near, 32768.0),
                    super::super::look_to(eye, direction, up),
                );
                let inverse = inverse(uniforms.view_proj);
                let mut minimum = [f64::INFINITY; 3];
                let mut maximum = [f64::NEG_INFINITY; 3];
                for x in [-1.0, 1.0] {
                    for y in [-1.0, 1.0] {
                        let clip = [x, y, 1.0, 1.0];
                        let point: [f64; 4] = std::array::from_fn(|row| {
                            (0..4).map(|col| inverse[row][col] * clip[col]).sum()
                        });
                        for axis in 0..3 {
                            minimum[axis] = minimum[axis].min(point[axis] / point[3] - 7.0);
                            maximum[axis] = maximum[axis].max(point[axis] / point[3] + 7.0);
                        }
                    }
                }
                let bounds = near_bounds(&uniforms);
                for axis in 0..3 {
                    assert!((f64::from(bounds.min[axis]) - minimum[axis]).abs() < 0.002);
                    assert!((f64::from(bounds.max[axis]) - maximum[axis]).abs() < 0.002);
                }
            }
        }
    }
}
