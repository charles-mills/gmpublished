//! Views report visible rows. Feature updates translate those rows into demand
//! sets, and this manager is the only path that starts decode work.

use std::{collections::HashSet, fmt, path::PathBuf, sync::Arc, time::Duration};

use iced::Task;
use quick_cache::{DefaultHashBuilder, unsync::Cache};

use crate::{
    bridge::tasks::BackendContext,
    media::thumbnail_worker::{
        FetchProfile, PreparedThumbnail, ThumbnailError, ThumbnailKey, ThumbnailMode,
        ThumbnailWorkerOutcome, WorkerDiskCache, run_prepared_thumbnail_request, validate_max_edge,
    },
};

#[cfg(test)]
use crate::generation::Generation;
#[cfg(test)]
use crate::media::thumbnail_worker::{Thumbnail, ThumbnailInput, ThumbnailMetadata};

mod delivery;
mod demand;
mod index;
mod placeholders;

pub use delivery::{Delivery, DeliveryResult, Message, ReadyThumbnail, ThumbnailDeliveryError};
pub use demand::{
    Demand, DemandId, DemandSet, Owner, Priority, ReplaceMode, bucketed_thumbnail_scale,
    physical_thumbnail_edge, prefetch_ranges, retained_rows,
};
pub use index::{JobId, RetryAttempt, RetryId};
pub use placeholders::PlaceholderImage;

use delivery::{ReadyThumbnailWeighter, ready_thumbnail, worker_result_message};
#[cfg(test)]
use index::DemandState;
use index::{DemandEntry, DemandIndex, StartCandidate};
use placeholders::PlaceholderStore;

const DEFAULT_ESTIMATED_ITEMS: usize = 128;
// Two media-pool widths hide the WorkerFinished -> pump round trip while
// cancelled FIFO entries yield before doing I/O.
const DEFAULT_MAX_IN_FLIGHT: usize = 32;
// Flat cache budget: rows release off-screen handles, so the cache is the
// actual ceiling; 256MB covers a broad retina-tile recency window without scaling
// quadratically with density.
const DEFAULT_MEMORY_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_DISK_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Ceiling on background warm jobs in flight.
///
/// A cap on warm, not a reservation for it: the gate counts *total* in-flight
/// work, so interactive jobs occupy the same pipe and warm gets whatever is
/// left under this number. That is the conservative direction — a scroll burst
/// squeezes warming out rather than queueing behind it.
const WARM_MAX_IN_FLIGHT: usize = 8;

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

pub struct Manager {
    config: Config,
    disk_cache: Option<WorkerDiskCache>,
    cache: HandleCache,
    index: DemandIndex,
    scale_factor: f32,
    scale_bucket: f32,
    next_sequence: u64,
    next_work_id: u64,
    placeholders: PlaceholderStore,
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
            placeholders: PlaceholderStore::default(),
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
            self.placeholders.remember(&url, hash);
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
        for mut demand in set.demands {
            // The owner decides warm-ness; the priority carried by a demand is
            // only its position within a tier. Five downstream sites classify a
            // demand as warm by its priority and one by its owner, so they can
            // only agree if the two are reconciled here, at the single point
            // demands enter the manager.
            if set.owner.is_warm() {
                demand.priority = Priority::WarmLibrary;
            }

            let physical_max_edge =
                physical_thumbnail_edge(demand.logical_max_edge, self.scale_factor);
            let key = if set.owner == Owner::SizeAnalyzer {
                demand
                    .input
                    .cache_key_with_mode(physical_max_edge, ThumbnailMode::Static)
            } else {
                demand.input.cache_key(physical_max_edge)
            };
            let interest = index::InterestKey {
                owner: set.owner,
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
                    set.owner,
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
                // key. Warm writes no derived entries, so a derived-key check
                // could never hit — it would re-enqueue the entire library on
                // every session after the first.
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
                    set.owner,
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
                    set.owner,
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
                && let Some(placeholder) = self.placeholders.get(demand.input.source_url())
            {
                immediate.push(Message::Delivered(Box::new(Delivery::placeholder(
                    set.owner,
                    set.generation,
                    demand.id.clone(),
                    key.clone(),
                    placeholder,
                ))));
            }

            let state = self.index.state_for_key(&key);
            let sequence = self.allocate_sequence();
            self.index.add(DemandEntry::new(
                set.owner,
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
                    self.placeholders.remember(url, hash);
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
        let request = candidate.request;
        let key = request.key().clone();
        let message_key = key.clone();
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
            run_prepared_thumbnail_request(disk_cache.as_ref(), &request, profile, &cancellation)
        })
        .map(move |result| worker_result_message(message_key.clone(), job_id, attempt, result))
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
        self.index.pending_len()
    }

    /// A clone of the disk-cache handle, for work that belongs on a blocking
    /// thread rather than in `update`. Clones share one index and one budget.
    pub(crate) fn disk_cache_handle(&self) -> Option<WorkerDiskCache> {
        self.disk_cache.clone()
    }

    /// Scales the disk-cache eviction budget to the library so a full warm
    /// actually fits (the 256 MB default thrashes below library size).
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
            .field("pending", &self.index.pending_len())
            .field("in_flight", &self.index.in_flight_count())
            .finish()
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
    fn cached_demand_delivers_ready_without_queueing_decode() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/shared.jpg");
        let key = input.cache_key(64);
        let cached = manager.cache_thumbnail(key.clone(), solid_thumbnail(8, 6, 12));

        let messages = manager.apply_demands(DemandSet {
            owner: Owner::InstalledAddons,
            generation: Generation::from_raw(9),
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", input, 64, Priority::VisibleRow)],
        });

        assert_eq!(manager.pending_count(), 0);
        assert_eq!(messages.len(), 1);
        let Message::Delivered(delivery) = &messages[0] else {
            panic!("cached thumbnail should deliver immediately");
        };
        assert_eq!(delivery.owner, Owner::InstalledAddons);
        assert_eq!(delivery.generation, Generation::from_raw(9));
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
            owner: Owner::InstalledAddons,
            generation: Generation::from_raw(3),
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
            generation: Generation::INITIAL,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", input, 64, Priority::SizeAnalyzer)],
        });

        assert!(messages.is_empty());
        let candidate = manager
            .next_candidate_for_test()
            .expect("analyzer decode should be queued");
        assert_eq!(candidate.request.key().mode(), ThumbnailMode::Static);
    }

    #[test]
    fn size_analyzer_replaces_queued_warm_work_for_the_same_addon() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/shared.jpg");
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::WarmLibrary,
            generation: Generation::INITIAL,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input.clone(), 256, Priority::WarmLibrary)],
        });
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::SizeAnalyzer,
            generation: Generation::INITIAL,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input, 64, Priority::SizeAnalyzer)],
        });

        assert_eq!(manager.pending_count(), 1);
        let candidate = manager
            .next_candidate_for_test()
            .expect("static analyzer work remains");
        assert_eq!(candidate.request.key().mode(), ThumbnailMode::Static);
    }

    #[test]
    fn size_analyzer_waits_for_an_active_animated_source() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/shared.jpg");
        let animated_key = input.cache_key(256);
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::WarmLibrary,
            generation: Generation::INITIAL,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input.clone(), 256, Priority::WarmLibrary)],
        });
        let warm = manager
            .next_candidate_for_test()
            .expect("warm source starts first");
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::SizeAnalyzer,
            generation: Generation::INITIAL,
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
        assert_eq!(candidate.request.key().mode(), ThumbnailMode::Static);
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
            generation: Generation::INITIAL,
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
            generation: Generation::INITIAL,
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
            generation: Generation::INITIAL,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input.clone(), 256, Priority::WarmLibrary)],
        });
        let warm = manager.next_candidate_for_test().expect("warm job queued");

        // The row arrives mid-flight, so it joins the running job rather than
        // starting one of its own.
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::InstalledAddons,
            generation: Generation::from_raw(1),
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
        assert_eq!(*restarted.request.key(), key);
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
    #[test]
    fn a_row_promoted_to_visible_is_re_offered_at_its_new_priority() {
        let mut manager = Manager::new(Config::default());
        let ahead = ThumbnailInput::from_url("https://example.invalid/ahead.jpg");
        let promoted = ThumbnailInput::from_url("https://example.invalid/promoted.jpg");

        // `promoted` is demanded first, so it wins any tie on sequence — the
        // test would pass for the wrong reason if priority were ignored.
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::MyWorkshop,
            generation: Generation::INITIAL,
            replace: ReplaceMode::Owner,
            demands: vec![
                demand("row-1", promoted.clone(), 256, Priority::Prefetch),
                demand("row-2", ahead.clone(), 256, Priority::Prefetch),
            ],
        });

        // The user scrolls toward row-1: same owner, same id, same key, higher
        // priority. Re-demanded alongside row-2 exactly as a real grid tick does.
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::MyWorkshop,
            generation: Generation::INITIAL,
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
            *first.request.key(),
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
            owner: Owner::MyWorkshop,
            generation: Generation::INITIAL,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", settling.clone(), 256, Priority::VisibleRow)],
        });
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::MyWorkshop,
            generation: Generation::INITIAL,
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
            *first.request.key(),
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
            owner: Owner::MyWorkshop,
            generation: Generation::INITIAL,
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", input.clone(), 256, Priority::Prefetch)],
        });
        let started = manager.next_candidate_for_test().expect("job starts");
        assert_eq!(started.priority, Priority::Prefetch);

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::MyWorkshop,
            generation: Generation::INITIAL,
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

    /// Warm's skip has to ask the *source* tier. Asking the derived key could
    /// never hit — warm writes no derived entries — so every session after the
    /// first would re-enqueue the entire library.
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
            generation: Generation::INITIAL,
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
            generation: Generation::INITIAL,
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
            generation: Generation::INITIAL,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", input.clone(), 256, Priority::WarmLibrary)],
        });
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::InstalledAddons,
            generation: Generation::from_raw(1),
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
            generation: Generation::INITIAL,
            replace: ReplaceMode::Owner,
            demands: vec![demand("101", warm_input, 256, Priority::WarmLibrary)],
        });
        assert!(
            manager.index.next_candidate(JobId(900), false).is_none(),
            "warm entries never start outside the warm headroom"
        );

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::InstalledAddons,
            generation: Generation::from_raw(1),
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", visible_input, 256, Priority::VisibleRow)],
        });
        let first = manager
            .index
            .next_candidate(JobId(901), true)
            .expect("a candidate is available");
        assert_eq!(
            *first.request.key(),
            visible_key,
            "interactive work outranks warm even in warm-allowed slots"
        );
    }

    #[test]
    fn unknown_thumbhash_url_paints_no_placeholder() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.invalid/unseeded.jpg");

        let messages = manager.apply_demands(DemandSet {
            owner: Owner::InstalledAddons,
            generation: Generation::from_raw(1),
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
                    owner: Owner::InstalledAddons,
                    generation: Generation::from_raw(1),
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
        assert_eq!(*candidate.request.key(), key);
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
    fn owner_replacement_drops_offscreen_demand() {
        let mut manager = Manager::new(Config::default());
        let owner = Owner::InstalledAddons;
        let input = ThumbnailInput::from_url("https://example.invalid/old.jpg");

        let _ = manager.apply_demands(DemandSet {
            owner,
            generation: Generation::from_raw(1),
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
        let owner = Owner::InstalledAddons;
        let input = ThumbnailInput::from_url("https://example.invalid/old.jpg");

        let _ = manager.apply_demands(DemandSet {
            owner,
            generation: Generation::from_raw(1),
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
            owner: Owner::InstalledAddons,
            generation: Generation::from_raw(2),
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

    /// A backpressured job that is then abandoned must not leave its attempt
    /// count behind: the key is out of `active_jobs` and never was a delayed
    /// retry, so it is reachable only through `retry_attempts`. A survivor
    /// there silently costs the *next* demand for that key its retries.
    #[test]
    fn abandoning_a_backpressured_job_does_not_strand_its_attempt_count() {
        let mut manager = Manager::new(Config::default());
        let owner = Owner::InstalledAddons;
        let input = ThumbnailInput::from_url("https://example.invalid/backpressured.jpg");

        let _ = manager.apply_demands(DemandSet {
            owner,
            generation: Generation::from_raw(1),
            replace: ReplaceMode::Owner,
            demands: vec![demand("row", input.clone(), 64, Priority::VisibleRow)],
        });
        let candidate = manager
            .next_candidate_for_test()
            .expect("thumbnail job should start");

        // The worker pool refused it, so the key returns to queued carrying an
        // attempt count.
        let key = candidate.request.key().clone();
        let attempt = candidate.attempt.next().expect("a first retry is allowed");
        manager
            .index
            .mark_key_queued(&key, candidate.job_id, attempt);

        let _ = manager.apply_demands(DemandSet::empty(owner));
        assert_eq!(manager.pending_count(), 0);

        let _ = manager.apply_demands(DemandSet {
            owner,
            generation: Generation::from_raw(2),
            replace: ReplaceMode::Owner,
            demands: vec![demand("row", input, 64, Priority::VisibleRow)],
        });
        let fresh = manager
            .next_candidate_for_test()
            .expect("re-demanded thumbnail should start");

        assert_eq!(
            fresh.attempt,
            RetryAttempt::default(),
            "a fresh demand must begin at attempt zero, not inherit the abandoned job's"
        );
    }

    #[test]
    fn owner_replacement_keeps_job_alive_when_the_key_remains_demanded() {
        let mut manager = Manager::new(Config::default());
        let owner = Owner::InstalledAddons;
        let input = ThumbnailInput::from_url("https://example.invalid/visible.jpg");

        let _ = manager.apply_demands(DemandSet {
            owner,
            generation: Generation::from_raw(1),
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
            generation: Generation::from_raw(2),
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
        let owner = Owner::InstalledAddons;
        let input = ThumbnailInput::from_url("https://example.invalid/old.jpg");
        let key = input.cache_key(64);

        let _ = manager.apply_demands(DemandSet {
            owner,
            generation: Generation::from_raw(1),
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
                owner: Owner::InstalledAddons,
                generation: Generation::from_raw(1),
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
                Err(fetch_error(source)),
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
        let error = fetch_error(ureq::Error::ConnectionFailed);

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
            fetch_error(ureq::Error::StatusCode(404)),
            ThumbnailDeliveryError::Thumbnail(Arc::new(
                crate::media::thumbnail_worker::ThumbnailDecodeError::ImageIo(
                    std::io::Error::other("invalid image bytes"),
                )
                .into(),
            )),
        ];

        for error in errors {
            let mut manager = Manager::new(Config::default());
            let input = ThumbnailInput::from_url("https://example.invalid/permanent.jpg");
            let key = input.cache_key(64);
            let _ = manager.apply_demands(DemandSet {
                owner: Owner::InstalledAddons,
                generation: Generation::from_raw(1),
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

    /// The invariant the two removal paths kept breaking: an interest dropped
    /// from `entries` without pruning `by_owner` leaves a ghost that
    /// accumulates for the whole session, because nothing ever revisits that
    /// bucket.
    #[test]
    fn replacing_an_owners_demands_leaves_no_ghosts_in_its_bucket() {
        let mut manager = Manager::new(Config::default());
        let first = ThumbnailInput::from_url("https://example.invalid/first.jpg");
        let second = ThumbnailInput::from_url("https://example.invalid/second.jpg");

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::InstalledAddons,
            generation: Generation::from_raw(1),
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-1", first, 256, Priority::VisibleRow)],
        });
        let _ = manager.apply_demands(DemandSet {
            owner: Owner::InstalledAddons,
            generation: Generation::from_raw(2),
            replace: ReplaceMode::Owner,
            demands: vec![demand("row-2", second, 256, Priority::VisibleRow)],
        });

        assert_eq!(manager.pending_count(), 1);
        assert_eq!(
            manager
                .index
                .owner_bucket_len(&Owner::InstalledAddons)
                .unwrap_or(0),
            1,
            "the replaced interest must not survive in the owner bucket"
        );

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::InstalledAddons,
            generation: Generation::from_raw(3),
            replace: ReplaceMode::Owner,
            demands: Vec::new(),
        });

        assert_eq!(manager.pending_count(), 0);
        assert!(
            manager
                .index
                .owner_bucket_len(&Owner::InstalledAddons)
                .is_none(),
            "an owner with no interests must not keep an empty bucket"
        );
    }

    #[test]
    fn priority_prefers_active_detail_over_visible_rows() {
        let mut manager = Manager::new(Config::default());
        let row = ThumbnailInput::from_url("https://example.invalid/row.jpg");
        let detail = ThumbnailInput::from_url("https://example.invalid/detail.jpg");
        let detail_key = detail.cache_key(256);

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::PreviewGma,
            generation: Generation::from_raw(1),
            replace: ReplaceMode::Owner,
            demands: vec![
                demand("row", row, 256, Priority::VisibleRow),
                demand("detail", detail, 256, Priority::ActiveDetail),
            ],
        });

        let candidate = manager
            .next_candidate_for_test()
            .expect("highest-priority demand should start first");

        assert_eq!(*candidate.request.key(), detail_key);
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
                owner: Owner::InstalledAddons,
                generation: Generation::from_raw(1),
                replace: ReplaceMode::Owner,
                demands,
            });

            let candidate = manager
                .next_candidate_for_test()
                .expect("visible row should start first");

            assert_eq!(*candidate.request.key(), visible_key);
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

    /// Warm-ness is asked of the priority at five sites and of the owner at
    /// one. A set whose owner is warm but whose demands carry an interactive
    /// priority must not be classified both ways.
    #[test]
    fn a_warm_owner_overrides_an_interactive_demand_priority() {
        let mut manager = Manager::new(Config::default());
        let input = ThumbnailInput::from_url("https://example.com/warm.png");

        let _ = manager.apply_demands(DemandSet {
            owner: Owner::WarmLibrary,
            generation: Generation::INITIAL,
            replace: ReplaceMode::Owner,
            demands: vec![demand("1", input, 256, Priority::VisibleRow)],
        });

        assert_eq!(manager.pending_count(), 1, "the demand should be queued");
        assert!(
            manager
                .index
                .priorities()
                .iter()
                .all(|priority| *priority == Priority::WarmLibrary),
            "a warm owner's demands are warm regardless of the priority asked for"
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

    fn fetch_error(source: ureq::Error) -> ThumbnailDeliveryError {
        ThumbnailDeliveryError::Thumbnail(Arc::new(ThumbnailError::UrlFetch {
            url: String::from("https://example.invalid/thumbnail.jpg"),
            source,
        }))
    }
}
