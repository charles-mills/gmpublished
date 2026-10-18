use super::cpu::factory::SelectedModelLight;
use gmpublished_domain::{math::Vec3, scene::map::AmbientCube};

const STUDIOHDR_FLAGS_AMBIENT_BOOST: u32 = 0x0001_0000;

pub(super) fn model_draw_ambient(
    mdl_flags: u32,
    lighting_origin: Vec3,
    mut ambient: AmbientCube,
    lights: &[SelectedModelLight],
) -> AmbientCube {
    if mdl_flags & STUDIOHDR_FLAGS_AMBIENT_BOOST == 0 || lights.is_empty() {
        return ambient;
    }
    let mut sum = 0.0;
    let mut maximum = 0.0_f32;
    for color in ambient.colors {
        let brightness = luminance(color);
        sum += brightness;
        maximum = maximum.max(brightness);
    }
    let mean = sum * (1.0_f32 / 6.0);
    if mean >= 0.3 {
        return ambient;
    }
    // Boost uses unstyled radiance at the raw shader origin, without cone falloff.
    let mut local_brightness = 0.0;
    for selected in lights {
        let light = selected.light;
        let delta = Vec3::from(light.origin) - lighting_origin;
        let distance_squared = delta.length_squared();
        let denominator = (light.quadratic_attenuation * distance_squared
            + light.constant_attenuation)
            + light.linear_attenuation * distance_squared.sqrt();
        let attenuation = if denominator > 0.000_01 {
            denominator.recip()
        } else {
            1.0
        };
        local_brightness += luminance(Vec3::from(light.intensity)) * attenuation;
    }
    let target = 0.1 * local_brightness;
    if mean < target {
        let factor = (target / maximum).min(5.0);
        for color in &mut ambient.colors {
            *color *= factor;
        }
    }
    ambient
}

fn luminance(color: Vec3) -> f32 {
    // Draw boost and light selection use different luminance coefficients.
    (color[0] * 0.30 + color[1] * 0.59) + color[2] * 0.11
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmpublished_domain::scene::map::MapWorldLight;
    use vformats::bsp::WorldLightKind;

    fn light() -> SelectedModelLight {
        SelectedModelLight {
            light: MapWorldLight {
                original_index: 0,
                origin: [0.0, 0.0, 8.0],
                intensity: [64.0; 3],
                normal: [0.0; 3],
                cluster: 0,
                kind: WorldLightKind::Point,
                style: 32,
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
            },
            style_weight: 2.0,
        }
    }

    #[test]
    fn boost_requires_authored_flag_local_lights_and_both_brightness_thresholds() {
        let ambient = AmbientCube {
            colors: [Vec3::splat(0.01); 6],
        };
        let lights = [light()];
        assert_eq!(model_draw_ambient(0, Vec3::ZERO, ambient, &lights), ambient);
        assert_eq!(
            model_draw_ambient(STUDIOHDR_FLAGS_AMBIENT_BOOST, Vec3::ZERO, ambient, &[]),
            ambient
        );
        let bright = AmbientCube {
            colors: [Vec3::splat(0.3); 6],
        };
        assert_eq!(
            model_draw_ambient(STUDIOHDR_FLAGS_AMBIENT_BOOST, Vec3::ZERO, bright, &lights),
            bright
        );
        let dim_light = SelectedModelLight {
            light: MapWorldLight {
                intensity: [0.01; 3],
                ..lights[0].light
            },
            ..lights[0]
        };
        assert_eq!(
            model_draw_ambient(
                STUDIOHDR_FLAGS_AMBIENT_BOOST,
                Vec3::ZERO,
                ambient,
                &[dim_light]
            ),
            ambient
        );
        let boosted =
            model_draw_ambient(STUDIOHDR_FLAGS_AMBIENT_BOOST, Vec3::ZERO, ambient, &lights);
        assert_eq!(boosted.colors, [Vec3::splat(0.01 * 5.0); 6]);
    }

    #[test]
    fn boost_uses_raw_origin_raw_intensity_and_maximum_axis_for_the_common_factor() {
        let ambient = AmbientCube {
            colors: std::array::from_fn(|axis| Vec3::splat(if axis == 0 { 0.04 } else { 0.01 })),
        };
        let lights = [light()];
        let boosted =
            model_draw_ambient(STUDIOHDR_FLAGS_AMBIENT_BOOST, Vec3::ZERO, ambient, &lights);
        let factor = 0.1 / luminance(ambient.colors[0]);
        assert_eq!(boosted.colors, ambient.colors.map(|color| color * factor));
        let far_origin = model_draw_ambient(
            STUDIOHDR_FLAGS_AMBIENT_BOOST,
            Vec3::new(0.0, 0.0, -80.0),
            ambient,
            &lights,
        );
        assert_eq!(far_origin, ambient);
        let unweighted = SelectedModelLight {
            style_weight: 0.25,
            ..lights[0]
        };
        assert_eq!(
            model_draw_ambient(
                STUDIOHDR_FLAGS_AMBIENT_BOOST,
                Vec3::ZERO,
                ambient,
                &[unweighted]
            ),
            boosted
        );
        let zero = AmbientCube {
            colors: [Vec3::ZERO; 6],
        };
        assert_eq!(
            model_draw_ambient(STUDIOHDR_FLAGS_AMBIENT_BOOST, Vec3::ZERO, zero, &lights),
            zero
        );
    }
}
