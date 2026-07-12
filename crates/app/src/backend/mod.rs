use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub mod archive;
pub mod domain;
pub mod gma;
pub mod library;
pub mod library_watch;
#[cfg(feature = "asset-studio")]
pub mod materials;
pub mod metadata_snapshot;
pub mod native;
pub mod publish;
pub mod size_analyzer;
pub mod tasks;
pub mod ui_error;
pub mod vpk;

use self::domain::PublishedFileId;
pub use self::gma::{ExtractDestination, ExtractionOverwriteMode};
pub use gmpublished_backend::appdata::{
    AppDataSnapshot as BackendAppDataSnapshot, Settings as BackendSettings,
    SettingsPublishedFileId, TitlebarPreference,
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
            play_gifs_by_default: settings.play_gifs_by_default,
            download_count_format: settings.download_count_format,
            theme_preset: settings.theme_preset,
        }
    }

    pub(crate) fn load_from_file_or_default(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
                Ok(value) => Self::from_json_value(&value),
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
    fn from_json_value(value: &serde_json::Value) -> Self {
        serde_json::from_value::<UiSettingsDto>(value.clone())
            .map_or_else(|_| Self::default(), |dto| Self::from_dto(&dto))
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

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent user preference toggle, not exclusive states"
)]
pub struct Settings {
    pub(crate) temp: Option<PathBuf>,
    pub(crate) gmod: Option<PathBuf>,
    pub(crate) user_data: Option<PathBuf>,
    pub(crate) downloads: Option<PathBuf>,
    pub(crate) sounds: bool,
    pub(crate) play_gifs_by_default: bool,
    pub(crate) window_size: (f64, f64),
    pub(crate) window_maximized: bool,
    pub(crate) titlebar: TitlebarPreference,
    pub(crate) extract_destination: ExtractDestination,
    pub(crate) destinations: Vec<PathBuf>,
    pub(crate) create_folder_on_extract: bool,
    pub(crate) ignore_globs: Vec<String>,
    pub(crate) my_workshop_local_paths: HashMap<PublishedFileId, PathBuf>,
    pub(crate) upscale_addon_icon: bool,
    pub(crate) language: Option<String>,
    pub(crate) download_count_format: DownloadCountFormat,
    pub(crate) theme_preset: ThemePreset,
    pub(crate) extract_overwrite_mode: ExtractionOverwriteMode,
    pub(crate) color_neutral: u32,
    pub(crate) color_error: u32,
    pub(crate) color_success: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_backend(BackendSettings::default(), &UiSettings::default())
    }
}

impl Settings {
    pub(crate) fn from_backend(backend: BackendSettings, ui: &UiSettings) -> Self {
        Self {
            temp: backend.temp,
            gmod: backend.gmod,
            user_data: backend.user_data,
            downloads: backend.downloads,
            sounds: backend.sounds,
            play_gifs_by_default: ui.play_gifs_by_default,
            window_size: backend.window_size,
            window_maximized: backend.window_maximized,
            titlebar: backend.titlebar,
            extract_destination: backend.extract_destination,
            destinations: backend.destinations,
            create_folder_on_extract: backend.create_folder_on_extract,
            ignore_globs: backend.ignore_globs,
            my_workshop_local_paths: backend
                .my_workshop_local_paths
                .into_iter()
                .filter_map(|(id, path)| Some((PublishedFileId::new(id.0)?, path)))
                .collect(),
            upscale_addon_icon: backend.upscale_addon_icon,
            language: backend.language,
            download_count_format: ui.download_count_format,
            theme_preset: ui.theme_preset,
            extract_overwrite_mode: backend.extract_overwrite_mode,
            color_neutral: backend.color_neutral,
            color_error: backend.color_error,
            color_success: backend.color_success,
        }
    }

    pub(crate) fn to_backend(&self) -> BackendSettings {
        BackendSettings {
            temp: self.temp.clone(),
            gmod: self.gmod.clone(),
            user_data: self.user_data.clone(),
            downloads: self.downloads.clone(),
            sounds: self.sounds,
            window_size: self.window_size,
            window_maximized: self.window_maximized,
            titlebar: self.titlebar,
            extract_destination: self.extract_destination.clone(),
            destinations: self.destinations.clone(),
            create_folder_on_extract: self.create_folder_on_extract,
            ignore_globs: self.ignore_globs.clone(),
            my_workshop_local_paths: self
                .my_workshop_local_paths
                .iter()
                .map(|(id, path)| (SettingsPublishedFileId(id.get()), path.clone()))
                .collect(),
            upscale_addon_icon: self.upscale_addon_icon,
            language: self.language.clone(),
            extract_overwrite_mode: self.extract_overwrite_mode.clone(),
            color_neutral: self.color_neutral,
            color_error: self.color_error,
            color_success: self.color_success,
        }
    }

    pub(crate) fn apply_ui_settings(&mut self, ui: &UiSettings) {
        self.play_gifs_by_default = ui.play_gifs_by_default;
        self.download_count_format = ui.download_count_format;
        self.theme_preset = ui.theme_preset;
    }

    pub(crate) fn sanitize(&mut self, paths: &AppPaths) {
        self.destinations
            .retain(|dir| dir.is_absolute() && dir.is_dir());
        self.my_workshop_local_paths
            .retain(|_, dir| dir.is_absolute() && dir.is_dir());
        self.extract_destination = self.sanitized_extract_destination(paths);
        self.destinations.truncate(MAX_DESTINATIONS);
    }

    /// The `extract_destination` `sanitize` would settle on, without
    /// touching `destinations`/`my_workshop_local_paths` or requiring a
    /// mutable (or cloned) `Settings` — for read-only callers such as a
    /// status label that only care about the resolved destination.
    pub(crate) fn sanitized_extract_destination(&self, paths: &AppPaths) -> ExtractDestination {
        match &self.extract_destination {
            ExtractDestination::Directory(path) => {
                if self.create_folder_on_extract || !path.is_dir() {
                    ExtractDestination::NamedDirectory(path.to_owned())
                } else {
                    self.extract_destination.clone()
                }
            }
            ExtractDestination::NamedDirectory(path) => {
                if !self.create_folder_on_extract || !path.is_dir() {
                    ExtractDestination::Directory(path.to_owned())
                } else {
                    self.extract_destination.clone()
                }
            }
            ExtractDestination::Downloads if paths.downloads_dir.is_none() => {
                ExtractDestination::default()
            }
            ExtractDestination::Addons if paths.gmod_dir.is_none() => ExtractDestination::default(),
            ExtractDestination::Downloads
            | ExtractDestination::Addons
            | ExtractDestination::Temp => self.extract_destination.clone(),
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
        defaults.temp_dir =
            valid_dir(settings.temp.as_ref()).unwrap_or_else(|| defaults.default_temp_dir.clone());
        defaults.user_data_dir = valid_dir(settings.user_data.as_ref())
            .unwrap_or_else(|| defaults.default_user_data_dir.clone());
        defaults.downloads_dir = valid_dir(settings.downloads.as_ref())
            .or_else(|| defaults.default_downloads_dir.clone());
        let default_gmod_dir = defaults.gmod_dir.take();
        defaults.gmod_dir = valid_dir(settings.gmod.as_ref())
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
            UiSettings {
                play_gifs_by_default: true,
                download_count_format: DownloadCountFormat::Space,
                theme_preset: ThemePreset::Dark,
            }
        );

        assert_eq!(
            UiSettings::from_json_value(&serde_json::json!({
                "play_gifs_by_default": "yes",
                "download_count_format": "grouped",
                "theme_preset": "system",
            })),
            UiSettings::default()
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
                open_count: 0,
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

        assert!(settings.gmod.is_none());
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
                open_count: 0,
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

        assert!(settings.downloads.is_none());
        assert_eq!(paths.default_downloads_dir, Some(downloads_dir.clone()));
        assert_eq!(paths.downloads_dir, Some(downloads_dir.clone()));

        let mut settings_with_invalid_override = settings;
        settings_with_invalid_override.downloads = Some(temp.path().join("missing-downloads"));
        let resolved = AppPaths::resolve_with_defaults(&settings_with_invalid_override, paths);
        assert_eq!(resolved.downloads_dir, Some(downloads_dir));
    }
}

pub mod theme {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Rgb {
        pub(crate) r: u8,
        pub(crate) g: u8,
        pub(crate) b: u8,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DerivedColor {
        pub(crate) base: Rgb,
        pub(crate) dark: Rgb,
    }

    pub fn derive(rgb: u32) -> DerivedColor {
        let base = Rgb {
            r: ((rgb & 0xFF0000) >> 16) as u8,
            g: ((rgb & 0x00FF00) >> 8) as u8,
            b: (rgb & 0x0000FF) as u8,
        };
        let (h, s, l) = rgb_to_hsl(base);
        let dark = hsl_to_rgb(h, s, l * 0.85);
        DerivedColor { base, dark }
    }

    fn rgb_to_hsl(rgb: Rgb) -> (f64, f64, f64) {
        let r = f64::from(rgb.r) / 255.0;
        let g = f64::from(rgb.g) / 255.0;
        let b = f64::from(rgb.b) / 255.0;

        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;

        if (max - min).abs() < f64::EPSILON {
            return (0.0, 0.0, l);
        }

        let d = max - min;
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };

        let mut h = if (max - r).abs() < f64::EPSILON {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if (max - g).abs() < f64::EPSILON {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        h /= 6.0;

        (h, s, l)
    }

    fn hsl_to_rgb(h: f64, s: f64, l: f64) -> Rgb {
        if s.abs() < f64::EPSILON {
            let channel = float_channel(l);
            return Rgb {
                r: channel,
                g: channel,
                b: channel,
            };
        }

        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;

        Rgb {
            r: float_channel(hue_to_rgb(p, q, h + (1.0 / 3.0))),
            g: float_channel(hue_to_rgb(p, q, h)),
            b: float_channel(hue_to_rgb(p, q, h - (1.0 / 3.0))),
        }
    }

    fn hue_to_rgb(p: f64, q: f64, mut t: f64) -> f64 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            return p + (q - p) * 6.0 * t;
        }
        if t < 1.0 / 2.0 {
            return q;
        }
        if t < 2.0 / 3.0 {
            return p + (q - p) * ((2.0 / 3.0) - t) * 6.0;
        }
        p
    }

    fn float_channel(value: f64) -> u8 {
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
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
