//! The live ledger of demand: every interest that currently exists, the maps
//! that key it by owner and by thumbnail, and the queued/in-flight/retrying
//! state that decides which job starts next.

use std::{
    collections::{BinaryHeap, HashMap, HashSet},
    time::Duration,
};

use crate::{
    generation::Generation,
    media::thumbnail_worker::{
        ThumbnailCancellation, ThumbnailInput, ThumbnailKey, ThumbnailMode, ThumbnailRequest,
    },
};

use super::{Demand, DemandCapabilities, DemandId, Owner, Priority};

#[derive(Default)]
pub(super) struct DemandIndex {
    entries: HashMap<InterestKey, DemandEntry>,
    key_to_interests: HashMap<ThumbnailKey, Vec<InterestKey>>,
    active_jobs: HashMap<ThumbnailKey, ActiveJob>,
    delayed_retries: HashMap<ThumbnailKey, RetryId>,
    retry_attempts: HashMap<ThumbnailKey, RetryAttempt>,
    /// Interests grouped by owner, so a demand-set replacement touches only
    /// that owner's entries.
    ///
    /// Without it, tearing down a grid's ~72-entry window scanned every entry
    /// in the index — including the whole-library cache-only set, which on a cold
    /// disk cache is one entry per addon.
    by_owner: HashMap<Owner, HashSet<InterestKey>>,
    /// Queued interactive candidates, ordered by `(priority, sequence)`.
    ///
    /// Lazily deleted: entries are pushed on queue and simply skipped when
    /// popped if they are no longer queued. Removing from the middle of a heap
    /// is what an index of heap positions would buy, and the bookkeeping costs
    /// more than the stale pops it avoids.
    ready: BinaryHeap<Candidate>,
    /// Queued cache-only candidates, kept in their own heap.
    ///
    /// Cache-only work is blocked outright whenever the interactive tiers hold
    /// more than `CACHE_ONLY_MAX_IN_FLIGHT` slots. Sharing one heap meant every
    /// call in that state popped the entire background backlog just to discover none of it
    /// was startable, then pushed it all back — reintroducing the O(library)
    /// scan the heap exists to remove. Partitioning lets that case skip the
    /// backlog entirely.
    ready_cache_only: BinaryHeap<Candidate>,
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
    pub(super) fn add(&mut self, entry: DemandEntry) {
        let interest = entry.interest_key();
        let key = entry.key.clone();
        self.key_to_interests
            .entry(key)
            .or_default()
            .push(interest.clone());
        let _ = self
            .by_owner
            .entry(entry.owner)
            .or_default()
            .insert(interest.clone());
        if entry.state == DemandState::Queued {
            self.enqueue(
                &interest,
                entry.priority,
                entry.sequence,
                entry.capabilities,
            );
        }
        self.entries.insert(interest, entry);
    }

    fn enqueue(
        &mut self,
        interest: &InterestKey,
        priority: Priority,
        sequence: u64,
        capabilities: DemandCapabilities,
    ) {
        let candidate = Candidate {
            priority: std::cmp::Reverse(priority),
            sequence: std::cmp::Reverse(sequence),
            interest: interest.clone(),
        };
        if capabilities.is_cache_only() {
            self.ready_cache_only.push(candidate);
        } else {
            self.ready.push(candidate);
        }
    }

    /// Drops this owner's interests that are absent from `keep`, leaving the
    /// rest untouched.
    pub(super) fn retain_owner(&mut self, owner: &Owner, keep: &HashSet<InterestKey>) {
        let Some(interests) = self.by_owner.get(owner) else {
            return;
        };
        let stale = interests.difference(keep).cloned().collect::<Vec<_>>();
        for interest in &stale {
            let _ = self.remove_interest(interest);
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
    pub(super) fn reprioritise_existing(
        &mut self,
        interest: &InterestKey,
        priority: Priority,
        promoted_sequence: u64,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(interest) else {
            return false;
        };

        if entry.priority != priority {
            entry.priority = priority;
            entry.sequence = promoted_sequence;
            let (state, sequence, capabilities) = (entry.state, entry.sequence, entry.capabilities);
            if state == DemandState::Queued {
                // The old heap candidate still carries the old priority and
                // sequence; it is skipped on pop by the sequence check.
                self.enqueue(interest, priority, sequence, capabilities);
            }
        }
        true
    }

    pub(super) fn remove_queued_cache_only(&mut self, ids: &HashSet<DemandId>) {
        self.remove_where(|entry| {
            entry.capabilities.is_cache_only()
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

    /// The only way an interest leaves the index, so every derived map stays
    /// in step. Callers must not prune `by_owner` themselves — an entry
    /// removed while its bucket keeps it is a ghost nothing revisits.
    ///
    /// Any stale heap candidate is skipped on pop.
    fn remove_interest(&mut self, interest: &InterestKey) -> Option<DemandEntry> {
        let entry = self.entries.remove(interest)?;
        if let Some(interests) = self.key_to_interests.get_mut(&entry.key) {
            interests.retain(|candidate| candidate != interest);
            if interests.is_empty() {
                self.key_to_interests.remove(&entry.key);
            }
        }
        if let Some(interests) = self.by_owner.get_mut(&entry.owner) {
            let _ = interests.remove(interest);
            if interests.is_empty() {
                let _ = self.by_owner.remove(&entry.owner);
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
    pub(super) fn cancel_uninterested_work(&mut self) {
        // `retry_attempts` is swept alongside the two work maps because a
        // backpressured job leaves a key in *only* that one: it is out of
        // `active_jobs` and was never a delayed retry. Left behind, the count
        // outlives every interest in its key, and the next demand for that key
        // — an hour and a scroll later — inherits the elevated attempt and
        // gives up sooner than a first attempt should.
        let keys = self
            .active_jobs
            .keys()
            .chain(self.delayed_retries.keys())
            .chain(self.retry_attempts.keys())
            .filter(|key| !self.has_interests(key))
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            self.cancel_key(&key);
        }
    }

    pub(super) fn state_for_key(&self, key: &ThumbnailKey) -> DemandState {
        if self.active_jobs.contains_key(key) {
            DemandState::InFlight
        } else if self.delayed_retries.contains_key(key) {
            DemandState::RetryWaiting
        } else {
            DemandState::Queued
        }
    }

    pub(super) fn next_candidate(
        &mut self,
        job_id: JobId,
        allow_cache_only: bool,
    ) -> Option<StartCandidate> {
        // Pops in scheduling order, discarding candidates that are stale
        // (their entry is gone, or no longer queued) and setting aside the ones
        // that are merely blocked right now so they are not lost.
        let selected = self.take_startable(false).or_else(|| {
            allow_cache_only
                .then(|| self.take_startable(true))
                .flatten()
        });

        let (key, input, physical_max_edge, _priority, capabilities) = selected?;
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

        Some(StartCandidate {
            request: ThumbnailRequest::new(input, physical_max_edge, key.mode()),
            #[cfg(test)]
            priority: _priority,
            capabilities,
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
        cache_only: bool,
    ) -> Option<(
        ThumbnailKey,
        ThumbnailInput,
        u32,
        Priority,
        DemandCapabilities,
    )> {
        let mut deferred = Vec::new();
        let selected = loop {
            let heap = if cache_only {
                &mut self.ready_cache_only
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
                entry.capabilities,
            ));
        };

        if cache_only {
            self.ready_cache_only.extend(deferred);
        } else {
            self.ready.extend(deferred);
        }
        selected
    }

    /// True when `key` has at least one interest and every one is cache-only —
    /// the completion should fill the disk cache without churning the memory
    /// cache's recency window. No interests at all is NOT cache-only: a
    /// scrolled-past completion is exactly what the memory cache wants.
    pub(super) fn interests_cache_only(&self, key: &ThumbnailKey) -> bool {
        self.key_to_interests.get(key).is_some_and(|interests| {
            !interests.is_empty()
                && interests.iter().all(|interest| {
                    self.entries
                        .get(interest)
                        .is_some_and(|entry| entry.capabilities.is_cache_only())
                })
        })
    }

    pub(super) fn mark_key_queued(
        &mut self,
        key: &ThumbnailKey,
        job_id: JobId,
        attempt: RetryAttempt,
    ) {
        if !self.finish_job(key, job_id) {
            return;
        }
        self.retry_attempts.insert(key.clone(), attempt);
        self.mark_interests_queued(key);
    }

    /// Retires the cache-only interests in `key` and re-queues everything else.
    ///
    /// A cache-only job hands back no pixels, so the two halves have to be
    /// handled differently. Cache-only interests are *satisfied* — the bytes they wanted are
    /// on disk — and must not be re-queued, or the pump would restart a job
    /// that banks-and-completes immediately, forever. An interactive interest
    /// that attached while the cache-only job was in flight is *not* satisfied, and
    /// would otherwise sit `InFlight` waiting for a delivery that is never
    /// coming; re-queuing it restarts it at its own priority, and that restart
    /// is local, since the source it needs was just banked.
    pub(super) fn retire_cache_only_interests(&mut self, key: &ThumbnailKey) {
        let Some(interests) = self.key_to_interests.get(key).cloned() else {
            return;
        };
        // The cache-only fetch succeeded, so any backoff it accumulated is spent.
        self.retry_attempts.remove(key);
        for interest in interests {
            let Some(entry) = self.entries.get_mut(&interest) else {
                continue;
            };
            if entry.capabilities.is_cache_only() {
                let _ = self.remove_interest(&interest);
                continue;
            }
            entry.state = DemandState::Queued;
            let (priority, sequence, capabilities) =
                (entry.priority, entry.sequence, entry.capabilities);
            self.enqueue(&interest, priority, sequence, capabilities);
        }
    }

    pub(super) fn mark_interests_queued(&mut self, key: &ThumbnailKey) {
        let Some(interests) = self.key_to_interests.get(key).cloned() else {
            return;
        };
        for interest in interests {
            let Some(entry) = self.entries.get_mut(&interest) else {
                continue;
            };
            entry.state = DemandState::Queued;
            let (priority, sequence, capabilities) =
                (entry.priority, entry.sequence, entry.capabilities);
            // Re-offer: the entry's previous heap candidate was consumed when
            // it left the queue, so becoming queued again needs a fresh one.
            self.enqueue(&interest, priority, sequence, capabilities);
        }
    }

    pub(super) fn begin_retry(
        &mut self,
        key: &ThumbnailKey,
        retry_id: RetryId,
        attempt: RetryAttempt,
    ) {
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

    pub(super) fn mark_retry_ready(&mut self, key: &ThumbnailKey, retry_id: RetryId) {
        if self.delayed_retries.get(key) != Some(&retry_id) {
            return;
        }
        self.delayed_retries.remove(key);
        self.mark_interests_queued(key);
    }

    pub(super) fn finish_job(&mut self, key: &ThumbnailKey, job_id: JobId) -> bool {
        if self.active_jobs.get(key).map(|job| job.job_id) != Some(job_id) {
            return false;
        }
        self.active_jobs.remove(key);
        true
    }

    pub(super) fn has_interests(&self, key: &ThumbnailKey) -> bool {
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
    /// accumulated for a whole session under an owner that applies exactly one
    /// demand set and so never prunes again, and any
    /// future reader of `by_owner` would have inherited a lie.
    pub(super) fn complete_key(&mut self, key: &ThumbnailKey) -> Vec<DemandEntry> {
        self.cancel_key(key);
        let interests = self.key_to_interests.remove(key).unwrap_or_default();
        interests
            .into_iter()
            .filter_map(|interest| self.remove_interest(&interest))
            .collect()
    }

    pub(super) fn in_flight_count(&self) -> usize {
        self.active_jobs.len()
    }

    pub(super) fn pending_len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(super) fn owner_bucket_len(&self, owner: &Owner) -> Option<usize> {
        self.by_owner.get(owner).map(HashSet::len)
    }

    #[cfg(test)]
    pub(super) fn priorities(&self) -> Vec<Priority> {
        self.entries.values().map(|entry| entry.priority).collect()
    }
}

struct ActiveJob {
    job_id: JobId,
    cancellation: ThumbnailCancellation,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct InterestKey {
    pub(super) owner: Owner,
    pub(super) generation: Generation,
    pub(super) id: DemandId,
    pub(super) key: ThumbnailKey,
    pub(super) capabilities: DemandCapabilities,
}

pub(super) struct DemandEntry {
    pub(super) owner: Owner,
    pub(super) generation: Generation,
    pub(super) id: DemandId,
    input: ThumbnailInput,
    key: ThumbnailKey,
    physical_max_edge: u32,
    priority: Priority,
    pub(super) capabilities: DemandCapabilities,
    state: DemandState,
    sequence: u64,
}

impl DemandEntry {
    pub(super) fn new(
        owner: Owner,
        generation: Generation,
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
            capabilities: demand.capabilities,
            state,
            sequence,
        }
    }

    fn interest_key(&self) -> InterestKey {
        InterestKey {
            owner: self.owner,
            generation: self.generation,
            id: self.id.clone(),
            key: self.key.clone(),
            capabilities: self.capabilities,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DemandState {
    Queued,
    InFlight,
    RetryWaiting,
}

pub(super) struct StartCandidate {
    /// The whole request, not the parts to rebuild one from: the key it
    /// caches under is derived here and cannot disagree with the input and
    /// edge the job actually fetches.
    pub(super) request: ThumbnailRequest,
    #[cfg(test)]
    pub(super) priority: Priority,
    pub(super) capabilities: DemandCapabilities,
    pub(super) job_id: JobId,
    pub(super) attempt: RetryAttempt,
    pub(super) cancellation: ThumbnailCancellation,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JobId(pub(super) u64);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RetryId(pub(super) u64);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetryAttempt(pub(super) u8);

impl RetryAttempt {
    pub(super) fn next(self) -> Option<Self> {
        (self.0 < 2).then(|| Self(self.0 + 1))
    }

    pub(super) fn delay(self) -> Duration {
        match self.0 {
            1 => Duration::from_secs(1),
            2 => Duration::from_secs(4),
            _ => Duration::ZERO,
        }
    }
}
