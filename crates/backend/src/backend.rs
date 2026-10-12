use std::{
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::Arc,
};

use crate::{
    appdata::{AppData, AppDataPaths},
    events::{BackendEventSink, NullEventSink},
    execution::{ExecutionConfig, ExecutionResources},
    gma::{
        ExtractDestination, ExtractOptions, ExtractionContext, GmaFile, read::GmaView,
        whitelist::AddonWhitelist,
    },
    search::Search,
    steam::{
        Steam,
        background::{SteamBackgroundRuntime, SteamBackgroundStart},
        downloads::Downloads,
    },
    transactions::Transactions,
};

pub struct BackendConfig {
    pub event_sink: Arc<dyn BackendEventSink>,
    /// Overrides settings, temp, user-data, and download paths. Tests use a private tempdir.
    pub data_root: Option<PathBuf>,
    pub background_services: BackgroundServices,
    /// Logging has one process-wide sink; tests with separate data roots must disable it.
    pub file_logging: bool,
    pub execution: ExecutionConfig,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BackgroundServices {
    #[default]
    Enabled,
    Disabled,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            event_sink: Arc::new(NullEventSink),
            data_root: None,
            background_services: BackgroundServices::Enabled,
            file_logging: true,
            execution: ExecutionConfig::for_machine(),
        }
    }
}

impl BackendConfig {
    #[must_use]
    pub fn for_test(data_root: &std::path::Path) -> Self {
        Self {
            event_sink: Arc::new(NullEventSink),
            data_root: Some(data_root.to_path_buf()),
            background_services: BackgroundServices::Disabled,
            file_logging: false,
            execution: ExecutionConfig {
                cpu_threads: 2,
                blocking_threads: 2,
                network_threads: 2,
                queue_capacity: 64,
            },
        }
    }
}

pub struct Backend {
    execution: ExecutionResources,
    transactions: Transactions,
    app_data: Arc<AppData>,
    steam: Arc<Steam>,
    search: Arc<Search>,
    downloads: Arc<Downloads>,
    whitelist: AddonWhitelist,
    background_runtime: SteamBackgroundRuntime,
}

impl fmt::Debug for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Backend").finish_non_exhaustive()
    }
}

impl Drop for Backend {
    /// A safety net for non-app owners. The app also shuts the runtime down
    /// explicitly because worker-held `Arc<Backend>` clones can outlive its
    /// root model.
    fn drop(&mut self) {
        self.background_runtime.shutdown();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BackendInitError {
    #[error("failed to install backend logger: {0}")]
    LoggerInstall(String),
    #[error(transparent)]
    Execution(#[from] crate::execution::ExecutionInitError),
    #[error("backend initialization stage '{stage}' panicked: {message}")]
    StagePanic {
        stage: &'static str,
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("failed to start backend background services: {source}")]
pub struct BackgroundStartError {
    source: Box<dyn std::error::Error + Send + Sync>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackgroundStartOutcome {
    Started,
    AlreadyRunning,
    Disabled,
    Stopped,
}

impl Backend {
    #[must_use]
    pub fn execution_resources(&self) -> ExecutionResources {
        self.execution.clone()
    }

    #[must_use]
    pub fn cpu_executor(&self) -> &crate::CpuExecutor {
        self.execution.cpu()
    }

    #[must_use]
    pub fn begin_transaction(&self) -> crate::Transaction {
        self.transactions.begin()
    }

    #[must_use]
    pub fn cancel_all_transactions(&self) -> usize {
        self.transactions.cancel_all()
    }

    pub fn cancel_transaction(
        &self,
        id: crate::transactions::TransactionId,
    ) -> Option<crate::transactions::FinalizeOutcome> {
        self.transactions.cancel_by_id(id)
    }

    #[must_use]
    pub fn app_data_snapshot(&self) -> crate::appdata::AppDataSnapshot {
        self.app_data.snapshot()
    }

    pub fn update_settings(
        &self,
        update: impl FnOnce(&mut crate::appdata::Settings) -> crate::appdata::SettingsEnvironment,
    ) -> Result<(), crate::appdata::SettingsError> {
        self.app_data.update_settings(update)
    }

    #[must_use]
    pub fn discover_gmod_dir(&self) -> Option<PathBuf> {
        self.app_data.discover_gmod_dir(&self.steam)
    }

    #[must_use]
    pub fn discover_gmod_dir_for_settings(
        &self,
        configured: Option<&std::path::Path>,
    ) -> Option<PathBuf> {
        self.app_data
            .discover_gmod_dir_for_settings(configured, &self.steam)
    }

    #[must_use]
    pub fn steam_runtime(&self) -> crate::steam::SteamRuntime {
        crate::steam::SteamRuntime::new(Arc::clone(&self.steam))
    }

    pub fn connected_steam(
        &self,
    ) -> Result<crate::steam::ConnectedSteam<'_>, crate::steam::SteamRuntimeError> {
        self.steam.require_client()
    }

    #[must_use]
    pub fn steam_connected(&self) -> bool {
        self.steam.connected()
    }

    pub fn sync_installed_search(
        &self,
        addons: Vec<crate::search::SearchItem>,
        files: Vec<crate::search::SearchItem>,
    ) {
        self.search.sync_installed_addons(addons);
        self.search.sync_installed_addon_files(files);
    }

    #[must_use]
    pub fn quick_search(
        &self,
        query: String,
        scope: crate::search::SearchScope,
    ) -> crate::search::QuickSearchResult {
        self.search.quick_search_with_scope(query, scope)
    }

    pub fn full_search(
        &self,
        query: String,
        scope: crate::search::SearchScope,
        transaction: crate::Transaction,
    ) -> crate::transactions::TransactionId {
        self.search
            .full_with_transaction_scope(query, scope, transaction)
    }

    pub fn cancel_all_downloads(&self) {
        self.downloads.cancel_all();
    }

    pub fn queue_workshop_downloads(
        &self,
        ids: impl IntoIterator<Item = crate::WorkshopId>,
    ) -> Result<(), crate::steam::SteamRuntimeError> {
        crate::steam::downloads::queue_workshop_downloads(&self.downloads, ids)
    }

    pub fn queue_workshop_download_to(
        &self,
        item: crate::WorkshopId,
        destination: ExtractDestination,
        request_id: crate::events::WorkshopSnapshotId,
    ) -> Result<(), crate::steam::SteamRuntimeError> {
        crate::steam::downloads::queue_workshop_download_to(
            &self.downloads,
            item,
            destination,
            request_id,
        )
    }

    #[must_use]
    pub fn workshop_language(&self) -> crate::WorkshopLanguage {
        self.steam.workshop_language()
    }
    pub fn set_workshop_language(&self, language: crate::WorkshopLanguage) -> bool {
        if !self.steam.set_workshop_language(language) {
            return false;
        }
        self.search.invalidate_workshop_titles();
        true
    }
    #[must_use]
    pub fn browse_my_workshop_page(
        &self,
        page: u32,
        language: crate::WorkshopLanguage,
    ) -> Option<crate::steam::workshop::WorkshopPage> {
        crate::steam::workshop::browse_my_workshop_page(&self.steam, &self.search, page, language)
    }

    #[must_use]
    pub fn whitelist_snapshot(&self) -> Arc<[String]> {
        self.whitelist.snapshot()
    }

    pub fn refresh_whitelist(&self) {
        self.whitelist.refresh_from_remote();
    }

    pub fn submit_publish(
        &self,
        submission: crate::steam::publishing::PublishSubmission,
        transaction: &crate::Transaction,
    ) -> Result<
        crate::steam::publishing::PublishSubmissionOutcome,
        crate::steam::publishing::PublishError,
    > {
        crate::steam::publishing::submit_with_transaction(
            submission,
            transaction,
            &self.app_data,
            &self.steam,
            &self.whitelist,
            self.execution.cpu(),
        )
    }

    pub fn update_publish_description(
        &self,
        workshop_id: crate::WorkshopId,
        description: &str,
        transaction: &crate::Transaction,
    ) -> Result<bool, crate::steam::publishing::PublishError> {
        self.update_publish_metadata(
            workshop_id,
            crate::WorkshopLanguage::English,
            None,
            Some(description),
            transaction,
        )
    }

    pub fn update_publish_metadata(
        &self,
        workshop_id: crate::WorkshopId,
        language: crate::WorkshopLanguage,
        title: Option<&str>,
        description: Option<&str>,
        transaction: &crate::Transaction,
    ) -> Result<bool, crate::publishing::PublishError> {
        self.steam.require_client()?.update_metadata(
            workshop_id,
            language,
            title,
            description,
            transaction,
        )
    }

    pub fn update_publish_icon(
        &self,
        workshop_id: crate::WorkshopId,
        icon: crate::steam::publishing::WorkshopIcon,
        transaction: &crate::Transaction,
    ) -> Result<bool, crate::steam::publishing::PublishError> {
        self.steam
            .require_client()?
            .update_icon(workshop_id, icon, transaction, &self.app_data)
    }

    pub fn extract_gma_entry(
        &self,
        view: &GmaView,
        gma: &GmaFile,
        entry_path: String,
        transaction: &crate::Transaction,
        options: ExtractOptions,
    ) -> Result<PathBuf, crate::GmaError> {
        view.extract_entry(
            gma,
            entry_path,
            transaction,
            options,
            &self.app_data,
            &self.steam,
        )
    }

    pub fn extract_gma(
        &self,
        view: &GmaView,
        gma: &GmaFile,
        transaction: &crate::Transaction,
        context: ExtractionContext,
    ) -> Result<PathBuf, crate::GmaError> {
        view.extract(gma, transaction, context, self.execution.cpu())
    }

    #[cfg(feature = "test-support")]
    pub fn emit_test_event(&self, event: crate::events::BackendEvent) {
        self.transactions.emit(event);
    }

    #[cfg(feature = "test-support")]
    pub fn clear_search_for_test(&self) {
        self.search.clear();
    }

    /// Construction leaves background work dormant until start_background_services is called.
    pub fn init(config: BackendConfig) -> Result<Arc<Self>, BackendInitError> {
        let BackendConfig {
            event_sink,
            data_root,
            background_services,
            file_logging,
            execution,
        } = config;

        initialize_stage("logging", || {
            crate::logging::install()
                .map_err(|error| BackendInitError::LoggerInstall(error.to_string()))
        })??;

        let paths = data_root
            .as_ref()
            .map_or_else(AppDataPaths::production, |root| {
                AppDataPaths::for_test_root(root)
            });

        let transactions = Transactions::new(event_sink);
        let execution = initialize_stage("execution", || ExecutionResources::build(execution))??;

        log::info!("initializing appdata");
        let app_data = initialize_stage("appdata", || {
            Arc::new(AppData::load(paths, transactions.clone()))
        })?;
        if file_logging {
            crate::logging::enable_file_sink(app_data.logging_logs_dir());
        }

        log::info!("initializing steamworks");
        let steam = initialize_stage("steamworks", || Arc::new(Steam::new(transactions.clone())))?;

        log::info!("initializing search");
        let search = initialize_stage("search", || Arc::new(Search::new(execution.cpu().clone())))?;

        let whitelist = AddonWhitelist::new();

        let downloads = initialize_stage("downloads", || {
            Arc::new(Downloads::new(
                Arc::clone(&app_data),
                Arc::clone(&steam),
                whitelist.clone(),
                transactions.clone(),
                execution.clone(),
            ))
        })?;

        let backend = Arc::new(Self {
            execution,
            transactions,
            app_data,
            steam,
            search,
            downloads,
            whitelist,
            background_runtime: SteamBackgroundRuntime::new(matches!(
                background_services,
                BackgroundServices::Enabled
            )),
        });

        Ok(backend)
    }

    pub fn resolve_extraction(
        &self,
        handle: &GmaFile,
        destination: ExtractDestination,
        options: ExtractOptions,
    ) -> Result<ExtractionContext, crate::GmaError> {
        ExtractionContext::resolve(
            handle,
            destination,
            options,
            &self.whitelist,
            &self.app_data,
            &self.steam,
        )
    }

    pub fn start_background_services(
        self: &Arc<Self>,
    ) -> Result<BackgroundStartOutcome, BackgroundStartError> {
        let outcome = self
            .background_runtime
            .start(
                Arc::clone(&self.steam),
                Arc::clone(&self.app_data),
                Arc::clone(&self.search),
                Arc::clone(&self.downloads),
            )
            .map_err(|error| BackgroundStartError {
                source: Box::new(error),
            })?;
        let outcome = match outcome {
            SteamBackgroundStart::Started => BackgroundStartOutcome::Started,
            SteamBackgroundStart::AlreadyRunning => BackgroundStartOutcome::AlreadyRunning,
            SteamBackgroundStart::Disabled => BackgroundStartOutcome::Disabled,
            SteamBackgroundStart::Stopped => BackgroundStartOutcome::Stopped,
        };
        if outcome != BackgroundStartOutcome::Started {
            return Ok(outcome);
        }

        log::info!("warming GMA whitelist");
        let whitelist = self.whitelist.clone();
        if let Err(error) = self
            .execution
            .spawn_network("GMA whitelist warm-up", move || {
                whitelist.refresh_from_remote();
            })
        {
            log::warn!("could not schedule the GMA whitelist warm-up: {error}");
        }
        Ok(BackgroundStartOutcome::Started)
    }

    /// Cooperatively stops every process-lifetime service and joins its
    /// workers under one bounded deadline. Idempotent.
    pub fn shutdown_background_services(&self) {
        self.background_runtime.shutdown();
    }
}

fn initialize_stage<T>(
    stage: &'static str,
    init: impl FnOnce() -> T,
) -> Result<T, BackendInitError> {
    catch_unwind(AssertUnwindSafe(init)).map_err(|panic| BackendInitError::StagePanic {
        stage,
        message: crate::panic_payload_message(&panic),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_builds_every_service_with_a_private_test_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let backend = Backend::init(BackendConfig::for_test(temp.path())).expect("backend init");

        assert!(!backend.steam.connected());
        assert_eq!(backend.app_data.gmod_dir(), None);
    }

    #[test]
    fn init_is_independent_across_instances() {
        let temp_a = tempfile::tempdir().expect("tempdir");
        let temp_b = tempfile::tempdir().expect("tempdir");
        let backend_a = Backend::init(BackendConfig::for_test(temp_a.path())).expect("backend a");
        let backend_b = Backend::init(BackendConfig::for_test(temp_b.path())).expect("backend b");

        backend_a.app_data.mutate_settings(|settings| {
            settings.language = Some("en-US".to_owned());
        });

        assert_eq!(
            backend_a.app_data.settings().language.as_deref(),
            Some("en-US")
        );
        assert_eq!(backend_b.app_data.settings().language, None);
    }

    #[test]
    fn disabled_background_services_never_start() {
        let temp = tempfile::tempdir().expect("tempdir");
        let backend = Backend::init(BackendConfig::for_test(temp.path())).expect("backend init");

        assert_eq!(
            backend.start_background_services().expect("disabled start"),
            BackgroundStartOutcome::Disabled
        );
        assert_eq!(
            backend
                .start_background_services()
                .expect("repeated disabled start"),
            BackgroundStartOutcome::Disabled
        );
    }
}
