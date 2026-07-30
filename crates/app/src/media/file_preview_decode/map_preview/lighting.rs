//! Ambient and direct-light projections for placed props.

use super::{
    AmbientCube, Arc, MapAmbientLighting, MapEnvironmentLighting, MapWalkCollision, PropModelAsset,
    StaticPropPlacement, Vec3,
};

#[derive(Debug)]
pub(in super::super) struct SelectedPropPlacement<'a> {
    pub(in super::super) placement: &'a StaticPropPlacement,
    pub(in super::super) model: Arc<PropModelAsset>,
    pub(in super::super) lighting: PropPlacementLighting,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in super::super) struct PropPlacementLighting {
    pub(in super::super) ambient_cube: AmbientCube,
    pub(in super::super) sun: Option<PropSunLighting>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in super::super) struct PropSunLighting {
    pub(in super::super) direction_to_sun: Vec3,
    pub(in super::super) color_linear: Vec3,
    pub(in super::super) visible: bool,
}

#[derive(Clone, Copy)]
pub(in super::super) struct StaticPropLightingInputs<'a> {
    pub(in super::super) ambient: &'a MapAmbientLighting,
    pub(in super::super) environment_lighting: Option<&'a MapEnvironmentLighting>,
    pub(in super::super) walk_collision: Option<&'a MapWalkCollision>,
}

const PROP_LIGHT_SAMPLE_OFFSET: Vec3 = Vec3::new(0.0, 0.0, 16.0);
const PROP_LIGHT_LINEAR_CLAMP: f32 = 2.0;

impl PropPlacementLighting {
    pub(in super::super) fn evaluate(self, normal: Vec3) -> Vec3 {
        let normal = normal.normalize_or_zero();
        let mut color = self.ambient_cube.evaluate(normal);
        if let Some(sun) = self.sun.filter(|sun| sun.visible) {
            let amount = normal.dot(sun.direction_to_sun).max(0.0);
            color += sun.color_linear * amount;
        }
        // No separate skylight term: the ambient cube already integrates sky
        // bounce (the engine likewise skips sky-ambient world lights for
        // entities because ambient cubes cover them).
        color.map(|channel| channel.clamp(0.0, PROP_LIGHT_LINEAR_CLAMP))
    }
}

pub(in super::super) fn prop_placement_lighting(
    placement: &StaticPropPlacement,
    lighting: StaticPropLightingInputs<'_>,
) -> PropPlacementLighting {
    let ambient_cube = lighting.ambient.cube_at(placement.origin);
    let Some(environment_lighting) = lighting.environment_lighting else {
        return PropPlacementLighting {
            ambient_cube,
            sun: None,
        };
    };
    let ray_start = placement.origin + PROP_LIGHT_SAMPLE_OFFSET;
    let sun = environment_lighting.sun.map(|sun| PropSunLighting {
        direction_to_sun: sun.direction_to_sun,
        color_linear: sun.color_linear,
        visible: lighting
            .walk_collision
            .is_some_and(|collision| collision.ray_hits_sky(ray_start, sun.direction_to_sun)),
    });
    PropPlacementLighting { ambient_cube, sun }
}
