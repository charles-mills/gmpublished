use gmpublished_domain::{math::Vec3, scene::map::AmbientCube};
use std::sync::Arc;

#[path = "model_lighting/cpu.rs"]
mod cpu;
pub use cpu::factory::{ModelLightingFailure, ModelLightingSeed};
#[path = "model_lighting/origin.rs"]
mod origin;
pub use origin::{ModelLightingOriginFailure, placement_sample_point, placement_static_bounds};
#[path = "model_lighting/boost.rs"]
mod boost;
#[path = "model_lighting/history.rs"]
mod history;
#[path = "model_lighting/sphere.rs"]
mod sphere;
#[path = "model_lighting/styles.rs"]
mod styles;
pub use history::ModelLightingHistory;

#[derive(Clone, Debug, PartialEq)]
pub struct ModelLightingSceneInputs {
    pub ambient: gmpublished_domain::scene::map::MapAmbientLighting,
    pub world_lighting: gmpublished_domain::scene::map::MapWorldLighting,
    pub visibility: Option<gmpublished_domain::scene::map::MapVisibility>,
    pub collision: Option<gmpublished_domain::scene::map::MapLightingCollision>,
    pub radiosity_surfaces: Option<gmpublished_domain::scene::map::MapRadiositySurfaces>,
    pub surface_materials: Vec<Option<crate::bridge::materials::ResolvedSurfaceLighting>>,
}

impl ModelLightingSceneInputs {
    pub fn seed_static(&self, contexts: &mut [ModelLightingContext]) {
        let sampler = sphere::Sampler::new(self);
        self.seed_static_with(contexts, |point| {
            sampler.as_ref().map_err(|error| *error)?.sample(point)
        });
    }
    pub fn style_values(&self, time: f64) -> [f32; 128] {
        styles::model_light_style_values(&self.world_lighting.light_style_entities, time)
    }

    pub fn is_animated(&self) -> bool {
        self.world_lighting.selected().is_some_and(|lights| {
            styles::model_light_styles_animated(
                &self.world_lighting.light_style_entities,
                &lights.lights,
            )
        })
    }
    pub fn seed_static_with(
        &self,
        contexts: &mut [ModelLightingContext],
        mut spherical_ambient: impl FnMut(Vec3) -> Result<AmbientCube, ModelLightingFailure>,
    ) {
        let factory = self
            .collision
            .as_ref()
            .ok_or(ModelLightingFailure::MissingCollision)
            .and_then(|collision| {
                cpu::factory::ModelLightingFactory::new(
                    &self.ambient,
                    &self.world_lighting,
                    collision,
                    self.visibility.as_ref(),
                )
            });
        for context in contexts.iter_mut().filter(|context| !context.entity) {
            let Ok(point) = context.sample_point else {
                continue;
            };
            context.cpu_seed = context
                .static_world_bounds
                .ok_or(ModelLightingFailure::InvalidPosition)
                .and_then(|bounds| {
                    let factory = factory.as_ref().map_err(|error| *error)?;
                    factory.seed_static(point, bounds, spherical_ambient(point)?)
                })
                .map(Some);
            if let Err(error) = context.cpu_seed {
                log::warn!(
                    "map static model lighting {:?} at {:?}: {error:?}",
                    context.model_path,
                    point
                );
            }
        }
    }

    pub fn seed_entities(&self, contexts: &mut [ModelLightingContext]) {
        let mut factory = self
            .collision
            .as_ref()
            .ok_or(ModelLightingFailure::MissingCollision)
            .and_then(|collision| {
                cpu::factory::ModelLightingFactory::new(
                    &self.ambient,
                    &self.world_lighting,
                    collision,
                    self.visibility.as_ref(),
                )
            });
        for context in contexts.iter_mut().filter(|context| context.entity) {
            let Ok(point) = context.sample_point else {
                continue;
            };
            context.cpu_seed = match &mut factory {
                Ok(factory) => factory.seed_dynamic(point).map(Some),
                Err(error) => Err(*error),
            };
            if let Err(error) = context.cpu_seed {
                log::warn!(
                    "map model lighting {:?} at {:?}: {error:?}",
                    context.model_path,
                    point
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelLightKind {
    Directional,
    Point,
    Spot,
}

impl TryFrom<vformats::bsp::WorldLightKind> for ModelLightKind {
    type Error = vformats::bsp::WorldLightKind;

    fn try_from(kind: vformats::bsp::WorldLightKind) -> Result<Self, Self::Error> {
        use vformats::bsp::WorldLightKind;
        match kind {
            WorldLightKind::Point => Ok(Self::Point),
            WorldLightKind::Surface | WorldLightKind::Spotlight => Ok(Self::Spot),
            WorldLightKind::Skylight => Ok(Self::Directional),
            // Legacy Quake lights participate in CPU selection/folding, but
            // Source omits them when converting selected hardware descriptors.
            other => Err(other),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelLocalLight {
    pub kind: ModelLightKind,
    pub position: Vec3,
    pub direction: Vec3,
    pub radiance: Vec3,
    pub attenuation: [f32; 3],
    pub range: f32,
    pub spot_exponent: f32,
    pub inner_cosine: f32,
    pub outer_cosine: f32,
}

/// One placement's directional lighting, independent of merged material batches.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelLightingContext {
    pub mdl_flags: u32,
    pub compiled_lighting: bool,
    pub model_path: String,
    pub origin: Vec3,
    pub lighting_origin: Vec3,
    pub entity: bool,
    pub sample_point: Result<Vec3, ModelLightingOriginFailure>,
    pub static_world_bounds: Option<gmpublished_domain::scene::map::MapBounds>,
    pub lighting_origin_reference:
        Option<gmpublished_domain::scene::map::MapLightingOriginReference>,
    pub cpu_seed: Result<Option<Arc<ModelLightingSeed>>, ModelLightingFailure>,
    pub diffuse_modulation: [u8; 4],
    pub render_mode: i32,
    pub ambient_cube: AmbientCube,
    pub lights: Vec<ModelLocalLight>,
}

impl ModelLightingContext {
    #[cfg(test)]
    pub(crate) fn ambient_fixture(value: f32) -> Self {
        let ambient_cube = AmbientCube {
            colors: [Vec3::splat(value); 6],
        };
        Self {
            mdl_flags: 0,
            compiled_lighting: false,
            model_path: "models/test/ambient.mdl".into(),
            origin: Vec3::ZERO,
            lighting_origin: Vec3::ZERO,
            entity: true,
            sample_point: Ok(Vec3::ZERO),
            static_world_bounds: None,
            lighting_origin_reference: None,
            cpu_seed: Ok(Some(Arc::new(ModelLightingSeed::ambient_fixture(
                ambient_cube,
            )))),
            diffuse_modulation: [255; 4],
            render_mode: 0,
            ambient_cube,
            lights: Vec::new(),
        }
    }

    pub fn evaluate(
        &self,
        styles: &[f32; 128],
    ) -> Result<Option<(AmbientCube, Vec<ModelLocalLight>)>, ModelLightingFailure> {
        let Some(seed) = self.cpu_seed.as_ref().map_err(|error| *error)? else {
            return Ok(None);
        };
        let evaluated = seed.evaluate(styles, 4, 0.0002)?;
        Ok(Some(self.finish_evaluation(&evaluated)))
    }

    pub fn evaluate_with_history(
        &self,
        styles: &[f32; 128],
        time: f64,
        history: &mut ModelLightingHistory,
    ) -> Result<Option<(AmbientCube, Vec<ModelLocalLight>)>, ModelLightingFailure> {
        let Some(seed) = self.cpu_seed.as_ref().map_err(|error| *error)? else {
            return Ok(None);
        };
        let evaluated = seed.evaluate(styles, 4, 0.0002)?;
        let evaluated = if self.entity {
            history.update(time, evaluated, styles)?
        } else {
            evaluated
        };
        Ok(Some(self.finish_evaluation(&evaluated)))
    }

    fn finish_evaluation(
        &self,
        evaluated: &cpu::factory::ModelLightingEvaluation,
    ) -> (AmbientCube, Vec<ModelLocalLight>) {
        let ambient = boost::model_draw_ambient(
            self.mdl_flags,
            self.lighting_origin,
            evaluated.ambient_cube,
            &evaluated.selected,
        );
        let lights = evaluated
            .selected
            .iter()
            .filter_map(|selected| {
                let light = selected.light;
                let kind = ModelLightKind::try_from(light.kind).ok()?;
                let surface = light.kind == vformats::bsp::WorldLightKind::Surface;
                Some(ModelLocalLight {
                    kind,
                    position: Vec3::from(light.origin),
                    direction: Vec3::from(light.normal),
                    radiance: Vec3::from(light.intensity) * selected.style_weight,
                    attenuation: if surface {
                        [0.0, 0.0, 1.0]
                    } else {
                        [
                            light.constant_attenuation,
                            light.linear_attenuation,
                            light.quadratic_attenuation,
                        ]
                    },
                    range: light.radius,
                    spot_exponent: if surface { 1.0 } else { light.exponent },
                    inner_cosine: if surface { 0.0 } else { light.stopdot },
                    outer_cosine: if surface { 0.0 } else { light.stopdot2 },
                })
            })
            .collect();
        (ambient, lights)
    }
}
