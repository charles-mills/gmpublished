use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub mod archive;
pub mod content_path;
pub mod domain;
pub mod gma;
pub mod library;
pub mod library_watch;
pub mod materials;
pub mod metadata_snapshot;
pub mod native;
pub mod publish;
pub mod snapshot;
pub mod tasks;
pub mod ui_error;
pub mod vpk;

pub use self::gma::{ExtractDestination, ExtractionOverwriteMode};
pub use gmpublished_backend::appdata::{
    AppDataSnapshot as BackendAppDataSnapshot, Settings as BackendSettings, TitlebarPreference,
};

const MAX_DESTINATIONS: usize = 20;
const UI_SETTINGS_FILE_NAME: &str = "ui-settings.json";
const UI_SETTINGS_SCHEMA_VERSION: u64 = 1;

/// Iced-only preferences that are not part of the shared backend appdata settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiSettings {
    pub(crate) play_gifs_by_default: bool,
    pub(crate) download_count_format: DownloadCountFormat,
    pub(crate) theme_preset: ThemePreset,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            play_gifs_by_default: true,
            download_count_format: DownloadCountFormat::default(),
            theme_preset: ThemePreset::default(),
        }
    }
}

impl UiSettings {
    pub(crate) fn from_settings(settings: &Settings) -> Self {
        Self {
            play_gifs_by_default: settings.ui.play_gifs_by_default,
            download_count_format: settings.ui.download_count_format,
            theme_preset: settings.ui.theme_preset,
        }
    }

    pub(crate) fn load_from_file_or_default(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
                Ok(value) => Self::from_json_value(&value).unwrap_or_else(|| {
                    log::warn!(
                        "UI settings at {} are from a newer version; starting from defaults \
                         rather than rewriting them in this build's shape",
                        path.display()
                    );
                    Self::default()
                }),
                Err(error) => {
                    log::warn!(
                        "failed to parse UI settings from {}: {error}",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                log::warn!(
                    "failed to load UI settings from {}: {error}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    pub(crate) fn save_to_file(&self, path: &Path) -> Result<(), SettingsPersistError> {
        let bytes = serde_json::to_vec_pretty(&self.to_json_value()).map_err(|source| {
            SettingsPersistError::Serialize {
                path: path.to_path_buf(),
                source,
            }
        })?;
        crate::util::fs::atomic_write(path, &bytes).map_err(|source| SettingsPersistError::Write {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Converts a parsed JSON value into settings, falling back to the
    /// default for the whole struct if `value` is not an object, and
    /// independently, per field, if a field is missing or holds a value of
    /// the wrong shape.
    /// `None` for a file this build must not adopt.
    ///
    /// A newer `version` means fields this build does not know: taking it
    /// would drop them and the next save would write the truncated shape back
    /// over the user's file. Older versions load as-is — every field is
    /// independently optional, so a missing one takes its default.
    fn from_json_value(value: &serde_json::Value) -> Option<Self> {
        let dto = serde_json::from_value::<UiSettingsDto>(value.clone()).ok()?;
        (dto.version <= UI_SETTINGS_SCHEMA_VERSION).then(|| Self::from_dto(&dto))
    }

    fn from_dto(dto: &UiSettingsDto) -> Self {
        let defaults = Self::default();
        Self {
            play_gifs_by_default: dto
                .play_gifs_by_default
                .unwrap_or(defaults.play_gifs_by_default),
            download_count_format: dto
                .download_count_format
                .as_deref()
                .and_then(DownloadCountFormat::from_value)
                .unwrap_or(defaults.download_count_format),
            theme_preset: dto
                .theme_preset
                .as_deref()
                .and_then(ThemePreset::from_value)
                .unwrap_or(defaults.theme_preset),
        }
    }

    fn to_json_value(&self) -> serde_json::Value {
        let dto = UiSettingsDto {
            version: UI_SETTINGS_SCHEMA_VERSION,
            play_gifs_by_default: Some(self.play_gifs_by_default),
            download_count_format: Some(self.download_count_format.as_value().to_owned()),
            theme_preset: Some(self.theme_preset.as_value().to_owned()),
        };
        serde_json::to_value(dto).unwrap_or_default()
    }
}

/// On-disk shape of [`UiSettings`]. Every field is independently optional so
/// a missing key or a value of the wrong type falls back to that field's
/// default rather than rejecting the whole file (`lenient_field` swallows
/// per-field type mismatches; `#[serde(default)]` covers absent keys).
#[derive(Serialize, Deserialize)]
struct UiSettingsDto {
    #[serde(default)]
    version: u64,
    #[serde(default, deserialize_with = "lenient_field")]
    play_gifs_by_default: Option<bool>,
    #[serde(default, deserialize_with = "lenient_field")]
    download_count_format: Option<String>,
    #[serde(default, deserialize_with = "lenient_field")]
    theme_preset: Option<String>,
}

/// Deserializes a field as `Option<T>`, treating a present-but-wrong-shape
/// value the same as an absent one (`None`) instead of failing the parse.
fn lenient_field<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// The two settings files the app reads as one value.
///
/// Composed rather than flattened. Mirroring [`BackendSettings`] field by
/// field would mean a new backend setting needs a field here and a line in
/// each direction of the conversion — and a forgotten line still compiles,
/// silently dropping the value on the next save. Holding the backend's own
/// struct leaves nothing to keep in step.
///
/// Which half a setting lives in is which file it persists to, so
/// `settings.backend` / `settings.ui` at a call site is information, not noise.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub(crate) backend: BackendSettings,
    pub(crate) ui: UiSettings,
}

impl Settings {
    pub(crate) fn from_backend(backend: BackendSettings, ui: &UiSettings) -> Self {
        Self {
            backend,
            ui: ui.clone(),
        }
    }

    pub(crate) fn to_backend(&self) -> BackendSettings {
        self.backend.clone()
    }

    pub(crate) fn apply_ui_settings(&mut self, ui: &UiSettings) {
        self.ui = ui.clone();
    }

    pub(crate) fn sanitize(&mut self, paths: &AppPaths) {
        self.backend
            .destinations
            .retain(|dir| dir.is_absolute() && dir.is_dir());
        self.backend
            .my_workshop_local_paths
            .retain(|_, dir| dir.is_absolute() && dir.is_dir());
        self.backend.extract_destination = self.sanitized_extract_destination(paths);
        self.backend.destinations.truncate(MAX_DESTINATIONS);
    }

    /// The `extract_destination` `sanitize` would settle on, without
    /// touching `destinations`/`my_workshop_local_paths` or requiring a
    /// mutable (or cloned) `Settings` — for read-only callers such as a
    /// status label that only care about the resolved destination.
    pub(crate) fn sanitized_extract_destination(&self, paths: &AppPaths) -> ExtractDestination {
        match &self.backend.extract_destination {
            ExtractDestination::Directory(path) => {
                if self.backend.create_folder_on_extract || !path.is_dir() {
                    ExtractDestination::NamedDirectory(path.to_owned())
                } else {
                    self.backend.extract_destination.clone()
                }
            }
            ExtractDestination::NamedDirectory(path) => {
                if !self.backend.create_folder_on_extract || !path.is_dir() {
                    ExtractDestination::Directory(path.to_owned())
                } else {
                    self.backend.extract_destination.clone()
                }
            }
            ExtractDestination::Downloads if paths.downloads_dir.is_none() => {
                ExtractDestination::default()
            }
            ExtractDestination::Addons if paths.gmod_dir.is_none() => ExtractDestination::default(),
            ExtractDestination::Downloads
            | ExtractDestination::Addons
            | ExtractDestination::Temp => self.backend.extract_destination.clone(),
        }
    }
}

pub fn ui_settings_file_for(settings_file: &Path) -> PathBuf {
    settings_file.parent().map_or_else(
        || PathBuf::from(UI_SETTINGS_FILE_NAME),
        |parent| parent.join(UI_SETTINGS_FILE_NAME),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub(crate) settings_file: PathBuf,
    pub(crate) default_user_data_dir: PathBuf,
    pub(crate) default_temp_dir: PathBuf,
    pub(crate) default_downloads_dir: Option<PathBuf>,
    pub(crate) temp_dir: PathBuf,
    pub(crate) user_data_dir: PathBuf,
    pub(crate) downloads_dir: Option<PathBuf>,
    pub(crate) gmod_dir: Option<PathBuf>,
}

impl AppPaths {
    pub(crate) fn from_backend(snapshot: &BackendAppDataSnapshot) -> Self {
        let paths = &snapshot.paths;
        Self {
            settings_file: paths.settings_file.clone(),
            default_user_data_dir: paths.default_user_data_dir.clone(),
            default_temp_dir: paths.default_temp_dir.clone(),
            default_downloads_dir: paths.default_downloads_dir.clone(),
            temp_dir: paths.temp_dir.clone(),
            user_data_dir: paths.user_data_dir.clone(),
            downloads_dir: paths.downloads_dir.clone(),
            gmod_dir: paths.gmod_dir.clone(),
        }
    }

    pub(crate) fn resolve_with_defaults(settings: &Settings, mut defaults: Self) -> Self {
        defaults.temp_dir = valid_dir(settings.backend.temp.as_ref())
            .unwrap_or_else(|| defaults.default_temp_dir.clone());
        defaults.user_data_dir = valid_dir(settings.backend.user_data.as_ref())
            .unwrap_or_else(|| defaults.default_user_data_dir.clone());
        defaults.downloads_dir = valid_dir(settings.backend.downloads.as_ref())
            .or_else(|| defaults.default_downloads_dir.clone());
        let default_gmod_dir = defaults.gmod_dir.take();
        defaults.gmod_dir = valid_dir(settings.backend.gmod.as_ref())
            .or_else(|| default_gmod_dir.and_then(|path| path.is_dir().then_some(path)));
        defaults
    }
}

pub fn appdata_snapshot_from_backend(
    snapshot: BackendAppDataSnapshot,
    ui: &UiSettings,
) -> (Settings, AppPaths) {
    let paths = AppPaths::from_backend(&snapshot);
    let mut settings = Settings::from_backend(snapshot.settings, ui);
    settings.sanitize(&paths);
    (settings, paths)
}

fn valid_dir(path: Option<&PathBuf>) -> Option<PathBuf> {
    path.filter(|path| path.is_dir()).cloned()
}

pub fn validate_gmod(path: impl AsRef<Path>) -> bool {
    gmpublished_backend::appdata::validate_gmod(path.as_ref().to_path_buf())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ThemePreset {
    Dark,
    Light,
    ClassicSource,
    #[default]
    Auto,
}

impl ThemePreset {
    pub(crate) const fn accent_colors(self) -> (u32, u32, u32) {
        match self {
            Self::Dark => (0x0000_6DC7, 0x0030_A661, 0x00A8_0000),
            Self::Light => (0x0000_6DC7, 0x0025_8F52, 0x00B3_261E),
            Self::ClassicSource => (0x00E0_8A2E, 0x0087_9A57, 0x00B8_5E42),
            Self::Auto => Self::Dark.accent_colors(),
        }
    }

    pub(crate) const fn as_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
            Self::ClassicSource => "classic_source",
        }
    }

    pub(crate) fn from_value(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            "classic_source" => Some(Self::ClassicSource),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemColorScheme {
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectiveThemePreset {
    Dark,
    Light,
    ClassicSource,
}

pub const fn effective_theme_preset(
    stored: ThemePreset,
    system: SystemColorScheme,
) -> EffectiveThemePreset {
    match stored {
        ThemePreset::Auto => match system {
            SystemColorScheme::Dark => EffectiveThemePreset::Dark,
            SystemColorScheme::Light => EffectiveThemePreset::Light,
        },
        ThemePreset::Dark => EffectiveThemePreset::Dark,
        ThemePreset::Light => EffectiveThemePreset::Light,
        ThemePreset::ClassicSource => EffectiveThemePreset::ClassicSource,
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DownloadCountFormat {
    #[default]
    Automatic,
    Comma,
    Period,
    Space,
    Plain,
}

impl DownloadCountFormat {
    pub(crate) const fn as_value(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Comma => "comma",
            Self::Period => "period",
            Self::Space => "space",
            Self::Plain => "plain",
        }
    }

    pub(crate) fn from_value(value: &str) -> Option<Self> {
        match value {
            "automatic" => Some(Self::Automatic),
            "comma" => Some(Self::Comma),
            "period" => Some(Self::Period),
            "space" => Some(Self::Space),
            "plain" => Some(Self::Plain),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Crossing the bridge must lose nothing. Holding the backend's own struct
    /// is what guarantees it: there is no per-field list to fall out of step
    /// with, so a setting added to `BackendSettings` needs no change here and
    /// cannot be dropped on the next save.
    #[test]
    fn backend_settings_survive_a_round_trip_through_the_bridge() {
        let mut backend = BackendSettings {
            gmod: Some(PathBuf::from("/games/gmod")),
            ignore_globs: vec!["materials/private/*".to_owned()],
            destinations: vec![PathBuf::from("/extract/here")],
            color_neutral: 0x00AA_BBCC,
            ..BackendSettings::default()
        };
        backend.my_workshop_local_paths.insert(
            gmpublished_backend::WorkshopId::new(4321).expect("fixture ids are nonzero"),
            PathBuf::from("/addons/mine"),
        );

        let settings = Settings::from_backend(backend.clone(), &UiSettings::default());

        assert_eq!(settings.to_backend(), backend);
    }

    /// The UI half is replaced wholesale for the same reason.
    #[test]
    fn applying_ui_settings_replaces_every_ui_field_and_no_backend_field() {
        let backend = BackendSettings::default();
        let mut settings = Settings::from_backend(backend.clone(), &UiSettings::default());
        let ui = UiSettings {
            play_gifs_by_default: false,
            download_count_format: DownloadCountFormat::default(),
            theme_preset: ThemePreset::Light,
        };

        settings.apply_ui_settings(&ui);

        assert_eq!(settings.ui, ui);
        assert_eq!(settings.to_backend(), backend);
    }

    #[test]
    fn ui_settings_file_path_stays_next_to_upstream_settings() {
        assert_eq!(
            ui_settings_file_for(Path::new("/tmp/gmpublisher/settings.json")),
            PathBuf::from("/tmp/gmpublisher/ui-settings.json")
        );
        assert_eq!(
            ui_settings_file_for(Path::new("settings.json")),
            PathBuf::from("ui-settings.json")
        );
    }

    #[test]
    fn ui_settings_defaults_missing_and_malformed_values_by_field() {
        assert_eq!(
            UiSettings::from_json_value(&serde_json::json!({
                "version": 1,
                "download_count_format": "space",
                "theme_preset": "dark",
            })),
            Some(UiSettings {
                play_gifs_by_default: true,
                download_count_format: DownloadCountFormat::Space,
                theme_preset: ThemePreset::Dark,
            })
        );

        assert_eq!(
            UiSettings::from_json_value(&serde_json::json!({
                "play_gifs_by_default": "yes",
                "download_count_format": "grouped",
                "theme_preset": "system",
            })),
            Some(UiSettings::default())
        );

        // A file from a newer build carries fields this one would drop, so
        // adopting it would rewrite the user's settings in a smaller shape.
        assert_eq!(
            UiSettings::from_json_value(&serde_json::json!({
                "version": UI_SETTINGS_SCHEMA_VERSION + 1,
                "theme_preset": "dark",
            })),
            None
        );
    }

    #[test]
    fn ui_settings_round_trip_to_separate_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config/gmpublisher/ui-settings.json");

        assert_eq!(
            UiSettings::load_from_file_or_default(&path),
            UiSettings::default()
        );

        let settings = UiSettings {
            play_gifs_by_default: false,
            download_count_format: DownloadCountFormat::Period,
            theme_preset: ThemePreset::ClassicSource,
        };
        settings.save_to_file(&path).expect("save UI settings");

        assert_eq!(UiSettings::load_from_file_or_default(&path), settings);
        let value: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(&path).expect("persisted UI settings should be readable"),
        )
        .expect("persisted UI settings should be JSON");
        assert_eq!(value["version"], UI_SETTINGS_SCHEMA_VERSION);
        assert_eq!(value["play_gifs_by_default"], false);
        assert_eq!(value["download_count_format"], "period");
        assert_eq!(value["theme_preset"], "classic_source");
    }

    #[test]
    fn backend_appdata_gmod_fallback_path_projects_into_app_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let gmod_dir = temp.path().join("GarrysMod");
        let temp_dir = temp.path().join("temp");
        let user_data_dir = temp.path().join("user-data");
        fs::create_dir_all(&gmod_dir).expect("gmod dir");
        fs::create_dir_all(&temp_dir).expect("temp dir");
        fs::create_dir_all(&user_data_dir).expect("user data dir");

        let (settings, paths) = appdata_snapshot_from_backend(
            BackendAppDataSnapshot {
                settings: BackendSettings::default(),
                version: "test",
                paths: gmpublished_backend::appdata::AppDataPathsSnapshot {
                    settings_file: temp.path().join("settings.json"),
                    default_user_data_dir: user_data_dir.clone(),
                    default_temp_dir: temp_dir.clone(),
                    default_downloads_dir: None,
                    temp_dir,
                    user_data_dir,
                    downloads_dir: None,
                    gmod_dir: Some(gmod_dir.clone()),
                },
            },
            &UiSettings::default(),
        );

        assert!(settings.backend.gmod.is_none());
        assert_eq!(paths.gmod_dir, Some(gmod_dir));
    }

    #[test]
    fn backend_appdata_downloads_fallback_path_projects_into_app_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let downloads_dir = temp.path().join("Downloads");
        let temp_dir = temp.path().join("temp");
        let user_data_dir = temp.path().join("user-data");
        fs::create_dir_all(&downloads_dir).expect("downloads dir");
        fs::create_dir_all(&temp_dir).expect("temp dir");
        fs::create_dir_all(&user_data_dir).expect("user data dir");

        let (settings, paths) = appdata_snapshot_from_backend(
            BackendAppDataSnapshot {
                settings: BackendSettings::default(),
                version: "test",
                paths: gmpublished_backend::appdata::AppDataPathsSnapshot {
                    settings_file: temp.path().join("settings.json"),
                    default_user_data_dir: user_data_dir.clone(),
                    default_temp_dir: temp_dir.clone(),
                    default_downloads_dir: Some(downloads_dir.clone()),
                    temp_dir,
                    user_data_dir,
                    downloads_dir: Some(downloads_dir.clone()),
                    gmod_dir: None,
                },
            },
            &UiSettings::default(),
        );

        assert!(settings.backend.downloads.is_none());
        assert_eq!(paths.default_downloads_dir, Some(downloads_dir.clone()));
        assert_eq!(paths.downloads_dir, Some(downloads_dir.clone()));

        let mut settings_with_invalid_override = settings;
        settings_with_invalid_override.backend.downloads =
            Some(temp.path().join("missing-downloads"));
        let resolved = AppPaths::resolve_with_defaults(&settings_with_invalid_override, paths);
        assert_eq!(resolved.downloads_dir, Some(downloads_dir));
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsPersistError {
    #[error("failed to serialize UI settings for {}: {source}", path.display())]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to write UI settings to {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    AppData(#[from] gmpublished_backend::appdata::SettingsError),
}

impl gmpublished_backend::error_key::HasErrorKey for SettingsPersistError {
    fn error_key(&self) -> gmpublished_backend::error_key::ErrorKey {
        gmpublished_backend::error_key::keys::IO_ERROR
    }

    fn error_detail(&self) -> Option<String> {
        Some(self.to_string())
    }
}
