use gmpublished_backend::error_key::keys;

#[cfg(target_os = "macos")]
use super::run_document_open_extraction;
use super::{
    App, NativeOpenTarget, PathBuf, PublishedFileId, RootMessage, Task, UiError,
    destination_select, downloader, flatten_blocking_ui_result, gma, modal_stack,
    parse_dropped_workshop_ids, prepare_publish, run_downloader_local_extraction,
    run_downloader_submission, schedule_native_open_target, send_root_message,
    spawn_blocking_detached_or_warn, stream,
};

impl App {
    pub(super) fn sync_downloader_destination_label(&mut self) -> Task<RootMessage> {
        let label = destination_select::destination_label(
            self.state.destination_select.settings(),
            self.state.destination_select.paths(),
        );
        self.apply_downloader_message(downloader::Message::DestinationLabelChanged(label))
    }

    pub(super) fn handle_file_drop(&self, path: PathBuf) -> Task<RootMessage> {
        // An open Prepare Publish modal accepts a dropped folder as the
        // addon path.
        if self.state.modal_stack.active() == Some(modal_stack::ActiveModal::PreparePublish)
            && self.state.prepare_publish.open()
            && path.is_dir()
        {
            return Task::done(RootMessage::PreparePublish(
                prepare_publish::Message::AddonPathBrowseCompleted(Some(path)),
            ));
        }

        if path.is_file() && gma::is_gma_path(&path) {
            return Task::done(RootMessage::Downloader(
                downloader::Message::BulkExtractPathsSelected(vec![path]),
            ));
        }

        if !self.state.shell.downloader_drop_target_hovered() {
            return Task::none();
        }

        self.ctx
            .run_blocking("workshop-drag-drop", move |_app| {
                Ok::<_, UiError>(parse_dropped_workshop_ids(&path))
            })
            .map(|result| {
                let item_ids = match flatten_blocking_ui_result(result) {
                    Ok(ids) => ids,
                    Err(error) => {
                        log::warn!("failed to parse dropped Workshop payload: {error}");
                        Vec::new()
                    }
                };
                RootMessage::Downloader(downloader::Message::WorkshopIdsSubmitted(item_ids))
            })
    }

    /// Runs the quick document-open extraction flow for `.gma` paths opened
    /// via the OS file association (macOS double-click / "Open With").
    ///
    /// Paths are filtered (existing, unique, `.gma`) at the bridge, each
    /// archive extracts to the temp destination on a worker thread with its
    /// own task overlay row, and the extracted folder opens on success.
    #[cfg(target_os = "macos")]
    pub(super) fn gma_documents_opened_task(&self, paths: Vec<PathBuf>) -> Task<RootMessage> {
        for path in crate::platform_open::filter_open_gma_paths(paths) {
            let ctx = self.ctx.clone();
            let subject = path.display().to_string();
            spawn_blocking_detached_or_warn(
                &self.ctx,
                "document-open-extract-gma",
                &format!("document-open extraction for `{subject}`"),
                move |_app| {
                    run_document_open_extraction(&ctx, &path);
                },
            );
        }
        Task::none()
    }

    pub(super) fn downloader_open_paths_task(&self, paths: Vec<PathBuf>) -> Task<RootMessage> {
        if paths.is_empty() {
            return Task::none();
        }

        let ctx = self.ctx.clone();
        Task::future(async move {
            for path in paths {
                schedule_native_open_target(
                    &ctx,
                    "native-open-downloader-path",
                    NativeOpenTarget::path(path),
                );
            }
        })
        .discard()
    }

    pub(super) fn downloader_bulk_extract_picker_task(&self) -> Task<RootMessage> {
        let title = self.state.i18n.tr("native-dialog-select-gma-archives");
        let filter = self.state.i18n.tr("native-dialog-gma-filter");
        Task::future(async move {
            let paths = pick_bulk_extract_paths(title, filter).await;
            RootMessage::Downloader(downloader::Message::BulkExtractPathsSelected(paths))
        })
    }

    pub(super) fn downloader_local_extraction_task(
        &self,
        paths: Vec<PathBuf>,
    ) -> Task<RootMessage> {
        if paths.is_empty() {
            return Task::none();
        }

        let ctx = self.ctx.clone();
        Task::stream(stream::channel(100, async move |output| {
            let worker_ctx = ctx.clone();
            spawn_blocking_detached_or_warn(
                &ctx,
                "downloader-local-extract",
                "downloader local extraction",
                move |app| {
                    run_downloader_local_extraction(&worker_ctx, &app, paths, output);
                },
            );
        }))
    }

    pub(super) fn downloader_submission_task(
        &self,
        item_ids: Vec<PublishedFileId>,
    ) -> Task<RootMessage> {
        let ctx = self.ctx.clone();
        Task::stream(stream::channel(100, async move |output| {
            let fallback_item_ids = item_ids.clone();
            let mut schedule_error_output = output.clone();
            let worker_ctx = ctx.clone();
            let scheduled = spawn_blocking_detached_or_warn(
                &ctx,
                "downloader-workshop-submit",
                "downloader Workshop submission",
                move |app| {
                    run_downloader_submission(worker_ctx, &app, item_ids, output);
                },
            );
            if !scheduled {
                let _sent = send_root_message(
                    &mut schedule_error_output,
                    RootMessage::Downloader(downloader::Message::EventReceived(
                        downloader::DownloaderEvent::SubmissionFailed {
                            item_ids: fallback_item_ids,
                            error_key: UiError::new(keys::UNKNOWN),
                        },
                    )),
                );
            }
        }))
    }

    pub(super) fn downloader_title_query_task(
        &self,
        item_ids: Vec<PublishedFileId>,
    ) -> Task<RootMessage> {
        self.ctx
            .run_blocking("downloader-workshop-title", move |app| {
                let requested_item_ids = item_ids.clone();
                let (mut items, stale_ids) = app.resolve_workshop_metadata(&item_ids);
                if !stale_ids.is_empty() && app.steam_connected() {
                    match app.refresh_workshop_metadata(&stale_ids) {
                        Ok(fresh_items) => items.extend(fresh_items),
                        Err(error) => {
                            log::debug!("Downloader Workshop title refresh failed: {error}");
                        }
                    }
                }
                downloader::DownloaderEvent::WorkshopMetadataResolved {
                    requested_item_ids,
                    items,
                }
            })
            .map(|result| match result {
                Ok(event) => RootMessage::Downloader(downloader::Message::EventReceived(event)),
                Err(error) => {
                    log::warn!("failed to schedule downloader Workshop title query: {error}");
                    RootMessage::Downloader(downloader::Message::EventReceived(
                        downloader::DownloaderEvent::WorkshopMetadataResolved {
                            requested_item_ids: Vec::new(),
                            items: Vec::new(),
                        },
                    ))
                }
            })
    }

    #[cfg(target_os = "macos")]
    pub(super) fn menu_open_gma_task(&self) -> Task<RootMessage> {
        let title = self.state.i18n.tr("menu-open-gma");
        Task::future(async move {
            rfd::AsyncFileDialog::new()
                .add_filter("GMA", &["gma"])
                .set_title(title)
                .pick_file()
                .await
                .map(|file| file.path().to_path_buf())
        })
        .map(RootMessage::MenuOpenGmaCompleted)
    }
}

pub(super) async fn pick_bulk_extract_paths(title: String, filter: String) -> Vec<PathBuf> {
    rfd::AsyncFileDialog::new()
        .add_filter(filter, &["gma"])
        .set_title(title)
        .pick_files()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|file| file.path().to_path_buf())
        .collect()
}
