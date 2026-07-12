use std::sync::Arc;

use super::*;
use crate::backend::{archive::PreviewArchiveSource, gma::PreviewArchive};
use crate::features::file_preview::model::{
    InfoReason, PreviewContent, PreviewData, RelatedPreviewKind, RelatedPreviewTarget,
};
use crate::test_support::GmaFixtureBuilder;

fn request() -> crate::features::file_preview::PreviewRequest {
    let archive = PreviewArchive::from_gma(
        GmaFixtureBuilder::new("Fixture")
            .entry("data/blob.bin", vec![1, 2, 3])
            .build(),
    )
    .expect("fixture archive should load");

    crate::features::file_preview::PreviewRequest {
        request_id: 0,
        archive: PreviewArchiveSource::from_gma(Arc::new(archive)),
        entry_path: "data/blob.bin".to_owned(),
        display_name: "blob.bin".to_owned(),
        size_bytes: 3,
        crc32: 0x1234_5678,
        bypass_size_limits: false,
    }
}

fn request_from_archive(
    archive: Arc<PreviewArchive>,
    entry_path: &str,
    size_bytes: u64,
    crc32: u32,
) -> crate::features::file_preview::PreviewRequest {
    crate::features::file_preview::PreviewRequest {
        request_id: 0,
        archive: PreviewArchiveSource::from_gma(archive),
        entry_path: entry_path.to_owned(),
        display_name: entry_path
            .rsplit('/')
            .next()
            .unwrap_or(entry_path)
            .to_owned(),
        size_bytes,
        crc32,
        bypass_size_limits: false,
    }
}

#[test]
fn open_requested_emits_load_effects_without_modal_stack_work() {
    let mut state = State::default();

    let effects = update(&mut state, Message::OpenRequested(request()));

    assert!(state.loading());
    #[cfg(feature = "asset-studio")]
    assert!(matches!(
        effects.as_slice(),
        [
            Effect::AudioStopRequested,
            Effect::LoadRequested(request)
        ] if request.request_id == 1
    ));
    #[cfg(not(feature = "asset-studio"))]
    assert!(matches!(
        effects.as_slice(),
        [
            Effect::LoadRequested(request)
        ] if request.request_id == 1
    ));
}

#[test]
fn load_anyway_reloads_the_current_entry_without_size_gates() {
    let mut state = State::default();
    let _ = update(&mut state, Message::OpenRequested(request()));

    let effects = update(&mut state, Message::LoadAnywayRequested);

    assert!(state.loading());
    let load = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::LoadRequested(request) => Some(request),
            _ => None,
        })
        .expect("load anyway should re-request the entry");
    assert!(load.bypass_size_limits);
    assert_eq!(load.request_id, 2);

    // Without a current request there is nothing to re-load.
    let mut state = State::default();
    assert!(update(&mut state, Message::LoadAnywayRequested).is_empty());
}

#[test]
fn back_requested_asks_root_to_finish_preview_close() {
    let mut state = State::default();

    let effects = update(&mut state, Message::BackRequested);

    assert_eq!(effects, vec![Effect::ModalCloseRequested]);
}

#[test]
fn expand_toggled_updates_state_and_stops_door_audio_on_collapse() {
    let mut state = State::default();
    let _request = state.begin_open(request());

    assert!(update(&mut state, Message::ExpandToggled).is_empty());
    assert!(state.expanded());

    let effects = update(&mut state, Message::ExpandToggled);
    #[cfg(feature = "asset-studio")]
    assert_eq!(effects, vec![Effect::DoorAudioStopRequested]);
    #[cfg(not(feature = "asset-studio"))]
    assert!(effects.is_empty());
    assert!(!state.expanded());
}

#[test]
fn related_preview_requested_opens_related_entry() {
    let archive = Arc::new(
        PreviewArchive::from_gma(
            GmaFixtureBuilder::new("Fixture")
                .entry("materials/test/thing.vmt", b"vmt".to_vec())
                .entry("materials/test/thing.vtf", b"vtf".to_vec())
                .build(),
        )
        .expect("fixture archive should load"),
    );
    let source = request_from_archive(
        Arc::clone(&archive),
        "materials/test/thing.vmt",
        3,
        0x1111_1111,
    );
    let mut state = State::default();
    let source = state.begin_open(source);
    let mut data = PreviewData::from_request(
        &source,
        PreviewContent::Info {
            reason: InfoReason::Binary,
        },
    );
    data.related_preview = Some(RelatedPreviewTarget {
        entry_path: "materials/test/thing.vtf".to_owned(),
        kind: RelatedPreviewKind::Texture,
    });
    assert!(state.apply_loaded(source.request_id, Ok(data)));

    let effects = update(
        &mut state,
        Message::RelatedPreviewRequested("materials/test/thing.vtf".to_owned()),
    );

    assert!(state.loading());
    assert_eq!(
        state.request().map(|request| request.entry_path.as_str()),
        Some("materials/test/thing.vtf")
    );
    #[cfg(feature = "asset-studio")]
    assert!(matches!(
        effects.as_slice(),
        [
            Effect::AudioStopRequested,
            Effect::LoadRequested(request)
        ] if request.request_id == 2
            && request.entry_path == "materials/test/thing.vtf"
            && request.size_bytes == 3
    ));
    #[cfg(not(feature = "asset-studio"))]
    assert!(matches!(
        effects.as_slice(),
        [
            Effect::LoadRequested(request)
        ] if request.request_id == 2
            && request.entry_path == "materials/test/thing.vtf"
            && request.size_bytes == 3
    ));
}

#[cfg(feature = "asset-studio")]
#[test]
fn map_fog_toggle_round_trips_without_effects() {
    let mut state = State::default();
    let _request = state.begin_open(request());

    assert!(update(&mut state, Message::MapFogToggled(false)).is_empty());
    assert!(!state.map_fog_enabled());

    assert!(update(&mut state, Message::MapFogToggled(true)).is_empty());
    assert!(state.map_fog_enabled());
}

#[cfg(feature = "asset-studio")]
#[test]
fn map_skybox_toggle_round_trips_without_effects() {
    let mut state = State::default();
    let _request = state.begin_open(request());

    assert!(update(&mut state, Message::MapSkyboxToggled(true)).is_empty());
    assert!(state.map_skybox_enabled());

    assert!(update(&mut state, Message::MapSkyboxToggled(false)).is_empty());
    assert!(!state.map_skybox_enabled());
}

#[cfg(feature = "asset-studio")]
#[test]
fn map_visibility_toggle_round_trips_without_effects() {
    let mut state = State::default();
    let _request = state.begin_open(request());

    assert!(update(&mut state, Message::MapVisibilityToggled(false)).is_empty());
    assert!(!state.map_visibility_enabled());

    assert!(update(&mut state, Message::MapVisibilityToggled(true)).is_empty());
    assert!(state.map_visibility_enabled());
}

#[cfg(feature = "asset-studio")]
#[test]
fn phy_debug_toggle_defaults_off_and_round_trips_without_effects() {
    let mut state = State::default();
    let _request = state.begin_open(request());

    assert!(!state.phy_debug_enabled());
    assert!(update(&mut state, Message::PhyDebugToggled(true)).is_empty());
    assert!(state.phy_debug_enabled());

    assert!(update(&mut state, Message::PhyDebugToggled(false)).is_empty());
    assert!(!state.phy_debug_enabled());
}

#[cfg(feature = "asset-studio")]
#[test]
fn pose_messages_update_state_without_effects() {
    let mut state = State::default();
    let _request = state.begin_open(request());
    let fly_pose = crate::features::file_preview::state::FlyPose {
        position: [1.0, 2.0, 3.0],
        yaw: 0.25,
        pitch: -0.5,
        speed: 2.0,
    };
    let orbit_pose = crate::features::file_preview::state::OrbitPose {
        yaw: 0.5,
        pitch: 0.25,
        distance: 3.0,
    };

    assert!(
        update(
            &mut state,
            Message::FlyCameraChanged {
                pose: fly_pose,
                mode: crate::features::file_preview::state::MovementMode::Walk,
            },
        )
        .is_empty()
    );
    assert!(update(&mut state, Message::OrbitPoseChanged(orbit_pose)).is_empty());

    assert_eq!(state.fly_pose(), Some(fly_pose));
    assert_eq!(
        state.fly_movement_mode(),
        Some(crate::features::file_preview::state::MovementMode::Walk)
    );
    assert_eq!(state.orbit_pose(), Some(orbit_pose));
}

#[cfg(feature = "asset-studio")]
#[test]
fn fly_speed_changed_updates_pose_and_readout() {
    let mut state = State::default();
    let _request = state.begin_open(request());
    let fly_pose = crate::features::file_preview::state::FlyPose {
        position: [1.0, 2.0, 3.0],
        yaw: 0.25,
        pitch: -0.5,
        speed: 2.0,
    };

    assert!(
        update(
            &mut state,
            Message::FlySpeedChanged {
                pose: fly_pose,
                mode: crate::features::file_preview::state::MovementMode::Fly,
            },
        )
        .is_empty()
    );

    assert_eq!(state.fly_pose(), Some(fly_pose));
    assert_eq!(
        state.fly_movement_mode(),
        Some(crate::features::file_preview::state::MovementMode::Fly)
    );
    assert_eq!(state.fly_speed_readout(), Some(2.0));
}

#[cfg(feature = "asset-studio")]
#[test]
fn movement_mode_selected_requests_shader_mode_change() {
    let mut state = State::default();
    let _request = state.begin_open(request());

    assert!(
        update(
            &mut state,
            Message::MovementModeSelected(crate::features::file_preview::state::MovementMode::Walk)
        )
        .is_empty()
    );

    assert_eq!(
        state.requested_movement_mode(),
        Some(crate::features::file_preview::state::MovementMode::Walk)
    );
    assert_eq!(state.fly_movement_mode(), None);
}

#[cfg(feature = "asset-studio")]
#[test]
fn movement_mode_selected_is_noop_for_active_mode() {
    let mut state = State::default();
    let _request = state.begin_open(request());
    let fly_pose = crate::features::file_preview::state::FlyPose {
        position: [1.0, 2.0, 3.0],
        yaw: 0.25,
        pitch: -0.5,
        speed: 2.0,
    };
    state.set_fly_camera(
        fly_pose,
        crate::features::file_preview::state::MovementMode::Walk,
    );

    assert!(
        update(
            &mut state,
            Message::MovementModeSelected(crate::features::file_preview::state::MovementMode::Walk)
        )
        .is_empty()
    );

    assert_eq!(state.fly_pose(), Some(fly_pose));
    assert_eq!(
        state.fly_movement_mode(),
        Some(crate::features::file_preview::state::MovementMode::Walk)
    );
    assert_eq!(state.requested_movement_mode(), None);
}

#[cfg(feature = "asset-studio")]
#[test]
fn expanded_round_trip_preserves_viewer_poses() {
    let mut state = State::default();
    let _request = state.begin_open(request());
    let fly_pose = crate::features::file_preview::state::FlyPose {
        position: [1.0, 2.0, 3.0],
        yaw: 0.25,
        pitch: -0.5,
        speed: 2.0,
    };
    let orbit_pose = crate::features::file_preview::state::OrbitPose {
        yaw: 0.5,
        pitch: 0.25,
        distance: 3.0,
    };
    state.set_fly_camera(
        fly_pose,
        crate::features::file_preview::state::MovementMode::Walk,
    );
    state.set_orbit_pose(orbit_pose);

    assert!(update(&mut state, Message::ExpandToggled).is_empty());
    assert!(state.expanded());
    assert_eq!(state.fly_pose(), Some(fly_pose));
    assert_eq!(
        state.fly_movement_mode(),
        Some(crate::features::file_preview::state::MovementMode::Walk)
    );
    assert_eq!(state.orbit_pose(), Some(orbit_pose));

    assert_eq!(
        update(&mut state, Message::ExpandToggled),
        vec![Effect::DoorAudioStopRequested]
    );
    assert!(!state.expanded());
    assert_eq!(state.fly_pose(), Some(fly_pose));
    assert_eq!(
        state.fly_movement_mode(),
        Some(crate::features::file_preview::state::MovementMode::Walk)
    );
    assert_eq!(state.orbit_pose(), Some(orbit_pose));
}

#[test]
fn extract_requested_emits_current_info_path() {
    let mut state = State::default();
    let request = state.begin_open(request());
    let data = PreviewData::from_request(
        &request,
        PreviewContent::Info {
            reason: InfoReason::Binary,
        },
    );
    state.apply_loaded(request.request_id, Ok(data));

    let effects = update(&mut state, Message::ExtractRequested);

    assert_eq!(
        effects,
        vec![Effect::ExtractRequested {
            entry_path: "data/blob.bin".to_owned(),
        }]
    );
}

#[cfg(feature = "asset-studio")]
#[test]
fn audio_toggle_requests_play_then_pause_via_state_messages() {
    let mut state = State::default();
    let request = state.begin_open(request());
    let bytes = Arc::new(vec![1, 2, 3]);
    let data = PreviewData::from_request(
        &request,
        PreviewContent::Audio {
            bytes: Arc::clone(&bytes),
            duration_secs: Some(4.0),
        },
    );
    state.apply_loaded(request.request_id, Ok(data));
    state.update_audio_position(1.25);

    let effects = update(&mut state, Message::AudioToggleRequested);

    assert_eq!(
        effects,
        vec![Effect::AudioPlayRequested {
            bytes,
            resume_at: 1.25,
        }]
    );
    assert!(!state.audio_playing());

    assert!(update(&mut state, Message::AudioPlaybackStarted).is_empty());
    assert!(state.audio_playing());

    let effects = update(&mut state, Message::AudioToggleRequested);

    assert_eq!(effects, vec![Effect::AudioPauseRequested]);

    assert!(update(&mut state, Message::AudioPlaybackPaused).is_empty());
    assert!(!state.audio_playing());
}

#[cfg(feature = "asset-studio")]
#[test]
fn animation_tick_polls_audio_position_only_while_playing() {
    let mut state = State::default();
    let request = state.begin_open(request());
    let data = PreviewData::from_request(
        &request,
        PreviewContent::Audio {
            bytes: Arc::new(vec![1, 2, 3]),
            duration_secs: None,
        },
    );
    state.apply_loaded(request.request_id, Ok(data));
    let now = std::time::Instant::now();

    assert!(update(&mut state, Message::AnimationTick(now)).is_empty());

    assert!(update(&mut state, Message::AudioPlaybackStarted).is_empty());
    assert_eq!(
        update(&mut state, Message::AnimationTick(now)),
        vec![Effect::AudioPositionPollRequested]
    );
}
