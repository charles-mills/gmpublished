//! Where the app keeps its state on disk, and the settings living there.
//!
//! One [`AppData`] owns every derived path (settings file, temp, user data,
//! downloads) so nothing else has to re-derive them, and it resolves the
//! Garry's Mod directory — configured, discovered through Steam, or absent.
//! Every root is overridable, which is what lets tests run against a private
//! tempdir instead of the developer's real profile.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::gma::{ExtractDestination, ExtractionOverwriteMode};

use crate::WorkshopId;
use crate::events::BackendEvent;
use crate::steam::Steam;
use crate::transactions::Transactions;
use crate::{GMOD_APP_ID, STEAM_GMOD_APP_ID};
use arc_swap::ArcSwap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Environment-derived roots `AppData` resolves paths against. Production
/// builds derive these from `dirs`/`std::env`; tests supply a private
/// tempdir root so parallel tests never share a settings file.
#[derive(Clone, Debug)]
pub struct AppDataPaths {
    pub settings_file: PathBuf,
    pub default_user_data_dir: PathBuf,
    pub default_temp_dir: PathBuf,
    pub default_downloads_dir: Option<PathBuf>,
}

impl AppDataPaths {
    #[must_use]
    pub fn production() -> Self {
        // `current_exe()` names a file, not a directory, and its parent may
        // be read-only (notably for packaged installs). If the platform does
        // not expose a persistent data root, the system temp directory is the
        // only environment-provided location with directory semantics and a
        // reasonable expectation of writability.
        let data_root = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
        let settings_root = dirs::config_dir().unwrap_or_else(|| data_root.clone());

        Self {
            settings_file: settings_root.join("gmpublished/settings.json"),
            default_user_data_dir: data_root.join("gmpublisher"),
            default_temp_dir: default_temp_dir(),
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
            default_user_data_dir: root.join("default-user-data"),
            default_temp_dir: root.join("default-temp"),
            default_downloads_dir: None,
        }
    }
}

/// Default scratch root: where a publish packs its GMA and materializes the
/// preview icon it then hands Steam an absolute path to.
///
/// On Linux this must sit under the user's home rather than the system temp
/// dir. The Steam *client* — not this process — reads the content at that
/// path, and packaged Steam commonly runs sandboxed with a private `/tmp`
/// (NixOS's FHS wrapper and Flatpak both mount a fresh tmpfs over it, while
/// binding home through). A path under `/tmp` therefore reads back as empty
/// and the upload fails with "Build for workshop item has no content". Every
/// other platform runs Steam unsandboxed, so the system temp dir is fine.
fn default_temp_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    if let Some(cache_dir) = cache_dir() {
        return cache_dir.join("temp");
    }

    std::env::temp_dir().join("gmpublisher")
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

/// The `steamapps/common` directory of every Steam library on this machine,
/// from Steam's own library-folders manifest. Empty when Steam cannot be
/// located. Stateless and environment-derived, like [`cache_dir`].
#[must_use]
pub fn steam_library_common_dirs() -> Vec<PathBuf> {
    let Ok(steam_dir) = steamlocate::SteamDir::locate() else {
        return Vec::new();
    };
    let Ok(libraries) = steam_dir.libraries() else {
        return Vec::new();
    };
    libraries
        .filter_map(Result::ok)
        .map(|library| library.path().join("steamapps").join("common"))
        .filter(|dir| dir.is_dir())
        .collect()
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum TitlebarPreference {
    #[default]
    Auto,
    System,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, orthogonal user setting, not a mode"
)]
pub struct Settings {
    /// The schema version that wrote the file.
    ///
    /// A file with no `schema` predates versioning and is by definition the
    /// original shape, so it reads as `1` rather than as whatever this build
    /// writes — otherwise a future bump would mistake every pre-versioning
    /// file for a current one and skip its migration. Serializing always
    /// writes the schema this build produces, whatever was loaded.
    #[serde(
        default = "original_settings_schema",
        serialize_with = "serialize_current_schema"
    )]
    pub schema: u32,

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

    #[serde(deserialize_with = "deserialize_workshop_local_paths")]
    pub my_workshop_local_paths: HashMap<WorkshopId, PathBuf>,
    pub upscale_addon_icon: bool,

    pub language: Option<String>,

    pub extract_overwrite_mode: ExtractionOverwriteMode,

    pub color_neutral: u32,
    pub color_error: u32,
    pub color_success: u32,

    /// App-owned settings that the backend deliberately does not interpret.
    /// Backend-only writes preserve this versioned document verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppDataSnapshot {
    pub settings: Settings,
    /// Process-local ordering for settings publications. Path-only refreshes
    /// keep the same revision; durable settings writes advance it.
    pub settings_revision: u64,
    pub version: &'static str,
    pub paths: AppDataPathsSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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

/// The shape [`Settings`] is written in today.
///
/// Schema 2 makes the app-owned `ui` section part of this authoritative file.
/// Although serde could absorb that additive field, the bump is intentional:
/// a schema-1 binary must reject the combined document rather than accept it,
/// discard `ui`, and later rewrite the file in its older shape.
pub(crate) const SETTINGS_SCHEMA: u32 = 2;

const fn original_settings_schema() -> u32 {
    1
}

fn serialize_current_schema<S: serde::Serializer>(
    _loaded: &u32,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_u32(SETTINGS_SCHEMA)
}

/// Reads the map through raw ids and drops any key of zero, rather than
/// letting [`WorkshopId`]'s own rejection abort the parse.
///
/// A settings file is loaded as a whole: a single unusable key failing the
/// field would fail `Settings`, and the caller's recovery from that is to
/// replace every setting the user has with defaults. Dropping the one entry
/// that names no item costs a path this build could not have used anyway.
fn deserialize_workshop_local_paths<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<HashMap<WorkshopId, PathBuf>, D::Error> {
    let raw = HashMap::<u64, PathBuf>::deserialize(deserializer)?;

    let total = raw.len();
    let kept: HashMap<WorkshopId, PathBuf> = raw
        .into_iter()
        .filter_map(|(id, path)| Some((WorkshopId::new(id)?, path)))
        .collect();

    // The next save rewrites the file without them, so this line is the only
    // trace a dropped key leaves.
    if kept.len() < total {
        log::warn!(
            "settings held {} workshop local path(s) under an id of zero, which names no item; dropping them",
            total - kept.len()
        );
    }

    Ok(kept)
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: SETTINGS_SCHEMA,
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

            ui: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SettingsEnvironment {
    downloads_dir_available: bool,
    gmod_dir_available: bool,
}

impl SettingsEnvironment {
    #[must_use]
    pub const fn new(downloads_dir_available: bool, gmod_dir_available: bool) -> Self {
        Self {
            downloads_dir_available,
            gmod_dir_available,
        }
    }
}

/// Errors that can occur while loading or saving the settings file.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("settings schema {found} is newer than the supported {SETTINGS_SCHEMA}")]
    UnsupportedSchema { found: u32 },
}
impl crate::error_key::HasErrorKey for SettingsError {
    fn error_key(&self) -> crate::error_key::ErrorKey {
        crate::error_key::keys::IO_ERROR
    }

    fn error_detail(&self) -> Option<String> {
        Some(self.to_string())
    }
}

/// Makes a preceding rename durable on filesystems that require directory
/// metadata to be flushed separately from file contents.
#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

impl Settings {
    pub fn load_or_default(paths: &AppDataPaths) -> Self {
        log::info!("initializing settings");
        match Self::load(paths) {
            Ok(settings) => settings,
            Err(error) => {
                if matches!(&error, SettingsError::Io(io) if io.kind() == std::io::ErrorKind::NotFound)
                {
                    log::warn!(
                        "settings file {} was not found; using defaults",
                        paths.settings_file.display()
                    );
                } else if let SettingsError::UnsupportedSchema { found } = error {
                    log::warn!(
                        "settings file {} was written by a newer version (schema {found}); \
                         keeping it aside and starting from defaults",
                        paths.settings_file.display()
                    );
                    Self::back_up_unreadable(paths);
                } else {
                    log::warn!(
                        "failed to load settings from {}: {error}; using defaults",
                        paths.settings_file.display()
                    );
                    // Defaults are about to be written back over this file on
                    // the next save, so keep the unreadable original: a parse
                    // failure must not silently discard the user's settings.
                    Self::back_up_unreadable(paths);
                }
                Self::default()
            }
        }
    }

    /// Moves an unreadable settings file to `<name>.bak` so the defaults that
    /// replace it do not destroy the only copy. Best-effort: a failure here
    /// must not stop the app from starting.
    fn back_up_unreadable(paths: &AppDataPaths) {
        let source = &paths.settings_file;
        if !source.exists() {
            return;
        }

        let backup = source.with_extension("json.bak");
        // Never over an existing backup: the first unusable file is the one
        // closest to what the user configured, and a later failure must not
        // overwrite it with a file that is already a copy of defaults.
        if backup.exists() {
            log::warn!(
                "leaving {} in place; {} is not preserved",
                backup.display(),
                source.display()
            );
            return;
        }
        match fs::rename(source, &backup) {
            Ok(()) => log::warn!("kept the unusable settings at {}", backup.display()),
            Err(error) => log::warn!("could not preserve {}: {error}", source.display()),
        }
    }

    fn load(paths: &AppDataPaths) -> Result<Self, SettingsError> {
        let contents = fs::read_to_string(&paths.settings_file)?;
        let mut settings: Self = serde_json::de::from_str(&contents)?;
        settings.migrate()?;
        Ok(settings)
    }

    /// Brings a parsed file up to [`SETTINGS_SCHEMA`], or refuses it.
    ///
    /// A file from a newer build is an error rather than a silent downgrade:
    /// serde has already dropped the fields this build does not know, and
    /// saving would write that truncated shape back over the user's config.
    fn migrate(&mut self) -> Result<(), SettingsError> {
        if self.schema > SETTINGS_SCHEMA {
            return Err(SettingsError::UnsupportedSchema { found: self.schema });
        }

        match self.schema {
            // Schema 2 adds the optional, opaque UI document. Serde has
            // already supplied `None` for schema 1; stamping 2 provides the
            // downgrade barrier even when there is no UI section yet.
            0 | 1 => {}
            2 => {}
            _ => unreachable!("newer schemas returned above"),
        }
        self.schema = SETTINGS_SCHEMA;
        Ok(())
    }

    pub fn save(&self, paths: &AppDataPaths) -> Result<(), SettingsError> {
        self.save_with_directory_sync(paths, sync_directory)
    }

    fn save_with_directory_sync(
        &self,
        paths: &AppDataPaths,
        sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<(), SettingsError> {
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
        // The rename is atomic, but without this the renamed file can still
        // reference unwritten blocks after a power loss.
        tmp.as_file().sync_all()?;
        tmp.persist(&paths.settings_file)
            .map_err(|error| SettingsError::Io(error.error))?;
        if let Some(parent) = parent {
            // The rename above is the logical commit point: returning an
            // error now would leave disk holding the new value while the live
            // store kept the old one. Directory sync only strengthens crash
            // durability, so report degradation without rolling back the
            // already-committed update.
            if let Err(error) = sync_parent(parent) {
                log::warn!(
                    "settings {} were committed, but directory {} could not be synced: {error}",
                    paths.settings_file.display(),
                    parent.display()
                );
            }
        }

        Ok(())
    }

    pub fn sanitize(&mut self, context: SettingsEnvironment) {
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
    settings: ArcSwap<Settings>,
    /// Even values are stable snapshots; odd values mean a writer is between
    /// publishing settings and completing their revision.
    settings_sequence: AtomicU64,
    /// Serializes persistence and publication. Readers load an immutable
    /// `Arc` and never take this lock.
    settings_writer: Mutex<()>,
    pub version: &'static str,
    /// Populated the first time [`Self::discover_gmod_dir`] finds a path via
    /// Steam, so the cheap [`Self::gmod_dir`] accessor (and therefore
    /// `Serialize`/`snapshot`) can report it without blocking.
    discovered_gmod_dir: Mutex<Option<PathBuf>>,
    paths: AppDataPaths,
    transactions: Transactions,
}
impl AppData {
    #[must_use]
    pub fn load(paths: AppDataPaths, transactions: Transactions) -> Self {
        let mut settings = Settings::load_or_default(&paths);
        let environment = SettingsEnvironment::new(
            settings
                .downloads
                .as_ref()
                .filter(|path| path.is_dir())
                .or(paths.default_downloads_dir.as_ref())
                .is_some(),
            settings.gmod.as_ref().is_some_and(|path| path.is_dir()),
        );
        settings.sanitize(environment);
        Self {
            settings: ArcSwap::from_pointee(settings),
            settings_sequence: AtomicU64::new(0),
            settings_writer: Mutex::new(()),
            version: env!("CARGO_PKG_VERSION"),
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
        loop {
            let sequence = self.settings_sequence.load(Ordering::Acquire);
            if !sequence.is_multiple_of(2) {
                std::hint::spin_loop();
                continue;
            }

            let settings = Settings::clone(&self.settings.load());
            let temp_dir = settings
                .temp
                .as_ref()
                .filter(|path| path.is_dir())
                .cloned()
                .unwrap_or_else(|| self.paths.default_temp_dir.clone());
            let user_data_dir = settings
                .user_data
                .as_ref()
                .filter(|path| path.is_dir())
                .cloned()
                .unwrap_or_else(|| self.paths.default_user_data_dir.clone());
            let downloads_dir = settings
                .downloads
                .as_ref()
                .filter(|path| path.is_dir())
                .cloned()
                .or_else(|| self.paths.default_downloads_dir.clone());
            let gmod_dir = settings
                .gmod
                .as_ref()
                .filter(|path| path.is_dir())
                .cloned()
                .or_else(|| self.discovered_gmod_dir.lock().clone());

            if self.settings_sequence.load(Ordering::Acquire) != sequence {
                continue;
            }

            return AppDataSnapshot {
                settings,
                settings_revision: sequence / 2,
                version: self.version,
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
            };
        }
    }

    fn publish_settings(&self, settings: Settings) {
        let settings = std::sync::Arc::new(settings);
        // AcqRel at entry prevents the settings publication below from
        // becoming visible before readers can observe the odd writer marker.
        self.settings_sequence.fetch_add(1, Ordering::AcqRel);
        self.settings.store(settings);
        self.settings_sequence.fetch_add(1, Ordering::Release);
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
        let configured = self.settings.load().gmod.clone();
        self.discover_gmod_dir_for_settings(configured.as_deref(), steam)
    }

    /// Resolves Garry's Mod against a proposed settings value rather than the
    /// currently published one. Configuration writers use this before they
    /// persist the proposal, so an old configured path cannot leak into the
    /// new snapshot's resolution.
    pub fn discover_gmod_dir_for_settings(
        &self,
        configured: Option<&Path>,
        steam: &Steam,
    ) -> Option<PathBuf> {
        log::info!("Locating Garry's Mod...");
        if let Some(gmod) = configured.filter(|path| path.is_dir()) {
            log::info!("Using user-defined or previously discovered path");
            return Some(gmod.to_path_buf());
        }
        if let Some(gmod) = self.discovered_gmod_dir.lock().clone()
            && gmod.is_dir()
        {
            log::info!("Using previously discovered path");
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
                    .find_app(GMOD_APP_ID)
                    .ok()
                    .flatten()
                    .map(|(app, library)| library.resolve_app_dir(&app))
            }) {
                log::info!("Located!");
                return Some(path);
            }
            log::warn!("Failed to parse Steam library folders. Waiting for Steam...");
            if steam.wait_for_connected(std::time::Duration::from_secs(3)) {
                log::info!("Steam connected!");
            } else {
                log::warn!("Gave up.");
                return None;
            }
        }

        log::info!("Getting Garry's Mod location from Steamworks...");
        let gmod: PathBuf = steam
            .client()
            .ok()?
            .client()
            .apps()
            .app_install_dir(STEAM_GMOD_APP_ID)
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

    /// The live settings. Runtime mutations go through
    /// [`Self::update_settings`], which sanitizes, persists and emits.
    #[must_use]
    pub fn settings(&self) -> arc_swap::Guard<std::sync::Arc<Settings>> {
        self.settings.load()
    }

    /// Edits the settings in place without sanitizing, saving or emitting.
    /// Test-only: production changes must go through [`Self::update_settings`].
    #[cfg(test)]
    pub(crate) fn mutate_settings(&self, mut mutate: impl FnMut(&mut Settings)) {
        self.settings.rcu(|settings| {
            let mut settings = Settings::clone(settings);
            mutate(&mut settings);
            settings
        });
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

    /// Serializes the complete clone -> mutate -> sanitize -> persist ->
    /// publish transaction. The mutation returns environment facts it
    /// resolved explicitly; saving itself never performs Steam discovery.
    /// Persistence happens first, so a failed save leaves live state intact.
    pub fn update_settings(
        &self,
        update: impl FnOnce(&mut Settings) -> SettingsEnvironment,
    ) -> Result<(), SettingsError> {
        let writer = self.settings_writer.lock();
        let mut settings = Settings::clone(&self.settings.load());
        let environment = update(&mut settings);
        settings.sanitize(environment);
        settings.save(&self.paths)?;
        let rediscover_addons = self.settings.load().gmod != settings.gmod;
        self.publish_settings(settings);
        drop(writer);

        if rediscover_addons {
            self.transactions
                .emit(BackendEvent::InstalledAddonsRefreshed);
        }
        self.send();
        Ok(())
    }
}

/// Whether a Steam client installation exists on this machine at all, as
/// distinct from one that exists but isn't running. Reads only the Steam
/// directory layout, so it answers with Steam closed.
#[must_use]
pub fn steam_client_installed() -> bool {
    steamlocate::SteamDir::locate().is_ok()
}

pub fn validate_gmod(mut path: PathBuf) -> bool {
    path.push("GarrysMod");
    path.push("addons");
    path.is_absolute() && path.is_dir()
}

#[cfg(test)]
mod tests;
