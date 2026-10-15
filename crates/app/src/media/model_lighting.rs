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

pub const MODEL_LOCAL_LIGHT_LIMIT: usize = 4;
pub const MODEL_LIGHT_MINIMUM: f32 = 0.0002;

#[derive(Clone, Debug, PartialEq)]
pub struct ModelLightingSnapshot {
    pub ambient_cube: AmbientCube,
    pub lights: Vec<ModelLocalLight>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelLightingFallbackReason {
    LegacyVertexLighting,
    Origin(ModelLightingOriginFailure),
    Preparation(ModelLightingFailure),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModelLightingSource {
    Runtime {
        seed: Arc<ModelLightingSeed>,
        fallback: ModelLightingSnapshot,
    },
    #[cfg(any(test, feature = "map-capture"))]
    Explicit(ModelLightingSnapshot),
    DiagnosedFallback {
        reason: ModelLightingFallbackReason,
        snapshot: ModelLightingSnapshot,
    },
}

impl ModelLightingSource {
    fn is_explicit(&self) -> bool {
        match self {
            #[cfg(any(test, feature = "map-capture"))]
            Self::Explicit(_) => true,
            Self::Runtime { .. } | Self::DiagnosedFallback { .. } => false,
        }
    }

    pub fn runtime_seed(&self) -> Option<&Arc<ModelLightingSeed>> {
        match self {
            Self::Runtime { seed, .. } => Some(seed),
            #[cfg(any(test, feature = "map-capture"))]
            Self::Explicit(_) => None,
            Self::DiagnosedFallback { .. } => None,
        }
    }

    pub fn fallback_snapshot(&self) -> &ModelLightingSnapshot {
        match self {
            Self::Runtime { fallback, .. } => fallback,
            #[cfg(any(test, feature = "map-capture"))]
            Self::Explicit(snapshot) => snapshot,
            Self::DiagnosedFallback { snapshot, .. } => snapshot,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelLightingSceneInputs {
    ambient: gmpublished_domain::scene::map::MapAmbientLighting,
    world_lighting: gmpublished_domain::scene::map::MapWorldLighting,
    visibility: Option<gmpublished_domain::scene::map::MapVisibility>,
    collision: Option<gmpublished_domain::scene::map::MapLightingCollision>,
    radiosity_surfaces: Option<gmpublished_domain::scene::map::MapRadiositySurfaces>,
    surface_materials: Vec<Option<crate::bridge::materials::ResolvedSurfaceLighting>>,
    styles: styles::ModelLightStyles,
}

impl ModelLightingSceneInputs {
    pub fn new(
        ambient: gmpublished_domain::scene::map::MapAmbientLighting,
        world_lighting: gmpublished_domain::scene::map::MapWorldLighting,
        visibility: Option<gmpublished_domain::scene::map::MapVisibility>,
        collision: Option<gmpublished_domain::scene::map::MapLightingCollision>,
        radiosity_surfaces: Option<gmpublished_domain::scene::map::MapRadiositySurfaces>,
        surface_materials: Vec<Option<crate::bridge::materials::ResolvedSurfaceLighting>>,
    ) -> Self {
        let styles = styles::ModelLightStyles::new(&world_lighting.light_style_entities);
        Self {
            ambient,
            world_lighting,
            visibility,
            collision,
            radiosity_surfaces,
            surface_materials,
            styles,
        }
    }

    pub fn ambient(&self) -> &gmpublished_domain::scene::map::MapAmbientLighting {
        &self.ambient
    }

    pub fn seed_static(&self, contexts: &mut [ModelLightingContext]) {
        let sampler = sphere::Sampler::new(self);
        self.seed_static_with(contexts, |point| {
            sampler.as_ref().map_err(|error| *error)?.sample(point)
        });
    }
    pub fn style_values(&self, time: f64) -> [f32; 128] {
        self.styles.values(time)
    }

    pub fn is_animated(&self) -> bool {
        self.world_lighting
            .selected()
            .is_some_and(|lights| self.styles.is_animated(&lights.lights))
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
            if context.source.is_explicit() {
                continue;
            }
            let point = match &context.sample_point {
                Ok(point) => *point,
                Err(error) => {
                    context.set_fallback(ModelLightingFallbackReason::Origin(error.clone()));
                    continue;
                }
            };
            let seed = context
                .static_world_bounds
                .ok_or(ModelLightingFailure::InvalidPosition)
                .and_then(|bounds| {
                    let factory = factory.as_ref().map_err(|error| *error)?;
                    factory.seed_static(point, bounds, spherical_ambient(point)?)
                });
            context.set_prepared_seed(seed);
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
            if context.source.is_explicit() {
                continue;
            }
            let point = match &context.sample_point {
                Ok(point) => *point,
                Err(error) => {
                    context.set_fallback(ModelLightingFallbackReason::Origin(error.clone()));
                    continue;
                }
            };
            let seed = match &mut factory {
                Ok(factory) => factory.seed_dynamic(point),
                Err(error) => Err(*error),
            };
            context.set_prepared_seed(seed);
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
            // Quake emitters fold on the CPU without a hardware descriptor.
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
    pub source: ModelLightingSource,
    pub diffuse_modulation: [u8; 4],
    pub render_mode: i32,
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
            source: ModelLightingSource::Runtime {
                seed: Arc::new(ModelLightingSeed::ambient_fixture(ambient_cube)),
                fallback: ModelLightingSnapshot {
                    ambient_cube,
                    lights: Vec::new(),
                },
            },
            diffuse_modulation: [255; 4],
            render_mode: 0,
        }
    }

    fn set_fallback(&mut self, reason: ModelLightingFallbackReason) {
        log::warn!("model lighting {:?}: {reason:?}", self.model_path);
        self.source = ModelLightingSource::DiagnosedFallback {
            reason,
            snapshot: self.source.fallback_snapshot().clone(),
        };
    }

    fn set_prepared_seed(&mut self, seed: Result<Arc<ModelLightingSeed>, ModelLightingFailure>) {
        match seed {
            Ok(seed) => {
                self.source = ModelLightingSource::Runtime {
                    seed,
                    fallback: self.source.fallback_snapshot().clone(),
                }
            }
            Err(error) => self.set_fallback(ModelLightingFallbackReason::Preparation(error)),
        }
    }

    pub fn evaluate(
        &self,
        styles: &[f32; 128],
    ) -> Result<ModelLightingSnapshot, ModelLightingFailure> {
        let Some(seed) = self.source.runtime_seed() else {
            return Ok(self.source.fallback_snapshot().clone());
        };
        let evaluated = seed.evaluate(styles, MODEL_LOCAL_LIGHT_LIMIT, MODEL_LIGHT_MINIMUM)?;
        Ok(self.finish_evaluation(&evaluated))
    }

    pub fn evaluate_with_history(
        &self,
        styles: &[f32; 128],
        time: f64,
        history: &mut ModelLightingHistory,
    ) -> Result<ModelLightingSnapshot, ModelLightingFailure> {
        let Some(seed) = self.source.runtime_seed() else {
            return Ok(self.source.fallback_snapshot().clone());
        };
        let evaluated = seed.evaluate(styles, MODEL_LOCAL_LIGHT_LIMIT, MODEL_LIGHT_MINIMUM)?;
        let evaluated = if self.entity {
            history.update(time, evaluated, styles)?
        } else {
            evaluated
        };
        Ok(self.finish_evaluation(&evaluated))
    }

    fn finish_evaluation(
        &self,
        evaluated: &cpu::factory::ModelLightingEvaluation,
    ) -> ModelLightingSnapshot {
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
        ModelLightingSnapshot {
            ambient_cube: ambient,
            lights,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_scene() -> ModelLightingSceneInputs {
        ModelLightingSceneInputs::new(
            gmpublished_domain::scene::map::MapAmbientLighting::neutral(),
            gmpublished_domain::scene::map::MapWorldLighting::default(),
            None,
            None,
            None,
            Vec::new(),
        )
    }

    #[test]
    fn explicit_shader_inputs_survive_scene_preparation_and_do_not_advance_history() {
        let mut context = ModelLightingContext::ambient_fixture(1.0);
        let snapshot = ModelLightingSnapshot {
            ambient_cube: AmbientCube::WHITE,
            lights: Vec::new(),
        };
        context.source = ModelLightingSource::Explicit(snapshot.clone());
        let mut contexts = [context];
        empty_scene().seed_entities(&mut contexts);
        empty_scene().seed_static(&mut contexts);
        assert_eq!(
            contexts[0].source,
            ModelLightingSource::Explicit(snapshot.clone())
        );
        let mut history = ModelLightingHistory::default();
        assert_eq!(
            contexts[0].evaluate_with_history(&[1.0; 128], 1.0, &mut history),
            Ok(snapshot)
        );
        assert_eq!(history, ModelLightingHistory::default());
    }

    #[test]
    fn unresolved_origin_is_diagnosed_and_cannot_remain_a_runtime_source() {
        let mut context = ModelLightingContext::ambient_fixture(0.25);
        let origin_error = ModelLightingOriginFailure::UnresolvedAnchor {
            name: "missing".into(),
        };
        context.sample_point = Err(origin_error.clone());
        let fallback = context.source.fallback_snapshot().clone();
        let mut contexts = [context];
        empty_scene().seed_entities(&mut contexts);
        assert_eq!(
            contexts[0].source,
            ModelLightingSource::DiagnosedFallback {
                reason: ModelLightingFallbackReason::Origin(origin_error),
                snapshot: fallback.clone(),
            }
        );
        assert!(contexts[0].source.runtime_seed().is_none());
        assert_eq!(contexts[0].evaluate(&[1.0; 128]), Ok(fallback));
    }
}
