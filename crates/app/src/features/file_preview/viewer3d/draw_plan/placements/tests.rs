use super::*;
use crate::features::file_preview::viewer3d::Uniforms;
use gmpublished_domain::math::Vec3;
use gmpublished_domain::scene::map::{MapMeshClusterRanges, MapMeshIndexRange};

fn bounds(x: f32) -> MapBounds {
    MapBounds {
        min: Vec3::new(x, -0.1, 0.4),
        max: Vec3::new(x + 0.1, 0.1, 0.6),
    }
}

fn part(indices: Range<u32>, x: f32, context: usize) -> Part {
    Part {
        owner: context,
        indices,
        bounds: Some(bounds(x)),
        contexts: vec![context],
        visibility: PartVisibility::Always(0),
    }
}

fn range(face: u32, start: u32, count: u32) -> MapMeshIndexRange {
    MapMeshIndexRange { face, start, count }
}

#[test]
fn shared_material_placements_are_culled_individually_and_optical_views_admit_their_own_ranges() {
    let mesh =
        MeshPlacements::prepare(6, vec![part(0..3, 0.0, 0), part(3..6, 3.0, 1)], None).unwrap();
    let main = mesh.select(None, |_, bounds| bounds.min[0] < 1.0);
    assert_eq!(main.indices, vec![0..3]);
    assert_eq!(main.contexts, vec![0]);
    let optical = mesh.select(None, |_, bounds| bounds.min[0] > 1.0);
    assert_eq!(optical.indices, vec![3..6]);
    assert_eq!(optical.contexts, vec![1]);
    let hidden = mesh.select(None, |_, _| false);
    assert!(hidden.indices.is_empty() && hidden.contexts.is_empty());
}

#[test]
fn incomplete_overlapping_or_out_of_bounds_ownership_stays_on_legacy_mesh_path() {
    for parts in [
        vec![part(3..6, 0.0, 0)],
        vec![part(0..3, 0.0, 0)],
        vec![part(0..4, 0.0, 0), part(3..6, 0.0, 1)],
        vec![part(0..7, 0.0, 0)],
    ] {
        assert!(MeshPlacements::prepare(6, parts, None).is_none());
    }
}

#[test]
fn pvs_buckets_match_authored_ranges_and_outside_pvs_stands_down_without_losing_frustum_admission()
{
    let visibility = MapMeshVisibility {
        always_visible: vec![],
        clusters: vec![
            MapMeshClusterRanges {
                cluster: 0,
                ranges: vec![range(10, 0, 3)],
            },
            MapMeshClusterRanges {
                cluster: 1,
                ranges: vec![range(11, 3, 3)],
            },
        ],
    };
    let mesh = MeshPlacements::prepare(
        6,
        vec![part(0..3, 0.0, 0), part(3..6, 3.0, 1)],
        Some(&visibility),
    )
    .unwrap();
    assert_eq!(
        mesh.select(Some(&[false, true]), |_, _| true).indices,
        vec![3..6]
    );
    assert!(
        mesh.select(Some(&[false, false]), |_, _| true)
            .indices
            .is_empty()
    );
    assert_eq!(
        mesh.select(None, |_, bounds| bounds.min[0] < 1.0).indices,
        vec![0..3]
    );
}

#[test]
fn ambiguous_face_or_partial_pvs_ranges_fall_back_instead_of_changing_first_visible_range_semantics()
 {
    for visibility in [
        MapMeshVisibility {
            always_visible: vec![range(10, 0, 2)],
            clusters: vec![],
        },
        MapMeshVisibility {
            always_visible: vec![range(10, 0, 3)],
            clusters: vec![MapMeshClusterRanges {
                cluster: 0,
                ranges: vec![range(10, 3, 3)],
            }],
        },
    ] {
        assert!(
            MeshPlacements::prepare(
                6,
                vec![part(0..3, 0.0, 0), part(3..6, 0.0, 1)],
                Some(&visibility)
            )
            .is_none()
        );
    }
}

#[test]
fn always_visible_wins_and_duplicate_cluster_membership_does_not_duplicate_draws() {
    let visibility = MapMeshVisibility {
        always_visible: vec![range(10, 0, 3)],
        clusters: vec![MapMeshClusterRanges {
            cluster: 0,
            ranges: vec![range(10, 0, 3), range(10, 0, 3)],
        }],
    };
    let mesh = MeshPlacements::prepare(3, vec![part(0..3, 0.0, 0)], Some(&visibility)).unwrap();
    assert_eq!(mesh.select(Some(&[false]), |_, _| true).indices, vec![0..3]);
    assert_eq!(mesh.select(Some(&[true]), |_, _| true).indices, vec![0..3]);
}

#[test]
fn only_adjacent_surviving_ranges_coalesce_and_original_index_order_is_preserved() {
    let mesh = MeshPlacements::prepare(
        9,
        vec![part(6..9, 0.0, 2), part(0..3, 0.0, 0), part(3..6, 2.0, 1)],
        None,
    )
    .unwrap();
    assert_eq!(mesh.select(None, |_, _| true).indices, vec![0..9]);
    let selected = mesh.select(None, |_, bounds| bounds.min[0] < 1.0);
    assert_eq!(selected.indices, vec![0..3, 6..9]);
    assert_eq!(selected.contexts, vec![0, 2]);
}

#[test]
fn baked_pose_bounds_supplement_compiled_bounds_and_missing_sequence_bounds_do_not_discard_geometry()
 {
    let actual = owner_bounds([Some(bounds(3.0)), Some(bounds(-2.0))], Some(bounds(0.0))).unwrap();
    assert_eq!(actual.min[0], -2.0);
    assert_eq!(actual.max[0], 3.1);
    assert_eq!(owner_bounds([Some(bounds(3.0))], None), Some(bounds(3.0)));
    assert_eq!(
        owner_bounds([Some(bounds(0.0)), None], Some(bounds(0.0))),
        None
    );
    let mut unknown = part(0..3, 0.0, 0);
    unknown.bounds = None;
    let mesh = MeshPlacements::prepare(3, vec![unknown], None).unwrap();
    assert_eq!(mesh.select(None, |_, _| false).indices, vec![0..3]);
}

#[test]
fn baked_reference_scan_uses_indexed_geometry_and_keeps_runtime_contexts_when_bounds_are_unavailable()
 {
    let mut first: ModelVertex = bytemuck::Zeroable::zeroed();
    first.position = Vec3::new(1.0, 2.0, 3.0);
    first.lighting_mode = vertex_lighting::RUNTIME_CONTEXT;
    first.lighting_context = 1;
    let mut second = first;
    second.position = Vec3::new(4.0, 5.0, 6.0);
    second.lighting_mode = 0;
    second.lighting_context = 2;
    let mut unused = first;
    unused.position = Vec3::splat(1000.0);
    let (actual, contexts) = geometry_references(&[first, second, unused], &[0, 1, 0]);
    assert_eq!(
        actual.unwrap(),
        MapBounds {
            min: first.position,
            max: second.position
        }
    );
    assert_eq!(contexts, vec![0]);
    first.position[0] = f32::NAN;
    let (actual, contexts) = geometry_references(&[first, second], &[0, 1, 99]);
    assert!(actual.is_none());
    assert_eq!(contexts, vec![0]);
}

#[test]
fn supplied_optical_clip_is_used_while_main_unclipped_state_remains_conservative() {
    use bytemuck::Zeroable;
    let mut view = Uniforms::zeroed();
    view.view_proj = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let mesh = MeshPlacements::prepare(3, vec![part(0..3, 0.0, 0)], None).unwrap();
    let query = view.bounds_query();
    assert_eq!(
        mesh.select(None, |_, bounds| query.intersects(bounds))
            .indices,
        vec![0..3]
    );
    view.raster_clip.view_projection = view.view_proj;
    view.raster_clip.plane = [0.0, 0.0, 0.0, -1.0];
    let query = view.bounds_query();
    assert!(
        mesh.select(None, |_, bounds| query.intersects(bounds))
            .indices
            .is_empty()
    );
}

#[test]
fn draw_count_uses_admitted_items_only_so_world_only_filters_cannot_reactivate_removed_entities() {
    let item = |mesh_index| super::super::DrawItem {
        mesh_index,
        material_slot: 0,
    };
    let mut plan = DrawPlan {
        opaque: vec![item(0), item(1), item(2)],
        ..DrawPlan::default()
    };
    plan.opaque_ranges.insert(
        0,
        OpaqueRanges {
            indices: vec![0..3, 6..9],
            contexts: vec![0],
        },
    );
    plan.opaque_ranges.insert(1, OpaqueRanges::default());
    assert_eq!(plan.opaque_draw_count(), 3);
    plan.opaque.retain(|item| item.mesh_index != 0);
    assert_eq!(plan.opaque_draw_count(), 1);
}

#[test]
fn pvs_range_encounter_order_matches_the_existing_compacted_buffer() {
    let visibility = MapMeshVisibility {
        always_visible: vec![range(11, 3, 3)],
        clusters: vec![MapMeshClusterRanges {
            cluster: 0,
            ranges: vec![range(10, 0, 3)],
        }],
    };
    let mesh = MeshPlacements::prepare(
        6,
        vec![part(0..3, 0.0, 0), part(3..6, 0.0, 1)],
        Some(&visibility),
    )
    .unwrap();
    assert_eq!(
        mesh.select(Some(&[true]), |_, _| true).indices,
        vec![3..6, 0..3]
    );
    assert_eq!(mesh.select(None, |_, _| true).indices, vec![0..6]);
}

#[test]
fn duplicate_range_with_distinct_faces_retains_legacy_duplicate_draw_behavior() {
    let visibility = MapMeshVisibility {
        always_visible: vec![range(10, 0, 3), range(11, 0, 3)],
        clusters: vec![],
    };
    assert!(MeshPlacements::prepare(3, vec![part(0..3, 0.0, 0)], Some(&visibility)).is_none());
}
