//! Composition root: constructs every backend service, wires their
//! dependencies together explicitly, and spawns the process-lifetime
//! background threads (Steam connect/watchdog, workshop fetcher, downloads
//! watchdog, whitelist warm-up). Services take their dependencies as
//! constructor parameters or fields set here rather than reaching for each
//! other.
//!
//! Not yet true of process globals: four `LazyLock` rayon pools
//! (`steam::downloads`, `gma::write`, `gma::extract`) and two `AtomicU64`
//! counters are still statics, and none of them observes shutdown. See
//! CODE_REVIEW.md §33a.

use std::{
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
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
    /// Whether process-lifetime Steam and whitelist services may be started.
    pub background_services: BackgroundServices,
    /// Whether process-global logging should write to this backend's app-data
    /// directory. Test backends disable it because many isolated roots coexist
    /// in one process and only the first can own the global sink.
    pub file_logging: bool,
}

/// Controls process-lifetime services independently from construction of the
/// cheap service handles they use.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BackgroundServices {
    /// Allow an explicit, one-shot start after backend construction.
    #[default]
    Enabled,
    /// Never start them (CLI and isolated tests).
    Disabled,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            cli_mode: false,
            event_sink: Arc::new(NullEventSink),
            data_root: None,
            background_services: BackgroundServices::Enabled,
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
            background_services: BackgroundServices::Disabled,
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
    background_services: BackgroundServiceGate,
}

#[derive(Debug)]
struct BackgroundServiceGate {
    enabled: bool,
    started: AtomicBool,
}

impl BackgroundServiceGate {
    const fn new(mode: BackgroundServices) -> Self {
        Self {
            enabled: matches!(mode, BackgroundServices::Enabled),
            started: AtomicBool::new(false),
        }
    }

    fn try_start(&self) -> bool {
        self.enabled
            && self
                .started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
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

/// A failure during backend construction. Fatal: it aborts startup before any
/// window exists, so it is printed rather than localized — no `HasErrorKey`.
#[derive(Debug, thiserror::Error)]
pub enum BackendInitError {
    #[error("failed to install backend logger: {0}")]
    LoggerInstall(String),
    #[error("backend initialization stage '{stage}' panicked: {message}")]
    StagePanic {
        stage: &'static str,
        message: String,
    },
}

impl Backend {
    /// Constructs every service in dependency order. Process-lifetime work
    /// remains dormant until the caller explicitly releases it with
    /// [`Self::start_background_services`].
    pub fn init(config: BackendConfig) -> Result<Arc<Self>, BackendInitError> {
        let BackendConfig {
            cli_mode,
            event_sink,
            data_root,
            background_services,
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
            background_services: BackgroundServiceGate::new(background_services),
        });

        Ok(backend)
    }

    /// Starts process-lifetime services at most once. Returns whether this
    /// call won the start gate; disabled backends always return `false`.
    pub fn start_background_services(self: &Arc<Self>) -> bool {
        if !self.background_services.try_start() {
            return false;
        }

        Steam::spawn_background_threads(&self.steam, &self.app_data, &self.search, &self.downloads);

        log::info!("warming GMA whitelist");
        // A plain thread keeps the 12-thread rayon pool lazy; spawning here
        // would build the whole pool at startup for a one-shot warm-up.
        let whitelist = self.whitelist.clone();
        std::thread::spawn(move || whitelist.refresh_from_remote());
        true
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
            backend_a.app_data.settings().language.as_deref(),
            Some("en-US")
        );
        assert_eq!(backend_b.app_data.settings().language, None);
    }

    #[test]
    fn enabled_background_service_gate_opens_once() {
        let gate = BackgroundServiceGate::new(BackgroundServices::Enabled);

        assert!(gate.try_start());
        assert!(!gate.try_start());
    }

    #[test]
    fn disabled_background_service_gate_never_opens() {
        let gate = BackgroundServiceGate::new(BackgroundServices::Disabled);

        assert!(!gate.try_start());
        assert!(!gate.try_start());
    }
}
