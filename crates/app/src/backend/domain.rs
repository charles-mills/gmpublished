use std::{collections::HashSet, fmt, num::NonZeroU64, path::PathBuf, sync::Arc};

use super::{gma::GmaMeta, tasks::TaskId};

pub const RESULTS_PER_PAGE: usize = 50;
pub const WORKSHOP_LEGAL_URL: &str = "https://steamcommunity.com/workshop/workshoplegalagreement";

/// Zero is never a valid Steam Workshop id (the backend already treats it
/// as "no id"); the inner `NonZeroU64` makes that invariant unrepresentable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PublishedFileId(NonZeroU64);

impl PublishedFileId {
    pub(crate) fn new(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for PublishedFileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AvatarRgba {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Arc<[u8]>,
}

impl AvatarRgba {
    pub(crate) fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        let expected_len = usize::try_from(width)
            .ok()?
            .checked_mul(usize::try_from(height).ok()?)?
            .checked_mul(4)?;
        (rgba.len() == expected_len).then(|| Self {
            width,
            height,
            rgba: Arc::from(rgba.into_boxed_slice()),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamUser {
    pub(crate) steamid: u64,
    pub(crate) name: String,
    pub(crate) avatar: Option<AvatarRgba>,
    pub(crate) dead: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkshopItem {
    pub(crate) id: PublishedFileId,
    pub(crate) title: String,
    pub(crate) owner: Option<SteamUser>,
    pub(crate) steamid: Option<u64>,
    pub(crate) time_created: u32,
    pub(crate) time_updated: u32,
    pub(crate) description: Option<String>,
    pub(crate) score: f32,
    pub(crate) tags: Vec<String>,
    pub(crate) preview_url: Option<String>,
    pub(crate) subscriptions: u64,
    pub(crate) local_file: Option<PathBuf>,
    pub(crate) dead: bool,
}

impl WorkshopItem {
    pub(crate) fn dead(id: PublishedFileId) -> Self {
        Self::from(id)
    }
}

impl From<PublishedFileId> for WorkshopItem {
    fn from(id: PublishedFileId) -> Self {
        Self {
            id,
            title: id.get().to_string(),
            owner: None,
            steamid: None,
            time_created: 0,
            time_updated: 0,
            description: None,
            score: 0.0,
            tags: Vec::new(),
            preview_url: None,
            subscriptions: 0,
            local_file: None,
            dead: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkshopPage {
    pub(crate) total: u32,
    pub(crate) items: Vec<WorkshopItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkshopMetadata {
    pub(crate) id: PublishedFileId,
    pub(crate) title: String,
    pub(crate) time_created: u32,
    pub(crate) time_updated: u32,
    pub(crate) score: f32,
    pub(crate) tags: Vec<String>,
    pub(crate) preview_url: Option<String>,
    pub(crate) subscriptions: u64,
    /// ThumbHash of the preview image, computed locally the first time it is
    /// decoded (Steam never supplies it). Persisted so placeholders paint on
    /// the next launch.
    pub(crate) thumbhash: Option<Arc<[u8]>>,
}

impl WorkshopMetadata {
    pub(crate) fn from_workshop_item(item: &WorkshopItem) -> Option<Self> {
        if item.dead {
            return None;
        }

        Some(Self {
            id: item.id,
            title: item.title.clone(),
            time_created: item.time_created,
            time_updated: item.time_updated,
            score: item.score,
            tags: item.tags.clone(),
            preview_url: item
                .preview_url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(str::to_owned),
            subscriptions: item.subscriptions,
            thumbhash: None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledAddon {
    pub(crate) path: PathBuf,
    /// `path` resolved once at discovery time (background thread). Search
    /// indexing needs the canonical form to cross-reference reliably, but
    /// re-resolving it per addon on every index rebuild put a filesystem
    /// syscall storm on the UI thread on every hide/library refresh; reusing
    /// this cached copy keeps that resolution off the hot path entirely.
    pub(crate) canonical_path: PathBuf,
    pub(crate) workshop_id: Option<PublishedFileId>,
    pub(crate) file_size_bytes: u64,
    pub(crate) modified_epoch_seconds: u64,
    pub(crate) meta: GmaMeta,
}

impl InstalledAddon {
    /// User-visible title with the same fallback everywhere the addon is
    /// shown or indexed (list rows, search corpus): GMA title → file stem
    /// → full path. Keeping search on this exact string means an addon is
    /// always findable by the name the user sees.
    pub(crate) fn display_title(&self) -> String {
        let title = self.meta.title().trim();
        if !title.is_empty() {
            return title.to_owned();
        }

        if let Some(name) = self.path.file_stem().and_then(|name| name.to_str()) {
            let name = name.trim();
            if !name.is_empty() {
                return name.to_owned();
            }
        }

        self.path.to_string_lossy().into_owned()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopDownloadSuccess {
    pub(crate) item_id: PublishedFileId,
    pub(crate) installed_path: Option<PathBuf>,
    pub(crate) extracted_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopDownloadResult {
    pub(crate) item_id: PublishedFileId,
    pub(crate) outcome: Result<WorkshopDownloadSuccess, super::ui_error::UiError>,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SearchGeneration(u64);

impl SearchGeneration {
    fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SearchMode {
    #[default]
    Addons,
    #[cfg_attr(not(feature = "asset-studio"), allow(dead_code))]
    Files,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRequestKey {
    generation: SearchGeneration,
    mode: SearchMode,
    query: String,
}

impl SearchRequestKey {
    pub(crate) fn new_with_mode(
        generation: SearchGeneration,
        mode: SearchMode,
        query: impl Into<String>,
    ) -> Self {
        Self {
            generation,
            mode,
            query: query.into(),
        }
    }

    pub(crate) const fn generation(&self) -> SearchGeneration {
        self.generation
    }

    pub(crate) const fn mode(&self) -> SearchMode {
        self.mode
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchQuickCarry {
    index_epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQuickRequest {
    key: SearchRequestKey,
    carry: SearchQuickCarry,
}

impl SearchQuickRequest {
    pub(crate) fn new(key: SearchRequestKey, carry: SearchQuickCarry) -> Self {
        Self { key, carry }
    }

    pub(crate) const fn key(&self) -> &SearchRequestKey {
        &self.key
    }

    pub(crate) fn query(&self) -> &str {
        self.key.query()
    }

    pub(crate) const fn generation(&self) -> SearchGeneration {
        self.key.generation()
    }

    pub(crate) const fn mode(&self) -> SearchMode {
        self.key.mode()
    }

    pub(crate) const fn carry(&self) -> &SearchQuickCarry {
        &self.carry
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQuickBatch {
    key: SearchRequestKey,
    hits: Vec<SearchHit>,
    has_more: bool,
    carry: SearchQuickCarry,
}

impl SearchQuickBatch {
    pub(crate) fn new(
        key: SearchRequestKey,
        hits: Vec<SearchHit>,
        has_more: bool,
        carry: SearchQuickCarry,
    ) -> Self {
        Self {
            key,
            hits,
            has_more,
            carry,
        }
    }

    pub(crate) const fn key(&self) -> &SearchRequestKey {
        &self.key
    }

    #[cfg(test)]
    pub(crate) fn query(&self) -> &str {
        self.key.query()
    }

    #[cfg(test)]
    pub(crate) const fn generation(&self) -> SearchGeneration {
        self.key.generation()
    }

    #[cfg(test)]
    pub(crate) const fn has_more(&self) -> bool {
        self.has_more
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchAcceptedQuickBatch {
    hits: Vec<SearchHit>,
    has_more: bool,
}

impl SearchAcceptedQuickBatch {
    pub(crate) fn into_parts(self) -> (Vec<SearchHit>, bool) {
        (self.hits, self.has_more)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchFullRequest {
    key: SearchRequestKey,
    task_id: TaskId,
}

impl SearchFullRequest {
    pub(crate) fn new(key: SearchRequestKey, task_id: TaskId) -> Self {
        Self { key, task_id }
    }

    pub(crate) const fn key(&self) -> &SearchRequestKey {
        &self.key
    }

    pub(crate) fn query(&self) -> &str {
        self.key.query()
    }

    pub(crate) const fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub(crate) const fn mode(&self) -> SearchMode {
        self.key.mode()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SearchFullHits {
    Owned(Vec<SearchHit>),
}

impl SearchFullHits {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Owned(hits) => hits.len(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn map_rows<R>(&self, mut map: impl FnMut(u32, &SearchItem) -> R) -> Vec<R> {
        match self {
            Self::Owned(hits) => hits.iter().map(|hit| map(hit.score, &hit.item)).collect(),
        }
    }

    #[cfg(test)]
    pub(crate) fn to_hits(&self) -> Vec<SearchHit> {
        match self {
            Self::Owned(hits) => hits.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchFullBatch {
    key: SearchRequestKey,
    task_id: TaskId,
    sequence: u64,
    hits: SearchFullHits,
}

impl SearchFullBatch {
    pub(crate) fn new(
        key: SearchRequestKey,
        task_id: TaskId,
        sequence: u64,
        hits: Vec<SearchHit>,
    ) -> Self {
        Self {
            key,
            task_id,
            sequence,
            hits: SearchFullHits::Owned(hits),
        }
    }

    pub(crate) const fn key(&self) -> &SearchRequestKey {
        &self.key
    }

    pub(crate) const fn task_id(&self) -> TaskId {
        self.task_id
    }

    #[cfg(test)]
    pub(crate) const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[cfg(test)]
    pub(crate) fn to_hits(&self) -> Vec<SearchHit> {
        self.hits.to_hits()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchFullBatchMode {
    ReplaceQuickRows,
    AppendRows,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchAcceptedFullBatch {
    mode: SearchFullBatchMode,
    hits: SearchFullHits,
}

impl SearchAcceptedFullBatch {
    pub(crate) fn into_parts(self) -> (SearchFullBatchMode, SearchFullHits) {
        (self.mode, self.hits)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchQueryChange {
    pub(crate) quick_request: Option<SearchQuickRequest>,
    pub(crate) cancel_task: Option<TaskId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchFullStart {
    pub(crate) request: SearchFullRequest,
    pub(crate) cancel_task: Option<TaskId>,
}

#[derive(Clone, Debug, Default)]
pub struct SearchSession {
    generation: SearchGeneration,
    latest_mode: SearchMode,
    latest_query: String,
    loading: bool,
    has_more: bool,
    active_full_task: Option<TaskId>,
    full_replace_pending: bool,
    quick_carry: SearchQuickCarry,
}

impl SearchSession {
    pub(crate) const fn generation(&self) -> SearchGeneration {
        self.generation
    }

    pub(crate) fn query(&self) -> &str {
        &self.latest_query
    }

    pub(crate) const fn loading(&self) -> bool {
        self.loading
    }

    pub(crate) const fn has_more(&self) -> bool {
        self.has_more
    }

    pub(crate) const fn active_full_task(&self) -> Option<TaskId> {
        self.active_full_task
    }

    pub(crate) const fn full_replace_pending(&self) -> bool {
        self.full_replace_pending
    }

    pub(crate) fn begin_query(&mut self, input: &str, mode: SearchMode) -> SearchQueryChange {
        self.bump_generation();
        self.latest_mode = mode;
        let cancel_task = self.active_full_task.take();
        self.full_replace_pending = false;

        let Some(query) = normalized_search_query(input) else {
            self.clear_current_generation();
            return SearchQueryChange {
                quick_request: None,
                cancel_task,
            };
        };

        self.latest_query = query;
        self.loading = true;
        self.has_more = false;

        SearchQueryChange {
            quick_request: Some(SearchQuickRequest::new(
                self.current_key(),
                self.quick_carry.clone(),
            )),
            cancel_task,
        }
    }

    pub(crate) fn clear(&mut self) -> SearchQueryChange {
        self.bump_generation();
        let cancel_task = self.active_full_task.take();
        self.clear_current_generation();
        SearchQueryChange {
            quick_request: None,
            cancel_task,
        }
    }

    pub(crate) fn accept_quick_batch(
        &mut self,
        batch: SearchQuickBatch,
    ) -> Option<SearchAcceptedQuickBatch> {
        if !self.is_current_key(batch.key()) {
            return None;
        }

        self.loading = false;
        self.active_full_task = None;
        self.full_replace_pending = false;
        self.has_more = batch.has_more;
        self.quick_carry = batch.carry;

        Some(SearchAcceptedQuickBatch {
            hits: batch.hits,
            has_more: batch.has_more,
        })
    }

    pub(crate) fn fail_quick(&mut self, key: &SearchRequestKey) -> bool {
        if !self.is_current_key(key) {
            return false;
        }

        self.loading = false;
        self.active_full_task = None;
        self.full_replace_pending = false;
        self.has_more = false;
        true
    }

    pub(crate) fn can_begin_full_search(&self) -> bool {
        !self.latest_query.is_empty() && (self.has_more || self.active_full_task.is_some())
    }

    pub(crate) fn begin_full_search(
        &mut self,
        task_id: TaskId,
        mode: SearchMode,
    ) -> Option<SearchFullStart> {
        if !self.can_begin_full_search() {
            return None;
        }

        self.bump_generation();
        self.latest_mode = mode;
        let cancel_task = self.active_full_task.replace(task_id);
        self.loading = true;
        self.has_more = false;
        self.full_replace_pending = true;

        Some(SearchFullStart {
            request: SearchFullRequest::new(self.current_key(), task_id),
            cancel_task,
        })
    }

    pub(crate) fn accept_full_batch(
        &mut self,
        batch: SearchFullBatch,
    ) -> Option<SearchAcceptedFullBatch> {
        if batch.hits.is_empty() || !self.is_active_full_batch(&batch) {
            return None;
        }

        let mode = if self.full_replace_pending {
            self.full_replace_pending = false;
            SearchFullBatchMode::ReplaceQuickRows
        } else {
            SearchFullBatchMode::AppendRows
        };

        Some(SearchAcceptedFullBatch {
            mode,
            hits: batch.hits,
        })
    }

    pub(crate) fn finish_full_search(&mut self, request: &SearchFullRequest) -> bool {
        if !self.is_active_full_request(request) {
            return false;
        }

        self.loading = false;
        self.has_more = false;
        self.active_full_task = None;
        self.full_replace_pending = false;
        true
    }

    pub(crate) fn is_current(
        &self,
        generation: SearchGeneration,
        mode: SearchMode,
        query: &str,
    ) -> bool {
        self.generation == generation
            && self.latest_mode == mode
            && self.latest_query == query
            && !self.latest_query.is_empty()
    }

    pub(crate) fn current_key(&self) -> SearchRequestKey {
        SearchRequestKey::new_with_mode(
            self.generation,
            self.latest_mode,
            self.latest_query.clone(),
        )
    }

    fn bump_generation(&mut self) {
        self.generation = self.generation.next();
    }

    fn clear_current_generation(&mut self) {
        self.latest_query.clear();
        self.loading = false;
        self.has_more = false;
        self.active_full_task = None;
        self.full_replace_pending = false;
        self.quick_carry = SearchQuickCarry::default();
    }

    fn is_current_key(&self, key: &SearchRequestKey) -> bool {
        self.generation == key.generation()
            && self.latest_mode == key.mode()
            && self.latest_query == key.query()
            && !self.latest_query.is_empty()
    }

    fn is_active_full_batch(&self, batch: &SearchFullBatch) -> bool {
        self.is_current_key(batch.key()) && self.active_full_task == Some(batch.task_id())
    }

    fn is_active_full_request(&self, request: &SearchFullRequest) -> bool {
        self.is_current_key(request.key()) && self.active_full_task == Some(request.task_id())
    }
}

fn normalized_search_query(input: &str) -> Option<String> {
    let query = input.trim();
    if query.is_empty() {
        return None;
    }

    Some(
        workshop_url::parse_workshop_id(query)
            .map_or_else(|| query.to_owned(), |id| id.get().to_string()),
    )
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum SearchItemSource {
    InstalledAddons(PathBuf, Option<PublishedFileId>),
    InstalledAddonFile {
        addon_path: PathBuf,
        addon_title: String,
        workshop_id: Option<PublishedFileId>,
        entry_path: String,
        size_bytes: u64,
        crc32: u32,
    },
    MyWorkshop(PublishedFileId),
    WorkshopItem(PublishedFileId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchItem {
    pub(crate) label: String,
    pub(crate) terms: Vec<String>,
    pub(crate) timestamp: u64,
    pub(crate) len: usize,
    pub(crate) source: SearchItemSource,
}

impl SearchItem {
    #[cfg(test)]
    pub(crate) fn new<L, Terms, Term>(
        source: SearchItemSource,
        label: L,
        terms: Terms,
        timestamp: u64,
    ) -> Self
    where
        L: Into<String>,
        Terms: IntoIterator<Item = Term>,
        Term: Into<String>,
    {
        let label = label.into();
        let mut terms: Vec<String> = terms.into_iter().map(Into::into).collect();
        terms.sort_by_key(String::len);
        terms.dedup();

        let len = terms
            .iter()
            .map(String::len)
            .max()
            .unwrap_or(0)
            .max(label.len());

        Self {
            label,
            terms,
            timestamp,
            len,
            source,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchHit {
    pub(crate) score: u32,
    pub(crate) item: SearchItem,
}

pub mod workshop_url {
    use super::{HashSet, PublishedFileId, fmt};
    use thiserror::Error;

    const WORKSHOP_ITEM_URL_PREFIX: &str =
        "https://steamcommunity.com/sharedfiles/filedetails/?id=";

    #[derive(Debug, Clone, Eq, PartialEq, Error)]
    pub enum ParseWorkshopIdsError {
        #[error("invalid Workshop id token `{0}`")]
        InvalidToken(String),
    }

    pub fn workshop_item_url(item_id: impl fmt::Display) -> String {
        format!("{WORKSHOP_ITEM_URL_PREFIX}{item_id}")
    }

    pub fn parse_workshop_id(input: &str) -> Option<PublishedFileId> {
        let token = input.trim();
        parse_plain_workshop_id(token)
            .or_else(|| parse_steam_workshop_url(token))
            .and_then(PublishedFileId::new)
    }

    pub fn parse_workshop_ids(input: &str) -> Result<Vec<PublishedFileId>, ParseWorkshopIdsError> {
        let mut seen = HashSet::new();
        let mut ids = Vec::new();

        for token in input.split(|c: char| c == ',' || c.is_ascii_whitespace()) {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }

            let Some(id) = parse_workshop_id(token) else {
                return Err(ParseWorkshopIdsError::InvalidToken(token.to_owned()));
            };

            if id.get() == 0 {
                return Err(ParseWorkshopIdsError::InvalidToken(token.to_owned()));
            }

            if seen.insert(id) {
                ids.push(id);
            }
        }

        Ok(ids)
    }

    fn parse_plain_workshop_id(token: &str) -> Option<u64> {
        (!token.is_empty() && token.chars().all(|c| c.is_ascii_digit()))
            .then(|| token.parse::<u64>().ok())
            .flatten()
    }

    fn parse_steam_workshop_url(token: &str) -> Option<u64> {
        let token = token.to_ascii_lowercase();
        let without_scheme = token
            .strip_prefix("https://")
            .or_else(|| token.strip_prefix("http://"))
            .unwrap_or(token.as_str());
        let without_www = without_scheme
            .strip_prefix("www.")
            .unwrap_or(without_scheme);
        let path = without_www.strip_prefix("steamcommunity.com/")?;

        if let Some(rest) = path.strip_prefix("sharedfiles/filedetails") {
            return workshop_id_from_url_tail(rest);
        }

        if let Some(rest) = path.strip_prefix("workshop/filedetails") {
            return workshop_id_from_url_tail(rest);
        }

        if let Some(rest) = path.strip_prefix("workshop") {
            return workshop_id_from_url_tail(rest);
        }

        None
    }

    fn workshop_id_from_url_tail(rest: &str) -> Option<u64> {
        if let Some(query) = rest.split_once('?').map(|(_, query)| query) {
            for pair in query.split('&') {
                let Some((key, value)) = pair.split_once('=') else {
                    continue;
                };
                if key == "id" {
                    return parse_leading_workshop_id(value);
                }
            }
        }

        parse_plain_workshop_id(rest.trim_matches('/'))
    }

    fn parse_leading_workshop_id(value: &str) -> Option<u64> {
        let digit_count = value
            .chars()
            .take_while(char::is_ascii_digit)
            .map(char::len_utf8)
            .sum();
        if digit_count == 0 {
            return None;
        }

        value[..digit_count].parse::<u64>().ok()
    }
}
