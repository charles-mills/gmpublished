#[test]
fn overlay_quad_uses_packed_u_basis_and_v_flip_flag() {
    // Wall overlay: normal +X. vbsp packs BasisU into the UV points'
    // z components; corners live in the (U, V) plane as xy pairs.
    let mut overlay = OverlayBasis {
        uv_points: [
            // xy = corner (-8, -4); z components 0..2 pack BasisU = +Y.
            Vec3::new(-8.0, -4.0, 0.0),
            Vec3::new(-8.0, 4.0, 1.0),
            Vec3::new(8.0, 4.0, 0.0),
            // z here is the V-flip flag (0 = unflipped).
            Vec3::new(8.0, -4.0, 0.0),
        ],
        origin: Vec3::new(100.0, 200.0, 300.0),
        basis_normal: Vec3::new(1.0, 0.0, 0.0),
    };

    let quad = overlay_quad_positions(overlay).expect("valid overlay");
    assert_eq!(quad[0], Vec3::new(100.0, 200.0 - 8.0, 300.0 - 4.0));
    assert_eq!(quad[2], Vec3::new(100.0, 200.0 + 8.0, 300.0 + 4.0));

    overlay.uv_points[3][2] = 1.0;
    let flipped = overlay_quad_positions(overlay).expect("valid overlay");
    assert_eq!(flipped[0], Vec3::new(100.0, 200.0 - 8.0, 300.0 + 4.0));

    overlay.uv_points[1][2] = 0.0;
    overlay.uv_points[3][2] = 0.0;
    assert!(overlay_quad_positions(overlay).is_none());
}
use super::*;
use crate::math::Vec3;

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

    assert_eq!(texture_coord(Vec3::new(7.0, 11.0, 13.0), transform), 104.0);
}

#[test]
fn color_rgb_exp32_decodes_to_source_ldr_lightmap_bytes() {
    assert_eq!(
        decode_light_sample(ColorRgbExp {
            r: 255,
            g: 128,
            b: 0,
            exponent: 0,
        }),
        [128, 93, 0, 255]
    );
    assert_eq!(
        decode_light_sample(ColorRgbExp {
            r: 128,
            g: 64,
            b: 32,
            exponent: -1,
        }),
        [68, 50, 36, 255]
    );
}

#[test]
fn ambient_decode_uses_color_rgb_exp32_to_vector_scale() {
    // The gm_construct sample (143,211,254)e-9 decodes to about 0.28/0.41/0.50.
    // Applying the lightmap formula would divide it by 255 again.
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
    let mut colors = [Vec3::ZERO; 6];
    colors[4] = Vec3::new(0.0, 1.0, 0.0);
    let cube = AmbientCube { colors };

    assert_eq!(
        cube.evaluate(Vec3::new(0.0, 0.0, 1.0)),
        Vec3::new(0.0, 1.0, 0.0)
    );
    assert_eq!(cube.evaluate(Vec3::new(0.0, 0.0, -1.0)), Vec3::splat(0.0));
}

#[test]
fn ambient_lookup_selects_nearest_sample_in_leaf() {
    let red = AmbientCube {
        colors: [Vec3::new(1.0, 0.0, 0.0); 6],
    };
    let green = AmbientCube {
        colors: [Vec3::new(0.0, 1.0, 0.0); 6],
    };
    let ambient = test_ambient_lighting(vec![
        MapAmbientSample {
            position: Vec3::splat(0.0),
            encoded_position: [128; 3],
            cube: red,
            cluster: 7,
        },
        MapAmbientSample {
            position: Vec3::new(10.0, 0.0, 0.0),
            encoded_position: [207, 128, 128],
            cube: green,
            cluster: 7,
        },
    ]);

    assert_eq!(ambient.cube_at(Vec3::new(9.0, 0.0, 0.0)), green);
}

#[test]
fn load_map_uses_white_ambient_when_lumps_are_absent() {
    let map = load_map(&bsp_fixture(None)).expect("fixture bsp should load");

    assert_eq!(map.ambient.source(), AmbientLightSource::Neutral);
    assert_eq!(map.ambient.cube_at(Vec3::splat(0.0)), AmbientCube::WHITE);
}

#[test]
fn explicit_pool_decode_matches_the_serial_domain_path() {
    let bytes = bsp_fixture(None);
    let serial = load_map(&bytes).expect("serial fixture decode");
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .expect("caller-owned test pool");
    let parallel = load_map_with_pool(&bytes, &pool).expect("parallel fixture decode");

    assert_eq!(parallel, serial);
}

#[test]
fn lightmap_uv_math_uses_face_min_size_and_half_luxel_bias() {
    let uv = brush_lightmap_uv_from_transforms(
        Vec3::new(10.0, 20.0, 30.0),
        [0.5, 0.0, 0.0, 2.0],
        [0.0, 0.25, 0.0, 1.0],
        [3, 4],
        [8, 4],
    );

    assert_eq!(uv, [0.5, 0.5]);
}

#[test]
fn displacement_tessellation_matches_base_face_fan_winding() {
    let steps = 2_usize;
    let side = steps + 1;
    let mut grid = Vec::new();
    for column in 0..side {
        for row in 0..side {
            grid.push(DisplacementGridVertex {
                position: Vec3::new(column as f32, row as f32, 0.0),
                base_position: Vec3::new(column as f32, row as f32, 0.0),
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
fn displacement_textures_follow_base_quad_under_lateral_and_vertical_offsets() {
    let corners = [
        Vec3::new(16.0, -32.0, 8.0),
        Vec3::new(208.0, 16.0, 32.0),
        Vec3::new(176.0, 112.0, 80.0),
        Vec3::new(-16.0, 64.0, 56.0),
    ];
    for start_corner in 0..4 {
        let mut bsp = displacement_test_bsp(2, corners, start_corner);
        let texinfo = TexInfo {
            texture_vecs: [[0.5, -0.25, 0.75, 19.0], [-0.125, 0.625, -0.5, -7.0]],
            lightmap_vecs: [[0.0; 4]; 2],
            flags: 0,
            texdata: -1,
        };
        for (index, vertex) in bsp.displacement_verts.iter_mut().enumerate() {
            vertex.vector = [0.6, 0.0, 0.8];
            vertex.dist = (index + 1) as f32 * 3.0;
            vertex.alpha = index as f32 * 10.0;
        }
        let displacement = &bsp.displacements[0];
        let vertices = displacement_vertices(&bsp, &bsp.faces[0], displacement).unwrap();
        let mut meshes = BuildMeshes::default();
        let mesh = meshes.get_or_insert(0, GeometryPartition::Visible, false);
        super::mesh::append_displacement(
            &bsp,
            &bsp.faces[0],
            displacement,
            &texinfo,
            Vec3::new(0.0, 0.0, 1.0),
            mesh,
            None,
        )
        .unwrap();
        assert_eq!(mesh.vertices.len(), 4 * 4 * 6);
        for (built, grid_vertex) in mesh.vertices.iter().zip(&vertices) {
            let u = grid_vertex.column as f32 / 4.0;
            let v = grid_vertex.row as f32 / 4.0;
            let corner = |index| corners[(start_corner + index) % 4];
            let base = corner(0) * ((1.0 - u) * (1.0 - v))
                + corner(1) * (u * (1.0 - v))
                + corner(2) * (u * v)
                + corner(3) * ((1.0 - u) * v);
            let disp_index = grid_vertex.column * 5 + grid_vertex.row;
            let offset = Vec3::new(0.6, 0.0, 0.8) * ((disp_index + 1) as f32 * 3.0);
            assert!((built.vertex.position - base - offset).length_squared() < 1.0e-8);
            for (actual, transform) in [built.vertex.tex_s, built.vertex.tex_t]
                .into_iter()
                .zip(texinfo.texture_vecs)
            {
                let expected = base[0] * transform[0]
                    + base[1] * transform[1]
                    + base[2] * transform[2]
                    + transform[3];
                assert!(
                    (actual - expected).abs() < 1.0e-5,
                    "{start_corner}: {actual} vs {expected}"
                );
            }
            assert_eq!(built.vertex.blend_alpha, disp_index as f32 * 10.0 / 255.0);
        }
    }
}

#[test]
fn displacement_triangles_match_source_checkerboard_for_all_supported_powers() {
    for power in 2..=4 {
        let steps = 1 << power;
        let bsp = displacement_test_bsp(
            power,
            [
                Vec3::ZERO,
                Vec3::new(steps as f32, 0.0, 0.0),
                Vec3::new(steps as f32, steps as f32, 0.0),
                Vec3::new(0.0, steps as f32, 0.0),
            ],
            0,
        );
        let vertices = displacement_vertices(&bsp, &bsp.faces[0], &bsp.displacements[0]).unwrap();
        let mut actual = vertices
            .chunks_exact(3)
            .map(|triangle| {
                let mut indices = triangle
                    .iter()
                    .map(|vertex| vertex.row * (steps + 1) + vertex.column)
                    .collect::<Vec<_>>();
                indices.sort_unstable();
                indices
            })
            .collect::<Vec<_>>();
        let mut expected = Vec::new();
        // Source indexes its grid across rows. Compare unordered triangles so
        // traversal order and cyclic rotations cannot disguise a diagonal error.
        for y in 0..steps {
            for x in 0..steps {
                let a = y * (steps + 1) + x;
                let b = a + 1;
                let c = a + steps + 1;
                let d = c + 1;
                let triangles = if (x + y) % 2 == 0 {
                    [[a, b, d], [a, d, c]]
                } else {
                    [[a, b, c], [b, d, c]]
                };
                for mut triangle in triangles {
                    triangle.sort_unstable();
                    expected.push(triangle.to_vec());
                }
            }
        }
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected, "displacement power {power}");
    }
}

#[test]
fn displacement_walk_collision_uses_the_rendered_checkerboard_surface() {
    let mut bsp = displacement_test_bsp(
        2,
        [
            Vec3::ZERO,
            Vec3::new(256.0, 0.0, 0.0),
            Vec3::new(256.0, 256.0, 0.0),
            Vec3::new(0.0, 256.0, 0.0),
        ],
        0,
    );
    bsp.displacement_verts[6].dist = 64.0;
    let mut collision = empty_walk_collision();
    collision.displacements = super::walk::walk_displacements_from_bsp(&bsp);
    assert_eq!(collision.displacements.len(), 1);
    for (x, y, expected_height) in [(48.0, 16.0, 16.0), (32.0, 96.0, 32.0)] {
        let hit = collision.trace_aabb(Vec3::new(x, y, 128.0), Vec3::new(x, y, -64.0), Vec3::ZERO);
        assert!(!hit.start_solid && hit.fraction < 1.0, "{hit:?}");
        assert!(
            (hit.end_position[2] - expected_height).abs() < 1.0e-4,
            "at {x},{y}: expected ground {expected_height}, got {hit:?}"
        );
    }
}

pub(super) fn displacement_test_bsp(power: i32, corners: [Vec3; 4], start_corner: usize) -> MapBsp {
    let mut bsp = read_fixture_bsp(&bsp_fixture(None));
    bsp.vertices = corners.to_vec();
    bsp.edges = vec![[0, 1], [1, 2], [2, 3], [3, 0]];
    bsp.surfedges = vec![0, 1, 2, 3];
    bsp.faces = vec![Face {
        plane: 0,
        side: 0,
        on_node: 0,
        first_edge: 0,
        edge_count: 4,
        texinfo: -1,
        displacement: 0,
        styles: [255; 4],
        light_offset: -1,
        area: 0.0,
        lightmap_mins: [0; 2],
        lightmap_size: [0; 2],
        original_face: -1,
    }];
    bsp.displacements = vec![DispInfo {
        start_position: corners[start_corner].into(),
        vert_start: 0,
        tri_start: 0,
        power,
        min_tess: 0,
        smoothing_angle: 0.0,
        contents: contents_flags::SOLID,
        map_face: 0,
        lightmap_alpha_start: 0,
        lightmap_sample_position_start: 0,
        edge_neighbors: [[vformats::bsp::DispSubNeighbor::default(); 2]; 4],
        corner_neighbors: [vformats::bsp::DispCornerNeighbors::default(); 4],
        allowed_vertices: [u32::MAX; 10],
    }];
    let side = (1 << power) + 1;
    bsp.displacement_verts = vec![
        DispVert {
            vector: [0.0, 0.0, 1.0],
            dist: 0.0,
            alpha: 0.0,
        };
        side * side
    ];
    bsp
}

#[test]
fn walk_trace_hits_solid_brush_head_on_with_expanded_hull() {
    let collision = box_collision(Vec3::new(10.0, -16.0, -16.0), Vec3::new(20.0, 16.0, 16.0));
    let hit = collision.trace_aabb(Vec3::ZERO, Vec3::new(30.0, 0.0, 0.0), Vec3::splat(1.0));

    assert!(!hit.start_solid);
    assert!(hit.fraction > 0.29 && hit.fraction < 0.30, "{hit:?}");
    assert!((hit.end_position[0] - 8.96875).abs() < 1.0e-4);
    assert_eq!(hit.normal, Vec3::new(-1.0, 0.0, 0.0));
}

#[test]
fn walk_trace_glancing_hit_supplies_slide_plane_normal() {
    let collision = box_collision(Vec3::new(10.0, -16.0, -16.0), Vec3::new(20.0, 16.0, 16.0));
    let start = Vec3::ZERO;
    let end = Vec3::new(30.0, 10.0, 0.0);
    let hit = collision.trace_aabb(start, end, Vec3::splat(1.0));
    let delta = end - start;
    let slide = delta - hit.normal * delta.dot(hit.normal);

    assert_eq!(hit.normal, Vec3::new(-1.0, 0.0, 0.0));
    assert!(slide[0].abs() < 1.0e-5, "{slide:?}");
    assert!(slide[1] > 9.9, "{slide:?}");
}

#[test]
fn walk_trace_low_ledge_clears_after_step_height_lift() {
    let collision = box_collision(Vec3::new(10.0, -16.0, -8.0), Vec3::new(20.0, 16.0, 6.0));
    let half_extents = Vec3::new(1.0, 1.0, 8.0);
    let start = Vec3::new(0.0, 0.0, 8.0);
    let end = Vec3::new(30.0, 0.0, 8.0);

    let blocked = collision.trace_aabb(start, end, half_extents);
    let lifted = collision.trace_aabb(
        start + (Vec3::new(0.0, 0.0, 18.0)),
        end + (Vec3::new(0.0, 0.0, 18.0)),
        half_extents,
    );

    assert!(blocked.fraction < 1.0, "{blocked:?}");
    assert_eq!(blocked.normal, Vec3::new(-1.0, 0.0, 0.0));
    assert_eq!(lifted.fraction, 1.0);
}

#[test]
fn prop_trace_blocks_and_resting_contact_uses_same_solidity_predicate() {
    let ledge = box_ledge(Vec3::new(10.0, -16.0, -16.0), Vec3::new(20.0, 16.0, 16.0));
    let collision = empty_walk_collision().with_prop_collisions([MapWalkPropCollisionSource {
        ledges: std::slice::from_ref(&ledge),
        origin: Vec3::splat(0.0),
        angles: Vec3::splat(0.0),
        scale: 1.0,
    }]);

    let hit = collision.trace_aabb(Vec3::ZERO, Vec3::new(30.0, 0.0, 0.0), Vec3::splat(1.0));
    assert_eq!(collision.prop_hull_count(), 1);
    assert!(!hit.start_solid);
    assert!(hit.fraction > 0.29 && hit.fraction < 0.30, "{hit:?}");
    assert_eq!(hit.normal, Vec3::new(-1.0, 0.0, 0.0));

    let half_extents = Vec3::new(1.0, 1.0, 1.0);
    assert!(!collision.aabb_trace_solid(
        Vec3::new(15.0, 0.0, 17.0 + TRACE_PLANE_EPSILON),
        half_extents
    ));
    assert!(collision.aabb_trace_solid(Vec3::new(15.0, 0.0, 16.5), half_extents));
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
        origin: Vec3::splat(0.0),
        angles: Vec3::splat(0.0),
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
    let half_extents = Vec3::new(8.0, 8.0, 8.0);
    let directions = [
        Vec3::new(96.0, 96.0, 0.0),
        Vec3::new(96.0, 0.0, 96.0),
        Vec3::new(0.0, 96.0, 96.0),
        Vec3::new(96.0, 96.0, 96.0),
        Vec3::new(-96.0, 96.0, 0.0),
        Vec3::new(96.0, -96.0, 0.0),
    ];
    let mut repro = None;
    'search: for x in (-32..=80).step_by(8) {
        for y in (-32..=80).step_by(8) {
            for z in (-32..=80).step_by(8) {
                let start = Vec3::new(x as f32, y as f32, z as f32);
                for direction in directions {
                    let end = start + direction;
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
        box_planes(Vec3::new(10.0, -16.0, -16.0), Vec3::new(20.0, 16.0, 16.0)),
        contents_flags::PLAYERCLIP,
    )
    .expect("playerclip brush should enter walk collision");
    let collision = MapWalkCollision {
        brushes: vec![brush],
        water_brushes: Vec::new(),
        displacements: Vec::new(),
        props: MapWalkPropCollision::default(),
        physics_brushes: Vec::new(),
        physics_ray_brushes: Vec::new(),
    };

    let hit = collision.trace_aabb(Vec3::ZERO, Vec3::new(30.0, 0.0, 0.0), Vec3::splat(1.0));

    assert!(hit.fraction < 1.0, "{hit:?}");
    assert_eq!(hit.normal, Vec3::new(-1.0, 0.0, 0.0));
}

#[test]
fn walk_trace_water_only_brush_does_not_block() {
    let water = walk_brush_from_planes(
        box_planes(Vec3::new(10.0, -16.0, -16.0), Vec3::new(20.0, 16.0, 16.0)),
        contents_flags::WATER,
    )
    .expect("water brush should enter volume collision");
    let collision = MapWalkCollision {
        brushes: Vec::new(),
        water_brushes: vec![water],
        displacements: Vec::new(),
        props: MapWalkPropCollision::default(),
        physics_brushes: Vec::new(),
        physics_ray_brushes: Vec::new(),
    };

    let hit = collision.trace_aabb(Vec3::ZERO, Vec3::new(30.0, 0.0, 0.0), Vec3::splat(1.0));

    assert_eq!(hit.fraction, 1.0);
    assert!(!hit.start_solid);
}

#[test]
fn water_at_reports_inside_outside_and_surface_height() {
    let collision = MapWalkCollision::empty()
        .with_water_box_for_tests(Vec3::new(-32.0, -24.0, -16.0), Vec3::new(32.0, 24.0, 48.0));

    assert_eq!(
        collision.water_at(Vec3::new(4.0, -3.0, 12.0)),
        Some(WaterVolume { surface_z: 48.0 })
    );
    assert_eq!(collision.water_at(Vec3::new(40.0, 0.0, 12.0)), None);
    assert_eq!(collision.water_at(Vec3::new(0.0, 0.0, 49.0)), None);
}

#[test]
fn water_at_uses_highest_surface_for_stacked_volumes() {
    let collision = MapWalkCollision::empty()
        .with_water_box_for_tests(Vec3::splat(-32.0), Vec3::new(32.0, 32.0, 24.0))
        .with_water_box_for_tests(Vec3::new(-16.0, -16.0, -8.0), Vec3::new(16.0, 16.0, 48.0));

    assert_eq!(
        collision.water_at(Vec3::splat(0.0)),
        Some(WaterVolume { surface_z: 48.0 })
    );
    assert!(
        walk_brush_from_planes(
            box_planes(Vec3::splat(-1.0), Vec3::splat(1.0)),
            contents_flags::SLIME
        )
        .is_some()
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
            Vec3::new(-16.0, -16.0, 10.0),
            Vec3::new(16.0, 16.0, 20.0),
            5,
        )],
        water_brushes: Vec::new(),
        displacements: Vec::new(),
        props: MapWalkPropCollision::default(),
        physics_brushes: Vec::new(),
        physics_ray_brushes: Vec::new(),
    };
    assert!(sky_ceiling.ray_hits_sky(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)));
    assert!(sky_ceiling.ray_hits_sky(Vec3::new(64.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)));

    let plain_ceiling = MapWalkCollision {
        brushes: vec![box_brush_with_sky_side(
            Vec3::new(-16.0, -16.0, 10.0),
            Vec3::new(16.0, 16.0, 20.0),
            usize::MAX,
        )],
        water_brushes: Vec::new(),
        displacements: Vec::new(),
        props: MapWalkPropCollision::default(),
        physics_brushes: Vec::new(),
        physics_ray_brushes: Vec::new(),
    };
    assert!(!plain_ceiling.ray_hits_sky(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)));
    assert!(!plain_ceiling.ray_hits_sky(Vec3::new(64.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0)));
}

#[test]
fn walk_trace_displacement_triangle_hits_from_above() {
    let triangle = MapWalkTriangle::new([
        Vec3::new(-64.0, -64.0, 0.0),
        Vec3::new(64.0, -64.0, 0.0),
        Vec3::new(-64.0, 64.0, 0.0),
    ])
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
        physics_brushes: Vec::new(),
        physics_ray_brushes: Vec::new(),
    };

    let hit = collision.trace_aabb(
        Vec3::new(0.0, 0.0, 100.0),
        Vec3::new(0.0, 0.0, -20.0),
        Vec3::new(16.0, 16.0, 36.0),
    );

    assert!(!hit.start_solid);
    assert!(hit.fraction > 0.53 && hit.fraction < 0.54, "{hit:?}");
    assert!(hit.normal[2] > 0.99, "{hit:?}");
    assert!((hit.end_position[2] - 36.0).abs() < 0.1, "{hit:?}");
}

#[test]
fn sloped_displacement_rays_touch_exactly_and_hulls_back_off_along_the_plane_normal() {
    let triangle = MapWalkTriangle::new([
        Vec3::new(-1024.0, -1024.0, -512.0),
        Vec3::new(1024.0, -1024.0, 512.0),
        Vec3::new(0.0, 1024.0, 0.0),
    ])
    .unwrap();
    let mut collision = empty_walk_collision();
    collision.displacements.push(MapWalkDisplacement {
        triangles: vec![triangle],
        bounds_min: triangle.bounds_min,
        bounds_max: triangle.bounds_max,
    });
    let normal = Vec3::new(-1.0, 0.0, 2.0).normalize().unwrap();
    let extents = Vec3::new(16.0, 12.0, 36.0);
    let radius = 88.0 / 5.0_f32.sqrt();
    // A large tangential component distinguishes distance along the sweep
    // from the perpendicular plane tolerance. Backface approaches pass through.
    for (start, end, facing) in [
        (
            Vec3::new(0.0, 0.0, 256.0),
            Vec3::new(128.0, 512.0, -128.0),
            1.0,
        ),
        (
            Vec3::new(0.0, 0.0, -256.0),
            Vec3::new(128.0, 512.0, 128.0),
            -1.0,
        ),
    ] {
        let ray = collision.trace_aabb(start, end, Vec3::ZERO);
        let hull = collision.trace_aabb(start, end, extents);
        if facing < 0.0 {
            for trace in [ray, hull] {
                assert!(!trace.start_solid, "{trace:?}");
                assert_eq!(trace.fraction, 1.0, "{trace:?}");
                assert_eq!(trace.end_position, end, "{trace:?}");
            }
            continue;
        }
        let expected_fraction = -start.dot(normal) / (end - start).dot(normal);
        assert!(!ray.start_solid && ray.fraction < 1.0, "{ray:?}");
        assert!((ray.fraction - expected_fraction).abs() < 1.0e-6, "{ray:?}");
        assert!(ray.end_position.dot(normal).abs() < 3.0e-5, "{ray:?}");
        assert!(ray.normal.dot(normal * facing) > 0.9999, "{ray:?}");

        let gap = hull.end_position.dot(normal) * facing - radius;
        assert!(
            !hull.start_solid && hull.fraction < ray.fraction,
            "{hull:?}"
        );
        assert!(
            (gap - TRACE_PLANE_EPSILON).abs() < 3.0e-5,
            "expected normal gap {TRACE_PLANE_EPSILON}, got {gap}: {hull:?}"
        );
        assert!(hull.normal.dot(normal * facing) > 0.9999, "{hull:?}");
    }
}

#[test]
fn displacement_static_hull_overlap_is_solid_but_initial_swept_overlap_is_ignored() {
    let triangle = MapWalkTriangle::new([
        Vec3::new(-128.0, -128.0, 0.0),
        Vec3::new(128.0, -128.0, 0.0),
        Vec3::new(0.0, 128.0, 0.0),
    ])
    .unwrap();
    let mut collision = empty_walk_collision();
    collision.displacements.push(MapWalkDisplacement {
        triangles: vec![triangle],
        bounds_min: triangle.bounds_min,
        bounds_max: triangle.bounds_max,
    });
    let center = Vec3::new(0.0, 0.0, 4.0);
    let extents = Vec3::splat(8.0);
    let stationary = collision.trace_aabb(center, center, extents);
    assert!(stationary.start_solid, "{stationary:?}");
    assert_eq!(stationary.fraction, 0.0);

    for end in [Vec3::new(0.0, 0.0, -24.0), Vec3::new(0.0, 0.0, 24.0)] {
        let swept = collision.trace_aabb(center, end, extents);
        assert!(!swept.start_solid, "{swept:?}");
        assert_eq!(swept.fraction, 1.0, "{swept:?}");
        assert_eq!(swept.end_position, end);
    }
    let stationary_ray = collision.trace_aabb(Vec3::ZERO, Vec3::ZERO, Vec3::ZERO);
    assert!(!stationary_ray.start_solid);
    assert_eq!(stationary_ray.fraction, 1.0);

    // Inside the triangle's bounds, but outside the triangle itself.
    let outside = Vec3::new(100.0, 100.0, 4.0);
    let stationary_outside = collision.trace_aabb(outside, outside, extents);
    assert!(!stationary_outside.start_solid, "{stationary_outside:?}");
    assert_eq!(stationary_outside.fraction, 1.0);
}

#[test]
fn brush_start_solid_preserves_exit_and_nearest_collision_independent_of_brush_order() {
    let start = Vec3::ZERO;
    let end = Vec3::new(20.0, 0.0, 0.0);
    let extents = Vec3::splat(1.0);
    let mut collision = box_collision(Vec3::splat(-4.0), Vec3::splat(4.0));
    let exiting = collision.trace_aabb(start, end, extents);
    assert!(exiting.start_solid);
    assert_eq!(exiting.fraction, 1.0);
    assert_eq!(exiting.end_position, end);
    let contained = collision.trace_aabb(start, Vec3::new(1.0, 0.0, 0.0), extents);
    assert!(contained.start_solid);
    assert_eq!(contained.fraction, 0.0);
    assert_eq!(contained.end_position, start);

    collision
        .brushes
        .extend(box_collision(Vec3::new(10.0, -16.0, -16.0), Vec3::new(12.0, 16.0, 16.0)).brushes);
    for _ in 0..2 {
        let hit = collision.trace_aabb(start, end, extents);
        assert!(hit.start_solid, "{hit:?}");
        assert!(
            (hit.end_position[0] - (9.0 - TRACE_PLANE_EPSILON)).abs() < 1.0e-5,
            "{hit:?}"
        );
        assert_eq!(hit.normal, Vec3::new(-1.0, 0.0, 0.0));
        collision.brushes.reverse();
    }
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
    assert!(is_pakfile_retained_entry("sp_0.vhv"));
    assert!(is_pakfile_retained_entry("sp_hdr_181.vhv"));
    assert!(!is_pakfile_retained_entry("models/sp_0.vhv"));
    assert!(!is_pakfile_retained_entry("sp_.vhv"));
    assert!(!is_pakfile_retained_entry("sp_hdr_bad.vhv"));
    assert!(!is_pakfile_retained_entry("maps/thumb.png"));
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

    assert_vec3_close(sun.direction_to_sun, Vec3::new(0.0, -0.866_025_4, 0.5));
    // VRAD LightForString semantics: linear = (c/255)^2.2 * (intensity/255).
    let expected_channel = |channel: f32, intensity: f32| -> f32 {
        (channel / 255.0_f32).powf(2.2) * (intensity / 255.0)
    };
    assert_vec3_close(
        sun.color_linear,
        Vec3::new(
            expected_channel(255.0, 128.0),
            expected_channel(128.0, 128.0),
            0.0,
        ),
    );
    assert_vec3_close(
        lighting.skylight_linear.expect("skylight"),
        Vec3::new(
            expected_channel(64.0, 64.0),
            expected_channel(128.0, 64.0),
            expected_channel(255.0, 64.0),
        ),
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
        Vec3::new(
            400.0 / 255.0,
            (241.0 / 255.0_f32).powf(2.2) * (400.0 / 255.0),
            (224.0 / 255.0_f32).powf(2.2) * (400.0 / 255.0),
        ),
    );
    assert_vec3_close(
        lighting.skylight_linear.expect("three-component ambient"),
        Vec3::new(
            (160.0 / 255.0_f32).powf(2.2),
            (180.0 / 255.0_f32).powf(2.2),
            (200.0 / 255.0_f32).powf(2.2),
        ),
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
        Vec3::new(0.0, (128.0 / 255.0_f32).powf(2.2), 1.0),
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
            origin: Vec3::new(128.0, -64.0, 32.0),
            angles: Vec3::new(5.0, 90.0, 0.0),
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
            origin: Vec3::new(1.0, 2.0, 3.0),
            angles: Vec3::new(0.0, 270.0, 0.0),
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
            radial: false,
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
            origin: Vec3::new(10.0, 20.0, 30.0),
            scale: 8.0,
            fog: Some(MapFog {
                color_srgb: [32, 64, 128],
                start: 100.0,
                end: 900.0,
                max_density: 0.5,
                radial: false,
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
            entity: false,
            initially_visible: true,
            animation: None,
            static_prop_index: Some(0),
            lighting_origin: None,
            lighting_origin_reference: None,
            diffuse_modulation: [255; 4],
            render_mode: 0,
            flags: 0,
            flags_ex: None,
            extra_flags: Some(0),
            model_path: "models/props_c17/oildrum001.mdl".to_owned(),
            origin: Vec3::new(10.0, 20.0, 30.0),
            angles: Vec3::new(1.0, 90.0, 3.0),
            skin: 2,
            scale: 1.0,
            solid: MapPropSolid::None,
            leaf_membership: MapPropLeafMembership::Compiled(vec![0, 1]),
            visibility: MapPropVisibility::Always,
        }]
    );
}

#[test]
fn static_prop_render_membership_retains_authored_leaf_order_and_duplicates() {
    let mut bsp = read_fixture_bsp(&skybox_partition_bsp_fixture(true, true));
    bsp.leaves[1].cluster = bsp.leaves[0].cluster;
    let props = bsp.static_props.as_mut().unwrap();
    props.leaves = vec![1, 0, 1];
    props.props[0].first_leaf = 0;
    props.props[0].leaf_count = 3;
    let (props, skybox) =
        partitioned_static_prop_placements(&bsp, &SkyboxPartition::inactive(false));
    assert!(skybox.is_empty());
    assert_eq!(props.len(), 1);
    assert_eq!(
        props[0].leaf_membership,
        MapPropLeafMembership::Compiled(vec![1, 0, 1])
    );
    assert_eq!(props[0].visibility, MapPropVisibility::Clusters(vec![0]));
}

#[test]
fn unregistered_static_props_keep_compiled_empty_membership_without_spatial_fallback() {
    let mut bsp = read_fixture_bsp(&static_prop_bsp_fixture());
    for (first, count) in [(0, 0), (3, 1), (u16::MAX, u16::MAX)] {
        let prop = &mut bsp.static_props.as_mut().unwrap().props[0];
        prop.first_leaf = first;
        prop.leaf_count = count;
        let (props, _) =
            partitioned_static_prop_placements(&bsp, &SkyboxPartition::inactive(false));
        assert_eq!(
            props[0].leaf_membership,
            MapPropLeafMembership::Compiled(Vec::new())
        );
    }
}

#[test]
fn model_entities_retain_render_color_alpha_and_mode_before_draw_policy() {
    let entities = r#"
    { "classname" "prop_dynamic" "model" "models/test/model.mdl" "origin" "0 0 0"
      "rendercolor" "121 67 233" "renderamt" "128" "rendermode" "0" }
    { "classname" "prop_dynamic" "model" "models/test/model.mdl" "origin" "1 0 0"
      "rendercolor" "32 64 128 191" "rendermode" "2" }
    { "classname" "prop_dynamic" "model" "models/test/model.mdl" "origin" "2 0 0"
      "rendercolor" "32 64 128 191" "renderamt" "96" "rendermode" "10" }
    "#;
    let map = load_map(&bsp_fixture_with_entities(entities)).unwrap();
    let entities = map
        .static_props
        .iter()
        .filter(|prop| prop.entity)
        .collect::<Vec<_>>();
    assert_eq!(entities.len(), 3);
    assert_eq!(entities[0].diffuse_modulation, [121, 67, 233, 128]);
    assert_eq!(entities[0].render_mode, 0);
    assert_eq!(entities[1].diffuse_modulation, [32, 64, 128, 191]);
    assert_eq!(entities[1].render_mode, 2);
    assert_eq!(entities[2].diffuse_modulation, [32, 64, 128, 96]);
    assert_eq!(entities[2].render_mode, 10);
}

#[test]
fn static_prop_version_eleven_preserves_lighting_tint_flags_and_scale() {
    let mut bytes = static_prop_bsp_fixture();
    let mut data = static_prop_game_lump_data();
    let record = data.len() - 72;
    data.truncate(record + 56);
    data[record + 31] = 0x42;
    for (axis, value) in [12.0_f32, 34.0, 56.0].into_iter().enumerate() {
        data[record + 44 + axis * 4..record + 48 + axis * 4].copy_from_slice(&value.to_le_bytes());
    }
    data.extend_from_slice(&1.0_f32.to_le_bytes());
    data.extend_from_slice(&[0, 0, 0, 0]);
    data.extend_from_slice(&[31, 63, 127, 191]);
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&0x0123_4567_u32.to_le_bytes());
    data.extend_from_slice(&2.5_f32.to_le_bytes());
    let game_offset = bytes.len();
    append_sprp_game_lump(&mut bytes, &data);
    bytes[game_offset + 10..game_offset + 12].copy_from_slice(&11_u16.to_le_bytes());
    let map = load_map(&bytes).unwrap();
    let prop = &map.static_props[0];
    assert_eq!(prop.static_prop_index, Some(0));
    assert_eq!(prop.lighting_origin, Some(Vec3::new(12.0, 34.0, 56.0)));
    assert_eq!(prop.diffuse_modulation, [31, 63, 127, 191]);
    assert_eq!(prop.flags, 0x42);
    assert_eq!(prop.flags_ex, Some(0x0123_4567));
    assert_eq!(prop.extra_flags, None);
    assert_eq!(prop.scale, 2.5);
}

#[test]
fn skipped_static_props_do_not_shift_compiled_lighting_indices() {
    let mut bytes = static_prop_bsp_fixture();
    let mut data = static_prop_game_lump_data();
    let record = data.len() - 72;
    let second = data[record..].to_vec();
    data[record - 4..record].copy_from_slice(&2_i32.to_le_bytes());
    data[record + 24..record + 26].copy_from_slice(&999_u16.to_le_bytes());
    data.extend_from_slice(&second);
    append_sprp_game_lump(&mut bytes, &data);
    let map = load_map(&bytes).unwrap();
    assert_eq!(map.static_props.len(), 1);
    assert_eq!(map.static_props[0].static_prop_index, Some(1));
}

#[test]
fn invalid_static_prop_scale_and_lighting_origin_have_finite_fallbacks() {
    let mut bsp = read_fixture_bsp(&static_prop_bsp_fixture());
    let partition = SkyboxPartition::inactive(false);
    for scale in [0.0, -1.0, f32::INFINITY, f32::NAN] {
        let prop = &mut bsp.static_props.as_mut().unwrap().props[0];
        prop.uniform_scale = Some(scale);
        prop.flags = 0x02;
        prop.lighting_origin = [f32::NAN, 0.0, 0.0];
        let (world, _) = partitioned_static_prop_placements(&bsp, &partition);
        assert_eq!(world[0].scale, 1.0);
        assert_eq!(world[0].lighting_origin, None);
    }
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
    assert_eq!(entity_props[0].origin, Vec3::new(1.0, 2.0, 3.0));
    assert_eq!(entity_props[0].angles, Vec3::new(10.0, 20.0, 30.0));
    assert_eq!(entity_props[0].skin, 4);
    assert_eq!(entity_props[0].scale, 2.5);
    assert_eq!(entity_props[0].solid, MapPropSolid::None);
    assert_eq!(entity_props[1].skin, 0);
    assert_eq!(entity_props[1].scale, 1.0);
    assert_eq!(entity_props[4].angles, Vec3::new(14.0, 24.0, 34.0));
    assert!(
        entity_props
            .iter()
            .all(|prop| prop.visibility == MapPropVisibility::Always)
    );
    assert!(
        entity_props
            .iter()
            .all(|prop| prop.leaf_membership == MapPropLeafMembership::SpatialBounds)
    );
}

#[test]
fn entity_animation_state_is_placement_specific_and_keeps_source_identity() {
    let map = load_map(&bsp_fixture_with_entities(r#"
        { "classname" "prop_dynamic" "model" "models/entity/shared.mdl" "origin" "0 0 0" "DefaultAnim" "Flag_Idle" "hammerid" "4110" "cycle" "0.25" "playbackrate" "0.5" }
        { "classname" "dynamic_prop" "model" "models/entity/shared.mdl" "origin" "1 0 0" "sequence" "2" "cycle" "0.75" }
        { "classname" "prop_dynamic" "model" "models/entity/shared.mdl" "origin" "2 0 0" }
        { "classname" "prop_physics" "model" "models/entity/shared.mdl" "origin" "3 0 0" "DefaultAnim" "Flag_Idle" }
    "#)).unwrap();
    let props: Vec<_> = map.static_props.iter().filter(|prop| prop.entity).collect();
    assert_eq!(props.len(), 4);
    let first = props[0].animation.as_ref().unwrap();
    assert_eq!(first.default_sequence.as_deref(), Some("flag_idle"));
    assert_eq!(first.hammer_id, Some(4110));
    assert_eq!(first.initial_cycle, 0.25);
    assert_eq!(first.playback_rate, 0.5);
    let second = props[1].animation.as_ref().unwrap();
    assert_eq!(second.sequence_index, 2);
    assert_eq!(second.initial_cycle, 0.75);
    assert_ne!(first.entity_index, second.entity_index);
    assert!(props[2].animation.is_none());
    assert!(props[3].animation.is_none());
}

#[test]
fn initial_entity_visibility_is_separate_from_render_mode_and_animation() {
    let map = load_map(&bsp_fixture_with_entities(r#"
        { "classname" "prop_dynamic" "model" "models/entity/hidden.mdl" "origin" "0 0 0" "DefaultAnim" "idle" "StartDisabled" "1" }
        { "classname" "prop_dynamic" "model" "models/entity/effect_hidden.mdl" "origin" "1 0 0" "effects" "32" }
        { "classname" "prop_physics" "model" "models/entity/physics.mdl" "origin" "2 0 0" "StartDisabled" "1" }
    "#)).unwrap();
    let props: Vec<_> = map.static_props.iter().filter(|prop| prop.entity).collect();
    assert_eq!(
        props
            .iter()
            .map(|prop| prop.initially_visible)
            .collect::<Vec<_>>(),
        [false, false, true]
    );
    assert!(props[0].animation.is_some());
    assert!(props.iter().all(|prop| prop.render_mode == 0));
}

#[test]
fn entity_lighting_anchor_retains_first_reference_and_missing_or_ambiguous_diagnostics() {
    let map = load_map(&bsp_fixture_with_entities(r#"
        { "classname" "info_lighting" "targetname" "unique" "origin" "101 202 303" }
        { "classname" "info_lighting" "targetname" "duplicate" "origin" "4 5 6" }
        { "classname" "info_lighting" "targetname" "duplicate" "origin" "7 8 9" }
        { "classname" "prop_dynamic" "model" "models/anchor_unique.mdl" "origin" "1 2 3" "lightingorigin" "unique" }
        { "classname" "prop_dynamic" "model" "models/anchor_missing.mdl" "origin" "1 2 3" "lightingorigin" "missing" }
        { "classname" "prop_dynamic" "model" "models/anchor_duplicate.mdl" "origin" "1 2 3" "lightingorigin" "duplicate" }
        "#)).unwrap();
    let prop = |name: &str| {
        map.static_props
            .iter()
            .find(|prop| prop.model_path == name)
            .unwrap()
    };
    let unique = prop("models/anchor_unique.mdl");
    assert_eq!(unique.lighting_origin, Some(Vec3::new(101.0, 202.0, 303.0)));
    assert_eq!(
        unique
            .lighting_origin_reference
            .as_ref()
            .unwrap()
            .match_count,
        1
    );
    assert!(
        unique
            .lighting_origin_reference
            .as_ref()
            .unwrap()
            .first_entity
            .is_some()
    );
    let missing = prop("models/anchor_missing.mdl");
    assert_eq!(missing.lighting_origin, None);
    assert_eq!(
        missing
            .lighting_origin_reference
            .as_ref()
            .unwrap()
            .match_count,
        0
    );
    assert_eq!(
        missing
            .lighting_origin_reference
            .as_ref()
            .unwrap()
            .first_entity,
        None
    );
    let duplicate = prop("models/anchor_duplicate.mdl");
    assert_eq!(duplicate.lighting_origin, Some(Vec3::new(4.0, 5.0, 6.0)));
    assert_eq!(
        duplicate
            .lighting_origin_reference
            .as_ref()
            .unwrap()
            .match_count,
        2
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
    assert_eq!(entity_props[0].origin, Vec3::new(1.0, 2.0, 3.0));
    assert_eq!(entity_props[0].angles, Vec3::splat(0.0));
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
fn a_door_that_stays_open_ignores_its_wait_keyvalue() {
    let doors = pending_doors_from_fixture(
        r#"
        { "classname" "func_door" "model" "*1" "origin" "32 0 0" "wait" "5" "spawnflags" "32" }
        { "classname" "func_door_rotating" "model" "*1" "origin" "96 0 0" "distance" "45" "spawnflags" "32" }
        { "classname" "func_door" "model" "*1" "origin" "64 0 0" "wait" "5" }
        { "classname" "func_door" "model" "*1" "origin" "160 0 0" "wait" "-1" }
        "#,
    );

    assert_eq!(doors.len(), 4);
    assert_eq!(doors[0].auto_close_after, None);
    assert_eq!(doors[1].auto_close_after, None);
    assert_eq!(
        doors[2].auto_close_after,
        Some(5.0),
        "the same keyvalue without the flag still auto-closes"
    );
    assert_eq!(
        doors[3].auto_close_after, None,
        "a negative wait is Source's usual way to pin a door open"
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
    assert_eq!(linear.origin, Vec3::new(32.0, 0.0, 0.0));
    assert_eq!(linear.auto_close_after, Some(3.0));
    assert_eq!(linear.initial_progress, 0.0);
    match linear.motion {
        MapDoorMotion::Linear {
            direction,
            distance,
            speed,
        } => {
            assert_vec3_close(direction, Vec3::new(0.0, 1.0, 0.0));
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
    assert_eq!(
        movelinear.auto_close_after, None,
        "func_movelinear is a mover, not a door: Source never auto-returns it"
    );
    match movelinear.motion {
        MapDoorMotion::Linear {
            direction,
            distance,
            speed,
        } => {
            assert_vec3_close(direction, Vec3::new(-1.0, 0.0, 0.0));
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
            assert_eq!(angle_delta, Vec3::new(0.0, 0.0, -45.0));
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
    assert_eq!(prop.origin, Vec3::new(128.0, 0.0, 0.0));
    assert_eq!(prop.angles, Vec3::new(0.0, 90.0, 0.0));
    assert_eq!(prop.auto_close_after, None);
    match prop.motion {
        MapDoorMotion::Rotating {
            angle_delta,
            degrees,
            speed,
            open_direction,
        } => {
            assert_eq!(angle_delta, Vec3::new(0.0, 90.0, 0.0));
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

    assert!((linear_door_distance(model, Vec3::new(0.0, 1.0, 0.0), 4.0) - 34.0).abs() < 1.0e-4);
    assert!((linear_door_distance(model, Vec3::new(1.0, 0.0, 0.0), 1.0) - 13.0).abs() < 1.0e-4);
    assert_eq!(
        linear_door_distance(model, Vec3::new(0.0, 0.0, 1.0), 100.0),
        0.0
    );
}

#[test]
fn door_extraction_removes_bmodel_faces_without_shifting_downstream_ranges() {
    let map = load_map(&door_bmodel_bsp_fixture(
        r#"{ "classname" "func_door" "model" "*1" "origin" "32 0 0" }"#,
    ))
    .expect("fixture bsp should load");

    assert_eq!(map.doors.len(), 1);
    let world = map.meshes.first().expect("static mesh");
    assert!(!world.entity);
    assert_eq!(world.indices.len(), 3);
    assert_eq!(
        map.meshes
            .iter()
            .map(|mesh| mesh.indices.len())
            .sum::<usize>(),
        6
    );
    assert_eq!(
        world.visibility.always_visible,
        vec![MapMeshIndexRange {
            face: 0,
            start: 0,
            count: 3,
        },]
    );

    let MapDoorGeometry::Brush {
        model_index,
        meshes,
    } = &map.doors[0].geometry
    else {
        panic!("expected brush door");
    };
    assert_eq!(*model_index, ModelIndex::new(1));
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
fn unowned_bmodel_is_not_instantiated_as_world_geometry() {
    let map = load_map(&door_bmodel_bsp_fixture(""))
        .expect("fixture bsp without door entities should load");

    assert!(map.doors.is_empty());
    let world = map.meshes.first().expect("static mesh");
    assert!(!world.entity);
    assert_eq!(world.indices.len(), 3);
    assert_eq!(
        map.meshes
            .iter()
            .map(|mesh| mesh.indices.len())
            .sum::<usize>(),
        6
    );
    assert_eq!(
        world
            .visibility
            .always_visible
            .iter()
            .map(|range| (range.face, range.start, range.count))
            .collect::<Vec<_>>(),
        vec![(0, 0, 3)]
    );
}

#[test]
fn brush_placement_rotates_positions_and_normals_without_changing_authored_uvs() {
    let lit_fixture = |entities: &str| {
        let mut bytes = door_bmodel_bsp_fixture(entities);
        let offset = |bytes: &[u8], lump: usize| {
            i32::from_le_bytes(bytes[8 + lump * 16..12 + lump * 16].try_into().unwrap()) as usize
        };
        let face = offset(&bytes, FACES_LUMP_INDEX) + 56;
        bytes[face + 16..face + 20].copy_from_slice(&[0, 255, 255, 255]);
        bytes[face + 20..face + 24].copy_from_slice(&0_i32.to_le_bytes());
        bytes[face + 28..face + 32].copy_from_slice(&2_i32.to_le_bytes());
        bytes[face + 36..face + 40].copy_from_slice(&1_i32.to_le_bytes());
        bytes[face + 40..face + 44].copy_from_slice(&1_i32.to_le_bytes());
        let texinfo = offset(&bytes, TEXTURE_INFO_LUMP_INDEX);
        for index in [32, 52] {
            bytes[texinfo + index..texinfo + index + 4]
                .copy_from_slice(&(1.0_f32 / 16.0).to_le_bytes());
        }
        append_lump(
            &mut bytes,
            8,
            &[
                128, 80, 40, 0, 80, 128, 40, 0, 40, 80, 128, 0, 128, 128, 128, 0,
            ],
            0,
        );
        bytes
    };
    let local = load_map(&lit_fixture(
        r#"{ "classname" "func_brush" "model" "*1" "origin" "0 0 0" }"#,
    ))
    .unwrap();
    let placed = load_map(&lit_fixture(r#"{ "classname" "func_brush" "model" "*1" "origin" "300 40 120" "angles" "90 0 0" "parentname" "parent" } { "classname" "info_target" "targetname" "parent" "origin" "4000 2000 6000" "angles" "0 60 0" }"#)).unwrap();
    assert!(local.lightmap.is_some());
    assert_eq!(local.lightmap, placed.lightmap);
    let vertices_for_face = |map: &MapData| {
        let mesh = map
            .meshes
            .iter()
            .find(|mesh| mesh.visibility.always_visible.iter().any(|r| r.face == 1))
            .unwrap();
        assert!(mesh.entity);
        let range = mesh
            .visibility
            .always_visible
            .iter()
            .find(|r| r.face == 1)
            .unwrap();
        mesh.indices[range.start as usize..(range.start + range.count) as usize]
            .iter()
            .map(|index| mesh.vertices[*index as usize])
            .collect::<Vec<_>>()
    };
    for (local, placed) in vertices_for_face(&local)
        .iter()
        .zip(vertices_for_face(&placed))
    {
        let expected = Vec3::new(
            300.0 + local.position[2],
            40.0 + local.position[1],
            120.0 - local.position[0],
        );
        assert!((placed.position - expected).length_squared() < 1.0e-8);
        assert!((placed.normal - Vec3::new(1.0, 0.0, 0.0)).length_squared() < 1.0e-8);
        assert_eq!(
            (placed.tex_s, placed.tex_t, placed.lightmap_uv),
            (local.tex_s, local.tex_t, local.lightmap_uv)
        );
    }
}

#[test]
fn door_meshes_retain_compiled_model_space_instead_of_subtracting_origin_twice() {
    let map = load_map(&door_bmodel_bsp_fixture(r#"{ "classname" "func_door_rotating" "model" "*1" "origin" "300 40 120" "angles" "0 35 0" }"#)).unwrap();
    let MapDoorGeometry::Brush { meshes, .. } = &map.doors[0].geometry else {
        panic!("brush door")
    };
    assert_eq!(meshes[0].vertices[0].position, Vec3::new(32.0, 0.0, 0.0));
    assert_eq!(map.doors[0].angles, Vec3::new(0.0, 35.0, 0.0));
}

pub(super) fn brush_collision_bsp(extra_entities: &str) -> MapBsp {
    let mut bsp = read_fixture_bsp(&door_bmodel_bsp_fixture(extra_entities));
    let mut leaf = bsp.leaves[0];
    leaf.first_leaf_brush = 0;
    leaf.leaf_brush_count = 1;
    bsp.leaves.push(leaf);
    bsp.models[1].head_node = -2;
    bsp.leaf_brushes = vec![0];
    for (normal, dist) in [
        ([1.0, 0.0, 0.0], 48.0),
        ([-1.0, 0.0, 0.0], -32.0),
        ([0.0, 1.0, 0.0], 40.0),
        ([0.0, -1.0, 0.0], 0.0),
        ([0.0, 0.0, 1.0], 80.0),
        ([0.0, 0.0, -1.0], 0.0),
    ] {
        bsp.brush_sides.push(BrushSide {
            plane: bsp.planes.len() as u16,
            texinfo: 0,
            displacement: -1,
            bevel: 0,
        });
        bsp.planes.push(Plane {
            normal,
            dist,
            axis_type: 0,
        });
    }
    bsp.brushes.push(Brush {
        first_side: 0,
        side_count: 6,
        contents: contents_flags::SOLID,
    });
    bsp
}

#[test]
fn brush_collision_follows_entity_ownership_translation_and_rotation() {
    let bsp = brush_collision_bsp(
        r#"{ "classname" "func_brush" "model" "*1" "origin" "300 40 120" "angles" "0 90 0" }"#,
    );
    let collision = MapWalkCollision::from_bsp_instances(
        &bsp,
        &brushes::brush_instances(&bsp),
        &BTreeSet::new(),
    )
    .unwrap();
    let hit = collision.trace_aabb(
        Vec3::new(240.0, 80.0, 160.0),
        Vec3::new(320.0, 80.0, 160.0),
        Vec3::splat(2.0),
    );
    assert!((hit.end_position[0] - 257.96875).abs() < 1.0e-4, "{hit:?}");
    assert!((hit.normal - Vec3::new(-1.0, 0.0, 0.0)).length_squared() < 1.0e-8);
    assert!(!collision.aabb_trace_solid(Vec3::new(40.0, 20.0, 40.0), Vec3::ZERO));
    // A hidden collision-only model has no render faces to infer its position from.
    let mut bsp = bsp;
    bsp.models[1].face_count = 0;
    let invisible = MapWalkCollision::from_bsp_instances(
        &bsp,
        &brushes::brush_instances(&bsp),
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(collision, invisible);
}

#[test]
fn shared_brush_models_keep_static_and_door_placements_separate() {
    let map = load_map(&door_bmodel_bsp_fixture(
        r#"
        { "classname" "func_brush" "model" "*1" "origin" "300 0 0" }
        { "classname" "func_door" "model" "*1" "origin" "100 0 0" }
        { "classname" "func_door_rotating" "model" "*1" "origin" "200 0 0" "angles" "0 35 0" }
    "#,
    ))
    .unwrap();
    assert_eq!(
        map.doors
            .iter()
            .map(|door| door.entity_index)
            .collect::<Vec<_>>(),
        [3, 4]
    );
    let mesh = map
        .meshes
        .iter()
        .find(|mesh| {
            mesh.surface_owners.iter().any(|surface| {
                matches!(
                    surface.owner,
                    MapSurfaceOwner::Brush {
                        entity_index: 2,
                        ..
                    }
                )
            })
        })
        .expect("static instance sharing a door model must remain visible");
    assert!(
        mesh.vertices
            .iter()
            .any(|vertex| vertex.position[0] == 332.0)
    );
    assert_eq!(
        map.meshes
            .iter()
            .map(|mesh| mesh.indices.len())
            .sum::<usize>(),
        9
    );
    for door in &map.doors {
        let MapDoorGeometry::Brush { meshes, .. } = &door.geometry else {
            panic!("brush door")
        };
        assert_eq!(
            meshes.iter().map(|mesh| mesh.indices.len()).sum::<usize>(),
            3
        );
    }
}

#[test]
fn shared_brush_model_collision_uses_each_entity_solidity_and_exclusion() {
    let bsp = brush_collision_bsp(
        r#"
        { "classname" "func_brush" "model" "*1" "origin" "0 0 0" "solidity" "1" }
        { "classname" "func_door" "model" "*1" "origin" "300 0 0" }
        { "classname" "func_door_rotating" "model" "*1" "origin" "600 0 0" "spawnflags" "8" }
        { "classname" "func_brush" "model" "*1" "origin" "900 0 0" }
    "#,
    );
    let instances = brushes::brush_instances(&bsp);
    let doors = pending_map_doors(&bsp);
    let excluded = doors
        .iter()
        .filter_map(PendingMapDoor::brush_entity_index)
        .collect();
    let collision = MapWalkCollision::from_bsp_instances(&bsp, &instances, &excluded).unwrap();
    for x in [40.0, 340.0, 640.0] {
        assert!(!collision.aabb_trace_solid(Vec3::new(x, 20.0, 40.0), Vec3::splat(1.0)));
    }
    assert!(collision.aabb_trace_solid(Vec3::new(940.0, 20.0, 40.0), Vec3::splat(1.0)));
    for (door, solid) in doors.iter().zip([true, false]) {
        let instance = *instances
            .iter()
            .find(|instance| instance.entity_index == Some(door.entity_index))
            .unwrap();
        let local = MapBrushCollision::from_bsp_instance(&bsp, instance);
        assert_eq!(
            local.aabb_trace_solid(Vec3::new(40.0, 20.0, 40.0), Vec3::splat(1.0)),
            solid
        );
    }
}

#[test]
fn brush_initial_solidity_and_visibility_follow_entity_semantics() {
    for (keys, visible, solid) in [
        (
            r#""classname" "func_brush" "StartDisabled" "0""#,
            true,
            true,
        ),
        (
            r#""classname" "func_brush" "StartDisabled" "1""#,
            false,
            false,
        ),
        (
            r#""classname" "func_brush" "StartDisabled" "1" "Solidity" "2""#,
            false,
            true,
        ),
        (r#""classname" "func_brush" "Solidity" "1""#, true, false),
        (r#""classname" "func_illusionary""#, true, false),
        (
            r#""classname" "func_wall_toggle" "spawnflags" "1""#,
            false,
            false,
        ),
        (r#""classname" "trigger_multiple""#, false, false),
        (r#""classname" "func_vehicleclip""#, false, false),
    ] {
        let entities = format!(r#"{{ {keys} "model" "*1" "origin" "0 0 0" }}"#);
        let bsp = brush_collision_bsp(&entities);
        let instances = brushes::brush_instances(&bsp);
        let instance = instances
            .iter()
            .find(|i| i.model_index == ModelIndex::new(1))
            .unwrap();
        assert_eq!(
            (instance.visible, instance.solid),
            (visible, solid),
            "{keys}"
        );
        let collision = MapWalkCollision::from_bsp_instances(&bsp, &instances, &BTreeSet::new());
        assert_eq!(collision.is_some(), solid, "{keys}");
        let map = load_map(&door_bmodel_bsp_fixture(&entities)).unwrap();
        assert_eq!(
            map.meshes
                .iter()
                .map(|mesh| mesh.indices.len())
                .sum::<usize>(),
            if visible { 9 } else { 6 },
            "{keys}"
        );
    }
}

#[test]
fn brush_visibility_uses_placed_bounds_and_preserves_multiple_clusters() {
    let mut bsp = read_fixture_bsp(&skybox_partition_bsp_fixture(true, true));
    let mut model = bsp.models[0];
    model.mins = [-4.0; 3];
    model.maxs = [4.0; 3];
    bsp.models.push(model);
    let sky = SkyboxPartition::from_bsp(
        &bsp,
        info_player_start(&bsp.entities).as_ref(),
        map_sky_camera(&bsp.entities),
    )
    .unwrap();
    let locator = MapLeafLocator::from_bsp(&bsp);
    let mut instance = brushes::BrushInstance {
        entity_index: Some(1),
        use_vphysics: false,
        model_index: ModelIndex::new(1),
        transform: brushes::BrushTransform {
            origin: Vec3::new(32.0, 0.0, 0.0),
            angles: Vec3::ZERO,
        },
        visible: true,
        solid: true,
    };
    let (partition, visibility) = instance.attribution(&bsp, &locator, &sky);
    assert_eq!(partition, GeometryPartition::Skybox);
    assert_eq!(visibility.clusters, vec![1]);
    instance.transform.origin = Vec3::new(-32.0, 0.0, 0.0);
    assert_eq!(
        instance.attribution(&bsp, &locator, &sky).0,
        GeometryPartition::Visible
    );
    instance.transform.origin = Vec3::ZERO;
    assert_eq!(
        instance.attribution(&bsp, &locator, &sky).0,
        GeometryPartition::Visible
    );
    assert_eq!(
        instance.attribution(&bsp, &locator, &sky).1.clusters,
        vec![0, 1]
    );
}

#[test]
fn native_brush_rays_and_player_hulls_use_their_distinct_compiled_shapes() {
    let make_box =
        |min, max| walk_brush_from_planes(box_planes(min, max), contents_flags::SOLID).unwrap();
    let collision = MapWalkCollision {
        physics_ray_brushes: vec![make_box(
            Vec3::new(10.0, -20.0, -20.0),
            Vec3::new(20.0, 20.0, 20.0),
        )],
        physics_brushes: vec![make_box(
            Vec3::new(10.5, -19.5, -19.5),
            Vec3::new(19.5, 19.5, 19.5),
        )],
        ..MapWalkCollision::empty()
    };
    let ray = collision.trace_aabb(Vec3::ZERO, Vec3::new(30.0, 0.0, 0.0), Vec3::ZERO);
    let hull = collision.trace_aabb(Vec3::ZERO, Vec3::new(30.0, 0.0, 0.0), Vec3::splat(1.0));
    assert!((ray.end_position[0] - 9.96875).abs() < 1.0e-5, "{ray:?}");
    assert!((hull.end_position[0] - 9.46875).abs() < 1.0e-5, "{hull:?}");
}

#[test]
fn legacy_wall_spawn_clears_angles_and_rotating_door_passable_flag_removes_collision() {
    for class in [
        "func_illusionary",
        "func_wall",
        "func_wall_toggle",
        "func_conveyor",
    ] {
        let bsp = brush_collision_bsp(&format!(
            r#"{{ "classname" "{class}" "model" "*1" "angles" "20 35 15" "origin" "100 200 300" }}"#
        ));
        let instances = brushes::brush_instances(&bsp);
        let instance = instances
            .iter()
            .find(|i| i.model_index == ModelIndex::new(1))
            .unwrap();
        assert_eq!(instance.transform.angles, Vec3::ZERO, "{class}");
        assert_eq!(instance.transform.origin, Vec3::new(100.0, 200.0, 300.0));
    }
    let bsp = brush_collision_bsp(
        r#"{ "classname" "func_door_rotating" "model" "*1" "spawnflags" "8" }"#,
    );
    let instance = brushes::brush_instances(&bsp)
        .into_iter()
        .find(|instance| instance.model_index == ModelIndex::new(1))
        .unwrap();
    let collision = MapBrushCollision::from_bsp_instance(&bsp, instance);
    assert!(!collision.aabb_trace_solid(Vec3::new(40.0, 20.0, 40.0), Vec3::splat(1.0)));
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
    let map = load_map(&skybox_completion_bsp_fixture(Vec3::new(-32.0, 0.0, 0.0)))
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
    let map = load_map(&skybox_completion_bsp_fixture(Vec3::new(-32.0, 256.0, 0.0)))
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
    assert_eq!(visibility.cluster_at(Vec3::new(-32.0, 0.0, 0.0)), Some(0));
    assert_eq!(visibility.cluster_at(Vec3::new(32.0, 0.0, 0.0)), Some(1));
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
        visibility.clusters_for_aabb(Vec3::new(-48.0, -8.0, -8.0), Vec3::new(-16.0, 8.0, 8.0)),
        MapPropVisibility::Clusters(vec![0])
    );
    assert_eq!(
        visibility.clusters_for_aabb(Vec3::new(-8.0, -8.0, -8.0), Vec3::new(8.0, 8.0, 8.0)),
        MapPropVisibility::Clusters(vec![0, 1])
    );
    assert_eq!(
        visibility.clusters_for_aabb(Vec3::ZERO, Vec3::ZERO),
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
    let visibility = map.visibility.as_ref().expect("BSP traversal without PVS");
    assert_eq!(visibility.cluster_count(), 0);
    assert!(visibility.visible_clusters(0).is_none());
    assert!(visibility.cluster_at(Vec3::ZERO).is_none());
    assert_eq!(
        visibility.ordered_faces(Vec3::new(-32.0, 0.0, 8.0), |_| true),
        [0, 1]
    );
    assert_eq!(
        visibility.ordered_faces(Vec3::new(32.0, 0.0, 8.0), |_| true),
        [1, 0]
    );
    assert_eq!(map, unpartitioned);
}

#[test]
fn load_map_retains_water_undersides_and_already_oriented_plane_normals() {
    let mut bsp_bytes = water_underside_bsp_fixture();
    let entry = 8 + FACES_LUMP_INDEX * 16;
    let offset = i32::from_le_bytes(bsp_bytes[entry..entry + 4].try_into().unwrap()) as usize;
    // Source repeats the oriented plane's sign bit in side. Do not flip twice.
    bsp_bytes[offset + 56 + 2] = 1;
    let map = load_map(&bsp_bytes).expect("fixture BSP should load");
    assert_eq!(map.stats.face_count, 3);
    assert_eq!(map.material_names, ["water/down", "water/up", "brick/down"]);
    assert_eq!(map.meshes.len(), 3);
    assert!(map.meshes[0].vertices.iter().all(|v| v.normal[2] < 0.0));
    assert!(map.meshes[1].vertices.iter().all(|v| v.normal[2] > 0.0));
    assert!(map.meshes[2].vertices.iter().all(|v| v.normal[2] < 0.0));
    assert!(
        map.meshes
            .iter()
            .flat_map(|m| &m.vertices)
            .all(|v| v.texture_size == [512, 512])
    );
}

#[test]
fn unsupported_versions_are_reported_before_decode() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"VBSP");
    bytes.extend_from_slice(&21_u32.to_le_bytes());

    assert!(matches!(
        load_map(&bytes),
        Err(BspError::UnsupportedVersion { version: 21 })
    ));
}

fn block(width: usize, height: usize) -> PendingLightmapBlock {
    let face_width = width.max(3) - 2;
    let face_height = height.max(3) - 2;
    PendingLightmapBlock {
        sample_start: 0,
        face_width,
        face_height,
        styles: vec![0],
        samples: vec![
            ColorRgbExp {
                r: 255,
                g: 255,
                b: 255,
                exponent: 0
            };
            face_width * face_height
        ],
        sample_basis: lighting::LightmapBasis::Scalar,
        output_basis: lighting::LightmapBasis::Scalar,
    }
}

fn box_collision(min: Vec3, max: Vec3) -> MapWalkCollision {
    MapWalkCollision::solid_box_for_tests(min, max)
}

fn empty_walk_collision() -> MapWalkCollision {
    MapWalkCollision {
        brushes: Vec::new(),
        water_brushes: Vec::new(),
        displacements: Vec::new(),
        props: MapWalkPropCollision::default(),
        physics_brushes: Vec::new(),
        physics_ray_brushes: Vec::new(),
    }
}

fn box_ledge(min: Vec3, max: Vec3) -> ConvexHull {
    ConvexHull {
        vertices: vec![
            Vec3::new(min[0], min[1], min[2]),
            Vec3::new(max[0], min[1], min[2]),
            Vec3::new(min[0], max[1], min[2]),
            Vec3::new(max[0], max[1], min[2]),
            Vec3::new(min[0], min[1], max[2]),
            Vec3::new(max[0], min[1], max[2]),
            Vec3::new(min[0], max[1], max[2]),
            Vec3::new(max[0], max[1], max[2]),
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
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(64.0, 0.0, 0.0),
            Vec3::new(0.0, 64.0, 0.0),
            Vec3::new(0.0, 0.0, 64.0),
            Vec3::new(64.0, 0.0, 64.0),
            Vec3::new(0.0, 64.0, 64.0),
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

fn box_planes(min: Vec3, max: Vec3) -> Vec<MapPlane> {
    vec![
        MapPlane {
            normal: Vec3::new(1.0, 0.0, 0.0),
            dist: max[0],
        },
        MapPlane {
            normal: Vec3::new(-1.0, 0.0, 0.0),
            dist: -min[0],
        },
        MapPlane {
            normal: Vec3::new(0.0, 1.0, 0.0),
            dist: max[1],
        },
        MapPlane {
            normal: Vec3::new(0.0, -1.0, 0.0),
            dist: -min[1],
        },
        MapPlane {
            normal: Vec3::new(0.0, 0.0, 1.0),
            dist: max[2],
        },
        MapPlane {
            normal: Vec3::new(0.0, 0.0, -1.0),
            dist: -min[2],
        },
    ]
}

fn box_brush_with_sky_side(min: Vec3, max: Vec3, sky_side_index: usize) -> MapWalkBrush {
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

fn assert_vec3_close(actual: Vec3, expected: Vec3) {
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
    let mut bytes = bsp_fixture_header();

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
    push_plane(&mut planes, Vec3::new(1.0, 0.0, 0.0));
    push_plane(&mut planes, Vec3::new(0.0, 0.0, 1.0));
    append_lump(&mut bytes, PLANES_LUMP_INDEX, &planes, 0);

    let mut texture_data = Vec::new();
    push_texture_data(&mut texture_data, 0);
    push_texture_data(&mut texture_data, 1);
    append_lump(&mut bytes, TEXTURE_DATA_LUMP_INDEX, &texture_data, 0);

    let mut vertices = Vec::new();
    for position in [
        Vec3::new(-64.0, -32.0, 0.0),
        Vec3::new(-16.0, -32.0, 0.0),
        Vec3::new(-64.0, 32.0, 0.0),
        Vec3::new(16.0, -32.0, 0.0),
        Vec3::new(64.0, -32.0, 0.0),
        Vec3::new(16.0, 32.0, 0.0),
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
    push_vec3(&mut models, Vec3::new(-64.0, -32.0, 0.0));
    push_vec3(&mut models, Vec3::new(64.0, 32.0, 0.0));
    push_vec3(&mut models, Vec3::new(0.0, 0.0, 0.0));
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&2_i32.to_le_bytes());
    append_lump(&mut bytes, MODELS_LUMP_INDEX, &models, 0);

    let mut leaf_faces = Vec::new();
    leaf_faces.extend_from_slice(&0_u16.to_le_bytes());
    leaf_faces.extend_from_slice(&1_u16.to_le_bytes());
    append_lump(&mut bytes, LEAF_FACES_LUMP_INDEX, &leaf_faces, 0);

    let prop_data = static_prop_game_lump_data();
    append_sprp_game_lump(&mut bytes, &prop_data);

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
    let mut bytes = bsp_fixture_header();

    append_lump(
        &mut bytes,
        ENTITIES_LUMP_INDEX,
        br#"{ "classname" "worldspawn" }"#,
        0,
    );

    let mut planes = Vec::new();
    push_plane(&mut planes, Vec3::new(1.0, 0.0, 0.0));
    push_plane(&mut planes, Vec3::new(0.0, 0.0, 1.0));
    append_lump(&mut bytes, PLANES_LUMP_INDEX, &planes, 0);

    let mut texture_data = Vec::new();
    push_texture_data(&mut texture_data, 0);
    push_texture_data(&mut texture_data, 1);
    append_lump(&mut bytes, TEXTURE_DATA_LUMP_INDEX, &texture_data, 0);
    append_lump(&mut bytes, VISIBILITY_LUMP_INDEX, &partition_vis_lump(), 0);

    let mut vertices = Vec::new();
    for position in [
        Vec3::new(-64.0, -32.0, 0.0),
        Vec3::new(-16.0, -32.0, 0.0),
        Vec3::new(-64.0, 32.0, 0.0),
        Vec3::new(16.0, -32.0, 0.0),
        Vec3::new(64.0, -32.0, 0.0),
        Vec3::new(16.0, 32.0, 0.0),
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
    push_vec3(&mut models, Vec3::new(-64.0, -32.0, 0.0));
    push_vec3(&mut models, Vec3::new(64.0, 32.0, 0.0));
    push_vec3(&mut models, Vec3::new(0.0, 0.0, 0.0));
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&2_i32.to_le_bytes());
    append_lump(&mut bytes, MODELS_LUMP_INDEX, &models, 0);

    let mut leaf_faces = Vec::new();
    leaf_faces.extend_from_slice(&0_u16.to_le_bytes());
    leaf_faces.extend_from_slice(&1_u16.to_le_bytes());
    append_lump(&mut bytes, LEAF_FACES_LUMP_INDEX, &leaf_faces, 0);

    let prop_data = static_prop_game_lump_data_with(Vec3::new(10.0, 20.0, 30.0), &[1, 2]);
    append_sprp_game_lump(&mut bytes, &prop_data);

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

fn skybox_completion_bsp_fixture(spawn_origin: Vec3) -> Vec<u8> {
    let mut bytes = bsp_fixture_header();

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
    push_plane(&mut planes, Vec3::new(1.0, 0.0, 0.0));
    push_plane(&mut planes, Vec3::new(0.0, 0.0, 1.0));
    append_lump(&mut bytes, PLANES_LUMP_INDEX, &planes, 0);

    let mut texture_data = Vec::new();
    for index in 0..3 {
        push_texture_data(&mut texture_data, index);
    }
    append_lump(&mut bytes, TEXTURE_DATA_LUMP_INDEX, &texture_data, 0);

    let mut vertices = Vec::new();
    for position in [
        Vec3::new(-144.0, -32.0, 0.0),
        Vec3::new(-96.0, -32.0, 0.0),
        Vec3::new(-144.0, 32.0, 0.0),
        Vec3::new(-48.0, 240.0, 0.0),
        Vec3::new(-16.0, 240.0, 0.0),
        Vec3::new(-48.0, 280.0, 0.0),
        Vec3::new(-44.0, 248.0, 0.0),
        Vec3::new(-20.0, 248.0, 0.0),
        Vec3::new(-44.0, 272.0, 0.0),
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
    push_vec3(&mut models, Vec3::new(-160.0, -64.0, -128.0));
    push_vec3(&mut models, Vec3::new(96.0, 384.0, 128.0));
    push_vec3(&mut models, Vec3::new(0.0, 0.0, 0.0));
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
    append_sprp_game_lump(&mut bytes, &prop_data);

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
    let bounds = bounds_from_meshes(&map.meshes);
    map.bounds_min = bounds.min;
    map.bounds_max = bounds.max;
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

pub(super) fn bsp_fixture_with_entities(entities: &str) -> Vec<u8> {
    bsp_fixture(Some(entities))
}

pub(super) fn parse_test_entities(entities: &str) -> Vec<MapEntity> {
    read_fixture_bsp(&bsp_fixture_with_entities(entities)).entities
}

fn pending_doors_from_fixture(entities: &str) -> Vec<PendingMapDoor> {
    let bytes = door_bmodel_bsp_fixture(entities);
    let bsp = read_fixture_bsp(&bytes);
    pending_map_doors(&bsp)
}

fn read_fixture_bsp(bytes: &[u8]) -> MapBsp {
    MapBsp::parse(bytes, &Limits::default(), None).expect("fixture bsp should parse")
}

pub(super) fn door_bmodel_bsp_fixture(extra_entities: &str) -> Vec<u8> {
    let mut bytes = bsp_fixture_header();

    let entities = format!(
        r#"
        {{ "classname" "worldspawn" }}
        {{ "classname" "func_brush" "model" "*2" "origin" "0 0 0" }}
        {extra_entities}
        "#
    );
    append_lump(&mut bytes, ENTITIES_LUMP_INDEX, entities.as_bytes(), 0);

    let mut planes = Vec::new();
    push_plane(&mut planes, Vec3::new(0.0, 0.0, 1.0));
    append_lump(&mut bytes, PLANES_LUMP_INDEX, &planes, 0);

    let mut texture_data = Vec::new();
    push_texture_data(&mut texture_data, 0);
    append_lump(&mut bytes, TEXTURE_DATA_LUMP_INDEX, &texture_data, 0);

    let mut vertices = Vec::new();
    for position in [
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(16.0, 0.0, 0.0),
        Vec3::new(0.0, 16.0, 0.0),
        Vec3::new(32.0, 0.0, 0.0),
        Vec3::new(48.0, 0.0, 0.0),
        Vec3::new(32.0, 16.0, 0.0),
        Vec3::new(64.0, 0.0, 0.0),
        Vec3::new(80.0, 0.0, 0.0),
        Vec3::new(64.0, 16.0, 0.0),
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
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(16.0, 16.0, 0.0),
        Vec3::ZERO,
        0,
        0,
        1,
    );
    push_model(
        &mut models,
        Vec3::new(32.0, 0.0, 0.0),
        Vec3::new(48.0, 40.0, 80.0),
        Vec3::new(32.0, 0.0, 0.0),
        0,
        1,
        1,
    );
    push_model(
        &mut models,
        Vec3::new(64.0, 0.0, 0.0),
        Vec3::new(80.0, 16.0, 0.0),
        Vec3::new(64.0, 0.0, 0.0),
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
    append_sprp_game_lump(&mut bytes, &prop_data);

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
    const SURF_WARP: u32 = 0x8;

    let mut bytes = bsp_fixture_header();

    let mut planes = Vec::new();
    push_plane(&mut planes, Vec3::new(0.0, 0.0, -1.0));
    push_plane(&mut planes, Vec3::new(0.0, 0.0, 1.0));
    append_lump(&mut bytes, PLANES_LUMP_INDEX, &planes, 0);

    let mut texture_data = Vec::new();
    for index in 0..3 {
        push_texture_data(&mut texture_data, index);
    }
    append_lump(&mut bytes, TEXTURE_DATA_LUMP_INDEX, &texture_data, 0);

    let mut vertices = Vec::new();
    for position in [
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(64.0, 0.0, 0.0),
        Vec3::new(0.0, 64.0, 0.0),
        Vec3::new(0.0, 0.0, 16.0),
        Vec3::new(64.0, 0.0, 16.0),
        Vec3::new(0.0, 64.0, 16.0),
        Vec3::new(0.0, 0.0, 32.0),
        Vec3::new(64.0, 0.0, 32.0),
        Vec3::new(0.0, 64.0, 32.0),
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
    push_vec3(&mut models, Vec3::new(0.0, 0.0, 0.0));
    push_vec3(&mut models, Vec3::new(64.0, 64.0, 32.0));
    push_vec3(&mut models, Vec3::new(0.0, 0.0, 0.0));
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&0_i32.to_le_bytes());
    models.extend_from_slice(&3_i32.to_le_bytes());
    append_lump(&mut bytes, MODELS_LUMP_INDEX, &models, 0);

    let prop_data = empty_static_prop_game_lump_data();
    append_sprp_game_lump(&mut bytes, &prop_data);

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
    let mut bytes = bsp_fixture_header();

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

    let mut plane = Vec::new();
    push_plane(&mut plane, Vec3::new(0.0, 0.0, 1.0));
    append_lump(&mut bytes, PLANES_LUMP_INDEX, &plane, 0);

    let mut node = Vec::new();
    node.extend_from_slice(&0_i32.to_le_bytes());
    node.extend_from_slice(&(-1_i32).to_le_bytes());
    node.extend_from_slice(&(-1_i32).to_le_bytes());
    node.extend_from_slice(&[0_u8; 20]);
    append_lump(&mut bytes, NODES_LUMP_INDEX, &node, 0);

    append_lump(&mut bytes, LEAVES_LUMP_INDEX, &[0_u8; 56], 0);

    let prop_data = static_prop_game_lump_data();
    append_sprp_game_lump(&mut bytes, &prop_data);

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

fn append_sprp_game_lump(bytes: &mut Vec<u8>, prop_data: &[u8]) {
    const GAME_LUMP_HEADER_LEN: usize = 20;
    const SPRP_VERSION: u16 = 10;

    let game_lump_offset = bytes.len();
    let prop_offset = game_lump_offset + GAME_LUMP_HEADER_LEN;
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    bytes.extend_from_slice(&i32::from_be_bytes(*b"sprp").to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&SPRP_VERSION.to_le_bytes());
    bytes.extend_from_slice(&(prop_offset as i32).to_le_bytes());
    bytes.extend_from_slice(&(prop_data.len() as i32).to_le_bytes());
    bytes.extend_from_slice(prop_data);
    write_lump_entry(
        bytes,
        GAME_LUMP_INDEX,
        game_lump_offset,
        GAME_LUMP_HEADER_LEN,
        0,
    );
}

fn append_lump(bytes: &mut Vec<u8>, lump_index: usize, data: &[u8], version: u32) {
    let offset = bytes.len();
    bytes.extend_from_slice(data);
    write_lump_entry(bytes, lump_index, offset, data.len(), version);
}

fn push_plane(bytes: &mut Vec<u8>, normal: Vec3) {
    push_vec3(bytes, normal);
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    bytes.extend_from_slice(&0_i32.to_le_bytes());
}

fn push_texture_data(bytes: &mut Vec<u8>, name_string_table_id: i32) {
    push_vec3(bytes, Vec3::new(1.0, 1.0, 1.0));
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
    mins: Vec3,
    maxs: Vec3,
    origin: Vec3,
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
    static_prop_game_lump_data_with(Vec3::new(10.0, 20.0, 30.0), &[0, 1])
}

fn static_prop_game_lump_data_with(origin: Vec3, leaves: &[u16]) -> Vec<u8> {
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
    push_vec3(&mut bytes, Vec3::new(1.0, 90.0, 3.0));
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&(leaves.len() as u16).to_le_bytes());
    bytes.push(0);
    bytes.push(0);
    bytes.extend_from_slice(&2_i32.to_le_bytes());
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    bytes.extend_from_slice(&0.0_f32.to_le_bytes());
    push_vec3(&mut bytes, Vec3::splat(0.0));
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

fn push_vec3(bytes: &mut Vec<u8>, values: Vec3) {
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

const HEADER_LEN: usize = 8;
const LUMP_COUNT: usize = 64;
const LUMP_ENTRY_LEN: usize = 16;
const GAME_LUMP_INDEX: usize = 35;
const EDGES_LUMP_INDEX: usize = 12;
const FACES_LUMP_INDEX: usize = 7;
const NODES_LUMP_INDEX: usize = 5;
const LEAVES_LUMP_INDEX: usize = 10;
const MODELS_LUMP_INDEX: usize = 14;
const PLANES_LUMP_INDEX: usize = 1;
const PAKFILE_LUMP_INDEX: usize = 40;
const ENTITIES_LUMP_INDEX: usize = 0;
const VERTICES_LUMP_INDEX: usize = 3;
const LEAF_FACES_LUMP_INDEX: usize = 16;
const VISIBILITY_LUMP_INDEX: usize = 4;
const TEXTURE_DATA_LUMP_INDEX: usize = 2;
const TEXTURE_INFO_LUMP_INDEX: usize = 6;
const SURFACE_EDGES_LUMP_INDEX: usize = 13;
const TEXTURE_STRING_DATA_LUMP_INDEX: usize = 43;
const TEXTURE_STRING_TABLE_LUMP_INDEX: usize = 44;

fn bsp_fixture_header() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"VBSP");
    bytes.extend_from_slice(&20_u32.to_le_bytes());
    bytes.resize(HEADER_LEN + LUMP_COUNT * LUMP_ENTRY_LEN, 0);
    bytes
}

fn write_lump_entry(
    bytes: &mut [u8],
    lump_index: usize,
    offset: usize,
    length: usize,
    version: u32,
) {
    let start = HEADER_LEN + lump_index * LUMP_ENTRY_LEN;
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
                normal: Vec3::new(0.0, 0.0, 1.0),
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

#[test]
fn a_cyclic_node_graph_terminates_instead_of_spinning() {
    use std::sync::mpsc;
    use std::time::Duration;

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // 0 -> 1 -> 0, both children non-negative so the walk never reaches
        // the `next < 0` leaf exit.
        let result = super::walk_to_leaf(
            Vec3::new(1.0, 0.0, 0.0),
            2,
            |index| match index {
                0 => Some((0, [1, 1])),
                1 => Some((0, [0, 0])),
                _ => None,
            },
            |_| Some((Vec3::new(1.0, 0.0, 0.0), 0.0)),
        );
        let _ = tx.send(result);
    });

    let result = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("a cyclic node graph must not loop forever");
    assert_eq!(result, None, "a cycle has no reachable leaf");
}

#[test]
fn an_acyclic_walk_still_reaches_its_leaf_at_full_depth() {
    const DEPTH: usize = 64;

    // A degenerate maximally deep tree: every node's front child is the next
    // node, and the last one points at leaf 0 (encoded as !0 == -1). The
    // iteration bound must not cut this short.

    let leaf = super::walk_to_leaf(
        Vec3::new(1.0, 0.0, 0.0),
        DEPTH,
        |index| {
            (index < DEPTH).then(|| {
                let front = if index + 1 < DEPTH {
                    (index + 1) as i32
                } else {
                    -1
                };
                (0, [front, front])
            })
        },
        |_| Some((Vec3::new(1.0, 0.0, 0.0), 0.0)),
    );

    assert_eq!(leaf, Some(0));
}

#[test]
fn a_straddling_aabb_visits_a_shared_child_once() {
    use super::walk::PlaneSide;

    let visit = |side: PlaneSide, children| side.children(children).collect::<Vec<_>>();

    assert_eq!(visit(PlaneSide::Front, [7, 9]), vec![7]);
    assert_eq!(visit(PlaneSide::Back, [7, 9]), vec![9]);
    assert_eq!(visit(PlaneSide::Both, [7, 9]), vec![7, 9]);
    // Both sides of a plane can name the same subtree; descending it twice
    // doubles the work at every level below.
    assert_eq!(visit(PlaneSide::Both, [7, 7]), vec![7]);
}

#[test]
fn node_children_decode_by_sign() {
    use super::NodeChild;

    assert_eq!(NodeChild::decode(0), Some(NodeChild::Node(0)));
    assert_eq!(NodeChild::decode(12), Some(NodeChild::Node(12)));
    assert_eq!(NodeChild::decode(-1), Some(NodeChild::Leaf(0)));
    assert_eq!(
        NodeChild::decode(i32::MIN),
        Some(NodeChild::Leaf(i32::MAX as usize))
    );
}

#[test]
fn all_non_finite_vertices_degrade_to_a_zero_box_not_nan() {
    let vertex = |position| MapVertex {
        texture_size: [0; 2],
        position,
        base_position: position,
        tangent_s: Vec3::ZERO,
        tangent_t: Vec3::ZERO,
        normal: Vec3::new(0.0, 0.0, 1.0),
        tex_s: 0.0,
        tex_t: 0.0,
        lightmap_uv: [0.0; 2],
        lightmap_page: None,
        lightmap_bump_step: 0.0,
        blend_alpha: 0.0,
        opacity: 1.0,
    };
    let mesh = MapMesh {
        entity: false,
        vertices: vec![
            vertex(Vec3::splat(f32::NAN)),
            vertex(Vec3::splat(f32::INFINITY)),
        ],
        indices: Vec::new(),
        material_index: 0,
        visibility: MapMeshVisibility::default(),
        surface_owners: Vec::new(),
    };

    let bounds = bounds_from_meshes(std::slice::from_ref(&mesh));

    assert_eq!(bounds.min, Vec3::splat(0.0));
    assert_eq!(bounds.max, Vec3::splat(0.0));
}

#[test]
fn a_face_in_no_cluster_finishes_as_always_visible() {
    use super::MapVisibilityBucket;
    use super::visibility::MapFaceVisibilityBuilder;

    let no_buckets = MapFaceVisibilityBuilder::default().finish();
    assert!(no_buckets.always_visible);
    assert!(no_buckets.clusters.is_empty());

    let mut clustered = MapFaceVisibilityBuilder::default();
    clustered.push(MapVisibilityBucket::Cluster(3));
    clustered.push(MapVisibilityBucket::Cluster(1));
    let clustered = clustered.finish();
    assert!(!clustered.always_visible);
    assert_eq!(clustered.clusters, vec![1, 3]);
}

#[test]
fn compiled_water_lookup_distinguishes_stacked_liquids_and_air_gap() {
    let mut bsp = read_fixture_bsp(&water_underside_bsp_fixture());
    let plane = bsp.planes[0];
    bsp.planes = [100.0, 0.0, -32.0, -100.0]
        .into_iter()
        .map(|dist| {
            let mut p = plane;
            p.normal = [0.0, 0.0, 1.0];
            p.dist = dist;
            p
        })
        .collect();
    let node = bsp.nodes[0];
    bsp.nodes = [(0, [-1, 1]), (1, [-2, 2]), (2, [-1, 3]), (3, [-3, -1])]
        .into_iter()
        .map(|(plane, children)| {
            let mut n = node;
            n.plane = plane;
            n.children = children;
            n
        })
        .collect();
    let leaf = bsp.leaves[0];
    bsp.leaves = [
        (-1, 0),
        (0, contents_flags::WATER),
        (1, contents_flags::SLIME),
    ]
    .into_iter()
    .map(|(index, contents)| {
        let mut l = leaf;
        l.leaf_water_data = index;
        l.contents = contents;
        l
    })
    .collect();
    bsp.leaf_water_data = vec![
        bsp::LeafWaterData {
            surface_z: 100.0,
            min_z: 0.0,
            surface_texinfo: 0,
        },
        bsp::LeafWaterData {
            surface_z: -32.0,
            min_z: -100.0,
            surface_texinfo: 1,
        },
    ];
    let volumes = MapWaterVolumes::from_bsp(&bsp).unwrap();
    let upper = volumes.at(Vec3::new(0.0, 0.0, 50.0)).unwrap();
    let lower = volumes.at(Vec3::new(0.0, 0.0, -64.0)).unwrap();
    assert_eq!(upper.body.material_name, "water/down");
    assert_eq!(upper.body.surface_z, 100.0);
    assert_eq!(lower.body.material_name, "water/up");
    assert_eq!(lower.body.surface_z, -32.0);
    assert_eq!(lower.contents, contents_flags::SLIME);
    for z in [200.0, -10.0, -120.0] {
        assert!(volumes.at(Vec3::new(0.0, 0.0, z)).is_none());
    }
    assert!(volumes.at(Vec3::splat(f32::NAN)).is_none());
    bsp.leaf_water_data[0].surface_texinfo = -1;
    let volumes = MapWaterVolumes::from_bsp(&bsp).unwrap();
    assert!(volumes.at(Vec3::new(0.0, 0.0, 50.0)).is_none());
    assert_eq!(
        volumes.at(Vec3::new(0.0, 0.0, -64.0)).unwrap().body_index,
        1
    );
}

#[test]
fn baked_lightstyle_entity_start_state_follows_named_light_precedence() {
    let entities = parse_test_entities(
        r#"
        { "classname" "light" "style" "32" "targetname" "off" "spawnflags" "1" "pattern" "z" }
        { "classname" "light_spot" "style" "33" "targetname" "pattern" "pattern" "az" "defaultstyle" "4" }
        { "classname" "light" "style" "34" "targetname" "preset" "defaultstyle" "4" }
        { "classname" "light" "style" "35" "pattern" "a" }
    "#,
    );
    let atlas = lighting::bake_lightmap_set(
        std::iter::empty(),
        vec![block(4, 4)],
        Some(LightmapSource::Ldr),
        &entities,
    )
    .unwrap();
    let first = atlas.style_values(0.0);
    let second = atlas.style_values(0.1);
    assert_eq!(first[0], 1.0);
    assert_eq!(first[32], 0.0);
    assert_eq!(first[33], 0.0);
    assert!(second[33] > 1.0);
    assert_eq!(first[34], 1.0);
    assert_eq!(second[34], 0.0);
    assert_eq!(first[35], 1.0);
    assert_eq!(first[63], 0.0);
}

#[test]
fn hdr_lightmaps_require_and_select_matching_hdr_face_records() {
    let mut bytes = bsp_fixture(None);
    let mut ldr_faces = Vec::new();
    push_face(&mut ldr_faces, 0, 0, 0, 7);
    append_lump(&mut bytes, FACES_LUMP_INDEX, &ldr_faces, 0);
    append_lump(&mut bytes, 53, &[255, 128, 0, 0], 0);
    let missing = read_fixture_bsp(&bytes);
    assert!(missing.lighting_hdr.is_empty());
    assert_eq!(missing.faces[0].original_face, 7);
    let mut hdr_faces = Vec::new();
    push_face(&mut hdr_faces, 0, 0, 0, 9);
    append_lump(&mut bytes, 58, &hdr_faces, 0);
    let hdr = read_fixture_bsp(&bytes);
    assert_eq!(hdr.faces[0].original_face, 9);
    assert_eq!(hdr.lighting_hdr.len(), 1);
    append_lump(&mut bytes, 8, &[128, 255, 0, 0], 0);
    let ldr = read_fixture_bsp(&bytes);
    assert_eq!(ldr.faces[0].original_face, 7);
    assert_eq!(ldr.lighting.len(), 1);
    assert!(ldr.lighting_hdr.is_empty());
}

#[test]
fn radial_fog_is_preserved_for_world_and_sky_camera() {
    let map=load_map(&bsp_fixture_with_entities(r#"
        { "classname" "env_fog_controller" "fogenable" "1" "fogradial" "1" "fogcolor" "24 48 72" "fogstart" "10" "fogend" "200" }
        { "classname" "sky_camera" "origin" "0 0 0" "fogenable" "1" "fogradial" "1" "fogcolor" "24 48 72" "fogstart" "10" "fogend" "200" }
    "#)).unwrap();
    assert!(map.fog.unwrap().radial);
    assert!(map.sky_camera.unwrap().fog.unwrap().radial);
}

pub(super) fn lightmap_stats_fixture(
    offset: i32,
    size: [i32; 2],
    styles: [u8; 4],
    sample_count: usize,
) -> Vec<u8> {
    let mut bytes = door_bmodel_bsp_fixture("");
    let lump = HEADER_LEN + FACES_LUMP_INDEX * LUMP_ENTRY_LEN;
    let face = u32::from_le_bytes(bytes[lump..lump + 4].try_into().unwrap()) as usize;
    bytes[face + 16..face + 20].copy_from_slice(&styles);
    bytes[face + 20..face + 24].copy_from_slice(&offset.to_le_bytes());
    bytes[face + 36..face + 40].copy_from_slice(&size[0].to_le_bytes());
    bytes[face + 40..face + 44].copy_from_slice(&size[1].to_le_bytes());
    if sample_count > 0 {
        append_lump(&mut bytes, 8, &[128, 128, 128, 0].repeat(sample_count), 0);
    }
    bytes
}

#[test]
fn lightmap_stats_distinguish_valid_missing_and_absent_lighting_channels() {
    for (offset, samples, valid, missing) in [(0, 1, 1, 1), (-1, 1, 0, 2), (0, 0, 0, 2)] {
        let map = load_map(&lightmap_stats_fixture(
            offset,
            [0, 0],
            [0, 255, 255, 255],
            samples,
        ))
        .unwrap();
        assert_eq!(
            map.stats.lightmap,
            MapLightmapStats {
                unlightmapped_surfaces: 0,
                valid_payloads: valid,
                missing_payloads: missing,
                invalid_payloads: 0,
                atlas_dropped_blocks: 0,
            }
        );
        assert_eq!(map.lightmap.is_some(), valid > 0);
    }
}

#[test]
fn lightmap_stats_report_corrupt_offsets_extents_and_style_bundles() {
    for (offset, size, styles) in [
        (-2, [0, 0], [0, 255, 255, 255]),
        (1, [0, 0], [0, 255, 255, 255]),
        (4, [0, 0], [0, 255, 255, 255]),
        (0, [-1, 0], [0, 255, 255, 255]),
        (0, [0, 0], [255; 4]),
        (0, [0, 0], [0, 32, 255, 255]),
    ] {
        let map = load_map(&lightmap_stats_fixture(offset, size, styles, 1)).unwrap();
        assert_eq!(
            map.stats.lightmap,
            MapLightmapStats {
                unlightmapped_surfaces: 0,
                valid_payloads: 0,
                missing_payloads: 1,
                invalid_payloads: 1,
                atlas_dropped_blocks: 0,
            },
            "offset={offset}, size={size:?}, styles={styles:?}"
        );
        assert!(map.lightmap.is_none());
        assert!(
            map.meshes
                .iter()
                .flat_map(|mesh| &mesh.vertices)
                .all(|vertex| vertex.lightmap_page.is_none())
        );
    }
}

#[test]
fn lightmap_storage_preserves_oversized_valid_payloads_and_rejects_truncation() {
    for (sample_count, valid, invalid) in [(4095, 1, 0), (4094, 0, 1)] {
        let map = load_map(&lightmap_stats_fixture(
            0,
            [4094, 0],
            [0, 255, 255, 255],
            sample_count,
        ))
        .unwrap();
        assert_eq!(map.stats.lightmap.valid_payloads, valid);
        assert_eq!(map.stats.lightmap.invalid_payloads, invalid);
        assert_eq!(map.stats.lightmap.missing_payloads, 1);
        assert_eq!(map.stats.lightmap.atlas_dropped_blocks, 0);
        assert_eq!(map.lightmap.is_some(), valid != 0);
        if let Some(lightmap) = &map.lightmap {
            assert!(!lightmap.pages().is_empty());
            assert!(
                lightmap
                    .pages()
                    .iter()
                    .all(|page| page.width <= 1024 && page.height <= 1024)
            );
        }
    }
}
