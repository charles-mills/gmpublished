#[cfg(test)]
use super::SettingsPersistError;
use super::{
    AppPaths, AppWorkerRuntime, Arc, BACKEND_EVENT_QUEUE_CAPACITY, BackendAppDataSnapshot,
    BackendEventSinkRegistration, BackendEventStreamFactory, BackendRuntimeEvent,
    BackendRuntimeEventEffects, BackendServices, BackendTaskCancelResult, BackendTaskSource,
    BackendTransactionTasks, LibraryRefresh, LibraryRefreshReason, LibrarySnapshot,
    NativeOpenTarget, PathBuf, RunBlockingError, ScheduleError, Settings, Subscription, Task,
    TaskEvent, TaskEventStreamFactory, TaskHandle, TaskId, TaskKind, Tasks, TransactionStatus,
    UiError, WorkerPoolSpawner, fmt, install_backend_event_sink_by_default, mpsc, oneshot,
    show_native_open_error_dialog,
};
use gmpublished_backend::transactions::TransactionId;

/// Root-owned backend boundary cloned into Iced workers and subscriptions.
#[derive(Clone)]
pub struct BackendContext {
    pub(super) services: Arc<BackendServices>,
    runtime: Arc<AppWorkerRuntime>,
    tasks: Arc<Tasks>,
    pub(super) transaction_tasks: Arc<BackendTransactionTasks>,
    pub(super) task_events: TaskEventStreamFactory,
    pub(super) backend_events: BackendEventStreamFactory,
}

impl BackendContext {
    pub(crate) fn new() -> Result<Self, gmpublished_backend::BackendInitError> {
        Self::with_backend_event_sink(install_backend_event_sink_by_default())
    }

    fn with_backend_event_sink(
        install_backend_event_sink: bool,
    ) -> Result<Self, gmpublished_backend::BackendInitError> {
        Self::with_backend_event_sink_and_services(install_backend_event_sink, BackendServices::new)
    }

    fn with_backend_event_sink_and_services(
        install_backend_event_sink: bool,
        services: impl FnOnce(
            Option<BackendEventSinkRegistration>,
        ) -> Result<BackendServices, gmpublished_backend::BackendInitError>,
    ) -> Result<Self, gmpublished_backend::BackendInitError> {
        let runtime = Arc::new(AppWorkerRuntime::new());
        let transaction_tasks = Arc::new(BackendTransactionTasks::default());
        let (backend_event_sender, backend_event_receiver) =
            mpsc::sync_channel(BACKEND_EVENT_QUEUE_CAPACITY);
        let backend_event_sink = install_backend_event_sink
            .then(|| BackendEventSinkRegistration::new(backend_event_sender));
        let services = Arc::new(services(backend_event_sink)?);
        let (tasks, receiver) = Tasks::channel();
        let task_events = TaskEventStreamFactory::new(Some(receiver));
        let backend_events = BackendEventStreamFactory::new(Some(backend_event_receiver));

        Ok(Self {
            services,
            runtime,
            tasks: Arc::new(tasks),
            transaction_tasks,
            task_events,
            backend_events,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_with_backend_event_sink_for_test() -> Self {
        Self::with_backend_event_sink(true).expect("test backend context")
    }

    pub(crate) fn run_blocking<T: Send + 'static>(
        &self,
        name: impl Into<Arc<str>>,
        job: impl FnOnce(&BackendServices) -> T + Send + 'static,
    ) -> Task<Result<T, RunBlockingError>> {
        self.run_worker_pool(name, job, AppWorkerRuntime::spawn_blocking_job)
    }

    /// [`Self::run_blocking`] for a job that is itself fallible.
    ///
    /// Flattens at the boundary instead of at each call site. A job that fails
    /// and a job that never got scheduled both arrive as one `UiError`, which
    /// is the point: the nested `Result` made "discard the scheduling error"
    /// the shortest thing to write, and several call sites took it — turning a
    /// failure to schedule into a result indistinguishable from success.
    pub(crate) fn run_blocking_ui<T: Send + 'static>(
        &self,
        name: impl Into<Arc<str>>,
        job: impl FnOnce(&BackendServices) -> Result<T, UiError> + Send + 'static,
    ) -> Task<Result<T, UiError>> {
        self.run_blocking(name, job).map(|result| match result {
            Ok(inner) => inner,
            Err(error) => Err(UiError::from(&error)),
        })
    }

    pub(crate) fn run_blocking_media<T: Send + 'static>(
        &self,
        name: impl Into<Arc<str>>,
        job: impl FnOnce(&BackendServices) -> T + Send + 'static,
    ) -> Task<Result<T, RunBlockingError>> {
        self.run_worker_pool(name, job, AppWorkerRuntime::spawn_media_job)
    }

    fn run_worker_pool<T: Send + 'static>(
        &self,
        name: impl Into<Arc<str>>,
        job: impl FnOnce(&BackendServices) -> T + Send + 'static,
        spawn: WorkerPoolSpawner,
    ) -> Task<Result<T, RunBlockingError>> {
        let services = Arc::clone(&self.services);
        let (sender, receiver) = oneshot::channel();

        match spawn(
            self.runtime.as_ref(),
            name.into(),
            Box::new(move || {
                let _send_result = sender.send(job(&services));
            }),
        ) {
            Ok(()) => {
                Task::future(
                    async move { receiver.await.map_err(|_| RunBlockingError::WorkerDropped) },
                )
            }
            Err(error) => Task::done(Err(RunBlockingError::Schedule(error))),
        }
    }

    pub(crate) fn spawn_blocking_detached(
        &self,
        name: impl Into<Arc<str>>,
        job: impl FnOnce(Arc<BackendServices>) + Send + 'static,
    ) -> Result<(), ScheduleError> {
        let services = Arc::clone(&self.services);
        self.runtime.spawn_blocking(name, move || job(services))
    }

    pub(crate) fn open_native_target_detached(
        &self,
        name: impl Into<Arc<str>>,
        target: NativeOpenTarget,
    ) -> Result<(), ScheduleError> {
        self.spawn_blocking_detached(name, move |services| {
            if let Err(error) = services.open_native_target(target) {
                show_native_open_error_dialog(error.to_string());
            }
        })
    }

    pub(crate) fn play_gifs_by_default(&self) -> bool {
        self.services.settings_snapshot().ui.play_gifs_by_default
    }

    pub(crate) fn sounds_enabled(&self) -> bool {
        self.services.settings_snapshot().backend.sounds
    }

    pub(crate) fn settings_and_paths_snapshot(&self) -> (Settings, AppPaths) {
        self.services.settings_and_paths_snapshot()
    }

    /// The configured Garry's Mod path paired with the one that resolved.
    pub(crate) fn game_paths(&self) -> (Option<PathBuf>, Option<PathBuf>) {
        self.services.game_paths()
    }

    pub(crate) fn begin_transaction(&self) -> gmpublished_backend::Transaction {
        self.services.begin_transaction()
    }

    /// The whole backend, for this module's own tests.
    ///
    /// No production caller: reaching a service through here would bypass
    /// [`BackendServices`], so what the app can ask the backend for stays
    /// enumerable as the capability methods below rather than being whatever
    /// any call site happens to reach for.
    #[cfg(test)]
    pub(super) fn backend(&self) -> &Arc<gmpublished_backend::Backend> {
        &self.services.backend
    }

    /// The search corpus itself, for tests asserting that a library refresh
    /// reached it. Production syncs through [`Self::sync_installed_addon_search`]
    /// and queries through `search_quick`.
    #[cfg(test)]
    pub(crate) fn search_for_test(&self) -> &gmpublished_backend::search::Search {
        &self.services.backend.search
    }

    /// Republishes the installed-addon and installed-file search corpora.
    pub(crate) fn sync_installed_addon_search(
        &self,
        addons: Vec<gmpublished_backend::search::SearchItem>,
        files: Vec<gmpublished_backend::search::SearchItem>,
    ) {
        self.services.sync_installed_addon_search(addons, files);
    }

    /// Extracts every entry of an opened preview archive.
    pub(crate) fn extract_preview_archive(
        &self,
        archive: &super::super::gma::PreviewArchive,
        destination: super::super::gma::ExtractDestination,
        options: &super::super::gma::PreviewExtractOptions,
        transaction: &gmpublished_backend::Transaction,
    ) -> Result<PathBuf, super::super::gma::GmaError> {
        self.services
            .extract_preview_archive(archive, destination, options, transaction)
    }

    /// Extracts one entry of an opened preview archive to the temp directory.
    pub(crate) fn extract_preview_archive_entry(
        &self,
        archive: &super::super::gma::PreviewArchive,
        entry_path: &str,
        transaction: &gmpublished_backend::Transaction,
    ) -> Result<PathBuf, super::super::gma::GmaError> {
        self.services
            .extract_preview_archive_entry(archive, entry_path, transaction)
    }

    /// Stops every background service this process owns, for app exit.
    /// Returns how many in-flight transactions were cancelled.
    pub(crate) fn shutdown(&self) -> usize {
        self.services.shutdown()
    }

    pub(crate) fn library_snapshot(&self) -> Option<LibrarySnapshot> {
        self.services.library_snapshot()
    }

    pub(crate) fn record_thumbhash(&self, url: &str, hash: &[u8]) {
        self.services.record_thumbhash(url, hash);
    }

    pub(crate) fn thumbhash_seed(&self) -> Vec<(String, Arc<[u8]>)> {
        self.services.thumbhash_seed()
    }

    pub(crate) fn begin_library_refresh(
        &self,
        reason: LibraryRefreshReason,
    ) -> Option<Task<Result<LibraryRefresh, RunBlockingError>>> {
        if !self.services.begin_library_refresh(reason) {
            return None;
        }

        Some(self.run_blocking("library-refresh", move |services| {
            services.refresh_library(reason)
        }))
    }

    pub(crate) fn abort_library_refresh(&self) -> Option<LibraryRefreshReason> {
        self.services.abort_library_refresh()
    }

    #[cfg(test)]
    pub(crate) fn update_settings_snapshot_for_test(
        &self,
        update: impl FnOnce(&mut Settings),
    ) -> Result<(), SettingsPersistError> {
        self.services.update_settings_snapshot(update)
    }

    pub(crate) fn steam_connected(&self) -> bool {
        self.services.steam_connected()
    }

    pub(crate) fn activate_startup_services(&self) -> bool {
        // Hydrate before starting Steam so a live metadata response cannot be
        // overwritten by the persisted snapshot loaded a moment later.
        self.services.hydrate_workshop_metadata_snapshot();
        self.services.backend.start_background_services()
    }

    #[cfg(test)]
    pub(crate) fn connect_steam(&self) -> Result<(), UiError> {
        self.services.connect_steam()
    }

    /// Stops in-flight Workshop submission batches from queueing further
    /// downloads; already-queued work is cancelled per-task instead.
    pub(crate) fn cancel_all_workshop_downloads(&self) {
        self.services.backend.downloads.cancel_all();
    }

    /// Cancels a task if it is correlated with a live backend transaction.
    /// A task not yet correlated (e.g. still resolving its first backend
    /// event) has no mechanism to cancel and reports `false`.
    pub(crate) fn cancel_task(&self, id: TaskId) -> bool {
        matches!(
            self.transaction_tasks
                .cancel_task(id, &self.services.backend.transactions),
            BackendTaskCancelResult::Cancelled
        )
    }

    pub(crate) fn create_task(&self, kind: TaskKind, status: TransactionStatus) -> TaskHandle {
        self.tasks.create(kind, status)
    }

    pub(crate) fn correlate_backend_transaction(
        &self,
        transaction_id: TransactionId,
        task: TaskHandle,
    ) -> TaskId {
        let task_id = task.id();
        self.transaction_tasks
            .correlate(transaction_id, task, BackendTaskSource::Generic);
        task_id
    }

    pub(crate) fn is_backend_transaction_active(&self, transaction_id: TransactionId) -> bool {
        self.transaction_tasks.is_active(transaction_id)
    }

    pub(crate) fn handle_backend_runtime_event(
        &self,
        event: &BackendRuntimeEvent,
    ) -> BackendRuntimeEventEffects {
        match event {
            BackendRuntimeEvent::DownloadStarted {
                transaction_id,
                request_id,
            } => {
                let task = self.create_task(
                    if request_id.is_some() {
                        TaskKind::WorkshopSnapshot
                    } else {
                        TaskKind::Download
                    },
                    TransactionStatus::Downloading,
                );
                self.transaction_tasks.correlate(
                    *transaction_id,
                    task,
                    BackendTaskSource::WorkshopDownload {
                        item_id: None,
                        start_emitted: false,
                        request_id: *request_id,
                    },
                );
                BackendRuntimeEventEffects::handled()
            }
            BackendRuntimeEvent::ExtractionStarted {
                transaction_id,
                workshop_id,
                source_path,
                request_id,
                ..
            } => {
                let task = self.create_task(
                    if request_id.is_some() {
                        TaskKind::WorkshopSnapshot
                    } else {
                        TaskKind::Extract
                    },
                    TransactionStatus::Extracting,
                );
                let effects = self.transaction_tasks.correlate(
                    *transaction_id,
                    task,
                    BackendTaskSource::WorkshopExtraction {
                        item_id: *workshop_id,
                        start_emitted: false,
                        source_gma: source_path.clone(),
                        request_id: *request_id,
                    },
                );
                BackendRuntimeEventEffects::handled_with(effects)
            }
            BackendRuntimeEvent::Transaction(event) => self.transaction_tasks.apply(event),
            BackendRuntimeEvent::SteamConnected
            | BackendRuntimeEvent::SteamDisconnected
            | BackendRuntimeEvent::AppDataUpdated(_)
            | BackendRuntimeEvent::InstalledAddonsRefreshed => {
                BackendRuntimeEventEffects::ignored()
            }
        }
    }

    pub(crate) fn apply_appdata_snapshot(
        &self,
        snapshot: BackendAppDataSnapshot,
    ) -> (Settings, AppPaths) {
        self.services.apply_appdata_snapshot(snapshot)
    }

    pub(crate) fn error_backend_transaction_task(
        &self,
        transaction_id: TransactionId,
        error: impl Into<UiError>,
    ) -> bool {
        self.transaction_tasks.error(transaction_id, error.into())
    }

    pub(crate) fn task_events(&self) -> Subscription<TaskEvent> {
        self.task_events.subscription()
    }

    pub(crate) fn backend_events(&self) -> Subscription<BackendRuntimeEvent> {
        self.backend_events.subscription()
    }
}

impl fmt::Debug for BackendContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackendContext")
            .field("services", &self.services)
            .field("task_events", &self.task_events)
            .field("backend_events", &self.backend_events)
            .field("transaction_tasks", &self.transaction_tasks)
            .finish_non_exhaustive()
    }
}

/// Last-resort `AppPaths` for when nothing better has resolved yet — the
/// backend before it reads settings, and the pickers rendering before a
/// backend exists. The directories land under one temp subdirectory so a
/// fallback run's scratch space is removable in one go.
pub fn fallback_paths(settings: &Settings) -> AppPaths {
    let temp = std::env::temp_dir().join("gmpublished");
    AppPaths::resolve_with_defaults(
        settings,
        AppPaths {
            settings_file: std::env::temp_dir().join("gmpublished-settings.json"),
            default_user_data_dir: temp.join("user-data"),
            default_temp_dir: temp.join("temp"),
            default_downloads_dir: Some(temp.join("downloads")),
            temp_dir: temp.join("temp"),
            user_data_dir: temp.join("user-data"),
            downloads_dir: Some(temp.join("downloads")),
            gmod_dir: None,
        },
    )
}
