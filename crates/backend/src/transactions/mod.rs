mod payload;

use parking_lot::RwLock;
use serde::Serialize;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU8, AtomicU32, Ordering},
};

use crate::error_key::{ErrorKey, HasErrorKey};
use crate::events::{BackendEvent, BackendEventSink, TransactionEvent};

pub use self::payload::TransactionPayload;

/// A failed transaction's identity on the event wire: a stable [`ErrorKey`]
/// plus optional contextual payload (a path, an entry name, an upstream
/// error message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionError {
    pub key: ErrorKey,
    pub detail: Option<Arc<str>>,
}

impl TransactionError {
    #[must_use]
    pub fn new(key: ErrorKey) -> Self {
        Self { key, detail: None }
    }

    #[must_use]
    pub fn detailed(key: ErrorKey, detail: Option<String>) -> Self {
        Self {
            key,
            detail: detail.map(Into::into),
        }
    }
}

impl From<ErrorKey> for TransactionError {
    fn from(key: ErrorKey) -> Self {
        Self::new(key)
    }
}

impl<E: HasErrorKey> From<&E> for TransactionError {
    fn from(error: &E) -> Self {
        Self::detailed(error.error_key(), error.error_detail())
    }
}

/// Internals shared between a [`Transactions`] handle and every
/// [`TransactionInner`] it creates. Kept separate from `Transactions` itself
/// (rather than requiring `Arc<Transactions>`) so `Transactions::begin` only
/// needs `&self`.
struct TransactionsShared {
    registry: RwLock<Vec<TransactionRef>>,
    id: AtomicU32,
    sink: Arc<dyn BackendEventSink>,
    /// Whether this `Transactions` was built for the CLI-only extraction
    /// path: transaction events are suppressed there (no UI is listening),
    /// mirroring the desktop app's opposite default.
    cli_mode: bool,
}

/// Owns transaction bookkeeping (id allocation, the live-transaction
/// registry) and event emission. Cheap to clone: internally an `Arc`, so
/// every service that needs to create transactions or emit plain backend
/// events can hold its own `Transactions` handle.
#[derive(Clone)]
pub struct Transactions {
    shared: Arc<TransactionsShared>,
}

impl std::fmt::Debug for Transactions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transactions").finish_non_exhaustive()
    }
}

impl Transactions {
    #[must_use]
    pub fn new(sink: Arc<dyn BackendEventSink>, cli_mode: bool) -> Self {
        Self {
            shared: Arc::new(TransactionsShared {
                registry: RwLock::new(Vec::new()),
                id: AtomicU32::new(0),
                sink,
                cli_mode,
            }),
        }
    }

    /// Emits a plain (non-transaction) backend event, e.g. `SteamConnected`
    /// or `AppDataUpdated`. Never suppressed by CLI mode: only transaction
    /// progress/status/error events are cli-gated.
    pub fn emit(&self, event: BackendEvent) {
        self.shared.sink.emit(event);
    }

    #[must_use]
    pub fn begin(&self) -> Transaction {
        let id = self.shared.id.fetch_add(1, Ordering::SeqCst);
        let transaction = Arc::new(TransactionInner {
            id,
            state: AtomicU8::new(STATE_RUNNING),
            shared: Arc::clone(&self.shared),
        });

        {
            let mut registry = self.shared.registry.write();
            registry.push(TransactionRef {
                id: transaction.id,
                ptr: Arc::downgrade(&transaction),
            });
            registry.reserve(1);
        }

        transaction
    }

    #[must_use]
    pub fn find(&self, transaction_id: u32) -> Option<Transaction> {
        let registry = self.shared.registry.read();
        if let Ok(pos) =
            registry.binary_search_by_key(&transaction_id, |transaction| transaction.id)
        {
            let transaction = registry.get(pos).unwrap().upgrade();
            drop(registry);
            if transaction.is_some() {
                return transaction;
            }
            #[cfg(debug_assertions)]
            panic!("Stale transaction found in transactions list");
        }

        None
    }

    pub fn cancel_by_id(&self, id: u32) -> bool {
        let Some(transaction) = self.find(id) else {
            return false;
        };
        transaction.cancel()
    }

    pub fn cancel(&self, id: u32) {
        let _cancelled = self.cancel_by_id(id);
    }
}

pub struct TransactionRef {
    pub id: u32,
    ptr: Weak<TransactionInner>,
}
impl TransactionRef {
    fn upgrade(&self) -> Option<Transaction> {
        self.ptr.upgrade()
    }
}
impl PartialOrd for TransactionRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for TransactionRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}
impl PartialEq for TransactionRef {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for TransactionRef {}

#[inline(always)]
fn progress_as_int(progress: f64) -> u16 {
    u16::min((progress * 10000.) as u16, 10000)
}

const STATE_RUNNING: u8 = 0;
const STATE_FINISHED: u8 = 1;
const STATE_ERRORED: u8 = 2;
const STATE_CANCELLED: u8 = 3;

pub type Transaction = Arc<TransactionInner>;
pub struct TransactionInner {
    pub id: u32,
    state: AtomicU8,
    shared: Arc<TransactionsShared>,
}
impl std::fmt::Debug for TransactionInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransactionInner")
            .field("id", &self.id)
            .field("aborted", &self.aborted())
            .finish_non_exhaustive()
    }
}
impl TransactionInner {
    fn emit(&self, event: TransactionEvent) {
        if self.shared.cli_mode {
            return;
        }

        self.emit_desktop(event);
    }

    fn emit_desktop(&self, event: TransactionEvent) {
        self.shared.sink.emit(BackendEvent::Transaction(event));
    }

    /// Attempts the one-way Running -> Terminal transition. Returns `Ok(())`
    /// if this call won it (the caller may now emit its terminal message);
    /// `Err(existing)` if another call already finalized the transaction
    /// first, naming the state that won.
    fn try_finalize(&self, target: u8) -> Result<(), u8> {
        self.state
            .compare_exchange(STATE_RUNNING, target, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| {
                let mut registry = self.shared.registry.write();
                if let Ok(pos) =
                    registry.binary_search_by_key(&self.id, |transaction| transaction.id)
                {
                    registry.remove(pos);
                }
            })
    }

    pub fn data(&self, payload: TransactionPayload) {
        self.emit(TransactionEvent::Data {
            id: self.id,
            payload,
        });
    }

    pub fn status<S: Into<String>>(&self, status: S) {
        self.emit(TransactionEvent::Status {
            id: self.id,
            status: status.into(),
        });
    }

    pub fn progress(&self, progress: f64) {
        if self.aborted() {
            log::warn!("Tried to progress an aborted transaction!");
        } else {
            self.emit(TransactionEvent::Progress {
                id: self.id,
                progress: progress_as_int(progress),
            });
        }
    }

    pub fn progress_incr(&self, progress: f64) {
        if self.aborted() {
            log::warn!("Tried to progress an aborted transaction!");
        } else {
            self.emit(TransactionEvent::IncrProgress {
                id: self.id,
                incr: progress_as_int(progress),
            });
        }
    }

    pub fn progress_reset(&self) {
        if self.aborted() {
            log::warn!("Tried to reset the progress of an aborted transaction!");
        } else {
            self.emit(TransactionEvent::ResetProgress { id: self.id });
        }
    }

    /// Finalizes with an error. A no-op if the transaction is already
    /// terminal: only a concurrent [`Self::cancel`] is a legitimate reason
    /// for that (asserted below), everything else double-finalizing is a bug.
    pub fn error(&self, error: impl Into<TransactionError>) {
        if let Err(existing) = self.try_finalize(STATE_ERRORED) {
            debug_assert_eq!(
                existing, STATE_CANCELLED,
                "Tried to error an already-finished transaction!"
            );
            return;
        }
        self.emit(TransactionEvent::Error {
            id: self.id,
            error: error.into(),
        });
    }

    /// Finalizes as finished. Same no-op-unless-cancelled contract as
    /// [`Self::error`].
    pub fn finished(&self, payload: TransactionPayload) {
        if let Err(existing) = self.try_finalize(STATE_FINISHED) {
            debug_assert_eq!(
                existing, STATE_CANCELLED,
                "Tried to finish an already-finished transaction!"
            );
            return;
        }
        self.emit(TransactionEvent::Finished {
            id: self.id,
            payload,
        });
    }

    /// Requests cancellation. Returns whether this call actually finalized
    /// the transaction: losing the race to a concurrent [`Self::finished`]
    /// or [`Self::error`] is expected (the work already completed) and not
    /// a bug, so callers get a plain `bool` rather than an assertion.
    pub fn cancel(&self) -> bool {
        let Ok(()) = self.try_finalize(STATE_CANCELLED) else {
            return false;
        };
        self.emit(TransactionEvent::Error {
            id: self.id,
            error: TransactionError::new(crate::error_key::keys::CANCELLED),
        });
        true
    }

    pub fn aborted(&self) -> bool {
        self.state.load(Ordering::Acquire) != STATE_RUNNING
    }
}
impl Drop for TransactionInner {
    fn drop(&mut self) {
        if !self.aborted() {
            self.error(crate::error_key::keys::UNKNOWN);

            #[cfg(debug_assertions)]
            log::debug!("{}", std::backtrace::Backtrace::force_capture());
        }
    }
}
impl serde::Serialize for TransactionInner {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u64(self.id as u64)
    }
}

pub(crate) fn detail_from_serialize<D: Serialize>(data: D) -> Option<String> {
    let Ok(value) = serde_json::to_value(data) else {
        return None;
    };
    if value.is_null() {
        None
    } else if let Some(value) = value.as_str() {
        Some(value.to_owned())
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicU8};

    use super::{STATE_RUNNING, TransactionInner, TransactionsShared, progress_as_int};

    use crate::events::{BackendEvent, BackendEventCollector, TransactionEvent};

    fn shared_for_test(sink: BackendEventCollector) -> Arc<TransactionsShared> {
        Arc::new(TransactionsShared {
            registry: parking_lot::RwLock::new(Vec::new()),
            id: std::sync::atomic::AtomicU32::new(0),
            sink: Arc::new(sink),
            cli_mode: false,
        })
    }

    #[test]
    fn progress_quantization_matches_upstream_range() {
        assert_eq!(progress_as_int(0.0), 0);
        assert_eq!(progress_as_int(0.0001), 1);
        assert_eq!(progress_as_int(0.12345), 1234);
        assert_eq!(progress_as_int(1.0), 10000);
        assert_eq!(progress_as_int(1.5), 10000);
    }

    #[test]
    fn transaction_emit_collects_typed_event_on_primary_path() {
        let collector = BackendEventCollector::default();

        let transaction = TransactionInner {
            id: 42,
            state: AtomicU8::new(STATE_RUNNING),
            shared: shared_for_test(collector.clone()),
        };
        transaction.emit_desktop(TransactionEvent::Status {
            id: transaction.id,
            status: "packing".to_owned(),
        });

        assert_eq!(
            collector.drain(),
            vec![BackendEvent::Transaction(TransactionEvent::Status {
                id: transaction.id,
                status: "packing".to_owned(),
            })]
        );
    }

    #[test]
    fn cancel_transaction_by_id_aborts_registered_transaction() {
        let transactions =
            super::Transactions::new(Arc::new(BackendEventCollector::default()), false);
        let transaction = transactions.begin();

        assert!(transactions.cancel_by_id(transaction.id));
        assert!(transaction.aborted());
        assert!(!transactions.cancel_by_id(transaction.id));
        assert!(!transactions.cancel_by_id(u32::MAX));
    }

    #[test]
    #[should_panic(expected = "Tried to error an already-finished transaction")]
    fn erroring_an_already_finished_transaction_is_flagged_as_misuse() {
        let transactions =
            super::Transactions::new(Arc::new(BackendEventCollector::default()), false);
        let transaction = transactions.begin();

        transaction.finished(super::TransactionPayload::None);
        // Not a race with `cancel`: nothing should have called this a second
        // time, and the debug assertion exists to catch exactly that.
        transaction.error(crate::error_key::keys::UNKNOWN);
    }

    #[test]
    fn cancel_loses_to_an_already_finished_transaction() {
        let transaction = TransactionInner {
            id: 7,
            state: AtomicU8::new(STATE_RUNNING),
            shared: shared_for_test(BackendEventCollector::default()),
        };

        transaction.finished(super::TransactionPayload::None);

        assert!(!transaction.cancel());
        assert!(transaction.aborted());
    }

    #[test]
    fn cancel_wins_against_a_later_finish_and_no_finished_follows() {
        let collector = BackendEventCollector::default();
        let transaction = TransactionInner {
            id: 9,
            state: AtomicU8::new(STATE_RUNNING),
            shared: shared_for_test(collector.clone()),
        };

        assert!(transaction.cancel());
        // A worker that only notices cancellation after the fact must not
        // also deliver a contradicting Finished.
        transaction.finished(super::TransactionPayload::None);

        assert_eq!(
            collector.drain(),
            vec![BackendEvent::Transaction(TransactionEvent::Error {
                id: transaction.id,
                error: super::TransactionError::new(crate::error_key::keys::CANCELLED),
            })]
        );
    }
}
