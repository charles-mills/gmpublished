use gmpublished_backend::error_key::keys;

use super::{
    Arc, BackendContext, BackendRuntimeAction, BackendRuntimeEvent, BackendServices, Duration,
    HashMap, NativeOpenTarget, Path, PathBuf, PublishedFileId, RootMessage, RunBlockingError,
    SearchFullRequest, TaskHandle, TaskKind, UiError, WorkshopDownloadResult,
    WorkshopDownloadSuccess, downloader, gma, iced_mpsc, installed_addons, prepare_publish,
    preview_gma, search, size_analyzer, steam_session, tasks,
};

pub(super) fn flatten_blocking_ui_result<T>(
    result: Result<Result<T, UiError>, RunBlockingError>,
) -> Result<T, UiError> {
    match result {
        Ok(inner) => inner,
        Err(error) => Err(UiError::from(&error)),
    }
}

pub(super) fn run_search_full(
    ctx: &BackendContext,
    app: &BackendServices,
    request: SearchFullRequest,
    task: TaskHandle,
    mut output: iced_mpsc::Sender<RootMessage>,
) {
    let Some(events) = app.subscribe_backend_events() else {
        task.error(keys::SEARCH_EVENT_SINK_UNAVAILABLE);
        let _sent = send_root_message(
            &mut output,
            RootMessage::Search(search::Message::FullSearchFinished(request)),
        );
        return;
    };

    let transaction = app.begin_transaction();
    let transaction_id = transaction.id;
    ctx.correlate_backend_transaction(transaction_id, task);
    let started_id = app.start_search_full(&request, transaction);
    debug_assert_eq!(started_id, transaction_id);

    let mut sequence = 0;
    loop {
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(BackendRuntimeEvent::Transaction(event))
                if search_full_transaction_id(&event) == transaction_id =>
            {
                match event {
                    tasks::TransactionRuntimeEvent::Data { payload, .. } => {
                        match app.search_full_batch_from_transaction_payload(
                            &request, sequence, &payload,
                        ) {
                            Ok(batch) => {
                                sequence = sequence.wrapping_add(1);
                                let _sent = send_root_message(
                                    &mut output,
                                    RootMessage::Search(search::Message::FullSearchBatchReceived(
                                        batch,
                                    )),
                                );
                            }
                            Err(error) => {
                                log::warn!(
                                    "failed to project full-search transaction data for `{}`: {error}",
                                    request.query()
                                );
                                let _handled =
                                    ctx.error_backend_transaction_task(transaction_id, error);
                                break;
                            }
                        }
                    }
                    tasks::TransactionRuntimeEvent::Finished { .. } => {
                        let _effects = ctx
                            .handle_backend_runtime_event(&BackendRuntimeEvent::Transaction(event));
                        break;
                    }
                    tasks::TransactionRuntimeEvent::Error { error, .. } => {
                        let _handled = ctx
                            .error_backend_transaction_task(transaction_id, UiError::from(error));
                        break;
                    }
                    tasks::TransactionRuntimeEvent::Status { .. }
                    | tasks::TransactionRuntimeEvent::Progress { .. }
                    | tasks::TransactionRuntimeEvent::IncrProgress { .. }
                    | tasks::TransactionRuntimeEvent::ResetProgress { .. } => {}
                }
            }
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !ctx.is_backend_transaction_active(transaction_id) {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _handled = ctx.error_backend_transaction_task(
                    transaction_id,
                    keys::SEARCH_EVENT_SINK_DISCONNECTED,
                );
                break;
            }
        }
    }

    let _sent = send_root_message(
        &mut output,
        RootMessage::Search(search::Message::FullSearchFinished(request)),
    );
}

pub(super) fn run_installed_metadata_refresh(
    app: &BackendServices,
    generation: u64,
    item_ids: &[PublishedFileId],
    mut output: iced_mpsc::Sender<RootMessage>,
) {
    let result = installed_addons::refresh_metadata_streaming(app, item_ids, |patches| {
        let _sent = send_root_message(
            &mut output,
            RootMessage::InstalledAddons(installed_addons::Message::MetadataRefreshCompleted(
                generation,
                Ok(patches),
            )),
        );
    });
    if let Err(error) = result {
        let _sent = send_root_message(
            &mut output,
            RootMessage::InstalledAddons(installed_addons::Message::MetadataRefreshCompleted(
                generation,
                Err(error),
            )),
        );
    }
}

pub(super) fn run_size_analyzer_preview_urls(
    app: &BackendServices,
    ids: &[PublishedFileId],
    mut output: iced_mpsc::Sender<RootMessage>,
) {
    let (cached_metadata, stale_ids) = app.resolve_workshop_metadata(ids);
    send_preview_urls(&mut output, preview_urls_from_metadata(cached_metadata));

    if !stale_ids.is_empty() && app.steam_connected() {
        let result = app.refresh_workshop_metadata_streaming(&stale_ids, |metadata| {
            send_preview_urls(&mut output, preview_urls_from_metadata(metadata));
        });
        if let Err(error) = result {
            log::debug!("Size Analyzer preview URL refresh failed: {error}");
        }
    }
}

fn preview_urls_from_metadata(
    metadata: Vec<crate::bridge::domain::WorkshopMetadata>,
) -> HashMap<PublishedFileId, String> {
    metadata
        .into_iter()
        .filter_map(|metadata| {
            metadata
                .preview_url
                .map(|preview_url| (metadata.id, preview_url))
        })
        .collect()
}

fn send_preview_urls(
    output: &mut iced_mpsc::Sender<RootMessage>,
    preview_urls: HashMap<PublishedFileId, String>,
) {
    if preview_urls.is_empty() {
        return;
    }
    let _sent = send_root_message(
        output,
        RootMessage::SizeAnalyzer(size_analyzer::Message::PreviewUrlsResolved(preview_urls)),
    );
}

pub(super) fn search_full_transaction_id(event: &tasks::TransactionRuntimeEvent) -> u32 {
    match event {
        tasks::TransactionRuntimeEvent::Finished { id, .. }
        | tasks::TransactionRuntimeEvent::Error { id, .. }
        | tasks::TransactionRuntimeEvent::Data { id, .. }
        | tasks::TransactionRuntimeEvent::Status { id, .. }
        | tasks::TransactionRuntimeEvent::Progress { id, .. }
        | tasks::TransactionRuntimeEvent::IncrProgress { id, .. }
        | tasks::TransactionRuntimeEvent::ResetProgress { id } => *id,
    }
}

pub(super) fn run_downloader_local_extraction(
    ctx: &BackendContext,
    app: &BackendServices,
    paths: Vec<PathBuf>,
    mut output: iced_mpsc::Sender<RootMessage>,
) {
    let (settings, path_snapshot) = app.settings_and_paths_snapshot();
    let plan = gma::build_preview_extract_request(settings, &path_snapshot);
    let paths = paths
        .into_iter()
        .filter(|path| path.is_file() && gma::is_gma_path(path))
        .collect::<Vec<_>>();

    if paths.is_empty() {
        log::debug!("bulk extract selection contained no valid local .gma files");
        return;
    }

    for path in paths {
        let total_bytes = path.metadata().map_or(0, |metadata| metadata.len());
        let task = create_downloader_local_extract_task(ctx, total_bytes);
        let task_id = task.id();

        if !send_root_message(
            &mut output,
            RootMessage::Downloader(downloader::Message::EventReceived(
                downloader::DownloaderEvent::LocalExtractionStarted {
                    path: path.clone(),
                    task_id,
                    total_bytes,
                },
            )),
        ) {
            return;
        }

        let result =
            run_local_gma_extraction(ctx, &path, plan.destination.clone(), &plan.options, task);

        if !send_root_message(
            &mut output,
            RootMessage::Downloader(downloader::Message::EventReceived(
                downloader::DownloaderEvent::LocalExtractionFinished {
                    task_id,
                    outcome: local_extraction_outcome(result),
                },
            )),
        ) {
            return;
        }
    }
}

pub(super) fn create_downloader_local_extract_task(
    ctx: &BackendContext,
    total_bytes: u64,
) -> TaskHandle {
    create_extract_task(ctx, TaskKind::Extract, total_bytes)
}

/// Correlates a fresh backend transaction with `task` (mirroring the
/// Steam-download path) so terminal delivery and per-task cancellation both
/// flow through the same mechanism instead of a hidden, uncorrelated
/// transaction extraction would otherwise create for itself.
pub(super) fn run_local_gma_extraction(
    ctx: &BackendContext,
    path: &Path,
    destination: gma::ExtractDestination,
    options: &gma::PreviewExtractOptions,
    task: TaskHandle,
) -> Result<PathBuf, gma::GmaError> {
    let archive = gma::PreviewArchive::open(path)?;

    let transaction = ctx.begin_transaction();
    ctx.correlate_backend_transaction(transaction.id, task);
    let result =
        archive.extract_all_with_transaction(destination, options, &transaction, ctx.backend());
    if let Err(error) = &result {
        let _handled = ctx.error_backend_transaction_task(transaction.id, UiError::from(error));
    }
    result
}

pub(super) fn local_extraction_outcome(
    result: Result<PathBuf, gma::GmaError>,
) -> downloader::LocalExtractionOutcome {
    match result {
        Ok(path) => downloader::LocalExtractionOutcome::Success(path),
        Err(error) => downloader::LocalExtractionOutcome::Error(UiError::from(&error)),
    }
}

/// Extracts one document-opened `.gma` archive with quick-open semantics:
/// temp destination, task-overlay progress row, open-after-extract.
#[cfg(target_os = "macos")]
pub(super) fn run_document_open_extraction(ctx: &BackendContext, path: &Path) {
    let total_bytes = path.metadata().map_or(0, |metadata| metadata.len());
    let task = create_preview_gma_extract_task(ctx, total_bytes);

    let result = run_local_gma_extraction(
        ctx,
        path,
        gma::ExtractDestination::Temp,
        &gma::PreviewExtractOptions::default(),
        task,
    );

    open_preview_gma_extracted_path(
        ctx,
        result.as_ref().ok(),
        "document-open archive",
        &path.display().to_string(),
    );
    log_preview_gma_extraction_result(
        "Document-open archive",
        &path.display().to_string(),
        &result,
    );
}

pub(super) fn run_preview_gma_entry_extraction(
    ctx: &BackendContext,
    request: preview_gma::ExtractionRequest,
) {
    let preview_gma::ExtractionIntent::Entry { path, size_bytes } = request.intent else {
        return;
    };

    let transaction = create_preview_gma_extract_transaction(ctx, size_bytes);
    let result = request
        .archive
        .extract_entry_with_transaction(&path, &transaction, ctx.backend());
    if let Err(error) = &result {
        ctx.error_backend_transaction_task(transaction.id, UiError::from(error));
    }
    open_preview_gma_extracted_path(ctx, result.as_ref().ok(), "PreviewGMA entry", &path);
    log_preview_gma_extraction_result("PreviewGMA entry", &path, &result);
}

pub(super) fn run_preview_gma_archive_extraction(
    ctx: &BackendContext,
    request: &preview_gma::ExtractionRequest,
    destination: gma::ExtractDestination,
    options: &gma::PreviewExtractOptions,
) {
    let total_bytes = request.intent.total_bytes();
    let subject = request.request_id.to_string();
    let transaction = create_preview_gma_extract_transaction(ctx, total_bytes);
    let result = request.archive.extract_all_with_transaction(
        destination,
        options,
        &transaction,
        ctx.backend(),
    );
    if let Err(error) = &result {
        ctx.error_backend_transaction_task(transaction.id, UiError::from(error));
    }
    open_preview_gma_extracted_path(ctx, result.as_ref().ok(), "PreviewGMA archive", &subject);
    log_preview_gma_extraction_result("PreviewGMA archive", &subject, &result);
}

pub(super) fn create_preview_gma_extract_transaction(
    ctx: &BackendContext,
    total_bytes: u64,
) -> gmpublished_backend::Transaction {
    let transaction = ctx.begin_transaction();
    let task = create_preview_gma_extract_task(ctx, total_bytes);
    ctx.correlate_backend_transaction(transaction.id, task);
    transaction
}

pub(super) fn create_preview_gma_extract_task(
    ctx: &BackendContext,
    total_bytes: u64,
) -> TaskHandle {
    // Extractions invoked outside the Downloader page surface as overlay
    // toasts instead of Downloader rows.
    create_extract_task(ctx, TaskKind::OverlayExtract, total_bytes)
}

fn create_extract_task(ctx: &BackendContext, kind: TaskKind, total_bytes: u64) -> TaskHandle {
    let task = ctx.create_task(kind, downloader::EXTRACT_STATUS);
    if total_bytes > 0 {
        task.total(total_bytes);
    }
    task
}

pub(super) fn open_preview_gma_extracted_path(
    ctx: &BackendContext,
    extracted: Option<&PathBuf>,
    label: &str,
    subject: &str,
) {
    let Some(path) = extracted else {
        return;
    };
    schedule_native_open_target(
        ctx,
        "native-open-preview-gma-extraction",
        NativeOpenTarget::path(path.clone()),
    );
    log::debug!("scheduled native open for {label} extraction path for `{subject}`");
}

pub(super) fn schedule_native_open_target(
    ctx: &BackendContext,
    name: &'static str,
    target: NativeOpenTarget,
) {
    if let Err(error) = ctx.open_native_target_detached(name, target) {
        log::warn!("failed to schedule native operation `{name}`: {error}");
    }
}

/// Schedules `job` on the backend's blocking pool via `spawn_blocking_detached`,
/// logging a warning naming `what` if scheduling itself failed. Returns
/// whether scheduling succeeded, for callers that need to react further
/// (e.g. sending a fallback message) beyond the log line.
pub(super) fn spawn_blocking_detached_or_warn(
    ctx: &BackendContext,
    name: impl Into<Arc<str>>,
    what: &str,
    job: impl FnOnce(Arc<BackendServices>) + Send + 'static,
) -> bool {
    match ctx.spawn_blocking_detached(name, job) {
        Ok(()) => true,
        Err(error) => {
            log::warn!("failed to schedule {what}: {error}");
            false
        }
    }
}

pub(super) fn log_preview_gma_extraction_result(
    label: &str,
    subject: &str,
    result: &Result<PathBuf, gma::GmaError>,
) {
    match result {
        Ok(path) => log::info!("{label} `{subject}` extracted to {}", path.display()),
        Err(error) => log::warn!("{label} `{subject}` extraction failed: {error}"),
    }
}

pub(super) fn backend_runtime_action_message(action: BackendRuntimeAction) -> RootMessage {
    match action {
        BackendRuntimeAction::DownloadTaskStarted {
            kind,
            item_id,
            task_id,
        } => RootMessage::Downloader(downloader::Message::EventReceived(
            downloader::DownloaderEvent::TaskStarted {
                kind,
                item_id,
                task_id,
            },
        )),
        BackendRuntimeAction::DownloadFinished {
            request_id: Some(request_id),
            item_id,
            installed_path,
            extracted_path,
        } => RootMessage::PreparePublish(prepare_publish::Message::WorkshopContentDownloaded(
            request_id,
            WorkshopDownloadSuccess {
                item_id,
                installed_path,
                extracted_path,
            },
        )),
        BackendRuntimeAction::DownloadFinished {
            request_id: None,
            item_id,
            installed_path,
            extracted_path,
        } => RootMessage::Downloader(downloader::Message::EventReceived(
            downloader::DownloaderEvent::WorkshopDownloadFinished(WorkshopDownloadResult {
                item_id,
                outcome: Ok(WorkshopDownloadSuccess {
                    item_id,
                    installed_path,
                    extracted_path,
                }),
            }),
        )),
        BackendRuntimeAction::SnapshotFailed { request_id, error } => RootMessage::PreparePublish(
            prepare_publish::Message::WorkshopSnapshotFailed(request_id, error),
        ),
    }
}

pub(super) fn run_downloader_submission(
    _ctx: BackendContext,
    app: &BackendServices,
    item_ids: Vec<PublishedFileId>,
    mut output: iced_mpsc::Sender<RootMessage>,
) {
    let attempt = steam_session::connect_context_for_operation(app);
    let connected = attempt.connected();
    let connection_error = attempt.error().cloned();
    if !send_root_message(
        &mut output,
        RootMessage::SteamSession(steam_session::Message::ConnectionAttemptCompleted(attempt)),
    ) {
        return;
    }

    if !connected {
        send_downloader_submission_failed(
            &mut output,
            item_ids,
            connection_error.unwrap_or_else(|| UiError::new(keys::STEAM_ERROR)),
        );
        return;
    }

    if let Err(error) = app.submit_workshop_downloads(item_ids.clone()) {
        send_downloader_submission_failed(&mut output, item_ids, error);
    }
}

pub(super) fn send_downloader_submission_failed(
    output: &mut iced_mpsc::Sender<RootMessage>,
    item_ids: Vec<PublishedFileId>,
    error_key: UiError,
) {
    let _sent = send_root_message(
        output,
        RootMessage::Downloader(downloader::Message::EventReceived(
            downloader::DownloaderEvent::SubmissionFailed {
                item_ids,
                error_key,
            },
        )),
    );
}

pub(super) fn send_root_message(
    output: &mut iced_mpsc::Sender<RootMessage>,
    message: RootMessage,
) -> bool {
    crate::util::channel::send_blocking(output, message)
}
