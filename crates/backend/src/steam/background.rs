//! Ownership and supervision for Steam's process-lifetime threads.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    thread::JoinHandle,
    time::Duration,
};

use parking_lot::Mutex;
use steamworks::{Client, SteamServersConnected, SteamServersDisconnected};

use crate::{appdata::AppData, search::Search, signal::Signal};

use super::{Interface, Steam, downloads::Downloads};

const CONNECT_RETRY_INITIAL: Duration = Duration::from_millis(50);
const CONNECT_RETRY_MAX: Duration = Duration::from_secs(1);
const CALLBACK_PUMP_INTERVAL: Duration = Duration::from_millis(50);
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_millis(500);

const CONNECT_THREAD: &str = "gmpublished-steam-connect";
const CALLBACK_THREAD: &str = "gmpublished-steam-callbacks";
const WORKSHOP_THREAD: &str = "gmpublished-steam-workshop";
const DOWNLOADS_THREAD: &str = "gmpublished-steam-downloads";

/// Root-owned lifecycle for Steam's long-lived services.
///
/// The domain-facing [`Steam`] value intentionally owns no thread handles or
/// shutdown state. This runtime is the one place that may start, stop, wake,
/// and join its background work.
pub(crate) struct SteamBackgroundRuntime {
    state: Mutex<RuntimeState>,
}

enum RuntimeState {
    Disabled,
    Dormant,
    Running(Arc<BackgroundControl>),
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SteamBackgroundStart {
    Started,
    AlreadyRunning,
    Disabled,
    Stopped,
}

impl std::fmt::Debug for SteamBackgroundRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        let state = match &*state {
            RuntimeState::Disabled => "disabled",
            RuntimeState::Dormant => "dormant",
            RuntimeState::Running(_) => "running",
            RuntimeState::Stopped => "stopped",
        };
        formatter
            .debug_struct("SteamBackgroundRuntime")
            .field("state", &state)
            .finish()
    }
}

impl SteamBackgroundRuntime {
    pub(crate) const fn new(enabled: bool) -> Self {
        Self {
            state: Mutex::new(if enabled {
                RuntimeState::Dormant
            } else {
                RuntimeState::Disabled
            }),
        }
    }

    /// Starts the supervisor once and reports the exact lifecycle outcome.
    pub(crate) fn start(
        &self,
        steam: Arc<Steam>,
        app_data: Arc<AppData>,
        search: Arc<Search>,
        downloads: Arc<Downloads>,
    ) -> Result<SteamBackgroundStart, SteamBackgroundStartError> {
        let mut state = self.state.lock();
        match *state {
            RuntimeState::Disabled => return Ok(SteamBackgroundStart::Disabled),
            RuntimeState::Running(_) => return Ok(SteamBackgroundStart::AlreadyRunning),
            RuntimeState::Stopped => return Ok(SteamBackgroundStart::Stopped),
            RuntimeState::Dormant => {}
        }

        let control = Arc::new(BackgroundControl::new());
        control.register_waker({
            let steam = Arc::clone(&steam);
            move || {
                steam.set_connected(false);
                steam.wake_workshop_fetcher();
            }
        });
        control.register_waker({
            let downloads = Arc::clone(&downloads);
            move || downloads.wake_watchdog()
        });
        control.spawn(CONNECT_THREAD, move |control| {
            connect(control, steam, app_data, search, downloads);
        })?;

        *state = RuntimeState::Running(control);
        Ok(SteamBackgroundStart::Started)
    }

    /// Signals and joins all workers. Safe before startup and on repeated calls.
    pub(crate) fn shutdown(&self) {
        let control = {
            let mut state = self.state.lock();
            match std::mem::replace(&mut *state, RuntimeState::Stopped) {
                RuntimeState::Running(control) => Some(control),
                RuntimeState::Disabled => {
                    *state = RuntimeState::Disabled;
                    None
                }
                RuntimeState::Dormant | RuntimeState::Stopped => None,
            }
        };
        if let Some(control) = control {
            control.shutdown_and_join();
        }
    }
}

impl Drop for SteamBackgroundRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SteamBackgroundStartError {
    #[error("failed to start Steam background thread `{thread_name}`: {source}")]
    Spawn {
        thread_name: &'static str,
        source: std::io::Error,
    },
    #[error("Steam background runtime stopped while starting `{thread_name}`")]
    Stopped { thread_name: &'static str },
}

struct BackgroundControl {
    shutdown: Signal,
    handles: Mutex<Vec<JoinHandle<()>>>,
    wakers: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

impl BackgroundControl {
    fn new() -> Self {
        Self {
            shutdown: Signal::new(),
            handles: Mutex::new(Vec::new()),
            wakers: Mutex::new(Vec::new()),
        }
    }

    fn spawn(
        self: &Arc<Self>,
        thread_name: &'static str,
        run: impl FnOnce(Arc<Self>) + Send + 'static,
    ) -> Result<(), SteamBackgroundStartError> {
        let mut handles = self.handles.lock();
        if self.shutdown.is_set() {
            return Err(SteamBackgroundStartError::Stopped { thread_name });
        }
        let control = Arc::clone(self);
        let handle = std::thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| run(Arc::clone(&control))));
                if let Err(panic) = result {
                    log::error!(
                        "Steam background thread `{thread_name}` panicked: {}",
                        crate::panic_payload_message(&panic)
                    );
                    control.request_shutdown();
                }
            })
            .map_err(|source| SteamBackgroundStartError::Spawn {
                thread_name,
                source,
            })?;
        handles.push(handle);
        Ok(())
    }

    fn register_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.wakers.lock().push(Arc::new(wake));
    }

    fn request_shutdown(&self) {
        self.shutdown.set();
        let wakers = self.wakers.lock().clone();
        for wake in wakers {
            wake();
        }
    }

    fn shutdown_and_join(&self) {
        self.request_shutdown();
        let current = std::thread::current().id();
        let mut handles = std::mem::take(&mut *self.handles.lock());
        handles.retain(|handle| handle.thread().id() != current);
        crate::threads::join_all_within(handles, SHUTDOWN_JOIN_TIMEOUT, "Steam");
    }
}

fn connect(
    control: Arc<BackgroundControl>,
    steam: Arc<Steam>,
    app_data: Arc<AppData>,
    search: Arc<Search>,
    downloads: Arc<Downloads>,
) {
    let mut client = None;
    retry_until_shutdown(
        &control.shutdown,
        CONNECT_RETRY_INITIAL,
        CONNECT_RETRY_MAX,
        || {
            Client::init_app(4000).is_ok_and(|initialized| {
                client = Some(initialized);
                true
            })
        },
    );
    let Some(client) = client else {
        return;
    };
    if control.shutdown.is_set() {
        return;
    }

    log::info!("[Steam] Client initialized");
    let pump = client.clone();
    if steam.interface.set(Interface::from(client)).is_err() {
        log::error!("Steam interface was initialized more than once");
        control.request_shutdown();
        return;
    }
    steam.set_connected(true);

    if let Err(error) = start_connected_workers(
        &control,
        Arc::clone(&steam),
        Arc::clone(&search),
        Arc::clone(&downloads),
        pump,
    ) {
        log::error!("{error}");
        steam.set_connected(false);
        control.request_shutdown();
        return;
    }

    app_data.send_after_steam_init_if_gmod_unset(&steam);
}

fn start_connected_workers(
    control: &Arc<BackgroundControl>,
    steam: Arc<Steam>,
    search: Arc<Search>,
    downloads: Arc<Downloads>,
    pump: Client,
) -> Result<(), SteamBackgroundStartError> {
    let callback_client = pump.clone();
    let callback_steam = Arc::clone(&steam);
    control.spawn(CALLBACK_THREAD, move |control| {
        callback_watchdog(&control.shutdown, &callback_steam, &callback_client);
    })?;

    let workshop_client = pump.clone();
    control.spawn(WORKSHOP_THREAD, move |control| {
        Steam::workshop_fetcher(&steam, &search, workshop_client, &control.shutdown);
    })?;

    control.spawn(DOWNLOADS_THREAD, move |control| {
        Downloads::watchdog(&downloads, pump, &control.shutdown);
    })?;
    Ok(())
}

fn callback_watchdog(shutdown: &Signal, steam: &Arc<Steam>, pump: &Client) {
    #[cfg(debug_assertions)]
    let _connect_failure_callback = {
        let steam = Arc::clone(steam);
        pump.register_callback(move |failure: steamworks::SteamServerConnectFailure| {
            steam.set_connected(false);
            log::warn!("[Steam] SteamServerConnectFailure {failure:#?}");
        })
    };

    let _connected_callback = {
        let steam = Arc::clone(steam);
        pump.register_callback(move |_: SteamServersConnected| {
            steam.set_connected(true);
            log::info!("[Steam] Connected");
        })
    };

    let _disconnected_callback = {
        let steam = Arc::clone(steam);
        pump.register_callback(move |event: SteamServersDisconnected| {
            steam.set_connected(false);
            log::warn!("[Steam] SteamServersDisconnected {event:#?}");
        })
    };

    loop {
        pump.run_callbacks();
        if shutdown.wait_until_set(CALLBACK_PUMP_INTERVAL) {
            return;
        }
    }
}

fn retry_until_shutdown(
    shutdown: &Signal,
    initial: Duration,
    max: Duration,
    mut attempt: impl FnMut() -> bool,
) -> bool {
    let mut delay = initial;
    loop {
        if shutdown.is_set() {
            return false;
        }
        if attempt() {
            return true;
        }
        if shutdown.wait_until_set(delay) {
            return false;
        }
        delay = (delay * 2).min(max);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Instant,
    };

    use super::*;

    #[test]
    fn dormant_runtime_shutdown_is_terminal_and_idempotent() {
        let runtime = SteamBackgroundRuntime::new(true);

        runtime.shutdown();
        runtime.shutdown();

        assert!(matches!(&*runtime.state.lock(), RuntimeState::Stopped));
    }

    #[test]
    fn disabled_runtime_stays_disabled_when_shutdown_is_requested() {
        let runtime = SteamBackgroundRuntime::new(false);

        runtime.shutdown();

        assert!(matches!(&*runtime.state.lock(), RuntimeState::Disabled));
    }

    #[test]
    fn retry_exits_promptly_once_signaled() {
        let shutdown = Arc::new(Signal::new());
        let signaler = Arc::clone(&shutdown);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            signaler.set();
        });

        let started = Instant::now();
        let succeeded = retry_until_shutdown(
            &shutdown,
            Duration::from_millis(50),
            Duration::from_secs(10),
            || false,
        );
        assert!(!succeeded);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn retry_does_not_attempt_after_shutdown_was_already_latched() {
        let shutdown = Signal::new();
        shutdown.set();
        let attempted = AtomicBool::new(false);

        assert!(!retry_until_shutdown(
            &shutdown,
            Duration::from_millis(1),
            Duration::from_millis(1),
            || {
                attempted.store(true, Ordering::Release);
                true
            }
        ));
        assert!(!attempted.load(Ordering::Acquire));
    }

    #[test]
    fn control_names_workers_and_joins_them_on_shutdown() {
        let control = Arc::new(BackgroundControl::new());
        let (name_tx, name_rx) = std::sync::mpsc::channel();
        control
            .spawn("gmpublished-steam-test", move |control| {
                name_tx
                    .send(std::thread::current().name().map(str::to_owned))
                    .unwrap();
                control.shutdown.wait_until_set(Duration::from_secs(30));
            })
            .expect("spawn worker");

        assert_eq!(
            name_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .as_deref(),
            Some("gmpublished-steam-test")
        );
        control.shutdown_and_join();
        assert!(control.handles.lock().is_empty());
    }

    #[test]
    fn shutdown_runs_registered_wakers() {
        let control = BackgroundControl::new();
        let woke = Arc::new(AtomicBool::new(false));
        control.register_waker({
            let woke = Arc::clone(&woke);
            move || woke.store(true, Ordering::Release)
        });

        control.request_shutdown();
        assert!(woke.load(Ordering::Acquire));
    }

    #[test]
    fn every_join_shares_one_shutdown_deadline() {
        let control = Arc::new(BackgroundControl::new());
        for index in 0..4 {
            control
                .spawn("gmpublished-steam-stuck-test", move |_| {
                    let _ = index;
                    Signal::new().wait_until_set(Duration::from_secs(3600));
                })
                .expect("spawn stuck worker");
        }

        let started = Instant::now();
        control.shutdown_and_join();
        assert!(started.elapsed() < SHUTDOWN_JOIN_TIMEOUT * 3);
    }
}
