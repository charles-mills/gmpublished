use iced::{Event, Subscription, event, keyboard};

#[cfg(target_os = "macos")]
use crate::bridge::TitlebarPreference;

use super::{Effect, Message, State};

pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    if state.reset_busy()
        && !matches!(
            &message,
            Message::SaveCompleted(..)
                | Message::ResetCompleted(..)
                | Message::CloseRequested
                | Message::CloseFinished
                | Message::EscapePressed
        )
    {
        return Vec::new();
    }
    match message {
        Message::FocusMoved(reverse) => {
            let mutation = state.commit_text_size();
            let mut effects = mutation_effect(mutation, state);
            effects.push(Effect::FocusMoved(reverse));
            effects
        }
        Message::OpenRequested(snapshot) => {
            state.open_with_snapshot(*snapshot);
            vec![Effect::ModalOpenRequested]
        }
        Message::CloseRequested => close_effects(state),
        Message::CloseFinished => {
            let mutation = state.commit_text_size();
            let effects = mutation_effect(mutation, state);
            state.close();
            effects
        }
        Message::TabSelected(tab) => {
            let mutation = state.commit_text_size();
            let mut effects = mutation_effect(mutation, state);
            state.select_tab(tab);
            if tab == super::state::Tab::About {
                effects.push(Effect::DebugInfoRequested(state.about.load()));
            }
            effects
        }
        Message::AboutLinkOpened(link) => vec![Effect::OpenUrlRequested(link.url())],
        Message::DebugInfoRequested => vec![Effect::DebugInfoRequested(state.about.load())],
        Message::DebugInfoLoaded(generation, result) => {
            state.about.complete(generation, result);
            Vec::new()
        }
        Message::DebugInfoToggled => {
            state.about.expanded = !state.about.expanded;
            Vec::new()
        }
        Message::CopyDebugInfoRequested => {
            if let super::about::DebugReport::Ready(report) = &state.about.report {
                vec![Effect::CopyDebugInfoRequested(
                    state.about.generation,
                    report.clone(),
                )]
            } else {
                Vec::new()
            }
        }
        Message::DebugInfoCopied(generation) => {
            if generation == state.about.generation {
                state.about.copied = true;
            }
            Vec::new()
        }
        Message::SoundsToggled(enabled) => {
            let mutation = state.scalar_mutation(super::SettingsMutation::Sounds(enabled));
            mutation_effect(mutation, state)
        }
        Message::PlayGifsByDefaultToggled(enabled) => {
            let mutation =
                state.scalar_mutation(super::SettingsMutation::PlayGifsByDefault(enabled));
            mutation_effect(mutation, state)
        }
        #[cfg(target_os = "macos")]
        Message::SystemTitlebarToggled(enabled) => {
            let mutation = state.scalar_mutation(super::SettingsMutation::Titlebar(if enabled {
                TitlebarPreference::System
            } else {
                TitlebarPreference::Auto
            }));
            mutation_effect(mutation, state)
        }
        Message::LanguageSelected(option) => {
            let mutation = state.language_mutation(option);
            mutation_effect(mutation, state)
        }
        Message::DownloadCountFormatSelected(option) => {
            let mutation = state.download_count_format_mutation(option);
            mutation_effect(mutation, state)
        }
        Message::DensitySelected(density) => {
            let mutation = state.scalar_mutation(super::SettingsMutation::Density(density));
            mutation_effect(mutation, state)
        }
        Message::ReduceMotionToggled(enabled) => {
            let mutation = state.scalar_mutation(super::SettingsMutation::ReduceMotion(enabled));
            mutation_effect(mutation, state)
        }
        Message::TextSizeChanged(value) => state
            .preview_text_size(value)
            .map(|_| Effect::SettingsChanged)
            .into_iter()
            .collect(),
        Message::TextSizeIdle(now) => {
            let mutation = state.commit_text_size_if_idle(now);
            mutation_effect(mutation, state)
        }
        Message::TextSizeCommitted => {
            let mutation = state.commit_text_size();
            mutation_effect(mutation, state)
        }
        Message::TextSizeReset => {
            state.preview_text_size(crate::bridge::DEFAULT_TEXT_SIZE_PERCENT);
            let mutation = state.commit_text_size();
            mutation_effect(mutation, state)
        }
        Message::DisplayReset => {
            state.preview_text_size(crate::bridge::DEFAULT_TEXT_SIZE_PERCENT);
            let text_mutation = state.commit_text_size();
            let mut effects = mutation_effect(text_mutation, state);
            let density_mutation = state.scalar_mutation(super::SettingsMutation::Density(
                crate::bridge::UiDensity::default(),
            ));
            effects.extend(mutation_effect(density_mutation, state));
            let motion_mutation =
                state.scalar_mutation(super::SettingsMutation::ReduceMotion(false));
            effects.extend(mutation_effect(motion_mutation, state));
            effects.truncate(1);
            effects
        }
        Message::ThemeSelected(option) => {
            let mutation = state.theme_mutation(option);
            mutation_effect(mutation, state)
        }
        Message::OverwriteModeSelected(option) => {
            let mutation = state.overwrite_mode_mutation(option);
            mutation_effect(mutation, state)
        }
        Message::PathEdited(kind, value) => {
            state.set_path_text(kind, value);
            Vec::new()
        }
        Message::PathAccepted(kind) => {
            let request = state.path_validation_request(kind);
            vec![Effect::PathValidationRequested(request)]
        }
        Message::PathBrowseRequested(kind) => vec![Effect::PathBrowseRequested(kind)],
        Message::PathBrowseCompleted(kind, Some(path)) => {
            state.set_path_text(kind, path.to_string_lossy().into_owned());
            let request = state.path_validation_request(kind);
            vec![Effect::PathValidationRequested(request)]
        }
        Message::PathBrowseCompleted(_, None) => Vec::new(),
        Message::PathValidationCompleted(result) => {
            let mutation = state.apply_path_validation(result);
            mutation_effect(mutation, state)
        }
        Message::ColorEdited(kind, value) => {
            let mutation = state.color_edited(kind, value);
            mutation_effect(mutation, state)
        }
        Message::ColorPickerToggled(kind) => {
            state.toggle_color_picker(kind);
            Vec::new()
        }
        Message::ColorPickerChannelChanged(kind, channel, value) => {
            state.set_color_picker_channel(kind, channel, value);
            Vec::new()
        }
        Message::ColorPickerApplied(kind) => {
            let mutation = state.apply_color_picker(kind);
            mutation_effect(mutation, state)
        }
        Message::ColorPickerCancelled => {
            state.close_color_picker();
            Vec::new()
        }
        Message::ResetRequested(action) => {
            state.request_reset(action);
            Vec::new()
        }
        Message::ResetCancelled => {
            state.cancel_reset();
            Vec::new()
        }
        Message::ResetConfirmed => state
            .begin_reset()
            .map(|_| Effect::PersistenceRequested)
            .into_iter()
            .collect(),
        Message::SaveCompleted(generation, result) => {
            let snapshot = state.apply_save_completed(generation, result.map(|snapshot| *snapshot));
            snapshot
                .map(|_| Effect::SnapshotApplied)
                .into_iter()
                .collect()
        }
        Message::ResetCompleted(action, result) => {
            let snapshot = state.apply_reset_completed(
                action,
                result.map(|snapshot| snapshot.map(|snapshot| *snapshot)),
            );
            snapshot
                .map(|_| Effect::SnapshotApplied)
                .into_iter()
                .collect()
        }
        Message::EscapePressed => {
            if state.cancel_top_layer() {
                Vec::new()
            } else {
                close_effects(state)
            }
        }
    }
}

fn close_effects(state: &mut State) -> Vec<Effect> {
    let mutation = state.commit_text_size();
    let mut effects = mutation_effect(mutation, state);
    effects.push(Effect::ModalCloseRequested);
    effects
}

fn mutation_effect(mutation: Option<super::SettingsMutation>, state: &mut State) -> Vec<Effect> {
    mutation
        .map(|mutation| {
            state.apply_save_started(mutation);
            Effect::SettingsChanged
        })
        .into_iter()
        .collect()
}

pub fn subscription(state: &State) -> Subscription<Message> {
    if state.open() {
        let keyboard = event::listen_with(keyboard_event);
        if state.has_text_preview() {
            Subscription::batch([
                keyboard,
                iced::time::every(std::time::Duration::from_millis(250)).map(Message::TextSizeIdle),
            ])
        } else {
            keyboard
        }
    } else {
        Subscription::none()
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "signature is dictated by iced's event::listen_with fn-pointer type"
)]
fn keyboard_event(
    event: Event,
    _status: event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Tab),
            modifiers,
            ..
        }) => Some(Message::FocusMoved(modifiers.shift())),
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        }) => Some(Message::EscapePressed),
        Event::Keyboard(keyboard::Event::KeyReleased {
            key:
                keyboard::Key::Named(
                    keyboard::key::Named::ArrowUp
                    | keyboard::key::Named::ArrowDown
                    | keyboard::key::Named::ArrowLeft
                    | keyboard::key::Named::ArrowRight
                    | keyboard::key::Named::Home
                    | keyboard::key::Named::End,
                ),
            ..
        }) => Some(Message::TextSizeCommitted),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::bridge::ui_error::UiError;
    use crate::bridge::{AppPaths, Settings, SystemColorScheme};
    use crate::generation::Generation;

    use super::super::state::{
        ColorSetting, PathSetting, ResetAction, SettingsMutation, SettingsSnapshot, Tab,
    };
    use super::*;

    fn open_settings(state: &mut State) {
        let settings = Settings::default();
        let paths = AppPaths::resolve_with_defaults(
            &settings,
            AppPaths {
                settings_file: std::env::temp_dir().join("gmpublished-settings.json"),
                default_user_data_dir: std::env::temp_dir(),
                default_temp_dir: std::env::temp_dir(),
                default_downloads_dir: None,
                temp_dir: std::env::temp_dir(),
                user_data_dir: std::env::temp_dir(),
                downloads_dir: None,
                gmod_dir: None,
            },
        );
        state.open_with_snapshot(SettingsSnapshot::new(
            settings,
            paths,
            SystemColorScheme::Dark,
        ));
    }

    fn test_snapshot() -> SettingsSnapshot {
        SettingsSnapshot::new(
            Settings::default(),
            AppPaths::resolve_with_defaults(
                &Settings::default(),
                AppPaths {
                    settings_file: std::env::temp_dir().join("gmpublished-settings.json"),
                    default_user_data_dir: std::env::temp_dir(),
                    default_temp_dir: std::env::temp_dir(),
                    default_downloads_dir: None,
                    temp_dir: std::env::temp_dir(),
                    user_data_dir: std::env::temp_dir(),
                    downloads_dir: None,
                    gmod_dir: None,
                },
            ),
            SystemColorScheme::Dark,
        )
    }

    #[test]
    fn text_drag_previews_live_and_saves_only_the_final_value() {
        let mut state = State::default();
        open_settings(&mut state);
        let generation = state.save_generation_for_test();
        for value in [110, 125, 150] {
            assert_eq!(
                update(&mut state, Message::TextSizeChanged(value)),
                vec![Effect::SettingsChanged]
            );
        }
        assert_eq!(state.save_generation_for_test(), generation);
        assert_eq!(state.settings().ui.text_size_percent, 150);
        assert_eq!(
            update(&mut state, Message::TextSizeCommitted),
            vec![Effect::SettingsChanged]
        );
        assert!(update(&mut state, Message::TextSizeCommitted).is_empty());
    }

    #[test]
    fn completing_a_text_size_save_preserves_other_settings_drafts() {
        let mut state = State::default();
        open_settings(&mut state);
        let _effects = update(&mut state, Message::TextSizeChanged(125));
        let _effects = update(&mut state, Message::TextSizeCommitted);
        let generation = state.save_generation_for_test();
        let _effects = update(
            &mut state,
            Message::PathEdited(PathSetting::Downloads, "/a/path/still/being/typed".into()),
        );
        let _effects = update(
            &mut state,
            Message::ColorEdited(ColorSetting::Success, "#12".into()),
        );
        let mut saved = test_snapshot();
        saved.settings.ui.text_size_percent = 125;
        let _effects = update(
            &mut state,
            Message::SaveCompleted(generation, Ok(Box::new(saved))),
        );
        assert_eq!(
            state.path_text(PathSetting::Downloads),
            "/a/path/still/being/typed"
        );
        assert_eq!(state.color_text(ColorSetting::Success), "#12");
        assert!(state.color_invalid(ColorSetting::Success));
    }

    #[test]
    fn a_running_reset_blocks_new_display_edits_and_tab_changes() {
        let mut state = State::default();
        open_settings(&mut state);
        update(&mut state, Message::ResetRequested(ResetAction::Settings));
        update(&mut state, Message::ResetConfirmed);
        let original = state.settings().ui.clone();
        let tab = state.active_tab();
        assert!(state.reset_busy());
        assert!(update(&mut state, Message::TextSizeChanged(150)).is_empty());
        assert!(
            update(
                &mut state,
                Message::DensitySelected(crate::bridge::UiDensity::Compact)
            )
            .is_empty()
        );
        assert!(update(&mut state, Message::TabSelected(Tab::Accessibility)).is_empty());
        assert_eq!(state.settings().ui, original);
        assert_eq!(state.active_tab(), tab);
    }

    #[test]
    fn wheel_and_keyboard_previews_save_after_the_edit_becomes_idle() {
        let mut state = State::default();
        open_settings(&mut state);
        update(&mut state, Message::TextSizeChanged(125));
        let now = std::time::Instant::now();
        assert!(update(&mut state, Message::TextSizeIdle(now)).is_empty());
        assert!(state.has_text_preview());
        assert!(matches!(
            update(
                &mut state,
                Message::TextSizeIdle(now + std::time::Duration::from_millis(400))
            )
            .as_slice(),
            [Effect::SettingsChanged]
        ));
        assert!(!state.has_text_preview());
        assert!(
            update(
                &mut state,
                Message::TextSizeIdle(now + std::time::Duration::from_secs(1))
            )
            .is_empty()
        );
    }

    #[test]
    fn an_older_save_completion_preserves_an_in_progress_text_preview() {
        let mut state = State::default();
        open_settings(&mut state);
        update(&mut state, Message::SoundsToggled(false));
        let generation = state.save_generation_for_test();
        let mut saved = test_snapshot();
        saved.settings.backend.sounds = false;
        update(&mut state, Message::TextSizeChanged(150));
        let effects = update(
            &mut state,
            Message::SaveCompleted(generation, Ok(Box::new(saved))),
        );
        assert_eq!(state.settings().ui.text_size_percent, 150);
        assert!(matches!(effects.as_slice(), [Effect::SnapshotApplied]));
        assert!(matches!(
            update(&mut state, Message::TextSizeCommitted).as_slice(),
            [Effect::SettingsChanged]
        ));
    }

    #[test]
    fn closing_settings_flushes_the_pending_text_value() {
        for close in [Message::CloseRequested, Message::EscapePressed] {
            let mut state = State::default();
            open_settings(&mut state);
            update(&mut state, Message::TextSizeChanged(125));
            assert!(matches!(
                update(&mut state, close).as_slice(),
                [Effect::SettingsChanged, Effect::ModalCloseRequested,]
            ));
        }
    }

    #[test]
    fn displaced_settings_flush_pending_text_on_close_finished() {
        let mut state = State::default();
        open_settings(&mut state);
        update(&mut state, Message::TextSizeChanged(125));
        assert!(matches!(
            update(&mut state, Message::CloseFinished).as_slice(),
            [Effect::SettingsChanged]
        ));
        assert!(!state.open());
    }

    fn complete_next_save(state: &mut State, durable: &mut SettingsSnapshot) -> Generation {
        let Some(super::super::PersistenceCommand::Save(generation, mutation)) =
            state.next_persistence()
        else {
            panic!("expected queued save");
        };
        super::super::apply_settings_mutation(&mut durable.settings, mutation);
        update(
            state,
            Message::SaveCompleted(generation, Ok(Box::new(durable.clone()))),
        );
        generation
    }

    #[test]
    fn optimistic_preferences_survive_drafts_close_failed_writes_and_reopen() {
        let mut state = State::default();
        open_settings(&mut state);
        update(
            &mut state,
            Message::DensitySelected(crate::bridge::UiDensity::Compact),
        );
        let first = state.next_persistence();
        update(&mut state, Message::TextSizeChanged(125));
        update(&mut state, Message::TextSizeCommitted);
        update(
            &mut state,
            Message::PathEdited(PathSetting::Temp, "/tmp/draft".into()),
        );
        update(&mut state, Message::CloseFinished);
        let Some(super::super::PersistenceCommand::Save(generation, _)) = first else {
            panic!("save");
        };
        update(
            &mut state,
            Message::SaveCompleted(
                generation,
                Err(UiError::new(gmpublished_backend::error_keys::IO_ERROR)),
            ),
        );
        state.open_with_snapshot(test_snapshot());
        assert_eq!(
            state.settings().ui.density,
            crate::bridge::UiDensity::Compact
        );
        assert_eq!(state.settings().ui.text_size_percent, 125);
        let mut durable = test_snapshot();
        complete_next_save(&mut state, &mut durable);
        assert_eq!(
            state.settings().ui.density,
            crate::bridge::UiDensity::Compact
        );
        durable.settings.ui.density = crate::bridge::UiDensity::Compact;
        state.observe_snapshot(durable);
        assert!(state.saves_acknowledged_for_test());
    }

    #[test]
    fn older_completions_and_external_snapshots_replay_the_entire_pending_queue() {
        let mut state = State::default();
        open_settings(&mut state);
        update(
            &mut state,
            Message::DensitySelected(crate::bridge::UiDensity::Compact),
        );
        update(&mut state, Message::SoundsToggled(false));
        update(&mut state, Message::ReduceMotionToggled(true));
        let mut durable = test_snapshot();
        complete_next_save(&mut state, &mut durable);
        state.observe_snapshot(test_snapshot());
        // The external snapshot may acknowledge older values; pending fields still win.
        assert!(!state.settings().backend.sounds);
        assert!(state.settings().ui.reduce_motion);
        complete_next_save(&mut state, &mut durable);
        complete_next_save(&mut state, &mut durable);
        assert!(state.saves_acknowledged_for_test());
        assert!(state.settings().ui.reduce_motion);
    }

    #[test]
    fn reduced_motion_is_live_and_a_matching_snapshot_does_not_acknowledge_a_newer_write() {
        let mut state = State::default();
        open_settings(&mut state);
        for enabled in [true, false, true] {
            update(&mut state, Message::ReduceMotionToggled(enabled));
        }
        let mut durable = test_snapshot();
        complete_next_save(&mut state, &mut durable);
        state.close();
        state.open_with_snapshot(durable.clone());
        assert!(state.settings().ui.reduce_motion);
        assert!(!state.saves_acknowledged_for_test());
        complete_next_save(&mut state, &mut durable);
        assert!(state.settings().ui.reduce_motion);
        complete_next_save(&mut state, &mut durable);
        assert!(state.saves_acknowledged_for_test());
    }

    #[test]
    fn a_newer_successful_write_supersedes_a_failed_value_for_the_same_setting() {
        let mut state = State::default();
        open_settings(&mut state);
        update(&mut state, Message::ReduceMotionToggled(true));
        let Some(super::super::PersistenceCommand::Save(generation, _)) = state.next_persistence()
        else {
            panic!("save");
        };
        update(
            &mut state,
            Message::SaveCompleted(
                generation,
                Err(UiError::new(gmpublished_backend::error_keys::IO_ERROR)),
            ),
        );
        assert_eq!(state.status_key(), Some("settings-save-failed"));
        state.observe_snapshot(test_snapshot());
        assert!(state.settings().ui.reduce_motion);
        update(&mut state, Message::ReduceMotionToggled(false));
        complete_next_save(&mut state, &mut test_snapshot());
        assert!(!state.settings().ui.reduce_motion);
        assert!(state.saves_acknowledged_for_test());
    }

    #[test]
    fn successful_color_edit_overrides_only_its_field_in_a_failed_theme_write() {
        let mut state = State::default();
        open_settings(&mut state);
        update(
            &mut state,
            Message::ThemeSelected(crate::bridge::ThemePreset::Light),
        );
        let Some(super::super::PersistenceCommand::Save(generation, _)) = state.next_persistence()
        else {
            panic!("save");
        };
        update(
            &mut state,
            Message::SaveCompleted(
                generation,
                Err(UiError::new(gmpublished_backend::error_keys::IO_ERROR)),
            ),
        );
        update(
            &mut state,
            Message::ColorEdited(ColorSetting::Neutral, "#123456".into()),
        );
        complete_next_save(&mut state, &mut test_snapshot());
        assert_eq!(state.settings().backend.color_neutral, 0x123456);
        assert_eq!(state.theme_preset(), crate::bridge::ThemePreset::Light);
        assert!(!state.saves_acknowledged_for_test());
    }

    #[test]
    fn reset_waits_for_queued_saves_and_remains_busy_when_reopened() {
        let mut state = State::default();
        open_settings(&mut state);
        update(&mut state, Message::SoundsToggled(false));
        update(&mut state, Message::TextSizeChanged(125));
        update(&mut state, Message::TextSizeCommitted);
        update(&mut state, Message::ResetRequested(ResetAction::Settings));
        update(&mut state, Message::ResetConfirmed);
        let mut durable = test_snapshot();
        complete_next_save(&mut state, &mut durable);
        complete_next_save(&mut state, &mut durable);
        assert_eq!(
            state.next_persistence(),
            Some(super::super::PersistenceCommand::Reset(
                ResetAction::Settings
            ))
        );
        assert_eq!(state.next_persistence(), None);
        state.open_with_snapshot(durable);
        assert!(state.reset_busy());
        update(
            &mut state,
            Message::ResetCompleted(ResetAction::Settings, Ok(Some(Box::new(test_snapshot())))),
        );
        assert!(!state.reset_busy());
        assert_eq!(state.settings().ui.text_size_percent, 100);
        assert_eq!(
            state.status_key(),
            Some("settings-resets-reset-settings-done")
        );
    }

    #[test]
    fn resetting_display_keeps_theme_and_non_display_preferences() {
        let mut state = State::default();
        open_settings(&mut state);
        update(
            &mut state,
            Message::ThemeSelected(crate::bridge::ThemePreset::Light),
        );
        update(&mut state, Message::SoundsToggled(false));
        update(
            &mut state,
            Message::DensitySelected(crate::bridge::UiDensity::Compact),
        );
        update(&mut state, Message::TextSizeChanged(150));
        update(&mut state, Message::ReduceMotionToggled(true));
        let effects = update(&mut state, Message::DisplayReset);
        assert_eq!(
            state.settings().ui.density,
            crate::bridge::UiDensity::Comfortable
        );
        assert_eq!(state.settings().ui.text_size_percent, 100);
        assert!(!state.settings().ui.reduce_motion);
        assert_eq!(
            state.settings().ui.theme_preset,
            crate::bridge::ThemePreset::Light
        );
        assert!(!state.settings().backend.sounds);
        assert_eq!(effects, vec![Effect::SettingsChanged]);
    }

    #[test]
    fn about_is_first_but_general_opens_on_every_settings_open() {
        let mut state = State::default();
        assert_eq!(Tab::ALL[0], Tab::About);
        assert_eq!(Tab::ALL[1], Tab::General);
        open_settings(&mut state);
        assert_eq!(state.active_tab(), Tab::General);
        let effects = update(&mut state, Message::TabSelected(Tab::About));
        assert!(matches!(
            effects.as_slice(),
            [Effect::DebugInfoRequested(_)]
        ));
        update(&mut state, Message::CloseFinished);
        open_settings(&mut state);
        assert_eq!(state.active_tab(), Tab::General);
        assert!(!state.about.expanded);
        assert!(!state.about.copied);
    }

    #[test]
    fn debug_copy_uses_the_displayed_report_and_waits_for_clipboard_dispatch() {
        let mut state = State::default();
        open_settings(&mut state);
        assert!(update(&mut state, Message::CopyDebugInfoRequested).is_empty());
        update(&mut state, Message::TabSelected(Tab::About));
        let generation = state.about.generation;
        assert!(update(&mut state, Message::CopyDebugInfoRequested).is_empty());
        let report = "Version: test\nOS: test".to_owned();
        update(
            &mut state,
            Message::DebugInfoLoaded(generation, Ok(report.clone())),
        );
        assert_eq!(
            update(&mut state, Message::CopyDebugInfoRequested),
            vec![Effect::CopyDebugInfoRequested(generation, report),]
        );
        assert!(!state.about.copied);
        update(&mut state, Message::DebugInfoCopied(generation));
        assert!(state.about.copied);
        update(&mut state, Message::DebugInfoRequested);
        update(&mut state, Message::DebugInfoCopied(generation));
        assert!(
            !state.about.copied,
            "a previous copy must not confirm a newer report"
        );
    }

    #[test]
    fn about_links_open_the_intended_external_target() {
        let mut state = State::default();
        assert_eq!(
            update(
                &mut state,
                Message::AboutLinkOpened(super::super::AboutLink::Upstream)
            ),
            vec![Effect::OpenUrlRequested(
                "https://github.com/WilliamVenner/gmpublisher"
            )]
        );
    }

    #[test]
    fn an_older_completion_publishes_durable_changes_with_newer_edits_overlaid() {
        let mut state = State::default();
        let stale = state.apply_save_started(SettingsMutation::Sounds(false));
        let current = state.apply_save_started(SettingsMutation::Sounds(true));
        assert_ne!(stale, current);

        let snapshot = test_snapshot();
        let effects = update(
            &mut state,
            Message::SaveCompleted(stale, Ok(Box::new(snapshot.clone()))),
        );
        assert_eq!(effects, vec![Effect::SnapshotApplied]);
        assert!(state.settings().backend.sounds);

        let effects = update(
            &mut state,
            Message::SaveCompleted(current, Ok(Box::new(snapshot))),
        );
        assert_eq!(effects.len(), 1, "the current save still applies");
    }

    #[test]
    fn tab_selection_updates_active_tab() {
        let mut state = State::default();

        let _task = update(&mut state, Message::TabSelected(Tab::Paths));

        assert_eq!(state.active_tab(), Tab::Paths);
    }

    #[test]
    fn close_clears_open_state() {
        let mut state = State::default();
        open_settings(&mut state);

        let effects = update(&mut state, Message::CloseRequested);

        assert_eq!(effects, vec![Effect::ModalCloseRequested]);
        assert!(state.open());

        let effects = update(&mut state, Message::CloseFinished);

        assert!(effects.is_empty());
        assert!(!state.open());
    }

    #[test]
    fn escape_closes_color_picker_before_settings_modal() {
        let mut state = State::default();
        open_settings(&mut state);
        state.toggle_color_picker(ColorSetting::Neutral);

        let effects = update(&mut state, Message::EscapePressed);

        assert!(effects.is_empty());
        assert!(state.open());
        assert!(!state.picker_expanded(ColorSetting::Neutral));
    }

    #[test]
    fn escape_without_inner_layer_requests_modal_close() {
        let mut state = State::default();
        open_settings(&mut state);

        let effects = update(&mut state, Message::EscapePressed);

        assert_eq!(effects, vec![Effect::ModalCloseRequested]);
        assert!(state.open());
    }

    #[test]
    fn unchanged_scalar_setting_emits_no_persistence_effect() {
        let mut state = State::default();
        open_settings(&mut state);
        let enabled = state.settings().backend.sounds;

        let effects = update(&mut state, Message::SoundsToggled(enabled));

        assert!(effects.is_empty());
    }

    #[test]
    fn changed_scalar_setting_emits_mutation_effect_and_marks_saving() {
        let mut state = State::default();
        open_settings(&mut state);
        let enabled = !state.settings().backend.sounds;

        let effects = update(&mut state, Message::SoundsToggled(enabled));

        assert_eq!(effects, vec![Effect::SettingsChanged]);
        assert_eq!(state.status_key(), Some("settings-saving"));
    }

    #[test]
    fn accepted_path_requests_validation_effect() {
        let mut state = State::default();
        open_settings(&mut state);
        let _effects = update(
            &mut state,
            Message::PathEdited(
                PathSetting::Temp,
                std::env::temp_dir().display().to_string(),
            ),
        );

        let effects = update(&mut state, Message::PathAccepted(PathSetting::Temp));

        assert!(matches!(
            effects.as_slice(),
            [Effect::PathValidationRequested(request)] if request.kind == PathSetting::Temp
        ));
    }

    #[test]
    fn save_success_emits_snapshot_effect_but_failure_does_not() {
        let mut state = State::default();
        let snapshot = test_snapshot();

        let generation =
            state.apply_save_started(SettingsMutation::Sounds(snapshot.settings.backend.sounds));
        let effects = update(
            &mut state,
            Message::SaveCompleted(generation, Ok(Box::new(snapshot))),
        );

        assert_eq!(effects, vec![Effect::SnapshotApplied]);

        let effects = update(
            &mut state,
            Message::SaveCompleted(
                generation,
                Err(crate::bridge::ui_error::UiError::detailed(
                    gmpublished_backend::error_keys::IO_ERROR,
                    Some("failed".to_owned()),
                )),
            ),
        );

        assert!(effects.is_empty());
    }

    #[test]
    fn reset_confirmation_emits_run_effect() {
        let mut state = State::default();
        open_settings(&mut state);
        let _effects = update(&mut state, Message::ResetRequested(ResetAction::Settings));

        let effects = update(&mut state, Message::ResetConfirmed);

        assert_eq!(effects, vec![Effect::PersistenceRequested]);
        assert!(state.reset_busy());
    }
}
