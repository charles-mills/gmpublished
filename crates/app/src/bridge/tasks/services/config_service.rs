use super::BackendServices;
use crate::bridge::{
    AppPaths, Settings, SettingsPersistError,
    tasks::projections::clear_directory_contents,
    ui_error::{ResultExt as _, UiError},
};
use gmpublished_backend::AppDataSnapshot as BackendAppDataSnapshot;
use std::path::PathBuf;

impl BackendServices {
    pub(crate) fn settings_snapshot(&self) -> Settings {
        self.configuration.snapshot().settings.clone()
    }
    pub(crate) fn sounds_enabled(&self) -> bool {
        self.configuration.snapshot().settings.backend.sounds
    }
    pub(crate) fn paths(&self) -> AppPaths {
        self.configuration.snapshot().paths.clone()
    }
    pub(crate) fn settings_and_paths_snapshot(&self) -> (Settings, AppPaths) {
        let configuration = self.configuration.snapshot();
        (configuration.settings.clone(), configuration.paths.clone())
    }
    pub(crate) fn game_paths(&self) -> (Option<PathBuf>, Option<PathBuf>) {
        let configuration = self.configuration.snapshot();
        (
            configuration.settings.backend.gmod.clone(),
            configuration.paths.gmod_dir.clone(),
        )
    }
    pub(crate) fn rediscover_gmod_dir(&self) -> Option<PathBuf> {
        let discovered = self.backend.discover_gmod_dir();
        self.configuration
            .publish_discovered_gmod(discovered.clone());
        discovered
    }
    pub(crate) fn update_settings_snapshot(
        &self,
        update: impl FnOnce(&mut Settings),
    ) -> Result<(), SettingsPersistError> {
        self.configuration.update(&self.backend, update)
    }
    pub(crate) fn reset_settings(&self) -> Result<Settings, SettingsPersistError> {
        self.update_settings_snapshot(|settings| *settings = Settings::default())?;
        Ok(self.settings_snapshot())
    }
    pub(crate) fn apply_appdata_snapshot(
        &self,
        snapshot: BackendAppDataSnapshot,
    ) -> (Settings, AppPaths) {
        let configuration = self.configuration.apply_backend_snapshot(snapshot);
        (configuration.settings.clone(), configuration.paths.clone())
    }
    pub(crate) fn clear_temp_files(&self) -> Result<(), UiError> {
        clear_directory_contents(&self.paths().temp_dir).ui_err()
    }
    pub(crate) fn clear_user_data(&self) -> Result<(), UiError> {
        clear_directory_contents(&self.paths().user_data_dir).ui_err()
    }
}
