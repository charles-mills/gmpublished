#[cfg(target_os = "macos")]
use super::window;
use super::{
    App, LibraryRefreshReason, RootMessage, Task, UiError, destination_select,
    flatten_blocking_ui_result, resolve_tokens, settings, shell, theme,
};

impl App {
    pub(super) fn apply_settings_snapshot_runtime(
        &mut self,
        snapshot: &settings::SettingsSnapshot,
    ) -> Task<RootMessage> {
        let previous = self.state.chrome_strategy;
        let previous_gmod_dir = self.state.installed_addons.watch_gmod_dir().cloned();
        self.state.apply_runtime_settings(&snapshot.settings);
        self.state
            .installed_addons
            .set_watch_gmod_dir(snapshot.paths.gmod_dir.clone());
        self.state.destination_select.reset_from_snapshot(
            destination_select::SettingsSnapshot::new(
                snapshot.settings.clone(),
                snapshot.paths.clone(),
            ),
        );
        let label = destination_select::destination_label(&snapshot.settings, &snapshot.paths);
        self.state.downloader.set_destination_label(label);
        let library_refresh = if previous_gmod_dir != snapshot.paths.gmod_dir {
            Task::done(RootMessage::LibraryRefreshRequested(
                LibraryRefreshReason::SettingsChanged,
            ))
        } else {
            Task::none()
        };
        #[cfg(target_os = "macos")]
        self.install_macos_menu();
        Task::batch([self.chrome_strategy_apply_task(previous), library_refresh])
    }

    pub(super) fn apply_settings_mutation_runtime(
        &mut self,
        mutation: &settings::SettingsMutation,
    ) -> Task<RootMessage> {
        let previous = self.state.chrome_strategy;
        match mutation {
            settings::SettingsMutation::PlayGifsByDefault(enabled) => {
                self.state.set_play_gifs_by_default(*enabled);
            }
            settings::SettingsMutation::Titlebar(preference) => {
                self.state.chrome_strategy = shell::ChromeStrategy::resolve(*preference);
            }
            settings::SettingsMutation::Language(language) => {
                self.state.apply_runtime_language(language.as_deref());
                #[cfg(target_os = "macos")]
                self.install_macos_menu();
            }
            settings::SettingsMutation::DownloadCountFormat(format) => {
                self.state.download_count_format = *format;
                self.state.apply_download_count_formatter();
            }
            settings::SettingsMutation::Theme {
                preset,
                neutral,
                success,
                error,
            } => {
                self.state.theme_preset = *preset;
                self.state.accent_inputs = theme::AccentInputs {
                    neutral: *neutral,
                    success: *success,
                    error: *error,
                };
                self.state.tokens = resolve_tokens(
                    self.state.theme_preset,
                    self.state.system_scheme,
                    self.state.accent_inputs,
                );
            }
            settings::SettingsMutation::Color { kind, rgb } => {
                match kind {
                    settings::ColorSetting::Neutral => self.state.accent_inputs.neutral = *rgb,
                    settings::ColorSetting::Success => self.state.accent_inputs.success = *rgb,
                    settings::ColorSetting::Error => self.state.accent_inputs.error = *rgb,
                }
                self.state.tokens = resolve_tokens(
                    self.state.theme_preset,
                    self.state.system_scheme,
                    self.state.accent_inputs,
                );
            }
            settings::SettingsMutation::Sounds(_)
            | settings::SettingsMutation::OverwriteMode(_)
            | settings::SettingsMutation::Path { .. } => {}
        }
        self.chrome_strategy_apply_task(previous)
    }

    fn chrome_strategy_apply_task(&self, previous: shell::ChromeStrategy) -> Task<RootMessage> {
        if self.state.chrome_strategy == previous {
            return Task::none();
        }

        #[cfg(target_os = "macos")]
        {
            let Some(window_id) = self.window_id else {
                return Task::none();
            };
            let apply_task = window::run(
                window_id,
                crate::platform_chrome::apply(self.state.chrome_strategy.mac_native_inset()),
            )
            .discard();
            apply_task.chain(self.traffic_light_position_task(window_id))
        }

        #[cfg(not(target_os = "macos"))]
        {
            Task::none()
        }
    }

    pub(super) fn settings_snapshot(&self) -> Box<settings::SettingsSnapshot> {
        let (settings, paths) = self.ctx.settings_and_paths_snapshot();
        Box::new(settings::SettingsSnapshot::new(
            settings,
            paths,
            self.state.system_scheme,
        ))
    }

    pub(super) fn settings_open_task(&self) -> Task<RootMessage> {
        Task::done(RootMessage::Settings(settings::Message::OpenRequested(
            self.settings_snapshot(),
        )))
    }

    pub(super) fn settings_folder_picker_task(
        &self,
        kind: settings::PathSetting,
    ) -> Task<RootMessage> {
        let directory = self.state.settings.initial_browse_directory(kind);
        let title = self.state.i18n.tr("native-dialog-select-settings-folder");
        Task::future(async move {
            let selected = rfd::AsyncFileDialog::new()
                .set_title(title)
                .set_directory(directory)
                .set_can_create_directories(true)
                .pick_folder()
                .await
                .map(|folder| folder.path().to_path_buf());
            RootMessage::Settings(settings::Message::PathBrowseCompleted(kind, selected))
        })
    }

    pub(super) fn settings_path_validation_task(
        &self,
        request: settings::PathValidationRequest,
    ) -> Task<RootMessage> {
        self.ctx
            .run_blocking("settings-validate-path", move |_app| {
                Ok(settings::validate_path_request(request))
            })
            .map(|result| match flatten_blocking_ui_result(result) {
                Ok(result) => {
                    RootMessage::Settings(settings::Message::PathValidationCompleted(result))
                }
                Err(error) => RootMessage::Settings(settings::Message::SaveCompleted(Err(error))),
            })
    }

    pub(super) fn settings_save_task(
        &self,
        mutation: settings::SettingsMutation,
    ) -> Task<RootMessage> {
        let system_scheme = self.state.system_scheme;
        self.ctx
            .run_blocking("settings-save", move |app| {
                app.update_settings_snapshot(|settings| {
                    settings::apply_settings_mutation(settings, mutation);
                })
                .map(|()| {
                    Box::new(settings::SettingsSnapshot::new(
                        app.settings_snapshot(),
                        app.paths(),
                        system_scheme,
                    ))
                })
                .map_err(|error| UiError::from(&error))
            })
            .map(|result| {
                RootMessage::Settings(settings::Message::SaveCompleted(
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn settings_reset_task(&self, action: settings::ResetAction) -> Task<RootMessage> {
        let system_scheme = self.state.system_scheme;
        self.ctx
            .run_blocking("settings-reset", move |app| {
                let settings = match action {
                    settings::ResetAction::Settings => app
                        .reset_settings()
                        .map(Some)
                        .map_err(|error| UiError::from(&error))?,
                    settings::ResetAction::TempFiles => app.clear_temp_files().map(|()| None)?,
                    settings::ResetAction::UserData => app.clear_user_data().map(|()| None)?,
                };
                Ok(settings.map(|settings| {
                    Box::new(settings::SettingsSnapshot::new(
                        settings,
                        app.paths(),
                        system_scheme,
                    ))
                }))
            })
            .map(move |result| {
                RootMessage::Settings(settings::Message::ResetCompleted(
                    action,
                    flatten_blocking_ui_result(result),
                ))
            })
    }
}
