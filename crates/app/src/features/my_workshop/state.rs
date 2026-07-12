use std::collections::HashMap;
use std::ops::Range;
use std::time::Instant;

use iced::widget::image;

use crate::backend::Settings;
use crate::backend::domain::PublishedFileId;
use crate::backend::ui_error::UiError;
use crate::format::DownloadCountFormatter;
use crate::media::thumbnail_demand;
use crate::widgets::addon_grid;

use super::model::{
    self, COUNT_ROLL_TICK_INTERVAL, ContextMenuRequest, FIRST_WORKSHOP_PAGE, PUBLISH_NEW_ROW_ID,
    PageResult, PreparePublishTarget, Row,
};

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "route visibility, load progress, and playback/focus flags are independent UI state"
)]
pub struct State {
    route_visible: bool,
    grid: addon_grid::State,
    load_status: LoadStatus,
    generation: u64,
    rows: Vec<Row>,
    publish_new_title: String,
    next_page: u32,
    loaded_pages: u32,
    total_count: u32,
    loading_page: bool,
    complete: bool,
    stats_in_flight: bool,
    last_animation_tick: Option<Instant>,
    play_gifs_by_default: bool,
    window_focused: bool,
    download_count_formatter: DownloadCountFormatter,
    last_roll_tick: Option<Instant>,
    pending_prepare_publish: Option<PreparePublishTarget>,
    pending_context_menu: Option<ContextMenuRequest>,
}

impl Default for State {
    fn default() -> Self {
        let play_gifs_by_default = Settings::default().play_gifs_by_default;
        let download_count_formatter = DownloadCountFormatter::default();
        let mut grid = addon_grid::State::default();
        let _ = grid.set_items(model::grid_items(
            &[],
            play_gifs_by_default,
            download_count_formatter,
            "",
        ));

        Self {
            route_visible: false,
            grid,
            load_status: LoadStatus::Idle,
            generation: 0,
            rows: Vec::new(),
            publish_new_title: String::new(),
            next_page: FIRST_WORKSHOP_PAGE,
            loaded_pages: 0,
            total_count: 0,
            loading_page: false,
            complete: false,
            stats_in_flight: false,
            last_animation_tick: None,
            play_gifs_by_default,
            window_focused: true,
            download_count_formatter,
            last_roll_tick: None,
            pending_prepare_publish: None,
            pending_context_menu: None,
        }
    }
}

impl State {
    pub(crate) const fn is_route_visible(&self) -> bool {
        self.route_visible
    }

    pub(crate) const fn load_status(&self) -> &LoadStatus {
        &self.load_status
    }

    pub(crate) fn loaded_count(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn total_count(&self) -> usize {
        self.total_count as usize
    }

    #[cfg(test)]
    pub(crate) fn publish_new_title_for_test(&self) -> &str {
        &self.publish_new_title
    }

    pub(crate) const fn grid(&self) -> &addon_grid::State {
        &self.grid
    }

    pub(crate) fn workshop_id_for_card(&self, id: &str) -> Option<PublishedFileId> {
        if id == PUBLISH_NEW_ROW_ID {
            return None;
        }
        self.rows
            .iter()
            .find(|row| row.id() == id)
            .map(Row::workshop_id)
    }

    pub(crate) fn drag_thumbnail_for_card(&self, id: &str) -> Option<image::Handle> {
        self.rows
            .iter()
            .find(|row| row.id() == id)
            .and_then(Row::drag_thumbnail)
    }

    pub(crate) fn thumbnail_demands(&self) -> thumbnail_demand::DemandSet {
        if !self.route_visible {
            return model::empty_thumbnail_demands();
        }

        model::thumbnail_demands(&self.rows, self.visible_row_range(), self.generation)
    }

    pub(crate) fn apply_thumbnail_delivery(
        &mut self,
        delivery: &thumbnail_demand::Delivery,
    ) -> bool {
        if delivery.owner != model::thumbnail_owner() || delivery.generation != self.generation {
            return false;
        }

        let mut changed = false;
        for row in &mut self.rows {
            changed |= row.apply_thumbnail_delivery(delivery);
        }
        if changed {
            self.refresh_item_thumbnails();
        }
        changed
    }

    pub(crate) fn invalidate_ready_thumbnails(&mut self) -> bool {
        let changed = model::invalidate_ready_thumbnails(&mut self.rows);
        if changed {
            self.sync_grid_items();
            self.last_animation_tick = None;
        }
        changed
    }

    /// Drops Ready thumbnails outside the visible+prefetch window (all of
    /// them while the route is hidden); the demand/cache path re-delivers.
    pub(crate) fn release_offscreen_thumbnails(&mut self) -> bool {
        let changed = if self.route_visible {
            let visible_range = self.visible_row_range();
            model::release_offscreen_thumbnails(&mut self.rows, visible_range)
        } else {
            model::invalidate_ready_thumbnails(&mut self.rows)
        };
        if changed {
            self.refresh_item_thumbnails();
        }
        changed
    }

    pub(crate) fn has_active_animations(&self) -> bool {
        self.window_focused
            && self.route_visible
            && self
                .rows
                .get(self.visible_row_range())
                .unwrap_or_default()
                .iter()
                .any(|row| row.has_active_animation(self.play_gifs_by_default))
    }

    pub(crate) fn needs_card_motion_ticks(&self) -> bool {
        self.route_visible && self.grid.needs_visible_card_ticks()
    }

    pub(crate) fn set_play_gifs_by_default(&mut self, enabled: bool) -> bool {
        if self.play_gifs_by_default == enabled {
            return false;
        }

        self.play_gifs_by_default = enabled;
        self.last_animation_tick = None;
        self.sync_grid_items();
        true
    }

    /// GIF playback pauses on the current frame while the window is
    /// unfocused, so the clock subscription can drop to idle.
    pub(crate) fn set_window_focused(&mut self, focused: bool) -> bool {
        if self.window_focused == focused {
            return false;
        }

        self.window_focused = focused;
        self.last_animation_tick = None;
        true
    }

    pub(crate) fn set_download_count_formatter(
        &mut self,
        formatter: DownloadCountFormatter,
    ) -> bool {
        if self.download_count_formatter == formatter {
            return false;
        }

        self.download_count_formatter = formatter;
        self.sync_grid_items();
        true
    }

    pub(crate) fn set_publish_new_title(&mut self, title: String) -> bool {
        if self.publish_new_title == title {
            return false;
        }

        self.publish_new_title = title;
        self.sync_grid_items();
        true
    }

    pub(crate) fn has_active_count_rolls(&self) -> bool {
        self.route_visible && self.rows.iter().any(Row::has_active_count_roll)
    }

    pub(super) const fn grid_mut(&mut self) -> &mut addon_grid::State {
        &mut self.grid
    }

    pub(super) fn enter_route(&mut self) -> Option<(u64, u32)> {
        self.route_visible = true;
        if matches!(self.load_status, LoadStatus::Idle | LoadStatus::Error(_))
            && self.rows.is_empty()
        {
            return self.begin_next_page();
        }
        self.reconcile_visible_counts();
        None
    }

    pub(super) fn exit_route(&mut self) {
        self.route_visible = false;
        self.stats_in_flight = false;
        self.last_animation_tick = None;
        self.last_roll_tick = None;
        self.pending_prepare_publish = None;
        self.pending_context_menu = None;
        let _ = self.grid.set_page_status(false, false);
    }

    pub(super) fn begin_next_page(&mut self) -> Option<(u64, u32)> {
        if !self.route_visible || self.loading_page || self.complete {
            return None;
        }

        if self.next_page == FIRST_WORKSHOP_PAGE && self.rows.is_empty() {
            self.generation = self.generation.wrapping_add(1).max(1);
            self.loaded_pages = 0;
            self.total_count = 0;
            self.complete = false;
            self.stats_in_flight = false;
            self.last_animation_tick = None;
            self.last_roll_tick = None;
            self.pending_prepare_publish = None;
            self.pending_context_menu = None;
            self.load_status = LoadStatus::Loading;
            self.rows.clear();
            self.sync_grid_items();
        } else if !matches!(self.load_status, LoadStatus::Error(_)) {
            self.load_status = LoadStatus::Ready;
        }

        self.loading_page = true;
        let _ = self.grid.set_page_status(true, false);
        Some((self.generation, self.next_page))
    }

    pub(super) fn apply_page(
        &mut self,
        generation: u64,
        page: u32,
        result: Result<PageResult, UiError>,
    ) {
        if generation != self.generation {
            return;
        }

        self.loading_page = false;
        match result {
            Ok(result) if result.page == page => {
                self.apply_page_result(result);
            }
            Ok(_) => {
                self.load_status = LoadStatus::Error("stale My Workshop page result".to_owned());
            }
            Err(error) => {
                self.load_status = LoadStatus::Error(error.to_string());
            }
        }
        self.sync_grid_items();
    }

    pub(super) fn request_stats_refresh(&mut self) -> Option<(u64, u32)> {
        if !self.route_visible || self.stats_in_flight || self.loaded_pages == 0 {
            return None;
        }

        self.stats_in_flight = true;
        Some((self.generation, self.loaded_pages))
    }

    pub(super) fn apply_stats_counts(
        &mut self,
        generation: u64,
        result: Result<HashMap<PublishedFileId, u64>, UiError>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }

        self.stats_in_flight = false;
        if !self.route_visible {
            return false;
        }

        let Ok(counts) = result else {
            return false;
        };
        if counts.is_empty() {
            return false;
        }

        let mut changed = false;
        for row in &mut self.rows {
            if let Some(count) = counts.get(&row.workshop_id()) {
                changed |= row.record_actual_count(*count);
            }
        }
        changed |= self.reconcile_visible_counts();
        if changed {
            self.sync_grid_items();
        }
        changed
    }

    pub(super) fn tick_count_rolls(&mut self, now: Instant) -> bool {
        if !self.has_active_count_rolls() {
            self.last_roll_tick = None;
            return false;
        }

        let elapsed = self
            .last_roll_tick
            .and_then(|last| now.checked_duration_since(last))
            .unwrap_or(COUNT_ROLL_TICK_INTERVAL);
        self.last_roll_tick = Some(now);

        let mut changed = false;
        for row in &mut self.rows {
            changed |= row.advance_count_roll(elapsed);
        }
        if !self.rows.iter().any(Row::has_active_count_roll) {
            self.last_roll_tick = None;
        }
        if changed {
            self.sync_grid_items();
        }
        changed
    }

    pub(super) fn tick_visible_card_motion(&mut self, now: Instant) {
        if self.route_visible {
            self.grid.tick_visible_card_motion(now);
        }
    }

    pub(super) fn tick_visible_animations(&mut self, now: Instant) -> bool {
        if !self.has_active_animations() {
            self.last_animation_tick = None;
            return false;
        }

        let elapsed = self
            .last_animation_tick
            .and_then(|last| now.checked_duration_since(last))
            .unwrap_or(crate::media::thumbnail_animation::ANIMATION_TICK_INTERVAL);
        self.last_animation_tick = Some(now);

        let visible = self.visible_row_range();
        let mut changed = false;
        if let Some(rows) = self.rows.get_mut(visible.clone()) {
            for row in rows {
                changed |= row.advance_animation(elapsed, self.play_gifs_by_default);
            }
        }
        if changed {
            // Swap advanced frames in place; a full sync_grid_items rebuild
            // (every card re-allocated + re-layout) per 16ms tick is churn.
            if let Some(rows) = self.rows.get(visible.clone()) {
                for (offset, row) in rows.iter().enumerate() {
                    let thumbnail = row.card_thumbnail(self.play_gifs_by_default);
                    // Grid item 0 is the publish-new lead card; rows start at 1.
                    let _ = self.grid.update_item_thumbnail(
                        visible.start + offset + 1,
                        &row.id(),
                        thumbnail,
                    );
                }
            }
        }
        changed
    }

    pub(super) fn take_prepare_publish_target(&mut self, id: &str) -> Option<PreparePublishTarget> {
        let target = if id == PUBLISH_NEW_ROW_ID {
            Some(PreparePublishTarget::New)
        } else {
            self.rows
                .iter()
                .find(|row| row.id() == id)
                .and_then(Row::prepare_publish_target)
        }?;
        self.pending_prepare_publish = Some(target.clone());
        Some(target)
    }

    pub(super) fn take_context_menu(
        &mut self,
        id: &str,
        position: iced::Point,
    ) -> Option<ContextMenuRequest> {
        let mut request = self
            .rows
            .iter()
            .find(|row| row.id() == id)?
            .context_menu()?;
        request.position = position;
        self.pending_context_menu = Some(request.clone());
        Some(request)
    }

    #[cfg(feature = "debug")]
    pub(crate) fn adjust_subscription_count(
        &mut self,
        workshop_id: PublishedFileId,
        delta: i64,
    ) -> bool {
        let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| row.workshop_id() == workshop_id)
        else {
            return false;
        };
        if !row.adjust_subscription_count(delta) {
            return false;
        }

        let changed = row.reconcile_displayed_count();
        if changed {
            self.sync_grid_items();
        }
        changed
    }

    #[cfg(feature = "debug")]
    pub(crate) fn hide_workshop_id(&mut self, workshop_id: PublishedFileId) -> bool {
        let previous_len = self.rows.len();
        self.rows.retain(|row| row.workshop_id() != workshop_id);
        if self.rows.len() == previous_len {
            return false;
        }
        self.total_count = self.total_count.saturating_sub(1);
        self.pending_prepare_publish = None;
        self.pending_context_menu = None;
        self.sync_grid_items();
        true
    }

    pub(super) fn set_card_hovered(&mut self, id: &str, hovered: bool) -> bool {
        if id == PUBLISH_NEW_ROW_ID {
            return false;
        }

        let Some((index, row)) = self
            .rows
            .iter_mut()
            .enumerate()
            .find(|(_, row)| row.id() == id)
        else {
            return false;
        };
        // The play flag is recorded either way (a GIF delivered mid-hover
        // starts playing), but only a row that already has an animation
        // changes appearance — and then a thumbnail swap in place suffices.
        if !row.set_thumbnail_play_requested(hovered) || !row.has_animation() {
            return false;
        }
        let thumbnail = row.card_thumbnail(self.play_gifs_by_default);
        // Grid item 0 is the publish-new lead card; rows start at 1.
        let _ = self
            .grid
            .update_item_thumbnail(index + 1, &row.id(), thumbnail);
        true
    }

    pub(super) fn reconcile_visible_counts(&mut self) -> bool {
        if !self.route_visible {
            return false;
        }

        let visible = self.visible_row_range();
        let mut changed = false;
        if let Some(rows) = self.rows.get_mut(visible) {
            for row in rows {
                changed |= row.reconcile_displayed_count();
            }
        }
        if changed {
            self.sync_grid_items();
        }
        changed
    }

    fn apply_page_result(&mut self, page: PageResult) {
        let page_empty = page.rows.is_empty();
        self.total_count = page.total;
        self.next_page = page.page.saturating_add(1);
        self.loaded_pages = self.loaded_pages.max(page.page);
        self.rows.extend(page.rows);
        self.complete = self.total_count == 0
            || page_empty
            || usize::try_from(self.total_count).is_ok_and(|total| self.rows.len() >= total);
        self.load_status = if self.rows.is_empty() {
            LoadStatus::Empty
        } else {
            LoadStatus::Ready
        };
    }

    /// Pushes every row's current thumbnail into the grid in place. A
    /// thumbnail never changes card geometry, so thumbnail-only changes
    /// (delivery, offscreen release, hover play/pause) skip the full
    /// `sync_grid_items` rebuild — re-allocating every card and re-measuring
    /// every title per scroll event is what makes large libraries lag.
    fn refresh_item_thumbnails(&mut self) {
        for (index, row) in self.rows.iter().enumerate() {
            let thumbnail = row.card_thumbnail(self.play_gifs_by_default);
            // Grid item 0 is the publish-new lead card; rows start at 1.
            let _ = self
                .grid
                .update_item_thumbnail(index + 1, &row.id(), thumbnail);
        }
    }

    fn sync_grid_items(&mut self) {
        let _ = self.grid.set_items(model::grid_items(
            &self.rows,
            self.play_gifs_by_default,
            self.download_count_formatter,
            &self.publish_new_title,
        ));
        let has_more_pages = !self.complete && !matches!(self.load_status, LoadStatus::Error(_));
        let _ = self.grid.set_page_status(self.loading_page, has_more_pages);
    }

    fn visible_row_range(&self) -> Range<usize> {
        grid_range_to_row_range(self.grid.visible_item_range(), self.rows.len())
    }

    #[cfg(test)]
    pub(crate) fn begin_for_test(&mut self) -> (u64, u32) {
        self.route_visible = true;
        self.begin_next_page().expect("page request should start")
    }

    #[cfg(test)]
    pub(crate) fn row_for_test(&self, id: u64) -> Option<&Row> {
        self.rows.iter().find(|row| {
            row.workshop_id()
                == PublishedFileId::new(id).expect("test fixture ids are always nonzero")
        })
    }

    #[cfg(test)]
    pub(crate) fn push_rows_for_test(&mut self, rows: Vec<Row>, total_count: u32) {
        self.route_visible = true;
        self.generation = 1;
        self.rows = rows;
        self.total_count = total_count;
        self.loaded_pages = 1;
        self.next_page = 2;
        self.complete = false;
        self.load_status = LoadStatus::Ready;
        self.sync_grid_items();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadStatus {
    Idle,
    Loading,
    Ready,
    Empty,
    Error(String),
}

fn grid_range_to_row_range(range: Range<usize>, row_count: usize) -> Range<usize> {
    if row_count == 0 {
        return 0..0;
    }

    let start = range.start.saturating_sub(1).min(row_count);
    let end = range.end.saturating_sub(1).min(row_count);
    start..end.max(start)
}

#[cfg(test)]
mod tests {
    use crate::backend::domain::PublishedFileId;
    use crate::widgets::addon_grid;

    use super::super::model::PageResult;
    use super::{LoadStatus, Row, State, grid_range_to_row_range};

    fn ready_delivery(row_id: u64, generation: u64) -> crate::media::thumbnail_demand::Delivery {
        use crate::media::{thumbnail_demand, thumbnail_worker};

        let input = thumbnail_worker::ThumbnailInput::from_url(format!(
            "https://example.test/{row_id}.jpg"
        ));
        let key = input.cache_key(96);
        let metadata = thumbnail_worker::ThumbnailMetadata {
            width: 8,
            height: 8,
            source_width: 8,
            source_height: 8,
            max_edge: 96,
        };
        thumbnail_demand::Delivery {
            owner: super::super::model::thumbnail_owner(),
            generation,
            id: thumbnail_demand::DemandId::new(row_id.to_string()),
            key: key.clone(),
            result: thumbnail_demand::DeliveryResult::Ready(
                thumbnail_demand::ReadyThumbnail::for_test(key, metadata, vec![9_u8; 8 * 8 * 4]),
            ),
        }
    }

    fn ready_row_count(state: &State) -> usize {
        state
            .rows
            .iter()
            .filter(|row| row.has_ready_thumbnail_for_test())
            .count()
    }

    #[test]
    fn scrolled_away_rows_release_their_ready_thumbnails() {
        let mut state = State::default();
        let (generation, page) = state.begin_for_test();
        let rows: Vec<Row> = (1..=200)
            .map(|i| Row::for_test(i, &format!("Addon {i}"), 10))
            .collect();
        state.apply_page(
            generation,
            page,
            Ok(PageResult {
                page: 1,
                total: 200,
                rows,
            }),
        );

        // Lay the grid out so a real visible window exists.
        let _ = super::super::update(
            &mut state,
            super::super::Message::Grid(addon_grid::Message::ColumnsChanged(4)),
        );
        let _ = super::super::update(
            &mut state,
            super::super::Message::Grid(addon_grid::Message::ViewportResized(800, 600)),
        );
        let _ = super::super::update(
            &mut state,
            super::super::Message::Grid(addon_grid::Message::Scrolled(0)),
        );
        assert!(!state.visible_row_range().is_empty());

        for i in 1..=200 {
            assert!(state.apply_thumbnail_delivery(&ready_delivery(i, generation)));
        }
        assert_eq!(ready_row_count(&state), 200);

        assert!(state.release_offscreen_thumbnails());
        let retained = ready_row_count(&state);
        assert!(
            retained > 0 && retained <= 100,
            "visible+prefetch window should retain a bounded set, kept {retained}"
        );

        // Hidden route drops the rest; the demand/cache path re-delivers.
        state.exit_route();
        assert!(state.release_offscreen_thumbnails());
        assert_eq!(ready_row_count(&state), 0);
    }

    #[test]
    fn route_entry_marks_page_visible_and_requests_first_page() {
        let mut state = State::default();

        let request = state.enter_route();

        assert!(state.is_route_visible());
        assert_eq!(request, Some((1, 1)));
    }

    #[test]
    fn route_exit_hides_the_page() {
        let mut state = State::default();
        let _request = state.enter_route();

        state.exit_route();

        assert!(!state.is_route_visible());
    }

    #[test]
    fn page_completion_populates_loaded_rows() {
        let mut state = State::default();
        let (generation, page) = state.begin_for_test();

        state.apply_page(
            generation,
            page,
            Ok(PageResult {
                page: 1,
                total: 1,
                rows: vec![Row::for_test(42, "Addon 42", 10)],
            }),
        );

        assert_eq!(state.loaded_count(), 1);
        assert_eq!(state.total_count(), 1);
        assert!(matches!(state.load_status(), LoadStatus::Ready));
    }

    #[test]
    fn grid_range_skips_publish_new_leading_card() {
        assert_eq!(grid_range_to_row_range(0..1, 10), 0..0);
        assert_eq!(grid_range_to_row_range(0..4, 10), 0..3);
        assert_eq!(grid_range_to_row_range(2..5, 10), 1..4);
    }

    #[test]
    fn stats_refresh_reconciles_visible_rows() {
        let mut state = State::default();
        state.push_rows_for_test(vec![Row::for_test(42, "Addon", 10)], 1);
        let _ = addon_grid::update(
            state.grid_mut(),
            addon_grid::Message::ViewportResized(500, 500),
        );

        let changed = state.apply_stats_counts(
            1,
            Ok([(
                PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
                25,
            )]
            .into_iter()
            .collect()),
        );

        assert!(changed);
        let row = state.row_for_test(42).expect("row should remain");
        assert_eq!(row.displayed_count(), 10);
        assert!(row.has_active_count_roll());
    }

    #[test]
    fn route_reentry_with_loaded_rows_can_request_immediate_stats_refresh() {
        let mut state = State::default();
        state.push_rows_for_test(vec![Row::for_test(42, "Addon", 10)], 1);
        state.exit_route();

        let page_request = state.enter_route();
        let stats_request = state.request_stats_refresh();

        assert_eq!(page_request, None);
        assert_eq!(stats_request, Some((1, 1)));
    }
}
