use gmpublished_backend::error_keys as keys;

#[cfg(target_os = "macos")]
use super::run_document_open_extraction;
use super::{
    App, NativeOpenTarget, PathBuf, PublishedFileId, RootMessage, Task, UiError,
    destination_select, downloader, gma, modal_stack, parse_dropped_workshop_ids, prepare_publish,
    run_downloader_local_extraction, run_downloader_submission, schedule_native_open_target,
    send_root_message, spawn_blocking_detached_or_warn, stream,
};

impl App {
    pub(super) fn sync_downloader_destination_label(&mut self) -> Task<RootMessage> {
        let label = destination_select::destination_label(
            self.state.features.destination_select.settings(),
            self.state.features.destination_select.paths(),
        );
        self.apply_downloader_message(
            downloader::Message::DestinationLabelChanged(label),
            self.update_context,
        )
    }

    pub(super) fn handle_file_drop(&self, path: PathBuf) -> Task<RootMessage> {
        let prepare_publish_accepts = self.state.features.modal_stack.active()
            == Some(modal_stack::ActiveModal::PreparePublish)
            && self.state.features.prepare_publish.open();
        let downloader_accepts = self.state.features.shell.downloader_drop_target_hovered();
        if !prepare_publish_accepts && !downloader_accepts && !gma::is_gma_path(&path) {
            return Task::none();
        }

        self.environment
            .ctx
            .run_blocking("workshop-drag-drop", move |_app| {
                if prepare_publish_accepts && path.is_dir() {
                    return FileDropAction::PreparePublishPath(path);
                }
                if path.is_file() && gma::is_gma_path(&path) {
                    return FileDropAction::ExtractGma(path);
                }
                if downloader_accepts {
                    return FileDropAction::SubmitWorkshopIds(parse_dropped_workshop_ids(&path));
                }
                FileDropAction::Ignore
            })
            .map(|result| {
                result.map_or_else(
                    |error| {
                        log::warn!("failed to inspect dropped path: {error}");
                        RootMessage::Noop
                    },
                    FileDropAction::into_message,
                )
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
            let ctx = self.environment.ctx.clone();
            let subject = path.display().to_string();
            spawn_blocking_detached_or_warn(
                &self.environment.ctx,
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

        let ctx = self.environment.ctx.clone();
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

        let ctx = self.environment.ctx.clone();
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
        let ctx = self.environment.ctx.clone();
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
        self.environment
            .ctx
            .run_blocking("downloader-workshop-title", move |app| {
                let requested_item_ids = item_ids.clone();
                let (mut items, stale_ids) = app.workshop().resolve_metadata(&item_ids);
                if !stale_ids.is_empty() && app.workshop().connected() {
                    match app.workshop().refresh_metadata(&stale_ids) {
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
        .map(|path| RootMessage::Platform(crate::platform::Message::MenuOpenGmaCompleted(path)))
    }
}

enum FileDropAction {
    PreparePublishPath(PathBuf),
    ExtractGma(PathBuf),
    SubmitWorkshopIds(Vec<PublishedFileId>),
    Ignore,
}

impl FileDropAction {
    fn into_message(self) -> RootMessage {
        match self {
            Self::PreparePublishPath(path) => RootMessage::PreparePublish(
                prepare_publish::Message::AddonPathBrowseCompleted(Some(path)),
            ),
            Self::ExtractGma(path) => {
                RootMessage::Downloader(downloader::Message::BulkExtractPathsSelected(vec![path]))
            }
            Self::SubmitWorkshopIds(ids) => {
                RootMessage::Downloader(downloader::Message::WorkshopIdsSubmitted(ids))
            }
            Self::Ignore => RootMessage::Noop,
        }
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
