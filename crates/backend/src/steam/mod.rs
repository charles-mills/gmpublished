use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
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

/// Bound on [`Steam::shutdown`]'s join, shared across every thread rather
/// than applied per-thread: the joins run concurrently, so one thread that
/// refuses to stop costs this much in total, not this much each. Anything
/// still running past it is logged and left detached rather than blocking
/// process exit.
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_millis(500);

/// Generous default for [`Steam::client_wait`] call sites with no more
/// specific deadline of their own.
pub const CLIENT_WAIT_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Nudges for threads that park on something other than the `shutdown`
    /// condvar (a channel, a queue-specific condvar). Setting the flag alone
    /// never reaches those threads, so they would sit blocked until the join
    /// bound expired and they were detached. See
    /// [`Self::register_shutdown_waker`].
    shutdown_wakers: Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,

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
            shutdown_wakers: Mutex::new(Vec::new()),
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
    /// the downloads watchdog. Called exactly once through the backend's
    /// start gate, either during construction or explicitly after frame one;
    /// every dependency remains an explicit `Arc` clone.
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
        // The downloads watchdog parks on its own condvar with no timeout
        // while nothing is downloading — the idle case — so the shutdown
        // flag alone never reaches it.
        steam.register_shutdown_waker({
            let downloads = Arc::clone(&downloads);
            move || downloads.wake_watchdog()
        });
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

    /// Whether [`Self::shutdown`] has been signaled. Background threads that
    /// block on something other than the shutdown condvar check this after
    /// every wake-up.
    pub(crate) fn shutting_down(&self) -> bool {
        *self.shutdown.0.lock()
    }

    /// Registers a nudge run by [`Self::shutdown`] after the flag is set.
    ///
    /// A thread parked on the `shutdown` condvar needs nothing here — the
    /// `notify_all` reaches it. This is for threads parked elsewhere, whose
    /// wait has no idle timeout by design: without a nudge they never
    /// observe the flag at all, and shutdown pays the full join bound
    /// waiting for a thread that was never going to wake.
    pub(crate) fn register_shutdown_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.shutdown_wakers.lock().push(Box::new(wake));
    }

    /// Signals every background thread spawned from this `Steam` to stop and
    /// joins them, concurrently, within a single bounded wait. Threads still
    /// running past the bound are logged and left detached rather than
    /// blocking process exit. Idempotent: safe to call more than once (e.g.
    /// from both an explicit app-exit path and a `Backend` drop).
    pub fn shutdown(&self) {
        *self.shutdown.0.lock() = true;
        self.shutdown.1.notify_all();

        // The workshop fetcher parks in a blocking `recv` on this queue, and
        // the sender it waits on lives on the `Steam` its own `Arc` clone
        // keeps alive — so the channel never disconnects on its own. An
        // empty batch unblocks it; it re-checks the flag before draining.
        let _ = self.workshop_queue_tx.send(Vec::new());
        for wake in self.shutdown_wakers.lock().iter() {
            wake();
        }

        // Bind before iterating: holding the `threads` guard across the join
        // would block anything still trying to register a thread.
        let handles = std::mem::take(&mut *self.threads.lock());
        join_all_within(handles, SHUTDOWN_JOIN_TIMEOUT);
    }
}

/// Joins every handle concurrently, giving up (and logging) once `timeout`
/// has elapsed overall. The joins themselves still complete eventually on
/// detached helper threads; giving up here just stops a thread that ignored
/// the shutdown signal from blocking whoever called us (process exit).
fn join_all_within(handles: Vec<JoinHandle<()>>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let expected = handles.len();

    let (done_tx, done_rx) = mpsc::channel();
    for handle in handles {
        let done_tx = done_tx.clone();
        std::thread::spawn(move || {
            let _ = handle.join();
            let _ = done_tx.send(());
        });
    }
    // Otherwise the receiver below never sees a disconnect.
    drop(done_tx);

    let mut exited = 0;
    while exited < expected {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        if done_rx.recv_timeout(remaining).is_err() {
            break;
        }
        exited += 1;
    }

    if exited < expected {
        log::warn!(
            "[Steam] {} background thread(s) did not exit within {timeout:?} of shutdown; detaching them",
            expected - exited
        );
    }
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
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };

    use parking_lot::{Condvar, Mutex};

    fn test_steam() -> Arc<super::Steam> {
        Arc::new(super::Steam::new(crate::transactions::Transactions::new(
            Arc::new(crate::events::NullEventSink),
            false,
        )))
    }

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

    #[test]
    fn shutdown_wakes_a_thread_blocked_on_the_workshop_queue() {
        let steam = test_steam();
        let exited = Arc::new(AtomicBool::new(false));

        // The workshop fetcher's shape: parked in a blocking `recv` whose
        // sender lives on the `Steam` this thread's own clone keeps alive, so
        // the channel can never disconnect by itself.
        let handle = {
            let steam = Arc::clone(&steam);
            let exited = Arc::clone(&exited);
            std::thread::spawn(move || {
                loop {
                    let rx = steam.workshop_queue_rx.lock();
                    if rx.recv().is_err() {
                        return;
                    }
                    drop(rx);
                    if steam.shutting_down() {
                        exited.store(true, Ordering::SeqCst);
                        return;
                    }
                }
            })
        };
        steam.threads.lock().push(handle);

        steam.shutdown();

        // `shutdown` joins, so a set flag means the thread genuinely woke and
        // returned rather than being detached at the bound.
        assert!(
            exited.load(Ordering::SeqCst),
            "shutdown must unblock the workshop queue receiver"
        );
    }

    #[test]
    fn shutdown_wakes_a_thread_parked_on_a_registered_waker() {
        let steam = test_steam();
        let park = Arc::new((Mutex::new(false), Condvar::new()));
        let exited = Arc::new(AtomicBool::new(false));

        // Only the waker sets the predicate, so the thread below cannot
        // escape its park by any other route — including by racing ahead of
        // `shutdown` and observing the flag before it ever waits.
        steam.register_shutdown_waker({
            let park = Arc::clone(&park);
            move || {
                *park.0.lock() = true;
                park.1.notify_all();
            }
        });

        // The downloads watchdog's shape: an untimed park on a condvar that
        // `shutdown` does not itself notify. Looping on the predicate is what
        // makes an early nudge safe rather than a lost wake-up.
        let handle = {
            let park = Arc::clone(&park);
            let exited = Arc::clone(&exited);
            std::thread::spawn(move || {
                let mut guard = park.0.lock();
                while !*guard {
                    park.1.wait(&mut guard);
                }
                exited.store(true, Ordering::SeqCst);
            })
        };
        steam.threads.lock().push(handle);

        steam.shutdown();

        assert!(
            exited.load(Ordering::SeqCst),
            "shutdown must run registered wakers"
        );
    }

    #[test]
    fn shutdown_bounds_every_join_together_rather_than_one_bound_each() {
        let steam = test_steam();

        // Threads that deliberately never observe the signal. Joined
        // sequentially at one bound each, this would take six times the
        // bound; joined concurrently against a shared deadline, one.
        for _ in 0..6 {
            let handle = std::thread::spawn(move || {
                let never = (Mutex::new(false), Condvar::new());
                let mut guard = never.0.lock();
                while !*guard {
                    never.1.wait(&mut guard);
                }
            });
            steam.threads.lock().push(handle);
        }

        let started = Instant::now();
        steam.shutdown();
        let elapsed = started.elapsed();

        assert!(
            elapsed < super::SHUTDOWN_JOIN_TIMEOUT * 3,
            "six stuck threads took {elapsed:?}, which is more than one shared bound"
        );
    }
}
