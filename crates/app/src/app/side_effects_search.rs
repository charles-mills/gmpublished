use super::{
    App, PublishedFileId, RootMessage, SearchFullRequest, SearchQuickRequest, Task, TaskHandle,
    TaskKind, UiError, flatten_blocking_ui_result, prepare_publish, preview_gma, run_search_full,
    search, send_root_message, spawn_blocking_detached_or_warn, steam_session, stream,
    workshop_url,
};

use iced::widget::operation;

impl App {
    pub(super) fn search_focus_input_task(&self) -> Task<RootMessage> {
        operation::focus(search::SEARCH_INPUT_ID)
    }

    pub(super) fn search_quick_debounce_task(
        &self,
        request: SearchQuickRequest,
    ) -> Task<RootMessage> {
        Task::future(async move {
            tokio::time::sleep(search::QUICK_SEARCH_DEBOUNCE).await;
            RootMessage::Search(search::Message::QuickDebounced(request))
        })
    }

    pub(super) fn search_quick_task(&self, request: SearchQuickRequest) -> Task<RootMessage> {
        let key = request.key().clone();
        self.ctx
            .run_blocking("search-quick", move |app| app.search_quick(&request))
            .map(move |result| {
                RootMessage::Search(search::Message::QuickSearchCompleted(
                    key.clone(),
                    result.map_err(|error| UiError::from(&error)),
                ))
            })
    }

    pub(super) fn search_metadata_task(
        &self,
        generation: u64,
        item_ids: Vec<PublishedFileId>,
    ) -> Task<RootMessage> {
        let worker_item_ids = item_ids.clone();
        let delivery_item_ids = item_ids;
        self.ctx
            .run_blocking("search-metadata", move |app| {
                search::resolve_metadata(app, &worker_item_ids)
            })
            .map(move |result| {
                RootMessage::Search(search::Message::MetadataCompleted(
                    generation,
                    delivery_item_ids.clone(),
                    result.map_err(|error| UiError::from(&error)),
                ))
            })
    }

    pub(super) fn search_metadata_refresh_task(
        &mut self,
        generation: u64,
        item_ids: Vec<PublishedFileId>,
    ) -> Task<RootMessage> {
        if let Some(task) =
            self.defer_steam_operation(steam_session::PendingRetry::SearchMetadataRefresh {
                generation,
                item_ids: item_ids.clone(),
            })
        {
            return task;
        }

        let worker_item_ids = item_ids.clone();
        let delivery_item_ids = item_ids;
        self.ctx
            .run_blocking("search-metadata-refresh", move |app| {
                search::refresh_metadata(app, &worker_item_ids)
            })
            .map(move |result| {
                RootMessage::Search(search::Message::MetadataRefreshCompleted(
                    generation,
                    delivery_item_ids.clone(),
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn search_full_task(&mut self) -> Task<RootMessage> {
        let task = self.ctx.create_task(TaskKind::Search, "search");
        let Some(start) = self.state.search.begin_full_search(task.id()) else {
            task.error(gmpublished_backend::error_key::keys::CANCELLED);
            return Task::none();
        };

        if let Some(task_id) = start.cancel_task {
            let _cancelled = self.ctx.cancel_task(task_id);
        }

        self.search_full_stream_task(start.request, task)
    }

    pub(super) fn search_full_stream_task(
        &self,
        request: SearchFullRequest,
        task: TaskHandle,
    ) -> Task<RootMessage> {
        let ctx = self.ctx.clone();
        Task::stream(stream::channel(100, async move |output| {
            let fallback_request = request.clone();
            let mut schedule_error_output = output.clone();
            let worker_ctx = ctx.clone();
            let scheduled = spawn_blocking_detached_or_warn(
                &ctx,
                "search-full",
                "full-search worker",
                move |app| {
                    run_search_full(&worker_ctx, &app, request, task, output);
                },
            );
            if !scheduled {
                let _sent = send_root_message(
                    &mut schedule_error_output,
                    RootMessage::Search(search::Message::FullSearchFinished(fallback_request)),
                );
            }
        }))
    }

    pub(super) fn search_result_task(&mut self, row_id: usize) -> Task<RootMessage> {
        let Some(selection) = self.state.search.selection_for(row_id) else {
            log::debug!("ignored search activation for unknown row id `{row_id}`");
            return Task::none();
        };

        match selection.action {
            search::SelectionAction::InstalledAddon {
                path,
                workshop_id,
                preview_url,
            } => self.apply_preview_gma_message(preview_gma::Message::OpenRequested(
                preview_gma::OpenTarget::new(path, selection.title, workshop_id).with_seed(
                    preview_gma::OpenSeed {
                        preview_url,
                        ..preview_gma::OpenSeed::default()
                    },
                ),
            )),
            search::SelectionAction::MyWorkshop {
                workshop_id,
                title,
                tags,
                preview_url,
            } => Task::done(RootMessage::PreparePublish(
                prepare_publish::Message::OpenRequested {
                    target: prepare_publish::OpenTarget::Update(prepare_publish::UpdateTarget {
                        workshop_id,
                        title,
                        tags,
                        preview_url,
                        saved_path: self.prepare_publish_saved_path(workshop_id),
                    }),
                    ignored_patterns: self.prepare_publish_ignored_patterns(),
                    upscale_icon_default: self.prepare_publish_upscale_default(),
                },
            )),
            search::SelectionAction::SteamWorkshop { workshop_id } => {
                self.open_url_task(workshop_url::workshop_item_url(workshop_id))
            }
            search::SelectionAction::InstalledAddonFile {
                addon_path,
                addon_title,
                workshop_id,
                entry_path,
            } => self.apply_preview_gma_message(preview_gma::Message::OpenRequested(
                preview_gma::OpenTarget::new(addon_path, addon_title, workshop_id)
                    .with_initial_entry_preview(entry_path),
            )),
        }
    }
}
