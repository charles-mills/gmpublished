use std::sync::Arc;

use super::*;
use crate::bridge::{archive::PreviewArchiveSource, gma::PreviewArchive};
use crate::generation::Generation;
use crate::media::preview_model::{InfoReason, PreviewContent};
use crate::media::preview_model::{
    MapFog, MapPreview, MapStats, MaterialSlot, MeshData, ModelPreview, ModelStats, ModelVertex,
    RenderScene,
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
        pose_cache: Default::default(),
        root_lod: 0,
        selected_lod: 0,
        lods: vec![],
        animation: None,
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
            pose_cache: Default::default(),
            root_lod: 0,
            selected_lod: 0,
            lods: vec![],
            animation: None,
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

fn staged_state() -> State {
    staged_with_model(Arc::new(ModelPreview {
        pose_cache: Default::default(),
        root_lod: 0,
        selected_lod: 0,
        lods: vec![],
        animation: None,
        scene: blank_scene(),
        skin_tables: vec![vec![0], vec![1]],
        bodygroups: vec![2],
    }))
}

fn staged_with_model(model: Arc<ModelPreview>) -> State {
    let mut state = State::default();
    let (_model_request, map_request) =
        state.begin_stage(request("models/test.mdl"), request("maps/test.bsp"));
    let data = PreviewData::from_request(
        &map_request,
        PreviewContent::Map {
            scene: Arc::new(empty_map(blank_scene())),
            fog: None,
            sky_camera: None,
            spawn: None,
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
        },
    );
    assert!(state.apply_stage_loaded(map_request.request_id, Ok((data, model))));
    state
}

#[test]
fn stage_selection_and_transform_preserve_map_and_model_identity() {
    let mut state = staged_state();
    let map_id = state.current().unwrap().content_id();
    let stage_id = state.stage_content_id();
    let model = Arc::clone(state.stage_model().unwrap());
    state.select_skin(1);
    state.select_bodygroup_choice(0, 1);
    let transform = StageTransform {
        position: Vec3::new(16.0, 32.0, 48.0),
        angles: Vec3::new(0.0, 90.0, 0.0),
    };
    state.set_stage_transform(transform);
    assert_eq!(state.stage_transform(), Some(transform));
    assert_eq!(state.selected_skin(), 1);
    assert_eq!(state.bodygroup_choices(), &[1]);
    assert_eq!(state.current().unwrap().content_id(), map_id);
    assert_eq!(state.stage_content_id(), stage_id);
    assert!(Arc::ptr_eq(state.stage_model().unwrap(), &model));
    state.select_skin(999);
    state.select_bodygroup_choice(0, 999);
    assert_eq!(state.selected_skin(), 1);
    assert_eq!(state.bodygroup_choices(), &[1]);
}

#[test]
fn stage_drag_is_one_undo_step_and_restore_has_no_undo_history() {
    let mut state = staged_state();
    let initial = state.stage_transform().unwrap();
    state.stage_move_started();
    for x in [12.0, 24.0, 36.0] {
        state.set_stage_transform(StageTransform {
            position: Vec3::new(x, 0.0, 0.0),
            ..initial
        });
    }
    state.stage_move_finished();
    state.undo_stage();
    assert_eq!(state.stage_transform(), Some(initial));
    assert!(!state.stage_can_undo());
    state.restore_stage_placement([42.0, 0.0, 0.0], [0.0, 180.0, 0.0]);
    assert_eq!(
        state.staged_placement(),
        Some(([42.0, 0.0, 0.0], [0.0, 180.0, 0.0]))
    );
    assert!(!state.stage_can_undo());
    state.set_stage_transform(StageTransform {
        position: Vec3::splat(f32::NAN),
        ..initial
    });
    assert_eq!(state.staged_placement().unwrap().0[0], 42.0);
}

#[test]
fn stage_load_is_discarded_after_new_request_or_close() {
    let mut state = staged_state();
    let old_id = state.request().unwrap().request_id;
    let data = state.current().unwrap().clone();
    let model = Arc::clone(state.stage_model().unwrap());
    state.begin_open(request("lua/autorun/init.lua"));
    assert!(!state.apply_stage_loaded(old_id, Ok((data.clone(), Arc::clone(&model)))));
    assert!(state.stage_model().is_none());
    state.close();
    assert!(!state.apply_stage_loaded(old_id, Ok((data, model))));
    assert!(state.stage_model().is_none());
}

#[test]
fn stage_transform_source_rotation_roundtrips_points_and_directions() {
    let transform = StageTransform {
        position: Vec3::new(20.0, -40.0, 8.0),
        angles: Vec3::new(35.0, 90.0, -20.0),
    };
    let local = Vec3::new(4.0, -5.0, 6.0);
    let rotated = local.rotate_source(transform.angles);
    let recovered = transform.local_point(rotated + transform.position);
    assert!((recovered - local).length_squared() < 1e-8);
    assert!((transform.inverse_vector(rotated) - local).length_squared() < 1e-8);
}

#[test]
fn stage_gravity_drop_stops_at_exported_collision_bounds_and_undo_restores_start() {
    use gmpublished_domain::scene::map::MapWalkCollision;
    let mut state = staged_state();
    let stage = state.stage.as_mut().unwrap();
    let scene = Arc::make_mut(&mut Arc::make_mut(&mut stage.model).scene);
    let vertex = |position| ModelVertex {
        position,
        normal: Vec3::new(0.0, 0.0, 1.0),
        uv: [0.0; 2],
        lightmap_uv: [0.0; 2],
        color: Vec3::splat(1.0),
        blend_alpha: 0.0,
    };
    scene.phy_debug_meshes = vec![MeshData {
        vertices: vec![vertex(Vec3::splat(-2.0)), vertex(Vec3::splat(2.0))],
        indices: vec![],
        material_index: 0,
        bodygroup: 0,
        bodygroup_choice: 0,
    }];
    let LoadState::Ready(data) = &mut state.load else {
        panic!("expected loaded preview");
    };
    let PreviewContent::Map { scene, .. } = &mut data.content else {
        panic!("expected map");
    };
    Arc::make_mut(scene).walk_collision = Some(MapWalkCollision::solid_box_for_tests(
        Vec3::new(-1000.0, -1000.0, -32.0),
        Vec3::new(1000.0, 1000.0, 0.0),
    ));
    state.restore_stage_placement([0.0, 0.0, 64.0], [0.0; 3]);
    assert!(state.stage_can_drop());
    state.drop_stage();
    let now = Instant::now();
    for tick in 0..120 {
        state.tick_animation(now + Duration::from_millis(tick * 16));
    }
    assert!(!state.stage_falling());
    let landed = state.stage_transform().unwrap().position[2];
    assert!((landed - 2.0).abs() < 0.1, "landed at {landed}");
    state.undo_stage();
    assert_eq!(state.stage_transform().unwrap().position[2], 64.0);
}

#[test]
fn stage_controls_are_accessible_in_every_locale() {
    use crate::i18n::I18n;
    use crate::theme::{Tokens, ViewCtx};
    let state = staged_state();
    let tokens = Tokens::dark();
    for locale in [
        "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
    ] {
        let i18n = I18n::for_locale(Some(locale));
        for (width, height) in [(560.0, 560.0), (900.0, 720.0)] {
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(width, height),
                super::super::view::pane(&state, ViewCtx::new(&tokens, &i18n), true, width),
            );
            for key in [
                "file-preview-stage-title",
                "file-preview-model-skin",
                "file-preview-stage-position",
                "file-preview-stage-rotation",
            ] {
                let widget = ui.find(i18n.tr(key).as_str()).unwrap();
                assert!(
                    widget.bounds().x >= 0.0 && widget.bounds().x + widget.bounds().width <= width,
                    "stage control overflows in {locale}: {key}"
                );
            }
            let label = i18n.tr("file-preview-stage-place-ahead");
            for _ in 0..20 {
                if ui.find(label.as_str()).unwrap().visible_bounds().is_some() {
                    break;
                }
                ui.point_at(iced::Point::new(width - 70.0, height * 0.5));
                ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
                    delta: iced::mouse::ScrollDelta::Lines { x: 0.0, y: -3.0 },
                })]);
            }
            ui.click(label.as_str()).unwrap();
            assert!(
                ui.into_messages().any(|message| matches!(
                    message,
                    super::super::Message::StagePlaceAheadRequested
                ))
            );
        }
    }
}

#[test]
fn undo_cancels_an_active_stage_drag_before_earlier_history() {
    let mut state = staged_state();
    let initial = state.stage_transform().unwrap();
    let before_drag = StageTransform {
        position: Vec3::new(16.0, 0.0, 0.0),
        ..initial
    };
    state.set_stage_transform(before_drag);
    state.stage_move_started();
    state.set_stage_transform(StageTransform {
        position: Vec3::new(64.0, 0.0, 0.0),
        ..initial
    });
    state.undo_stage();
    assert_eq!(state.stage_transform(), Some(before_drag));
    state.undo_stage();
    assert_eq!(state.stage_transform(), Some(initial));
}

#[test]
fn stage_placement_frames_off_origin_geometry_without_changing_its_pivot() {
    let mut state = staged_state();
    let data = state.current().unwrap().clone();
    let mut model = Arc::clone(state.stage_model().unwrap());
    let scene = Arc::make_mut(&mut Arc::make_mut(&mut model).scene);
    scene.bounds_min = Vec3::new(-1000.0, 500.0, -30.0);
    scene.bounds_max = Vec3::new(-800.0, 700.0, 70.0);
    scene.meshes = vec![MeshData {
        vertices: [
            scene.bounds_min,
            scene.bounds_max,
            Vec3::new(-1000.0, 700.0, 70.0),
        ]
        .into_iter()
        .map(|position| ModelVertex {
            position,
            normal: Vec3::new(0.0, 0.0, 1.0),
            uv: [0.0; 2],
            lightmap_uv: [0.0; 2],
            color: Vec3::splat(1.0),
            blend_alpha: 0.0,
        })
        .collect(),
        indices: vec![0, 1, 2],
        material_index: 0,
        bodygroup: 0,
        bodygroup_choice: 0,
    }];
    let center = (scene.bounds_min + scene.bounds_max) * 0.5;
    let (_, map) = state.begin_stage(request("models/offset.mdl"), request("maps/test.bsp"));
    assert!(state.apply_stage_loaded(map.request_id, Ok((data, Arc::clone(&model)))));
    let initial = state.stage_transform().unwrap();
    assert_eq!((initial.position + center)[0], 192.0);
    assert_eq!((initial.position + center)[1], 0.0);
    assert_eq!(initial.position[2] + model.scene.bounds_min[2], 0.0);
    state.restore_stage_placement(*initial.position.as_array(), [0.0, 90.0, 0.0]);
    let eye = Vec3::new(10.0, 20.0, 100.0);
    state.set_fly_camera(
        FlyPose {
            position: eye,
            yaw: 0.0,
            pitch: 0.0,
            speed: 1.0,
        },
        MovementMode::Fly,
    );
    state.place_stage_ahead();
    let placed = state.stage_transform().unwrap();
    let world_center = placed.position + center.rotate_source(placed.angles);
    assert!((world_center - (eye + Vec3::new(192.0, 0.0, 0.0))).length_squared() < 1e-6);
    assert_eq!(model.scene.bounds_min, Vec3::new(-1000.0, 500.0, -30.0));
    assert!(Arc::ptr_eq(state.stage_model().unwrap(), &model));
}

#[test]
fn localized_map_stat_labels_do_not_overlap_values_in_narrow_inspectors() {
    use crate::i18n::I18n;
    use crate::theme::{Tokens, ViewCtx};
    let mut state = staged_state();
    let LoadState::Ready(data) = &mut state.load else {
        panic!("expected loaded preview");
    };
    let PreviewContent::Map { stats, .. } = &mut data.content else {
        panic!("expected map");
    };
    stats.displacement_count = 123_456_789;
    let tokens = Tokens::dark();
    for locale in [
        "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
    ] {
        let i18n = I18n::for_locale(Some(locale));
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(560.0, 560.0),
            super::super::view::pane(&state, ViewCtx::new(&tokens, &i18n), true, 560.0),
        );
        let label = ui
            .find(i18n.tr("file-preview-map-displacements").as_str())
            .unwrap();
        let value = ui.find("123456789").unwrap();
        assert!(
            label.bounds().x + label.bounds().width <= value.bounds().x,
            "stat label overlaps value in {locale}"
        );
        assert!(
            value.bounds().x + value.bounds().width <= 560.0,
            "stat value overflows viewport in {locale}"
        );
    }
}

#[test]
fn skeletal_playback_state_preserves_selections_and_staged_placement() {
    let model = crate::model_animation_test_support::animated_model();
    let mut state = State::default();
    let model_request = state.begin_open(request("models/preview.mdl"));
    assert!(state.apply_loaded(
        model_request.request_id,
        Ok(PreviewData::from_request(
            &model_request,
            PreviewContent::Model(Arc::clone(&model))
        ))
    ));
    state.select_skin(1);
    state.select_model_sequence(1);
    state.seek_model_animation(0.5);
    state.toggle_model_playback();
    let now = Instant::now();
    state.tick_animation(now);
    state.tick_animation(now + Duration::from_millis(125));
    assert!((state.model_cycle() - 0.75).abs() < 0.001);
    assert_eq!(state.selected_skin(), 1);
    state.tick_animation(now + Duration::from_secs(1));
    assert_eq!(state.model_cycle(), 1.0);
    assert!(!state.model_playing());
    state.select_model_sequence(2);
    state.toggle_model_playback();
    state.tick_animation(now);
    state.tick_animation(now + Duration::from_millis(625));
    assert!((state.model_cycle() - 0.25).abs() < 0.001);
    state.seek_model_animation(f32::NAN);
    assert!((state.model_cycle() - 0.25).abs() < 0.001);
    state.select_model_sequence(99);
    assert_eq!(state.model_sequence(), Some(1));
    state.close();
    assert_eq!(state.model_animation_pose(), None);
    assert!(!state.model_playing());

    let mut state = staged_state();
    state.stage.as_mut().unwrap().model = model;
    let placement = state.stage_transform();
    state.select_model_sequence(1);
    state.seek_model_animation(0.75);
    assert_eq!(
        state.model_animation_pose(),
        Some(gmpublished_backend::model_decompile::PreviewPose {
            sequence: 0,
            cycle: 0.75,
            parameters: Vec::new()
        })
    );
    assert_eq!(state.stage_transform(), placement);
}

#[test]
fn pose_parameters_update_playback_and_preserve_staged_placement() {
    let model = crate::model_animation_test_support::blended_model();
    let mut state = staged_state();
    state.stage.as_mut().unwrap().model = model;
    let placement = state.stage_transform();
    state.select_model_sequence(1);
    assert_eq!(state.model_pose_parameters(), &[0.0]);
    assert!((state.model_sequence_duration() - 1.0).abs() < 0.001);
    state.set_model_pose_parameter(0, 10.0);
    assert_eq!(state.model_pose_parameters(), &[1.0]);
    assert!((state.model_sequence_duration() - 0.5).abs() < 0.001);
    state.seek_model_animation(0.25);
    state.toggle_model_playback();
    let now = Instant::now();
    state.tick_animation(now);
    state.tick_animation(now + Duration::from_millis(125));
    assert!((state.model_cycle() - 0.5).abs() < 0.001);
    state.set_model_pose_parameter(0, 0.0);
    state.tick_animation(now + Duration::from_secs(10));
    assert!((state.model_cycle() - 0.5).abs() < 0.001);
    state.tick_animation(now + Duration::from_millis(10_125));
    assert!((state.model_cycle() - 0.625).abs() < 0.001);
    state.set_model_pose_parameter(0, f32::NAN);
    state.set_model_pose_parameter(99, 1.0);
    assert_eq!(state.model_pose_parameters(), &[0.0]);
    state.set_model_pose_parameter(0, -10.0);
    assert_eq!(state.model_pose_parameters(), &[-1.0]);
    assert_eq!(state.model_sequence_duration(), 0.0);
    state.tick_animation(now + Duration::from_secs(11));
    assert!(!state.model_playing());
    state.select_model_sequence(2);
    state.set_model_pose_parameter(0, 1.0);
    assert_eq!(state.model_pose_parameters(), &[-1.0]);
    state.select_model_sequence(1);
    assert_eq!(state.model_pose_parameters(), &[-1.0]);
    assert_eq!(state.stage_transform(), placement);
    let pose = state.model_animation_pose().unwrap();
    assert_eq!(pose.parameters, vec![-1.0]);
    state.close();
    state.set_model_pose_parameter(0, 1.0);
    assert!(state.model_animation_pose().is_none());
}

#[test]
fn pose_parameter_controls_show_model_names_and_values() {
    use crate::i18n::I18n;
    use crate::theme::{Tokens, ViewCtx};
    let model = crate::model_animation_test_support::blended_model();
    let mut state = staged_state();
    state.stage.as_mut().unwrap().model = Arc::clone(&model);
    state.select_model_sequence(1);
    state.set_model_pose_parameter(0, 0.25);
    let tokens = Tokens::dark();
    let i18n = I18n::for_locale(Some("en"));
    let mut ui = iced_test::Simulator::with_size(
        iced::Settings::default(),
        iced::Size::new(280.0, 220.0),
        super::super::view::model_animation_controls(
            &state,
            &model,
            ViewCtx::new(&tokens, &i18n),
            true,
        ),
    );
    for label in ["aim", "0.25"] {
        let widget = ui.find(label).unwrap();
        assert!(widget.bounds().x >= 0.0 && widget.bounds().x + widget.bounds().width <= 280.0);
    }
}

#[test]
fn skeletal_playback_controls_are_localized_accessible_and_clickable() {
    use crate::i18n::I18n;
    use crate::theme::{Tokens, ViewCtx};
    let model = crate::model_animation_test_support::animated_model();
    let mut state = State::default();
    let request = state.begin_open(request("models/preview.mdl"));
    assert!(state.apply_loaded(
        request.request_id,
        Ok(PreviewData::from_request(
            &request,
            PreviewContent::Model(Arc::clone(&model))
        ))
    ));
    state.select_model_sequence(1);
    state.seek_model_animation(0.5);
    let tokens = Tokens::dark();
    for locale in [
        "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
    ] {
        let i18n = I18n::for_locale(Some(locale));
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(280.0, 180.0),
            super::super::view::model_animation_controls(
                &state,
                &model,
                ViewCtx::new(&tokens, &i18n),
                true,
            ),
        );
        for key in ["file-preview-model-sequence", "file-preview-model-play"] {
            let label = i18n.tr(key);
            assert_ne!(label, key);
            let widget = ui.find(label.as_str()).unwrap();
            assert!(widget.bounds().x >= 0.0 && widget.bounds().x + widget.bounds().width <= 280.0);
        }
        ui.click(i18n.tr("file-preview-model-play").as_str())
            .unwrap();
        assert!(
            ui.into_messages()
                .any(|message| matches!(message, super::super::Message::ModelPlaybackToggled))
        );
    }
}

#[test]
fn lod_selection_swaps_geometry_clamps_to_root_and_resets_after_reload() {
    let scenes = [blank_scene(), blank_scene()];
    for staged in [false, true] {
        let mut state = if staged {
            staged_state()
        } else {
            State::default()
        };
        let model = Arc::new(ModelPreview {
            pose_cache: Default::default(),
            root_lod: 1,
            selected_lod: 1,
            lods: scenes.to_vec(),
            scene: Arc::clone(&scenes[0]),
            animation: None,
            skin_tables: vec![vec![0], vec![1]],
            bodygroups: vec![3],
        });
        if staged {
            state.stage.as_mut().unwrap().model = Arc::clone(&model);
        } else {
            let request = state.begin_open(request("models/test.mdl"));
            state.apply_loaded(
                request.request_id,
                Ok(PreviewData::from_request(
                    &request,
                    PreviewContent::Model(Arc::clone(&model)),
                )),
            );
        }
        state.select_skin(1);
        state.select_bodygroup_choice(0, 1);
        state.select_lod(2);
        assert_eq!(state.selected_lod(), 2);
        assert!(Arc::ptr_eq(
            &state.current_model().unwrap().scene,
            &scenes[1]
        ));
        assert_eq!(state.selected_skin(), 1);
        assert_eq!(state.bodygroup_choices(), &[1]);
        let selected = Arc::clone(state.current_model().unwrap());
        state.select_lod(usize::MAX);
        assert!(Arc::ptr_eq(state.current_model().unwrap(), &selected));
        state.select_lod(0);
        assert_eq!(state.selected_lod(), 1);
        assert!(Arc::ptr_eq(
            &state.current_model().unwrap().scene,
            &scenes[0]
        ));
        state.select_lod(2);
        state.close();
        assert_eq!(state.selected_lod(), 0);
        let request = state.begin_open(request("models/rebuilt.mdl"));
        state.apply_loaded(
            request.request_id,
            Ok(PreviewData::from_request(
                &request,
                PreviewContent::Model(model),
            )),
        );
        assert_eq!(state.selected_lod(), 1);
        assert_eq!(state.selected_skin(), 0);
        assert_eq!(state.bodygroup_choices(), &[0]);
    }
}

#[test]
fn model_load_uses_engine_sequence_zero_orientation_and_keeps_explicit_bind_pose_available() {
    let model = crate::model_animation_test_support::lod_blended_model();
    for staged in [false, true] {
        let mut state = if staged {
            staged_with_model(Arc::clone(&model))
        } else {
            let mut state = State::default();
            let request = state.begin_open(request("models/preview.mdl"));
            assert!(state.apply_loaded(
                request.request_id,
                Ok(PreviewData::from_request(
                    &request,
                    PreviewContent::Model(Arc::clone(&model))
                ))
            ));
            state
        };
        assert_eq!(state.model_sequence(), Some(0));
        assert_eq!(state.model_cycle(), 0.0);
        assert!(!state.model_playing());
        assert_eq!(state.model_pose_parameters(), &[0.0]);
        let pose = state.model_animation_pose().unwrap();
        let vertices = model
            .animation
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .evaluate_lod(
                pose.sequence,
                pose.cycle,
                &pose.parameters,
                state.selected_lod(),
            )
            .unwrap();
        let bind = &model.scene.meshes[0].vertices[0];
        let expected = Vec3::new(-bind.position[1], bind.position[0], bind.position[2]);
        let actual = Vec3::from(vertices[0][0].position);
        assert!(
            (actual - expected).length() < 0.002,
            "{actual:?} vs {expected:?}"
        );
        assert!(
            (actual - bind.position).length() > 1.0,
            "fixture must expose the bind/engine rotation difference"
        );
        state.select_model_sequence(2);
        state.seek_model_animation(0.25);
        let selected = state.model_animation_pose();
        state.select_lod(1);
        assert_eq!(state.model_animation_pose(), selected);
        state.select_model_sequence(0);
        assert_eq!(state.model_sequence(), None);
        assert!(state.model_animation_pose().is_none());
        state.select_lod(0);
        assert!(state.model_animation_pose().is_none());
    }
    for unavailable in [
        None,
        Some(Err("Cannot load animation data".to_owned())),
        Some({
            let mut animation = model
                .animation
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .as_ref()
                .clone();
            animation.sequences[0].unavailable = Some("Unsupported first sequence".to_owned());
            assert!(animation.sequences[1].unavailable.is_none());
            Ok(Arc::new(animation))
        }),
    ] {
        let mut fallback = model.as_ref().clone();
        fallback.animation = unavailable;
        let fallback = Arc::new(fallback);
        let mut state = State::default();
        let request = state.begin_open(request("models/fallback.mdl"));
        assert!(state.apply_loaded(
            request.request_id,
            Ok(PreviewData::from_request(
                &request,
                PreviewContent::Model(Arc::clone(&fallback))
            ))
        ));
        assert!(state.model_sequence().is_none());
        assert!(state.model_animation_pose().is_none());
        let staged = staged_with_model(fallback);
        assert!(staged.model_sequence().is_none());
        assert!(staged.model_animation_pose().is_none());
    }
}

#[test]
fn visible_bounds_ignore_other_choices_unused_vertices_and_removed_lod_meshes() {
    use crate::media::preview_model::ModelBounds;
    let vertex = |position| ModelVertex {
        position,
        normal: Vec3::new(0.0, 0.0, 1.0),
        uv: [0.0; 2],
        lightmap_uv: [0.0; 2],
        color: Vec3::splat(1.0),
        blend_alpha: 0.0,
    };
    let mesh = |choice, min, max, indices| MeshData {
        vertices: vec![vertex(min), vertex(max), vertex(Vec3::splat(1_000_000.0))],
        indices,
        material_index: 0,
        bodygroup: 0,
        bodygroup_choice: choice,
    };
    let scene = render_scene(
        vec![
            mesh(0, Vec3::ZERO, Vec3::new(2.0, 3.0, 4.0), vec![0, 1, 0]),
            mesh(1, Vec3::splat(100.0), Vec3::splat(102.0), vec![0, 1, 0]),
            mesh(2, Vec3::splat(500.0), Vec3::splat(501.0), vec![]),
        ],
        vec![],
        vec![],
        empty_stats(),
        Vec3::splat(-1_000_000.0),
        Vec3::splat(1_000_000.0),
    );
    let lower = render_scene(
        vec![
            mesh(0, Vec3::splat(5.0), Vec3::splat(6.0), vec![0, 1, 0]),
            mesh(1, Vec3::splat(100.0), Vec3::splat(102.0), vec![]),
            mesh(2, Vec3::splat(500.0), Vec3::splat(501.0), vec![]),
        ],
        vec![],
        vec![],
        empty_stats(),
        Vec3::splat(-1_000_000.0),
        Vec3::splat(1_000_000.0),
    );
    let model = Arc::new(ModelPreview {
        pose_cache: Default::default(),
        root_lod: 0,
        selected_lod: 0,
        lods: vec![Arc::clone(&scene), lower],
        scene,
        animation: None,
        skin_tables: vec![],
        bodygroups: vec![3],
    });
    let expected = ModelBounds {
        min: Vec3::ZERO,
        max: Vec3::new(2.0, 3.0, 4.0),
    };
    assert_eq!(model.visible_bounds(None, &[0]), Some(expected));
    let cached = model.evaluated_geometry(None).unwrap();
    assert_eq!(
        model.visible_bounds(None, &[1]),
        Some(ModelBounds {
            min: Vec3::splat(100.0),
            max: Vec3::splat(102.0)
        })
    );
    assert!(Arc::ptr_eq(
        &cached,
        &model.evaluated_geometry(None).unwrap()
    ));
    assert!(model.visible_bounds(None, &[2]).is_none());
    let lower = model.at_lod(1);
    assert_eq!(
        lower.visible_bounds(None, &[0]),
        Some(ModelBounds {
            min: Vec3::splat(5.0),
            max: Vec3::splat(6.0)
        })
    );
    assert!(lower.visible_bounds(None, &[1]).is_none());
    assert!(!Arc::ptr_eq(
        &cached,
        &lower.evaluated_geometry(None).unwrap()
    ));
    assert_eq!(model.visible_bounds(None, &[0]), Some(expected));
    let mut invalid = model.as_ref().clone();
    Arc::make_mut(&mut invalid.scene).meshes[0]
        .indices
        .push(u32::MAX);
    assert!(invalid.visible_bounds(None, &[0]).is_none());
    let mut invalid = model.as_ref().clone();
    Arc::make_mut(&mut invalid.scene).meshes[0].vertices[0].position[0] = f32::NAN;
    assert!(invalid.visible_bounds(None, &[0]).is_none());
}

#[test]
fn off_origin_engine_pose_frames_and_places_visible_geometry_while_playback_camera_stays_fixed() {
    use crate::media::preview_model::ModelBounds;
    let model = crate::model_animation_test_support::off_origin_model();
    for staged in [false, true] {
        let mut state = if staged {
            staged_with_model(Arc::clone(&model))
        } else {
            let mut state = State::default();
            let request = state.begin_open(request("models/offset.mdl"));
            state.apply_loaded(
                request.request_id,
                Ok(PreviewData::from_request(
                    &request,
                    PreviewContent::Model(Arc::clone(&model)),
                )),
            );
            state
        };
        let initial = state.model_bounds().unwrap();
        let golden = ModelBounds {
            min: Vec3::new(-3.0, 98.0, -4.0),
            max: Vec3::new(3.0, 102.0, 24.0),
        };
        assert!((initial.min - golden.min).length() < 0.005, "{initial:?}");
        assert!((initial.max - golden.max).length() < 0.005, "{initial:?}");
        let framing = state.model_framing_bounds();
        assert_eq!(framing, Some(initial));
        if staged {
            let placement = state.stage_transform().unwrap();
            let center = placement.position + initial.center();
            assert!((center[0] - 192.0).abs() < 0.005 && center[1].abs() < 0.005);
            assert!((placement.position[2] + initial.min[2]).abs() < 0.005);
        }
        state.toggle_model_playback();
        let now = Instant::now();
        state.tick_animation(now);
        state.tick_animation(now + Duration::from_millis(125));
        let posed = state.model_bounds().unwrap();
        assert!(posed.center()[1] - initial.center()[1] > 2.9);
        assert_eq!(state.model_framing_bounds(), framing);
        let first = model
            .evaluated_geometry(state.model_animation_pose().as_ref())
            .unwrap();
        let second = model
            .evaluated_geometry(state.model_animation_pose().as_ref())
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        if staged {
            let eye = Vec3::new(10.0, 20.0, 100.0);
            state.set_fly_camera(
                FlyPose {
                    position: eye,
                    yaw: 0.0,
                    pitch: 0.0,
                    speed: 1.0,
                },
                MovementMode::Fly,
            );
            state.place_stage_ahead();
            let placement = state.stage_transform().unwrap();
            let center = placement.position + posed.center().rotate_source(placement.angles);
            assert!((center - eye - Vec3::new(192.0, 0.0, 0.0)).length() < 0.005);
        }
        state.select_model_sequence(0);
        let bind = state.model_bounds().unwrap();
        assert!((bind.center() - Vec3::new(100.0, 0.0, 10.0)).length() < 0.005);
        assert_eq!(state.model_framing_bounds(), Some(bind));
    }
}
