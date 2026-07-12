#[test]
fn overlay_quad_uses_packed_u_basis_and_v_flip_flag() {
    // Wall overlay: normal +X. vbsp packs BasisU into the UV points'
    // z components; corners live in the (U, V) plane as xy pairs.
    let mut overlay = OverlayBasis {
        id: 7,
        uv_points: [
            // xy = corner (-8, -4); z components 0..2 pack BasisU = +Y.
            [-8.0, -4.0, 0.0],
            [-8.0, 4.0, 1.0],
            [8.0, 4.0, 0.0],
            // z here is the V-flip flag (0 = unflipped).
            [8.0, -4.0, 0.0],
        ],
        origin: [100.0, 200.0, 300.0],
        basis_normal: [1.0, 0.0, 0.0],
    };

    // Unflipped: V = normal x U = X x Y = +Z.
    let quad = overlay_quad_positions(overlay).expect("valid overlay");
    assert_eq!(quad[0], [100.0, 200.0 - 8.0, 300.0 - 4.0]);
    assert_eq!(quad[2], [100.0, 200.0 + 8.0, 300.0 + 4.0]);

    // Flip flag set: V becomes -Z; a decode that ignored the flag (or the
    // packing itself) cannot produce this corner.
    overlay.uv_points[3][2] = 1.0;
    let flipped = overlay_quad_positions(overlay).expect("valid overlay");
    assert_eq!(flipped[0], [100.0, 200.0 - 8.0, 300.0 + 4.0]);

    // Degenerate packed basis (all z zero) is skipped, not guessed at.
    overlay.uv_points[1][2] = 0.0;
    overlay.uv_points[3][2] = 0.0;
    assert!(overlay_quad_positions(overlay).is_none());
}
use super::*;

#[test]
fn filters_source_tool_and_sky_materials() {
    let cases = [
        ("brick/wall01", true),
        ("Tools/ToolNoDraw", false),
        ("tools/toolsclip", false),
        ("skybox/sky_day01_01up", false),
        ("custom/skybox/fake", false),
        ("sky", false),
        ("skybox", false),
    ];

    for (input, expected) in cases {
        let material = normalize_material_name(input).expect("material should normalize");
        assert_eq!(is_preview_material_visible(&material), expected, "{input}");
    }
}

#[test]
fn fan_indices_triangulate_convex_faces() {
    assert_eq!(fan_indices(0).unwrap(), Vec::<u32>::new());
    assert_eq!(fan_indices(3).unwrap(), vec![0, 1, 2]);
    assert_eq!(fan_indices(4).unwrap(), vec![0, 1, 2, 0, 2, 3]);
    assert_eq!(fan_indices(5).unwrap(), vec![0, 1, 2, 0, 2, 3, 0, 3, 4]);
}

#[test]
fn raw_texture_coords_are_texel_space() {
    let transform = [2.0, 3.0, 4.0, 5.0];

    assert_eq!(texture_coord([7.0, 11.0, 13.0], transform), 104.0);
}

#[test]
fn color_rgb_exp32_decodes_to_srgb_bytes() {
    assert_eq!(
        decode_light_sample(ColorRgbExp {
            r: 255,
            g: 128,
            b: 0,
            exponent: 0,
        }),
        [255, 188, 0, 255]
    );
    assert_eq!(
        decode_light_sample(ColorRgbExp {
            r: 128,
            g: 64,
            b: 32,
            exponent: -1,
        }),
        [137, 99, 71, 255]
    );
}

#[test]
fn ambient_decode_uses_color_rgb_exp32_to_vector_scale() {
    // ColorRGBExp32ToVector semantics: byte * 2^exp, no /255. A real
    // outdoor sample from gm_construct — (143,211,254)e-9 — must decode
    // to a sky-blue ~0.28/0.41/0.50, not the ~0.001 the lightmap formula
    // (TexLightToLinear) yields.
    let sample = ColorRgbExp {
        r: 143,
        g: 211,
        b: 254,
        exponent: -9,
    };
    let ambient = decode_ambient_sample_linear(sample);
    let lightmap = decode_light_sample_linear(sample);
    for (channel, (ambient, lightmap)) in ambient.into_iter().zip(lightmap).enumerate() {
        assert!(
            (ambient - lightmap * 255.0).abs() < 1.0e-6,
            "channel {channel}: ambient {ambient} vs lightmap*255 {}",
            lightmap * 255.0
        );
    }
    assert!(
        (ambient[2] - 0.496_093_75).abs() < 1.0e-6,
        "blue channel should be sky-bright, got {}",
        ambient[2]
    );
}

#[test]
fn ambient_cube_weights_source_sides_by_facing_normal() {
    let mut colors = [[0.0_f32; 3]; 6];
    colors[4] = [0.0, 1.0, 0.0];
    let cube = AmbientCube { colors };

    assert_eq!(cube.evaluate([0.0, 0.0, 1.0]), [0.0, 1.0, 0.0]);
    assert_eq!(cube.evaluate([0.0, 0.0, -1.0]), [0.0; 3]);
}

#[test]
fn ambient_lookup_selects_nearest_sample_in_leaf() {
    let red = AmbientCube {
        colors: [[1.0, 0.0, 0.0]; 6],
    };
    let green = AmbientCube {
        colors: [[0.0, 1.0, 0.0]; 6],
    };
    let ambient = test_ambient_lighting(vec![
        MapAmbientSample {
            position: [0.0; 3],
            cube: red,
            cluster: 7,
        },
        MapAmbientSample {
            position: [10.0, 0.0, 0.0],
            cube: green,
            cluster: 7,
        },
    ]);

    assert_eq!(ambient.cube_at([9.0, 0.0, 0.0]), green);
}

#[test]
fn load_map_uses_white_ambient_when_lumps_are_absent() {
    let map = load_map(&bsp_fixture(None)).expect("fixture bsp should load");

    assert_eq!(map.ambient.source(), AmbientLightSource::Neutral);
    assert_eq!(map.ambient.cube_at([0.0; 3]), AmbientCube::WHITE);
}

#[test]
fn lightmap_uv_math_uses_face_min_size_and_half_luxel_bias() {
    let uv = brush_lightmap_uv_from_transforms(
        [10.0, 20.0, 30.0],
        [0.5, 0.0, 0.0, 2.0],
        [0.0, 0.25, 0.0, 1.0],
        [3, 4],
        [8, 4],
    );

    assert_eq!(uv, [0.5, 0.5]);
}

#[test]
fn shelf_packer_fits_grows_and_overflows() {
    let fits = vec![block(2, 2), block(2, 2)];
    let (width, height, placements) = pack_lightmap_blocks(&fits).expect("fits");
    assert_eq!((width, height), (4, 2));
    assert!(placements.iter().all(Option::is_some));

    let grows = vec![block(600, 500), block(600, 20)];
    let (width, height, placements) = pack_lightmap_blocks(&grows).expect("grows");
    assert_eq!((width, height), (1024, 1024));
    assert!(placements.iter().all(Option::is_some));

    let overflow = vec![block(4096, 4096), block(1, 1)];
    let (width, height, placements) = pack_lightmap_blocks(&overflow).expect("overflows");
    assert_eq!((width, height), (4096, 4096));
    assert!(placements[0].is_some());
    assert!(placements[1].is_none());
}

#[test]
fn displacement_tessellation_matches_base_face_fan_winding() {
    // Column-major grid over a flat quad, matching displacement_grid's
    // layout: corners c0=(0,0) → c1 along the column axis (+x), c3
    // along the row axis (+y). The base face fan (c0,c1,c2)(c0,c2,c3)
    // winds +z, so every displacement triangle must too; reading the
    // grid transposed flips them all to -z.
    let steps = 2_usize;
    let side = steps + 1;
    let mut grid = Vec::new();
    for column in 0..side {
        for row in 0..side {
            grid.push(DisplacementGridVertex {
                position: [column as f32, row as f32, 0.0],
                alpha: 0.0,
            });
        }
    }
    let vertices = tessellate_displacement_grid(&grid, steps, side);
    assert_eq!(vertices.len(), steps * steps * 6);
    for triangle in vertices.chunks_exact(3) {
        let p = |i: usize| triangle[i].position;
        let e1 = [p(1)[0] - p(0)[0], p(1)[1] - p(0)[1]];
        let e2 = [p(2)[0] - p(0)[0], p(2)[1] - p(0)[1]];
        let winding_z = e1[0] * e2[1] - e1[1] * e2[0];
        assert!(
            winding_z > 0.0,
            "triangle wound against its base face: {:?}",
            triangle.iter().map(|v| v.position).collect::<Vec<_>>()
        );
    }
}

#[test]
fn walk_trace_hits_solid_brush_head_on_with_expanded_hull() {
    let collision = box_collision([10.0, -16.0, -16.0], [20.0, 16.0, 16.0]);
    let hit = collision.trace_aabb([0.0; 3], [30.0, 0.0, 0.0], [1.0; 3]);

    assert!(!hit.start_solid);
    assert!(hit.fraction > 0.29 && hit.fraction < 0.30, "{hit:?}");
    assert!((hit.end_position[0] - 8.96875).abs() < 1.0e-4);
    assert_eq!(hit.normal, [-1.0, 0.0, 0.0]);
}

#[test]
fn walk_trace_glancing_hit_supplies_slide_plane_normal() {
    let collision = box_collision([10.0, -16.0, -16.0], [20.0, 16.0, 16.0]);
    let start = [0.0; 3];
    let end = [30.0, 10.0, 0.0];
    let hit = collision.trace_aabb(start, end, [1.0; 3]);
    let delta = sub(end, start);
    let slide = sub(delta, mul(hit.normal, dot(delta, hit.normal)));

    assert_eq!(hit.normal, [-1.0, 0.0, 0.0]);
    assert!(slide[0].abs() < 1.0e-5, "{slide:?}");
    assert!(slide[1] > 9.9, "{slide:?}");
}

#[test]
fn walk_trace_low_ledge_clears_after_step_height_lift() {
    let collision = box_collision([10.0, -16.0, -8.0], [20.0, 16.0, 6.0]);
    let half_extents = [1.0, 1.0, 8.0];
    let start = [0.0, 0.0, 8.0];
    let end = [30.0, 0.0, 8.0];

    let blocked = collision.trace_aabb(start, end, half_extents);
    let lifted = collision.trace_aabb(
        add(start, [0.0, 0.0, 18.0]),
        add(end, [0.0, 0.0, 18.0]),
        half_extents,
    );

    assert!(blocked.fraction < 1.0, "{blocked:?}");
    assert_eq!(blocked.normal, [-1.0, 0.0, 0.0]);
    assert_eq!(lifted.fraction, 1.0);
}

#[test]
fn prop_trace_blocks_and_resting_contact_uses_same_solidity_predicate() {
    let ledge = box_ledge([10.0, -16.0, -16.0], [20.0, 16.0, 16.0]);
    let collision = empty_walk_collision().with_prop_collisions([MapWalkPropCollisionSource {
        ledges: std::slice::from_ref(&ledge),
        origin: [0.0; 3],
        angles: [0.0; 3],
        scale: 1.0,
    }]);

    let hit = collision.trace_aabb([0.0; 3], [30.0, 0.0, 0.0], [1.0; 3]);
    assert_eq!(collision.prop_hull_count(), 1);
    assert!(!hit.start_solid);
    assert!(hit.fraction > 0.29 && hit.fraction < 0.30, "{hit:?}");
    assert_eq!(hit.normal, [-1.0, 0.0, 0.0]);

    let half_extents = [1.0, 1.0, 1.0];
    assert!(!collision.aabb_trace_solid([15.0, 0.0, 17.0 + TRACE_PLANE_EPSILON], half_extents));
    assert!(collision.aabb_trace_solid([15.0, 0.0, 16.5], half_extents));
}

#[test]
fn prop_bevel_planes_prevent_wedge_corner_misclassification() {
    let ledge = wedge_ledge();
    let with_local = local_prop_brush_from_hull(&ledge, true).expect("beveled local wedge");
    let face_local = local_prop_brush_from_hull(&ledge, false).expect("face-only local wedge");
    let with_model = MapWalkPropModel {
        brushes: vec![with_local],
    };
    let face_model = MapWalkPropModel {
        brushes: vec![face_local],
    };
    let with_source = MapWalkPropModelPlacement {
        model: &with_model,
        origin: [0.0; 3],
        angles: [0.0; 3],
        scale: 1.0,
    };
    let face_source = MapWalkPropModelPlacement {
        model: &face_model,
        ..with_source
    };
    let with_bevels =
        prop_brush_from_local(&with_model.brushes[0], with_source).expect("beveled wedge");
    let face_only =
        prop_brush_from_local(&face_model.brushes[0], face_source).expect("face-only wedge");
    let half_extents = [8.0, 8.0, 8.0];
    let directions = [
        [96.0, 96.0, 0.0],
        [96.0, 0.0, 96.0],
        [0.0, 96.0, 96.0],
        [96.0, 96.0, 96.0],
        [-96.0, 96.0, 0.0],
        [96.0, -96.0, 0.0],
    ];
    let mut repro = None;
    'search: for x in (-32..=80).step_by(8) {
        for y in (-32..=80).step_by(8) {
            for z in (-32..=80).step_by(8) {
                let start = [x as f32, y as f32, z as f32];
                for direction in directions {
                    let end = add(start, direction);
                    let beveled = trace_brush_aabb(&with_bevels, start, end, half_extents);
                    let plain = trace_brush_aabb(&face_only, start, end, half_extents);
                    if beveled.is_none() && plain.is_some() {
                        repro = Some((start, end));
                        break 'search;
                    }
                }
            }
        }
    }
    let (start, end) = repro.expect("synthetic wedge should expose a bevel flip");
    assert!(
        trace_brush_aabb(&with_bevels, start, end, half_extents).is_none(),
        "beveled wedge overblocked {start:?}->{end:?}"
    );
    assert!(
        trace_brush_aabb(&face_only, start, end, half_extents).is_some(),
        "face-only wedge unexpectedly passed {start:?}->{end:?}"
    );
}

#[test]
fn walk_trace_playerclip_brush_blocks() {
    let brush = walk_brush_from_planes(
        box_planes([10.0, -16.0, -16.0], [20.0, 16.0, 16.0]),
        contents_flags::PLAYERCLIP,
    )
    .expect("playerclip brush should enter walk collision");
    let collision = MapWalkCollision {
        brushes: vec![brush],
        water_brushes: Vec::new(),
        displacements: Vec::new(),
        props: MapWalkPropCollision::default(),
    };

    let hit = collision.trace_aabb([0.0; 3], [30.0, 0.0, 0.0], [1.0; 3]);

    assert!(hit.fraction < 1.0, "{hit:?}");
    assert_eq!(hit.normal, [-1.0, 0.0, 0.0]);
}

#[test]
fn walk_trace_water_only_brush_does_not_block() {
    let water = walk_brush_from_planes(
        box_planes([10.0, -16.0, -16.0], [20.0, 16.0, 16.0]),
        contents_flags::WATER,
    )
    .expect("water brush should enter volume collision");
    let collision = MapWalkCollision {
        brushes: Vec::new(),
        water_brushes: vec![water],
        displacements: Vec::new(),
        props: MapWalkPropCollision::default(),
    };

    let hit = collision.trace_aabb([0.0; 3], [30.0, 0.0, 0.0], [1.0; 3]);

    assert_eq!(hit.fraction, 1.0);
    assert!(!hit.start_solid);
}

#[test]
fn water_at_reports_inside_outside_and_surface_height() {
    let collision = MapWalkCollision::empty()
        .with_water_box_for_tests([-32.0, -24.0, -16.0], [32.0, 24.0, 48.0]);

    assert_eq!(
        collision.water_at([4.0, -3.0, 12.0]),
        Some(WaterVolume { surface_z: 48.0 })
    );
    assert_eq!(collision.water_at([40.0, 0.0, 12.0]), None);
    assert_eq!(collision.water_at([0.0, 0.0, 49.0]), None);
}

#[test]
fn water_at_uses_highest_surface_for_stacked_volumes() {
    let collision = MapWalkCollision::empty()
        .with_water_box_for_tests([-32.0; 3], [32.0, 32.0, 24.0])
        .with_water_box_for_tests([-16.0, -16.0, -8.0], [16.0, 16.0, 48.0]);

    assert_eq!(
        collision.water_at([0.0; 3]),
        Some(WaterVolume { surface_z: 48.0 })
    );
    assert!(
        walk_brush_from_planes(box_planes([-1.0; 3], [1.0; 3]), contents_flags::SLIME).is_some()
    );
}

#[test]
fn brush_side_sky_flag_accepts_sky_texinfo_and_ignores_plain_or_bevel_sides() {
    let real_side = BrushSide {
        plane: 0,
        texinfo: 0,
        displacement: -1,
        bevel: 0,
    };
    let bevel_side = BrushSide {
        bevel: 1,
        ..real_side
    };

    assert!(brush_side_sky_from_texture_flags(
        &real_side,
        Some(texture_flags::SKY)
    ));
    assert!(brush_side_sky_from_texture_flags(
        &real_side,
        Some(texture_flags::SKY2D)
    ));
    assert!(!brush_side_sky_from_texture_flags(&real_side, Some(0)));
    assert!(!brush_side_sky_from_texture_flags(
        &bevel_side,
        Some(texture_flags::SKY)
    ));
    assert!(!brush_side_sky_from_texture_flags(&real_side, None));
}

#[test]
fn point_ray_reports_sky_only_for_first_sky_side_or_sky_escape() {
    let sky_ceiling = MapWalkCollision {
        brushes: vec![box_brush_with_sky_side(
            [-16.0, -16.0, 10.0],
            [16.0, 16.0, 20.0],
            5,
        )],
        water_brushes: Vec::new(),
        displacements: Vec::new(),
        props: MapWalkPropCollision::default(),
    };
    assert!(sky_ceiling.ray_hits_sky([0.0, 0.0, 0.0], [0.0, 0.0, 1.0]));
    assert!(sky_ceiling.ray_hits_sky([64.0, 0.0, 0.0], [0.0, 0.0, 1.0]));

    let plain_ceiling = MapWalkCollision {
        brushes: vec![box_brush_with_sky_side(
            [-16.0, -16.0, 10.0],
            [16.0, 16.0, 20.0],
            usize::MAX,
        )],
        water_brushes: Vec::new(),
        displacements: Vec::new(),
        props: MapWalkPropCollision::default(),
    };
    assert!(!plain_ceiling.ray_hits_sky([0.0, 0.0, 0.0], [0.0, 0.0, 1.0]));
    assert!(!plain_ceiling.ray_hits_sky([64.0, 0.0, 0.0], [0.0, 0.0, 1.0]));
}

#[test]
fn walk_trace_displacement_triangle_hits_from_above() {
    let triangle =
        MapWalkTriangle::new([[-64.0, -64.0, 0.0], [64.0, -64.0, 0.0], [-64.0, 64.0, 0.0]])
            .expect("valid triangle");
    let collision = MapWalkCollision {
        brushes: Vec::new(),
        water_brushes: Vec::new(),
        displacements: vec![MapWalkDisplacement {
            triangles: vec![triangle],
            bounds_min: triangle.bounds_min,
            bounds_max: triangle.bounds_max,
        }],
        props: MapWalkPropCollision::default(),
    };

    let hit = collision.trace_aabb([0.0, 0.0, 100.0], [0.0, 0.0, -20.0], [16.0, 16.0, 36.0]);

    assert!(!hit.start_solid);
    assert!(hit.fraction > 0.53 && hit.fraction < 0.54, "{hit:?}");
    assert!(hit.normal[2] > 0.99, "{hit:?}");
    assert!((hit.end_position[2] - 36.0).abs() < 0.1, "{hit:?}");
}

#[test]
fn displacement_alpha_is_normalized_and_clamped() {
    assert_eq!(displacement_blend_alpha(0.0), 0.0);
    assert_eq!(displacement_blend_alpha(127.5), 0.5);
    assert_eq!(displacement_blend_alpha(255.0), 1.0);
    assert_eq!(displacement_blend_alpha(300.0), 1.0);
}

#[test]
fn pakfile_paths_are_normalized_and_limited_to_preview_entries() {
    assert_eq!(
        normalize_pakfile_path(r"\Materials\Props\Door.VMT").as_deref(),
        Some("materials/props/door.vmt")
    );
    assert_eq!(
        normalize_pakfile_path("materials/props/door.vtf").as_deref(),
        Some("materials/props/door.vtf")
    );
    assert_eq!(normalize_pakfile_path("../escape.vmt"), None);
    assert!(is_pakfile_retained_entry("materials/props/door.vmt"));
    assert!(is_pakfile_retained_entry("materials/props/door.vtf"));
    assert!(is_pakfile_retained_entry("models/props/tree.mdl"));
    assert!(is_pakfile_retained_entry("models/props/tree.dx90.vtx"));
    assert!(is_pakfile_retained_entry("models/props/tree.vvd"));
    assert!(is_pakfile_retained_entry("models/props/tree.phy"));
    assert!(!is_pakfile_retained_entry("maps/thumb.png"));
    assert!(!is_pakfile_entry_oversized(MAX_PAKFILE_ENTRY_BYTES));
    assert!(is_pakfile_entry_oversized(
        MAX_PAKFILE_ENTRY_BYTES.saturating_add(1)
    ));
}

#[test]
fn static_prop_model_paths_are_normalized_and_limited_to_mdl() {
    assert_eq!(
        normalize_static_prop_model_path(r"\Models\Props_C17\Oildrum001.MDL").as_deref(),
        Some("models/props_c17/oildrum001.mdl")
    );
    assert_eq!(normalize_static_prop_model_path("../escape.mdl"), None);
    assert_eq!(
        normalize_static_prop_model_path("models/props/tree.vtx"),
        None
    );
}

#[test]
fn entity_prop_model_paths_must_be_models_mdl() {
    assert_eq!(
        normalize_entity_prop_model_path(r"\Models\Props_C17\Oildrum001.MDL").as_deref(),
        Some("models/props_c17/oildrum001.mdl")
    );
    assert_eq!(normalize_entity_prop_model_path("props/tree.mdl"), None);
    assert_eq!(
        normalize_entity_prop_model_path("models/props/tree.vtx"),
        None
    );
}

#[test]
fn load_map_extracts_normalized_worldspawn_skyname() {
    let map = load_map(&bsp_fixture_with_entities(
        r#"
        {
            "classname" "WorldSpawn"
            "skyname" "Sky_Day01_01"
        }
        {
            "classname" "light_environment"
            "skyname" "wrong"
        }
        "#,
    ))
    .expect("fixture bsp should load");

    assert_eq!(map.skyname.as_deref(), Some("sky_day01_01"));
}

#[test]
fn load_map_omits_absent_or_empty_worldspawn_skyname() {
    let absent = load_map(&bsp_fixture_with_entities(
        r#"{ "classname" "worldspawn" "mapversion" "1" }"#,
    ))
    .expect("fixture bsp should load");
    let empty = load_map(&bsp_fixture_with_entities(
        r#"{ "classname" "worldspawn" "skyname" "   " }"#,
    ))
    .expect("fixture bsp should load");

    assert_eq!(absent.skyname, None);
    assert_eq!(empty.skyname, None);
}

#[test]
fn light_environment_parses_direction_light_and_ambient() {
    let entities = parse_test_entities(
        r#"
        {
            "classname" "light_environment"
            "angles" "45 90 0"
            "pitch" "-30"
            "_light" "255 128 0 128"
            "_ambient" "64 128 255 64"
        }
        "#,
    );

    let lighting = map_environment_lighting(&entities).expect("light_environment");
    let sun = lighting.sun.expect("sun term");

    assert_vec3_close(sun.direction_to_sun, [0.0, -0.866_025_4, 0.5]);
    // VRAD LightForString semantics: linear = (c/255)^2.2 * (intensity/255).
    let expected_channel = |channel: f32, intensity: f32| -> f32 {
        (channel / 255.0_f32).powf(2.2) * (intensity / 255.0)
    };
    assert_vec3_close(
        sun.color_linear,
        [
            expected_channel(255.0, 128.0),
            expected_channel(128.0, 128.0),
            0.0,
        ],
    );
    assert_vec3_close(
        lighting.skylight_linear.expect("skylight"),
        [
            expected_channel(64.0, 64.0),
            expected_channel(128.0, 64.0),
            expected_channel(255.0, 64.0),
        ],
    );
}

#[test]
fn light_environment_accepts_bright_intensity_and_three_component_light() {
    // Real maps commonly set sun intensity above 255 (300-500); the parser
    // must not reject it, and the 3-component form implies intensity 255.
    let entities = parse_test_entities(
        r#"
        {
            "classname" "light_environment"
            "pitch" "-60"
            "_light" "255 241 224 400"
            "_ambient" "160 180 200"
        }
        "#,
    );
    let lighting = map_environment_lighting(&entities).expect("light_environment");
    let sun = lighting.sun.expect("bright sun survives");

    assert_vec3_close(
        sun.color_linear,
        [
            400.0 / 255.0,
            (241.0 / 255.0_f32).powf(2.2) * (400.0 / 255.0),
            (224.0 / 255.0_f32).powf(2.2) * (400.0 / 255.0),
        ],
    );
    assert_vec3_close(
        lighting.skylight_linear.expect("three-component ambient"),
        [
            (160.0 / 255.0_f32).powf(2.2),
            (180.0 / 255.0_f32).powf(2.2),
            (200.0 / 255.0_f32).powf(2.2),
        ],
    );
}

#[test]
fn light_environment_missing_or_malformed_keys_drop_only_that_term() {
    assert_eq!(
        map_environment_lighting(&parse_test_entities(r#"{ "classname" "worldspawn" }"#)),
        None
    );

    let entities = parse_test_entities(
        r#"
        {
            "classname" "light_environment"
            "angles" "0 180 0"
            "_light" "255 nope 255 255"
            "_ambient" "0 128 255 255"
        }
        "#,
    );
    let lighting = map_environment_lighting(&entities).expect("light_environment");

    assert_eq!(lighting.sun, None);
    assert_vec3_close(
        lighting.skylight_linear.expect("valid ambient survives"),
        [0.0, (128.0 / 255.0_f32).powf(2.2), 1.0],
    );
}

#[test]
fn load_map_extracts_first_info_player_start() {
    let map = load_map(&bsp_fixture_with_entities(
        r#"
        {
            "classname" "info_player_start"
            "origin" "128 -64 32"
            "angles" "5 90 0"
        }
        {
            "classname" "info_player_start"
            "origin" "1 2 3"
            "angles" "0 180 0"
        }
        "#,
    ))
    .expect("fixture bsp should load");

    assert_eq!(
        map.player_start,
        Some(MapPlayerStart {
            origin: [128.0, -64.0, 32.0],
            angles: [5.0, 90.0, 0.0],
        })
    );
}

#[test]
fn load_map_omits_invalid_info_player_start_and_accepts_legacy_angle() {
    let invalid = load_map(&bsp_fixture_with_entities(
        r#"{ "classname" "info_player_start" "origin" "nan 0 0" }"#,
    ))
    .expect("fixture bsp should load");
    let legacy = load_map(&bsp_fixture_with_entities(
        r#"{ "classname" "info_player_start" "origin" "1 2 3" "angle" "270" }"#,
    ))
    .expect("fixture bsp should load");

    assert_eq!(invalid.player_start, None);
    assert_eq!(
        legacy.player_start,
        Some(MapPlayerStart {
            origin: [1.0, 2.0, 3.0],
            angles: [0.0, 270.0, 0.0],
        })
    );
}

#[test]
fn load_map_extracts_first_enabled_env_fog_controller() {
    let map = load_map(&bsp_fixture_with_entities(
        r#"
        {
            "classname" "env_fog_controller"
            "fogenable" "0"
            "fogcolor" "1 2 3"
            "fogstart" "10"
            "fogend" "20"
        }
        {
            "classname" "env_fog_controller"
            "fogenable" "1"
            "fogcolor" "64 128 255"
            "fogstart" "256.5"
            "fogend" "2048"
            "fogmaxdensity" "0.75"
        }
        {
            "classname" "env_fog_controller"
            "fogenable" "1"
            "fogcolor" "255 0 0"
            "fogstart" "1"
            "fogend" "2"
        }
        "#,
    ))
    .expect("fixture bsp should load");

    assert_eq!(
        map.fog,
        Some(MapFog {
            color_srgb: [64, 128, 255],
            start: 256.5,
            end: 2048.0,
            max_density: 0.75,
        })
    );
}

#[test]
fn load_map_omits_disabled_env_fog_controller() {
    let map = load_map(&bsp_fixture_with_entities(
        r#"
        {
            "classname" "env_fog_controller"
            "fogenable" "0"
            "fogcolor" "64 128 255"
            "fogstart" "256"
            "fogend" "2048"
            "fogmaxdensity" "0.75"
        }
        "#,
    ))
    .expect("fixture bsp should load");

    assert_eq!(map.fog, None);
}

#[test]
fn load_map_omits_missing_env_fog_controller() {
    let map = load_map(&bsp_fixture_with_entities(
        r#"{ "classname" "worldspawn" "mapversion" "1" }"#,
    ))
    .expect("fixture bsp should load");

    assert_eq!(map.fog, None);
}

#[test]
fn load_map_treats_garbage_env_fog_controller_values_as_absent() {
    for entities in [
        r#"
        {
            "classname" "env_fog_controller"
            "fogenable" "1"
            "fogcolor" "64 nope 255"
            "fogstart" "256"
            "fogend" "2048"
        }
        "#,
        r#"
        {
            "classname" "env_fog_controller"
            "fogenable" "1"
            "fogcolor" "64 128 255"
            "fogstart" "start"
            "fogend" "2048"
        }
        "#,
        r#"
        {
            "classname" "env_fog_controller"
            "fogenable" "1"
            "fogcolor" "64 128 255"
            "fogstart" "2048"
            "fogend" "256"
        }
        "#,
    ] {
        let map = load_map(&bsp_fixture_with_entities(entities))
            .expect("fixture bsp should load despite garbage fog values");

        assert_eq!(map.fog, None);
    }
}

#[test]
fn load_map_defaults_missing_or_garbage_fog_density_to_one() {
    for density in [None, Some(r#""fogmaxdensity" "garbage""#)] {
        let density = density.unwrap_or("");
        let entities = format!(
            r#"
            {{
                "classname" "env_fog_controller"
                "fogenable" "1"
                "fogcolor" "64 128 255"
                "fogstart" "256"
                "fogend" "2048"
                {density}
            }}
            "#
        );
        let map = load_map(&bsp_fixture_with_entities(&entities)).expect("fixture bsp should load");

        assert_eq!(map.fog.unwrap().max_density, 1.0);
    }
}

#[test]
fn load_map_extracts_sky_camera_origin_scale_and_fog() {
    let map = load_map(&bsp_fixture_with_entities(
        r#"
        {
            "classname" "sky_camera"
            "origin" "10 20 30"
            "scale" "8"
            "fogenable" "1"
            "fogcolor" "32 64 128"
            "fogstart" "100"
            "fogend" "900"
            "fogmaxdensity" "0.5"
        }
        "#,
    ))
    .expect("fixture bsp should load");

    assert_eq!(
        map.sky_camera,
        Some(MapSkyCamera {
            origin: [10.0, 20.0, 30.0],
            scale: 8.0,
            fog: Some(MapFog {
                color_srgb: [32, 64, 128],
                start: 100.0,
                end: 900.0,
                max_density: 0.5,
            }),
        })
    );
}

#[test]
fn load_map_defaults_missing_or_nonpositive_sky_camera_scale_to_sixteen() {
    for scale in [
        "",
        r#""scale" "0""#,
        r#""scale" "-2""#,
        r#""scale" "garbage""#,
    ] {
        let entities = format!(
            r#"
            {{
                "classname" "sky_camera"
                "origin" "10 20 30"
                {scale}
            }}
            "#
        );
        let map = load_map(&bsp_fixture_with_entities(&entities)).expect("fixture bsp should load");

        assert_eq!(map.sky_camera.unwrap().scale, 16.0);
    }
}

#[test]
fn load_map_omits_disabled_or_malformed_sky_camera_fog() {
    for fog in [
        r#"
        "fogenable" "0"
        "fogcolor" "32 64 128"
        "fogstart" "100"
        "fogend" "900"
        "#,
        r#"
        "fogenable" "2"
        "fogcolor" "32 64 128"
        "fogstart" "100"
        "fogend" "900"
        "#,
        r#"
        "fogenable" "1"
        "fogcolor" "32 nope 128"
        "fogstart" "100"
        "fogend" "900"
        "#,
    ] {
        let entities = format!(
            r#"
            {{
                "classname" "sky_camera"
                "origin" "10 20 30"
                {fog}
            }}
            "#
        );
        let map = load_map(&bsp_fixture_with_entities(&entities)).expect("fixture bsp should load");

        assert_eq!(map.sky_camera.unwrap().fog, None);
    }
}

#[test]
fn load_map_extracts_static_prop_placements_from_game_lump_fixture() {
    let map = load_map(&static_prop_bsp_fixture()).expect("fixture bsp should load");

    assert_eq!(map.stats.static_prop_count, 1);
    assert_eq!(
        map.static_props,
        vec![StaticPropPlacement {
            model_path: "models/props_c17/oildrum001.mdl".to_owned(),
            origin: [10.0, 20.0, 30.0],
            angles: [1.0, 90.0, 3.0],
            skin: 2,
            scale: 1.0,
            solid: MapPropSolid::None,
            visibility: MapPropVisibility::Always,
        }]
    );
}

#[test]
fn load_map_uses_static_prop_leaf_lists_for_multi_cluster_visibility() {
    let bytes = skybox_partition_bsp_fixture(false, true);
    let map = load_map(&bytes).expect("fixture bsp should load");
    let prop = map
        .static_props
        .iter()
        .find(|prop| prop.model_path == "models/props_c17/oildrum001.mdl")
        .expect("fixture static prop");

    assert_eq!(prop.visibility, MapPropVisibility::Clusters(vec![0, 1]));
}

#[test]
fn load_map_uses_static_prop_leaf_lists_when_origin_is_in_solid() {
    let bytes = in_solid_static_prop_bsp_fixture();
    let map = load_map(&bytes).expect("fixture bsp should load");
    let visibility = map.visibility.as_ref().expect("fixture vis data");
    let prop = map
        .static_props
        .iter()
        .find(|prop| prop.model_path == "models/props_c17/oildrum001.mdl")
        .expect("fixture static prop");

    assert_eq!(visibility.cluster_at(prop.origin), None);
    assert_eq!(prop.visibility, MapPropVisibility::Clusters(vec![0, 1]));
}

#[test]
fn load_map_extracts_supported_entity_prop_classnames() {
    let map = load_map(&bsp_fixture_with_entities(
        r#"
        { "classname" "prop_dynamic" "model" "models/entity/dynamic.mdl" "origin" "1 2 3" "angles" "10 20 30" "skin" "4" "modelscale" "2.5" }
        { "classname" "prop_dynamic_override" "model" "models/entity/dynamic_override.mdl" "origin" "4 5 6" "angles" "11 21 31" }
        { "classname" "prop_physics" "model" "models/entity/physics.mdl" "origin" "7 8 9" "angles" "12 22 32" "skin" "6" }
        { "classname" "prop_physics_multiplayer" "model" "models/entity/physics_multiplayer.mdl" "origin" "10 11 12" "angles" "13 23 33" "skin" "7" }
        { "classname" "prop_physics_override" "model" "models/entity/physics_override.mdl" "origin" "13 14 15" "angles" "14 24 34" "skin" "8" }
        "#,
    ))
    .expect("fixture bsp should load");

    let entity_props = map
        .static_props
        .iter()
        .filter(|prop| prop.model_path.starts_with("models/entity/"))
        .collect::<Vec<_>>();

    assert_eq!(map.stats.entity_prop_count, 5);
    assert_eq!(entity_props.len(), 5);
    assert_eq!(entity_props[0].model_path, "models/entity/dynamic.mdl");
    assert_eq!(entity_props[0].origin, [1.0, 2.0, 3.0]);
    assert_eq!(entity_props[0].angles, [10.0, 20.0, 30.0]);
    assert_eq!(entity_props[0].skin, 4);
    assert_eq!(entity_props[0].scale, 2.5);
    assert_eq!(entity_props[0].solid, MapPropSolid::None);
    assert_eq!(entity_props[1].skin, 0);
    assert_eq!(entity_props[1].scale, 1.0);
    assert_eq!(entity_props[4].angles, [14.0, 24.0, 34.0]);
    assert!(
        entity_props
            .iter()
            .all(|prop| prop.visibility == MapPropVisibility::Always)
    );
}

#[test]
fn load_map_uses_entity_prop_solid_key_for_physics_collision() {
    let map = load_map(&bsp_fixture_with_entities(
        r#"
        { "classname" "prop_dynamic" "model" "models/entity/solid6.mdl" "origin" "1 0 0" "solid" "6" "startdisabled" "0" }
        { "classname" "prop_dynamic" "model" "models/entity/solid0.mdl" "origin" "2 0 0" "solid" "0" }
        { "classname" "prop_dynamic" "model" "models/entity/disabled.mdl" "origin" "3 0 0" "solid" "6" "startdisabled" "1" }
        { "classname" "prop_physics" "model" "models/entity/missing.mdl" "origin" "4 0 0" }
        "#,
    ))
    .expect("fixture bsp should load");
    let solids = map
        .static_props
        .iter()
        .filter(|prop| prop.model_path.starts_with("models/entity/"))
        .map(|prop| (prop.model_path.as_str(), prop.solid))
        .collect::<Vec<_>>();

    assert_eq!(
        solids,
        vec![
            ("models/entity/solid6.mdl", MapPropSolid::Physics),
            ("models/entity/solid0.mdl", MapPropSolid::None),
            ("models/entity/disabled.mdl", MapPropSolid::None),
            ("models/entity/missing.mdl", MapPropSolid::None),
        ]
    );
}

#[test]
fn load_map_degrades_entity_prop_fields_per_entity() {
    let map = load_map(&bsp_fixture_with_entities(
        r#"
        { "classname" "prop_dynamic" "model" "models/entity/defaults.mdl" "origin" "1 2 3" "angles" "bad" "skin" "bad" "modelscale" "-2" }
        { "classname" "prop_dynamic" "origin" "1 2 3" }
        { "classname" "prop_dynamic" "model" "props/not_models.mdl" "origin" "1 2 3" }
        { "classname" "prop_dynamic" "model" "models/entity/not_mdl.vtx" "origin" "1 2 3" }
        { "classname" "prop_dynamic" "model" "models/entity/missing_origin.mdl" }
        { "classname" "prop_dynamic" "model" "models/entity/bad_origin.mdl" "origin" "nan 0 0" }
        "#,
    ))
    .expect("fixture bsp should load");

    let entity_props = map
        .static_props
        .iter()
        .filter(|prop| prop.model_path.starts_with("models/entity/"))
        .collect::<Vec<_>>();

    assert_eq!(map.stats.entity_prop_count, 1);
    assert_eq!(entity_props.len(), 1);
    assert_eq!(entity_props[0].model_path, "models/entity/defaults.mdl");
    assert_eq!(entity_props[0].origin, [1.0, 2.0, 3.0]);
    assert_eq!(entity_props[0].angles, [0.0; 3]);
    assert_eq!(entity_props[0].skin, 0);
    assert_eq!(entity_props[0].scale, 1.0);
}

#[test]
fn parse_entity_prop_model_scale_defaults_invalid_values() {
    for value in ["0", "-0.1", "nan", "inf", "bad"] {
        assert_eq!(parse_entity_prop_model_scale(value, "prop_dynamic"), 1.0);
    }
    assert_eq!(
        parse_entity_prop_model_scale("1.75", "prop_dynamic_override"),
        1.75
    );
}

#[test]
fn door_keyvalues_parse_all_supported_classes_and_degrade_per_field() {
    let doors = pending_doors_from_fixture(
        r#"
        { "classname" "func_door" "model" "*1" "origin" "32 0 0" "angles" "0 90 0" "lip" "4" "spawnflags" "2048" }
        { "classname" "func_movelinear" "model" "*1" "movedir" "0 180 0" "MoveDistance" "24" "StartPosition" "0.25" "speed" "bad" "wait" "bad" }
        { "classname" "func_door_rotating" "model" "*1" "origin" "96 0 0" "spawnflags" "66" "distance" "45" "speed" "30" }
        { "classname" "prop_door_rotating" "model" "models/props_door/testdoor.mdl" "origin" "128 0 0" "angles" "0 90 0" "distance" "bad" "speed" "bad" "opendir" "1" "returndelay" "bad" }
        { "classname" "func_door" "model" "*99" }
        { "classname" "func_door" "model" "not-a-bmodel" }
        "#,
    );

    assert_eq!(doors.len(), 4);

    let linear = &doors[0];
    assert_eq!(linear.class, MapDoorClass::FuncDoor);
    assert_eq!(linear.origin, [32.0, 0.0, 0.0]);
    assert_eq!(linear.wait, 3.0);
    assert_eq!(linear.initial_progress, 0.0);
    match linear.motion {
        MapDoorMotion::Linear {
            direction,
            distance,
            speed,
        } => {
            assert_vec3_close(direction, [0.0, 1.0, 0.0]);
            assert!((distance - 34.0).abs() < 1.0e-4);
            assert_eq!(speed, 100.0);
        }
        other @ MapDoorMotion::Rotating { .. } => {
            panic!("expected linear func_door, got {other:?}")
        }
    }

    let movelinear = &doors[1];
    assert_eq!(movelinear.class, MapDoorClass::FuncMoveLinear);
    assert_eq!(movelinear.initial_progress, 0.25);
    assert_eq!(movelinear.wait, 3.0);
    match movelinear.motion {
        MapDoorMotion::Linear {
            direction,
            distance,
            speed,
        } => {
            assert_vec3_close(direction, [-1.0, 0.0, 0.0]);
            assert_eq!(distance, 24.0);
            assert_eq!(speed, 100.0);
        }
        other @ MapDoorMotion::Rotating { .. } => {
            panic!("expected linear func_movelinear, got {other:?}")
        }
    }

    let rotating = &doors[2];
    assert_eq!(rotating.class, MapDoorClass::FuncDoorRotating);
    match rotating.motion {
        MapDoorMotion::Rotating {
            angle_delta,
            degrees,
            speed,
            open_direction,
        } => {
            assert_eq!(angle_delta, [0.0, 0.0, -45.0]);
            assert_eq!(degrees, 45.0);
            assert_eq!(speed, 30.0);
            assert_eq!(open_direction, MapDoorOpenDirection::Both);
        }
        other @ MapDoorMotion::Linear { .. } => {
            panic!("expected rotating func_door_rotating, got {other:?}")
        }
    }

    let prop = &doors[3];
    assert_eq!(prop.class, MapDoorClass::PropDoorRotating);
    assert_eq!(prop.origin, [128.0, 0.0, 0.0]);
    assert_eq!(prop.angles, [0.0, 90.0, 0.0]);
    assert_eq!(prop.wait, -1.0);
    match prop.motion {
        MapDoorMotion::Rotating {
            angle_delta,
            degrees,
            speed,
            open_direction,
        } => {
            assert_eq!(angle_delta, [0.0, 90.0, 0.0]);
            assert_eq!(degrees, 90.0);
            assert_eq!(speed, 100.0);
            assert_eq!(open_direction, MapDoorOpenDirection::Forward);
        }
        other @ MapDoorMotion::Linear { .. } => {
            panic!("expected prop_door_rotating, got {other:?}")
        }
    }
}

#[test]
fn linear_door_distance_uses_bmodel_bounds_direction_and_lip() {
    let bytes = door_bmodel_bsp_fixture("");
    let bsp = read_fixture_bsp(&bytes);
    let model = &bsp.models[1];

    assert!((linear_door_distance(model, [0.0, 1.0, 0.0], 4.0) - 34.0).abs() < 1.0e-4);
    assert!((linear_door_distance(model, [1.0, 0.0, 0.0], 1.0) - 13.0).abs() < 1.0e-4);
    assert_eq!(linear_door_distance(model, [0.0, 0.0, 1.0], 100.0), 0.0);
}

#[test]
fn door_extraction_removes_bmodel_faces_without_shifting_downstream_ranges() {
    let map = load_map(&door_bmodel_bsp_fixture(
        r#"{ "classname" "func_door" "model" "*1" "origin" "32 0 0" }"#,
    ))
    .expect("fixture bsp should load");

    assert_eq!(map.doors.len(), 1);
    let world = map.meshes.first().expect("static mesh");
    assert_eq!(world.indices.len(), 6);
    assert_eq!(
        world.visibility.always_visible,
        vec![
            MapMeshIndexRange {
                face: 0,
                start: 0,
                count: 3,
            },
            MapMeshIndexRange {
                face: 2,
                start: 3,
                count: 3,
            },
        ]
    );

    let MapDoorGeometry::Brush {
        model_index,
        meshes,
    } = &map.doors[0].geometry
    else {
        panic!("expected brush door");
    };
    assert_eq!(*model_index, 1);
    assert_eq!(
        meshes.iter().map(|mesh| mesh.indices.len()).sum::<usize>(),
        3
    );
    assert_eq!(
        meshes[0].visibility.always_visible,
        vec![MapMeshIndexRange {
            face: 1,
            start: 0,
            count: 3,
        }]
    );
}

#[test]
fn no_door_fixture_keeps_all_bmodel_faces_in_static_world_mesh() {
    let map = load_map(&door_bmodel_bsp_fixture(""))
        .expect("fixture bsp without door entities should load");

    assert!(map.doors.is_empty());
    let world = map.meshes.first().expect("static mesh");
    assert_eq!(world.indices.len(), 9);
    assert_eq!(
        world
            .visibility
            .always_visible
            .iter()
            .map(|range| (range.face, range.start, range.count))
            .collect::<Vec<_>>(),
        vec![(0, 0, 3), (1, 3, 3), (2, 6, 3)]
    );
}

#[test]
fn load_map_partitions_disconnected_sky_camera_cluster() {
    let bytes = skybox_partition_bsp_fixture(true, true);
    let map = load_map(&bytes).expect("fixture bsp should load");

    assert!(map.skybox_partition.sky_camera_present);
    assert_eq!(map.skybox_partition.face_count, 1);
    assert_eq!(map.skybox_partition.static_prop_count, 1);
    assert_eq!(
        map.meshes
            .iter()
            .map(|mesh| mesh.indices.len() / 3)
            .sum::<usize>(),
        1
    );
    assert_eq!(
        map.skybox_meshes
            .iter()
            .map(|mesh| mesh.indices.len() / 3)
            .sum::<usize>(),
        1
    );
    assert!(map.static_props.is_empty());
    assert_eq!(map.skybox_static_props.len(), 1);

    let unpartitioned =
        load_map_with_skybox_partition(&bytes, false).expect("fixture bsp should load");
    assert_eq!(merge_skybox_collections(map), unpartitioned);
}

#[test]
fn load_map_partitions_entity_props_by_containing_leaf() {
    let bytes = skybox_partition_bsp_fixture_with_extra_entities(
        true,
        true,
        r#"
        { "classname" "prop_dynamic" "model" "models/entity/world.mdl" "origin" "-32 0 0" }
        { "classname" "prop_physics" "model" "models/entity/skybox.mdl" "origin" "32 0 0" }
        "#,
    );
    let map = load_map(&bytes).expect("fixture bsp should load");

    assert_eq!(map.stats.static_prop_count, 1);
    assert_eq!(map.stats.entity_prop_count, 2);
    assert!(
        map.static_props
            .iter()
            .any(|prop| prop.model_path == "models/entity/world.mdl"
                && prop.visibility == MapPropVisibility::Clusters(vec![0]))
    );
    assert!(
        map.skybox_static_props
            .iter()
            .any(|prop| prop.model_path == "models/entity/skybox.mdl"
                && prop.visibility == MapPropVisibility::Clusters(vec![1]))
    );
    assert_eq!(map.skybox_partition.static_prop_count, 2);
}

#[test]
fn load_map_reattributes_world_faces_inside_skybox_completion_aabb() {
    let map = load_map(&skybox_completion_bsp_fixture([-32.0, 0.0, 0.0]))
        .expect("fixture bsp should load");

    assert_eq!(map.skybox_partition.completion_reattributed_face_count, 1);
    assert_eq!(map.skybox_partition.face_count, 2);
    assert!(map.skybox_completion_bounds.is_some());
    assert_eq!(
        map.meshes
            .iter()
            .map(|mesh| mesh.indices.len() / 3)
            .sum::<usize>(),
        1
    );
    assert_eq!(
        map.skybox_meshes
            .iter()
            .map(|mesh| mesh.indices.len() / 3)
            .sum::<usize>(),
        2
    );
}

#[test]
fn load_map_skybox_completion_spawn_overlap_guard_keeps_world_faces() {
    let map = load_map(&skybox_completion_bsp_fixture([-32.0, 256.0, 0.0]))
        .expect("fixture bsp should load");

    assert_eq!(map.skybox_partition.completion_reattributed_face_count, 0);
    assert_eq!(map.skybox_partition.face_count, 1);
    assert_eq!(map.skybox_completion_bounds, None);
    assert_eq!(
        map.meshes
            .iter()
            .map(|mesh| mesh.indices.len() / 3)
            .sum::<usize>(),
        2
    );
    assert_eq!(
        map.skybox_meshes
            .iter()
            .map(|mesh| mesh.indices.len() / 3)
            .sum::<usize>(),
        1
    );
}

#[test]
fn load_map_without_sky_camera_keeps_output_unpartitioned() {
    let bytes = skybox_partition_bsp_fixture(false, true);
    let map = load_map(&bytes).expect("fixture bsp should load");
    let unpartitioned =
        load_map_with_skybox_partition(&bytes, false).expect("fixture bsp should load");

    assert_eq!(map.skybox_partition, MapSkyboxPartitionStats::default());
    assert_eq!(map, unpartitioned);
}

#[test]
fn load_map_exposes_visibility_rows_and_world_cluster_ranges() {
    let bytes = skybox_partition_bsp_fixture(false, true);
    let map = load_map(&bytes).expect("fixture bsp should load");
    let visibility = map.visibility.as_ref().expect("fixture vis data");

    assert_eq!(map.stats.cluster_count, 2);
    assert_eq!(visibility.cluster_count(), 2);
    assert_eq!(visibility.cluster_at([-32.0, 0.0, 0.0]), Some(0));
    assert_eq!(visibility.cluster_at([32.0, 0.0, 0.0]), Some(1));
    assert_eq!(
        visibility.visible_clusters(0).expect("cluster 0 row"),
        vec![true, false]
    );
    assert_eq!(
        visibility.visible_clusters(1).expect("cluster 1 row"),
        vec![false, true]
    );

    let world = map
        .meshes
        .iter()
        .find(|mesh| mesh.material_index == 0)
        .expect("world material mesh");
    assert!(world.visibility.always_visible.is_empty());
    assert_eq!(world.visibility.clusters.len(), 1);
    assert_eq!(world.visibility.clusters[0].cluster, 0);
    assert_eq!(world.visibility.clusters[0].ranges[0].count, 3);

    let mini = map
        .meshes
        .iter()
        .find(|mesh| mesh.material_index == 1)
        .expect("mini material mesh");
    assert!(mini.visibility.always_visible.is_empty());
    assert_eq!(mini.visibility.clusters.len(), 1);
    assert_eq!(mini.visibility.clusters[0].cluster, 1);
    assert_eq!(mini.visibility.clusters[0].ranges[0].count, 3);
}

#[test]
fn visibility_aabb_walk_collects_clusters_on_both_sides_of_split() {
    let bytes = skybox_partition_bsp_fixture(false, true);
    let map = load_map(&bytes).expect("fixture bsp should load");
    let visibility = map.visibility.as_ref().expect("fixture vis data");

    assert_eq!(
        visibility.clusters_for_aabb([-48.0, -8.0, -8.0], [-16.0, 8.0, 8.0]),
        MapPropVisibility::Clusters(vec![0])
    );
    assert_eq!(
        visibility.clusters_for_aabb([-8.0, -8.0, -8.0], [8.0, 8.0, 8.0]),
        MapPropVisibility::Clusters(vec![0, 1])
    );
    assert_eq!(
        visibility.clusters_for_aabb([0.0; 3], [0.0; 3]),
        MapPropVisibility::Always
    );
}

#[test]
fn load_map_with_missing_vis_keeps_output_unpartitioned() {
    let bytes = skybox_partition_bsp_fixture(true, false);
    let map = load_map(&bytes).expect("fixture bsp should load");
    let unpartitioned =
        load_map_with_skybox_partition(&bytes, false).expect("fixture bsp should load");

    assert!(map.skybox_partition.sky_camera_present);
    assert_eq!(map.skybox_partition.face_count, 0);
    assert_eq!(map.skybox_partition.static_prop_count, 0);
    assert_eq!(map.stats.cluster_count, 0);
    assert!(map.visibility.is_none());
    assert_eq!(map, unpartitioned);
}

#[test]
fn load_map_skips_down_facing_warp_faces_only() {
    let map = load_map(&water_underside_bsp_fixture()).expect("fixture bsp should load");

    assert_eq!(map.stats.face_count, 3);
    assert_eq!(
        map.material_names,
        vec!["water/up".to_owned(), "brick/down".to_owned()]
    );
    assert_eq!(
        map.meshes
            .iter()
            .map(|mesh| mesh.indices.len() / 3)
            .sum::<usize>(),
        2
    );

    let water = &map.meshes[0];
    assert_eq!(water.material_index, 0);
    assert!(water.vertices.iter().all(|vertex| vertex.normal[2] > 0.0));

    let brick = &map.meshes[1];
    assert_eq!(brick.material_index, 1);
    assert!(brick.vertices.iter().all(|vertex| vertex.normal[2] < 0.0));
}

#[test]
fn unsupported_versions_are_reported_before_decode() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"VBSP");
    bytes.extend_from_slice(&21_u32.to_le_bytes());

    assert_eq!(
        load_map(&bytes),
        Err(BspError::UnsupportedVersion { version: 21 })
    );
}

fn block(width: usize, height: usize) -> PendingLightmapBlock {
    PendingLightmapBlock {
        rgba: vec![255; width * height * 4],
        width,
        height,
    }
}

fn box_collision(min: [f32; 3], max: [f32; 3]) -> MapWalkCollision {
    MapWalkCollision::solid_box_for_tests(min, max)
}

fn empty_walk_collision() -> MapWalkCollision {
    MapWalkCollision {
        brushes: Vec::new(),
        water_brushes: Vec::new(),
        displacements: Vec::new(),
        props: MapWalkPropCollision::default(),
    }
}

fn box_ledge(min: [f32; 3], max: [f32; 3]) -> ConvexHull {
    ConvexHull {
        vertices: vec![
            [min[0], min[1], min[2]],
            [max[0], min[1], min[2]],
            [min[0], max[1], min[2]],
            [max[0], max[1], min[2]],
            [min[0], min[1], max[2]],
            [max[0], min[1], max[2]],
            [min[0], max[1], max[2]],
            [max[0], max[1], max[2]],
        ],
        triangles: vec![
            [0, 2, 1],
            [1, 2, 3],
            [4, 5, 6],
            [5, 7, 6],
            [0, 1, 4],
            [1, 5, 4],
            [2, 6, 3],
            [3, 6, 7],
            [0, 4, 2],
            [2, 4, 6],
            [1, 3, 5],
            [3, 7, 5],
        ],
    }
}

fn wedge_ledge() -> ConvexHull {
    ConvexHull {
        vertices: vec![
            [0.0, 0.0, 0.0],
            [64.0, 0.0, 0.0],
            [0.0, 64.0, 0.0],
            [0.0, 0.0, 64.0],
            [64.0, 0.0, 64.0],
            [0.0, 64.0, 64.0],
        ],
        triangles: vec![
            [0, 2, 1],
            [3, 4, 5],
            [0, 1, 3],
            [1, 4, 3],
            [0, 3, 2],
            [2, 3, 5],
            [1, 2, 4],
            [2, 5, 4],
        ],
    }
}

fn box_planes(min: [f32; 3], max: [f32; 3]) -> Vec<MapPlane> {
    vec![
        MapPlane {
            normal: [1.0, 0.0, 0.0],
            dist: max[0],
        },
        MapPlane {
            normal: [-1.0, 0.0, 0.0],
            dist: -min[0],
        },
        MapPlane {
            normal: [0.0, 1.0, 0.0],
            dist: max[1],
        },
        MapPlane {
            normal: [0.0, -1.0, 0.0],
            dist: -min[1],
        },
        MapPlane {
            normal: [0.0, 0.0, 1.0],
            dist: max[2],
        },
        MapPlane {
            normal: [0.0, 0.0, -1.0],
            dist: -min[2],
        },
    ]
}

fn box_brush_with_sky_side(min: [f32; 3], max: [f32; 3], sky_side_index: usize) -> MapWalkBrush {
    let planes = box_planes(min, max)
        .into_iter()
        .enumerate()
        .map(|(index, plane)| MapWalkBrushPlane {
            plane,
            is_sky: index == sky_side_index,
        })
        .collect::<Vec<_>>();
    walk_brush_from_brush_planes(planes, contents_flags::SOLID).expect("box brush should be valid")
}

fn assert_vec3_close(actual: [f32; 3], expected: [f32; 3]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!(
            (actual - expected).abs() < 1.0e-4,
            "actual {actual} expected {expected}"
        );
    }
}

fn static_prop_bsp_fixture() -> Vec<u8> {
    bsp_fixture(None)
}

fn skybox_partition_bsp_fixture(include_sky_camera: bool, include_vis: bool) -> Vec<u8> {
    skybox_partition_bsp_fixture_with_extra_entities(include_sky_camera, include_vis, "")
}

fn skybox_partition_bsp_fixture_with_extra_entities(
    include_sky_camera: bool,
    include_vis: bool,
    extra_entities: &str,
) -> Vec<u8> {
    const HEADER_LEN: usize = 8;
    const LUMP_COUNT: usize = 64;
    const LUMP_ENTRY_LEN: usize = 16;
    const ENTITIES_LUMP_INDEX: usize = 0;
    const PLANES_LUMP_INDEX: usize = 1;
    const TEXTURE_DATA_LUMP_INDEX: usize = 2;
    const VERTICES_LUMP_INDEX: usize = 3;
    const VISIBILITY_LUMP_INDEX: usize = 4;
    const NODES_LUMP_INDEX: usize = 5;
    const TEXTURE_INFO_LUMP_INDEX: usize = 6;
    const FACES_LUMP_INDEX: usize = 7;
    const LEAVES_LUMP_INDEX: usize = 10;
    const EDGES_LUMP_INDEX: usize = 12;
    const SURFACE_EDGES_LUMP_INDEX: usize = 13;
    const MODELS_LUMP_INDEX: usize = 14;
    const LEAF_FACES_LUMP_INDEX: usize = 16;
    const GAME_LUMP_INDEX: usize = 35;
    const PAKFILE_LUMP_INDEX: usize = 40;
    const TEXTURE_STRING_DATA_LUMP_INDEX: usize = 43;
    const TEXTURE_STRING_TABLE_LUMP_INDEX: usize = 44;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"VBSP");
    bytes.extend_from_slice(&20_u32.to_le_bytes());
    bytes.resize(HEADER_LEN + LUMP_COUNT * LUMP_ENTRY_LEN, 0);

    let sky_camera_entity = if include_sky_camera {
        r#"{ "classname" "sky_camera" "origin" "32 0 0" }"#
    } else {
        ""
    };
    let entities = format!(
        r#"
        {{ "classname" "worldspawn" }}
        {{ "classname" "info_player_start" "origin" "-32 0 0" }}
        {sky_camera_entity}
        {extra_entities}
        "#
    );
    append_lump(&mut bytes, ENTITIES_LUMP_INDEX, entities.as_bytes(), 0);

    let mut planes = Vec::new();
    push_plane(&mut planes, [1.0, 0.0, 0.0]);
    push_plane(&mut planes, [0.0, 0.0, 1.0]);
    append_lump(&mut bytes, PLANES_LUMP_INDEX, &planes, 0);

    let mut texture_data = Vec::new();
    push_texture_data(&mut texture_data, 0);
    push_texture_data(&mut texture_data, 1);
    append_lump(&mut bytes, TEXTURE_DATA_LUMP_INDEX, &texture_data, 0);

    let mut vertices = Vec::new();
    for position in [
        [-64.0, -32.0, 0.0],
        [-16.0, -32.0, 0.0],
        [-64.0, 32.0, 0.0],
        [16.0, -32.0, 0.0],
        [64.0, -32.0, 0.0],
        [16.0, 32.0, 0.0],
    ] {
        push_vec3(&mut vertices, position);
    }
    append_lump(&mut bytes, VERTICES_LUMP_INDEX, &vertices, 0);

    if include_vis {
        append_lump(&mut bytes, VISIBILITY_LUMP_INDEX, &partition_vis_lump(), 0);
    }

    let mut node = Vec::new();
    node.extend_from_slice(&0_i32.to_le_bytes());
    node.extend_from_slice(&(-2_i32).to_le_bytes());
    node.extend_from_slice(&(-1_i32).to_le_bytes());
    node.extend_from_slice(&[0_u8; 20]);
    append_lump(&mut bytes, NODES_LUMP_INDEX, &node, 0);

    let mut texture_info = Vec::new();
    push_texture_info(&mut texture_info, 0, 0);
    push_texture_info(&mut texture_info, 0, 1);
    append_lump(&mut bytes, TEXTURE_INFO_LUMP_INDEX, &texture_info, 0);

    let mut faces = Vec::new();
    push_face(&mut faces, 1, 0, 0, 0);
    push_face(&mut faces, 1, 3, 1, 1);
    append_lump(&mut bytes, FACES_LUMP_INDEX, &faces, 0);

    let mut leaves = Vec::new();
    push_partition_leaf(&mut leaves, 0, 0, 1);
    push_partition_leaf(&mut leaves, 1, 1, 1);
    append_lump(&mut bytes, LEAVES_LUMP_INDEX, &leaves, 0);

    let mut edges = Vec::new();
    for base in [0_u16, 3] {
        push_edge(&mut edges, base, base + 1);
        push_edge(&mut edges, base + 1, base + 2);
        push_edge(&mut edges, base + 2, base);
    }
    append_lump(&mut bytes, EDGES_LUMP_INDEX, &edges, 0);

    let mut surface_edges = Vec::new();
    for edge in 0_i32..6 {
        surface_edges.extend_from_slice(&edge.to_le_bytes());
    }
    append_lump(&mut bytes, SURFACE_EDGES_LUMP_INDEX, &surface_edges, 0);

    let mut models = Vec::new();
    push_vec3(&mut models, [-64.0, -32.0, 0.0]);
    push_vec3(&mut models, [64.0, 32.0, 0.0]);
    push_vec3(&mut models, [0.0, 0.0, 0.0]);
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&2_i32.to_le_bytes());
    append_lump(&mut bytes, MODELS_LUMP_INDEX, &models, 0);

    let mut leaf_faces = Vec::new();
    leaf_faces.extend_from_slice(&0_u16.to_le_bytes());
    leaf_faces.extend_from_slice(&1_u16.to_le_bytes());
    append_lump(&mut bytes, LEAF_FACES_LUMP_INDEX, &leaf_faces, 0);

    let prop_data = static_prop_game_lump_data();
    let game_lump_offset = bytes.len();
    let prop_offset = game_lump_offset + 20;
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    bytes.extend_from_slice(&i32::from_be_bytes(*b"sprp").to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&10_u16.to_le_bytes());
    bytes.extend_from_slice(&(prop_offset as i32).to_le_bytes());
    bytes.extend_from_slice(&(prop_data.len() as i32).to_le_bytes());
    bytes.extend_from_slice(&prop_data);
    write_lump_entry(&mut bytes, GAME_LUMP_INDEX, game_lump_offset, 20, 0);

    let pakfile_offset = bytes.len();
    let empty_zip = empty_zip_bytes();
    bytes.extend_from_slice(&empty_zip);
    write_lump_entry(
        &mut bytes,
        PAKFILE_LUMP_INDEX,
        pakfile_offset,
        empty_zip.len(),
        0,
    );

    let texture_names = b"brick/world\0brick/mini\0";
    append_lump(&mut bytes, TEXTURE_STRING_DATA_LUMP_INDEX, texture_names, 0);

    let mut texture_string_table = Vec::new();
    texture_string_table.extend_from_slice(&0_i32.to_le_bytes());
    texture_string_table.extend_from_slice(&12_i32.to_le_bytes());
    append_lump(
        &mut bytes,
        TEXTURE_STRING_TABLE_LUMP_INDEX,
        &texture_string_table,
        0,
    );

    bytes
}

fn in_solid_static_prop_bsp_fixture() -> Vec<u8> {
    const HEADER_LEN: usize = 8;
    const LUMP_COUNT: usize = 64;
    const LUMP_ENTRY_LEN: usize = 16;
    const ENTITIES_LUMP_INDEX: usize = 0;
    const PLANES_LUMP_INDEX: usize = 1;
    const TEXTURE_DATA_LUMP_INDEX: usize = 2;
    const VERTICES_LUMP_INDEX: usize = 3;
    const VISIBILITY_LUMP_INDEX: usize = 4;
    const NODES_LUMP_INDEX: usize = 5;
    const TEXTURE_INFO_LUMP_INDEX: usize = 6;
    const FACES_LUMP_INDEX: usize = 7;
    const LEAVES_LUMP_INDEX: usize = 10;
    const EDGES_LUMP_INDEX: usize = 12;
    const SURFACE_EDGES_LUMP_INDEX: usize = 13;
    const MODELS_LUMP_INDEX: usize = 14;
    const LEAF_FACES_LUMP_INDEX: usize = 16;
    const GAME_LUMP_INDEX: usize = 35;
    const PAKFILE_LUMP_INDEX: usize = 40;
    const TEXTURE_STRING_DATA_LUMP_INDEX: usize = 43;
    const TEXTURE_STRING_TABLE_LUMP_INDEX: usize = 44;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"VBSP");
    bytes.extend_from_slice(&20_u32.to_le_bytes());
    bytes.resize(HEADER_LEN + LUMP_COUNT * LUMP_ENTRY_LEN, 0);

    append_lump(
        &mut bytes,
        ENTITIES_LUMP_INDEX,
        br#"{ "classname" "worldspawn" }"#,
        0,
    );

    let mut planes = Vec::new();
    push_plane(&mut planes, [1.0, 0.0, 0.0]);
    push_plane(&mut planes, [0.0, 0.0, 1.0]);
    append_lump(&mut bytes, PLANES_LUMP_INDEX, &planes, 0);

    let mut texture_data = Vec::new();
    push_texture_data(&mut texture_data, 0);
    push_texture_data(&mut texture_data, 1);
    append_lump(&mut bytes, TEXTURE_DATA_LUMP_INDEX, &texture_data, 0);
    append_lump(&mut bytes, VISIBILITY_LUMP_INDEX, &partition_vis_lump(), 0);

    let mut vertices = Vec::new();
    for position in [
        [-64.0, -32.0, 0.0],
        [-16.0, -32.0, 0.0],
        [-64.0, 32.0, 0.0],
        [16.0, -32.0, 0.0],
        [64.0, -32.0, 0.0],
        [16.0, 32.0, 0.0],
    ] {
        push_vec3(&mut vertices, position);
    }
    append_lump(&mut bytes, VERTICES_LUMP_INDEX, &vertices, 0);

    let mut node = Vec::new();
    node.extend_from_slice(&0_i32.to_le_bytes());
    node.extend_from_slice(&(-1_i32).to_le_bytes());
    node.extend_from_slice(&(-2_i32).to_le_bytes());
    node.extend_from_slice(&[0_u8; 20]);
    append_lump(&mut bytes, NODES_LUMP_INDEX, &node, 0);

    let mut texture_info = Vec::new();
    push_texture_info(&mut texture_info, 0, 0);
    push_texture_info(&mut texture_info, 0, 1);
    append_lump(&mut bytes, TEXTURE_INFO_LUMP_INDEX, &texture_info, 0);

    let mut faces = Vec::new();
    push_face(&mut faces, 1, 0, 0, 0);
    push_face(&mut faces, 1, 3, 1, 1);
    append_lump(&mut bytes, FACES_LUMP_INDEX, &faces, 0);

    let mut leaves = Vec::new();
    push_partition_leaf(&mut leaves, -1, 0, 0);
    push_partition_leaf(&mut leaves, 0, 0, 1);
    push_partition_leaf(&mut leaves, 1, 1, 1);
    append_lump(&mut bytes, LEAVES_LUMP_INDEX, &leaves, 0);

    let mut edges = Vec::new();
    for base in [0_u16, 3] {
        push_edge(&mut edges, base, base + 1);
        push_edge(&mut edges, base + 1, base + 2);
        push_edge(&mut edges, base + 2, base);
    }
    append_lump(&mut bytes, EDGES_LUMP_INDEX, &edges, 0);

    let mut surface_edges = Vec::new();
    for edge in 0_i32..6 {
        surface_edges.extend_from_slice(&edge.to_le_bytes());
    }
    append_lump(&mut bytes, SURFACE_EDGES_LUMP_INDEX, &surface_edges, 0);

    let mut models = Vec::new();
    push_vec3(&mut models, [-64.0, -32.0, 0.0]);
    push_vec3(&mut models, [64.0, 32.0, 0.0]);
    push_vec3(&mut models, [0.0, 0.0, 0.0]);
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&2_i32.to_le_bytes());
    append_lump(&mut bytes, MODELS_LUMP_INDEX, &models, 0);

    let mut leaf_faces = Vec::new();
    leaf_faces.extend_from_slice(&0_u16.to_le_bytes());
    leaf_faces.extend_from_slice(&1_u16.to_le_bytes());
    append_lump(&mut bytes, LEAF_FACES_LUMP_INDEX, &leaf_faces, 0);

    let prop_data = static_prop_game_lump_data_with([10.0, 20.0, 30.0], &[1, 2]);
    let game_lump_offset = bytes.len();
    let prop_offset = game_lump_offset + 20;
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    bytes.extend_from_slice(&i32::from_be_bytes(*b"sprp").to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&10_u16.to_le_bytes());
    bytes.extend_from_slice(&(prop_offset as i32).to_le_bytes());
    bytes.extend_from_slice(&(prop_data.len() as i32).to_le_bytes());
    bytes.extend_from_slice(&prop_data);
    write_lump_entry(&mut bytes, GAME_LUMP_INDEX, game_lump_offset, 20, 0);

    let pakfile_offset = bytes.len();
    let empty_zip = empty_zip_bytes();
    bytes.extend_from_slice(&empty_zip);
    write_lump_entry(
        &mut bytes,
        PAKFILE_LUMP_INDEX,
        pakfile_offset,
        empty_zip.len(),
        0,
    );

    let texture_names = b"brick/world\0brick/mini\0";
    append_lump(&mut bytes, TEXTURE_STRING_DATA_LUMP_INDEX, texture_names, 0);

    let mut texture_string_table = Vec::new();
    texture_string_table.extend_from_slice(&0_i32.to_le_bytes());
    texture_string_table.extend_from_slice(&12_i32.to_le_bytes());
    append_lump(
        &mut bytes,
        TEXTURE_STRING_TABLE_LUMP_INDEX,
        &texture_string_table,
        0,
    );

    bytes
}

fn skybox_completion_bsp_fixture(spawn_origin: [f32; 3]) -> Vec<u8> {
    const HEADER_LEN: usize = 8;
    const LUMP_COUNT: usize = 64;
    const LUMP_ENTRY_LEN: usize = 16;
    const ENTITIES_LUMP_INDEX: usize = 0;
    const PLANES_LUMP_INDEX: usize = 1;
    const TEXTURE_DATA_LUMP_INDEX: usize = 2;
    const VERTICES_LUMP_INDEX: usize = 3;
    const VISIBILITY_LUMP_INDEX: usize = 4;
    const NODES_LUMP_INDEX: usize = 5;
    const TEXTURE_INFO_LUMP_INDEX: usize = 6;
    const FACES_LUMP_INDEX: usize = 7;
    const LEAVES_LUMP_INDEX: usize = 10;
    const EDGES_LUMP_INDEX: usize = 12;
    const SURFACE_EDGES_LUMP_INDEX: usize = 13;
    const MODELS_LUMP_INDEX: usize = 14;
    const LEAF_FACES_LUMP_INDEX: usize = 16;
    const GAME_LUMP_INDEX: usize = 35;
    const PAKFILE_LUMP_INDEX: usize = 40;
    const TEXTURE_STRING_DATA_LUMP_INDEX: usize = 43;
    const TEXTURE_STRING_TABLE_LUMP_INDEX: usize = 44;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"VBSP");
    bytes.extend_from_slice(&20_u32.to_le_bytes());
    bytes.resize(HEADER_LEN + LUMP_COUNT * LUMP_ENTRY_LEN, 0);

    let entities = format!(
        r#"
        {{ "classname" "worldspawn" }}
        {{ "classname" "info_player_start" "origin" "{} {} {}" }}
        {{ "classname" "sky_camera" "origin" "32 0 0" }}
        "#,
        spawn_origin[0], spawn_origin[1], spawn_origin[2]
    );
    append_lump(&mut bytes, ENTITIES_LUMP_INDEX, entities.as_bytes(), 0);

    let mut planes = Vec::new();
    push_plane(&mut planes, [1.0, 0.0, 0.0]);
    push_plane(&mut planes, [0.0, 0.0, 1.0]);
    append_lump(&mut bytes, PLANES_LUMP_INDEX, &planes, 0);

    let mut texture_data = Vec::new();
    for index in 0..3 {
        push_texture_data(&mut texture_data, index);
    }
    append_lump(&mut bytes, TEXTURE_DATA_LUMP_INDEX, &texture_data, 0);

    let mut vertices = Vec::new();
    for position in [
        [-144.0, -32.0, 0.0],
        [-96.0, -32.0, 0.0],
        [-144.0, 32.0, 0.0],
        [-48.0, 240.0, 0.0],
        [-16.0, 240.0, 0.0],
        [-48.0, 280.0, 0.0],
        [-44.0, 248.0, 0.0],
        [-20.0, 248.0, 0.0],
        [-44.0, 272.0, 0.0],
    ] {
        push_vec3(&mut vertices, position);
    }
    append_lump(&mut bytes, VERTICES_LUMP_INDEX, &vertices, 0);
    append_lump(&mut bytes, VISIBILITY_LUMP_INDEX, &partition_vis_lump(), 0);

    let mut node = Vec::new();
    node.extend_from_slice(&0_i32.to_le_bytes());
    node.extend_from_slice(&(-2_i32).to_le_bytes());
    node.extend_from_slice(&(-1_i32).to_le_bytes());
    node.extend_from_slice(&[0_u8; 20]);
    append_lump(&mut bytes, NODES_LUMP_INDEX, &node, 0);

    let mut texture_info = Vec::new();
    for texture_index in 0..3 {
        push_texture_info(&mut texture_info, 0, texture_index);
    }
    append_lump(&mut bytes, TEXTURE_INFO_LUMP_INDEX, &texture_info, 0);

    let mut faces = Vec::new();
    push_face(&mut faces, 1, 0, 0, 0);
    push_face(&mut faces, 1, 3, 1, 1);
    push_face(&mut faces, 1, 6, 2, 2);
    append_lump(&mut bytes, FACES_LUMP_INDEX, &faces, 0);

    let mut leaves = Vec::new();
    push_partition_leaf(&mut leaves, 0, 0, 2);
    push_partition_leaf(&mut leaves, 1, 2, 1);
    append_lump(&mut bytes, LEAVES_LUMP_INDEX, &leaves, 0);

    let mut edges = Vec::new();
    for base in [0_u16, 3, 6] {
        push_edge(&mut edges, base, base + 1);
        push_edge(&mut edges, base + 1, base + 2);
        push_edge(&mut edges, base + 2, base);
    }
    append_lump(&mut bytes, EDGES_LUMP_INDEX, &edges, 0);

    let mut surface_edges = Vec::new();
    for edge in 0_i32..9 {
        surface_edges.extend_from_slice(&edge.to_le_bytes());
    }
    append_lump(&mut bytes, SURFACE_EDGES_LUMP_INDEX, &surface_edges, 0);

    let mut models = Vec::new();
    push_vec3(&mut models, [-160.0, -64.0, -128.0]);
    push_vec3(&mut models, [96.0, 384.0, 128.0]);
    push_vec3(&mut models, [0.0, 0.0, 0.0]);
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&3_i32.to_le_bytes());
    append_lump(&mut bytes, MODELS_LUMP_INDEX, &models, 0);

    let mut leaf_faces = Vec::new();
    for face in [0_u16, 2, 1] {
        leaf_faces.extend_from_slice(&face.to_le_bytes());
    }
    append_lump(&mut bytes, LEAF_FACES_LUMP_INDEX, &leaf_faces, 0);

    let prop_data = empty_static_prop_game_lump_data();
    let game_lump_offset = bytes.len();
    let prop_offset = game_lump_offset + 20;
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    bytes.extend_from_slice(&i32::from_be_bytes(*b"sprp").to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&10_u16.to_le_bytes());
    bytes.extend_from_slice(&(prop_offset as i32).to_le_bytes());
    bytes.extend_from_slice(&(prop_data.len() as i32).to_le_bytes());
    bytes.extend_from_slice(&prop_data);
    write_lump_entry(&mut bytes, GAME_LUMP_INDEX, game_lump_offset, 20, 0);

    let pakfile_offset = bytes.len();
    let empty_zip = empty_zip_bytes();
    bytes.extend_from_slice(&empty_zip);
    write_lump_entry(
        &mut bytes,
        PAKFILE_LUMP_INDEX,
        pakfile_offset,
        empty_zip.len(),
        0,
    );

    let texture_names = b"brick/world\0brick/mini\0brick/leak\0";
    append_lump(&mut bytes, TEXTURE_STRING_DATA_LUMP_INDEX, texture_names, 0);

    let mut texture_string_table = Vec::new();
    texture_string_table.extend_from_slice(&0_i32.to_le_bytes());
    texture_string_table.extend_from_slice(&12_i32.to_le_bytes());
    texture_string_table.extend_from_slice(&23_i32.to_le_bytes());
    append_lump(
        &mut bytes,
        TEXTURE_STRING_TABLE_LUMP_INDEX,
        &texture_string_table,
        0,
    );

    bytes
}

fn partition_vis_lump() -> Vec<u8> {
    let mut bytes = Vec::new();
    let cluster_count = 2_u32;
    let directory_bytes = 4 + cluster_count as usize * 8;
    bytes.extend_from_slice(&cluster_count.to_le_bytes());
    bytes.extend_from_slice(&(directory_bytes as i32).to_le_bytes());
    bytes.extend_from_slice(&(directory_bytes as i32).to_le_bytes());
    bytes.extend_from_slice(&((directory_bytes + 1) as i32).to_le_bytes());
    bytes.extend_from_slice(&((directory_bytes + 1) as i32).to_le_bytes());
    bytes.push(0b0000_0001);
    bytes.push(0b0000_0010);
    bytes
}

fn push_partition_leaf(bytes: &mut Vec<u8>, cluster: i16, first_leaf_face: u16, face_count: u16) {
    let mut leaf = [0_u8; 56];
    leaf[4..6].copy_from_slice(&cluster.to_le_bytes());
    leaf[20..22].copy_from_slice(&first_leaf_face.to_le_bytes());
    leaf[22..24].copy_from_slice(&face_count.to_le_bytes());
    bytes.extend_from_slice(&leaf);
}

fn merge_skybox_collections(mut map: MapData) -> MapData {
    map.meshes.append(&mut map.skybox_meshes);
    map.static_props.append(&mut map.skybox_static_props);
    map.detail_sprites.append(&mut map.skybox_detail_sprites);
    map.overlays.append(&mut map.skybox_overlays);
    map.bounds_min = bounds_from_meshes(&map.meshes).0;
    map.bounds_max = bounds_from_meshes(&map.meshes).1;
    map.stats.world_static_prop_count = map.stats.static_prop_count;
    map.stats.skybox_static_prop_count = 0;
    map.stats.world_entity_prop_count = map.stats.entity_prop_count;
    map.stats.skybox_entity_prop_count = 0;
    map.skybox_partition = MapSkyboxPartitionStats {
        sky_camera_present: map.skybox_partition.sky_camera_present,
        ..MapSkyboxPartitionStats::default()
    };
    map.skybox_completion_bounds = None;
    map
}

fn bsp_fixture_with_entities(entities: &str) -> Vec<u8> {
    bsp_fixture(Some(entities))
}

/// Parses raw entity-lump text through the real BSP pipeline, for
/// tests that exercise entity-parsing functions directly rather than
/// through `load_map`.
fn parse_test_entities(entities: &str) -> Vec<MapEntity> {
    read_fixture_bsp(&bsp_fixture_with_entities(entities)).entities
}

fn pending_doors_from_fixture(entities: &str) -> Vec<PendingMapDoor> {
    let bytes = door_bmodel_bsp_fixture(entities);
    let bsp = read_fixture_bsp(&bytes);
    pending_map_doors(&bsp)
}

fn read_fixture_bsp(bytes: &[u8]) -> MapBsp {
    MapBsp::parse(bytes, &Limits::default()).expect("fixture bsp should parse")
}

fn door_bmodel_bsp_fixture(extra_entities: &str) -> Vec<u8> {
    const HEADER_LEN: usize = 8;
    const LUMP_COUNT: usize = 64;
    const LUMP_ENTRY_LEN: usize = 16;
    const ENTITIES_LUMP_INDEX: usize = 0;
    const PLANES_LUMP_INDEX: usize = 1;
    const TEXTURE_DATA_LUMP_INDEX: usize = 2;
    const VERTICES_LUMP_INDEX: usize = 3;
    const NODES_LUMP_INDEX: usize = 5;
    const TEXTURE_INFO_LUMP_INDEX: usize = 6;
    const FACES_LUMP_INDEX: usize = 7;
    const LEAVES_LUMP_INDEX: usize = 10;
    const EDGES_LUMP_INDEX: usize = 12;
    const SURFACE_EDGES_LUMP_INDEX: usize = 13;
    const MODELS_LUMP_INDEX: usize = 14;
    const LEAF_FACES_LUMP_INDEX: usize = 16;
    const GAME_LUMP_INDEX: usize = 35;
    const PAKFILE_LUMP_INDEX: usize = 40;
    const TEXTURE_STRING_DATA_LUMP_INDEX: usize = 43;
    const TEXTURE_STRING_TABLE_LUMP_INDEX: usize = 44;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"VBSP");
    bytes.extend_from_slice(&20_u32.to_le_bytes());
    bytes.resize(HEADER_LEN + LUMP_COUNT * LUMP_ENTRY_LEN, 0);

    let entities = format!(
        r#"
        {{ "classname" "worldspawn" }}
        {extra_entities}
        "#
    );
    append_lump(&mut bytes, ENTITIES_LUMP_INDEX, entities.as_bytes(), 0);

    let mut planes = Vec::new();
    push_plane(&mut planes, [0.0, 0.0, 1.0]);
    append_lump(&mut bytes, PLANES_LUMP_INDEX, &planes, 0);

    let mut texture_data = Vec::new();
    push_texture_data(&mut texture_data, 0);
    append_lump(&mut bytes, TEXTURE_DATA_LUMP_INDEX, &texture_data, 0);

    let mut vertices = Vec::new();
    for position in [
        [0.0, 0.0, 0.0],
        [16.0, 0.0, 0.0],
        [0.0, 16.0, 0.0],
        [32.0, 0.0, 0.0],
        [48.0, 0.0, 0.0],
        [32.0, 16.0, 0.0],
        [64.0, 0.0, 0.0],
        [80.0, 0.0, 0.0],
        [64.0, 16.0, 0.0],
    ] {
        push_vec3(&mut vertices, position);
    }
    append_lump(&mut bytes, VERTICES_LUMP_INDEX, &vertices, 0);

    let mut node = Vec::new();
    node.extend_from_slice(&0_i32.to_le_bytes());
    node.extend_from_slice(&(-1_i32).to_le_bytes());
    node.extend_from_slice(&(-1_i32).to_le_bytes());
    node.extend_from_slice(&[0_u8; 20]);
    append_lump(&mut bytes, NODES_LUMP_INDEX, &node, 0);

    let mut texture_info = Vec::new();
    push_texture_info(&mut texture_info, 0, 0);
    append_lump(&mut bytes, TEXTURE_INFO_LUMP_INDEX, &texture_info, 0);

    let mut faces = Vec::new();
    push_face(&mut faces, 0, 0, 0, 0);
    push_face(&mut faces, 0, 3, 0, 1);
    push_face(&mut faces, 0, 6, 0, 2);
    append_lump(&mut bytes, FACES_LUMP_INDEX, &faces, 0);

    let mut leaf = [0_u8; 56];
    leaf[20..22].copy_from_slice(&0_u16.to_le_bytes());
    leaf[22..24].copy_from_slice(&3_u16.to_le_bytes());
    append_lump(&mut bytes, LEAVES_LUMP_INDEX, &leaf, 0);

    let mut edges = Vec::new();
    for base in [0_u16, 3, 6] {
        push_edge(&mut edges, base, base + 1);
        push_edge(&mut edges, base + 1, base + 2);
        push_edge(&mut edges, base + 2, base);
    }
    append_lump(&mut bytes, EDGES_LUMP_INDEX, &edges, 0);

    let mut surface_edges = Vec::new();
    for edge in 0_i32..9 {
        surface_edges.extend_from_slice(&edge.to_le_bytes());
    }
    append_lump(&mut bytes, SURFACE_EDGES_LUMP_INDEX, &surface_edges, 0);

    let mut models = Vec::new();
    push_model(
        &mut models,
        [0.0, 0.0, 0.0],
        [16.0, 16.0, 0.0],
        [0.0; 3],
        0,
        0,
        1,
    );
    push_model(
        &mut models,
        [32.0, 0.0, 0.0],
        [48.0, 40.0, 80.0],
        [32.0, 0.0, 0.0],
        0,
        1,
        1,
    );
    push_model(
        &mut models,
        [64.0, 0.0, 0.0],
        [80.0, 16.0, 0.0],
        [64.0, 0.0, 0.0],
        0,
        2,
        1,
    );
    append_lump(&mut bytes, MODELS_LUMP_INDEX, &models, 0);

    let mut leaf_faces = Vec::new();
    for face in [0_u16, 1, 2] {
        leaf_faces.extend_from_slice(&face.to_le_bytes());
    }
    append_lump(&mut bytes, LEAF_FACES_LUMP_INDEX, &leaf_faces, 0);

    let prop_data = empty_static_prop_game_lump_data();
    let game_lump_offset = bytes.len();
    let prop_offset = game_lump_offset + 20;
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    bytes.extend_from_slice(&i32::from_be_bytes(*b"sprp").to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&10_u16.to_le_bytes());
    bytes.extend_from_slice(&(prop_offset as i32).to_le_bytes());
    bytes.extend_from_slice(&(prop_data.len() as i32).to_le_bytes());
    bytes.extend_from_slice(&prop_data);
    write_lump_entry(&mut bytes, GAME_LUMP_INDEX, game_lump_offset, 20, 0);

    let pakfile_offset = bytes.len();
    let empty_zip = empty_zip_bytes();
    bytes.extend_from_slice(&empty_zip);
    write_lump_entry(
        &mut bytes,
        PAKFILE_LUMP_INDEX,
        pakfile_offset,
        empty_zip.len(),
        0,
    );

    append_lump(
        &mut bytes,
        TEXTURE_STRING_DATA_LUMP_INDEX,
        b"brick/door\0",
        0,
    );
    let mut texture_string_table = Vec::new();
    texture_string_table.extend_from_slice(&0_i32.to_le_bytes());
    append_lump(
        &mut bytes,
        TEXTURE_STRING_TABLE_LUMP_INDEX,
        &texture_string_table,
        0,
    );

    bytes
}

fn water_underside_bsp_fixture() -> Vec<u8> {
    const HEADER_LEN: usize = 8;
    const LUMP_COUNT: usize = 64;
    const LUMP_ENTRY_LEN: usize = 16;
    const PLANES_LUMP_INDEX: usize = 1;
    const TEXTURE_DATA_LUMP_INDEX: usize = 2;
    const VERTICES_LUMP_INDEX: usize = 3;
    const NODES_LUMP_INDEX: usize = 5;
    const TEXTURE_INFO_LUMP_INDEX: usize = 6;
    const FACES_LUMP_INDEX: usize = 7;
    const LEAVES_LUMP_INDEX: usize = 10;
    const EDGES_LUMP_INDEX: usize = 12;
    const SURFACE_EDGES_LUMP_INDEX: usize = 13;
    const MODELS_LUMP_INDEX: usize = 14;
    const GAME_LUMP_INDEX: usize = 35;
    const PAKFILE_LUMP_INDEX: usize = 40;
    const TEXTURE_STRING_DATA_LUMP_INDEX: usize = 43;
    const TEXTURE_STRING_TABLE_LUMP_INDEX: usize = 44;
    const SURF_WARP: u32 = 0x8;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"VBSP");
    bytes.extend_from_slice(&20_u32.to_le_bytes());
    bytes.resize(HEADER_LEN + LUMP_COUNT * LUMP_ENTRY_LEN, 0);

    let mut planes = Vec::new();
    push_plane(&mut planes, [0.0, 0.0, -1.0]);
    push_plane(&mut planes, [0.0, 0.0, 1.0]);
    append_lump(&mut bytes, PLANES_LUMP_INDEX, &planes, 0);

    let mut texture_data = Vec::new();
    for index in 0..3 {
        push_texture_data(&mut texture_data, index);
    }
    append_lump(&mut bytes, TEXTURE_DATA_LUMP_INDEX, &texture_data, 0);

    let mut vertices = Vec::new();
    for position in [
        [0.0, 0.0, 0.0],
        [64.0, 0.0, 0.0],
        [0.0, 64.0, 0.0],
        [0.0, 0.0, 16.0],
        [64.0, 0.0, 16.0],
        [0.0, 64.0, 16.0],
        [0.0, 0.0, 32.0],
        [64.0, 0.0, 32.0],
        [0.0, 64.0, 32.0],
    ] {
        push_vec3(&mut vertices, position);
    }
    append_lump(&mut bytes, VERTICES_LUMP_INDEX, &vertices, 0);

    let mut node = Vec::new();
    node.extend_from_slice(&0_i32.to_le_bytes());
    node.extend_from_slice(&(-1_i32).to_le_bytes());
    node.extend_from_slice(&(-1_i32).to_le_bytes());
    node.extend_from_slice(&[0_u8; 20]);
    append_lump(&mut bytes, NODES_LUMP_INDEX, &node, 0);

    let mut texture_info = Vec::new();
    push_texture_info(&mut texture_info, SURF_WARP, 0);
    push_texture_info(&mut texture_info, SURF_WARP, 1);
    push_texture_info(&mut texture_info, 0, 2);
    append_lump(&mut bytes, TEXTURE_INFO_LUMP_INDEX, &texture_info, 0);

    let mut faces = Vec::new();
    push_face(&mut faces, 0, 0, 0, 0);
    push_face(&mut faces, 1, 3, 1, 1);
    push_face(&mut faces, 0, 6, 2, 2);
    append_lump(&mut bytes, FACES_LUMP_INDEX, &faces, 0);

    append_lump(&mut bytes, LEAVES_LUMP_INDEX, &[0_u8; 56], 0);

    let mut edges = Vec::new();
    for base in [0_u16, 3, 6] {
        push_edge(&mut edges, base, base + 1);
        push_edge(&mut edges, base + 1, base + 2);
        push_edge(&mut edges, base + 2, base);
    }
    append_lump(&mut bytes, EDGES_LUMP_INDEX, &edges, 0);

    let mut surface_edges = Vec::new();
    for edge in 0_i32..9 {
        surface_edges.extend_from_slice(&edge.to_le_bytes());
    }
    append_lump(&mut bytes, SURFACE_EDGES_LUMP_INDEX, &surface_edges, 0);

    let mut models = Vec::new();
    push_vec3(&mut models, [0.0, 0.0, 0.0]);
    push_vec3(&mut models, [64.0, 64.0, 32.0]);
    push_vec3(&mut models, [0.0, 0.0, 0.0]);
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&3_i32.to_le_bytes());
    append_lump(&mut bytes, MODELS_LUMP_INDEX, &models, 0);

    let prop_data = empty_static_prop_game_lump_data();
    let game_lump_offset = bytes.len();
    let prop_offset = game_lump_offset + 20;
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    bytes.extend_from_slice(&i32::from_be_bytes(*b"sprp").to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&10_u16.to_le_bytes());
    bytes.extend_from_slice(&(prop_offset as i32).to_le_bytes());
    bytes.extend_from_slice(&(prop_data.len() as i32).to_le_bytes());
    bytes.extend_from_slice(&prop_data);
    write_lump_entry(&mut bytes, GAME_LUMP_INDEX, game_lump_offset, 20, 0);

    let pakfile_offset = bytes.len();
    let empty_zip = empty_zip_bytes();
    bytes.extend_from_slice(&empty_zip);
    write_lump_entry(
        &mut bytes,
        PAKFILE_LUMP_INDEX,
        pakfile_offset,
        empty_zip.len(),
        0,
    );

    let texture_names = b"water/down\0water/up\0brick/down\0";
    append_lump(&mut bytes, TEXTURE_STRING_DATA_LUMP_INDEX, texture_names, 0);

    let mut texture_string_table = Vec::new();
    texture_string_table.extend_from_slice(&0_i32.to_le_bytes());
    texture_string_table.extend_from_slice(&11_i32.to_le_bytes());
    texture_string_table.extend_from_slice(&20_i32.to_le_bytes());
    append_lump(
        &mut bytes,
        TEXTURE_STRING_TABLE_LUMP_INDEX,
        &texture_string_table,
        0,
    );

    bytes
}

fn bsp_fixture(entities: Option<&str>) -> Vec<u8> {
    const HEADER_LEN: usize = 8;
    const LUMP_COUNT: usize = 64;
    const LUMP_ENTRY_LEN: usize = 16;
    const ENTITIES_LUMP_INDEX: usize = 0;
    const PLANES_LUMP_INDEX: usize = 1;
    const NODES_LUMP_INDEX: usize = 5;
    const LEAVES_LUMP_INDEX: usize = 10;
    const GAME_LUMP_INDEX: usize = 35;
    const PAKFILE_LUMP_INDEX: usize = 40;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"VBSP");
    bytes.extend_from_slice(&20_u32.to_le_bytes());
    bytes.resize(HEADER_LEN + LUMP_COUNT * LUMP_ENTRY_LEN, 0);

    if let Some(entities) = entities {
        let entities_offset = bytes.len();
        bytes.extend_from_slice(entities.as_bytes());
        write_lump_entry(
            &mut bytes,
            ENTITIES_LUMP_INDEX,
            entities_offset,
            entities.len(),
            0,
        );
    }

    let planes_offset = bytes.len();
    push_vec3(&mut bytes, [0.0, 0.0, 1.0]);
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    write_lump_entry(&mut bytes, PLANES_LUMP_INDEX, planes_offset, 20, 0);

    let nodes_offset = bytes.len();
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&(-1_i32).to_le_bytes());
    bytes.extend_from_slice(&(-1_i32).to_le_bytes());
    bytes.extend_from_slice(&[0_u8; 20]);
    write_lump_entry(&mut bytes, NODES_LUMP_INDEX, nodes_offset, 32, 0);

    let leaves_offset = bytes.len();
    bytes.extend_from_slice(&[0_u8; 56]);
    write_lump_entry(&mut bytes, LEAVES_LUMP_INDEX, leaves_offset, 56, 0);

    let prop_data = static_prop_game_lump_data();
    let game_lump_offset = bytes.len();
    let prop_offset = game_lump_offset + 20;
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    bytes.extend_from_slice(&i32::from_be_bytes(*b"sprp").to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&10_u16.to_le_bytes());
    bytes.extend_from_slice(&(prop_offset as i32).to_le_bytes());
    bytes.extend_from_slice(&(prop_data.len() as i32).to_le_bytes());
    bytes.extend_from_slice(&prop_data);
    write_lump_entry(&mut bytes, GAME_LUMP_INDEX, game_lump_offset, 20, 0);

    let pakfile_offset = bytes.len();
    let empty_zip = empty_zip_bytes();
    bytes.extend_from_slice(&empty_zip);
    write_lump_entry(
        &mut bytes,
        PAKFILE_LUMP_INDEX,
        pakfile_offset,
        empty_zip.len(),
        0,
    );

    bytes
}

fn append_lump(bytes: &mut Vec<u8>, lump_index: usize, data: &[u8], version: u32) {
    let offset = bytes.len();
    bytes.extend_from_slice(data);
    write_lump_entry(bytes, lump_index, offset, data.len(), version);
}

fn push_plane(bytes: &mut Vec<u8>, normal: [f32; 3]) {
    push_vec3(bytes, normal);
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
}

fn push_texture_data(bytes: &mut Vec<u8>, name_string_table_id: i32) {
    push_vec3(bytes, [1.0, 1.0, 1.0]);
    bytes.extend_from_slice(&name_string_table_id.to_le_bytes());
    bytes.extend_from_slice(&512_i32.to_le_bytes());
    bytes.extend_from_slice(&512_i32.to_le_bytes());
    bytes.extend_from_slice(&512_i32.to_le_bytes());
    bytes.extend_from_slice(&512_i32.to_le_bytes());
}

fn push_texture_info(bytes: &mut Vec<u8>, flags: u32, texture_data_index: i32) {
    for transform in [
        [1.0_f32, 0.0, 0.0, 0.0],
        [0.0_f32, 1.0, 0.0, 0.0],
        [1.0_f32, 0.0, 0.0, 0.0],
        [0.0_f32, 1.0, 0.0, 0.0],
    ] {
        for value in transform {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(&texture_data_index.to_le_bytes());
}

fn push_face(
    bytes: &mut Vec<u8>,
    plane_num: u16,
    first_edge: i32,
    texture_info: i16,
    original_face: i32,
) {
    bytes.extend_from_slice(&plane_num.to_le_bytes());
    bytes.push(0);
    bytes.push(0);
    bytes.extend_from_slice(&first_edge.to_le_bytes());
    bytes.extend_from_slice(&3_i16.to_le_bytes());
    bytes.extend_from_slice(&texture_info.to_le_bytes());
    bytes.extend_from_slice(&(-1_i16).to_le_bytes());
    bytes.extend_from_slice(&0_i16.to_le_bytes());
    bytes.extend_from_slice(&[0_u8; 4]);
    bytes.extend_from_slice(&(-1_i32).to_le_bytes());
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&original_face.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
}

fn push_model(
    bytes: &mut Vec<u8>,
    mins: [f32; 3],
    maxs: [f32; 3],
    origin: [f32; 3],
    head_node: i32,
    first_face: i32,
    face_count: i32,
) {
    push_vec3(bytes, mins);
    push_vec3(bytes, maxs);
    push_vec3(bytes, origin);
    bytes.extend_from_slice(&head_node.to_le_bytes());
    bytes.extend_from_slice(&first_face.to_le_bytes());
    bytes.extend_from_slice(&face_count.to_le_bytes());
}

fn push_edge(bytes: &mut Vec<u8>, start: u16, end: u16) {
    bytes.extend_from_slice(&start.to_le_bytes());
    bytes.extend_from_slice(&end.to_le_bytes());
}

fn static_prop_game_lump_data() -> Vec<u8> {
    static_prop_game_lump_data_with([10.0, 20.0, 30.0], &[0, 1])
}

fn static_prop_game_lump_data_with(origin: [f32; 3], leaves: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    let mut name = [0_u8; 128];
    let source_name = br"\Models\Props_C17\Oildrum001.MDL";
    name[..source_name.len()].copy_from_slice(source_name);
    bytes.extend_from_slice(&name);
    bytes.extend_from_slice(&(leaves.len() as i32).to_le_bytes());
    for leaf in leaves {
        bytes.extend_from_slice(&leaf.to_le_bytes());
    }
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    push_vec3(&mut bytes, origin);
    push_vec3(&mut bytes, [1.0, 90.0, 3.0]);
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&(leaves.len() as u16).to_le_bytes());
    bytes.push(0);
    bytes.push(0);
    bytes.extend_from_slice(&2_i32.to_le_bytes());
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    push_vec3(&mut bytes, [0.0; 3]);
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes
}

fn empty_static_prop_game_lump_data() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    bytes
}

fn push_vec3(bytes: &mut Vec<u8>, values: [f32; 3]) {
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

fn write_lump_entry(
    bytes: &mut [u8],
    lump_index: usize,
    offset: usize,
    length: usize,
    version: u32,
) {
    let start = 8 + lump_index * 16;
    bytes[start..start + 4].copy_from_slice(&(offset as u32).to_le_bytes());
    bytes[start + 4..start + 8].copy_from_slice(&(length as u32).to_le_bytes());
    bytes[start + 8..start + 12].copy_from_slice(&version.to_le_bytes());
}

fn empty_zip_bytes() -> [u8; 22] {
    [
        0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]
}

fn test_ambient_lighting(samples: Vec<MapAmbientSample>) -> MapAmbientLighting {
    MapAmbientLighting {
        source: AmbientLightSource::Ldr,
        locator: MapLeafLocator {
            planes: vec![MapPlane {
                normal: [0.0, 0.0, 1.0],
                dist: 0.0,
            }],
            nodes: vec![MapNode {
                plane_index: 0,
                children: [-1, -1],
            }],
            leaves: vec![MapLeaf {
                cluster: 7,
                mins: [-16; 3],
                maxs: [16; 3],
            }],
        },
        leaf_sample_ranges: vec![AmbientSampleRange {
            start: 0,
            count: samples.len(),
        }],
        samples,
    }
}
