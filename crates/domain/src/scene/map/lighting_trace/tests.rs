use super::*;
use vformats::bsp::texture_flags;

fn box_brush(contents: i32) -> LightingBrush {
    let min = Vec3::new(10.0, -8.0, -8.0);
    let max = Vec3::new(20.0, 8.0, 8.0);
    LightingBrush {
        index: 7,
        contents,
        bounds: MapBounds { min, max },
        sides: [
            ([1.0, 0.0, 0.0], 20.0),
            ([-1.0, 0.0, 0.0], -10.0),
            ([0.0, 1.0, 0.0], 8.0),
            ([0.0, -1.0, 0.0], 8.0),
            ([0.0, 0.0, 1.0], 8.0),
            ([0.0, 0.0, -1.0], 8.0),
        ]
        .into_iter()
        .map(|(normal, dist)| LightingSide {
            plane: Plane {
                normal,
                dist,
                axis_type: 3,
            },
            flags: texture_flags::SKY,
        })
        .collect(),
    }
}

fn collision(brushes: Vec<LightingBrush>) -> MapLightingCollision {
    MapLightingCollision {
        brush_tree: BoundsTree::new(brushes.iter().map(|brush| brush.bounds)),
        triangle_tree: BoundsTree::new(std::iter::empty()),
        brushes,
        triangles: Vec::new(),
        planes: Vec::new(),
        nodes: Vec::new(),
        leaf_contents: vec![0],
        leaf_flags: vec![0],
        root: -1,
    }
}

#[test]
fn accelerated_traces_preserve_linear_hits_ties_and_startsolid_metadata() {
    let brushes = (0..192)
        .map(|index| {
            let mut brush = box_brush(if index % 7 == 0 {
                contents_flags::PLAYERCLIP
            } else {
                contents_flags::OPAQUE
            });
            let offset = Vec3::new((index % 48) as f32 * 24.0, (index / 48) as f32 * 24.0, 0.0);
            brush.index = index;
            brush.bounds.min += offset;
            brush.bounds.max += offset;
            for side in &mut brush.sides {
                side.plane.dist += Vec3::from(side.plane.normal).dot(offset);
            }
            brush
        })
        .collect();
    let mut world = collision(brushes);
    let mut duplicate = world.brushes[1].clone();
    duplicate.index = 999;
    world.brushes.push(duplicate);
    world.brush_tree = BoundsTree::new(world.brushes.iter().map(|brush| brush.bounds));
    for i in 0..256 {
        let start = Vec3::new((i % 64) as f32 * 18.0, (i / 64) as f32 * 24.0, 0.0);
        let end = match i % 4 {
            0 => start,
            1 => start + Vec3::new(100.0, 0.0, 0.0),
            2 => start - Vec3::new(100.0, 0.0, 0.0),
            _ => start + Vec3::new(0.0, 100.0, 0.0),
        };
        let mut linear = MapLightingTrace::clear(end);
        let ray_bounds = super::super::bounds_from_points_iter([start, end].into_iter()).unwrap();
        for brush in &world.brushes {
            if brush.contents & MASK_OPAQUE != 0
                && intersects(ray_bounds, brush.bounds)
                && let Some(hit) = trace_brush(brush, start, end)
            {
                merge_hit(&mut linear, hit);
            }
        }
        linear.end_position = start.lerp(end, linear.fraction);
        assert_eq!(world.trace_opaque(start, end), Some(linear));
    }
}

#[test]
fn opaque_mask_excludes_playerclip_water_grate_and_windows() {
    for contents in [
        contents_flags::PLAYERCLIP,
        contents_flags::WATER,
        contents_flags::GRATE,
        contents_flags::WINDOW,
    ] {
        let world = collision(vec![box_brush(contents)]);
        let trace = world
            .trace_opaque(Vec3::ZERO, Vec3::new(30.0, 0.0, 0.0))
            .unwrap();
        assert_eq!(trace.fraction, 1.0);
        assert!(!trace.start_solid);
        assert_eq!(trace.brush_index, None);
    }
}

#[test]
fn opaque_and_moveable_contents_survive_without_solid_or_playerclip() {
    for contents in [
        contents_flags::OPAQUE,
        contents_flags::MOVEABLE,
        contents_flags::SOLID | contents_flags::PLAYERCLIP,
    ] {
        let world = collision(vec![box_brush(contents)]);
        let trace = world
            .trace_opaque(Vec3::ZERO, Vec3::new(30.0, 0.0, 0.0))
            .unwrap();
        assert_eq!(trace.fraction, (10.0 - DIST_EPSILON) / 30.0);
        assert_eq!(trace.end_position, Vec3::new(9.96875, 0.0, 0.0));
        assert_eq!(trace.normal, Vec3::new(-1.0, 0.0, 0.0));
        assert_eq!(trace.contents, contents);
        assert_eq!(trace.surface_flags, texture_flags::SKY);
        assert_eq!(trace.brush_index, Some(7));
    }
}

#[test]
fn stationary_inside_and_exiting_rays_preserve_startsolid_allsolid() {
    let world = collision(vec![box_brush(contents_flags::SOLID)]);
    let inside = Vec3::new(15.0, 0.0, 0.0);
    let stationary = world.trace_opaque(inside, inside).unwrap();
    assert!(stationary.start_solid && stationary.all_solid);
    assert_eq!(stationary.fraction, 0.0);
    let exiting = world.trace_opaque(inside, Vec3::ZERO).unwrap();
    assert!(exiting.start_solid);
    assert!(!exiting.all_solid);
    assert_eq!(exiting.fraction, 1.0);
    assert_eq!(exiting.contents, contents_flags::SOLID);
    assert_eq!(exiting.brush_index, Some(7));
    let boundary = world
        .trace_opaque(Vec3::new(10.0, 0.0, 0.0), inside)
        .unwrap();
    assert!(boundary.start_solid && boundary.all_solid);
}

#[test]
fn branch_plane_snap_preserves_exact_plane_contact_on_both_branches() {
    let mut world = collision(Vec::new());
    world.root = 0;
    world.planes = vec![Plane {
        normal: [1.0, 0.0, 0.0],
        dist: 10.0,
        axis_type: 0,
    }];
    world.nodes = vec![Node {
        plane: 0,
        children: [-1, -2],
        mins: [0; 3],
        maxs: [0; 3],
        first_face: 0,
        face_count: 0,
        area: 0,
    }];
    world.leaf_contents = vec![0, contents_flags::SOLID];
    assert_eq!(world.leaf_at(Vec3::new(10.0, 0.0, 0.0)), Some(0));
    assert_eq!(world.leaf_contents(1), Some(contents_flags::SOLID));
    assert_eq!(
        world.snap_to_point_leaf(Vec3::new(12.0, 1.0, 2.0), Vec3::new(8.0, 3.0, 4.0)),
        Some(Vec3::new(10.5, 3.0, 4.0))
    );
    assert_eq!(
        world.snap_to_point_leaf(Vec3::new(8.0, 1.0, 2.0), Vec3::new(10.0, 3.0, 4.0)),
        Some(Vec3::new(10.0, 3.0, 4.0))
    );
    assert_eq!(
        world.snap_to_point_leaf(Vec3::new(8.0, 1.0, 2.0), Vec3::new(11.0, 3.0, 4.0)),
        Some(Vec3::new(9.5, 3.0, 4.0))
    );
    world.nodes[0].children = [0, -2];
    assert_eq!(world.leaf_at(Vec3::new(12.0, 0.0, 0.0)), None);
    assert_eq!(
        world.snap_to_point_leaf(Vec3::new(12.0, 0.0, 0.0), Vec3::ZERO),
        None
    );
}

#[test]
fn invalid_ray_coordinates_are_explicitly_unavailable() {
    let world = collision(vec![box_brush(contents_flags::SOLID)]);
    assert!(
        world
            .trace_opaque(Vec3::splat(f32::NAN), Vec3::ZERO)
            .is_none()
    );
    assert!(
        world
            .trace_opaque(Vec3::ZERO, Vec3::splat(f32::INFINITY))
            .is_none()
    );
    assert!(
        world
            .trace_opaque(Vec3::splat(-f32::MAX), Vec3::splat(f32::MAX))
            .is_none()
    );
    assert!(world.leaf_at(Vec3::splat(f32::NAN)).is_none());
}

#[test]
fn full_displacement_grid_ignores_render_allowed_vertex_reduction() {
    let corners = [
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(16.0, 0.0, 0.0),
        Vec3::new(16.0, 16.0, 0.0),
        Vec3::new(0.0, 16.0, 0.0),
    ];
    let mut bsp = super::super::tests::displacement_test_bsp(2, corners, 0);
    bsp.models.push(vformats::bsp::BspModel {
        mins: [0.0; 3],
        maxs: [16.0; 3],
        origin: [0.0; 3],
        head_node: -1,
        first_face: 0,
        face_count: 1,
    });
    bsp.displacements[0].allowed_vertices = [0; 10];
    bsp.displacements[0].contents = contents_flags::OPAQUE;
    let world = MapLightingCollision::from_bsp(&bsp).unwrap();
    assert_eq!(world.displacement_triangle_count(), 32);
    let trace = world
        .trace_opaque(Vec3::new(3.0, 3.0, 10.0), Vec3::new(3.0, 3.0, -10.0))
        .unwrap();
    assert_eq!(trace.fraction, 0.5);
    assert_eq!(trace.displacement_index, Some(0));
    assert_eq!(trace.contents, contents_flags::OPAQUE);
    assert_eq!(
        world
            .trace_opaque(Vec3::new(3.0, 3.0, -10.0), Vec3::new(3.0, 3.0, 10.0))
            .unwrap()
            .fraction,
        1.0
    );
    // The runtime's even-cell diagonal crosses the raised opposite corner.
    // Offline VRAD's fixed opposite diagonal would hit the base plane here.
    bsp.displacement_verts[6].dist = 100.0;
    let raised = MapLightingCollision::from_bsp(&bsp).unwrap();
    let trace = raised
        .trace_opaque(Vec3::new(1.0, 1.0, 200.0), Vec3::new(1.0, 1.0, -10.0))
        .unwrap();
    assert!((trace.end_position[2] - 25.0).abs() < 1.0e-4);
}

#[test]
fn world_model_excludes_entity_brushes_and_rejects_partial_invalid_hulls() {
    let corners = [
        Vec3::ZERO,
        Vec3::new(16.0, 0.0, 0.0),
        Vec3::new(16.0, 16.0, 0.0),
        Vec3::new(0.0, 16.0, 0.0),
    ];
    let mut bsp = super::super::tests::displacement_test_bsp(2, corners, 0);
    bsp.faces.clear();
    bsp.displacements.clear();
    let model = vformats::bsp::BspModel {
        mins: [0.0; 3],
        maxs: [20.0; 3],
        origin: [0.0; 3],
        head_node: -1,
        first_face: 0,
        face_count: 0,
    };
    bsp.models = vec![
        model,
        vformats::bsp::BspModel {
            head_node: -2,
            ..model
        },
    ];
    let mut leaf = bsp.leaves[0];
    leaf.first_leaf_brush = 0;
    leaf.leaf_brush_count = 0;
    let mut entity_leaf = leaf;
    entity_leaf.leaf_brush_count = 1;
    bsp.leaves = vec![leaf, entity_leaf];
    bsp.leaf_brushes = vec![0];
    let brush = box_brush(contents_flags::OPAQUE);
    bsp.planes = brush.sides.iter().map(|side| side.plane).collect();
    bsp.brush_sides = (0..6)
        .map(|plane| vformats::bsp::BrushSide {
            plane,
            texinfo: -1,
            displacement: -1,
            bevel: 0,
        })
        .collect();
    bsp.brushes = vec![vformats::bsp::Brush {
        first_side: 0,
        side_count: 6,
        contents: contents_flags::OPAQUE,
    }];
    assert_eq!(
        MapLightingCollision::from_bsp(&bsp).unwrap().brush_count(),
        0
    );
    bsp.leaves[0].leaf_brush_count = 1;
    assert_eq!(
        MapLightingCollision::from_bsp(&bsp).unwrap().brush_count(),
        1
    );
    bsp.brush_sides[5].plane = u16::MAX;
    assert_eq!(
        MapLightingCollision::from_bsp(&bsp).unwrap().brush_count(),
        0
    );
}
