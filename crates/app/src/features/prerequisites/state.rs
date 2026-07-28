use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::features::shell::Route;
use crate::features::steam_session::ConnectionStatus;

/// Which prerequisite a route depends on. A route depends on exactly one:
/// the Downloader needs Steam and merely prefers a Garry's Mod folder for its
/// default destination, and Installed Addons needs the folder while Steam only
/// enriches what it found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Requirement {
    Steam,
    Game,
}

impl Requirement {
    pub(super) const fn of(route: Route) -> Self {
        match route {
            Route::MyWorkshop | Route::Downloader => Self::Steam,
            Route::InstalledAddons | Route::SizeAnalyzer => Self::Game,
        }
    }
}

/// Where Garry's Mod discovery currently stands.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum GameStatus {
    /// Discovery hasn't reported yet. Renders as searching: on a cold start
    /// the answer is moments away, and guessing "missing" would flash a
    /// call to action at someone whose game is about to be found.
    #[default]
    Unknown,
    Searching,
    Found,
    /// No configured path and nothing discovered.
    Missing,
    /// A path was configured and has since stopped resolving — moved library,
    /// unmounted drive, uninstalled game.
    Broken(PathBuf),
}

impl GameStatus {
    /// Classifies from the pair the settings snapshot already carries: the
    /// path the user chose, and the path that actually resolved.
    pub fn from_paths(configured: Option<&Path>, resolved: Option<&Path>) -> Self {
        match (configured, resolved) {
            (_, Some(_)) => Self::Found,
            (Some(configured), None) => Self::Broken(configured.to_path_buf()),
            (None, None) => Self::Missing,
        }
    }
}

/// What a route shows instead of its content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Blocker {
    /// First connect of the session, or a reconnect with no prior failure.
    SteamConnecting,
    SteamNotRunning {
        /// A retry is in flight: the panel holds still and only the button
        /// it belongs to changes.
        retrying: bool,
    },
    SteamNotInstalled,
    GameSearching,
    GameMissing {
        /// `steam://install/4000` only reaches an installed, running client;
        /// without one the panel drops the button rather than offering a
        /// control that does nothing.
        can_install: bool,
    },
    GameBroken {
        path: PathBuf,
    },
}

/// Prerequisite facts the shell tracks for every route.
#[derive(Debug)]
pub struct State {
    game: GameStatus,
    steam_installed: bool,
    /// Set the first time a connection attempt fails. After that a
    /// `Connecting` status is a retry of a known-bad connection, not a
    /// first attempt, so the panel keeps its copy instead of flipping to
    /// the neutral connecting state.
    steam_failed: bool,
    /// Last observed connection level, so `observe_steam` can report the
    /// disconnected → connected edge rather than the level.
    steam_connected: bool,
    /// Anchored on the first tick after a busy panel appears, so the spinner
    /// phase is a pure function of the clock the shell already runs.
    spinner_anchor: Option<Instant>,
    spinner_now: Option<Instant>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            game: GameStatus::default(),
            // Assume present until discovery says otherwise: the alternative
            // flashes "Steam isn't installed" during the startup window on
            // every machine that does have it.
            steam_installed: true,
            steam_failed: false,
            steam_connected: false,
            spinner_anchor: None,
            spinner_now: None,
        }
    }
}

impl State {
    pub(crate) fn set_game(&mut self, game: GameStatus) {
        self.game = game;
    }

    pub(crate) fn begin_game_search(&mut self) {
        self.game = GameStatus::Searching;
    }

    pub(crate) const fn set_steam_installed(&mut self, installed: bool) {
        self.steam_installed = installed;
    }

    /// Records connection status edges so a retry can be told from a first
    /// attempt. Connecting deliberately does not clear the failure flag: it is
    /// set again only by a fresh failure and cleared by an actual connection.
    ///
    /// Returns true on the disconnected → connected edge only. Routes that
    /// gave up while Steam was down are still holding the error that Steam
    /// handed them, and nothing else in the app re-asks on their behalf.
    pub(crate) const fn observe_steam(&mut self, status: ConnectionStatus) -> bool {
        match status {
            ConnectionStatus::Unavailable => {
                self.steam_failed = true;
                self.steam_connected = false;
                false
            }
            ConnectionStatus::Disconnected => {
                self.steam_connected = false;
                false
            }
            ConnectionStatus::Connected => {
                self.steam_failed = false;
                let reconnected = !self.steam_connected;
                self.steam_connected = true;
                reconnected
            }
            ConnectionStatus::Connecting => false,
        }
    }

    /// True while the route panel should keep animating its spinner.
    pub(crate) fn animating(&self, route: Route, steam: ConnectionStatus) -> bool {
        matches!(
            blocker_for(route, steam, self),
            Some(Blocker::SteamConnecting | Blocker::GameSearching)
        )
    }

    pub(crate) fn tick(&mut self, now: Instant) {
        self.spinner_anchor.get_or_insert(now);
        self.spinner_now = Some(now);
    }

    pub(crate) fn spinner_elapsed(&self) -> f32 {
        match (self.spinner_anchor, self.spinner_now) {
            (Some(anchor), Some(now)) => now.saturating_duration_since(anchor).as_secs_f32(),
            _ => 0.0,
        }
    }
}

/// The single decision this module exists for: what, if anything, stands
/// between this route and its content.
pub fn blocker_for(route: Route, steam: ConnectionStatus, state: &State) -> Option<Blocker> {
    match Requirement::of(route) {
        Requirement::Steam => steam_blocker(steam, state),
        Requirement::Game => game_blocker(steam, state),
    }
}

fn steam_blocker(steam: ConnectionStatus, state: &State) -> Option<Blocker> {
    match steam {
        ConnectionStatus::Connected => None,
        // Disconnected is the pre-attempt level, not a verdict: the shell
        // warm-connects at startup, so treat it as connecting until an
        // attempt actually resolves.
        ConnectionStatus::Disconnected | ConnectionStatus::Connecting => {
            if state.steam_failed {
                Some(Blocker::SteamNotRunning {
                    retrying: steam == ConnectionStatus::Connecting,
                })
            } else {
                Some(Blocker::SteamConnecting)
            }
        }
        ConnectionStatus::Unavailable => {
            if state.steam_installed {
                Some(Blocker::SteamNotRunning { retrying: false })
            } else {
                Some(Blocker::SteamNotInstalled)
            }
        }
    }
}

fn game_blocker(steam: ConnectionStatus, state: &State) -> Option<Blocker> {
    match &state.game {
        GameStatus::Found => None,
        GameStatus::Unknown | GameStatus::Searching => Some(Blocker::GameSearching),
        GameStatus::Missing => Some(Blocker::GameMissing {
            can_install: steam.connected(),
        }),
        GameStatus::Broken(path) => Some(Blocker::GameBroken { path: path.clone() }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Blocker, ConnectionStatus, GameStatus, Route, State, blocker_for};

    fn connected_steam() -> State {
        let mut state = State::default();
        state.set_game(GameStatus::Found);
        state
    }

    #[test]
    fn game_routes_ignore_steam_and_steam_routes_ignore_the_game() {
        let mut state = State::default();
        state.set_game(GameStatus::Found);

        // Steam down, game present: the local routes carry on.
        assert_eq!(
            blocker_for(
                Route::InstalledAddons,
                ConnectionStatus::Unavailable,
                &state
            ),
            None
        );
        assert_eq!(
            blocker_for(Route::SizeAnalyzer, ConnectionStatus::Unavailable, &state),
            None
        );

        // Steam up, game absent: the Steam routes carry on.
        state.set_game(GameStatus::Missing);
        assert_eq!(
            blocker_for(Route::MyWorkshop, ConnectionStatus::Connected, &state),
            None
        );
        assert_eq!(
            blocker_for(Route::Downloader, ConnectionStatus::Connected, &state),
            None
        );
    }

    /// The reported setup: Steam running and signed in, no game installed.
    #[test]
    fn steam_running_without_the_game_blocks_only_the_local_routes() {
        let mut state = connected_steam();
        state.set_game(GameStatus::Missing);

        assert_eq!(
            blocker_for(Route::MyWorkshop, ConnectionStatus::Connected, &state),
            None
        );
        assert_eq!(
            blocker_for(Route::InstalledAddons, ConnectionStatus::Connected, &state),
            Some(Blocker::GameMissing { can_install: true })
        );
        assert_eq!(
            blocker_for(Route::SizeAnalyzer, ConnectionStatus::Connected, &state),
            Some(Blocker::GameMissing { can_install: true })
        );
    }

    #[test]
    fn install_is_only_offered_while_steam_can_receive_the_request() {
        let mut state = State::default();
        state.set_game(GameStatus::Missing);

        assert_eq!(
            blocker_for(Route::SizeAnalyzer, ConnectionStatus::Unavailable, &state),
            Some(Blocker::GameMissing { can_install: false })
        );
    }

    #[test]
    fn a_first_connect_never_shows_the_failure_copy() {
        let mut state = connected_steam();

        assert_eq!(
            blocker_for(Route::MyWorkshop, ConnectionStatus::Disconnected, &state),
            Some(Blocker::SteamConnecting)
        );
        assert_eq!(
            blocker_for(Route::MyWorkshop, ConnectionStatus::Connecting, &state),
            Some(Blocker::SteamConnecting)
        );

        // Once an attempt has failed, a later Connecting is a retry of a
        // known-bad connection: keep the copy, spin the button.
        state.observe_steam(ConnectionStatus::Unavailable);
        assert_eq!(
            blocker_for(Route::MyWorkshop, ConnectionStatus::Connecting, &state),
            Some(Blocker::SteamNotRunning { retrying: true })
        );
        assert_eq!(
            blocker_for(Route::MyWorkshop, ConnectionStatus::Unavailable, &state),
            Some(Blocker::SteamNotRunning { retrying: false })
        );
    }

    #[test]
    fn connecting_after_a_success_is_a_first_attempt_again() {
        let mut state = connected_steam();
        state.observe_steam(ConnectionStatus::Unavailable);
        state.observe_steam(ConnectionStatus::Connected);

        assert_eq!(
            blocker_for(Route::MyWorkshop, ConnectionStatus::Connecting, &state),
            Some(Blocker::SteamConnecting)
        );
    }

    #[test]
    fn a_missing_steam_client_replaces_the_start_prompt() {
        let mut state = connected_steam();
        state.set_steam_installed(false);

        assert_eq!(
            blocker_for(Route::Downloader, ConnectionStatus::Unavailable, &state),
            Some(Blocker::SteamNotInstalled)
        );
    }

    #[test]
    fn unresolved_discovery_reads_as_searching_not_missing() {
        let state = State::default();

        assert_eq!(
            blocker_for(Route::InstalledAddons, ConnectionStatus::Connected, &state),
            Some(Blocker::GameSearching)
        );
    }

    #[test]
    fn a_configured_path_that_stopped_resolving_is_broken_not_missing() {
        let configured = PathBuf::from("/mnt/games/SteamLibrary/steamapps/common/GarrysMod");

        assert_eq!(
            GameStatus::from_paths(Some(&configured), None),
            GameStatus::Broken(configured.clone())
        );
        assert_eq!(GameStatus::from_paths(None, None), GameStatus::Missing);
        assert_eq!(
            GameStatus::from_paths(Some(&configured), Some(&configured)),
            GameStatus::Found
        );
        // A path discovered without one being configured still counts.
        assert_eq!(
            GameStatus::from_paths(None, Some(&configured)),
            GameStatus::Found
        );
    }

    #[test]
    fn only_the_in_flight_states_keep_the_clock_running() {
        let mut state = State::default();
        assert!(state.animating(Route::InstalledAddons, ConnectionStatus::Connected));

        state.set_game(GameStatus::Missing);
        assert!(!state.animating(Route::InstalledAddons, ConnectionStatus::Connected));

        assert!(state.animating(Route::MyWorkshop, ConnectionStatus::Disconnected));
        state.observe_steam(ConnectionStatus::Unavailable);
        assert!(!state.animating(Route::MyWorkshop, ConnectionStatus::Unavailable));
    }
}
