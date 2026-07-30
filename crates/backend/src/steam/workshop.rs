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
use crate::{GMOD_APP_ID, search::Search};

type WorkshopChunkQueryResult = Result<Vec<WorkshopItem>, WorkshopQueryError>;

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

/// Deliberately has no `Eq`/`Ord`: the ordering this type invites is browse
/// chronology, which as an `Eq` makes two snapshots of one item compare
/// unequal. Presentation order belongs at the call site that wants it.
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

/// Steam supplies the id, so it is external input: a zero names no item and
/// there is nothing this side could do with the rest of the row.
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
/// `None` when Steam's row names no item, which leaves nothing to enrich.
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
    /// steamworks never delivered a result for the query — it dropped the
    /// callback without invoking it, or held it past
    /// [`CALLBACK_RESULT_TIMEOUT`]. Distinct from [`Self::Steam`], which is a
    /// result Steam did return.
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
    pub fn workshop_fetcher(steam: &Arc<Self>, search: &Arc<Search>, client: steamworks::Client) {
        loop {
            let rx = steam.workshop_queue_rx.lock();
            let Ok(mut queue) = rx.recv() else {
                return;
            };

            while let Ok(mut next) = rx.try_recv() {
                queue.append(&mut next);
            }
            drop(rx);

            // `Steam::shutdown` pushes an empty batch purely to unblock the
            // `recv` above — this thread parks there with no timeout, and the
            // sender lives on the `Steam` its own `Arc` clone keeps alive, so
            // the channel never disconnects on its own. Anything still queued
            // at this point is abandoned.
            if steam.shutting_down() {
                return;
            }

            while !queue.is_empty() {
                let chunk_len = super::RESULTS_PER_PAGE.min(queue.len());
                let chunk = queue.drain(..chunk_len).collect::<Vec<_>>();
                let chunk_for_callback = chunk.clone();
                let (done_tx, done_rx) = mpsc::channel();

                search.reserve(chunk.len());

                if let Ok(query) = client
                    .ugc()
                    .query_items(chunk.into_iter().map(Into::into).collect())
                {
                    let steam_for_callback = Arc::clone(steam);
                    let search_for_callback = Arc::clone(search);
                    query.allow_cached_response(600).fetch(
                        move |results: Result<QueryResults<'_>, SteamError>| {
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

                let _ = done_rx.recv_timeout(CALLBACK_RESULT_TIMEOUT);
            }
        }
    }

    /// Runs `use_cache` against the set of Workshop ids the metadata fetcher
    /// already knows, or against `None` if the cache is busy.
    ///
    /// Bounded rather than blocking: both callers are on a download path, and
    /// `None` ("assume nothing is known") only costs a wider Steam query.
    ///
    /// `use_cache` runs under the fetcher's lock — keep it O(ids), no I/O.
    pub(crate) fn with_known_workshop_items<R>(
        &self,
        use_cache: impl FnOnce(Option<&HashSet<WorkshopId>>) -> R,
    ) -> R {
        let cache = self
            .workshop_dedup
            .try_lock_for(super::CALLBACK_PUMP_INTERVAL + Duration::from_millis(1));
        use_cache(cache.as_deref())
    }

    pub fn fetch_workshop_items(&self, ids: Vec<WorkshopId>) {
        let ids = filter_new_workshop_ids(&mut self.workshop_dedup.lock(), ids);
        if !ids.is_empty() {
            let _ = self.workshop_queue_tx.send(ids);
        }
    }

    pub fn browse_my_workshop_page(&self, page: u32, search: &Arc<Search>) -> Option<WorkshopPage> {
        self.browse_user_workshop_page(
            steamworks::UserList::Published,
            steamworks::UserListOrder::LastUpdatedDesc,
            page,
            Some(Arc::clone(search)),
        )
    }

    /// Shared by the "my workshop" and "subscribed" browse pages, which query
    /// the same user-scoped UGC endpoint and differ only in which list is
    /// requested, its ordering, and whether results get indexed for search.
    pub(crate) fn browse_user_workshop_page(
        &self,
        user_list: steamworks::UserList,
        user_list_order: steamworks::UserListOrder,
        page: u32,
        index_into: Option<Arc<Search>>,
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
                steamworks::AppIDs::ConsumerAppId(GMOD_APP_ID),
                page,
            )
            .ok()?
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
                            if let Some(search) = &index_into {
                                search.add(&item);
                            }
                            Some(item)
                        })
                        .collect(),
                });
                let _ = tx.send(page);
            });

        rx.recv_timeout(CALLBACK_RESULT_TIMEOUT).ok().flatten()
    }
}

pub fn fetch_workshop_items(steam: &Steam, items: Vec<WorkshopId>) {
    steam.fetch_workshop_items(items);
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
                    // A row Steam omitted, and one whose id names no item,
                    // both leave the requested id with nothing but itself.
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

/// Waits for one chunk result, giving up at `deadline`.
///
/// `None` covers a dropped sender and an expired deadline alike: steamworks
/// owns the callback's lifetime, so a result that never arrives is a failed
/// query, not a broken invariant.
///
/// One deadline spans a whole fan-out — the chunks are concurrent, so a
/// per-`recv` budget would multiply by the chunk count.
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

    /// Same concurrent chunk queries as [`Self::query_workshop_items`], but
    /// hands each chunk to `on_chunk` the moment it lands instead of joining
    /// all chunks first, so callers hydrate on-screen rows after a single
    /// round trip. A failed chunk is logged and skipped (its ids stay stale);
    /// the call only errors when every chunk failed.
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
                query.allow_cached_response(600).fetch(
                    move |results: Result<QueryResults<'_>, SteamError>| {
                        let _ = result_tx
                            .send((chunk_index, query_results_to_workshop_items(&ids, results)));
                    },
                );
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

pub fn browse_my_workshop_page(
    steam: &Steam,
    search: &Arc<Search>,
    page: u32,
) -> Option<WorkshopPage> {
    steam.client_wait(super::CLIENT_WAIT_DEFAULT_TIMEOUT).ok()?;
    steam.browse_my_workshop_page(page, search)
}

#[cfg(test)]
mod tests {
    use crate::workshop_id::workshop_id;

    use std::collections::HashSet;

    use super::{
        DescriptionLength, Instant, WorkshopChunkQueryResult, WorkshopItem, WorkshopQueryError,
        combine_workshop_chunk_results, drain_workshop_chunk_results, filter_new_workshop_ids,
        workshop_item_id_chunks,
    };

    /// Every drain in these tests is fed a channel that is already closed or
    /// already full, so nothing waits — the deadline only has to be in the
    /// future, not realistic.
    fn deadline() -> Instant {
        Instant::now() + super::CALLBACK_RESULT_TIMEOUT
    }

    #[test]
    fn detail_queries_request_full_descriptions() {
        assert!(DescriptionLength::Full.returns_full_description());
        assert!(!DescriptionLength::Summary.returns_full_description());
    }

    /// An empty id list must reach steamworks zero times.
    ///
    /// `query_workshop_items_with_description` short-circuits before chunking,
    /// so this pins the chunker itself: were that guard removed, an empty list
    /// still yields no chunks and therefore no `query_items` call.
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

        // The failed chunk delivered nothing; each successful chunk arrived as
        // its own batch, so the first is observable before the last lands.
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

    /// steamworks may drop a query's callback without ever invoking it, so a
    /// chunk that never reports is an outcome the drain has to survive rather
    /// than an invariant it can rely on.
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
