use parking_lot::{Condvar, Mutex, MutexGuard};
use rayon::ThreadPool;

use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use steamworks::{FileType, ItemState, PublishedFileId, QueryResults, UGC};

use crate::{
    GMAError, GMAFile, GMOD_APP_ID,
    appdata::AppData,
    events::{BackendEvent, DownloadStartedEvent, ExtractionStartedEvent},
    gma::{
        ExtractDestination, ExtractOptions, Whitelist, read::GmaView, whitelist::AddonWhitelist,
    },
    steam::Steam,
    transactions::Transactions,
};

static THREAD_POOL: LazyLock<ThreadPool> = LazyLock::new(|| thread_pool!());

/// Pool for parallel legacy-CDN payload downloads. Separate from
/// [`THREAD_POOL`] so completed downloads pipeline straight into extraction
/// instead of queueing behind the remaining (slow, network-bound) downloads.
static HTTP_DOWNLOAD_POOL: LazyLock<ThreadPool> = LazyLock::new(|| thread_pool!(8));

/// Cadence `Downloads::watchdog` re-checks in-progress download callbacks at.
const CALLBACK_PUMP_INTERVAL: Duration = Duration::from_millis(50);

/// Granularity of legacy-CDN download progress events.
const HTTP_PROGRESS_STEP: f64 = 0.01;

#[derive(Debug)]
pub struct DownloadInner {
    item: PublishedFileId,
    transaction: crate::transactions::Transaction,
    sent_total: AtomicBool,
    extract_destination: ExtractDestination,
}
impl std::hash::Hash for DownloadInner {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.item.hash(state);
    }
}
impl Eq for DownloadInner {}
impl PartialEq for DownloadInner {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item
    }
}
pub type Download = Arc<DownloadInner>;

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkshopDownloadQueryItem {
    Item(PublishedFileId),
    Collection { children: Vec<PublishedFileId> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkshopDownloadAction {
    FetchWorkshopItems(Vec<PublishedFileId>),
    QueueDownload(PublishedFileId),
    MissingItem(PublishedFileId),
}

struct PossibleCollectionsState {
    queue: Vec<PublishedFileId>,
    downloaded: HashSet<PublishedFileId>,
}
impl PossibleCollectionsState {
    fn new(queue: Vec<PublishedFileId>) -> Self {
        Self {
            downloaded: HashSet::from_iter(queue.iter().copied()),
            queue,
        }
    }

    fn split_initial(
        ids: &mut Vec<PublishedFileId>,
        workshop_cache: Option<&HashSet<PublishedFileId>>,
    ) -> Self {
        Self::new(if let Some(workshop_cache) = workshop_cache {
            let mut possible_collections = Vec::with_capacity(ids.len());
            ids.retain(|id| {
                if workshop_cache.contains(id) {
                    true
                } else {
                    possible_collections.push(*id);
                    false
                }
            });
            possible_collections
        } else {
            std::mem::take(ids)
        })
    }

    fn next_query(&mut self, connected: bool) -> Option<Vec<PublishedFileId>> {
        if self.queue.is_empty() || !connected {
            None
        } else {
            Some(core::mem::take(&mut self.queue))
        }
    }

    fn apply_query_results(
        &mut self,
        query: &[PublishedFileId],
        results: impl IntoIterator<Item = Option<WorkshopDownloadQueryItem>>,
    ) -> Vec<WorkshopDownloadAction> {
        let mut actions = Vec::new();
        let mut not_collections = Vec::with_capacity(query.len());

        for (i, item) in results.into_iter().enumerate() {
            match item {
                Some(WorkshopDownloadQueryItem::Collection { children }) => {
                    actions.push(WorkshopDownloadAction::FetchWorkshopItems(children.clone()));
                    for item in children {
                        if self.downloaded.insert(item) {
                            self.queue.push(item);
                        }
                    }
                }
                Some(WorkshopDownloadQueryItem::Item(item)) => {
                    not_collections.push(item);
                    actions.push(WorkshopDownloadAction::QueueDownload(item));
                }
                None => {
                    if let Some(item) = query.get(i) {
                        actions.push(WorkshopDownloadAction::MissingItem(*item));
                    }
                }
            }
        }

        if !not_collections.is_empty() {
            actions.push(WorkshopDownloadAction::FetchWorkshopItems(not_collections));
        }

        actions
    }
}

fn append_pending_batch<T>(downloading: &mut Vec<T>, pending: &mut Vec<T>) -> usize {
    let batch_len = pending.len();
    downloading.append(pending);
    batch_len
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkshopDownloadPreflightLimits {
    pub max_item_bytes: u64,
    pub max_total_bytes: u64,
    pub query_timeout: Duration,
}

#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopDownloadPreflight {
    pub items: Vec<WorkshopDownloadPreflightItem>,
    pub total_file_size: u64,
    pub limits: WorkshopDownloadPreflightLimits,
}

#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkshopDownloadPreflightItem {
    pub id: PublishedFileId,
    pub file_type: FileType,
    pub file_size: u64,
    pub state: ItemState,
    pub has_install_info: bool,
}

impl WorkshopDownloadPreflightItem {
    pub fn installed(&self) -> bool {
        self.state.intersects(ItemState::INSTALLED)
    }

    pub fn needs_update(&self) -> bool {
        self.state.intersects(ItemState::NEEDS_UPDATE)
    }

    pub fn queues_steam_download(&self) -> bool {
        !self.installed() || self.needs_update()
    }
}

#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WorkshopDownloadPreflightError {
    #[error("no Workshop ids were supplied")]
    EmptyIds,
    #[error("Steam is not connected")]
    SteamNotConnected,
    #[error("Steam UGC query creation failed")]
    QueryCreateFailed,
    #[error("Steam UGC query failed: {0}")]
    QueryFailed(String),
    #[error("Steam UGC query did not complete within {timeout:?}")]
    QueryTimedOut { timeout: Duration },
    #[error("Workshop item {} was not returned", .0.0)]
    MissingItem(PublishedFileId),
    #[error(
        "Steam returned Workshop item {} for requested item {}",
        actual.0,
        expected.0
    )]
    QueryItemMismatch {
        expected: PublishedFileId,
        actual: PublishedFileId,
    },
    #[error(
        "Workshop item {} is a collection; explicit uncached proof requires direct items",
        .0.0
    )]
    CollectionItem(PublishedFileId),
    #[error("Workshop item {} reported a zero/unknown file size", .0.0)]
    UnknownFileSize(PublishedFileId),
    #[error(
        "Workshop item {} is {} bytes, above the {} byte per-item limit",
        id.0,
        file_size,
        max_item_bytes
    )]
    ItemTooLarge {
        id: PublishedFileId,
        file_size: u64,
        max_item_bytes: u64,
    },
    #[error(
        "requested Workshop items total {total_file_size} bytes, above the {max_total_bytes} byte total limit"
    )]
    TotalTooLarge {
        total_file_size: u64,
        max_total_bytes: u64,
    },
    #[error(
        "Workshop item {} is already installed and does not need update ({:?})",
        id.0,
        state
    )]
    AlreadyInstalled {
        id: PublishedFileId,
        state: ItemState,
    },
}

fn validate_workshop_download_preflight_items(
    items: Vec<WorkshopDownloadPreflightItem>,
    limits: WorkshopDownloadPreflightLimits,
) -> Result<WorkshopDownloadPreflight, WorkshopDownloadPreflightError> {
    let mut total_file_size = 0_u64;
    for item in &items {
        if item.file_type == FileType::Collection {
            return Err(WorkshopDownloadPreflightError::CollectionItem(item.id));
        }

        if !item.queues_steam_download() {
            return Err(WorkshopDownloadPreflightError::AlreadyInstalled {
                id: item.id,
                state: item.state,
            });
        }

        if item.file_size == 0 {
            return Err(WorkshopDownloadPreflightError::UnknownFileSize(item.id));
        }

        if item.file_size > limits.max_item_bytes {
            return Err(WorkshopDownloadPreflightError::ItemTooLarge {
                id: item.id,
                file_size: item.file_size,
                max_item_bytes: limits.max_item_bytes,
            });
        }

        total_file_size = total_file_size.saturating_add(item.file_size);
    }

    if total_file_size > limits.max_total_bytes {
        return Err(WorkshopDownloadPreflightError::TotalTooLarge {
            total_file_size,
            max_total_bytes: limits.max_total_bytes,
        });
    }

    Ok(WorkshopDownloadPreflight {
        items,
        total_file_size,
        limits,
    })
}

#[doc(hidden)]
pub fn preflight_workshop_downloads(
    steam: &Arc<Steam>,
    ids: Vec<PublishedFileId>,
    limits: WorkshopDownloadPreflightLimits,
) -> Result<WorkshopDownloadPreflight, WorkshopDownloadPreflightError> {
    if ids.is_empty() {
        return Err(WorkshopDownloadPreflightError::EmptyIds);
    }
    if !steam.connected() {
        return Err(WorkshopDownloadPreflightError::SteamNotConnected);
    }

    let (result_tx, result_rx) = mpsc::channel();
    let query_ids = ids.clone();

    steam
        .client()
        .expect("just confirmed connected() above; the interface stays set once connected")
        .ugc()
        .query_items(ids)
        .map_err(|_| WorkshopDownloadPreflightError::QueryCreateFailed)?
        .fetch({
            // `UGC` wraps a raw pointer and is not `Send`; re-derive it fresh
            // inside the callback (run on the Steam callback pump thread)
            // rather than moving one in from here.
            let steam = Arc::clone(steam);
            move |results: Result<QueryResults<'_>, steamworks::SteamError>| {
                let ugc = steam
                    .client()
                    .expect("Steam UGC callbacks only fire from the connected callback pump")
                    .ugc();
                let result = match results {
                    Ok(results) => {
                        let mut items = Vec::with_capacity(query_ids.len());
                        for (index, expected) in query_ids.iter().copied().enumerate() {
                            let Some(item) = results.get(index as u32) else {
                                let _ = result_tx.send(Err(
                                    WorkshopDownloadPreflightError::MissingItem(expected),
                                ));
                                return;
                            };

                            if item.published_file_id != expected {
                                let _ = result_tx.send(Err(
                                    WorkshopDownloadPreflightError::QueryItemMismatch {
                                        expected,
                                        actual: item.published_file_id,
                                    },
                                ));
                                return;
                            }

                            let state = ugc.item_state(item.published_file_id);
                            items.push(WorkshopDownloadPreflightItem {
                                id: item.published_file_id,
                                file_type: item.file_type,
                                file_size: u64::from(item.file_size),
                                state,
                                has_install_info: ugc
                                    .item_install_info(item.published_file_id)
                                    .is_some(),
                            });
                        }
                        validate_workshop_download_preflight_items(items, limits)
                    }
                    Err(error) => Err(WorkshopDownloadPreflightError::QueryFailed(format!(
                        "{error:?}"
                    ))),
                };
                let _ = result_tx.send(result);
            }
        });

    let deadline = Instant::now() + limits.query_timeout;
    let timeout = deadline
        .checked_duration_since(Instant::now())
        .unwrap_or(Duration::ZERO);
    result_rx
        .recv_timeout(timeout)
        .unwrap_or(Err(WorkshopDownloadPreflightError::QueryTimedOut {
            timeout: limits.query_timeout,
        }))
}

#[doc(hidden)]
pub fn preflight_workshop_download_ids(
    steam: &Arc<Steam>,
    ids: Vec<u64>,
    limits: WorkshopDownloadPreflightLimits,
) -> Result<WorkshopDownloadPreflight, WorkshopDownloadPreflightError> {
    preflight_workshop_downloads(
        steam,
        ids.into_iter().map(PublishedFileId).collect(),
        limits,
    )
}

pub struct Downloads {
    pending: Mutex<Vec<Download>>,
    downloading: Mutex<Vec<Download>>,
    watchdog: Condvar,
    /// Bumped by [`Self::cancel_all`]. Submission batches capture the epoch
    /// on entry and stop queueing new downloads once it moves, so a
    /// cancel-all also covers items still resolving (Web API preflight,
    /// collection expansion) that have no transaction to abort yet.
    cancel_epoch: AtomicU64,
    app_data: Arc<AppData>,
    steam: Arc<Steam>,
    whitelist: AddonWhitelist,
    transactions: Transactions,
}
impl Downloads {
    #[must_use]
    pub fn new(
        app_data: Arc<AppData>,
        steam: Arc<Steam>,
        whitelist: AddonWhitelist,
        transactions: Transactions,
    ) -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
            downloading: Mutex::new(Vec::new()),
            watchdog: Condvar::new(),
            cancel_epoch: AtomicU64::new(0),
            app_data,
            steam,
            whitelist,
            transactions,
        }
    }

    /// Stops in-flight submission batches from queueing any further
    /// downloads. Already-queued items are cancelled through their
    /// individual transactions, not here.
    pub fn cancel_all(&self) {
        self.cancel_epoch.fetch_add(1, Ordering::SeqCst);
    }

    fn cancelled_since(&self, epoch: u64) -> bool {
        self.cancel_epoch.load(Ordering::SeqCst) != epoch
    }

    fn extract(
        self: &Arc<Self>,
        folder: PathBuf,
        item: PublishedFileId,
        extract_destination: ExtractDestination,
    ) {
        self.extract_with_cleanup(folder, item, extract_destination, None);
    }

    fn extract_with_cleanup(
        self: &Arc<Self>,
        folder: PathBuf,
        item: PublishedFileId,
        extract_destination: ExtractDestination,
        temp_guard: Option<tempfile::TempPath>,
    ) {
        let downloads = Arc::clone(self);
        THREAD_POOL.spawn(move || {
            // Keeps a downloaded temp payload alive until extraction is done.
            let _temp_guard = temp_guard;

            let transaction = downloads.transactions.begin();
            transaction.status("locating");

            // Installed workshop content keeps its .gma on disk after
            // extraction, so the started event advertises it as a
            // previewable source. Temp payloads (the .bin branch below) are
            // deleted once extraction finishes, so they stay anonymous.
            let source_gma = if folder.is_dir() {
                unique_gma_in_dir(&folder)
            } else {
                None
            };

            downloads
                .transactions
                .emit(BackendEvent::ExtractionStarted(ExtractionStartedEvent {
                    transaction_id: transaction.id,
                    source_path: source_gma.clone(),
                    file_name: None,
                    workshop_id: Some(item),
                }));

            let open_on_disk = |path: PathBuf| -> Result<(GMAFile, GmaView), GMAError> {
                let gma = GMAFile::open(path)?;
                let view = gma.view()?;
                Ok((gma, view))
            };

            let (mut gma, view) = if folder.is_dir() {
                if let Some(path) = source_gma {
                    match open_on_disk(path) {
                        Ok(gma) => gma,
                        Err(err) => return transaction.error(&err),
                    }
                } else {
                    return transaction.error(crate::error_key::keys::DOWNLOAD_MISSING);
                }
            } else if folder.is_file() && crate::path::has_extension(&folder, "bin") {
                if let Ok(gma) = open_on_disk(folder.clone()) {
                    gma
                } else {
                    transaction.status("decompressing");
                    match GMAFile::decompress(
                        folder,
                        &transaction,
                        &downloads.app_data,
                        &downloads.steam,
                    ) {
                        Ok(gma) => {
                            transaction.progress_reset();
                            gma
                        }
                        Err(err) => return transaction.error(&err),
                    }
                }
            } else {
                return transaction.error(crate::error_key::keys::DOWNLOAD_MISSING);
            };

            gma.id = Some(item);

            transaction.status("reading_metadata");
            transaction.data(crate::transactions::TransactionPayload::ByteSize {
                source: Some(gma.metadata.title().to_owned()),
                bytes: gma.size,
            });

            if let Err(err) = view.extract(
                &gma,
                extract_destination,
                &transaction,
                ExtractOptions {
                    open_after: false,
                    whitelist: Whitelist::Ignore,
                },
                &downloads.whitelist,
                &downloads.app_data,
                &downloads.steam,
            ) {
                transaction.error(&err);
            }
        });
    }

    fn push_download(
        self: &Arc<Self>,
        ugc: &UGC,
        pending: &mut MutexGuard<Vec<Arc<DownloadInner>>>,
        extract_destination: &Arc<ExtractDestination>,
        item: PublishedFileId,
        epoch: u64,
    ) {
        if self.cancelled_since(epoch) {
            return;
        }

        let state = ugc.item_state(item);
        if state.intersects(ItemState::INSTALLED) && !state.intersects(ItemState::NEEDS_UPDATE) {
            if let Some(info) = ugc.item_install_info(item) {
                self.extract(
                    PathBuf::from(info.folder),
                    item,
                    (**extract_destination).clone(),
                );
            } else {
                let transaction = self.transactions.begin();
                self.transactions
                    .emit(BackendEvent::DownloadStarted(DownloadStartedEvent {
                        transaction_id: transaction.id,
                    }));
                transaction.data(crate::transactions::TransactionPayload::WorkshopItem(item));
                transaction.error(crate::error_key::keys::DOWNLOAD_MISSING);
            }
        } else {
            let download = Arc::new(DownloadInner {
                item,
                sent_total: AtomicBool::new(false),
                transaction: self.transactions.begin(),
                extract_destination: (**extract_destination).clone(),
            });

            self.transactions
                .emit(BackendEvent::DownloadStarted(DownloadStartedEvent {
                    transaction_id: download.transaction.id,
                }));
            download
                .transaction
                .data(crate::transactions::TransactionPayload::WorkshopItem(item));

            pending.push(download);
        }
    }

    /// Emits the failed-download row shown when Steam does not know an item.
    fn missing_item(&self, item: PublishedFileId, epoch: u64) {
        if self.cancelled_since(epoch) {
            return;
        }

        let transaction = self.transactions.begin();
        self.transactions
            .emit(BackendEvent::DownloadStarted(DownloadStartedEvent {
                transaction_id: transaction.id,
            }));
        transaction.data(crate::transactions::TransactionPayload::WorkshopItem(item));
        transaction.error(crate::error_key::keys::ITEM_NOT_FOUND);
    }

    pub fn download(self: &Arc<Self>, ids: impl IntoIterator<Item = PublishedFileId>) {
        let ids: Vec<PublishedFileId> = ids.into_iter().collect();
        if ids.is_empty() {
            return;
        }
        let epoch = self.cancel_epoch.load(Ordering::SeqCst);
        let extract_destination = Arc::new(self.app_data.extract_destination_snapshot());

        // Web API preflight: resolves collections and legacy CDN URLs in a
        // few batched keyless calls, with no side effects on failure —
        // items with a public `file_url` then bypass the Steam client's
        // serial download queue entirely.
        let (known_items, possible_collections) = {
            let workshop = self
                .steam
                .workshop_dedup
                .try_lock_for(CALLBACK_PUMP_INTERVAL + Duration::from_millis(1));
            workshop.as_deref().map_or_else(
                || (Vec::new(), ids.clone()),
                |cache| ids.iter().partition(|id| cache.contains(id)),
            )
        };
        match webapi::resolve_downloads(known_items, possible_collections) {
            Ok(details) => {
                self.dispatch_preflighted(details, &extract_destination, epoch);
                return;
            }
            Err(error) => log::warn!(
                "Workshop Web API preflight failed, falling back to Steamworks queries: {error}"
            ),
        }

        self.download_via_steamworks(ids, &extract_destination, epoch);
    }

    /// Routes preflighted items to the cheapest lane: already-installed
    /// content extracts from disk, legacy items download in parallel over
    /// HTTPS, SteamPipe items queue on the Steam client.
    fn dispatch_preflighted(
        self: &Arc<Self>,
        details: Vec<webapi::PublishedFileDetail>,
        extract_destination: &Arc<ExtractDestination>,
        epoch: u64,
    ) {
        self.steam.fetch_workshop_items(
            details
                .iter()
                .filter(|detail| detail.found)
                .map(|detail| detail.id)
                .collect(),
        );

        let ugc = self
            .steam
            .client()
            .expect("download() is only reached once the app-layer connected check has passed")
            .ugc();
        let mut queued_steam_downloads = false;
        // The keyless Web API cannot see private/friends-only items (e.g.
        // the user's own unlisted addons); let the authenticated Steamworks
        // lane decide whether they are truly missing.
        let mut unresolved = Vec::new();
        for detail in details {
            if !detail.found {
                unresolved.push(detail.id);
                continue;
            }

            let state = ugc.item_state(detail.id);
            let extract_installed = state.intersects(ItemState::INSTALLED)
                && !state.intersects(ItemState::NEEDS_UPDATE)
                && ugc.item_install_info(detail.id).is_some();

            match detail.file_url {
                Some(url) if !extract_installed => {
                    self.queue_http_download(
                        detail.id,
                        url,
                        detail.file_size,
                        extract_destination,
                        epoch,
                    );
                }
                _ => {
                    let mut pending = self.pending.lock();
                    self.push_download(&ugc, &mut pending, extract_destination, detail.id, epoch);
                    queued_steam_downloads |= !pending.is_empty();
                }
            }
        }

        if queued_steam_downloads {
            self.start();
        }

        if !unresolved.is_empty() {
            self.download_via_steamworks(unresolved, extract_destination, epoch);
        }
    }

    fn queue_http_download(
        self: &Arc<Self>,
        item: PublishedFileId,
        url: String,
        file_size: u64,
        extract_destination: &Arc<ExtractDestination>,
        epoch: u64,
    ) {
        if self.cancelled_since(epoch) {
            return;
        }

        let transaction = self.transactions.begin();
        self.transactions
            .emit(BackendEvent::DownloadStarted(DownloadStartedEvent {
                transaction_id: transaction.id,
            }));
        transaction.data(crate::transactions::TransactionPayload::WorkshopItem(item));

        let downloads = Arc::clone(self);
        let extract_destination = (**extract_destination).clone();
        HTTP_DOWNLOAD_POOL.spawn(move || {
            match downloads.http_download(&url, file_size, &transaction) {
                Ok(Some(temp_path)) => {
                    log::info!("Legacy CDN download SUCCESS: {item:?}");
                    transaction.finished(crate::transactions::TransactionPayload::None);
                    downloads.extract_with_cleanup(
                        temp_path.to_path_buf(),
                        item,
                        extract_destination,
                        Some(temp_path),
                    );
                }
                Ok(None) => {} // aborted by the user; the row is already gone
                Err(error) => {
                    log::error!("Legacy CDN download ERROR for {item:?}: {error}");
                    transaction.error(crate::transactions::TransactionError::detailed(
                        crate::error_key::keys::DOWNLOAD_FAILED,
                        crate::transactions::detail_from_serialize(error.to_string()),
                    ));
                }
            }
        });
    }

    /// Streams one legacy CDN payload to a temp `.bin`. Returns `Ok(None)`
    /// when the transaction was aborted mid-transfer.
    fn http_download(
        &self,
        url: &str,
        expected_size: u64,
        transaction: &crate::transactions::Transaction,
    ) -> Result<Option<tempfile::TempPath>, std::io::Error> {
        let temp_dir = self
            .app_data
            .extraction_context(&self.steam, false)
            .temp_dir;
        let temp_file = std::fs::create_dir_all(&temp_dir)
            .ok()
            .and_then(|()| {
                tempfile::Builder::new()
                    .prefix("gmpublisher_download")
                    .suffix(".bin")
                    .tempfile_in(&temp_dir)
                    .ok()
            })
            .map_or_else(
                || {
                    tempfile::Builder::new()
                        .prefix("gmpublisher_download")
                        .suffix(".bin")
                        .tempfile()
                },
                Ok,
            )?;
        let (file, temp_path) = temp_file.into_parts();

        let mut response = webapi::download_agent()
            .get(url)
            .call()
            .map_err(std::io::Error::other)?;
        let total = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(expected_size);
        if total > 0 {
            transaction.data(crate::transactions::TransactionPayload::TotalBytes(total));
        }

        let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();
        let mut writer = std::io::BufWriter::new(file);
        let mut buffer = vec![0u8; 64 * 1024];
        let mut written = 0u64;
        let mut last_progress = 0.0f64;
        loop {
            if transaction.aborted() {
                return Ok(None);
            }
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            writer.write_all(&buffer[..n])?;
            written += n as u64;
            if total > 0 {
                let progress = written as f64 / total as f64;
                if progress - last_progress >= HTTP_PROGRESS_STEP {
                    last_progress = progress;
                    transaction.progress(progress);
                }
            }
        }
        writer.flush()?;

        Ok(Some(temp_path))
    }

    // The pending guard is deliberately held across the whole batch append
    // so a concurrent download() cannot interleave mid-batch.
    #[expect(clippy::significant_drop_tightening)]
    fn download_via_steamworks(
        self: &Arc<Self>,
        mut ids: Vec<PublishedFileId>,
        extract_destination: &Arc<ExtractDestination>,
        epoch: u64,
    ) {
        let possible_collections: Arc<Mutex<PossibleCollectionsState>> = Arc::new(Mutex::new({
            let workshop = self
                .steam
                .workshop_dedup
                .try_lock_for(CALLBACK_PUMP_INTERVAL + Duration::from_millis(1));
            PossibleCollectionsState::split_initial(&mut ids, workshop.as_deref())
        }));

        loop {
            let possible_collections_query;
            {
                let Some(query) = possible_collections
                    .lock()
                    .next_query(self.steam.connected())
                else {
                    break;
                };
                possible_collections_query = query;
            }

            let extract_destination = extract_destination.clone();
            let possible_collections = possible_collections.clone();

            let query = match self
                .steam
                .client()
                .expect("this loop only reaches here when next_query() saw connected() true")
                .ugc()
                .query_items(possible_collections_query.clone())
            {
                Ok(query) => query,
                Err(error) => {
                    // Steam refused to create the query; surface it as a failed
                    // download task instead of aborting the process.
                    let transaction = self.transactions.begin();
                    self.transactions
                        .emit(BackendEvent::DownloadStarted(DownloadStartedEvent {
                            transaction_id: transaction.id,
                        }));
                    transaction.error(crate::transactions::TransactionError::detailed(
                        crate::error_key::keys::STEAM_ERROR,
                        crate::transactions::detail_from_serialize(error.to_string()),
                    ));
                    break;
                }
            };

            let (done_tx, done_rx) = mpsc::channel();
            let downloads = Arc::clone(self);
            query
                .include_children(true)
                .fetch(
                    move |results: Result<QueryResults<'_>, steamworks::SteamError>| {
                        if let Ok(results) = results {
                            let mut pending = downloads.pending.lock();
                            pending.reserve(results.returned_results() as usize);

                            let mut query_results =
                                Vec::with_capacity(results.returned_results() as usize);
                            for (i, item) in results.iter().enumerate() {
                                query_results.push(if let Some(item) = item {
                                    if item.file_type == steamworks::FileType::Collection {
                                        Some(WorkshopDownloadQueryItem::Collection {
                                            children: results
                                                .get_children(i as u32)
                                                .unwrap_or_else(|| {
                                                    log::warn!(
                                                        "Steam returned a collection with unreadable children (result index {i}); treating it as empty"
                                                    );
                                                    Vec::new()
                                                }),
                                        })
                                    } else {
                                        Some(WorkshopDownloadQueryItem::Item(
                                            item.published_file_id,
                                        ))
                                    }
                                } else {
                                    None
                                });
                            }

                            let actions = possible_collections
                                .lock()
                                .apply_query_results(&possible_collections_query, query_results);
                            let ugc = downloads
                                .steam
                                .client()
                                .expect("Steam UGC callbacks only fire from the connected callback pump")
                                .ugc();
                            for action in actions {
                                match action {
                                    WorkshopDownloadAction::FetchWorkshopItems(items) => {
                                        downloads.steam.fetch_workshop_items(items);
                                    }
                                    WorkshopDownloadAction::QueueDownload(item) => {
                                        downloads.push_download(
                                            &ugc,
                                            &mut pending,
                                            &extract_destination,
                                            item,
                                            epoch,
                                        );
                                    }
                                    WorkshopDownloadAction::MissingItem(item) => {
                                        downloads.missing_item(item, epoch);
                                    }
                                }
                            }
                        }

                        let _ = done_tx.send(());
                    },
                );

            let _ = done_rx.recv();
        }

        let mut pending = self.pending.lock();
        pending.reserve(ids.len());

        let ugc = self
            .steam
            .client()
            .expect("download_via_steamworks is only reached once Steam has connected")
            .ugc();
        for item in ids {
            self.push_download(&ugc, &mut pending, extract_destination, item, epoch);
        }

        if !pending.is_empty() {
            drop(pending);
            self.start();
        }
    }

    pub fn start(&self) {
        let mut downloading = self.downloading.lock();
        append_pending_batch(&mut downloading, &mut self.pending.lock());

        self.watchdog.notify_one();
    }

    // Condvar pairing in the drain loop: the guard is handed to wait_for.
    #[expect(clippy::significant_drop_tightening)]
    pub(super) fn watchdog(downloads: &Arc<Self>, steam: &Arc<Steam>) {
        let in_progress_state: Arc<(Mutex<BTreeMap<PublishedFileId, Download>>, Condvar)> =
            Arc::new((Mutex::new(BTreeMap::new()), Condvar::new()));
        let in_progress_ref = in_progress_state.clone();
        let downloads_for_callback = Arc::clone(downloads);
        let steam_for_callback = Arc::clone(steam);
        let _cb = steam.register_callback(move |result: steamworks::DownloadItemResult| {
            if result.app_id == GMOD_APP_ID {
                let mut in_progress = in_progress_ref.0.lock();
                if let Some(download) = in_progress.remove(&result.published_file_id) {
                    if let Some(error) = result.error {
                        log::error!("ISteamUGC Download ERROR: {:?}", download.item);
                        download.transaction.error(
                            crate::transactions::TransactionError::detailed(
                                crate::error_key::keys::STEAM_ERROR,
                                crate::transactions::detail_from_serialize(error),
                            ),
                        );
                    } else if let Some(info) = steam_for_callback
                        .client()
                        .expect("Steam UGC callbacks only fire from the connected callback pump")
                        .ugc()
                        .item_install_info(result.published_file_id)
                    {
                        log::info!("ISteamUGC Download SUCCESS: {:?}", download.item);
                        let extract_destination = download.extract_destination.clone();
                        download
                            .transaction
                            .finished(crate::transactions::TransactionPayload::None);
                        downloads_for_callback.extract(
                            PathBuf::from(info.folder),
                            download.item,
                            extract_destination,
                        );
                    } else {
                        log::error!("ISteamUGC Download MISSING: {:?}", download.item);
                        download
                            .transaction
                            .error(crate::error_key::keys::DOWNLOAD_MISSING);
                    }
                } else {
                    log::warn!("ISteamUGC Download ???: {:?}", result.published_file_id);
                }
                drop(in_progress);
                in_progress_ref.1.notify_all();
            }
        });

        // The Steam client downloads workshop items serially, and every
        // high-priority DownloadItem call preempts the in-flight one — so
        // items are fed one at a time: high priority (which also overrides
        // the client's "pause downloads during gameplay" policy, active
        // because this process runs as appid 4000) without ever preempting
        // our own queue.
        let mut queue: VecDeque<Download> = VecDeque::new();
        loop {
            queue.extend(std::mem::take(&mut *downloads.downloading.lock()));

            let mut in_progress = in_progress_state.0.lock();

            if in_progress.is_empty() {
                let Some(download) = queue.pop_front() else {
                    drop(in_progress);
                    let mut downloading = downloads.downloading.lock();
                    if downloading.is_empty() {
                        downloads.watchdog.wait(&mut downloading);
                    }
                    continue;
                };

                if download.transaction.aborted() {
                    continue;
                }

                let download_started = steam
                    .client()
                    .expect("Downloads::watchdog only runs after Steam has connected")
                    .ugc()
                    .download_item(download.item, true);
                if !download_started {
                    download
                        .transaction
                        .error(crate::error_key::keys::DOWNLOAD_FAILED);
                    continue;
                }
                log::info!("Starting ISteamUGC Download for {:?}", download.item);

                in_progress.insert(download.item, download);
            }

            let ugc = steam
                .client()
                .expect("Downloads::watchdog only runs after Steam has connected")
                .ugc();
            in_progress.retain(|_, download| {
                // ISteamUGC has no per-item cancel: dropping our tracking is
                // all a cancel can do here — the Steam client finishes the
                // transfer in the background, and the completion callback
                // above ignores untracked items instead of extracting them.
                if download.transaction.aborted() {
                    return false;
                }

                if let Some((current, total)) = ugc.item_download_info(download.item)
                    && total > 0
                {
                    if !download
                        .sent_total
                        .fetch_or(true, std::sync::atomic::Ordering::SeqCst)
                    {
                        download
                            .transaction
                            .data(crate::transactions::TransactionPayload::TotalBytes(total));
                    }
                    download.transaction.progress(current as f64 / total as f64);
                }

                true
            });

            if in_progress.is_empty() {
                // Feed the next queued item immediately.
                continue;
            }

            in_progress_state
                .1
                .wait_for(&mut in_progress, CALLBACK_PUMP_INTERVAL);
        }
    }
}

pub fn queue_workshop_downloads(
    downloads: &Arc<Downloads>,
    ids: impl IntoIterator<Item = PublishedFileId>,
) {
    downloads.download(ids);
}

/// The folder's single `.gma` payload; `None` when absent or ambiguous
/// (multiple `.gma` files mean we'd be guessing which one to use).
fn unique_gma_in_dir(folder: &std::path::Path) -> Option<PathBuf> {
    let mut gma_path = None;
    for entry in folder.read_dir().ok()?.flatten() {
        if !crate::path::has_extension(entry.path(), "gma") {
            continue;
        }
        if gma_path.is_some() {
            return None;
        }
        gma_path = Some(entry.path());
    }
    gma_path
}

mod webapi;

#[cfg(test)]
mod tests;
