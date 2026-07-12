use super::{
    App, PublishedFileId, RootMessage, Task, preview_gma, run_size_analyzer_preview_urls,
    size_analyzer, spawn_blocking_detached_or_warn, stream,
};

impl App {
    pub(super) fn size_analyzer_preview_url_task(
        &self,
        ids: Vec<PublishedFileId>,
    ) -> Task<RootMessage> {
        let ctx = self.ctx.clone();
        Task::stream(stream::channel(100, async move |output| {
            let _scheduled = spawn_blocking_detached_or_warn(
                &ctx,
                "size-analyzer-preview-url-resolve",
                "size-analyzer preview URL worker",
                move |app| {
                    run_size_analyzer_preview_urls(&app, &ids, output);
                },
            );
        }))
    }

    pub(super) fn size_analyzer_context_menu_task(
        &mut self,
        menu: &size_analyzer::ContextMenuRequest,
    ) -> Task<RootMessage> {
        log::debug!(
            "Size Analyzer prepared {} context-menu entries for {}",
            menu.entries()
                .iter()
                .filter(|entry| !entry.separator_row())
                .count(),
            menu.target().title()
        );
        self.open_size_analyzer_context_menu(menu)
    }

    pub(super) fn size_analyzer_preview_task(
        &mut self,
        target: &size_analyzer::PreviewTarget,
    ) -> Task<RootMessage> {
        log::info!(
            "Size Analyzer requested Preview GMA for {} ({})",
            target.title,
            target.path.display()
        );
        self.apply_preview_gma_message(preview_gma::Message::OpenRequested(
            preview_gma::OpenTarget::new(
                target.path.clone(),
                target.title.clone(),
                target.workshop_id,
            ),
        ))
    }
}
