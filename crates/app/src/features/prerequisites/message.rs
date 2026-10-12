use std::path::PathBuf;

use crate::bridge::ui_error::UiError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    StartSteam,
    GetSteam,
    RetrySteam,
    InstallGame,
    LocateGame,
    SearchGame,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    Activated(Action),
    GameFolderPicked(Option<PathBuf>),
    /// Discovery finished, carrying whatever path it resolved.
    /// The search's outcome, not just its result: `Ok(None)` is "no Garry's
    /// Mod folder found", which is a different thing from never having looked.
    GameSearchCompleted(Result<Option<PathBuf>, UiError>),
}
