use gmpublished_backend::error_key::keys;

use super::{
    App, BackendRuntimeEvent, LibraryRefreshReason, RootMessage, Task, UiError,
    backend_runtime_action_message, flatten_blocking_ui_result, installed_addons, my_workshop,
    search, settings, shell, steam_session,
};

impl App {
    pub(super) fn apply_steam_session_message(
        &mut self,
        message: steam_session::Message,
    ) -> Task<RootMessage> {
        let failure = match &message {
            steam_session::Message::ConnectionAttemptCompleted(attempt) => attempt.error().cloned(),
            _ => None,
        };
        let identity_completed = matches!(message, steam_session::Message::IdentityFetched(_, _));

        let effects = steam_session::update(&mut self.state.steam_session, message);
        let session_task = self.run_steam_session_effects(effects);
        let shell_status_task = self.sync_shell_steam_status();
        let retry_task = if self.state.steam_session.status().connected() {
            self.retry_pending_steam_operation()
        } else {
            Task::none()
        };
        let shell_identity_task = if identity_completed {
            self.sync_shell_steam_identity()
        } else {
            Task::none()
        };
        let failure_task =
            failure.map_or_else(Task::none, |error| self.fail_pending_steam_retry(error));
        Task::batch([
            session_task,
            shell_status_task,
            retry_task,
            shell_identity_task,
            failure_task,
        ])
    }

    fn run_steam_session_effects(
        &mut self,
        effects: Vec<steam_session::Effect>,
    ) -> Task<RootMessage> {
        self.batch_effects(effects, Self::run_steam_session_effect)
    }

    fn run_steam_session_effect(&mut self, effect: steam_session::Effect) -> Task<RootMessage> {
        match effect {
            steam_session::Effect::IdentityFetchRequested(generation) => {
                self.steam_identity_task(generation)
            }
        }
    }

    pub(super) fn defer_steam_operation(
        &mut self,
        retry: steam_session::PendingRetry,
    ) -> Option<Task<RootMessage>> {
        if self.ctx.steam_connected() {
            return None;
        }

        let set_retry_effects = steam_session::update(
            &mut self.state.steam_session,
            steam_session::Message::PendingRetrySet(retry),
        );
        let set_retry = self.run_steam_session_effects(set_retry_effects);
        if self.state.steam_session.status() == steam_session::ConnectionStatus::Connecting {
            return Some(set_retry);
        }

        let connecting_effects = steam_session::update(
            &mut self.state.steam_session,
            steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connecting),
        );
        let connecting = self.run_steam_session_effects(connecting_effects);
        Some(Task::batch([
            set_retry,
            connecting,
            self.steam_connect_task(),
        ]))
    }

    pub(super) fn backend_event_task(&mut self, event: BackendRuntimeEvent) -> Task<RootMessage> {
        match event {
            BackendRuntimeEvent::SteamConnected => self.update(RootMessage::SteamSession(
                steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connected),
            )),
            BackendRuntimeEvent::SteamDisconnected => self.update(RootMessage::SteamSession(
                steam_session::Message::ConnectionEvent(
                    steam_session::ConnectionEvent::Disconnected,
                ),
            )),
            BackendRuntimeEvent::AppDataUpdated(snapshot) => {
                let (settings, paths) = self.ctx.apply_appdata_snapshot(*snapshot);
                let snapshot =
                    settings::SettingsSnapshot::new(settings, paths, self.state.system_scheme);
                self.apply_settings_snapshot_runtime(&snapshot)
            }
            BackendRuntimeEvent::InstalledAddonsRefreshed => Task::done(
                RootMessage::LibraryRefreshRequested(LibraryRefreshReason::SettingsChanged),
            ),
            BackendRuntimeEvent::DownloadStarted { .. }
            | BackendRuntimeEvent::ExtractionStarted { .. }
            | BackendRuntimeEvent::Transaction(_) => Task::batch(
                self.ctx
                    .handle_backend_runtime_event(&event)
                    .into_actions()
                    .into_iter()
                    .map(|action| Task::done(backend_runtime_action_message(action))),
            ),
        }
    }

    /// Warms the Steam connection once per session after the launch-critical
    /// path (first frame + library snapshot) is done, so the first
    /// Steam-backed click skips SteamAPI init + connect. Rides the same
    /// machinery as a deferred operation's lazy connect — a failed attempt
    /// is silent and measurement modes ignore it — just without a retry.
    pub(super) fn warm_steam_connect_task(&mut self) -> Task<RootMessage> {
        if !self.state.steam_session.take_warm_connect_cue()
            || self.ctx.steam_connected()
            || self.state.steam_session.status() == steam_session::ConnectionStatus::Connecting
        {
            return Task::none();
        }

        let connecting_effects = steam_session::update(
            &mut self.state.steam_session,
            steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connecting),
        );
        Task::batch([
            self.run_steam_session_effects(connecting_effects),
            self.steam_connect_task(),
        ])
    }

    pub(super) fn steam_connect_task(&self) -> Task<RootMessage> {
        self.ctx
            .run_blocking(
                "steam-connect",
                steam_session::connect_context_for_operation,
            )
            .map(|result| {
                let attempt = match result {
                    Ok(attempt) => attempt,
                    Err(error) => steam_session::ConnectionAttempt::unavailable(UiError::detailed(
                        keys::STEAM_ERROR,
                        Some(error.to_string()),
                    )),
                };
                RootMessage::SteamSession(steam_session::Message::ConnectionAttemptCompleted(
                    attempt,
                ))
            })
    }

    pub(super) fn steam_identity_task(&self, generation: u64) -> Task<RootMessage> {
        self.ctx
            .run_blocking("steam-current-user", |app| {
                app.current_steam_user()
                    .map(steam_session::SteamIdentity::from_user)
            })
            .map(move |result| {
                RootMessage::SteamSession(steam_session::Message::IdentityFetched(
                    generation,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn fail_pending_steam_retry(&mut self, error: UiError) -> Task<RootMessage> {
        self.state
            .steam_session
            .take_pending_retry()
            .map_or_else(Task::none, |retry| Task::done(retry.fail_message(error)))
    }

    pub(super) fn retry_pending_steam_operation(&mut self) -> Task<RootMessage> {
        let Some(retry) = self.state.steam_session.take_pending_retry() else {
            return Task::none();
        };
        retry.retry_message(self)
    }

    pub(super) fn sync_shell_steam_status(&mut self) -> Task<RootMessage> {
        self.apply_shell_message(shell::Message::SteamStatusChanged(
            self.state.steam_session.status(),
        ))
    }

    pub(super) fn sync_shell_steam_identity(&mut self) -> Task<RootMessage> {
        let identity = self.state.steam_session.identity().cloned();
        self.apply_shell_message(shell::Message::SteamIdentityChanged(identity))
    }
}

/// Dispatch for a deferred Steam-backed operation, kept next to
/// `defer_steam_operation`'s call sites rather than in `steam_session` itself:
/// both outcomes route through `RootMessage`, which is an app-level type the
/// `steam_session` feature module does not otherwise depend on.
impl steam_session::PendingRetry {
    /// Resumes the operation now that Steam has connected.
    fn retry_message(self, app: &mut App) -> Task<RootMessage> {
        match self {
            Self::MyWorkshopPage { generation, page } => {
                app.my_workshop_page_worker_task(generation, page)
            }
            Self::MyWorkshopStats { generation, pages } => {
                app.my_workshop_stats_refresh_worker_task(generation, pages)
            }
            Self::InstalledMetadata {
                generation,
                item_ids,
            } => app.run_installed_addons_effect(installed_addons::Effect::MetadataRequested {
                generation,
                item_ids,
            }),
            Self::InstalledMetadataRefresh {
                generation,
                item_ids,
            } => app.run_installed_addons_effect(
                installed_addons::Effect::MetadataRefreshRequested {
                    generation,
                    item_ids,
                },
            ),
            Self::SearchMetadataRefresh {
                generation,
                item_ids,
            } => app.run_search_effect(search::Effect::MetadataRefreshRequested {
                generation,
                item_ids,
            }),
        }
    }

    /// The message this deferred operation resolves to when the connection
    /// attempt itself failed, so the caller sees the same terminal shape it
    /// would have gotten had Steam simply refused the request outright.
    fn fail_message(self, error: UiError) -> RootMessage {
        match self {
            Self::MyWorkshopPage { generation, page } => RootMessage::MyWorkshop(
                my_workshop::Message::PageCompleted(generation, page, Err(error)),
            ),
            Self::MyWorkshopStats { generation, .. } => RootMessage::MyWorkshop(
                my_workshop::Message::StatsRefreshCompleted(generation, Err(error)),
            ),
            Self::InstalledMetadata {
                generation,
                item_ids,
            } => RootMessage::InstalledAddons(installed_addons::Message::MetadataCompleted(
                generation,
                item_ids,
                Err(error),
            )),
            Self::InstalledMetadataRefresh { generation, .. } => RootMessage::InstalledAddons(
                installed_addons::Message::MetadataRefreshCompleted(generation, Err(error)),
            ),
            Self::SearchMetadataRefresh {
                generation,
                item_ids,
            } => RootMessage::Search(search::Message::MetadataRefreshCompleted(
                generation,
                item_ids,
                Err(error),
            )),
        }
    }
}
