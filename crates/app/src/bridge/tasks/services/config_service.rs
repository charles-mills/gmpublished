use super::BackendServices;
use crate::bridge::{
    AppPaths, Settings, SettingsPersistError,
    tasks::projections::clear_directory_contents,
    ui_error::{ResultExt as _, UiError},
};
use gmpublished_backend::AppDataSnapshot as BackendAppDataSnapshot;
use std::path::PathBuf;

/// Configuration capability exposed to app features.
#[derive(Clone, Copy)]
pub struct ConfigService<'a> {
    inner: &'a BackendServices,
}

impl<'a> ConfigService<'a> {
    pub(super) const fn new(inner: &'a BackendServices) -> Self {
        Self { inner }
    }

    pub(crate) fn settings_snapshot(self) -> Settings {
        self.inner.configuration.snapshot().settings.clone()
    }
    pub(crate) fn sounds_enabled(self) -> bool {
        self.inner.configuration.snapshot().settings.backend.sounds
    }
    pub(crate) fn paths(self) -> AppPaths {
        self.inner.configuration.snapshot().paths.clone()
    }
    pub(crate) fn settings_and_paths_snapshot(self) -> (Settings, AppPaths) {
        let configuration = self.inner.configuration.snapshot();
        (configuration.settings.clone(), configuration.paths.clone())
    }
    pub(crate) fn game_paths(self) -> (Option<PathBuf>, Option<PathBuf>) {
        let configuration = self.inner.configuration.snapshot();
        (
            configuration.settings.backend.gmod.clone(),
            configuration.paths.gmod_dir.clone(),
        )
    }
    pub(crate) fn rediscover_gmod_dir(self) -> Option<PathBuf> {
        let discovered = self.inner.backend.discover_gmod_dir();
        self.inner
            .configuration
            .publish_discovered_gmod(discovered.clone());
        discovered
    }
    pub(crate) fn update_settings_snapshot(
        self,
        update: impl FnOnce(&mut Settings),
    ) -> Result<(), SettingsPersistError> {
        self.inner.configuration.update(&self.inner.backend, update)
    }
    pub(crate) fn reset_settings(self) -> Result<Settings, SettingsPersistError> {
        self.update_settings_snapshot(|settings| *settings = Settings::default())?;
        Ok(self.settings_snapshot())
    }
    pub(crate) fn apply_appdata_snapshot(
        self,
        snapshot: BackendAppDataSnapshot,
    ) -> (Settings, AppPaths) {
        let configuration = self.inner.configuration.apply_backend_snapshot(snapshot);
        (configuration.settings.clone(), configuration.paths.clone())
    }
    pub(crate) fn clear_temp_files(self) -> Result<(), UiError> {
        clear_directory_contents(&self.paths().temp_dir).ui_err()
    }
    pub(crate) fn clear_user_data(self) -> Result<(), UiError> {
        clear_directory_contents(&self.paths().user_data_dir).ui_err()
    }
}
