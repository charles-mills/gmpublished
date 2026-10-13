use super::{
    App, Instant, RootMessage, State, Task, UiDensity, prepare_publish, preview_gma,
    resolve_tokens, search,
};
use crate::widgets::file_browser;

impl State {
    pub(super) fn resolve_ui_tokens(&mut self) {
        self.tokens = resolve_tokens(self.theme_preset, self.system_scheme, self.accent_inputs)
            .with_density_progress(self.density_progress, self.text_size_percent);
        self.tokens.motion.reduce_motion = self.reduce_motion;
        self.tokens.motion.layout_animating = self.density_motion.needs_ticks();
    }

    pub(super) fn transition_density(&mut self, density: UiDensity, now: Instant, animate: bool) {
        self.density = density;
        let target = if density == UiDensity::Compact {
            1.0
        } else {
            0.0
        };
        if self.reduce_motion || !animate {
            self.density_motion.snap(target);
        } else {
            self.density_motion.go(target, now);
        }
        self.density_progress = self.density_motion.current(now);
    }

    pub(super) fn tick_density_motion(&mut self, now: Instant) -> bool {
        if !self.density_motion.needs_ticks() {
            return false;
        }
        let _settled = self.density_motion.tick(now);
        self.density_progress = self.density_motion.current(now);
        self.resolve_ui_tokens();
        true
    }

    pub(super) fn initialize_ui_metrics(&mut self) {
        self.transition_density(self.density, Instant::now(), false);
        self.resolve_ui_tokens();
        let _closed = self
            .features
            .modal_stack
            .set_reduce_motion(self.reduce_motion);
        let _search_closed = self.features.search.set_reduce_motion(self.reduce_motion);
        self.features
            .context_menu
            .set_reduce_motion(self.reduce_motion);
        self.features
            .tasks_overlay
            .set_reduce_motion(self.reduce_motion);
        self.features.shell.set_reduce_motion(self.reduce_motion);
        self.features
            .prepare_publish
            .set_reduce_motion(self.reduce_motion);
        let tokens = self.tokens;
        let _effects = self.features.my_workshop.apply_ui_metrics(&tokens);
        let _effects = self.features.installed_addons.apply_ui_metrics(&tokens);
        let _offset = self
            .features
            .search
            .set_row_height(search::result_row_height(&tokens));
        let _offset = self
            .features
            .preview_gma
            .set_browser_row_height(file_browser::row_height(&tokens));
        let _offset = self
            .features
            .prepare_publish
            .set_browser_row_height(file_browser::row_height(&tokens));
        self.features
            .size_analyzer
            .set_text_scale(tokens.text_scale());
    }
}

impl App {
    pub(super) fn refresh_ui_metrics_task(&mut self) -> Task<RootMessage> {
        let reduce = self.state.reduce_motion;
        let closed = self.state.features.modal_stack.set_reduce_motion(reduce);
        let search_closed = self.state.features.search.set_reduce_motion(reduce);
        self.state.features.context_menu.set_reduce_motion(reduce);
        self.state.features.tasks_overlay.set_reduce_motion(reduce);
        self.state.features.shell.set_reduce_motion(reduce);
        self.state
            .features
            .prepare_publish
            .set_reduce_motion(reduce);
        let mut motion_tasks = closed
            .into_iter()
            .map(|modal| self.finish_modal_close_task(modal))
            .collect::<Vec<_>>();
        if search_closed {
            motion_tasks.push(self.search_thumbnail_demands());
        }
        let tokens = self.state.tokens;
        let workshop = self.state.features.my_workshop.apply_ui_metrics(&tokens);
        let installed = self
            .state
            .features
            .installed_addons
            .apply_ui_metrics(&tokens);
        let search_offset = self
            .state
            .features
            .search
            .set_row_height(search::result_row_height(&tokens));
        let browser_offset = self
            .state
            .features
            .preview_gma
            .set_browser_row_height(file_browser::row_height(&tokens));
        let publish_offset = self
            .state
            .features
            .prepare_publish
            .set_browser_row_height(file_browser::row_height(&tokens));
        self.state
            .features
            .size_analyzer
            .set_text_scale(tokens.text_scale());
        Task::batch([
            Task::batch(motion_tasks),
            self.batch_effects(workshop, Self::run_my_workshop_effect),
            self.batch_effects(installed, Self::run_installed_addons_effect),
            scroll_to(search::dropdown_scrollable_id(), search_offset),
            scroll_to(preview_gma::browser_rows_scrollable_id(), browser_offset),
            scroll_to(
                prepare_publish::browser_rows_scrollable_id(),
                publish_offset,
            ),
        ])
    }
}

fn scroll_to(id: iced::widget::Id, offset: Option<f32>) -> Task<RootMessage> {
    offset.map_or_else(Task::none, |y| {
        iced::widget::operation::scroll_to(
            id,
            iced::widget::scrollable::AbsoluteOffset { x: 0.0, y },
        )
    })
}
