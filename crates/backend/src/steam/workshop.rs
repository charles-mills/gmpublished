use std::{
    collections::HashSet,
    fmt,
    path::PathBuf,
    sync::Arc,
    sync::mpsc,
    time::{Duration, Instant},
};

use steamworks::{QueryResult, QueryResults, SteamError, SteamId};

use crate::WorkshopId;

use super::{CALLBACK_RESULT_TIMEOUT, ConnectedSteam, Steam, users::SteamUser};

use crate::util::main_thread_forbidden;
use crate::{STEAM_GMOD_APP_ID, search::Search};

type WorkshopChunkQueryResult = Result<Vec<WorkshopItem>, WorkshopQueryError>;
const WORKSHOP_DEDUP_LOCK_TIMEOUT: Duration = Duration::from_millis(51);
const BACKGROUND_CALLBACK_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DescriptionLength {
    Summary,
    Full,
}

impl DescriptionLength {
    const fn returns_full_description(self) -> bool {
        matches!(self, Self::Full)
    }
}

#[derive(Clone, Debug)]
pub struct WorkshopItem {
    pub id: WorkshopId,
    pub title: String,
    pub owner: Option<SteamUser>,
    pub time_created: u32,
    pub time_updated: u32,
    pub description: Option<String>,
    pub score: f32,
    pub tags: Vec<String>,
    pub preview_url: Option<String>,
    pub subscriptions: u64,
    pub local_file: Option<PathBuf>,
    pub steamid: Option<SteamId>,

    pub dead: bool,
}

#[derive(Clone, Debug)]
pub struct WorkshopPage {
    pub total_results: u32,
    pub items: Vec<WorkshopItem>,
}

impl TryFrom<QueryResult> for WorkshopItem {
    type Error = crate::workshop_id::ZeroWorkshopId;

    fn try_from(result: QueryResult) -> Result<Self, Self::Error> {
        Ok(Self {
            id: WorkshopId::try_from(result.published_file_id)?,
            title: result.title.clone(),
            steamid: Some(result.owner),
            owner: None,
            time_created: result.time_created,
            time_updated: result.time_updated,
            description: Some(result.description),
            score: result.score,
            tags: result.tags,
            preview_url: None,
            subscriptions: 0,
            local_file: None,
            dead: false,
        })
    }
}
impl From<WorkshopId> for WorkshopItem {
    fn from(id: WorkshopId) -> Self {
        Self {
            id,
            title: id.get().to_string(),
            steamid: None,
            owner: None,
            time_created: 0,
            time_updated: 0,
            description: None,
            score: 0.,
            tags: Vec::new(),
            preview_url: None,
            subscriptions: 0,
            local_file: None,
            dead: true,
        }
    }
}
fn enrich_workshop_item(
    item: QueryResult,
    index: u32,
    results: &QueryResults<'_>,
) -> Option<WorkshopItem> {
    let mut item = WorkshopItem::try_from(item).ok()?;
    item.preview_url = results.preview_url(index);
    item.subscriptions = results
        .statistic(index, steamworks::UGCStatisticType::Subscriptions)
        .unwrap_or(0);
    Some(item)
}

fn format_steam_query_error(error: &str, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    if error.is_empty() {
        formatter.write_str("ERR_STEAM_ERROR")
    } else {
        write!(formatter, "ERR_STEAM_ERROR:{error}")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WorkshopQueryError {
    #[error("could not create a Workshop query")]
    QueryCreateFailed,
    /// No callback arrived before the deadline. Self::Steam means Steam did
    /// respond, but returned an error.
    #[error("Steam did not answer the Workshop query")]
    Abandoned,
    #[error(fmt = format_steam_query_error)]
    Steam(String),
}

impl crate::error_key::HasErrorKey for WorkshopQueryError {
    fn error_key(&self) -> crate::error_key::ErrorKey {
        crate::error_key::keys::STEAM_ERROR
    }

    fn error_detail(&self) -> Option<String> {
        match self {
            Self::QueryCreateFailed => Some("QUERY_CREATE_FAILED".to_owned()),
            Self::Abandoned => Some("CALLBACK_ABANDONED".to_owned()),
            Self::Steam(error) => Some(error.clone()),
        }
    }
}

impl Steam {
    pub(super) fn workshop_fetcher(
        steam: &Arc<Self>,
        search: &Arc<Search>,
        client: &steamworks::Client,
        shutdown: &crate::signal::Signal,
    ) {
        loop {
            let rx = steam.workshop_queue_rx.lock();
            let Ok(mut queue) = rx.recv() else {
                return;
            };

            while let Ok(mut next) = rx.try_recv() {
                queue.append(&mut next);
            }
            drop(rx);

            // Shutdown sends an empty batch to wake `recv`. The sender stays alive
            // through this thread's own `Arc<Steam>`, so it cannot disconnect itself.
            if shutdown.is_set() {
                return;
            }

            while !queue.is_empty() {
                let chunk_len = super::RESULTS_PER_PAGE.min(queue.len());
                let chunk = queue.drain(..chunk_len).collect::<Vec<_>>();
                let chunk_for_callback = chunk.clone();
                let language = steam.workshop_language();
                let (done_tx, done_rx) = mpsc::channel();

                search.reserve(chunk.len());

                if let Ok(query) = client
                    .ugc()
                    .query_items(chunk.into_iter().map(Into::into).collect())
                {
                    let steam_for_callback = Arc::clone(steam);
                    let search_for_callback = Arc::clone(search);
                    query.language(language.api_code()).allow_cached_response(600).fetch(
                        move |results: Result<QueryResults<'_>, SteamError>| {
                            if steam_for_callback.workshop_language() != language {
                                let _ = done_tx.send(());
                                return;
                            }
                            if let Ok(results) = results {
                                let items = results
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(i, item)| {
                                        item.and_then(|item| {
                                            enrich_workshop_item(item, i as u32, &results)
                                        })
                                    })
                                    .collect::<Vec<_>>();

                                search_for_callback.refresh_installed_addon_labels(&items);
                            } else {
                                log::warn!(
                                    "workshop enrichment query failed for a chunk; leaving its items unenriched"
                                );
                                let mut dedup = steam_for_callback.workshop_dedup.lock();
                                for id in chunk_for_callback.into_iter() {
                                    dedup.remove(&id);
                                }
                            }
                            let _ = done_tx.send(());
                        },
                    );
                } else {
                    log::warn!(
                        "workshop enrichment query failed to create for a chunk; leaving its items unenriched"
                    );
                    let mut dedup = steam.workshop_dedup.lock();
                    for id in chunk_for_callback.into_iter() {
                        dedup.remove(&id);
                    }
                    drop(dedup);
                    let _ = done_tx.send(());
                }

                if !wait_for_background_callback(&done_rx, shutdown) {
                    return;
                }
            }
        }
    }

    pub(super) fn wake_workshop_fetcher(&self) {
        let _ = self.workshop_queue_tx.send(Vec::new());
    }

    /// Returns `None` if the cache is busy. `use_cache` runs under its lock;
    /// keep it O(ids) with no I/O.
    pub(crate) fn with_known_workshop_items<R>(
        &self,
        use_cache: impl FnOnce(Option<&HashSet<WorkshopId>>) -> R,
    ) -> R {
        let cache = self
            .workshop_dedup
            .try_lock_for(WORKSHOP_DEDUP_LOCK_TIMEOUT);
        use_cache(cache.as_deref())
    }

    pub fn fetch_workshop_items(&self, ids: Vec<WorkshopId>) {
        let ids = filter_new_workshop_ids(&mut self.workshop_dedup.lock(), ids);
        if !ids.is_empty() {
            let _ = self.workshop_queue_tx.send(ids);
        }
    }

    pub fn browse_my_workshop_page(
        &self,
        page: u32,
        search: &Arc<Search>,
        language: crate::WorkshopLanguage,
    ) -> Option<WorkshopPage> {
        self.browse_user_workshop_page(
            steamworks::UserList::Published,
            steamworks::UserListOrder::LastUpdatedDesc,
            page,
            Some(Arc::clone(search)),
            language,
        )
    }

    pub(crate) fn browse_user_workshop_page(
        &self,
        user_list: steamworks::UserList,
        user_list_order: steamworks::UserListOrder,
        page: u32,
        index_into: Option<Arc<Search>>,
        language: crate::WorkshopLanguage,
    ) -> Option<WorkshopPage> {
        let (tx, rx) = mpsc::sync_channel(1);

        let client = self.client().ok()?;
        client
            .client()
            .ugc()
            .query_user(
                client.steam_id.account_id(),
                user_list,
                steamworks::UGCType::ItemsReadyToUse,
                user_list_order,
                steamworks::AppIDs::ConsumerAppId(STEAM_GMOD_APP_ID),
                page,
            )
            .ok()?
            .language(language.api_code())
            .require_tag("addon")
            .fetch(move |result: Result<QueryResults<'_>, SteamError>| {
                let page = result.ok().map(|data| WorkshopPage {
                    total_results: data.total_results(),
                    items: data
                        .iter()
                        .enumerate()
                        .filter_map(|(i, x)| {
                            let Some(x) = x else {
                                log::debug!(
                                    "workshop page query returned no data for result index {i}"
                                );
                                return None;
                            };
                            let item = enrich_workshop_item(x, i as u32, &data)?;
                            Some(item)
                        })
                        .collect(),
                });
                let _ = tx.send(page);
            });

        let page = rx.recv_timeout(CALLBACK_RESULT_TIMEOUT).ok().flatten();
        let current_language = self.workshop_language.read();
        if *current_language == language
            && let Some(page) = page.as_ref()
            && let Some(search) = index_into
        {
            for item in &page.items {
                search.add(item);
            }
        }
        drop(current_language);
        page
    }
}

/// Waits for the metadata callback without making runtime shutdown pay the
/// callback's full deadline. `false` means shutdown won the race.
fn wait_for_background_callback(
    done: &mpsc::Receiver<()>,
    shutdown: &crate::signal::Signal,
) -> bool {
    let deadline = Instant::now() + CALLBACK_RESULT_TIMEOUT;
    loop {
        if shutdown.is_set() {
            return false;
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return true;
        };
        match done.recv_timeout(remaining.min(BACKGROUND_CALLBACK_POLL_INTERVAL)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn workshop_item_id_chunks(ids: &[WorkshopId]) -> Vec<Vec<WorkshopId>> {
    ids.chunks(super::RESULTS_PER_PAGE.max(1))
        .map(<[WorkshopId]>::to_vec)
        .collect()
}

fn filter_new_workshop_ids(
    cache: &mut HashSet<WorkshopId>,
    ids: Vec<WorkshopId>,
) -> Vec<WorkshopId> {
    ids.into_iter().filter(|id| cache.insert(*id)).collect()
}

fn query_results_to_workshop_items(
    ids: &[WorkshopId],
    results: Result<QueryResults<'_>, SteamError>,
) -> WorkshopChunkQueryResult {
    results
        .map(|results| {
            results
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    item.and_then(|item| enrich_workshop_item(item, i as u32, &results))
                        .unwrap_or_else(|| WorkshopItem::from(ids[i]))
                })
                .collect()
        })
        .map_err(|error| WorkshopQueryError::Steam(format!("{error:?}")))
}

fn combine_workshop_chunk_results(
    chunk_results: Vec<WorkshopChunkQueryResult>,
    item_capacity: usize,
) -> Result<Vec<WorkshopItem>, WorkshopQueryError> {
    let mut items = Vec::with_capacity(item_capacity);
    let mut any_chunk_succeeded = false;
    let mut last_error = None;

    for result in chunk_results {
        match result {
            Ok(mut chunk_items) => {
                any_chunk_succeeded = true;
                items.append(&mut chunk_items);
            }
            Err(error) => {
                last_error = Some(error);
            }
        }
    }

    if any_chunk_succeeded {
        return Ok(items);
    }

    Err(last_error.unwrap_or(WorkshopQueryError::QueryCreateFailed))
}

/// Waits until the shared fan-out deadline. Dropped senders and expired
/// deadlines both return `None`; Steam may discard callbacks without invoking them.
fn recv_chunk_by(
    results: &mpsc::Receiver<(usize, WorkshopChunkQueryResult)>,
    deadline: Instant,
) -> Option<(usize, WorkshopChunkQueryResult)> {
    results
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()
}

/// Drains chunk query results as they arrive, handing each successful chunk
/// to `on_chunk` immediately for incremental hydration. A failed chunk is
/// logged and skipped so its ids stay stale for a later refresh; the call
/// only errors when every chunk failed.
fn drain_workshop_chunk_results(
    results: &mpsc::Receiver<(usize, WorkshopChunkQueryResult)>,
    chunk_count: usize,
    deadline: Instant,
    mut on_chunk: impl FnMut(Vec<WorkshopItem>),
) -> Result<(), WorkshopQueryError> {
    let mut any_chunk_succeeded = false;
    let mut last_error = None;

    for _ in 0..chunk_count {
        let Some((_chunk_index, result)) = recv_chunk_by(results, deadline) else {
            log::warn!("Steam abandoned a workshop metadata chunk query; leaving its ids stale");
            last_error = Some(WorkshopQueryError::Abandoned);
            break;
        };
        match result {
            Ok(items) => {
                any_chunk_succeeded = true;
                on_chunk(items);
            }
            Err(error) => {
                log::warn!("workshop metadata chunk query failed; leaving its ids stale: {error}");
                last_error = Some(error);
            }
        }
    }

    if any_chunk_succeeded {
        Ok(())
    } else {
        Err(last_error.unwrap_or(WorkshopQueryError::QueryCreateFailed))
    }
}

impl ConnectedSteam<'_> {
    pub fn query_workshop_items(
        &self,
        ids: &[WorkshopId],
    ) -> Result<Vec<WorkshopItem>, WorkshopQueryError> {
        self.query_workshop_items_with_description(ids, DescriptionLength::Summary)
    }

    pub fn query_workshop_item_details(
        &self,
        id: WorkshopId,
    ) -> Result<WorkshopItem, WorkshopQueryError> {
        self.query_workshop_items_with_description(&[id], DescriptionLength::Full)
            .map(|mut items| items.pop().unwrap_or_else(|| WorkshopItem::from(id)))
    }

    pub fn query_localized_item_details(
        &self,
        id: WorkshopId,
        language: crate::WorkshopLanguage,
    ) -> Result<WorkshopItem, WorkshopQueryError> {
        main_thread_forbidden!();
        let (tx, rx) = mpsc::channel();
        let query = self
            .interface
            .client()
            .ugc()
            .query_items(vec![id.into()])
            .map_err(|_| WorkshopQueryError::QueryCreateFailed)?;
        query
            .language(language.api_code())
            .set_return_long_description(true)
            .allow_cached_response(0)
            .fetch(move |result| {
                let _sent = tx.send(query_results_to_workshop_items(&[id], result));
            });
        let items = rx
            .recv_timeout(CALLBACK_RESULT_TIMEOUT)
            .map_err(|_| WorkshopQueryError::Abandoned)??;
        items
            .into_iter()
            .find(|item| !item.dead)
            .ok_or(WorkshopQueryError::QueryCreateFailed)
    }

    fn query_workshop_items_with_description(
        &self,
        ids: &[WorkshopId],
        description_length: DescriptionLength,
    ) -> Result<Vec<WorkshopItem>, WorkshopQueryError> {
        main_thread_forbidden!();

        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let chunks = workshop_item_id_chunks(ids);
        let chunk_count = chunks.len();
        let (result_tx, result_rx) = mpsc::channel();

        for (chunk_index, chunk) in chunks.into_iter().enumerate() {
            self.register_workshop_items_chunk_query(
                chunk_index,
                chunk,
                description_length,
                result_tx.clone(),
            );
        }
        drop(result_tx);

        let deadline = Instant::now() + CALLBACK_RESULT_TIMEOUT;
        let mut chunk_results: Vec<Option<WorkshopChunkQueryResult>> = vec![None; chunk_count];
        for _ in 0..chunk_count {
            let Some((chunk_index, result)) = recv_chunk_by(&result_rx, deadline) else {
                break;
            };
            chunk_results[chunk_index] = Some(result);
        }
        let chunk_results = chunk_results
            .into_iter()
            .map(|result| result.unwrap_or(Err(WorkshopQueryError::Abandoned)))
            .collect();

        combine_workshop_chunk_results(chunk_results, ids.len())
    }

    /// Deliver chunks as they arrive. Skip failed chunks; return an error only if all fail.
    pub fn query_workshop_items_streaming(
        &self,
        ids: &[WorkshopId],
        on_chunk: impl FnMut(Vec<WorkshopItem>),
    ) -> Result<(), WorkshopQueryError> {
        main_thread_forbidden!();

        if ids.is_empty() {
            return Ok(());
        }

        let chunks = workshop_item_id_chunks(ids);
        let chunk_count = chunks.len();
        let (result_tx, result_rx) = mpsc::channel();

        for (chunk_index, chunk) in chunks.into_iter().enumerate() {
            self.register_workshop_items_chunk_query(
                chunk_index,
                chunk,
                DescriptionLength::Summary,
                result_tx.clone(),
            );
        }
        drop(result_tx);

        drain_workshop_chunk_results(
            &result_rx,
            chunk_count,
            Instant::now() + CALLBACK_RESULT_TIMEOUT,
            on_chunk,
        )
    }

    fn register_workshop_items_chunk_query(
        &self,
        chunk_index: usize,
        ids: Vec<WorkshopId>,
        description_length: DescriptionLength,
        result_tx: mpsc::Sender<(usize, WorkshopChunkQueryResult)>,
    ) {
        let query = self
            .interface
            .client()
            .ugc()
            .query_items(ids.iter().copied().map(Into::into).collect());

        match query {
            Ok(query) => {
                let query = if description_length.returns_full_description() {
                    query.set_return_long_description(true)
                } else {
                    query
                };
                query
                    .language(self.workshop_language.api_code())
                    .allow_cached_response(if description_length.returns_full_description() {
                        0
                    } else {
                        600
                    })
                    .fetch(move |results: Result<QueryResults<'_>, SteamError>| {
                        let _ = result_tx
                            .send((chunk_index, query_results_to_workshop_items(&ids, results)));
                    });
            }
            Err(_) => {
                let _ = result_tx.send((chunk_index, Err(WorkshopQueryError::QueryCreateFailed)));
            }
        }
    }
}

pub fn query_workshop_items(
    steam: ConnectedSteam<'_>,
    ids: &[WorkshopId],
) -> Result<Vec<WorkshopItem>, WorkshopQueryError> {
    steam.query_workshop_items(ids)
}

pub fn query_workshop_item_details(
    steam: ConnectedSteam<'_>,
    id: WorkshopId,
) -> Result<WorkshopItem, WorkshopQueryError> {
    steam.query_workshop_item_details(id)
}

pub fn query_workshop_items_streaming(
    steam: ConnectedSteam<'_>,
    ids: &[WorkshopId],
    on_chunk: impl FnMut(Vec<WorkshopItem>),
) -> Result<(), WorkshopQueryError> {
    steam.query_workshop_items_streaming(ids, on_chunk)
}

pub(crate) fn browse_my_workshop_page(
    steam: &Steam,
    search: &Arc<Search>,
    page: u32,
    language: crate::WorkshopLanguage,
) -> Option<WorkshopPage> {
    steam.client_wait(super::CLIENT_WAIT_DEFAULT_TIMEOUT).ok()?;
    steam.browse_my_workshop_page(page, search, language)
}

#[cfg(test)]
mod tests {
    use crate::workshop_id::workshop_id;

    use std::collections::HashSet;

    use super::{
        Instant, WorkshopChunkQueryResult, WorkshopItem, WorkshopQueryError,
        combine_workshop_chunk_results, drain_workshop_chunk_results, filter_new_workshop_ids,
        wait_for_background_callback, workshop_item_id_chunks,
    };

    fn deadline() -> Instant {
        Instant::now() + super::CALLBACK_RESULT_TIMEOUT
    }

    #[test]
    fn background_callback_wait_yields_immediately_to_shutdown() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let shutdown = crate::signal::Signal::new();
        shutdown.set();

        assert!(!wait_for_background_callback(&rx, &shutdown));
    }

    #[test]
    fn background_callback_wait_accepts_a_delivered_result() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(()).unwrap();

        assert!(wait_for_background_callback(
            &rx,
            &crate::signal::Signal::new()
        ));
    }

    #[test]
    fn empty_id_list_produces_no_query_chunks() {
        assert!(workshop_item_id_chunks(&[]).is_empty());
    }

    #[test]
    fn workshop_item_id_chunks_split_at_steamworks_page_cap() {
        let ids = (1..=super::super::RESULTS_PER_PAGE * 2 + 3)
            .map(|id| workshop_id(id as u64))
            .collect::<Vec<_>>();

        let chunks = workshop_item_id_chunks(&ids);

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), super::super::RESULTS_PER_PAGE);
        assert_eq!(chunks[1].len(), super::super::RESULTS_PER_PAGE);
        assert_eq!(chunks[2].len(), 3);
        assert_eq!(chunks[0][0], workshop_id(1));
        assert_eq!(
            chunks[2][2],
            workshop_id((super::super::RESULTS_PER_PAGE * 2 + 3) as u64)
        );
    }

    #[test]
    fn filter_new_workshop_ids_preserves_order_and_rejects_known_ids() {
        let mut cache = HashSet::from([workshop_id(2), workshop_id(4)]);

        let filtered = filter_new_workshop_ids(
            &mut cache,
            vec![
                workshop_id(1),
                workshop_id(2),
                workshop_id(3),
                workshop_id(1),
                workshop_id(4),
                workshop_id(5),
            ],
        );

        assert_eq!(
            filtered,
            vec![workshop_id(1), workshop_id(3), workshop_id(5)]
        );
        assert!(cache.contains(&workshop_id(1)));
        assert!(cache.contains(&workshop_id(2)));
        assert!(cache.contains(&workshop_id(3)));
        assert!(cache.contains(&workshop_id(4)));
        assert!(cache.contains(&workshop_id(5)));
    }

    #[test]
    fn workshop_chunk_results_keep_ordered_partial_successes_and_last_error() {
        let items = combine_workshop_chunk_results(
            vec![
                Ok(vec![WorkshopItem::from(workshop_id(10))]),
                Err(WorkshopQueryError::Steam("first".to_owned())),
                Ok(vec![
                    WorkshopItem::from(workshop_id(20)),
                    WorkshopItem::from(workshop_id(21)),
                ]),
            ],
            3,
        )
        .expect("partial success should return successful chunks");

        assert_eq!(
            items
                .into_iter()
                .map(|item| item.id)
                .collect::<Vec<crate::WorkshopId>>(),
            vec![workshop_id(10), workshop_id(20), workshop_id(21)]
        );

        let error = combine_workshop_chunk_results(
            vec![
                Err(WorkshopQueryError::QueryCreateFailed),
                Err(WorkshopQueryError::Steam("last".to_owned())),
            ],
            0,
        )
        .expect_err("all failed chunks should return the last failure");

        assert_eq!(error, WorkshopQueryError::Steam("last".to_owned()));
    }

    #[test]
    fn drain_streams_each_ok_chunk_and_isolates_a_failed_one() {
        use std::sync::mpsc;

        let (tx, rx) = mpsc::channel();
        tx.send((0, Ok(vec![WorkshopItem::from(workshop_id(10))])))
            .unwrap();
        tx.send((1, Err(WorkshopQueryError::Steam("boom".to_owned()))))
            .unwrap();
        tx.send((
            2,
            Ok(vec![
                WorkshopItem::from(workshop_id(20)),
                WorkshopItem::from(workshop_id(21)),
            ]),
        ))
        .unwrap();
        drop(tx);

        let mut delivered: Vec<Vec<crate::WorkshopId>> = Vec::new();
        drain_workshop_chunk_results(&rx, 3, deadline(), |chunk: Vec<WorkshopItem>| {
            delivered.push(chunk.into_iter().map(|item| item.id).collect());
        })
        .expect("a partially successful query is not an error");

        assert_eq!(
            delivered,
            vec![
                vec![workshop_id(10)],
                vec![workshop_id(20), workshop_id(21)],
            ]
        );
    }

    #[test]
    fn drain_errors_only_when_every_chunk_failed() {
        use std::sync::mpsc;

        let (tx, rx) = mpsc::channel();
        tx.send((0, Err(WorkshopQueryError::QueryCreateFailed)))
            .unwrap();
        tx.send((1, Err(WorkshopQueryError::Steam("last".to_owned()))))
            .unwrap();
        drop(tx);

        let error = drain_workshop_chunk_results(&rx, 2, deadline(), |_| {
            panic!("no chunk should be delivered when all fail")
        })
        .expect_err("all failed chunks should surface the last failure");

        assert_eq!(error, WorkshopQueryError::Steam("last".to_owned()));
    }

    #[test]
    fn a_chunk_steam_never_answers_is_an_error_rather_than_a_panic() {
        use std::sync::mpsc;

        let (tx, rx) = mpsc::channel();
        tx.send((0, Ok(vec![WorkshopItem::from(workshop_id(10))])))
            .unwrap();
        drop(tx);

        let mut delivered = 0;
        drain_workshop_chunk_results(&rx, 2, deadline(), |_| delivered += 1)
            .expect("the chunk that did answer still counts as a success");
        assert_eq!(delivered, 1);

        let (tx, rx) = mpsc::channel::<(usize, WorkshopChunkQueryResult)>();
        drop(tx);
        let error = drain_workshop_chunk_results(&rx, 1, deadline(), |_| {
            panic!("no chunk should be delivered when Steam answers none")
        })
        .expect_err("a query Steam never answered is a failed query");

        assert_eq!(error, WorkshopQueryError::Abandoned);
    }
}
