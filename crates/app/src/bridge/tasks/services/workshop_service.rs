use super::{
    BackendServices, backend_steam_id, backend_workshop_ids, record_thumbhash_in_cache,
    steam_not_connected,
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

impl BackendServices {
    pub(crate) fn workshop_language(&self) -> gmpublished_backend::WorkshopLanguage {
        self.backend.workshop_language()
    }
    pub(crate) fn set_workshop_language(
        &self,
        language: gmpublished_backend::WorkshopLanguage,
    ) -> bool {
        self.backend.set_workshop_language(language)
    }

    pub(crate) fn hydrate_metadata_snapshot(&self) {
        let Some(path) = self.metadata_snapshot_file.as_deref() else {
            return;
        };
        let loaded = metadata_snapshot::load(path);
        if !loaded.is_empty() {
            *self.workshop_metadata.lock() = loaded;
        }
    }

    pub(crate) fn browse_my_page(&self, page: u32) -> Result<WorkshopPage, UiError> {
        if !self.steam_connected() {
            return Err(steam_not_connected());
        }
        let page = self.fetch_my_page_connected(page)?;
        if !page.items.is_empty() {
            self.write_metadata_snapshot_best_effort();
        }
        Ok(page)
    }

    pub(crate) fn refresh_subscription_counts(
        &self,
        pages: u32,
    ) -> Result<HashMap<PublishedFileId, u64>, UiError> {
        if pages == 0 {
            return Ok(HashMap::new());
        }
        if !self.steam_connected() {
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
        &self,
        item_ids: &[PublishedFileId],
    ) -> (Vec<WorkshopMetadata>, Vec<PublishedFileId>) {
        let now = metadata_snapshot::now_unix_seconds();
        let language = self.workshop_language();
        let cache = self.workshop_metadata.lock();
        let mut metadata = Vec::new();
        let mut stale = Vec::new();
        for id in item_ids.iter().copied() {
            match cache.get(&id) {
                Some(cached) if cached.language == language => {
                    metadata.push(cached.metadata.clone());
                    if !cached.is_fresh_at(now) {
                        stale.push(id);
                    }
                }
                _ => stale.push(id),
            }
        }
        (metadata, stale)
    }

    pub(crate) fn refresh_metadata(
        &self,
        item_ids: &[PublishedFileId],
    ) -> Result<Vec<WorkshopMetadata>, UiError> {
        let language = self.workshop_language();
        let items = self.fetch_items(item_ids, language)?;
        let metadata = self.cache_items(&items, language);
        if !metadata.is_empty() {
            self.write_metadata_snapshot_best_effort();
        }
        Ok(metadata)
    }

    pub(crate) fn refresh_metadata_streaming(
        &self,
        item_ids: &[PublishedFileId],
        mut on_batch: impl FnMut(Vec<WorkshopMetadata>),
    ) -> Result<(), UiError> {
        if item_ids.is_empty() {
            return Ok(());
        }
        let steam = self.require_steam_client()?;
        let language = steam.workshop_language();
        let mut cached_any = false;
        let ids = backend_workshop_ids(item_ids);
        let result = steam_workshop::query_workshop_items_streaming(steam, &ids, |items| {
            let items = items
                .into_iter()
                .map(workshop_item_from_backend)
                .collect::<Vec<_>>();
            let metadata = self.cache_items(&items, language);
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

    pub(crate) fn item_details(&self, id: PublishedFileId) -> Result<WorkshopItem, UiError> {
        let steam = self.require_steam_client()?;
        let language = steam.workshop_language();
        let mut item = steam_workshop::query_workshop_item_details(steam, id.into())
            .map(workshop_item_from_backend)
            .ui_err()?;
        if !item.dead
            && let Ok(user) = self.current_steam_user()
        {
            let text = self.text_journal.lock().reconcile(
                user.steamid,
                id,
                language,
                crate::bridge::workshop_text::WorkshopText {
                    title: item.title,
                    description: item.description.unwrap_or_default(),
                },
            );
            item.title = text.title;
            item.description = Some(text.description);
        }
        self.cache_item_details(&item, language);
        Ok(item)
    }

    pub(crate) fn localized_text(
        &self,
        id: PublishedFileId,
        language: crate::bridge::workshop_text::WorkshopLanguage,
    ) -> Result<crate::bridge::workshop_text::WorkshopText, UiError> {
        let account = self.current_steam_user()?.steamid;
        let item = self
            .require_steam_client()?
            .query_localized_item_details(id.into(), language)
            .ui_err()?;
        let text = crate::bridge::workshop_text::WorkshopText {
            title: item.title,
            description: item.description.unwrap_or_default(),
        };
        Ok(self
            .text_journal
            .lock()
            .reconcile(account, id, language, text))
    }

    pub(crate) fn cached_item_details(&self, id: PublishedFileId) -> Option<WorkshopMetadata> {
        self.workshop_metadata
            .lock()
            .get(&id)
            .filter(|cached| cached.language == self.workshop_language())
            .map(|cached| cached.metadata.clone())
            .filter(|metadata| metadata.full_description.is_some())
    }

    #[cfg(test)]
    pub(crate) fn user_details(&self, steamid: SteamId) -> Result<SteamUser, UiError> {
        Ok(steam_user_from_workshop_backend(
            steam_users::fetch_steam_user(self.require_steam_client()?, backend_steam_id(steamid)),
        ))
    }

    pub(crate) fn user_details_streaming(
        &self,
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

    pub(crate) fn steam_connected(&self) -> bool {
        self.steam_runtime.is_connected()
    }

    pub(crate) fn connect_steam(&self) -> Result<(), UiError> {
        self.steam_runtime.connect().ui_err()
    }

    pub(crate) fn current_steam_user(&self) -> Result<SteamUser, UiError> {
        self.steam_runtime
            .current_user()
            .map(steam_user_from_backend)
            .ui_err()
    }

    pub(crate) fn submit_downloads(&self, item_ids: Vec<PublishedFileId>) -> Result<(), UiError> {
        self.backend
            .queue_workshop_downloads(item_ids.into_iter().map(Into::into))
            .ui_err()
    }

    pub(crate) fn submit_snapshot(
        &self,
        item_id: PublishedFileId,
        destination: crate::bridge::gma::ExtractDestination,
        request_id: WorkshopSnapshotId,
    ) -> Result<(), UiError> {
        self.backend
            .queue_workshop_download_to(item_id.into(), destination, request_id)
            .ui_err()
    }

    pub(crate) fn record_thumbhash(&self, url: &str, hash: &[u8]) {
        record_thumbhash_in_cache(&mut self.workshop_metadata.lock(), url.trim(), hash);
    }

    pub(crate) fn thumbhash_seed(&self) -> Vec<(String, Arc<[u8]>)> {
        self.workshop_metadata
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

    pub(crate) fn persist_metadata_cache(&self) {
        self.write_metadata_snapshot_best_effort();
    }

    fn require_steam_client(&self) -> Result<gmpublished_backend::ConnectedSteam<'_>, UiError> {
        self.backend.connected_steam().ui_err()
    }

    fn fetch_items(
        &self,
        item_ids: &[PublishedFileId],
        language: gmpublished_backend::WorkshopLanguage,
    ) -> Result<Vec<WorkshopItem>, UiError> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        steam_workshop::query_workshop_items(
            self.require_steam_client()?
                .with_workshop_language(language),
            &backend_workshop_ids(item_ids),
        )
        .map(|items| items.into_iter().map(workshop_item_from_backend).collect())
        .ui_err()
    }

    fn fetch_my_page_connected(&self, page: u32) -> Result<WorkshopPage, UiError> {
        let language = self.workshop_language();
        let page = self
            .backend
            .browse_my_workshop_page(page, language)
            .ok_or_else(|| UiError::new(keys::STEAM_ERROR))?;
        let items = page
            .items
            .into_iter()
            .map(workshop_item_from_backend)
            .collect::<Vec<_>>();
        self.cache_items(&items, language);
        Ok(WorkshopPage {
            total: page.total_results,
            items,
        })
    }

    fn cache_items(
        &self,
        items: &[WorkshopItem],
        language: gmpublished_backend::WorkshopLanguage,
    ) -> Vec<WorkshopMetadata> {
        let mut metadata = items
            .iter()
            .filter_map(WorkshopMetadata::from_workshop_item)
            .collect::<Vec<_>>();
        if metadata.is_empty() {
            return metadata;
        }

        let fetched_at = metadata_snapshot::now_unix_seconds();
        let mut cache = self.workshop_metadata.lock();
        for item in &mut metadata {
            if let Some(existing) = cache
                .get(&item.id)
                .filter(|cached| cached.language == language)
            {
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
                    language,
                    metadata: item.clone(),
                    fetched_at,
                },
            );
        }
        metadata
    }

    fn cache_item_details(
        &self,
        item: &WorkshopItem,
        language: gmpublished_backend::WorkshopLanguage,
    ) {
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
        let mut cache = self.workshop_metadata.lock();
        if let Some(existing) = cache.get(&metadata.id)
            && existing.language == language
            && metadata.thumbhash.is_none()
        {
            metadata.thumbhash.clone_from(&existing.metadata.thumbhash);
        }
        cache.insert(
            metadata.id,
            CachedWorkshopMetadata {
                language,
                metadata,
                fetched_at,
            },
        );
    }

    fn write_metadata_snapshot_best_effort(&self) {
        let Some(path) = self.metadata_snapshot_file.as_deref() else {
            return;
        };
        let snapshot = metadata_snapshot::prepare(&self.workshop_metadata.lock());
        if let Err(error) = metadata_snapshot::write_prepared(path, &snapshot) {
            log::warn!(
                "failed to write Workshop metadata snapshot {}: {error}",
                path.display()
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn cache_items_for_test(&self, items: &[WorkshopItem]) -> Vec<WorkshopMetadata> {
        self.cache_items(items, self.workshop_language())
    }

    #[cfg(test)]
    pub(crate) fn cache_item_details_for_test(&self, item: &WorkshopItem) {
        self.cache_item_details(item, self.workshop_language());
    }

    #[cfg(test)]
    pub(crate) fn set_metadata_fetched_at_for_test(&self, id: PublishedFileId, fetched_at: u64) {
        if let Some(cached) = self.workshop_metadata.lock().get_mut(&id) {
            cached.fetched_at = fetched_at;
        }
    }
}
