use std::{
    cell::RefCell,
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering as AtomicOrdering},
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

fn score(pattern: &Pattern, haystack: &str) -> Option<u32> {
    MATCHER.with(|state| {
        let (matcher, buf) = &mut *state.borrow_mut();
        pattern.score(Utf32Str::new(haystack, buf), matcher)
    })
}

fn best_search_item_score(pattern: &Pattern, search_item: &SearchItem) -> Option<u32> {
    let label_score = score(pattern, search_item.label());

    // File haystacks use the shared source; the entry path includes name and extension.
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
        .filter_map(|term| score(pattern, term))
        .max(),
        _ => search_item
            .terms()
            .iter()
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

#[derive(Eq, PartialEq)]
struct RankedHit(QuickSearchHit);

impl Ord for RankedHit {
    fn cmp(&self, other: &Self) -> Ordering {
        quick_hit_order(&self.0, &other.0)
    }
}

impl PartialOrd for RankedHit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct BestHits {
    hits: BinaryHeap<RankedHit>,
    limit: usize,
}

impl BestHits {
    fn new(limit: usize) -> Self {
        Self {
            hits: BinaryHeap::new(),
            limit,
        }
    }

    fn push(&mut self, hit: QuickSearchHit) {
        if self.hits.len() < self.limit {
            self.hits.push(RankedHit(hit));
        } else if let Some(mut worst) = self.hits.peek_mut()
            && quick_hit_order(&hit, &worst.0).is_lt()
        {
            *worst = RankedHit(hit);
        }
    }

    fn into_sorted(self) -> Vec<QuickSearchHit> {
        self.hits
            .into_sorted_vec()
            .into_iter()
            .map(|hit| hit.0)
            .collect()
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
    MyWorkshop {
        id: WorkshopId,
        tags: Vec<String>,
    },
    WorkshopItem(WorkshopId),
}

#[derive(Debug, Serialize)]
pub struct FileSearchAddon {
    pub path: PathBuf,
    pub title: String,
    pub workshop_id: Option<WorkshopId>,
    pub id_str: Option<Box<str>>,
}

impl FileSearchAddon {
    pub fn new(path: PathBuf, title: String, workshop_id: Option<WorkshopId>) -> Arc<Self> {
        Arc::new(Self {
            path,
            title,
            workshop_id,
            id_str: workshop_id.map(|id| id.to_string().into_boxed_str()),
        })
    }
}

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
        SearchItemSource::MyWorkshop { id, .. } => (2, None, None, id.get()),
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
                | SearchItemSource::MyWorkshop { .. }
                | SearchItemSource::WorkshopItem(_),
            SearchScope::Addons
        )
    )
}

impl PartialEq for SearchItem {
    fn eq(&self, other: &Self) -> bool {
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

    /// The caller must canonicalize path; index rebuilds must not make filesystem calls per addon.
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

    pub fn new_installed_addon_file<D: Into<u64>>(
        addon: Arc<FileSearchAddon>,
        entry_path: String,
        size_bytes: u64,
        crc32: u32,
        timestamp: D,
    ) -> Self {
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
            SearchItemSource::MyWorkshop {
                id: self.id,
                tags: self.tags.clone(),
            },
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

#[derive(Default)]
struct SearchIndex {
    items: Vec<Arc<SearchItem>>,
    installed_addons: BTreeMap<WorkshopId, Arc<SearchItem>>,
}

impl SearchIndex {
    fn upsert(&mut self, additions: &[Arc<SearchItem>]) {
        let mut identities = HashSet::new();
        let replacements = additions
            .iter()
            .rev()
            .filter(|item| identities.insert(source_order_key(&item.source)))
            .cloned()
            .collect::<Vec<_>>();
        self.items
            .retain(|item| !identities.contains(&source_order_key(&item.source)));
        self.items.extend(replacements);
        self.rebuild_installed_lookup();
    }

    fn rebuild_installed_lookup(&mut self) {
        self.installed_addons = self
            .items
            .iter()
            .filter_map(|item| match &item.source {
                SearchItemSource::InstalledAddons(_, Some(id)) => Some((*id, Arc::clone(item))),
                _ => None,
            })
            .collect();
    }
}

pub struct Search {
    cpu: crate::execution::CpuExecutor,
    index: RwLock<SearchIndex>,
}
impl Search {
    #[must_use]
    pub fn new(cpu: crate::execution::CpuExecutor) -> Self {
        Self {
            cpu,
            index: RwLock::new(SearchIndex::default()),
        }
    }

    #[cfg(test)]
    fn add<V: Searchable>(&self, item: &V) {
        if let Some(item) = item.search_item() {
            self.index.write().upsert(&[Arc::new(item)]);
        }
    }

    pub fn add_bulk<V: Searchable>(&self, items: &[V]) {
        let additions = items
            .iter()
            .filter_map(Searchable::search_item)
            .map(Arc::new)
            .collect::<Vec<_>>();
        self.index.write().upsert(&additions);
    }

    pub fn sync_installed_addons(&self, items: Vec<SearchItem>) {
        let mut index = self.index.write();
        index
            .items
            .retain(|item| !matches!(item.source, SearchItemSource::InstalledAddons(_, _)));
        index.upsert(&items.into_iter().map(Arc::new).collect::<Vec<_>>());
    }

    pub fn sync_installed_addon_files(&self, items: Vec<SearchItem>) {
        let mut index = self.index.write();
        index
            .items
            .retain(|item| !matches!(item.source, SearchItemSource::InstalledAddonFile { .. }));
        index.items.extend(items.into_iter().map(Arc::new));
    }

    pub fn quick_search_with_scope(&self, query: &str, scope: SearchScope) -> QuickSearchResult {
        self.cpu.install(|| self.quick_scored(query, scope))
    }

    fn quick_scored(&self, query: &str, scope: SearchScope) -> QuickSearchResult {
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

        let result = self
            .index
            .read()
            .items
            .par_iter()
            .filter_map(|search_item| {
                if !search_scope_matches(&search_item.source, scope) {
                    return None;
                }

                best_search_item_score(&pattern, search_item).map(|score| QuickSearchHit {
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

    pub fn full_with_transaction_scope(
        self: &Arc<Self>,
        query: String,
        scope: SearchScope,
        transaction: Transaction,
    ) -> TransactionId {
        let id = transaction.id();
        let search = Arc::clone(self);

        self.cpu.spawn(move || {
            search.full_scored(&query, scope, &transaction);
        });

        id
    }

    fn full_scored(&self, query: &str, scope: SearchScope, transaction: &Transaction) {
        let index = self.index.read();
        let items_n = index
            .items
            .iter()
            .filter(|item| search_scope_matches(&item.source, scope))
            .count()
            .max(1) as u64;
        let progress = AtomicU32::new(0);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let hits = index
            .items
            .par_iter()
            .try_fold(
                || BestHits::new(FULL_SEARCH_MAX_HITS),
                |mut hits, item| {
                    if transaction.aborted() {
                        return Err(());
                    }
                    if search_scope_matches(&item.source, scope) {
                        let idx = u64::from(progress.fetch_add(1, AtomicOrdering::Relaxed));
                        if idx * 100 / items_n != (idx + 1) * 100 / items_n {
                            transaction.progress((idx + 1) as f64 / items_n as f64);
                        }
                        if let Some(score) = best_search_item_score(&pattern, item) {
                            hits.push(QuickSearchHit {
                                score,
                                item: Arc::clone(item),
                            });
                        }
                    }
                    Ok(hits)
                },
            )
            .try_reduce(
                || BestHits::new(FULL_SEARCH_MAX_HITS),
                |mut left, right| {
                    for hit in right.hits {
                        left.push(hit.0);
                    }
                    Ok(left)
                },
            );
        drop(index);
        let Ok(hits) = hits else { return };
        let hits = hits.into_sorted();

        if !hits.is_empty() {
            transaction.data(TransactionPayload::SearchHits(hits));
        }
        transaction.finished(TransactionPayload::None);
    }

    pub(crate) fn invalidate_workshop_titles(&self) {
        self.index
            .write()
            .items
            .retain(|item| !matches!(item.source, SearchItemSource::MyWorkshop { .. }));
    }

    pub fn clear(&self) {
        *self.index.write() = SearchIndex::default();
    }

    pub fn refresh_installed_addon_labels(&self, items: &[WorkshopItem]) {
        let mut index = self.index.write();
        for item in items {
            let Some(existing) = index.installed_addons.get(&item.id).cloned() else {
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
            index
                .installed_addons
                .insert(item.id, Arc::clone(&replacement));
            if let Some(slot) = index
                .items
                .iter_mut()
                .find(|slot| Arc::ptr_eq(slot, &existing))
            {
                *slot = replacement;
            }
        }
    }
}

#[cfg(test)]
mod tests;
