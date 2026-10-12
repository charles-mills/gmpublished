use std::{collections::HashMap, fmt, ops::Range, sync::Arc, time::Duration, time::Instant};

use crate::format::DownloadCountFormatter;
use crate::widgets::addon_card;

use crate::generation::Generation;
use crate::media::thumbnail_demand;
use crate::widgets::addon_grid;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CardId(Arc<str>);

impl CardId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for CardId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<String> for CardId {
    fn from(id: String) -> Self {
        Self(Arc::from(id))
    }
}

impl From<&str> for CardId {
    fn from(id: &str) -> Self {
        Self(Arc::from(id))
    }
}

impl fmt::Display for CardId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub const ADDON_THUMBNAIL_MAX_EDGE: u32 = 256;

pub const THUMBNAIL_PLAY_POLICY: crate::media::thumbnail_animation::PlayPolicy =
    crate::media::thumbnail_animation::PlayPolicy::OnHover;

#[derive(Clone, Debug, PartialEq)]
pub enum RowThumbnail {
    Loading,
    Placeholder(iced::widget::image::Handle),
    Dead,
    Ready {
        still: iced::widget::image::Handle,
        animation: Option<crate::media::thumbnail_animation::Playback>,
    },
}

pub trait GridRow {
    fn id(&self) -> &CardId;

    fn to_grid_item(
        &self,
        play_gifs_by_default: bool,
        formatter: DownloadCountFormatter,
    ) -> addon_grid::Item;

    fn card_thumbnail(&self, play_gifs_by_default: bool) -> addon_card::Thumbnail;

    /// Whether this row would animate *right now* under the user's default.
    /// Distinct from merely holding an animation: the play policy (hover,
    /// window focus) decides, and a pane that ignored it would tick frames
    /// for cards that are not playing.
    fn is_animating(&self, play_gifs_by_default: bool) -> bool;

    fn advance_animation(&mut self, elapsed: Duration, play_gifs_by_default: bool) -> bool;

    fn thumbnail_demand(
        &self,
        priority: thumbnail_demand::Priority,
    ) -> Option<thumbnail_demand::Demand>;

    fn invalidate_ready_thumbnail(&mut self) -> bool;
}

pub fn thumbnail_demands<R: GridRow>(
    rows: &[R],
    visible_range: Range<usize>,
    generation: Generation,
    owner: thumbnail_demand::Owner,
) -> thumbnail_demand::DemandSet {
    let visible_range = visible_range.start.min(rows.len())..visible_range.end.min(rows.len());
    let (prefetch_before, prefetch_after) =
        thumbnail_demand::prefetch_ranges(visible_range.clone(), rows.len());
    let demands =
        thumbnail_demands_for_range(rows, visible_range, thumbnail_demand::Priority::VisibleRow)
            .chain(thumbnail_demands_for_range(
                rows,
                prefetch_before,
                thumbnail_demand::Priority::Prefetch,
            ))
            .chain(thumbnail_demands_for_range(
                rows,
                prefetch_after,
                thumbnail_demand::Priority::Prefetch,
            ))
            .collect();

    thumbnail_demand::DemandSet {
        owner,
        generation,
        replace: thumbnail_demand::ReplaceMode::Owner,
        demands,
    }
}

fn thumbnail_demands_for_range<R: GridRow>(
    rows: &[R],
    range: Range<usize>,
    priority: thumbnail_demand::Priority,
) -> impl Iterator<Item = thumbnail_demand::Demand> + '_ {
    rows.get(range)
        .unwrap_or_default()
        .iter()
        .filter_map(move |row| row.thumbnail_demand(priority))
}

pub fn release_offscreen_thumbnails<R: GridRow>(
    rows: &mut [R],
    visible_range: Range<usize>,
) -> Vec<usize> {
    let Some(retained) = thumbnail_demand::retained_rows(visible_range, rows.len()) else {
        return Vec::new();
    };

    let mut changed = Vec::new();
    for (index, row) in rows.iter_mut().enumerate() {
        if !retained.contains(&index) && row.invalidate_ready_thumbnail() {
            changed.push(index);
        }
    }
    changed
}

pub fn score_bucket(score: f32) -> i32 {
    (score.clamp(0.0, 1.0) * 5.0).round() as i32
}

pub fn score_label(score: f32) -> String {
    format!("{:.2}%", score.clamp(0.0, 1.0) * 100.0)
}

pub fn replace_if_changed<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}

pub fn append_grid_follow_up_effects<S, E>(
    state: &mut S,
    messages: Vec<addon_grid::Message>,
    effects: &mut Vec<E>,
    mut apply: impl FnMut(&mut S, addon_grid::Message, &mut Vec<E>),
) {
    for message in messages {
        apply(state, message, effects);
    }
}

#[derive(Debug)]
pub struct GridPane {
    grid: addon_grid::State,
    row_index: HashMap<CardId, usize>,
    lead_cards: usize,
    play_gifs_by_default: bool,
    window_focused: bool,
    download_count_formatter: DownloadCountFormatter,
    last_animation_tick: Option<Instant>,
}

impl GridPane {
    #[must_use]
    pub fn new(play_gifs_by_default: bool) -> Self {
        Self {
            grid: addon_grid::State::default(),
            row_index: HashMap::new(),
            lead_cards: 0,
            play_gifs_by_default,
            window_focused: true,
            download_count_formatter: DownloadCountFormatter::default(),
            last_animation_tick: None,
        }
    }

    pub const fn grid(&self) -> &addon_grid::State {
        &self.grid
    }

    pub const fn grid_mut(&mut self) -> &mut addon_grid::State {
        &mut self.grid
    }

    pub const fn play_gifs_by_default(&self) -> bool {
        self.play_gifs_by_default
    }

    pub const fn window_focused(&self) -> bool {
        self.window_focused
    }

    pub const fn formatter(&self) -> DownloadCountFormatter {
        self.download_count_formatter
    }

    pub const fn clear_animation_clock(&mut self) {
        self.last_animation_tick = None;
    }

    pub fn set_play_gifs_by_default(&mut self, enabled: bool) {
        self.play_gifs_by_default = enabled;
    }

    pub fn set_window_focused(&mut self, focused: bool) {
        self.window_focused = focused;
    }

    pub fn set_download_count_formatter(&mut self, formatter: DownloadCountFormatter) {
        self.download_count_formatter = formatter;
    }

    pub fn index_of(&self, id: &CardId) -> Option<usize> {
        self.index_of_str(id.as_str())
    }

    pub fn index_of_str(&self, id: &str) -> Option<usize> {
        self.row_index.get(id).copied()
    }

    /// Rebuild the id index after every change to the rows.
    pub fn reindex<R: GridRow>(&mut self, rows: &[R]) {
        self.row_index.clear();
        self.row_index.reserve(rows.len());
        for (index, row) in rows.iter().enumerate() {
            let _replaced = self.row_index.insert(row.id().clone(), index);
        }
    }

    #[must_use = "grid follow-ups must be relayed to the owner or explicitly bound and justified"]
    pub fn sync_items<R: GridRow>(
        &mut self,
        rows: &[R],
        lead: impl IntoIterator<Item = addon_grid::Item>,
    ) -> Vec<addon_grid::Message> {
        self.reindex(rows);

        let mut items: Vec<addon_grid::Item> = lead.into_iter().collect();
        self.lead_cards = items.len();
        items.reserve(rows.len());
        items.extend(
            rows.iter().map(|row| {
                row.to_grid_item(self.play_gifs_by_default, self.download_count_formatter)
            }),
        );
        self.grid.set_items(items)
    }

    /// Swaps one row's thumbnail into its card in place. A thumbnail never
    /// changes card layout, so this skips the rebuild `set_items` would cost.
    pub fn refresh_item_thumbnail<R: GridRow>(&mut self, rows: &[R], index: usize) {
        let Some(row) = rows.get(index) else {
            return;
        };
        let thumbnail = row.card_thumbnail(self.play_gifs_by_default);
        let id = row.id().clone();
        let _replaced = self
            .grid
            .update_item_thumbnail(index + self.lead_cards, &id, thumbnail);
    }

    pub fn visible_row_range(&self, row_count: usize) -> Range<usize> {
        self.rows_within(self.grid.visible_item_range(), row_count)
    }

    fn rows_within(&self, items: Range<usize>, row_count: usize) -> Range<usize> {
        let start = items.start.saturating_sub(self.lead_cards);
        let end = items.end.saturating_sub(self.lead_cards).min(row_count);
        start.min(end)..end
    }

    #[cfg(test)]
    fn with_lead_cards_for_test(lead_cards: usize) -> Self {
        let mut pane = Self::new(true);
        pane.lead_cards = lead_cards;
        pane
    }

    #[cfg(test)]
    fn visible_row_range_for_test(&self, items: Range<usize>, row_count: usize) -> Range<usize> {
        self.rows_within(items, row_count)
    }

    #[cfg(test)]
    const fn lead_cards_for_test(&self) -> usize {
        self.lead_cards
    }

    pub fn thumbnail_demands<R: GridRow>(
        &self,
        rows: &[R],
        generation: Generation,
        owner: thumbnail_demand::Owner,
    ) -> thumbnail_demand::DemandSet {
        thumbnail_demands(rows, self.visible_row_range(rows.len()), generation, owner)
    }

    pub fn invalidate_ready_thumbnails<R: GridRow>(&mut self, rows: &mut [R]) -> bool {
        let mut changed = false;
        for index in 0..rows.len() {
            if rows[index].invalidate_ready_thumbnail() {
                self.refresh_item_thumbnail(rows, index);
                changed = true;
            }
        }
        if changed {
            self.clear_animation_clock();
        }
        changed
    }

    /// Retain the visible and prefetch window even while hidden so returning
    /// to a route paints cached pixels without replaying every card's fade-in.
    pub fn release_offscreen_thumbnails<R: GridRow>(&mut self, rows: &mut [R]) -> bool {
        let visible = self.visible_row_range(rows.len());
        let changed = release_offscreen_thumbnails(rows, visible);
        for index in &changed {
            self.refresh_item_thumbnail(rows, *index);
        }
        !changed.is_empty()
    }

    pub fn has_active_animations<R: GridRow>(&self, rows: &[R]) -> bool {
        self.window_focused
            && rows
                .get(self.visible_row_range(rows.len()))
                .unwrap_or_default()
                .iter()
                .any(|row| row.is_animating(self.play_gifs_by_default))
    }

    pub fn tick_visible_animations<R: GridRow>(
        &mut self,
        rows: &mut [R],
        now: Instant,
        tick: Duration,
    ) -> bool {
        let elapsed = self
            .last_animation_tick
            .map_or(tick, |last| now.saturating_duration_since(last));
        self.last_animation_tick = Some(now);

        let visible = self.visible_row_range(rows.len());
        let play = self.play_gifs_by_default;
        let mut changed = false;
        if let Some(window) = rows.get_mut(visible.clone()) {
            for row in window {
                changed |= row.advance_animation(elapsed, play);
            }
        }
        if changed {
            for index in visible {
                self.refresh_item_thumbnail(rows, index);
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::{CardId, GridPane, GridRow};
    use crate::format::DownloadCountFormatter;
    use crate::media::thumbnail_demand;
    use crate::widgets::{addon_card, addon_grid};

    struct TestRow(CardId);

    impl GridRow for TestRow {
        fn id(&self) -> &CardId {
            &self.0
        }

        fn to_grid_item(
            &self,
            _play_gifs_by_default: bool,
            _formatter: DownloadCountFormatter,
        ) -> addon_grid::Item {
            addon_grid::Item::new(addon_card::Data::addon(self.0.clone(), "Row"))
        }

        fn card_thumbnail(&self, _play_gifs_by_default: bool) -> addon_card::Thumbnail {
            addon_card::Thumbnail::Dead
        }

        fn is_animating(&self, _play_gifs_by_default: bool) -> bool {
            false
        }

        fn advance_animation(
            &mut self,
            _elapsed: std::time::Duration,
            _play_gifs_by_default: bool,
        ) -> bool {
            false
        }

        fn thumbnail_demand(
            &self,
            _priority: thumbnail_demand::Priority,
        ) -> Option<thumbnail_demand::Demand> {
            None
        }

        fn invalidate_ready_thumbnail(&mut self) -> bool {
            false
        }
    }

    fn rows(count: usize) -> Vec<TestRow> {
        (0..count)
            .map(|index| TestRow(CardId::from(format!("row-{index}"))))
            .collect()
    }

    #[test]
    fn the_row_offset_is_derived_from_the_lead_actually_placed() {
        let mut pane = GridPane::new(true);
        let rows = rows(3);

        let _follow_ups = pane.sync_items(&rows, []);
        assert_eq!(pane.lead_cards_for_test(), 0);
        assert_eq!(pane.grid().items_len(), 3);

        let lead = addon_grid::Item::new(addon_card::Data::addon(CardId::from("lead"), "Lead"));
        let _follow_ups = pane.sync_items(&rows, [lead]);
        assert_eq!(pane.lead_cards_for_test(), 1);
        assert_eq!(pane.grid().items_len(), 4);
    }

    #[test]
    fn dropping_the_lead_returns_the_offset_to_zero() {
        let mut pane = GridPane::new(true);
        let rows = rows(2);
        let lead = addon_grid::Item::new(addon_card::Data::addon(CardId::from("lead"), "Lead"));

        let _follow_ups = pane.sync_items(&rows, [lead]);
        assert_eq!(pane.lead_cards_for_test(), 1);

        let _follow_ups = pane.sync_items(&rows, []);
        assert_eq!(pane.lead_cards_for_test(), 0);
    }

    #[test]
    fn a_lead_card_shifts_item_indices_off_the_row_range() {
        let with_lead = GridPane::with_lead_cards_for_test(1);

        assert_eq!(with_lead.visible_row_range_for_test(0..1, 10), 0..0);
        assert_eq!(with_lead.visible_row_range_for_test(0..4, 10), 0..3);
        assert_eq!(with_lead.visible_row_range_for_test(2..5, 10), 1..4);
    }

    #[test]
    fn without_lead_cards_item_and_row_indices_agree() {
        let no_lead = GridPane::with_lead_cards_for_test(0);

        assert_eq!(no_lead.visible_row_range_for_test(0..4, 10), 0..4);
        assert_eq!(no_lead.visible_row_range_for_test(2..5, 10), 2..5);
    }

    #[test]
    fn a_range_beyond_the_rows_is_clamped() {
        let pane = GridPane::with_lead_cards_for_test(1);

        assert_eq!(pane.visible_row_range_for_test(0..40, 3), 0..3);
        assert_eq!(pane.visible_row_range_for_test(0..1, 0), 0..0);
    }
}
