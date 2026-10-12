//! Steam may drop a callback without calling it. All callback waits need
//! a deadline.

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

pub(crate) const CLIENT_WAIT_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) const CALLBACK_RESULT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Interface {
    client: Client,
    pub steam_id: SteamId,
}
impl Interface {
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

    /// Initialized on first connection and never cleared. Disconnecting changes
    /// `connected`, but previously obtained Steam interfaces remain valid.
    interface: OnceLock<Interface>,

    users: RwLock<HashMap<SteamId, SteamUser>>,
    persona_slot: callback_slot::CallbackSlot,

    workshop_language: RwLock<crate::WorkshopLanguage>,
    workshop_dedup: Mutex<HashSet<WorkshopId>>,
    workshop_queue_tx: mpsc::Sender<Vec<WorkshopId>>,
    workshop_queue_rx: Mutex<mpsc::Receiver<Vec<WorkshopId>>>,

    transactions: Transactions,
}

impl Steam {
    pub fn workshop_language(&self) -> crate::WorkshopLanguage {
        *self.workshop_language.read()
    }
    pub fn set_workshop_language(&self, language: crate::WorkshopLanguage) -> bool {
        {
            let mut current = self.workshop_language.write();
            if *current == language {
                return false;
            }
            *current = language;
        }
        let ids = self.workshop_dedup.lock().drain().collect();
        self.fetch_workshop_items(ids);
        true
    }
    #[must_use]
    pub fn new(transactions: Transactions) -> Self {
        let (workshop_queue_tx, workshop_queue_rx) = mpsc::channel();
        Self {
            connected: AtomicBool::new(false),
            connected_wait: Signal::new(),
            interface: OnceLock::new(),
            users: RwLock::new(HashMap::new()),
            persona_slot: callback_slot::CallbackSlot::default(),
            workshop_language: RwLock::new(crate::WorkshopLanguage::English),

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

    pub fn client(&self) -> Result<&Interface, runtime::SteamRuntimeError> {
        self.interface
            .get()
            .ok_or(runtime::SteamRuntimeError::NotConnected)
    }

    pub fn require_client(&self) -> Result<ConnectedSteam<'_>, runtime::SteamRuntimeError> {
        if !self.connected() {
            return Err(runtime::SteamRuntimeError::NotConnected);
        }
        Ok(ConnectedSteam {
            steam: self,
            interface: self.client()?,
            workshop_language: self.workshop_language(),
        })
    }

    pub fn client_wait(&self, timeout: Duration) -> Result<&Interface, runtime::SteamRuntimeError> {
        if self.connected_wait.wait_until_set(timeout) {
            self.client()
        } else {
            Err(runtime::SteamRuntimeError::NotConnected)
        }
    }

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

#[derive(Clone, Copy)]
pub struct ConnectedSteam<'a> {
    // All chunks of an operation must use the same language across settings changes.
    workshop_language: crate::WorkshopLanguage,
    pub(crate) steam: &'a Steam,
    pub(crate) interface: &'a Interface,
}

impl ConnectedSteam<'_> {
    #[must_use]
    pub const fn workshop_language(&self) -> crate::WorkshopLanguage {
        self.workshop_language
    }
    #[must_use]
    pub const fn with_workshop_language(mut self, language: crate::WorkshopLanguage) -> Self {
        self.workshop_language = language;
        self
    }
}
