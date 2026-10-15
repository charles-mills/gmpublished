//! Audited model-light input math, independent of visibility, interpolation and ranking.
use gmpublished_domain::{math::Vec3, scene::map::MapWorldLight};
use vformats::bsp::WorldLightKind;

#[path = "cpu/factory.rs"]
pub(super) mod factory;
#[path = "cpu/initialization.rs"]
mod initialization;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct LightCacheKey {
    pub grid: [i32; 3],
    pub leaf: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct LightCacheCell {
    pub key: LightCacheKey,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    /// Candidate before Source validates the cell center against BSP contents
    /// and traces from the requested position. It is not a validated sample.
    pub center: Vec3,
}

impl LightCacheCell {
    pub fn from_position(position: Vec3, leaf: usize) -> Option<Self> {
        let coordinates = position.to_array();
        // Source ASSERT_COORD admits twice MAX_COORD_FLOAT. Constrain integer
        // conversions to that finite range instead of emulating overflow.
        if coordinates
            .into_iter()
            .any(|v| !v.is_finite() || !(-32_768.0..=32_768.0).contains(&v))
        {
            return None;
        }
        let shifts = [3, 3, 4];
        let grid = std::array::from_fn(|axis| (coordinates[axis] as i32 + 32_768) >> shifts[axis]);
        // The native cache hash truncates signed coordinates before shifting,
        // while its sample bounds truncate absolute coordinates and complement
        // the grid index for negatives. Exact negative boundaries can therefore
        // share a key with a different cell center, depending on the first query.
        let minimum = std::array::from_fn(|axis| {
            let cell = (coordinates[axis].abs() as i32) >> shifts[axis];
            let signed = if coordinates[axis] < 0.0 { !cell } else { cell };
            (signed << shifts[axis]) as f32
        });
        let bounds_min = Vec3::from(minimum);
        let size = Vec3::new(8.0, 8.0, 16.0);
        Some(Self {
            key: LightCacheKey { grid, leaf },
            bounds_min,
            bounds_max: bounds_min + size,
            center: bounds_min + size * 0.5,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CompiledPointLight {
    /// Preserve source ID, style, flags, cluster and attenuation without inventing policy.
    pub light: MapWorldLight,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PointLightResponse {
    pub direction_to_light: Vec3,
    pub attenuation: f32,
    pub radiance: Vec3,
}

impl CompiledPointLight {
    pub fn from_compiled(light: MapWorldLight) -> Option<Self> {
        if light.kind != WorldLightKind::Point
            || !light.origin.into_iter().all(f32::is_finite)
            || !light.intensity.into_iter().all(f32::is_finite)
            || ![
                light.radius,
                light.constant_attenuation,
                light.linear_attenuation,
                light.quadratic_attenuation,
            ]
            .into_iter()
            .all(f32::is_finite)
            || light.radius < 0.0
        {
            return None;
        }
        Some(Self { light })
    }

    /// The caller supplies the shared lightstyle weight and independently decides
    /// whether the emitter is visible or already included in ambient lighting.
    pub fn response(self, position: Vec3, style_weight: f32) -> Option<PointLightResponse> {
        if !position.is_finite() || !style_weight.is_finite() || style_weight < 0.0 {
            return None;
        }
        let delta = Vec3::from(self.light.origin) - position;
        let distance_squared = delta.length_squared();
        if !distance_squared.is_finite() || distance_squared <= 0.0 {
            return None;
        }
        let distance = distance_squared.sqrt();
        let denominator = (self.light.quadratic_attenuation * distance_squared
            + self.light.constant_attenuation)
            + self.light.linear_attenuation * distance;
        if !denominator.is_finite() || denominator <= 0.0 {
            return None;
        }
        // Source LightDesc uses zero for infinite range. Radius edge/near-source
        // behavior and generalized coefficients remain separate native gates.
        let in_range = self.light.radius == 0.0 || distance <= self.light.radius;
        let attenuation = if in_range { denominator.recip() } else { 0.0 };
        let radiance = Vec3::from(self.light.intensity) * attenuation * style_weight;
        radiance.is_finite().then_some(PointLightResponse {
            direction_to_light: delta / distance,
            attenuation,
            radiance,
        })
    }
}

#[cfg(test)]
impl PointLightResponse {
    pub fn lambert(self, normal: Vec3) -> Vec3 {
        self.radiance
            * normal
                .normalize_or_zero()
                .dot(self.direction_to_light)
                .max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_cells_preserve_native_signed_boundaries_and_asymmetric_axis_sizes() {
        let cell = LightCacheCell::from_position(Vec3::new(-3008.0, -1228.0, -52.0), 3372).unwrap();
        assert_eq!(cell.key.grid, [3720, 3942, 2044]);
        assert_eq!(cell.bounds_min, Vec3::new(-3016.0, -1232.0, -64.0));
        assert_eq!(cell.bounds_max, Vec3::new(-3008.0, -1224.0, -48.0));
        assert_eq!(cell.center, Vec3::new(-3012.0, -1228.0, -56.0));
        let positive = LightCacheCell::from_position(Vec3::new(8.0, 8.0, 16.0), 0).unwrap();
        assert_eq!(positive.bounds_min, Vec3::new(8.0, 8.0, 16.0));
        assert_eq!(positive.center, Vec3::new(12.0, 12.0, 24.0));
        let negative = LightCacheCell::from_position(Vec3::new(-8.0, -8.0, -16.0), 0).unwrap();
        assert_eq!(negative.bounds_min, Vec3::new(-16.0, -16.0, -32.0));
        assert_eq!(negative.center, Vec3::new(-12.0, -12.0, -24.0));
    }

    #[test]
    fn negative_boundary_queries_can_seed_the_same_key_with_different_centers() {
        use std::collections::HashMap;
        let boundary =
            LightCacheCell::from_position(Vec3::new(-3008.0, -1228.0, -52.0), 3372).unwrap();
        let interior =
            LightCacheCell::from_position(Vec3::new(-3004.0, -1228.0, -52.0), 3372).unwrap();
        assert_eq!(boundary.key, interior.key);
        assert_eq!(interior.center, Vec3::new(-3004.0, -1228.0, -56.0));
        assert_ne!(boundary.center, interior.center);
        let seed = |queries: [LightCacheCell; 2]| {
            let mut cached = HashMap::new();
            for query in queries {
                cached.entry(query.key).or_insert(query.center);
            }
            cached[&boundary.key]
        };
        assert_eq!(seed([boundary, interior]), boundary.center);
        assert_eq!(seed([interior, boundary]), interior.center);
        assert_ne!(
            LightCacheCell::from_position(Vec3::new(-3004.0, -1228.0, -52.0), 3373)
                .unwrap()
                .key,
            interior.key
        );
    }

    #[test]
    fn cache_fractional_coordinates_zero_and_invalid_inputs_remain_distinct() {
        let zero = LightCacheCell::from_position(Vec3::ZERO, 0).unwrap();
        let negative_zero = LightCacheCell::from_position(Vec3::new(-0.0, -0.0, -0.0), 0).unwrap();
        assert_eq!(zero, negative_zero);
        let fraction = LightCacheCell::from_position(Vec3::new(-0.25, -0.25, -0.25), 0).unwrap();
        assert_eq!(fraction.key, zero.key);
        assert_eq!(zero.center, Vec3::new(4.0, 4.0, 8.0));
        assert_eq!(fraction.center, Vec3::new(-4.0, -4.0, -8.0));
        for coordinate in [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            32_769.0,
            -32_769.0,
        ] {
            assert!(LightCacheCell::from_position(Vec3::new(coordinate, 0.0, 0.0), 0).is_none());
        }
        assert!(LightCacheCell::from_position(Vec3::new(-32_768.0, 32_768.0, 0.0), 0).is_some());
    }

    fn point() -> MapWorldLight {
        MapWorldLight {
            original_index: 0,
            origin: [-3001.7256, -1231.3098, -32.0],
            intensity: [400.0, 200.0, 100.0],
            normal: [0.0; 3],
            cluster: 392,
            kind: WorldLightKind::Point,
            style: 0,
            stopdot: 0.0,
            stopdot2: 0.0,
            exponent: 0.0,
            radius: 512.0,
            constant_attenuation: 0.0,
            linear_attenuation: 0.0,
            quadratic_attenuation: 1.0,
            flags: 0,
            texinfo: -1,
            owner: -1,
        }
    }

    #[test]
    fn isolated_compiled_point_matches_independent_native_linear_readouts() {
        let light = CompiledPointLight::from_compiled(point()).unwrap();
        // Native model-lighting-game-reference02, actual float32 BSP/query inputs.
        for (position, normal, expected) in [
            (
                Vec3::new(-3001.7256, -1231.3098, -55.843136),
                Vec3::new(0.0, 0.0, 1.0),
                [0.703_611_97, 0.351_805_98, 0.175_902_99],
            ),
            (
                Vec3::new(-3092.0, -1184.0, -56.0),
                Vec3::new(1.0, 0.0, 0.0),
                [0.031_454_995, 0.015_727_498, 0.007_863_749],
            ),
            (
                Vec3::new(-2936.0, -1376.0, -64.0),
                Vec3::new(0.0, 0.0, 1.0),
                [0.003_004_652_2, 0.001_502_326_1, 0.000_751_163_05],
            ),
        ] {
            let actual = light.response(position, 1.0).unwrap().lambert(normal);
            assert!(actual.distance(Vec3::from(expected)) < 1.0e-6);
        }
        assert_eq!(
            light
                .response(Vec3::new(-3001.7256, -1231.3098, -55.843136), 1.0)
                .unwrap()
                .lambert(-Vec3::new(0.0, 0.0, 1.0)),
            Vec3::ZERO
        );
    }

    #[test]
    fn conversion_retains_policy_metadata_and_rejects_other_emitter_kinds() {
        let mut raw = point();
        raw.original_index = 17;
        raw.flags = 0x81;
        raw.style = 33;
        raw.cluster = -1;
        raw.owner = 12;
        assert_eq!(CompiledPointLight::from_compiled(raw).unwrap().light, raw);
        raw.kind = WorldLightKind::Spotlight;
        assert!(CompiledPointLight::from_compiled(raw).is_none());
    }

    #[test]
    fn style_and_range_inputs_are_explicit_and_invalid_math_has_no_white_fallback() {
        let light = CompiledPointLight::from_compiled(point()).unwrap();
        let position = Vec3::new(-3001.7256, -1231.3098, -55.843136);
        assert_eq!(light.response(position, 0.0).unwrap().radiance, Vec3::ZERO);
        assert!(light.response(position, f32::NAN).is_none());
        assert!(light.response(position, -1.0).is_none());
        assert!(
            light
                .response(Vec3::from(light.light.origin), 1.0)
                .is_none()
        );
        assert!(
            light
                .response(Vec3::new(f32::INFINITY, 0.0, 0.0), 1.0)
                .is_none()
        );
        assert_eq!(
            light.response(Vec3::ZERO, 1.0).unwrap().radiance,
            Vec3::ZERO
        );
        let mut invalid = point();
        invalid.quadratic_attenuation = 0.0;
        assert!(
            CompiledPointLight::from_compiled(invalid)
                .unwrap()
                .response(position, 1.0)
                .is_none()
        );
    }
}
