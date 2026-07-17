use std::collections::{HashMap, HashSet};
use std::mem;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::widget::image;

use crate::bridge::Settings;
use crate::bridge::domain::PublishedFileId;
use crate::bridge::ui_error::UiError;
use crate::format::DownloadCountFormatter;
use crate::media::thumbnail_demand;
use crate::widgets::addon_grid;

use super::model::{
    self, ContextMenuRequest, INSTALLED_ADDONS_PAGE_SIZE, MetadataPatch, MetadataResolution,
    PreviewTarget, Row,
};

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "route visibility, watch health, and playback/focus flags are independent UI state"
)]
pub struct State {
    route_visible: bool,
    grid: addon_grid::State,
    load_status: LoadStatus,
    generation: u64,
    watch_gmod_dir: Option<PathBuf>,
    watch_degraded: bool,
    watch_retry_attempted: bool,
    watch_arm_epoch: u64,
    discovered_rows: Option<Vec<Row>>,
    loaded_rows: Vec<Row>,
    /// workshop_id -> indices into `discovered_rows` sharing that id.
    /// Rebuilt any time `discovered_rows` is structurally replaced.
    discovered_index: HashMap<PublishedFileId, Vec<usize>>,
    /// workshop_id -> indices into `loaded_rows` sharing that id.
    /// Rebuilt any time `loaded_rows` is structurally replaced/extended.
    loaded_index: HashMap<PublishedFileId, Vec<usize>>,
    next_offset: usize,
    metadata_in_flight: HashSet<PublishedFileId>,
    metadata_finished: HashSet<PublishedFileId>,
    last_animation_tick: Option<Instant>,
    play_gifs_by_default: bool,
    window_focused: bool,
    download_count_formatter: DownloadCountFormatter,
    pending_preview: Option<PreviewTarget>,
    pending_context_menu: Option<ContextMenuRequest>,
}

impl Default for State {
    fn default() -> Self {
        let mut grid = addon_grid::State::default();
        let _ = grid.set_items(Vec::<addon_grid::Item>::new());

        Self {
            route_visible: false,
            grid,
            load_status: LoadStatus::Idle,
            generation: 0,
            watch_gmod_dir: None,
            watch_degraded: false,
            watch_retry_attempted: false,
            watch_arm_epoch: 0,
            discovered_rows: None,
            loaded_rows: Vec::new(),
            discovered_index: HashMap::new(),
            loaded_index: HashMap::new(),
            next_offset: 0,
            metadata_in_flight: HashSet::new(),
            metadata_finished: HashSet::new(),
            last_animation_tick: None,
            play_gifs_by_default: Settings::default().play_gifs_by_default,
            window_focused: true,
            download_count_formatter: DownloadCountFormatter::default(),
            pending_preview: None,
            pending_context_menu: None,
        }
    }
}

impl State {
    #[cfg(test)]
    pub(crate) const fn is_route_visible(&self) -> bool {
        self.route_visible
    }

    pub(crate) const fn load_status(&self) -> &LoadStatus {
        &self.load_status
    }

    pub(crate) fn loaded_count(&self) -> usize {
        self.loaded_rows.len()
    }

    #[cfg(feature = "debug")]
    pub(crate) fn hide_addon(
        &mut self,
        workshop_id: Option<PublishedFileId>,
        path: Option<&std::path::Path>,
    ) -> bool {
        let matches = |row: &Row| {
            path.is_some_and(|path| row.id() == path.to_string_lossy())
                || workshop_id.is_some_and(|id| row.workshop_id() == Some(id))
        };
        let previous_loaded_len = self.loaded_rows.len();
        let previous_offset = self.next_offset;
        let removed_before_offset = self.discovered_rows.as_ref().map_or(0, |rows| {
            rows.iter()
                .take(previous_offset)
                .filter(|row| matches(row))
                .count()
        });
        if let Some(rows) = &mut self.discovered_rows {
            rows.retain(|row| !matches(row));
        }
        self.loaded_rows.retain(|row| !matches(row));
        if self.loaded_rows.len() == previous_loaded_len && removed_before_offset == 0 {
            return false;
        }
        self.next_offset = self.next_offset.saturating_sub(removed_before_offset);
        self.discovered_index = self
            .discovered_rows
            .as_deref()
            .map_or_else(HashMap::new, build_workshop_index);
        self.loaded_index = build_workshop_index(&self.loaded_rows);
        self.pending_preview = None;
        self.pending_context_menu = None;
        self.sync_grid_items();
        true
    }

    pub(crate) fn total_count(&self) -> usize {
        self.discovered_rows.as_ref().map_or(0, Vec::len)
    }

    pub(crate) const fn watch_arm_epoch(&self) -> u64 {
        self.watch_arm_epoch
    }

    pub(crate) fn watch_gmod_dir(&self) -> Option<&PathBuf> {
        self.watch_gmod_dir.as_ref()
    }

    /// Points the library watcher at a (new) gmod dir. The subscription is
    /// keyed on the derived roots, so a change re-arms the watcher by itself;
    /// the fresh stream reports its own `WatchArmed` status.
    pub(crate) fn set_watch_gmod_dir(&mut self, gmod_dir: Option<PathBuf>) {
        if self.watch_gmod_dir == gmod_dir {
            return;
        }
        self.watch_gmod_dir = gmod_dir;
        self.watch_degraded = false;
        self.watch_retry_attempted = false;
    }

    pub(crate) const fn grid(&self) -> &addon_grid::State {
        &self.grid
    }

    pub(crate) fn workshop_id_for_card(&self, id: &str) -> Option<PublishedFileId> {
        self.loaded_rows
            .iter()
            .find(|row| row.id() == id)
            .and_then(Row::workshop_id)
    }

    pub(crate) fn drag_thumbnail_for_card(&self, id: &str) -> Option<image::Handle> {
        self.loaded_rows
            .iter()
            .find(|row| row.id() == id)
            .and_then(Row::drag_thumbnail)
    }

    pub(crate) fn thumbnail_demands(&self) -> thumbnail_demand::DemandSet {
        if !self.route_visible {
            return model::empty_thumbnail_demands();
        }

        model::thumbnail_demands(
            &self.loaded_rows,
            self.grid.visible_item_range(),
            self.generation,
        )
    }

    pub(crate) fn apply_thumbnail_delivery(
        &mut self,
        delivery: &thumbnail_demand::Delivery,
    ) -> bool {
        if delivery.owner != model::thumbnail_owner() || delivery.generation != self.generation {
            return false;
        }

        let mut changed = false;
        for row in &mut self.loaded_rows {
            changed |= row.apply_thumbnail_delivery(delivery.generation, delivery, self.generation);
        }
        if changed {
            self.refresh_item_thumbnails();
        }
        changed
    }

    pub(crate) fn invalidate_ready_thumbnails(&mut self) -> bool {
        let changed = model::invalidate_ready_thumbnails(&mut self.loaded_rows);
        if changed {
            self.sync_grid_items();
            self.last_animation_tick = None;
        }
        changed
    }

    /// Drops Ready thumbnails outside the visible+prefetch window; the
    /// demand/cache path re-delivers. The window is kept even while the
    /// route is hidden so returning to it paints real pixels on the first
    /// frame instead of replaying every card's fade-in.
    pub(crate) fn release_offscreen_thumbnails(&mut self) -> bool {
        let changed = model::release_offscreen_thumbnails(
            &mut self.loaded_rows,
            self.grid.visible_item_range(),
        );
        if changed {
            self.refresh_item_thumbnails();
        }
        changed
    }

    pub(crate) fn has_active_animations(&self) -> bool {
        self.window_focused
            && self.route_visible
            && self
                .loaded_rows
                .get(self.grid.visible_item_range())
                .unwrap_or_default()
                .iter()
                .any(|row| row.has_active_animation(self.play_gifs_by_default))
    }

    pub(crate) fn needs_card_motion_ticks(&self) -> bool {
        self.route_visible && self.grid.needs_visible_card_ticks()
    }

    pub(super) fn tick_visible_card_motion(&mut self, now: Instant) {
        if self.route_visible {
            self.grid.tick_visible_card_motion(now);
        }
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

    pub(super) const fn grid_mut(&mut self) -> &mut addon_grid::State {
        &mut self.grid
    }

    pub(super) fn enter_route(&mut self) {
        self.route_visible = true;
        self.rearm_watch_on_route_entry();
        if self.discovered_rows.is_none()
            && matches!(self.load_status, LoadStatus::Idle | LoadStatus::Error(_))
        {
            self.load_status = LoadStatus::Loading;
            // Grid is always empty here (guarded above): no layout reflow,
            // so no follow-up message can be produced.
            let _ = self.grid.set_page_status(true, false);
        }
    }

    pub(super) fn exit_route(&mut self) {
        self.route_visible = false;
        self.last_animation_tick = None;
        self.pending_preview = None;
        self.pending_context_menu = None;
        self.watch_retry_attempted = false;
        // Items/scroll/viewport are untouched and has_more_pages is forced
        // false, so reconciliation can't change the visible range or ask
        // for another page.
        let _ = self.grid.set_page_status(false, false);
    }

    pub(super) fn refresh_started(&mut self, reason: crate::bridge::library::LibraryRefreshReason) {
        log::info!(
            "installed addons refresh started: {reason:?}, route_visible {}, discovered {}",
            self.route_visible,
            self.total_count(),
        );
        if reason.loud() {
            self.begin_loud_refresh();
        } else if self.route_visible && self.discovered_rows.is_none() {
            self.load_status = LoadStatus::Loading;
            // Same as enter_route: grid is empty while discovery is pending.
            let _ = self.grid.set_page_status(true, false);
        }
    }

    fn begin_loud_refresh(&mut self) {
        if self.route_visible {
            self.load_status = LoadStatus::Loading;
        } else {
            self.load_status = LoadStatus::Idle;
        }
        self.discovered_rows = None;
        self.discovered_index.clear();
        self.loaded_rows.clear();
        self.loaded_index.clear();
        self.next_offset = 0;
        self.metadata_in_flight.clear();
        self.metadata_finished.clear();
        self.last_animation_tick = None;
        self.pending_preview = None;
        self.pending_context_menu = None;
        // Clearing to empty can shift the visible range, but the pending
        // `SnapshotPushed` that always follows a refresh re-syncs metadata
        // and thumbnail demands from scratch, so an echoed
        // `VisibleRangeChanged` here would be redundant.
        let _ = self.grid.set_items(Vec::new());
        let _ = self.grid.set_page_status(
            self.route_visible && matches!(self.load_status, LoadStatus::Loading),
            false,
        );
    }

    pub(super) fn apply_snapshot(
        &mut self,
        reason: crate::bridge::library::LibraryRefreshReason,
        result: Result<Vec<Row>, UiError>,
    ) {
        self.generation = self.generation.wrapping_add(1).max(1);

        let incoming = result.as_ref().ok().map(Vec::len);
        if reason.loud() || self.discovered_rows.is_none() {
            self.apply_loud_discovery(result);
        } else {
            self.apply_quiet_discovery(result);
        }
        // Launch-time triage: says whether rows reached this state and what
        // the grid was told, so a short grid is attributable to delivery,
        // discovery, or paging.
        log::info!(
            "installed addons snapshot applied: {reason:?} incoming {incoming:?} -> \
             discovered {}, loaded {}, grid items {}, status {:?}, route_visible {}",
            self.total_count(),
            self.loaded_rows.len(),
            self.grid.items_len(),
            self.load_status,
            self.route_visible,
        );
    }

    pub(super) fn apply_watch_armed(&mut self, degraded: bool) {
        self.watch_degraded = degraded;
        if !degraded {
            self.watch_retry_attempted = false;
        }
    }

    /// A degraded watch (some root failed to arm — e.g. dir didn't exist
    /// yet) gets one retry per route entry: bumping the epoch re-keys the
    /// subscription, which drops and re-arms the watcher on every root.
    fn rearm_watch_on_route_entry(&mut self) {
        if self.watch_degraded && !self.watch_retry_attempted {
            self.watch_retry_attempted = true;
            self.watch_arm_epoch = self.watch_arm_epoch.wrapping_add(1);
        }
    }

    fn apply_loud_discovery(&mut self, result: Result<Vec<Row>, UiError>) {
        self.metadata_in_flight.clear();
        self.metadata_finished.clear();
        self.pending_preview = None;
        self.pending_context_menu = None;

        match result {
            Ok(rows) if rows.is_empty() => {
                self.discovered_rows = Some(Vec::new());
                self.discovered_index.clear();
                self.loaded_rows.clear();
                self.loaded_index.clear();
                self.next_offset = 0;
                self.load_status = LoadStatus::Empty;
                self.sync_grid_items();
            }
            Ok(rows) => {
                self.discovered_index = build_workshop_index(&rows);
                self.discovered_rows = Some(rows);
                self.loaded_rows.clear();
                self.loaded_index.clear();
                self.next_offset = 0;
                self.load_status = LoadStatus::Ready;
                self.append_next_page();
            }
            Err(error) => {
                self.discovered_rows = None;
                self.discovered_index.clear();
                self.loaded_rows.clear();
                self.loaded_index.clear();
                self.next_offset = 0;
                self.load_status = LoadStatus::Error(error.to_string());
                self.sync_grid_items();
            }
        }
    }

    fn apply_quiet_discovery(&mut self, result: Result<Vec<Row>, UiError>) {
        let rows = match result {
            Ok(rows) => rows,
            Err(error) => {
                // A transient scan error (file mid-move) must never blank a
                // list that was fine a second ago; keep what's on screen.
                log::debug!("quiet installed addon discovery failed: {error}");
                return;
            }
        };

        let old_next_offset = self.next_offset;
        let mut old_by_id = mem::take(&mut self.loaded_rows)
            .into_iter()
            .map(|row| (row.id().to_owned(), row))
            .collect::<HashMap<_, _>>();
        let mut unchanged_workshop_ids = HashSet::new();
        let merged_rows = rows
            .into_iter()
            .map(|row| match old_by_id.remove(row.id()) {
                Some(old) if old.has_same_file_fingerprint(&row) => {
                    if let Some(workshop_id) = old.workshop_id() {
                        unchanged_workshop_ids.insert(workshop_id);
                    }
                    old
                }
                _ => row,
            })
            .collect::<Vec<_>>();

        let new_len = merged_rows.len();
        self.discovered_index = build_workshop_index(&merged_rows);
        self.discovered_rows = Some(merged_rows);
        self.loaded_index.clear();
        self.next_offset = if new_len == 0 {
            0
        } else {
            old_next_offset
                .min(new_len)
                .max(INSTALLED_ADDONS_PAGE_SIZE.min(new_len))
        };

        if let Some(discovered_rows) = &self.discovered_rows {
            self.loaded_rows
                .extend_from_slice(&discovered_rows[..self.next_offset]);
        }
        self.loaded_index = build_workshop_index(&self.loaded_rows);
        self.metadata_in_flight.clear();
        self.metadata_finished
            .retain(|workshop_id| unchanged_workshop_ids.contains(workshop_id));
        self.pending_preview = None;
        self.pending_context_menu = None;
        self.load_status = if self.loaded_rows.is_empty() {
            LoadStatus::Empty
        } else {
            LoadStatus::Ready
        };
        self.sync_grid_items();
    }

    pub(super) fn append_next_page(&mut self) -> bool {
        let appended = self.extend_loaded_rows_by_one_page();
        self.sync_grid_items();
        log::info!(
            "installed addons page appended: {appended}, loaded {} of {}, status {:?}",
            self.loaded_rows.len(),
            self.total_count(),
            self.load_status,
        );
        appended
    }

    /// Extends `loaded_rows` by one page from `discovered_rows`, without
    /// reconciling the grid; callers must follow up with `sync_grid_items`,
    /// which itself calls back in here when a page still doesn't fill the
    /// viewport.
    fn extend_loaded_rows_by_one_page(&mut self) -> bool {
        if matches!(self.load_status, LoadStatus::Loading) {
            return false;
        }
        let Some(discovered_rows) = &self.discovered_rows else {
            return false;
        };

        let start = self.next_offset.min(discovered_rows.len());
        let end = start
            .saturating_add(INSTALLED_ADDONS_PAGE_SIZE)
            .min(discovered_rows.len());
        if start == end {
            return false;
        }

        let base_index = self.loaded_rows.len();
        self.loaded_rows
            .extend_from_slice(&discovered_rows[start..end]);
        for (offset, row) in self.loaded_rows[base_index..].iter().enumerate() {
            if let Some(workshop_id) = row.workshop_id() {
                self.loaded_index
                    .entry(workshop_id)
                    .or_default()
                    .push(base_index + offset);
            }
        }
        self.next_offset = end;
        self.load_status = if self.loaded_rows.is_empty() {
            LoadStatus::Empty
        } else {
            LoadStatus::Ready
        };
        true
    }

    /// Returns metadata IDs for visible rows, then the thumbnail prefetch
    /// window.
    ///
    /// The metadata window must match `model::thumbnail_demands` and
    /// `thumbnail_demand::retained_rows`: all three use `prefetch_ranges` so
    /// rows retained or requested for thumbnail prefetch have a resolved
    /// `preview_url`; without metadata, no thumbnail demand can exist. Visible
    /// IDs stay first because Steam UGC queries are chunked at 50.
    pub(super) fn take_visible_metadata_request(&mut self) -> Option<(u64, Vec<PublishedFileId>)> {
        if !self.route_visible || self.loaded_rows.is_empty() {
            return None;
        }

        let mut seen = HashSet::new();
        let mut item_ids = Vec::new();
        let visible = self.grid.visible_item_range();
        let (before, after) =
            thumbnail_demand::prefetch_ranges(visible.clone(), self.loaded_rows.len());
        for range in [visible, after, before] {
            for row in self.loaded_rows.get(range).unwrap_or_default() {
                let Some(item_id) = row_workshop_id(row) else {
                    continue;
                };
                if self.metadata_in_flight.contains(&item_id)
                    || self.metadata_finished.contains(&item_id)
                    || !seen.insert(item_id)
                {
                    continue;
                }
                item_ids.push(item_id);
            }
        }

        if item_ids.is_empty() {
            return None;
        }

        self.metadata_in_flight.extend(item_ids.iter().copied());
        Some((self.generation, item_ids))
    }

    pub(super) fn finish_metadata_request(
        &mut self,
        generation: u64,
        item_ids: &[PublishedFileId],
        result: Result<MetadataResolution, UiError>,
    ) -> Option<(u64, Vec<PublishedFileId>)> {
        if generation != self.generation {
            return None;
        }

        for item_id in item_ids {
            self.metadata_in_flight.remove(item_id);
            self.metadata_finished.insert(*item_id);
        }

        let Ok(resolution) = result else {
            return None;
        };
        self.apply_metadata_patches(generation, &resolution.patches);
        (!resolution.stale_ids.is_empty()).then_some((generation, resolution.stale_ids))
    }

    pub(super) fn apply_metadata_refresh(
        &mut self,
        generation: u64,
        result: Result<Vec<MetadataPatch>, UiError>,
    ) {
        if let Ok(patches) = result {
            self.apply_metadata_patches(generation, &patches);
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
            .unwrap_or(thumbnail_animation_tick());
        self.last_animation_tick = Some(now);

        let visible = self.grid.visible_item_range();
        let mut changed = false;
        if let Some(rows) = self.loaded_rows.get_mut(visible.clone()) {
            for row in rows {
                changed |= row.advance_animation(elapsed, self.play_gifs_by_default);
            }
        }
        if changed {
            // Swap advanced frames in place; a full sync_grid_items rebuild
            // (every card re-allocated + re-layout) per 16ms tick is churn.
            if let Some(rows) = self.loaded_rows.get(visible.clone()) {
                for (offset, row) in rows.iter().enumerate() {
                    let thumbnail = row.card_thumbnail(self.play_gifs_by_default);
                    let _ = self.grid.update_item_thumbnail(
                        visible.start + offset,
                        row.id(),
                        thumbnail,
                    );
                }
            }
        }
        changed
    }

    pub(super) fn take_preview_target(&mut self, id: &str) -> Option<PreviewTarget> {
        let target = self
            .loaded_rows
            .iter()
            .find(|row| row.id() == id)?
            .preview_target()?;
        self.pending_preview = Some(target.clone());
        Some(target)
    }

    pub(super) fn take_context_menu(
        &mut self,
        id: &str,
        position: iced::Point,
    ) -> Option<ContextMenuRequest> {
        let mut request = self
            .loaded_rows
            .iter()
            .find(|row| row.id() == id)?
            .context_menu()?;
        request.position = position;
        self.pending_context_menu = Some(request.clone());
        Some(request)
    }

    pub(super) fn set_card_hovered(&mut self, id: &str, hovered: bool) -> bool {
        let Some((index, row)) = self
            .loaded_rows
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
        let _ = self.grid.update_item_thumbnail(index, row.id(), thumbnail);
        true
    }

    fn apply_metadata_patches(&mut self, generation: u64, patches: &[MetadataPatch]) {
        if generation != self.generation || patches.is_empty() {
            return;
        }

        let mut changed = false;
        if let Some(discovered_rows) = &mut self.discovered_rows {
            for patch in patches {
                for &index in self
                    .discovered_index
                    .get(&patch.workshop_id())
                    .map_or(&[][..], Vec::as_slice)
                {
                    if let Some(row) = discovered_rows.get_mut(index) {
                        changed |= row.apply_metadata_patch(patch);
                    }
                }
            }
        }
        for patch in patches {
            for &index in self
                .loaded_index
                .get(&patch.workshop_id())
                .map_or(&[][..], Vec::as_slice)
            {
                if let Some(row) = self.loaded_rows.get_mut(index) {
                    changed |= row.apply_metadata_patch(patch);
                }
            }
        }
        if changed {
            self.sync_grid_items();
        }
    }

    /// Pushes every row's current thumbnail into the grid in place. A
    /// thumbnail never changes card geometry, so thumbnail-only changes
    /// (delivery, offscreen release, hover play/pause) skip the full
    /// `sync_grid_items` rebuild — re-allocating every card and re-measuring
    /// every title per scroll event is what makes large libraries lag.
    fn refresh_item_thumbnails(&mut self) {
        for (index, row) in self.loaded_rows.iter().enumerate() {
            let thumbnail = row.card_thumbnail(self.play_gifs_by_default);
            let _ = self.grid.update_item_thumbnail(index, row.id(), thumbnail);
        }
    }

    /// Rebuilds the grid's item list and page status from `loaded_rows`.
    ///
    /// The grid's own reconciliation can ask for another page (e.g. a page
    /// of appended rows still doesn't fill a tall viewport); normally that
    /// travels as a `Message::Grid(NextPageRequested)` round trip, but this
    /// method is called directly rather than through the message bus, so it
    /// loops here instead until the viewport is full or there are no more
    /// pages. Every other follow-up message (hover, visible-range) is
    /// already re-derived by the caller from state, not from these echoes.
    fn sync_grid_items(&mut self) {
        loop {
            let items = self
                .loaded_rows
                .iter()
                .map(|row| {
                    row.to_grid_item(self.play_gifs_by_default, self.download_count_formatter)
                })
                .collect();
            let mut messages = self.grid.set_items(items);
            messages.extend(self.grid.set_page_status(
                matches!(self.load_status, LoadStatus::Loading),
                self.next_offset < self.total_count(),
            ));

            let wants_next_page = messages
                .iter()
                .any(|message| matches!(message, addon_grid::Message::NextPageRequested));
            if !wants_next_page || !self.extend_loaded_rows_by_one_page() {
                break;
            }
        }
    }
}

const fn thumbnail_animation_tick() -> Duration {
    crate::media::thumbnail_animation::ANIMATION_TICK_INTERVAL
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadStatus {
    Idle,
    Loading,
    Ready,
    Empty,
    Error(String),
}

fn row_workshop_id(row: &Row) -> Option<PublishedFileId> {
    row.workshop_id()
}

/// Builds a `workshop_id -> row indices` lookup so metadata patches can be
/// applied in O(patches + rows) instead of scanning the whole slice per
/// patch. Call whenever `rows` is structurally replaced.
fn build_workshop_index(rows: &[Row]) -> HashMap<PublishedFileId, Vec<usize>> {
    let mut index = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        if let Some(workshop_id) = row.workshop_id() {
            index.entry(workshop_id).or_insert_with(Vec::new).push(i);
        }
    }
    index
}

#[cfg(test)]
mod tests;
