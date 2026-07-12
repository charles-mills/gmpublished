use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use crate::gma::{ExtractDestination, ExtractionOverwriteMode};

use crate::GMOD_APP_ID;
use crate::events::BackendEvent;
use crate::steam::Steam;
use crate::transactions::Transactions;
use arc_swap::ArcSwap;
use parking_lot::Mutex;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
pub use steamworks::PublishedFileId as SettingsPublishedFileId;
use steamworks::PublishedFileId;

/// Environment-derived roots `AppData` resolves paths against. Production
/// builds derive these from `dirs`/`std::env`; tests supply a private
/// tempdir root so parallel tests never share a settings file.
#[derive(Debug, Clone)]
pub struct AppDataPaths {
    pub settings_file: PathBuf,
    /// Upstream gmpublisher's settings file: read once to seed a fresh
    /// install, never written. Sharing the path would be lossy — upstream's
    /// save rewrites the file without the fields this fork added (theme,
    /// …), so an upstream run would silently erase them.
    pub legacy_settings_file: PathBuf,
    pub default_user_data_dir: PathBuf,
    pub default_temp_dir: PathBuf,
    pub default_downloads_dir: Option<PathBuf>,
}

impl AppDataPaths {
    #[must_use]
    pub fn production() -> Self {
        let user_data_dir = dirs::data_dir()
            .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| std::env::temp_dir()))
            .join("gmpublisher");
        let settings_root = dirs::config_dir().unwrap_or_else(|| {
            dirs::data_dir()
                .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| std::env::temp_dir()))
        });

        Self {
            settings_file: settings_root.join("gmpublished/settings.json"),
            legacy_settings_file: settings_root.join("gmpublisher/settings.json"),
            default_user_data_dir: user_data_dir,
            default_temp_dir: std::env::temp_dir().join("gmpublisher"),
            default_downloads_dir: dirs::download_dir(),
        }
    }

    /// A private root for exactly one test: every path lives under `root`,
    /// so distinct tests (and nextest's per-test processes) never share a
    /// settings file on disk.
    #[must_use]
    pub fn for_test_root(root: &Path) -> Self {
        Self {
            settings_file: root.join("gmpublished/settings.json"),
            legacy_settings_file: root.join("gmpublisher/settings.json"),
            default_user_data_dir: root.join("default-user-data"),
            default_temp_dir: root.join("default-temp"),
            default_downloads_dir: None,
        }
    }
}

/// Returns the app-owned cache root (`<OS cache dir>/gmpublished`).
///
/// Every file under this directory is disposable: deleting it at any moment
/// loses no user data and self-heals on next use. Stateless and
/// environment-derived only (no settings override, no test-mode branch), so
/// it is a plain recomputed lookup rather than a stored path.
#[must_use]
pub fn cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|dir| dir.join("gmpublished"))
}

#[derive(Debug)]
pub struct OpenCount(AtomicU32);
impl OpenCount {
    fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    fn get(&self) -> u32 {
        self.0.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn set(&self, value: u32) {
        self.0.store(value, Ordering::Release);
    }
}
impl serde::Serialize for OpenCount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u32(self.get())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TitlebarPreference {
    #[default]
    Auto,
    System,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal user setting, not a mode"
)]
pub struct Settings {
    pub temp: Option<PathBuf>,
    pub gmod: Option<PathBuf>,
    pub user_data: Option<PathBuf>,
    pub downloads: Option<PathBuf>,

    pub sounds: bool,

    pub window_size: (f64, f64),
    pub window_maximized: bool,
    #[serde(default)]
    pub titlebar: TitlebarPreference,

    pub extract_destination: ExtractDestination,
    pub destinations: Vec<PathBuf>,
    pub create_folder_on_extract: bool,

    pub ignore_globs: Vec<String>,

    pub my_workshop_local_paths: HashMap<PublishedFileId, PathBuf>,
    pub upscale_addon_icon: bool,

    pub language: Option<String>,

    pub extract_overwrite_mode: ExtractionOverwriteMode,

    pub color_neutral: u32,
    pub color_error: u32,
    pub color_success: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppDataSnapshot {
    pub settings: Settings,
    pub version: &'static str,
    pub open_count: u32,
    pub paths: AppDataPathsSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDataPathsSnapshot {
    pub settings_file: PathBuf,
    pub default_user_data_dir: PathBuf,
    pub default_temp_dir: PathBuf,
    pub default_downloads_dir: Option<PathBuf>,
    pub temp_dir: PathBuf,
    pub user_data_dir: PathBuf,
    pub downloads_dir: Option<PathBuf>,
    pub gmod_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            temp: None,
            gmod: None,
            user_data: None,
            downloads: None,

            extract_destination: ExtractDestination::default(),
            sounds: true,

            window_size: (800., 600.),
            window_maximized: false,
            titlebar: TitlebarPreference::default(),

            destinations: Vec::new(),
            create_folder_on_extract: true,

            ignore_globs: Vec::new(),
            my_workshop_local_paths: HashMap::new(),
            upscale_addon_icon: true,

            language: None,

            extract_overwrite_mode: ExtractionOverwriteMode::default(),

            color_neutral: 28103,
            color_error: 11010048,
            color_success: 3188321,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SettingsSanitizeContext {
    downloads_dir_available: bool,
    gmod_dir_available: bool,
}

/// Errors that can occur while loading or saving the settings file.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
impl crate::error_key::HasErrorKey for SettingsError {
    fn error_key(&self) -> crate::error_key::ErrorKey {
        crate::error_key::keys::IO_ERROR
    }

    fn error_detail(&self) -> Option<String> {
        Some(self.to_string())
    }
}

impl Settings {
    pub fn load_or_default(paths: &AppDataPaths) -> Self {
        log::info!("initializing settings");
        match Self::load(paths) {
            Ok(settings) => {
                if !paths.settings_file.exists() {
                    // Loaded via the legacy fallback: persist to our own path
                    // now so the settings survive the upstream install being
                    // removed.
                    match settings.save(paths) {
                        Ok(()) => log::info!(
                            "migrated settings from legacy path {}",
                            paths.legacy_settings_file.display()
                        ),
                        Err(error) => {
                            log::warn!("failed to persist migrated legacy settings: {error}");
                        }
                    }
                }
                settings
            }
            Err(error) => {
                if matches!(&error, SettingsError::Io(io) if io.kind() == std::io::ErrorKind::NotFound)
                {
                    log::warn!(
                        "settings file {} was not found; using defaults",
                        paths.settings_file.display()
                    );
                } else {
                    log::warn!(
                        "failed to load settings from {}: {error}; using defaults",
                        paths.settings_file.display()
                    );
                }
                Self::default()
            }
        }
    }

    fn load(paths: &AppDataPaths) -> Result<Self, SettingsError> {
        let contents = match fs::read_to_string(&paths.settings_file) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::read_to_string(&paths.legacy_settings_file)?
            }
            Err(error) => return Err(error.into()),
        };
        let settings: Self = serde_json::de::from_str(&contents)?;
        Ok(settings)
    }

    pub fn save(&self, paths: &AppDataPaths) -> Result<(), SettingsError> {
        let parent = paths.settings_file.parent();
        if let Some(parent) = parent {
            std::fs::create_dir_all(parent)?;
        }

        // Write-then-rename so a crash mid-write can never corrupt
        // settings.json. The tempfile lives in the same directory as the
        // target to keep the rename atomic (same filesystem).
        let mut tmp = match parent {
            Some(parent) => tempfile::NamedTempFile::new_in(parent)?,
            None => tempfile::NamedTempFile::new()?,
        };
        serde_json::ser::to_writer(&mut tmp, self)?;
        tmp.persist(&paths.settings_file)
            .map_err(|error| SettingsError::Io(error.error))?;

        Ok(())
    }

    fn sanitize_with_context(&mut self, context: &SettingsSanitizeContext) {
        self.destinations
            .retain(|dir| dir.is_absolute() && dir.is_dir());
        self.my_workshop_local_paths
            .retain(|_, dir| dir.is_absolute() && dir.is_dir());

        match &self.extract_destination {
            ExtractDestination::Directory(path) => {
                if self.create_folder_on_extract || !path.is_dir() {
                    self.extract_destination = ExtractDestination::NamedDirectory(path.to_owned());
                }
            }
            ExtractDestination::NamedDirectory(path) => {
                if !self.create_folder_on_extract || !path.is_dir() {
                    self.extract_destination = ExtractDestination::Directory(path.to_owned());
                }
            }
            ExtractDestination::Downloads if !context.downloads_dir_available => {
                self.extract_destination = ExtractDestination::default();
            }
            ExtractDestination::Addons if !context.gmod_dir_available => {
                self.extract_destination = ExtractDestination::default();
            }
            _ => {}
        }

        self.destinations.truncate(20);
    }
}

#[derive(Debug)]
pub struct AppData {
    pub settings: ArcSwap<Settings>,
    pub version: &'static str,
    pub open_count: OpenCount,
    /// Populated the first time [`Self::discover_gmod_dir`] finds a path via
    /// Steam, so the cheap [`Self::gmod_dir`] accessor (and therefore
    /// `Serialize`/`snapshot`) can report it without blocking.
    discovered_gmod_dir: Mutex<Option<PathBuf>>,
    paths: AppDataPaths,
    transactions: Transactions,
}
impl Serialize for AppData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("AppData", 7)?;
        state.serialize_field("settings", &**self.settings.load())?;
        state.serialize_field("version", self.version)?;
        state.serialize_field("open_count", &self.open_count)?;
        state.serialize_field("temp_dir", &self.temp_dir())?;
        state.serialize_field("gmod_dir", &self.gmod_dir())?;
        state.serialize_field("user_data_dir", &self.user_data_dir())?;
        state.serialize_field("downloads_dir", &self.downloads_dir())?;
        state.end()
    }
}
impl AppData {
    #[must_use]
    pub fn load(paths: AppDataPaths, transactions: Transactions) -> Self {
        let settings = Settings::load_or_default(&paths);
        Self {
            settings: ArcSwap::from_pointee(settings),
            version: env!("CARGO_PKG_VERSION"),
            open_count: OpenCount::new(),
            discovered_gmod_dir: Mutex::new(None),
            paths,
            transactions,
        }
    }

    pub fn send(&self) {
        self.transactions
            .emit(BackendEvent::AppDataUpdated(Box::new(self.snapshot())));
    }

    pub fn snapshot(&self) -> AppDataSnapshot {
        let settings = Settings::clone(&self.settings.load());
        let temp_dir = self.temp_dir();
        let user_data_dir = self.user_data_dir();
        let downloads_dir = self.downloads_dir();
        let gmod_dir = self.gmod_dir();

        AppDataSnapshot {
            settings,
            version: self.version,
            open_count: self.open_count.get(),
            paths: AppDataPathsSnapshot {
                settings_file: self.paths.settings_file.clone(),
                default_user_data_dir: self.paths.default_user_data_dir.clone(),
                default_temp_dir: self.paths.default_temp_dir.clone(),
                default_downloads_dir: self.paths.default_downloads_dir.clone(),
                temp_dir,
                user_data_dir,
                downloads_dir,
                gmod_dir,
            },
        }
    }

    /// Cheap snapshot accessor: the user-configured path, else a previously
    /// [`Self::discover_gmod_dir`]-ed path. Never blocks or performs I/O
    /// beyond a single `is_dir` check, so `Serialize`/`snapshot` can call it
    /// freely.
    pub fn gmod_dir(&self) -> Option<PathBuf> {
        let settings = self.settings.load();
        if let Some(gmod) = settings.gmod.as_ref()
            && gmod.is_dir()
        {
            return Some(gmod.to_owned());
        }
        drop(settings);

        self.discovered_gmod_dir.lock().clone()
    }

    /// Full Garry's Mod discovery: the user-configured path, else Steam
    /// library folders, else (after a short wait for Steam to connect) the
    /// Steamworks install-dir query. May block for several seconds and
    /// perform I/O; never call from a path that must not block (accessors,
    /// `Serialize`). A discovered path is cached for [`Self::gmod_dir`].
    pub fn discover_gmod_dir(&self, steam: &Steam) -> Option<PathBuf> {
        log::info!("Locating Garry's Mod...");

        if let Some(gmod) = self.gmod_dir() {
            log::info!("Using user-defined or previously discovered path");
            return Some(gmod);
        }

        let discovered = self.discover_gmod_dir_uncached(steam);
        if let Some(path) = &discovered {
            *self.discovered_gmod_dir.lock() = Some(path.clone());
        }
        discovered
    }

    fn discover_gmod_dir_uncached(&self, steam: &Steam) -> Option<PathBuf> {
        if !steam.connected() {
            log::info!("Steam is not connected, parsing Steam library folders...");
            if let Some(path) = steamlocate::SteamDir::locate().ok().and_then(|steam_dir| {
                steam_dir
                    .find_app(GMOD_APP_ID.0)
                    .ok()
                    .flatten()
                    .map(|(app, library)| library.resolve_app_dir(&app))
            }) {
                log::info!("Located!");
                return Some(path);
            }
            log::warn!("Failed to parse Steam library folders. Waiting for Steam...");
            for i in 0..3_u8 {
                std::thread::sleep(std::time::Duration::from_secs(1));
                if steam.connected() {
                    log::info!("Steam connected!");
                    break;
                } else if i == 2 {
                    log::warn!("Gave up.");
                    return None;
                }
            }
        }

        log::info!("Getting Garry's Mod location from Steamworks...");
        let gmod: PathBuf = steam
            .client()
            .ok()?
            .apps()
            .app_install_dir(GMOD_APP_ID)
            .into();
        if gmod.is_dir() {
            log::info!("Located!");
            Some(gmod)
        } else {
            log::warn!("Failed.");
            None
        }
    }

    pub fn temp_dir(&self) -> PathBuf {
        let settings = self.settings.load();
        if let Some(temp) = settings.temp.as_ref()
            && temp.is_dir()
        {
            return temp.clone();
        }

        self.paths.default_temp_dir.clone()
    }

    pub fn user_data_dir(&self) -> PathBuf {
        let settings = self.settings.load();
        if let Some(user_data) = settings.user_data.as_ref()
            && user_data.is_dir()
        {
            return user_data.clone();
        }

        self.paths.default_user_data_dir.clone()
    }

    pub fn downloads_dir(&self) -> Option<PathBuf> {
        let settings = self.settings.load();
        if let Some(downloads) = settings.downloads.as_ref()
            && downloads.is_dir()
        {
            return settings.downloads.clone();
        }

        self.paths.default_downloads_dir.clone()
    }

    pub(crate) fn logging_logs_dir(&self) -> PathBuf {
        let mut logs = self.temp_dir();
        logs.push("logs");
        logs
    }

    pub(crate) fn extraction_context(
        &self,
        steam: &Steam,
        resolve_gmod_dir: bool,
    ) -> crate::gma::extract::ExtractionAppDataContext {
        let temp_dir = self.temp_dir();
        let downloads_dir = self.downloads_dir();
        let gmod_dir = if resolve_gmod_dir {
            self.discover_gmod_dir(steam)
        } else {
            None
        };
        let overwrite_mode = self.settings.load().extract_overwrite_mode.clone();

        crate::gma::extract::ExtractionAppDataContext {
            temp_dir,
            downloads_dir,
            gmod_dir,
            overwrite_mode,
        }
    }

    pub(crate) fn extract_destination_snapshot(&self) -> ExtractDestination {
        self.settings.load().extract_destination.clone()
    }

    pub(crate) fn publish_ignore_globs_snapshot(&self) -> Vec<String> {
        self.settings.load().ignore_globs.clone()
    }

    pub(crate) fn apply_publish_settings_snapshot(
        &self,
        temp: Option<&Path>,
        ignore_globs: &[String],
    ) {
        self.settings.rcu(|settings| {
            let mut settings = Settings::clone(settings);
            settings.temp = temp.map(Path::to_path_buf);
            settings.ignore_globs = ignore_globs.to_vec();
            settings
        });
    }

    /// Clones the current settings, lets `mutate` edit the copy, and
    /// publishes it atomically. Readers are never blocked; concurrent
    /// mutations retry (rcu).
    pub fn mutate_settings(&self, mut mutate: impl FnMut(&mut Settings)) {
        self.settings.rcu(|settings| {
            let mut settings = Settings::clone(settings);
            mutate(&mut settings);
            settings
        });
    }

    pub(crate) fn record_published_local_path(
        &self,
        published_file_id: PublishedFileId,
        content_path_src: &Path,
    ) {
        self.settings.rcu(|settings| {
            let mut settings = Settings::clone(settings);
            settings
                .my_workshop_local_paths
                .insert(published_file_id, content_path_src.to_path_buf());
            settings
        });
        if let Err(error) = self.settings.load().save(&self.paths) {
            log::warn!(
                "failed to save settings to {} after recording workshop item local path: {error}",
                self.paths.settings_file.display()
            );
        }
        self.send();
    }

    fn should_send_after_steam_init_if_gmod_unset(&self) -> bool {
        self.settings.load().gmod.is_none()
    }

    pub(crate) fn send_after_steam_init_if_gmod_unset(&self, steam: &Steam) {
        // Discover eagerly so the event this fires actually carries a
        // resolved path (the cheap accessor alone would still report None).
        if self.should_send_after_steam_init_if_gmod_unset() {
            self.discover_gmod_dir(steam);
            self.send();
        }
    }

    /// Sanitizes and persists `settings`, then installs it as the live
    /// state. Persistence happens first: a failed save leaves the live
    /// `ArcSwap` (and the file on disk) exactly as they were.
    pub fn update_settings(
        &self,
        mut settings: Settings,
        steam: &Steam,
    ) -> Result<(), SettingsError> {
        let context = self.sanitize_context(&settings.extract_destination, steam);
        settings.sanitize_with_context(&context);

        settings.save(&self.paths)?;

        let rediscover_addons = self.settings.load().gmod != settings.gmod;

        self.settings.store(std::sync::Arc::new(settings));

        if rediscover_addons {
            self.transactions
                .emit(BackendEvent::InstalledAddonsRefreshed);
        }

        self.send();

        Ok(())
    }

    fn sanitize_context(
        &self,
        destination: &ExtractDestination,
        steam: &Steam,
    ) -> SettingsSanitizeContext {
        if !matches!(
            destination,
            ExtractDestination::Downloads | ExtractDestination::Addons
        ) {
            return SettingsSanitizeContext::default();
        }

        match destination {
            ExtractDestination::Downloads => SettingsSanitizeContext {
                downloads_dir_available: self.downloads_dir().is_some(),
                ..SettingsSanitizeContext::default()
            },
            ExtractDestination::Addons => SettingsSanitizeContext {
                gmod_dir_available: self.discover_gmod_dir(steam).is_some(),
                ..SettingsSanitizeContext::default()
            },
            _ => SettingsSanitizeContext::default(),
        }
    }
}

pub fn validate_gmod(mut path: PathBuf) -> bool {
    path.push("GarrysMod");
    path.push("addons");
    path.is_absolute() && path.is_dir()
}

#[cfg(test)]
mod tests;
