use std::path::PathBuf;

use super::model::{DestinationKind, SettingsSnapshot};
use super::state::OpenContext;
use crate::bridge::ui_error::UiError;
use crate::generation::Generation;

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    OpenRequested {
        snapshot: Box<SettingsSnapshot>,
        context: OpenContext,
    },
    KindToggled(DestinationKind),
    PathInputEdited(String),
    PathAccepted,
    CustomPathValidated {
        generation: Generation,
        path: PathBuf,
        valid: bool,
    },
    BrowseCompleted(Option<PathBuf>),
    CreateFolderToggled(bool),
    CreateFolderSaved(Result<Box<SettingsSnapshot>, UiError>),
    HistorySelected(PathBuf),
    ConfirmRequested,
    SaveCompleted(Result<Box<SettingsSnapshot>, UiError>),
    CloseFinished,
}
