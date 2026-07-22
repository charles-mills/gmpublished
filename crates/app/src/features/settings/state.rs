use std::path::{Path, PathBuf};

use crate::bridge::ExtractionOverwriteMode;
use crate::bridge::ui_error::UiError;
use crate::bridge::{
    AppPaths, DownloadCountFormat, EffectiveThemePreset, Settings, SystemColorScheme, ThemePreset,
    TitlebarPreference, effective_theme_preset, validate_gmod,
};

use crate::theme::AccentInputs;
use crate::util::paths::{fallback_current_dir, fallback_paths, path_to_display};

const DEFAULT_LANGUAGE_VALUE: &str = "default";

#[derive(Clone, Debug, PartialEq)]
pub struct SettingsSnapshot {
    pub(crate) settings: Settings,
    pub(crate) paths: AppPaths,
    pub(crate) system_scheme: SystemColorScheme,
}

impl SettingsSnapshot {
    pub(crate) fn new(
        settings: Settings,
        paths: AppPaths,
        system_scheme: SystemColorScheme,
    ) -> Self {
        Self {
            settings,
            paths,
            system_scheme,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Tab {
    #[default]
    General,
    Paths,
    Accessibility,
    Resets,
}

impl Tab {
    pub(crate) const ALL: [Self; 4] = [
        Self::General,
        Self::Paths,
        Self::Accessibility,
        Self::Resets,
    ];

    pub(crate) const fn label_key(self) -> &'static str {
        match self {
            Self::General => "settings-tab-general",
            Self::Paths => "settings-tab-paths",
            Self::Accessibility => "settings-tab-accessibility",
            Self::Resets => "settings-tab-resets",
        }
    }
}

pub use crate::widgets::select_option::SelectOption;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathSetting {
    Gmod,
    Downloads,
    UserData,
    Temp,
}

impl PathSetting {
    pub(crate) const ALL: [Self; 4] = [Self::Gmod, Self::Downloads, Self::UserData, Self::Temp];

    pub(crate) const fn label_key(self) -> &'static str {
        match self {
            Self::Gmod => "settings-paths-gmod",
            Self::Downloads => "settings-paths-downloads",
            Self::UserData => "settings-paths-user-data",
            Self::Temp => "settings-paths-temp",
        }
    }

    /// Slot in the `PathSetting::ALL`-ordered per-setting storage arrays.
    const fn index(self) -> usize {
        match self {
            Self::Gmod => 0,
            Self::Downloads => 1,
            Self::UserData => 2,
            Self::Temp => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorSetting {
    Neutral,
    Success,
    Error,
}

impl ColorSetting {
    pub(crate) const ALL: [Self; 3] = [Self::Neutral, Self::Success, Self::Error];

    pub(crate) const fn label_key(self) -> &'static str {
        match self {
            Self::Neutral => "settings-accessibility-color-neutral",
            Self::Success => "settings-accessibility-color-success",
            Self::Error => "settings-accessibility-color-error",
        }
    }

    /// Slot in the `ColorSetting::ALL`-ordered per-setting storage arrays,
    /// also used to position the color-picker popover in `view.rs`.
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Neutral => 0,
            Self::Success => 1,
            Self::Error => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorChannel {
    Hue,
    Saturation,
    Value,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HsvColor {
    pub(crate) hue: f32,
    pub(crate) saturation: f32,
    pub(crate) value: f32,
}

impl HsvColor {
    fn from_rgb(rgb: u32) -> Self {
        let [_, red, green, blue] = (rgb & 0xFF_FFFF).to_be_bytes();
        let red = f32::from(red) / 255.0;
        let green = f32::from(green) / 255.0;
        let blue = f32::from(blue) / 255.0;
        let max = red.max(green).max(blue);
        let min = red.min(green).min(blue);
        let delta = max - min;

        let hue = if delta <= f32::EPSILON {
            0.0
        } else if (max - red).abs() <= f32::EPSILON {
            60.0 * ((green - blue) / delta).rem_euclid(6.0)
        } else if (max - green).abs() <= f32::EPSILON {
            60.0 * (((blue - red) / delta) + 2.0)
        } else {
            60.0 * (((red - green) / delta) + 4.0)
        };

        let saturation = if max <= f32::EPSILON {
            0.0
        } else {
            delta / max
        };

        Self {
            hue: hue.clamp(0.0, 360.0),
            saturation: saturation.clamp(0.0, 1.0),
            value: max.clamp(0.0, 1.0),
        }
    }

    pub(crate) fn to_rgb(self) -> u32 {
        let hue = self.hue.rem_euclid(360.0);
        let saturation = self.saturation.clamp(0.0, 1.0);
        let value = self.value.clamp(0.0, 1.0);
        let chroma = value * saturation;
        let x = chroma * (1.0 - (((hue / 60.0) % 2.0) - 1.0).abs());
        let m = value - chroma;
        let (red, green, blue) = match hue {
            h if h < 60.0 => (chroma, x, 0.0),
            h if h < 120.0 => (x, chroma, 0.0),
            h if h < 180.0 => (0.0, chroma, x),
            h if h < 240.0 => (0.0, x, chroma),
            h if h < 300.0 => (x, 0.0, chroma),
            _ => (chroma, 0.0, x),
        };

        let red = ((red + m) * 255.0).round().clamp(0.0, 255.0) as u32;
        let green = ((green + m) * 255.0).round().clamp(0.0, 255.0) as u32;
        let blue = ((blue + m) * 255.0).round().clamp(0.0, 255.0) as u32;
        (red << 16) | (green << 8) | blue
    }

    fn set_channel(&mut self, channel: ColorChannel, value: f32) {
        match channel {
            ColorChannel::Hue => self.hue = value.clamp(0.0, 360.0),
            ColorChannel::Saturation => self.saturation = value.clamp(0.0, 1.0),
            ColorChannel::Value => self.value = value.clamp(0.0, 1.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResetAction {
    Settings,
    TempFiles,
    UserData,
}

impl ResetAction {
    pub(crate) const fn label_key(self) -> &'static str {
        match self {
            Self::Settings => "settings-resets-settings",
            Self::TempFiles => "settings-resets-clear-temp",
            Self::UserData => "settings-resets-clear-user-data",
        }
    }

    pub(crate) const fn success_key(self) -> &'static str {
        match self {
            Self::Settings => "settings-resets-reset-settings-done",
            Self::TempFiles => "settings-resets-clear-temp-done",
            Self::UserData => "settings-resets-clear-user-data-done",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsMutation {
    Sounds(bool),
    PlayGifsByDefault(bool),
    // Only ever constructed from the macOS-only system-titlebar toggle
    // (see `Message::SystemTitlebarToggled`), but `Settings::titlebar`
    // itself is a cross-platform persisted field, so the variant stays.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Titlebar(TitlebarPreference),
    Language(Option<String>),
    DownloadCountFormat(DownloadCountFormat),
    Theme {
        preset: ThemePreset,
        neutral: u32,
        success: u32,
        error: u32,
    },
    OverwriteMode(ExtractionOverwriteMode),
    Path {
        kind: PathSetting,
        path: Option<PathBuf>,
    },
    Color {
        kind: ColorSetting,
        rgb: u32,
    },
}

pub fn apply_settings_mutation(settings: &mut Settings, mutation: SettingsMutation) {
    match mutation {
        SettingsMutation::Sounds(enabled) => settings.sounds = enabled,
        SettingsMutation::PlayGifsByDefault(enabled) => {
            settings.play_gifs_by_default = enabled;
        }
        SettingsMutation::Titlebar(preference) => settings.titlebar = preference,
        SettingsMutation::Language(language) => settings.language = language,
        SettingsMutation::DownloadCountFormat(format) => settings.download_count_format = format,
        SettingsMutation::Theme {
            preset,
            neutral,
            success,
            error,
        } => {
            settings.theme_preset = preset;
            settings.color_neutral = neutral;
            settings.color_success = success;
            settings.color_error = error;
        }
        SettingsMutation::OverwriteMode(mode) => settings.extract_overwrite_mode = mode,
        SettingsMutation::Path { kind, path } => set_path_setting(settings, kind, path),
        SettingsMutation::Color { kind, rgb } => set_color_setting(settings, kind, rgb),
    }
}

/// Whether applying `mutation` would actually change `settings`, checked
/// against just the field(s) the mutation touches instead of cloning the
/// whole `Settings` to diff it afterward.
fn mutation_changes(settings: &Settings, mutation: &SettingsMutation) -> bool {
    match mutation {
        SettingsMutation::Sounds(enabled) => settings.sounds != *enabled,
        SettingsMutation::PlayGifsByDefault(enabled) => settings.play_gifs_by_default != *enabled,
        SettingsMutation::Titlebar(preference) => settings.titlebar != *preference,
        SettingsMutation::Language(language) => &settings.language != language,
        SettingsMutation::DownloadCountFormat(format) => settings.download_count_format != *format,
        SettingsMutation::Theme {
            preset,
            neutral,
            success,
            error,
        } => {
            settings.theme_preset != *preset
                || settings.color_neutral != *neutral
                || settings.color_success != *success
                || settings.color_error != *error
        }
        SettingsMutation::OverwriteMode(mode) => settings.extract_overwrite_mode != *mode,
        SettingsMutation::Path { kind, path } => {
            path_setting_value(settings, *kind) != path.as_deref()
        }
        SettingsMutation::Color { kind, rgb } => get_color_setting(settings, *kind) != *rgb,
    }
}

fn path_setting_value(settings: &Settings, kind: PathSetting) -> Option<&Path> {
    match kind {
        PathSetting::Gmod => settings.gmod.as_deref(),
        PathSetting::Downloads => settings.downloads.as_deref(),
        PathSetting::UserData => settings.user_data.as_deref(),
        PathSetting::Temp => settings.temp.as_deref(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    open: bool,
    active_tab: Tab,
    settings: Settings,
    paths: AppPaths,
    system_scheme: SystemColorScheme,
    path_texts: PathTexts,
    path_errors: PathErrors,
    color_texts: ColorTexts,
    color_errors: ColorErrors,
    active_color_picker: Option<ColorSetting>,
    color_picker_draft: Option<HsvColor>,
    status: SettingsStatus,
    reset_busy: bool,
    pending_reset: Option<ResetAction>,
}

impl Default for State {
    fn default() -> Self {
        let mut settings = Settings::default();
        let paths = fallback_paths(&settings);
        settings.sanitize(&paths);
        Self {
            open: false,
            active_tab: Tab::default(),
            path_texts: PathTexts::from_settings(&settings),
            path_errors: PathErrors::default(),
            color_texts: ColorTexts::from_settings(&settings),
            color_errors: ColorErrors::default(),
            active_color_picker: None,
            color_picker_draft: None,
            settings,
            paths,
            system_scheme: SystemColorScheme::Dark,
            status: SettingsStatus::None,
            reset_busy: false,
            pending_reset: None,
        }
    }
}

impl State {
    pub(crate) const fn open(&self) -> bool {
        self.open
    }

    pub(crate) const fn active_tab(&self) -> Tab {
        self.active_tab
    }

    pub(crate) const fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(crate) const fn pending_reset(&self) -> Option<ResetAction> {
        self.pending_reset
    }

    pub(crate) const fn reset_busy(&self) -> bool {
        self.reset_busy
    }

    pub(crate) fn status_key(&self) -> Option<&'static str> {
        match self.status {
            SettingsStatus::None => None,
            SettingsStatus::Saved => Some("settings-saved"),
            SettingsStatus::Saving => Some("settings-saving"),
            SettingsStatus::SaveFailed => Some("settings-save-failed"),
            SettingsStatus::ResetRunning => Some("settings-resets-running"),
            SettingsStatus::ResetComplete(action) => Some(action.success_key()),
            SettingsStatus::ResetFailed => Some("settings-resets-failed"),
        }
    }

    pub(crate) const fn status_error(&self) -> bool {
        matches!(
            self.status,
            SettingsStatus::SaveFailed | SettingsStatus::ResetFailed
        )
    }

    pub(crate) fn open_with_snapshot(&mut self, snapshot: SettingsSnapshot) {
        self.open = true;
        self.pending_reset = None;
        self.reset_busy = false;
        self.status = SettingsStatus::None;
        self.apply_snapshot(snapshot);
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.pending_reset = None;
        self.close_color_picker();
    }

    pub(crate) fn select_tab(&mut self, tab: Tab) {
        self.active_tab = tab;
        self.pending_reset = None;
        self.close_color_picker();
    }

    pub(crate) fn apply_system_scheme(&mut self, system_scheme: SystemColorScheme) {
        self.system_scheme = system_scheme;
    }

    pub(crate) fn path_text(&self, kind: PathSetting) -> &str {
        self.path_texts.get(kind)
    }

    pub(crate) fn path_placeholder(&self, kind: PathSetting) -> String {
        option_path_to_display(path_setting_runtime_path(&self.paths, kind).as_ref())
    }

    pub(crate) fn path_error_key(&self, kind: PathSetting) -> Option<&'static str> {
        self.path_errors.get(kind).map(PathValidationError::key)
    }

    pub(crate) const fn blocks_scrim_close(&self) -> bool {
        self.pending_reset.is_some()
    }

    pub(crate) fn picker_expanded(&self, kind: ColorSetting) -> bool {
        self.active_color_picker == Some(kind)
    }

    pub(crate) const fn active_color_picker(&self) -> Option<ColorSetting> {
        self.active_color_picker
    }

    pub(crate) fn color_text(&self, kind: ColorSetting) -> &str {
        self.color_texts.get(kind)
    }

    pub(crate) fn color_rgb(&self, kind: ColorSetting) -> u32 {
        get_color_setting(&self.settings, kind)
    }

    pub(crate) fn color_invalid(&self, kind: ColorSetting) -> bool {
        self.color_errors.get(kind)
    }

    pub(crate) fn color_hsv(&self, kind: ColorSetting) -> HsvColor {
        if self.picker_expanded(kind)
            && let Some(draft) = self.color_picker_draft
        {
            return draft;
        }

        HsvColor::from_rgb(self.color_rgb(kind))
    }

    pub(crate) fn color_preview_rgb(&self, kind: ColorSetting) -> u32 {
        self.color_hsv(kind).to_rgb()
    }

    pub(crate) fn color_picker_changed(&self, kind: ColorSetting) -> bool {
        self.picker_expanded(kind) && self.color_preview_rgb(kind) != self.color_rgb(kind)
    }

    pub(crate) fn language_value(&self) -> String {
        self.settings
            .language
            .clone()
            .unwrap_or_else(|| DEFAULT_LANGUAGE_VALUE.to_owned())
    }

    pub(crate) fn theme_value(&self) -> &'static str {
        theme_preset_value(self.settings.theme_preset)
    }

    pub(crate) fn download_count_format_value(&self) -> &'static str {
        download_count_format_value(self.settings.download_count_format)
    }

    pub(crate) fn overwrite_mode_value(&self) -> &'static str {
        overwrite_mode_value(&self.settings.extract_overwrite_mode)
    }

    pub(crate) fn set_path_text(&mut self, kind: PathSetting, value: String) {
        self.path_texts.set(kind, value);
        self.path_errors.clear(kind);
        self.close_color_picker();
        self.status = SettingsStatus::None;
    }

    pub(crate) fn path_validation_request(&mut self, kind: PathSetting) -> PathValidationRequest {
        let request = path_validation_request(kind, self.path_text(kind));
        self.path_texts.set(kind, request.requested_display.clone());
        self.path_errors.clear(kind);
        self.status = SettingsStatus::None;
        request
    }

    pub(crate) fn initial_browse_directory(&self, kind: PathSetting) -> PathBuf {
        path_setting_runtime_path(&self.paths, kind)
            .or_else(|| path_option_from_input(self.path_text(kind)))
            .unwrap_or_else(fallback_current_dir)
    }

    pub(crate) fn apply_path_validation(
        &mut self,
        result: PathValidationResult,
    ) -> Option<SettingsMutation> {
        if self.path_text(result.kind) != result.requested_display {
            return None;
        }

        match result.outcome {
            PathValidationOutcome::Accepted { path } => {
                let mutation = SettingsMutation::Path {
                    kind: result.kind,
                    path,
                };
                self.apply_mutation(&mutation).then_some(mutation)
            }
            PathValidationOutcome::Rejected { error } => {
                self.path_texts = PathTexts::from_settings(&self.settings);
                self.path_errors.set(result.kind, error);
                self.status = SettingsStatus::None;
                None
            }
        }
    }

    pub(crate) fn color_edited(
        &mut self,
        kind: ColorSetting,
        value: String,
    ) -> Option<SettingsMutation> {
        self.close_color_picker();
        if let Ok(rgb) = parse_hex_color(&value) {
            let mutation = SettingsMutation::Color { kind, rgb };
            self.apply_mutation(&mutation).then_some(mutation)
        } else {
            self.color_texts.set(kind, value);
            self.color_errors.set(kind, true);
            self.status = SettingsStatus::None;
            None
        }
    }

    pub(crate) fn toggle_color_picker(&mut self, kind: ColorSetting) {
        if self.picker_expanded(kind) {
            self.close_color_picker();
            return;
        }

        self.active_color_picker = Some(kind);
        self.color_picker_draft = Some(HsvColor::from_rgb(self.color_rgb(kind)));
    }

    pub(crate) fn set_color_picker_channel(
        &mut self,
        kind: ColorSetting,
        channel: ColorChannel,
        value: f32,
    ) {
        if !self.picker_expanded(kind) {
            return;
        }

        let mut draft = self
            .color_picker_draft
            .unwrap_or_else(|| HsvColor::from_rgb(self.color_rgb(kind)));
        draft.set_channel(channel, value);
        self.color_picker_draft = Some(draft);
    }

    pub(crate) fn apply_color_picker(&mut self, kind: ColorSetting) -> Option<SettingsMutation> {
        if !self.picker_expanded(kind) {
            return None;
        }

        let rgb = self
            .color_picker_draft
            .unwrap_or_else(|| HsvColor::from_rgb(self.color_rgb(kind)))
            .to_rgb();
        self.close_color_picker();
        let mutation = SettingsMutation::Color { kind, rgb };
        self.apply_mutation(&mutation).then_some(mutation)
    }

    pub(crate) fn close_color_picker(&mut self) {
        self.active_color_picker = None;
        self.color_picker_draft = None;
    }

    pub(crate) fn cancel_top_layer(&mut self) -> bool {
        if self.pending_reset.is_some() {
            self.pending_reset = None;
            return true;
        }

        if self.active_color_picker.is_some() {
            self.close_color_picker();
            return true;
        }

        false
    }

    pub(crate) fn request_reset(&mut self, action: ResetAction) {
        if !self.reset_busy {
            self.close_color_picker();
            self.pending_reset = Some(action);
        }
    }

    pub(crate) fn cancel_reset(&mut self) {
        self.pending_reset = None;
    }

    pub(crate) fn begin_reset(&mut self) -> Option<ResetAction> {
        let action = self.pending_reset.take()?;
        if self.reset_busy {
            return None;
        }
        self.reset_busy = true;
        self.status = SettingsStatus::ResetRunning;
        Some(action)
    }

    /// Applies a finished reset and, on success, hands back the snapshot now
    /// stored in `self` (post-sanitize) so the caller can forward it as an
    /// effect without keeping its own separate clone of the pre-sanitize
    /// value.
    pub(crate) fn apply_reset_completed(
        &mut self,
        action: ResetAction,
        result: Result<Option<SettingsSnapshot>, UiError>,
    ) -> Option<SettingsSnapshot> {
        self.reset_busy = false;
        match result {
            Ok(Some(snapshot)) => {
                self.apply_snapshot(snapshot);
                self.status = SettingsStatus::ResetComplete(action);
                Some(self.current_snapshot())
            }
            Ok(None) => {
                self.status = SettingsStatus::ResetComplete(action);
                None
            }
            Err(error) => {
                log::warn!("Settings reset failed: {error}");
                self.status = SettingsStatus::ResetFailed;
                None
            }
        }
    }

    pub(crate) fn apply_save_started(&mut self) {
        self.status = SettingsStatus::Saving;
    }

    /// Applies a finished save and, on success, hands back the snapshot now
    /// stored in `self` (see `apply_reset_completed`).
    pub(crate) fn apply_save_completed(
        &mut self,
        result: Result<SettingsSnapshot, UiError>,
    ) -> Option<SettingsSnapshot> {
        match result {
            Ok(snapshot) => {
                self.apply_snapshot(snapshot);
                self.status = SettingsStatus::Saved;
                Some(self.current_snapshot())
            }
            Err(error) => {
                log::warn!("Settings save failed: {error}");
                self.status = SettingsStatus::SaveFailed;
                None
            }
        }
    }

    pub(crate) fn scalar_mutation(
        &mut self,
        mutation: SettingsMutation,
    ) -> Option<SettingsMutation> {
        self.apply_mutation(&mutation).then_some(mutation)
    }

    pub(crate) fn language_mutation(&mut self, value: &str) -> Option<SettingsMutation> {
        self.scalar_mutation(SettingsMutation::Language(language_setting_from_value(
            value,
        )))
    }

    pub(crate) fn download_count_format_mutation(
        &mut self,
        value: &str,
    ) -> Option<SettingsMutation> {
        let format = download_count_format_from_value(value)?;
        self.scalar_mutation(SettingsMutation::DownloadCountFormat(format))
    }

    pub(crate) fn theme_mutation(&mut self, value: &str) -> Option<SettingsMutation> {
        let preset = theme_preset_from_value(value)?;
        let concrete_preset =
            concrete_preset_for_effective(effective_theme_preset(preset, self.system_scheme));
        let (neutral, success, error) = concrete_preset.accent_colors();
        self.scalar_mutation(SettingsMutation::Theme {
            preset,
            neutral,
            success,
            error,
        })
    }

    pub(crate) fn overwrite_mode_mutation(&mut self, value: &str) -> Option<SettingsMutation> {
        let mode = overwrite_mode_from_value(value)?;
        self.scalar_mutation(SettingsMutation::OverwriteMode(mode))
    }

    fn current_snapshot(&self) -> SettingsSnapshot {
        SettingsSnapshot::new(
            self.settings.clone(),
            self.paths.clone(),
            self.system_scheme,
        )
    }

    fn apply_snapshot(&mut self, snapshot: SettingsSnapshot) {
        let SettingsSnapshot {
            mut settings,
            paths,
            system_scheme,
        } = snapshot;
        settings.sanitize(&paths);
        self.path_texts = PathTexts::from_settings(&settings);
        self.path_errors = PathErrors::default();
        self.color_texts = ColorTexts::from_settings(&settings);
        self.color_errors = ColorErrors::default();
        self.close_color_picker();
        self.settings = settings;
        self.paths = paths;
        self.system_scheme = system_scheme;
    }

    fn apply_mutation(&mut self, mutation: &SettingsMutation) -> bool {
        if !mutation_changes(&self.settings, mutation) {
            return false;
        }
        apply_settings_mutation(&mut self.settings, mutation.clone());

        match mutation {
            SettingsMutation::Path { kind, .. } => {
                self.path_texts = PathTexts::from_settings(&self.settings);
                self.path_errors.clear(*kind);
            }
            SettingsMutation::Theme { .. } => {
                self.color_texts = ColorTexts::from_settings(&self.settings);
                self.color_errors = ColorErrors::default();
                self.close_color_picker();
            }
            SettingsMutation::Color { kind, .. } => {
                self.color_texts = ColorTexts::from_settings(&self.settings);
                self.color_errors.set(*kind, false);
                self.close_color_picker();
            }
            SettingsMutation::Sounds(_)
            | SettingsMutation::PlayGifsByDefault(_)
            | SettingsMutation::Titlebar(_)
            | SettingsMutation::Language(_)
            | SettingsMutation::DownloadCountFormat(_)
            | SettingsMutation::OverwriteMode(_) => {}
        }
        self.status = SettingsStatus::None;
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingsStatus {
    None,
    Saving,
    Saved,
    SaveFailed,
    ResetRunning,
    ResetComplete(ResetAction),
    ResetFailed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PathTexts([String; PathSetting::ALL.len()]);

impl PathTexts {
    fn from_settings(settings: &Settings) -> Self {
        Self(PathSetting::ALL.map(|kind| {
            option_path_to_display(match kind {
                PathSetting::Gmod => settings.gmod.as_ref(),
                PathSetting::Downloads => settings.downloads.as_ref(),
                PathSetting::UserData => settings.user_data.as_ref(),
                PathSetting::Temp => settings.temp.as_ref(),
            })
        }))
    }

    fn get(&self, kind: PathSetting) -> &str {
        &self.0[kind.index()]
    }

    fn set(&mut self, kind: PathSetting, value: String) {
        self.0[kind.index()] = value;
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PathErrors([Option<PathValidationError>; PathSetting::ALL.len()]);

impl PathErrors {
    fn get(&self, kind: PathSetting) -> Option<PathValidationError> {
        self.0[kind.index()]
    }

    fn set(&mut self, kind: PathSetting, error: PathValidationError) {
        self.0[kind.index()] = Some(error);
    }

    fn clear(&mut self, kind: PathSetting) {
        self.0[kind.index()] = None;
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ColorTexts([String; ColorSetting::ALL.len()]);

impl ColorTexts {
    fn from_settings(settings: &Settings) -> Self {
        Self(ColorSetting::ALL.map(|kind| format_hex_color(get_color_setting(settings, kind))))
    }

    fn get(&self, kind: ColorSetting) -> &str {
        &self.0[kind.index()]
    }

    fn set(&mut self, kind: ColorSetting, value: String) {
        self.0[kind.index()] = value;
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ColorErrors([bool; ColorSetting::ALL.len()]);

impl ColorErrors {
    fn get(&self, kind: ColorSetting) -> bool {
        self.0[kind.index()]
    }

    fn set(&mut self, kind: ColorSetting, invalid: bool) {
        self.0[kind.index()] = invalid;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathValidationRequest {
    pub(crate) kind: PathSetting,
    input: String,
    requested_display: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathValidationResult {
    pub(crate) kind: PathSetting,
    requested_display: String,
    outcome: PathValidationOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PathValidationOutcome {
    Accepted { path: Option<PathBuf> },
    Rejected { error: PathValidationError },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PathValidationError {
    InvalidGmod,
    InvalidDirectory,
}

impl PathValidationError {
    const fn key(self) -> &'static str {
        match self {
            Self::InvalidGmod => "settings-paths-invalid-gmod",
            Self::InvalidDirectory => "settings-paths-invalid-directory",
        }
    }
}

pub fn validate_path_request(request: PathValidationRequest) -> PathValidationResult {
    let path = path_option_from_input(&request.input);
    let outcome = match validate_path_choice(request.kind, path.as_deref()) {
        Ok(()) => PathValidationOutcome::Accepted { path },
        Err(error) => PathValidationOutcome::Rejected { error },
    };
    PathValidationResult {
        kind: request.kind,
        requested_display: request.requested_display,
        outcome,
    }
}

fn path_validation_request(kind: PathSetting, input: &str) -> PathValidationRequest {
    let path = path_option_from_input(input);
    let requested_display = path.as_ref().map(path_to_display).unwrap_or_default();
    PathValidationRequest {
        kind,
        input: input.to_owned(),
        requested_display,
    }
}

fn validate_path_choice(kind: PathSetting, path: Option<&Path>) -> Result<(), PathValidationError> {
    let Some(path) = path else {
        return Ok(());
    };

    match kind {
        PathSetting::Gmod if validate_gmod(path) => Ok(()),
        PathSetting::Gmod => Err(PathValidationError::InvalidGmod),
        PathSetting::Downloads | PathSetting::UserData | PathSetting::Temp if path.is_dir() => {
            Ok(())
        }
        PathSetting::Downloads | PathSetting::UserData | PathSetting::Temp => {
            Err(PathValidationError::InvalidDirectory)
        }
    }
}

fn set_path_setting(settings: &mut Settings, kind: PathSetting, path: Option<PathBuf>) {
    match kind {
        PathSetting::Gmod => settings.gmod = path,
        PathSetting::Downloads => settings.downloads = path,
        PathSetting::UserData => settings.user_data = path,
        PathSetting::Temp => settings.temp = path,
    }
}

fn path_setting_runtime_path(paths: &AppPaths, kind: PathSetting) -> Option<PathBuf> {
    match kind {
        PathSetting::Gmod => paths.gmod_dir.clone(),
        PathSetting::Downloads => paths.downloads_dir.clone(),
        PathSetting::UserData => Some(paths.user_data_dir.clone()),
        PathSetting::Temp => Some(paths.temp_dir.clone()),
    }
}

fn set_color_setting(settings: &mut Settings, kind: ColorSetting, rgb: u32) {
    match kind {
        ColorSetting::Neutral => settings.color_neutral = rgb,
        ColorSetting::Success => settings.color_success = rgb,
        ColorSetting::Error => settings.color_error = rgb,
    }
}

fn get_color_setting(settings: &Settings, kind: ColorSetting) -> u32 {
    match kind {
        ColorSetting::Neutral => settings.color_neutral,
        ColorSetting::Success => settings.color_success,
        ColorSetting::Error => settings.color_error,
    }
}

fn theme_preset_value(preset: ThemePreset) -> &'static str {
    preset.as_value()
}

fn theme_preset_from_value(value: &str) -> Option<ThemePreset> {
    ThemePreset::from_value(value)
}

fn concrete_preset_for_effective(preset: EffectiveThemePreset) -> ThemePreset {
    match preset {
        EffectiveThemePreset::Dark => ThemePreset::Dark,
        EffectiveThemePreset::Light => ThemePreset::Light,
        EffectiveThemePreset::ClassicSource => ThemePreset::ClassicSource,
    }
}

fn download_count_format_value(format: DownloadCountFormat) -> &'static str {
    format.as_value()
}

fn download_count_format_from_value(value: &str) -> Option<DownloadCountFormat> {
    DownloadCountFormat::from_value(value)
}

fn overwrite_mode_value(mode: &ExtractionOverwriteMode) -> &'static str {
    match mode {
        ExtractionOverwriteMode::Recycle => "recycle",
        ExtractionOverwriteMode::Delete => "delete",
        ExtractionOverwriteMode::Overwrite => "overwrite",
    }
}

fn overwrite_mode_from_value(value: &str) -> Option<ExtractionOverwriteMode> {
    match value {
        "recycle" => Some(ExtractionOverwriteMode::Recycle),
        "delete" => Some(ExtractionOverwriteMode::Delete),
        "overwrite" => Some(ExtractionOverwriteMode::Overwrite),
        _ => None,
    }
}

fn language_setting_from_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == DEFAULT_LANGUAGE_VALUE {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

pub fn default_language_value() -> &'static str {
    DEFAULT_LANGUAGE_VALUE
}

pub fn format_hex_color(rgb: u32) -> String {
    format!("#{:06X}", rgb & 0xFF_FFFF)
}

fn parse_hex_color(input: &str) -> Result<u32, ()> {
    let trimmed = input.trim();
    let Some(hex) = trimmed.strip_prefix('#') else {
        return Err(());
    };
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(());
    }
    u32::from_str_radix(hex, 16).map_err(|_| ())
}

fn path_option_from_input(input: &str) -> Option<PathBuf> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn option_path_to_display(path: Option<&PathBuf>) -> String {
    path.map(path_to_display).unwrap_or_default()
}

pub fn accent_inputs_from_settings(settings: &Settings) -> AccentInputs {
    AccentInputs {
        neutral: settings.color_neutral,
        success: settings.color_success,
        error: settings.color_error,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::bridge::{DownloadCountFormat, ThemePreset};

    use super::*;

    #[test]
    fn theme_options_are_auto_first_and_persist_auto_preference() {
        let mut state = State::default();
        state.settings.theme_preset = ThemePreset::Dark;

        let mutation = state
            .theme_mutation("auto")
            .expect("auto selection should produce a mutation");

        assert!(matches!(
            mutation,
            SettingsMutation::Theme {
                preset: ThemePreset::Auto,
                ..
            }
        ));
        assert_eq!(state.theme_value(), "auto");
    }

    #[test]
    fn language_default_maps_to_none() {
        let mut state = State::default();
        state.settings.language = Some("fr".to_owned());

        let mutation = state
            .language_mutation(default_language_value())
            .expect("default should update explicit language");

        assert_eq!(mutation, SettingsMutation::Language(None));
        assert_eq!(state.settings.language, None);
    }

    #[test]
    fn download_count_format_values_round_trip() {
        let mut state = State::default();

        let mutation = state
            .download_count_format_mutation("period")
            .expect("period should be valid");

        assert_eq!(
            mutation,
            SettingsMutation::DownloadCountFormat(DownloadCountFormat::Period)
        );
        assert_eq!(state.download_count_format_value(), "period");
    }

    #[test]
    fn titlebar_mutation_updates_persisted_preference() {
        let mut state = State::default();

        let mutation = state
            .scalar_mutation(SettingsMutation::Titlebar(TitlebarPreference::System))
            .expect("system titlebar should produce a mutation");

        assert_eq!(
            mutation,
            SettingsMutation::Titlebar(TitlebarPreference::System)
        );
        assert_eq!(state.settings.titlebar, TitlebarPreference::System);
    }

    #[test]
    fn overwrite_mode_values_round_trip_to_backend_extraction_settings() {
        let mut state = State::default();

        let mutation = state
            .overwrite_mode_mutation("delete")
            .expect("delete should be valid");

        assert_eq!(
            mutation,
            SettingsMutation::OverwriteMode(ExtractionOverwriteMode::Delete)
        );
        assert_eq!(state.overwrite_mode_value(), "delete");
        assert_eq!(
            state.overwrite_mode_mutation("overwrite"),
            Some(SettingsMutation::OverwriteMode(
                ExtractionOverwriteMode::Overwrite
            ))
        );
        assert_eq!(
            state.overwrite_mode_mutation("recycle"),
            Some(SettingsMutation::OverwriteMode(
                ExtractionOverwriteMode::Recycle
            ))
        );
        assert_eq!(state.overwrite_mode_mutation("unknown"), None);
    }

    #[test]
    fn path_validation_accepts_blank_as_default_override() {
        let mut state = State::default();
        state.settings.downloads = Some(PathBuf::from("/tmp/downloads"));
        state.path_texts = PathTexts::from_settings(&state.settings);
        state.set_path_text(PathSetting::Downloads, String::new());

        let request = state.path_validation_request(PathSetting::Downloads);
        let result = validate_path_request(request);
        let mutation = state
            .apply_path_validation(result)
            .expect("blank path should clear override");

        assert_eq!(
            mutation,
            SettingsMutation::Path {
                kind: PathSetting::Downloads,
                path: None,
            }
        );
    }

    #[test]
    fn path_validation_rejects_missing_directories() {
        let mut state = State::default();
        state.set_path_text(PathSetting::Temp, "/definitely/missing".to_owned());

        let request = state.path_validation_request(PathSetting::Temp);
        let result = validate_path_request(request);

        assert_eq!(state.apply_path_validation(result), None);
        assert_eq!(
            state.path_error_key(PathSetting::Temp),
            Some("settings-paths-invalid-directory")
        );
    }

    #[test]
    fn path_validation_accepts_existing_directories() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let mut state = State::default();
        state.set_path_text(
            PathSetting::Downloads,
            temp.path().to_string_lossy().into_owned(),
        );

        let request = state.path_validation_request(PathSetting::Downloads);
        let result = validate_path_request(request);
        let mutation = state
            .apply_path_validation(result)
            .expect("existing directory should be accepted");

        assert_eq!(
            mutation,
            SettingsMutation::Path {
                kind: PathSetting::Downloads,
                path: Some(temp.path().to_path_buf()),
            }
        );
    }

    #[test]
    fn gmod_validation_requires_garrysmod_addons_directory() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let gmod = temp.path().join("gmod");
        fs::create_dir_all(gmod.join("GarrysMod").join("addons"))
            .expect("gmod addon folder should be created");
        let mut state = State::default();
        state.set_path_text(PathSetting::Gmod, gmod.to_string_lossy().into_owned());

        let request = state.path_validation_request(PathSetting::Gmod);
        let result = validate_path_request(request);

        assert!(state.apply_path_validation(result).is_some());
    }

    #[test]
    fn invalid_color_stays_local_and_valid_color_saves() {
        let mut state = State::default();

        assert_eq!(
            state.color_edited(ColorSetting::Neutral, "#12".to_owned()),
            None
        );
        assert!(state.color_invalid(ColorSetting::Neutral));

        let mutation = state
            .color_edited(ColorSetting::Neutral, "#123456".to_owned())
            .expect("complete hex color should save");

        assert_eq!(
            mutation,
            SettingsMutation::Color {
                kind: ColorSetting::Neutral,
                rgb: 0x123456,
            }
        );
        assert_eq!(state.color_text(ColorSetting::Neutral), "#123456");
        assert!(!state.color_invalid(ColorSetting::Neutral));
    }

    #[test]
    fn color_picker_edits_draft_until_applied() {
        let mut state = State::default();
        let original = state.color_rgb(ColorSetting::Neutral);

        state.toggle_color_picker(ColorSetting::Neutral);

        assert_eq!(state.active_color_picker(), Some(ColorSetting::Neutral));
        assert!(!state.color_picker_changed(ColorSetting::Neutral));

        state.set_color_picker_channel(ColorSetting::Neutral, ColorChannel::Hue, 180.0);

        assert!(state.picker_expanded(ColorSetting::Neutral));
        assert!(state.color_picker_changed(ColorSetting::Neutral));
        assert_eq!(state.color_rgb(ColorSetting::Neutral), original);
        assert_ne!(state.color_preview_rgb(ColorSetting::Neutral), original);

        let mutation = state
            .apply_color_picker(ColorSetting::Neutral)
            .expect("applying the active picker should emit a color mutation");

        assert!(!state.picker_expanded(ColorSetting::Neutral));
        assert_eq!(
            mutation,
            SettingsMutation::Color {
                kind: ColorSetting::Neutral,
                rgb: state.color_rgb(ColorSetting::Neutral),
            }
        );
        assert_ne!(state.color_rgb(ColorSetting::Neutral), original);
    }
}
