use super::super::{MapMeshIndexRange, MapMeshSurfaceOwner, MapMeshVisibility, MapVertex};
use super::*;

fn plane(normal: Vec3, distance: f32) -> MapMesh {
    let [u, v] = decal_axes(normal).unwrap();
    let origin = normal * distance;
    let vertices = [[-16.0, -16.0], [-16.0, 16.0], [16.0, 16.0], [16.0, -16.0]]
        .map(|[x, y]| MapVertex {
            position: origin + u * x + v * y,
            base_position: origin + u * x + v * y,
            normal,
            tangent_s: u,
            tangent_t: v,
            tex_s: x,
            tex_t: y,
            texture_size: [64; 2],
            lightmap_uv: [0.5 + x / 128.0, 0.5 + y / 128.0],
            lightmap_page: Some(crate::scene::map::LightmapPageId(0)),
            lightmap_bump_step: 0.0,
            blend_alpha: 0.0,
            opacity: 1.0,
        })
        .to_vec();
    MapMesh {
        entity: false,
        vertices,
        indices: vec![0, 1, 2, 0, 2, 3],
        material_index: 0,
        visibility: MapMeshVisibility {
            always_visible: vec![MapMeshIndexRange {
                face: 0,
                start: 0,
                count: 6,
            }],
            clusters: Vec::new(),
        },
        surface_owners: vec![MapMeshSurfaceOwner {
            index_start: 0,
            owner: MapSurfaceOwner::World,
            solid: true,
            allows_decals: true,
            plane_normal: normal,
            plane_distance: distance,
            displacement: None,
            texture_projection: Some(super::super::MapSurfaceTextureProjection {
                vectors: [[u[0], u[1], u[2], 0.0], [v[0], v[1], v[2], 0.0]],
                minimum: [-16; 2],
                extent: [32; 2],
            }),
        }],
    }
}

fn decal(origin: Vec3) -> MapDecal {
    MapDecal {
        id: 5,
        material_name: "decals/test".to_owned(),
        position: MapDecalPosition {
            authored_world: origin,
            receiver_local: origin,
        },
        owner: MapSurfaceOwner::World,
        permanence: MapDecalPermanence::Permanent,
        projection_order: None,
    }
}

fn dimensions(extent: [f32; 2]) -> MapDecalDimensions {
    MapDecalDimensions {
        extent,
        radius: extent[0].max(extent[1]) * 0.5,
    }
}

#[test]
fn static_decal_network_coordinates_truncate_magnitude_to_five_fractional_bits() {
    assert_eq!(
        network_decal_position(Vec3::new(-1666.86, -2698.57, 256.0)),
        Vec3::new(-53_339.0 / 32.0, -86_354.0 / 32.0, 256.0)
    );
    assert_eq!(
        network_decal_position(Vec3::new(0.03124, -0.03124, -0.03125)),
        Vec3::new(0.0, 0.0, -0.03125)
    );
    assert_eq!(
        network_decal_position(Vec3::new(13.999, -13.999, 16385.125)),
        Vec3::new(13.96875, -13.96875, 1.125)
    );
}

fn displacement(mesh: &MapMesh) -> super::super::MapDisplacementProjection {
    let corners: [Vec3; 4] = std::array::from_fn(|index| mesh.vertices[index].base_position);
    super::super::MapDisplacementProjection {
        base_corners: corners,
        steps: 2,
        positions: (0..=2)
            .flat_map(|column| {
                (0..=2).map(move |row| {
                    corners[0].lerp(corners[1], column as f32 / 2.0).lerp(
                        corners[3].lerp(corners[2], column as f32 / 2.0),
                        row as f32 / 2.0,
                    )
                })
            })
            .collect(),
    }
}

#[test]
fn displacement_leaf_bounds_include_disallowed_full_grid_vertices_without_expanding_draw_bounds() {
    let corners = [
        Vec3::ZERO,
        Vec3::new(256.0, 0.0, 0.0),
        Vec3::new(256.0, 256.0, 0.0),
        Vec3::new(0.0, 256.0, 0.0),
    ];
    let mut bsp = super::super::tests::displacement_test_bsp(2, corners, 0);
    bsp.displacements[0].allowed_vertices[0] &= !(1 << 9);
    bsp.displacement_verts[9].dist = 128.0;
    let face = &bsp.faces[0];
    let displacement = &bsp.displacements[0];
    let grid = super::super::mesh::displacement_grid(&bsp, face, displacement, 4).unwrap();
    let triangles = super::super::mesh::displacement_vertices(&bsp, face, displacement).unwrap();
    let mut mesh = plane(Vec3::new(0.0, 0.0, 1.0), 0.0);
    let template = mesh.vertices[0];
    mesh.vertices = triangles
        .into_iter()
        .map(|vertex| MapVertex {
            position: vertex.position,
            base_position: vertex.base_position,
            ..template
        })
        .collect();
    mesh.indices = (0..mesh.vertices.len() as u32).collect();
    mesh.visibility.always_visible[0].count = mesh.indices.len() as u32;
    mesh.surface_owners[0].displacement = Some(super::super::MapDisplacementProjection {
        base_corners: corners,
        steps: 4,
        positions: grid.into_iter().map(|vertex| vertex.position).collect(),
    });
    let world = [mesh];
    let projector = MapSurfaceProjector::new(&world, &[], &[]);
    let receiver = projector.receivers_for_face(0).next().unwrap();
    assert_eq!(receiver.bounds.max[2], 0.0);
    let linked = projector.displacement_bounds().collect::<Vec<_>>();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].0, 0);
    assert_eq!(linked[0].1.min, Vec3::ZERO);
    assert_eq!(linked[0].1.max, Vec3::new(256.0, 256.0, 128.0));
    assert_eq!(projector.receiver_metadata()[0].bounds, receiver.bounds);
}

#[test]
fn native_decal_basis_orientation_is_preserved_on_axis_aligned_surfaces() {
    for (normal, expected) in [
        (
            Vec3::new(0.0, 0.0, 1.0),
            [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, -1.0, 0.0)],
        ),
        (
            Vec3::new(1.0, 0.0, 0.0),
            [Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, -1.0)],
        ),
        (
            Vec3::new(0.0, 1.0, 0.0),
            [Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0)],
        ),
        (
            Vec3::new(0.0, 0.0, -1.0),
            [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)],
        ),
    ] {
        assert_eq!(decal_axes(normal).unwrap(), expected);
    }
}

#[test]
fn sloped_decal_axes_are_orthonormal_and_keep_source_handedness() {
    for normal in [Vec3::new(0.2, 0.3, 1.0), Vec3::new(0.8, 0.9, 0.2)] {
        let normal = normal.normalize().unwrap();
        let [u, v] = decal_axes(normal).unwrap();
        assert!(u.dot(normal).abs() < 1e-6);
        assert!(v.dot(normal).abs() < 1e-6);
        assert!(u.dot(v).abs() < 1e-6);
        assert!(u.cross(v).dot(normal) < -0.99999);
    }
}

#[test]
fn native_base_texture_bounds_truncate_negative_minima_and_ceil_maxima() {
    let texture = super::super::MapSurfaceTextureProjection::from_points(
        [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
        &[Vec3::new(-1.75, -3.25, 0.0), Vec3::new(0.25, -1.75, 0.0)],
    )
    .unwrap();
    assert_eq!(texture.minimum, [-1, -3]);
    assert_eq!(texture.extent, [2, 2]);
}

#[test]
fn native_base_texture_bounds_keep_finite_seeds_and_signed_short_extents() {
    let texture = super::super::MapSurfaceTextureProjection::from_points(
        [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
        &[
            Vec3::new(0.0, -439_480.0, 0.0),
            Vec3::new(1.0, -438_800.0, 0.0),
        ],
    )
    .unwrap();
    assert_eq!(texture.minimum, [0, -439_480]);
    assert_eq!(texture.extent, [1, 11_801]);
}

#[test]
fn base_texture_bounds_keep_native_strict_lower_and_conservative_upper_limits() {
    let mesh = plane(Vec3::new(0.0, 0.0, 1.0), 0.0);
    let texture = mesh.surface_owners[0].texture_projection.unwrap();
    let axes = decal_axes(Vec3::new(0.0, 0.0, 1.0)).unwrap();
    for (x, accepted) in [(-20.0, false), (-19.99, true), (28.0, true), (28.01, false)] {
        assert_eq!(
            texture.admits_decal(Vec3::new(x, 0.0, 0.0), axes, [8.0; 2]),
            accepted
        );
    }
}

#[test]
fn zero_tangent_span_rejects_brush_decals_without_suppressing_displacements() {
    let mut mesh = plane(Vec3::new(-1.0, 0.0, 0.0), 4.0);
    assert_eq!(
        MapSurfaceProjector::new(std::slice::from_ref(&mesh), &[], &[])
            .project_decal(
                &decal(Vec3::new(-4.0, 0.0, 0.0)),
                dimensions([8.0; 2]),
                &[true]
            )
            .unwrap()
            .len(),
        1
    );
    mesh.surface_owners[0].texture_projection =
        super::super::MapSurfaceTextureProjection::from_points(
            [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0]],
            &mesh
                .vertices
                .iter()
                .map(|vertex| vertex.position)
                .collect::<Vec<_>>(),
        );
    assert!(
        MapSurfaceProjector::new(std::slice::from_ref(&mesh), &[], &[])
            .project_decal(
                &decal(Vec3::new(-4.0, 0.0, 0.0)),
                dimensions([8.0; 2]),
                &[true]
            )
            .unwrap()
            .is_empty()
    );
    mesh.surface_owners[0].displacement = Some(displacement(&mesh));
    assert_eq!(
        MapSurfaceProjector::new(&[mesh], &[], &[])
            .project_decal(
                &decal(Vec3::new(-4.0, 0.0, 0.0)),
                dimensions([8.0; 2]),
                &[true]
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn missing_base_texture_metadata_rejects_planar_decals() {
    let mut mesh = plane(Vec3::new(0.0, 0.0, 1.0), 0.0);
    mesh.surface_owners[0].texture_projection = None;
    assert!(
        MapSurfaceProjector::new(&[mesh], &[], &[])
            .project_decal(&decal(Vec3::ZERO), dimensions([8.0; 2]), &[true])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn base_texture_bounds_use_authored_origin_including_plane_normal_component() {
    let mut mesh = plane(Vec3::new(0.0, 0.0, 1.0), 0.0);
    mesh.surface_owners[0]
        .texture_projection
        .as_mut()
        .unwrap()
        .vectors[0][2] = 10.0;
    let world = [mesh];
    let projector = MapSurfaceProjector::new(&world, &[], &[]);
    assert_eq!(
        projector
            .project_decal(&decal(Vec3::ZERO), dimensions([8.0; 2]), &[true])
            .unwrap()
            .len(),
        1
    );
    assert!(
        projector
            .project_decal(
                &decal(Vec3::new(0.0, 0.0, 3.0)),
                dimensions([8.0; 2]),
                &[true]
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn projection_uses_full_material_dimensions_and_receiver_lightmap() {
    let world = [plane(Vec3::new(0.0, 0.0, 1.0), 0.0)];
    let projector = MapSurfaceProjector::new(&world, &[], &[]);
    let marks = projector
        .project_decal(&decal(Vec3::ZERO), dimensions([8.0, 4.0]), &[true])
        .unwrap();
    assert_eq!(marks.len(), 1);
    let mark = &marks[0];
    assert!(!mark.indices.is_empty());
    for vertex in &mark.vertices {
        let p = vertex.surface.position;
        assert!(p[0].abs() <= 4.00001);
        assert!(p[1].abs() <= 2.00001);
        assert!((vertex.uv[0] - (p[0] / 8.0 + 0.5)).abs() < 1e-6);
        assert!((vertex.uv[1] - (-p[1] / 4.0 + 0.5)).abs() < 1e-6);
        assert!((vertex.surface.lightmap_uv[0] - (0.5 + p[0] / 128.0)).abs() < 1e-6);
    }
}

#[test]
fn receiver_depth_has_strict_native_four_unit_limits() {
    let world = [plane(Vec3::new(0.0, 0.0, 1.0), 0.0)];
    let projector = MapSurfaceProjector::new(&world, &[], &[]);
    for z in [-4.0, 4.0] {
        assert!(
            projector
                .project_decal(
                    &decal(Vec3::new(0.0, 0.0, z)),
                    dimensions([8.0, 8.0]),
                    &[true]
                )
                .unwrap()
                .is_empty()
        );
    }
    for z in [-3.99, 0.0, 3.99] {
        assert_eq!(
            projector
                .project_decal(
                    &decal(Vec3::new(0.0, 0.0, z)),
                    dimensions([8.0, 8.0]),
                    &[true]
                )
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn decals_wrap_nearby_receiver_planes_without_a_hit_normal_filter() {
    let world = [
        plane(Vec3::new(1.0, 0.0, 0.0), 0.0),
        plane(Vec3::new(0.0, 1.0, 0.0), 0.0),
    ];
    let projector = MapSurfaceProjector::new(&world, &[], &[]);
    assert_eq!(
        projector
            .project_decal(
                &decal(Vec3::new(1.0, 1.0, 0.0)),
                dimensions([8.0, 8.0]),
                &[true]
            )
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn compiled_and_effective_receiver_suppression_are_independent() {
    let mut world = [plane(Vec3::new(0.0, 0.0, 1.0), 0.0)];
    assert!(
        MapSurfaceProjector::new(&world, &[], &[])
            .project_decal(&decal(Vec3::ZERO), dimensions([8.0, 8.0]), &[false])
            .unwrap()
            .is_empty()
    );
    world[0].surface_owners[0].allows_decals = false;
    assert!(
        MapSurfaceProjector::new(&world, &[], &[])
            .project_decal(&decal(Vec3::ZERO), dimensions([8.0, 8.0]), &[true])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn decals_stay_on_the_entity_selected_by_the_initial_trace() {
    let mut world = [plane(Vec3::new(0.0, 0.0, 1.0), 0.0)];
    world[0].surface_owners[0].owner = MapSurfaceOwner::Brush {
        entity_index: 3,
        model_index: crate::scene::map::ModelIndex::new(1),
        origin: Vec3::ZERO,
        angles: Vec3::ZERO,
    };
    assert!(
        MapSurfaceProjector::new(&world, &[], &[])
            .project_decal(&decal(Vec3::ZERO), dimensions([8.0, 8.0]), &[true])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn segment_hits_have_bounded_fractions_and_ignore_unrelated_triangles() {
    let triangle = [
        Vec3::new(-1.0, -1.0, 0.0),
        Vec3::new(1.0, -1.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
    ];
    assert_eq!(
        triangle_hit_fraction(
            Vec3::new(0.0, 0.0, -5.0),
            Vec3::new(0.0, 0.0, 5.0),
            triangle
        ),
        Some(0.5)
    );
    assert_eq!(
        triangle_hit_fraction(
            Vec3::new(4.0, 4.0, -5.0),
            Vec3::new(4.0, 4.0, 5.0),
            triangle
        ),
        None
    );
}

#[test]
fn displacement_decals_follow_elevated_triangles_without_base_plane_rejection() {
    let mut world = [plane(Vec3::new(0.0, 0.0, 1.0), 0.0)];
    world[0].surface_owners[0].displacement = Some(displacement(&world[0]));
    for vertex in &mut world[0].vertices {
        vertex.position[2] = 64.0 + vertex.position[0];
    }
    let marks = MapSurfaceProjector::new(&world, &[], &[])
        .project_decal(
            &decal(Vec3::new(0.0, 0.0, 64.0)),
            dimensions([8.0, 8.0]),
            &[true],
        )
        .unwrap();
    assert_eq!(marks.len(), 1);
    for vertex in &marks[0].vertices {
        assert!((vertex.surface.position[2] - (64.0 + vertex.surface.position[0])).abs() < 1e-6);
    }
    assert!(
        MapSurfaceProjector::new(&world, &[], &[])
            .project_decal(
                &decal(Vec3::new(0.0, 0.0, 100.0)),
                dimensions([8.0, 8.0]),
                &[true]
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn authored_static_decal_trace_selects_brush_owner_and_skips_triggered_entities() {
    let bsp = crate::scene::map::tests::brush_collision_bsp(
        r#"
        { "classname" "func_brush" "model" "*1" "origin" "300 40 120" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "348 60 160" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "348 60 160" "targetname" "trigger_me" }
        { "classname" "infodecal" "origin" "348 60 160" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "9000 9000 9000" }
    "#,
    );
    let decals = map_decals(&bsp, &MapSurfaceProjector::new(&[], &[], &[]), &[], None);
    assert_eq!(decals.len(), 1);
    assert_eq!(
        decals[0].position,
        MapDecalPosition {
            authored_world: Vec3::new(348.0, 60.0, 160.0),
            receiver_local: Vec3::new(48.0, 20.0, 40.0),
        }
    );
    assert!(matches!(decals[0].owner, MapSurfaceOwner::Brush { .. }));
}

#[test]
fn static_decal_network_coordinates_follow_owner_inverse_transform() {
    let authored = Vec3::new(348.037, 60.071, 160.019);
    let bsp = crate::scene::map::tests::brush_collision_bsp(
        r#"
        { "classname" "func_brush" "model" "*1" "origin" "300.017 40.023 120.011" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "348.037 60.071 160.019" }
        "#,
    );
    let decals = map_decals(&bsp, &MapSurfaceProjector::new(&[], &[], &[]), &[], None);
    assert_eq!(decals.len(), 1);
    assert_eq!(decals[0].position.authored_world, authored);
    assert_eq!(
        decals[0].position.receiver_local,
        Vec3::new(48.0, 20.03125, 40.0)
    );
    assert_ne!(
        decals[0].position.receiver_local,
        network_decal_position(authored) - Vec3::new(300.017, 40.023, 120.011)
    );
}

#[test]
fn rotated_brush_decal_network_coordinates_keep_local_fractional_axes() {
    let transform = brushes::BrushTransform {
        origin: Vec3::new(300.017, 40.023, 120.011),
        angles: Vec3::new(0.0, 45.0, 0.0),
    };
    let local = Vec3::new(48.019, 20.049, 40.007);
    let authored = transform.point(local);
    let bsp = crate::scene::map::tests::brush_collision_bsp(&format!(
        r#"
        {{ "classname" "func_brush" "model" "*1" "origin" "300.017 40.023 120.011" "angles" "0 45 0" }}
        {{ "classname" "infodecal" "texture" "decals/test" "origin" "{} {} {}" }}
        "#,
        authored[0], authored[1], authored[2]
    ));
    let decals = map_decals(&bsp, &MapSurfaceProjector::new(&[], &[], &[]), &[], None);
    assert_eq!(decals.len(), 1);
    assert_eq!(decals[0].position.authored_world, authored);
    assert_eq!(
        decals[0].position.receiver_local,
        Vec3::new(48.0, 20.03125, 40.0)
    );
    assert_ne!(
        decals[0].position.receiver_local,
        inverse_transform_vector(
            network_decal_position(authored) - transform.origin,
            transform.angles
        )
    );
}

#[test]
fn static_decal_projection_uses_decoded_position_instead_of_authored_origin() {
    let world = [plane(Vec3::new(0.0, 0.0, 1.0), 0.0)];
    let mut stamp = decal(Vec3::new(0.049, -0.071, 0.019));
    stamp.position.receiver_local = network_decal_position(stamp.position.authored_world);
    let marks = MapSurfaceProjector::new(&world, &[], &[])
        .project_decal(&stamp, dimensions([8.0; 2]), &[true])
        .unwrap();
    assert_eq!(marks.len(), 1);
    let center = Vec3::new(0.03125, -0.0625, 0.0);
    assert_eq!(marks[0].fade_origin, center);
    for vertex in &marks[0].vertices {
        assert!(
            (vertex.uv[0] - ((vertex.surface.position[0] - center[0]) / 8.0 + 0.5)).abs() < 1e-6
        );
        assert!(
            (vertex.uv[1] - (-(vertex.surface.position[1] - center[1]) / 8.0 + 0.5)).abs() < 1e-6
        );
    }
}

#[test]
fn static_decal_low_priority_controls_permanence() {
    let bsp = crate::scene::map::tests::brush_collision_bsp(
        r#"
        { "classname" "func_brush" "model" "*1" "origin" "300 40 120" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "348 60 160" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "348 60 160" "LowPriority" "0" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "348 60 160" "LowPriority" "1" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "348 60 160" "LowPriority" "2" }
        "#,
    );
    let decals = map_decals(&bsp, &MapSurfaceProjector::new(&[], &[], &[]), &[], None);
    assert_eq!(
        decals
            .iter()
            .map(|decal| decal.permanence)
            .collect::<Vec<_>>(),
        [
            MapDecalPermanence::Permanent,
            MapDecalPermanence::Permanent,
            MapDecalPermanence::NonPermanent,
            MapDecalPermanence::NonPermanent,
        ]
    );

    let world = [plane(Vec3::new(0.0, 0.0, 1.0), 0.0)];
    let projector = MapSurfaceProjector::new(&world, &[], &[]);
    for permanence in [
        MapDecalPermanence::Permanent,
        MapDecalPermanence::NonPermanent,
    ] {
        let mut decal = decal(Vec3::ZERO);
        decal.permanence = permanence;
        let marks = projector
            .project_decal(&decal, dimensions([8.0; 2]), &[true])
            .unwrap();
        assert_eq!(marks.len(), 1);
        assert_eq!(
            marks[0].source,
            super::super::MapSurfaceMarkSource::Decal {
                entity_index: decal.id,
                permanence,
            }
        );
    }
}

#[test]
fn shared_brush_models_trace_decals_to_distinct_doors_and_static_instances() {
    let bsp = crate::scene::map::tests::brush_collision_bsp(
        r#"
        { "classname" "func_door" "model" "*1" "origin" "300 40 120" }
        { "classname" "func_door" "model" "*1" "origin" "600 40 120" }
        { "classname" "func_brush" "model" "*1" "origin" "900 40 120" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "348 60 160" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "648 60 160" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "948 60 160" }
    "#,
    );
    let doors = super::super::pending_map_doors(&bsp)
        .into_iter()
        .map(|door| {
            super::super::PendingDoorBuild {
                bounds: Some(super::super::MapBounds {
                    min: Vec3::ZERO,
                    max: Vec3::splat(64.0),
                }),
                collision: None,
                door,
                meshes: Vec::new(),
            }
            .into_map_door()
            .unwrap()
        })
        .collect::<Vec<_>>();
    let projector = MapSurfaceProjector::new(&[], &[], &doors);
    let decals = map_decals(&bsp, &projector, &doors, None);
    assert_eq!(decals.len(), 3);
    assert_eq!(
        decals[0].owner,
        MapSurfaceOwner::Door {
            door_index: 0,
            model_index: super::super::ModelIndex::new(1)
        }
    );
    assert_eq!(
        decals[1].owner,
        MapSurfaceOwner::Door {
            door_index: 1,
            model_index: super::super::ModelIndex::new(1)
        }
    );
    let static_entity = bsp
        .entities
        .iter()
        .position(|entity| entity.is_class("func_brush") && entity.prop("model") == Some("*1"))
        .unwrap();
    assert!(
        matches!(decals[2].owner, MapSurfaceOwner::Brush { entity_index, .. } if entity_index == static_entity)
    );
}

#[test]
fn static_decal_trace_does_not_select_playerclip_or_non_solid_brushes() {
    let entities = r#"
        { "classname" "func_brush" "model" "*1" "origin" "300 40 120" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "348 60 160" }
    "#;
    let mut bsp = crate::scene::map::tests::brush_collision_bsp(entities);
    bsp.brushes[0].contents = super::super::contents_flags::PLAYERCLIP;
    assert!(map_decals(&bsp, &MapSurfaceProjector::new(&[], &[], &[]), &[], None).is_empty());
    let bsp = crate::scene::map::tests::brush_collision_bsp(&entities.replace(
        "\"origin\" \"300 40 120\"",
        "\"origin\" \"300 40 120\" \"solidity\" \"1\"",
    ));
    assert!(map_decals(&bsp, &MapSurfaceProjector::new(&[], &[], &[]), &[], None).is_empty());
}

#[test]
fn door_marks_keep_local_vertices_and_initial_pose_uvs() {
    let door = MapDoor {
        entity_index: 7,
        brush_collision: None,
        class: super::super::MapDoorClass::FuncDoorRotating,
        origin: Vec3::new(200.0, 300.0, 100.0),
        angles: Vec3::new(0.0, 15.0, 0.0),
        local_bounds_min: Vec3::new(-16.0, -16.0, 0.0),
        local_bounds_max: Vec3::new(16.0, 16.0, 0.0),
        visibility: super::super::MapVisibilityBucket::Always,
        auto_close_after: None,
        initial_progress: 1.0,
        motion: super::super::MapDoorMotion::Rotating {
            angle_delta: Vec3::new(0.0, 45.0, 0.0),
            degrees: 45.0,
            speed: 45.0,
            open_direction: super::super::MapDoorOpenDirection::Both,
        },
        sounds: super::super::MapDoorSounds::default(),
        geometry: super::super::MapDoorGeometry::Brush {
            model_index: crate::scene::map::ModelIndex::new(1),
            meshes: vec![plane(Vec3::new(0.0, 0.0, 1.0), 0.0)],
        },
    };
    let mut decal = decal(door.origin);
    decal.owner = MapSurfaceOwner::Door {
        door_index: 0,
        model_index: super::super::ModelIndex::new(1),
    };
    decal.position.receiver_local = Vec3::ZERO;
    let doors = [door];
    let marks = MapSurfaceProjector::new(&[], &[], &doors)
        .project_decal(&decal, dimensions([8.0, 8.0]), &[true])
        .unwrap();
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].owner, decal.owner);
    assert_eq!(marks[0].fade_origin, Vec3::ZERO);
    for vertex in &marks[0].vertices {
        assert!(vertex.surface.position[0].abs() <= 4.0001);
        assert!(vertex.surface.position[1].abs() <= 4.0001);
        assert!(vertex.surface.position[2].abs() < 1e-5);
        assert!((vertex.uv[0] - (vertex.surface.position[0] / 8.0 + 0.5)).abs() < 1e-5);
        assert!((vertex.uv[1] - (-vertex.surface.position[1] / 8.0 + 0.5)).abs() < 1e-5);
    }
}

#[test]
fn decal_projection_keeps_radius_independent_of_truncated_mapping_extent() {
    let mut world = [plane(Vec3::new(0.0, 0.0, 1.0), 0.0)];
    world[0].surface_owners[0].displacement = Some(displacement(&world[0]));
    let projector = MapSurfaceProjector::new(&world, &[], &[]);
    let stamp = decal(Vec3::new(0.0, 0.0, 4.1));
    assert_eq!(
        projector
            .project_decal(
                &stamp,
                MapDecalDimensions {
                    extent: [8.0; 2],
                    radius: 4.2
                },
                &[true]
            )
            .unwrap()
            .len(),
        1
    );
    assert!(
        projector
            .project_decal(
                &stamp,
                MapDecalDimensions {
                    extent: [8.0; 2],
                    radius: 4.0
                },
                &[true]
            )
            .unwrap()
            .is_empty()
    );
    let touching = decal(Vec3::new(0.0, 0.0, 4.2));
    assert!(
        projector
            .project_decal(
                &touching,
                MapDecalDimensions {
                    extent: [8.0; 2],
                    radius: 4.2
                },
                &[true]
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn one_texel_decal_with_zero_trace_radius_still_projects_to_planar_receivers() {
    let world = [plane(Vec3::new(0.0, 0.0, 1.0), 0.0)];
    let projector = MapSurfaceProjector::new(&world, &[], &[]);
    let marks = projector
        .project_decal(
            &decal(Vec3::ZERO),
            MapDecalDimensions {
                extent: [1.0; 2],
                radius: 0.0,
            },
            &[true],
        )
        .unwrap();
    assert_eq!(marks.len(), 1);
}

#[test]
fn decal_fragment_creation_order_survives_receiver_material_and_mesh_order() {
    let mut bsp = crate::scene::map::tests::brush_collision_bsp("");
    bsp.planes[0].normal = [0.0, 0.0, 1.0];
    bsp.planes[0].dist = 0.0;
    let mut face = bsp.faces[0];
    face.plane = 0;
    face.displacement = -1;
    bsp.faces = vec![face; 2];
    bsp.nodes = vec![super::super::Node {
        plane: 0,
        children: [-1, -2],
        mins: [-16; 3],
        maxs: [16; 3],
        first_face: 1,
        face_count: 1,
        area: 0,
    }];
    bsp.leaves[0].first_leaf_face = 0;
    bsp.leaves[0].leaf_face_count = 1;
    bsp.leaves[1].leaf_face_count = 0;
    bsp.leaf_faces = vec![0];
    let visibility = super::super::MapVisibility::from_bsp(&bsp, std::iter::empty()).unwrap();
    let mut stamp = decal(Vec3::ZERO);
    stamp.projection_order = Some(visibility.decal_projection(0, Vec3::ZERO));
    let mut world = [
        plane(Vec3::new(0.0, 0.0, 1.0), 0.0),
        plane(Vec3::new(0.0, 0.0, 1.0), 0.0),
    ];
    world[1].visibility.always_visible[0].face = 1;
    world[1].material_index = 1;
    let marks = MapSurfaceProjector::new(&world, &[], &[])
        .project_decal(&stamp, dimensions([8.0; 2]), &[true, true])
        .unwrap();
    assert_eq!(
        marks
            .iter()
            .map(|mark| mark.receiver_range.face)
            .collect::<Vec<_>>(),
        [1, 0]
    );
    assert_eq!(
        marks
            .iter()
            .map(|mark| mark.decal_creation_order)
            .collect::<Vec<_>>(),
        [Some(0), Some(1)]
    );
    assert_eq!(marks[0].source, marks[1].source);
}

#[test]
fn receiver_planes_preserve_uploaded_door_space_at_large_placed_origins() {
    let door = MapDoor {
        entity_index: 7,
        brush_collision: None,
        class: super::super::MapDoorClass::FuncDoorRotating,
        origin: Vec3::new(1_000_000.0, -600_000.0, 800_000.0),
        angles: Vec3::new(11.0, 37.0, -8.0),
        local_bounds_min: Vec3::new(-16.0, -16.0, 0.0),
        local_bounds_max: Vec3::new(16.0, 16.0, 0.0),
        visibility: super::super::MapVisibilityBucket::Always,
        auto_close_after: None,
        initial_progress: 1.0,
        motion: super::super::MapDoorMotion::Rotating {
            angle_delta: Vec3::new(0.0, 45.0, 0.0),
            degrees: 45.0,
            speed: 45.0,
            open_direction: super::super::MapDoorOpenDirection::Both,
        },
        sounds: super::super::MapDoorSounds::default(),
        geometry: super::super::MapDoorGeometry::Brush {
            model_index: crate::scene::map::ModelIndex::new(1),
            meshes: vec![plane(Vec3::new(-1.0, 0.0, 0.0), 4.0)],
        },
    };
    let doors = [door];
    let projector = MapSurfaceProjector::new(&[], &[], &doors);
    let receiver = &projector.receiver_metadata()[0];
    assert_eq!(receiver.plane.normal, Vec3::new(-1.0, 0.0, 0.0));
    assert_eq!(receiver.plane.distance, 4.0);
    assert_eq!(
        receiver.plane.signed_distance(Vec3::new(-5.0, 0.0, 0.0)),
        1.0
    );
    assert_eq!(
        receiver.owner,
        MapSurfaceOwner::Door {
            door_index: 0,
            model_index: super::super::ModelIndex::new(1)
        }
    );
    let super::super::MapDoorGeometry::Brush { meshes, .. } = &doors[0].geometry else {
        panic!("brush")
    };
    for vertex in &meshes[0].vertices {
        assert_eq!(receiver.plane.signed_distance(vertex.position), 0.0);
    }
}

#[test]
fn static_brush_receiver_planes_remain_in_placed_world_space() {
    let mut world = [plane(Vec3::new(0.0, 0.0, 1.0), 120.0)];
    world[0].surface_owners[0].owner = MapSurfaceOwner::Brush {
        entity_index: 7,
        model_index: super::super::ModelIndex::new(1),
        origin: Vec3::new(300.0, 600.0, 120.0),
        angles: Vec3::new(11.0, 37.0, -8.0),
    };
    let receivers = MapSurfaceProjector::new(&world, &[], &[]).receiver_metadata();
    assert_eq!(receivers[0].plane.normal, Vec3::new(0.0, 0.0, 1.0));
    assert_eq!(receivers[0].plane.distance, 120.0);
    assert_eq!(
        receivers[0]
            .plane
            .signed_distance(Vec3::new(0.0, 0.0, 121.0)),
        1.0
    );
    for vertex in &world[0].vertices {
        assert_eq!(receivers[0].plane.signed_distance(vertex.position), 0.0);
    }
}

#[test]
fn receiver_owner_identity_distinguishes_shared_models_without_capturing_pose() {
    let first = MapSurfaceOwner::Brush {
        entity_index: 123,
        model_index: super::super::ModelIndex::new(1),
        origin: Vec3::splat(40.0),
        angles: Vec3::splat(10.0),
    };
    let second = MapSurfaceOwner::Brush {
        entity_index: 456,
        model_index: super::super::ModelIndex::new(1),
        origin: Vec3::splat(80.0),
        angles: Vec3::splat(20.0),
    };
    let moved = MapSurfaceOwner::Brush {
        entity_index: 123,
        model_index: super::super::ModelIndex::new(1),
        origin: Vec3::splat(120.0),
        angles: Vec3::splat(30.0),
    };
    assert_eq!(first.id(), moved.id());
    assert_eq!(first.model_index(), Some(super::super::ModelIndex::new(1)));
    assert_eq!(MapSurfaceOwner::World.model_index(), None);
    let shared_model_door = MapSurfaceOwner::Door {
        door_index: 123,
        model_index: super::super::ModelIndex::new(1),
    };
    assert_eq!(shared_model_door.model_index(), first.model_index());
    assert_ne!(shared_model_door.id(), first.id());
    let owners = std::collections::BTreeSet::from([
        MapSurfaceOwner::World.id(),
        first.id(),
        second.id(),
        moved.id(),
        MapSurfaceOwner::Door {
            door_index: 123,
            model_index: super::super::ModelIndex::new(1),
        }
        .id(),
        MapSurfaceOwner::Door {
            door_index: 456,
            model_index: super::super::ModelIndex::new(1),
        }
        .id(),
    ]);
    assert_eq!(owners.len(), 5);
}
