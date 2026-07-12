//! Composition root: constructs every backend service, wires their
//! dependencies together explicitly, and spawns the process-lifetime
//! background threads (Steam connect/watchdog, workshop fetcher, downloads
//! watchdog, whitelist warm-up). No backend service reaches for a process
//! global; everything it needs is either a constructor parameter or a field
//! set here.

use std::{
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::Arc,
};

use crate::{
    appdata::{AppData, AppDataPaths},
    events::{BackendEventSink, NullEventSink},
    gma::whitelist::AddonWhitelist,
    search::Search,
    steam::{Steam, downloads::Downloads},
    transactions::Transactions,
};

/// Configures one `Backend` instance: the event sink it delivers to, whether
/// it runs in CLI mode (transaction events suppressed — no UI is
/// listening), and environment-path overrides for tests.
pub struct BackendConfig {
    pub cli_mode: bool,
    pub event_sink: Arc<dyn BackendEventSink>,
    /// Overrides the OS-derived settings/temp/user-data/downloads roots.
    /// Production leaves this `None`; tests pass a private tempdir so
    /// parallel test processes never share a settings file.
    pub data_root: Option<PathBuf>,
    /// Whether to spawn the Steam connect/watchdog/workshop-fetcher threads
    /// and the whitelist network warm-up. Production always wants these;
    /// tests that just need service handles (not a live Steam attempt or an
    /// outbound HTTPS call per test process) set this `false`.
    pub background_threads: bool,
    /// Whether process-global logging should write to this backend's app-data
    /// directory. Test backends disable it because many isolated roots coexist
    /// in one process and only the first can own the global sink.
    pub file_logging: bool,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            cli_mode: false,
            event_sink: Arc::new(NullEventSink),
            data_root: None,
            background_threads: true,
            file_logging: true,
        }
    }
}

impl BackendConfig {
    /// A config appropriate for tests: no event delivery beyond what the
    /// caller wires up, a private tempdir root, and no background threads
    /// (no Steam connect attempt, no whitelist network fetch).
    #[must_use]
    pub fn for_test(data_root: &std::path::Path) -> Self {
        Self {
            cli_mode: false,
            event_sink: Arc::new(NullEventSink),
            data_root: Some(data_root.to_path_buf()),
            background_threads: false,
            file_logging: false,
        }
    }
}

pub struct Backend {
    pub transactions: Transactions,
    pub app_data: Arc<AppData>,
    pub steam: Arc<Steam>,
    pub search: Arc<Search>,
    pub downloads: Arc<Downloads>,
    pub whitelist: AddonWhitelist,
}

impl fmt::Debug for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Backend").finish_non_exhaustive()
    }
}

impl Drop for Backend {
    /// A safety net for anything that owns a `Backend` outside the iced app
    /// (tests, CLI mode): the app's own exit path calls
    /// [`Steam::shutdown`](crate::steam::Steam::shutdown) explicitly rather
    /// than waiting on this, since other clones of the services `Arc` this
    /// is reached through can keep it alive past the moment the window
    /// closes.
    fn drop(&mut self) {
        self.steam.shutdown();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendInitError {
    LoggerInstall(String),
    StagePanic {
        stage: &'static str,
        message: String,
    },
}

impl fmt::Display for BackendInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoggerInstall(error) => {
                write!(f, "failed to install backend logger: {error}")
            }
            Self::StagePanic { stage, message } => {
                write!(
                    f,
                    "backend initialization stage '{stage}' panicked: {message}"
                )
            }
        }
    }
}

impl std::error::Error for BackendInitError {}

impl Backend {
    /// Constructs every service in dependency order and spawns the
    /// background threads that need them, mirroring the process's
    /// historical startup order (appdata, transactions, steamworks, search,
    /// whitelist warm-up).
    pub fn init(config: BackendConfig) -> Result<Arc<Self>, BackendInitError> {
        let BackendConfig {
            cli_mode,
            event_sink,
            data_root,
            background_threads,
            file_logging,
        } = config;

        initialize_stage("logging", || {
            crate::logging::install()
                .map_err(|error| BackendInitError::LoggerInstall(error.to_string()))
        })??;

        let paths = data_root
            .as_ref()
            .map_or_else(AppDataPaths::production, |root| {
                AppDataPaths::for_test_root(root)
            });

        let transactions = Transactions::new(event_sink, cli_mode);

        log::info!("initializing appdata");
        let app_data = initialize_stage("appdata", || {
            Arc::new(AppData::load(paths, transactions.clone()))
        })?;
        if file_logging {
            crate::logging::enable_file_sink(app_data.logging_logs_dir());
        }

        log::info!("initializing steamworks");
        let steam = initialize_stage("steamworks", || Arc::new(Steam::new(transactions.clone())))?;

        log::info!("initializing search");
        let search = initialize_stage("search", || Arc::new(Search::new()))?;

        let whitelist = AddonWhitelist::new();

        let downloads = initialize_stage("downloads", || {
            Arc::new(Downloads::new(
                Arc::clone(&app_data),
                Arc::clone(&steam),
                whitelist.clone(),
                transactions.clone(),
            ))
        })?;

        let backend = Arc::new(Self {
            transactions,
            app_data,
            steam,
            search,
            downloads,
            whitelist,
        });

        if background_threads {
            Steam::spawn_background_threads(
                &backend.steam,
                &backend.app_data,
                &backend.search,
                &backend.downloads,
            );

            log::info!("warming GMA whitelist");
            // A plain thread keeps the 12-thread rayon pool lazy; spawning
            // here would build the whole pool at startup for a one-shot
            // warm-up.
            let whitelist = backend.whitelist.clone();
            std::thread::spawn(move || whitelist.refresh_from_remote());
        }

        Ok(backend)
    }
}

fn initialize_stage<T>(
    stage: &'static str,
    init: impl FnOnce() -> T,
) -> Result<T, BackendInitError> {
    catch_unwind(AssertUnwindSafe(init)).map_err(|panic| BackendInitError::StagePanic {
        stage,
        message: panic_payload_message(&panic),
    })
}

fn panic_payload_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic.downcast_ref::<&str>().map_or_else(
        || {
            panic
                .downcast_ref::<String>()
                .map_or_else(|| "non-string panic payload".to_owned(), Clone::clone)
        },
        |message| (*message).to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_builds_every_service_with_a_private_test_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let backend = Backend::init(BackendConfig::for_test(temp.path())).expect("backend init");

        assert!(!backend.steam.connected());
        assert_eq!(backend.app_data.gmod_dir(), None);
    }

    #[test]
    fn init_is_independent_across_instances() {
        let temp_a = tempfile::tempdir().expect("tempdir");
        let temp_b = tempfile::tempdir().expect("tempdir");
        let backend_a = Backend::init(BackendConfig::for_test(temp_a.path())).expect("backend a");
        let backend_b = Backend::init(BackendConfig::for_test(temp_b.path())).expect("backend b");

        backend_a.app_data.mutate_settings(|settings| {
            settings.language = Some("en-US".to_owned());
        });

        assert_eq!(
            backend_a.app_data.settings.load().language.as_deref(),
            Some("en-US")
        );
        assert_eq!(backend_b.app_data.settings.load().language, None);
    }
}
