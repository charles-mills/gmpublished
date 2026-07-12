use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

use parking_lot::{Condvar, Mutex, RwLock};
use steamworks::{
    Callback, CallbackHandle, Client, PublishedFileId, SteamId, SteamServersConnected,
    SteamServersDisconnected,
};

use crate::appdata::AppData;
use crate::events::BackendEvent;
use crate::search::Search;
use crate::steam::downloads::Downloads;
use crate::transactions::Transactions;

use self::users::SteamUser;

pub mod downloads;
pub mod publishing;
pub mod runtime;
pub mod subscriptions;
pub mod users;
pub mod workshop;

pub use runtime::{
    SteamAvatarRgba, SteamRuntime, SteamRuntimeError, SteamRuntimeStatus, SteamRuntimeUser,
};

pub const RESULTS_PER_PAGE: usize = steamworks::RESULTS_PER_PAGE as usize;

/// Initial delay, and cap, for the connect retry backoff. It's a retry
/// against a local daemon, not a pump: no need to hammer it every tick.
const CONNECT_RETRY_INITIAL: Duration = Duration::from_millis(50);
const CONNECT_RETRY_MAX: Duration = Duration::from_secs(1);

/// Cadence the callback pump re-checks Steam callbacks at. Shorter burns
/// wakeups through steamclient.dylib at idle for no benefit.
const CALLBACK_PUMP_INTERVAL: Duration = Duration::from_millis(50);

/// Per-thread bound on [`Steam::shutdown`]'s join. A thread still running
/// past this is logged and left detached rather than blocking process exit.
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_millis(500);

/// Generous default for [`Steam::client_wait`] call sites with no more
/// specific deadline of their own.
pub const CLIENT_WAIT_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub fn serialize_opt_steamid<S>(steamid: &Option<SteamId>, serialize: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match steamid {
        Some(steamid) => serialize.serialize_some(&steamid.raw().to_string()),
        None => serialize.serialize_none(),
    }
}

pub fn serialize_steamid<S>(steamid: &SteamId, serialize: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serialize.serialize_str(&steamid.raw().to_string())
}

pub struct Interface {
    client: Client,
    pub steam_id: SteamId,
}
impl std::ops::Deref for Interface {
    type Target = Client;
    fn deref(&self) -> &Self::Target {
        &self.client
    }
}
impl From<Client> for Interface {
    fn from(client: Client) -> Self {
        let user = client.user();

        Self {
            steam_id: user.steam_id(),
            client,
        }
    }
}

pub struct Steam {
    connected: AtomicBool,
    connected_wait: (Mutex<bool>, Condvar),

    /// Set exactly once, by [`Self::connect`]'s first success. Never cleared
    /// afterward: a later disconnect flips `connected` back to `false` but
    /// leaves a previously obtained interface valid, so [`Self::client`]
    /// keeps succeeding for the rest of the process's life. Code that reads
    /// `connected()` before calling [`Self::client`] (e.g.
    /// `discover_gmod_dir`'s race) leans on this.
    interface: OnceLock<Interface>,

    /// Signals every background thread spawned from this `Steam` to stop.
    /// Paired with a `Condvar` so a sleeping thread wakes immediately
    /// instead of finishing out its tick.
    shutdown: (Mutex<bool>, Condvar),
    /// Handles for every thread spawned from this `Steam`, joined by
    /// [`Self::shutdown`].
    threads: Mutex<Vec<JoinHandle<()>>>,

    users: RwLock<HashMap<SteamId, SteamUser>>,
    /// steamworks keeps a single callback slot per event type — a later
    /// registration replaces the current one, and any handle's drop clears
    /// the slot — so persona waits ([`Self::fetch_user`]) must not overlap.
    persona_fetch: Mutex<()>,

    workshop_dedup: Mutex<HashSet<PublishedFileId>>,
    workshop_queue_tx: mpsc::Sender<Vec<PublishedFileId>>,
    workshop_queue_rx: Mutex<mpsc::Receiver<Vec<PublishedFileId>>>,

    transactions: Transactions,
}

impl Steam {
    #[must_use]
    pub fn new(transactions: Transactions) -> Self {
        let (workshop_queue_tx, workshop_queue_rx) = mpsc::channel();
        Self {
            connected: AtomicBool::new(false),
            connected_wait: (Mutex::new(false), Condvar::new()),
            interface: OnceLock::new(),
            shutdown: (Mutex::new(false), Condvar::new()),
            threads: Mutex::new(Vec::new()),
            users: RwLock::new(HashMap::new()),
            persona_fetch: Mutex::new(()),

            workshop_dedup: Mutex::new(HashSet::new()),
            workshop_queue_tx,
            workshop_queue_rx: Mutex::new(workshop_queue_rx),
            transactions,
        }
    }

    /// Spawns the process-lifetime Steam connection loop plus, once
    /// connected, the callback watchdog, the workshop-metadata fetcher, and
    /// the downloads watchdog. Called exactly once, by [`crate::Backend`]
    /// construction — never lazily, so every background thread's
    /// dependencies are explicit `Arc` clones rather than global lookups.
    pub fn spawn_background_threads(
        steam: &Arc<Self>,
        app_data: &Arc<AppData>,
        search: &Arc<Search>,
        downloads: &Arc<Downloads>,
    ) {
        let handle = {
            let steam = Arc::clone(steam);
            let app_data = Arc::clone(app_data);
            let search = Arc::clone(search);
            let downloads = Arc::clone(downloads);
            std::thread::spawn(move || Self::connect(&steam, &app_data, &search, &downloads))
        };
        steam.threads.lock().push(handle);
    }

    fn watchdog(steam: &Arc<Self>, pump: &Client) {
        #[cfg(debug_assertions)]
        let _connect_failure_callback = {
            let for_callback = Arc::clone(steam);
            steam.register_callback(move |c: steamworks::SteamServerConnectFailure| {
                for_callback.set_connected(false);
                log::warn!("[Steam] SteamServerConnectFailure {c:#?}");
            })
        };

        let _connected_callback = {
            let for_callback = Arc::clone(steam);
            steam.register_callback(move |_: SteamServersConnected| {
                for_callback.set_connected(true);
                log::info!("[Steam] Connected");
            })
        };

        let _disconnected_callback = {
            let for_callback = Arc::clone(steam);
            steam.register_callback(move |c: SteamServersDisconnected| {
                for_callback.set_connected(false);
                log::warn!("[Steam] SteamServersDisconnected {c:#?}");
            })
        };

        // These callback handles are held for the lifetime of this thread.
        loop {
            pump.run_callbacks();
            // Parked on the shutdown signal rather than a plain sleep, so
            // exit is prompt.
            if condvar_wait_bool(&steam.shutdown, CALLBACK_PUMP_INTERVAL) {
                return;
            }
        }
    }

    fn on_initialized(
        steam: &Arc<Self>,
        pump: Client,
        app_data: &Arc<AppData>,
        search: Arc<Search>,
        downloads: Arc<Downloads>,
    ) {
        let watchdog_handle = {
            let steam = Arc::clone(steam);
            std::thread::spawn(move || Self::watchdog(&steam, &pump))
        };
        let workshop_fetcher_handle = {
            let steam = Arc::clone(steam);
            std::thread::spawn(move || Self::workshop_fetcher(&steam, &search))
        };
        let downloads_watchdog_handle = {
            let steam = Arc::clone(steam);
            std::thread::spawn(move || Downloads::watchdog(&downloads, &steam))
        };
        steam.threads.lock().extend([
            watchdog_handle,
            workshop_fetcher_handle,
            downloads_watchdog_handle,
        ]);

        app_data.send_after_steam_init_if_gmod_unset(steam);
    }

    fn connect(
        steam: &Arc<Self>,
        app_data: &Arc<AppData>,
        search: &Arc<Search>,
        downloads: &Arc<Downloads>,
    ) {
        let mut client = None;
        retry_until_shutdown(
            &steam.shutdown,
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
            // Shutdown was signaled before a connection succeeded.
            return;
        };

        log::info!("[Steam] Client initialized");

        let pump = client.clone();
        if steam.interface.set(Interface::from(client)).is_err() {
            panic!("Steam interface should only be initialized once");
        }

        steam.set_connected(true);

        Self::on_initialized(
            steam,
            pump,
            app_data,
            Arc::clone(search),
            Arc::clone(downloads),
        );
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Release);
        {
            let mut connected_wait = self.connected_wait.0.lock();
            *connected_wait = connected;
            drop(connected_wait);
            self.connected_wait.1.notify_all();
        }
        self.transactions.emit(if connected {
            BackendEvent::SteamConnected
        } else {
            BackendEvent::SteamDisconnected
        });
    }

    /// The connected interface, or [`runtime::SteamRuntimeError::NotConnected`]
    /// before the first successful [`Self::connect`]. See the `interface`
    /// field doc for why a prior success is never invalidated by a later
    /// disconnect.
    pub fn client(&self) -> Result<&Interface, runtime::SteamRuntimeError> {
        self.interface
            .get()
            .ok_or(runtime::SteamRuntimeError::NotConnected)
    }

    /// Blocks until connected or `timeout` elapses, then returns the
    /// interface (or [`runtime::SteamRuntimeError::NotConnected`] if the
    /// deadline passed first).
    pub fn client_wait(&self, timeout: Duration) -> Result<&Interface, runtime::SteamRuntimeError> {
        if condvar_wait_bool(&self.connected_wait, timeout) {
            self.client()
        } else {
            Err(runtime::SteamRuntimeError::NotConnected)
        }
    }

    /// Blocks until connected or `timeout` elapses, returning whether it connected.
    pub fn wait_for_connected(&self, timeout: Duration) -> bool {
        condvar_wait_bool(&self.connected_wait, timeout)
    }

    pub fn callback_once_with_data<C, EqF>(&self, eq_f: EqF, timeout: Duration) -> Option<C>
    where
        C: Callback + Send + 'static,
        EqF: Fn(&C) -> bool + 'static + Send,
    {
        let (tx, rx) = mpsc::channel();
        let _cb = {
            let mut tx = Some(tx);
            self.register_callback(move |c: C| {
                if eq_f(&c)
                    && let Some(tx) = tx.take()
                {
                    let _ = tx.send(c);
                }
            })
        };

        recv_callback_data(&rx, timeout)
    }

    pub fn callback_once<C, EqF>(&self, eq_f: EqF, timeout: Duration) -> bool
    where
        C: Callback + Send,
        EqF: Fn(&C) -> bool + 'static + Send,
    {
        let (tx, rx) = mpsc::channel();
        let _cb = {
            let mut tx = Some(tx);
            self.register_callback(move |c: C| {
                if eq_f(&c)
                    && let Some(tx) = tx.take()
                {
                    let _ = tx.send(());
                }
            })
        };

        recv_callback_signal(&rx, timeout)
    }

    pub fn register_callback<C, F>(&self, f: F) -> CallbackHandle
    where
        C: Callback,
        F: FnMut(C) + 'static + Send,
    {
        self.client()
            .expect(
                "register_callback is only ever invoked from contexts that already hold a \
                 connected client",
            )
            .register_callback(f)
    }

    /// Signals every background thread spawned from this `Steam` to stop and
    /// joins each with a bounded wait. A thread still running past the bound
    /// is logged and left detached rather than blocking process exit.
    /// Idempotent: safe to call more than once (e.g. from both an explicit
    /// app-exit path and a `Backend` drop).
    pub fn shutdown(&self) {
        *self.shutdown.0.lock() = true;
        self.shutdown.1.notify_all();

        for handle in std::mem::take(&mut *self.threads.lock()) {
            join_with_timeout(handle, SHUTDOWN_JOIN_TIMEOUT);
        }
    }
}

/// Joins `handle`, giving up (and logging) after `timeout`. The join itself
/// still completes eventually on a detached helper thread; giving up here
/// just stops it from blocking whoever called us (process exit).
fn join_with_timeout(handle: JoinHandle<()>, timeout: Duration) {
    let (done_tx, done_rx) = mpsc::channel();
    let joiner = std::thread::spawn(move || {
        let _ = handle.join();
        let _ = done_tx.send(());
    });
    if done_rx.recv_timeout(timeout).is_err() {
        log::warn!(
            "[Steam] a background thread did not exit within {timeout:?} of shutdown; detaching it"
        );
    }
    drop(joiner);
}

/// Retries `attempt` with exponential backoff (`initial`, doubling to `max`)
/// until it returns `true` or `shutdown` is signaled. Returns whether it
/// succeeded — `false` only means shutdown interrupted the retry first.
fn retry_until_shutdown(
    shutdown: &(Mutex<bool>, Condvar),
    initial: Duration,
    max: Duration,
    mut attempt: impl FnMut() -> bool,
) -> bool {
    let mut delay = initial;
    loop {
        if attempt() {
            return true;
        }
        if condvar_wait_bool(shutdown, delay) {
            return false;
        }
        delay = (delay * 2).min(max);
    }
}

fn recv_callback_data<C>(rx: &mpsc::Receiver<C>, timeout: Duration) -> Option<C> {
    rx.recv_timeout(timeout).ok()
}

fn recv_callback_signal(rx: &mpsc::Receiver<()>, timeout: Duration) -> bool {
    recv_callback_data(rx, timeout).is_some()
}

// Condvar pairing: the guard is handed to wait_while_for.
#[expect(clippy::significant_drop_tightening)]
fn condvar_wait_bool(pair: &(Mutex<bool>, Condvar), timeout: Duration) -> bool {
    let mut value = pair.0.lock();
    if *value {
        return true;
    }
    !pair
        .1
        .wait_while_for(&mut value, |value| !*value, timeout)
        .timed_out()
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    use parking_lot::{Condvar, Mutex};

    #[test]
    fn condvar_wait_bool_returns_immediately_when_already_true() {
        let pair = (Mutex::new(true), Condvar::new());

        assert!(super::condvar_wait_bool(&pair, Duration::from_millis(1)));
    }

    #[test]
    fn condvar_wait_bool_wakes_up_when_set_before_timeout() {
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        let setter = pair.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            *setter.0.lock() = true;
            setter.1.notify_all();
        });

        assert!(super::condvar_wait_bool(&pair, Duration::from_secs(5)));
    }

    #[test]
    fn condvar_wait_bool_times_out_when_never_set() {
        let pair = (Mutex::new(false), Condvar::new());

        assert!(!super::condvar_wait_bool(&pair, Duration::from_millis(20)));
    }

    #[test]
    fn client_before_connect_errs_instead_of_panicking() {
        let steam = super::Steam::new(crate::transactions::Transactions::new(
            Arc::new(crate::events::NullEventSink),
            false,
        ));

        assert_eq!(
            steam.client().err(),
            Some(crate::steam::runtime::SteamRuntimeError::NotConnected)
        );
    }

    #[test]
    fn retry_until_shutdown_exits_promptly_once_signaled() {
        let shutdown = Arc::new((Mutex::new(false), Condvar::new()));
        let signaler = Arc::clone(&shutdown);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            *signaler.0.lock() = true;
            signaler.1.notify_all();
        });

        let started = Instant::now();
        // A backoff cap far longer than the signal delay proves the retry
        // loop wakes on the signal rather than sleeping out a full tick.
        let succeeded = super::retry_until_shutdown(
            &shutdown,
            Duration::from_millis(50),
            Duration::from_secs(10),
            || false,
        );

        assert!(!succeeded);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn retry_until_shutdown_returns_true_on_first_success() {
        let shutdown = (Mutex::new(false), Condvar::new());

        let succeeded = super::retry_until_shutdown(
            &shutdown,
            Duration::from_millis(50),
            Duration::from_secs(1),
            || true,
        );

        assert!(succeeded);
    }

    #[test]
    fn shutdown_signals_and_joins_a_fake_thread_within_the_bound() {
        let steam = Arc::new(super::Steam::new(crate::transactions::Transactions::new(
            Arc::new(crate::events::NullEventSink),
            false,
        )));

        // Mirrors the shape of the real background threads: owns an
        // `Arc<Steam>` clone, loops checking the shutdown signal each tick,
        // parked on the same condvar pair `shutdown()` notifies.
        let handle = {
            let steam = Arc::clone(&steam);
            std::thread::spawn(move || {
                loop {
                    if super::condvar_wait_bool(&steam.shutdown, Duration::from_secs(30)) {
                        return;
                    }
                }
            })
        };
        steam.threads.lock().push(handle);

        let started = Instant::now();
        steam.shutdown();

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(steam.threads.lock().is_empty());
    }
}
