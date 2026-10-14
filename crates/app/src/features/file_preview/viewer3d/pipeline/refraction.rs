//! Refraction uses the real camera and its own clipped scene/depth, never a
//! copy of the main opaque image. Visibility and optical state are view-local.
use super::super::draw_plan::{DrawPlanRequest, DrawPlanSelection, prepare_draw_plans};
use super::reflections::{PlanarView, Plane, SceneView};
use super::targets::RenderTargets;
use super::visibility::upload_visible_indices;
use super::{ModelPrimitive, RenderResources, UploadedModel, Vec3, wgpu};

#[derive(Clone, Copy)]
pub struct OpticalInputs<'a> {
    pub size: (u32, u32),
    pub lightmap: &'a super::lightmaps::Frame,
}
impl PlanarView {
    pub fn prepare_optics(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        primitive: &ModelPrimitive,
        upload: &UploadedModel,
        inputs: OpticalInputs<'_>,
    ) {
        let OpticalInputs { size, lightmap } = inputs;
        let source = &upload.reflectors[self.surface_index];
        let shader = &primitive.preview.render_scene().materials[source.material_slot].shader;
        let (reflect, refract) = super::reflections::optical_passes(shader);
        self.reflect_enabled = reflect;
        if self.reflect_enabled {
            self.prepare(device, queue, resources, primitive, upload, size);
        } else {
            self.view.targets = None;
            self.view.plans = None;
        }
        if !refract {
            self.refraction = None;
            return;
        }
        let view = self
            .refraction
            .get_or_insert_with(|| SceneView::new(device, resources, lightmap));
        let mut uniforms = primitive.uniforms;
        uniforms.water_time_sky_tint[1..].copy_from_slice(upload.sky_tint.as_array());
        let plane = source.plane;
        let below_water = shader.water().is_some_and(|water| !water.above_water);
        // Source's above-water refraction view clips two units above the
        // exact water plane to avoid shoreline cracks (DF_FUDGE_UP).
        let clip_bias = if below_water {
            0.0
        } else if shader.water().is_some() {
            2.0
        } else {
            0.1
        };
        uniforms.clip_plane = [
            -plane.normal[0],
            -plane.normal[1],
            -plane.normal[2],
            plane.distance + clip_bias,
        ];
        uniforms.medium_plane = [
            plane.normal[0],
            plane.normal[1],
            plane.normal[2],
            -plane.distance,
        ];
        self.refraction_clear = wgpu::Color::BLACK;
        if below_water {
            // Looking out of water captures the air scene with its own world
            // fog. The underside material applies the in-water segment later.
            uniforms.medium_plane = [0.0; 4];
            uniforms.fog_color = primitive.air_fog.map_or([0.0; 4], |fog| {
                [
                    fog.color_linear[0],
                    fog.color_linear[1],
                    fog.color_linear[2],
                    0.0,
                ]
            });
            uniforms.fog_params = primitive.air_fog.map_or([0.0, 1.0, 0.0, 0.0], |fog| {
                [
                    fog.start,
                    fog.end,
                    fog.max_density,
                    if fog.radial { 4.0 } else { 1.0 },
                ]
            });
        } else if let Some(water) = shader.water() {
            uniforms.fog_color = [
                water.fog_color_linear[0],
                water.fog_color_linear[1],
                water.fog_color_linear[2],
                f32::from(water.fog_enabled),
            ];
            uniforms.fog_params = [water.fog_start, water.fog_end, 1.0, 2.0];
            self.refraction_clear = wgpu::Color {
                r: f64::from(water.fog_clear_linear[0]),
                g: f64::from(water.fog_clear_linear[1]),
                b: f64::from(water.fog_clear_linear[2]),
                a: 1.0,
            };
        }
        view.world.write(queue, &uniforms);
        if let Some(sky) = primitive.sky_uniforms {
            view.sky.write(queue, &sky);
        }
        let skybox = primitive.map_skybox_uniforms.map(|mut skybox| {
            let (origin, scale) = primitive.skybox_transform.unwrap_or((Vec3::ZERO, 16.0));
            let distance = plane.distance / scale + plane.normal.dot(origin);
            skybox.clip_plane = [
                -plane.normal[0],
                -plane.normal[1],
                -plane.normal[2],
                distance + clip_bias / scale,
            ];
            skybox
        });
        if let Some(skybox) = skybox {
            view.skybox.write(queue, &skybox);
        }
        if let Some(map) = primitive.preview.map() {
            view.visibility.ensure_world_visibility(
                device,
                queue,
                map,
                upload,
                primitive.visibility_culling,
                Vec3::from([
                    uniforms.camera_position[0],
                    uniforms.camera_position[1],
                    uniforms.camera_position[2],
                ]),
            );
        }
        let plans = prepare_draw_plans(
            upload,
            DrawPlanRequest {
                content_id: primitive.content_id,
                selection: DrawPlanSelection {
                    skin_remap: &primitive.skin_remap,
                    bodygroup_choices: &primitive.bodygroup_choices,
                },
                camera_position: uniforms.camera_position,
                map_skybox_visible: primitive.map_skybox_visible
                    && (shader.water().is_none() || below_water),
                map_skybox_content_present: upload.has_map_skybox_content(),
                map_skybox_camera_position: skybox.map(|uniforms| uniforms.camera_position),
                visibility_plan: view.visibility.plan.as_ref(),
            },
        );
        // Exclude the source plane without dropping other heights/orientations
        // batched into the same material mesh. The original PVS plan remains
        // intact so a later visibility rebuild can safely repeat this filter.
        let generation = view.visibility.tracker.rebuild_count;
        if view.planar_exclusion_generation != Some(generation) {
            let scene = primitive.preview.render_scene();
            for item in &plans.world.planar {
                let uploaded = &upload.meshes[item.mesh_index];
                if uploaded.door_index.is_some() {
                    continue;
                }
                let Some(mesh) = scene.meshes.get(uploaded.scene_mesh_index) else {
                    continue;
                };
                let indices =
                    view.visibility
                        .plan
                        .as_ref()
                        .map_or(mesh.indices.as_slice(), |plan| {
                            plan.mesh_indices
                                .get(uploaded.scene_mesh_index)
                                .map_or(&[], Vec::as_slice)
                        });
                let selected = without_plane(&mesh.vertices, indices, plane);
                view.visibility.mesh_indices.insert(
                    item.mesh_index,
                    upload_visible_indices(device, queue, &selected),
                );
            }
            view.planar_exclusion_generation = Some(generation);
        }
        view.plans = Some(plans);
        super::transparency::prepare(
            view,
            upload,
            uniforms,
            skybox,
            shader.water().map(|water| water.above_water),
        );
        RenderTargets::ensure(&mut view.targets, device, resources, size);
    }
}

fn without_plane(vertices: &[super::ModelVertex], indices: &[u32], plane: Plane) -> Vec<u32> {
    indices
        .chunks_exact(3)
        .filter(|triangle| {
            Plane::from_triangle(vertices, triangle).is_none_or(|candidate| {
                !candidate.same(plane)
                    && !candidate.same(Plane {
                        normal: -plane.normal,
                        distance: -plane.distance,
                    })
            })
        })
        .flatten()
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusion_keeps_a_second_water_height_in_the_same_material_mesh() {
        let make = |z| super::super::ModelVertex {
            position: Vec3::new(0.0, 0.0, z),
            normal: Vec3::new(0.0, 0.0, 1.0),
            uv: [0.0; 2],
            lightmap_uv: [0.0; 2],
            lightmap_bump_step: 0.0,
            color: Vec3::splat(1.0),
            blend_alpha: 0.0,
        };
        let mut vertices = vec![make(-160.0); 3];
        vertices[1].position[0] = 64.0;
        vertices[2].position[1] = 64.0;
        let mut lower = vertices.clone();
        for vertex in &mut lower {
            vertex.position[2] = -400.0;
        }
        vertices.extend(lower);
        assert_eq!(
            without_plane(
                &vertices,
                &[0, 1, 2, 3, 4, 5],
                Plane {
                    normal: Vec3::new(0.0, 0.0, 1.0),
                    distance: -160.0
                }
            ),
            vec![3, 4, 5]
        );
    }
}
