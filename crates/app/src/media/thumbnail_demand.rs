//! Views report visible rows. Feature updates translate those rows into demand
//! sets, and this manager is the only path that starts decode work.

use std::{
    collections::{BinaryHeap, HashMap, HashSet},
    fmt,
    ops::Range,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use iced::{Task, widget::image};
use quick_cache::{DefaultHashBuilder, Weighter, unsync::Cache};

use crate::{
    bridge::tasks::{BackendContext, RunBlockingError, ScheduleError},
    media::thumbnail_worker::{
        FetchProfile, PreparedAnimation, PreparedAnimationFrame, PreparedThumbnail,
        ThumbnailCancellation, ThumbnailError, ThumbnailInput, ThumbnailKey, ThumbnailMetadata,
        ThumbnailMode, ThumbnailWorkerOutcome, WorkerDiskCache, normalize_url,
        run_prepared_thumbnail_request, validate_max_edge,
    },
};

#[cfg(test)]
use crate::media::thumbnail_worker::Thumbnail;

const DEFAULT_ESTIMATED_ITEMS: usize = 128;
// Two media-pool widths hide the WorkerFinished -> pump round trip while
// cancelled FIFO entries yield before doing I/O.
const DEFAULT_MAX_IN_FLIGHT: usize = 32;
// Flat cache budget: rows release off-screen handles, so the cache is the
// actual ceiling; 256MB covers a broad retina-tile recency window without scaling
// quadratically with density.
const DEFAULT_MEMORY_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_DISK_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Slots background warming may occupy; the rest of the pipe stays free for
/// interactive tiers so a scroll burst never queues behind warm fetches.
const WARM_MAX_IN_FLIGHT: usize = 8;
const THUMBNAIL_SCALE_BUCKET: f32 = 0.5;
const WORKSHOP_ICON_SOURCE_MAX_EDGE: u32 = 512;
const WORKSHOP_ICON_SOURCE_MAX_SCALE: f32 = 2.0;

/// The cache deliberately uses `quick_cache`'s default lifecycle.
///
/// `Lifecycle` also exposes `is_pinned`, which is what a visible-window pin
/// would use. That was built and measured and does **not** ship: it removed
/// every eviction of an on-screen key (20 → 0 under animation pressure) while
/// saving exactly zero decode work, because rows hold `Arc`d handles and an
/// evicted-but-visible entry costs nothing until it is re-demanded. The
/// hit-rate collapse it was meant to fix is a capacity problem, not an
/// eviction-policy one — so raise [`Config::memory_capacity_bytes`] rather
/// than reaching for a custom lifecycle again.
type HandleCache = Cache<ThumbnailKey, ReadyThumbnail, ReadyThumbnailWeighter, DefaultHashBuilder>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub(crate) memory_capacity_bytes: u64,
    pub(crate) estimated_items: usize,
    pub(crate) max_in_flight: usize,
    pub(crate) disk_cache_dir: Option<PathBuf>,
    pub(crate) disk_cache_max_bytes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            memory_capacity_bytes: DEFAULT_MEMORY_CACHE_BYTES,
            estimated_items: DEFAULT_ESTIMATED_ITEMS,
            max_in_flight: DEFAULT_MAX_IN_FLIGHT,
            disk_cache_dir: None,
            disk_cache_max_bytes: DEFAULT_DISK_CACHE_MAX_BYTES,
        }
    }
}

pub fn bucketed_thumbnail_scale(scale_factor: f32) -> f32 {
    if !scale_factor.is_finite() || scale_factor <= 1.0 {
        return 1.0;
    }

    ((scale_factor / THUMBNAIL_SCALE_BUCKET).ceil() * THUMBNAIL_SCALE_BUCKET)
        .min(WORKSHOP_ICON_SOURCE_MAX_SCALE)
}

pub fn physical_thumbnail_edge(logical_edge: u32, scale_factor: f32) -> u32 {
    let scaled = f64::from(logical_edge) * f64::from(bucketed_thumbnail_scale(scale_factor));
    scaled
        .round()
        .max(f64::from(logical_edge))
        .min(f64::from(WORKSHOP_ICON_SOURCE_MAX_EDGE)) as u32
}

pub fn prefetch_ranges(visible: Range<usize>, total: usize) -> (Range<usize>, Range<usize>) {
    if total == 0 {
        return (0..0, 0..0);
    }

    let start = visible.start.min(total);
    let end = visible.end.min(total).max(start);
    let visible_len = end.saturating_sub(start);
    if visible_len == 0 {
        return (0..0, 0..0);
    }

    let before_len = visible_len.max(4);
    let after_len = visible_len.saturating_mul(2).max(4);
    (
        start.saturating_sub(before_len)..start,
        end..end.saturating_add(after_len).min(total),
    )
}

/// Rows inside this window keep their thumbnails when a grid releases
/// off-screen handles; everything else downgrades to Loading. `None` means
/// "release nothing" (transient empty viewport, e.g. before the first
/// visible-range event — releasing everything then would flash the grid).
pub fn retained_rows(visible: Range<usize>, total: usize) -> Option<Range<usize>> {
    let visible = visible.start.min(total)..visible.end.min(total);
    if visible.is_empty() {
        return None;
    }
    let (prefetch_before, prefetch_after) = prefetch_ranges(visible, total);
    Some(prefetch_before.start..prefetch_after.end)
}

pub struct Manager {
    config: Config,
    disk_cache: Option<WorkerDiskCache>,
    cache: HandleCache,
    index: DemandIndex,
    scale_factor: f32,
    scale_bucket: f32,
    next_sequence: u64,
    next_work_id: u64,
    // Preview-URL ThumbHashes seeded from the metadata snapshot and topped up by
    // completions, plus the tiny placeholder images they decode to (kept once
    // per URL so a scrolled row reuses a stable handle instead of re-uploading).
    thumbhashes: HashMap<String, Arc<[u8]>>,
    placeholders: HashMap<String, PlaceholderImage>,
}

impl Manager {
    pub(crate) fn new(config: Config) -> Self {
        let cache = Cache::with(
            config.estimated_items.max(1),
            config.memory_capacity_bytes.max(1),
            ReadyThumbnailWeighter,
            DefaultHashBuilder::default(),
            quick_cache::unsync::DefaultLifecycle::default(),
        );
        let disk_cache = config
            .disk_cache_dir
            .clone()
            .map(|dir| WorkerDiskCache::new(dir, config.disk_cache_max_bytes));

        Self {
            config,
            disk_cache,
            cache,
            index: DemandIndex::default(),
            scale_factor: 1.0,
            scale_bucket: bucketed_thumbnail_scale(1.0),
            next_sequence: 1,
            next_work_id: 1,
            thumbhashes: HashMap::new(),
            placeholders: HashMap::new(),
        }
    }

    /// Seeds preview-URL ThumbHashes (from the persisted metadata snapshot) so
    /// a placeholder can paint on the very first demand of a URL, before any
    /// decode runs.
    pub(crate) fn seed_thumbhashes(
        &mut self,
        entries: impl IntoIterator<Item = (String, Arc<[u8]>)>,
    ) {
        for (url, hash) in entries {
            self.thumbhashes.entry(normalize_url(url)).or_insert(hash);
        }
    }

    pub(crate) fn set_scale_factor(&mut self, scale_factor: f32) -> bool {
        let next_bucket = bucketed_thumbnail_scale(scale_factor);
        self.scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };

        if (self.scale_bucket - next_bucket).abs() < f32::EPSILON {
            return false;
        }

        self.scale_bucket = next_bucket;
        true
    }

    pub(crate) fn set_demands(&mut self, ctx: &BackendContext, set: DemandSet) -> Task<Message> {
        let immediate = self.apply_demands(set);
        self.batch_with_pump(ctx, immediate)
    }

    pub(crate) fn set_demand_sets(
        &mut self,
        ctx: &BackendContext,
        sets: impl IntoIterator<Item = DemandSet>,
    ) -> Task<Message> {
        let mut immediate = Vec::new();
        for set in sets {
            immediate.extend(self.apply_demands(set));
        }
        self.batch_with_pump(ctx, immediate)
    }

    pub(crate) fn update(&mut self, ctx: &BackendContext, message: Message) -> Task<Message> {
        match message {
            Message::WorkerFinished {
                key,
                job_id,
                attempt,
                result,
            } => {
                let effects = self.complete_job(&key, job_id, attempt, *result);
                self.batch_effects_with_pump(ctx, effects)
            }
            Message::WorkerBackpressured {
                key,
                job_id,
                attempt,
            } => {
                self.index.mark_key_queued(&key, job_id, attempt);
                self.pump(ctx)
            }
            Message::RetryReady { key, retry_id } => {
                self.index.mark_retry_ready(&key, retry_id);
                self.pump(ctx)
            }
            Message::Delivered(_) => Task::none(),
        }
    }

    fn apply_demands(&mut self, set: DemandSet) -> Vec<Message> {
        // Diff rather than teardown-and-rebuild.
        //
        // A scroll tick re-demands the whole visible+prefetch window even
        // though usually only a row or two entered or left it. Replacing the
        // owner's set outright meant discarding ~72 identical interests and
        // immediately recreating them, every tick — measured as 4,608 demand
        // applications for 242 distinct thumbnails across one fling.
        //
        // Interests are keyed by (owner, generation, id, key), so an unchanged
        // row produces an identical interest and is simply left alone: its
        // queue position, in-flight job, and retry state all survive.
        let mut retained = HashSet::with_capacity(set.demands.len());
        let mut resolved = Vec::with_capacity(set.demands.len());
        for demand in set.demands {
            let physical_max_edge =
                physical_thumbnail_edge(demand.logical_max_edge, self.scale_factor);
            let key = if set.owner == Owner::SizeAnalyzer {
                demand
                    .input
                    .cache_key_with_mode(physical_max_edge, ThumbnailMode::Static)
            } else {
                demand.input.cache_key(physical_max_edge)
            };
            let interest = InterestKey {
                owner: set.owner.clone(),
                generation: set.generation,
                id: demand.id.clone(),
                key: key.clone(),
            };
            let _ = retained.insert(interest);
            resolved.push((demand, key, physical_max_edge));
        }

        match set.replace {
            ReplaceMode::Owner => self.index.retain_owner(&set.owner, &retained),
        }
        if set.owner == Owner::SizeAnalyzer {
            let ids = resolved
                .iter()
                .map(|(demand, _, _)| demand.id.clone())
                .collect::<HashSet<_>>();
            self.index.remove_queued_warm(&ids);
        }

        let mut immediate = Vec::new();
        for (demand, key, physical_max_edge) in resolved {
            // Already demanded: leave the entry, its queue position, and any
            // running job as they are — but pick up a priority promotion, since
            // a row entering the visible window is the same interest served
            // more urgently.
            //
            // The sequence is allocated up front rather than lazily: it is a
            // free-running counter, so spending one on a demand that turns out
            // to be new is cheaper than borrowing `self` twice to defer it.
            let promoted_sequence = self.allocate_sequence();
            if self.index.reprioritise_existing(
                &set.owner,
                set.generation,
                &demand.id,
                &key,
                demand.priority,
                promoted_sequence,
            ) {
                continue;
            }

            if let Err(error) = validate_max_edge(physical_max_edge) {
                immediate.push(Message::Delivered(Box::new(Delivery::failed(
                    set.owner.clone(),
                    set.generation,
                    demand.id,
                    key,
                    ThumbnailDeliveryError::Thumbnail(Arc::new(error)),
                ))));
                continue;
            }

            // Warm is resolved *before* the memory cache is consulted, because
            // the two ask different questions. Warm's product is bytes on disk;
            // resident pixels do not imply a banked source (a legacy
            // derived-tier hit has pixels and no source), and a memory hit here
            // would both skip the banking warm exists to do and emit a
            // `Delivered` to `Owner::WarmLibrary`, which nothing consumes. It
            // would also drag the whole warm sweep through `quick_cache`'s
            // recency window on the way past.
            if demand.priority == Priority::WarmLibrary {
                // Warming only exists to fill the disk cache; a URL already on
                // disk needs no job, and nothing paints for the warm owner so
                // no placeholder either. (A URL banked between this check and
                // job start just costs one redundant `contains_source` probe in
                // the worker.)
                //
                // The question is asked of the *source* tier, not the derived
                // key. Warm no longer writes derived entries, so a derived-key
                // check is absent by construction and would re-enqueue the
                // entire library on every session after the first.
                if let Some(url) = key.source_url()
                    && self
                        .disk_cache
                        .as_ref()
                        .is_some_and(|cache| cache.contains_source(url))
                {
                    continue;
                }
                let sequence = self.allocate_sequence();
                let state = self.index.state_for_key(&key);
                self.index.add(DemandEntry::new(
                    set.owner.clone(),
                    set.generation,
                    sequence,
                    demand,
                    key,
                    physical_max_edge,
                    state,
                ));
                continue;
            }

            if let Some(ready) = self.cache.get(&key).cloned() {
                immediate.push(Message::Delivered(Box::new(Delivery::ready(
                    set.owner.clone(),
                    set.generation,
                    demand.id,
                    key,
                    ready,
                ))));
                continue;
            }

            // No pixels yet: paint a ThumbHash placeholder now if we know one for
            // this URL. Surfaces ignore a placeholder once they hold real pixels,
            // so re-emitting during the in-flight window is a harmless no-op.
            if set.owner != Owner::SizeAnalyzer
                && let Some(placeholder) = self.placeholder_for(demand.input.source_url())
            {
                immediate.push(Message::Delivered(Box::new(Delivery::placeholder(
                    set.owner.clone(),
                    set.generation,
                    demand.id.clone(),
                    key.clone(),
                    placeholder,
                ))));
            }

            let state = self.index.state_for_key(&key);
            let sequence = self.allocate_sequence();
            self.index.add(DemandEntry::new(
                set.owner.clone(),
                set.generation,
                sequence,
                demand,
                key,
                physical_max_edge,
                state,
            ));
        }
        self.index.cancel_uninterested_work();
        immediate
    }

    fn complete_job(
        &mut self,
        key: &ThumbnailKey,
        job_id: JobId,
        attempt: RetryAttempt,
        result: Result<ThumbnailWorkerOutcome<PreparedThumbnail>, ThumbnailDeliveryError>,
    ) -> CompletionEffects {
        let ready = match &result {
            Ok(ThumbnailWorkerOutcome::Completed(thumbnail)) => {
                if let (Some(url), Some(hash)) =
                    (key.source_url(), thumbnail.thumbnail().thumbhash_arc())
                {
                    self.remember_thumbhash(url, hash);
                }
                let ready = ready_thumbnail(key.clone(), thumbnail);
                // Warm-only completions fill the disk cache without churning
                // the memory cache's recency window; everything else — even a
                // completion nobody wants anymore — is kept, so a scroll-back
                // hits memory instead of re-decoding.
                if !self.index.interests_warm_only(key) {
                    self.cache.insert(key.clone(), ready.clone());
                }
                Some(ready)
            }
            Ok(ThumbnailWorkerOutcome::SourceBanked | ThumbnailWorkerOutcome::Cancelled)
            | Err(_) => None,
        };
        if !self.index.finish_job(key, job_id) {
            return CompletionEffects::default();
        }

        match result {
            Ok(ThumbnailWorkerOutcome::Completed(_)) => CompletionEffects::messages(
                self.index
                    .complete_key(key)
                    .into_iter()
                    .map(|entry| {
                        Message::Delivered(Box::new(Delivery::ready(
                            entry.owner,
                            entry.generation,
                            entry.id,
                            key.clone(),
                            ready
                                .clone()
                                .expect("completed thumbnail has a ready handle"),
                        )))
                    })
                    .collect(),
            ),
            Ok(ThumbnailWorkerOutcome::SourceBanked) => {
                self.index.retire_warm_interests(key);
                CompletionEffects::default()
            }
            Ok(ThumbnailWorkerOutcome::Cancelled) => {
                self.index.mark_interests_queued(key);
                CompletionEffects::default()
            }
            Err(error)
                if retry_delay(attempt, &error).is_some() && self.index.has_interests(key) =>
            {
                let next_attempt = attempt.next().expect("retryable attempt has a successor");
                let retry_id = RetryId(self.allocate_work_id());
                self.index.begin_retry(key, retry_id, next_attempt);
                CompletionEffects {
                    messages: Vec::new(),
                    retry: Some(RetrySchedule {
                        key: key.clone(),
                        retry_id,
                        delay: next_attempt.delay(),
                    }),
                }
            }
            Err(error) => CompletionEffects::messages(
                self.index
                    .complete_key(key)
                    .into_iter()
                    .map(|entry| {
                        Message::Delivered(Box::new(Delivery::failed(
                            entry.owner,
                            entry.generation,
                            entry.id,
                            key.clone(),
                            error.clone(),
                        )))
                    })
                    .collect(),
            ),
        }
    }

    fn batch_with_pump(&mut self, ctx: &BackendContext, immediate: Vec<Message>) -> Task<Message> {
        let mut tasks = immediate.into_iter().map(Task::done).collect::<Vec<_>>();
        tasks.push(self.pump(ctx));
        Task::batch(tasks)
    }

    fn batch_effects_with_pump(
        &mut self,
        ctx: &BackendContext,
        effects: CompletionEffects,
    ) -> Task<Message> {
        let mut tasks = effects
            .messages
            .into_iter()
            .map(Task::done)
            .collect::<Vec<_>>();
        if let Some(retry) = effects.retry {
            tasks.push(Task::future(async move {
                tokio::time::sleep(retry.delay).await;
                Message::RetryReady {
                    key: retry.key,
                    retry_id: retry.retry_id,
                }
            }));
        }
        tasks.push(self.pump(ctx));
        Task::batch(tasks)
    }

    fn pump(&mut self, ctx: &BackendContext) -> Task<Message> {
        let mut tasks = Vec::new();
        while self.index.in_flight_count() < self.config.max_in_flight.max(1) {
            // Warm jobs only take slots interactive tiers leave idle, so a
            // scroll burst always finds most of the pipe free.
            let allow_warm = self.index.in_flight_count() < WARM_MAX_IN_FLIGHT;
            let job_id = JobId(self.allocate_work_id());
            let Some(candidate) = self.index.next_candidate(job_id, allow_warm) else {
                break;
            };
            tasks.push(self.start_candidate(ctx, candidate));
        }
        Task::batch(tasks)
    }

    fn start_candidate(&self, ctx: &BackendContext, candidate: StartCandidate) -> Task<Message> {
        let disk_cache = self.disk_cache.clone();
        let key = candidate.key;
        let worker_key = key.clone();
        let message_key = key.clone();
        let input = candidate.input;
        let physical_max_edge = candidate.physical_max_edge;
        let profile = if candidate.priority == Priority::WarmLibrary {
            FetchProfile::BackgroundWarm
        } else {
            FetchProfile::Interactive
        };
        let cancellation = candidate.cancellation;
        let job_id = candidate.job_id;
        let attempt = candidate.attempt;
        let job_name = format!("thumbnail-{}", key.disk_file_name());

        ctx.run_blocking_media(job_name, move |_app| {
            run_prepared_thumbnail_request(
                disk_cache.as_ref(),
                &worker_key,
                input,
                physical_max_edge,
                profile,
                &cancellation,
            )
        })
        .map(move |result| worker_result_message(message_key.clone(), job_id, attempt, result))
    }

    fn remember_thumbhash(&mut self, url: &str, hash: Arc<[u8]>) {
        self.thumbhashes.entry(normalize_url(url)).or_insert(hash);
    }

    /// Returns (decoding and caching once per URL) the placeholder image for a
    /// URL whose ThumbHash we know, or `None` if we don't or it won't decode.
    fn placeholder_for(&mut self, url: &str) -> Option<PlaceholderImage> {
        if let Some(placeholder) = self.placeholders.get(url) {
            return Some(placeholder.clone());
        }
        let hash = self.thumbhashes.get(url)?.clone();
        let placeholder = decode_placeholder(&hash)?;
        self.placeholders
            .insert(url.to_owned(), placeholder.clone());
        Some(placeholder)
    }

    fn allocate_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1).max(1);
        sequence
    }

    fn allocate_work_id(&mut self) -> u64 {
        let id = self.next_work_id;
        self.next_work_id = self.next_work_id.wrapping_add(1).max(1);
        id
    }

    #[cfg(test)]
    fn cache_thumbnail(&mut self, key: ThumbnailKey, thumbnail: Thumbnail) -> ReadyThumbnail {
        let ready = ready_thumbnail(key.clone(), &PreparedThumbnail::from_thumbnail(thumbnail));
        self.cache.insert(key, ready.clone());
        ready
    }

    #[cfg(test)]
    fn cache_len(&self) -> usize {
        self.cache.len()
    }

    #[cfg(test)]
    fn pending_count(&self) -> usize {
        self.index.entries.len()
    }

    /// Scales the disk-cache eviction budget to the library so a full warm
    /// actually fits (the 256 MB default thrashes below library size).
    /// A clone of the disk-cache handle, for work that belongs on a blocking
    /// thread rather than in `update`. Clones share one index and one budget.
    pub(crate) fn disk_cache_handle(&self) -> Option<WorkerDiskCache> {
        self.disk_cache.clone()
    }

    pub(crate) fn scale_disk_cache_to_library(&self, addon_count: usize) {
        const PER_ADDON_BYTES: u64 = 1_310_720; // ~1.25 MiB decoded at 512px
        const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
        if let Some(cache) = &self.disk_cache {
            let bytes = (addon_count as u64 * PER_ADDON_BYTES)
                .clamp(DEFAULT_DISK_CACHE_MAX_BYTES, MAX_BYTES);
            cache.set_max_bytes(bytes);
        }
    }

    #[cfg(test)]
    fn next_candidate_for_test(&mut self) -> Option<StartCandidate> {
        let job_id = JobId(self.allocate_work_id());
        self.index.next_candidate(job_id, true)
    }

    #[cfg(test)]
    fn disk_cache_path(&self, key: &ThumbnailKey) -> Option<PathBuf> {
        self.config
            .disk_cache_dir
            .as_ref()
            .map(|dir| crate::media::thumbnail_worker::disk_cache_path(dir, key))
    }
}

impl fmt::Debug for Manager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Manager")
            .field("config", &self.config)
            .field("cache_len", &self.cache.len())
            .field("cache_weight", &self.cache.weight())
            .field("pending", &self.index.entries.len())
            .field("in_flight", &self.index.active_jobs.len())
            .finish()
    }
}

/// Why a thumbnail request failed: either the decode/resize pipeline
/// rejected it, or the worker pool couldn't schedule it. Kept transparent so
/// `Display` reproduces the underlying error text verbatim for logging.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ThumbnailDeliveryError {
    #[error(transparent)]
    Thumbnail(#[from] Arc<ThumbnailError>),
    #[error(transparent)]
    Schedule(#[from] Arc<RunBlockingError>),
}

#[derive(Clone, Debug)]
pub enum Message {
    WorkerFinished {
        key: ThumbnailKey,
        job_id: JobId,
        attempt: RetryAttempt,
        /// Boxed to keep this variant from setting the size of `RootMessage`,
        /// which every message in the app pays.
        result: Box<Result<ThumbnailWorkerOutcome<PreparedThumbnail>, ThumbnailDeliveryError>>,
    },
    WorkerBackpressured {
        key: ThumbnailKey,
        job_id: JobId,
        attempt: RetryAttempt,
    },
    RetryReady {
        key: ThumbnailKey,
        retry_id: RetryId,
    },
    /// Boxed: `Delivery` embeds a `ReadyThumbnail` (~184 bytes) inline, which
    /// would otherwise set the size of `RootMessage`.
    Delivered(Box<Delivery>),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Owner {
    AddonGrid(&'static str),
    PreparePublish,
    PreviewGma,
    SizeAnalyzer,
    WarmLibrary,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DemandId(Arc<str>);

impl DemandId {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(Arc::from(value.into()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Priority {
    ActiveDetail = 0,
    VisibleRow = 1,
    SizeAnalyzer = 2,
    Prefetch = 3,
    /// Whole-library disk-cache warming; only runs in slots interactive
    /// tiers leave idle, and its completions skip the memory cache.
    WarmLibrary = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaceMode {
    Owner,
}

#[derive(Clone, Debug)]
pub struct Demand {
    pub(crate) id: DemandId,
    pub(crate) input: ThumbnailInput,
    pub(crate) logical_max_edge: u32,
    pub(crate) priority: Priority,
}

#[derive(Clone, Debug)]
pub struct DemandSet {
    pub(crate) owner: Owner,
    pub(crate) generation: u64,
    pub(crate) replace: ReplaceMode,
    pub(crate) demands: Vec<Demand>,
}

impl DemandSet {
    pub(crate) fn empty(owner: Owner) -> Self {
        Self {
            owner,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct ReadyThumbnail {
    key: ThumbnailKey,
    handle: image::Handle,
    metadata: ThumbnailMetadata,
    animation: Option<ReadyAnimation>,
    thumbhash: Option<Arc<[u8]>>,
    byte_len: usize,
}

impl ReadyThumbnail {
    pub(crate) fn key(&self) -> &ThumbnailKey {
        &self.key
    }

    pub(crate) fn handle(&self) -> &image::Handle {
        &self.handle
    }

    pub(crate) fn metadata(&self) -> &ThumbnailMetadata {
        &self.metadata
    }

    pub(crate) fn animation(&self) -> Option<&ReadyAnimation> {
        self.animation.as_ref()
    }

    pub(crate) fn thumbhash(&self) -> Option<&[u8]> {
        self.thumbhash.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn for_test(key: ThumbnailKey, metadata: ThumbnailMetadata, rgba: Vec<u8>) -> Self {
        let byte_len = rgba.len();
        let handle = image::Handle::from_rgba(metadata.width, metadata.height, rgba);
        Self {
            key,
            handle,
            metadata,
            animation: None,
            thumbhash: None,
            byte_len,
        }
    }
}

impl fmt::Debug for ReadyThumbnail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadyThumbnail")
            .field("key", &self.key)
            .field("metadata", &self.metadata)
            .field(
                "animation_frames",
                &self.animation.as_ref().map(ReadyAnimation::frame_count),
            )
            .field("byte_len", &self.byte_len)
            .finish()
    }
}

#[derive(Clone)]
pub struct ReadyAnimation {
    frames: Vec<ReadyAnimationFrame>,
    byte_len: usize,
}

impl ReadyAnimation {
    pub(crate) fn frames(&self) -> &[ReadyAnimationFrame] {
        &self.frames
    }

    pub(crate) fn frame_count(&self) -> usize {
        self.frames.len()
    }
}

impl fmt::Debug for ReadyAnimation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadyAnimation")
            .field("frame_count", &self.frames.len())
            .field("byte_len", &self.byte_len)
            .finish()
    }
}

#[derive(Clone)]
pub struct ReadyAnimationFrame {
    handle: image::Handle,
    delay: std::time::Duration,
}

impl ReadyAnimationFrame {
    pub(crate) fn handle(&self) -> &image::Handle {
        &self.handle
    }

    pub(crate) const fn delay(&self) -> std::time::Duration {
        self.delay
    }
}

impl fmt::Debug for ReadyAnimationFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadyAnimationFrame")
            .field("delay", &self.delay)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct Delivery {
    pub(crate) owner: Owner,
    pub(crate) generation: u64,
    pub(crate) id: DemandId,
    pub(crate) key: ThumbnailKey,
    pub(crate) result: DeliveryResult,
}

impl Delivery {
    fn ready(
        owner: Owner,
        generation: u64,
        id: DemandId,
        key: ThumbnailKey,
        ready: ReadyThumbnail,
    ) -> Self {
        Self {
            owner,
            generation,
            id,
            key,
            result: DeliveryResult::Ready(ready),
        }
    }

    fn failed(
        owner: Owner,
        generation: u64,
        id: DemandId,
        key: ThumbnailKey,
        error: ThumbnailDeliveryError,
    ) -> Self {
        Self {
            owner,
            generation,
            id,
            key,
            result: DeliveryResult::Failed { error },
        }
    }

    fn placeholder(
        owner: Owner,
        generation: u64,
        id: DemandId,
        key: ThumbnailKey,
        placeholder: PlaceholderImage,
    ) -> Self {
        Self {
            owner,
            generation,
            id,
            key,
            result: DeliveryResult::Placeholder(placeholder),
        }
    }
}

#[derive(Clone, Debug)]
pub enum DeliveryResult {
    Ready(ReadyThumbnail),
    /// A blurred ThumbHash stand-in painted before real pixels arrive. Replaced
    /// by [`DeliveryResult::Ready`] once the decode lands.
    Placeholder(PlaceholderImage),
    Failed {
        error: ThumbnailDeliveryError,
    },
}

/// Tiny ThumbHash-decoded image the GPU upscales into a blurred placeholder.
#[derive(Clone)]
pub struct PlaceholderImage {
    handle: image::Handle,
    width: u32,
    height: u32,
}

impl PlaceholderImage {
    pub(crate) fn handle(&self) -> &image::Handle {
        &self.handle
    }

    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    #[cfg(test)]
    pub(crate) fn for_test(width: u32, height: u32) -> Self {
        Self {
            handle: image::Handle::from_rgba(
                width,
                height,
                vec![32; (width * height * 4) as usize],
            ),
            width,
            height,
        }
    }
}

impl fmt::Debug for PlaceholderImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaceholderImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

fn decode_placeholder(hash: &[u8]) -> Option<PlaceholderImage> {
    let (width, height, rgba) = crate::media::thumbhash::decode(hash)?;
    Some(PlaceholderImage {
        handle: image::Handle::from_rgba(width, height, rgba),
        width,
        height,
    })
}

#[derive(Default)]
struct DemandIndex {
    entries: HashMap<InterestKey, DemandEntry>,
    key_to_interests: HashMap<ThumbnailKey, Vec<InterestKey>>,
    active_jobs: HashMap<ThumbnailKey, ActiveJob>,
    delayed_retries: HashMap<ThumbnailKey, RetryId>,
    retry_attempts: HashMap<ThumbnailKey, RetryAttempt>,
    /// Interests grouped by owner, so a demand-set replacement touches only
    /// that owner's entries.
    ///
    /// Without it, tearing down a grid's ~72-entry window scanned every entry
    /// in the index — including the whole-library warm set, which on a cold
    /// disk cache is one entry per addon.
    by_owner: HashMap<Owner, HashSet<InterestKey>>,
    /// Queued interactive candidates, ordered by `(priority, sequence)`.
    ///
    /// Lazily deleted: entries are pushed on queue and simply skipped when
    /// popped if they are no longer queued. Removing from the middle of a heap
    /// is what an index of heap positions would buy, and the bookkeeping costs
    /// more than the stale pops it avoids.
    ready: BinaryHeap<Candidate>,
    /// Queued whole-library warm candidates, kept in their own heap.
    ///
    /// Warm work is blocked outright whenever the interactive tiers hold more
    /// than `WARM_MAX_IN_FLIGHT` slots. Sharing one heap meant every call in
    /// that state popped the entire warm backlog just to discover none of it
    /// was startable, then pushed it all back — reintroducing the O(library)
    /// scan the heap exists to remove. Partitioning lets that case skip the
    /// backlog entirely.
    ready_warm: BinaryHeap<Candidate>,
}

/// Heap entry ordering candidates by scheduling preference.
///
/// `Reverse` on both fields because `BinaryHeap` is a max-heap and the most
/// preferred candidate is the lowest priority value, then the lowest sequence.
#[derive(Clone, Eq, PartialEq)]
struct Candidate {
    priority: std::cmp::Reverse<Priority>,
    sequence: std::cmp::Reverse<u64>,
    interest: InterestKey,
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| self.sequence.cmp(&other.sequence))
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl DemandIndex {
    fn add(&mut self, entry: DemandEntry) {
        let interest = entry.interest_key();
        let key = entry.key.clone();
        self.key_to_interests
            .entry(key)
            .or_default()
            .push(interest.clone());
        let _ = self
            .by_owner
            .entry(entry.owner.clone())
            .or_default()
            .insert(interest.clone());
        if entry.state == DemandState::Queued {
            self.enqueue(&interest, entry.priority, entry.sequence);
        }
        self.entries.insert(interest, entry);
    }

    fn enqueue(&mut self, interest: &InterestKey, priority: Priority, sequence: u64) {
        let candidate = Candidate {
            priority: std::cmp::Reverse(priority),
            sequence: std::cmp::Reverse(sequence),
            interest: interest.clone(),
        };
        if priority == Priority::WarmLibrary {
            self.ready_warm.push(candidate);
        } else {
            self.ready.push(candidate);
        }
    }

    /// Drops this owner's interests that are absent from `keep`, leaving the
    /// rest untouched.
    fn retain_owner(&mut self, owner: &Owner, keep: &HashSet<InterestKey>) {
        let Some(interests) = self.by_owner.get(owner) else {
            return;
        };
        let stale = interests.difference(keep).cloned().collect::<Vec<_>>();
        if stale.is_empty() {
            return;
        }

        if let Some(interests) = self.by_owner.get_mut(owner) {
            for interest in &stale {
                let _ = interests.remove(interest);
            }
            if interests.is_empty() {
                let _ = self.by_owner.remove(owner);
            }
        }
        for interest in &stale {
            let _ = self.detach_interest(interest);
        }
    }

    /// Whether this exact interest is already demanded, re-prioritising it if
    /// the caller now wants it more urgently.
    ///
    /// Priority is deliberately *not* part of `InterestKey` — the same row at a
    /// new priority is the same interest, not a second one. But that means a
    /// row promoted from `Prefetch` to `VisibleRow` as the user scrolls toward
    /// it looks unchanged to a plain existence check, and would keep serving at
    /// prefetch priority for as long as it stayed on screen. Nothing in the
    /// delivered results would look wrong; it would simply be slow.
    fn reprioritise_existing(
        &mut self,
        owner: &Owner,
        generation: u64,
        id: &DemandId,
        key: &ThumbnailKey,
        priority: Priority,
        promoted_sequence: u64,
    ) -> bool {
        let interest = InterestKey {
            owner: owner.clone(),
            generation,
            id: id.clone(),
            key: key.clone(),
        };
        let Some(entry) = self.entries.get_mut(&interest) else {
            return false;
        };

        if entry.priority != priority {
            entry.priority = priority;
            entry.sequence = promoted_sequence;
            let (state, sequence) = (entry.state, entry.sequence);
            if state == DemandState::Queued {
                // The old heap candidate still carries the old priority and
                // sequence; it is skipped on pop by the sequence check.
                self.enqueue(&interest, priority, sequence);
            }
        }
        true
    }

    fn remove_queued_warm(&mut self, ids: &HashSet<DemandId>) {
        self.remove_where(|entry| {
            entry.owner == Owner::WarmLibrary
                && entry.state != DemandState::InFlight
                && ids.contains(&entry.id)
        });
    }

    fn remove_where(&mut self, predicate: impl Fn(&DemandEntry) -> bool) {
        let interests = self
            .entries
            .iter()
            .filter_map(|(interest, entry)| predicate(entry).then_some(interest.clone()))
            .collect::<Vec<_>>();
        for interest in interests {
            self.remove_interest(&interest);
        }
    }

    fn remove_interest(&mut self, interest: &InterestKey) -> Option<DemandEntry> {
        let entry = self.detach_interest(interest)?;
        // `retain_owner` prunes the bucket itself before detaching, so it uses
        // `detach_interest` directly; every other caller removes one interest
        // and must keep the owner bucket in step.
        if let Some(interests) = self.by_owner.get_mut(&entry.owner) {
            let _ = interests.remove(interest);
            if interests.is_empty() {
                let _ = self.by_owner.remove(&entry.owner);
            }
        }
        Some(entry)
    }

    /// Removes an interest from `entries` and `key_to_interests` without
    /// touching `by_owner`. Any stale heap candidate is skipped on pop.
    fn detach_interest(&mut self, interest: &InterestKey) -> Option<DemandEntry> {
        let entry = self.entries.remove(interest)?;
        if let Some(interests) = self.key_to_interests.get_mut(&entry.key) {
            interests.retain(|candidate| candidate != interest);
            if interests.is_empty() {
                self.key_to_interests.remove(&entry.key);
            }
        }
        Some(entry)
    }

    /// Drops work nobody wants any more, releasing its media slot.
    ///
    /// Cancelling an in-flight job wastes less than it looks like it should:
    /// cancellation is cooperative and only checked before I/O, so
    /// a job already fetching runs to completion, and `complete_job` inserts the
    /// result into the memory cache *before* it checks whether anyone still
    /// wants it. The bytes are kept either way. What cancelling actually buys is
    /// the slot, immediately, for rows the user has now scrolled to.
    fn cancel_uninterested_work(&mut self) {
        let keys = self
            .active_jobs
            .keys()
            .chain(self.delayed_retries.keys())
            .filter(|key| !self.has_interests(key))
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            self.cancel_key(&key);
        }
    }

    fn state_for_key(&self, key: &ThumbnailKey) -> DemandState {
        if self.active_jobs.contains_key(key) {
            DemandState::InFlight
        } else if self.delayed_retries.contains_key(key) {
            DemandState::RetryWaiting
        } else {
            DemandState::Queued
        }
    }

    fn next_candidate(&mut self, job_id: JobId, allow_warm: bool) -> Option<StartCandidate> {
        // Pops in scheduling order, discarding candidates that are stale
        // (their entry is gone, or no longer queued) and setting aside the ones
        // that are merely blocked right now so they are not lost.
        let selected = self
            .take_startable(false)
            .or_else(|| allow_warm.then(|| self.take_startable(true)).flatten());

        let (key, input, physical_max_edge, priority) = selected?;
        if let Some(interests) = self.key_to_interests.get(&key) {
            for interest in interests {
                if let Some(entry) = self.entries.get_mut(interest) {
                    entry.state = DemandState::InFlight;
                }
            }
        }
        let attempt = self.retry_attempts.remove(&key).unwrap_or_default();
        let cancellation = ThumbnailCancellation::default();
        self.active_jobs.insert(
            key.clone(),
            ActiveJob {
                job_id,
                cancellation: cancellation.clone(),
            },
        );

        // Emitted here rather than in `pump` so every caller that claims a job
        // is counted, including the headless driver's simulated worker pool.

        Some(StartCandidate {
            key,
            input,
            physical_max_edge,
            priority,
            job_id,
            attempt,
            cancellation,
        })
    }

    /// Pops the best startable candidate from one tier's heap.
    ///
    /// Candidates that are stale (entry gone, no longer queued, or superseded
    /// by a newer sequence) are dropped. Candidates that are merely blocked
    /// right now — their source already has a job running, or a static request
    /// is waiting on the animated decode of the same source — are set aside and
    /// returned to the heap, because they are still wanted.
    fn take_startable(
        &mut self,
        warm: bool,
    ) -> Option<(ThumbnailKey, ThumbnailInput, u32, Priority)> {
        let mut deferred = Vec::new();
        let selected = loop {
            let heap = if warm {
                &mut self.ready_warm
            } else {
                &mut self.ready
            };
            let Some(candidate) = heap.pop() else {
                break None;
            };

            let Some(entry) = self.entries.get(&candidate.interest) else {
                continue;
            };
            if entry.state != DemandState::Queued || entry.sequence != candidate.sequence.0 {
                continue;
            }

            let blocked = self.active_jobs.contains_key(&entry.key)
                || (entry.key.mode() == ThumbnailMode::Static
                    && self.active_jobs.keys().any(|active| {
                        active.mode() == ThumbnailMode::Animated
                            && active.source == entry.key.source
                    }));
            if blocked {
                deferred.push(candidate);
                continue;
            }

            break Some((
                entry.key.clone(),
                entry.input.clone(),
                entry.physical_max_edge,
                entry.priority,
            ));
        };

        if warm {
            self.ready_warm.extend(deferred);
        } else {
            self.ready.extend(deferred);
        }
        selected
    }

    /// True when `key` has at least one interest and every one is warm-tier —
    /// the completion should fill the disk cache without churning the memory
    /// cache's recency window. No interests at all is NOT warm-only: a
    /// scrolled-past completion is exactly what the memory cache wants.
    fn interests_warm_only(&self, key: &ThumbnailKey) -> bool {
        self.key_to_interests.get(key).is_some_and(|interests| {
            !interests.is_empty()
                && interests.iter().all(|interest| {
                    self.entries
                        .get(interest)
                        .is_some_and(|entry| entry.priority == Priority::WarmLibrary)
                })
        })
    }

    fn mark_key_queued(&mut self, key: &ThumbnailKey, job_id: JobId, attempt: RetryAttempt) {
        if !self.finish_job(key, job_id) {
            return;
        }
        self.retry_attempts.insert(key.clone(), attempt);
        self.mark_interests_queued(key);
    }

    /// Retires the warm interests in `key` and re-queues everything else.
    ///
    /// A warm job hands back no pixels, so the two halves have to be handled
    /// differently. Warm interests are *satisfied* — the bytes they wanted are
    /// on disk — and must not be re-queued, or the pump would restart a job
    /// that banks-and-completes immediately, forever. An interactive interest
    /// that attached while the warm job was in flight is *not* satisfied, and
    /// would otherwise sit `InFlight` waiting for a delivery that is never
    /// coming; re-queuing it restarts it at its own priority, and that restart
    /// is local, since the source it needs was just banked.
    fn retire_warm_interests(&mut self, key: &ThumbnailKey) {
        let Some(interests) = self.key_to_interests.get(key).cloned() else {
            return;
        };
        // The warm fetch succeeded, so any backoff it accumulated is spent.
        self.retry_attempts.remove(key);
        for interest in interests {
            let Some(entry) = self.entries.get_mut(&interest) else {
                continue;
            };
            if entry.priority == Priority::WarmLibrary {
                let _ = self.remove_interest(&interest);
                continue;
            }
            entry.state = DemandState::Queued;
            let (priority, sequence) = (entry.priority, entry.sequence);
            self.enqueue(&interest, priority, sequence);
        }
    }

    fn mark_interests_queued(&mut self, key: &ThumbnailKey) {
        let Some(interests) = self.key_to_interests.get(key).cloned() else {
            return;
        };
        for interest in interests {
            let Some(entry) = self.entries.get_mut(&interest) else {
                continue;
            };
            entry.state = DemandState::Queued;
            let (priority, sequence) = (entry.priority, entry.sequence);
            // Re-offer: the entry's previous heap candidate was consumed when
            // it left the queue, so becoming queued again needs a fresh one.
            self.enqueue(&interest, priority, sequence);
        }
    }

    fn begin_retry(&mut self, key: &ThumbnailKey, retry_id: RetryId, attempt: RetryAttempt) {
        self.delayed_retries.insert(key.clone(), retry_id);
        self.retry_attempts.insert(key.clone(), attempt);
        if let Some(interests) = self.key_to_interests.get(key) {
            for interest in interests {
                if let Some(entry) = self.entries.get_mut(interest) {
                    entry.state = DemandState::RetryWaiting;
                }
            }
        }
    }

    fn mark_retry_ready(&mut self, key: &ThumbnailKey, retry_id: RetryId) {
        if self.delayed_retries.get(key) != Some(&retry_id) {
            return;
        }
        self.delayed_retries.remove(key);
        self.mark_interests_queued(key);
    }

    fn finish_job(&mut self, key: &ThumbnailKey, job_id: JobId) -> bool {
        if self.active_jobs.get(key).map(|job| job.job_id) != Some(job_id) {
            return false;
        }
        self.active_jobs.remove(key);
        true
    }

    fn has_interests(&self, key: &ThumbnailKey) -> bool {
        self.key_to_interests
            .get(key)
            .is_some_and(|interests| !interests.is_empty())
    }

    fn cancel_key(&mut self, key: &ThumbnailKey) {
        if let Some(job) = self.active_jobs.remove(key) {
            // Only an actually-running job needs signalling; a queued entry is
            // dropped by removing it from `active_jobs` above.
            job.cancellation.cancel();
        }
        self.delayed_retries.remove(key);
        self.retry_attempts.remove(key);
    }

    /// Retires every interest in `key`, returning them so the caller can
    /// deliver to each.
    ///
    /// Removals go through `remove_interest` so the owner bucket is kept in
    /// step. Dropping straight out of `entries` left the interest behind in
    /// `by_owner` — harmless today, because `retain_owner` is that map's only
    /// reader and it tolerates entries that no longer exist, but the ghosts
    /// accumulated for a whole session under `Owner::WarmLibrary` (which
    /// applies exactly one demand set and so never prunes again), and any
    /// future reader of `by_owner` would have inherited a lie.
    fn complete_key(&mut self, key: &ThumbnailKey) -> Vec<DemandEntry> {
        self.cancel_key(key);
        let interests = self.key_to_interests.remove(key).unwrap_or_default();
        interests
            .into_iter()
            .filter_map(|interest| self.remove_interest(&interest))
            .collect()
    }

    fn in_flight_count(&self) -> usize {
        self.active_jobs.len()
    }
}

struct ActiveJob {
    job_id: JobId,
    cancellation: ThumbnailCancellation,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct InterestKey {
    owner: Owner,
    generation: u64,
    id: DemandId,
    key: ThumbnailKey,
}

struct DemandEntry {
    owner: Owner,
    generation: u64,
    id: DemandId,
    input: ThumbnailInput,
    key: ThumbnailKey,
    physical_max_edge: u32,
    priority: Priority,
    state: DemandState,
    sequence: u64,
}

impl DemandEntry {
    fn new(
        owner: Owner,
        generation: u64,
        sequence: u64,
        demand: Demand,
        key: ThumbnailKey,
        physical_max_edge: u32,
        state: DemandState,
    ) -> Self {
        Self {
            owner,
            generation,
            id: demand.id,
            input: demand.input,
            key,
            physical_max_edge,
            priority: demand.priority,
            state,
            sequence,
        }
    }

    fn interest_key(&self) -> InterestKey {
        InterestKey {
            owner: self.owner.clone(),
            generation: self.generation,
            id: self.id.clone(),
            key: self.key.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DemandState {
    Queued,
    InFlight,
    RetryWaiting,
}

struct StartCandidate {
    key: ThumbnailKey,
    input: ThumbnailInput,
    physical_max_edge: u32,
    priority: Priority,
    job_id: JobId,
    attempt: RetryAttempt,
    cancellation: ThumbnailCancellation,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JobId(u64);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RetryId(u64);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetryAttempt(u8);

impl RetryAttempt {
    fn next(self) -> Option<Self> {
        (self.0 < 2).then(|| Self(self.0 + 1))
    }

    fn delay(self) -> Duration {
        match self.0 {
            1 => Duration::from_secs(1),
            2 => Duration::from_secs(4),
            _ => Duration::ZERO,
        }
    }
}

#[derive(Default)]
struct CompletionEffects {
    messages: Vec<Message>,
    retry: Option<RetrySchedule>,
}

impl CompletionEffects {
    fn messages(messages: Vec<Message>) -> Self {
        Self {
            messages,
            retry: None,
        }
    }
}

struct RetrySchedule {
    key: ThumbnailKey,
    retry_id: RetryId,
    delay: Duration,
}

#[derive(Clone)]
struct ReadyThumbnailWeighter;

impl Weighter<ThumbnailKey, ReadyThumbnail> for ReadyThumbnailWeighter {
    fn weight(&self, _key: &ThumbnailKey, value: &ReadyThumbnail) -> u64 {
        value.byte_len as u64
    }
}

fn ready_thumbnail(key: ThumbnailKey, thumbnail: &PreparedThumbnail) -> ReadyThumbnail {
    let metadata = thumbnail.thumbnail().metadata().clone();
    let rgba_len = thumbnail.thumbnail().rgba_bytes().len();
    let animation = thumbnail.animation().map(ready_animation);
    let byte_len = rgba_len + animation.as_ref().map_or(0, |animation| animation.byte_len);
    let handle = image::Handle::from_rgba(
        metadata.width,
        metadata.height,
        Bytes::from_owner(thumbnail.thumbnail().rgba_arc()),
    );
    ReadyThumbnail {
        key,
        handle,
        metadata,
        animation,
        thumbhash: thumbnail.thumbnail().thumbhash_arc(),
        byte_len,
    }
}

fn ready_animation(animation: &PreparedAnimation) -> ReadyAnimation {
    let mut byte_len = 0_usize;
    let frames = animation
        .frames()
        .iter()
        .map(|frame| {
            byte_len = byte_len.saturating_add(frame.rgba_bytes().len());
            ready_animation_frame(frame)
        })
        .collect();

    ReadyAnimation { frames, byte_len }
}

fn ready_animation_frame(frame: &PreparedAnimationFrame) -> ReadyAnimationFrame {
    ReadyAnimationFrame {
        handle: image::Handle::from_rgba(
            frame.width(),
            frame.height(),
            Bytes::from_owner(frame.rgba_arc()),
        ),
        delay: frame.delay(),
    }
}

fn worker_result_message(
    key: ThumbnailKey,
    job_id: JobId,
    attempt: RetryAttempt,
    result: Result<
        Result<ThumbnailWorkerOutcome<PreparedThumbnail>, ThumbnailError>,
        RunBlockingError,
    >,
) -> Message {
    match result {
        Err(RunBlockingError::Schedule(ScheduleError::QueueFull { .. })) => {
            Message::WorkerBackpressured {
                key,
                job_id,
                attempt,
            }
        }
        Err(error) => Message::WorkerFinished {
            key,
            job_id,
            attempt,
            result: Box::new(Err(ThumbnailDeliveryError::Schedule(Arc::new(error)))),
        },
        Ok(Err(error)) => Message::WorkerFinished {
            key,
            job_id,
            attempt,
            result: Box::new(Err(ThumbnailDeliveryError::Thumbnail(Arc::new(error)))),
        },
        Ok(Ok(outcome)) => Message::WorkerFinished {
            key,
            job_id,
            attempt,
            result: Box::new(Ok(outcome)),
        },
    }
}

fn retry_delay(attempt: RetryAttempt, error: &ThumbnailDeliveryError) -> Option<Duration> {
    let next_attempt = attempt.next()?;
    let ThumbnailDeliveryError::Thumbnail(error) = error else {
        return None;
    };
    match error.as_ref() {
        ThumbnailError::UrlFetch {
            source: ureq::Error::StatusCode(status),
            ..
        } => (*status >= 500).then(|| next_attempt.delay()),
        ThumbnailError::UrlFetch { .. } | ThumbnailError::UrlRead { .. } => {
            Some(next_attempt.delay())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn retained_rows_covers_visible_plus_prefetch_and_guards_empty_viewports() {
        assert_eq!(retained_rows(0..0, 200), None);
        assert_eq!(retained_rows(5..5, 200), None);
        assert_eq!(retained_rows(10..20, 0), None);

        let retained = retained_rows(40..52, 200).expect("window");
        let (before, after) = prefetch_ranges(40..52, 200);
        assert_eq!(retained, before.start..after.end);
        assert!(retained.contains(&40) && retained.contains(&51));
        assert!(retained.start < 40 && retained.end > 52);

        let retained = retained_rows(0..12, 200).expect("window");
        assert_eq!(retained.start, 0);
        let retained = retained_rows(190..210, 200).expect("window");
        assert_eq!(retained.end, 200);
    }

    #[test]
    fn physical_thumbnail_edge_keeps_standard_dpi_size() {
        assert_eq!(physical_thumbnail_edge(256, 1.0), 256);
        assert_eq!(physical_thumbnail_edge(256, 0.0), 256);
        assert_eq!(physical_thumbnail_edge(256, f32::NAN), 256);
    }

    #[test]
    fn physical_thumbnail_edge_rounds_up_to_hidpi_bucket_and_source_cap() {
        assert_eq!(physical_thumbnail_edge(256, 1.25), 384);
        assert_eq!(physical_thumbnail_edge(256, 2.0), 512);
        assert_eq!(physical_thumbnail_edge(256, 9.0), 512);
    }

    #[test]
    fn prefetch_ranges_expand_middle_visible_window() {
        assert_eq!(prefetch_ranges(20..30, 100), (10..20, 30..50));
    }

    #[test]
    fn prefetch_ranges_clamp_at_start() {
        assert_eq!(prefetch_ranges(0..5, 100), (0..0, 5..15));
    }

    #[test]
    fn prefetch_ranges_clamp_at_end() {
        assert_eq!(prefetch_ranges(95..100, 100), (90..95, 100..100));
    }

    #[test]
    fn prefetch_ranges_use_minimum_window_for_tiny_lists() {
        assert_eq!(prefetch_ranges(1..2, 3), (0..1, 2..3));
    }

    #[test]
    fn prefetch_ranges_keep_empty_visible_range_empty() {
        assert_eq!(prefetch_ranges(3..3, 10), (0..0, 0..0));
        assert_eq!(prefetch_ranges(0..0, 0), (0..0, 0..0));
    }

    #[test]
    fn cached_demand_delivers_ready_without_queueing_decode() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/shared.jpg");
        let key = input.cache_key(64);
        let cached = manager.cache_thumbnail(key.clone(), solid_thumbnail(8, 6, 12));

        let messages = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("Installed Addons"),
            generation: 9,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", input, 64, Priority::VisibleRow)],
        });

        assert_eq!(manager.pending_count(), 0);
        assert_eq!(messages.len(), 1);
        let Message::Delivered(delivery) = &messages[0] else {
            panic!("cached thumbnail should deliver immediately");
        };
        assert_eq!(delivery.owner, Owner::AddonGrid("Installed Addons"));
        assert_eq!(delivery.generation, 9);
        assert_eq!(delivery.key, key);
        let DeliveryResult::Ready(ready) = &delivery.result else {
            panic!("cached thumbnail should be ready");
        };
        assert_eq!(ready.key(), cached.key());
        assert_eq!(ready.metadata(), cached.metadata());
        assert_eq!(ready.handle(), cached.handle());
    }

    #[test]
    fn seeded_thumbhash_paints_placeholder_then_real_pixels_replace_it() {
        let mut manager = Manager::new(Config::default());
        let url = "https://example.invalid/poster.jpg";
        let input = ThumbnailInput::from_url(url);
        let key = input.cache_key(physical_thumbnail_edge(64, 1.0));
        let hash = crate::media::thumbhash::encode(4, 4, &[128; 4 * 4 * 4]).expect("hash encodes");
        manager.seed_thumbhashes([(url.to_owned(), Arc::from(hash))]);

        let messages = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("Installed Addons"),
            generation: 3,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", input, 64, Priority::VisibleRow)],
        });

        let placeholder = messages
            .iter()
            .find_map(|message| match message {
                Message::Delivered(delivery) => {
                    matches!(delivery.result, DeliveryResult::Placeholder(_)).then_some(delivery)
                }
                _ => None,
            })
            .expect("placeholder should paint before pixels exist");
        assert_eq!(placeholder.key, key);
        assert_eq!(manager.cache_len(), 0);

        // The real decode is still queued; completing it delivers Ready pixels
        // that replace the placeholder.
        let candidate = manager
            .next_candidate_for_test()
            .expect("decode should be queued");
        let effects = manager.complete_job(
            &key,
            candidate.job_id,
            candidate.attempt,
            Ok(ThumbnailWorkerOutcome::Completed(prepared_thumbnail(
                solid_thumbnail(16, 12, 3),
            ))),
        );
        assert!(effects.messages.iter().any(|message| matches!(
            message,
            Message::Delivered(delivery)
                if matches!(delivery.result, DeliveryResult::Ready(_))
        )));
    }

    #[test]
    fn size_analyzer_uses_static_keys_without_creating_image_placeholders() {
        let mut manager = Manager::new(Config::default());
        let url = "https://example.invalid/poster.jpg";
        let input = ThumbnailInput::from_url(url);
        let hash = crate::media::thumbhash::encode(4, 4, &[128; 4 * 4 * 4]).expect("hash encodes");
        manager.seed_thumbhashes([(url.to_owned(), Arc::from(hash))]);

        let messages = manager.apply_demands(DemandSet {
            owner: Owner::SizeAnalyzer,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", input, 64, Priority::SizeAnalyzer)],
        });

        assert!(messages.is_empty());
        let candidate = manager
            .next_candidate_for_test()
            .expect("analyzer decode should be queued");
        assert_eq!(candidate.key.mode(), ThumbnailMode::Static);
    }

    #[test]
    fn size_analyzer_replaces_queued_warm_work_for_the_same_addon() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/shared.jpg");
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::WarmLibrary,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input.clone(), 256, Priority::WarmLibrary)],
        });
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::SizeAnalyzer,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input, 64, Priority::SizeAnalyzer)],
        });

        assert_eq!(manager.pending_count(), 1);
        let candidate = manager
            .next_candidate_for_test()
            .expect("static analyzer work remains");
        assert_eq!(candidate.key.mode(), ThumbnailMode::Static);
    }

    #[test]
    fn size_analyzer_waits_for_an_active_animated_source() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/shared.jpg");
        let animated_key = input.cache_key(256);
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::WarmLibrary,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input.clone(), 256, Priority::WarmLibrary)],
        });
        let warm = manager
            .next_candidate_for_test()
            .expect("warm source starts first");
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::SizeAnalyzer,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input, 64, Priority::SizeAnalyzer)],
        });

        assert!(manager.next_candidate_for_test().is_none());
        let _ = manager.complete_job(
            &animated_key,
            warm.job_id,
            warm.attempt,
            Ok(ThumbnailWorkerOutcome::SourceBanked),
        );
        let candidate = manager
            .next_candidate_for_test()
            .expect("static work follows the shared source");
        assert_eq!(candidate.key.mode(), ThumbnailMode::Static);
    }

    /// A warm job's product is bytes on disk. It paints nothing, delivers
    /// nothing, and must not churn the memory cache's recency window.
    #[test]
    fn a_warm_completion_delivers_nothing_and_enters_no_cache() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/warm.jpg");
        let key = input.cache_key(physical_thumbnail_edge(256, 1.0));

        let messages = manager.apply_demands(DemandSet {
            owner: Owner::WarmLibrary,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input, 256, Priority::WarmLibrary)],
        });
        assert!(messages.is_empty(), "warm demands paint nothing up front");

        let candidate = manager.next_candidate_for_test().expect("warm job queued");
        let effects = manager.complete_job(
            &key,
            candidate.job_id,
            candidate.attempt,
            Ok(ThumbnailWorkerOutcome::SourceBanked),
        );

        assert_eq!(manager.cache_len(), 0, "warm fills disk, not memory");
        assert!(
            effects.messages.is_empty(),
            "nothing paints for the warm owner, so nothing is delivered"
        );
    }

    /// The warm interest is *satisfied* by the banked bytes, so it must be
    /// retired rather than re-queued. Re-queuing it would have the pump restart
    /// a job that banks-and-completes immediately, forever.
    #[test]
    fn a_warm_completion_does_not_re_offer_its_own_interest() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/warm-once.jpg");
        let key = input.cache_key(physical_thumbnail_edge(256, 1.0));

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::WarmLibrary,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input, 256, Priority::WarmLibrary)],
        });
        let candidate = manager.next_candidate_for_test().expect("warm job queued");
        let _ = manager.complete_job(
            &key,
            candidate.job_id,
            candidate.attempt,
            Ok(ThumbnailWorkerOutcome::SourceBanked),
        );

        assert!(
            manager.next_candidate_for_test().is_none(),
            "a banked source is finished work, not work to start again"
        );
    }

    /// A visible row can attach to a key while its warm job is already in
    /// flight. That job hands back no pixels, so without a re-queue the row
    /// would sit `InFlight` waiting for a delivery that is never coming — a
    /// card stuck on its placeholder for the rest of the session.
    #[test]
    fn an_interactive_interest_joining_a_running_warm_job_is_requeued() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/joined.jpg");
        let key = input.cache_key(physical_thumbnail_edge(256, 1.0));

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::WarmLibrary,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input.clone(), 256, Priority::WarmLibrary)],
        });
        let warm = manager.next_candidate_for_test().expect("warm job queued");

        // The row arrives mid-flight, so it joins the running job rather than
        // starting one of its own.
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("Installed Addons"),
            generation: 1,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", input, 256, Priority::VisibleRow)],
        });
        assert!(
            manager.next_candidate_for_test().is_none(),
            "the row joins the in-flight job instead of duplicating it"
        );

        let effects = manager.complete_job(
            &key,
            warm.job_id,
            warm.attempt,
            Ok(ThumbnailWorkerOutcome::SourceBanked),
        );
        assert!(effects.messages.is_empty(), "warm still delivers nothing");

        let restarted = manager
            .next_candidate_for_test()
            .expect("the visible row must be re-offered");
        assert_eq!(restarted.key, key);
        assert_eq!(
            restarted.priority,
            Priority::VisibleRow,
            "and at its own priority, not the warm tier's"
        );
    }

    /// `InterestKey` deliberately excludes priority — the same row at a new
    /// priority is the same interest, not a second one. That makes a plain
    /// existence check silently wrong for a row the user scrolls *toward*: it
    /// looks unchanged, so it keeps serving at prefetch priority for as long as
    /// it stays on screen. Nothing in the delivered results looks wrong; the
    /// card is just slow.
    ///
    /// Covered directly here: this behaviour was once guarded only by a
    /// benchmark harness's count-determinism, which no longer exists.
    #[test]
    fn a_row_promoted_to_visible_is_re_offered_at_its_new_priority() {
        let mut manager = Manager::new(Config::default());
        let ahead = ThumbnailInput::from_url("https://example.invalid/ahead.jpg");
        let promoted = ThumbnailInput::from_url("https://example.invalid/promoted.jpg");

        // `promoted` is demanded first, so it wins any tie on sequence — the
        // test would pass for the wrong reason if priority were ignored.
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("My Workshop"),
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![
                demand("row-1", promoted.clone(), 256, Priority::Prefetch),
                demand("row-2", ahead.clone(), 256, Priority::Prefetch),
            ],
        });

        // The user scrolls toward row-1: same owner, same id, same key, higher
        // priority. Re-demanded alongside row-2 exactly as a real grid tick does.
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("My Workshop"),
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![
                demand("row-1", promoted.clone(), 256, Priority::VisibleRow),
                demand("row-2", ahead, 256, Priority::Prefetch),
            ],
        });

        let first = manager
            .next_candidate_for_test()
            .expect("promoted row is startable");
        assert_eq!(
            first.priority,
            Priority::VisibleRow,
            "the promotion must reach the entry, not just the demand"
        );
        assert_eq!(
            first.key,
            promoted.cache_key(physical_thumbnail_edge(256, 1.0)),
            "and the promoted row must be scheduled ahead of the prefetch work"
        );
    }

    /// The same mechanism in reverse: a row that scrolls off the visible window
    /// into the prefetch band must stop holding a visible-tier slot, or a long
    /// scroll would accumulate stale high-priority entries.
    #[test]
    fn a_row_demoted_to_prefetch_gives_up_its_visible_priority() {
        let mut manager = Manager::new(Config::default());
        let settling = ThumbnailInput::from_url("https://example.invalid/settling.jpg");
        let arriving = ThumbnailInput::from_url("https://example.invalid/arriving.jpg");

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("My Workshop"),
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", settling.clone(), 256, Priority::VisibleRow)],
        });
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("My Workshop"),
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![
                demand("row-1", settling, 256, Priority::Prefetch),
                demand("row-2", arriving.clone(), 256, Priority::VisibleRow),
            ],
        });

        let first = manager
            .next_candidate_for_test()
            .expect("the newly visible row is startable");
        assert_eq!(
            first.key,
            arriving.cache_key(physical_thumbnail_edge(256, 1.0)),
            "the demoted row must not keep the visible slot it no longer deserves"
        );
    }

    /// Promotion of an entry whose job is already running must reach the entry
    /// without re-offering it: the work is in flight, and a second candidate
    /// for a running key is the duplicate-job class the scheduler exists to
    /// prevent — but the new priority still has to stick, or a row that was
    /// promoted mid-flight would be re-queued at prefetch tier if its job is
    /// ever cancelled or retried.
    #[test]
    fn promoting_an_in_flight_row_keeps_the_priority_without_queueing_twice() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/in-flight.jpg");
        let key = input.cache_key(physical_thumbnail_edge(256, 1.0));

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("My Workshop"),
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", input.clone(), 256, Priority::Prefetch)],
        });
        let started = manager.next_candidate_for_test().expect("job starts");
        assert_eq!(started.priority, Priority::Prefetch);

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("My Workshop"),
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", input, 256, Priority::VisibleRow)],
        });

        assert!(
            manager.next_candidate_for_test().is_none(),
            "the running job covers the promoted interest"
        );

        // Cancel the in-flight job so the entry is re-offered. Its priority on
        // the way back out is the only observable proof the promotion reached
        // the entry rather than being swallowed — without it, this test passes
        // even if `reprioritise_existing` did nothing, because `take_startable`
        // discards a duplicate candidate on state alone.
        let _ = manager.complete_job(
            &key,
            started.job_id,
            started.attempt,
            Ok(ThumbnailWorkerOutcome::Cancelled),
        );
        let requeued = manager
            .next_candidate_for_test()
            .expect("a cancelled job re-offers its interest");
        assert_eq!(
            requeued.priority,
            Priority::VisibleRow,
            "the mid-flight promotion must survive the job it was applied to"
        );
    }

    /// Warm's skip has to ask the *source* tier. It once asked the derived key,
    /// which warm no longer writes — so every session after the first
    /// re-enqueued the entire library.
    #[test]
    fn warm_skips_a_url_whose_source_is_already_banked() {
        let root = crate::test_support::TestDir::new("warm-skip-source-tier");
        let url = "https://example.invalid/already-banked.jpg";
        let cache = crate::media::thumbnail_worker::WorkerDiskCache::new(
            root.path().to_path_buf(),
            1024 * 1024,
        );
        crate::media::thumbnail_worker::write_source_bytes(&cache, url, &[1, 2, 3, 4]);

        let mut manager = Manager::new(Config {
            disk_cache_dir: Some(root.path().to_path_buf()),
            ..Config::default()
        });
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::WarmLibrary,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand(
                "101",
                ThumbnailInput::from_url(url),
                256,
                Priority::WarmLibrary,
            )],
        });

        assert!(
            manager.next_candidate_for_test().is_none(),
            "a URL already in the source tier needs no warm job"
        );
    }

    /// The counterpart: a derived entry is not a source, so it must not satisfy
    /// the skip. Otherwise nothing the user scrolled past would ever have its
    /// source banked.
    #[test]
    fn warm_still_runs_when_only_a_derived_entry_exists() {
        let root = crate::test_support::TestDir::new("warm-skip-derived-only");
        let url = "https://example.invalid/derived-only.jpg";
        let input = ThumbnailInput::from_url(url);
        let key = input.cache_key(physical_thumbnail_edge(256, 1.0));
        let cache = crate::media::thumbnail_worker::WorkerDiskCache::new(
            root.path().to_path_buf(),
            1024 * 1024,
        );
        crate::media::thumbnail_worker::write_disk_cache(&cache, &key, &solid_thumbnail(16, 12, 3));

        let mut manager = Manager::new(Config {
            disk_cache_dir: Some(root.path().to_path_buf()),
            ..Config::default()
        });
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::WarmLibrary,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input, 256, Priority::WarmLibrary)],
        });

        assert!(
            manager.next_candidate_for_test().is_some(),
            "a derived entry does not mean the source is local"
        );
    }

    #[test]
    fn interactive_interest_makes_a_warm_completion_enter_memory() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/shared-warm.jpg");
        let key = input.cache_key(physical_thumbnail_edge(256, 1.0));

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::WarmLibrary,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input.clone(), 256, Priority::WarmLibrary)],
        });
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("Installed Addons"),
            generation: 1,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", input, 256, Priority::VisibleRow)],
        });

        let candidate = manager.next_candidate_for_test().expect("job queued");
        let _ = manager.complete_job(
            &key,
            candidate.job_id,
            candidate.attempt,
            Ok(ThumbnailWorkerOutcome::Completed(prepared_thumbnail(
                solid_thumbnail(16, 12, 3),
            ))),
        );

        assert_eq!(manager.cache_len(), 1);
    }

    #[test]
    fn warm_candidates_start_only_when_allowed_and_yield_to_interactive() {
        let mut manager = Manager::new(Config::default());
        let warm_input = ThumbnailInput::from_url("https://example.invalid/warm-yield.jpg");
        let visible_input = ThumbnailInput::from_url("https://example.invalid/visible.jpg");
        let visible_key = visible_input.cache_key(physical_thumbnail_edge(256, 1.0));

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::WarmLibrary,
            generation: 0,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", warm_input, 256, Priority::WarmLibrary)],
        });
        assert!(
            manager.index.next_candidate(JobId(900), false).is_none(),
            "warm entries never start outside the warm headroom"
        );

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("Installed Addons"),
            generation: 1,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", visible_input, 256, Priority::VisibleRow)],
        });
        let first = manager
            .index
            .next_candidate(JobId(901), true)
            .expect("a candidate is available");
        assert_eq!(
            first.key, visible_key,
            "interactive work outranks warm even in warm-allowed slots"
        );
    }

    #[test]
    fn unknown_thumbhash_url_paints_no_placeholder() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/unseeded.jpg");

        let messages = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("Installed Addons"),
            generation: 1,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row", input, 64, Priority::VisibleRow)],
        });

        assert!(messages.iter().all(|message| !matches!(
            message,
            Message::Delivered(delivery)
                if matches!(delivery.result, DeliveryResult::Placeholder(_))
        )));
    }

    #[test]
    fn duplicate_key_starts_once_and_fans_out_completion_to_current_interests() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/shared.jpg");
        let key = input.cache_key(128);

        assert!(
            manager
                .apply_demands(DemandSet {
                    owner: Owner::AddonGrid("Installed Addons"),
                    generation: 1,
                    replace: ReplaceMode::Owner,
                    demands: vec![
                        demand("row-a", input.clone(), 128, Priority::VisibleRow),
                        demand("row-b", input, 128, Priority::VisibleRow),
                    ],
                })
                .is_empty()
        );

        let candidate = manager
            .next_candidate_for_test()
            .expect("deduped demand should start once");
        assert_eq!(candidate.key, key);
        assert!(manager.next_candidate_for_test().is_none());

        let effects = manager.complete_job(
            &key,
            candidate.job_id,
            candidate.attempt,
            Ok(ThumbnailWorkerOutcome::Completed(prepared_thumbnail(
                solid_thumbnail(16, 12, 3),
            ))),
        );
        let messages = effects.messages;

        assert_eq!(messages.len(), 2);
        assert_eq!(manager.cache_len(), 1);
        assert!(messages.iter().all(|message| match message {
            Message::Delivered(delivery) => {
                delivery.key == key && matches!(delivery.result, DeliveryResult::Ready(_))
            }
            _ => false,
        }));
    }

    #[test]
    fn ready_thumbnail_creates_animation_frame_handles_once() {
        let dir = crate::test_support::TestDir::new("gmpublished-ready-animation");
        let gif = dir.gif("animated.gif", 8, 8);
        let thumbnail = crate::media::thumbnail_worker::ThumbnailDecoder::new()
            .decode_and_resize_file(gif, 64)
            .expect("animated GIF thumbnail should decode");
        let ready = ready_thumbnail(
            ThumbnailKey::for_bytes("animated", 64),
            &prepared_thumbnail(thumbnail),
        );

        let animation = ready
            .animation()
            .expect("animated GIF should prepare ready frames");

        assert_eq!(animation.frame_count(), 2);
        assert!(animation.frames().iter().all(|frame| {
            frame.delay() > std::time::Duration::ZERO && frame.handle().id() != ready.handle().id()
        }));
    }

    #[test]
    fn owner_replacement_drops_offscreen_demand() {
        let mut manager = Manager::new(Config::default());
        let owner = Owner::AddonGrid("Installed Addons");
        let input = ThumbnailInput::from_url("https://example.invalid/old.jpg");

        let _ = manager.apply_demands(DemandSet {
            owner: owner.clone(),
            generation: 1,
            replace: ReplaceMode::Owner,
            demands: vec![demand("old-row", input, 64, Priority::VisibleRow)],
        });
        assert_eq!(manager.pending_count(), 1);

        let messages = manager.apply_demands(DemandSet::empty(owner));

        assert!(messages.is_empty());
        assert_eq!(manager.pending_count(), 0);
        assert!(manager.next_candidate_for_test().is_none());
    }

    #[test]
    fn owner_replacement_cancels_dequeued_job_and_releases_its_slot() {
        let mut manager = Manager::new(Config::default());
        let owner = Owner::AddonGrid("Installed Addons");
        let input = ThumbnailInput::from_url("https://example.invalid/old.jpg");

        let _ = manager.apply_demands(DemandSet {
            owner: owner.clone(),
            generation: 1,
            replace: ReplaceMode::Owner,
            demands: vec![demand("old-row", input.clone(), 64, Priority::VisibleRow)],
        });
        let candidate = manager
            .next_candidate_for_test()
            .expect("thumbnail job should start");
        assert_eq!(manager.index.in_flight_count(), 1);
        assert!(!candidate.cancellation.is_cancelled());

        let messages = manager.apply_demands(DemandSet::empty(owner));

        assert!(messages.is_empty());
        assert!(candidate.cancellation.is_cancelled());
        assert_eq!(manager.index.in_flight_count(), 0);
        assert_eq!(manager.pending_count(), 0);

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::AddonGrid("Installed Addons"),
            generation: 2,
            replace: ReplaceMode::Owner,
            demands: vec![demand("old-row", input, 64, Priority::VisibleRow)],
        });
        let fresh = manager
            .next_candidate_for_test()
            .expect("re-demanded thumbnail should start fresh");
        assert_ne!(fresh.job_id, candidate.job_id);
        assert_eq!(fresh.attempt, RetryAttempt::default());
        assert!(!fresh.cancellation.is_cancelled());
    }

    #[test]
    fn owner_replacement_keeps_job_alive_when_the_key_remains_demanded() {
        let mut manager = Manager::new(Config::default());
        let owner = Owner::AddonGrid("Installed Addons");
        let input = ThumbnailInput::from_url("https://example.invalid/visible.jpg");

        let _ = manager.apply_demands(DemandSet {
            owner: owner.clone(),
            generation: 1,
            replace: ReplaceMode::Owner,
            demands: vec![demand(
                "visible-row",
                input.clone(),
                64,
                Priority::VisibleRow,
            )],
        });
        let candidate = manager
            .next_candidate_for_test()
            .expect("thumbnail job should start");

        let _ = manager.apply_demands(DemandSet {
            owner,
            generation: 2,
            replace: ReplaceMode::Owner,
            demands: vec![demand("visible-row", input, 64, Priority::VisibleRow)],
        });

        assert!(!candidate.cancellation.is_cancelled());
        assert_eq!(manager.index.in_flight_count(), 1);
        assert_eq!(manager.pending_count(), 1);
    }

    #[test]
    fn successful_stale_completion_still_enters_memory_cache() {
        let mut manager = Manager::new(Config::default());
        let owner = Owner::AddonGrid("Installed Addons");
        let input = ThumbnailInput::from_url("https://example.invalid/old.jpg");
        let key = input.cache_key(64);

        let _ = manager.apply_demands(DemandSet {
            owner: owner.clone(),
            generation: 1,
            replace: ReplaceMode::Owner,
            demands: vec![demand("old-row", input, 64, Priority::VisibleRow)],
        });
        let candidate = manager
            .next_candidate_for_test()
            .expect("thumbnail job should start");
        let _ = manager.apply_demands(DemandSet::empty(owner));

        let effects = manager.complete_job(
            &key,
            candidate.job_id,
            candidate.attempt,
            Ok(ThumbnailWorkerOutcome::Completed(prepared_thumbnail(
                solid_thumbnail(16, 12, 7),
            ))),
        );

        assert!(effects.messages.is_empty());
        assert!(effects.retry.is_none());
        assert_eq!(manager.cache_len(), 1);
        assert!(manager.cache.get(&key).is_some());
    }

    #[test]
    fn transient_fetch_failures_wait_for_retry_without_terminal_delivery() {
        for source in [
            ureq::Error::ConnectionFailed,
            ureq::Error::StatusCode(503),
            ureq::Error::Timeout(ureq::Timeout::RecvResponse),
        ] {
            let mut manager = Manager::new(Config::default());
            let input = ThumbnailInput::from_url("https://example.invalid/retry.jpg");
            let key = input.cache_key(64);
            let _ = manager.apply_demands(DemandSet {
                owner: Owner::AddonGrid("Installed Addons"),
                generation: 1,
                replace: ReplaceMode::Owner,
                demands: vec![demand("row", input, 64, Priority::VisibleRow)],
            });
            let candidate = manager
                .next_candidate_for_test()
                .expect("thumbnail job should start");

            let effects = manager.complete_job(
                &key,
                candidate.job_id,
                candidate.attempt,
                Err(fetch_error(&key, source)),
            );

            assert!(effects.messages.is_empty());
            assert_eq!(
                effects.retry.as_ref().map(|retry| retry.delay),
                Some(Duration::from_secs(1))
            );
            assert_eq!(manager.pending_count(), 1);
            assert_eq!(manager.index.state_for_key(&key), DemandState::RetryWaiting);
        }
    }

    #[test]
    fn retry_backoff_is_one_then_four_seconds_and_stops_after_two_retries() {
        let error = fetch_error(
            &ThumbnailKey::for_url("https://example.invalid/retry.jpg", 64),
            ureq::Error::ConnectionFailed,
        );

        assert_eq!(
            retry_delay(RetryAttempt(0), &error),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            retry_delay(RetryAttempt(1), &error),
            Some(Duration::from_secs(4))
        );
        assert_eq!(retry_delay(RetryAttempt(2), &error), None);
    }

    #[test]
    fn client_and_decode_failures_deliver_terminal_failure_without_retry() {
        let errors = [
            fetch_error(
                &ThumbnailKey::for_url("https://example.invalid/missing.jpg", 64),
                ureq::Error::StatusCode(404),
            ),
            ThumbnailDeliveryError::Thumbnail(Arc::new(ThumbnailError::ImageIo(
                std::io::Error::other("invalid image bytes"),
            ))),
        ];

        for error in errors {
            let mut manager = Manager::new(Config::default());
            let input = ThumbnailInput::from_url("https://example.invalid/permanent.jpg");
            let key = input.cache_key(64);
            let _ = manager.apply_demands(DemandSet {
                owner: Owner::AddonGrid("Installed Addons"),
                generation: 1,
                replace: ReplaceMode::Owner,
                demands: vec![demand("row", input, 64, Priority::VisibleRow)],
            });
            let candidate = manager
                .next_candidate_for_test()
                .expect("thumbnail job should start");

            let effects =
                manager.complete_job(&key, candidate.job_id, candidate.attempt, Err(error));

            assert!(effects.retry.is_none());
            assert_eq!(effects.messages.len(), 1);
            assert!(matches!(
                &effects.messages[0],
                Message::Delivered(delivery)
                    if matches!(delivery.result, DeliveryResult::Failed { .. })
            ));
            assert_eq!(manager.pending_count(), 0);
        }
    }

    #[test]
    fn priority_prefers_active_detail_over_visible_rows() {
        let mut manager = Manager::new(Config::default());
        let row = ThumbnailInput::from_url("https://example.invalid/row.jpg");
        let detail = ThumbnailInput::from_url("https://example.invalid/detail.jpg");
        let detail_key = detail.cache_key(256);

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::PreviewGma,
            generation: 1,
            replace: ReplaceMode::Owner,
            demands: vec![
                demand("row", row, 256, Priority::VisibleRow),
                demand("detail", detail, 256, Priority::ActiveDetail),
            ],
        });

        let candidate = manager
            .next_candidate_for_test()
            .expect("highest-priority demand should start first");

        assert_eq!(candidate.key, detail_key);
    }

    #[test]
    fn priority_prefers_visible_row_over_prefetch_regardless_of_insertion_order() {
        for visible_first in [true, false] {
            let mut manager = Manager::new(Config {
                max_in_flight: 1,
                ..Config::default()
            });
            let visible = ThumbnailInput::from_url("https://example.invalid/visible.jpg");
            let prefetch = ThumbnailInput::from_url("https://example.invalid/prefetch.jpg");
            let visible_key = visible.cache_key(256);
            let visible_demand = demand("visible", visible, 256, Priority::VisibleRow);
            let prefetch_demand = demand("prefetch", prefetch, 256, Priority::Prefetch);
            let demands = if visible_first {
                vec![visible_demand, prefetch_demand]
            } else {
                vec![prefetch_demand, visible_demand]
            };

            let _ = manager.apply_demands(DemandSet {
                owner: Owner::AddonGrid("Installed Addons"),
                generation: 1,
                replace: ReplaceMode::Owner,
                demands,
            });

            let candidate = manager
                .next_candidate_for_test()
                .expect("visible row should start first");

            assert_eq!(candidate.key, visible_key);
        }
    }

    #[test]
    fn disk_cache_path_uses_worker_lru_cache_directory() {
        let manager = Manager::new(Config {
            disk_cache_dir: Some(PathBuf::from("/tmp/gmpublished-thumbnails")),
            ..Config::default()
        });
        let key = ThumbnailKey::for_bytes("avatar", 32);

        assert_eq!(
            manager.disk_cache_path(&key),
            Some(PathBuf::from("/tmp/gmpublished-thumbnails").join(key.disk_file_name()))
        );
    }

    fn demand(
        id: impl Into<String>,
        input: ThumbnailInput,
        max_edge: u32,
        priority: Priority,
    ) -> Demand {
        Demand {
            id: DemandId::new(id),
            input,
            logical_max_edge: max_edge,
            priority,
        }
    }

    fn solid_thumbnail(width: u32, height: u32, seed: u8) -> Thumbnail {
        let mut pixels = vec![0; (width * height * 4) as usize];
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.copy_from_slice(&[seed, seed.wrapping_add(1), seed.wrapping_add(2), 255]);
        }

        Thumbnail::new(
            pixels,
            ThumbnailMetadata {
                width,
                height,
                source_width: width,
                source_height: height,
                max_edge: width.max(height),
            },
        )
        .expect("solid thumbnail fixture should be valid")
    }

    fn prepared_thumbnail(thumbnail: Thumbnail) -> PreparedThumbnail {
        PreparedThumbnail::from_thumbnail(thumbnail)
    }

    fn fetch_error(_key: &ThumbnailKey, source: ureq::Error) -> ThumbnailDeliveryError {
        ThumbnailDeliveryError::Thumbnail(Arc::new(ThumbnailError::UrlFetch {
            url: String::from("https://example.invalid/thumbnail.jpg"),
            source,
        }))
    }
}
