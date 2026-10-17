use super::*;
use crate::bridge::materials::{DecodedTextureBudget, TextureLoadOutcome};
use crate::media::file_preview_decode::tests::{prop_placement, test_prop_asset};
use crate::test_support::fixture_vtf_bytes;

fn loaded(asset: &PropModelAsset) -> Arc<LoadedPropModel> {
    Arc::new(LoadedPropModel {
        model: Arc::clone(&asset.model),
        physics: LoadedPropPhysics::Missing,
        collision: None,
    })
}

#[test]
fn raw_selection_preserves_authored_prefix_skip_counts_and_lookup_order() {
    let mut placements = [
        "good", "hidden", "invalid", "missing", "empty", "good", "big", "suffix", "outside",
    ]
    .map(|name| prop_placement(&format!("models/{name}.mdl"), Vec3::ZERO, Vec3::ZERO, 0));
    placements[1].entity = true;
    placements[1].render_mode = 6;
    placements[2].model_path = "models/invalid.txt".into();
    placements[5].skin = 99;
    let good = Arc::new(test_prop_asset(vec![0, 1]));
    let mut empty = test_prop_asset(vec![]);
    Arc::make_mut(&mut empty.model).material_names.clear();
    let empty = Arc::new(empty);
    let mut big = test_prop_asset(vec![0, 1]);
    Arc::make_mut(&mut big.model).meshes[0].indices = vec![0, 1, 2, 0, 1, 2, 0, 1, 2];
    let big = Arc::new(big);
    let assets = HashMap::from([
        ("models/good.mdl", Arc::clone(&good)),
        ("models/empty.mdl", empty),
        ("models/big.mdl", big),
        ("models/suffix.mdl", good),
    ]);
    let raw = assets
        .iter()
        .map(|(name, asset)| (*name, loaded(asset)))
        .collect::<HashMap<_, _>>();
    let mut material_lookups = Vec::new();
    let mut raw_lookups = Vec::new();
    let selected = select_static_prop_placements(
        &placements,
        8,
        3,
        &mut |placement| {
            material_lookups.push(placement.model_path.clone());
            assets.get(placement.model_path.as_str()).cloned()
        },
        StaticPropLightingInputs {
            ambient: &MapAmbientLighting::neutral(),
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
    );
    let raw_selected = select_models(&placements, 8, 3, &mut |placement| {
        raw_lookups.push(placement.model_path.clone());
        raw.get(placement.model_path.as_str()).cloned()
    });
    assert_eq!(material_lookups, raw_lookups);
    assert_eq!(
        raw_lookups,
        [
            "models/good.mdl",
            "models/missing.mdl",
            "models/empty.mdl",
            "models/good.mdl",
            "models/big.mdl"
        ]
    );
    assert_eq!(
        raw_selected
            .placements
            .iter()
            .map(|selected| selected.source_index)
            .collect::<Vec<_>>(),
        [0, 5]
    );
    assert_eq!(
        selected
            .placements
            .iter()
            .map(|selected| (selected.source_index, selected.lighting_context))
            .collect::<Vec<_>>(),
        [(0, 1), (5, 2)]
    );
    assert_eq!(raw_selected.skip_stats, selected.skip_stats);
    assert_eq!(
        raw_selected.skip_stats,
        PropBakeSkipStats {
            hidden_placement: 1,
            placement_cap: 1,
            triangle_cap: 2,
            invalid_model_path: 1,
            load_failure: 1,
            no_bakeable_mesh: 1,
            geometry_rejected: 0
        }
    );
}

#[test]
fn raw_material_eligibility_matches_bound_slots_for_skin_fallback_and_bodygroups() {
    let mut asset = test_prop_asset(vec![0, 1]);
    let placement = prop_placement("models/test.mdl", Vec3::ZERO, Vec3::ZERO, 0);
    for skin in [-1, 0, 1, 99] {
        let mut placement = placement.clone();
        placement.skin = skin;
        assert_eq!(
            has_bakeable_mesh(&placement, &asset.model, |slot| asset
                .material_indices
                .contains_key(&slot)),
            has_bakeable_mesh(&placement, &asset.model, |slot| slot
                < asset.model.material_names.len())
        );
    }
    let model = Arc::make_mut(&mut asset.model);
    model.meshes[0].bodygroup_choice = 1;
    assert!(!has_bakeable_mesh(&placement, model, |slot| slot < model.material_names.len()));
    model.meshes[0].bodygroup_choice = 0;
    model.meshes[0].material_index = 99;
    assert!(!has_bakeable_mesh(&placement, model, |slot| slot < model.material_names.len()));
}

#[test]
fn omitted_prefix_resources_never_select_textures_or_consume_admission_budget() {
    let names = ["test/hidden", "test/admitted", "test/capped", "test/suffix"];
    let mut entries = Vec::new();
    for (index, name) in names.into_iter().enumerate() {
        entries.push((
            format!("materials/{name}.vmt"),
            format!(r#"VertexLitGeneric {{ "$basetexture" "{name}" }}"#).into_bytes(),
        ));
        entries.push((
            format!("materials/{name}.vtf"),
            fixture_vtf_bytes(
                2,
                2,
                vformats::vtf::VtfFormat::Rgba8888,
                &[&[112 + index as u8; 16]],
            ),
        ));
    }
    let resolver = MaterialResolver::prop_lighting_fixture(
        Vec::new(),
        entries
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.clone()))
            .collect(),
    );
    let budget = Arc::new(DecodedTextureBudget::new(16));
    let resolver = resolver.with_decoded_texture_budget(Arc::clone(&budget));
    let mut placements =
        names.map(|name| prop_placement(&format!("models/{name}.mdl"), Vec3::ZERO, Vec3::ZERO, 0));
    placements[0].entity = true;
    placements[0].render_mode = 6;
    let models = names
        .iter()
        .zip(&placements)
        .map(|(name, placement)| {
            let mut asset = test_prop_asset(vec![0]);
            let model = Arc::make_mut(&mut asset.model);
            model.material_names = vec![(*name).into()];
            model.skin_tables = vec![vec![0]];
            (placement.model_path.clone(), Some(loaded(&asset)))
        })
        .collect::<HashMap<_, _>>();
    let mut table = MaterialTable::default();
    let selection = select_models(&placements, placements.len(), 1, &mut |placement| {
        models.get(&placement.model_path).cloned().flatten()
    });
    let prepared = pre_resolve_prop_materials(selection.material_inputs(), &resolver);
    let baked = bake_loaded_prop_selection(
        selection,
        &resolver,
        &mut table,
        Some(&prepared),
        StaticPropLightingInputs {
            ambient: &MapAmbientLighting::neutral(),
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
        Instant::now(),
    );
    assert_eq!(baked.placed_count, 1);
    assert_eq!(baked.skip_stats.hidden_placement, 1);
    assert_eq!(baked.skip_stats.triangle_cap, 2);
    assert_eq!(baked.model_lighting.len(), 1);
    assert_eq!(baked.meshes[0].vertices[0].lighting_context, 1);
    assert_eq!(
        resolver.selected_texture_paths(),
        ["materials/test/admitted.vtf"]
    );
    assert_eq!(resolver.resident_texture_asset_count(), 1);
    assert_eq!(budget.decoded_bytes(), 16);
    assert_eq!(budget.rejected_textures(), 0);
    assert!(table.slots().iter().all(|slot| {
        slot.texture
            .as_ref()
            .is_some_and(|texture| texture.load_outcome() == TextureLoadOutcome::Loaded)
    }));
}

fn texture_fixture(names: &[&str], bytes: usize) -> (MaterialResolver, Arc<DecodedTextureBudget>) {
    let mut entries = Vec::new();
    for (index, name) in names.iter().enumerate() {
        entries.push((
            format!("materials/{name}.vmt"),
            format!(r#"VertexLitGeneric {{ "$basetexture" "{name}" }}"#).into_bytes(),
        ));
        entries.push((
            format!("materials/{name}.vtf"),
            fixture_vtf_bytes(
                2,
                2,
                vformats::vtf::VtfFormat::Rgba8888,
                &[&[96 + index as u8; 16]],
            ),
        ));
    }
    let resolver = MaterialResolver::prop_lighting_fixture(
        Vec::new(),
        entries
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.clone()))
            .collect(),
    );
    let budget = Arc::new(DecodedTextureBudget::new(bytes));
    (
        resolver.with_decoded_texture_budget(Arc::clone(&budget)),
        budget,
    )
}

fn one_material(name: &str) -> Arc<LoadedPropModel> {
    let mut asset = test_prop_asset(vec![0]);
    let model = Arc::make_mut(&mut asset.model);
    model.material_names = vec![name.into()];
    model.skin_tables = vec![vec![0]];
    loaded(&asset)
}

fn bake_loaded(
    placements: &[StaticPropPlacement],
    models: &HashMap<String, Option<Arc<LoadedPropModel>>>,
    resolver: &MaterialResolver,
    table: &mut MaterialTable,
) -> PropBakeResult {
    bake_static_props_from_loaded_model_cache(
        placements,
        resolver,
        table,
        models,
        StaticPropLightingInputs {
            ambient: &MapAmbientLighting::neutral(),
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
        Instant::now(),
    )
}

#[test]
fn only_effective_skin_and_bodygroup_slots_select_textures() {
    let (resolver, budget) = texture_fixture(&["test/skin0", "test/skin1", "test/unused"], 32);
    let mut asset = test_prop_asset(vec![0, 1, 2]);
    let model = Arc::make_mut(&mut asset.model);
    model.material_names = vec![
        "test/skin0".into(),
        "test/skin1".into(),
        "test/unused".into(),
    ];
    model.skin_tables = vec![vec![0, 1, 2], vec![1, 0, 2]];
    let mut inactive = model.meshes[0].clone();
    inactive.material_index = 2;
    inactive.bodygroup_choice = 1;
    inactive.indices[0] = 99;
    model.meshes.push(inactive);
    let models = HashMap::from([("models/skins.mdl".into(), Some(loaded(&asset)))]);
    let placements =
        [0, 1].map(|skin| prop_placement("models/skins.mdl", Vec3::ZERO, Vec3::ZERO, skin));
    let mut table = MaterialTable::default();
    let baked = bake_loaded(&placements, &models, &resolver, &mut table);
    assert_eq!(baked.placed_count, 2);
    assert!(baked.geometry.rejected.is_empty());
    assert_eq!(
        resolver.selected_texture_paths(),
        ["materials/test/skin0.vtf", "materials/test/skin1.vtf"]
    );
    assert_eq!(resolver.resident_texture_asset_count(), 2);
    assert_eq!(budget.decoded_bytes(), 32);
    assert_eq!(budget.rejected_textures(), 0);
    assert_eq!(table.slots().len(), 2);
    let selected = select_loaded_prop_placements(&placements, &models);
    let mut context = PropMaterialContext {
        resolver: &resolver,
        pre_resolved_prop_materials: None,
        table: &mut table,
    };
    for selected in selected.placements {
        let geometry = selected.geometry.unwrap();
        let asset = prop_model_asset_from_loaded(
            &selected.model,
            geometry.used_material_slots(),
            &mut context,
        );
        assert_eq!(
            asset.material_indices.keys().copied().collect::<Vec<_>>(),
            [selected.placement.skin as usize]
        );
        assert!(Arc::ptr_eq(&geometry.source, &asset.model));
    }
}

#[test]
fn malformed_selected_geometry_does_not_spend_texture_budget_or_compact_lighting_ids() {
    let (resolver, budget) = texture_fixture(&["test/bad", "test/good"], 16);
    let mut bad = one_material("test/bad");
    let model = Arc::make_mut(&mut Arc::get_mut(&mut bad).unwrap().model);
    model.meshes.push(model.meshes[0].clone());
    model.meshes[1].indices[2] = 99;
    let models = HashMap::from([
        ("models/bad.mdl".into(), Some(bad)),
        ("models/good.mdl".into(), Some(one_material("test/good"))),
    ]);
    let placements = ["bad", "good"]
        .map(|name| prop_placement(&format!("models/{name}.mdl"), Vec3::ZERO, Vec3::ZERO, 0));
    let mut table = MaterialTable::default();
    let baked = bake_loaded(&placements, &models, &resolver, &mut table);
    assert_eq!(baked.placed_count, 1);
    assert_eq!(baked.geometry.rejected.len(), 1);
    assert_eq!(baked.geometry.rejected[0].placement_index, 0);
    assert_eq!(baked.skip_stats.geometry_rejected, 1);
    assert_eq!(baked.model_lighting.len(), 2);
    assert_eq!(baked.meshes[0].vertices[0].lighting_context, 2);
    assert_eq!(
        resolver.selected_texture_paths(),
        ["materials/test/good.vtf"]
    );
    assert_eq!(budget.decoded_bytes(), 16);
    assert_eq!(budget.rejected_textures(), 0);
}

#[test]
fn referenced_missing_material_keeps_a_real_error_slot_without_resolving_unused_slots() {
    let (resolver, _) = texture_fixture(&["test/unused"], 16);
    let mut asset = test_prop_asset(vec![0, 1]);
    let model = Arc::make_mut(&mut asset.model);
    model.material_names = vec!["test/unused".into(), "test/missing".into()];
    let placements = [prop_placement(
        "models/missing.mdl",
        Vec3::ZERO,
        Vec3::ZERO,
        1,
    )];
    let models = HashMap::from([("models/missing.mdl".into(), Some(loaded(&asset)))]);
    let mut table = MaterialTable::default();
    let baked = bake_loaded(&placements, &models, &resolver, &mut table);
    assert_eq!(baked.placed_count, 1);
    assert!(baked.geometry.rejected.is_empty());
    assert_eq!(table.slots().len(), 1);
    let material = &table.slots()[baked.meshes[0].material_index];
    assert_ne!(
        material.resolution,
        crate::bridge::materials::MaterialResolutionOutcome::Loaded
    );
    assert!(
        material
            .texture
            .as_ref()
            .is_some_and(|texture| texture.has_effective_base_texture())
    );
    assert!(
        !resolver
            .selected_texture_paths()
            .iter()
            .any(|path| path == "materials/test/unused.vtf")
    );
}

fn door(path: &str, mode: i32) -> MapDoor {
    use gmpublished_domain::scene::map::{MapDoorClass, MapDoorMotion, MapDoorOpenDirection};
    let mut placement = prop_placement(path, Vec3::splat(30.0), Vec3::ZERO, 0);
    placement.render_mode = mode;
    MapDoor {
        entity_index: 1,
        brush_collision: None,
        class: MapDoorClass::PropDoorRotating,
        origin: placement.origin,
        angles: placement.angles,
        local_bounds_min: Vec3::ZERO,
        local_bounds_max: Vec3::ZERO,
        visibility: gmpublished_domain::scene::map::MapVisibilityBucket::Always,
        auto_close_after: None,
        initial_progress: 0.0,
        motion: MapDoorMotion::Rotating {
            angle_delta: Vec3::new(0.0, 90.0, 0.0),
            degrees: 90.0,
            speed: 100.0,
            open_direction: MapDoorOpenDirection::Both,
        },
        sounds: Default::default(),
        geometry: MapDoorGeometry::Prop {
            placement: Box::new(placement),
        },
    }
}

#[test]
fn hidden_and_malformed_doors_prepare_bounds_without_crowding_visible_textures() {
    let (resolver, budget) = texture_fixture(&["test/hidden", "test/bad", "test/good"], 16);
    let mut bad = one_material("test/bad");
    let model = Arc::make_mut(&mut Arc::get_mut(&mut bad).unwrap().model);
    model.meshes.push(model.meshes[0].clone());
    model.meshes[1].indices[2] = 99;
    let models = HashMap::from([
        (
            "models/hidden.mdl".into(),
            Some(one_material("test/hidden")),
        ),
        ("models/bad.mdl".into(), Some(bad)),
        ("models/good.mdl".into(), Some(one_material("test/good"))),
    ]);
    let doors = [
        door("models/hidden.mdl", 6),
        door("models/bad.mdl", 0),
        door("models/good.mdl", 0),
    ];
    let mut table = MaterialTable::default();
    let baked = bake_map_doors_from_loaded_model_cache(
        &doors,
        &resolver,
        &mut table,
        StaticPropLightingInputs {
            ambient: &MapAmbientLighting::neutral(),
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
        &models,
    );
    assert_eq!(baked.prop_door_count, 3);
    assert_eq!(baked.skipped_prop_door_count, 1);
    assert_eq!(baked.geometry.rejected[0].placement_index, 1);
    assert_eq!(
        baked
            .doors
            .iter()
            .map(|door| door.source_index)
            .collect::<Vec<_>>(),
        [0, 2]
    );
    assert!(baked.doors[0].meshes.is_empty());
    assert_eq!(baked.doors[0].local_bounds_min, Vec3::ZERO);
    assert_eq!(baked.doors[0].local_bounds_max, Vec3::splat(1.0));
    assert_eq!(baked.doors[1].meshes.len(), 1);
    assert_eq!(
        resolver.selected_texture_paths(),
        ["materials/test/good.vtf"]
    );
    assert_eq!(budget.decoded_bytes(), 16);
    assert_eq!(budget.rejected_textures(), 0);
    assert_eq!(table.slots().len(), 1);
}

#[test]
fn malformed_oversized_geometry_does_not_charge_or_terminate_valid_prefix() {
    let placements = ["bad", "good", "suffix"]
        .map(|name| prop_placement(&format!("models/{name}.mdl"), Vec3::ZERO, Vec3::ZERO, 0));
    let mut bad = test_prop_asset(vec![0, 1]);
    Arc::make_mut(&mut bad.model).meshes[0].indices = vec![0, 1, 99, 0, 1, 2, 0, 1, 2];
    let bad = Arc::new(bad);
    let good = Arc::new(test_prop_asset(vec![0, 1]));
    let selected = select_static_prop_placements(
        &placements,
        3,
        1,
        &mut |placement| {
            Some(if placement.model_path.ends_with("bad.mdl") {
                Arc::clone(&bad)
            } else {
                Arc::clone(&good)
            })
        },
        StaticPropLightingInputs {
            ambient: &MapAmbientLighting::neutral(),
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
    );
    assert_eq!(
        selected
            .placements
            .iter()
            .map(|p| (p.source_index, p.lighting_context))
            .collect::<Vec<_>>(),
        [(0, 1), (1, 2)]
    );
    assert!(matches!(
        selected.placements[0].geometry,
        Err(GeometryError::IndexOutOfBounds { .. })
    ));
    assert_eq!(
        selected.placements[1]
            .geometry
            .as_ref()
            .unwrap()
            .counts()
            .indices,
        3
    );
    assert_eq!(selected.skip_stats.triangle_cap, 1);
    let baked = bake_selected_prop_placements_serial(&selected.placements);
    assert_eq!(baked.admitted, [1]);
    assert_eq!(baked.geometry.admitted.indices, 3);
    assert_eq!(baked.geometry.rejected[0].placement_index, 0);
}
