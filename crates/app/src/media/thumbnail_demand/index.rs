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
    by_owner: HashMap<Owner, HashSet<InterestKey>>,
    ready: BinaryHeap<Candidate>,
    ready_cache_only: BinaryHeap<Candidate>,
}

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

    pub(super) fn retain_owner(&mut self, owner: &Owner, keep: &HashSet<InterestKey>) {
        let Some(interests) = self.by_owner.get(owner) else {
            return;
        };
        let stale = interests.difference(keep).cloned().collect::<Vec<_>>();
        for interest in &stale {
            let _ = self.remove_interest(interest);
        }
    }

    /// Checks an existing interest and applies priority promotions.
    /// Priority is excluded from `InterestKey`, so a newly visible row keeps its
    /// identity while moving out of the prefetch tier.
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

    /// Releases media slots for unwanted work. A fetch already in progress may
    /// still finish and populate the memory cache before delivery is checked.
    pub(super) fn cancel_uninterested_work(&mut self) {
        // Clear retry counts too: backpressured keys may exist in neither work map
        // and must not pass elevated attempts to a future demand.
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

    /// True only if at least one interest exists and all interests are cache-only.
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

    /// Retires cache-only interests after source banking. Requeues interactive
    /// interests that attached mid-flight because they still need decoded pixels.
    pub(super) fn retire_cache_only_interests(&mut self, key: &ThumbnailKey) {
        let Some(interests) = self.key_to_interests.get(key).cloned() else {
            return;
        };
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
            job.cancellation.cancel();
        }
        self.delayed_retries.remove(key);
        self.retry_attempts.remove(key);
    }

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
