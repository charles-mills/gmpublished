use super::{
    App, RootMessage, Task, UiError, connect_steam_for_operation, flatten_blocking_ui_result, gma,
    preview_gma, run_preview_gma_archive_extraction, run_preview_gma_entry_extraction,
    send_root_message, spawn_blocking_detached_or_warn, stream,
};

use gmpublished_backend::error_keys as keys;
use iced::widget::operation;

impl App {
    /// Preview GMA keeps its base-layer modal open while Destination Select
    /// runs in the overlay: dismissing the overlay only drops the pending
    /// extraction, a successful save starts it, and a failed save leaves the
    /// overlay open showing the error.
    pub(super) fn preview_gma_destination_dismissed_task(&mut self) -> Task<RootMessage> {
        if self.state.features.preview_gma.has_pending_extraction() {
            self.state.features.preview_gma.clear_pending_extraction();
        }
        Task::none()
    }

    pub(super) fn preview_gma_destination_persisted_task(&mut self) -> Task<RootMessage> {
        if self.state.features.preview_gma.has_pending_extraction() {
            return self.preview_gma_archive_extraction_task();
        }
        Task::none()
    }

    pub(super) fn preview_gma_open_archive_task(
        &self,
        request: preview_gma::OpenRequest,
    ) -> Task<RootMessage> {
        let request_id = request.request_id;
        self.environment
            .ctx
            .run_blocking("preview-gma-open-archive", move |_app| {
                preview_gma::LoadedArchive::open_path(&request.path, request.workshop_id)
            })
            .map(move |result| {
                RootMessage::PreviewGma(preview_gma::Message::ArchiveOpened(
                    request_id,
                    Box::new(flatten_blocking_ui_result(result)),
                ))
            })
    }

    pub(super) fn preview_gma_workshop_metadata_task(
        &self,
        request: &preview_gma::MetadataRequest,
    ) -> Task<RootMessage> {
        let request_id = request.request_id;
        let workshop_id = request.workshop_id;
        let ctx = self.environment.ctx.clone();
        Task::stream(stream::channel(8, async move |output| {
            let mut schedule_error_output = output.clone();
            let scheduled = spawn_blocking_detached_or_warn(
                &ctx,
                "preview-gma-workshop-metadata",
                "Preview GMA Workshop metadata",
                move |app| {
                    let mut output = output;
                    if let Some(cached) = app
                        .workshop()
                        .cached_item_details(workshop_id)
                        .map(preview_gma::cached_workshop_metadata)
                    {
                        let _sent = send_root_message(
                            &mut output,
                            RootMessage::PreviewGma(
                                preview_gma::Message::WorkshopMetadataCompleted(
                                    request_id,
                                    workshop_id,
                                    Box::new(Ok(Some(cached))),
                                ),
                            ),
                        );
                    }

                    let attempt = connect_steam_for_operation(app.workshop());
                    let result = if attempt.connected() {
                        app.workshop()
                            .item_details(workshop_id)
                            .map(preview_gma::workshop_metadata_from_details)
                    } else {
                        Err(attempt
                            .error()
                            .cloned()
                            .unwrap_or_else(|| UiError::new(keys::STEAM_ERROR)))
                    };
                    let should_persist = matches!(&result, Ok(Some(_)));
                    let _sent = send_root_message(
                        &mut output,
                        RootMessage::PreviewGma(preview_gma::Message::WorkshopMetadataCompleted(
                            request_id,
                            workshop_id,
                            Box::new(result),
                        )),
                    );
                    // Keep snapshot I/O behind delivery so a cold detail query
                    // can paint as soon as Steam responds.
                    if should_persist {
                        app.workshop().persist_metadata_cache();
                    }
                },
            );
            if !scheduled {
                let _sent = send_root_message(
                    &mut schedule_error_output,
                    RootMessage::PreviewGma(preview_gma::Message::WorkshopMetadataCompleted(
                        request_id,
                        workshop_id,
                        Box::new(Err(UiError::new(keys::STEAM_ERROR))),
                    )),
                );
            }
        }))
    }

    pub(super) fn preview_gma_author_task(
        &self,
        request: &preview_gma::AuthorRequest,
    ) -> Task<RootMessage> {
        let request_id = request.request_id;
        let steamid64 = request.steamid64;
        let ctx = self.environment.ctx.clone();
        Task::stream(stream::channel(8, async move |output| {
            let mut schedule_error_output = output.clone();
            let scheduled = spawn_blocking_detached_or_warn(
                &ctx,
                "preview-gma-author",
                "Preview GMA author",
                move |app| {
                    let mut output = output;
                    let result = app.workshop().user_details_streaming(steamid64, |user| {
                        let result = preview_gma::author_info_from_user(user);
                        let _sent = send_root_message(
                            &mut output,
                            RootMessage::PreviewGma(preview_gma::Message::AuthorFetchCompleted(
                                request_id, steamid64, result,
                            )),
                        );
                    });
                    if let Err(error) = result {
                        let _sent = send_root_message(
                            &mut output,
                            RootMessage::PreviewGma(preview_gma::Message::AuthorFetchCompleted(
                                request_id,
                                steamid64,
                                Err(error),
                            )),
                        );
                    }
                },
            );
            if !scheduled {
                let _sent = send_root_message(
                    &mut schedule_error_output,
                    RootMessage::PreviewGma(preview_gma::Message::AuthorFetchCompleted(
                        request_id,
                        steamid64,
                        Err(UiError::new(keys::STEAM_ERROR)),
                    )),
                );
            }
        }))
    }

    pub(super) fn preview_gma_nav_autoscroll_task(&self) -> Task<RootMessage> {
        // The model resets its scroll offset on navigation; snap the rows
        // scrollable with it, or the widget keeps its old offset and the
        // viewport lands on the virtualization spacer.
        Task::batch([
            operation::snap_to_end(preview_gma::nav_path_scrollable_id()),
            operation::scroll_to(
                preview_gma::browser_rows_scrollable_id(),
                iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: 0.0 },
            ),
        ])
    }

    pub(super) fn preview_gma_entry_extraction_task(
        &self,
        request: preview_gma::ExtractionRequest,
    ) -> Task<RootMessage> {
        let ctx = self.environment.ctx.clone();
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
        let Some(request) = self
            .state
            .features
            .preview_gma
            .take_pending_archive_extraction()
        else {
            return Task::none();
        };

        let settings = self.state.features.destination_select.settings().clone();

        let ctx = self.environment.ctx.clone();
        Task::future(async move {
            let worker_ctx = ctx.clone();
            spawn_blocking_detached_or_warn(
                &ctx,
                "preview-gma-extract-archive",
                "Preview GMA archive extraction",
                move |app| {
                    let _app = app;
                    let plan = gma::build_preview_extract_request(settings);
                    run_preview_gma_archive_extraction(
                        &worker_ctx,
                        &request,
                        plan.destination,
                        &plan.options,
                    );
                },
            );
        })
        .discard()
    }
}
