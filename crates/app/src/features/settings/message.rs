use crate::bridge::ui_error::UiError;
use crate::bridge::{DownloadCountFormat, ExtractionOverwriteMode, ThemePreset};
use crate::generation::Generation;
use std::path::PathBuf;

use super::state::{
    ColorChannel, ColorSetting, PathSetting, PathValidationResult, ResetAction, SettingsSnapshot,
    Tab,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    OpenRequested(Box<SettingsSnapshot>),
    CloseRequested,
    CloseFinished,
    TabSelected(Tab),
    SoundsToggled(bool),
    PlayGifsByDefaultToggled(bool),
    #[cfg(target_os = "macos")]
    SystemTitlebarToggled(bool),
    LanguageSelected(Option<String>),
    DownloadCountFormatSelected(DownloadCountFormat),
    ThemeSelected(ThemePreset),
    OverwriteModeSelected(ExtractionOverwriteMode),
    PathEdited(PathSetting, String),
    PathAccepted(PathSetting),
    PathBrowseRequested(PathSetting),
    PathBrowseCompleted(PathSetting, Option<PathBuf>),
    PathValidationCompleted(PathValidationResult),
    ColorEdited(ColorSetting, String),
    ColorPickerToggled(ColorSetting),
    ColorPickerChannelChanged(ColorSetting, ColorChannel, f32),
    ColorPickerApplied(ColorSetting),
    ColorPickerCancelled,
    ResetRequested(ResetAction),
    ResetCancelled,
    ResetConfirmed,
    SaveCompleted(Generation, Result<Box<SettingsSnapshot>, UiError>),
    ResetCompleted(ResetAction, Result<Option<Box<SettingsSnapshot>>, UiError>),
    EscapePressed,
}
