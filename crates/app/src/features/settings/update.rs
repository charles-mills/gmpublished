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
            .map(Effect::PreviewApplied)
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
            .map(Effect::ResetRunRequested)
            .into_iter()
            .collect(),
        Message::SaveCompleted(generation, result) => {
            let snapshot = state.apply_save_completed(generation, result.map(|snapshot| *snapshot));
            snapshot
                .map(|snapshot| Effect::SnapshotApplied(Box::new(snapshot)))
                .into_iter()
                .collect()
        }
        Message::ResetCompleted(action, result) => {
            let snapshot = state.apply_reset_completed(
                action,
                result.map(|snapshot| snapshot.map(|snapshot| *snapshot)),
            );
            snapshot
                .map(|snapshot| Effect::SnapshotApplied(Box::new(snapshot)))
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
        .map(|mutation| Effect::MutationApplied(state.apply_save_started(), mutation))
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
    use crate::bridge::{AppPaths, Settings, SystemColorScheme};

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
                vec![Effect::PreviewApplied(SettingsMutation::TextSize(value))]
            );
        }
        assert_eq!(state.save_generation_for_test(), generation);
        assert_eq!(state.settings().ui.text_size_percent, 150);
        assert_eq!(
            update(&mut state, Message::TextSizeCommitted),
            vec![Effect::MutationApplied(
                generation.next(),
                SettingsMutation::TextSize(150)
            )]
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
            [Effect::MutationApplied(_, SettingsMutation::TextSize(125))]
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
        assert!(
            matches!(effects.as_slice(), [Effect::SnapshotApplied(snapshot)]
            if snapshot.settings.ui.text_size_percent == 150)
        );
        assert!(matches!(
            update(&mut state, Message::TextSizeCommitted).as_slice(),
            [Effect::MutationApplied(_, SettingsMutation::TextSize(150))]
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
                [
                    Effect::MutationApplied(_, SettingsMutation::TextSize(125)),
                    Effect::ModalCloseRequested,
                ]
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
            [Effect::MutationApplied(_, SettingsMutation::TextSize(125))]
        ));
        assert!(!state.open());
    }

    #[test]
    fn durable_display_override_survives_drafts_close_and_failed_writes() {
        let mut state = State::default();
        open_settings(&mut state);
        assert_eq!(state.display_preferences_override(), None);
        update(
            &mut state,
            Message::DensitySelected(crate::bridge::UiDensity::Compact),
        );
        update(&mut state, Message::TextSizeChanged(125));
        update(&mut state, Message::TextSizeCommitted);
        let generation = state.save_generation_for_test();
        let expected = Some((crate::bridge::UiDensity::Compact, 125));
        update(
            &mut state,
            Message::PathEdited(PathSetting::Temp, "/tmp/draft".into()),
        );
        assert_eq!(state.display_preferences_override(), expected);
        update(&mut state, Message::CloseFinished);
        assert_eq!(state.display_preferences_override(), expected);
        update(
            &mut state,
            Message::SaveCompleted(
                generation,
                Err(crate::bridge::ui_error::UiError::new(
                    gmpublished_backend::error_keys::IO_ERROR,
                )),
            ),
        );
        assert_eq!(state.display_preferences_override(), expected);
        assert_eq!(state.status_key(), Some("settings-save-failed"));
        let mut durable = test_snapshot();
        durable.settings.ui.density = crate::bridge::UiDensity::Compact;
        durable.settings.ui.text_size_percent = 125;
        update(
            &mut state,
            Message::SaveCompleted(generation, Ok(Box::new(durable))),
        );
        assert_eq!(state.display_preferences_override(), None);
    }

    #[test]
    fn unrelated_save_cannot_revert_a_pending_durable_display_mutation() {
        let mut state = State::default();
        open_settings(&mut state);
        update(
            &mut state,
            Message::DensitySelected(crate::bridge::UiDensity::Compact),
        );
        let display_generation = state.save_generation_for_test();
        update(&mut state, Message::SoundsToggled(false));
        let latest_generation = state.save_generation_for_test();
        let mut unrelated = test_snapshot();
        unrelated.settings.backend.sounds = false;
        update(
            &mut state,
            Message::SaveCompleted(latest_generation, Ok(Box::new(unrelated))),
        );
        assert_eq!(
            state.settings().ui.density,
            crate::bridge::UiDensity::Compact
        );
        assert!(state.display_preferences_override().is_some());
        let mut durable = test_snapshot();
        durable.settings.ui.density = crate::bridge::UiDensity::Compact;
        update(
            &mut state,
            Message::SaveCompleted(display_generation, Ok(Box::new(durable))),
        );
        assert!(state.display_preferences_override().is_some());
        let mut durable = test_snapshot();
        durable.settings.ui.density = crate::bridge::UiDensity::Compact;
        durable.settings.backend.sounds = false;
        state.reconcile_display_preferences(&durable.settings);
        assert_eq!(state.display_preferences_override(), None);
        assert!(!state.settings().backend.sounds);
    }

    #[test]
    fn reduced_motion_updates_immediately_and_survives_stale_snapshots() {
        let mut state = State::default();
        open_settings(&mut state);
        let effects = update(&mut state, Message::ReduceMotionToggled(true));
        assert!(state.settings().ui.reduce_motion);
        assert_eq!(state.reduce_motion_override(), Some(true));
        assert!(matches!(
            effects.as_slice(),
            [Effect::MutationApplied(
                _,
                SettingsMutation::ReduceMotion(true)
            )]
        ));
        assert!(update(&mut state, Message::ReduceMotionToggled(true)).is_empty());
        state.reconcile_display_preferences(&Settings::default());
        assert_eq!(state.reduce_motion_override(), Some(true));
        state.close();
        state.open_with_snapshot(test_snapshot());
        assert!(state.settings().ui.reduce_motion);
        let mut durable = test_snapshot();
        durable.settings.ui.reduce_motion = true;
        state.reconcile_display_preferences(&durable.settings);
        assert_eq!(state.reduce_motion_override(), None);
    }

    #[test]
    fn unrelated_or_failed_save_cannot_revert_pending_motion_preference() {
        let mut state = State::default();
        open_settings(&mut state);
        update(&mut state, Message::ReduceMotionToggled(true));
        update(&mut state, Message::SoundsToggled(false));
        let latest = state.save_generation_for_test();
        let mut unrelated = test_snapshot();
        unrelated.settings.backend.sounds = false;
        state.apply_save_completed(latest, Ok(unrelated));
        assert!(state.settings().ui.reduce_motion);
        assert_eq!(state.reduce_motion_override(), Some(true));
        state.apply_save_completed(
            latest,
            Err(crate::bridge::ui_error::UiError::new(
                gmpublished_backend::error_keys::IO_ERROR,
            )),
        );
        assert_eq!(state.reduce_motion_override(), Some(true));
        let mut durable = test_snapshot();
        durable.settings.ui.reduce_motion = true;
        state.apply_save_completed(latest, Ok(durable));
        assert_eq!(state.reduce_motion_override(), None);
    }

    #[test]
    fn superseded_motion_save_does_not_clear_latest_toggle_protection() {
        let mut state = State::default();
        open_settings(&mut state);
        update(&mut state, Message::ReduceMotionToggled(true));
        let older = state.save_generation_for_test();
        update(&mut state, Message::ReduceMotionToggled(false));
        update(&mut state, Message::ReduceMotionToggled(true));
        let mut durable = test_snapshot();
        durable.settings.ui.reduce_motion = true;
        state.apply_save_completed(older, Ok(durable));
        assert_eq!(state.reduce_motion_override(), Some(true));
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
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::MutationApplied(_, SettingsMutation::ReduceMotion(false))
        )));
        assert_eq!(
            state.settings().ui.theme_preset,
            crate::bridge::ThemePreset::Light
        );
        assert!(!state.settings().backend.sounds);
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::MutationApplied(_, SettingsMutation::TextSize(100))
        )));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::MutationApplied(
                _,
                SettingsMutation::Density(crate::bridge::UiDensity::Comfortable)
            )
        )));
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
    fn a_save_completing_after_a_later_edit_is_discarded() {
        let mut state = State::default();
        let stale = state.apply_save_started();
        let current = state.apply_save_started();
        assert_ne!(stale, current);

        let snapshot = test_snapshot();
        let effects = update(
            &mut state,
            Message::SaveCompleted(stale, Ok(Box::new(snapshot.clone()))),
        );
        assert!(
            effects.is_empty(),
            "a superseded save must not re-publish its snapshot"
        );

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

        assert_eq!(
            effects,
            vec![Effect::MutationApplied(
                state.save_generation_for_test(),
                SettingsMutation::Sounds(enabled)
            )]
        );
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

        let generation = state.save_generation_for_test();
        let effects = update(
            &mut state,
            Message::SaveCompleted(generation, Ok(Box::new(snapshot.clone()))),
        );

        assert_eq!(effects, vec![Effect::SnapshotApplied(Box::new(snapshot))]);

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

        assert_eq!(
            effects,
            vec![Effect::ResetRunRequested(ResetAction::Settings)]
        );
        assert!(state.reset_busy());
    }
}
