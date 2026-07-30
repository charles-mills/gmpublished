//! The Steam client connection and everything reached through it: workshop
//! browsing, subscriptions, downloads, publishing and user details.
//!
//! steamworks is single-threaded and callback-driven. [`Steam`] owns the
//! connection and domain state; [`background::SteamBackgroundRuntime`] at the
//! composition root owns the process-lifetime loops that drive it. Every call
//! that waits on a callback carries a deadline — the library may drop a
//! callback without invoking it.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use parking_lot::{Mutex, RwLock};
use steamworks::{Callback, CallbackHandle, Client};

use crate::WorkshopId;

/// Steam's own account id, re-exported so the app builds one at its boundary
/// rather than passing a raw integer through this crate's API.
///
/// `steamworks::PublishedFileId` is deliberately *not* re-exported:
/// [`crate::WorkshopId`] is this crate's workshop-item id, and it refuses the
/// zero that the steamworks type accepts. Re-exporting the looser type beside
/// it would leave the boundary open at the one place it exists to close.
pub use steamworks::SteamId;

use crate::events::BackendEvent;
use crate::signal::Signal;
use crate::transactions::Transactions;

use self::users::SteamUser;

pub(crate) mod background;
mod callback_slot;
pub(crate) mod downloads;
pub(crate) mod publishing;
pub(crate) mod runtime;
pub(crate) mod users;
pub(crate) mod workshop;

pub use runtime::{
    SteamAvatarRgba, SteamRuntime, SteamRuntimeError, SteamRuntimeStatus, SteamRuntimeUser,
};

pub(crate) const RESULTS_PER_PAGE: usize = steamworks::RESULTS_PER_PAGE as usize;

/// Generous default for [`Steam::client_wait`] call sites with no more
/// specific deadline of their own.
pub(crate) const CLIENT_WAIT_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound on waiting for a steamworks callback to deliver its result.
///
/// steamworks owns the callback's lifetime: it may drop the closure without
/// invoking it, or hold it indefinitely, and neither is something this side
/// can observe. Every wait on a callback result therefore needs a deadline,
/// or a caller blocks for the life of the process.
pub(crate) const CALLBACK_RESULT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Interface {
    client: Client,
    pub steam_id: SteamId,
}
impl Interface {
    /// The underlying steamworks client. Explicit rather than a `Deref`, so
    /// `Interface`'s own surface stays visible at its definition.
    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn register_callback<C, F>(&self, f: F) -> CallbackHandle
    where
        C: Callback,
        F: FnMut(C) + 'static + Send,
    {
        self.client.register_callback(f)
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
    connected_wait: Signal,

    /// Set exactly once by the background runtime's first successful
    /// connection. Never cleared afterward: a later disconnect flips
    /// `connected` back to `false` but
    /// leaves a previously obtained interface valid, so [`Self::client`]
    /// keeps succeeding for the rest of the process's life. Code that reads
    /// `connected()` before calling [`Self::client`] (e.g.
    /// `discover_gmod_dir`'s race) leans on this.
    interface: OnceLock<Interface>,

    users: RwLock<HashMap<SteamId, SteamUser>>,
    /// The slot persona waits ([`Self::fetch_user`]) contend for.
    persona_slot: callback_slot::CallbackSlot,

    workshop_dedup: Mutex<HashSet<WorkshopId>>,
    workshop_queue_tx: mpsc::Sender<Vec<WorkshopId>>,
    workshop_queue_rx: Mutex<mpsc::Receiver<Vec<WorkshopId>>>,

    transactions: Transactions,
}

impl Steam {
    #[must_use]
    pub fn new(transactions: Transactions) -> Self {
        let (workshop_queue_tx, workshop_queue_rx) = mpsc::channel();
        Self {
            connected: AtomicBool::new(false),
            connected_wait: Signal::new(),
            interface: OnceLock::new(),
            users: RwLock::new(HashMap::new()),
            persona_slot: callback_slot::CallbackSlot::default(),

            workshop_dedup: Mutex::new(HashSet::new()),
            workshop_queue_tx,
            workshop_queue_rx: Mutex::new(workshop_queue_rx),
            transactions,
        }
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    fn set_connected(&self, connected: bool) {
        let was_connected = self.connected.swap(connected, Ordering::AcqRel);
        self.connected_wait.store(connected);
        if was_connected == connected {
            return;
        }
        self.transactions.emit(if connected {
            BackendEvent::SteamConnected
        } else {
            BackendEvent::SteamDisconnected
        });
    }

    /// The connected interface, or [`runtime::SteamRuntimeError::NotConnected`]
    /// before the first successful background connection. See the `interface`
    /// field doc for why a prior success is never invalidated by a later
    /// disconnect.
    pub fn client(&self) -> Result<&Interface, runtime::SteamRuntimeError> {
        self.interface
            .get()
            .ok_or(runtime::SteamRuntimeError::NotConnected)
    }

    /// A handle to the operations that need a live connection.
    ///
    /// Stricter than [`Self::client`], which keeps returning the interface
    /// after a disconnect: this also requires [`Self::connected`], so holding
    /// one is the whole precondition those operations need.
    pub fn require_client(&self) -> Result<ConnectedSteam<'_>, runtime::SteamRuntimeError> {
        if !self.connected() {
            return Err(runtime::SteamRuntimeError::NotConnected);
        }
        Ok(ConnectedSteam {
            steam: self,
            interface: self.client()?,
        })
    }

    /// Blocks until connected or `timeout` elapses, then returns the
    /// interface (or [`runtime::SteamRuntimeError::NotConnected`] if the
    /// deadline passed first).
    pub fn client_wait(&self, timeout: Duration) -> Result<&Interface, runtime::SteamRuntimeError> {
        if self.connected_wait.wait_until_set(timeout) {
            self.client()
        } else {
            Err(runtime::SteamRuntimeError::NotConnected)
        }
    }

    /// Blocks until connected or `timeout` elapses, returning whether it connected.
    pub fn wait_for_connected(&self, timeout: Duration) -> bool {
        self.connected_wait.wait_until_set(timeout)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    #[test]
    fn client_before_connect_errs_instead_of_panicking() {
        let steam = super::Steam::new(crate::transactions::Transactions::new(Arc::new(
            crate::events::NullEventSink,
        )));

        assert_eq!(
            steam.client().err(),
            Some(crate::steam::runtime::SteamRuntimeError::NotConnected)
        );
    }
}

/// Steam operations paired with the live state they read.
///
/// Only the backend connection boundary constructs one, after checking both
/// that the interface exists and that the connection is live, so these
/// operations never re-check either.
#[derive(Clone, Copy)]
pub struct ConnectedSteam<'a> {
    pub(crate) steam: &'a Steam,
    pub(crate) interface: &'a Interface,
}
