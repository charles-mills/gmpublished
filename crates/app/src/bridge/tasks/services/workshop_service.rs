use super::{
    BackendServices, backend_steam_id, backend_workshop_id, backend_workshop_ids,
    record_thumbhash_in_cache, steam_not_connected,
};
use crate::bridge::{
    domain::{PublishedFileId, SteamId, SteamUser, WorkshopItem, WorkshopMetadata, WorkshopPage},
    metadata_snapshot::{self, CachedWorkshopMetadata},
    tasks::projections::{
        steam_user_from_backend, steam_user_from_workshop_backend, subscription_counts_from_items,
        workshop_item_from_backend,
    },
    ui_error::{ResultExt as _, UiError},
};
use gmpublished_backend::{
    WorkshopSnapshotId, error_keys as keys, steam_users, workshop as steam_workshop,
};
use std::{collections::HashMap, sync::Arc};

/// Steam session, Workshop browsing, metadata, and download capability.
#[derive(Clone, Copy)]
pub struct WorkshopService<'a> {
    inner: &'a BackendServices,
}

impl<'a> WorkshopService<'a> {
    pub(super) const fn new(inner: &'a BackendServices) -> Self {
        Self { inner }
    }

    pub(crate) fn hydrate_metadata_snapshot(self) {
        let Some(path) = self.inner.metadata_snapshot_file.as_deref() else {
            return;
        };
        let loaded = metadata_snapshot::load(path);
        if !loaded.is_empty() {
            *self.inner.workshop_metadata.lock() = loaded;
        }
    }

    pub(crate) fn browse_my_page(self, page: u32) -> Result<WorkshopPage, UiError> {
        if !self.connected() {
            return Err(steam_not_connected());
        }
        let page = self.fetch_my_page_connected(page)?;
        if !page.items.is_empty() {
            self.write_metadata_snapshot_best_effort();
        }
        Ok(page)
    }

    pub(crate) fn refresh_subscription_counts(
        self,
        pages: u32,
    ) -> Result<HashMap<PublishedFileId, u64>, UiError> {
        if pages == 0 {
            return Ok(HashMap::new());
        }
        if !self.connected() {
            return Err(steam_not_connected());
        }

        let mut counts = HashMap::new();
        for page in 1..=pages {
            let page = self.fetch_my_page_connected(page)?;
            counts.extend(subscription_counts_from_items(&page.items));
        }
        if !counts.is_empty() {
            self.write_metadata_snapshot_best_effort();
        }
        Ok(counts)
    }

    pub(crate) fn resolve_metadata(
        self,
        item_ids: &[PublishedFileId],
    ) -> (Vec<WorkshopMetadata>, Vec<PublishedFileId>) {
        let now = metadata_snapshot::now_unix_seconds();
        let cache = self.inner.workshop_metadata.lock();
        let mut metadata = Vec::new();
        let mut stale = Vec::new();
        for id in item_ids.iter().copied() {
            match cache.get(&id) {
                Some(cached) => {
                    metadata.push(cached.metadata.clone());
                    if !cached.is_fresh_at(now) {
                        stale.push(id);
                    }
                }
                None => stale.push(id),
            }
        }
        (metadata, stale)
    }

    pub(crate) fn refresh_metadata(
        self,
        item_ids: &[PublishedFileId],
    ) -> Result<Vec<WorkshopMetadata>, UiError> {
        let items = self.fetch_items(item_ids)?;
        let metadata = self.cache_items(&items);
        if !metadata.is_empty() {
            self.write_metadata_snapshot_best_effort();
        }
        Ok(metadata)
    }

    pub(crate) fn refresh_metadata_streaming(
        self,
        item_ids: &[PublishedFileId],
        mut on_batch: impl FnMut(Vec<WorkshopMetadata>),
    ) -> Result<(), UiError> {
        if item_ids.is_empty() {
            return Ok(());
        }
        let steam = self.require_steam_client()?;
        let mut cached_any = false;
        let ids = backend_workshop_ids(item_ids);
        let result = steam_workshop::query_workshop_items_streaming(steam, &ids, |items| {
            let items = items
                .into_iter()
                .map(workshop_item_from_backend)
                .collect::<Vec<_>>();
            let metadata = self.cache_items(&items);
            if !metadata.is_empty() {
                cached_any = true;
                on_batch(metadata);
            }
        });
        if cached_any {
            self.write_metadata_snapshot_best_effort();
        }
        result.ui_err()
    }

    pub(crate) fn item_details(self, id: PublishedFileId) -> Result<WorkshopItem, UiError> {
        let item = steam_workshop::query_workshop_item_details(
            self.require_steam_client()?,
            backend_workshop_id(id),
        )
        .map(workshop_item_from_backend)
        .ui_err()?;
        self.cache_item_details(&item);
        Ok(item)
    }

    pub(crate) fn cached_item_details(self, id: PublishedFileId) -> Option<WorkshopMetadata> {
        self.inner
            .workshop_metadata
            .lock()
            .get(&id)
            .map(|cached| cached.metadata.clone())
            .filter(|metadata| metadata.full_description.is_some())
    }

    #[cfg(test)]
    pub(crate) fn user_details(self, steamid: SteamId) -> Result<SteamUser, UiError> {
        Ok(steam_user_from_workshop_backend(
            steam_users::fetch_steam_user(self.require_steam_client()?, backend_steam_id(steamid)),
        ))
    }

    pub(crate) fn user_details_streaming(
        self,
        steamid: SteamId,
        mut on_user: impl FnMut(SteamUser),
    ) -> Result<(), UiError> {
        steam_users::fetch_steam_user_streaming(
            self.require_steam_client()?,
            backend_steam_id(steamid),
            |user| on_user(steam_user_from_workshop_backend(user)),
        );
        Ok(())
    }

    pub(crate) fn connected(self) -> bool {
        self.inner.steam_runtime.is_connected()
    }

    pub(crate) fn connect(self) -> Result<(), UiError> {
        self.inner.steam_runtime.connect().ui_err()
    }

    pub(crate) fn current_user(self) -> Result<SteamUser, UiError> {
        self.inner
            .steam_runtime
            .current_user()
            .map(steam_user_from_backend)
            .ui_err()
    }

    pub(crate) fn submit_downloads(self, item_ids: Vec<PublishedFileId>) -> Result<(), UiError> {
        self.inner
            .backend
            .queue_workshop_downloads(item_ids.into_iter().map(Into::into))
            .ui_err()
    }

    pub(crate) fn submit_snapshot(
        self,
        item_id: PublishedFileId,
        destination: crate::bridge::gma::ExtractDestination,
        request_id: WorkshopSnapshotId,
    ) -> Result<(), UiError> {
        self.inner
            .backend
            .queue_workshop_download_to(item_id.into(), destination, request_id)
            .ui_err()
    }

    pub(crate) fn record_thumbhash(self, url: &str, hash: &[u8]) {
        record_thumbhash_in_cache(&mut self.inner.workshop_metadata.lock(), url.trim(), hash);
    }

    pub(crate) fn thumbhash_seed(self) -> Vec<(String, Arc<[u8]>)> {
        self.inner
            .workshop_metadata
            .lock()
            .values()
            .filter_map(|cached| {
                Some((
                    cached.metadata.preview_url.as_deref()?.to_owned(),
                    cached.metadata.thumbhash.clone()?,
                ))
            })
            .collect()
    }

    pub(crate) fn persist_metadata_cache(self) {
        self.write_metadata_snapshot_best_effort();
    }

    fn require_steam_client(self) -> Result<gmpublished_backend::ConnectedSteam<'a>, UiError> {
        self.inner.backend.connected_steam().ui_err()
    }

    fn fetch_items(self, item_ids: &[PublishedFileId]) -> Result<Vec<WorkshopItem>, UiError> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        steam_workshop::query_workshop_items(
            self.require_steam_client()?,
            &backend_workshop_ids(item_ids),
        )
        .map(|items| items.into_iter().map(workshop_item_from_backend).collect())
        .ui_err()
    }

    fn fetch_my_page_connected(self, page: u32) -> Result<WorkshopPage, UiError> {
        let page = self
            .inner
            .backend
            .browse_my_workshop_page(page)
            .ok_or_else(|| UiError::new(keys::STEAM_ERROR))?;
        let items = page
            .items
            .into_iter()
            .map(workshop_item_from_backend)
            .collect::<Vec<_>>();
        self.cache_items(&items);
        Ok(WorkshopPage {
            total: page.total_results,
            items,
        })
    }

    fn cache_items(self, items: &[WorkshopItem]) -> Vec<WorkshopMetadata> {
        let mut metadata = items
            .iter()
            .filter_map(WorkshopMetadata::from_workshop_item)
            .collect::<Vec<_>>();
        if metadata.is_empty() {
            return metadata;
        }

        let fetched_at = metadata_snapshot::now_unix_seconds();
        let mut cache = self.inner.workshop_metadata.lock();
        for item in &mut metadata {
            if let Some(existing) = cache.get(&item.id) {
                if item.thumbhash.is_none() {
                    item.thumbhash.clone_from(&existing.metadata.thumbhash);
                }
                if item.full_description.is_none() {
                    item.full_description
                        .clone_from(&existing.metadata.full_description);
                }
                if item.owner_steamid.is_none() {
                    item.owner_steamid = existing.metadata.owner_steamid;
                }
            }
            cache.insert(
                item.id,
                CachedWorkshopMetadata {
                    metadata: item.clone(),
                    fetched_at,
                },
            );
        }
        metadata
    }

    fn cache_item_details(self, item: &WorkshopItem) {
        let Some(mut metadata) = WorkshopMetadata::from_workshop_item(item) else {
            return;
        };
        metadata.full_description = Some(
            item.description
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .to_owned(),
        );
        metadata.owner_steamid = item.steamid;

        let fetched_at = metadata_snapshot::now_unix_seconds();
        let mut cache = self.inner.workshop_metadata.lock();
        if let Some(existing) = cache.get(&metadata.id)
            && metadata.thumbhash.is_none()
        {
            metadata.thumbhash.clone_from(&existing.metadata.thumbhash);
        }
        cache.insert(
            metadata.id,
            CachedWorkshopMetadata {
                metadata,
                fetched_at,
            },
        );
    }

    fn write_metadata_snapshot_best_effort(self) {
        let Some(path) = self.inner.metadata_snapshot_file.as_deref() else {
            return;
        };
        let snapshot = metadata_snapshot::prepare(&self.inner.workshop_metadata.lock());
        if let Err(error) = metadata_snapshot::write_prepared(path, &snapshot) {
            log::warn!(
                "failed to write Workshop metadata snapshot {}: {error}",
                path.display()
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn cache_items_for_test(self, items: &[WorkshopItem]) -> Vec<WorkshopMetadata> {
        self.cache_items(items)
    }

    #[cfg(test)]
    pub(crate) fn cache_item_details_for_test(self, item: &WorkshopItem) {
        self.cache_item_details(item);
    }

    #[cfg(test)]
    pub(crate) fn set_metadata_fetched_at_for_test(self, id: PublishedFileId, fetched_at: u64) {
        if let Some(cached) = self.inner.workshop_metadata.lock().get_mut(&id) {
            cached.fetched_at = fetched_at;
        }
    }
}
