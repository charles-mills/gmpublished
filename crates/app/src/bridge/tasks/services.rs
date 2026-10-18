use gmpublished_backend::TransactionId;
use std::sync::Arc;

#[cfg(test)]
use super::WorkshopMetadata;
use super::{
    BackendEventSinkRegistration, BackendRuntimeEventSubscription, CachedWorkshopMetadata,
    LibraryStore, NativeOpenTarget, PublishedFileId, UiError, native,
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

#[derive(Debug)]
pub struct BackendServices {
    backend: Arc<Backend>,
    configuration: ConfigStore,
    steam_runtime: SteamRuntime,
    library: LibraryStore,
    workshop_metadata: Mutex<WorkshopMetadataCache>,
    metadata_writer: Mutex<u64>,
    metadata_snapshot_file: Option<PathBuf>,
    text_journal: Mutex<crate::bridge::workshop_text::TextJournal>,
    _backend_event_sink: Option<BackendEventSinkRegistration>,
    #[cfg(test)]
    _test_data_root: Option<tempfile::TempDir>,
}

#[derive(Debug, Default)]
struct WorkshopMetadataCache {
    entries: HashMap<PublishedFileId, CachedWorkshopMetadata>,
    revision: u64,
}

impl WorkshopMetadataCache {
    fn changed(&mut self) {
        self.revision += 1;
    }
}

fn steam_not_connected() -> UiError {
    UiError::from(&SteamRuntimeError::NotConnected)
}

pub(super) struct BackendServicesConfig {
    persist_settings: bool,
    /// Where the Workshop metadata snapshot lives; `None` keeps it in memory.
    metadata_snapshot_file: Option<PathBuf>,
    /// Where the library header snapshot lives; `None` keeps it in memory.
    header_snapshot_file: Option<PathBuf>,
}

impl BackendServicesConfig {
    #[cfg(not(test))]
    fn production() -> Self {
        let cache_dir = gmpublished_backend::cache_dir();
        Self {
            persist_settings: true,
            metadata_snapshot_file: cache_dir.as_ref().map(|dir| dir.join("metadata.snap")),
            header_snapshot_file: cache_dir.map(|dir| dir.join("library-headers.json")),
        }
    }

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
        #[cfg(not(test))]
        let config = BackendServicesConfig::production();
        #[cfg(test)]
        let config = BackendServicesConfig::isolated();
        let services = Self::with_steam_runtime(backend, steam_runtime, backend_event_sink, config);
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

        let journal_path = config.persist_settings.then(|| {
            configuration
                .snapshot()
                .paths
                .default_user_data_dir
                .join("workshop-text-edits.json")
        });
        let text_journal = Mutex::new(crate::bridge::workshop_text::TextJournal::load(
            journal_path,
        ));
        Self {
            text_journal,
            backend,
            configuration,
            steam_runtime,
            library,
            workshop_metadata: Mutex::new(WorkshopMetadataCache::default()),
            metadata_writer: Mutex::new(0),
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

    /// Enables real settings persistence under the test's private data root.
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

    /// Uses the supplied event sink so tests can observe backend events directly.
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

    pub(crate) fn begin_transaction_with(
        &self,
        initialize: impl FnOnce(&Transaction),
    ) -> Transaction {
        self.backend.begin_transaction_with(initialize)
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
        query: &str,
    ) -> gmpublished_backend::QuickSearchResult {
        self.backend
            .quick_search(query, gmpublished_backend::SearchScope::Addons)
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
) -> bool {
    let hash: Arc<[u8]> = Arc::from(hash);
    let mut changed = false;
    for cached in cache.values_mut().filter(|cached| {
        cached.metadata.thumbhash.is_none()
            && cached.metadata.preview_url.as_deref().map(str::trim) == Some(url)
    }) {
        cached.metadata.thumbhash = Some(Arc::clone(&hash));
        changed = true;
    }
    changed
}

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

fn backend_workshop_ids(ids: &[PublishedFileId]) -> Vec<gmpublished_backend::WorkshopId> {
    ids.iter().copied().map(Into::into).collect()
}

fn backend_steam_id(id: SteamId) -> gmpublished_backend::SteamId {
    gmpublished_backend::SteamId::from_raw(id.get())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constructor_keeps_caches_and_settings_in_memory() {
        let services = BackendServices::new(None).unwrap();
        let settings_file = services.app_data_snapshot_for_test().paths.settings_file;
        assert!(services.metadata_snapshot_file.is_none());
        assert!(!settings_file.exists());

        services
            .update_settings_snapshot(|settings| {
                settings.backend.sounds = !settings.backend.sounds;
            })
            .unwrap();
        assert!(!settings_file.exists());
    }

    #[test]
    fn a_decoded_hash_updates_every_item_sharing_the_preview_url() {
        let mut cache = (1..=2)
            .map(|raw_id| {
                let id = PublishedFileId::fixture(raw_id);
                (
                    id,
                    CachedWorkshopMetadata {
                        language: Default::default(),
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

        assert!(record_thumbhash_in_cache(
            &mut cache,
            "https://example.test/shared.jpg",
            &[1, 2, 3]
        ));

        assert_eq!(
            cache
                .values()
                .filter(|cached| cached.metadata.thumbhash.is_some())
                .count(),
            2
        );
    }
}
