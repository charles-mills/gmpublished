use super::{
    AmbientCube, Arc, MapAmbientLighting, MapEnvironmentLighting, MapWalkCollision, PropModelAsset,
    StaticPropPlacement, Vec3,
};

#[derive(Debug)]
pub(in super::super) struct SelectedPropPlacement<'a> {
    pub(in super::super) placement: &'a StaticPropPlacement,
    pub(in super::super) model: Arc<PropModelAsset>,
    pub(in super::super) lighting: LegacyPropLightingFallback,
    pub(in super::super) lighting_context: u32,
    pub(in super::super) compiled_lighting:
        Option<Arc<super::compiled_lighting::CompiledPropLighting>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in super::super) struct LegacyPropLightingFallback {
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
    pub(in super::super) environment_maps: Option<super::cubemaps::PropEnvironmentMapInputs<'a>>,
    pub(in super::super) environment_lighting: Option<&'a MapEnvironmentLighting>,
    pub(in super::super) lightmap_source: Option<gmpublished_domain::scene::map::LightmapSource>,
    pub(in super::super) walk_collision: Option<&'a MapWalkCollision>,
}

const PROP_LIGHT_SAMPLE_OFFSET: Vec3 = Vec3::new(0.0, 0.0, 16.0);
const PROP_LIGHT_LINEAR_CLAMP: f32 = 2.0;

pub(in super::super) struct LegacyVertexLighting {
    pub color: Vec3,
    pub illumination: Vec3,
    pub flags: u32,
}

impl LegacyPropLightingFallback {
    pub fn vertex_lighting(
        self,
        normal: Vec3,
        tint: Vec3,
        authored_modulation: bool,
    ) -> LegacyVertexLighting {
        use crate::media::preview_model::vertex_lighting;
        let color = self.evaluate(normal);
        if authored_modulation {
            LegacyVertexLighting {
                color: tint,
                illumination: color * 2.0,
                flags: vertex_lighting::LINEAR_ILLUMINATION,
            }
        } else {
            let flags = if color
                .to_array()
                .into_iter()
                .all(|channel| (channel - 1.0).abs() < 0.5 / 255.0)
            {
                vertex_lighting::PREVIEW
            } else {
                vertex_lighting::LEGACY_VERTEX_COLOR
            };
            LegacyVertexLighting {
                color,
                illumination: Vec3::splat(1.0),
                flags,
            }
        }
    }

    pub(in super::super) fn evaluate(self, normal: Vec3) -> Vec3 {
        let normal = normal.normalize_or_zero();
        let mut color = self.ambient_cube.evaluate(normal);
        if let Some(sun) = self.sun.filter(|sun| sun.visible) {
            let amount = normal.dot(sun.direction_to_sun).max(0.0);
            color += sun.color_linear * amount;
        }
        color.map(|channel| channel.clamp(0.0, PROP_LIGHT_LINEAR_CLAMP))
    }
}

pub(in super::super) fn legacy_prop_lighting_fallback(
    placement: &StaticPropPlacement,
    lighting: StaticPropLightingInputs<'_>,
) -> LegacyPropLightingFallback {
    let sample_origin = placement.lighting_origin.unwrap_or(placement.origin);
    let ambient_cube = lighting.ambient.cube_at(sample_origin);
    let Some(environment_lighting) = lighting.environment_lighting else {
        return LegacyPropLightingFallback {
            ambient_cube,
            sun: None,
        };
    };
    let ray_start = sample_origin + PROP_LIGHT_SAMPLE_OFFSET;
    let sun = environment_lighting.sun.map(|sun| PropSunLighting {
        direction_to_sun: sun.direction_to_sun,
        color_linear: sun.color_linear,
        visible: lighting
            .walk_collision
            .is_some_and(|collision| collision.ray_hits_sky(ray_start, sun.direction_to_sun)),
    });
    LegacyPropLightingFallback { ambient_cube, sun }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::preview_model::vertex_lighting;

    #[test]
    fn legacy_preview_selection_is_explicit_and_preserves_authored_modulation() {
        let fallback = LegacyPropLightingFallback {
            ambient_cube: AmbientCube::WHITE,
            sun: None,
        };
        let normal = Vec3::new(0.0, 0.0, 1.0);
        let preview = fallback.vertex_lighting(normal, Vec3::splat(1.0), false);
        assert_eq!(preview.flags, vertex_lighting::PREVIEW);
        assert_eq!(preview.color, Vec3::splat(1.0));
        let tinted = fallback.vertex_lighting(normal, Vec3::splat(0.5), true);
        assert_eq!(tinted.flags, vertex_lighting::LINEAR_ILLUMINATION);
        assert_eq!(tinted.color, Vec3::splat(0.5));
        assert_eq!(tinted.illumination, Vec3::splat(2.0));
    }
}
