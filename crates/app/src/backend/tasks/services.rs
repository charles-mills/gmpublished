use std::sync::Arc;

use gmpublished_backend::error_key::keys;

use super::{
    AppPaths, Backend, BackendAppDataSnapshot, BackendEventSinkRegistration,
    BackendRuntimeEventSubscription, CachedWorkshopMetadata, HashMap, LibraryRefresh,
    LibraryRefreshReason, LibrarySnapshot, LibraryStore, Mutex, NativeOpenTarget, PathBuf,
    PublishSubmitOutcome, PublishSubmitRequest, PublishedFileId, SearchFullBatch,
    SearchFullRequest, SearchMode, SearchQuickBatch, SearchQuickRequest, Settings,
    SettingsPersistError, SteamRuntime, SteamRuntimeError, SteamUser, TransactionPayload, UiError,
    UiSettings, WorkshopItem, WorkshopMetadata, WorkshopPage, appdata_snapshot_from_backend,
    clear_directory_contents, default_paths, downloads, library, metadata_snapshot, native,
    persist_appdata_settings_by_default, persist_ui_settings_by_default,
    publish_submission_from_app_request, search_full_batch_from_transaction_payload,
    search_quick_batch_from_backend, steam_publishing, steam_user_from_backend,
    steam_user_from_workshop_backend, steam_users, steam_workshop, subscription_counts_from_items,
    transactions, ui_settings_file_for, workshop_item_from_backend,
};

#[derive(Debug)]
pub struct BackendServices {
    pub(super) backend: Arc<Backend>,
    settings: Mutex<Settings>,
    paths: Mutex<AppPaths>,
    persist_appdata_settings: bool,
    persist_ui_settings: bool,
    ui_settings_file: PathBuf,
    steam_runtime: SteamRuntime,
    library: LibraryStore,
    workshop_metadata: Mutex<HashMap<PublishedFileId, CachedWorkshopMetadata>>,
    metadata_snapshot_file: Option<PathBuf>,
    _backend_event_sink: Option<BackendEventSinkRegistration>,
    #[cfg(test)]
    _test_data_root: Option<tempfile::TempDir>,
}

impl BackendServices {
    /// The default entry point every `App::new()` (production) or
    /// `BackendContext::new()` (tests) goes through. Builds one `Backend`
    /// (real, with background threads, in production; a private-tempdir,
    /// no-background-threads one in tests — [`build_default_backend`]) and
    /// derives the initial settings/paths from its `AppData` snapshot.
    pub(super) fn new(
        backend_event_sink: Option<BackendEventSinkRegistration>,
    ) -> Result<Self, gmpublished_backend::BackendInitError> {
        let event_sink = backend_event_sink.as_ref().map_or_else(
            || Arc::new(gmpublished_backend::events::NullEventSink) as _,
            BackendEventSinkRegistration::sink,
        );
        #[cfg(not(test))]
        let backend = build_default_backend(event_sink)?;
        #[cfg(test)]
        let (backend, test_data_root) = build_default_backend(event_sink)?;
        let (settings, paths) =
            appdata_snapshot_from_backend(backend.app_data.snapshot(), &UiSettings::default());
        let steam_runtime = SteamRuntime::new(Arc::clone(&backend.steam));
        let services = Self::new_with_steam_runtime(
            backend,
            settings,
            paths,
            steam_runtime,
            backend_event_sink,
        );
        #[cfg(test)]
        let services = {
            let mut services = services;
            services._test_data_root = Some(test_data_root);
            services
        };
        Ok(services)
    }

    fn new_with_steam_runtime(
        backend: Arc<Backend>,
        settings: Settings,
        paths: AppPaths,
        steam_runtime: SteamRuntime,
        backend_event_sink: Option<BackendEventSinkRegistration>,
    ) -> Self {
        let mut services =
            Self::with_steam_runtime(backend, settings, paths, steam_runtime, backend_event_sink);
        // Tests construct through this path too; never let them read or write
        // the developer's real cache directory.
        if !cfg!(test) {
            services.metadata_snapshot_file = metadata_snapshot::snapshot_path();
            if let Some(path) = library::header_snapshot_path() {
                services.library.set_header_snapshot_file(path);
            }
            services.hydrate_workshop_metadata_snapshot();
        }
        services
    }

    fn with_steam_runtime(
        backend: Arc<Backend>,
        settings: Settings,
        paths: AppPaths,
        steam_runtime: SteamRuntime,
        backend_event_sink: Option<BackendEventSinkRegistration>,
    ) -> Self {
        let ui_settings_file = ui_settings_file_for(&paths.settings_file);
        let persist_ui_settings = persist_ui_settings_by_default();
        let mut settings = settings;
        if persist_ui_settings {
            settings.apply_ui_settings(&UiSettings::load_from_file_or_default(&ui_settings_file));
        }

        Self {
            backend,
            settings: Mutex::new(settings),
            paths: Mutex::new(paths),
            persist_appdata_settings: persist_appdata_settings_by_default(),
            persist_ui_settings,
            ui_settings_file,
            steam_runtime,
            library: LibraryStore::new(),
            workshop_metadata: Mutex::new(HashMap::new()),
            metadata_snapshot_file: None,
            _backend_event_sink: backend_event_sink,
            #[cfg(test)]
            _test_data_root: None,
        }
    }

    fn hydrate_workshop_metadata_snapshot(&self) {
        let Some(path) = self.metadata_snapshot_file.as_deref() else {
            return;
        };
        let loaded = metadata_snapshot::load(path);
        if !loaded.is_empty() {
            *self.workshop_metadata.lock() = loaded;
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self::for_test_with_event_sink(Arc::new(gmpublished_backend::events::NullEventSink))
    }

    /// Like [`Self::for_test`], but with the real `AppData`-backed settings
    /// persistence path enabled (disabled by default in tests), so a test
    /// can exercise `update_settings_snapshot`'s actual disk write and its
    /// failure propagation.
    #[cfg(test)]
    pub(crate) fn for_test_with_appdata_persist_enabled() -> Self {
        let mut services = Self::for_test();
        services.persist_appdata_settings = true;
        services
    }

    /// Like [`Self::for_test`], but with an explicit event sink (a
    /// `BackendEventCollector`, typically) so the test can observe events
    /// the backend emits directly, without going through a `BackendContext`.
    #[cfg(test)]
    pub(crate) fn for_test_with_event_sink(
        event_sink: Arc<dyn gmpublished_backend::events::BackendEventSink>,
    ) -> Self {
        let (backend, test_data_root) =
            build_default_backend(event_sink).expect("test backend init");
        let settings = Settings::default();
        let paths = default_paths(&settings);
        let mut services = Self::with_steam_runtime(
            backend,
            settings,
            paths,
            SteamRuntime::unavailable_for_tests(),
            None,
        );
        services.persist_appdata_settings = false;
        services._test_data_root = Some(test_data_root);
        services
    }

    #[cfg(test)]
    pub(super) fn for_test_with_ui_settings_file(ui_settings_file: PathBuf) -> Self {
        let (backend, test_data_root) =
            build_default_backend(Arc::new(gmpublished_backend::events::NullEventSink))
                .expect("test backend init");
        let ui = UiSettings::load_from_file_or_default(&ui_settings_file);
        let mut settings = Settings::default();
        settings.apply_ui_settings(&ui);
        let paths = default_paths(&settings);
        let mut services = Self::with_steam_runtime(
            backend,
            settings,
            paths,
            SteamRuntime::unavailable_for_tests(),
            None,
        );
        services.persist_appdata_settings = false;
        services.persist_ui_settings = true;
        services.ui_settings_file = ui_settings_file;
        services._test_data_root = Some(test_data_root);
        services
    }

    pub(crate) fn begin_transaction(&self) -> transactions::Transaction {
        self.backend.transactions.begin()
    }

    pub(crate) fn whitelist_snapshot(&self) -> Arc<Vec<String>> {
        self.backend.whitelist.snapshot()
    }

    pub(crate) fn settings_snapshot(&self) -> Settings {
        self.settings.lock().clone()
    }

    pub(crate) fn paths(&self) -> AppPaths {
        self.paths.lock().clone()
    }

    pub(crate) fn settings_and_paths_snapshot(&self) -> (Settings, AppPaths) {
        (self.settings_snapshot(), self.paths())
    }

    /// Mutates a copy of the current settings, persists it, and only then
    /// publishes it as live state. The settings lock is held for the whole
    /// mutate-persist-publish sequence, so a slower concurrent save can
    /// never land after (and overwrite) a faster later one, and a failed
    /// persist never leaves an unsaved draft installed as live state.
    pub(crate) fn update_settings_snapshot(
        &self,
        update: impl FnOnce(&mut Settings),
    ) -> Result<(), SettingsPersistError> {
        let mut guard = self.settings.lock();
        let mut settings = guard.clone();
        update(&mut settings);
        let ui = UiSettings::from_settings(&settings);
        let backend_settings = settings.to_backend();
        if self.persist_appdata_settings {
            self.persist_ui_settings(&ui)?;
            self.backend
                .app_data
                .update_settings(backend_settings, &self.backend.steam)?;
            let (settings, paths) =
                appdata_snapshot_from_backend(self.backend.app_data.snapshot(), &ui);
            *guard = settings;
            *self.paths.lock() = paths;
        } else {
            self.persist_ui_settings(&ui)?;
            let paths = AppPaths::resolve_with_defaults(&settings, default_paths(&settings));
            *guard = settings;
            *self.paths.lock() = paths;
        }
        drop(guard);
        Ok(())
    }

    /// Same held-lock discipline as [`Self::update_settings_snapshot`]: the
    /// default settings are only published once they're confirmed persisted.
    pub(crate) fn reset_settings(&self) -> Result<Settings, SettingsPersistError> {
        let mut guard = self.settings.lock();
        self.persist_ui_settings(&UiSettings::default())?;
        if self.persist_appdata_settings {
            self.backend
                .app_data
                .update_settings(Settings::default().to_backend(), &self.backend.steam)?;
            let (settings, paths) = appdata_snapshot_from_backend(
                self.backend.app_data.snapshot(),
                &UiSettings::default(),
            );
            *guard = settings.clone();
            *self.paths.lock() = paths;
            drop(guard);
            Ok(settings)
        } else {
            *guard = Settings::default();
            let paths = AppPaths::resolve_with_defaults(&guard, default_paths(&guard));
            *self.paths.lock() = paths;
            let settings = guard.clone();
            drop(guard);
            Ok(settings)
        }
    }

    pub(crate) fn apply_appdata_snapshot(
        &self,
        snapshot: BackendAppDataSnapshot,
    ) -> (Settings, AppPaths) {
        let ui = {
            let settings = self.settings.lock();
            UiSettings::from_settings(&settings)
        };
        self.apply_appdata_snapshot_with_ui(snapshot, &ui)
    }

    fn apply_appdata_snapshot_with_ui(
        &self,
        snapshot: BackendAppDataSnapshot,
        ui: &UiSettings,
    ) -> (Settings, AppPaths) {
        let (settings, paths) = appdata_snapshot_from_backend(snapshot, ui);
        *self.settings.lock() = settings.clone();
        *self.paths.lock() = paths.clone();
        (settings, paths)
    }

    fn persist_ui_settings(&self, ui: &UiSettings) -> Result<(), SettingsPersistError> {
        if self.persist_ui_settings {
            ui.save_to_file(&self.ui_settings_file)?;
        }
        Ok(())
    }

    pub(crate) fn clear_temp_files(&self) -> Result<(), UiError> {
        let temp_dir = self.paths().temp_dir;
        clear_directory_contents(&temp_dir).map_err(|error| UiError::from(&error))
    }

    pub(crate) fn clear_user_data(&self) -> Result<(), UiError> {
        let user_data_dir = self.paths().user_data_dir;
        clear_directory_contents(&user_data_dir).map_err(|error| UiError::from(&error))
    }

    pub(crate) fn library_snapshot(&self) -> Option<LibrarySnapshot> {
        self.library.snapshot()
    }

    pub(crate) fn begin_library_refresh(&self, reason: LibraryRefreshReason) -> bool {
        self.library.begin_refresh(reason)
    }

    pub(crate) fn refresh_library(&self, reason: LibraryRefreshReason) -> LibraryRefresh {
        self.library.refresh_blocking(&self.paths(), reason)
    }

    pub(crate) fn abort_library_refresh(&self) -> Option<LibraryRefreshReason> {
        self.library.abort_refresh()
    }

    pub(crate) fn browse_my_workshop_page(&self, page: u32) -> Result<WorkshopPage, UiError> {
        if !self.steam_connected() {
            return Err(UiError::from(&SteamRuntimeError::NotConnected));
        }

        let page = self.fetch_my_workshop_page_connected(page)?;
        // One user-paced page fetch = one discrete snapshot write.
        if !page.items.is_empty() {
            self.write_metadata_snapshot_best_effort();
        }
        Ok(page)
    }

    pub(crate) fn refresh_my_workshop_subscription_counts(
        &self,
        pages: u32,
    ) -> Result<HashMap<PublishedFileId, u64>, UiError> {
        if pages == 0 {
            return Ok(HashMap::new());
        }

        if !self.steam_connected() {
            return Err(UiError::from(&SteamRuntimeError::NotConnected));
        }

        let mut counts = HashMap::new();
        for page in 1..=pages {
            let page = self.fetch_my_workshop_page_connected(page)?;
            counts.extend(subscription_counts_from_items(&page.items));
        }
        // Coalesce the multi-page refresh into a single snapshot write.
        if !counts.is_empty() {
            self.write_metadata_snapshot_best_effort();
        }

        Ok(counts)
    }

    pub(crate) fn resolve_workshop_metadata(
        &self,
        item_ids: &[PublishedFileId],
    ) -> (Vec<WorkshopMetadata>, Vec<PublishedFileId>) {
        let now_unix_seconds = metadata_snapshot::now_unix_seconds();
        let cache = self.workshop_metadata.lock();
        let mut metadata = Vec::new();
        let mut stale = Vec::new();
        for id in item_ids.iter().copied() {
            match cache.get(&id) {
                Some(cached) => {
                    // Stale-while-revalidate: aged entries keep rendering but
                    // are re-queued for the existing background refresh.
                    metadata.push(cached.metadata.clone());
                    if !cached.is_fresh_at(now_unix_seconds) {
                        stale.push(id);
                    }
                }
                None => stale.push(id),
            }
        }
        (metadata, stale)
    }

    pub(crate) fn refresh_workshop_metadata(
        &self,
        item_ids: &[PublishedFileId],
    ) -> Result<Vec<WorkshopMetadata>, UiError> {
        let items = self.fetch_workshop_items(item_ids)?;
        let metadata = self.cache_workshop_items(&items);
        if !metadata.is_empty() {
            self.write_metadata_snapshot_best_effort();
        }
        Ok(metadata)
    }

    /// Like [`Self::refresh_workshop_metadata`], but hands each Workshop query
    /// chunk to `on_batch` as it lands so callers hydrate incrementally after
    /// a single round trip. Each chunk is cached on arrival (making it visible
    /// to every surface); the snapshot is persisted once after the whole
    /// query, preserving the once-per-query write granularity.
    pub(crate) fn refresh_workshop_metadata_streaming(
        &self,
        item_ids: &[PublishedFileId],
        mut on_batch: impl FnMut(Vec<WorkshopMetadata>),
    ) -> Result<(), UiError> {
        if item_ids.is_empty() {
            return Ok(());
        }

        if !self.steam_connected() {
            return Err(UiError::from(&SteamRuntimeError::NotConnected));
        }

        let mut cached_any = false;
        let raw_ids = item_ids.iter().map(|id| id.get()).collect();
        let result =
            steam_workshop::query_workshop_items_streaming(&self.backend.steam, raw_ids, |items| {
                let items = items
                    .into_iter()
                    .map(workshop_item_from_backend)
                    .collect::<Vec<_>>();
                let metadata = self.cache_workshop_items(&items);
                if !metadata.is_empty() {
                    cached_any = true;
                    on_batch(metadata);
                }
            });

        if cached_any {
            self.write_metadata_snapshot_best_effort();
        }
        result.map_err(|error| UiError::from(&error))
    }

    pub(crate) fn workshop_item_details(
        &self,
        id: PublishedFileId,
    ) -> Result<crate::backend::domain::WorkshopItem, UiError> {
        if !self.steam_connected() {
            return Err(UiError::from(&SteamRuntimeError::NotConnected));
        }

        let item = steam_workshop::query_workshop_item_details(&self.backend.steam, id.get())
            .map(workshop_item_from_backend)
            .map_err(|error| UiError::from(&error))?;
        self.cache_workshop_item_details(&item);
        Ok(item)
    }

    pub(crate) fn cached_workshop_item_details(
        &self,
        id: PublishedFileId,
    ) -> Option<WorkshopMetadata> {
        self.workshop_metadata
            .lock()
            .get(&id)
            .map(|cached| cached.metadata.clone())
            .filter(|metadata| metadata.full_description.is_some())
    }

    #[cfg(test)]
    pub(crate) fn steam_user_details(&self, steamid: u64) -> Result<SteamUser, UiError> {
        if !self.steam_connected() {
            return Err(UiError::from(&SteamRuntimeError::NotConnected));
        }

        Ok(steam_user_from_workshop_backend(
            steam_users::fetch_steam_user(&self.backend.steam, steamid),
        ))
    }

    pub(crate) fn steam_user_details_streaming(
        &self,
        steamid: u64,
        mut on_user: impl FnMut(SteamUser),
    ) -> Result<(), UiError> {
        if !self.steam_connected() {
            return Err(UiError::from(&SteamRuntimeError::NotConnected));
        }

        steam_users::fetch_steam_user_streaming(&self.backend.steam, steamid, |user| {
            on_user(steam_user_from_workshop_backend(user));
        });
        Ok(())
    }

    pub(crate) fn steam_connected(&self) -> bool {
        self.steam_runtime.is_connected()
    }

    pub(crate) fn connect_steam(&self) -> Result<(), UiError> {
        self.steam_runtime
            .connect()
            .map_err(|error| UiError::from(&error))
    }

    pub(crate) fn current_steam_user(&self) -> Result<SteamUser, UiError> {
        self.steam_runtime
            .current_user()
            .map(steam_user_from_backend)
            .map_err(|error| UiError::from(&error))
    }

    pub(crate) fn submit_workshop_downloads(
        &self,
        item_ids: Vec<PublishedFileId>,
    ) -> Result<(), UiError> {
        if !self.steam_connected() {
            return Err(UiError::from(&SteamRuntimeError::NotConnected));
        }

        downloads::queue_workshop_downloads(
            &self.backend.downloads,
            item_ids
                .into_iter()
                .map(|id| gmpublished_backend::appdata::SettingsPublishedFileId(id.get())),
        );
        Ok(())
    }

    pub(crate) fn submit_workshop_snapshot(
        &self,
        item_id: PublishedFileId,
        destination: crate::backend::gma::ExtractDestination,
        request_id: u64,
    ) -> Result<(), UiError> {
        if !self.steam_connected() {
            return Err(UiError::from(&SteamRuntimeError::NotConnected));
        }

        downloads::queue_workshop_download_to(
            &self.backend.downloads,
            gmpublished_backend::appdata::SettingsPublishedFileId(item_id.get()),
            destination,
            request_id,
        );
        Ok(())
    }

    pub(crate) fn submit_publish_request(
        &self,
        request: PublishSubmitRequest,
        transaction: &transactions::Transaction,
    ) -> Result<PublishSubmitOutcome, UiError> {
        if !self.steam_connected() {
            transaction.error(&SteamRuntimeError::NotConnected);
            return Err(UiError::from(&SteamRuntimeError::NotConnected));
        }

        let content_source_path = request.content_source_path.clone();
        let submission = publish_submission_from_app_request(request);
        let outcome = steam_publishing::submit_with_transaction(
            submission,
            transaction,
            &self.backend.app_data,
            &self.backend.steam,
            &self.backend.whitelist,
        )
        .map_err(|error| UiError::from(&error))?;
        let outcome = PublishSubmitOutcome {
            published_file_id: PublishedFileId::new(outcome.published_file_id)
                .expect("Steam never issues a zero published file id"),
            legal_agreement_required: outcome.legal_agreement_required,
        };
        self.record_published_local_path(outcome.published_file_id, content_source_path);
        Ok(outcome)
    }

    pub(crate) fn submit_publish_icon_request(
        &self,
        icon_source_path: &PathBuf,
        upscale: bool,
        workshop_id: PublishedFileId,
        transaction: &transactions::Transaction,
    ) -> Result<bool, UiError> {
        if !self.steam_connected() {
            transaction.error(&SteamRuntimeError::NotConnected);
            return Err(UiError::from(&SteamRuntimeError::NotConnected));
        }

        let icon = steam_publishing::WorkshopIcon::new(icon_source_path, upscale)
            .map_err(|error| UiError::from(&error))?;
        self.backend
            .steam
            .update_icon(
                gmpublished_backend::appdata::SettingsPublishedFileId(workshop_id.get()),
                icon,
                transaction,
                &self.backend.app_data,
            )
            .map_err(|error| UiError::from(&error))
    }

    pub(crate) fn search_quick(&self, request: &SearchQuickRequest) -> SearchQuickBatch {
        let result = match request.mode() {
            SearchMode::Addons => self.backend.search.quick_search(request.query().to_owned()),
            SearchMode::Files => self.backend.search.quick_search_with_scope(
                request.query().to_owned(),
                gmpublished_backend::search::SearchScope::Files,
            ),
        };
        search_quick_batch_from_backend(request, &result)
    }

    pub(crate) fn start_search_full(
        &self,
        request: &SearchFullRequest,
        transaction: transactions::Transaction,
    ) -> u32 {
        match request.mode() {
            SearchMode::Addons => self
                .backend
                .search
                .full_with_transaction(request.query().to_owned(), transaction),
            SearchMode::Files => self.backend.search.full_with_transaction_scope(
                request.query().to_owned(),
                gmpublished_backend::search::SearchScope::Files,
                transaction,
            ),
        }
    }

    pub(crate) fn search_full_batch_from_transaction_payload(
        &self,
        request: &SearchFullRequest,
        sequence: u64,
        payload: &TransactionPayload,
    ) -> Result<SearchFullBatch, UiError> {
        search_full_batch_from_transaction_payload(request, sequence, payload)
    }

    pub(crate) fn open_native_target(
        &self,
        target: NativeOpenTarget,
    ) -> Result<(), native::NativeOpenError> {
        native::open_target(target)
    }

    pub(crate) fn subscribe_backend_events(&self) -> Option<BackendRuntimeEventSubscription> {
        self._backend_event_sink
            .as_ref()
            .map(BackendEventSinkRegistration::subscribe)
    }

    fn record_published_local_path(&self, id: PublishedFileId, content_source_path: PathBuf) {
        self.settings
            .lock()
            .my_workshop_local_paths
            .insert(id, content_source_path.clone());
        steam_publishing::record_published_local_path(
            &self.backend.app_data,
            id.get(),
            content_source_path,
        );
    }

    fn fetch_workshop_items(
        &self,
        item_ids: &[PublishedFileId],
    ) -> Result<Vec<WorkshopItem>, UiError> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }

        if !self.steam_connected() {
            return Err(UiError::from(&SteamRuntimeError::NotConnected));
        }

        let raw_ids = item_ids.iter().map(|id| id.get()).collect();
        steam_workshop::query_workshop_items(&self.backend.steam, raw_ids)
            .map(|items| items.into_iter().map(workshop_item_from_backend).collect())
            .map_err(|error| UiError::from(&error))
    }

    fn fetch_my_workshop_page_connected(&self, page: u32) -> Result<WorkshopPage, UiError> {
        let page = steam_workshop::browse_my_workshop_page(
            &self.backend.steam,
            &self.backend.search,
            page,
        )
        .ok_or_else(|| UiError::new(keys::STEAM_ERROR))?;
        let items = page
            .items
            .into_iter()
            .map(workshop_item_from_backend)
            .collect::<Vec<_>>();
        self.cache_workshop_items(&items);

        Ok(WorkshopPage {
            total: page.total_results,
            items,
        })
    }

    pub(super) fn cache_workshop_items(&self, items: &[WorkshopItem]) -> Vec<WorkshopMetadata> {
        let mut metadata = items
            .iter()
            .filter_map(WorkshopMetadata::from_workshop_item)
            .collect::<Vec<_>>();
        if !metadata.is_empty() {
            let fetched_at = metadata_snapshot::now_unix_seconds();
            let mut cache = self.workshop_metadata.lock();
            for item in &mut metadata {
                // Steam never returns a ThumbHash; carry forward one we already
                // computed so a metadata refresh doesn't wipe placeholders.
                if item.thumbhash.is_none()
                    && let Some(existing) = cache.get(&item.id)
                {
                    item.thumbhash.clone_from(&existing.metadata.thumbhash);
                }
                if let Some(existing) = cache.get(&item.id) {
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
        }
        metadata
    }

    pub(super) fn cache_workshop_item_details(&self, item: &WorkshopItem) {
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

    /// Records the ThumbHash a media worker computed for a preview URL into the
    /// live metadata cache. RAM-only: the next ordinary snapshot write (metadata
    /// refresh, page browse) flushes accumulated hashes, keeping this off the UI
    /// thread's I/O path.
    pub(crate) fn record_thumbhash(&self, url: &str, hash: &[u8]) {
        let url = url.trim();
        let mut cache = self.workshop_metadata.lock();
        for cached in cache.values_mut() {
            if cached.metadata.thumbhash.is_none()
                && cached.metadata.preview_url.as_deref().map(str::trim) == Some(url)
            {
                cached.metadata.thumbhash = Some(Arc::from(hash));
            }
        }
    }

    /// Preview-URL/ThumbHash pairs for every cached entry that has both, used to
    /// seed the thumbnail manager so placeholders paint on the first demand.
    pub(crate) fn thumbhash_seed(&self) -> Vec<(String, Arc<[u8]>)> {
        self.workshop_metadata
            .lock()
            .values()
            .filter_map(|cached| {
                let url = cached.metadata.preview_url.as_deref()?;
                Some((url.to_owned(), cached.metadata.thumbhash.clone()?))
            })
            .collect()
    }

    pub(super) fn write_metadata_snapshot_best_effort(&self) {
        let Some(path) = self.metadata_snapshot_file.as_deref() else {
            return;
        };
        let entries = self.workshop_metadata.lock().clone();
        if let Err(error) = metadata_snapshot::write(path, &entries) {
            log::warn!(
                "failed to write Workshop metadata snapshot {}: {error}",
                path.display()
            );
        }
    }

    pub(crate) fn persist_workshop_metadata_cache(&self) {
        self.write_metadata_snapshot_best_effort();
    }

    #[cfg(test)]
    pub(super) fn set_metadata_snapshot_file_for_test(&mut self, path: PathBuf) {
        self.metadata_snapshot_file = Some(path);
    }

    #[cfg(test)]
    pub(super) fn hydrate_workshop_metadata_snapshot_for_test(&self) {
        self.hydrate_workshop_metadata_snapshot();
    }

    #[cfg(test)]
    pub(super) fn set_workshop_metadata_fetched_at_for_test(
        &self,
        id: PublishedFileId,
        fetched_at: u64,
    ) {
        if let Some(cached) = self.workshop_metadata.lock().get_mut(&id) {
            cached.fetched_at = fetched_at;
        }
    }
}

/// Production: the one real `Backend` (background threads included, OS
/// paths). Tests: a throwaway `Backend` on a private tempdir root, with no
/// background threads (no Steam connect attempt, no whitelist network
/// fetch) — so every test is fully isolated.
#[cfg(not(test))]
fn build_default_backend(
    event_sink: Arc<dyn gmpublished_backend::events::BackendEventSink>,
) -> Result<Arc<Backend>, gmpublished_backend::BackendInitError> {
    gmpublished_backend::Backend::init(gmpublished_backend::BackendConfig {
        event_sink,
        ..gmpublished_backend::BackendConfig::default()
    })
}

#[cfg(test)]
fn build_default_backend(
    event_sink: Arc<dyn gmpublished_backend::events::BackendEventSink>,
) -> Result<(Arc<Backend>, tempfile::TempDir), gmpublished_backend::BackendInitError> {
    let root = tempfile::tempdir().expect("test backend tempdir");
    let backend = gmpublished_backend::Backend::init(gmpublished_backend::BackendConfig {
        event_sink,
        ..gmpublished_backend::BackendConfig::for_test(root.path())
    })?;
    Ok((backend, root))
}
