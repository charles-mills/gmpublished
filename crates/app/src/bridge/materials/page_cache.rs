use super::{
    ReloadableTextureSource, TextureAssetError, TextureFrameRequest, TexturePage,
    TexturePageIdentity, TextureWorkingBudget,
};
use crate::preparation::PreparationCancellation;
use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, VecDeque};
use std::sync::{
    Arc, OnceLock, Weak,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct PageCachePolicy {
    pub resident_bytes: usize,
    pub max_entries: usize,
    pub queued_jobs: usize,
    pub workers: usize,
}
impl Default for PageCachePolicy {
    fn default() -> Self {
        Self {
            resident_bytes: 1024 * 1024 * 1024,
            max_entries: 4096,
            queued_jobs: 128,
            workers: 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PageCacheInitError {
    #[error("page cache requires nonzero worker, queue and entry limits")]
    InvalidPolicy,
    #[error("texture page worker could not start: {0}")]
    Worker(Arc<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PageRetry {
    ResidentBytes { required: usize, capacity: usize },
    Queue { capacity: usize },
    Entries { capacity: usize },
    WorkingBytes { required: usize, capacity: usize },
    Cancelled,
    Closed,
}

#[derive(Clone, Debug)]
pub enum PageStatus {
    Pending,
    Ready(Arc<CachedTexturePage>),
    Failed(Arc<TextureAssetError>),
    PendingRetry(PageRetry),
}

#[derive(Debug)]
pub struct CachedTexturePage {
    page: TexturePage,
    _resident: ResidentPermit,
}
impl CachedTexturePage {
    pub fn request(&self) -> &TextureFrameRequest {
        self.page.request()
    }
    pub fn resident(&self) -> &super::ResidentTexture {
        self.page.resident()
    }
}

#[derive(Clone, Debug)]
pub struct PageSubscription {
    pub id: u64,
    pub receiver: watch::Receiver<u64>,
}

#[derive(Debug)]
struct Signal {
    id: u64,
    sender: watch::Sender<u64>,
}
impl Signal {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            sender: watch::channel(0).0,
        }
    }
    fn notify(&self) {
        self.sender
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
    fn subscribe(&self) -> PageSubscription {
        PageSubscription {
            id: self.id,
            receiver: self.sender.subscribe(),
        }
    }
}

#[derive(Debug)]
struct Ledger {
    capacity: usize,
    reserved: AtomicUsize,
    peak: AtomicUsize,
    signal: Arc<Signal>,
}
impl Ledger {
    fn reserve(self: &Arc<Self>, bytes: usize) -> Option<ResidentPermit> {
        let previous = self
            .reserved
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|&total| total <= self.capacity)
            })
            .ok()?;
        self.peak.fetch_max(previous + bytes, Ordering::Relaxed);
        Some(ResidentPermit {
            ledger: Arc::clone(self),
            bytes,
        })
    }
}
#[derive(Debug)]
struct ResidentPermit {
    ledger: Arc<Ledger>,
    bytes: usize,
}
impl Drop for ResidentPermit {
    fn drop(&mut self) {
        self.ledger.reserved.fetch_sub(self.bytes, Ordering::AcqRel);
        self.ledger.signal.notify();
    }
}

#[derive(Debug)]
struct Flight {
    status: Mutex<PageStatus>,
    cancellation: PreparationCancellation,
}
#[derive(Debug)]
struct Entry {
    flight: Arc<Flight>,
    source: Weak<ReloadableTextureSource>,
    interests: usize,
    touched: u64,
}
#[derive(Debug)]
struct Job {
    source: Weak<ReloadableTextureSource>,
    request: TextureFrameRequest,
    flight: Arc<Flight>,
    permit: ResidentPermit,
}
#[derive(Debug, Default)]
struct State {
    entries: HashMap<TexturePageIdentity, Entry>,
    jobs: VecDeque<Job>,
    clock: u64,
    running: usize,
    closed: bool,
}
#[derive(Debug)]
struct Inner {
    policy: PageCachePolicy,
    working: Arc<TextureWorkingBudget>,
    ledger: Arc<Ledger>,
    signal: Arc<Signal>,
    state: Mutex<State>,
    available: Condvar,
}

#[derive(Debug)]
pub struct TexturePageCache {
    inner: Arc<Inner>,
}

#[derive(Clone, Debug)]
pub struct PageTicket {
    interest: Arc<Interest>,
}
#[derive(Debug)]
struct Interest {
    _source: Option<Arc<ReloadableTextureSource>>,
    cache: Weak<Inner>,
    request: TextureFrameRequest,
    flight: Arc<Flight>,
}
impl PageTicket {
    pub fn status(&self) -> PageStatus {
        self.interest.flight.status.lock().clone()
    }
}
impl Drop for Interest {
    fn drop(&mut self) {
        let Some(cache) = self.cache.upgrade() else {
            return;
        };
        let (cancel, removed_job) = {
            let mut state = cache.state.lock();
            let Some(entry) = state
                .entries
                .get_mut(&self.request.identity())
                .filter(|entry| Arc::ptr_eq(&entry.flight, &self.flight))
            else {
                return;
            };
            entry.interests -= 1;
            let cancel =
                entry.interests == 0 && matches!(*entry.flight.status.lock(), PageStatus::Pending);
            let removed_job = if cancel {
                state.entries.remove(&self.request.identity());
                let position = state
                    .jobs
                    .iter()
                    .position(|job| Arc::ptr_eq(&job.flight, &self.flight));
                position.and_then(|position| state.jobs.remove(position))
            } else {
                None
            };
            drop(state);
            (cancel, removed_job)
        };
        if cancel {
            self.flight.cancellation.cancel();
        }
        drop(removed_job);
        cache.signal.notify();
    }
}

#[cfg(any(test, feature = "map-capture"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct PageCacheStats {
    pub resident_reserved_bytes: usize,
    pub resident_reserved_peak: usize,
    pub entries: usize,
    pub queued: usize,
    pub running: usize,
}

impl TexturePageCache {
    pub fn new(
        policy: PageCachePolicy,
        working: Arc<TextureWorkingBudget>,
    ) -> Result<Arc<Self>, PageCacheInitError> {
        if policy.workers == 0 || policy.queued_jobs == 0 || policy.max_entries == 0 {
            return Err(PageCacheInitError::InvalidPolicy);
        }
        let signal = Arc::new(Signal::new());
        let cache = Arc::new(Self {
            inner: Arc::new(Inner {
                policy,
                working,
                ledger: Arc::new(Ledger {
                    capacity: policy.resident_bytes,
                    reserved: AtomicUsize::new(0),
                    peak: AtomicUsize::new(0),
                    signal: Arc::clone(&signal),
                }),
                signal,
                state: Mutex::new(State::default()),
                available: Condvar::new(),
            }),
        });
        for index in 0..policy.workers {
            let inner = Arc::clone(&cache.inner);
            if let Err(error) = std::thread::Builder::new()
                .name(format!("texture-page-{index}"))
                .spawn(move || worker(&inner))
            {
                cache.close();
                return Err(PageCacheInitError::Worker(error.to_string().into()));
            }
        }
        Ok(cache)
    }

    pub fn shared() -> Result<Arc<Self>, PageCacheInitError> {
        static SHARED: OnceLock<Result<Arc<TexturePageCache>, PageCacheInitError>> =
            OnceLock::new();
        SHARED
            .get_or_init(|| {
                Self::new(
                    PageCachePolicy::default(),
                    Arc::new(TextureWorkingBudget::new(1024 * 1024 * 1024)),
                )
            })
            .clone()
    }
    pub fn policy(&self) -> PageCachePolicy {
        self.inner.policy
    }
    pub fn subscribe(&self) -> PageSubscription {
        self.inner.signal.subscribe()
    }
    #[cfg(any(test, feature = "map-capture"))]
    #[cfg(any(test, feature = "map-capture"))]
    pub fn stats(&self) -> PageCacheStats {
        let state = self.inner.state.lock();
        PageCacheStats {
            resident_reserved_bytes: self.inner.ledger.reserved.load(Ordering::Acquire),
            resident_reserved_peak: self.inner.ledger.peak.load(Ordering::Relaxed),
            entries: state.entries.len(),
            queued: state.jobs.len(),
            running: state.running,
        }
    }

    pub fn request(
        &self,
        source: Arc<ReloadableTextureSource>,
        request: TextureFrameRequest,
    ) -> PageTicket {
        let cost = match source.validate_page(&request) {
            Ok(cost) => cost,
            Err(error) => return self.detached(request, PageStatus::Failed(Arc::new(error))),
        };
        if let Err(error) = self.inner.working.check_required(cost.decode_working_bytes) {
            return self.detached(request, failure(error));
        }
        let mut retired = Vec::new();
        let mut state = self.inner.state.lock();
        if state.closed {
            return self.detached(request, PageStatus::PendingRetry(PageRetry::Closed));
        }
        state.clock = state.clock.saturating_add(1);
        let touched = state.clock;
        if let Some(entry) = state.entries.get_mut(&request.identity()) {
            entry.interests += 1;
            entry.touched = touched;
            return self.ticket(source, request, Arc::clone(&entry.flight));
        }
        if state.jobs.len() >= self.inner.policy.queued_jobs {
            return self.detached(
                request,
                PageStatus::PendingRetry(PageRetry::Queue {
                    capacity: self.inner.policy.queued_jobs,
                }),
            );
        }
        if cost.resident_bytes > self.inner.policy.resident_bytes {
            return self.detached(
                request,
                PageStatus::PendingRetry(PageRetry::ResidentBytes {
                    required: cost.resident_bytes,
                    capacity: self.inner.policy.resident_bytes,
                }),
            );
        }
        loop {
            let fits_bytes = self
                .inner
                .ledger
                .reserved
                .load(Ordering::Acquire)
                .checked_add(cost.resident_bytes)
                .is_some_and(|total| total <= self.inner.policy.resident_bytes);
            if fits_bytes && state.entries.len() < self.inner.policy.max_entries {
                break;
            }
            let oldest = state
                .entries
                .iter()
                .filter(|(_, entry)| {
                    entry.interests == 0
                        && !matches!(*entry.flight.status.lock(), PageStatus::Pending)
                })
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else {
                let reason = if state.entries.len() >= self.inner.policy.max_entries {
                    PageRetry::Entries {
                        capacity: self.inner.policy.max_entries,
                    }
                } else {
                    PageRetry::ResidentBytes {
                        required: cost.resident_bytes,
                        capacity: self.inner.policy.resident_bytes,
                    }
                };
                return self.detached(request, PageStatus::PendingRetry(reason));
            };
            retired.extend(state.entries.remove(&oldest));
            drop(state);
            retired.clear();
            state = self.inner.state.lock();
            if state.closed {
                return self.detached(request, PageStatus::PendingRetry(PageRetry::Closed));
            }
            if let Some(entry) = state.entries.get_mut(&request.identity()) {
                entry.interests += 1;
                entry.touched = touched;
                return self.ticket(source, request, Arc::clone(&entry.flight));
            }
        }
        if state.jobs.len() >= self.inner.policy.queued_jobs {
            return self.detached(
                request,
                PageStatus::PendingRetry(PageRetry::Queue {
                    capacity: self.inner.policy.queued_jobs,
                }),
            );
        }
        let Some(permit) = self.inner.ledger.reserve(cost.resident_bytes) else {
            return self.detached(
                request,
                PageStatus::PendingRetry(PageRetry::ResidentBytes {
                    required: cost.resident_bytes,
                    capacity: self.inner.policy.resident_bytes,
                }),
            );
        };
        let flight = Arc::new(Flight {
            status: Mutex::new(PageStatus::Pending),
            cancellation: PreparationCancellation::default(),
        });
        state.entries.insert(
            request.identity(),
            Entry {
                flight: Arc::clone(&flight),
                source: Arc::downgrade(&source),
                interests: 1,
                touched,
            },
        );
        state.jobs.push_back(Job {
            source: Arc::downgrade(&source),
            request: request.clone(),
            flight: Arc::clone(&flight),
            permit,
        });
        drop(state);
        self.inner.available.notify_one();
        self.ticket(source, request, flight)
    }

    fn ticket(
        &self,
        source: Arc<ReloadableTextureSource>,
        request: TextureFrameRequest,
        flight: Arc<Flight>,
    ) -> PageTicket {
        PageTicket {
            interest: Arc::new(Interest {
                _source: Some(source),
                cache: Arc::downgrade(&self.inner),
                request,
                flight,
            }),
        }
    }
    fn detached(&self, request: TextureFrameRequest, status: PageStatus) -> PageTicket {
        PageTicket {
            interest: Arc::new(Interest {
                _source: None,
                cache: Weak::new(),
                request,
                flight: Arc::new(Flight {
                    status: Mutex::new(status),
                    cancellation: PreparationCancellation::default(),
                }),
            }),
        }
    }
    pub fn trim_orphaned(&self) {
        trim_orphaned(&self.inner);
    }

    pub fn close(&self) {
        let (flights, jobs) = {
            let mut state = self.inner.state.lock();
            state.closed = true;
            let jobs = std::mem::take(&mut state.jobs);
            let flights = state
                .entries
                .drain()
                .map(|(_, entry)| entry.flight)
                .collect::<Vec<_>>();
            drop(state);
            (flights, jobs)
        };
        for flight in flights {
            flight.cancellation.cancel();
            *flight.status.lock() = PageStatus::PendingRetry(PageRetry::Closed);
        }
        drop(jobs);
        self.inner.available.notify_all();
        self.inner.signal.notify();
    }
}
impl Drop for TexturePageCache {
    fn drop(&mut self) {
        self.close();
    }
}

fn trim_orphaned(inner: &Inner) {
    let (flights, removed_jobs) = {
        let mut state = inner.state.lock();
        let orphaned: Vec<_> = state
            .entries
            .iter()
            .filter(|(_, entry)| entry.interests == 0 && entry.source.strong_count() == 0)
            .map(|(key, _)| key.clone())
            .collect();
        let flights: Vec<_> = orphaned
            .into_iter()
            .filter_map(|key| state.entries.remove(&key).map(|entry| entry.flight))
            .collect();
        let mut removed_jobs = Vec::new();
        for flight in &flights {
            if let Some(position) = state
                .jobs
                .iter()
                .position(|job| Arc::ptr_eq(&job.flight, flight))
            {
                removed_jobs.extend(state.jobs.remove(position));
            }
        }
        drop(state);
        (flights, removed_jobs)
    };
    for flight in flights {
        flight.cancellation.cancel();
    }
    drop(removed_jobs);
    inner.signal.notify();
}

fn failure(error: TextureAssetError) -> PageStatus {
    match error {
        TextureAssetError::Cancelled(_) => PageStatus::PendingRetry(PageRetry::Cancelled),
        TextureAssetError::WorkingLimit { required, capacity } => {
            PageStatus::PendingRetry(PageRetry::WorkingBytes { required, capacity })
        }
        error => PageStatus::Failed(Arc::new(error)),
    }
}

fn worker(inner: &Inner) {
    loop {
        let job = {
            let mut state = inner.state.lock();
            while !state.closed && state.jobs.is_empty() {
                inner.available.wait(&mut state);
            }
            if state.closed {
                return;
            }
            state.running += 1;
            state.jobs.pop_front().expect("nonempty page queue")
        };
        inner.signal.notify();
        let Job {
            source,
            request,
            flight,
            permit,
        } = job;
        let result = source.upgrade().map_or_else(
            || Err(TextureAssetError::Cancelled(crate::preparation::Cancelled)),
            |source| {
                source
                    .load_page(&request, &flight.cancellation, &inner.working)
                    .map(|page| {
                        Arc::new(CachedTexturePage {
                            page,
                            _resident: permit,
                        })
                    })
            },
        );
        let status = match result {
            Ok(page) => PageStatus::Ready(page),
            Err(error) => failure(error),
        };
        {
            let mut state = inner.state.lock();
            state.running -= 1;
            if state
                .entries
                .get(&request.identity())
                .is_some_and(|entry| Arc::ptr_eq(&entry.flight, &flight))
            {
                let retry = matches!(status, PageStatus::PendingRetry(_));
                *flight.status.lock() = status;
                if retry {
                    state.entries.remove(&request.identity());
                }
            }
        }
        if source.strong_count() == 0 {
            trim_orphaned(inner);
        } else {
            inner.signal.notify();
        }
    }
}

#[cfg(test)]
mod tests;
