mod payload;

use parking_lot::RwLock;
use serde::Serialize;
use std::fmt;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU8, AtomicU32, Ordering},
};

use crate::error_key::{ErrorKey, HasErrorKey};
use crate::events::{BackendEvent, BackendEventSink, TransactionEvent};

pub use self::payload::TransactionPayload;

/// What a long-running operation is currently doing, as the UI names it.
///
/// A closed set rather than free text: the app renders a status by looking its
/// [`translation_key`](Self::translation_key) up in the Fluent catalogs, so a
/// value with no entry reaches the user as raw wire text. Being an enum is
/// what makes that unrepresentable — an undeclared status is a compile error
/// rather than something a catalog-coverage test has to notice afterwards.
///
/// The keys are frozen. Renaming a variant is free; changing what
/// `translation_key` returns silently breaks localization in twelve catalogs.
/// Their inconsistent shape (`locating` beside `PUBLISH_STARTING`) is part of
/// what is frozen.
///
/// Covers both sides of the bridge. The backend raises most of these from
/// inside a transaction; the app raises [`Self::Downloading`],
/// [`Self::Extracting`], [`Self::Searching`] and [`Self::Notice`] for work it
/// runs itself, and both arrive at the same task overlay.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TransactionStatus {
    Locating,
    Decompressing,
    ReadingMetadata,
    Downloading,
    Extracting,
    Searching,
    PublishStarting,
    PublishProcessingIcon,
    PublishPacking,
    PublishPreparingConfig,
    PublishPreparingContent,
    PublishUploadingContent,
    PublishUploadingPreviewFile,
    PublishCommittingChanges,
    /// Raised only by the developer context menu, but still a catalog key, so
    /// the coverage test enumerates it either way.
    Notice,
}

impl TransactionStatus {
    /// Every status, for the catalog-coverage test to walk. Kept exhaustive by
    /// [`Self::translation_key`]'s match rather than by review: a new variant
    /// fails to compile until it is spelled there.
    pub const ALL: &'static [Self] = &[
        Self::Locating,
        Self::Decompressing,
        Self::ReadingMetadata,
        Self::Downloading,
        Self::Extracting,
        Self::Searching,
        Self::PublishStarting,
        Self::PublishProcessingIcon,
        Self::PublishPacking,
        Self::PublishPreparingConfig,
        Self::PublishPreparingContent,
        Self::PublishUploadingContent,
        Self::PublishUploadingPreviewFile,
        Self::PublishCommittingChanges,
        Self::Notice,
    ];

    /// The Fluent message id this status renders as. Frozen — see the type doc.
    #[must_use]
    pub const fn translation_key(self) -> &'static str {
        match self {
            Self::Locating => "locating",
            Self::Decompressing => "decompressing",
            Self::ReadingMetadata => "reading_metadata",
            Self::Downloading => "downloading",
            Self::Extracting => "extracting_progress",
            Self::Searching => "searching",
            Self::PublishStarting => "PUBLISH_STARTING",
            Self::PublishProcessingIcon => "PUBLISH_PROCESSING_ICON",
            Self::PublishPacking => "PUBLISH_PACKING",
            Self::PublishPreparingConfig => "PUBLISH_PREPARING_CONFIG",
            Self::PublishPreparingContent => "PUBLISH_PREPARING_CONTENT",
            Self::PublishUploadingContent => "PUBLISH_UPLOADING_CONTENT",
            Self::PublishUploadingPreviewFile => "PUBLISH_UPLOADING_PREVIEW_FILE",
            Self::PublishCommittingChanges => "PUBLISH_COMMITTING_CHANGES",
            Self::Notice => "context-menu-debug-toast-notice",
        }
    }
}

impl fmt::Display for TransactionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.translation_key())
    }
}

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
                registry: RwLock::new(Vec::new()),
                id: AtomicU32::new(0),
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
        let id = TransactionId(self.shared.id.fetch_add(1, Ordering::SeqCst));
        let transaction = Arc::new(TransactionInner {
            id,
            state: AtomicU8::new(State::Running as u8),
            shared: Arc::clone(&self.shared),
        });

        {
            let mut registry = self.shared.registry.write();
            registry.push(TransactionRef {
                id: transaction.id,
                ptr: Arc::downgrade(&transaction),
            });
        }

        Transaction(transaction)
    }

    #[must_use]
    pub fn find(&self, transaction_id: TransactionId) -> Option<Transaction> {
        let registry = self.shared.registry.read();
        if let Ok(pos) =
            registry.binary_search_by_key(&transaction_id, |transaction| transaction.id)
        {
            let transaction = registry.get(pos).unwrap().upgrade();
            drop(registry);
            // A failed upgrade is not a leak: the entry is removed by
            // `try_finalize`, which `Drop` reaches only after the last strong
            // reference is already gone. A concurrent lookup in that window
            // sees a live entry it cannot upgrade, and "already finished" is
            // the right answer for it.
            return transaction;
        }

        None
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
        // Bind before cancelling: `cancel` can finalize a transaction, whose
        // `Drop` takes the registry write lock to remove itself.
        let live: Vec<Transaction> = {
            let registry = self.shared.registry.read();
            registry
                .iter()
                .filter_map(TransactionRef::upgrade)
                .collect()
        };

        live.iter()
            .filter(|transaction| transaction.cancel())
            .count()
    }

    pub fn cancel_by_id(&self, id: TransactionId) -> bool {
        let Some(transaction) = self.find(id) else {
            return false;
        };
        transaction.cancel()
    }
}

pub struct TransactionRef {
    pub id: TransactionId,
    ptr: Weak<TransactionInner>,
}
impl TransactionRef {
    fn upgrade(&self) -> Option<Transaction> {
        self.ptr.upgrade().map(Transaction)
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

/// The one-way `Running -> terminal` lifecycle, stored in an [`AtomicU8`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum State {
    Running = 0,
    Finished = 1,
    Errored = 2,
    Cancelled = 3,
}

impl State {
    /// Only ever fed a discriminant this module wrote — the initial
    /// `State::Running` or a `try_finalize` `compare_exchange` — so the
    /// catch-all arm is `Running` itself, not a fallback for unknown input.
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
    shared: Arc<TransactionsShared>,
}
impl TransactionInner {
    fn emit(&self, event: TransactionEvent) {
        self.shared.sink.emit(BackendEvent::Transaction(event));
    }

    /// Attempts the one-way Running -> Terminal transition. Returns `Ok(())`
    /// if this call won it (the caller may now emit its terminal message);
    /// `Err(existing)` if another call already finalized the transaction
    /// first, naming the state that won.
    fn try_finalize(&self, target: State) -> Result<(), State> {
        self.state
            .compare_exchange(
                State::Running as u8,
                target as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(State::from_raw)
            .map(|_| {
                let mut registry = self.shared.registry.write();
                if let Ok(pos) =
                    registry.binary_search_by_key(&self.id, |transaction| transaction.id)
                {
                    registry.remove(pos);
                }
            })
    }

    fn data(&self, payload: TransactionPayload) {
        self.emit(TransactionEvent::Data {
            id: self.id,
            payload,
        });
    }

    fn status(&self, status: TransactionStatus) {
        self.emit(TransactionEvent::Status {
            id: self.id,
            status,
        });
    }

    fn progress(&self, progress: f64) {
        if self.aborted() {
            log::warn!("Tried to progress an aborted transaction!");
        } else {
            self.emit(TransactionEvent::Progress {
                id: self.id,
                progress: progress_as_int(progress),
            });
        }
    }

    fn progress_incr(&self, progress: f64) {
        if self.aborted() {
            log::warn!("Tried to progress an aborted transaction!");
        } else {
            self.emit(TransactionEvent::IncrProgress {
                id: self.id,
                incr: progress_as_int(progress),
            });
        }
    }

    fn progress_reset(&self) {
        if self.aborted() {
            log::warn!("Tried to reset the progress of an aborted transaction!");
        } else {
            self.emit(TransactionEvent::ResetProgress { id: self.id });
        }
    }

    /// Finalizes with an error. A no-op if the transaction is already
    /// terminal: only a concurrent [`Self::cancel`] is a legitimate reason
    /// for that (asserted below), everything else double-finalizing is a bug.
    fn error(&self, error: impl Into<TransactionError>) {
        if let Err(existing) = self.try_finalize(State::Errored) {
            debug_assert_eq!(
                existing,
                State::Cancelled,
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
    fn finished(&self, payload: TransactionPayload) {
        if let Err(existing) = self.try_finalize(State::Finished) {
            debug_assert_eq!(
                existing,
                State::Cancelled,
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
    fn cancel(&self) -> bool {
        let Ok(()) = self.try_finalize(State::Cancelled) else {
            return false;
        };
        self.emit(TransactionEvent::Error {
            id: self.id,
            error: TransactionError::new(crate::error_key::keys::CANCELLED),
        });
        true
    }

    fn aborted(&self) -> bool {
        State::from_raw(self.state.load(Ordering::Acquire)) != State::Running
    }
}
impl Transaction {
    #[must_use]
    pub fn id(&self) -> TransactionId {
        self.0.id
    }

    pub fn data(&self, payload: TransactionPayload) {
        self.0.data(payload);
    }

    pub fn status(&self, status: TransactionStatus) {
        self.0.status(status);
    }

    pub fn progress(&self, progress: f64) {
        self.0.progress(progress);
    }

    pub fn progress_incr(&self, progress: f64) {
        self.0.progress_incr(progress);
    }

    pub fn progress_reset(&self) {
        self.0.progress_reset();
    }

    pub fn error(&self, error: impl Into<TransactionError>) {
        self.0.error(error);
    }

    pub fn finished(&self, payload: TransactionPayload) {
        self.0.finished(payload);
    }

    /// Returns whether this call won the race to finalize; losing to a
    /// concurrent completion is expected, not an error.
    pub fn cancel(&self) -> bool {
        self.0.cancel()
    }

    #[must_use]
    pub fn aborted(&self) -> bool {
        self.0.aborted()
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

    use super::{
        State, TransactionId, TransactionInner, TransactionRef, TransactionStatus, Transactions,
        TransactionsShared, progress_as_int,
    };

    use crate::events::{BackendEvent, BackendEventCollector, TransactionEvent};

    fn shared_for_test(sink: BackendEventCollector) -> Arc<TransactionsShared> {
        Arc::new(TransactionsShared {
            registry: parking_lot::RwLock::new(Vec::new()),
            id: std::sync::atomic::AtomicU32::new(0),
            sink: Arc::new(sink),
        })
    }

    /// `find` must not treat a registry entry it cannot upgrade as a bug:
    /// `Drop` removes the entry strictly after the strong count reaches zero,
    /// so a concurrent lookup can legitimately observe one.
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

    #[test]
    fn find_returns_none_for_an_entry_that_cannot_upgrade() {
        let transactions = Transactions::new(Arc::new(crate::events::NullEventSink));
        let id = TransactionId::from_raw(9);
        transactions.shared.registry.write().push(TransactionRef {
            id,
            ptr: std::sync::Weak::new(),
        });

        assert!(transactions.find(id).is_none());
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
            shared: shared_for_test(collector.clone()),
        };
        transaction.emit(TransactionEvent::Status {
            id: transaction.id,
            status: TransactionStatus::PublishPacking,
        });

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

        assert!(transactions.cancel_by_id(transaction.id()));
        assert!(transaction.aborted());
        assert!(!transactions.cancel_by_id(transaction.id()));
        assert!(!transactions.cancel_by_id(TransactionId(u32::MAX)));
    }

    #[test]
    #[should_panic(expected = "Tried to error an already-finished transaction")]
    fn erroring_an_already_finished_transaction_is_flagged_as_misuse() {
        let transactions = super::Transactions::new(Arc::new(BackendEventCollector::default()));
        let transaction = transactions.begin();

        transaction.finished(super::TransactionPayload::None);
        // Not a race with `cancel`: nothing should have called this a second
        // time, and the debug assertion exists to catch exactly that.
        transaction.error(crate::error_key::keys::UNKNOWN);
    }

    #[test]
    fn cancel_loses_to_an_already_finished_transaction() {
        let transaction = TransactionInner {
            id: TransactionId(7),
            state: AtomicU8::new(State::Running as u8),
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
            id: TransactionId(9),
            state: AtomicU8::new(State::Running as u8),
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
