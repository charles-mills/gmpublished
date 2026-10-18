use super::reflections::{PlanarView, SceneView};
use super::targets::RenderTargets;
use super::uniforms::FogUniform;
use super::{ModelPrimitive, RenderResources, Vec3, ViewerScene, wgpu};

#[derive(Clone, Copy)]
pub struct OpticalInputs<'a> {
    pub size: (u32, u32),
    pub surface_marks: &'a wgpu::Buffer,
    pub lighting: &'a super::lighting::Frame,
}
impl PlanarView {
    pub fn prepare_optics(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        primitive: &ModelPrimitive,
        upload: &ViewerScene,
        inputs: OpticalInputs<'_>,
    ) {
        let OpticalInputs {
            size,
            lighting,
            surface_marks,
        } = inputs;
        let source = &upload.assets.reflectors[self.surface_index];
        let shader = &primitive.preview.render_scene().materials[source.material_slot].shader;
        let (reflect, refract) = super::reflections::optical_passes(shader);
        if reflect {
            self.reflection
                .get_or_insert_with(|| SceneView::new(device, resources, lighting, surface_marks));
            self.prepare_reflection(device, queue, resources, primitive, upload, size);
        } else {
            self.reflection = None;
        }
        if !refract {
            self.refraction = None;
            return;
        }
        let input =
            self.prepare_refraction_input(device, queue, resources, primitive, upload, inputs);
        super::optical_planning::prepare_view(
            self.refraction.as_mut().expect("initialized refraction"),
            &input,
            upload,
            super::optical_planning::Common::new(primitive, upload),
            false,
        );
        self.finish_refraction(device, queue, resources, primitive, upload, &input);
    }

    pub(super) fn prepare_refraction_input(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        primitive: &ModelPrimitive,
        upload: &ViewerScene,
        inputs: OpticalInputs<'_>,
    ) -> super::optical_planning::Input {
        let OpticalInputs {
            size,
            lighting,
            surface_marks,
        } = inputs;
        let source = &upload.assets.reflectors[self.surface_index];
        let shader = &primitive.preview.render_scene().materials[source.material_slot].shader;
        let view = self
            .refraction
            .get_or_insert_with(|| SceneView::new(device, resources, lighting, surface_marks));
        let mut uniforms = primitive
            .uniforms
            .with_raster_offset(size, primitive.raster_offset());
        uniforms.water_time_sky_tint[1..].copy_from_slice(upload.assets.sky_tint.as_array());
        let plane = source.plane;
        let below_water = shader.water().is_some_and(|water| !water.above_water);
        let skybox_clip_bias = if below_water {
            0.0
        } else if shader.water().is_some() {
            super::water_clip::WATER_CLIP_SPREAD
        } else {
            0.1
        };
        let clip = if shader.water().is_some() {
            super::water_clip::world_plane(plane, super::water_clip::WorldWaterPass::Refraction)
        } else {
            super::reflections::Plane {
                normal: -plane.normal,
                distance: -plane.distance - 0.1,
            }
        };
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
        self.refraction_clear = wgpu::Color::BLACK;
        if below_water {
            // The air scene and underwater segment apply their own fog separately.
            uniforms.medium_plane = [0.0; 4];
            uniforms.fog_color = primitive.air_fog.map_or([0.0; 4], |fog| {
                [
                    fog.color_linear[0],
                    fog.color_linear[1],
                    fog.color_linear[2],
                    0.0,
                ]
            });
            uniforms.fog = FogUniform::for_map(primitive.air_fog, 1.0);
        } else if let Some(water) = shader.water() {
            uniforms.fog_color = [
                water.scene_fog_color_linear[0],
                water.scene_fog_color_linear[1],
                water.scene_fog_color_linear[2],
                f32::from(water.fog_enabled),
            ];
            uniforms.fog = FogUniform::height(water);
            self.refraction_clear = wgpu::Color {
                r: f64::from(water.scene_fog_color_linear[0]),
                g: f64::from(water.scene_fog_color_linear[1]),
                b: f64::from(water.scene_fog_color_linear[2]),
                a: 1.0,
            };
        }
        uniforms = uniforms.with_clip_camera(primitive.view_camera);
        view.world.write(queue, &uniforms);
        if let Some(sky) = primitive.sky_uniforms {
            view.sky.write(
                queue,
                &sky.with_raster_offset(size, primitive.raster_offset()),
            );
        }
        let skybox = primitive.map_skybox_uniforms.map(|mut skybox| {
            let (origin, scale) = primitive.skybox_transform.unwrap_or((Vec3::ZERO, 16.0));
            let distance = plane.distance / scale + plane.normal.dot(origin);
            skybox.clip_plane = [
                -plane.normal[0],
                -plane.normal[1],
                -plane.normal[2],
                distance + skybox_clip_bias / scale,
            ];
            skybox
                .with_clip_camera(
                    primitive
                        .view_camera
                        .map(|camera| camera.skybox(origin, scale)),
                )
                .with_raster_offset(size, primitive.raster_offset())
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
        super::optical_planning::Input {
            world: uniforms,
            skybox,
            skybox_visible: primitive.map_skybox_visible
                && (shader.water().is_none() || below_water),
            entities: super::view_preparation::Entities::All,
            water_side: shader
                .water()
                .map(|water| super::transparency::WaterSide::from_submerged(water.above_water)),
            include_water: shader.water().is_none(),
            kind: super::draw::ViewKind::Refraction,
            excluded_plane: Some(plane),
            size,
        }
    }

    pub(super) fn finish_refraction(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &RenderResources,
        primitive: &ModelPrimitive,
        upload: &ViewerScene,
        input: &super::optical_planning::Input,
    ) {
        let view = self.refraction.as_mut().expect("initialized refraction");
        super::planar::prepare(
            view,
            upload,
            primitive.preview.render_scene(),
            device,
            queue,
            input.excluded_plane,
        );
        super::transparent_indirect::prepare(view, upload, device, queue);
        RenderTargets::ensure(
            &mut view.targets,
            device,
            resources.for_view(input.kind),
            input.size,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::planar::without_plane;
    use super::super::reflections::Plane;
    use super::*;
    #[test]
    fn exclusion_keeps_a_second_water_height_in_the_same_material_mesh() {
        let make = |z| super::super::ModelVertex {
            position: Vec3::new(0.0, 0.0, z),
            tangent_s: Vec3::ZERO,
            tangent_t: Vec3::ZERO,
            tangent_sign: 0.0,
            lighting_context: 0,
            surface_mark: 0,
            normal: Vec3::new(0.0, 0.0, 1.0),
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
