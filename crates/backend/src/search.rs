use std::{
    cell::RefCell,
    cmp::Ordering,
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32},
    },
};

use crate::WorkshopId;
use parking_lot::RwLock;
use rayon::prelude::*;
use serde::{Serialize, ser::SerializeTuple};

use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

use crate::transactions::TransactionId;
use crate::{GmaFile, Transaction, WorkshopItem, transactions::TransactionPayload};

const MAX_QUICK_RESULTS: usize = 10;

/// Hard cap on full-search hits delivered to the UI. Broad file-scope queries
/// can match most of the index; only the best-scored slice is worth showing.
const FULL_SEARCH_MAX_HITS: usize = 10_000;

thread_local! {
    // `Matcher` requires `&mut` access, so each rayon worker keeps its own
    // matcher plus a scratch buffer for `Utf32Str` conversion.
    static MATCHER: RefCell<(Matcher, Vec<char>)> =
        RefCell::new((Matcher::new(Config::DEFAULT), Vec::new()));
}

/// Scores `haystack` against `pattern`, reusing this thread's matcher state.
fn score(pattern: &Pattern, haystack: &str) -> Option<u32> {
    MATCHER.with(|state| {
        let (matcher, buf) = &mut *state.borrow_mut();
        pattern.score(Utf32Str::new(haystack, buf), matcher)
    })
}

/// Best fuzzy-match score for `search_item` against `pattern`: its label if
/// long enough to match `query`, and its best-scoring qualifying term,
/// whichever is higher.
fn best_search_item_score(pattern: &Pattern, query: &str, search_item: &SearchItem) -> Option<u32> {
    let label_score = {
        let label = search_item.label();
        (label.len() >= query.len())
            .then(|| score(pattern, label))
            .flatten()
    };

    // File items don't carry a `terms` vec — their haystacks live on the
    // shared source (entry path subsumes file name and extension).
    let term_score = match &search_item.source {
        SearchItemSource::InstalledAddonFile {
            addon, entry_path, ..
        } => [
            Some(entry_path.as_str()),
            Some(addon.title.as_str()),
            addon.id_str.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter(|term| term.len() >= query.len())
        .filter_map(|term| score(pattern, term))
        .max(),
        _ => search_item
            .terms()
            .iter()
            .filter(|term| term.len() >= query.len())
            .filter_map(|term| score(pattern, term))
            .max(),
    };

    label_score.into_iter().chain(term_score).max()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuickSearchHit {
    pub score: u32,
    pub item: Arc<SearchItem>,
}

#[derive(Clone, Debug)]
pub struct QuickSearchResult {
    pub hits: Vec<QuickSearchHit>,
    pub has_more: bool,
}

fn quick_hit_order(left: &QuickSearchHit, right: &QuickSearchHit) -> Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.item.cmp(&right.item))
}

#[derive(Default)]
struct QuickSearchAccumulator {
    hits: Vec<QuickSearchHit>,
    matches: usize,
}

impl QuickSearchAccumulator {
    fn push(&mut self, hit: QuickSearchHit) {
        self.matches += 1;
        self.retain_if_best(hit);
    }

    fn merge(mut self, other: Self) -> Self {
        self.matches += other.matches;
        for hit in other.hits {
            self.retain_if_best(hit);
        }
        self
    }

    fn retain_if_best(&mut self, hit: QuickSearchHit) {
        let position = self
            .hits
            .binary_search_by(|existing| quick_hit_order(existing, &hit))
            .unwrap_or_else(|position| position);

        if position < MAX_QUICK_RESULTS {
            self.hits.insert(position, hit);
            self.hits.truncate(MAX_QUICK_RESULTS);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchScope {
    Addons,
    Files,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "source", content = "association")]
pub enum SearchItemSource {
    InstalledAddons(PathBuf, Option<WorkshopId>),
    InstalledAddonFile {
        #[serde(serialize_with = "serialize_shared_addon")]
        addon: Arc<FileSearchAddon>,
        entry_path: String,
        size_bytes: u64,
        crc32: u32,
    },
    MyWorkshop(WorkshopId),
    WorkshopItem(WorkshopId),
}

/// Shared per-addon identity for file search items: a library has ~10³
/// addons but ~10⁵ file entries, so per-file copies of the addon path,
/// title and id string dominated the file index's memory. One `Arc` per
/// addon, shared by all of its files; the file items themselves carry only
/// their entry path (label and extension are substrings of it).
#[derive(Debug, Serialize)]
pub struct FileSearchAddon {
    /// Canonicalized addon path (see [`SearchItem::new_installed_addon`]).
    pub path: PathBuf,
    pub title: String,
    pub workshop_id: Option<WorkshopId>,
    /// The workshop id formatted once, so id queries can match file items
    /// without a per-file copy.
    pub id_str: Option<Box<str>>,
}

impl FileSearchAddon {
    /// Takes the id already refined, so the searchable text and the typed id
    /// cannot describe different addons — a raw `Some(0)` would otherwise
    /// leave `workshop_id: None` beside an `id_str` of `"0"` that file search
    /// happily matches on.
    pub fn new(path: PathBuf, title: String, workshop_id: Option<WorkshopId>) -> Arc<Self> {
        Arc::new(Self {
            path,
            title,
            workshop_id,
            id_str: workshop_id.map(|id| id.to_string().into_boxed_str()),
        })
    }
}

// serde's blanket `Arc<T>` impl is feature-gated (`rc`); serialize the
// shared struct directly — this only feeds transaction detail logging.
fn serialize_shared_addon<S>(addon: &Arc<FileSearchAddon>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    FileSearchAddon::serialize(addon, serializer)
}

#[derive(Debug)]
pub struct SearchItem {
    pub label: String,
    pub terms: Vec<String>,
    pub timestamp: u64,
    pub len: usize,
    pub source: SearchItemSource,
}
impl SearchItem {
    pub fn label(&self) -> &str {
        // File items derive their label (the file name) from the entry
        // path instead of storing a per-file copy.
        if let SearchItemSource::InstalledAddonFile { entry_path, .. } = &self.source {
            entry_path
                .rsplit_once('/')
                .map_or(entry_path.as_str(), |(_, name)| name)
        } else {
            &self.label
        }
    }

    pub fn terms(&self) -> &[String] {
        &self.terms
    }
}
impl PartialOrd for SearchItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for SearchItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Tie-break by source identity so distinct addons never compare
        // Equal (upstream compared the two Orderings to each other, which is
        // not transitive and let binary-search replacement hit the wrong
        // item when timestamp and length collided).
        self.timestamp
            .cmp(&other.timestamp)
            .reverse()
            .then_with(|| self.len.cmp(&other.len).reverse())
            .then_with(|| source_order_key(&self.source).cmp(&source_order_key(&other.source)))
    }
}

fn source_order_key(
    source: &SearchItemSource,
) -> (u8, Option<&std::path::Path>, Option<&str>, u64) {
    match source {
        SearchItemSource::InstalledAddons(path, _) => (0, Some(path.as_path()), None, 0),
        SearchItemSource::InstalledAddonFile {
            addon, entry_path, ..
        } => (1, Some(addon.path.as_path()), Some(entry_path.as_str()), 0),
        SearchItemSource::MyWorkshop(id) => (2, None, None, id.get()),
        SearchItemSource::WorkshopItem(id) => (3, None, None, id.get()),
    }
}

fn search_scope_matches(source: &SearchItemSource, scope: SearchScope) -> bool {
    matches!(
        (source, scope),
        (
            SearchItemSource::InstalledAddonFile { .. },
            SearchScope::Files
        ) | (
            SearchItemSource::InstalledAddons(_, _)
                | SearchItemSource::MyWorkshop(_)
                | SearchItemSource::WorkshopItem(_),
            SearchScope::Addons
        )
    )
}

impl PartialEq for SearchItem {
    fn eq(&self, other: &Self) -> bool {
        // Delegate to the full sort key so Eq and Ord agree: a partial
        // comparison here (e.g. source identity alone) would violate the
        // Ord contract and break dedup()'s adjacency assumption after sorting.
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}
impl Eq for SearchItem {}
impl SearchItem {
    pub fn new<D: Into<u64>>(
        source: SearchItemSource,
        label: String,
        mut terms: Vec<String>,
        timestamp: D,
    ) -> Self {
        terms.shrink_to_fit();
        terms.sort_by_key(std::string::String::len);

        Self {
            len: terms
                .iter()
                .map(std::string::String::len)
                .reduce(std::cmp::Ord::max)
                .unwrap_or(0)
                .max(label.len()),
            label,
            terms,
            timestamp: timestamp.into(),
            source,
        }
    }

    /// `path` must already be canonicalized by the caller — resolving it here
    /// would mean a filesystem syscall per addon on every index rebuild,
    /// which is exactly the cost this split was made to avoid.
    pub fn new_installed_addon<D: Into<u64>>(
        path: PathBuf,
        workshop_id: Option<u64>,
        label: String,
        terms: Vec<String>,
        timestamp: D,
    ) -> Self {
        Self::new(
            SearchItemSource::InstalledAddons(path, workshop_id.and_then(WorkshopId::new)),
            label,
            terms,
            timestamp,
        )
    }

    /// One file entry of an installed addon. `addon` is the shared
    /// per-addon identity ([`FileSearchAddon::new`], one per addon, its
    /// `path` already canonicalized); the item itself owns only its entry
    /// path. Label, extension and search terms all derive from the source,
    /// so `label`/`terms` stay empty.
    pub fn new_installed_addon_file<D: Into<u64>>(
        addon: Arc<FileSearchAddon>,
        entry_path: String,
        size_bytes: u64,
        crc32: u32,
        timestamp: D,
    ) -> Self {
        // Mirrors `new`'s len: the longest haystack this item can match
        // (see `best_search_item_score`'s file-item arm).
        let len = entry_path
            .len()
            .max(addon.title.len())
            .max(addon.id_str.as_deref().map_or(0, str::len));
        Self {
            len,
            label: String::new(),
            terms: Vec::new(),
            timestamp: timestamp.into(),
            source: SearchItemSource::InstalledAddonFile {
                addon,
                entry_path,
                size_bytes,
                crc32,
            },
        }
    }
}
impl Serialize for SearchItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut tup = serializer.serialize_tuple(2)?;
        tup.serialize_element(self.label())?;
        tup.serialize_element(&self.source)?;
        tup.end()
    }
}

pub trait Searchable {
    fn search_item(&self) -> Option<SearchItem>;
}
impl Searchable for WorkshopItem {
    fn search_item(&self) -> Option<SearchItem> {
        let mut terms = self.tags.clone();

        terms.push(self.id.get().to_string());

        if let Some(steamid) = &self.steamid {
            terms.push(steamid.raw().to_string());
            terms.push(steamid.steamid32());
        }

        Some(SearchItem::new(
            SearchItemSource::MyWorkshop(self.id),
            self.title.clone(),
            terms,
            self.time_updated,
        ))
    }
}
impl Searchable for GmaFile {
    fn search_item(&self) -> Option<SearchItem> {
        let mut terms = self
            .metadata()
            .tags()
            .map(<[String]>::to_vec)
            .unwrap_or_default();
        if let Some(addon_type) = self.metadata().addon_type() {
            terms.push(addon_type.to_string());
        }
        let label = self.metadata().title().to_owned();

        if let Some(id) = self.workshop_id() {
            terms.push(id.get().to_string());
        }

        Some(SearchItem::new(
            SearchItemSource::InstalledAddons(
                dunce::canonicalize(self.path()).unwrap_or_else(|_| self.path().to_owned()),
                self.workshop_id(),
            ),
            label,
            terms,
            self.modified().unwrap_or(0),
        ))
    }
}
impl Searchable for std::sync::Arc<crate::Addon> {
    fn search_item(&self) -> Option<SearchItem> {
        match &**self {
            crate::Addon::Installed(installed) => installed.search_item(),
            crate::Addon::Workshop(workshop) => workshop.search_item(),
        }
    }
}

pub struct Search {
    cpu: crate::execution::CpuExecutor,
    dirty: AtomicBool,
    items: RwLock<Vec<Arc<SearchItem>>>,

    installed_addons: RwLock<BTreeMap<WorkshopId, Arc<SearchItem>>>,
}
impl Search {
    #[must_use]
    pub fn new(cpu: crate::execution::CpuExecutor) -> Self {
        Self {
            cpu,
            items: RwLock::new(Vec::new()),
            dirty: AtomicBool::new(false),

            installed_addons: RwLock::new(BTreeMap::new()),
        }
    }

    pub fn ensure_sorted(&self) {
        self.cpu.install(|| self.ensure_sorted_on_cpu());
    }

    fn ensure_sorted_on_cpu(&self) {
        // Cheap check before contending for the write lock; the swap below
        // is the one that actually matters.
        if !self.dirty.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }

        let mut items = self.items.write();
        // Clear the flag only once we hold the write lock: `add()`'s
        // binary-search path only runs when it sees a clean flag, and it
        // needs the same lock to do so, so it can never observe "clean"
        // while this sort is still in flight. Clearing the flag first (the
        // old order) left a window where a concurrent `add()` could read
        // "clean", grab the lock before this sort ran, and binary-search
        // into a still-unsorted vec. A bulk add racing us re-flags after it
        // releases its own write lock, so we never clear a flag out from
        // under a pending write.
        if !self.dirty.swap(false, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        items.par_sort();
        items.dedup();
    }

    pub fn add<V: Searchable>(&self, item: &V) {
        if let Some(search_item) = item.search_item() {
            let search_item = Arc::new(search_item);

            if let SearchItemSource::InstalledAddons(_, Some(id)) = &search_item.source {
                self.installed_addons
                    .write()
                    .insert(*id, search_item.clone());
            }

            if self.dirty.load(std::sync::atomic::Ordering::Acquire) {
                self.items.write().push(search_item);
            } else {
                let mut items = self.items.write();
                // Replace by identity (source), not by Ord position or full
                // equality: a re-added addon usually carries a new
                // timestamp, so its old entry sits elsewhere in the sort
                // order and no longer compares equal to the new one.
                if let Some(existing) = items.iter().position(|cmp| {
                    source_order_key(&cmp.source) == source_order_key(&search_item.source)
                }) {
                    items.remove(existing);
                }
                let pos = items.binary_search(&search_item).unwrap_or_else(|pos| pos);
                items.insert(pos, search_item);
            }
        }
    }

    pub fn reserve(&self, amount: usize) {
        self.items.write().reserve(amount);
    }

    pub fn add_bulk<V: Searchable>(&self, items: &[V]) {
        let mut new_installed_addons = Vec::new();

        let mut store = self.items.write();
        store.reserve(items.len());
        store.extend(items.iter().filter_map(|v| {
            v.search_item().map(|search_item| {
                let search_item = Arc::new(search_item);
                if let SearchItemSource::InstalledAddons(_, Some(id)) = &search_item.source {
                    new_installed_addons.push((*id, search_item.clone()));
                }
                search_item
            })
        }));
        drop(store);

        if !new_installed_addons.is_empty() {
            self.installed_addons.write().extend(new_installed_addons);
        }

        // Flag AFTER the items are appended: an ensure_sorted() racing the extend may
        // sort without them, but this store re-flags so the next query sorts.
        self.dirty.store(true, std::sync::atomic::Ordering::Release);
    }

    pub fn sync_installed_addons(&self, items: Vec<SearchItem>) {
        let installed_items = items.into_iter().map(Arc::new).collect::<Vec<_>>();

        {
            let mut store = self.items.write();
            store.retain(|item| !matches!(item.source, SearchItemSource::InstalledAddons(_, _)));
            store.extend(installed_items.iter().cloned());
        }

        {
            let mut installed_addons = self.installed_addons.write();
            *installed_addons = installed_items
                .iter()
                .filter_map(|item| match &item.source {
                    SearchItemSource::InstalledAddons(_, Some(id)) => Some((*id, item.clone())),
                    _ => None,
                })
                .collect();
        }

        self.dirty.store(true, std::sync::atomic::Ordering::Release);
    }

    pub fn sync_installed_addon_files(&self, items: Vec<SearchItem>) {
        let mut store = self.items.write();
        store.retain(|item| !matches!(item.source, SearchItemSource::InstalledAddonFile { .. }));
        store.reserve(items.len());
        store.extend(items.into_iter().map(Arc::new));
        drop(store);

        self.dirty.store(true, std::sync::atomic::Ordering::Release);
    }

    pub fn quick_search(&self, query: String) -> QuickSearchResult {
        self.quick_search_with_scope(query, SearchScope::Addons)
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "app-layer callers across the crate boundary already own this string"
    )]
    pub fn quick_search_with_scope(&self, query: String, scope: SearchScope) -> QuickSearchResult {
        self.cpu.install(|| {
            self.ensure_sorted_on_cpu();
            self.quick_scored(&query, scope)
        })
    }

    fn quick_scored(&self, query: &str, scope: SearchScope) -> QuickSearchResult {
        // Queries use fzf-style atom syntax: whitespace-separated AND terms,
        // with `!` negation, `^`/`$` anchors and `'` exact matching.
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

        let result = self
            .items
            .read()
            .par_iter()
            .filter_map(|search_item| {
                if !search_scope_matches(&search_item.source, scope) {
                    return None;
                }

                if search_item.len < query.len() {
                    return None;
                }

                best_search_item_score(&pattern, query, search_item).map(|score| QuickSearchHit {
                    score,
                    item: search_item.clone(),
                })
            })
            .fold(QuickSearchAccumulator::default, |mut result, hit| {
                result.push(hit);
                result
            })
            .reduce(
                QuickSearchAccumulator::default,
                QuickSearchAccumulator::merge,
            );

        QuickSearchResult {
            hits: result.hits,
            has_more: result.matches > MAX_QUICK_RESULTS,
        }
    }

    pub fn full_with_transaction(
        self: &Arc<Self>,
        query: String,
        transaction: Transaction,
    ) -> TransactionId {
        self.full_with_transaction_scope(query, SearchScope::Addons, transaction)
    }

    pub fn full_with_transaction_scope(
        self: &Arc<Self>,
        query: String,
        scope: SearchScope,
        transaction: Transaction,
    ) -> TransactionId {
        let id = transaction.id();
        let search = Arc::clone(self);

        self.cpu.spawn(move || {
            search.ensure_sorted_on_cpu();
            search.full_scored(&query, scope, &transaction);
        });

        id
    }

    fn full_scored(&self, query: &str, scope: SearchScope, transaction: &Transaction) {
        let items = self.items.read();
        let items_n = items
            .iter()
            .filter(|item| search_scope_matches(&item.source, scope))
            .count()
            .max(1) as u64;
        let i = Arc::new(AtomicU32::new(0));

        // Queries use fzf-style atom syntax: whitespace-separated AND terms,
        // with `!` negation, `^`/`$` anchors and `'` exact matching.
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

        // Hits are accumulated and delivered as a single batch, and progress
        // is only emitted on whole-percent boundaries: per-item events flood
        // the UI event queue on file-scope searches, where the index holds
        // hundreds of thousands of entries.
        let hits: Result<Vec<Option<QuickSearchHit>>, ()> = items
            .par_iter()
            .map_with(i, |i, search_item| {
                if !search_scope_matches(&search_item.source, scope) {
                    return Ok(None);
                }

                if transaction.aborted() {
                    return Err(());
                }

                let idx = u64::from(i.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
                if idx * 100 / items_n != (idx + 1) * 100 / items_n {
                    transaction.progress((idx + 1) as f64 / items_n as f64);
                }

                if search_item.len < query.len() {
                    return Ok(None);
                }

                Ok(
                    best_search_item_score(&pattern, query, search_item).map(|score| {
                        QuickSearchHit {
                            score,
                            item: search_item.clone(),
                        }
                    }),
                )
            })
            .collect();
        drop(items);

        // Err means the transaction was aborted mid-search; finishing an
        // aborted transaction violates the transaction contract.
        let Ok(hits) = hits else { return };

        let mut hits: Vec<QuickSearchHit> = hits.into_iter().flatten().collect();
        hits.sort_unstable_by_key(|hit| std::cmp::Reverse(hit.score));
        hits.truncate(FULL_SEARCH_MAX_HITS);

        if !hits.is_empty() {
            transaction.data(TransactionPayload::SearchHits(hits));
        }
        transaction.finished(TransactionPayload::None);
    }

    pub fn clear(&self) {
        *self.items.write() = Vec::new();
        *self.installed_addons.write() = BTreeMap::new();
        self.dirty
            .store(false, std::sync::atomic::Ordering::Release);
    }

    /// Refreshes installed-addon labels from freshly fetched workshop data,
    /// moving any stale label into `terms` so it stays searchable.
    ///
    /// `SearchItem` is immutable (`len` is a cached sort key derived from
    /// `label`/`terms` at construction), so a change in label builds a whole
    /// replacement item rather than editing the existing one in place, and
    /// swaps it into both `items` and `installed_addons`.
    pub fn refresh_installed_addon_labels(&self, items: &[WorkshopItem]) {
        self.ensure_sorted();
        for item in items {
            let Some(existing) = self.installed_addons.read().get(&item.id).cloned() else {
                continue;
            };
            if existing.label == item.title {
                continue;
            }

            let mut terms = existing.terms.clone();
            terms.push(existing.label.clone());
            let replacement = Arc::new(SearchItem::new(
                existing.source.clone(),
                item.title.clone(),
                terms,
                existing.timestamp,
            ));

            self.installed_addons
                .write()
                .insert(item.id, replacement.clone());

            let mut store = self.items.write();
            if let Some(index) = store.iter().position(|slot| Arc::ptr_eq(slot, &existing)) {
                store[index] = replacement;
            }
        }
    }
}

#[cfg(test)]
mod tests;
