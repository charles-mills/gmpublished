//! Progress and completion events for long-running work.
//! Each transaction emits running events followed by exactly one terminal event.
//! Cancellation is cooperative and irreversible; if it wins, its error is terminal.

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
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum TransactionStatus {
            $($(#[$meta])* $variant),+
        }

        impl TransactionStatus {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

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
    Notice => "context-menu-debug-toast-notice",
}

impl fmt::Display for TransactionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.translation_key())
    }
}

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
        registry
            .live
            .get(&transaction_id)
            .and_then(Weak::upgrade)
            .map(Transaction)
    }

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

/// One-way Running to terminal state. The emission mutex serializes transitions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum State {
    Running = 0,
    Finished = 1,
    Errored = 2,
    Cancelled = 3,
}

impl State {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmitOutcome {
    Emitted,
    AlreadyFinalized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizeOutcome {
    Finalized,
    AlreadyCancelled,
    AlreadyCompleted,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TransactionId(u32);

impl TransactionId {
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

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

    pub fn data(&self, payload: TransactionPayload) -> EmitOutcome {
        self.0.data(payload)
    }

    pub fn status(&self, status: TransactionStatus) -> EmitOutcome {
        self.0.status(status)
    }

    pub fn progress(&self, progress: f64) -> EmitOutcome {
        self.0.progress(progress)
    }

    pub fn progress_incr(&self, progress: f64) -> EmitOutcome {
        self.0.progress_incr(progress)
    }

    pub fn progress_reset(&self) -> EmitOutcome {
        self.0.progress_reset()
    }

    pub fn error(&self, error: impl Into<TransactionError>) -> FinalizeOutcome {
        self.0.error(error)
    }

    pub fn finished(&self, payload: TransactionPayload) -> FinalizeOutcome {
        self.0.finished(payload)
    }

    /// Returns the work's result even if cancellation wins. In that case,
    /// the cancellation event remains terminal.
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

    pub fn cancel(&self) -> FinalizeOutcome {
        self.0.cancel()
    }

    #[must_use]
    pub fn aborted(&self) -> bool {
        self.0.aborted()
    }

    /// Only cancellation returns true; `aborted` also includes success and error.
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

    #[test]
    fn cancel_all_cancels_every_live_transaction() {
        let transactions = Transactions::new(Arc::new(crate::events::NullEventSink));
        let first = transactions.begin();
        let second = transactions.begin();

        assert_eq!(transactions.cancel_all(), 2);
        assert!(first.aborted());
        assert!(second.aborted());
    }

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
