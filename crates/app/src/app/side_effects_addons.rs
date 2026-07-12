use gmpublished_backend::error_key::keys;

use super::{
    App, PublishedFileId, RootMessage, Task, UiError, flatten_blocking_ui_result, installed_addons,
    my_workshop, prepare_publish, preview_gma, run_installed_metadata_refresh, send_root_message,
    spawn_blocking_detached_or_warn, steam_session, stream,
};

impl App {
    pub(super) fn my_workshop_page_task(
        &mut self,
        generation: u64,
        page: u32,
    ) -> Task<RootMessage> {
        if let Some(task) = self
            .defer_steam_operation(steam_session::PendingRetry::MyWorkshopPage { generation, page })
        {
            return task;
        }
        self.my_workshop_page_worker_task(generation, page)
    }

    pub(super) fn my_workshop_page_worker_task(
        &self,
        generation: u64,
        page: u32,
    ) -> Task<RootMessage> {
        self.ctx
            .run_blocking("my-workshop-page", move |app| {
                my_workshop::browse_page(app, page)
            })
            .map(move |result| {
                RootMessage::MyWorkshop(my_workshop::Message::PageCompleted(
                    generation,
                    page,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn my_workshop_stats_refresh_task(
        &mut self,
        generation: u64,
        pages: u32,
    ) -> Task<RootMessage> {
        if let Some(task) =
            self.defer_steam_operation(steam_session::PendingRetry::MyWorkshopStats {
                generation,
                pages,
            })
        {
            return task;
        }
        self.my_workshop_stats_refresh_worker_task(generation, pages)
    }

    pub(super) fn my_workshop_stats_refresh_worker_task(
        &self,
        generation: u64,
        pages: u32,
    ) -> Task<RootMessage> {
        self.ctx
            .run_blocking("my-workshop-stats-refresh", move |app| {
                my_workshop::refresh_subscription_counts(app, pages)
            })
            .map(move |result| {
                RootMessage::MyWorkshop(my_workshop::Message::StatsRefreshCompleted(
                    generation,
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn my_workshop_prepare_publish_task(
        &self,
        target: my_workshop::PreparePublishTarget,
    ) -> Task<RootMessage> {
        let target = match target {
            my_workshop::PreparePublishTarget::New => prepare_publish::OpenTarget::New,
            my_workshop::PreparePublishTarget::Update(update) => {
                let workshop_id = update.workshop_id;
                prepare_publish::OpenTarget::Update(prepare_publish::UpdateTarget {
                    workshop_id,
                    title: update.title,
                    tags: update.tags,
                    preview_url: update.preview_url,
                    saved_path: self.prepare_publish_saved_path(workshop_id),
                })
            }
        };
        Task::done(RootMessage::PreparePublish(
            prepare_publish::Message::OpenRequested {
                target,
                ignored_patterns: self.prepare_publish_ignored_patterns(),
                upscale_icon_default: self.prepare_publish_upscale_default(),
            },
        ))
    }

    pub(super) fn my_workshop_context_menu_task(
        &mut self,
        menu: my_workshop::ContextMenuRequest,
    ) -> Task<RootMessage> {
        log::debug!(
            "My Workshop prepared {} context-menu entries for {}",
            menu.entries.len(),
            menu.workshop_id
        );
        self.open_my_workshop_context_menu(menu)
    }

    pub(super) fn installed_addons_metadata_task(
        &mut self,
        generation: u64,
        item_ids: Vec<PublishedFileId>,
    ) -> Task<RootMessage> {
        if let Some(task) =
            self.defer_steam_operation(steam_session::PendingRetry::InstalledMetadata {
                generation,
                item_ids: item_ids.clone(),
            })
        {
            return task;
        }
        let worker_item_ids = item_ids.clone();
        let delivery_item_ids = item_ids;
        self.ctx
            .run_blocking("installed-addons-metadata", move |app| {
                installed_addons::resolve_metadata(app, &worker_item_ids)
            })
            .map(move |result| {
                RootMessage::InstalledAddons(installed_addons::Message::MetadataCompleted(
                    generation,
                    delivery_item_ids.clone(),
                    result.map_err(|error| UiError::from(&error)),
                ))
            })
    }

    pub(super) fn installed_addons_metadata_refresh_task(
        &mut self,
        generation: u64,
        item_ids: Vec<PublishedFileId>,
    ) -> Task<RootMessage> {
        if let Some(task) =
            self.defer_steam_operation(steam_session::PendingRetry::InstalledMetadataRefresh {
                generation,
                item_ids: item_ids.clone(),
            })
        {
            return task;
        }
        let ctx = self.ctx.clone();
        Task::stream(stream::channel(100, async move |output| {
            let mut schedule_error_output = output.clone();
            let scheduled = spawn_blocking_detached_or_warn(
                &ctx,
                "installed-addons-metadata-refresh",
                "installed-addons metadata refresh worker",
                move |app| {
                    run_installed_metadata_refresh(&app, generation, &item_ids, output);
                },
            );
            if !scheduled {
                let _sent = send_root_message(
                    &mut schedule_error_output,
                    RootMessage::InstalledAddons(
                        installed_addons::Message::MetadataRefreshCompleted(
                            generation,
                            Err(UiError::new(keys::STEAM_ERROR)),
                        ),
                    ),
                );
            }
        }))
    }

    pub(super) fn installed_addons_preview_task(
        &mut self,
        target: installed_addons::PreviewTarget,
    ) -> Task<RootMessage> {
        log::info!(
            "Installed Addons requested Preview GMA for {} ({})",
            target.title,
            target.path.display()
        );
        self.apply_preview_gma_message(preview_gma::Message::OpenRequested(
            preview_gma::OpenTarget::new(
                target.path.clone(),
                target.title.clone(),
                target.workshop_id,
            )
            .with_seed(preview_gma::OpenSeed {
                preview_url: target.preview_url.clone(),
                subscription_count: Some(target.subscription_count),
                score_bucket: Some(target.score_bucket),
                score_label: Some(target.score_label),
            }),
        ))
    }

    pub(super) fn installed_addons_context_menu_task(
        &mut self,
        menu: installed_addons::ContextMenuRequest,
    ) -> Task<RootMessage> {
        log::debug!(
            "Installed Addons prepared {} context-menu entries for {}",
            menu.entries.len(),
            menu.path.display()
        );
        self.open_installed_addons_context_menu(menu)
    }
}
