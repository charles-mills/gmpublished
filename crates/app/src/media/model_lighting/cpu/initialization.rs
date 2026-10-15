use gmpublished_domain::{
    math::Vec3,
    scene::map::{LightmapSource, MapWorldLight},
};
use vformats::bsp::WorldLightKind;

pub(super) fn initialized_light(
    mut light: MapWorldLight,
    source: LightmapSource,
) -> Option<MapWorldLight> {
    if matches!(
        light.kind,
        WorldLightKind::Point | WorldLightKind::Spotlight
    ) && light.constant_attenuation == 0.0
        && light.linear_attenuation == 0.0
        && light.quadratic_attenuation == 0.0
    {
        light.quadratic_attenuation = 1.0;
    }
    if light.kind == WorldLightKind::Spotlight && light.exponent == 0.0 {
        light.exponent = 1.0;
    }
    if light.radius == 0.0 {
        let threshold = match source {
            LightmapSource::Ldr => 0.03,
            LightmapSource::Hdr => 0.015,
        };
        let strength = Vec3::from(light.intensity).length() / threshold;
        let constant = light.constant_attenuation;
        let linear = light.linear_attenuation;
        let quadratic = light.quadratic_attenuation;
        light.radius = if quadratic != 0.0 {
            let discriminant = linear * linear - (4.0 * quadratic) * (constant - strength);
            if discriminant < 0.0 {
                2000.0
            } else {
                ((discriminant.sqrt() - linear) / (quadratic + quadratic)).max(0.0)
            }
        } else if linear != 0.0 {
            (strength - constant) / linear
        } else {
            2000.0
        };
    }
    light.radius.is_finite().then_some(light)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(kind: WorldLightKind) -> MapWorldLight {
        MapWorldLight {
            original_index: 7,
            origin: [64.0, 0.0, 0.0],
            intensity: [3.0, 4.0, 0.0],
            normal: [-1.0, 0.0, 0.0],
            cluster: 2,
            kind,
            style: 32,
            stopdot: 0.9,
            stopdot2: 0.5,
            exponent: 0.0,
            radius: 0.0,
            constant_attenuation: 0.0,
            linear_attenuation: 0.0,
            quadratic_attenuation: 0.0,
            flags: 0x100,
            texinfo: 4,
            owner: 9,
        }
    }

    #[test]
    fn compiled_point_and_spot_defaults_are_effective_copies_of_preserved_source_data() {
        let authored = light(WorldLightKind::Spotlight);
        let effective = initialized_light(authored, LightmapSource::Ldr).unwrap();
        assert_eq!(authored.quadratic_attenuation, 0.0);
        assert_eq!(authored.exponent, 0.0);
        assert_eq!(authored.radius, 0.0);
        assert_eq!(effective.quadratic_attenuation, 1.0);
        assert_eq!(effective.exponent, 1.0);
        assert_eq!(
            effective.radius.to_bits(),
            (5.0_f32 / 0.03).sqrt().to_bits()
        );
        assert_eq!(
            (
                effective.original_index,
                effective.flags,
                effective.style,
                effective.cluster,
                effective.texinfo,
                effective.owner
            ),
            (7, 0x100, 32, 2, 4, 9)
        );
        let hdr = initialized_light(authored, LightmapSource::Hdr).unwrap();
        assert_eq!(hdr.radius, (5.0_f32 / 0.015).sqrt());
        let mut authored = authored;
        authored.radius = 7.0;
        authored.exponent = 2.0;
        let effective = initialized_light(authored, LightmapSource::Ldr).unwrap();
        assert_eq!(effective.radius, 7.0);
        assert_eq!(effective.exponent, 2.0);
    }

    #[test]
    fn zero_radius_uses_vector_intensity_and_preserves_native_linear_and_fallback_branches() {
        let mut authored = light(WorldLightKind::Point);
        authored.constant_attenuation = 1.0;
        assert_eq!(
            initialized_light(authored, LightmapSource::Ldr)
                .unwrap()
                .radius,
            2000.0
        );
        authored.linear_attenuation = 2.0;
        assert_eq!(
            initialized_light(authored, LightmapSource::Ldr)
                .unwrap()
                .radius,
            (5.0_f32 / 0.03 - 1.0) / 2.0
        );
        authored.constant_attenuation = 1000.0;
        authored.quadratic_attenuation = 1.0;
        assert_eq!(
            initialized_light(authored, LightmapSource::Ldr)
                .unwrap()
                .radius,
            2000.0
        );
        authored.constant_attenuation = 0.0;
        authored.linear_attenuation = 0.0;
        authored.intensity = [f32::MAX; 3];
        assert_eq!(initialized_light(authored, LightmapSource::Ldr), None);
    }
}
