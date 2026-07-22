use std::path::PathBuf;

use super::model::{DestinationKind, SettingsSnapshot};
use super::state::OpenContext;
use crate::bridge::ui_error::UiError;

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// The chooser should open, reset from fresh settings, with the given
    /// caller context (confirm label, extracted name, forced create-folder).
    OpenRequested {
        snapshot: Box<SettingsSnapshot>,
        context: OpenContext,
    },
    /// A destination tile was clicked (click on the active tile deselects).
    KindToggled(DestinationKind),
    PathInputEdited(String),
    PathAccepted,
    BrowseCompleted(Option<PathBuf>),
    /// The create-folder checkbox toggled; persists immediately.
    CreateFolderToggled(bool),
    CreateFolderSaved(Result<Box<SettingsSnapshot>, UiError>),
    HistorySelected(PathBuf),
    ConfirmRequested,
    /// Persistence finished; success closes the overlay.
    SaveCompleted(Result<Box<SettingsSnapshot>, UiError>),
    CloseFinished,
}
