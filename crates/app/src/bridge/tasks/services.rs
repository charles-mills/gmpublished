use gmpublished_backend::TransactionId;
use std::sync::Arc;

#[cfg(test)]
use super::WorkshopMetadata;
use super::{
    BackendEventSinkRegistration, BackendRuntimeEventSubscription, CachedWorkshopMetadata,
    LibraryStore, NativeOpenTarget, PublishedFileId, UiError, library, metadata_snapshot, native,
};
use crate::bridge::config_store::ConfigStore;
use crate::bridge::domain::SteamId;
#[cfg(test)]
use gmpublished_backend::AppDataSnapshot as BackendAppDataSnapshot;
use gmpublished_backend::Backend;
use gmpublished_backend::SteamRuntime;
use gmpublished_backend::SteamRuntimeError;
use gmpublished_backend::Transaction;
use parking_lot::Mutex;
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

mod archive_service;
mod config_service;
mod library_service;
mod publish_service;
mod search_service;
mod workshop_service;

pub use archive_service::ArchiveService;
pub use config_service::ConfigService;
pub use library_service::LibraryService;
pub use publish_service::PublishService;
pub use search_service::SearchService;
pub use workshop_service::WorkshopService;

/// App composition root. Features receive one of its narrow borrowed service
/// capabilities; scheduler and lifecycle code alone retain this registry.
#[derive(Debug)]
pub struct BackendServices {
    backend: Arc<Backend>,
    configuration: ConfigStore,
    steam_runtime: SteamRuntime,
    library: LibraryStore,
    workshop_metadata: Mutex<HashMap<PublishedFileId, CachedWorkshopMetadata>>,
    metadata_snapshot_file: Option<PathBuf>,
    _backend_event_sink: Option<BackendEventSinkRegistration>,
    #[cfg(test)]
    _test_data_root: Option<tempfile::TempDir>,
}

/// The error every Steam-facing call returns when it finds no connection.
fn steam_not_connected() -> UiError {
    UiError::from(&SteamRuntimeError::NotConnected)
}

/// What a [`BackendServices`] needs to know about its environment.
///
/// Production and tests both build one and call the same constructor. Deciding
/// it with `cfg!(test)` inside would run different code on the two paths,
/// making a production-only bug invisible to the suite.
pub(super) struct BackendServicesConfig {
    /// Whether settings changes are written back to the backend's store.
    persist_settings: bool,
    /// Where the Workshop metadata snapshot lives; `None` keeps it in memory.
    metadata_snapshot_file: Option<PathBuf>,
    /// Where the library header snapshot lives; `None` keeps it in memory.
    header_snapshot_file: Option<PathBuf>,
}

impl BackendServicesConfig {
    /// Reads and writes the real user directories.
    fn production() -> Self {
        Self {
            persist_settings: true,
            metadata_snapshot_file: metadata_snapshot::snapshot_path(),
            header_snapshot_file: library::header_snapshot_path(),
        }
    }

    /// Touches no directory the developer also uses, and persists nothing —
    /// many isolated roots coexist in one test process.
    #[cfg(test)]
    fn isolated() -> Self {
        Self {
            persist_settings: false,
            metadata_snapshot_file: None,
            header_snapshot_file: None,
        }
    }
}

impl BackendServices {
    #[must_use]
    pub(crate) fn config(&self) -> ConfigService<'_> {
        ConfigService::new(self)
    }

    #[must_use]
    pub(crate) fn library(&self) -> LibraryService<'_> {
        LibraryService::new(self)
    }

    #[must_use]
    pub(crate) fn workshop(&self) -> WorkshopService<'_> {
        WorkshopService::new(self)
    }

    #[must_use]
    pub(crate) fn publish(&self) -> PublishService<'_> {
        PublishService::new(self)
    }

    #[must_use]
    pub(crate) fn search(&self) -> SearchService<'_> {
        SearchService::new(self)
    }

    #[must_use]
    pub(crate) fn archive(&self) -> ArchiveService<'_> {
        ArchiveService::new(self)
    }

    /// The default entry point every `App::new()` (production) or
    /// `BackendContext::new()` (tests) goes through. Builds one `Backend`
    /// (with explicitly-started background services in production; a
    /// private-tempdir, no-background-services one in tests —
    /// [`build_default_backend`]) and derives the initial settings/paths from
    /// its `AppData` snapshot.
    pub(super) fn new(
        backend_event_sink: Option<BackendEventSinkRegistration>,
    ) -> Result<Self, gmpublished_backend::BackendInitError> {
        let event_sink = backend_event_sink.as_ref().map_or_else(
            || Arc::new(gmpublished_backend::NullEventSink) as _,
            BackendEventSinkRegistration::sink,
        );
        #[cfg(not(test))]
        let backend = build_default_backend(event_sink)?;
        #[cfg(test)]
        let (backend, test_data_root) = build_default_backend(event_sink)?;
        let steam_runtime = backend.steam_runtime();
        let services = Self::with_steam_runtime(
            backend,
            steam_runtime,
            backend_event_sink,
            BackendServicesConfig::production(),
        );
        #[cfg(test)]
        let services = {
            let mut services = services;
            services._test_data_root = Some(test_data_root);
            services
        };
        Ok(services)
    }

    fn with_steam_runtime(
        backend: Arc<Backend>,
        steam_runtime: SteamRuntime,
        backend_event_sink: Option<BackendEventSinkRegistration>,
        config: BackendServicesConfig,
    ) -> Self {
        let configuration = ConfigStore::from_backend(&backend, config.persist_settings);

        let library = LibraryStore::new(backend.execution_resources());
        if let Some(path) = config.header_snapshot_file {
            library.set_header_snapshot_file(path);
        }

        Self {
            backend,
            configuration,
            steam_runtime,
            library,
            workshop_metadata: Mutex::new(HashMap::new()),
            metadata_snapshot_file: config.metadata_snapshot_file,
            _backend_event_sink: backend_event_sink,
            #[cfg(test)]
            _test_data_root: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self::for_test_with_event_sink(Arc::new(gmpublished_backend::NullEventSink))
    }

    /// Like [`Self::for_test`], but with the real `AppData`-backed settings
    /// persistence path enabled (disabled by default in tests), so a test
    /// can exercise `update_settings_snapshot`'s actual disk write and its
    /// failure propagation.
    #[cfg(test)]
    pub(crate) fn for_test_with_settings_persist_enabled() -> Self {
        let (backend, test_data_root) =
            build_default_backend(Arc::new(gmpublished_backend::NullEventSink))
                .expect("test backend init");
        let mut services = Self::with_steam_runtime(
            backend,
            SteamRuntime::unavailable_for_tests(),
            None,
            BackendServicesConfig {
                persist_settings: true,
                ..BackendServicesConfig::isolated()
            },
        );
        services._test_data_root = Some(test_data_root);
        services
    }

    /// Like [`Self::for_test`], but with an explicit event sink (a
    /// `BackendEventCollector`, typically) so the test can observe events
    /// the backend emits directly, without going through a `BackendContext`.
    #[cfg(test)]
    pub(crate) fn for_test_with_event_sink(
        event_sink: Arc<dyn gmpublished_backend::BackendEventSink>,
    ) -> Self {
        let (backend, test_data_root) =
            build_default_backend(event_sink).expect("test backend init");
        let mut services = Self::with_steam_runtime(
            backend,
            SteamRuntime::unavailable_for_tests(),
            None,
            BackendServicesConfig::isolated(),
        );
        services._test_data_root = Some(test_data_root);
        services
    }

    pub(crate) fn begin_transaction(&self) -> Transaction {
        self.backend.begin_transaction()
    }

    pub(crate) fn cpu_executor(&self) -> &gmpublished_backend::CpuExecutor {
        self.backend.cpu_executor()
    }

    pub(super) fn execution_resources(&self) -> gmpublished_backend::ExecutionResources {
        self.backend.execution_resources()
    }

    #[cfg(test)]
    pub(super) fn for_test_with_data_root(root: &Path) -> Self {
        let backend = gmpublished_backend::Backend::init(gmpublished_backend::BackendConfig {
            event_sink: Arc::new(gmpublished_backend::NullEventSink),
            ..gmpublished_backend::BackendConfig::for_test(root)
        })
        .expect("test backend init");
        Self::with_steam_runtime(
            backend,
            SteamRuntime::unavailable_for_tests(),
            None,
            BackendServicesConfig {
                persist_settings: true,
                ..BackendServicesConfig::isolated()
            },
        )
    }

    /// Stops every background service this process owns, for app exit.
    ///
    /// Transactions are cancelled before Steam is shut down so a job still
    /// running gets the chance to stop at its own next checkpoint rather than
    /// being abandoned mid-write. Returns how many were cancelled.
    pub(crate) fn shutdown(&self) -> usize {
        let cancelled = self.backend.cancel_all_transactions();
        self.backend.shutdown_background_services();
        cancelled
    }

    pub(super) fn start_background_services(
        &self,
    ) -> Result<
        gmpublished_backend::BackgroundStartOutcome,
        gmpublished_backend::BackgroundStartError,
    > {
        self.backend.start_background_services()
    }

    pub(super) fn cancel_all_downloads(&self) {
        self.backend.cancel_all_downloads();
    }

    pub(super) fn cancel_transaction(
        &self,
        transaction_id: TransactionId,
    ) -> Option<gmpublished_backend::FinalizeOutcome> {
        self.backend.cancel_transaction(transaction_id)
    }

    #[cfg(test)]
    pub(super) fn app_data_snapshot_for_test(&self) -> BackendAppDataSnapshot {
        self.backend.app_data_snapshot()
    }

    #[cfg(test)]
    pub(super) fn emit_backend_event_for_test(&self, event: gmpublished_backend::BackendEvent) {
        self.backend.emit_test_event(event);
    }

    #[cfg(test)]
    pub(super) fn clear_search_for_test(&self) {
        self.backend.clear_search_for_test();
    }

    #[cfg(test)]
    pub(super) fn quick_addon_search_for_test(
        &self,
        query: String,
    ) -> gmpublished_backend::QuickSearchResult {
        self.backend
            .quick_search(query, gmpublished_backend::SearchScope::Addons)
    }

    #[cfg(test)]
    pub(super) fn extract_gma_for_test(
        &self,
        view: &gmpublished_backend::GmaView,
        gma: &gmpublished_backend::GmaFile,
        destination: gmpublished_backend::ExtractDestination,
        options: gmpublished_backend::ExtractOptions,
        transaction: &Transaction,
    ) -> Result<PathBuf, gmpublished_backend::GmaError> {
        let extraction = self.backend.resolve_extraction(gma, destination, options)?;
        self.backend.extract_gma(view, gma, transaction, extraction)
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

    #[cfg(test)]
    pub(super) fn set_metadata_snapshot_file_for_test(&mut self, path: PathBuf) {
        self.metadata_snapshot_file = Some(path);
    }
}

fn record_thumbhash_in_cache(
    cache: &mut HashMap<PublishedFileId, CachedWorkshopMetadata>,
    url: &str,
    hash: &[u8],
) {
    if let Some(cached) = cache.values_mut().find(|cached| {
        cached.metadata.thumbhash.is_none()
            && cached.metadata.preview_url.as_deref().map(str::trim) == Some(url)
    }) {
        cached.metadata.thumbhash = Some(Arc::from(hash));
    }
}

#[cfg(test)]
#[expect(
    clippy::items_after_test_module,
    reason = "production/test backend constructors remain adjacent below the performance-only module"
)]
mod performance_tests {
    use super::*;

    #[test]
    fn one_hash_per_shared_preview_url_is_sufficient() {
        let mut cache = (1..=2)
            .map(|raw_id| {
                let id = PublishedFileId::fixture(raw_id);
                (
                    id,
                    CachedWorkshopMetadata {
                        metadata: WorkshopMetadata {
                            id,
                            title: String::new(),
                            time_created: 0,
                            time_updated: 0,
                            score: 0.0,
                            tags: Vec::new(),
                            preview_url: Some("https://example.test/shared.jpg".to_owned()),
                            subscriptions: 0,
                            full_description: None,
                            owner_steamid: None,
                            thumbhash: None,
                        },
                        fetched_at: 0,
                    },
                )
            })
            .collect::<HashMap<_, _>>();

        record_thumbhash_in_cache(&mut cache, "https://example.test/shared.jpg", &[1, 2, 3]);

        assert_eq!(
            cache
                .values()
                .filter(|cached| cached.metadata.thumbhash.is_some())
                .count(),
            1
        );
    }
}

/// Production: the one real `Backend` (background threads included, OS
/// paths). Tests: a throwaway `Backend` on a private tempdir root, with no
/// background threads (no Steam connect attempt, no whitelist network
/// fetch) — so every test is fully isolated.
#[cfg(not(test))]
fn build_default_backend(
    event_sink: Arc<dyn gmpublished_backend::BackendEventSink>,
) -> Result<Arc<Backend>, gmpublished_backend::BackendInitError> {
    gmpublished_backend::Backend::init(gmpublished_backend::BackendConfig {
        event_sink,
        ..gmpublished_backend::BackendConfig::default()
    })
}

#[cfg(test)]
fn build_default_backend(
    event_sink: Arc<dyn gmpublished_backend::BackendEventSink>,
) -> Result<(Arc<Backend>, tempfile::TempDir), gmpublished_backend::BackendInitError> {
    let root = tempfile::tempdir().expect("test backend tempdir");
    let backend = gmpublished_backend::Backend::init(gmpublished_backend::BackendConfig {
        event_sink,
        ..gmpublished_backend::BackendConfig::for_test(root.path())
    })?;
    Ok((backend, root))
}

/// The backend's id types, built at the one place the app hands ids across.
fn backend_workshop_id(id: PublishedFileId) -> gmpublished_backend::WorkshopId {
    id.into()
}

fn backend_workshop_ids(ids: &[PublishedFileId]) -> Vec<gmpublished_backend::WorkshopId> {
    ids.iter().copied().map(backend_workshop_id).collect()
}

fn backend_steam_id(id: SteamId) -> gmpublished_backend::SteamId {
    gmpublished_backend::SteamId::from_raw(id.get())
}
