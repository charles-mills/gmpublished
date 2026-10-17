use super::*;
use crate::media::file_preview_decode::tests::{prop_placement, test_prop_asset};
use gmpublished_domain::math::Vec3;

#[test]
fn sparse_topology_accounts_for_every_source_vertex() {
    let mut asset = test_prop_asset(vec![10, 11]);
    let mesh = &mut Arc::make_mut(&mut asset.model).meshes[0];
    mesh.vertices.resize(1001, mesh.vertices[0]);
    let placement = prop_placement("models/test.mdl", Vec3::ZERO, Vec3::ZERO, 1);
    let parts = plan_door(&placement, &asset).unwrap();
    assert_eq!(parts[0].mesh_index, 0);
    assert_eq!(parts[0].destination.material_index, 11);
    assert_eq!(parts[0].counts.vertices, 1001);
    assert_eq!(parts[0].counts.indices, 3);
    assert_eq!(
        parts[0].counts.vertex_bytes,
        1001 * size_of::<ModelVertex>()
    );
    assert_eq!(parts[0].counts.index_bytes, 12);
}

#[test]
fn malformed_second_mesh_rejects_the_entire_model_plan() {
    let mut asset = test_prop_asset(vec![10, 11]);
    let model = Arc::make_mut(&mut asset.model);
    model.meshes.push(model.meshes[0].clone());
    model.meshes[1].indices[2] = 3;
    let placement = prop_placement("models/test.mdl", Vec3::ZERO, Vec3::ZERO, 0);
    assert_eq!(
        plan_door(&placement, &asset).unwrap_err(),
        GeometryError::IndexOutOfBounds {
            mesh: 1,
            offset: 2,
            index: 3,
            vertices: 3
        }
    );
}

#[test]
fn partial_triangles_and_nonfinite_vertices_are_rejected() {
    let placement = prop_placement("models/test.mdl", Vec3::ZERO, Vec3::ZERO, 0);
    let mut asset = test_prop_asset(vec![10, 11]);
    Arc::make_mut(&mut asset.model).meshes[0].indices.push(0);
    assert!(matches!(
        plan_door(&placement, &asset),
        Err(GeometryError::NonTriangular { .. })
    ));
    Arc::make_mut(&mut asset.model).meshes[0].indices.pop();
    Arc::make_mut(&mut asset.model).meshes[0].vertices[0].uv[1] = f32::NAN;
    assert!(matches!(
        plan_door(&placement, &asset),
        Err(GeometryError::NonFiniteVertex { .. })
    ));
}

#[test]
fn count_and_byte_overflow_require_no_large_allocation() {
    assert!(matches!(
        GeometryCounts::checked(usize::MAX, 0, 0),
        Err(GeometryError::Overflow {
            component: GeometryComponent::VertexBytes
        })
    ));
    let counts = GeometryCounts {
        indices: u32::MAX as usize,
        ..GeometryCounts::default()
    };
    assert!(matches!(
        counts.append_buffer(GeometryCounts::checked(0, 3, 1).unwrap()),
        Err(GeometryError::Overflow {
            component: GeometryComponent::Indices
        })
    ));
    let counts = GeometryCounts {
        mesh_ranges: u32::MAX as usize,
        ..GeometryCounts::default()
    };
    assert!(matches!(
        counts.append_buffer(GeometryCounts::checked(0, 0, 1).unwrap()),
        Err(GeometryError::Overflow {
            component: GeometryComponent::MeshRanges
        })
    ));
}

#[test]
fn a_late_destination_failure_does_not_commit_earlier_placement_parts() {
    let mut plan = GeometryPlan::default();
    let destination = Destination {
        material_index: 1,
        entity: false,
    };
    plan.buffers.insert(
        destination,
        BufferPlan {
            counts: GeometryCounts {
                indices: u32::MAX as usize,
                ..GeometryCounts::default()
            },
            parts: Vec::new(),
        },
    );
    let parts = [
        SourceMesh {
            mesh_index: 0,
            destination: Destination {
                material_index: 0,
                entity: false,
            },
            counts: GeometryCounts::checked(3, 3, 1).unwrap(),
        },
        SourceMesh {
            mesh_index: 1,
            destination,
            counts: GeometryCounts::checked(3, 3, 1).unwrap(),
        },
    ];
    assert!(append_placement(&mut plan, 0, &parts).is_err());
    assert_eq!(plan.buffers.len(), 1);
    assert!(plan.buffers[&destination].parts.is_empty());
    assert!(plan.admitted.is_empty());
    assert_eq!(plan.report.admitted, GeometryCounts::default());
}

fn selected<'a>(
    placements: &'a [StaticPropPlacement],
    models: &[Arc<PropModelAsset>],
) -> Vec<SelectedPropPlacement<'a>> {
    placements
        .iter()
        .zip(models)
        .enumerate()
        .map(|(index, (placement, model))| SelectedPropPlacement {
            geometry: world_fixture(placement, model),
            source_index: index,
            placement,
            model: Arc::clone(model),
            lighting: super::super::LegacyPropLightingFallback {
                ambient_cube: gmpublished_domain::scene::map::AmbientCube::WHITE,
                sun: None,
            },
            lighting_context: index as u32 + 1,
            compiled_lighting: None,
        })
        .collect()
}

#[test]
fn allocation_failure_rejects_whole_placement_and_reuses_survivor_buffers() {
    let placements = (0..3)
        .map(|index| {
            prop_placement(
                "models/test.mdl",
                Vec3::new(index as f32 * 10.0, 0.0, 0.0),
                Vec3::ZERO,
                0,
            )
        })
        .collect::<Vec<_>>();
    let mut mixed = test_prop_asset(vec![0, 1]);
    let model = Arc::make_mut(&mut mixed.model);
    model.meshes.push(model.meshes[0].clone());
    model.meshes[1].material_index = 1;
    let models = [
        Arc::new(mixed),
        Arc::new(test_prop_asset(vec![1, 1])),
        Arc::new(test_prop_asset(vec![2, 2])),
    ];
    let selected = selected(&placements, &models);
    let mut calls = BTreeMap::new();
    let result = bake_geometry(&selected, true, |destination, counts| {
        assert_eq!(calls.insert(destination, counts), None);
        if destination.material_index == 0 {
            return Err(GeometryError::Allocation {
                material_index: 0,
                entity: false,
                component: GeometryComponent::Vertices,
                bytes: counts.vertex_bytes,
                detail: "injected".into(),
            });
        }
        reserve_geometry_mesh(destination, counts)
    });
    assert_eq!(calls.len(), 3);
    assert_eq!(result.admitted, [1, 2]);
    assert_eq!(result.geometry.planned.vertices, 12);
    assert_eq!(result.geometry.admitted.vertices, 6);
    assert_eq!(result.geometry.rejected.len(), 1);
    assert_eq!(result.geometry.rejected[0].placement_index, 0);
    assert_eq!(result.meshes.len(), 2);
    let survivor = &result.meshes[&(1, false)];
    assert_eq!(survivor.vertices.len(), 3);
    assert!(survivor.vertices.capacity() >= 6);
    assert_eq!(survivor.indices, [0, 1, 2]);
    assert_eq!(survivor.vertices[0].lighting_context, 2);
    assert_eq!(survivor.renderable_ranges[0].source_index, 1);
    assert_eq!(survivor.visibility.always_visible[0].face, 0);
}

#[test]
fn malformed_geometry_is_atomic_and_lighting_context_ids_remain_stable() {
    let placements = [
        prop_placement("models/bad.mdl", Vec3::ZERO, Vec3::ZERO, 0),
        prop_placement("models/good.mdl", Vec3::splat(10.0), Vec3::ZERO, 0),
    ];
    let mut bad = test_prop_asset(vec![0, 1]);
    let model = Arc::make_mut(&mut bad.model);
    model.meshes.push(model.meshes[0].clone());
    model.meshes[1].indices[2] = 99;
    let models = [Arc::new(bad), Arc::new(test_prop_asset(vec![1, 1]))];
    let selected = selected(&placements, &models);
    let output = bake_selected_prop_placements_parallel(&selected);
    assert_eq!(output.admitted, [1]);
    assert_eq!(output.geometry.rejected.len(), 1);
    assert!(!output.meshes.contains_key(&(0, false)));
    assert_eq!(output.meshes[&(1, false)].vertices[0].lighting_context, 2);
    assert_eq!(output.meshes[&(1, false)].vertices.len(), 3);
    assert_eq!(output.geometry.admitted.indices, 3);
}

#[test]
fn compiled_lighting_uses_original_mesh_and_vertex_indices() {
    let placements = [prop_placement(
        "models/compiled.mdl",
        Vec3::ZERO,
        Vec3::ZERO,
        0,
    )];
    let mut asset = test_prop_asset(vec![0, 1]);
    let model = Arc::make_mut(&mut asset.model);
    model.meshes.push(model.meshes[0].clone());
    model.meshes.push(model.meshes[0].clone());
    model.meshes[1].bodygroup_choice = 1;
    model.meshes[1].indices[0] = 99;
    model.meshes[2].material_index = 1;
    let extra_vertex = model.meshes[2].vertices[0];
    model.meshes[2].vertices.push(extra_vertex);
    let mut selected = selected(&placements, &[Arc::new(asset)]);
    selected[0].compiled_lighting = Some(Arc::new(
        super::super::compiled_lighting::CompiledPropLighting {
            illumination: vec![
                vec![Vec3::splat(1.0); 3],
                vec![],
                vec![
                    Vec3::splat(2.0),
                    Vec3::splat(3.0),
                    Vec3::splat(4.0),
                    Vec3::splat(5.0),
                ],
            ],
        },
    ));
    let output = bake_selected_prop_placements_serial(&selected);
    assert!(output.geometry.rejected.is_empty());
    assert_eq!(output.meshes[&(1, false)].vertices.len(), 4);
    assert_eq!(
        output.meshes[&(1, false)].vertices[0].illumination,
        Vec3::splat(2.0)
    );
    assert_eq!(
        output.meshes[&(1, false)].vertices[3].illumination,
        Vec3::splat(5.0)
    );
    assert_eq!(output.geometry.admitted.vertices, 7);
}

#[test]
fn combined_geometry_bytes_overflow_is_explicit() {
    let counts = GeometryCounts {
        vertex_bytes: usize::MAX,
        index_bytes: 1,
        ..Default::default()
    };
    assert_eq!(
        counts.payload_bytes(),
        Err(GeometryError::Overflow {
            component: GeometryComponent::TotalBytes
        })
    );
    assert_eq!(
        counts.checked_add(GeometryCounts::default()),
        Err(GeometryError::Overflow {
            component: GeometryComponent::TotalBytes
        })
    );
}

#[test]
fn source_order_offsets_and_visibility_are_independent_of_worker_count() {
    let placements = (0..37)
        .map(|index| {
            let mut placement = prop_placement(
                "models/shared.mdl",
                Vec3::new(index as f32, 4.0, -2.0),
                Vec3::new(0.0, index as f32, 0.0),
                index % 2,
            );
            placement.entity = index % 3 == 0;
            placement.visibility =
                gmpublished_domain::scene::map::MapPropVisibility::Clusters(vec![index as u32 % 5]);
            placement
        })
        .collect::<Vec<_>>();
    let mut asset = test_prop_asset(vec![1, 0]);
    let model = Arc::make_mut(&mut asset.model);
    model.meshes.push(model.meshes[0].clone());
    model.meshes[1].material_index = 1;
    let shared = Arc::new(asset);
    let models = vec![shared; placements.len()];
    let selected = selected(&placements, &models);
    let expected = bake_selected_prop_placements_serial(&selected);
    for threads in [1, 2, 8] {
        let actual = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| bake_selected_prop_placements_parallel(&selected));
        assert_eq!(actual.geometry, expected.geometry);
        assert_eq!(actual.admitted, expected.admitted);
        assert_eq!(actual.meshes.len(), expected.meshes.len());
        for (key, actual) in &actual.meshes {
            let expected = &expected.meshes[key];
            assert_eq!(actual.vertices, expected.vertices);
            assert_eq!(actual.indices, expected.indices);
            assert_eq!(
                actual.visibility.always_visible,
                expected.visibility.always_visible
            );
            assert_eq!(actual.visibility.clusters, expected.visibility.clusters);
            assert_eq!(
                actual
                    .renderable_ranges
                    .iter()
                    .map(|range| (
                        range.source_index,
                        range.draw_order,
                        range.start,
                        range.count
                    ))
                    .collect::<Vec<_>>(),
                expected
                    .renderable_ranges
                    .iter()
                    .map(|range| (
                        range.source_index,
                        range.draw_order,
                        range.start,
                        range.count
                    ))
                    .collect::<Vec<_>>()
            );
        }
    }
    assert_eq!(expected.geometry.admitted.vertices, 37 * 6);
    assert_eq!(expected.geometry.admitted.indices, 37 * 6);
    assert_eq!(expected.geometry.admitted.mesh_ranges, 37 * 2);
}

#[test]
fn finite_inputs_that_overflow_placement_transform_are_rejected_before_allocation() {
    let mut placement = prop_placement("models/overflow.mdl", Vec3::ZERO, Vec3::ZERO, 0);
    placement.scale = f32::MAX;
    let mut asset = test_prop_asset(vec![0, 1]);
    Arc::make_mut(&mut asset.model).meshes[0].vertices[0].position = Vec3::splat(2.0);
    let placements = [placement];
    let selected = selected(&placements, &[Arc::new(asset)]);
    let output = bake_geometry(&selected, false, |_, _| {
        panic!("invalid placement reached allocation")
    });
    assert!(output.meshes.is_empty());
    assert!(matches!(
        output.geometry.rejected[0].reason,
        GeometryError::NonFiniteTransform { mesh: 0, vertex: 0 }
    ));
}

#[test]
fn door_planning_omits_empty_meshes_before_validation_without_changing_source_indices() {
    let placement = prop_placement("models/door.mdl", Vec3::ZERO, Vec3::ZERO, 0);
    let mut asset = test_prop_asset(vec![0, 1]);
    let model = Arc::make_mut(&mut asset.model);
    let mut empty = model.meshes[0].clone();
    empty.indices.clear();
    empty.vertices[0].uv[0] = f32::NAN;
    empty.material_index = usize::MAX;
    model.meshes.insert(0, empty);
    let parts = plan_door(&placement, &asset).unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].mesh_index, 1);
    assert_eq!(parts[0].counts.vertices, 3);
    assert!(matches!(
        world_fixture(&placement, &asset),
        Err(GeometryError::NonFiniteVertex { mesh: 0, .. })
    ));
}

#[test]
fn raw_plans_and_topology_cache_share_and_release_canonical_source_geometry() {
    let asset = test_prop_asset(vec![0, 1]);
    let source = Arc::downgrade(&asset.model);
    let placement = prop_placement("models/shared.mdl", Vec3::ZERO, Vec3::ZERO, 0);
    let mut planner = RawGeometryPlanner::default();
    let first = planner
        .world(&placement, &asset.model, |slot| slot < 2)
        .unwrap();
    let mut other = placement.clone();
    other.skin = 1;
    let second = planner
        .world(&other, &asset.model, |slot| slot < 2)
        .unwrap();
    assert_eq!(planner.models.len(), 1);
    assert!(Arc::ptr_eq(&first.source, &second.source));
    assert_eq!(
        first.used_material_slots(),
        std::collections::BTreeSet::from([0])
    );
    assert_eq!(
        second.used_material_slots(),
        std::collections::BTreeSet::from([1])
    );
    drop(asset);
    assert!(source.upgrade().is_some());
    drop(first);
    drop(second);
    assert!(source.upgrade().is_some());
    drop(planner);
    assert!(source.upgrade().is_none());
}
