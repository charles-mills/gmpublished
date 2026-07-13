use super::{Effect, Message, State};

/// Applies a File Preview modal message and returns any outward effects.
pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    match message {
        Message::OpenRequested(request) => {
            let request = state.begin_open(request);
            #[cfg(feature = "asset-studio")]
            {
                vec![Effect::AudioStopRequested, Effect::LoadRequested(request)]
            }
            #[cfg(not(feature = "asset-studio"))]
            {
                vec![Effect::LoadRequested(request)]
            }
        }
        Message::LoadStageChanged(request_id, stage) => {
            let _changed = state.apply_load_stage(request_id, stage);
            Vec::new()
        }
        Message::Loaded(request_id, result) => {
            let _changed = state.apply_loaded(request_id, result);
            Vec::new()
        }
        Message::AnimationTick(now) => {
            state.tick_animation(now);
            #[cfg(feature = "asset-studio")]
            if state.audio_playing() {
                return vec![Effect::AudioPositionPollRequested];
            }
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::AudioToggleRequested => {
            if state.audio_playing() {
                vec![Effect::AudioPauseRequested]
            } else {
                state.current_audio_bytes().map_or_else(Vec::new, |bytes| {
                    vec![Effect::AudioPlayRequested {
                        bytes,
                        resume_at: state.audio_position_secs(),
                    }]
                })
            }
        }
        #[cfg(feature = "asset-studio")]
        Message::AudioPlaybackStarted => {
            state.start_audio();
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::AudioPlaybackPaused => {
            state.pause_audio();
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::AudioPlaybackEnded => {
            state.finish_audio();
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::AudioPositionUpdated(position_secs) => {
            state.update_audio_position(position_secs);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::SkinSelected(skin) => {
            state.select_skin(skin);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::BodygroupChoiceSelected { group, choice } => {
            state.select_bodygroup_choice(group, choice);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::MapFogToggled(enabled) => {
            state.set_map_fog_enabled(enabled);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::MapSkyboxToggled(enabled) => {
            state.set_map_skybox_enabled(enabled);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::MapVisibilityToggled(enabled) => {
            state.set_map_visibility_enabled(enabled);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::PhyDebugToggled(enabled) => {
            state.set_phy_debug_enabled(enabled);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::FlyCameraChanged { pose, mode } => {
            state.set_fly_camera(pose, mode);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::FlyCameraAndDoorAudioChanged {
            pose,
            mode,
            door_audio_events,
        } => {
            state.set_fly_camera(pose, mode);
            door_audio_events
                .into_iter()
                .map(Effect::DoorAudioEvent)
                .collect()
        }
        #[cfg(feature = "asset-studio")]
        Message::FlySpeedChanged { pose, mode } => {
            state.set_fly_camera(pose, mode);
            state.show_fly_speed_readout(pose.speed);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::MovementModeSelected(mode) => {
            state.request_movement_mode(mode);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::DoorAudioEvents(events) => {
            events.into_iter().map(Effect::DoorAudioEvent).collect()
        }
        #[cfg(feature = "asset-studio")]
        Message::OrbitPoseChanged(pose) => {
            state.set_orbit_pose(pose);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::ParticleSystemSelected(index) => {
            state.select_particle_system(index);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::ParticlePlayToggled => {
            state.toggle_particle_playing();
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::ParticleRestartRequested => {
            state.request_particle_restart();
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::ParticleSpeedSelected(speed) => {
            state.set_particle_speed(speed);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::ParticleControlPointChanged { index, position } => {
            state.set_particle_control_point(index, position);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::InspectorResized { split, ratio } => {
            state.resize_inspector(split, ratio);
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::InspectorLayoutChanged(width) => {
            state.set_inspector_ratio(super::view::effective_inspector_ratio(
                state.inspector_ratio(),
                width,
            ));
            Vec::new()
        }
        #[cfg(feature = "asset-studio")]
        Message::InspectorReset(width) => {
            state.reset_inspector();
            state.set_inspector_ratio(super::view::effective_inspector_ratio(
                state.inspector_ratio(),
                width,
            ));
            Vec::new()
        }
        Message::BackRequested => vec![Effect::ModalCloseRequested],
        Message::ExpandToggled => {
            #[cfg(feature = "asset-studio")]
            let was_expanded = state.expanded();
            state.toggle_expanded();
            #[cfg(feature = "asset-studio")]
            {
                if was_expanded {
                    vec![Effect::DoorAudioStopRequested]
                } else {
                    Vec::new()
                }
            }
            #[cfg(not(feature = "asset-studio"))]
            {
                Vec::new()
            }
        }
        Message::CloseFinished => {
            state.close();
            #[cfg(feature = "asset-studio")]
            {
                vec![Effect::AudioStopRequested, Effect::DoorAudioStopRequested]
            }
            #[cfg(not(feature = "asset-studio"))]
            {
                Vec::new()
            }
        }
        Message::RelatedPreviewRequested(entry_path) => state
            .related_preview_request(&entry_path)
            .map_or_else(Vec::new, |request| {
                let request = state.begin_open(request);
                #[cfg(feature = "asset-studio")]
                {
                    vec![Effect::AudioStopRequested, Effect::LoadRequested(request)]
                }
                #[cfg(not(feature = "asset-studio"))]
                {
                    vec![Effect::LoadRequested(request)]
                }
            }),
        Message::LoadAnywayRequested => {
            state
                .load_anyway_request()
                .map_or_else(Vec::new, |request| {
                    let request = state.begin_open(request);
                    vec![Effect::LoadRequested(request)]
                })
        }
        Message::ExtractRequested => state
            .extract_entry_path()
            .map_or_else(Vec::new, |entry_path| {
                vec![Effect::ExtractRequested { entry_path }]
            }),
    }
}

#[cfg(test)]
mod tests;
