use super::*;
use gmpublished_domain::scene::map::{MapData, load_map};

fn point(index: usize, origin: Vec3) -> MapWorldLight {
    MapWorldLight {
        original_index: index,
        origin: origin.to_array(),
        intensity: [2048.0; 3],
        normal: [0.0; 3],
        cluster: 0,
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
fn spot_cone_preserves_raw_ranking_attenuation_and_native_zero_exponent_rule() {
    let mut light = point(4, Vec3::new(0.0, 0.0, 64.0));
    light.kind = WorldLightKind::Spotlight;
    light.normal = [0.0, 0.0, -1.0];
    light.stopdot = 0.9;
    light.stopdot2 = 0.5;
    light.exponent = 0.0;
    let between = Vec3::new((1.0_f32 - 0.7 * 0.7).sqrt(), 0.0, 0.7);
    let expected = (between[2] - light.stopdot2) / (light.stopdot - light.stopdot2);
    assert_eq!(spot_angular(light, between), Some(expected));
    light.exponent = 1.0;
    assert_eq!(spot_angular(light, between), Some(expected));
    light.exponent = 2.0;
    assert_eq!(
        spot_angular(light, between),
        Some(f64::from(expected).powi(2) as f32)
    );
    assert_eq!(spot_angular(light, Vec3::new(0.0, 0.0, 1.0)), Some(1.0));
    assert_eq!(spot_angular(light, Vec3::new(1.0, 0.0, 0.0)), Some(0.0));
    let outside = compiled_response(light, Vec3::new(64.0, 0.0, 64.0), 1.0).unwrap();
    assert!(outside.attenuation > 0.0);
    assert_eq!(outside.radiance, Vec3::ZERO);
    assert!(spot_intersects_cell(light, Vec3::ZERO, Vec3::splat(4.0)));
    assert!(!spot_intersects_cell(
        light,
        Vec3::new(0.0, 0.0, 128.0),
        Vec3::new(4.0, 4.0, 132.0)
    ));
}

#[test]
fn surface_fold_cosine_is_separate_from_selected_hemisphere_descriptor() {
    let mut surface = point(0, Vec3::new(64.0, 0.0, 0.0));
    surface.kind = WorldLightKind::Surface;
    surface.normal = [-1.0, 0.0, 0.0];
    let front = compiled_response(surface, Vec3::ZERO, 1.0).unwrap();
    assert_eq!(front.attenuation, 1.0 / 4096.0);
    assert_eq!(front.radiance, Vec3::splat(0.5));
    surface.normal = [
        -std::f32::consts::FRAC_1_SQRT_2,
        -std::f32::consts::FRAC_1_SQRT_2,
        0.0,
    ];
    let folded = compiled_response(surface, Vec3::ZERO, 1.0).unwrap();
    assert_eq!(
        folded.radiance,
        Vec3::splat(0.5 * std::f32::consts::FRAC_1_SQRT_2)
    );
    let selected = selected_response(surface, Vec3::ZERO, 1.0).unwrap();
    assert_eq!(selected.radiance, Vec3::splat(0.5));
    surface.normal = [1.0, 0.0, 0.0];
    assert_eq!(
        compiled_response(surface, Vec3::ZERO, 1.0)
            .unwrap()
            .radiance,
        Vec3::ZERO
    );
    assert_eq!(
        selected_response(surface, Vec3::ZERO, 1.0)
            .unwrap()
            .radiance,
        Vec3::ZERO
    );
    assert!(!spot_intersects_cell(surface, Vec3::ZERO, Vec3::splat(4.0)));
}

#[test]
fn quake_participates_in_selection_and_ambient_folding_but_has_no_hardware_descriptor() {
    let mut quake = point(4, Vec3::new(64.0, 0.0, 0.0));
    quake.kind = WorldLightKind::Quake;
    quake.linear_attenuation = 128.0;
    quake.intensity = [0.0, 0.0, 0.005];
    let response = compiled_response(quake, Vec3::ZERO, 1.0).unwrap();
    assert_eq!(response.attenuation, 64.0);
    assert_eq!(response.radiance, Vec3::new(0.0, 0.0, 0.005 * 64.0));
    assert!(selected_response(quake, Vec3::ZERO, 1.0).is_none());
    assert_eq!(
        compiled_response(quake, Vec3::new(-128.0, 0.0, 0.0), 1.0)
            .unwrap()
            .radiance,
        Vec3::ZERO
    );
    let map = map(&[quake]);
    let mut factory = ModelLightingFactory::new(
        &map.ambient,
        &map.world_lighting,
        map.lighting_collision.as_ref().unwrap(),
        None,
    )
    .unwrap();
    let seed = factory.seed_dynamic(Vec3::ZERO).unwrap();
    let selected = seed.evaluate(&[1.0; 128], 4, 0.0002).unwrap();
    assert_eq!(selected.selected[0].light.kind, WorldLightKind::Quake);
    assert_eq!(
        selected.lambert_at(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0)),
        Vec3::ZERO
    );
    let folded = seed.evaluate(&[1.0; 128], 0, 0.0002).unwrap();
    assert_eq!(folded.folded_ids, [0]);
    assert!(folded.ambient_cube.colors[0][2] > 0.3);
    assert!(seed.diagnostics.iter().all(|diagnostic| !matches!(
        diagnostic,
        ModelLightingDiagnostic::UnsupportedEmitter { .. }
    )));
}

#[test]
fn compiled_sky_keeps_authored_direction_and_has_no_origin_distance_falloff() {
    let mut light = point(9, Vec3::new(8192.0, -2048.0, 512.0));
    light.kind = WorldLightKind::Skylight;
    light.normal = [0.0, 0.0, -0.999_999_9];
    light.intensity = [0.2, 0.3, 0.4];
    let response = compiled_response(light, Vec3::ZERO, 2.0).unwrap();
    assert_eq!(
        response.direction_to_light,
        Vec3::new(0.0, 0.0, 0.999_999_9)
    );
    assert_eq!(response.attenuation, 1.0);
    assert_eq!(response.radiance, Vec3::new(0.4, 0.6, 0.8));
    assert_eq!(
        response,
        compiled_response(light, Vec3::new(128.0, 256.0, 512.0), 2.0).unwrap()
    );
}

#[test]
fn minimum_uses_native_luminance_score_rather_than_brightest_channel() {
    let mut light = point(0, Vec3::new(4.0, 4.0, 72.0));
    light.intensity = [0.0, 0.0, 2.0];
    let map = map(&[light]);
    let mut factory = ModelLightingFactory::new(
        &map.ambient,
        &map.world_lighting,
        map.lighting_collision.as_ref().unwrap(),
        None,
    )
    .unwrap();
    let seed = factory.seed_dynamic(Vec3::ZERO).unwrap();
    let below = seed.evaluate(&[1.0; 128], 4, 0.0002).unwrap();
    assert!(below.selected.is_empty());
    assert_eq!(below.folded_ids, [0]);
    assert!(below.ambient_cube.colors[4][2] > 0.0);
    assert_eq!(
        seed.evaluate(&[1.0; 128], 4, 0.00001)
            .unwrap()
            .selected
            .len(),
        1
    );
}

#[test]
fn point_cell_range_selects_native_draw_light_while_cpu_query_still_checks_descriptor_range() {
    let position = Vec3::new(1026.0043, 2304.0, 1037.3953);
    let mut light = point(0, Vec3::new(1038.0, 2308.0, 1032.0));
    light.intensity = [0.5; 3];
    light.constant_attenuation = 1.0;
    light.quadratic_attenuation = 0.0;
    light.radius = 7.0;
    let map = map(&[light]);
    let mut factory = ModelLightingFactory::new(
        &map.ambient,
        &map.world_lighting,
        map.lighting_collision.as_ref().unwrap(),
        None,
    )
    .unwrap();
    let seed = factory.seed_dynamic(position).unwrap();
    assert_eq!(seed.sample_point, Vec3::new(1028.0, 2308.0, 1032.0));
    let selected = seed.evaluate(&[1.0; 128], 4, 0.0002).unwrap();
    assert_eq!(selected.selected.len(), 1);
    assert_eq!(seed.candidates[0].response.attenuation, 1.0);
    assert_eq!(
        selected.lambert_at(position, Vec3::new(1.0, 0.0, 0.0)),
        Vec3::ZERO
    );
    assert!(point_intersects_cell(
        light,
        Vec3::new(1024.0, 2304.0, 1024.0),
        Vec3::new(1032.0, 2312.0, 1040.0)
    ));
    light.radius = 5.0;
    assert!(!point_intersects_cell(
        light,
        Vec3::new(1024.0, 2304.0, 1024.0),
        Vec3::new(1032.0, 2312.0, 1040.0)
    ));
}

#[test]
fn zero_radius_constant_light_uses_native_2000_unit_cell_boundary() {
    let position = Vec3::new(1026.0043, 2304.0, 1037.3953);
    for (source_x, expected_count) in [(-975.0, 1), (-977.0, 0)] {
        let mut authored = point(0, Vec3::new(source_x, 2308.0, 1032.0));
        authored.intensity = [0.5; 3];
        authored.constant_attenuation = 1.0;
        authored.quadratic_attenuation = 0.0;
        authored.radius = 0.0;
        let map = map(&[authored]);
        let mut factory = ModelLightingFactory::new(
            &map.ambient,
            &map.world_lighting,
            map.lighting_collision.as_ref().unwrap(),
            None,
        )
        .unwrap();
        let seed = factory.seed_dynamic(position).unwrap();
        let evaluated = seed.evaluate(&[1.0; 128], 4, 0.0002).unwrap();
        assert_eq!(evaluated.selected.len(), expected_count);
        if let Some(selected) = evaluated.selected.first() {
            assert_eq!(selected.light.radius, 2000.0);
            assert_eq!(selected.light.constant_attenuation, 1.0);
            assert_eq!(selected.light.quadratic_attenuation, 0.0);
        }
        assert_eq!(
            evaluated.lambert_at(position, Vec3::new(-1.0, 0.0, 0.0)),
            Vec3::ZERO
        );
        assert_eq!(map.world_lighting.selected().unwrap().lights[0].radius, 0.0);
    }
}

#[test]
fn style_class_composition_replaces_ordinary_ties_and_rebuilds_folded_ambient() {
    let position = Vec3::new(100.0, 100.0, 100.0);
    let mut lights = [
        Vec3::new(64.0, 0.0, 0.0),
        Vec3::new(0.0, 64.0, 0.0),
        Vec3::new(0.0, 0.0, 64.0),
        Vec3::new(0.0, -64.0, 0.0),
        Vec3::new(-64.0, 0.0, 0.0),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, offset)| {
        let mut light = point(index, position + offset);
        light.intensity = [0.5; 3];
        light.constant_attenuation = 1.0;
        light.quadratic_attenuation = 0.0;
        light
    })
    .collect::<Vec<_>>();
    lights[0].style = 32;
    let map = map(&lights);
    let mut factory = ModelLightingFactory::new(
        &map.ambient,
        &map.world_lighting,
        map.lighting_collision.as_ref().unwrap(),
        None,
    )
    .unwrap();
    let seed = factory.seed_dynamic(position).unwrap();
    let mut styles = [1.0; 128];
    styles[32] = 0.0;
    let dark = seed.evaluate(&styles, 4, 0.0002).unwrap();
    assert_eq!(
        dark.selected
            .iter()
            .map(|slot| slot.light.original_index)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert!(dark.folded_ids.is_empty());
    styles[32] = 1.0;
    let equal = seed.evaluate(&styles, 4, 0.0002).unwrap();
    assert_eq!(
        equal
            .selected
            .iter()
            .map(|slot| slot.light.original_index)
            .collect::<Vec<_>>(),
        [0, 2, 3, 4]
    );
    assert_eq!(equal.folded_ids, [1]);
    assert!(equal.ambient_cube.colors[2][0] > 0.4);
    styles[32] = 0.5;
    let weaker = seed.evaluate(&styles, 4, 0.0002).unwrap();
    assert_eq!(
        weaker
            .selected
            .iter()
            .map(|slot| slot.light.original_index)
            .collect::<Vec<_>>(),
        [0, 2, 3, 4]
    );
    assert_eq!(weaker.folded_ids, [1]);
    styles[32] = 0.0001;
    let negligible = seed.evaluate(&styles, 4, 0.0002).unwrap();
    assert_eq!(
        negligible
            .selected
            .iter()
            .map(|slot| slot.light.original_index)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert!(negligible.folded_ids.is_empty());
}

#[test]
fn static_seed_keeps_exact_point_and_separates_spherical_from_dynamic_leaf_policy() {
    let mut in_ambient = point(0, Vec3::new(0.0, 0.0, 64.0));
    in_ambient.flags = 1;
    let mut styled = point(1, Vec3::new(64.0, 0.0, 0.0));
    styled.style = 32;
    let map = map(&[in_ambient, styled]);
    let mut factory = ModelLightingFactory::new(
        &map.ambient,
        &map.world_lighting,
        map.lighting_collision.as_ref().unwrap(),
        None,
    )
    .unwrap();
    let point = Vec3::new(0.25, 0.5, 0.75);
    let bounds = MapBounds {
        min: point - Vec3::splat(8.0),
        max: point + Vec3::splat(8.0),
    };
    let cube = AmbientCube {
        colors: [Vec3::new(0.1, 0.2, 0.3); 6],
    };
    let static_seed = factory.seed_static(point, bounds, cube).unwrap();
    let dynamic_seed = factory.seed_dynamic(point).unwrap();
    assert_eq!(static_seed.sample_point, point);
    assert_ne!(dynamic_seed.sample_point, point);
    assert_eq!(static_seed.ambient_cube, cube);
    assert_ne!(dynamic_seed.ambient_cube, cube);
    assert_eq!(
        static_seed
            .evaluate(&[1.0; 128], 4, 0.0002)
            .unwrap()
            .selected
            .iter()
            .map(|selected| selected.light.original_index)
            .collect::<Vec<_>>(),
        [0]
    );
    assert_eq!(
        dynamic_seed
            .evaluate(&[1.0; 128], 4, 0.0002)
            .unwrap()
            .selected
            .iter()
            .map(|selected| selected.light.original_index)
            .collect::<Vec<_>>(),
        [1]
    );
}

#[test]
fn source_inside_opaque_does_not_use_exiting_trace_fraction_as_clear_visibility() {
    assert!(source_visible(8.0, 1.0, true));
    assert!(!source_visible(8.001, 1.0, true));
    assert!(!source_visible(64.0, 1.0, true));
    assert!(source_visible(64.0, 1.0, false));
    assert!(source_visible(16.0, 0.5, false));
    assert!(!source_visible(16.001, 0.5, false));
}

fn map(lights: &[MapWorldLight]) -> MapData {
    fn append(bytes: &mut Vec<u8>, id: usize, raw: &[u8], version: i32) {
        let offset = bytes.len() as i32;
        let at = 8 + id * 16;
        bytes[at..at + 4].copy_from_slice(&offset.to_le_bytes());
        bytes[at + 4..at + 8].copy_from_slice(&(raw.len() as i32).to_le_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&version.to_le_bytes());
        bytes.extend_from_slice(raw);
    }
    let mut bytes = vec![0; 1036];
    bytes[..4].copy_from_slice(b"VBSP");
    bytes[4..8].copy_from_slice(&20_i32.to_le_bytes());
    let mut plane = vec![0; 20];
    plane[..4].copy_from_slice(&1_f32.to_le_bytes());
    plane[12..16].copy_from_slice(&8192_f32.to_le_bytes());
    append(&mut bytes, 1, &plane, 0);
    let mut node = vec![0; 32];
    node[4..8].copy_from_slice(&(-1_i32).to_le_bytes());
    node[8..12].copy_from_slice(&(-1_i32).to_le_bytes());
    append(&mut bytes, 5, &node, 0);
    let mut leaf = vec![0; 32];
    for i in 0..3 {
        leaf[8 + i * 2..10 + i * 2].copy_from_slice(&(-16384_i16).to_le_bytes());
        leaf[14 + i * 2..16 + i * 2].copy_from_slice(&16384_i16.to_le_bytes());
    }
    leaf[28..30].copy_from_slice(&(-1_i16).to_le_bytes());
    append(&mut bytes, 10, &leaf, 1);
    append(&mut bytes, 14, &[0; 48], 0);
    let mut raw = Vec::new();
    for l in lights {
        let mut row = Vec::new();
        for value in l.origin.into_iter().chain(l.intensity).chain(l.normal) {
            row.extend(value.to_le_bytes());
        }
        for value in [l.cluster, l.kind.raw(), l.style] {
            row.extend(value.to_le_bytes());
        }
        for value in [
            l.stopdot,
            l.stopdot2,
            l.exponent,
            l.radius,
            l.constant_attenuation,
            l.linear_attenuation,
            l.quadratic_attenuation,
        ] {
            row.extend(value.to_le_bytes());
        }
        row.extend(l.flags.to_le_bytes());
        row.extend(l.texinfo.to_le_bytes());
        row.extend(l.owner.to_le_bytes());
        assert_eq!(row.len(), 88);
        raw.extend(row);
    }
    append(&mut bytes, 15, &raw, 0);
    append(&mut bytes, 52, &[1, 0, 0, 0], 0);
    let mut ambient = [0; 28];
    ambient[24..27].copy_from_slice(&[128; 3]);
    append(&mut bytes, 56, &ambient, 0);
    load_map(&bytes).unwrap()
}

#[test]
fn native_five_points_preserve_direct_slots_and_center_space_fold() {
    let position = Vec3::new(1026.0043, 2304.0, 1037.3953);
    let lights = [
        Vec3::new(64.0, 0.0, 0.0),
        Vec3::new(0.0, 64.0, 0.0),
        Vec3::new(0.0, 0.0, 64.0),
        Vec3::new(0.0, -64.0, 0.0),
        Vec3::new(-64.0, 0.0, 0.0),
    ]
    .map(|offset| position + offset);
    let map = map(&lights
        .into_iter()
        .enumerate()
        .map(|(i, p)| point(i, p))
        .collect::<Vec<_>>());
    let mut factory = ModelLightingFactory::new(
        &map.ambient,
        &map.world_lighting,
        map.lighting_collision.as_ref().unwrap(),
        None,
    )
    .unwrap();
    let seed = factory.seed_dynamic(position).unwrap();
    assert_eq!(seed.sample_point, Vec3::new(1028.0, 2308.0, 1032.0));
    assert_eq!(seed.leaf, 0);
    assert_eq!(
        seed.diagnostics,
        [ModelLightingDiagnostic::MissingVisibility]
    );
    let evaluated = seed.evaluate(&[1.0; 128], 4, 0.0002).unwrap();
    assert_eq!(
        evaluated
            .selected
            .iter()
            .map(|l| l.light.original_index)
            .collect::<Vec<_>>(),
        [0, 1, 4, 3]
    );
    assert_eq!(evaluated.folded_ids, [2]);
    let expected = [
        0.49999997, 0.51215464, 0.49999997, 0.52436143, 0.42264205, 0.0,
    ];
    for (side, expected) in expected.into_iter().enumerate() {
        let mut normal = Vec3::ZERO;
        normal[side / 2] = if side % 2 == 0 { 1.0 } else { -1.0 };
        assert!((evaluated.lambert_at(position, normal)[0] - expected).abs() < 2.0e-7);
    }
    let neighbor = factory
        .seed_dynamic(position + Vec3::new(0.5, 0.0, 0.0))
        .unwrap();
    assert!(Arc::ptr_eq(&seed, &neighbor));
}

#[test]
fn dark_style_stays_spatially_eligible_and_rebuilds_folded_ambient() {
    let position = Vec3::new(1026.0043, 2304.0, 1037.3953);
    let mut lights = [
        Vec3::new(64.0, 0.0, 0.0),
        Vec3::new(0.0, 64.0, 0.0),
        Vec3::new(0.0, 0.0, 64.0),
        Vec3::new(0.0, -64.0, 0.0),
        Vec3::new(-64.0, 0.0, 0.0),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, p)| point(i, position + p))
    .collect::<Vec<_>>();
    lights[4].style = 32;
    let map = map(&lights);
    let mut factory = ModelLightingFactory::new(
        &map.ambient,
        &map.world_lighting,
        map.lighting_collision.as_ref().unwrap(),
        None,
    )
    .unwrap();
    let seed = factory.seed_dynamic(position).unwrap();
    let mut styles = [1.0; 128];
    styles[32] = 0.0;
    let dark = seed.evaluate(&styles, 4, 0.0002).unwrap();
    assert!(dark.folded_ids.is_empty());
    assert_eq!(
        dark.selected
            .iter()
            .map(|l| l.light.original_index)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    styles[32] = 2.0;
    let bright = seed.evaluate(&styles, 4, 0.0002).unwrap();
    assert_eq!(bright.folded_ids, [2]);
    assert_eq!(bright.selected[2].light.style, 32);
    assert_eq!(bright.selected[2].style_weight, 2.0);
    assert!(bright.ambient_cube.colors[4][0] > 0.42);
    assert_eq!(seed.ambient_cube.colors, [Vec3::ZERO; 6]);
    styles[32] = f32::NAN;
    assert_eq!(
        seed.evaluate(&styles, 4, 0.0002),
        Err(ModelLightingFailure::InvalidStyle)
    );
    assert_eq!(
        seed.evaluate(&[1.0; 128], 5, 0.0002),
        Err(ModelLightingFailure::InvalidBudget)
    );
}

#[test]
fn missing_inputs_and_unsupported_emitters_remain_explicit() {
    let position = Vec3::new(100.0, 100.0, 100.0);
    let mut unsupported = point(0, position + Vec3::new(64.0, 0.0, 0.0));
    unsupported.kind = WorldLightKind::Unknown(77);
    let map = map(&[unsupported]);
    let mut factory = ModelLightingFactory::new(
        &map.ambient,
        &map.world_lighting,
        map.lighting_collision.as_ref().unwrap(),
        None,
    )
    .unwrap();
    let seed = factory.seed_dynamic(position).unwrap();
    assert!(
        seed.diagnostics
            .contains(&ModelLightingDiagnostic::UnsupportedEmitter {
                original_index: 0,
                kind: WorldLightKind::Unknown(77)
            })
    );
    assert!(
        seed.evaluate(&[1.0; 128], 4, 0.0002)
            .unwrap()
            .selected
            .is_empty()
    );
    let neutral = MapAmbientLighting::neutral();
    let mut missing = ModelLightingFactory::new(
        &neutral,
        &map.world_lighting,
        map.lighting_collision.as_ref().unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(
        missing.seed_dynamic(position),
        Err(ModelLightingFailure::MissingAmbientSpan(0))
    );
    assert!(
        ModelLightingFactory::new(
            &map.ambient,
            &MapWorldLighting::default(),
            map.lighting_collision.as_ref().unwrap(),
            None
        )
        .is_err()
    );
    assert_eq!(
        factory.seed_dynamic(Vec3::splat(f32::NAN)),
        Err(ModelLightingFailure::MissingLeaf)
    );
}
