use crate::bridge::ui_error::ResultExt as _;
use std::path::PathBuf;

use super::{App, LibraryRefreshReason, RootMessage, Task, UiError, prerequisites, steam_session};

const STEAM_OPEN_URL: &str = "steam://open/main";
const STEAM_DOWNLOAD_URL: &str = "https://store.steampowered.com/about/";
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
                self.game_search_completed_task(resolved)
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

    /// Enter Connecting before retrying so the panel keeps its content and spins the button.
    fn steam_retry_now_task(&mut self) -> Task<RootMessage> {
        if self.environment.ctx.steam_connected()
            || self.state.features.steam_session.status()
                == steam_session::ConnectionStatus::Connecting
        {
            return Task::none();
        }

        let connecting = self.apply_steam_session_message(steam_session::Message::ConnectionEvent(
            steam_session::ConnectionEvent::Connecting,
        ));
        Task::batch([connecting, self.steam_connect_task()])
    }

    fn game_folder_picker_task(&self) -> Task<RootMessage> {
        let (_, resolved) = self.environment.ctx.game_paths();
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

    fn game_folder_chosen_task(&mut self, path: PathBuf) -> Task<RootMessage> {
        if !gmpublished_backend::validate_gmod(path.clone()) {
            return Task::none();
        }

        self.state.features.prerequisites.begin_game_search();
        self.environment
            .ctx
            .run_blocking_ui("prerequisites-set-gmod-dir", move |app| {
                app.config()
                    .update_settings_snapshot(|settings| {
                        settings.backend.gmod = Some(path);
                    })
                    .map(|()| app.config().paths().gmod_dir)
                    .ui_err()
            })
            .map(|resolved| {
                RootMessage::Prerequisites(prerequisites::Message::GameSearchCompleted(resolved))
            })
    }

    fn game_search_task(&mut self) -> Task<RootMessage> {
        self.state.features.prerequisites.begin_game_search();
        self.environment
            .ctx
            .run_blocking_ui("prerequisites-discover-gmod-dir", |app| {
                Ok(app.config().rediscover_gmod_dir())
            })
            .map(|resolved| {
                RootMessage::Prerequisites(prerequisites::Message::GameSearchCompleted(resolved))
            })
    }

    fn game_search_completed_task(
        &mut self,
        resolved: Result<Option<PathBuf>, UiError>,
    ) -> Task<RootMessage> {
        // A failed search does not establish that the folder is missing. Keep the previous status.
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                log::warn!("Garry's Mod folder search could not run: {error}");
                return Task::none();
            }
        };
        let resolved = resolved.as_deref();
        let (configured, _) = self.environment.ctx.game_paths();
        self.state
            .features
            .prerequisites
            .set_game(prerequisites::GameStatus::from_paths(
                configured.as_deref(),
                resolved,
            ));
        if resolved.is_none() {
            return Task::none();
        }

        Task::done(RootMessage::LibraryRefreshRequested(
            LibraryRefreshReason::SettingsChanged,
        ))
    }

    pub(super) fn sync_game_prerequisite(&mut self) {
        let (configured, resolved) = self.environment.ctx.game_paths();
        self.state
            .features
            .prerequisites
            .set_game(prerequisites::GameStatus::from_paths(
                configured.as_deref(),
                resolved.as_deref(),
            ));
    }

    /// Probe the installation without requiring the Steam client to be running.
    pub(super) fn steam_installed_probe_task(&self) -> Task<RootMessage> {
        self.environment
            .ctx
            .run_blocking("prerequisites-steam-installed", |_app| {
                gmpublished_backend::steam_client_installed()
            })
            .map(|result| RootMessage::SteamClientInstalledProbed(result.unwrap_or(true)))
    }
}
