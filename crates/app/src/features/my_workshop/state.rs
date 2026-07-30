use std::collections::HashMap;
use std::ops::Range;
use std::time::Instant;

use iced::widget::image;

use crate::bridge::Settings;
use crate::bridge::domain::PublishedFileId;
use crate::bridge::ui_error::UiError;
use crate::format::DownloadCountFormatter;
use crate::media::thumbnail_demand;
use crate::widgets::addon_grid;
use gmpublished_backend::error_key::keys;

use super::model::{
    self, COUNT_ROLL_TICK_INTERVAL, ContextMenuRequest, FIRST_WORKSHOP_PAGE, PUBLISH_NEW_ROW_ID,
    PageResult, PreparePublishTarget, Row,
};
use crate::generation::Generation;
use crate::widgets::grid_rows;

use crate::widgets::grid_rows::{CardId, GridRow};

/// What a grid card id refers to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Card {
    /// The synthetic first item, which has no backing row.
    PublishNew,
    Row(usize),
}

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "route visibility, load progress, and playback/focus flags are independent UI state"
)]
pub struct State {
    route_visible: bool,
    pane: grid_rows::GridPane,
    page_status: PageStatus,
    generation: Generation,
    rows: Vec<Row>,
    publish_new_title: String,
    next_page: u32,
    loaded_pages: u32,
    total_count: u32,
    loading_page: bool,
    complete: bool,
    stats_in_flight: bool,
    last_roll_tick: Option<Instant>,
    pending_prepare_publish: Option<PreparePublishTarget>,
    pending_context_menu: Option<ContextMenuRequest>,
}

impl Default for State {
    fn default() -> Self {
        let play_gifs_by_default = Settings::default().ui.play_gifs_by_default;

        Self {
            route_visible: false,
            pane: grid_rows::GridPane::new(play_gifs_by_default),
            page_status: PageStatus::Idle,
            generation: Generation::INITIAL,
            rows: Vec::new(),
            publish_new_title: String::new(),
            next_page: FIRST_WORKSHOP_PAGE,
            loaded_pages: 0,
            total_count: 0,
            loading_page: false,
            complete: false,
            stats_in_flight: false,
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

    pub(crate) const fn page_status(&self) -> &PageStatus {
        &self.page_status
    }

    /// A completed fetch that found nothing, as opposed to one still running
    /// or one that failed.
    pub(crate) fn is_loaded_and_empty(&self) -> bool {
        matches!(self.page_status, PageStatus::Loaded) && self.rows.is_empty()
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
        self.pane.grid()
    }

    /// Resolves a grid card id, which is either the synthetic publish-new row
    /// or a row's index. The sentinel is compared here and nowhere else.
    ///
    /// `Card::Row` indexes `rows` as it stands right now, so it must be
    /// consumed before the next mutation rather than stored.
    fn card(&self, id: &CardId) -> Option<Card> {
        if id.as_str() == PUBLISH_NEW_ROW_ID {
            return Some(Card::PublishNew);
        }
        self.pane.index_of(id).map(Card::Row)
    }

    pub(crate) fn workshop_id_for_card(&self, id: &CardId) -> Option<PublishedFileId> {
        match self.card(id)? {
            Card::PublishNew => None,
            Card::Row(index) => self.rows.get(index).map(Row::workshop_id),
        }
    }

    pub(crate) fn drag_thumbnail_for_card(&self, id: &CardId) -> Option<image::Handle> {
        match self.card(id)? {
            Card::PublishNew => None,
            Card::Row(index) => self.rows.get(index).and_then(Row::drag_thumbnail),
        }
    }

    pub(crate) fn thumbnail_demands(&self) -> thumbnail_demand::DemandSet {
        if !self.route_visible {
            return model::empty_thumbnail_demands();
        }

        self.pane
            .thumbnail_demands(&self.rows, self.generation, model::thumbnail_owner())
    }

    pub(crate) fn apply_thumbnail_delivery(
        &mut self,
        delivery: &thumbnail_demand::Delivery,
    ) -> bool {
        if delivery.owner != model::thumbnail_owner() || delivery.generation != self.generation {
            return false;
        }

        let Some(row_key) = delivery.id.row_key() else {
            return false;
        };
        let Some(index) = self.pane.index_of_str(row_key) else {
            return false;
        };
        let Some(row) = self.rows.get_mut(index) else {
            return false;
        };
        if !row.apply_thumbnail_delivery(delivery) {
            return false;
        }

        self.refresh_item_thumbnail(index);
        true
    }

    pub(crate) fn invalidate_ready_thumbnails(&mut self) -> bool {
        let changed = grid_rows::invalidate_ready_thumbnails(&mut self.rows);
        if changed {
            self.sync_grid_items();
            self.pane.clear_animation_clock();
        }
        changed
    }

    /// Drops Ready thumbnails outside the visible+prefetch window; the
    /// demand/cache path re-delivers. The window is kept even while the
    /// route is hidden so returning to it paints real pixels on the first
    /// frame instead of replaying every card's fade-in.
    pub(crate) fn release_offscreen_thumbnails(&mut self) -> bool {
        self.pane.release_offscreen_thumbnails(&mut self.rows)
    }

    pub(crate) fn has_active_animations(&self) -> bool {
        self.route_visible && self.pane.has_active_animations(&self.rows)
    }

    pub(crate) fn needs_card_motion_ticks(&self) -> bool {
        self.route_visible && self.pane.grid().needs_visible_card_ticks()
    }

    pub(crate) fn set_play_gifs_by_default(&mut self, enabled: bool) {
        if self.pane.play_gifs_by_default() == enabled {
            return;
        }

        self.pane.set_play_gifs_by_default(enabled);
        self.pane.clear_animation_clock();
        self.sync_grid_items();
    }

    /// GIF playback pauses on the current frame while the window is
    /// unfocused, so the clock subscription can drop to idle.
    pub(crate) fn set_window_focused(&mut self, focused: bool) {
        if self.pane.window_focused() == focused {
            return;
        }

        self.pane.set_window_focused(focused);
        self.pane.clear_animation_clock();
    }

    pub(crate) fn set_download_count_formatter(&mut self, formatter: DownloadCountFormatter) {
        if self.pane.formatter() == formatter {
            return;
        }

        self.pane.set_download_count_formatter(formatter);
        self.sync_grid_items();
    }

    pub(crate) fn set_publish_new_title(&mut self, title: String) {
        if self.publish_new_title == title {
            return;
        }

        self.publish_new_title = title;
        self.sync_grid_items();
    }

    pub(crate) fn has_active_count_rolls(&self) -> bool {
        self.route_visible && self.rows.iter().any(Row::has_active_count_roll)
    }

    pub(super) const fn grid_mut(&mut self) -> &mut addon_grid::State {
        self.pane.grid_mut()
    }

    pub(super) fn enter_route(&mut self) -> Option<(Generation, u32)> {
        self.route_visible = true;
        if matches!(self.page_status, PageStatus::Idle | PageStatus::Failed(_))
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
        self.pane.clear_animation_clock();
        self.last_roll_tick = None;
        self.pending_prepare_publish = None;
        self.pending_context_menu = None;
        // Items, scroll and viewport are untouched and `has_more_pages` is
        // forced false, so nothing here can change the visible range.
        let _follow_ups = self.pane.grid_mut().set_page_status(false, false);
    }

    pub(super) fn begin_next_page(&mut self) -> Option<(Generation, u32)> {
        if !self.route_visible || self.loading_page || self.complete {
            return None;
        }

        if self.next_page == FIRST_WORKSHOP_PAGE && self.rows.is_empty() {
            self.generation.bump();
            self.loaded_pages = 0;
            self.total_count = 0;
            self.complete = false;
            self.stats_in_flight = false;
            self.pane.clear_animation_clock();
            self.last_roll_tick = None;
            self.pending_prepare_publish = None;
            self.pending_context_menu = None;
            self.page_status = PageStatus::LoadingFirstPage;
            self.rows.clear();
            self.sync_grid_items();
        } else if !matches!(self.page_status, PageStatus::Failed(_)) {
            self.page_status = PageStatus::Loaded;
        }

        self.loading_page = true;
        // Only the spinner changes: `has_more_pages` false cannot widen the
        // range, and the item list is whatever it already was.
        let _follow_ups = self.pane.grid_mut().set_page_status(true, false);
        Some((self.generation, self.next_page))
    }

    pub(super) fn apply_page(
        &mut self,
        generation: Generation,
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
                // A page result for a request we no longer want. Nothing
                // actionable to tell the user, so the generic key.
                log::debug!("discarding stale My Workshop page result for page {page}");
                self.page_status = PageStatus::Failed(UiError::new(keys::UNKNOWN));
            }
            Err(error) => {
                self.page_status = PageStatus::Failed(error);
            }
        }
        self.sync_grid_items();
    }

    pub(super) fn request_stats_refresh(&mut self) -> Option<(Generation, u32)> {
        if !self.route_visible || self.stats_in_flight || self.loaded_pages == 0 {
            return None;
        }

        self.stats_in_flight = true;
        Some((self.generation, self.loaded_pages))
    }

    pub(super) fn apply_stats_counts(
        &mut self,
        generation: Generation,
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
            self.pane.grid_mut().tick_visible_card_motion(now);
        }
    }

    pub(super) fn tick_visible_animations(&mut self, now: Instant) -> bool {
        if !self.has_active_animations() {
            self.pane.clear_animation_clock();
            return false;
        }

        self.pane.tick_visible_animations(
            &mut self.rows,
            now,
            crate::media::thumbnail_animation::ANIMATION_TICK_INTERVAL,
        )
    }

    pub(super) fn take_prepare_publish_target(
        &mut self,
        id: &CardId,
    ) -> Option<PreparePublishTarget> {
        let target = match self.card(id)? {
            Card::PublishNew => Some(PreparePublishTarget::New),
            Card::Row(index) => self.rows.get(index).and_then(Row::prepare_publish_target),
        }?;
        self.pending_prepare_publish = Some(target.clone());
        Some(target)
    }

    pub(super) fn take_context_menu(
        &mut self,
        id: &CardId,
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

    pub(super) fn set_card_hovered(&mut self, id: &CardId, hovered: bool) -> bool {
        let Some(Card::Row(index)) = self.card(id) else {
            return false;
        };
        let Some(row) = self.rows.get_mut(index) else {
            return false;
        };
        // The play flag is recorded either way (a GIF delivered mid-hover
        // starts playing), but only a row that already has an animation
        // changes appearance — and then a thumbnail swap in place suffices.
        if !row.set_thumbnail_play_requested(hovered) || !row.holds_animation() {
            return false;
        }
        let thumbnail = row.card_thumbnail(self.pane.play_gifs_by_default());
        // Grid item 0 is the publish-new lead card; rows start at 1.
        let _ = self
            .pane
            .grid_mut()
            .update_item_thumbnail(index + 1, row.id(), thumbnail);
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
        self.page_status = PageStatus::Loaded;
    }

    /// Pushes one row's current thumbnail into the grid in place.
    ///
    /// A delivery names exactly one row, so it has no reason to touch the
    /// others; the sweeping variant below exists for changes that really do
    /// affect every row.
    fn refresh_item_thumbnail(&mut self, index: usize) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let thumbnail = row.card_thumbnail(self.pane.play_gifs_by_default());
        // Grid item 0 is the publish-new lead card; rows start at 1.
        let _ = self
            .pane
            .grid_mut()
            .update_item_thumbnail(index + 1, row.id(), thumbnail);
    }

    /// Rebuilds the grid's items and page status from `rows`.
    ///
    /// Visible-range follow-ups are dropped because every caller re-derives
    /// thumbnail demands and stats from state right after the sync, so an
    /// echoed range would ask for work already queued. Hover follow-ups go
    /// with them — a hover retargeted by rows shifting under a stationary
    /// cursor misses one play/pause transition and self-corrects on the next
    /// cursor move.
    fn sync_grid_items(&mut self) {
        // Every mutation of `rows` is already followed by a sync, so hanging
        // the reindex here keeps the map correct without four separate call
        // sites to keep in step.
        let lead = model::publish_new_item(&self.publish_new_title);
        let _follow_ups = self.pane.sync_items(&self.rows, [lead]);
        let has_more_pages = !self.complete && !matches!(self.page_status, PageStatus::Failed(_));
        let _follow_ups = self
            .pane
            .grid_mut()
            .set_page_status(self.loading_page, has_more_pages);
    }

    fn visible_row_range(&self) -> Range<usize> {
        self.pane.visible_row_range(self.rows.len())
    }

    #[cfg(test)]
    pub(crate) fn begin_for_test(&mut self) -> (Generation, u32) {
        self.route_visible = true;
        self.begin_next_page().expect("page request should start")
    }

    #[cfg(test)]
    pub(crate) fn row_for_test(&self, id: u64) -> Option<&Row> {
        self.rows
            .iter()
            .find(|row| row.workshop_id() == PublishedFileId::fixture(id))
    }

    #[cfg(test)]
    pub(crate) fn push_rows_for_test(&mut self, rows: Vec<Row>, total_count: u32) {
        self.route_visible = true;
        self.generation = Generation::INITIAL.next();
        self.rows = rows;
        self.total_count = total_count;
        self.loaded_pages = 1;
        self.next_page = 2;
        self.complete = self.rows.len() as u32 >= total_count;
        self.page_status = PageStatus::Loaded;
        self.sync_grid_items();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// How far the paged fetch has got.
///
/// Deliberately says nothing about whether any rows arrived — that is `rows`,
/// and deriving it there means the two cannot disagree. `Failed` is about the
/// most recent page request only: earlier pages stay on screen, which is why
/// it is not a variant that owns the rows.
pub enum PageStatus {
    /// No page has been requested.
    Idle,
    /// The first page is in flight and nothing is on screen yet.
    LoadingFirstPage,
    /// At least one page landed.
    Loaded,
    /// The most recent page request failed.
    Failed(UiError),
}

#[cfg(test)]
mod tests {
    use crate::bridge::domain::PublishedFileId;
    use crate::generation::Generation;
    use crate::widgets::addon_grid;

    use super::super::model::PageResult;
    use super::{PageStatus, Row, State};

    fn ready_delivery(
        row_id: u64,
        generation: Generation,
    ) -> crate::media::thumbnail_demand::Delivery {
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
            id: thumbnail_demand::DemandId::row(row_id.to_string()),
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

        // The hidden route keeps its visible window so re-entry paints real
        // pixels immediately instead of replaying every card's fade-in.
        state.exit_route();
        assert!(!state.release_offscreen_thumbnails());
        assert_eq!(ready_row_count(&state), retained);
    }

    #[test]
    fn route_entry_marks_page_visible_and_requests_first_page() {
        let mut state = State::default();

        let request = state.enter_route();

        assert!(state.is_route_visible());
        assert_eq!(request, Some((Generation::from_raw(1), 1)));
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
        assert!(matches!(state.page_status(), PageStatus::Loaded));
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
            Generation::from_raw(1),
            Ok([(PublishedFileId::fixture(42), 25)].into_iter().collect()),
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
        assert_eq!(stats_request, Some((Generation::from_raw(1), 1)));
    }
}
