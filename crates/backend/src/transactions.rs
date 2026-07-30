//! Progress reporting for long-running work: a [`Transaction`] is the handle
//! an operation reports status, progress and completion through, and
//! [`Transactions`] mints them and routes their events to the app.
//!
//! Each transaction has one serialized event stream: zero or more running
//! events, exactly one terminal event, then nothing. Cancellation is
//! cooperative and one-way; operations decide where to poll their own
//! checkpoints, while this module guarantees the cancellation error is the
//! last event if cancellation wins.

mod payload;

use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU8, Ordering},
};
use std::{collections::BTreeMap, fmt};

use crate::error_key::{ErrorKey, HasErrorKey};
use crate::events::{BackendEvent, BackendEventSink, TransactionEvent};

pub use self::payload::TransactionPayload;

macro_rules! transaction_statuses {
    ($($(#[$meta:meta])* $variant:ident => $key:literal),+ $(,)?) => {
        /// What a long-running operation is currently doing, as the UI names it.
        ///
        /// A closed set rather than free text: the app renders a status by looking its
        /// [`translation_key`](Self::translation_key) up in the Fluent catalogs, so a
        /// value with no entry reaches the user as raw wire text. The keys are frozen;
        /// their inconsistent shape is part of the application protocol.
        ///
        /// Covers both sides of the bridge. The backend raises most variants inside a
        /// transaction; the app raises the UI-owned work states, and both arrive at the
        /// same task overlay.
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum TransactionStatus {
            $($(#[$meta])* $variant),+
        }

        impl TransactionStatus {
            /// Every status, in declaration order. Generated with the enum so
            /// a newly declared status cannot be omitted from catalog checks.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The Fluent message id this status renders as. Frozen — see the
            /// type documentation.
            #[must_use]
            pub const fn translation_key(self) -> &'static str {
                match self {
                    $(Self::$variant => $key),+
                }
            }
        }
    };
}

transaction_statuses! {
    Locating => "locating",
    Decompressing => "decompressing",
    ReadingMetadata => "reading_metadata",
    Downloading => "downloading",
    Extracting => "extracting_progress",
    Searching => "searching",
    PublishStarting => "PUBLISH_STARTING",
    PublishProcessingIcon => "PUBLISH_PROCESSING_ICON",
    PublishPacking => "PUBLISH_PACKING",
    PublishPreparingConfig => "PUBLISH_PREPARING_CONFIG",
    PublishPreparingContent => "PUBLISH_PREPARING_CONTENT",
    PublishUploadingContent => "PUBLISH_UPLOADING_CONTENT",
    PublishUploadingPreviewFile => "PUBLISH_UPLOADING_PREVIEW_FILE",
    PublishCommittingChanges => "PUBLISH_COMMITTING_CHANGES",
    /// Raised only by the developer context menu, but still a catalog key.
    Notice => "context-menu-debug-toast-notice",
}

impl fmt::Display for TransactionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.translation_key())
    }
}

/// A failed transaction's identity on the event wire: a stable [`ErrorKey`]
/// plus optional contextual payload (a path, an entry name, an upstream
/// error message).
#[derive(Clone, Debug, Eq, PartialEq)]
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
    registry: RwLock<TransactionRegistry>,
    sink: Arc<dyn BackendEventSink>,
}

#[derive(Default)]
struct TransactionRegistry {
    live: BTreeMap<TransactionId, Weak<TransactionInner>>,
    next_id: u32,
}

impl TransactionRegistry {
    fn allocate_id(&mut self) -> TransactionId {
        let first_candidate = self.next_id;

        loop {
            let id = TransactionId(self.next_id);
            self.next_id = self.next_id.wrapping_add(1);

            if !self.live.contains_key(&id) {
                return id;
            }

            assert_ne!(
                self.next_id, first_candidate,
                "transaction ID space exhausted"
            );
        }
    }
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
    pub fn new(sink: Arc<dyn BackendEventSink>) -> Self {
        Self {
            shared: Arc::new(TransactionsShared {
                registry: RwLock::new(TransactionRegistry::default()),
                sink,
            }),
        }
    }

    /// Emits a plain (non-transaction) backend event, e.g. `SteamConnected`
    /// or `AppDataUpdated`.
    pub fn emit(&self, event: BackendEvent) {
        self.shared.sink.emit(event);
    }

    #[must_use]
    pub fn begin(&self) -> Transaction {
        let mut registry = self.shared.registry.write();
        let id = registry.allocate_id();
        let transaction = Arc::new(TransactionInner {
            id,
            state: AtomicU8::new(State::Running as u8),
            emission: Mutex::new(()),
            shared: Arc::clone(&self.shared),
        });

        registry.live.insert(id, Arc::downgrade(&transaction));
        drop(registry);

        Transaction(transaction)
    }

    #[must_use]
    pub fn find(&self, transaction_id: TransactionId) -> Option<Transaction> {
        let registry = self.shared.registry.read();
        // A failed upgrade is not a leak: `Drop` can reach finalization only
        // after the last strong reference is already gone. A concurrent
        // lookup in that window sees a live entry it cannot upgrade, and
        // "already finished" is the right answer for it.
        registry
            .live
            .get(&transaction_id)
            .and_then(Weak::upgrade)
            .map(Transaction)
    }

    /// Cancels every transaction still running, and reports how many were.
    ///
    /// The cooperative half of shutdown. Long backend operations already poll
    /// [`Transaction::aborted`] between units of work — per archive entry when
    /// packing or extracting — so cancelling here lets an in-flight job stop at
    /// its next checkpoint instead of being abandoned mid-write when the worker
    /// pools are joined. Extraction writes each entry straight to its
    /// destination, so "abandoned mid-write" means a truncated file.
    pub fn cancel_all(&self) -> usize {
        // Collect before cancelling: finalization takes the registry write
        // lock to retire each transaction.
        let live: Vec<Transaction> = {
            let registry = self.shared.registry.read();
            registry
                .live
                .values()
                .filter_map(|transaction| transaction.upgrade().map(Transaction))
                .collect()
        };

        live.iter()
            .filter(|transaction| transaction.cancel() == FinalizeOutcome::Finalized)
            .count()
    }

    /// Attempts to cancel a registered transaction.
    ///
    /// `None` means the id is no longer registered (or never existed). A
    /// present value reports which side won a concurrent terminal race.
    pub fn cancel_by_id(&self, id: TransactionId) -> Option<FinalizeOutcome> {
        self.find(id).map(|transaction| transaction.cancel())
    }
}

#[inline(always)]
fn progress_as_int(progress: f64) -> u16 {
    u16::min((progress * 10000.) as u16, 10000)
}

/// The one-way `Running -> terminal` lifecycle, stored in an [`AtomicU8`] for
/// cheap cooperative cancellation checks. Transitions are serialized by the
/// transaction's emission mutex.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum State {
    Running = 0,
    Finished = 1,
    Errored = 2,
    Cancelled = 3,
}

impl State {
    /// Only ever fed a discriminant this module wrote, so the catch-all arm is
    /// `Running` itself, not a fallback for unknown input.
    const fn from_raw(raw: u8) -> Self {
        debug_assert!(raw <= Self::Cancelled as u8, "state came from elsewhere");
        match raw {
            1 => Self::Finished,
            2 => Self::Errored,
            3 => Self::Cancelled,
            _ => Self::Running,
        }
    }
}

/// Result of attempting to emit a non-terminal transaction event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmitOutcome {
    /// The transaction was running and the event was emitted.
    Emitted,
    /// A terminal transition won first, so the event was rejected.
    AlreadyFinalized,
}

/// Result of attempting a one-way `Running -> terminal` transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizeOutcome {
    /// This call performed the terminal transition and emitted its event.
    Finalized,
    /// Cancellation had already performed the terminal transition.
    AlreadyCancelled,
    /// Successful or failed completion had already performed the transition.
    AlreadyCompleted,
}

/// Identifies one transaction for its whole lifetime. The correlation key the
/// app matches backend progress events against its own tasks.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TransactionId(u32);

impl TransactionId {
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Only [`Transactions::begin`] mints these in production; tests need to
    /// name one that never existed.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub const fn from_raw(id: u32) -> Self {
        Self(id)
    }
}

impl fmt::Display for TransactionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A handle to one in-flight transaction. Cloning shares the same underlying
/// state machine, so any clone can finalize it.
#[derive(Clone)]
pub struct Transaction(Arc<TransactionInner>);

impl std::fmt::Debug for Transaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transaction")
            .field("id", &self.0.id)
            .field("aborted", &self.aborted())
            .finish_non_exhaustive()
    }
}

struct TransactionInner {
    id: TransactionId,
    state: AtomicU8,
    /// Serializes the running-state check with every event emission. This is
    /// the structural guarantee that a terminal event is last, even when a
    /// worker clone races cancellation.
    emission: Mutex<()>,
    shared: Arc<TransactionsShared>,
}
impl TransactionInner {
    fn emit(&self, event: TransactionEvent) {
        self.shared.sink.emit(BackendEvent::Transaction(event));
    }

    fn emit_while_running(&self, event: TransactionEvent) -> EmitOutcome {
        let _emission = self.emission.lock();
        if self.aborted() {
            return EmitOutcome::AlreadyFinalized;
        }

        self.emit(event);
        EmitOutcome::Emitted
    }

    fn finalize(&self, target: State, event: TransactionEvent) -> FinalizeOutcome {
        debug_assert_ne!(target, State::Running);

        let _emission = self.emission.lock();
        match State::from_raw(self.state.load(Ordering::Acquire)) {
            State::Running => {
                // The mutex is the sole writer discipline. Store the terminal
                // state before publishing the terminal event so cancellation
                // polling observes completion no later than event consumers.
                self.state.store(target as u8, Ordering::Release);
                self.shared.registry.write().live.remove(&self.id);
                self.emit(event);
                FinalizeOutcome::Finalized
            }
            State::Cancelled => FinalizeOutcome::AlreadyCancelled,
            State::Finished | State::Errored => FinalizeOutcome::AlreadyCompleted,
        }
    }

    fn data(&self, payload: TransactionPayload) -> EmitOutcome {
        self.emit_while_running(TransactionEvent::Data {
            id: self.id,
            payload,
        })
    }

    fn status(&self, status: TransactionStatus) -> EmitOutcome {
        self.emit_while_running(TransactionEvent::Status {
            id: self.id,
            status,
        })
    }

    fn progress(&self, progress: f64) -> EmitOutcome {
        self.emit_while_running(TransactionEvent::Progress {
            id: self.id,
            progress: progress_as_int(progress),
        })
    }

    fn progress_incr(&self, progress: f64) -> EmitOutcome {
        self.emit_while_running(TransactionEvent::IncrProgress {
            id: self.id,
            incr: progress_as_int(progress),
        })
    }

    fn progress_reset(&self) -> EmitOutcome {
        self.emit_while_running(TransactionEvent::ResetProgress { id: self.id })
    }

    fn error(&self, error: impl Into<TransactionError>) -> FinalizeOutcome {
        self.finalize(
            State::Errored,
            TransactionEvent::Error {
                id: self.id,
                error: error.into(),
            },
        )
    }

    fn finished(&self, payload: TransactionPayload) -> FinalizeOutcome {
        self.finalize(
            State::Finished,
            TransactionEvent::Finished {
                id: self.id,
                payload,
            },
        )
    }

    fn cancel(&self) -> FinalizeOutcome {
        self.finalize(
            State::Cancelled,
            TransactionEvent::Error {
                id: self.id,
                error: TransactionError::new(crate::error_key::keys::CANCELLED),
            },
        )
    }

    fn aborted(&self) -> bool {
        State::from_raw(self.state.load(Ordering::Acquire)) != State::Running
    }

    fn cancelled(&self) -> bool {
        State::from_raw(self.state.load(Ordering::Acquire)) == State::Cancelled
    }
}
impl Transaction {
    #[must_use]
    pub fn id(&self) -> TransactionId {
        self.0.id
    }

    /// Emits data if this transaction is still running.
    pub fn data(&self, payload: TransactionPayload) -> EmitOutcome {
        self.0.data(payload)
    }

    /// Emits a status if this transaction is still running.
    pub fn status(&self, status: TransactionStatus) -> EmitOutcome {
        self.0.status(status)
    }

    /// Sets progress if this transaction is still running.
    pub fn progress(&self, progress: f64) -> EmitOutcome {
        self.0.progress(progress)
    }

    /// Increments progress if this transaction is still running.
    pub fn progress_incr(&self, progress: f64) -> EmitOutcome {
        self.0.progress_incr(progress)
    }

    /// Resets progress if this transaction is still running.
    pub fn progress_reset(&self) -> EmitOutcome {
        self.0.progress_reset()
    }

    /// Attempts to finish this transaction with an error.
    pub fn error(&self, error: impl Into<TransactionError>) -> FinalizeOutcome {
        self.0.error(error)
    }

    /// Attempts to finish this transaction successfully.
    pub fn finished(&self, payload: TransactionPayload) -> FinalizeOutcome {
        self.0.finished(payload)
    }

    /// Runs fallible transaction work and performs its matching terminal
    /// transition exactly once on the normal path.
    ///
    /// The original result is returned unchanged. If cancellation wins while
    /// `work` is running, the attempted success or error transition is rejected
    /// by the transaction state machine, leaving cancellation as the terminal
    /// event.
    pub fn complete<T, E>(
        &self,
        work: impl FnOnce() -> Result<T, E>,
        success_payload: impl FnOnce(&T) -> TransactionPayload,
    ) -> Result<T, E>
    where
        E: HasErrorKey,
    {
        let result = work();
        match &result {
            Ok(value) => {
                self.finished(success_payload(value));
            }
            Err(error) => {
                self.error(TransactionError::from(error));
            }
        }
        result
    }

    /// Attempts to cancel this transaction.
    pub fn cancel(&self) -> FinalizeOutcome {
        self.0.cancel()
    }

    #[must_use]
    pub fn aborted(&self) -> bool {
        self.0.aborted()
    }

    /// Reports whether cancellation, specifically, won the terminal transition.
    ///
    /// Unlike [`Self::aborted`], successful and errored completion return
    /// `false`. Terminal states never change, so a `false` result after this
    /// transaction has attempted completion cannot race with a later cancel.
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.0.cancelled()
    }
}

impl Drop for TransactionInner {
    fn drop(&mut self) {
        if !self.aborted() {
            let outcome = self.error(crate::error_key::keys::UNKNOWN);
            debug_assert_eq!(outcome, FinalizeOutcome::Finalized);

            #[cfg(debug_assertions)]
            log::debug!("{}", std::backtrace::Backtrace::force_capture());
        }
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
    use std::sync::{Arc, Barrier, atomic::AtomicU8};

    use super::{
        EmitOutcome, FinalizeOutcome, State, TransactionId, TransactionInner, TransactionPayload,
        TransactionRegistry, TransactionStatus, Transactions, TransactionsShared, progress_as_int,
    };

    use crate::events::{BackendEvent, BackendEventCollector, TransactionEvent};

    fn shared_for_test(sink: BackendEventCollector) -> Arc<TransactionsShared> {
        Arc::new(TransactionsShared {
            registry: parking_lot::RwLock::new(TransactionRegistry::default()),
            sink: Arc::new(sink),
        })
    }

    /// Shutdown's cooperative half: every live transaction is cancelled, so a
    /// job polling `aborted()` stops at its own checkpoint rather than being
    /// abandoned mid-write when the pools are joined.
    #[test]
    fn cancel_all_cancels_every_live_transaction() {
        let transactions = Transactions::new(Arc::new(crate::events::NullEventSink));
        let first = transactions.begin();
        let second = transactions.begin();

        assert_eq!(transactions.cancel_all(), 2);
        assert!(first.aborted());
        assert!(second.aborted());
    }

    /// Already-finished transactions are not counted, and cancelling twice
    /// does not double-count — quit can reach this after a user cancel.
    #[test]
    fn cancel_all_ignores_transactions_that_already_finished() {
        let transactions = Transactions::new(Arc::new(crate::events::NullEventSink));
        let done = transactions.begin();
        done.finished(crate::transactions::TransactionPayload::None);
        let running = transactions.begin();

        assert_eq!(transactions.cancel_all(), 1);
        assert_eq!(transactions.cancel_all(), 0);
        assert!(running.aborted());
    }

    /// `find` must not treat a registry entry it cannot upgrade as a bug:
    /// `Drop` removes the entry strictly after the strong count reaches zero,
    /// so a concurrent lookup can legitimately observe one.
    #[test]
    fn find_returns_none_for_an_entry_that_cannot_upgrade() {
        let transactions = Transactions::new(Arc::new(crate::events::NullEventSink));
        let id = TransactionId::from_raw(9);
        transactions
            .shared
            .registry
            .write()
            .live
            .insert(id, std::sync::Weak::new());

        assert!(transactions.find(id).is_none());
    }

    #[test]
    fn id_allocation_skips_registered_ids_after_wraparound() {
        let mut registry = TransactionRegistry {
            next_id: u32::MAX,
            ..TransactionRegistry::default()
        };
        registry
            .live
            .insert(TransactionId(0), std::sync::Weak::new());

        assert_eq!(registry.allocate_id(), TransactionId(u32::MAX));
        assert_eq!(registry.allocate_id(), TransactionId(1));
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
            id: TransactionId(42),
            state: AtomicU8::new(State::Running as u8),
            emission: parking_lot::Mutex::new(()),
            shared: shared_for_test(collector.clone()),
        };
        assert_eq!(
            transaction.status(TransactionStatus::PublishPacking),
            EmitOutcome::Emitted
        );

        assert_eq!(
            collector.drain(),
            vec![BackendEvent::Transaction(TransactionEvent::Status {
                id: transaction.id,
                status: TransactionStatus::PublishPacking,
            })]
        );
    }

    #[test]
    fn cancel_transaction_by_id_aborts_registered_transaction() {
        let transactions = super::Transactions::new(Arc::new(BackendEventCollector::default()));
        let transaction = transactions.begin();

        assert_eq!(
            transactions.cancel_by_id(transaction.id()),
            Some(FinalizeOutcome::Finalized)
        );
        assert!(transaction.aborted());
        assert_eq!(transactions.cancel_by_id(transaction.id()), None);
        assert_eq!(transactions.cancel_by_id(TransactionId(u32::MAX)), None);
    }

    #[test]
    fn repeated_completion_reports_an_explicit_outcome() {
        let transactions = super::Transactions::new(Arc::new(BackendEventCollector::default()));
        let transaction = transactions.begin();

        assert_eq!(
            transaction.finished(super::TransactionPayload::None),
            FinalizeOutcome::Finalized
        );
        assert_eq!(
            transaction.error(crate::error_key::keys::UNKNOWN),
            FinalizeOutcome::AlreadyCompleted
        );
    }

    #[test]
    fn complete_maps_success_to_exactly_one_finished_event() {
        let collector = BackendEventCollector::default();
        let transactions = Transactions::new(Arc::new(collector.clone()));
        let transaction = transactions.begin();
        let id = transaction.id();

        let result = transaction.complete(
            || Ok::<_, crate::GmaError>(42_u64),
            |bytes| TransactionPayload::TotalBytes(*bytes),
        );

        assert_eq!(result, Ok(42));
        assert!(!transaction.cancelled());
        assert_eq!(
            collector.drain(),
            vec![BackendEvent::Transaction(TransactionEvent::Finished {
                id,
                payload: TransactionPayload::TotalBytes(42),
            })]
        );
    }

    #[test]
    fn complete_preserves_the_error_and_emits_it_exactly_once() {
        let collector = BackendEventCollector::default();
        let transactions = Transactions::new(Arc::new(collector.clone()));
        let transaction = transactions.begin();
        let id = transaction.id();

        let result = transaction.complete(
            || Err::<(), _>(crate::GmaError::FormatError),
            |_| TransactionPayload::None,
        );

        assert_eq!(result, Err(crate::GmaError::FormatError));
        assert!(!transaction.cancelled());
        assert_eq!(
            collector.drain(),
            vec![BackendEvent::Transaction(TransactionEvent::Error {
                id,
                error: super::TransactionError::new(crate::error_key::keys::GMA_FORMAT_ERROR),
            })]
        );
    }

    #[test]
    fn cancellation_remains_the_winner_when_complete_returns_later() {
        let collector = BackendEventCollector::default();
        let transactions = Transactions::new(Arc::new(collector.clone()));
        let transaction = transactions.begin();
        let id = transaction.id();
        let cancelling_transaction = transaction.clone();

        let result = transaction.complete(
            || {
                assert_eq!(cancelling_transaction.cancel(), FinalizeOutcome::Finalized);
                Ok::<_, crate::GmaError>(())
            },
            |_| TransactionPayload::None,
        );

        assert_eq!(result, Ok(()));
        assert!(transaction.cancelled());
        assert_eq!(
            collector.drain(),
            vec![BackendEvent::Transaction(TransactionEvent::Error {
                id,
                error: super::TransactionError::new(crate::error_key::keys::CANCELLED),
            })]
        );
    }

    #[test]
    fn cancel_loses_to_an_already_finished_transaction() {
        let transaction = TransactionInner {
            id: TransactionId(7),
            state: AtomicU8::new(State::Running as u8),
            emission: parking_lot::Mutex::new(()),
            shared: shared_for_test(BackendEventCollector::default()),
        };

        assert_eq!(
            transaction.finished(super::TransactionPayload::None),
            FinalizeOutcome::Finalized
        );

        assert_eq!(transaction.cancel(), FinalizeOutcome::AlreadyCompleted);
        assert!(transaction.aborted());
    }

    #[test]
    fn cancel_wins_against_a_later_finish_and_no_finished_follows() {
        let collector = BackendEventCollector::default();
        let transaction = TransactionInner {
            id: TransactionId(9),
            state: AtomicU8::new(State::Running as u8),
            emission: parking_lot::Mutex::new(()),
            shared: shared_for_test(collector.clone()),
        };

        assert_eq!(transaction.cancel(), FinalizeOutcome::Finalized);
        // A worker that only notices cancellation after the fact must not
        // also deliver a contradicting Finished.
        assert_eq!(
            transaction.finished(super::TransactionPayload::None),
            FinalizeOutcome::AlreadyCancelled
        );

        assert_eq!(
            collector.drain(),
            vec![BackendEvent::Transaction(TransactionEvent::Error {
                id: transaction.id,
                error: super::TransactionError::new(crate::error_key::keys::CANCELLED),
            })]
        );
    }

    #[test]
    fn every_ongoing_event_is_rejected_after_termination() {
        let collector = BackendEventCollector::default();
        let transaction = TransactionInner {
            id: TransactionId(10),
            state: AtomicU8::new(State::Running as u8),
            emission: parking_lot::Mutex::new(()),
            shared: shared_for_test(collector.clone()),
        };

        assert_eq!(
            transaction.finished(super::TransactionPayload::None),
            FinalizeOutcome::Finalized
        );
        assert_eq!(
            transaction.data(super::TransactionPayload::None),
            EmitOutcome::AlreadyFinalized
        );
        assert_eq!(
            transaction.status(TransactionStatus::PublishPacking),
            EmitOutcome::AlreadyFinalized
        );
        assert_eq!(transaction.progress(0.5), EmitOutcome::AlreadyFinalized);
        assert_eq!(
            transaction.progress_incr(0.1),
            EmitOutcome::AlreadyFinalized
        );
        assert_eq!(transaction.progress_reset(), EmitOutcome::AlreadyFinalized);

        assert_eq!(collector.drain().len(), 1);
    }

    #[test]
    fn terminal_event_is_structurally_last_when_emissions_race() {
        let collector = BackendEventCollector::default();
        let transaction = Arc::new(TransactionInner {
            id: TransactionId(11),
            state: AtomicU8::new(State::Running as u8),
            emission: parking_lot::Mutex::new(()),
            shared: shared_for_test(collector.clone()),
        });
        let start = Arc::new(Barrier::new(5));
        let mut workers = Vec::new();

        for _ in 0..4 {
            let transaction = Arc::clone(&transaction);
            let start = Arc::clone(&start);
            workers.push(std::thread::spawn(move || {
                start.wait();
                for _ in 0..1_000 {
                    let _outcome = transaction.status(TransactionStatus::PublishPacking);
                }
            }));
        }

        start.wait();
        assert_eq!(
            transaction.finished(super::TransactionPayload::None),
            FinalizeOutcome::Finalized
        );
        for worker in workers {
            worker.join().expect("emission worker");
        }

        let events = collector.drain();
        assert!(matches!(
            events.last(),
            Some(BackendEvent::Transaction(TransactionEvent::Finished { .. }))
        ));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    BackendEvent::Transaction(
                        TransactionEvent::Finished { .. } | TransactionEvent::Error { .. }
                    )
                ))
                .count(),
            1
        );
    }
}
