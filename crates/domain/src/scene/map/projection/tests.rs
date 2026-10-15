use super::super::{MapMeshClusterRanges, MapMeshSurfaceOwner, MapMeshVisibility};
use super::*;

fn vertex(x: f32, y: f32) -> MapVertex {
    MapVertex {
        position: Vec3::new(x, y, 2.0 * x + 3.0 * y),
        normal: Vec3::new(-2.0, -3.0, 1.0),
        tangent_s: Vec3::new(1.0, 0.0, 2.0),
        tangent_t: Vec3::new(0.0, 1.0, 3.0),
        tex_s: x * 2.0,
        tex_t: y * 4.0,
        texture_size: [64, 32],
        lightmap_uv: [0.2 + x * 0.01, 0.4 + y * 0.02],
        has_lightmap: true,
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
        assert!(projected.surface.has_lightmap);
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
        displacement: false,
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
fn whole_face_clipping_preserves_quad_uvs_and_receiver_lightmap() {
    let world = [mesh()];
    let receivers = receiver_surfaces(&world, &[], &[]);
    let polygon = projection().clip_quad_to_receiver(&receivers[&7][0]);
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
fn uncut_skew_quad_keeps_authored_corner_uvs_without_receiver_diagonal_vertices() {
    let mut receiver = mesh();
    receiver.vertices = vec![
        vertex(-10.0, -10.0),
        vertex(-10.0, 10.0),
        vertex(10.0, 10.0),
        vertex(10.0, -10.0),
    ];
    receiver.indices = vec![0, 1, 2, 0, 2, 3];
    for cluster in &mut receiver.visibility.clusters {
        cluster.ranges[0].count = 6;
    }
    let world = [receiver];
    let receivers = receiver_surfaces(&world, &[], &[]);
    let mut projection = projection();
    projection.corners = [[-3.0, -1.0], [-1.0, 3.0], [4.0, 2.0], [2.0, -2.0]];
    let polygon = projection.clip_quad_to_receiver(&receivers[&7][0]);
    assert_eq!(polygon.len(), 4);
    assert_eq!(
        polygon.iter().map(|vertex| vertex.uv).collect::<Vec<_>>(),
        vec![[0.25, 0.0], [0.25, 2.0], [0.75, 2.0], [0.75, 0.0]]
    );
}
