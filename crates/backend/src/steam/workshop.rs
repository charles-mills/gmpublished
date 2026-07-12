use std::{collections::HashSet, fmt, path::PathBuf, sync::Arc, sync::mpsc};

use steamworks::{PublishedFileId, QueryResult, QueryResults, SteamError, SteamId};

use super::{Steam, users::SteamUser};

use crate::{Addon, GMOD_APP_ID, search::Search};

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

#[derive(Clone, Debug)]
pub struct WorkshopItem {
    pub id: PublishedFileId,
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

impl From<QueryResult> for WorkshopItem {
    fn from(result: QueryResult) -> Self {
        Self {
            id: result.published_file_id,
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
        }
    }
}
impl From<PublishedFileId> for WorkshopItem {
    fn from(id: PublishedFileId) -> Self {
        Self {
            id,
            title: id.0.to_string(),
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
impl WorkshopItem {
    fn sort_key(&self) -> (u32, PublishedFileId) {
        let effective_timestamp = if self.time_created != 0 {
            self.time_created
        } else {
            self.time_updated
        };
        (effective_timestamp, self.id)
    }
}
impl PartialEq for WorkshopItem {
    fn eq(&self, other: &Self) -> bool {
        self.sort_key() == other.sort_key()
    }
}
impl Eq for WorkshopItem {}
impl PartialOrd for WorkshopItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WorkshopItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

fn enrich_workshop_item(item: QueryResult, index: u32, results: &QueryResults<'_>) -> WorkshopItem {
    let mut item: WorkshopItem = item.into();
    item.preview_url = results.preview_url(index);
    item.subscriptions = results
        .statistic(index, steamworks::UGCStatisticType::Subscriptions)
        .unwrap_or(0);
    item
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
    #[error("ERR_STEAM_ERROR:QUERY_CREATE_FAILED")]
    QueryCreateFailed,
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
            Self::Steam(error) => Some(error.clone()),
        }
    }
}

impl Steam {
    pub fn workshop_fetcher(steam: &Arc<Self>, search: &Arc<Search>) {
        loop {
            let rx = steam.workshop_queue_rx.lock();
            let Ok(mut queue) = rx.recv() else {
                return;
            };

            while let Ok(mut next) = rx.try_recv() {
                queue.append(&mut next);
            }
            drop(rx);

            while !queue.is_empty() {
                let chunk_len = super::RESULTS_PER_PAGE.min(queue.len());
                let chunk = queue.drain(..chunk_len).collect::<Vec<_>>();
                let chunk_for_callback = chunk.clone();
                let (done_tx, done_rx) = mpsc::channel();

                search.reserve(chunk.len());

                let client = steam
                    .client()
                    .expect("workshop_fetcher only runs after Steam has connected");
                if let Ok(query) = client.ugc().query_items(chunk) {
                    let steam_for_callback = Arc::clone(steam);
                    let search_for_callback = Arc::clone(search);
                    query.allow_cached_response(600).fetch(
                        move |results: Result<QueryResults<'_>, SteamError>| {
                            if let Ok(results) = results {
                                let items = results
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(i, item)| {
                                        item.map(|item| {
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

                let _ = done_rx.recv();
            }
        }
    }

    pub fn fetch_workshop_items(&self, ids: Vec<PublishedFileId>) {
        let ids = filter_new_workshop_ids(&mut self.workshop_dedup.lock(), ids);
        if !ids.is_empty() {
            let _ = self.workshop_queue_tx.send(ids);
        }
    }

    pub fn query_workshop_items(
        &self,
        ids: &[PublishedFileId],
    ) -> Result<Vec<WorkshopItem>, WorkshopQueryError> {
        self.query_workshop_items_with_description(ids, DescriptionLength::Summary)
    }

    pub fn query_workshop_item_details(
        &self,
        id: PublishedFileId,
    ) -> Result<WorkshopItem, WorkshopQueryError> {
        self.query_workshop_items_with_description(&[id], DescriptionLength::Full)
            .map(|mut items| items.pop().unwrap_or_else(|| WorkshopItem::from(id)))
    }

    fn query_workshop_items_with_description(
        &self,
        ids: &[PublishedFileId],
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

        let mut chunk_results: Vec<Option<WorkshopChunkQueryResult>> = vec![None; chunk_count];
        for _ in 0..chunk_count {
            let (chunk_index, result) = result_rx
                .recv()
                .expect("all workshop query chunks should be complete");
            chunk_results[chunk_index] = Some(result);
        }
        let chunk_results = chunk_results
            .into_iter()
            .map(|result| result.expect("all workshop query chunks should be complete"))
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
        ids: &[PublishedFileId],
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

        drain_workshop_chunk_results(&result_rx, chunk_count, on_chunk)
    }

    fn register_workshop_items_chunk_query(
        &self,
        chunk_index: usize,
        ids: Vec<PublishedFileId>,
        description_length: DescriptionLength,
        result_tx: mpsc::Sender<(usize, WorkshopChunkQueryResult)>,
    ) {
        let query = self
            .client()
            .expect("reached only through app-layer entry points that already checked steam_connected()")
            .ugc()
            .query_items(ids.clone());

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

    pub fn fetch_collection_items(
        &self,
        collection: PublishedFileId,
    ) -> Option<Vec<PublishedFileId>> {
        main_thread_forbidden!();

        let (tx, rx) = mpsc::sync_channel(1);
        self.client()
            .ok()?
            .ugc()
            .query_item(collection)
            .ok()?
            .include_children(true)
            .fetch(move |query: Result<QueryResults<'_>, SteamError>| {
                let children = query.ok().and_then(|results| {
                    let result = results.get(0)?;
                    if !matches!(result.file_type, steamworks::FileType::Collection) {
                        return None;
                    }
                    results
                        .get_children(0)
                        .filter(|children| !children.is_empty())
                });
                let _ = tx.send(children);
            });

        rx.recv().ok().flatten()
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
                            let item = enrich_workshop_item(x, i as u32, &data);
                            if let Some(search) = &index_into {
                                search.add(&item);
                            }
                            Some(item)
                        })
                        .collect(),
                });
                let _ = tx.send(page);
            });

        rx.recv().ok().flatten()
    }

    pub fn browse_my_workshop(&self, page: u32, search: &Arc<Search>) -> Option<(u32, Vec<Addon>)> {
        self.browse_my_workshop_page(page, search).map(|page| {
            (
                page.total_results,
                page.items.into_iter().map(Addon::from).collect(),
            )
        })
    }
}

pub fn browse_my_workshop(
    steam: &Steam,
    search: &Arc<Search>,
    page: u32,
) -> Option<(u32, Vec<Addon>)> {
    steam.client_wait(super::CLIENT_WAIT_DEFAULT_TIMEOUT).ok()?;
    rayon::scope(|_| steam.browse_my_workshop(page, search))
}

pub fn fetch_workshop_items(steam: &Steam, items: Vec<PublishedFileId>) {
    steam.fetch_workshop_items(items);
}

pub fn fetch_workshop_item(steam: &Steam, item: PublishedFileId) {
    steam.fetch_workshop_items(vec![item]);
}

fn workshop_item_id_chunks(ids: &[PublishedFileId]) -> Vec<Vec<PublishedFileId>> {
    ids.chunks(super::RESULTS_PER_PAGE.max(1))
        .map(<[PublishedFileId]>::to_vec)
        .collect()
}

fn filter_new_workshop_ids(
    cache: &mut HashSet<PublishedFileId>,
    ids: Vec<PublishedFileId>,
) -> Vec<PublishedFileId> {
    ids.into_iter().filter(|id| cache.insert(*id)).collect()
}

fn query_results_to_workshop_items(
    ids: &[PublishedFileId],
    results: Result<QueryResults<'_>, SteamError>,
) -> WorkshopChunkQueryResult {
    results
        .map(|results| {
            results
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    item.map_or_else(
                        || WorkshopItem::from(ids[i]),
                        |item| enrich_workshop_item(item, i as u32, &results),
                    )
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

/// Drains chunk query results as they arrive, handing each successful chunk
/// to `on_chunk` immediately for incremental hydration. A failed chunk is
/// logged and skipped so its ids stay stale for a later refresh; the call
/// only errors when every chunk failed.
fn drain_workshop_chunk_results(
    results: &mpsc::Receiver<(usize, WorkshopChunkQueryResult)>,
    chunk_count: usize,
    mut on_chunk: impl FnMut(Vec<WorkshopItem>),
) -> Result<(), WorkshopQueryError> {
    let mut any_chunk_succeeded = false;
    let mut last_error = None;

    for _ in 0..chunk_count {
        let (_chunk_index, result) = results
            .recv()
            .expect("all workshop query chunks should be complete");
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

pub fn query_workshop_items(
    steam: &Steam,
    ids: Vec<u64>,
) -> Result<Vec<WorkshopItem>, WorkshopQueryError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<PublishedFileId> = ids.into_iter().map(PublishedFileId).collect();
    steam.query_workshop_items(&ids)
}

pub fn query_workshop_item_details(
    steam: &Steam,
    id: u64,
) -> Result<WorkshopItem, WorkshopQueryError> {
    steam.query_workshop_item_details(PublishedFileId(id))
}

pub fn query_workshop_items_streaming(
    steam: &Steam,
    ids: Vec<u64>,
    on_chunk: impl FnMut(Vec<WorkshopItem>),
) -> Result<(), WorkshopQueryError> {
    if ids.is_empty() {
        return Ok(());
    }

    let ids: Vec<PublishedFileId> = ids.into_iter().map(PublishedFileId).collect();
    steam.query_workshop_items_streaming(&ids, on_chunk)
}

pub fn browse_my_workshop_page(
    steam: &Steam,
    search: &Arc<Search>,
    page: u32,
) -> Option<WorkshopPage> {
    steam.client_wait(super::CLIENT_WAIT_DEFAULT_TIMEOUT).ok()?;
    rayon::scope(|_| steam.browse_my_workshop_page(page, search))
}

pub fn free_caches(steam: &Steam) {
    steam.users.write().clear();
    steam.workshop_dedup.lock().clear();
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use steamworks::PublishedFileId;

    use super::{
        DescriptionLength, WorkshopItem, WorkshopQueryError, combine_workshop_chunk_results,
        drain_workshop_chunk_results, filter_new_workshop_ids, query_workshop_items,
        workshop_item_id_chunks,
    };

    #[test]
    fn detail_queries_request_full_descriptions() {
        assert!(DescriptionLength::Full.returns_full_description());
        assert!(!DescriptionLength::Summary.returns_full_description());
    }

    #[test]
    fn query_workshop_items_empty_is_noop_without_steam() {
        let steam = super::Steam::new(crate::transactions::Transactions::new(
            std::sync::Arc::new(crate::events::NullEventSink),
            false,
        ));
        assert_eq!(
            query_workshop_items(&steam, Vec::new()).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn workshop_item_id_chunks_split_at_steamworks_page_cap() {
        let ids = (0..super::super::RESULTS_PER_PAGE * 2 + 3)
            .map(|id| PublishedFileId(id as u64))
            .collect::<Vec<_>>();

        let chunks = workshop_item_id_chunks(&ids);

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), super::super::RESULTS_PER_PAGE);
        assert_eq!(chunks[1].len(), super::super::RESULTS_PER_PAGE);
        assert_eq!(chunks[2].len(), 3);
        assert_eq!(chunks[0][0], PublishedFileId(0));
        assert_eq!(
            chunks[2][2],
            PublishedFileId((super::super::RESULTS_PER_PAGE * 2 + 2) as u64)
        );
    }

    #[test]
    fn filter_new_workshop_ids_preserves_order_and_rejects_known_ids() {
        let mut cache = HashSet::from([PublishedFileId(2), PublishedFileId(4)]);

        let filtered = filter_new_workshop_ids(
            &mut cache,
            vec![
                PublishedFileId(1),
                PublishedFileId(2),
                PublishedFileId(3),
                PublishedFileId(1),
                PublishedFileId(4),
                PublishedFileId(5),
            ],
        );

        assert_eq!(
            filtered,
            vec![PublishedFileId(1), PublishedFileId(3), PublishedFileId(5)]
        );
        assert!(cache.contains(&PublishedFileId(1)));
        assert!(cache.contains(&PublishedFileId(2)));
        assert!(cache.contains(&PublishedFileId(3)));
        assert!(cache.contains(&PublishedFileId(4)));
        assert!(cache.contains(&PublishedFileId(5)));
    }

    #[test]
    fn workshop_chunk_results_keep_ordered_partial_successes_and_last_error() {
        let items = combine_workshop_chunk_results(
            vec![
                Ok(vec![WorkshopItem::from(PublishedFileId(10))]),
                Err(WorkshopQueryError::Steam("first".to_owned())),
                Ok(vec![
                    WorkshopItem::from(PublishedFileId(20)),
                    WorkshopItem::from(PublishedFileId(21)),
                ]),
            ],
            3,
        )
        .expect("partial success should return successful chunks");

        assert_eq!(
            items
                .into_iter()
                .map(|item| item.id)
                .collect::<Vec<PublishedFileId>>(),
            vec![
                PublishedFileId(10),
                PublishedFileId(20),
                PublishedFileId(21)
            ]
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
        tx.send((0, Ok(vec![WorkshopItem::from(PublishedFileId(10))])))
            .unwrap();
        tx.send((1, Err(WorkshopQueryError::Steam("boom".to_owned()))))
            .unwrap();
        tx.send((
            2,
            Ok(vec![
                WorkshopItem::from(PublishedFileId(20)),
                WorkshopItem::from(PublishedFileId(21)),
            ]),
        ))
        .unwrap();
        drop(tx);

        let mut delivered: Vec<Vec<PublishedFileId>> = Vec::new();
        drain_workshop_chunk_results(&rx, 3, |chunk| {
            delivered.push(chunk.into_iter().map(|item| item.id).collect());
        })
        .expect("a partially successful query is not an error");

        // The failed chunk delivered nothing; each successful chunk arrived as
        // its own batch, so the first is observable before the last lands.
        assert_eq!(
            delivered,
            vec![
                vec![PublishedFileId(10)],
                vec![PublishedFileId(20), PublishedFileId(21)],
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

        let error = drain_workshop_chunk_results(&rx, 2, |_| {
            panic!("no chunk should be delivered when all fail")
        })
        .expect_err("all failed chunks should surface the last failure");

        assert_eq!(error, WorkshopQueryError::Steam("last".to_owned()));
    }
}
