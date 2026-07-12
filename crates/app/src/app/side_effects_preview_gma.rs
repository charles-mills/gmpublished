use super::{
    App, RootMessage, Task, flatten_blocking_ui_result, gma, preview_gma,
    run_preview_gma_archive_extraction, run_preview_gma_entry_extraction,
    spawn_blocking_detached_or_warn,
};

use iced::widget::operation;

impl App {
    /// Preview GMA keeps its base-layer modal open while Destination Select
    /// runs in the overlay: dismissing the overlay only drops the pending
    /// extraction, a successful save starts it, and a failed save leaves the
    /// overlay open showing the error.
    pub(super) fn preview_gma_destination_dismissed_task(&mut self) -> Task<RootMessage> {
        if self.state.preview_gma.has_pending_extraction() {
            self.state.preview_gma.clear_pending_extraction();
        }
        Task::none()
    }

    pub(super) fn preview_gma_destination_persisted_task(&mut self) -> Task<RootMessage> {
        if self.state.preview_gma.has_pending_extraction() {
            return self.preview_gma_archive_extraction_task();
        }
        Task::none()
    }

    pub(super) fn preview_gma_open_archive_task(
        &self,
        request: preview_gma::OpenRequest,
    ) -> Task<RootMessage> {
        let request_id = request.request_id;
        self.ctx
            .run_blocking("preview-gma-open-archive", move |_app| {
                preview_gma::LoadedArchive::open_path(&request.path, request.workshop_id)
            })
            .map(move |result| {
                RootMessage::PreviewGma(preview_gma::Message::ArchiveOpened(
                    request_id,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn preview_gma_workshop_metadata_task(
        &self,
        request: &preview_gma::MetadataRequest,
    ) -> Task<RootMessage> {
        let request_id = request.request_id;
        let workshop_id = request.workshop_id;
        self.ctx
            .run_blocking("preview-gma-workshop-metadata", move |app| {
                preview_gma::query_workshop_metadata(app, workshop_id)
            })
            .map(move |result| {
                RootMessage::PreviewGma(preview_gma::Message::WorkshopMetadataCompleted(
                    request_id,
                    workshop_id,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn preview_gma_author_task(
        &self,
        request: &preview_gma::AuthorRequest,
    ) -> Task<RootMessage> {
        let request_id = request.request_id;
        let steamid64 = request.steamid64;
        self.ctx
            .run_blocking("preview-gma-author", move |app| {
                preview_gma::query_steam_user(app, steamid64)
            })
            .map(move |result| {
                RootMessage::PreviewGma(preview_gma::Message::AuthorFetchCompleted(
                    request_id,
                    steamid64,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn preview_gma_nav_autoscroll_task(&self) -> Task<RootMessage> {
        operation::snap_to_end(preview_gma::nav_path_scrollable_id())
    }

    pub(super) fn preview_gma_entry_extraction_task(
        &self,
        request: preview_gma::ExtractionRequest,
    ) -> Task<RootMessage> {
        let ctx = self.ctx.clone();
        Task::future(async move {
            let worker_ctx = ctx.clone();
            spawn_blocking_detached_or_warn(
                &ctx,
                "preview-gma-extract-entry",
                "Preview GMA entry extraction",
                move |app| {
                    let _app = app;
                    run_preview_gma_entry_extraction(&worker_ctx, request);
                },
            );
        })
        .discard()
    }

    pub(super) fn preview_gma_archive_extraction_task(&mut self) -> Task<RootMessage> {
        let Some(request) = self.state.preview_gma.take_pending_archive_extraction() else {
            return Task::none();
        };

        let mut settings = self.state.destination_select.settings().clone();
        let paths = self.state.destination_select.paths().clone();
        settings.sanitize(&paths);
        let plan = gma::build_preview_extract_request(settings, &paths);
        let destination = plan.destination;
        let options = plan.options;

        let ctx = self.ctx.clone();
        Task::future(async move {
            let worker_ctx = ctx.clone();
            spawn_blocking_detached_or_warn(
                &ctx,
                "preview-gma-extract-archive",
                "Preview GMA archive extraction",
                move |app| {
                    let _app = app;
                    run_preview_gma_archive_extraction(
                        &worker_ctx,
                        &request,
                        destination,
                        &options,
                    );
                },
            );
        })
        .discard()
    }
}
