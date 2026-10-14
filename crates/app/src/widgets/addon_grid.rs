use std::{collections::HashMap, ops::Range, time::Instant};

#[cfg(test)]
use iced::Task;
use iced::widget::{Space, column, container, mouse_area, row, scrollable, sensor};
use iced::{Element, Length, Point, Rectangle, Size, Vector};

mod reflow;

#[cfg(test)]
use crate::generation::Generation;
use crate::theme::{self, LayoutMetrics, Tokens};
use crate::widgets::addon_card;
use crate::widgets::grid_rows::CardId;

pub const MIN_CARD_WIDTH: f32 = 120.0;
/// Paint beyond the viewport because scroll reconciliation arrives a frame
/// after Iced draws. This does not widen thumbnail or metadata demand.
const RENDER_OVERSCAN_ROWS: usize = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    items: Vec<Item>,
    columns: usize,
    scroll_offset: f32,
    viewport_height: f32,
    content_width: f32,
    visible_rows: VisibleRowRange,
    last_reported_visible_rows: VisibleRowRange,
    last_reported_visible_items: Range<usize>,
    cursor: Option<Point>,
    hovered_id: Option<CardId>,
    loading: bool,
    has_more_pages: bool,
    next_page_requested: bool,
    card_heights: Vec<f32>,
    /// Cache height by title and width so thumbnail and metadata updates do not reshape
    /// unchanged text.
    title_measures: HashMap<String, TitleMeasure>,
    card_width: f32,
    card_gap: f32,
    layout: RowLayout,
    metrics: LayoutMetrics,
    reflow_offsets: HashMap<CardId, Vector>,
    reflow_motion: theme::motion::Presence<bool>,
    #[cfg(test)]
    layout_cache_generation: Generation,
}

impl Default for State {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            columns: 1,
            scroll_offset: 0.0,
            viewport_height: 0.0,
            content_width: 0.0,
            visible_rows: VisibleRowRange::empty(),
            last_reported_visible_rows: VisibleRowRange::empty(),
            last_reported_visible_items: 0..0,
            cursor: None,
            hovered_id: None,
            loading: false,
            has_more_pages: false,
            next_page_requested: false,
            card_heights: Vec::new(),
            title_measures: HashMap::new(),
            card_width: 0.0,
            card_gap: theme::invariant().spacing.gap,
            layout: RowLayout::default(),
            metrics: *theme::invariant(),
            reflow_offsets: HashMap::new(),
            reflow_motion: theme::motion::boolean(
                true,
                theme::invariant().motion.layout_duration(),
                iced::animation::Easing::EaseOut,
            ),
            #[cfg(test)]
            layout_cache_generation: Generation::INITIAL,
        }
    }
}

impl State {
    /// Rebuild measurement and pointer geometry together, retaining the item at the
    /// top of the viewport even when the number of columns changes.
    #[must_use = "metric follow-ups must be relayed to the route owner"]
    pub(crate) fn set_metrics(&mut self, metrics: &LayoutMetrics) -> Vec<Message> {
        if self.metrics == *metrics {
            return Vec::new();
        }
        let now = Instant::now();
        let before = self.visible_card_positions(now);
        let anchor = self.scroll_anchor();
        let previous_columns = self.columns;
        if self.metrics.typography != metrics.typography {
            self.title_measures.clear();
        }
        self.metrics = *metrics;
        if !self.metrics.motion.enabled() {
            self.finish_motion();
        }
        self.columns = columns_for_metrics(
            scrollable_content_width(self.content_width, &self.metrics),
            &self.metrics,
        );
        self.recompute_layout_cache();
        let mut messages = Vec::new();
        if previous_columns != self.columns {
            messages.push(Message::ColumnsChanged(
                u32::try_from(self.columns).unwrap_or(u32::MAX),
            ));
        }
        self.restore_scroll_anchor(anchor, &mut messages);
        if previous_columns != self.columns {
            self.begin_reflow(&before, now);
        }
        messages.extend(self.reconcile_layout_at(now));
        messages
    }

    fn render_item_range(&self) -> Range<usize> {
        self.visible_rows
            .overscanned(self.layout.rows.len())
            .flat_item_range(self.items.len(), self.columns)
    }

    fn card_position(&self, index: usize) -> Option<Point> {
        let row = self.layout.rows.get(index / self.columns.max(1))?;
        Some(Point::new(
            (index % self.columns.max(1)) as f32 * (self.card_width + self.card_gap),
            row.top - self.scroll_offset,
        ))
    }

    fn reflow_offset(&self, id: &CardId, now: Instant) -> Vector {
        self.reflow_offsets.get(id).copied().unwrap_or(Vector::ZERO)
            * (1.0 - self.reflow_motion.current(now))
    }

    fn reflow_opacity(&self, id: &CardId, now: Instant) -> f32 {
        if self
            .reflow_offsets
            .get(id)
            .is_some_and(|offset| offset.y.abs() > 0.5)
        {
            0.65 + 0.35 * self.reflow_motion.current(now)
        } else {
            1.0
        }
    }

    fn visible_card_positions(&self, now: Instant) -> HashMap<CardId, Point> {
        if !self.metrics.motion.enabled() || self.viewport_height <= 0.0 {
            return HashMap::new();
        }
        self.render_item_range()
            .filter_map(|index| {
                let item = self.items.get(index)?;
                Some((
                    item.id().clone(),
                    self.card_position(index)? + self.reflow_offset(item.id(), now),
                ))
            })
            .collect()
    }

    fn begin_reflow(&mut self, before: &HashMap<CardId, Point>, now: Instant) {
        if !self.metrics.motion.enabled() || before.is_empty() {
            self.reflow_offsets.clear();
            self.reflow_motion.snap(true);
            return;
        }
        self.reflow_offsets = self
            .render_item_range()
            .filter_map(|index| {
                let id = self.items.get(index)?.id();
                let target = self.card_position(index)?;
                let offset = before
                    .get(id)
                    .map_or(Vector::new(0.0, 6.0), |position| *position - target);
                // Cards crossing rows settle at their destination instead of sweeping
                // across unrelated cards and temporarily covering their labels.
                let offset = if offset.y.abs() > 8.0 || offset.x.abs() > self.card_width * 0.5 {
                    Vector::new(0.0, 6.0)
                } else {
                    offset
                };
                (offset.x.abs() > 0.5 || offset.y.abs() > 0.5).then(|| (id.clone(), offset))
            })
            .collect();
        self.reflow_motion.snap(self.reflow_offsets.is_empty());
        if !self.reflow_offsets.is_empty() {
            self.reflow_motion.go(true, now);
        }
    }

    fn finish_motion(&mut self) {
        self.reflow_offsets.clear();
        self.reflow_motion.snap(true);
        for item in &mut self.items {
            item.card.finish_motion();
        }
    }

    fn scroll_anchor(&self) -> (usize, f32, f32) {
        (
            self.visible_rows.start.saturating_mul(self.columns),
            self.scroll_offset - self.layout.top_offset(self.visible_rows.start),
            self.scroll_offset,
        )
    }

    fn restore_scroll_anchor(
        &mut self,
        (item, inset, previous_offset): (usize, f32, f32),
        messages: &mut Vec<Message>,
    ) {
        if previous_offset <= 0.0 || self.items.is_empty() {
            return;
        }
        let row = item.min(self.items.len() - 1) / self.columns;
        self.scroll_offset = finite_nonnegative(self.layout.top_offset(row) + inset.max(0.0))
            .min((self.layout.total_height() - self.viewport_height).max(0.0));
        let delta = self.scroll_offset - previous_offset;
        if delta.abs() >= 0.5 {
            messages.push(Message::ScrollAnchored(delta));
        }
    }

    #[must_use = "grid follow-ups must be relayed to the owner or explicitly bound and justified"]
    pub(crate) fn set_items(&mut self, items: Vec<Item>) -> Vec<Message> {
        self.set_items_at(items, Instant::now())
    }

    fn set_items_at(&mut self, mut items: Vec<Item>, now: Instant) -> Vec<Message> {
        preserve_hovered_items(&self.items, &mut items);
        self.items = items;
        self.next_page_requested = false;
        self.recompute_layout_cache();
        self.reconcile_layout_at(now)
    }

    /// Patches items by id while preserving motion and hover state.
    /// Uses `index_hint` when valid, otherwise scans by id; removed ids are skipped.
    #[must_use = "grid follow-ups must be relayed to the owner or explicitly bound and justified"]
    pub(crate) fn patch_items(&mut self, updates: Vec<(usize, Item)>) -> Vec<Message> {
        self.patch_items_at(updates, Instant::now())
    }

    fn patch_items_at(&mut self, updates: Vec<(usize, Item)>, now: Instant) -> Vec<Message> {
        let mut changed = false;
        for (index_hint, mut item) in updates {
            let slot = match self.items.get_mut(index_hint) {
                Some(slot) if slot.id() == item.id() => Some(slot),
                _ => self.items.iter_mut().find(|slot| slot.id() == item.id()),
            };
            let Some(slot) = slot else {
                continue;
            };
            item.preserve_card_motion(slot.card());
            *slot = item;
            changed = true;
        }
        if !changed {
            return Vec::new();
        }

        // Keep the first visible row stationary when patched titles change heights above it.
        let anchor_row = self.visible_rows.start;
        let anchor_top = self.layout.top_offset(anchor_row);
        self.recompute_layout_cache();
        let anchor_delta = self.layout.top_offset(anchor_row) - anchor_top;

        let mut messages = Vec::new();
        if anchor_delta.abs() >= 0.5 && self.scroll_offset > 0.0 {
            self.scroll_offset = finite_nonnegative(self.scroll_offset + anchor_delta);
            messages.push(Message::ScrollAnchored(anchor_delta));
        }
        messages.extend(self.reconcile_layout_at(now));
        messages
    }

    pub(crate) fn update_item_thumbnail(
        &mut self,
        index_hint: usize,
        id: &CardId,
        thumbnail: addon_card::Thumbnail,
    ) -> bool {
        let item = match self.items.get_mut(index_hint) {
            Some(item) if item.id() == id => Some(item),
            _ => self.items.iter_mut().find(|item| item.id() == id),
        };
        let Some(item) = item else {
            return false;
        };
        item.card.set_thumbnail(thumbnail);
        true
    }

    #[cfg(test)]
    pub(crate) fn metrics_for_test(&self) -> LayoutMetrics {
        self.metrics
    }

    #[cfg(test)]
    fn visible_rows(&self) -> VisibleRowRange {
        self.visible_rows
    }

    pub(crate) fn available_width(&self) -> f32 {
        self.content_width
    }

    pub(crate) fn items_len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn visible_item_range(&self) -> Range<usize> {
        self.visible_rows
            .flat_item_range(self.items.len(), self.columns)
    }

    pub(crate) fn needs_visible_card_ticks(&self) -> bool {
        self.metrics.motion.enabled()
            && (self.reflow_motion.needs_ticks()
                || self
                    .items
                    .get(self.visible_item_range())
                    .unwrap_or_default()
                    .iter()
                    .any(|item| item.card().needs_motion_ticks()))
    }

    pub(crate) fn tick_visible_card_motion(&mut self, now: Instant) -> Vec<Message> {
        if self.reflow_motion.tick(now) {
            self.reflow_offsets.clear();
        }
        let visible = self.visible_item_range();
        if let Some(items) = self.items.get_mut(visible) {
            for item in items {
                item.tick_card_motion(now);
            }
        }
        self.reconcile_hover(now)
    }

    #[must_use = "grid follow-ups must be relayed to the owner or explicitly bound and justified"]
    pub(crate) fn set_page_status(&mut self, loading: bool, has_more_pages: bool) -> Vec<Message> {
        self.set_page_status_at(loading, has_more_pages, Instant::now())
    }

    fn set_page_status_at(
        &mut self,
        loading: bool,
        has_more_pages: bool,
        now: Instant,
    ) -> Vec<Message> {
        self.loading = loading;
        self.has_more_pages = has_more_pages;
        if !loading {
            self.next_page_requested = false;
        }
        self.reconcile_layout_at(now)
    }

    /// Only claim this when the caller can fetch a page. The request stays
    /// latched until [`Self::set_page_status`] clears it.
    #[must_use]
    pub(crate) fn take_next_page_request(&mut self) -> bool {
        let row_count = self.layout.rows.len();
        if row_count == 0 || !self.has_more_pages || self.loading || self.next_page_requested {
            return false;
        }

        // With no measured viewport the visible range is empty and can never
        // reach the last row, so any scroll away from the top stands in for it.
        let reached_last_row = self.visible_rows.end >= row_count.saturating_sub(1);
        let scrolled_blind = self.viewport_height <= 0.0 && self.scroll_offset > 0.0;
        if reached_last_row || scrolled_blind {
            self.next_page_requested = true;
            return true;
        }
        false
    }

    pub(crate) fn scroll_offset(&self) -> f32 {
        self.scroll_offset
    }

    #[cfg(test)]
    fn next_page_was_requested(&self) -> bool {
        self.next_page_requested
    }

    #[cfg(test)]
    fn layout_cache_generation(&self) -> Generation {
        self.layout_cache_generation
    }

    #[cfg(test)]
    pub(crate) fn item_title_for_test(&self, index: usize) -> Option<&str> {
        self.items
            .get(index)
            .map(|item| item.card().display_title())
    }

    fn set_columns_at(&mut self, columns: usize, now: Instant) -> Vec<Message> {
        let columns = columns.max(1);
        if self.columns == columns {
            return self.reconcile_layout_at(now);
        }

        let anchor = self.scroll_anchor();
        let before = self.visible_card_positions(now);
        self.columns = columns;
        self.recompute_layout_cache();
        let mut messages = vec![Message::ColumnsChanged(
            u32::try_from(self.columns).unwrap_or(u32::MAX),
        )];
        self.restore_scroll_anchor(anchor, &mut messages);
        self.begin_reflow(&before, now);
        messages.extend(self.reconcile_layout_at(now));
        messages
    }

    fn set_scroll_offset_at(&mut self, offset: f32, now: Instant) -> Vec<Message> {
        if (offset - self.scroll_offset).abs() >= 1.0 {
            self.reflow_offsets.clear();
            self.reflow_motion.snap(true);
        }
        self.scroll_offset = finite_nonnegative(offset);
        self.reconcile_layout_at(now)
    }

    fn set_viewport_size_at(&mut self, size: Size, now: Instant) -> Vec<Message> {
        self.viewport_height = finite_nonnegative(size.height);
        self.content_width = finite_nonnegative(size.width);
        let tokens = &self.metrics;
        let columns =
            columns_for_metrics(scrollable_content_width(self.content_width, tokens), tokens);
        if self.columns != columns.max(1) {
            return self.set_columns_at(columns, now);
        }

        let card_width = self.resolved_card_width(tokens);
        let mut messages = Vec::new();
        if self.card_width != card_width {
            let anchor = self.scroll_anchor();
            self.recompute_layout_cache();
            self.restore_scroll_anchor(anchor, &mut messages);
        }
        messages.extend(self.reconcile_layout_at(now));
        messages
    }

    fn reconcile_layout_at(&mut self, now: Instant) -> Vec<Message> {
        let visible_rows =
            visible_rows_for_viewport(&self.layout.rows, self.scroll_offset, self.viewport_height);
        let visible_items = visible_rows.flat_item_range(self.items.len(), self.columns);
        let mut messages = Vec::new();
        if visible_rows == self.visible_rows && visible_items == self.last_reported_visible_items {
            messages.extend(self.reconcile_hover(now));
            return messages;
        }

        self.visible_rows = visible_rows;
        if visible_rows != self.last_reported_visible_rows
            || visible_items != self.last_reported_visible_items
        {
            self.last_reported_visible_rows = visible_rows;
            self.last_reported_visible_items = visible_items;
            messages.push(Message::VisibleRangeChanged(
                visible_rows.start,
                visible_rows.end,
            ));
        }

        messages.extend(self.reconcile_hover(now));
        messages
    }

    fn recompute_layout_cache(&mut self) {
        let tokens = &self.metrics;
        let card_width = self.resolved_card_width(tokens);
        if self.items.is_empty() {
            self.title_measures.clear();
        } else if self.title_measures.len() > self.items.len() * 2 + 64 {
            let current = self
                .items
                .iter()
                .map(|item| item.card.display_title())
                .collect::<std::collections::HashSet<&str>>();
            self.title_measures
                .retain(|title, _| current.contains(title.as_str()));
        }
        let exact_items = if self.metrics.motion.layout_animating {
            let columns = self.columns.max(1);
            let start = self
                .layout
                .rows
                .get(self.visible_rows.start)
                .map_or(0, |row| row.items.start)
                .saturating_sub(columns * 3);
            let end = self
                .layout
                .rows
                .get(self.visible_rows.end.saturating_sub(1))
                .map_or(columns * 3, |row| row.items.end)
                .saturating_add(columns * 3);
            start..end
        } else {
            0..self.items.len()
        };
        let title_measures = &mut self.title_measures;
        self.card_heights = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                if !exact_items.contains(&index)
                    && let Some((_, height)) = title_measures
                        .get(item.card.display_title())
                        .and_then(|measure| measure.measured)
                {
                    return addon_card::preferred_height_for_title_height(
                        card_width, height, tokens,
                    );
                }
                item.preferred_height_cached(card_width, tokens, title_measures)
            })
            .collect();
        self.layout = RowLayout::for_items(&self.items, &self.card_heights, self.columns, tokens);
        self.card_width = card_width;
        self.card_gap = tokens.spacing.gap;
        #[cfg(test)]
        {
            self.layout_cache_generation.bump();
        }
    }

    fn resolved_card_width(&self, tokens: &LayoutMetrics) -> f32 {
        card_width_for_columns(
            scrollable_content_width(self.content_width, tokens),
            self.columns,
            tokens,
        )
    }

    fn reconcile_hover(&mut self, now: Instant) -> Vec<Message> {
        let target_id = self.hover_target_id_at(now).cloned();
        if target_id == self.hovered_id {
            return Vec::new();
        }

        let previous_id = self.hovered_id.take();
        let mut messages = Vec::new();
        if let Some(previous_id) = previous_id {
            if let Some(item) = self.items.iter_mut().find(|item| *item.id() == previous_id) {
                item.set_hovered(false, now);
            }
            messages.push(Message::CardHoverChanged(previous_id, false));
        }

        if let Some(target_id) = target_id {
            if let Some(item) = self.items.iter_mut().find(|item| *item.id() == target_id) {
                item.set_hovered(true, now);
            }
            messages.push(Message::CardHoverChanged(target_id.clone(), true));
            self.hovered_id = Some(target_id);
        }

        messages
    }

    fn hover_target_id_at(&self, now: Instant) -> Option<&CardId> {
        let cursor = self.cursor?;
        if self.reflow_motion.needs_ticks() {
            return self.render_item_range().rev().find_map(|index| {
                let item = self.items.get(index)?;
                let position = self.card_position(index)? + self.reflow_offset(item.id(), now);
                let bounds = Rectangle::new(
                    position,
                    Size::new(self.card_width, self.card_heights[index]),
                );
                (item.card().is_enabled() && bounds.contains(cursor)).then(|| item.id())
            });
        }
        let columns = self.columns.max(1);
        let x = finite_nonnegative(cursor.x);
        let y = finite_nonnegative(cursor.y) + finite_nonnegative(self.scroll_offset);
        // Rows are sorted; bisect instead of scanning on every mouse move.
        let row_index = self.layout.rows.partition_point(|row| row.bottom() <= y);
        let row = self.layout.rows.get(row_index)?;
        if row.top() > y {
            return None;
        }
        let pitch = self.card_width + self.card_gap;
        if !pitch.is_finite() || pitch <= 0.0 {
            return None;
        }

        let col = (x / pitch).floor();
        if !col.is_finite() || col < 0.0 {
            return None;
        }
        let col = col as usize;
        if col >= columns || x >= col as f32 * pitch + self.card_width {
            return None;
        }

        let index = row.items.start.checked_add(col)?;
        if index >= row.items.end {
            return None;
        }
        if y - row.top() >= *self.card_heights.get(index)? {
            return None;
        }

        let item = self.items.get(index)?;
        item.card().is_enabled().then(|| item.id())
    }
}

fn preserve_hovered_items(previous: &[Item], next: &mut [Item]) {
    if previous.is_empty() {
        return;
    }

    let previous = previous
        .iter()
        .map(|item| (item.id(), item.card()))
        .collect::<HashMap<_, _>>();

    for item in next {
        if let Some(previous) = previous.get(item.id()) {
            item.preserve_card_motion(previous);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
/// Follow-up events for the owner to apply. Visible ranges drive demands and
/// scroll anchors require a relative runtime scroll. Discard only when the
/// grid is empty or the caller recomputes the affected state.
pub enum Message {
    ColumnsChanged(u32),
    Scrolled(u32),
    ViewportResized(u32, u32),
    VisibleRangeChanged(usize, usize),
    CardClicked(CardId),
    CardPressed(CardId),
    CardReleased(CardId),
    CardContextRequested(CardId, Point),
    CursorMoved(Point),
    CursorLeft,
    CardHoverChanged(CardId, bool),
    /// Relay as relative `scroll_by`. The mirrored offset is rounded by
    /// `Scrolled`; an absolute scroll would accumulate drift.
    ScrollAnchored(f32),
}

#[cfg(test)]
pub fn update(state: &mut State, message: Message) -> Task<Message> {
    let messages = apply(state, message);
    Task::batch(messages.into_iter().map(Task::done))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "public dispatch entry mirrors the crate-wide apply/update(state, Message) boundary, receiving the variant moved out of the parent enum by its callers"
)]
pub fn apply(state: &mut State, message: Message) -> Vec<Message> {
    apply_at(state, &message, Instant::now())
}

fn apply_at(state: &mut State, message: &Message, now: Instant) -> Vec<Message> {
    match *message {
        Message::ColumnsChanged(columns) => {
            state.set_columns_at(usize::try_from(columns).unwrap_or(usize::MAX), now)
        }
        Message::Scrolled(offset) => state.set_scroll_offset_at(offset as f32, now),
        Message::ViewportResized(width, height) => {
            state.set_viewport_size_at(Size::new(width as f32, height as f32), now)
        }
        Message::CursorMoved(position) => {
            state.cursor = Some(Point::new(
                finite_nonnegative(position.x),
                finite_nonnegative(position.y),
            ));
            state.reconcile_hover(now)
        }
        Message::CursorLeft => {
            state.cursor = None;
            state.reconcile_hover(now)
        }
        Message::VisibleRangeChanged(_, _) => Vec::new(),
        Message::CardClicked(_) | Message::CardPressed(_) | Message::CardReleased(_) => Vec::new(),
        Message::CardContextRequested(_, _) => Vec::new(),
        Message::CardHoverChanged(_, _) => Vec::new(),
        Message::ScrollAnchored(_) => Vec::new(),
    }
}

pub fn scrollable_id(key: &'static str) -> iced::widget::Id {
    iced::widget::Id::new(key)
}

/// Use a unique key per route so Iced fires `on_show` when switching
/// between otherwise identical grid trees.
pub fn view<'a>(state: &'a State, tokens: &Tokens, key: &'static str) -> Element<'a, Message> {
    view_at(state, tokens, key, Instant::now())
}

fn view_at<'a>(
    state: &'a State,
    tokens: &Tokens,
    key: &'static str,
    now: Instant,
) -> Element<'a, Message> {
    let tokens = *tokens;
    let layout = &state.layout;
    // Iced can miss `on_show` when switching routes. Until it measures the
    // viewport, draw a window around the saved offset rather than one row
    // or the entire library.
    let visible = if state.viewport_height > 0.0 {
        state
            .visible_rows
            .clamped(layout.rows.len())
            .overscanned(layout.rows.len())
    } else {
        fallback_rows(&layout.rows, state.scroll_offset)
    };
    let top_spacer = layout.top_offset(visible.start);
    let bottom_spacer = sub_clamped(layout.total_height(), layout.top_offset(visible.end));

    let mut list = column![
        Space::new()
            .height(Length::Fixed(top_spacer))
            .width(Length::Fill)
    ]
    .width(Length::Fill)
    .spacing(0.0);

    for row_model in layout.rows[visible.start..visible.end].iter() {
        list = list.push(row_view(
            &state.items[row_model.items.clone()],
            &state.card_heights[row_model.items.clone()],
            row_model.content_height,
            row_model.height,
            state.card_width,
            state.card_gap,
            &tokens,
            now,
            state,
        ));
    }

    list = list.push(
        Space::new()
            .height(Length::Fixed(bottom_spacer))
            .width(Length::Fill),
    );

    let content = scrollable(list)
        .id(scrollable_id(key))
        .width(Length::Fill)
        .height(Length::Fill)
        .direction(scrollable::Direction::Vertical(
            theme::styles::vertical_scrollbar(&tokens),
        ))
        .style(move |_, status| theme::styles::scrollbar(&tokens, status))
        .on_scroll(|viewport| Message::Scrolled(float_to_u32(viewport.absolute_offset().y)));

    mouse_area(
        sensor(content)
            .key(key)
            .on_show(|size| {
                Message::ViewportResized(float_to_u32(size.width), float_to_u32(size.height))
            })
            .on_resize(|size| {
                Message::ViewportResized(float_to_u32(size.width), float_to_u32(size.height))
            }),
    )
    .on_move(Message::CursorMoved)
    .on_exit(Message::CursorLeft)
    .into()
}

#[expect(
    clippy::too_many_arguments,
    reason = "hot view helper receives already-computed row geometry without allocating a wrapper"
)]
fn row_view<'a>(
    items: &'a [Item],
    item_heights: &[f32],
    content_height: f32,
    row_height: f32,
    card_width: f32,
    card_gap: f32,
    tokens: &Tokens,
    now: Instant,
    state: &State,
) -> Element<'a, Message> {
    let mut cards = row![]
        .width(Length::Fill)
        .height(Length::Fixed(content_height.max(1.0)))
        .spacing(card_gap);

    for (item, item_height) in items.iter().zip(item_heights.iter().copied()) {
        cards = cards.push(reflow::translated(
            card_view(
                item,
                card_width,
                content_height,
                item_height,
                tokens,
                now,
                state.reflow_opacity(item.id(), now),
            ),
            state.reflow_offset(item.id(), now),
        ));
    }

    container(cards)
        .width(Length::Fill)
        .height(Length::Fixed(row_height.max(1.0)))
        .align_y(iced::alignment::Vertical::Top)
        .into()
}

fn card_view<'a>(
    item: &'a Item,
    width: f32,
    cell_height: f32,
    content_height: f32,
    tokens: &Tokens,
    now: Instant,
    opacity: f32,
) -> Element<'a, Message> {
    addon_card::view(
        item.card(),
        width,
        cell_height,
        content_height,
        tokens,
        now,
        opacity,
    )
    .map(map_card_message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    card: addon_card::Data,
    #[cfg(test)]
    preferred_height_override: Option<f32>,
}

impl Item {
    pub(crate) const fn new(card: addon_card::Data) -> Self {
        Self {
            card,
            #[cfg(test)]
            preferred_height_override: None,
        }
    }

    pub(crate) fn card(&self) -> &addon_card::Data {
        &self.card
    }

    fn id(&self) -> &CardId {
        self.card.id()
    }

    fn set_hovered(&mut self, hovered: bool, now: Instant) {
        self.card.set_hovered_at(hovered, now);
    }

    fn preserve_card_motion(&mut self, previous: &addon_card::Data) {
        self.card.preserve_motion_from(previous);
    }

    fn tick_card_motion(&mut self, now: Instant) {
        self.card.tick_motion(now);
    }

    #[cfg(test)]
    fn preferred_height(&self, width: f32, tokens: &LayoutMetrics) -> f32 {
        self.preferred_height_cached(width, tokens, &mut HashMap::new())
    }

    fn preferred_height_cached(
        &self,
        width: f32,
        tokens: &LayoutMetrics,
        measures: &mut HashMap<String, TitleMeasure>,
    ) -> f32 {
        #[cfg(test)]
        if let Some(height) = self.preferred_height_override {
            return height;
        }

        let title_height = cached_title_height(self.card.display_title(), width, tokens, measures);
        addon_card::preferred_height_for_title_height(width, title_height, tokens)
    }

    #[cfg(test)]
    fn with_preferred_height(mut self, height: f32) -> Self {
        self.preferred_height_override = Some(height);
        self
    }
}

fn map_card_message(message: addon_card::Message) -> Message {
    match message {
        addon_card::Message::Pressed(id) => Message::CardPressed(id),
        addon_card::Message::Released(id) => Message::CardReleased(id),
        addon_card::Message::ContextRequested(id, position) => {
            Message::CardContextRequested(id, position)
        }
    }
}

/// A title that fits one line still fits at any wider width. Keep that
/// threshold across resizes to avoid shaping it again.
#[derive(Clone, Debug, Default, PartialEq)]
struct TitleMeasure {
    paragraph: Option<iced::advanced::graphics::text::Paragraph>,
    single_line_at: Option<f32>,
    measured: Option<(u32, f32)>,
}

fn cached_title_height(
    title: &str,
    width: f32,
    tokens: &LayoutMetrics,
    measures: &mut HashMap<String, TitleMeasure>,
) -> f32 {
    let content_width = (width - tokens.dims.card_padding * 2.0).max(1.0);
    let single_line = addon_card::single_line_title_height(tokens);
    let is_single_line = |height: f32| height <= single_line + 0.5;

    if let Some(measure) = measures.get_mut(title) {
        if let Some((width_bits, height)) = measure.measured
            && width_bits == content_width.to_bits()
        {
            return height;
        }
        if measure
            .single_line_at
            .is_some_and(|known| content_width >= known)
        {
            return single_line;
        }
        let (height, single_line_at) =
            addon_card::measure_title_cached(title, width, tokens, &mut measure.paragraph);
        measure.measured = Some((content_width.to_bits(), height));
        if is_single_line(height) {
            measure.single_line_at = Some(measure.single_line_at.map_or_else(
                || single_line_at.unwrap_or(content_width),
                |known| known.min(single_line_at.unwrap_or(content_width)),
            ));
        }
        return height;
    }

    let mut paragraph = None;
    let (height, single_line_at) =
        addon_card::measure_title_cached(title, width, tokens, &mut paragraph);
    let _ = measures.insert(
        title.to_owned(),
        TitleMeasure {
            paragraph,
            single_line_at,
            measured: Some((content_width.to_bits(), height)),
        },
    );
    height
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct VisibleRowRange {
    start: usize,
    end: usize,
}

impl VisibleRowRange {
    pub(crate) const fn empty() -> Self {
        Self { start: 0, end: 0 }
    }

    fn clamped(self, row_count: usize) -> Self {
        if row_count == 0 {
            return Self::empty();
        }

        let start = self.start.min(row_count - 1);
        let end = self.end.max(start + 1).min(row_count);
        Self { start, end }
    }

    fn overscanned(self, row_count: usize) -> Self {
        if row_count == 0 {
            return Self::empty();
        }

        Self {
            start: self.start.saturating_sub(RENDER_OVERSCAN_ROWS),
            end: self.end.saturating_add(RENDER_OVERSCAN_ROWS).min(row_count),
        }
    }

    fn flat_item_range(self, item_count: usize, columns: usize) -> Range<usize> {
        if item_count == 0 {
            return 0..0;
        }

        let columns = columns.max(1);
        let start = self.start.saturating_mul(columns).min(item_count);
        let end = self.end.saturating_mul(columns).min(item_count);
        start..end.max(start)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct RowLayout {
    rows: Vec<RowModel>,
}

impl RowLayout {
    fn for_items(
        items: &[Item],
        item_heights: &[f32],
        columns: usize,
        tokens: &LayoutMetrics,
    ) -> Self {
        debug_assert_eq!(items.len(), item_heights.len());
        let columns = columns.max(1);
        let rows = item_heights
            .chunks(columns)
            .enumerate()
            .scan(0.0_f32, |top, (row_index, chunk)| {
                let start = (*top).max(0.0);
                let content_height = chunk.iter().copied().fold(0.0_f32, f32::max);
                let height = (content_height + tokens.dims.card_row_gap).max(1.0);
                *top += height;
                Some(RowModel {
                    top: start,
                    content_height,
                    height,
                    items: row_index * columns..row_index * columns + chunk.len(),
                })
            })
            .collect();
        Self { rows }
    }

    #[cfg(test)]
    fn for_item_heights(heights: &[f32], columns: usize) -> Self {
        let columns = columns.max(1);
        let tokens = theme::invariant();
        let rows = heights
            .chunks(columns)
            .enumerate()
            .scan(0.0_f32, |top, (row_index, chunk)| {
                let start = (*top).max(0.0);
                let content_height = chunk.iter().copied().fold(0.0_f32, f32::max);
                let height = (content_height + tokens.dims.card_row_gap).max(1.0);
                *top += height;
                Some(RowModel {
                    top: start,
                    content_height,
                    height,
                    items: row_index * columns..row_index * columns + chunk.len(),
                })
            })
            .collect();
        Self { rows }
    }

    #[cfg(test)]
    fn rows(&self) -> &[RowModel] {
        &self.rows
    }

    fn total_height(&self) -> f32 {
        self.rows.last().map_or(0.0, RowModel::bottom)
    }

    fn top_offset(&self, row_index: usize) -> f32 {
        if row_index >= self.rows.len() {
            return self.total_height();
        }

        self.rows[row_index].top
    }
}

#[derive(Clone, Debug, PartialEq)]
struct RowModel {
    top: f32,
    content_height: f32,
    height: f32,
    items: Range<usize>,
}

impl RowModel {
    fn top(&self) -> f32 {
        self.top
    }

    #[cfg(test)]
    fn height(&self) -> f32 {
        self.height
    }

    #[cfg(test)]
    fn content_height(&self) -> f32 {
        self.content_height
    }

    fn bottom(&self) -> f32 {
        self.top + self.height
    }

    #[cfg(test)]
    fn items(&self) -> Range<usize> {
        self.items.clone()
    }
}

const FALLBACK_RENDER_HEIGHT: f32 = 4096.0;

fn fallback_rows(rows: &[RowModel], scroll_offset: f32) -> VisibleRowRange {
    let top = finite_nonnegative(scroll_offset);
    let bottom = top + FALLBACK_RENDER_HEIGHT;
    let start = rows
        .iter()
        .position(|row| row.bottom() > top)
        .unwrap_or(rows.len());
    let end = rows
        .iter()
        .position(|row| row.top() >= bottom)
        .unwrap_or(rows.len());
    VisibleRowRange {
        start,
        end: end.max(start),
    }
}

fn visible_rows_for_viewport(
    rows: &[RowModel],
    scroll_offset: f32,
    viewport_height: f32,
) -> VisibleRowRange {
    if rows.is_empty() || viewport_height <= 0.0 {
        return VisibleRowRange::empty();
    }

    let top = finite_nonnegative(scroll_offset);
    let bottom = top + finite_nonnegative(viewport_height);
    // Rows are sorted and contiguous; bisect both viewport edges on every scroll.
    let start = rows.partition_point(|row| row.bottom() <= top);
    if start == rows.len() {
        // Scrolled past the end (transient while content shrinks, before
        // Iced clamps the offset): pin the last row.
        return VisibleRowRange {
            start: rows.len() - 1,
            end: rows.len(),
        };
    }

    let end = rows
        .partition_point(|row| row.top() < bottom)
        .max(start + 1);
    VisibleRowRange { start, end }
}

#[cfg(test)]
pub fn columns_for_width(width: f32) -> usize {
    columns_for_metrics(width, theme::invariant())
}

fn columns_for_metrics(width: f32, tokens: &LayoutMetrics) -> usize {
    let gap = tokens.spacing.gap;
    let preferred = tokens.preferred_card_width.max(MIN_CARD_WIDTH);
    (((finite_nonnegative(width) + gap) / (preferred + gap)).floor() as usize).max(1)
}

fn card_width_for_columns(width: f32, columns: usize, tokens: &LayoutMetrics) -> f32 {
    let columns = columns.max(1);
    let content_width = finite_nonnegative(width);
    let gaps = tokens.spacing.gap * columns.saturating_sub(1) as f32;
    // A single card in a narrow pane may shrink below the preferred width.
    // Wide sparse collections keep the same bounded geometry as full ones.
    ((content_width - gaps) / columns as f32).clamp(1.0, tokens.preferred_card_width * 1.35)
}

fn scrollable_content_width(width: f32, tokens: &LayoutMetrics) -> f32 {
    sub_clamped(
        finite_nonnegative(width),
        tokens.dims.scrollbar_thumb_width + tokens.dims.scrollbar_track_inset,
    )
}

fn finite_nonnegative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn float_to_u32(value: f32) -> u32 {
    finite_nonnegative(value).round().min(u32::MAX as f32) as u32
}

fn sub_clamped(a: f32, b: f32) -> f32 {
    (a - b).max(0.0)
}

#[cfg(test)]
mod tests;
