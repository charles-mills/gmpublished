use std::path::PathBuf;

/// A button on a prerequisite panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    /// `steam://open/main` — starts the client, or focuses a running one.
    StartSteam,
    /// The Steam download page, for a machine with no client at all.
    GetSteam,
    /// Connect now rather than waiting for the next background attempt.
    RetrySteam,
    /// `steam://install/4000`.
    InstallGame,
    /// Native folder picker, validated the same way Settings validates it.
    LocateGame,
    /// Re-run discovery across the Steam libraries.
    SearchGame,
}

/// Facts the prerequisite feature reacts to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    Activated(Action),
    /// A folder picker closed; `None` means the user cancelled.
    GameFolderPicked(Option<PathBuf>),
    /// Discovery finished, carrying whatever path it resolved.
    GameSearchCompleted(Option<PathBuf>),
}
