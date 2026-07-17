use super::{App, RootMessage, Task, search, thumbnail_demand};
use crate::bridge::library::LibrarySnapshot;

/// Must equal the grids' demand edge (installed addons, size analyzer, My
/// Workshop all use 256) so a warm fill lands on the exact keys they read.
const WARM_THUMBNAIL_MAX_EDGE: u32 = 256;

impl App {
    pub(super) fn thumbnail_scale_changed_task(&mut self) -> Task<RootMessage> {
        let _changed = self.state.my_workshop.invalidate_ready_thumbnails();
        let _changed = self.state.installed_addons.invalidate_ready_thumbnails();
        let _changed = self.state.search.invalidate_ready_thumbnails();
        let _changed = self.state.preview_gma.invalidate_ready_thumbnail();
        let _invalidation = self.state.size_analyzer.invalidate_ready_thumbnails();

        let demand_sets = vec![
            self.state.my_workshop.thumbnail_demands(),
            self.state.installed_addons.thumbnail_demands(),
            self.search_thumbnail_demand_set(),
            self.state.prepare_publish.thumbnail_demands(),
            self.state.preview_gma.thumbnail_demands(),
            self.state.size_analyzer.thumbnail_demands(),
        ];

        self.thumbnails
            .set_demand_sets(&self.ctx, demand_sets)
            .map(RootMessage::ThumbnailDemand)
    }

    pub(super) fn my_workshop_thumbnail_demands(&mut self) -> Task<RootMessage> {
        let _released = self.state.my_workshop.release_offscreen_thumbnails();

        self.thumbnails
            .set_demands(&self.ctx, self.state.my_workshop.thumbnail_demands())
            .map(RootMessage::ThumbnailDemand)
    }

    pub(super) fn prepare_publish_thumbnail_demands(&mut self) -> Task<RootMessage> {
        self.thumbnails
            .set_demands(&self.ctx, self.state.prepare_publish.thumbnail_demands())
            .map(RootMessage::ThumbnailDemand)
    }

    pub(super) fn search_thumbnail_demands(&mut self) -> Task<RootMessage> {
        let viewport_height = self.search_dropdown_list_viewport_height();
        let metadata_task = self.search_thumbnail_metadata_task(viewport_height);
        let demands = self.state.search.thumbnail_demands(viewport_height);
        let thumbnail_task = self
            .thumbnails
            .set_demands(&self.ctx, demands)
            .map(RootMessage::ThumbnailDemand);
        Task::batch([metadata_task, thumbnail_task])
    }

    pub(super) fn preview_gma_thumbnail_demands(&mut self) -> Task<RootMessage> {
        self.thumbnails
            .set_demands(&self.ctx, self.state.preview_gma.thumbnail_demands())
            .map(RootMessage::ThumbnailDemand)
    }

    pub(super) fn search_thumbnail_demand_set(&self) -> thumbnail_demand::DemandSet {
        self.state
            .search
            .thumbnail_demands(self.search_dropdown_list_viewport_height())
    }

    fn search_thumbnail_metadata_task(&mut self, viewport_height: f32) -> Task<RootMessage> {
        let Some((generation, item_ids)) = self
            .state
            .search
            .take_thumbnail_metadata_request(viewport_height)
        else {
            return Task::none();
        };

        self.search_metadata_task(generation, item_ids)
    }

    fn search_dropdown_list_viewport_height(&self) -> f32 {
        search::dropdown_list_viewport_height(
            &self.state.search,
            self.state.viewport_size,
            &self.state.tokens,
        )
    }

    pub(super) fn installed_addons_thumbnail_demands(&mut self) -> Task<RootMessage> {
        let _released = self.state.installed_addons.release_offscreen_thumbnails();

        self.thumbnails
            .set_demands(&self.ctx, self.state.installed_addons.thumbnail_demands())
            .map(RootMessage::ThumbnailDemand)
    }

    /// Once per session, after the first library snapshot: resolve every
    /// library item's cached preview URL off-thread, then trickle the whole
    /// library into the thumbnail disk cache at the lowest priority. Also the
    /// point where the disk-cache budget learns the library's size. Items
    /// without cached metadata (fresh install, Steam offline) simply are not
    /// warmed this session — the interactive paths fetch them as ever.
    pub(super) fn warm_library_kick_task(
        &mut self,
        snapshot: &LibrarySnapshot,
    ) -> Task<RootMessage> {
        if self.library_warm_kicked || snapshot.addons.is_empty() {
            return Task::none();
        }
        self.library_warm_kicked = true;
        self.thumbnails
            .scale_disk_cache_to_library(snapshot.addons.len());

        let ids = snapshot
            .addons
            .iter()
            .filter_map(|addon| addon.workshop_id)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Task::none();
        }
        self.ctx
            .run_blocking("warm-library-preview-urls", move |app| {
                let (cached, _stale) = app.resolve_workshop_metadata(&ids);
                cached
                    .into_iter()
                    .filter_map(|metadata| {
                        metadata
                            .preview_url
                            .map(|preview_url| (metadata.id, preview_url))
                    })
                    .collect::<Vec<_>>()
            })
            .map(|result| match result {
                Ok(preview_urls) => RootMessage::WarmLibraryResolved(preview_urls),
                Err(error) => {
                    log::debug!("library warm preview-URL resolve failed: {error}");
                    RootMessage::WarmLibraryResolved(Vec::new())
                }
            })
    }

    pub(super) fn warm_library_demands_task(
        &mut self,
        preview_urls: Vec<(crate::bridge::domain::PublishedFileId, String)>,
    ) -> Task<RootMessage> {
        if preview_urls.is_empty() {
            return Task::none();
        }
        let demands = preview_urls
            .into_iter()
            .filter(|(_, url)| !url.is_empty())
            .map(|(id, url)| thumbnail_demand::Demand {
                id: thumbnail_demand::DemandId::new(id.to_string()),
                input: crate::media::thumbnail_worker::ThumbnailInput::from_url(url),
                logical_max_edge: WARM_THUMBNAIL_MAX_EDGE,
                priority: thumbnail_demand::Priority::WarmLibrary,
            })
            .collect();
        let set = thumbnail_demand::DemandSet {
            owner: thumbnail_demand::Owner::WarmLibrary,
            generation: 0,
            replace: thumbnail_demand::ReplaceMode::Owner,
            demands,
        };
        self.thumbnails
            .set_demands(&self.ctx, set)
            .map(RootMessage::ThumbnailDemand)
    }

    pub(super) fn size_analyzer_thumbnail_demands(&mut self) -> Task<RootMessage> {
        self.thumbnails
            .set_demands(&self.ctx, self.state.size_analyzer.thumbnail_demands())
            .map(RootMessage::ThumbnailDemand)
    }
}

pub(super) fn log_thumbnail_delivery(delivery: &thumbnail_demand::Delivery) {
    match &delivery.result {
        thumbnail_demand::DeliveryResult::Ready(ready) => {
            let metadata = ready.metadata();
            let _handle_id = ready.handle().id();
            log::debug!(
                "thumbnail ready for {:?} generation {} id {} key {:?} (ready key {:?}, {}x{})",
                delivery.owner,
                delivery.generation,
                delivery.id.as_str(),
                delivery.key,
                ready.key(),
                metadata.width,
                metadata.height
            );
        }
        thumbnail_demand::DeliveryResult::Placeholder(_) => {}
        thumbnail_demand::DeliveryResult::Failed { error } => {
            log::debug!(
                "thumbnail failed for {:?} generation {} id {} key {:?}: {error}",
                delivery.owner,
                delivery.generation,
                delivery.id.as_str(),
                delivery.key
            );
        }
    }
}
