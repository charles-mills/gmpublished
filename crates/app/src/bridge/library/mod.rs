//! The store is the single app-side filesystem walker for installed addons.
//! It keeps an in-memory GMA header cache so event-driven refreshes only parse
//! changed files, then exposes immutable snapshots for UI/search projections.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, Metadata},
    hash::Hash,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use parking_lot::Mutex;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::{
    AppPaths,
    domain::{InstalledAddon, PublishedFileId},
    gma::{GmaHeader, GmaMeta, GmaMetaEntry, GmaMetadata, is_gma_path},
};

const HEADER_SNAPSHOT_VERSION: u32 = 1;
const HEADER_SNAPSHOT_FILE_NAME: &str = "library-headers.json";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LibraryRefreshReason {
    Startup,
    DiskChanged,
    SettingsChanged,
}

impl LibraryRefreshReason {
    pub(crate) const fn loud(self) -> bool {
        matches!(self, Self::Startup | Self::SettingsChanged)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibrarySnapshot {
    pub(crate) addons: Arc<[InstalledAddon]>,
    pub(crate) epoch: u64,
}

#[derive(Clone, Debug)]
pub struct LibraryRefresh {
    pub(crate) reason: LibraryRefreshReason,
    pub(crate) snapshot: Option<LibrarySnapshot>,
    pub(crate) rerun_after: Option<LibraryRefreshReason>,
}

#[derive(Debug, Default)]
pub struct LibraryStore {
    state: Mutex<LibraryState>,
    header_cache: Arc<Mutex<HeaderCacheStore>>,
}

#[derive(Debug, Default)]
struct LibraryState {
    snapshot: Option<LibrarySnapshot>,
    epoch: u64,
    running: bool,
    pending_reason: Option<LibraryRefreshReason>,
}

type HeaderCache = HashMap<HeaderCacheKey, GmaMeta>;

#[derive(Debug, Default)]
struct HeaderCacheStore {
    entries: HeaderCache,
    snapshot_file: Option<PathBuf>,
    loaded: bool,
    dirty: bool,
    revision: u64,
    persist_state: HeaderCachePersistState,
    #[cfg(test)]
    write_count: usize,
    #[cfg(test)]
    persist_hook: Option<Arc<PersistTestHook>>,
}

#[derive(Debug, Default)]
enum HeaderCachePersistState {
    #[default]
    Idle,
    Running {
        pending: bool,
    },
}

#[cfg(test)]
#[derive(Debug, Default)]
struct PersistTestHook {
    state: Mutex<PersistTestHookState>,
    changed: parking_lot::Condvar,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct PersistTestHookState {
    started: usize,
    completed: usize,
    blocked: bool,
}

#[cfg(test)]
impl PersistTestHook {
    fn blocked() -> Self {
        Self {
            state: Mutex::new(PersistTestHookState {
                blocked: true,
                ..PersistTestHookState::default()
            }),
            changed: parking_lot::Condvar::new(),
        }
    }

    fn before_write(&self) {
        let mut state = self.state.lock();
        state.started += 1;
        self.changed.notify_all();
        while state.blocked {
            self.changed.wait(&mut state);
        }
    }

    fn after_write(&self) {
        let mut state = self.state.lock();
        state.completed += 1;
        drop(state);
        self.changed.notify_all();
    }

    fn wait_until_started(&self, expected: usize) {
        let mut state = self.state.lock();
        while state.started < expected {
            self.changed.wait(&mut state);
        }
    }

    fn wait_until_completed(&self, expected: usize) {
        let mut state = self.state.lock();
        while state.completed < expected {
            self.changed.wait(&mut state);
        }
    }

    fn allow_writes(&self) {
        let mut state = self.state.lock();
        state.blocked = false;
        drop(state);
        self.changed.notify_all();
    }
}

pub fn header_snapshot_path() -> Option<PathBuf> {
    gmpublished_backend::appdata::cache_dir().map(|dir| dir.join(HEADER_SNAPSHOT_FILE_NAME))
}

impl LibraryStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_header_snapshot_file(&self, path: PathBuf) {
        let mut cache = self.header_cache.lock();
        cache.snapshot_file = Some(path);
        cache.loaded = false;
    }

    #[cfg(test)]
    fn header_snapshot_write_count_for_test(&self) -> usize {
        self.header_cache.lock().write_count
    }

    #[cfg(test)]
    fn set_header_snapshot_persist_hook_for_test(&self, hook: Arc<PersistTestHook>) {
        self.header_cache.lock().persist_hook = Some(hook);
    }

    pub(crate) fn snapshot(&self) -> Option<LibrarySnapshot> {
        self.state.lock().snapshot.clone()
    }

    pub(crate) fn begin_refresh(&self, reason: LibraryRefreshReason) -> bool {
        let mut state = self.state.lock();
        if state.running {
            // Coalesce, but never let a quiet reason downgrade a pending
            // loud one (a settings change immediately followed by watcher
            // events must still produce the loud reset).
            state.pending_reason = match state.pending_reason {
                Some(pending) if pending.loud() => Some(pending),
                _ => Some(reason),
            };
            return false;
        }

        state.running = true;
        state.pending_reason = None;
        true
    }

    pub(crate) fn refresh_blocking(
        &self,
        paths: &AppPaths,
        reason: LibraryRefreshReason,
    ) -> LibraryRefresh {
        let snapshot = discover(paths, &self.header_cache).map_or_else(
            || {
                self.clear_snapshot_and_cache();
                None
            },
            |addons| Some(self.commit_snapshot(addons)),
        );
        persist_header_cache_if_dirty(&self.header_cache);
        let rerun_after = self.finish_refresh();

        LibraryRefresh {
            reason,
            snapshot,
            rerun_after,
        }
    }

    pub(crate) fn abort_refresh(&self) -> Option<LibraryRefreshReason> {
        self.finish_refresh()
    }

    fn commit_snapshot(&self, addons: Vec<InstalledAddon>) -> LibrarySnapshot {
        let mut state = self.state.lock();
        state.epoch = state.epoch.wrapping_add(1).max(1);
        let snapshot = LibrarySnapshot {
            addons: Arc::from(addons.into_boxed_slice()),
            epoch: state.epoch,
        };
        state.snapshot = Some(snapshot.clone());
        snapshot
    }

    fn clear_snapshot_and_cache(&self) {
        self.state.lock().snapshot = None;
        let mut cache = self.header_cache.lock();
        if !cache.entries.is_empty() {
            cache.entries.clear();
            mark_header_cache_dirty(&mut cache);
        }
        drop(cache);
    }

    fn finish_refresh(&self) -> Option<LibraryRefreshReason> {
        let mut state = self.state.lock();
        state.running = false;
        state.pending_reason.take()
    }
}

fn discover(
    paths: &AppPaths,
    header_cache: &Mutex<HeaderCacheStore>,
) -> Option<Vec<InstalledAddon>> {
    let gmod = paths.gmod_dir.as_ref()?;

    load_header_cache_if_needed(header_cache);

    let mut candidates = Vec::new();
    collect_addons_dir(gmod, &mut candidates);
    let addons_dir_count = candidates.len();
    collect_cache_dir(gmod, &mut candidates);
    let cache_dir_count = candidates.len() - addons_dir_count;
    collect_workshop_content_dir(gmod, &mut candidates);
    let workshop_count = candidates.len() - addons_dir_count - cache_dir_count;

    let candidate_count = candidates.len();
    let (mut addons, seen_cache_keys) = process_candidates(candidates, header_cache);
    // A partial launch scan is otherwise indistinguishable from a small
    // library; the per-root split says whether enumeration or per-file
    // reads lost addons.
    log::info!(
        "library scan: {} addons from {candidate_count} candidates \
         (addons dir {addons_dir_count}, cache {cache_dir_count}, workshop content {workshop_count}; \
         {} dropped)",
        addons.len(),
        candidate_count - addons.len(),
    );
    addons.sort_by(|left, right| {
        right
            .modified_epoch_seconds
            .cmp(&left.modified_epoch_seconds)
            .then_with(|| left.path.cmp(&right.path))
    });

    prune_header_cache(header_cache, &seen_cache_keys);

    Some(addons)
}

fn process_candidates(
    candidates: Vec<DiscoveredCandidate>,
    header_cache: &Mutex<HeaderCacheStore>,
) -> (Vec<InstalledAddon>, HashSet<HeaderCacheKey>) {
    let (addons, seen_cache_keys): (Vec<_>, HashSet<_>) = candidates
        .into_par_iter()
        .filter_map(|candidate| read_candidate(candidate, header_cache))
        .unzip();
    (addons, seen_cache_keys)
}

fn collect_addons_dir(gmod: &Path, candidates: &mut Vec<DiscoveredCandidate>) {
    let addons_dir = gmod.join("GarrysMod/addons");
    let Ok(read_dir) = addons_dir.read_dir() else {
        return;
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() || !is_gma_path(&path) {
            continue;
        }
        let workshop_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(workshop_id_from_name);
        candidates.push(DiscoveredCandidate { path, workshop_id });
    }
}

fn collect_cache_dir(gmod: &Path, candidates: &mut Vec<DiscoveredCandidate>) {
    let cache_dir = gmod.join("GarrysMod/cache/workshop");
    let Ok(read_dir) = cache_dir.read_dir() else {
        return;
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() || !is_gma_path(&path) {
            continue;
        }
        let workshop_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(workshop_id_from_name);
        candidates.push(DiscoveredCandidate { path, workshop_id });
    }
}

fn collect_workshop_content_dir(gmod: &Path, candidates: &mut Vec<DiscoveredCandidate>) {
    let Some(content_dir) = gmod
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("workshop/content/4000"))
    else {
        return;
    };
    let read_dir = match content_dir.read_dir() {
        Ok(read_dir) => read_dir,
        Err(error) => {
            log::warn!(
                "failed to enumerate workshop content dir {}: {error}",
                content_dir.display()
            );
            return;
        }
    };

    for entry in read_dir.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let workshop_id = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u64>().ok())
            .and_then(PublishedFileId::new);
        let Ok(addon_dir) = entry.path().read_dir() else {
            continue;
        };
        let Some(path) = addon_dir
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.is_file() && is_gma_path(path))
        else {
            continue;
        };
        candidates.push(DiscoveredCandidate { path, workshop_id });
    }
}

fn read_candidate(
    candidate: DiscoveredCandidate,
    header_cache: &Mutex<HeaderCacheStore>,
) -> Option<(InstalledAddon, HeaderCacheKey)> {
    let metadata = candidate.path.metadata().ok()?;
    let file_size_bytes = metadata.len();
    let modified_epoch_seconds = modified_epoch_seconds(&metadata);
    let cache_key = HeaderCacheKey::from_metadata(candidate.path.clone(), &metadata);
    let meta = match cached_meta(cache_key.clone(), header_cache, || {
        library_meta(&candidate.path)
    }) {
        Ok(meta) => meta,
        Err(error) => {
            log::debug!(
                "skipping installed GMA candidate {}: {error}",
                candidate.path.display()
            );
            return None;
        }
    };
    let canonical_path = gmpublished_backend::path::canonicalize(candidate.path.clone());
    Some((
        InstalledAddon {
            path: candidate.path,
            canonical_path,
            workshop_id: candidate.workshop_id,
            file_size_bytes,
            modified_epoch_seconds,
            meta,
        },
        cache_key,
    ))
}

fn library_meta(path: &Path) -> Result<GmaMeta, super::gma::GmaError> {
    #[cfg(feature = "asset-studio")]
    {
        GmaMeta::open_index(path)
    }
    #[cfg(not(feature = "asset-studio"))]
    {
        GmaMeta::open_header_only(path)
    }
}

fn cached_meta<E>(
    key: HeaderCacheKey,
    header_cache: &Mutex<HeaderCacheStore>,
    read: impl FnOnce() -> Result<GmaMeta, E>,
) -> Result<GmaMeta, E> {
    let cached = {
        let cache = header_cache.lock();
        cache.entries.get(&key).cloned()
    };
    if let Some(meta) = cached {
        return Ok(meta);
    }

    let meta = read()?;
    let mut cache = header_cache.lock();
    cache.entries.insert(key, meta.clone());
    mark_header_cache_dirty(&mut cache);
    drop(cache);
    Ok(meta)
}

fn mark_header_cache_dirty(cache: &mut HeaderCacheStore) {
    cache.dirty = true;
    cache.revision = cache.revision.wrapping_add(1);
}

fn load_header_cache_if_needed(header_cache: &Mutex<HeaderCacheStore>) {
    let mut cache = header_cache.lock();
    if cache.loaded {
        return;
    }
    cache.loaded = true;

    let Some(path) = cache.snapshot_file.as_deref() else {
        return;
    };
    cache.entries = load_header_cache_snapshot(path);
    cache.dirty = false;
    cache.revision = 0;
}

fn prune_header_cache(header_cache: &Mutex<HeaderCacheStore>, seen_keys: &HashSet<HeaderCacheKey>) {
    let mut cache = header_cache.lock();
    let before = cache.entries.len();
    cache.entries.retain(|key, _| seen_keys.contains(key));
    if cache.entries.len() != before {
        mark_header_cache_dirty(&mut cache);
    }
}

fn persist_header_cache_if_dirty(header_cache: &Arc<Mutex<HeaderCacheStore>>) {
    #[cfg(test)]
    let hook;
    {
        let mut cache = header_cache.lock();
        if !cache.dirty {
            return;
        }
        if cache.snapshot_file.is_none() {
            return;
        }
        match &mut cache.persist_state {
            HeaderCachePersistState::Idle => {
                cache.persist_state = HeaderCachePersistState::Running { pending: false };
            }
            HeaderCachePersistState::Running { pending } => {
                *pending = true;
                return;
            }
        }
        #[cfg(test)]
        {
            hook = cache.persist_hook.clone();
        }
        drop(cache);
    }

    let header_cache = Arc::clone(header_cache);
    #[cfg(not(test))]
    rayon::spawn(move || persist_header_cache_inner(&header_cache, || {}, || {}));
    #[cfg(test)]
    rayon::spawn(move || {
        persist_header_cache_inner(
            &header_cache,
            || {
                if let Some(hook) = hook.as_ref() {
                    hook.before_write();
                }
            },
            || {
                if let Some(hook) = hook.as_ref() {
                    hook.after_write();
                }
            },
        );
    });
}

fn persist_header_cache_inner(
    header_cache: &Mutex<HeaderCacheStore>,
    mut before_write: impl FnMut(),
    mut after_write: impl FnMut(),
) {
    loop {
        before_write();
        let prepared = {
            let cache = header_cache.lock();
            let path = cache
                .snapshot_file
                .clone()
                .expect("persistence only runs with a snapshot path");
            let revision = cache.revision;
            (
                path,
                revision,
                serialize_header_cache_snapshot(&cache.entries),
            )
        };
        let (path, revision, result) = prepared;
        let result = result.and_then(|bytes| {
            crate::util::fs::atomic_write(&path, &bytes).map_err(|source| {
                HeaderSnapshotWriteError::Write {
                    path: path.clone(),
                    source,
                }
            })
        });

        let mut cache = header_cache.lock();
        match result {
            Ok(()) => {
                if cache.revision == revision {
                    cache.dirty = false;
                }
                #[cfg(test)]
                {
                    cache.write_count += 1;
                }
            }
            Err(error) => {
                log::warn!(
                    "failed to write library header snapshot {}: {error}",
                    path.display()
                );
            }
        }

        let pending = matches!(
            cache.persist_state,
            HeaderCachePersistState::Running { pending: true }
        );
        let run_trailing = pending && cache.dirty;
        cache.persist_state = if run_trailing {
            HeaderCachePersistState::Running { pending: false }
        } else {
            HeaderCachePersistState::Idle
        };
        drop(cache);
        after_write();
        if !run_trailing {
            return;
        }
    }
}

fn modified_epoch_seconds(metadata: &Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs())
}

fn modified_epoch_nanos(metadata: &Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos())
}

fn workshop_id_from_name(name: &str) -> Option<PublishedFileId> {
    gmpublished_backend::gma::ws_id_from_file_name(name)
        .map(|id| PublishedFileId::new(id.0).expect("backend never returns a zero workshop id"))
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct HeaderCacheKey {
    path: PathBuf,
    file_len: u64,
    modified_epoch_nanos: u128,
}

impl HeaderCacheKey {
    fn from_metadata(path: PathBuf, metadata: &Metadata) -> Self {
        Self {
            path,
            file_len: metadata.len(),
            modified_epoch_nanos: modified_epoch_nanos(metadata),
        }
    }

    #[cfg(test)]
    fn for_test(path: impl Into<PathBuf>, file_len: u64, modified_epoch_nanos: u128) -> Self {
        Self {
            path: path.into(),
            file_len,
            modified_epoch_nanos,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct HeaderSnapshotFile {
    version: u32,
    entries: Vec<HeaderSnapshotEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
struct HeaderSnapshotEntry {
    path: String,
    file_len: u64,
    modified_epoch_nanos: String,
    header: HeaderSnapshotHeader,
    #[serde(default)]
    entries: Vec<HeaderSnapshotGmaEntry>,
}

impl HeaderSnapshotEntry {
    fn from_cache(key: &HeaderCacheKey, meta: &GmaMeta) -> Option<Self> {
        Some(Self {
            path: key.path.to_str()?.to_owned(),
            file_len: key.file_len,
            modified_epoch_nanos: key.modified_epoch_nanos.to_string(),
            header: HeaderSnapshotHeader::from(&meta.header),
            entries: meta
                .entries
                .iter()
                .map(HeaderSnapshotGmaEntry::from)
                .collect(),
        })
    }

    fn into_cache(self) -> Option<(HeaderCacheKey, GmaMeta)> {
        let modified_epoch_nanos = self.modified_epoch_nanos.parse::<u128>().ok()?;
        let path = PathBuf::from(self.path);
        let header = GmaHeader::from(self.header);
        let key = HeaderCacheKey {
            path: path.clone(),
            file_len: self.file_len,
            modified_epoch_nanos,
        };
        let meta = GmaMeta {
            path,
            header,
            entries: self.entries.into_iter().map(GmaMetaEntry::from).collect(),
        };
        Some((key, meta))
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct HeaderSnapshotGmaEntry {
    path: String,
    size: u64,
    crc32: u32,
}

impl From<&GmaMetaEntry> for HeaderSnapshotGmaEntry {
    fn from(entry: &GmaMetaEntry) -> Self {
        Self {
            path: entry.path.clone(),
            size: entry.size,
            crc32: entry.crc32,
        }
    }
}

impl From<HeaderSnapshotGmaEntry> for GmaMetaEntry {
    fn from(entry: HeaderSnapshotGmaEntry) -> Self {
        Self {
            path: entry.path,
            size: entry.size,
            crc32: entry.crc32,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct HeaderSnapshotHeader {
    version: u8,
    timestamp: u64,
    author: String,
    addon_version: i32,
    metadata: HeaderSnapshotMetadata,
}

impl From<&GmaHeader> for HeaderSnapshotHeader {
    fn from(header: &GmaHeader) -> Self {
        Self {
            version: header.version,
            timestamp: header.timestamp,
            author: header.author.clone(),
            addon_version: header.addon_version,
            metadata: HeaderSnapshotMetadata::from(&header.metadata),
        }
    }
}

impl From<HeaderSnapshotHeader> for GmaHeader {
    fn from(header: HeaderSnapshotHeader) -> Self {
        Self {
            version: header.version,
            timestamp: header.timestamp,
            author: header.author,
            addon_version: header.addon_version,
            metadata: GmaMetadata::from(header.metadata),
        }
    }
}

/// Explicitly tagged so Standard and Legacy metadata never depend on the
/// live untagged backend representation.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind")]
enum HeaderSnapshotMetadata {
    Standard {
        title: String,
        addon_type: String,
        tags: Vec<String>,
        ignore: Vec<String>,
    },
    Legacy {
        title: String,
        description: String,
    },
}

impl From<&GmaMetadata> for HeaderSnapshotMetadata {
    fn from(metadata: &GmaMetadata) -> Self {
        match metadata {
            GmaMetadata::Standard {
                title,
                addon_type,
                tags,
                ignore,
            } => Self::Standard {
                title: title.clone(),
                addon_type: addon_type.clone(),
                tags: tags.clone(),
                ignore: ignore.clone(),
            },
            GmaMetadata::Legacy { title, description } => Self::Legacy {
                title: title.clone(),
                description: description.clone(),
            },
        }
    }
}

impl From<HeaderSnapshotMetadata> for GmaMetadata {
    fn from(metadata: HeaderSnapshotMetadata) -> Self {
        match metadata {
            HeaderSnapshotMetadata::Standard {
                title,
                addon_type,
                tags,
                ignore,
            } => Self::Standard {
                title,
                addon_type,
                tags,
                ignore,
            },
            HeaderSnapshotMetadata::Legacy { title, description } => {
                Self::Legacy { title, description }
            }
        }
    }
}

fn load_header_cache_snapshot(path: &Path) -> HeaderCache {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) => {
            match error.kind() {
                io::ErrorKind::NotFound => {
                    log::debug!("library header snapshot {} is missing", path.display());
                }
                _ => {
                    log::debug!(
                        "ignoring unreadable library header snapshot {}: {error}",
                        path.display()
                    );
                }
            }
            return HeaderCache::default();
        }
    };

    let snapshot = match serde_json::from_str::<HeaderSnapshotFile>(&contents) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log::debug!(
                "discarding unparseable library header snapshot {}: {error}",
                path.display()
            );
            return HeaderCache::default();
        }
    };
    if snapshot.version != HEADER_SNAPSHOT_VERSION {
        log::debug!(
            "discarding library header snapshot {} with unsupported version {}",
            path.display(),
            snapshot.version
        );
        return HeaderCache::default();
    }

    snapshot
        .entries
        .into_iter()
        .filter_map(HeaderSnapshotEntry::into_cache)
        .collect()
}

#[derive(Debug, thiserror::Error)]
enum HeaderSnapshotWriteError {
    #[error("failed to serialize library header snapshot: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to persist library header snapshot {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn serialize_header_cache_snapshot(
    entries: &HeaderCache,
) -> Result<Vec<u8>, HeaderSnapshotWriteError> {
    let mut snapshot_entries = entries
        .iter()
        .filter_map(|(key, meta)| HeaderSnapshotEntry::from_cache(key, meta))
        .collect::<Vec<_>>();
    snapshot_entries.sort_unstable_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.file_len.cmp(&right.file_len))
            .then_with(|| left.modified_epoch_nanos.cmp(&right.modified_epoch_nanos))
    });
    let snapshot = HeaderSnapshotFile {
        version: HEADER_SNAPSHOT_VERSION,
        entries: snapshot_entries,
    };

    serde_json::to_vec(&snapshot).map_err(HeaderSnapshotWriteError::Serialize)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiscoveredCandidate {
    path: PathBuf,
    workshop_id: Option<PublishedFileId>,
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::bridge::gma::{GmaError, GmaHeader, GmaMetadata};
    use crate::test_support::{GmaFixtureBuilder, TestDir, write_gma_fixture};

    fn test_meta(title: &str) -> GmaMeta {
        test_meta_at(PathBuf::from(format!("/tmp/{title}.gma")), title)
    }

    fn test_meta_at(path: impl Into<PathBuf>, title: &str) -> GmaMeta {
        GmaMeta {
            path: path.into(),
            header: GmaHeader {
                version: 3,
                timestamp: 0,
                metadata: GmaMetadata::Standard {
                    title: title.to_owned(),
                    addon_type: "servercontent".to_owned(),
                    tags: vec!["build".to_owned()],
                    ignore: Vec::new(),
                },
                author: String::new(),
                addon_version: 1,
            },
            entries: Vec::new(),
        }
    }

    fn test_cache() -> Mutex<HeaderCacheStore> {
        Mutex::new(HeaderCacheStore::default())
    }

    fn paths_with_gmod(gmod_dir: PathBuf) -> AppPaths {
        let root = gmod_dir
            .parent()
            .unwrap_or(gmod_dir.as_path())
            .to_path_buf();
        AppPaths {
            settings_file: root.join("settings.json"),
            default_user_data_dir: root.join("default-user-data"),
            default_temp_dir: root.join("default-temp"),
            default_downloads_dir: Some(root.join("default-downloads")),
            temp_dir: root.join("temp"),
            user_data_dir: root.join("user-data"),
            downloads_dir: Some(root.join("downloads")),
            gmod_dir: Some(gmod_dir),
        }
    }

    fn write_installed_gma(gmod_dir: &Path, file_name: &str) -> PathBuf {
        let fixture = GmaFixtureBuilder::new("Installed Fixture")
            .entry("lua/autorun/init.lua", b"print('ok')\n".to_vec())
            .build();
        write_gma_fixture(gmod_dir.join("GarrysMod/addons").join(file_name), &fixture)
    }

    fn sorted_addons(mut addons: Vec<InstalledAddon>) -> Vec<InstalledAddon> {
        addons.sort_by(|left, right| {
            right
                .modified_epoch_seconds
                .cmp(&left.modified_epoch_seconds)
                .then_with(|| left.path.cmp(&right.path))
        });
        addons
    }

    #[test]
    fn refresh_coalesces_pending_requests_into_one_rerun() {
        let store = LibraryStore::new();

        assert!(store.begin_refresh(LibraryRefreshReason::DiskChanged));
        assert!(!store.begin_refresh(LibraryRefreshReason::DiskChanged));
        assert!(!store.begin_refresh(LibraryRefreshReason::DiskChanged));

        assert_eq!(
            store.finish_refresh(),
            Some(LibraryRefreshReason::DiskChanged)
        );
        assert!(store.begin_refresh(LibraryRefreshReason::DiskChanged));
        assert_eq!(store.finish_refresh(), None);
    }

    #[test]
    fn header_cache_reuses_unchanged_path_len_and_mtime() {
        let cache = test_cache();
        let parses = AtomicUsize::new(0);
        let key = HeaderCacheKey::for_test("/tmp/addon.gma", 10, 100);

        let first: Result<GmaMeta, GmaError> = cached_meta(key.clone(), &cache, || {
            parses.fetch_add(1, Ordering::SeqCst);
            Ok(test_meta("first"))
        });
        let second: Result<GmaMeta, GmaError> = cached_meta(key, &cache, || {
            parses.fetch_add(1, Ordering::SeqCst);
            Ok(test_meta("second"))
        });

        assert_eq!(parses.load(Ordering::SeqCst), 1);
        assert_eq!(first.expect("first meta"), second.expect("cached meta"));
    }

    #[test]
    fn header_cache_reparses_when_file_fingerprint_changes() {
        let cache = test_cache();
        let parses = AtomicUsize::new(0);

        let first: Result<GmaMeta, GmaError> = cached_meta(
            HeaderCacheKey::for_test("/tmp/addon.gma", 10, 100),
            &cache,
            || {
                parses.fetch_add(1, Ordering::SeqCst);
                Ok(test_meta("first"))
            },
        );
        let second: Result<GmaMeta, GmaError> = cached_meta(
            HeaderCacheKey::for_test("/tmp/addon.gma", 11, 100),
            &cache,
            || {
                parses.fetch_add(1, Ordering::SeqCst);
                Ok(test_meta("second"))
            },
        );

        assert_eq!(parses.load(Ordering::SeqCst), 2);
        assert_ne!(first.expect("first meta"), second.expect("changed meta"));
    }

    #[test]
    fn header_cache_snapshot_round_trips_and_mismatched_fingerprint_misses() {
        let temp = TestDir::new("gmpublished-library-header-cache");
        let snapshot_file = temp.join("cache/library-headers.json");
        let path = PathBuf::from("/tmp/addon.gma");
        let key = HeaderCacheKey::for_test(path.clone(), 10, 100);
        let mut meta = test_meta_at(path.clone(), "cached");
        meta.entries.push(GmaMetaEntry {
            path: "maps/rp_riverden_v1a.bsp".to_owned(),
            size: 123,
            crc32: 456,
        });

        let cache = test_cache();
        {
            let mut cache = cache.lock();
            cache.snapshot_file = Some(snapshot_file.clone());
            cache.loaded = true;
            cache.dirty = true;
            cache.entries.insert(key.clone(), meta.clone());
        }
        let bytes = serialize_header_cache_snapshot(&cache.lock().entries)
            .expect("header snapshot should serialize");
        crate::util::fs::atomic_write(&snapshot_file, &bytes)
            .expect("header snapshot should persist");

        let fresh = Mutex::new(HeaderCacheStore {
            snapshot_file: Some(snapshot_file),
            ..HeaderCacheStore::default()
        });
        load_header_cache_if_needed(&fresh);
        let loaded_entries = fresh.lock().entries.clone();
        let expected_entries: HeaderCache = [(key, meta)].into_iter().collect();
        assert_eq!(loaded_entries, expected_entries);

        let parses = AtomicUsize::new(0);
        let changed = test_meta_at(path, "changed");
        let loaded = cached_meta(
            HeaderCacheKey::for_test("/tmp/addon.gma", 11, 100),
            &fresh,
            || {
                parses.fetch_add(1, Ordering::SeqCst);
                Ok::<_, GmaError>(changed.clone())
            },
        )
        .expect("fingerprint miss should reparse");

        assert_eq!(parses.load(Ordering::SeqCst), 1);
        assert_eq!(loaded, changed);
    }

    #[cfg(feature = "asset-studio")]
    #[test]
    fn asset_studio_refresh_records_entry_metadata_for_file_search() {
        let temp = TestDir::new("gmpublished-library-entry-index");
        let gmod_dir = temp.dir("steamapps/common/GarrysMod");
        write_installed_gma(&gmod_dir, "addon.gma");
        let paths = paths_with_gmod(gmod_dir);
        let store = LibraryStore::new();

        let refresh = store.refresh_blocking(&paths, LibraryRefreshReason::Startup);
        let snapshot = refresh.snapshot.expect("gmod dir should produce snapshot");

        assert_eq!(snapshot.addons.len(), 1);
        assert!(
            snapshot.addons[0]
                .meta
                .entries
                .iter()
                .any(|entry| { entry.path == "lua/autorun/init.lua" && entry.size > 0 })
        );
    }

    #[test]
    fn header_cache_snapshot_corrupt_or_wrong_version_loads_empty() {
        let temp = TestDir::new("gmpublished-library-header-cache-corrupt");
        let snapshot_file = temp.join("cache/library-headers.json");

        fs::create_dir_all(snapshot_file.parent().expect("snapshot parent")).expect("cache dir");
        fs::write(&snapshot_file, b"{ not json").expect("corrupt snapshot");
        let corrupt = Mutex::new(HeaderCacheStore {
            snapshot_file: Some(snapshot_file.clone()),
            ..HeaderCacheStore::default()
        });
        load_header_cache_if_needed(&corrupt);
        assert!(corrupt.lock().entries.is_empty());

        fs::write(&snapshot_file, br#"{"version":999,"entries":[]}"#)
            .expect("wrong-version snapshot");
        let wrong_version = Mutex::new(HeaderCacheStore {
            snapshot_file: Some(snapshot_file),
            ..HeaderCacheStore::default()
        });
        load_header_cache_if_needed(&wrong_version);
        assert!(wrong_version.lock().entries.is_empty());
    }

    #[test]
    fn parallel_candidate_scan_matches_serial_snapshot_and_order() {
        let temp = TestDir::new("gmpublished-library-parallel-scan");
        let gmod_dir = temp.dir("steamapps/common/GarrysMod");
        for file_name in ["300.gma", "100.gma", "400.gma", "200.gma"] {
            write_installed_gma(&gmod_dir, file_name);
        }

        let mut candidates = Vec::new();
        collect_addons_dir(&gmod_dir, &mut candidates);
        let serial_cache = test_cache();
        let serial = sorted_addons(
            candidates
                .iter()
                .cloned()
                .filter_map(|candidate| read_candidate(candidate, &serial_cache))
                .map(|(addon, _)| addon)
                .collect(),
        );
        let parallel_cache = test_cache();
        let (parallel, _) = process_candidates(candidates, &parallel_cache);
        let parallel = sorted_addons(parallel);

        assert_eq!(parallel, serial);
        assert_eq!(parallel.len(), 4);
    }

    #[test]
    fn snapshot_is_committed_before_header_cache_persist() {
        let temp = TestDir::new("gmpublished-library-write-behind");
        let gmod_dir = temp.dir("steamapps/common/GarrysMod");
        write_installed_gma(&gmod_dir, "addon.gma");
        let paths = paths_with_gmod(gmod_dir);
        let snapshot_file = temp.join("cache/library-headers.json");
        let store = LibraryStore::new();
        store.set_header_snapshot_file(snapshot_file.clone());
        let hook = Arc::new(PersistTestHook::blocked());
        store.set_header_snapshot_persist_hook_for_test(Arc::clone(&hook));

        let refresh = store.refresh_blocking(&paths, LibraryRefreshReason::Startup);
        let returned = refresh.snapshot.expect("refresh should return a snapshot");
        hook.wait_until_started(1);

        assert_eq!(store.snapshot(), Some(returned));
        assert!(!snapshot_file.exists());

        hook.allow_writes();
        hook.wait_until_completed(1);
        assert!(snapshot_file.is_file());
    }

    #[test]
    fn unchanged_refresh_does_not_rewrite_header_cache_snapshot() {
        let temp = TestDir::new("gmpublished-library-header-cache-dirty");
        let gmod_dir = temp.dir("steamapps/common/GarrysMod");
        write_installed_gma(&gmod_dir, "addon.gma");
        let paths = paths_with_gmod(gmod_dir);
        let snapshot_file = temp.join("cache/library-headers.json");
        let store = LibraryStore::new();
        store.set_header_snapshot_file(snapshot_file.clone());
        let hook = Arc::new(PersistTestHook::default());
        store.set_header_snapshot_persist_hook_for_test(Arc::clone(&hook));

        let first = store.refresh_blocking(&paths, LibraryRefreshReason::Startup);
        assert!(first.snapshot.is_some());
        hook.wait_until_completed(1);
        assert_eq!(store.header_snapshot_write_count_for_test(), 1);
        assert!(snapshot_file.is_file());

        let second = store.refresh_blocking(&paths, LibraryRefreshReason::DiskChanged);
        assert!(second.snapshot.is_some());
        assert_eq!(store.header_snapshot_write_count_for_test(), 1);
    }

    #[test]
    fn committed_snapshot_epochs_are_monotonic() {
        let store = LibraryStore::new();

        let first = store.commit_snapshot(Vec::new());
        let second = store.commit_snapshot(Vec::new());

        assert_eq!(first.epoch, 1);
        assert_eq!(second.epoch, 2);
    }
}
