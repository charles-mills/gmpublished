use gmpublished_backend::error_keys as keys;

use super::{
    App, BackendRuntimeEvent, LibraryRefreshReason, RootMessage, RouteLifecycle, Task, UiError,
    backend_runtime_action_message, flatten_blocking_ui_result, installed_addons, my_workshop,
    prerequisites, search, settings, shell, steam_session,
};
use crate::generation::Generation;

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
        // Prerequisite panels need the edge, not just the level: a Connecting
        // that follows a failure is a retry, and must not read as a first
        // attempt.
        let reconnected = self
            .state
            .prerequisites
            .observe_steam(self.state.steam_session.status());
        let session_task = self.run_steam_session_effects(effects);
        let shell_status_task = self.sync_shell_steam_status();
        let (reload_task, installed_retry_task) = if reconnected {
            (
                self.reload_steam_route_after_reconnect(),
                // Installed Addons is not a Steam-gated route, so the reload
                // above skips it — but its metadata lookups are the ones most
                // likely to have failed against a Steam that was still coming
                // up, and nothing else re-asks on their behalf.
                self.apply_installed_addons_message(installed_addons::Message::SteamReconnected),
            )
        } else {
            (Task::none(), Task::none())
        };
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
            failure.map_or_else(Task::none, |error| self.fail_pending_steam_retry(&error));
        Task::batch([
            session_task,
            shell_status_task,
            reload_task,
            installed_retry_task,
            retry_task,
            shell_identity_task,
            failure_task,
        ])
    }

    /// Steam came up after a route had already given up on it. The prerequisite
    /// panel hides that route's stale failure while the connection is down, so
    /// the moment it clears, the error underneath would be the first thing the
    /// user sees — reporting an outage that is already over.
    ///
    /// Re-entering is exactly what leaving the route and coming back does —
    /// the manual workaround a user would otherwise have to find.
    fn reload_steam_route_after_reconnect(&mut self) -> Task<RootMessage> {
        let route = self.state.shell.route();
        if !prerequisites::requires_steam(route) {
            return Task::none();
        }

        // The features' own enter guards decide whether this actually refetches:
        // a route holding rows keeps them, and only an idle or failed one asks
        // again.
        self.route_lifecycle_task(route, RouteLifecycle::Entered)
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
                if self.appdata_snapshot_in_flight {
                    self.pending_appdata_snapshot = Some(snapshot);
                    Task::none()
                } else {
                    self.start_appdata_snapshot_task(snapshot)
                }
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

    pub(super) fn start_appdata_snapshot_task(
        &mut self,
        snapshot: Box<gmpublished_backend::AppDataSnapshot>,
    ) -> Task<RootMessage> {
        debug_assert!(!self.appdata_snapshot_in_flight);
        self.appdata_snapshot_in_flight = true;
        let system_scheme = self.state.system_scheme;
        self.ctx
            .run_blocking("apply-appdata-snapshot", move |services| {
                let (settings, paths) = services.config().apply_appdata_snapshot(*snapshot);
                Box::new(settings::SettingsSnapshot::new(
                    settings,
                    paths,
                    system_scheme,
                ))
            })
            .map(RootMessage::AppDataSnapshotApplied)
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
            .run_blocking("steam-connect", |app| {
                steam_session::connect_context_for_operation(app.workshop())
            })
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

    pub(super) fn steam_identity_task(&self, generation: Generation) -> Task<RootMessage> {
        self.ctx
            .run_blocking("steam-current-user", |app| {
                app.workshop()
                    .current_user()
                    .map(steam_session::SteamIdentity::from_user)
            })
            .map(move |result| {
                RootMessage::SteamSession(steam_session::Message::IdentityFetched(
                    generation,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn fail_pending_steam_retry(&mut self, error: &UiError) -> Task<RootMessage> {
        let retries = self.state.steam_session.take_pending_retries();
        Task::batch(
            retries
                .into_iter()
                .map(|retry| Task::done(retry.fail_message(error.clone()))),
        )
    }

    pub(super) fn retry_pending_steam_operation(&mut self) -> Task<RootMessage> {
        let retries = self.state.steam_session.take_pending_retries();
        let tasks = retries
            .into_iter()
            .map(|retry| retry.retry_message(self))
            .collect::<Vec<_>>();
        Task::batch(tasks)
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
            Self::InstalledMetadataRefresh {
                generation,
                item_ids,
            } => RootMessage::InstalledAddons(installed_addons::Message::MetadataRefreshCompleted(
                generation,
                item_ids,
                Err(error),
            )),
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
