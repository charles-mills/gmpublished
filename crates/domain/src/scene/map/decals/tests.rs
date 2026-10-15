use super::super::{MapMeshIndexRange, MapMeshSurfaceOwner, MapMeshVisibility, MapVertex};
use super::*;

fn plane(normal: Vec3, distance: f32) -> MapMesh {
    let [u, v] = decal_axes(normal).unwrap();
    let origin = normal * distance;
    let vertices = [[-16.0, -16.0], [-16.0, 16.0], [16.0, 16.0], [16.0, -16.0]]
        .map(|[x, y]| MapVertex {
            position: origin + u * x + v * y,
            normal,
            tangent_s: u,
            tangent_t: v,
            tex_s: x,
            tex_t: y,
            texture_size: [64; 2],
            lightmap_uv: [0.5 + x / 128.0, 0.5 + y / 128.0],
            has_lightmap: true,
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
            displacement: false,
        }],
    }
}

fn decal(origin: Vec3) -> MapDecal {
    MapDecal {
        id: 5,
        material_name: "decals/test".to_owned(),
        origin,
        owner: MapSurfaceOwner::World,
    }
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
fn projection_uses_full_material_dimensions_and_receiver_lightmap() {
    let world = [plane(Vec3::new(0.0, 0.0, 1.0), 0.0)];
    let projector = MapSurfaceProjector::new(&world, &[], &[]);
    let marks = projector
        .project_decal(&decal(Vec3::ZERO), [8.0, 4.0], &[true])
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
                .project_decal(&decal(Vec3::new(0.0, 0.0, z)), [8.0, 8.0], &[true])
                .unwrap()
                .is_empty()
        );
    }
    for z in [-3.99, 0.0, 3.99] {
        assert_eq!(
            projector
                .project_decal(&decal(Vec3::new(0.0, 0.0, z)), [8.0, 8.0], &[true])
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
            .project_decal(&decal(Vec3::new(1.0, 1.0, 0.0)), [8.0, 8.0], &[true])
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
            .project_decal(&decal(Vec3::ZERO), [8.0, 8.0], &[false])
            .unwrap()
            .is_empty()
    );
    world[0].surface_owners[0].allows_decals = false;
    assert!(
        MapSurfaceProjector::new(&world, &[], &[])
            .project_decal(&decal(Vec3::ZERO), [8.0, 8.0], &[true])
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
            .project_decal(&decal(Vec3::ZERO), [8.0, 8.0], &[true])
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
    world[0].surface_owners[0].displacement = true;
    for vertex in &mut world[0].vertices {
        vertex.position[2] = 64.0 + vertex.position[0];
    }
    let marks = MapSurfaceProjector::new(&world, &[], &[])
        .project_decal(&decal(Vec3::new(0.0, 0.0, 64.0)), [8.0, 8.0], &[true])
        .unwrap();
    assert_eq!(marks.len(), 1);
    for vertex in &marks[0].vertices {
        assert!((vertex.surface.position[2] - (64.0 + vertex.surface.position[0])).abs() < 1e-6);
    }
    assert!(
        MapSurfaceProjector::new(&world, &[], &[])
            .project_decal(&decal(Vec3::new(0.0, 0.0, 100.0)), [8.0, 8.0], &[true])
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
    let decals = map_decals(&bsp, &[], &[], &[]);
    assert_eq!(decals.len(), 1);
    assert_eq!(decals[0].origin, Vec3::new(348.0, 60.0, 160.0));
    assert!(matches!(decals[0].owner, MapSurfaceOwner::Brush { .. }));
}

#[test]
fn static_decal_trace_does_not_select_playerclip_or_non_solid_brushes() {
    let entities = r#"
        { "classname" "func_brush" "model" "*1" "origin" "300 40 120" }
        { "classname" "infodecal" "texture" "decals/test" "origin" "348 60 160" }
    "#;
    let mut bsp = crate::scene::map::tests::brush_collision_bsp(entities);
    bsp.brushes[0].contents = super::super::contents_flags::PLAYERCLIP;
    assert!(map_decals(&bsp, &[], &[], &[]).is_empty());
    let bsp = crate::scene::map::tests::brush_collision_bsp(&entities.replace(
        "\"origin\" \"300 40 120\"",
        "\"origin\" \"300 40 120\" \"solidity\" \"1\"",
    ));
    assert!(map_decals(&bsp, &[], &[], &[]).is_empty());
}

#[test]
fn door_marks_keep_local_vertices_and_initial_pose_uvs() {
    let door = MapDoor {
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
    decal.owner = MapSurfaceOwner::Door { door_index: 0 };
    let doors = [door];
    let marks = MapSurfaceProjector::new(&[], &[], &doors)
        .project_decal(&decal, [8.0, 8.0], &[true])
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
