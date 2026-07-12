#[cfg(test)]
use super::SettingsPersistError;
use super::{
    AppPaths, AppWorkerRuntime, Arc, BACKEND_EVENT_QUEUE_CAPACITY, BackendAppDataSnapshot,
    BackendEventSinkRegistration, BackendEventStreamFactory, BackendRuntimeEvent,
    BackendRuntimeEventEffects, BackendServices, BackendTaskCancelResult, BackendTaskSource,
    BackendTransactionTasks, DOWNLOAD_STATUS_DOWNLOADING, EXTRACT_STATUS, LibraryRefresh,
    LibraryRefreshReason, LibrarySnapshot, NativeOpenTarget, RunBlockingError, ScheduleError,
    Settings, StatusKey, Subscription, Task, TaskEvent, TaskEventStreamFactory, TaskHandle, TaskId,
    TaskKind, Tasks, UiError, WorkerPoolSpawner, fmt, install_backend_event_sink_by_default, mpsc,
    oneshot, show_native_open_error_dialog,
};

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
            Box::new(move |_| {
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
        self.runtime.spawn_blocking(name, move |_| job(services))
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
        self.services.settings_snapshot().play_gifs_by_default
    }

    pub(crate) fn sounds_enabled(&self) -> bool {
        self.services.settings_snapshot().sounds
    }

    pub(crate) fn settings_and_paths_snapshot(&self) -> (Settings, AppPaths) {
        self.services.settings_and_paths_snapshot()
    }

    pub(crate) fn begin_transaction(&self) -> gmpublished_backend::Transaction {
        self.services.begin_transaction()
    }

    /// Broad accessor for the few call sites (GMA extraction) that
    /// legitimately need several backend pieces together (whitelist,
    /// app_data, steam) rather than one narrow service.
    pub(crate) fn backend(&self) -> &Arc<gmpublished_backend::Backend> {
        &self.services.backend
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

    pub(crate) fn create_task(&self, kind: TaskKind, status: impl Into<StatusKey>) -> TaskHandle {
        self.tasks.create(kind, status)
    }

    pub(crate) fn correlate_backend_transaction(
        &self,
        transaction_id: u32,
        task: TaskHandle,
    ) -> TaskId {
        let task_id = task.id();
        self.transaction_tasks
            .correlate(transaction_id, task, BackendTaskSource::Generic);
        task_id
    }

    pub(crate) fn is_backend_transaction_active(&self, transaction_id: u32) -> bool {
        self.transaction_tasks.is_active(transaction_id)
    }

    pub(crate) fn handle_backend_runtime_event(
        &self,
        event: &BackendRuntimeEvent,
    ) -> BackendRuntimeEventEffects {
        match event {
            BackendRuntimeEvent::DownloadStarted { transaction_id } => {
                let task = self.create_task(TaskKind::Download, DOWNLOAD_STATUS_DOWNLOADING);
                self.transaction_tasks.correlate(
                    *transaction_id,
                    task,
                    BackendTaskSource::WorkshopDownload {
                        item_id: None,
                        start_emitted: false,
                    },
                );
                BackendRuntimeEventEffects::handled()
            }
            BackendRuntimeEvent::ExtractionStarted {
                transaction_id,
                workshop_id,
                source_path,
                ..
            } => {
                let task = self.create_task(TaskKind::Extract, EXTRACT_STATUS);
                let effects = self.transaction_tasks.correlate(
                    *transaction_id,
                    task,
                    BackendTaskSource::WorkshopExtraction {
                        item_id: *workshop_id,
                        start_emitted: false,
                        source_gma: source_path.clone(),
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
        transaction_id: u32,
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

pub(super) fn default_paths(settings: &Settings) -> AppPaths {
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
