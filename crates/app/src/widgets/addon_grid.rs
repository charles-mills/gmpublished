use std::{collections::HashMap, ops::Range, time::Instant};

#[cfg(test)]
use iced::Task;
use iced::widget::{Space, column, container, mouse_area, row, scrollable, sensor};
use iced::{Element, Length, Point, Size};

use crate::theme::{self, Tokens};
use crate::widgets::addon_card;

pub const DEFAULT_CARD_WIDTH: f32 = 200.0;
pub const MIN_CARD_WIDTH: f32 = 120.0;
const DEFAULT_CARD_GAP: f32 = 16.0;
/// Rows rendered beyond each edge of the viewport. A scroll event reaches the
/// visible-range reconcile one frame after Iced paints the new offset, so
/// edge rows would otherwise pop in a frame late; overscan keeps them already
/// painted. Render-only: `visible_rows` (demands, metadata windows) is not
/// widened.
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
    cursor: Option<Point>,
    hovered_id: Option<String>,
    loading: bool,
    has_more_pages: bool,
    next_page_requested: bool,
    card_heights: Vec<f32>,
    /// Measurement cache per title string. Titles are the only per-item
    /// input to card height, so re-layouts (metadata patches, thumbnail
    /// rebuilds, width changes) only shape titles whose answer is genuinely
    /// unknown; see [`TitleMeasure`] for what survives a width change.
    title_measures: HashMap<String, TitleMeasure>,
    card_width: f32,
    card_gap: f32,
    layout: RowLayout,
    #[cfg(test)]
    layout_cache_generation: u64,
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
            cursor: None,
            hovered_id: None,
            loading: false,
            has_more_pages: false,
            next_page_requested: false,
            card_heights: Vec::new(),
            title_measures: HashMap::new(),
            card_width: 0.0,
            card_gap: DEFAULT_CARD_GAP,
            layout: RowLayout::default(),
            #[cfg(test)]
            layout_cache_generation: 0,
        }
    }
}

impl State {
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

    /// Replaces items in place by id and re-reconciles the layout. The
    /// hydration path for metadata patches: a patch names a handful of rows,
    /// while `set_items` re-allocates every card in the grid. Motion/hover
    /// state carries over per item exactly as `set_items` preserves it.
    /// `index_hint` makes the common case O(1); like
    /// `update_item_thumbnail`, a stale hint falls back to an id scan, so
    /// the item is keyed by identity and a hint that drifts (e.g. a future
    /// lead card offsetting the grid) degrades to a slower lookup instead of
    /// a silently dropped patch. An id that is no longer in the grid at all
    /// is skipped — that item was removed.
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

        // Scroll anchoring: a patched title can re-wrap and change its row's
        // height. When that happens to a row above the viewport the content
        // would shift under the (pixel-anchored) scroll offset — visible as
        // the grid jumping mid-hydration. Item indices are stable across a
        // patch, so the first visible row is a valid anchor: shift the
        // offset by however far that row moved.
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

    /// Swaps just one item's thumbnail in place. Animation frame advances go
    /// through here instead of `set_items`: a thumbnail never changes card
    /// layout (the preview box is fixed), so no rebuild or re-layout runs.
    /// `index_hint` (the caller's row-to-item index) makes the common case
    /// O(1); an id mismatch falls back to a scan so a stale hint stays safe.
    pub(crate) fn update_item_thumbnail(
        &mut self,
        index_hint: usize,
        id: &str,
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
    fn visible_rows(&self) -> VisibleRowRange {
        self.visible_rows
    }

    pub(crate) fn items_len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn visible_item_range(&self) -> Range<usize> {
        self.visible_rows
            .flat_item_range(self.items.len(), self.columns)
    }

    pub(crate) fn needs_visible_card_ticks(&self) -> bool {
        self.items
            .get(self.visible_item_range())
            .unwrap_or_default()
            .iter()
            .any(|item| item.card().needs_motion_ticks())
    }

    pub(crate) fn tick_visible_card_motion(&mut self, now: Instant) {
        let visible = self.visible_item_range();
        if let Some(items) = self.items.get_mut(visible) {
            for item in items {
                item.tick_card_motion(now);
            }
        }
    }

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

    pub(crate) fn scroll_offset(&self) -> f32 {
        self.scroll_offset
    }

    #[cfg(test)]
    fn next_page_was_requested(&self) -> bool {
        self.next_page_requested
    }

    #[cfg(test)]
    fn layout_cache_generation(&self) -> u64 {
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

        self.columns = columns;
        self.recompute_layout_cache();
        let mut messages = vec![Message::ColumnsChanged(
            u32::try_from(self.columns).unwrap_or(u32::MAX),
        )];
        messages.extend(self.reconcile_layout_at(now));
        messages
    }

    fn set_scroll_offset_at(&mut self, offset: f32, now: Instant) -> Vec<Message> {
        self.scroll_offset = finite_nonnegative(offset);
        let mut messages = self.reconcile_layout_at(now);
        if self.viewport_height <= 0.0
            && self.scroll_offset > 0.0
            && !self.layout.rows.is_empty()
            && self.has_more_pages
            && !self.loading
            && !self.next_page_requested
        {
            self.next_page_requested = true;
            messages.push(Message::NextPageRequested);
        }
        messages
    }

    fn set_viewport_size_at(&mut self, size: Size, now: Instant) -> Vec<Message> {
        self.viewport_height = finite_nonnegative(size.height);
        self.content_width = finite_nonnegative(size.width);
        let tokens = Tokens::dark();
        let columns = columns_for_width(scrollable_content_width(self.content_width, &tokens));
        if self.columns != columns.max(1) {
            return self.set_columns_at(columns, now);
        }

        let card_width = self.resolved_card_width(&tokens);
        if self.card_width != card_width {
            self.recompute_layout_cache();
        }
        self.reconcile_layout_at(now)
    }

    fn reconcile_layout_at(&mut self, now: Instant) -> Vec<Message> {
        let visible_rows =
            visible_rows_for_viewport(&self.layout.rows, self.scroll_offset, self.viewport_height);
        let mut messages = Vec::new();
        if visible_rows == self.visible_rows {
            if let Some(message) = maybe_request_next_page(
                self.layout.rows.len(),
                visible_rows,
                self.has_more_pages,
                self.loading,
                &mut self.next_page_requested,
            ) {
                messages.push(message);
            }
            messages.extend(self.reconcile_hover(now));
            return messages;
        }

        self.visible_rows = visible_rows;
        if visible_rows != self.last_reported_visible_rows {
            self.last_reported_visible_rows = visible_rows;
            messages.push(Message::VisibleRangeChanged(
                visible_rows.start,
                visible_rows.end,
            ));
        }

        if let Some(message) = maybe_request_next_page(
            self.layout.rows.len(),
            visible_rows,
            self.has_more_pages,
            self.loading,
            &mut self.next_page_requested,
        ) {
            messages.push(message);
        }
        messages.extend(self.reconcile_hover(now));
        messages
    }

    fn recompute_layout_cache(&mut self) {
        let tokens = Tokens::dark();
        let card_width = self.resolved_card_width(&tokens);
        // Theme switches do not invalidate this cache: addon-card geometry
        // uses theme-invariant spacing, dimensions, and typography tokens.
        // Width changes do not clear it either — entries carry their own
        // width validity — so the map is bounded here instead, against a
        // long session churning through title sets.
        if !self.items.is_empty() && self.title_measures.len() > self.items.len() * 2 + 64 {
            let current = self
                .items
                .iter()
                .map(|item| item.card.display_title())
                .collect::<std::collections::HashSet<&str>>();
            self.title_measures
                .retain(|title, _| current.contains(title.as_str()));
        }
        let title_measures = &mut self.title_measures;
        self.card_heights = self
            .items
            .iter()
            .map(|item| item.preferred_height_cached(card_width, &tokens, title_measures))
            .collect();
        self.layout = RowLayout::for_items(&self.items, &self.card_heights, self.columns, &tokens);
        self.card_width = card_width;
        self.card_gap = tokens.spacing.gap;
        #[cfg(test)]
        {
            self.layout_cache_generation += 1;
        }
    }

    fn resolved_card_width(&self, tokens: &Tokens) -> f32 {
        card_width_for_columns(
            scrollable_content_width(self.content_width, tokens),
            self.columns,
            tokens,
        )
    }

    fn reconcile_hover(&mut self, now: Instant) -> Vec<Message> {
        let target_id = self.hover_target_id().map(str::to_owned);
        if target_id == self.hovered_id {
            return Vec::new();
        }

        let previous_id = self.hovered_id.take();
        let mut messages = Vec::new();
        if let Some(previous_id) = previous_id {
            if let Some(item) = self.items.iter_mut().find(|item| item.id() == previous_id) {
                item.set_hovered(false, now);
            }
            messages.push(Message::CardHoverChanged(previous_id, false));
        }

        if let Some(target_id) = target_id {
            if let Some(item) = self.items.iter_mut().find(|item| item.id() == target_id) {
                item.set_hovered(true, now);
            }
            messages.push(Message::CardHoverChanged(target_id.clone(), true));
            self.hovered_id = Some(target_id);
        }

        messages
    }

    fn hover_target_id(&self) -> Option<&str> {
        let cursor = self.cursor?;
        let columns = self.columns.max(1);
        let x = finite_nonnegative(cursor.x);
        let y = finite_nonnegative(cursor.y) + finite_nonnegative(self.scroll_offset);
        // Rows are contiguous and sorted; bisect — this runs per mouse move.
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
pub enum Message {
    ColumnsChanged(u32),
    Scrolled(u32),
    ViewportResized(u32, u32),
    VisibleRangeChanged(usize, usize),
    CardClicked(String),
    CardPressed(String),
    CardReleased(String),
    CardContextRequested(String, Point),
    CursorMoved(Point),
    CursorLeft,
    CardHoverChanged(String, bool),
    NextPageRequested,
    /// The grid moved its own scroll offset by this delta to keep the first
    /// visible row stationary after item heights changed above it. The owner
    /// must relay it to the runtime as a *relative* `scroll_by` — the
    /// widget's real offset lives in Iced's scrollable state, and this
    /// mirror is `u32`-quantized by the `Scrolled` echo, so an absolute
    /// `scroll_to` would inject up to 0.5px of drift per anchor.
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
        Message::NextPageRequested => {
            state.next_page_requested = true;
            Vec::new()
        }
        Message::ScrollAnchored(_) => Vec::new(),
    }
}

pub fn scrollable_id(key: &'static str) -> iced::widget::Id {
    iced::widget::Id::new(key)
}

/// `key` must be unique per route surface. My Workshop and Installed Addons
/// render structurally identical trees, so a direct switch between them
/// reuses this subtree's widget state in place: an unkeyed sensor would
/// never re-fire `on_show`, leaving the newly shown grid with a stale (or
/// never-observed) viewport.
pub fn view<'a>(state: &'a State, tokens: &Tokens, key: &'static str) -> Element<'a, Message> {
    let tokens = *tokens;
    let now = Instant::now();
    let layout = &state.layout;
    // A Sensor notification is an optimization prerequisite, not a
    // correctness prerequisite. If Iced misses the initial on_show during a
    // route-tree swap, render enough rows around the scroll offset to cover
    // any plausible viewport until a real measurement arrives. Clamping an
    // empty range to one row here would otherwise paint that row followed
    // by a full-height blank spacer — while rendering every row would build
    // a card widget per library entry on the pre-sensor frame.
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
) -> Element<'a, Message> {
    let mut cards = row![]
        .width(Length::Fill)
        .height(Length::Fixed(content_height.max(1.0)))
        .spacing(card_gap);

    for (item, item_height) in items.iter().zip(item_heights.iter().copied()) {
        cards = cards.push(card_view(
            item,
            card_width,
            content_height,
            item_height,
            tokens,
            now,
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
) -> Element<'a, Message> {
    addon_card::view(item.card(), width, cell_height, content_height, tokens, now)
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

    fn id(&self) -> &str {
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
    fn preferred_height(&self, width: f32, tokens: &Tokens) -> f32 {
        self.preferred_height_cached(width, tokens, &mut HashMap::new())
    }

    /// Like `addon_card::preferred_height`, but resolves the title
    /// measurement through the grid's cache before shaping.
    fn preferred_height_cached(
        &self,
        width: f32,
        tokens: &Tokens,
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
        addon_card::Message::Pressed(id) => Message::CardPressed(id.as_ref().to_owned()),
        addon_card::Message::Released(id) => Message::CardReleased(id.as_ref().to_owned()),
        addon_card::Message::ContextRequested(id, position) => {
            Message::CardContextRequested(id.as_ref().to_owned(), position)
        }
    }
}

/// What the grid knows about one title's measured geometry. Both fields
/// survive card-width changes: the memo carries the width it was shaped at,
/// and the watermark exploits monotonicity — wrapping only ever *removes*
/// line breaks as width grows, so a title that fit one line at some width
/// fits one line at every width above it. Live window resizes then re-shape
/// only titles that are genuinely multi-line at the new width, instead of
/// every title per resize event.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct TitleMeasure {
    /// Smallest card width observed to lay this title out as one line.
    single_line_at: Option<f32>,
    /// Exact measurement memo: (card width bits, clamped title height).
    measured: Option<(u32, f32)>,
}

fn cached_title_height(
    title: &str,
    width: f32,
    tokens: &Tokens,
    measures: &mut HashMap<String, TitleMeasure>,
) -> f32 {
    let single_line = addon_card::single_line_title_height(tokens);
    let is_single_line = |height: f32| height <= single_line + 0.5;

    if let Some(measure) = measures.get_mut(title) {
        if let Some((width_bits, height)) = measure.measured
            && width_bits == width.to_bits()
        {
            return height;
        }
        if measure.single_line_at.is_some_and(|known| width >= known) {
            return single_line;
        }
        let height = addon_card::measure_title_height(title, width, tokens);
        measure.measured = Some((width.to_bits(), height));
        if is_single_line(height) {
            measure.single_line_at = Some(
                measure
                    .single_line_at
                    .map_or(width, |known| known.min(width)),
            );
        }
        return height;
    }

    let height = addon_card::measure_title_height(title, width, tokens);
    let _ = measures.insert(
        title.to_owned(),
        TitleMeasure {
            single_line_at: is_single_line(height).then_some(width),
            measured: Some((width.to_bits(), height)),
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
    fn for_items(items: &[Item], item_heights: &[f32], columns: usize, tokens: &Tokens) -> Self {
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

/// Rows to paint while no viewport measurement exists: everything straddling
/// a window this tall — the tallest display we plausibly run on — starting
/// at the current scroll offset. Unmeasured usually also means unscrolled,
/// but a missed on_show does not reset an already-scrolled offset, so the
/// window follows it rather than assuming the top of the list.
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
    // Rows are contiguous and sorted by construction (each row's top is the
    // previous row's bottom), so both viewport edges bisect instead of
    // scanning — this runs on every scroll event.
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

pub fn columns_for_width(width: f32) -> usize {
    (((finite_nonnegative(width) + DEFAULT_CARD_GAP) / (DEFAULT_CARD_WIDTH + DEFAULT_CARD_GAP))
        .floor() as usize)
        .max(1)
}

fn card_width_for_columns(width: f32, columns: usize, tokens: &Tokens) -> f32 {
    let columns = columns.max(1);
    let content_width = finite_nonnegative(width);
    let gaps = tokens.spacing.gap * columns.saturating_sub(1) as f32;
    ((content_width - gaps) / columns as f32).max(MIN_CARD_WIDTH)
}

fn scrollable_content_width(width: f32, tokens: &Tokens) -> f32 {
    sub_clamped(
        finite_nonnegative(width),
        theme::styles::vertical_scrollbar_reserved_width(tokens),
    )
}

fn maybe_request_next_page(
    row_count: usize,
    visible_rows: VisibleRowRange,
    has_more_pages: bool,
    loading: bool,
    already_requested: &mut bool,
) -> Option<Message> {
    if row_count == 0 || !has_more_pages || loading || *already_requested {
        return None;
    }

    if visible_rows.end >= row_count.saturating_sub(1) {
        *already_requested = true;
        Some(Message::NextPageRequested)
    } else {
        None
    }
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
