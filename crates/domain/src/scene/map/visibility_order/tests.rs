use super::*;
use crate::scene::map::{MapLeaf, MapNode, MapPlane};

fn fixture() -> (FaceOrdering, MapLeafLocator) {
    let ordering = FaceOrdering {
        root: 0,
        models: Vec::new(),
        displacements: Vec::new(),
        node_faces: std::iter::once(0..2).collect(),
        node_bounds: vec![],
        child_bounds_contained: vec![],
        leaf_bounds: vec![],
        leaf_solid: vec![],
        leaf_underwater: vec![],
        leaf_displacements: vec![],
        leaf_ranges: vec![0..4, 0..4],
        leaf_faces: vec![0, 1, 2, 3],
        faces: vec![
            Face {
                plane: 0,
                back: false,
                node: true,
                underwater_membership: false,
                displacement: None,
                material_group: super::super::MapWorldMaterialGroup::AboveWater,
            },
            Face {
                plane: 0,
                back: true,
                node: true,
                underwater_membership: false,
                displacement: None,
                material_group: super::super::MapWorldMaterialGroup::AboveWater,
            },
            Face {
                plane: 1,
                back: false,
                node: false,
                underwater_membership: false,
                displacement: None,
                material_group: super::super::MapWorldMaterialGroup::AboveWater,
            },
            Face {
                plane: 1,
                back: false,
                node: false,
                underwater_membership: false,
                displacement: Some(0),
                material_group: super::super::MapWorldMaterialGroup::AboveWater,
            },
        ],
    };
    let leaf = MapLeaf {
        cluster: 0,
        mins: [-100; 3],
        maxs: [100; 3],
    };
    let locator = MapLeafLocator {
        planes: vec![
            MapPlane {
                normal: Vec3::new(1.0, 0.0, 0.0),
                dist: 0.0,
            },
            MapPlane {
                normal: Vec3::new(0.0, 0.0, 1.0),
                dist: 0.0,
            },
        ],
        nodes: vec![MapNode {
            plane_index: 0,
            children: [-1, -2],
        }],
        leaves: vec![leaf, leaf],
    };
    (ordering, locator)
}

#[test]
fn near_leaf_marks_node_faces_and_emits_displacements_before_details() {
    let (ordering, locator) = fixture();
    assert_eq!(
        ordering.ordered_faces(
            &locator,
            Vec3::new(10.0, 0.0, 10.0),
            |_| true,
            |_| true,
            |_| true
        ),
        [3, 2, 0]
    );
    assert_eq!(
        ordering.ordered_faces(
            &locator,
            Vec3::new(-10.0, 0.0, 10.0),
            |_| true,
            |_| true,
            |_| true
        ),
        [3, 2, 1]
    );
}

#[test]
fn underwater_node_faces_remain_admitted_from_both_sides() {
    let (mut ordering, locator) = fixture();
    ordering.faces[0].underwater_membership = true;
    ordering.faces[1].underwater_membership = true;
    for x in [-10.0, -0.001, 0.0, 0.001, 10.0] {
        assert_eq!(
            ordering.ordered_faces(
                &locator,
                Vec3::new(x, 0.0, 10.0),
                |_| true,
                |_| true,
                |_| true,
            ),
            [3, 2, 0, 1]
        );
    }
}

#[test]
fn underwater_node_admission_still_requires_visible_leaf_membership() {
    let (mut ordering, mut locator) = fixture();
    ordering.faces[1].underwater_membership = true;
    ordering.leaf_ranges = vec![0..1, 1..2];
    ordering.leaf_faces = vec![0, 1];
    locator.leaves[1].cluster = 1;
    assert_eq!(
        ordering.ordered_faces(
            &locator,
            Vec3::new(10.0, 0.0, 10.0),
            |_| true,
            |_| true,
            |cluster| cluster == 0,
        ),
        [0]
    );
    ordering.leaf_ranges[0] = 0..2;
    assert_eq!(
        ordering.ordered_faces(
            &locator,
            Vec3::new(10.0, 0.0, 10.0),
            |face| face != 1,
            |_| true,
            |_| true,
        ),
        [0]
    );
}

#[test]
fn underwater_membership_does_not_bypass_detail_plane_facing() {
    let (mut ordering, locator) = fixture();
    ordering.faces[2].underwater_membership = true;
    assert_eq!(
        ordering.ordered_faces(
            &locator,
            Vec3::new(10.0, 0.0, -0.02),
            |_| true,
            |_| true,
            |_| true,
        ),
        [3, 0]
    );
}

#[test]
fn receiver_visibility_filters_both_leaf_and_node_surfaces() {
    let (ordering, locator) = fixture();
    assert_eq!(
        ordering.ordered_faces(
            &locator,
            Vec3::new(10.0, 0.0, 10.0),
            |face| face == 0,
            |_| true,
            |_| true
        ),
        [0]
    );
}

#[test]
fn cyclic_nodes_terminate_without_duplicate_faces() {
    let (ordering, mut locator) = fixture();
    locator.nodes[0].children[1] = 0;
    assert_eq!(
        ordering.ordered_faces(
            &locator,
            Vec3::new(10.0, 0.0, 10.0),
            |_| true,
            |_| true,
            |_| true
        ),
        [3, 2, 0]
    );
}

#[test]
fn rejected_node_bounds_do_not_prime_surface_chains() {
    let (mut ordering, locator) = fixture();
    ordering.node_bounds.push(crate::scene::map::MapBounds {
        min: Vec3::ZERO,
        max: Vec3::splat(1.0),
    });
    assert!(
        ordering
            .ordered_faces(&locator, Vec3::splat(10.0), |_| true, |_| false, |_| true)
            .is_empty()
    );
}

#[test]
fn displacements_not_in_leaf_face_lump_still_enter_the_queue_once() {
    let (mut ordering, locator) = fixture();
    ordering.leaf_faces.retain(|&face| face != 3);
    ordering.leaf_ranges = vec![0..3, 0..3];
    ordering.leaf_displacements = vec![vec![3], vec![3]];
    assert_eq!(
        ordering.ordered_faces(
            &locator,
            Vec3::new(10.0, 0.0, 10.0),
            |_| true,
            |_| true,
            |_| true
        ),
        [3, 2, 0]
    );
}

#[test]
fn solid_leaves_do_not_emit_or_mark_receivers() {
    let (mut ordering, locator) = fixture();
    ordering.leaf_solid = vec![true, true];
    ordering.leaf_displacements = vec![vec![3], vec![3]];
    assert!(
        ordering
            .ordered_faces(&locator, Vec3::splat(10.0), |_| true, |_| true, |_| true)
            .is_empty()
    );
}

#[test]
fn hidden_leaf_clusters_do_not_prime_material_chains() {
    let (mut ordering, mut locator) = fixture();
    locator.leaves[1].cluster = 1;
    ordering.leaf_ranges = vec![0..2, 2..4];
    ordering.leaf_faces = vec![0, 2, 1, 3];
    assert_eq!(
        ordering.ordered_faces(
            &locator,
            Vec3::new(-10.0, 0.0, 10.0),
            |_| true,
            |_| true,
            |cluster| cluster == 0,
        ),
        [2]
    );
}

#[test]
fn decal_creation_visits_node_faces_then_fixed_child_order_without_facing_filter() {
    let (mut ordering, locator) = fixture();
    ordering.leaf_displacements = vec![vec![3], vec![3]];
    for origin in [Vec3::new(1.0, 0.0, 0.0), Vec3::new(-1.0, 0.0, 0.0)] {
        let mut created = Vec::new();
        ordering
            .visit_decal_faces(&locator, 0, origin, 8.0, |face| {
                created.push(face);
                Ok::<_, ()>(true)
            })
            .unwrap();
        assert_eq!(created, [0, 1, 2, 3]);
    }
}

#[test]
fn decal_creation_prunes_by_radius_and_retries_unsuccessful_leaf_projection() {
    let (mut ordering, locator) = fixture();
    ordering.node_faces = std::iter::once(0..0).collect();
    ordering.leaf_ranges = vec![0..1, 1..3];
    ordering.leaf_faces = vec![2, 2, 0];
    for (x, radius, expected) in [
        (5.0, 4.0, vec![2]),
        (-5.0, 4.0, vec![2]),
        (1.0, 4.0, vec![2, 2, 0]),
        (-1.0, 4.0, vec![2, 2, 0]),
    ] {
        let mut attempts = Vec::new();
        ordering
            .visit_decal_faces(&locator, 0, Vec3::new(x, 0.0, 0.0), radius, |face| {
                attempts.push(face);
                Ok::<_, ()>(face != 2)
            })
            .unwrap();
        assert_eq!(attempts, expected);
    }
}

#[test]
fn decal_creation_keeps_strict_plane_distance_and_accepts_zero_radius() {
    let (mut ordering, locator) = fixture();
    ordering.leaf_ranges.clear();
    for (x, radius, expected) in [
        (-4.0, 8.0, vec![]),
        (4.0, 8.0, vec![]),
        (-3.99, 8.0, vec![0, 1]),
        (3.99, 8.0, vec![0, 1]),
        (0.0, 0.0, vec![0, 1]),
        (0.01, 0.0, vec![]),
    ] {
        let mut created = Vec::new();
        ordering
            .visit_decal_faces(&locator, 0, Vec3::new(x, 0.0, 0.0), radius, |face| {
                created.push(face);
                Ok::<_, ()>(true)
            })
            .unwrap();
        assert_eq!(created, expected);
    }
}

#[test]
fn decal_creation_from_model_leaf_ignores_world_root_and_terminates_cycles() {
    let (ordering, mut locator) = fixture();
    let mut created = Vec::new();
    ordering
        .visit_decal_faces(&locator, -2, Vec3::ZERO, 8.0, |face| {
            created.push(face);
            Ok::<_, ()>(true)
        })
        .unwrap();
    assert_eq!(created, [0, 1, 2]);
    locator.nodes[0].children[1] = 0;
    created.clear();
    ordering
        .visit_decal_faces(&locator, 0, Vec3::ZERO, 8.0, |face| {
            created.push(face);
            Ok::<_, ()>(true)
        })
        .unwrap();
    assert_eq!(created, [0, 1, 2]);
}

#[test]
fn displacement_leaf_creation_follows_compiled_displacement_ordinal() {
    let mut bsp = crate::scene::map::tests::brush_collision_bsp("");
    bsp.displacements = vec![
        crate::scene::map::tests::displacement_test_bsp(2, [Vec3::ZERO; 4], 0)
            .displacements[0];
        3
    ];
    let mut face = bsp.faces[0];
    face.displacement = 2;
    bsp.faces = vec![face; 3];
    bsp.faces[1].displacement = 0;
    bsp.faces[2].displacement = 1;
    let bounds = super::super::super::MapBounds {
        min: Vec3::splat(-1.0),
        max: Vec3::splat(1.0),
    };
    let ordering = FaceOrdering::from_bsp(
        &bsp,
        &MapLeafLocator::from_bsp(&bsp),
        [0, 1, 2].into_iter().map(|face| (face, bounds)),
    );
    assert_eq!(
        ordering
            .displacement_surfaces()
            .map(|(face, descriptor)| (face, descriptor.ordinal))
            .collect::<Vec<_>>(),
        [(0, 2), (1, 0), (2, 1)]
    );
    assert!(
        ordering
            .leaf_displacements
            .iter()
            .any(|faces| faces == &[1, 2, 0])
    );
    for faces in ordering
        .leaf_displacements
        .iter()
        .filter(|faces| !faces.is_empty())
    {
        assert_eq!(faces, &[1, 2, 0]);
    }
}

#[test]
fn decal_projection_handles_share_visibility_topology_without_copying_vis_rows() {
    let bsp = crate::scene::map::tests::brush_collision_bsp("");
    let visibility = super::super::MapVisibility::from_bsp(&bsp, std::iter::empty()).unwrap();
    let first = visibility.decal_projection(0, Vec3::ZERO);
    let second = visibility.decal_projection(-1, Vec3::splat(2.0));
    assert!(std::sync::Arc::ptr_eq(&first.locator, &visibility.locator));
    assert!(std::sync::Arc::ptr_eq(
        &first.ordering,
        &visibility.ordering
    ));
    assert!(std::sync::Arc::ptr_eq(&first.locator, &second.locator));
    assert!(std::sync::Arc::ptr_eq(&first.ordering, &second.ordering));
    assert_ne!(first.origin, second.origin);
    assert_ne!(first.root, second.root);
}

#[test]
fn detail_faces_use_oriented_face_planes_without_applying_node_side_again() {
    let (mut ordering, locator) = fixture();
    ordering.faces[2].back = true;
    assert!(
        ordering
            .ordered_faces(
                &locator,
                Vec3::new(10.0, 0.0, 10.0),
                |_| true,
                |_| true,
                |_| true
            )
            .contains(&2)
    );
    assert!(
        !ordering
            .ordered_faces(
                &locator,
                Vec3::new(10.0, 0.0, -10.0),
                |_| true,
                |_| true,
                |_| true
            )
            .contains(&2)
    );
    assert!(
        ordering
            .ordered_faces(
                &locator,
                Vec3::new(10.0, 0.0, -0.01),
                |_| true,
                |_| true,
                |_| true
            )
            .contains(&2)
    );
}

#[test]
fn brush_groups_visit_far_node_then_facing_node_faces_then_near_node() {
    let (mut ordering, mut locator) = fixture();
    locator.nodes = vec![
        MapNode {
            plane_index: 0,
            children: [1, 2],
        },
        MapNode {
            plane_index: 0,
            children: [-1, -2],
        },
        MapNode {
            plane_index: 0,
            children: [-1, -2],
        },
    ];
    ordering.node_faces = vec![0..2, 2..3, 3..4];
    let model = BrushModel {
        root: 0,
        bounds: super::super::super::MapBounds {
            min: Vec3::splat(-100.0),
            max: Vec3::splat(100.0),
        },
        faces: 0..4,
    };
    let faces = |eye| {
        ordering
            .brush_surface_groups(&locator, &model, eye, |_| false)
            .into_iter()
            .map(|group| group.faces)
            .collect::<Vec<_>>()
    };
    assert_eq!(faces(Vec3::new(1.0, 0.0, 0.0)), [vec![3], vec![0], vec![2]]);
    assert_eq!(faces(-Vec3::new(1.0, 0.0, 0.0)), [vec![1]]);
    assert_eq!(faces(Vec3::ZERO), [vec![3], vec![0], vec![2]]);
    let groups =
        ordering.brush_surface_groups(&locator, &model, -Vec3::new(1.0, 0.0, 0.0), |face| {
            face == 2 || face == 3
        });
    assert_eq!(
        groups
            .into_iter()
            .map(|group| group.faces)
            .collect::<Vec<_>>(),
        [vec![2], vec![1], vec![3]]
    );
}

#[test]
fn brush_groups_bound_cycles_invalid_children_and_foreign_model_faces() {
    let (mut ordering, mut locator) = fixture();
    locator.nodes[0].children = [0, i32::MAX];
    ordering.node_faces[0] = 0..6;
    let mut model = BrushModel {
        root: 0,
        bounds: super::super::super::MapBounds {
            min: Vec3::splat(-1.0),
            max: Vec3::splat(1.0),
        },
        faces: 0..2,
    };
    assert_eq!(
        ordering.brush_surface_groups(&locator, &model, Vec3::new(1.0, 0.0, 0.0), |_| true),
        [super::super::MapBrushSurfaceGroup { faces: vec![0, 1] }]
    );
    model.root = -1;
    assert!(
        ordering
            .brush_surface_groups(&locator, &model, Vec3::new(1.0, 0.0, 0.0), |_| true)
            .is_empty()
    );
    model.root = 0;
    assert!(
        ordering
            .brush_surface_groups(&locator, &model, Vec3::splat(f32::NAN), |_| true)
            .is_empty()
    );
}

#[test]
fn brush_rendering_handles_share_topology_and_keep_compiled_bounds() {
    let mut bsp = crate::scene::map::tests::brush_collision_bsp("");
    bsp.models.truncate(1);
    bsp.models[0].mins = [-73.0, -41.0, -19.0];
    bsp.models[0].maxs = [11.0, 97.0, 109.0];
    let mut other = bsp.models[0];
    other.head_node = -1;
    bsp.models.push(other);
    let visibility = super::super::MapVisibility::from_bsp(&bsp, std::iter::empty()).unwrap();
    let first = visibility
        .brush_model(crate::scene::map::ModelIndex::new(0))
        .unwrap();
    let second = visibility
        .brush_model(crate::scene::map::ModelIndex::new(1))
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&first.locator, &visibility.locator));
    assert!(std::sync::Arc::ptr_eq(
        &first.ordering,
        &visibility.ordering
    ));
    assert!(std::sync::Arc::ptr_eq(&first.locator, &second.locator));
    assert!(std::sync::Arc::ptr_eq(&first.ordering, &second.ordering));
    assert_eq!(first.bounds().min, Vec3::new(-73.0, -41.0, -19.0));
    assert_eq!(first.bounds().max, Vec3::new(11.0, 97.0, 109.0));
    assert!(second.translucent_groups(Vec3::ZERO).is_empty());
    assert!(
        visibility
            .brush_model(crate::scene::map::ModelIndex::new(2))
            .is_none()
    );
    assert_ne!(first.model.root, second.model.root);
}

#[test]
fn brush_rendering_rejects_invalid_model_bounds_and_face_ranges() {
    let mut bsp = crate::scene::map::tests::brush_collision_bsp("");
    bsp.models.truncate(1);
    let model = bsp.models[0];
    bsp.models[0].mins[0] = f32::NAN;
    bsp.models.push(model);
    bsp.models[1].maxs[0] = bsp.models[1].mins[0] - 1.0;
    bsp.models.push(model);
    bsp.models[2].first_face = -1;
    let visibility = super::super::MapVisibility::from_bsp(&bsp, std::iter::empty()).unwrap();
    for index in 0..3 {
        assert!(
            visibility
                .brush_model(crate::scene::map::ModelIndex::new(index))
                .is_none()
        );
    }
}

fn world_leaf_fixture() -> (FaceOrdering, MapLeafLocator) {
    let (mut ordering, mut locator) = fixture();
    locator.nodes[0].children = [1, -3];
    locator.nodes.push(MapNode {
        plane_index: 1,
        children: [-1, -2],
    });
    locator.leaves.push(locator.leaves[0]);
    ordering.node_faces = vec![0..2, 2..3];
    ordering.faces[2].node = true;
    ordering.leaf_faces = vec![0, 1, 2, 0, 1];
    ordering.leaf_ranges = vec![0..3, 3..3, 3..5];
    ordering.node_bounds = vec![
        super::super::super::MapBounds {
            min: Vec3::splat(-100.0),
            max: Vec3::splat(100.0),
        };
        2
    ];
    ordering.leaf_solid = vec![false; 3];
    (ordering, locator)
}

#[test]
fn world_node_faces_attach_to_the_last_visited_near_subtree_leaf() {
    let (ordering, locator) = world_leaf_fixture();
    let eye = Vec3::new(1.0, 0.0, 1.0);
    let groups = ordering.world_surface_groups(&locator, eye, |_| true, |_| true, |_| true);
    assert_eq!(
        groups,
        [
            super::super::MapWorldSurfaceGroup {
                leaf_index: 0,
                faces: vec![2],
            },
            super::super::MapWorldSurfaceGroup {
                leaf_index: 1,
                faces: vec![0],
            },
            super::super::MapWorldSurfaceGroup {
                leaf_index: 2,
                faces: vec![],
            },
        ]
    );
    assert_eq!(
        groups
            .iter()
            .flat_map(|group| group.faces.iter().copied())
            .collect::<Vec<_>>(),
        ordering.ordered_faces(&locator, eye, |_| true, |_| true, |_| true)
    );
    assert_eq!(
        ordering
            .world_surface_groups(&locator, -eye, |_| true, |_| true, |_| true)
            .into_iter()
            .map(|group| group.leaf_index)
            .collect::<Vec<_>>(),
        [2, 1, 0]
    );
}

#[test]
fn empty_world_leaves_remain_available_for_transparent_owner_assignment() {
    let (ordering, locator) = world_leaf_fixture();
    let groups = ordering.world_surface_groups(
        &locator,
        Vec3::new(1.0, 0.0, 1.0),
        |_| false,
        |_| true,
        |_| true,
    );
    assert_eq!(
        groups
            .iter()
            .map(|group| group.leaf_index)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert!(groups.iter().all(|group| group.faces.is_empty()));
}

#[test]
fn world_surface_groups_keep_leaf_displacement_and_detail_encounters_for_reverse_draw() {
    let (ordering, locator) = fixture();
    let groups = ordering.world_surface_groups(
        &locator,
        Vec3::new(10.0, 0.0, 10.0),
        |_| true,
        |_| true,
        |_| true,
    );
    assert_eq!(groups[0].faces, [3, 2, 0]);
    assert_eq!(
        groups[0].faces.iter().rev().copied().collect::<Vec<_>>(),
        [0, 2, 3]
    );
    assert!(groups[1].faces.is_empty());
}

#[test]
fn world_leaf_queries_use_the_compiled_world_headnode_and_shared_topology() {
    let (mut ordering, mut locator) = world_leaf_fixture();
    ordering.root = 1;
    locator.nodes[0].plane_index = i32::MAX;
    let visibility = super::super::MapVisibility {
        cluster_count: 1,
        locator: std::sync::Arc::new(locator),
        ordering: std::sync::Arc::new(ordering),
        vis: None,
    };
    assert_eq!(
        visibility
            .leaves_for_aabb(super::super::super::MapBounds {
                min: Vec3::splat(-1.0),
                max: Vec3::splat(1.0),
            })
            .unwrap()
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from([0, 1])
    );
    assert_eq!(
        visibility
            .world_surface_groups_in_view(Vec3::splat(1.0), |_| false, |_| true, |_| true)
            .into_iter()
            .map(|group| group.leaf_index)
            .collect::<Vec<_>>(),
        [0, 1]
    );
}

#[test]
fn owner_leaf_bounds_use_strict_source_plane_overlap_including_zero_extent() {
    let (mut ordering, locator) = fixture();
    ordering.node_bounds = vec![super::super::super::MapBounds {
        min: Vec3::splat(-100.0),
        max: Vec3::splat(100.0),
    }];
    ordering.leaf_solid = vec![false; 2];
    for (min, max, expected) in [
        (-2.0, -1.0, vec![1]),
        (-1.0, 0.0, vec![1]),
        (0.0, 0.0, vec![1]),
        (0.0, 1.0, vec![0]),
        (-1.0, 1.0, vec![0, 1]),
    ] {
        let bounds = super::super::super::MapBounds {
            min: Vec3::new(min, -1.0, -1.0),
            max: Vec3::new(max, 1.0, 1.0),
        };
        let mut actual = ordering.leaves_for_aabb(&locator, bounds).unwrap();
        actual.sort_unstable();
        assert_eq!(actual, expected, "{bounds:?}");
    }
}

#[test]
fn owner_leaf_bounds_project_oblique_planes_and_preserve_enumerator_child_order() {
    let (mut ordering, mut locator) = fixture();
    ordering.node_bounds = vec![super::super::super::MapBounds {
        min: Vec3::splat(-100.0),
        max: Vec3::splat(100.0),
    }];
    ordering.leaf_solid = vec![false; 2];
    locator.planes[0].normal = Vec3::new(-1.0, 1.0, 0.0).normalize().unwrap();
    for (center, expected) in [
        (Vec3::new(-1.0, 1.0, 0.0), vec![0]),
        (Vec3::new(1.0, -1.0, 0.0), vec![1]),
        (Vec3::ZERO, vec![0, 1]),
    ] {
        assert_eq!(
            ordering
                .leaves_for_aabb(
                    &locator,
                    super::super::super::MapBounds {
                        min: center - Vec3::splat(0.1),
                        max: center + Vec3::splat(0.1),
                    }
                )
                .unwrap(),
            expected,
        );
    }
}

#[test]
fn owner_leaf_queries_reject_malformed_graphs_and_prune_solid_or_disjoint_bounds() {
    let (mut ordering, mut locator) = world_leaf_fixture();
    let bounds = super::super::super::MapBounds {
        min: Vec3::splat(-1.0),
        max: Vec3::splat(1.0),
    };
    assert!(
        ordering
            .leaves_for_aabb(
                &locator,
                super::super::super::MapBounds {
                    min: Vec3::splat(101.0),
                    max: Vec3::splat(102.0),
                }
            )
            .unwrap()
            .is_empty()
    );
    ordering.leaf_solid.fill(true);
    assert!(
        ordering
            .leaves_for_aabb(&locator, bounds)
            .unwrap()
            .is_empty()
    );
    ordering.leaf_solid.fill(false);
    locator.nodes[0].children[1] = 0;
    assert!(ordering.leaves_for_aabb(&locator, bounds).is_none());
    locator.nodes[0].children[1] = i32::MAX;
    assert!(ordering.leaves_for_aabb(&locator, bounds).is_none());
    locator.nodes[0].children[1] = -999;
    assert!(ordering.leaves_for_aabb(&locator, bounds).is_none());
    assert!(
        ordering
            .leaves_for_aabb(
                &locator,
                super::super::super::MapBounds {
                    min: Vec3::splat(f32::NAN),
                    max: Vec3::splat(1.0),
                }
            )
            .is_none()
    );
    assert!(
        ordering
            .leaves_for_aabb(
                &locator,
                super::super::super::MapBounds {
                    min: Vec3::splat(1.0),
                    max: Vec3::splat(-1.0),
                }
            )
            .is_none()
    );
}

fn material_group_bsp() -> crate::scene::map::MapBsp {
    let mut bsp = crate::scene::map::tests::brush_collision_bsp("");
    bsp.displacements =
        crate::scene::map::tests::displacement_test_bsp(2, [Vec3::ZERO; 4], 0).displacements;
    let mut face = bsp.faces[0];
    face.texinfo = 0;
    face.displacement = -1;
    bsp.faces = vec![face; 6];
    bsp.faces[3].texinfo = 1;
    bsp.faces[4].displacement = 0;
    let mut texinfo = bsp.texinfos[0];
    texinfo.flags = 0;
    bsp.texinfos = vec![texinfo; 2];
    bsp.texinfos[1].flags = crate::scene::map::texture_flags::WARP;
    bsp.planes[0].normal = [1.0, 0.0, 0.0];
    bsp.planes[0].dist = 0.0;
    let mut node = bsp.nodes[0];
    node.plane = 0;
    node.children = [-1, -2];
    node.mins = [-100; 3];
    node.maxs = [100; 3];
    node.face_count = 0;
    bsp.nodes = vec![node];
    let mut leaf = bsp.leaves[0];
    leaf.contents = 0;
    leaf.cluster = 0;
    leaf.mins = [-100; 3];
    leaf.maxs = [100; 3];
    bsp.leaves = vec![leaf; 3];
    bsp.leaves[0].first_leaf_face = 0;
    bsp.leaves[0].leaf_face_count = 4;
    bsp.leaves[0].leaf_water_data = -1;
    bsp.leaves[0].mins[0] = 0;
    bsp.leaves[1].first_leaf_face = 4;
    bsp.leaves[1].leaf_face_count = 4;
    bsp.leaves[1].leaf_water_data = 0;
    bsp.leaves[1].maxs[0] = 0;
    bsp.leaves[2].first_leaf_face = 8;
    bsp.leaves[2].leaf_face_count = 2;
    bsp.leaves[2].leaf_water_data = 0;
    bsp.leaf_faces = vec![0, 2, 3, 4, 1, 2, 3, 4, 0, 5];
    bsp.models[0].head_node = 0;
    bsp.models[0].first_face = 0;
    bsp.models[0].face_count = 6;
    bsp
}

#[test]
fn world_material_groups_use_all_reachable_leaf_memberships_and_raw_warp_override() {
    use super::super::MapWorldMaterialGroup::*;
    let bsp = material_group_bsp();
    let bounds = super::super::super::MapBounds {
        min: Vec3::new(10.0, -1.0, -1.0),
        max: Vec3::new(20.0, 1.0, 1.0),
    };
    let visibility =
        super::super::MapVisibility::from_bsp(&bsp, std::iter::once((4, bounds))).unwrap();
    let rendering = (0..6)
        .map(|face| visibility.surface_rendering(face).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        rendering
            .iter()
            .map(|surface| surface.material_group)
            .collect::<Vec<_>>(),
        [
            AboveWater,
            Underwater,
            IntersectsWater,
            Water,
            AboveWater,
            AboveWater
        ]
    );
    assert_eq!(rendering[4].displacement.unwrap().ordinal, 0);
    assert!(
        rendering
            .iter()
            .enumerate()
            .all(|(index, surface)| surface.displacement.is_some() == (index == 4))
    );
    assert!(visibility.surface_rendering(6).is_none());
    assert_eq!(
        [AboveWater, Underwater, IntersectsWater, Water]
            .map(super::super::MapWorldMaterialGroup::index),
        [0, 1, 2, 3]
    );
}

#[test]
fn node_facing_uses_underwater_membership_independently_of_water_group() {
    let mut bsp = material_group_bsp();
    bsp.nodes[0].face_count = 4;
    for face in &mut bsp.faces[..4] {
        face.on_node = 1;
        face.plane = 0;
        face.side = 1;
    }
    let locator = MapLeafLocator::from_bsp(&bsp);
    let ordering = FaceOrdering::from_bsp(&bsp, &locator, std::iter::empty());
    assert_eq!(
        ordering.faces[..4]
            .iter()
            .map(|face| face.underwater_membership)
            .collect::<Vec<_>>(),
        [false, true, true, false]
    );
    assert_eq!(
        ordering.ordered_faces(
            &locator,
            Vec3::new(10.0, 0.0, 10.0),
            |face| face < 4,
            |_| true,
            |_| true,
        ),
        [2]
    );
    assert_eq!(
        ordering.faces[3].material_group,
        super::super::MapWorldMaterialGroup::Water
    );
}

#[test]
fn displacement_material_groups_use_actual_bounds_links_instead_of_planar_mark_surfaces() {
    use super::super::MapWorldMaterialGroup::*;
    let bsp = material_group_bsp();
    for (min, max, expected) in [
        (-20.0, -10.0, Underwater),
        (-10.0, 0.0, Underwater),
        (0.0, 10.0, AboveWater),
        (-10.0, 10.0, IntersectsWater),
    ] {
        let bounds = super::super::super::MapBounds {
            min: Vec3::new(min, -1.0, -1.0),
            max: Vec3::new(max, 1.0, 1.0),
        };
        let visibility =
            super::super::MapVisibility::from_bsp(&bsp, std::iter::once((4, bounds))).unwrap();
        assert_eq!(
            visibility.surface_rendering(4).unwrap().material_group,
            expected,
            "{bounds:?}"
        );
    }
}

#[test]
fn deformed_displacement_enters_visible_leaf_outside_its_parent_face_membership() {
    let topology = material_group_bsp();
    let mut bsp = crate::scene::map::tests::displacement_test_bsp(
        2,
        [
            Vec3::new(10.0, -10.0, 0.0),
            Vec3::new(10.0, 10.0, 0.0),
            Vec3::new(20.0, 10.0, 0.0),
            Vec3::new(20.0, -10.0, 0.0),
        ],
        0,
    );
    bsp.nodes = topology.nodes;
    bsp.planes = topology.planes;
    bsp.models = topology.models;
    bsp.models[0].head_node = 0;
    bsp.leaves = topology.leaves[..2].to_vec();
    bsp.leaves[0].cluster = 0;
    bsp.leaves[0].first_leaf_face = 0;
    bsp.leaves[0].leaf_face_count = 1;
    bsp.leaves[1].cluster = 1;
    bsp.leaves[1].first_leaf_face = 1;
    bsp.leaves[1].leaf_face_count = 0;
    bsp.leaf_faces = vec![0];
    for vertex in &mut bsp.displacement_verts {
        vertex.vector = [-1.0, 0.0, 0.0];
        vertex.dist = 40.0;
    }
    let grid =
        crate::scene::map::mesh::displacement_grid(&bsp, &bsp.faces[0], &bsp.displacements[0], 4)
            .unwrap();
    let bounds =
        crate::scene::map::bounds_from_points_iter(grid.iter().map(|vertex| vertex.position))
            .unwrap();
    assert!(bsp.vertices.iter().all(|point| point[0] > 0.0));
    assert!(bounds.max[0] < 0.0);
    let visibility =
        super::super::MapVisibility::from_bsp(&bsp, [(0, bounds)].into_iter()).unwrap();
    let groups = visibility.world_surface_groups_in_view(
        Vec3::new(-25.0, 0.0, 10.0),
        |_| true,
        |_| true,
        |cluster| cluster == 1,
    );
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].leaf_index, 1);
    assert_eq!(groups[0].faces, [0]);
}

#[test]
fn world_material_groups_ignore_solid_leaves_and_remain_independent_of_view_culling() {
    use super::super::MapWorldMaterialGroup::*;
    let mut bsp = material_group_bsp();
    bsp.leaves[1].contents = crate::scene::map::contents_flags::SOLID;
    let visibility = super::super::MapVisibility::from_bsp(&bsp, std::iter::empty()).unwrap();
    assert_eq!(
        visibility.surface_rendering(1).unwrap().material_group,
        AboveWater
    );
    assert_eq!(
        visibility.surface_rendering(2).unwrap().material_group,
        AboveWater
    );
    assert_eq!(
        visibility.surface_rendering(3).unwrap().material_group,
        Water
    );
    assert!(
        visibility
            .world_surface_groups_in_view(Vec3::splat(1.0), |_| false, |_| false, |_| false)
            .is_empty()
    );
    assert_eq!(
        visibility.surface_rendering(2).unwrap().material_group,
        AboveWater
    );
}

#[test]
fn mixed_leaf_contents_remain_in_world_groups_and_owner_spatial_queries() {
    let mut bsp = material_group_bsp();
    bsp.leaves[1].contents =
        crate::scene::map::contents_flags::SOLID | crate::scene::map::contents_flags::WINDOW;
    let visibility = super::super::MapVisibility::from_bsp(&bsp, std::iter::empty()).unwrap();
    assert_eq!(
        visibility.surface_rendering(1).unwrap().material_group,
        super::super::MapWorldMaterialGroup::Underwater,
    );
    assert_eq!(
        visibility.leaves_for_aabb(super::super::super::MapBounds {
            min: Vec3::new(-10.0, -1.0, -1.0),
            max: Vec3::new(-5.0, 1.0, 1.0),
        }),
        Some(vec![1]),
    );
    assert!(
        visibility
            .world_surface_groups_in_view(Vec3::splat(1.0), |_| false, |_| true, |_| true)
            .iter()
            .any(|group| group.leaf_index == 1)
    );
}

#[test]
fn displacement_registry_counts_use_full_raw_grid_even_when_allowed_vertices_are_empty() {
    let mut bsp = material_group_bsp();
    bsp.displacements[0].allowed_vertices.fill(0);
    for (power, vertices, indices) in [(2, 25, 96), (3, 81, 384), (4, 289, 1536)] {
        bsp.displacements[0].power = power;
        let visibility = super::super::MapVisibility::from_bsp(&bsp, std::iter::empty()).unwrap();
        let expected = super::super::MapDisplacementRendering {
            ordinal: 0,
            full_grid_vertices: vertices,
            full_grid_indices: indices,
        };
        assert_eq!(
            visibility.surface_rendering(4).unwrap().displacement,
            Some(expected)
        );
        assert_eq!(
            visibility.displacement_surfaces().collect::<Vec<_>>(),
            [(4, expected)]
        );
    }
    bsp.displacements[0].power = i32::MAX;
    let visibility = super::super::MapVisibility::from_bsp(&bsp, std::iter::empty()).unwrap();
    assert!(
        visibility
            .surface_rendering(4)
            .unwrap()
            .displacement
            .is_none()
    );
    assert!(visibility.displacement_surfaces().next().is_none());
    for power in [-1, 16, 31, 32, i32::MAX] {
        assert!(DisplacementGridSize::from_power(power).is_none());
    }
}

#[test]
fn optical_leaf_medium_uses_compiled_water_data_instead_of_contents() {
    let mut bsp = material_group_bsp();
    bsp.leaves[0].contents = crate::scene::map::contents_flags::WATER;
    bsp.leaves[0].leaf_water_data = -1;
    bsp.leaves[1].contents = 0;
    bsp.leaves[1].leaf_water_data = 0;
    bsp.leaves[2].leaf_water_data = 7;
    let visibility = super::super::MapVisibility::from_bsp(&bsp, std::iter::empty()).unwrap();
    assert_eq!(visibility.leaf_is_underwater(0), Some(false));
    assert_eq!(visibility.leaf_is_underwater(1), Some(true));
    assert_eq!(visibility.leaf_is_underwater(2), Some(true));
    assert_eq!(visibility.leaf_is_underwater(3), None);
}

#[test]
fn inherited_plane_masks_preserve_face_order_and_empty_leaves() {
    use std::cell::RefCell;
    let (mut ordering, locator) = fixture();
    let bounds = super::super::super::MapBounds {
        min: Vec3::splat(-100.0),
        max: Vec3::splat(100.0),
    };
    ordering.node_bounds = vec![bounds];
    ordering.leaf_bounds = vec![bounds; 2];
    ordering.validate_bounds_hierarchy(&locator);
    assert_eq!(ordering.child_bounds_contained, [[true, true]]);
    for x in [-10.0, 10.0] {
        for include_faces in [false, true] {
            let observed = RefCell::new(Vec::new());
            let masked = ordering.world_surface_groups_with_culling(
                &locator,
                Vec3::new(x, 0.0, 10.0),
                |_| include_faces,
                |_, mask| {
                    observed.borrow_mut().push(mask);
                    MapBoundsCulling::Visible(MapBoundsCullMask::NONE)
                },
                |_| true,
            );
            let legacy = ordering.world_surface_groups(
                &locator,
                Vec3::new(x, 0.0, 10.0),
                |_| include_faces,
                |_| true,
                |_| true,
            );
            assert_eq!(masked, legacy);
            assert_eq!(
                observed.into_inner(),
                [
                    MapBoundsCullMask::ALL,
                    MapBoundsCullMask::NONE,
                    MapBoundsCullMask::NONE
                ]
            );
            assert_eq!(masked.len(), 2);
            if !include_faces {
                assert!(masked.iter().all(|leaf| leaf.faces.is_empty()));
            }
        }
    }
}

#[test]
fn uncontained_or_invalid_children_restore_all_plane_tests() {
    use std::cell::RefCell;
    let (mut ordering, locator) = fixture();
    let bounds = |min, max| super::super::super::MapBounds {
        min: Vec3::splat(min),
        max: Vec3::splat(max),
    };
    ordering.node_bounds = vec![bounds(0.0, 10.0)];
    ordering.leaf_bounds = vec![bounds(1.0, 2.0), bounds(-10.0, -1.0)];
    ordering.validate_bounds_hierarchy(&locator);
    assert_eq!(ordering.child_bounds_contained, [[true, false]]);
    let seen = RefCell::new(Vec::new());
    let masked = ordering.world_surface_groups_with_culling(
        &locator,
        Vec3::new(10.0, 0.0, 10.0),
        |_| true,
        |bounds, mask| {
            seen.borrow_mut().push(mask);
            if mask != MapBoundsCullMask::NONE && bounds.max[0] < 0.0 {
                MapBoundsCulling::Outside
            } else {
                MapBoundsCulling::Visible(MapBoundsCullMask::NONE)
            }
        },
        |_| true,
    );
    let legacy = ordering.world_surface_groups(
        &locator,
        Vec3::new(10.0, 0.0, 10.0),
        |_| true,
        |bounds| bounds.max[0] >= 0.0,
        |_| true,
    );
    assert_eq!(masked, legacy);
    assert_eq!(
        seen.into_inner(),
        [
            MapBoundsCullMask::ALL,
            MapBoundsCullMask::NONE,
            MapBoundsCullMask::ALL
        ]
    );
    ordering.leaf_bounds[0].min[0] = f32::NAN;
    ordering.validate_bounds_hierarchy(&locator);
    assert_eq!(ordering.child_bounds_contained, [[false, false]]);
    ordering.leaf_bounds[0] = bounds(3.0, 2.0);
    ordering.validate_bounds_hierarchy(&locator);
    assert_eq!(ordering.child_bounds_contained, [[false, false]]);
}

#[test]
fn uncontained_child_nodes_are_retested_before_their_faces_or_leaves() {
    use std::cell::RefCell;
    let (mut ordering, mut locator) = fixture();
    let bounds = |min, max| super::super::super::MapBounds {
        min: Vec3::splat(min),
        max: Vec3::splat(max),
    };
    locator.nodes[0].children = [1, -2];
    locator.nodes.push(MapNode {
        plane_index: 0,
        children: [-1, -2],
    });
    ordering.node_faces.push(0..0);
    ordering.node_bounds = vec![bounds(0.0, 10.0), bounds(-10.0, -1.0)];
    ordering.leaf_bounds = vec![bounds(-9.0, -2.0), bounds(1.0, 2.0)];
    ordering.validate_bounds_hierarchy(&locator);
    assert_eq!(
        ordering.child_bounds_contained,
        [[false, true], [true, false]]
    );
    let masks = RefCell::new(Vec::new());
    let masked = ordering.world_surface_groups_with_culling(
        &locator,
        Vec3::new(10.0, 0.0, 10.0),
        |_| true,
        |bounds, mask| {
            masks.borrow_mut().push(mask);
            if mask != MapBoundsCullMask::NONE && bounds.max[0] < 0.0 {
                MapBoundsCulling::Outside
            } else {
                MapBoundsCulling::Visible(MapBoundsCullMask::NONE)
            }
        },
        |_| true,
    );
    let legacy = ordering.world_surface_groups(
        &locator,
        Vec3::new(10.0, 0.0, 10.0),
        |_| true,
        |bounds| bounds.max[0] >= 0.0,
        |_| true,
    );
    assert_eq!(masked, legacy);
    assert_eq!(
        masks.into_inner(),
        [
            MapBoundsCullMask::ALL,
            MapBoundsCullMask::ALL,
            MapBoundsCullMask::NONE
        ]
    );
}
