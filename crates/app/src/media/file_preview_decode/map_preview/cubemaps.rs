use crate::bridge::materials::{EnvironmentMapMaterial, EnvironmentMapSource, MaterialResolver};
use gmpublished_domain::scene::map::MapCubemapProbe;
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(in super::super) struct PropEnvironmentMapInputs<'a> {
    pub map_name: &'a str,
    pub probes: &'a [MapCubemapProbe],
}

fn nearest_probe(
    probes: &[MapCubemapProbe],
    sample: gmpublished_domain::math::Vec3,
) -> Option<MapCubemapProbe> {
    if !sample.is_finite() {
        return None;
    }
    let mut best = None;
    let mut best_distance = f32::INFINITY;
    for &probe in probes {
        let offset = probe.position() - sample;
        let distance = offset.dot(offset);
        if distance < best_distance {
            best_distance = distance;
            best = Some(probe);
        }
    }
    best
}

fn map_name_valid(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

pub(super) fn bind_local_environment(
    authored: &Arc<EnvironmentMapMaterial>,
    inputs: PropEnvironmentMapInputs<'_>,
    sample: gmpublished_domain::math::Vec3,
    resolver: &MaterialResolver,
) -> Option<Arc<EnvironmentMapMaterial>> {
    if matches!(authored.source, EnvironmentMapSource::Named(_)) {
        return Some(Arc::clone(authored));
    }
    if !map_name_valid(inputs.map_name) {
        return None;
    }
    let probe = nearest_probe(inputs.probes, sample);
    let selected =
        probe.and_then(|probe| bind_selected_probe(authored, inputs.map_name, probe, resolver));
    if selected.is_some() {
        return selected;
    }
    let path = if probe.is_some() {
        format!("maps/{}/cubemapdefault", inputs.map_name)
    } else {
        "engine/defaultcubemap".to_owned()
    };
    let (path, cube) = resolver
        .resolve_cubemap(&path, 0)
        .map(|cube| (path, cube))
        .or_else(|| {
            resolver
                .resolve_cubemap("engine/defaultcubemap", 0)
                .map(|cube| ("engine/defaultcubemap".to_owned(), cube))
        })?;
    let mut bound = (**authored).clone();
    bound.path = path;
    bound.source = EnvironmentMapSource::Named(cube);
    Some(Arc::new(bound))
}

fn prop_sample_position(
    illumination_position: gmpublished_domain::math::Vec3,
    placement: &gmpublished_domain::scene::map::StaticPropPlacement,
) -> gmpublished_domain::math::Vec3 {
    placement.lighting_origin.unwrap_or_else(|| {
        if placement.entity {
            // Entity illumination positions ignore model scale.
            illumination_position.rotate_source(placement.angles) + placement.origin
        } else {
            super::props::transform_prop_position(illumination_position, placement)
        }
    })
}

pub(super) fn bind_prop_materials(
    model: &super::props::PropModelAsset,
    placement: &gmpublished_domain::scene::map::StaticPropPlacement,
    inputs: PropEnvironmentMapInputs<'_>,
    resolver: &MaterialResolver,
    table: &mut super::materials::MaterialTable,
) -> std::collections::BTreeMap<usize, usize> {
    let sample = prop_sample_position(model.model.illumination_position, placement);
    model
        .material_indices
        .iter()
        .map(|(&raw_slot, &index)| {
            let source = &table.slots()[index];
            let Some(environment) = source
                .shader
                .surface()
                .and_then(|surface| surface.envmap.as_ref())
            else {
                return (raw_slot, index);
            };
            if !matches!(environment.source, EnvironmentMapSource::LocalProbe) {
                return (raw_slot, index);
            }
            let Some(bound) = bind_local_environment(environment, inputs, sample, resolver) else {
                log::debug!(
                    "model environment unavailable in {} at {sample:?}",
                    inputs.map_name
                );
                return (raw_slot, index);
            };
            let key = format!("prop_environment\0{index}\0{}", bound.path);
            if let Some(existing) = table.index_of(&key) {
                return (raw_slot, existing);
            }
            let mut slot = source.clone();
            slot.shader.bind_environment_map(bound);
            (raw_slot, table.push(&key, slot))
        })
        .collect()
}

pub(super) fn bind_selected_probe(
    authored: &Arc<EnvironmentMapMaterial>,
    map_name: &str,
    probe: MapCubemapProbe,
    resolver: &MaterialResolver,
) -> Option<Arc<EnvironmentMapMaterial>> {
    if matches!(authored.source, EnvironmentMapSource::Named(_)) {
        return Some(Arc::clone(authored));
    }
    if !map_name_valid(map_name) {
        return None;
    }
    let [x, y, z] = probe.origin;
    let path = format!("maps/{map_name}/c{x}_{y}_{z}");
    let cube = resolver.resolve_cubemap(&path, authored.frame)?;
    let mut bound = (**authored).clone();
    bound.path = path;
    bound.source = EnvironmentMapSource::Named(cube);
    Some(Arc::new(bound))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::materials::{EnvironmentMapMask, ResolvedCubemap};

    fn local() -> Arc<EnvironmentMapMaterial> {
        Arc::new(EnvironmentMapMaterial {
            source: EnvironmentMapSource::LocalProbe,
            path: "env_cubemap".to_owned(),
            frame: 3,
            tint: [0.25, 0.5, 1.0],
            contrast: 0.2,
            saturation: 0.8,
            mask: EnvironmentMapMask::BaseAlpha,
            mask_texture: None,
            mask_frame: 2,
            mask_transform: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            fresnel: 1.0,
        })
    }

    #[test]
    fn native_probe_midpoint_retains_directory_order_and_lighting_sample_position() {
        use gmpublished_domain::math::Vec3;
        let probes = [
            MapCubemapProbe {
                origin: [-3072, -1472, -96],
                size: 6,
            },
            MapCubemapProbe {
                origin: [-2992, -1272, -48],
                size: 6,
            },
        ];
        for (sample, expected) in [
            (Vec3::new(-3092.0, -1184.0, -56.0), 1),
            (Vec3::new(-3032.0, -1372.0, -72.0), 0),
            (Vec3::new(-3031.6375, -1371.0938, -71.7825), 1),
            (Vec3::new(-3032.3625, -1372.9063, -72.2175), 0),
        ] {
            assert_eq!(nearest_probe(&probes, sample), Some(probes[expected]));
        }
        assert_eq!(nearest_probe(&[], Vec3::ZERO), None);
        assert_eq!(nearest_probe(&probes, Vec3::new(f32::NAN, 0.0, 0.0)), None);
    }

    #[test]
    fn empty_and_missing_probe_resources_choose_distinct_native_defaults() {
        use crate::test_support::fixture_vtf_bytes;
        use gmpublished_domain::math::Vec3;
        use vformats::vtf::{VtfFormat, texture_flags};
        let mut bytes = fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[128; 28]]);
        bytes[20..24].copy_from_slice(&texture_flags::ENVMAP.to_le_bytes());
        bytes[26..28].copy_from_slice(&0_u16.to_le_bytes());
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![
                (
                    "materials/maps/gm_example/cubemapdefault.vtf",
                    bytes.clone(),
                ),
                ("materials/engine/defaultcubemap.vtf", bytes),
            ],
            Vec::new(),
        );
        let mut authored = (*local()).clone();
        authored.frame = 0;
        let authored = Arc::new(authored);
        let probe = MapCubemapProbe {
            origin: [1, 2, 3],
            size: 6,
        };
        let empty = bind_local_environment(
            &authored,
            PropEnvironmentMapInputs {
                map_name: "gm_example",
                probes: &[],
            },
            Vec3::ZERO,
            &resolver,
        )
        .unwrap();
        let missing = bind_local_environment(
            &authored,
            PropEnvironmentMapInputs {
                map_name: "gm_example",
                probes: &[probe],
            },
            Vec3::ZERO,
            &resolver,
        )
        .unwrap();
        assert_eq!(empty.path, "engine/defaultcubemap");
        assert_eq!(missing.path, "maps/gm_example/cubemapdefault");
        let no_map_default = bind_local_environment(
            &authored,
            PropEnvironmentMapInputs {
                map_name: "gm_absent",
                probes: &[probe],
            },
            Vec3::ZERO,
            &resolver,
        )
        .unwrap();
        assert_eq!(no_map_default.path, "engine/defaultcubemap");
        assert_eq!(authored.source, EnvironmentMapSource::LocalProbe);
    }

    #[test]
    fn native_three_way_control_distinguishes_static_scale_from_entity_illumination() {
        use gmpublished_domain::math::Vec3;
        let mut placement = crate::media::file_preview_decode::tests::prop_placement(
            "models/test/probe.mdl",
            Vec3::new(100.0, 200.0, 300.0),
            Vec3::ZERO,
            0,
        );
        placement.scale = 2.0;
        let illumination = Vec3::new(0.0, 32.0, 0.0);
        assert_eq!(
            prop_sample_position(illumination, &placement),
            Vec3::new(100.0, 264.0, 300.0)
        );
        placement.entity = true;
        assert_eq!(
            prop_sample_position(illumination, &placement),
            Vec3::new(100.0, 232.0, 300.0)
        );
        placement.angles = Vec3::new(0.0, 90.0, 0.0);
        let rotated = prop_sample_position(illumination, &placement);
        assert!((rotated - Vec3::new(68.0, 200.0, 300.0)).length() < 0.0001);
        placement.lighting_origin = Some(Vec3::new(-1.0, -2.0, -3.0));
        assert_eq!(
            prop_sample_position(illumination, &placement),
            Vec3::new(-1.0, -2.0, -3.0)
        );
    }

    #[test]
    fn explicit_named_binding_is_preserved_and_absent_probe_is_not_guessed() {
        let resolver = MaterialResolver::prop_lighting_fixture(Vec::new(), Vec::new());
        let probe = MapCubemapProbe {
            origin: [i32::MAX, -96, 0],
            size: 0,
        };
        assert!(bind_selected_probe(&local(), "gm_example", probe, &resolver).is_none());
        assert!(bind_selected_probe(&local(), "../othermap", probe, &resolver).is_none());
        let mut named = (*local()).clone();
        named.source = EnvironmentMapSource::Named(Arc::new(
            ResolvedCubemap::from_rgba(
                0,
                3,
                std::array::from_fn(|_| {
                    vec![crate::bridge::materials::ResolvedTextureMip {
                        rgba: vec![0, 0, 0, 255],
                        width: 1,
                        height: 1,
                    }]
                }),
            )
            .unwrap(),
        ));
        named.path = "maps/explicit/c1_2_3".to_owned();
        let named = Arc::new(named);
        assert!(Arc::ptr_eq(
            &named,
            &bind_selected_probe(&named, "gm_example", probe, &resolver).unwrap()
        ));
    }

    #[test]
    fn selected_probe_binding_preserves_authored_state_without_mutating_shared_material() {
        use crate::test_support::fixture_vtf_bytes;
        use vformats::vtf::{VtfFormat, texture_flags};
        let mut bytes = fixture_vtf_bytes(1, 1, VtfFormat::Rgba8888, &[&[127; 96]]);
        bytes[20..24].copy_from_slice(&texture_flags::ENVMAP.to_le_bytes());
        bytes[24..26].copy_from_slice(&4_u16.to_le_bytes());
        bytes[26..28].copy_from_slice(&u16::MAX.to_le_bytes());
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![("materials/maps/gm_example/c2147483647_-96_0.vtf", bytes)],
            Vec::new(),
        );
        let authored = local();
        let bound = bind_selected_probe(
            &authored,
            "gm_example",
            MapCubemapProbe {
                origin: [i32::MAX, -96, 0],
                size: 0,
            },
            &resolver,
        )
        .unwrap();
        assert_eq!(bound.path, "maps/gm_example/c2147483647_-96_0");
        assert_eq!(bound.frame, authored.frame);
        assert_eq!(bound.mask_frame, authored.mask_frame);
        assert_eq!(bound.tint, authored.tint);
        assert_eq!(bound.mask, authored.mask);
        assert_eq!(bound.contrast, authored.contrast);
        assert!(matches!(bound.source, EnvironmentMapSource::Named(_)));
        assert!(matches!(authored.source, EnvironmentMapSource::LocalProbe));
        assert_eq!(authored.path, "env_cubemap");
    }
}
