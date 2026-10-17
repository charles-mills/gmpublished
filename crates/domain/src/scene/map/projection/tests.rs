use super::super::{MapMeshClusterRanges, MapMeshSurfaceOwner, MapMeshVisibility};
use super::*;

fn vertex(x: f32, y: f32) -> MapVertex {
    MapVertex {
        position: Vec3::new(x, y, 2.0 * x + 3.0 * y),
        base_position: Vec3::new(x, y, 2.0 * x + 3.0 * y),
        normal: Vec3::new(-2.0, -3.0, 1.0),
        tangent_s: Vec3::new(1.0, 0.0, 2.0),
        tangent_t: Vec3::new(0.0, 1.0, 3.0),
        tex_s: x * 2.0,
        tex_t: y * 4.0,
        texture_size: [64, 32],
        lightmap_uv: [0.2 + x * 0.01, 0.4 + y * 0.02],
        lightmap_page: Some(crate::scene::map::LightmapPageId(0)),
        lightmap_bump_step: 0.125,
        blend_alpha: 0.0,
        opacity: 1.0,
    }
}

fn projection() -> SurfaceProjection {
    SurfaceProjection {
        origin: Vec3::ZERO,
        u_axis: Vec3::new(1.0, 0.0, 0.0),
        v_axis: Vec3::new(0.0, 1.0, 0.0),
        corners: [[-1.0, -1.0], [-1.0, 1.0], [1.0, 1.0], [1.0, -1.0]],
        u: [0.25, 0.75],
        v: [0.0, 2.0],
    }
}

fn mesh() -> MapMesh {
    let range = MapMeshIndexRange {
        face: 7,
        start: 0,
        count: 3,
    };
    MapMesh {
        entity: false,
        vertices: vec![vertex(-5.0, -5.0), vertex(5.0, -5.0), vertex(0.0, 8.0)],
        indices: vec![0, 1, 2],
        material_index: 0,
        visibility: MapMeshVisibility {
            always_visible: Vec::new(),
            clusters: vec![
                MapMeshClusterRanges {
                    cluster: 2,
                    ranges: vec![range],
                },
                MapMeshClusterRanges {
                    cluster: 5,
                    ranges: vec![range],
                },
            ],
        },
        surface_owners: Vec::new(),
    }
}

#[test]
fn clipping_preserves_receiver_surface_and_atlas_coordinates() {
    let polygon =
        projection().clip_triangle(&[vertex(-5.0, -5.0), vertex(5.0, -5.0), vertex(0.0, 8.0)]);
    assert_eq!(polygon.len(), 4);
    for projected in polygon {
        let p = projected.surface.position;
        assert!((p[2] - (2.0 * p[0] + 3.0 * p[1])).abs() < 1e-5);
        assert!((projected.surface.lightmap_uv[0] - (0.2 + p[0] * 0.01)).abs() < 1e-6);
        assert!((projected.surface.lightmap_uv[1] - (0.4 + p[1] * 0.02)).abs() < 1e-6);
        assert_eq!(projected.surface.lightmap_bump_step, 0.125);
        assert!(projected.surface.lightmap_page.is_some());
        assert!((projected.uv[0] - (0.5 + p[0] * 0.25)).abs() < 1e-6);
        assert!((projected.uv[1] - (1.0 + p[1])).abs() < 1e-6);
    }
}

#[test]
fn clipping_keeps_displacement_triangle_height_instead_of_projection_plane() {
    let mut triangle = [vertex(-2.0, -2.0), vertex(2.0, -2.0), vertex(0.0, 2.0)];
    triangle[0].position = Vec3::new(-2.0, -2.0, 4.0);
    triangle[1].position = Vec3::new(2.0, -2.0, 4.0);
    triangle[2].position = Vec3::new(0.0, 2.0, 20.0);
    let polygon = projection().clip_triangle(&triangle);
    assert!(polygon.len() >= 3);
    for vertex in polygon {
        let p = vertex.surface.position;
        assert!((p[2] - (12.0 + 4.0 * p[1])).abs() < 1e-5);
    }
}

#[test]
fn skew_quad_uses_bilinear_texture_coordinates() {
    let mut projection = projection();
    projection.corners = [[-3.0, -1.0], [-1.0, 3.0], [4.0, 2.0], [2.0, -2.0]];
    for [u, v] in [[0.0, 0.0], [1.0, 1.0], [0.2, 0.7], [0.8, 0.1], [0.5, 0.5]] {
        let point = std::array::from_fn(|axis| {
            projection.corners[0][axis] * (1.0 - u) * (1.0 - v)
                + projection.corners[3][axis] * u * (1.0 - v)
                + projection.corners[1][axis] * (1.0 - u) * v
                + projection.corners[2][axis] * u * v
        });
        let actual = projection.inverse_bilinear(point).unwrap();
        assert!((actual[0] - u).abs() < 1e-6);
        assert!((actual[1] - v).abs() < 1e-6);
    }
}

#[test]
fn clipping_handles_both_corner_windings() {
    let triangle = [vertex(-5.0, -5.0), vertex(5.0, -5.0), vertex(0.0, 8.0)];
    let mut reversed = projection();
    reversed.corners.reverse();
    assert_eq!(projection().clip_triangle(&triangle).len(), 4);
    assert_eq!(reversed.clip_triangle(&triangle).len(), 4);
}

#[test]
fn unrelated_receiver_triangles_do_not_create_floating_geometry() {
    assert!(
        projection()
            .clip_triangle(&[vertex(10.0, 10.0), vertex(12.0, 10.0), vertex(11.0, 12.0)])
            .is_empty()
    );
}

#[test]
fn distant_small_receivers_do_not_lose_clipping_orientation() {
    let mut triangle = [
        vertex(10_000.0, 10_000.0),
        vertex(10_001.0, 10_000.0),
        vertex(10_000.0, 10_001.0),
    ];
    assert!(projection().clip_quad_to_triangle(&triangle).is_empty());
    triangle.reverse();
    assert!(projection().clip_quad_to_triangle(&triangle).is_empty());
}

fn displacement_receiver() -> (super::super::MapDisplacementProjection, Vec<[MapVertex; 3]>) {
    let base_corners = [
        Vec3::new(-2.0, -2.0, 0.0),
        Vec3::new(-2.0, 2.0, 0.0),
        Vec3::new(2.0, 2.0, 0.0),
        Vec3::new(2.0, -2.0, 0.0),
    ];
    let grid = (0..=2)
        .flat_map(|column| {
            (0..=2).map(move |row| {
                let x = row as f32 * 2.0 - 2.0;
                let y = column as f32 * 2.0 - 2.0;
                let mut surface = vertex(x, y);
                surface.base_position = Vec3::new(x, y, 0.0);
                surface.position = surface.base_position;
                if column == 1 && row == 1 {
                    surface.position += Vec3::new(0.4, 0.3, 5.0);
                }
                surface
            })
        })
        .collect::<Vec<_>>();
    let triangles = (0..2)
        .flat_map(|column| {
            (0..2).flat_map(move |row| super::super::mesh::displacement_cell_triangles(column, row))
        })
        .map(|triangle| triangle.map(|(column, row)| grid[column * 3 + row]))
        .collect();
    (
        super::super::MapDisplacementProjection {
            base_corners,
            steps: 2,
            positions: grid.iter().map(|surface| surface.position).collect(),
        },
        triangles,
    )
}

#[test]
fn compiled_displacement_textures_follow_base_grid_while_positions_follow_deformation() {
    let (displacement, triangles) = displacement_receiver();
    let mut projection = projection();
    projection.u = [0.0, 1.0];
    projection.v = [0.0, 1.0];
    let polygons = projection.project_displacement(&displacement, triangles.into_iter());
    assert!(!polygons.is_empty());
    let mut saw_displaced_center = false;
    for vertex in polygons.iter().flatten() {
        let base = vertex.surface.base_position;
        assert!((vertex.uv[0] - (base[0] + 1.0) * 0.5).abs() < 1e-5);
        assert!((vertex.uv[1] - (base[1] + 1.0) * 0.5).abs() < 1e-5);
        assert!((vertex.surface.lightmap_uv[0] - (0.2 + base[0] * 0.01)).abs() < 1e-6);
        assert!((vertex.surface.lightmap_uv[1] - (0.4 + base[1] * 0.02)).abs() < 1e-6);
        if base.length_squared() < 1e-8 {
            assert!((vertex.surface.position - Vec3::new(0.4, 0.3, 5.0)).length_squared() < 1e-8);
            saw_displaced_center = true;
        }
    }
    assert!(saw_displaced_center);
}

#[test]
fn compiled_displacements_sample_full_grid_when_render_triangles_are_coarse() {
    let (displacement, _) = displacement_receiver();
    let corners = displacement.base_corners.map(|base| {
        let mut surface = vertex(base[0], base[1]);
        surface.position = base;
        surface.base_position = base;
        surface
    });
    let triangles = [
        [corners[0], corners[1], corners[2]],
        [corners[0], corners[2], corners[3]],
    ];
    let polygons = projection().project_displacement(&displacement, triangles.into_iter());
    assert!(polygons.iter().flatten().any(|vertex| {
        (vertex.surface.position - Vec3::new(0.4, 0.3, 5.0)).length_squared() < 1e-8
    }));
}

#[test]
fn skew_displacement_uvs_keep_native_vertical_then_horizontal_split_interpolation() {
    let (displacement, triangles) = displacement_receiver();
    let mut projection = projection();
    projection.u = [0.0, 1.0];
    projection.v = [0.0, 1.0];
    projection.corners = [[-1.5, -1.5], [-1.5, 1.5], [1.8, 1.0], [1.5, -1.0]];
    let polygons = projection.project_displacement(&displacement, triangles.into_iter());
    let centers = polygons
        .iter()
        .flatten()
        .filter(|vertex| vertex.surface.base_position.length_squared() < 1e-8)
        .collect::<Vec<_>>();
    assert!(!centers.is_empty());
    for center in centers {
        assert!((center.uv[0] - 53.0 / 111.0).abs() < 1e-6);
        assert!((center.uv[1] - 55.0 / 111.0).abs() < 1e-6);
        let inverse = projection.inverse_bilinear([0.0, 0.0]).unwrap();
        assert!((center.uv[0] - inverse[0]).abs() > 1e-4);
    }
}

#[test]
fn malformed_projection_is_rejected() {
    let mut projection = projection();
    projection.corners.swap(1, 2);
    assert!(!projection.is_valid());
    projection.corners = [[0.0; 2]; 4];
    assert!(!projection.is_valid());
    projection = super::tests::projection();
    projection.u_axis = Vec3::new(f32::NAN, 0.0, 0.0);
    assert!(!projection.is_valid());
}

#[test]
fn shared_receiver_ranges_keep_all_clusters_without_duplicate_triangles() {
    let world = [mesh()];
    let receivers = receiver_surfaces(&world, &[], &[]);
    let face = &receivers[&7];
    assert_eq!(face.len(), 1);
    assert_eq!(face[0].triangles().count(), 1);
    assert_eq!(face[0].visibility, MapPropVisibility::Clusters(vec![2, 5]));
}

#[test]
fn receiver_partition_and_brush_ownership_are_independent_of_origin_leaf() {
    let mut brush = mesh();
    let owner = MapSurfaceOwner::Brush {
        entity_index: 3,
        model_index: crate::scene::map::ModelIndex::new(1),
        origin: Vec3::new(32.0, 64.0, 128.0),
        angles: Vec3::new(10.0, 20.0, 30.0),
    };
    brush.surface_owners.push(MapMeshSurfaceOwner {
        index_start: 0,
        owner,
        solid: false,
        allows_decals: false,
        plane_normal: Vec3::new(0.0, 0.0, 1.0),
        plane_distance: 0.0,
        displacement: None,
        texture_projection: None,
    });
    let sky = [brush];
    let receivers = receiver_surfaces(&[], &sky, &[]);
    let receiver = &receivers[&7][0];
    assert_eq!(receiver.partition, GeometryPartition::Skybox);
    assert_eq!(receiver.owner, owner);
    assert!(!receiver.solid);
    assert!(!receiver.allows_decals);
}

#[test]
fn placed_surface_round_trip_preserves_local_vertex_attributes() {
    let transform = BrushTransform {
        origin: Vec3::new(-200.0, 500.0, 64.0),
        angles: Vec3::new(11.0, 37.0, -8.0),
    };
    let original = vertex(5.0, 9.0);
    let restored = inverse_transform_surface(transform_surface(original, transform), transform);
    assert!((restored.position - original.position).length() < 1e-4);
    assert!((restored.normal - original.normal).length() < 1e-5);
    assert_eq!(restored.lightmap_uv, original.lightmap_uv);
}

#[test]
fn quad_clipping_preserves_receiver_lightmap() {
    let triangle = [vertex(-5.0, -5.0), vertex(5.0, -5.0), vertex(0.0, 8.0)];
    let polygon = projection().clip_quad_to_triangle(&triangle);
    assert_eq!(polygon.len(), 4);
    for vertex in polygon {
        let p = vertex.surface.position;
        assert!((p[2] - (2.0 * p[0] + 3.0 * p[1])).abs() < 1e-5);
        assert!((vertex.surface.lightmap_uv[0] - (0.2 + p[0] * 0.01)).abs() < 1e-6);
        assert!((vertex.surface.lightmap_uv[1] - (0.4 + p[1] * 0.02)).abs() < 1e-6);
        assert!((vertex.uv[0] - (0.5 + p[0] * 0.25)).abs() < 1e-6);
        assert!((vertex.uv[1] - (1.0 + p[1])).abs() < 1e-6);
    }
}

#[test]
fn skew_quad_clipping_keeps_receiver_fan_boundaries_and_inverse_bilinear_uvs() {
    let mut projection = projection();
    projection.corners = [[-3.0, -1.0], [-1.0, 3.0], [4.0, 2.0], [2.0, -2.0]];
    let triangles = [
        [
            vertex(-10.0, -10.0),
            vertex(-10.0, 10.0),
            vertex(10.0, 10.0),
        ],
        [
            vertex(-10.0, -10.0),
            vertex(10.0, 10.0),
            vertex(10.0, -10.0),
        ],
    ];
    let mut intersections = Vec::new();
    for (side, triangle) in triangles.iter().enumerate() {
        let polygon = projection.clip_quad_to_triangle(triangle);
        assert!(polygon.len() >= 3);
        if side == 0 {
            assert!((polygon[0].surface.position[0] + 3.0).abs() < 1e-5);
            assert!((polygon[0].surface.position[1] + 1.0).abs() < 1e-5);
        } else {
            assert!((polygon[0].surface.position[0] - 7.0 / 3.0).abs() < 1e-5);
            assert!((polygon[0].surface.position[1] - 7.0 / 3.0).abs() < 1e-5);
        }
        for vertex in polygon {
            let p = vertex.surface.position;
            if side == 0 {
                assert!(p[1] >= p[0] - 1e-5);
            } else {
                assert!(p[1] <= p[0] + 1e-5);
            }
            let uv = [
                (vertex.uv[0] - projection.u[0]) / (projection.u[1] - projection.u[0]),
                (vertex.uv[1] - projection.v[0]) / (projection.v[1] - projection.v[0]),
            ];
            let point = std::array::from_fn::<_, 2, _>(|axis| {
                projection.corners[0][axis] * (1.0 - uv[0]) * (1.0 - uv[1])
                    + projection.corners[3][axis] * uv[0] * (1.0 - uv[1])
                    + projection.corners[1][axis] * (1.0 - uv[0]) * uv[1]
                    + projection.corners[2][axis] * uv[0] * uv[1]
            });
            assert!((point[0] - p[0]).abs() < 1e-5);
            assert!((point[1] - p[1]).abs() < 1e-5);
            assert!((vertex.surface.lightmap_uv[0] - (0.2 + p[0] * 0.01)).abs() < 1e-6);
            if (p[0] - p[1]).abs() < 1e-5 {
                intersections.push(p);
            }
        }
    }
    assert!(intersections.len() >= 4);
}
