use super::{Effect, Message, State};

pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    let was_captured = state.mouse_captured();
    let mut effects = update_content(state, message);
    if was_captured != state.mouse_captured() {
        effects.insert(
            0,
            Effect::MouseCaptureChanged {
                captured: state.mouse_captured(),
                epoch: state.mouse_capture_epoch(),
            },
        );
    }
    effects
}

fn update_content(state: &mut State, message: Message) -> Vec<Effect> {
    match message {
        Message::WorkspaceLoadCancelled => {
            state.cancel_workspace_load();
            vec![]
        }
        Message::WorkspaceStageRequested { model, map } => {
            let (model, map) = state.begin_workspace_stage(*model, *map);
            vec![
                Effect::AudioStopRequested,
                Effect::LoadStage {
                    generation: map.request_id,
                    model,
                    map,
                },
            ]
        }
        Message::StageLoaded(generation, result) => {
            state.apply_stage_loaded(generation, *result);
            Vec::new()
        }
        Message::StagePlacementRestored { position, angles } => {
            state.restore_stage_placement(position, angles);
            Vec::new()
        }
        Message::StageTransformChanged(transform) => {
            state.set_stage_transform(transform);
            Vec::new()
        }
        Message::StageCameraChanged {
            pose,
            mode,
            transform,
            door_audio_events,
        } => {
            state.set_fly_camera(pose, mode);
            state.set_stage_transform(transform);
            door_audio_events
                .into_iter()
                .map(Effect::DoorAudioEvent)
                .collect()
        }
        Message::StageMoveStarted => {
            state.stage_move_started();
            Vec::new()
        }
        Message::StageMoveFinished => {
            state.stage_move_finished();
            Vec::new()
        }
        Message::StagePositionEdited { axis, value } => {
            state.edit_stage_field(false, axis, value);
            Vec::new()
        }
        Message::StageRotationEdited { axis, value } => {
            state.edit_stage_field(true, axis, value);
            Vec::new()
        }
        Message::StageDropRequested => {
            state.drop_stage();
            Vec::new()
        }
        Message::StageResetRequested => {
            state.reset_stage();
            Vec::new()
        }
        Message::StageUndoRequested => {
            state.undo_stage();
            Vec::new()
        }
        Message::StagePlaceAheadRequested => {
            state.place_stage_ahead();
            Vec::new()
        }
        Message::OpenRequested(request) => {
            let request = state.begin_open(request);
            vec![Effect::AudioStopRequested, Effect::LoadRequested(request)]
        }
        Message::WorkspaceOpenRequested(request) => {
            let request = state.begin_workspace_open(request);
            vec![Effect::AudioStopRequested, Effect::LoadRequested(request)]
        }
        Message::LoadStageChanged(request_id, stage) => {
            let _changed = state.apply_load_stage(request_id, stage);
            Vec::new()
        }
        Message::Loaded(request_id, result) => {
            let _changed = state.apply_loaded(request_id, *result);
            Vec::new()
        }
        Message::RenderFeedbackChanged(generation) => {
            if !state.accepts_render_feedback(generation) {
                return Vec::new();
            }
            Vec::new()
        }
        Message::AnimationTick(now) => {
            state.tick_animation(now);
            if state.audio_playing()
                && let Some(request_id) = state.current_audio_request_id()
            {
                return vec![Effect::AudioPositionPollRequested(request_id)];
            }
            Vec::new()
        }
        Message::AudioToggleRequested => {
            if state.audio_playing() {
                state
                    .current_audio_request_id()
                    .map_or_else(Vec::new, |request_id| {
                        vec![Effect::AudioPauseRequested(request_id)]
                    })
            } else {
                state.current_audio_bytes().map_or_else(Vec::new, |bytes| {
                    let request_id = state
                        .current_audio_request_id()
                        .expect("loaded audio has a request id");
                    vec![Effect::AudioPlayRequested {
                        request_id,
                        bytes,
                        resume_at: state.audio_position_secs(),
                    }]
                })
            }
        }
        Message::AudioPlaybackStarted(request_id) => {
            state.start_audio(request_id);
            Vec::new()
        }
        Message::AudioPlaybackPaused(request_id) => {
            state.pause_audio(request_id);
            Vec::new()
        }
        Message::AudioPlaybackEnded(request_id) => {
            state.finish_audio(request_id);
            Vec::new()
        }
        Message::AudioPositionUpdated(request_id, position_secs) => {
            state.update_audio_position(request_id, position_secs);
            Vec::new()
        }
        Message::ModelSequenceSelected(sequence) => {
            state.select_model_sequence(sequence);
            Vec::new()
        }
        Message::ModelPlaybackToggled => {
            state.toggle_model_playback();
            Vec::new()
        }
        Message::ModelAnimationSeeked(cycle) => {
            state.seek_model_animation(cycle);
            Vec::new()
        }
        Message::ModelPoseParameterChanged { parameter, value } => {
            state.set_model_pose_parameter(parameter, value);
            Vec::new()
        }
        Message::LodSelected(lod) => {
            state.select_lod(lod);
            Vec::new()
        }
        Message::SkinSelected(skin) => {
            state.select_skin(skin);
            Vec::new()
        }
        Message::BodygroupChoiceSelected { group, choice } => {
            state.select_bodygroup_choice(group, choice);
            Vec::new()
        }
        Message::MouseCaptureRequested(captured) => {
            state.set_mouse_captured(captured);
            Vec::new()
        }
        Message::MouseCaptureFailed(epoch) => {
            if epoch == state.mouse_capture_epoch() {
                state.set_mouse_captured(false);
            }
            Vec::new()
        }
        Message::MapFogToggled(enabled) => {
            state.set_map_fog_enabled(enabled);
            Vec::new()
        }
        Message::MapSkyboxToggled(enabled) => {
            state.set_map_skybox_enabled(enabled);
            Vec::new()
        }
        Message::MapVisibilityToggled(enabled) => {
            state.set_map_visibility_enabled(enabled);
            Vec::new()
        }
        Message::MapReducedTextureDetailToggled(reduced) => state
            .reload_map_texture_quality(reduced)
            .map_or_else(Vec::new, |request| vec![Effect::LoadRequested(request)]),
        Message::PhyDebugToggled(enabled) => {
            state.set_phy_debug_enabled(enabled);
            Vec::new()
        }
        Message::FlyCameraChanged { pose, mode } => {
            state.set_fly_camera(pose, mode);
            Vec::new()
        }
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
        Message::FlySpeedChanged { pose, mode } => {
            state.set_fly_camera(pose, mode);
            state.show_fly_speed_readout(pose.speed);
            Vec::new()
        }
        Message::MovementModeSelected(mode) => {
            state.request_movement_mode(mode);
            Vec::new()
        }
        Message::DoorAudioEvents(events) => {
            events.into_iter().map(Effect::DoorAudioEvent).collect()
        }
        Message::OrbitPoseChanged(pose) => {
            state.set_orbit_pose(pose);
            Vec::new()
        }
        Message::ModelCameraResetRequested => {
            state.reset_model_camera();
            Vec::new()
        }
        Message::ParticleSystemSelected(index) => {
            state.select_particle_system(index);
            Vec::new()
        }
        Message::ParticlePlayToggled => {
            state.toggle_particle_playing();
            Vec::new()
        }
        Message::ParticleRestartRequested => {
            state.request_particle_restart();
            Vec::new()
        }
        Message::ParticleSpeedSelected(speed) => {
            state.set_particle_speed(speed);
            Vec::new()
        }
        Message::ParticleControlPointChanged { index, position } => {
            state.set_particle_control_point(index, position);
            Vec::new()
        }
        Message::InspectorResized { split, ratio } => {
            state.resize_inspector(split, ratio);
            Vec::new()
        }
        Message::InspectorLayoutChanged(width) => {
            state.set_inspector_ratio(super::view::effective_inspector_ratio(
                state.inspector_ratio(),
                width,
            ));
            Vec::new()
        }
        Message::InspectorReset(width) => {
            state.reset_inspector();
            state.set_inspector_ratio(super::view::effective_inspector_ratio(
                state.inspector_ratio(),
                width,
            ));
            Vec::new()
        }
        Message::BackRequested => {
            state.set_mouse_captured(false);
            vec![Effect::ModalCloseRequested]
        }
        Message::ExpandToggled => {
            let was_expanded = state.expanded();
            state.toggle_expanded();
            {
                if was_expanded {
                    vec![Effect::DoorAudioStopRequested]
                } else {
                    Vec::new()
                }
            }
        }
        Message::CloseFinished => {
            state.close();
            vec![Effect::AudioStopRequested, Effect::DoorAudioStopRequested]
        }
        Message::RelatedPreviewRequested(entry_path) => state
            .related_preview_request(&entry_path)
            .map_or_else(Vec::new, |request| {
                let request = state.begin_open(request);
                vec![Effect::AudioStopRequested, Effect::LoadRequested(request)]
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
