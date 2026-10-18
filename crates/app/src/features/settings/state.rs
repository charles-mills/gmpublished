use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use crate::bridge::ExtractionOverwriteMode;
use crate::bridge::ui_error::UiError;
use crate::bridge::{
    AppPaths, DownloadCountFormat, EffectiveThemePreset, Settings, SystemColorScheme, ThemePreset,
    TitlebarPreference, UiDensity, effective_theme_preset, validate_gmod,
};
use crate::generation::Generation;

use crate::bridge::tasks::fallback_paths;
use crate::theme::AccentInputs;
use crate::util::paths::{fallback_current_dir, path_to_display};

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

mapped_enum_with_all! {
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub enum Tab {
        About => "settings-tab-about",
        #[default]
        General => "settings-tab-general",
        Paths => "settings-tab-paths",
        Accessibility => "settings-tab-accessibility",
        Resets => "settings-tab-resets",
    }
    label_key -> &'static str
}

mapped_enum_with_all! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum PathSetting {
        Gmod => "settings-paths-gmod",
        Downloads => "settings-paths-downloads",
        UserData => "settings-paths-user-data",
        Temp => "settings-paths-temp",
    }
    label_key -> &'static str
}

mapped_enum_with_all! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    #[repr(usize)]
    pub enum ColorSetting {
        Neutral => "settings-accessibility-color-neutral",
        Success => "settings-accessibility-color-success",
        Error => "settings-accessibility-color-error",
    }
    label_key -> &'static str
}

impl ColorSetting {
    #[cfg(test)]
    pub(crate) const fn index(self) -> usize {
        self as usize
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
    // Only macOS constructs this mutation; the persisted setting is cross-platform.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Titlebar(TitlebarPreference),
    Language(Option<String>),
    DownloadCountFormat(DownloadCountFormat),
    Density(UiDensity),
    TextSize(u16),
    ReduceMotion(bool),
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
        SettingsMutation::Sounds(enabled) => settings.backend.sounds = enabled,
        SettingsMutation::PlayGifsByDefault(enabled) => {
            settings.ui.play_gifs_by_default = enabled;
        }
        SettingsMutation::Titlebar(preference) => settings.backend.titlebar = preference,
        SettingsMutation::Language(language) => settings.backend.language = language,
        SettingsMutation::DownloadCountFormat(format) => settings.ui.download_count_format = format,
        SettingsMutation::Density(density) => settings.ui.density = density,
        SettingsMutation::ReduceMotion(enabled) => settings.ui.reduce_motion = enabled,
        SettingsMutation::TextSize(value) => {
            if crate::bridge::valid_text_size_percent(value) {
                settings.ui.text_size_percent = value;
            }
        }
        SettingsMutation::Theme {
            preset,
            neutral,
            success,
            error,
        } => {
            settings.ui.theme_preset = preset;
            settings.backend.color_neutral = neutral;
            settings.backend.color_success = success;
            settings.backend.color_error = error;
        }
        SettingsMutation::OverwriteMode(mode) => settings.backend.extract_overwrite_mode = mode,
        SettingsMutation::Path { kind, path } => set_path_setting(settings, kind, path),
        SettingsMutation::Color { kind, rgb } => set_color_setting(settings, kind, rgb),
    }
}

fn mutation_changes(settings: &Settings, mutation: &SettingsMutation) -> bool {
    let mut changed = settings.clone();
    apply_settings_mutation(&mut changed, mutation.clone());
    changed != *settings
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistenceCommand {
    Save(Generation, SettingsMutation),
    Reset(ResetAction),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteStatus {
    Queued,
    Running,
    Failed,
}

#[derive(Debug)]
struct PendingSave {
    generation: Generation,
    mutation: SettingsMutation,
    status: WriteStatus,
}

#[derive(Debug)]
pub struct State {
    pub(super) about: super::about::AboutState,
    open: bool,
    active_tab: Tab,
    settings: Settings,
    durable: SettingsSnapshot,
    saves: VecDeque<PendingSave>,
    reset: Option<(ResetAction, WriteStatus)>,
    path_fields: PathFields,
    color_fields: ColorFields,
    active_color_picker: Option<ColorSetting>,
    color_picker_draft: Option<HsvColor>,
    status: SettingsStatus,
    /// Identifies queued writes independently of external settings snapshots.
    save_generation: Generation,
    /// A slider preview is applied live but saved only when the edit completes.
    pending_text_size: Option<u16>,
    last_text_preview: Option<std::time::Instant>,
    pending_reset: Option<ResetAction>,
}

impl Default for State {
    fn default() -> Self {
        let settings = Settings::default();
        let paths = fallback_paths(&settings);
        Self {
            about: super::about::AboutState::default(),
            open: false,
            active_tab: Tab::default(),
            path_fields: PathFields::from_settings(&settings),
            color_fields: ColorFields::from_settings(&settings),
            active_color_picker: None,
            color_picker_draft: None,
            durable: SettingsSnapshot::new(settings.clone(), paths, SystemColorScheme::Dark),
            settings,
            saves: VecDeque::new(),
            reset: None,
            status: SettingsStatus::None,
            save_generation: Generation::INITIAL,
            pending_text_size: None,
            last_text_preview: None,
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
        self.reset.is_some()
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
        self.active_tab = Tab::General;
        self.about.reset();
        self.open = true;
        self.pending_reset = None;
        self.status = SettingsStatus::None;
        self.observe_snapshot(snapshot);
        self.path_fields = PathFields::from_settings(&self.settings);
        self.color_fields = ColorFields::from_settings(&self.settings);
        self.close_color_picker();
    }

    pub(crate) fn close(&mut self) {
        self.about.reset();
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
        self.durable.system_scheme = system_scheme;
    }

    pub(crate) fn path_text(&self, kind: PathSetting) -> &str {
        self.path_fields.text(kind)
    }

    pub(crate) fn path_placeholder(&self, kind: PathSetting) -> String {
        option_path_to_display(path_setting_runtime_path(&self.durable.paths, kind).as_deref())
    }

    pub(crate) fn path_error_key(&self, kind: PathSetting) -> Option<&'static str> {
        self.path_fields.error(kind).map(PathValidationError::key)
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
        self.color_fields.text(kind)
    }

    pub(crate) fn color_rgb(&self, kind: ColorSetting) -> u32 {
        get_color_setting(&self.settings, kind)
    }

    pub(crate) fn color_invalid(&self, kind: ColorSetting) -> bool {
        self.color_fields.invalid(kind)
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

    /// `None` is "follow the system language".
    pub(crate) fn language(&self) -> Option<&str> {
        self.settings.backend.language.as_deref()
    }

    pub(crate) const fn theme_preset(&self) -> ThemePreset {
        self.settings.ui.theme_preset
    }

    pub(crate) const fn download_count_format(&self) -> DownloadCountFormat {
        self.settings.ui.download_count_format
    }

    pub(crate) fn overwrite_mode(&self) -> ExtractionOverwriteMode {
        self.settings.backend.extract_overwrite_mode.clone()
    }

    pub(crate) fn set_path_text(&mut self, kind: PathSetting, value: String) {
        self.path_fields.set_text(kind, value);
        self.close_color_picker();
        self.status = SettingsStatus::None;
    }

    pub(crate) fn path_validation_request(&mut self, kind: PathSetting) -> PathValidationRequest {
        let request = path_validation_request(kind, self.path_text(kind));
        self.path_fields
            .set_text(kind, request.requested_display.clone());
        self.status = SettingsStatus::None;
        request
    }

    pub(crate) fn initial_browse_directory(&self, kind: PathSetting) -> PathBuf {
        path_setting_runtime_path(&self.durable.paths, kind)
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
                self.path_fields = PathFields::from_settings(&self.settings);
                self.path_fields.set_error(result.kind, error);
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
            self.color_fields.set_invalid_text(kind, value);
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
        if !self.reset_busy() {
            self.close_color_picker();
            self.pending_reset = Some(action);
        }
    }

    pub(crate) fn cancel_reset(&mut self) {
        self.pending_reset = None;
    }

    pub(crate) fn begin_reset(&mut self) -> Option<ResetAction> {
        if self.reset_busy() {
            return None;
        }
        let action = self.pending_reset.take()?;
        self.reset = Some((action, WriteStatus::Queued));
        self.status = SettingsStatus::ResetRunning;
        Some(action)
    }

    pub(crate) fn next_persistence(&mut self) -> Option<PersistenceCommand> {
        if self
            .saves
            .iter()
            .any(|save| save.status == WriteStatus::Running)
            || self
                .reset
                .is_some_and(|(_, status)| status == WriteStatus::Running)
        {
            return None;
        }
        if let Some(save) = self
            .saves
            .iter_mut()
            .find(|save| save.status == WriteStatus::Queued)
        {
            save.status = WriteStatus::Running;
            return Some(PersistenceCommand::Save(
                save.generation,
                save.mutation.clone(),
            ));
        }
        let (action, status) = self.reset.as_mut()?;
        *status = WriteStatus::Running;
        Some(PersistenceCommand::Reset(*action))
    }

    pub(crate) fn apply_reset_completed(
        &mut self,
        action: ResetAction,
        result: Result<Option<SettingsSnapshot>, UiError>,
    ) -> Option<()> {
        if !self.reset.is_some_and(|(active, _)| active == action) {
            return None;
        }
        self.reset = None;
        match result {
            Ok(Some(snapshot)) => {
                self.saves.clear();
                self.pending_text_size = None;
                self.last_text_preview = None;
                self.observe_snapshot(snapshot);
                self.status = SettingsStatus::ResetComplete(action);
                Some(())
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

    #[cfg(test)]
    pub(crate) const fn save_generation_for_test(&self) -> Generation {
        self.save_generation
    }

    #[cfg(test)]
    pub(super) fn saves_acknowledged_for_test(&self) -> bool {
        self.saves.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn active_save_for_test(&self) -> Option<(Generation, SettingsMutation)> {
        self.saves
            .iter()
            .find(|save| save.status == WriteStatus::Running)
            .map(|save| (save.generation, save.mutation.clone()))
    }

    #[cfg(test)]
    pub(crate) fn queued_saves_for_test(&self) -> usize {
        self.saves
            .iter()
            .filter(|save| save.status == WriteStatus::Queued)
            .count()
    }

    pub(crate) fn apply_save_started(&mut self, mutation: SettingsMutation) -> Generation {
        self.status = SettingsStatus::Saving;
        let generation = self.save_generation.bump();
        self.saves.push_back(PendingSave {
            generation,
            mutation,
            status: WriteStatus::Queued,
        });
        generation
    }

    pub(crate) fn apply_save_completed(
        &mut self,
        generation: Generation,
        result: Result<SettingsSnapshot, UiError>,
    ) -> Option<()> {
        let index = self
            .saves
            .iter()
            .position(|save| save.generation == generation)?;
        if self
            .saves
            .iter()
            .any(|save| save.status == WriteStatus::Running && save.generation != generation)
        {
            return None;
        }
        match result {
            Ok(snapshot) => {
                if mutation_changes(&snapshot.settings, &self.saves[index].mutation) {
                    self.saves[index].status = WriteStatus::Failed;
                } else {
                    let completed = self.saves.remove(index).expect("completed save exists");
                    let mut preceding = index;
                    self.saves.retain_mut(|save| {
                        if preceding == 0 {
                            return true;
                        }
                        preceding -= 1;
                        if save.status != WriteStatus::Failed {
                            return true;
                        }
                        // A color edit can acknowledge one field of a failed theme write.
                        if let (
                            SettingsMutation::Theme {
                                neutral,
                                success,
                                error,
                                ..
                            },
                            SettingsMutation::Color { kind, rgb },
                        ) = (&mut save.mutation, &completed.mutation)
                        {
                            *match kind {
                                ColorSetting::Neutral => neutral,
                                ColorSetting::Success => success,
                                ColorSetting::Error => error,
                            } = *rgb;
                        }
                        let mut replayed = snapshot.settings.clone();
                        apply_settings_mutation(&mut replayed, save.mutation.clone());
                        apply_settings_mutation(&mut replayed, completed.mutation.clone());
                        replayed != snapshot.settings
                    });
                }
                self.observe_snapshot(snapshot);
                if generation == self.save_generation && !self.reset_busy() {
                    self.status = SettingsStatus::Saved;
                }
                Some(())
            }
            Err(error) => {
                self.saves[index].status = WriteStatus::Failed;
                if generation == self.save_generation {
                    log::warn!("Settings save failed: {error}");
                    if !self.reset_busy() {
                        self.status = SettingsStatus::SaveFailed;
                    }
                }
                None
            }
        }
    }

    pub(crate) fn observe_snapshot(&mut self, snapshot: SettingsSnapshot) {
        self.saves.retain(|save| {
            save.status != WriteStatus::Failed
                || mutation_changes(&snapshot.settings, &save.mutation)
        });
        self.durable = snapshot;
        let mut effective = self.durable.settings.clone();
        for save in &self.saves {
            apply_settings_mutation(&mut effective, save.mutation.clone());
        }
        if let Some(value) = self.pending_text_size {
            effective.ui.text_size_percent = value;
        }
        self.apply_saved_settings(effective);
    }

    pub(crate) fn preview_text_size(&mut self, value: u16) -> Option<SettingsMutation> {
        let mutation = SettingsMutation::TextSize(value);
        if !self.apply_mutation(&mutation) {
            return None;
        }
        self.pending_text_size = Some(value);
        self.last_text_preview = Some(std::time::Instant::now());
        Some(mutation)
    }

    pub(crate) const fn has_text_preview(&self) -> bool {
        self.pending_text_size.is_some()
    }

    pub(crate) fn commit_text_size_if_idle(
        &mut self,
        now: std::time::Instant,
    ) -> Option<SettingsMutation> {
        if self.last_text_preview.is_some_and(|last| {
            now.saturating_duration_since(last) >= std::time::Duration::from_millis(250)
        }) {
            self.commit_text_size()
        } else {
            None
        }
    }

    pub(crate) fn commit_text_size(&mut self) -> Option<SettingsMutation> {
        self.last_text_preview = None;
        self.pending_text_size
            .take()
            .map(SettingsMutation::TextSize)
    }

    pub(crate) fn scalar_mutation(
        &mut self,
        mutation: SettingsMutation,
    ) -> Option<SettingsMutation> {
        self.apply_mutation(&mutation).then_some(mutation)
    }

    pub(crate) fn language_mutation(
        &mut self,
        language: Option<String>,
    ) -> Option<SettingsMutation> {
        self.scalar_mutation(SettingsMutation::Language(language))
    }

    pub(crate) fn download_count_format_mutation(
        &mut self,
        format: DownloadCountFormat,
    ) -> Option<SettingsMutation> {
        self.scalar_mutation(SettingsMutation::DownloadCountFormat(format))
    }

    pub(crate) fn theme_mutation(&mut self, preset: ThemePreset) -> Option<SettingsMutation> {
        let concrete_preset = concrete_preset_for_effective(effective_theme_preset(
            preset,
            self.durable.system_scheme,
        ));
        let (neutral, success, error) = concrete_preset.accent_colors();
        self.scalar_mutation(SettingsMutation::Theme {
            preset,
            neutral,
            success,
            error,
        })
    }

    pub(crate) fn overwrite_mode_mutation(
        &mut self,
        mode: ExtractionOverwriteMode,
    ) -> Option<SettingsMutation> {
        self.scalar_mutation(SettingsMutation::OverwriteMode(mode))
    }

    pub(crate) fn current_snapshot(&self) -> SettingsSnapshot {
        SettingsSnapshot::new(
            self.settings.clone(),
            self.durable.paths.clone(),
            self.durable.system_scheme,
        )
    }

    fn apply_saved_settings(&mut self, settings: Settings) {
        let path_defaults = PathFields::from_settings(&self.settings);
        let color_defaults = ColorFields::from_settings(&self.settings);
        let paths = self.path_fields.clone();
        let colors = self.color_fields.clone();
        let picker = (self.active_color_picker, self.color_picker_draft);
        self.path_fields = PathFields::from_settings(&settings);
        self.color_fields = ColorFields::from_settings(&settings);
        self.settings = settings;
        for kind in PathSetting::ALL {
            if paths.text(kind) != path_defaults.text(kind) || paths.error(kind).is_some() {
                *self.path_fields.get_mut(kind) = paths.get(kind).clone();
            }
        }
        for kind in ColorSetting::ALL {
            if colors.text(kind) != color_defaults.text(kind) || colors.invalid(kind) {
                *self.color_fields.get_mut(kind) = colors.get(kind).clone();
            }
        }
        (self.active_color_picker, self.color_picker_draft) = picker;
    }

    fn apply_mutation(&mut self, mutation: &SettingsMutation) -> bool {
        if !mutation_changes(&self.settings, mutation) {
            return false;
        }
        apply_settings_mutation(&mut self.settings, mutation.clone());

        match mutation {
            SettingsMutation::Path { .. } => {
                self.path_fields = PathFields::from_settings(&self.settings);
            }
            SettingsMutation::Theme { .. } => {
                self.color_fields = ColorFields::from_settings(&self.settings);
                self.close_color_picker();
            }
            SettingsMutation::Color { .. } => {
                self.color_fields = ColorFields::from_settings(&self.settings);
                self.close_color_picker();
            }
            SettingsMutation::Sounds(_)
            | SettingsMutation::PlayGifsByDefault(_)
            | SettingsMutation::Titlebar(_)
            | SettingsMutation::Language(_)
            | SettingsMutation::DownloadCountFormat(_)
            | SettingsMutation::Density(_)
            | SettingsMutation::TextSize(_)
            | SettingsMutation::ReduceMotion(_)
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
struct PathField {
    text: String,
    error: Option<PathValidationError>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PathFields {
    gmod: PathField,
    downloads: PathField,
    user_data: PathField,
    temp: PathField,
}

impl PathFields {
    fn from_settings(settings: &Settings) -> Self {
        let field = |path| PathField {
            text: option_path_to_display(path),
            error: None,
        };
        Self {
            gmod: field(settings.backend.gmod.as_deref()),
            downloads: field(settings.backend.downloads.as_deref()),
            user_data: field(settings.backend.user_data.as_deref()),
            temp: field(settings.backend.temp.as_deref()),
        }
    }

    fn get(&self, kind: PathSetting) -> &PathField {
        match kind {
            PathSetting::Gmod => &self.gmod,
            PathSetting::Downloads => &self.downloads,
            PathSetting::UserData => &self.user_data,
            PathSetting::Temp => &self.temp,
        }
    }

    fn get_mut(&mut self, kind: PathSetting) -> &mut PathField {
        match kind {
            PathSetting::Gmod => &mut self.gmod,
            PathSetting::Downloads => &mut self.downloads,
            PathSetting::UserData => &mut self.user_data,
            PathSetting::Temp => &mut self.temp,
        }
    }

    fn text(&self, kind: PathSetting) -> &str {
        &self.get(kind).text
    }

    fn error(&self, kind: PathSetting) -> Option<PathValidationError> {
        self.get(kind).error
    }

    fn set_text(&mut self, kind: PathSetting, value: String) {
        *self.get_mut(kind) = PathField {
            text: value,
            error: None,
        };
    }

    fn set_error(&mut self, kind: PathSetting, error: PathValidationError) {
        self.get_mut(kind).error = Some(error);
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ColorField {
    text: String,
    invalid: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ColorFields {
    neutral: ColorField,
    success: ColorField,
    error: ColorField,
}

impl ColorFields {
    fn from_settings(settings: &Settings) -> Self {
        let field = |kind| ColorField {
            text: format_hex_color(get_color_setting(settings, kind)),
            invalid: false,
        };
        Self {
            neutral: field(ColorSetting::Neutral),
            success: field(ColorSetting::Success),
            error: field(ColorSetting::Error),
        }
    }

    fn get(&self, kind: ColorSetting) -> &ColorField {
        match kind {
            ColorSetting::Neutral => &self.neutral,
            ColorSetting::Success => &self.success,
            ColorSetting::Error => &self.error,
        }
    }

    fn get_mut(&mut self, kind: ColorSetting) -> &mut ColorField {
        match kind {
            ColorSetting::Neutral => &mut self.neutral,
            ColorSetting::Success => &mut self.success,
            ColorSetting::Error => &mut self.error,
        }
    }

    fn text(&self, kind: ColorSetting) -> &str {
        &self.get(kind).text
    }

    fn invalid(&self, kind: ColorSetting) -> bool {
        self.get(kind).invalid
    }

    fn set_invalid_text(&mut self, kind: ColorSetting, value: String) {
        *self.get_mut(kind) = ColorField {
            text: value,
            invalid: true,
        };
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
        PathSetting::Gmod => settings.backend.gmod = path,
        PathSetting::Downloads => settings.backend.downloads = path,
        PathSetting::UserData => settings.backend.user_data = path,
        PathSetting::Temp => settings.backend.temp = path,
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
        ColorSetting::Neutral => settings.backend.color_neutral = rgb,
        ColorSetting::Success => settings.backend.color_success = rgb,
        ColorSetting::Error => settings.backend.color_error = rgb,
    }
}

fn get_color_setting(settings: &Settings, kind: ColorSetting) -> u32 {
    match kind {
        ColorSetting::Neutral => settings.backend.color_neutral,
        ColorSetting::Success => settings.backend.color_success,
        ColorSetting::Error => settings.backend.color_error,
    }
}

fn concrete_preset_for_effective(preset: EffectiveThemePreset) -> ThemePreset {
    match preset {
        EffectiveThemePreset::Dark => ThemePreset::Dark,
        EffectiveThemePreset::Light => ThemePreset::Light,
        EffectiveThemePreset::ClassicSource => ThemePreset::ClassicSource,
    }
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

fn option_path_to_display(path: Option<&Path>) -> String {
    path.map(path_to_display).unwrap_or_default()
}

pub fn accent_inputs_from_settings(settings: &Settings) -> AccentInputs {
    AccentInputs {
        neutral: settings.backend.color_neutral,
        success: settings.backend.color_success,
        error: settings.backend.color_error,
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
        state.settings.ui.theme_preset = ThemePreset::Dark;

        let mutation = state
            .theme_mutation(ThemePreset::Auto)
            .expect("auto selection should produce a mutation");

        assert!(matches!(
            mutation,
            SettingsMutation::Theme {
                preset: ThemePreset::Auto,
                ..
            }
        ));
        assert_eq!(state.theme_preset(), ThemePreset::Auto);
    }

    #[test]
    fn language_default_maps_to_none() {
        let mut state = State::default();
        state.settings.backend.language = Some("fr".to_owned());

        let mutation = state
            .language_mutation(None)
            .expect("default should update explicit language");

        assert_eq!(mutation, SettingsMutation::Language(None));
        assert_eq!(state.settings.backend.language, None);
    }

    #[test]
    fn download_count_format_values_round_trip() {
        let mut state = State::default();

        let mutation = state
            .download_count_format_mutation(DownloadCountFormat::Period)
            .expect("period should be valid");

        assert_eq!(
            mutation,
            SettingsMutation::DownloadCountFormat(DownloadCountFormat::Period)
        );
        assert_eq!(state.download_count_format(), DownloadCountFormat::Period);
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
        assert_eq!(state.settings.backend.titlebar, TitlebarPreference::System);
    }

    #[test]
    fn overwrite_mode_selection_reaches_backend_extraction_settings() {
        let mut state = State::default();

        let mutation = state
            .overwrite_mode_mutation(ExtractionOverwriteMode::Delete)
            .expect("a change from the default should produce a mutation");

        assert_eq!(
            mutation,
            SettingsMutation::OverwriteMode(ExtractionOverwriteMode::Delete)
        );
        assert_eq!(state.overwrite_mode(), ExtractionOverwriteMode::Delete);
        assert_eq!(
            state.overwrite_mode_mutation(ExtractionOverwriteMode::Overwrite),
            Some(SettingsMutation::OverwriteMode(
                ExtractionOverwriteMode::Overwrite
            ))
        );
        assert_eq!(
            state.overwrite_mode_mutation(ExtractionOverwriteMode::Recycle),
            Some(SettingsMutation::OverwriteMode(
                ExtractionOverwriteMode::Recycle
            ))
        );
    }

    #[test]
    fn color_setting_indices_follow_their_declaration_order() {
        for (position, kind) in ColorSetting::ALL.into_iter().enumerate() {
            assert_eq!(kind.index(), position, "{kind:?}");
        }
    }

    #[test]
    fn path_validation_accepts_blank_as_default_override() {
        let mut state = State::default();
        state.settings.backend.downloads = Some(PathBuf::from("/tmp/downloads"));
        state.path_fields = PathFields::from_settings(&state.settings);
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
