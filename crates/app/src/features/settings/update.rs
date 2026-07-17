use iced::{Event, Subscription, event, keyboard};

#[cfg(target_os = "macos")]
use crate::bridge::TitlebarPreference;

use super::{Effect, Message, State};

pub fn update(state: &mut State, message: Message) -> Vec<Effect> {
    match message {
        Message::OpenRequested(snapshot) => {
            state.open_with_snapshot(*snapshot);
            vec![Effect::ModalOpenRequested]
        }
        Message::CloseRequested => vec![Effect::ModalCloseRequested],
        Message::CloseFinished => {
            state.close();
            Vec::new()
        }
        Message::TabSelected(tab) => {
            state.select_tab(tab);
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
            let mutation = state.language_mutation(&option.value);
            mutation_effect(mutation, state)
        }
        Message::DownloadCountFormatSelected(option) => {
            let mutation = state.download_count_format_mutation(&option.value);
            mutation_effect(mutation, state)
        }
        Message::ThemeSelected(option) => {
            let mutation = state.theme_mutation(&option.value);
            mutation_effect(mutation, state)
        }
        Message::OverwriteModeSelected(option) => {
            let mutation = state.overwrite_mode_mutation(&option.value);
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
        Message::SaveCompleted(result) => {
            let snapshot = state.apply_save_completed(result.map(|snapshot| *snapshot));
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
                vec![Effect::ModalCloseRequested]
            }
        }
    }
}

fn mutation_effect(mutation: Option<super::SettingsMutation>, state: &mut State) -> Vec<Effect> {
    mutation
        .map(|mutation| {
            state.apply_save_started();
            Effect::MutationApplied(mutation)
        })
        .into_iter()
        .collect()
}

pub fn subscription(state: &State) -> Subscription<Message> {
    if state.open() {
        event::listen_with(keyboard_event)
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
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        }) => Some(Message::EscapePressed),
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
        let enabled = state.settings().sounds;

        let effects = update(&mut state, Message::SoundsToggled(enabled));

        assert!(effects.is_empty());
    }

    #[test]
    fn changed_scalar_setting_emits_mutation_effect_and_marks_saving() {
        let mut state = State::default();
        open_settings(&mut state);
        let enabled = !state.settings().sounds;

        let effects = update(&mut state, Message::SoundsToggled(enabled));

        assert_eq!(
            effects,
            vec![Effect::MutationApplied(SettingsMutation::Sounds(enabled))]
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
        let snapshot = SettingsSnapshot::new(
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
        );

        let effects = update(
            &mut state,
            Message::SaveCompleted(Ok(Box::new(snapshot.clone()))),
        );

        assert_eq!(effects, vec![Effect::SnapshotApplied(Box::new(snapshot))]);

        let effects = update(
            &mut state,
            Message::SaveCompleted(Err(crate::bridge::ui_error::UiError::detailed(
                gmpublished_backend::error_key::keys::IO_ERROR,
                Some("failed".to_owned()),
            ))),
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
