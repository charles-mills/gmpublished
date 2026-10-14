use std::sync::Arc;

use super::*;
use crate::bridge::{archive::PreviewArchiveSource, gma::PreviewArchive};
use crate::generation::Generation;
use crate::media::preview_model::{InfoReason, PreviewContent};
use crate::media::preview_model::{
    MapFog, MapPreview, MapStats, MaterialSlot, MeshData, ModelPreview, ModelStats, RenderScene,
};
use crate::test_support::GmaFixtureBuilder;

fn archive() -> Arc<PreviewArchiveSource> {
    PreviewArchiveSource::from_gma(Arc::new(
        PreviewArchive::from_gma(
            GmaFixtureBuilder::new("Fixture")
                .entry("lua/autorun/init.lua", b"print('ok')\n".to_vec())
                .build(),
        )
        .expect("fixture archive should load"),
    ))
}

fn request(path: &str) -> PreviewRequest {
    PreviewRequest {
        request_id: Generation::from_raw(0),
        archive: archive(),
        entry_path: path.to_owned(),
        display_name: path.rsplit('/').next().unwrap_or(path).to_owned(),
        size_bytes: 12,
        crc32: 0xABCD_EF01,
        bypass_size_limits: false,
    }
}

fn info_data(request: &PreviewRequest) -> PreviewData {
    PreviewData::from_request(
        request,
        PreviewContent::Info {
            reason: InfoReason::Binary,
        },
    )
}

fn load_audio_state(state: &mut State, duration_secs: f32, position_secs: f32) {
    let request = state.begin_open(request("sound/music.wav"));
    let data = PreviewData::from_request(
        &request,
        PreviewContent::Audio {
            bytes: Arc::from(Vec::new()),
            duration_secs: Some(duration_secs),
        },
    );
    assert!(state.apply_loaded(request.request_id, Ok(data)));
    state.start_audio(request.request_id);
    state.update_audio_position(request.request_id, position_secs);
}

fn empty_stats() -> ModelStats {
    ModelStats {
        bone_count: 0,
        sequence_count: 0,
        vertex_count: 0,
        triangle_count: 0,
        mesh_count: 0,
        material_count: 0,
        resolved_material_count: 0,
    }
}

fn render_scene(
    meshes: Vec<MeshData>,
    materials: Vec<MaterialSlot>,
    phy_debug_meshes: Vec<MeshData>,
    stats: ModelStats,
    bounds_min: Vec3,
    bounds_max: Vec3,
) -> Arc<RenderScene> {
    Arc::new(RenderScene {
        meshes,
        materials,
        phy_debug_meshes,
        stats,
        bounds_min,
        bounds_max,
    })
}

fn empty_map(scene: Arc<RenderScene>) -> MapPreview {
    MapPreview {
        water_volumes: None,
        entity_meshes: Vec::new(),
        skybox_entity_meshes: Vec::new(),
        scene,
        mesh_visibility: Vec::new(),
        map_skybox_meshes: Vec::new(),
        lightmap: None,
        skybox: None,
        detail_sprites: Vec::new(),
        map_skybox_detail_sprites: Vec::new(),
        overlays: Vec::new(),
        map_skybox_overlays: Vec::new(),
        doors: Vec::new(),
        visibility: None,
        walk_collision: None,
    }
}

fn blank_scene() -> Arc<RenderScene> {
    render_scene(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        empty_stats(),
        Vec3::splat(0.0),
        Vec3::splat(0.0),
    )
}

#[test]
fn begin_open_marks_modal_loading_and_stamps_request_id() {
    let mut state = State::default();

    let request = state.begin_open(request("lua/autorun/init.lua"));

    assert!(state.is_open());
    assert!(state.loading());
    assert_eq!(request.request_id, Generation::from_raw(1));
    assert_eq!(state.request().unwrap().request_id, Generation::from_raw(1));
    assert!(state.current().is_none());
    assert!(!state.expanded());
}

#[test]
fn begin_open_resets_expanded_state() {
    let mut state = State::default();
    let _request = state.begin_open(request("lua/autorun/init.lua"));
    state.toggle_expanded();
    assert!(state.expanded());

    let _request = state.begin_open(request("materials/icon.vtf"));

    assert!(!state.expanded());
}

#[test]
fn begin_open_clears_audio_state() {
    let mut state = State::default();
    load_audio_state(&mut state, 30.0, 12.5);

    let _request = state.begin_open(request("sound/music.wav"));

    assert!(!state.audio_playing());
    assert_eq!(state.audio_position_secs(), 0.0);
    assert_eq!(state.audio_duration_secs(), None);
}

#[test]
fn begin_open_resets_map_fog_enabled() {
    let mut state = State::default();
    state.set_map_fog_enabled(false);

    let _request = state.begin_open(request("maps/test.bsp"));

    assert!(state.map_fog_enabled());
}

#[test]
fn begin_open_resets_map_skybox_enabled() {
    let mut state = State::default();
    state.set_map_skybox_enabled(false);

    let _request = state.begin_open(request("maps/test.bsp"));

    assert!(state.map_skybox_enabled());
}

#[test]
fn begin_open_resets_map_visibility_enabled() {
    let mut state = State::default();
    state.set_map_visibility_enabled(false);

    let _request = state.begin_open(request("maps/test.bsp"));

    assert!(state.map_visibility_enabled());
}

#[test]
fn begin_open_clears_viewer_poses() {
    let mut state = State::default();
    let _request = state.begin_open(request("maps/test.bsp"));
    state.set_fly_pose(FlyPose {
        position: Vec3::new(1.0, 2.0, 3.0),
        yaw: 0.25,
        pitch: -0.5,
        speed: 2.0,
    });
    state.request_movement_mode(MovementMode::Walk);
    state.set_orbit_pose(OrbitPose {
        yaw: 0.5,
        pitch: 0.25,
        distance: 3.0,
    });

    let _request = state.begin_open(request("models/test/thing.mdl"));

    assert_eq!(state.fly_pose(), None);
    assert_eq!(state.fly_movement_mode(), None);
    assert_eq!(state.requested_movement_mode(), None);
    assert_eq!(state.orbit_pose(), None);
}

#[test]
fn apply_loaded_stores_current_preview() {
    let mut state = State::default();
    let request = state.begin_open(request("lua/autorun/init.lua"));
    let data = info_data(&request);

    assert!(state.apply_loaded(request.request_id, Ok(data.clone())));

    assert!(!state.loading());
    assert_eq!(state.current(), Some(&data));
    assert!(state.error().is_none());
}

#[test]
fn stale_loaded_result_is_ignored() {
    let mut state = State::default();
    let first = state.begin_open(request("lua/autorun/init.lua"));
    let _second = state.begin_open(request("materials/icon.vtf"));

    assert!(!state.apply_loaded(first.request_id, Ok(info_data(&first))));

    assert!(state.loading());
    assert!(state.current().is_none());
    assert_eq!(state.request().unwrap().entry_path, "materials/icon.vtf");
}

#[test]
fn load_stage_is_request_scoped_and_cleared_by_loaded_result() {
    let mut state = State::default();
    let first = state.begin_open(request("lua/autorun/init.lua"));
    let second = state.begin_open(request("materials/icon.vtf"));

    assert!(!state.apply_load_stage(first.request_id, PreviewLoadStage::ReadingArchive));
    assert_eq!(state.loading_stage(), None);

    assert!(state.apply_load_stage(second.request_id, PreviewLoadStage::ResolvingMaterials));
    assert_eq!(
        state.loading_stage(),
        Some(PreviewLoadStage::ResolvingMaterials)
    );

    assert!(state.apply_loaded(second.request_id, Ok(info_data(&second))));
    assert_eq!(state.loading_stage(), None);
}

#[test]
fn fly_speed_readout_expires_on_gated_ticks() {
    let mut state = State::default();
    let _request = state.begin_open(request("maps/test.bsp"));
    let now = Instant::now();

    state.show_fly_speed_readout(1.5);
    assert_eq!(state.fly_speed_readout(), Some(1.5));
    assert!(state.fly_speed_readout_visible());

    state.tick_animation(now);
    assert_eq!(state.fly_speed_readout(), Some(1.5));

    state.tick_animation(now + FLY_SPEED_READOUT_VISIBLE_FOR);
    assert_eq!(state.fly_speed_readout(), None);
    assert!(!state.fly_speed_readout_visible());
}

#[test]
fn close_clears_modal_state_and_invalidates_stale_loads() {
    let mut state = State::default();
    let request = state.begin_open(request("lua/autorun/init.lua"));

    state.close();

    assert!(!state.is_open());
    assert!(!state.loading());
    assert!(state.current().is_none());
    assert!(state.request().is_none());
    assert!(!state.expanded());
    assert!(!state.apply_loaded(request.request_id, Ok(info_data(&request))));
}

#[test]
fn close_clears_viewer_poses() {
    let mut state = State::default();
    let _request = state.begin_open(request("maps/test.bsp"));
    state.set_fly_pose(FlyPose {
        position: Vec3::new(1.0, 2.0, 3.0),
        yaw: 0.25,
        pitch: -0.5,
        speed: 2.0,
    });
    state.request_movement_mode(MovementMode::Walk);
    state.set_orbit_pose(OrbitPose {
        yaw: 0.5,
        pitch: 0.25,
        distance: 3.0,
    });

    state.close();

    assert_eq!(state.fly_pose(), None);
    assert_eq!(state.fly_movement_mode(), None);
    assert_eq!(state.requested_movement_mode(), None);
    assert_eq!(state.orbit_pose(), None);
}

#[test]
fn close_clears_audio_state() {
    let mut state = State::default();
    load_audio_state(&mut state, 16.0, 8.0);
    state.close();

    assert!(!state.audio_playing());
    assert_eq!(state.audio_position_secs(), 0.0);
    assert_eq!(state.audio_duration_secs(), None);
}

#[test]
fn truncated_code_loaded_state_is_preserved() {
    let mut state = State::default();
    let request = state.begin_open(request("lua/autorun/init.lua"));
    let data = PreviewData::from_request(
        &request,
        PreviewContent::Code {
            lines: vec![vec![crate::media::preview_model::CodeSpan {
                text: "print('ok')".to_owned(),
                color: None,
            }]],
            truncated: true,
        },
    );

    assert!(state.apply_loaded(request.request_id, Ok(data)));

    assert!(matches!(
        state.current().map(|data| &data.content),
        Some(PreviewContent::Code {
            truncated: true,
            ..
        })
    ));
}

#[test]
fn loading_a_map_with_fog_exposes_only_the_available_controls() {
    let mut state = State::default();
    let request = state.begin_open(request("maps/test.bsp"));
    let scene = Arc::new(empty_map(blank_scene()));
    let data = PreviewData::from_request(
        &request,
        PreviewContent::Map {
            scene,
            fog: Some(MapFog {
                color_linear: Vec3::new(0.25, 0.5, 1.0),
                start: 256.0,
                end: 2048.0,
                max_density: 0.75,
            }),
            sky_camera: None,
            stats: MapStats {
                face_count: 4,
                displacement_count: 1,
                entity_count: 2,
                material_count: 1,
                resolved_material_count: 0,
                static_prop_count: 0,
                placed_prop_count: 0,
                skipped_prop_count: 0,
                detail_sprite_count: 0,
                overlay_count: 0,
                skybox_face_count: 1,
                skybox_prop_count: 0,
                skybox_detail_sprite_count: 0,
                skybox_overlay_count: 0,
                cluster_count: 0,
                version: 20,
            },
            spawn: None,
        },
    );

    assert!(state.apply_loaded(request.request_id, Ok(data.clone())));

    assert_eq!(state.current(), Some(&data));
    assert!(state.bodygroup_choices().is_empty());
    assert_eq!(state.selected_skin(), 0);
    assert!(state.map_fog_control_visible());
    assert!(state.map_skybox_control_visible());
    assert!(!state.map_visibility_control_visible());
}

#[test]
fn map_controls_are_hidden_for_non_map_and_maps_without_features() {
    let mut state = State::default();
    let info_request = state.begin_open(request("data/blob.bin"));
    let info = info_data(&info_request);
    assert!(state.apply_loaded(info_request.request_id, Ok(info)));
    assert!(!state.map_fog_control_visible());
    assert!(!state.map_skybox_control_visible());

    let map_request = state.begin_open(request("maps/test.bsp"));
    let scene = Arc::new(empty_map(blank_scene()));
    let map = PreviewData::from_request(
        &map_request,
        PreviewContent::Map {
            scene,
            stats: MapStats {
                face_count: 0,
                displacement_count: 0,
                entity_count: 0,
                material_count: 0,
                resolved_material_count: 0,
                static_prop_count: 0,
                placed_prop_count: 0,
                skipped_prop_count: 0,
                detail_sprite_count: 0,
                overlay_count: 0,
                skybox_face_count: 0,
                skybox_prop_count: 0,
                skybox_detail_sprite_count: 0,
                skybox_overlay_count: 0,
                cluster_count: 0,
                version: 20,
            },
            fog: None,
            sky_camera: None,
            spawn: None,
        },
    );
    assert!(state.apply_loaded(map_request.request_id, Ok(map)));

    assert!(!state.map_fog_control_visible());
    assert!(!state.map_skybox_control_visible());
    assert!(!state.map_visibility_control_visible());
    assert!(!state.phy_debug_control_visible());
}

#[test]
fn phy_debug_control_is_visible_for_maps_and_models_with_debug_meshes() {
    let mut state = State::default();
    let map_request = state.begin_open(request("maps/test.bsp"));
    let render_scene = render_scene(
        Vec::new(),
        Vec::new(),
        vec![MeshData {
            vertices: Vec::new(),
            indices: Vec::new(),
            material_index: 0,
            bodygroup: 0,
            bodygroup_choice: 0,
        }],
        empty_stats(),
        Vec3::splat(0.0),
        Vec3::splat(0.0),
    );
    let scene = Arc::new(empty_map(Arc::clone(&render_scene)));
    let model_scene = Arc::new(ModelPreview {
        scene: render_scene,
        skin_tables: vec![vec![0]],
        bodygroups: Vec::new(),
    });
    let map = PreviewData::from_request(
        &map_request,
        PreviewContent::Map {
            scene,
            stats: MapStats {
                face_count: 0,
                displacement_count: 0,
                entity_count: 0,
                material_count: 0,
                resolved_material_count: 0,
                static_prop_count: 0,
                placed_prop_count: 0,
                skipped_prop_count: 0,
                detail_sprite_count: 0,
                overlay_count: 0,
                skybox_face_count: 0,
                skybox_prop_count: 0,
                skybox_detail_sprite_count: 0,
                skybox_overlay_count: 0,
                cluster_count: 0,
                version: 20,
            },
            fog: None,
            sky_camera: None,
            spawn: None,
        },
    );
    assert!(state.apply_loaded(map_request.request_id, Ok(map)));

    assert!(!state.phy_debug_enabled());
    assert!(state.phy_debug_control_visible());

    let model_request = state.begin_open(request("models/test/thing.mdl"));
    let model = PreviewData::from_request(&model_request, PreviewContent::Model(model_scene));
    assert!(state.apply_loaded(model_request.request_id, Ok(model)));

    assert!(!state.phy_debug_enabled());
    assert!(state.phy_debug_control_visible());
}

#[test]
fn model_selections_apply_within_bounds_and_reset_on_close() {
    let mut state = State::default();
    let request = state.begin_open(request("models/test/thing.mdl"));
    let data = PreviewData::from_request(
        &request,
        PreviewContent::Model(Arc::new(ModelPreview {
            scene: blank_scene(),
            skin_tables: vec![vec![0], vec![0]],
            bodygroups: vec![3],
        })),
    );
    assert!(state.apply_loaded(request.request_id, Ok(data)));
    assert_eq!(state.selected_skin(), 0);
    assert_eq!(state.bodygroup_choices(), &[0]);

    state.select_skin(1);
    state.select_bodygroup_choice(0, 2);
    assert_eq!(state.selected_skin(), 1);
    assert_eq!(state.bodygroup_choices(), &[2]);

    state.select_skin(5);
    state.select_bodygroup_choice(0, 9);
    state.select_bodygroup_choice(4, 0);
    assert_eq!(state.selected_skin(), 1);
    assert_eq!(state.bodygroup_choices(), &[2]);

    state.close();
    assert_eq!(state.selected_skin(), 0);
    assert!(state.bodygroup_choices().is_empty());
}

#[test]
fn loading_ui_uses_the_same_companion_and_audio_rules_as_the_decoder() {
    for path in [
        "models/example.ANI",
        "models/example.dx90.vtx",
        "models/example.PHY",
    ] {
        let mut state = State::default();
        state.begin_open(request(path));
        assert!(
            matches!(state.content_ui, ContentUiState::Model(_)),
            "{path}"
        );
    }
    let mut state = State::default();
    state.begin_open(request("sound/example.FLAC"));
    assert!(matches!(state.content_ui, ContentUiState::None));
    state.begin_open(request("sound/example.OGG"));
    assert!(matches!(state.content_ui, ContentUiState::Audio(_)));
}
