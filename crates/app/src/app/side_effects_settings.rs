#[cfg(target_os = "macos")]
use super::window;
use super::{
    App, LibraryRefreshReason, RootMessage, Task, flatten_blocking_ui_result, settings, shell,
};
use crate::bridge::ui_error::ResultExt as _;

impl App {
    pub(super) fn settings_debug_info_task(
        &self,
        generation: crate::generation::Generation,
    ) -> Task<RootMessage> {
        let ctx = self.environment.ctx.clone();
        let window_id = self.window_id;
        let locale = self.state.i18n.locale_id().to_owned();
        let theme = format!(
            "{:?} ({:?})",
            self.state.theme_preset, self.state.system_scheme
        );
        let steam = format!("{:?}", self.state.features.shell.steam_status());
        iced::system::information().then(move |info| {
            let ctx = ctx.clone();
            let locale = locale.clone();
            let theme = theme.clone();
            let steam = steam.clone();
            window_id
                .map_or_else(|| Task::done(1.0), iced::window::scale_factor)
                .then(move |scale| {
                    let info = info.clone();
                    let locale = locale.clone();
                    let theme = theme.clone();
                    let steam = steam.clone();
                    ctx.run_blocking_ui("settings-debug-info", move |_| {
                        Ok(settings::debug_report(
                            &info, scale, &locale, &theme, &steam,
                        ))
                    })
                    .map(move |report| {
                        RootMessage::Settings(settings::Message::DebugInfoLoaded(
                            generation, report,
                        ))
                    })
                })
        })
    }

    pub(super) fn apply_settings_snapshot_runtime(
        &mut self,
        snapshot: &settings::SettingsSnapshot,
    ) -> Task<RootMessage> {
        self.state
            .features
            .settings
            .observe_snapshot(snapshot.clone());
        self.sync_settings_runtime(true)
    }

    pub(super) fn sync_settings_runtime(&mut self, durable: bool) -> Task<RootMessage> {
        let snapshot = self.state.features.settings.current_snapshot();
        let (previous_chrome, gmod_dir_changed) = if durable {
            let result = self
                .state
                .apply_settings_snapshot(snapshot.settings, snapshot.paths);
            (result.previous_chrome, result.gmod_dir_changed)
        } else {
            let previous = self.state.chrome_strategy;
            self.state
                .apply_runtime_settings(&snapshot.settings, self.update_context.now, true);
            (previous, false)
        };
        self.sync_game_prerequisite();
        let workshop_language = self.sync_workshop_language_task();
        let library_refresh = if gmod_dir_changed {
            Task::done(RootMessage::LibraryRefreshRequested(
                LibraryRefreshReason::SettingsChanged,
            ))
        } else {
            Task::none()
        };
        #[cfg(target_os = "macos")]
        self.install_macos_menu();
        Task::batch([
            self.chrome_strategy_apply_task(previous_chrome),
            library_refresh,
            workshop_language,
            self.refresh_ui_metrics_task(),
        ])
    }

    #[cfg(test)]
    pub(super) fn apply_settings_mutation_runtime(
        &mut self,
        mutation: &settings::SettingsMutation,
    ) -> Task<RootMessage> {
        if let Some(mutation) = self
            .state
            .features
            .settings
            .scalar_mutation(mutation.clone())
        {
            self.state.features.settings.apply_save_started(mutation);
        }
        self.sync_settings_runtime(false)
    }

    fn sync_workshop_language_task(&mut self) -> Task<RootMessage> {
        let language =
            gmpublished_backend::WorkshopLanguage::from_locale(self.state.i18n.locale_id());
        if !self.environment.ctx.set_workshop_language(language) {
            return Task::none();
        }
        Task::batch([
            self.apply_my_workshop_message(
                crate::features::my_workshop::Message::WorkshopLanguageChanged,
            ),
            self.apply_installed_addons_message(
                crate::features::installed_addons::Message::WorkshopLanguageChanged,
            ),
            self.apply_preview_gma_message(
                crate::features::preview_gma::Message::WorkshopLanguageChanged(language),
                self.update_context,
            ),
        ])
    }

    #[cfg(target_os = "macos")]
    fn chrome_strategy_apply_task(&self, previous: shell::ChromeStrategy) -> Task<RootMessage> {
        if self.state.chrome_strategy == previous {
            return Task::none();
        }

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
    fn chrome_strategy_apply_task(&self, _previous: shell::ChromeStrategy) -> Task<RootMessage> {
        Task::none()
    }

    pub(super) fn settings_snapshot(&self) -> Box<settings::SettingsSnapshot> {
        let (settings, paths) = self.environment.ctx.settings_and_paths_snapshot();
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
        let directory = self.state.features.settings.initial_browse_directory(kind);
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
        self.environment
            .ctx
            .run_blocking("settings-validate-path", move |_app| {
                Ok(settings::validate_path_request(request))
            })
            .map(|result| match flatten_blocking_ui_result(result) {
                Ok(result) => {
                    RootMessage::Settings(settings::Message::PathValidationCompleted(result))
                }
                Err(error) => RootMessage::Settings(settings::Message::SaveCompleted(
                    crate::generation::Generation::INITIAL,
                    Err(error),
                )),
            })
    }

    pub(super) fn start_next_settings_save(&mut self) -> Task<RootMessage> {
        match self.state.features.settings.next_persistence() {
            Some(settings::PersistenceCommand::Save(generation, mutation)) => {
                self.settings_save_task(generation, mutation)
            }
            Some(settings::PersistenceCommand::Reset(action)) => self.settings_reset_task(action),
            None => Task::none(),
        }
    }

    pub(super) fn settings_save_task(
        &self,
        generation: crate::generation::Generation,
        mutation: settings::SettingsMutation,
    ) -> Task<RootMessage> {
        let system_scheme = self.state.system_scheme;
        self.environment
            .ctx
            .run_blocking("settings-save", move |app| {
                app.update_settings_snapshot(|settings| {
                    settings::apply_settings_mutation(settings, mutation);
                })
                .map(|()| {
                    let (settings, paths) = app.settings_and_paths_snapshot();
                    Box::new(settings::SettingsSnapshot::new(
                        settings,
                        paths,
                        system_scheme,
                    ))
                })
                .ui_err()
            })
            .map(move |result| {
                RootMessage::Settings(settings::Message::SaveCompleted(
                    generation,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn settings_reset_task(&self, action: settings::ResetAction) -> Task<RootMessage> {
        let system_scheme = self.state.system_scheme;
        self.environment
            .ctx
            .run_blocking("settings-reset", move |app| {
                let settings = match action {
                    settings::ResetAction::Settings => app.reset_settings().map(Some).ui_err()?,
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
