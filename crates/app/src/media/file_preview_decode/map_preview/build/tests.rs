use super::*;
use crate::bridge::gma::PreviewArchive;
use crate::media::file_preview_decode::map_preview::props::{LoadedPhy, LoadedPropPhysics};
use crate::media::file_preview_decode::tests::{prop_placement, test_prop_asset};
use crate::media::preview_model::ModelVertex;
use crate::test_support::GmaFixtureBuilder;
use gmpublished_domain::math::Vec3;
use gmpublished_domain::scene::map::{
    MapDoorClass, MapDoorGeometry, MapDoorMotion, MapDoorOpenDirection, MapDoorSounds,
    MapVisibilityBucket,
};
use std::collections::BTreeMap;
use std::sync::Mutex;

fn resolver() -> MaterialResolver {
    let archive = PreviewArchive::from_gma(GmaFixtureBuilder::new("Shared model roles").build())
        .expect("fixture archive");
    MaterialResolver::new(Arc::new(archive), None)
}

fn model() -> LoadedPropModel {
    let asset = test_prop_asset(vec![0, 1]);
    LoadedPropModel {
        model: asset.model,
        physics: LoadedPropPhysics::Parsed(Arc::new(LoadedPhy {
            ledges: Vec::new(),
            stats: Default::default(),
        })),
        collision: None,
    }
}

#[test]
fn one_raw_model_and_failed_load_are_shared_across_role_admission_views() {
    let resolver = resolver();
    let calls = Mutex::new(BTreeMap::<String, usize>::new());
    let [world, sky, doors] = load_role_models(
        [
            vec![
                "shared.mdl".into(),
                "world.mdl".into(),
                "missing.mdl".into(),
            ],
            vec!["shared.mdl".into(), "sky.mdl".into()],
            vec!["shared.mdl".into(), "missing.mdl".into()],
        ],
        &resolver,
        &|path, _| {
            *calls.lock().unwrap().entry(path.into()).or_default() += 1;
            (path != "missing.mdl").then(model)
        },
    );
    assert_eq!(
        calls.into_inner().unwrap(),
        BTreeMap::from([
            ("shared.mdl".into(), 1),
            ("world.mdl".into(), 1),
            ("sky.mdl".into(), 1),
            ("missing.mdl".into(), 1),
        ])
    );
    let shared = world["shared.mdl"].as_ref().unwrap();
    assert!(Arc::ptr_eq(shared, sky["shared.mdl"].as_ref().unwrap()));
    assert!(Arc::ptr_eq(shared, doors["shared.mdl"].as_ref().unwrap()));
    assert!(world["missing.mdl"].is_none());
    assert!(doors["missing.mdl"].is_none());
    assert!(!sky.contains_key("world.mdl"));
    assert!(!doors.contains_key("sky.mdl"));
    let shared_geometry = shared_model_geometry([&world, &sky, &doors]).unwrap();
    assert_eq!(shared_geometry.model_count, 3);
    assert_eq!(
        (
            shared_geometry.counts.vertices,
            shared_geometry.counts.indices
        ),
        (9, 9)
    );
    let (_, census) = enrich_walk_collision_with_prop_collision(None, &[], &world);
    assert_eq!(census.parsed_models, 2);
}

#[test]
fn a_model_loaded_for_another_role_does_not_bypass_discovery_cap() {
    let placement = prop_placement("models/shared.mdl", Vec3::ZERO, Vec3::ZERO, 0);
    let mut sky = vec![placement; crate::media::file_preview_decode::MAP_PROP_PLACEMENT_CAP];
    sky.push(prop_placement(
        "models/excluded.mdl",
        Vec3::ZERO,
        Vec3::ZERO,
        0,
    ));
    let sky_paths = unique_prop_model_paths_from(sky.iter());
    assert_eq!(sky_paths, ["models/shared.mdl"]);
    let [world, sky, _] = load_role_models(
        [vec!["models/excluded.mdl".into()], sky_paths, Vec::new()],
        &resolver(),
        &|_, _| Some(model()),
    );
    assert!(world["models/excluded.mdl"].is_some());
    assert!(!sky.contains_key("models/excluded.mdl"));
}

#[test]
fn shared_loading_keeps_world_sky_and_door_placement_state_separate() {
    let resolver = resolver();
    let calls = Mutex::new(0_usize);
    let mut world = [prop_placement(
        "models/shared.mdl",
        Vec3::new(10.0, 0.0, 0.0),
        Vec3::ZERO,
        0,
    )];
    let mut sky = [prop_placement(
        "models/shared.mdl",
        Vec3::new(0.0, 20.0, 0.0),
        Vec3::ZERO,
        1,
    )];
    let door = MapDoor {
        entity_index: 12,
        brush_collision: None,
        class: MapDoorClass::PropDoorRotating,
        origin: Vec3::new(0.0, 0.0, 30.0),
        angles: Vec3::ZERO,
        local_bounds_min: Vec3::ZERO,
        local_bounds_max: Vec3::splat(1.0),
        visibility: MapVisibilityBucket::Always,
        auto_close_after: None,
        initial_progress: 0.0,
        motion: MapDoorMotion::Rotating {
            angle_delta: Vec3::new(0.0, 90.0, 0.0),
            degrees: 90.0,
            speed: 100.0,
            open_direction: MapDoorOpenDirection::Both,
        },
        sounds: MapDoorSounds::default(),
        geometry: MapDoorGeometry::Prop {
            placement: prop_placement(
                "models/shared.mdl",
                Vec3::new(0.0, 0.0, 30.0),
                Vec3::ZERO,
                0,
            ),
        },
    };
    let stats = MapStatsRaw {
        face_count: 0,
        displacement_count: 0,
        entity_count: 1,
        model_count: 1,
        static_prop_count: 2,
        world_static_prop_count: 1,
        skybox_static_prop_count: 1,
        entity_prop_count: 0,
        world_entity_prop_count: 0,
        skybox_entity_prop_count: 0,
        cluster_count: 0,
        version: 20,
        lightmap: Default::default(),
        invalid_detail_leaves: 0,
    };
    let result = build_props(
        &mut world,
        &mut sky,
        &[door],
        &stats,
        None,
        None,
        &MapAmbientLighting::neutral(),
        super::super::cubemaps::PropEnvironmentMapInputs {
            map_name: "fixture",
            probes: &[],
        },
        None,
        None,
        &resolver,
        &mut MaterialTable::default(),
        &|_, _| {
            *calls.lock().unwrap() += 1;
            Some(model())
        },
    );
    assert_eq!(calls.into_inner().unwrap(), 1);
    let shared = result.geometry.shared_models.as_ref().unwrap();
    assert_eq!(shared.model_count, 1);
    assert_eq!((shared.counts.vertices, shared.counts.indices), (3, 3));
    assert_eq!(result.geometry.world.admitted.vertices, 3);
    assert_eq!(result.geometry.skybox.admitted.vertices, 3);
    assert_eq!(result.geometry.doors.admitted.vertices, 3);
    let total = result.geometry.admitted_totals().unwrap();
    assert_eq!(
        (total.vertices, total.indices, total.mesh_ranges),
        (9, 9, 3)
    );
    assert_eq!(
        total.payload_bytes().unwrap(),
        result.world.mesh_bytes()
            + result.skybox.mesh_bytes()
            + 3 * size_of::<ModelVertex>()
            + 3 * size_of::<u32>()
    );
    assert_eq!(result.world.placed_count, 1);
    assert_eq!(result.skybox.placed_count, 1);
    assert_eq!(result.doors.doors.len(), 1);
    assert_eq!(
        result.world.meshes[0].vertices[0].position,
        Vec3::new(11.0, 0.0, 0.0)
    );
    assert_eq!(
        result.skybox.meshes[0].vertices[0].position,
        Vec3::new(1.0, 20.0, 0.0)
    );
    assert_eq!(
        result.doors.doors[0].meshes[0].vertices[0].position,
        Vec3::new(1.0, 0.0, 0.0)
    );
    assert_eq!(result.doors.doors[0].origin, Vec3::new(0.0, 0.0, 30.0));
    assert_eq!(result.world.model_lighting[0].origin, world[0].origin);
    assert_eq!(result.skybox.model_lighting[0].origin, sky[0].origin);
    assert_ne!(
        result.world.meshes[0].material_index,
        result.skybox.meshes[0].material_index
    );
}

fn prop_door(path: &str, render_mode: i32) -> MapDoor {
    let mut placement = prop_placement(path, Vec3::splat(30.0), Vec3::ZERO, 0);
    placement.render_mode = render_mode;
    MapDoor {
        entity_index: 12,
        brush_collision: None,
        class: MapDoorClass::PropDoorRotating,
        origin: placement.origin,
        angles: placement.angles,
        local_bounds_min: Vec3::ZERO,
        local_bounds_max: Vec3::ZERO,
        visibility: MapVisibilityBucket::Always,
        auto_close_after: None,
        initial_progress: 0.0,
        motion: MapDoorMotion::Rotating {
            angle_delta: Vec3::new(0.0, 90.0, 0.0),
            degrees: 90.0,
            speed: 100.0,
            open_direction: MapDoorOpenDirection::Both,
        },
        sounds: MapDoorSounds::default(),
        geometry: MapDoorGeometry::Prop { placement },
    }
}

#[test]
fn malformed_door_geometry_rejects_all_meshes_while_hidden_doors_keep_bounds() {
    let mut bad = model();
    let source = Arc::make_mut(&mut bad.model);
    source.meshes.push(source.meshes[0].clone());
    source.meshes[1].indices[2] = 3;
    let loaded = HashMap::from([
        ("models/bad.mdl".into(), Some(Arc::new(bad))),
        ("models/hidden.mdl".into(), Some(Arc::new(model()))),
        ("models/good.mdl".into(), Some(Arc::new(model()))),
    ]);
    let output = bake_map_doors_from_loaded_model_cache(
        &[
            prop_door("models/bad.mdl", 0),
            prop_door("models/hidden.mdl", 6),
            prop_door("models/good.mdl", 0),
        ],
        &resolver(),
        &mut MaterialTable::default(),
        StaticPropLightingInputs {
            ambient: &MapAmbientLighting::neutral(),
            environment_maps: None,
            environment_lighting: None,
            lightmap_source: None,
            walk_collision: None,
        },
        &loaded,
    );
    assert_eq!(output.prop_door_count, 3);
    assert_eq!(output.skipped_prop_door_count, 1);
    assert_eq!(output.geometry.rejected.len(), 1);
    assert_eq!(output.geometry.rejected[0].placement_index, 0);
    assert!(matches!(
        output.geometry.rejected[0].reason,
        crate::media::preview_model::GeometryError::IndexOutOfBounds { mesh: 1, .. }
    ));
    assert_eq!(output.doors.len(), 2);
    assert_eq!(output.doors[0].source_index, 1);
    assert!(output.doors[0].meshes.is_empty());
    assert_eq!(output.doors[0].local_bounds_min, Vec3::ZERO);
    assert_eq!(output.doors[0].local_bounds_max, Vec3::splat(1.0));
    assert_eq!(output.doors[1].source_index, 2);
    assert_eq!(output.doors[1].meshes.len(), 1);
    assert_eq!(output.geometry.admitted.vertices, 3);
    assert_eq!(output.geometry.admitted.indices, 3);
    assert_eq!(output.geometry.planned, output.geometry.admitted);
}
