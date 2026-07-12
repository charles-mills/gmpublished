use crate::backend::ui_error::UiError;
use std::path::PathBuf;

use super::state::{
    ColorChannel, ColorSetting, PathSetting, PathValidationResult, ResetAction, SelectOption,
    SettingsSnapshot, Tab,
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
    LanguageSelected(SelectOption),
    DownloadCountFormatSelected(SelectOption),
    ThemeSelected(SelectOption),
    OverwriteModeSelected(SelectOption),
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
    SaveCompleted(Result<Box<SettingsSnapshot>, UiError>),
    ResetCompleted(ResetAction, Result<Option<Box<SettingsSnapshot>>, UiError>),
    EscapePressed,
}
