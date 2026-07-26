use std::path::{Path, PathBuf};

use super::{
    App, LibraryRefreshReason, RootMessage, Task, UiError, flatten_blocking_ui_result,
    prerequisites, steam_session,
};

/// Opens (or focuses) the Steam client.
const STEAM_OPEN_URL: &str = "steam://open/main";
/// Steam's own install page, for a machine with no client at all.
const STEAM_DOWNLOAD_URL: &str = "https://store.steampowered.com/about/";
/// Opens the client's install dialog for Garry's Mod.
const GMOD_INSTALL_URL: &str = "steam://install/4000";

impl App {
    pub(super) fn apply_prerequisites_message(
        &mut self,
        message: prerequisites::Message,
    ) -> Task<RootMessage> {
        match message {
            prerequisites::Message::Activated(action) => self.prerequisite_action_task(action),
            prerequisites::Message::GameFolderPicked(selected) => {
                selected.map_or_else(Task::none, |path| self.game_folder_chosen_task(path))
            }
            prerequisites::Message::GameSearchCompleted(resolved) => {
                self.game_search_completed_task(resolved.as_deref())
            }
        }
    }

    pub(super) fn prerequisite_action_task(
        &mut self,
        action: prerequisites::Action,
    ) -> Task<RootMessage> {
        match action {
            prerequisites::Action::StartSteam => self.open_url_task(STEAM_OPEN_URL.to_owned()),
            prerequisites::Action::GetSteam => self.open_url_task(STEAM_DOWNLOAD_URL.to_owned()),
            prerequisites::Action::RetrySteam => self.steam_retry_now_task(),
            prerequisites::Action::InstallGame => self.open_url_task(GMOD_INSTALL_URL.to_owned()),
            prerequisites::Action::LocateGame => self.game_folder_picker_task(),
            prerequisites::Action::SearchGame => self.game_search_task(),
        }
    }

    /// Connects now instead of waiting for the next background attempt. The
    /// session moves to Connecting first so the panel spins the button that
    /// was pressed rather than replacing itself.
    fn steam_retry_now_task(&mut self) -> Task<RootMessage> {
        if self.ctx.steam_connected()
            || self.state.steam_session.status() == steam_session::ConnectionStatus::Connecting
        {
            return Task::none();
        }

        let connecting = self.apply_steam_session_message(steam_session::Message::ConnectionEvent(
            steam_session::ConnectionEvent::Connecting,
        ));
        Task::batch([connecting, self.steam_connect_task()])
    }

    fn game_folder_picker_task(&self) -> Task<RootMessage> {
        let (_, resolved) = self.ctx.game_paths();
        let directory = resolved.unwrap_or_else(|| PathBuf::from("."));
        let title = self.state.i18n.tr("native-dialog-select-settings-folder");
        Task::future(async move {
            let selected = rfd::AsyncFileDialog::new()
                .set_title(title)
                .set_directory(directory)
                .pick_folder()
                .await
                .map(|folder| folder.path().to_path_buf());
            RootMessage::Prerequisites(prerequisites::Message::GameFolderPicked(selected))
        })
    }

    /// Persists the chosen folder exactly as Settings does, so an invalid
    /// pick is rejected by the one validator that already knows what a
    /// Garry's Mod folder looks like.
    fn game_folder_chosen_task(&mut self, path: PathBuf) -> Task<RootMessage> {
        if !gmpublished_backend::appdata::validate_gmod(path.clone()) {
            // Nothing was persisted, so there is nothing to re-evaluate: the
            // panel stays as it was rather than reporting a second, different
            // failure for what was really a mis-click.
            return Task::none();
        }

        self.state.prerequisites.begin_game_search();
        self.ctx
            .run_blocking("prerequisites-set-gmod-dir", move |app| {
                app.update_settings_snapshot(|settings| {
                    settings.gmod = Some(path);
                })
                .map(|()| app.paths().gmod_dir)
                .map_err(|error| UiError::from(&error))
            })
            .map(|result| {
                let resolved = flatten_blocking_ui_result(result).unwrap_or_default();
                RootMessage::Prerequisites(prerequisites::Message::GameSearchCompleted(resolved))
            })
    }

    fn game_search_task(&mut self) -> Task<RootMessage> {
        self.state.prerequisites.begin_game_search();
        self.ctx
            .run_blocking("prerequisites-discover-gmod-dir", |app| {
                app.rediscover_gmod_dir()
            })
            .map(|result| {
                RootMessage::Prerequisites(prerequisites::Message::GameSearchCompleted(
                    result.ok().flatten(),
                ))
            })
    }

    fn game_search_completed_task(&mut self, resolved: Option<&Path>) -> Task<RootMessage> {
        let (configured, _) = self.ctx.game_paths();
        self.state
            .prerequisites
            .set_game(prerequisites::GameStatus::from_paths(
                configured.as_deref(),
                resolved,
            ));
        if resolved.is_none() {
            return Task::none();
        }

        // A newly resolved folder is a library this session has never scanned.
        Task::done(RootMessage::LibraryRefreshRequested(
            LibraryRefreshReason::SettingsChanged,
        ))
    }

    /// Re-reads the prerequisite facts from whatever settings snapshot just
    /// landed. Cheap: both halves are already-cached values.
    pub(super) fn sync_game_prerequisite(&mut self) {
        let (configured, resolved) = self.ctx.game_paths();
        self.state
            .prerequisites
            .set_game(prerequisites::GameStatus::from_paths(
                configured.as_deref(),
                resolved.as_deref(),
            ));
    }

    /// One-shot probe at startup. Reads the Steam directory layout, so it
    /// answers with the client closed — which is exactly the case that has to
    /// tell "not running" apart from "not installed".
    pub(super) fn steam_installed_probe_task(&self) -> Task<RootMessage> {
        self.ctx
            .run_blocking("prerequisites-steam-installed", |_app| {
                gmpublished_backend::appdata::steam_client_installed()
            })
            .map(|result| RootMessage::SteamClientInstalledProbed(result.unwrap_or(true)))
    }
}
