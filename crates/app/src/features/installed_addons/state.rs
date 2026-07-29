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
    self, ContextMenuRequest, MetadataPatch, MetadataResolution, PreviewTarget, Row,
};
use crate::generation::Generation;
use crate::widgets::grid_rows;

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "route visibility, watch health, and playback/focus flags are independent UI state"
)]
pub struct State {
    route_visible: bool,
    grid: addon_grid::State,
    load_status: LoadStatus,
    generation: Generation,
    watch_gmod_dir: Option<PathBuf>,
    watch_degraded: bool,
    watch_retry_attempted: bool,
    watch_arm_epoch: u64,
    /// The full local library; `None` until the first scan lands. The grid
    /// virtualizes rendering and windows hydration itself, so every row is
    /// handed to it at once — paging a fully-known local list only made the
    /// scrollable's content (and scrollbar) grow while scrolling.
    rows: Option<Vec<Row>>,
    /// Row id -> index into `rows`.
    ///
    /// A delivery names exactly one row; scanning for it and then refreshing
    /// every card made each delivery O(rows) twice, which is what dominated
    /// per-tick tail latency on large libraries.
    row_index: HashMap<String, usize>,
    /// workshop_id -> indices into `rows` sharing that id. Rebuilt any time
    /// `rows` is structurally replaced.
    workshop_index: HashMap<PublishedFileId, Vec<usize>>,
    metadata_in_flight: HashSet<PublishedFileId>,
    /// Steam answered for these ids; there is nothing left to ask.
    metadata_finished: HashSet<PublishedFileId>,
    /// The lookup or the refresh behind it failed for these ids. Held
    /// separately from `metadata_finished` because the failure is transient:
    /// they are excluded from requests only until something that could change
    /// the answer happens — route entry, or Steam reaching *connected* — at
    /// which point [`Self::retry_failed_metadata`] releases them. Marking them
    /// finished instead stranded the rows until the next loud refresh.
    metadata_failed: HashSet<PublishedFileId>,
    last_animation_tick: Option<Instant>,
    play_gifs_by_default: bool,
    window_focused: bool,
    download_count_formatter: DownloadCountFormatter,
    pending_preview: Option<PreviewTarget>,
    pending_context_menu: Option<ContextMenuRequest>,
    /// Accumulated offset delta the grid anchored itself by after hydration
    /// changed row heights; drained by `update` into an effect that drives a
    /// relative `scroll_by` (the widget's real offset lives in Iced). An
    /// anchor landing while the route is hidden still moves the mirror
    /// offset only — route re-entry snaps Iced back to the mirror.
    pending_scroll_anchor: Option<f32>,
}

impl Default for State {
    fn default() -> Self {
        let mut grid = addon_grid::State::default();
        let _ = grid.set_items(Vec::<addon_grid::Item>::new());

        Self {
            route_visible: false,
            grid,
            load_status: LoadStatus::Idle,
            generation: Generation::INITIAL,
            watch_gmod_dir: None,
            watch_degraded: false,
            watch_retry_attempted: false,
            watch_arm_epoch: 0,
            rows: None,
            row_index: HashMap::new(),
            workshop_index: HashMap::new(),
            metadata_in_flight: HashSet::new(),
            metadata_finished: HashSet::new(),
            metadata_failed: HashSet::new(),
            last_animation_tick: None,
            play_gifs_by_default: Settings::default().play_gifs_by_default,
            window_focused: true,
            download_count_formatter: DownloadCountFormatter::default(),
            pending_preview: None,
            pending_context_menu: None,
            pending_scroll_anchor: None,
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

    pub(crate) fn row_count(&self) -> usize {
        self.rows().len()
    }

    fn rows(&self) -> &[Row] {
        self.rows.as_deref().unwrap_or_default()
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
        let Some(rows) = &mut self.rows else {
            return false;
        };
        let previous_len = rows.len();
        rows.retain(|row| !matches(row));
        if rows.len() == previous_len {
            return false;
        }
        self.workshop_index = build_workshop_index(rows);
        self.pending_preview = None;
        self.pending_context_menu = None;
        self.sync_grid_items();
        true
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

    /// `row_index` is rebuilt by `sync_grid_items` after every structural
    /// mutation of `rows`, so id lookups here are O(1) instead of scanning.
    fn row_by_id(&self, id: &str) -> Option<&Row> {
        self.rows().get(*self.row_index.get(id)?)
    }

    pub(crate) fn workshop_id_for_card(&self, id: &str) -> Option<PublishedFileId> {
        self.row_by_id(id).and_then(Row::workshop_id)
    }

    pub(crate) fn drag_thumbnail_for_card(&self, id: &str) -> Option<image::Handle> {
        self.row_by_id(id).and_then(Row::drag_thumbnail)
    }

    pub(crate) fn thumbnail_demands(&self) -> thumbnail_demand::DemandSet {
        if !self.route_visible {
            return model::empty_thumbnail_demands();
        }

        model::thumbnail_demands(self.rows(), self.grid.visible_item_range(), self.generation)
    }

    pub(crate) fn apply_thumbnail_delivery(
        &mut self,
        delivery: &thumbnail_demand::Delivery,
    ) -> bool {
        if delivery.owner != model::thumbnail_owner() || delivery.generation != self.generation {
            return false;
        }

        let Some(&index) = self.row_index.get(delivery.id.as_str()) else {
            return false;
        };
        let Some(row) = self
            .rows
            .as_deref_mut()
            .and_then(|rows| rows.get_mut(index))
        else {
            return false;
        };
        if !row.apply_thumbnail_delivery(delivery.generation, delivery, self.generation) {
            return false;
        }

        self.refresh_item_thumbnail(index);
        true
    }

    pub(crate) fn invalidate_ready_thumbnails(&mut self) -> bool {
        let changed =
            grid_rows::invalidate_ready_thumbnails(self.rows.as_deref_mut().unwrap_or_default());
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
        let visible = self.grid.visible_item_range();
        let changed = grid_rows::release_offscreen_thumbnails(
            self.rows.as_deref_mut().unwrap_or_default(),
            visible,
        );
        for index in &changed {
            self.refresh_item_thumbnail(*index);
        }
        !changed.is_empty()
    }

    pub(crate) fn has_active_animations(&self) -> bool {
        self.window_focused
            && self.route_visible
            && self
                .rows()
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
        // Re-entering is the gesture people already use to re-ask a route that
        // gave up, so it retries lookups that failed while Steam was down.
        let _ = self.retry_failed_metadata();
        if self.rows.is_none()
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
            self.row_count(),
        );
        if reason.loud() {
            self.begin_loud_refresh();
        } else if self.route_visible && self.rows.is_none() {
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
        self.rows = None;
        self.workshop_index.clear();
        self.metadata_in_flight.clear();
        self.metadata_finished.clear();
        self.metadata_failed.clear();
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
        self.generation.bump();

        let incoming = result.as_ref().ok().map(Vec::len);
        if reason.loud() || self.rows.is_none() {
            self.apply_loud_discovery(result);
        } else {
            self.apply_quiet_discovery(result);
        }
        // Launch-time triage: says whether rows reached this state and what
        // the grid was told, so a short grid is attributable to delivery,
        // discovery, or paging.
        log::info!(
            "installed addons snapshot applied: {reason:?} incoming {incoming:?} -> \
             rows {}, grid items {}, status {:?}, route_visible {}",
            self.row_count(),
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
        self.metadata_failed.clear();
        self.pending_preview = None;
        self.pending_context_menu = None;

        match result {
            Ok(rows) if rows.is_empty() => {
                self.rows = Some(Vec::new());
                self.workshop_index.clear();
                self.load_status = LoadStatus::Empty;
            }
            Ok(rows) => {
                self.workshop_index = build_workshop_index(&rows);
                self.rows = Some(rows);
                self.load_status = LoadStatus::Ready;
            }
            Err(error) => {
                self.rows = None;
                self.workshop_index.clear();
                self.load_status = LoadStatus::Error(error);
            }
        }
        self.sync_grid_items();
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

        let mut old_by_id = mem::take(&mut self.rows)
            .unwrap_or_default()
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

        self.workshop_index = build_workshop_index(&merged_rows);
        self.load_status = if merged_rows.is_empty() {
            LoadStatus::Empty
        } else {
            LoadStatus::Ready
        };
        self.rows = Some(merged_rows);
        self.metadata_in_flight.clear();
        self.metadata_finished
            .retain(|workshop_id| unchanged_workshop_ids.contains(workshop_id));
        self.metadata_failed
            .retain(|workshop_id| unchanged_workshop_ids.contains(workshop_id));
        self.pending_preview = None;
        self.pending_context_menu = None;
        self.sync_grid_items();
    }

    /// Returns metadata IDs for visible rows, then the thumbnail prefetch
    /// window.
    ///
    /// The metadata window must match `model::thumbnail_demands` and
    /// `thumbnail_demand::retained_rows`: all three use `prefetch_ranges` so
    /// rows retained or requested for thumbnail prefetch have a resolved
    /// `preview_url`; without metadata, no thumbnail demand can exist. Visible
    /// IDs stay first because Steam UGC queries are chunked at 50.
    pub(super) fn take_visible_metadata_request(
        &mut self,
    ) -> Option<(Generation, Vec<PublishedFileId>)> {
        if !self.route_visible || self.rows().is_empty() {
            return None;
        }

        let mut seen = HashSet::new();
        let mut item_ids = Vec::new();
        let visible = self.grid.visible_item_range();
        let (before, after) = thumbnail_demand::prefetch_ranges(visible.clone(), self.rows().len());
        for range in [visible, after, before] {
            for row in self.rows().get(range).unwrap_or_default() {
                let Some(item_id) = row_workshop_id(row) else {
                    continue;
                };
                if self.metadata_in_flight.contains(&item_id)
                    || self.metadata_finished.contains(&item_id)
                    || self.metadata_failed.contains(&item_id)
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
        generation: Generation,
        item_ids: &[PublishedFileId],
        result: Result<MetadataResolution, UiError>,
    ) -> Option<(Generation, Vec<PublishedFileId>)> {
        if generation != self.generation {
            return None;
        }

        for item_id in item_ids {
            self.metadata_in_flight.remove(item_id);
        }

        let resolution = match result {
            Ok(resolution) => resolution,
            Err(error) => {
                // A failed lookup says nothing about these ids, so they are
                // not finished — only parked until a retry point. The rows
                // keep the local title and dead thumbnail they were scanned
                // with; what is missing is everything Steam adds on top.
                log::debug!("installed addons metadata lookup failed: {error}");
                self.metadata_failed.extend(item_ids.iter().copied());
                return None;
            }
        };

        self.metadata_finished.extend(item_ids.iter().copied());
        self.apply_metadata_patches(generation, &resolution.patches);
        (!resolution.stale_ids.is_empty()).then_some((generation, resolution.stale_ids))
    }

    /// Releases ids parked by a failed lookup so the next poll asks again.
    ///
    /// Called on route entry and when Steam connects — the two points where
    /// the answer can plausibly have changed. Returns whether anything was
    /// actually released, so callers can skip a pointless request.
    pub(super) fn retry_failed_metadata(&mut self) -> bool {
        !std::mem::take(&mut self.metadata_failed).is_empty()
    }

    #[cfg(test)]
    pub(crate) fn has_failed_metadata(&self) -> bool {
        !self.metadata_failed.is_empty()
    }

    pub(super) fn apply_metadata_refresh(
        &mut self,
        generation: Generation,
        item_ids: &[PublishedFileId],
        result: Result<Vec<MetadataPatch>, UiError>,
    ) {
        match result {
            Ok(patches) => self.apply_metadata_patches(generation, &patches),
            Err(error) => {
                if generation != self.generation {
                    return;
                }
                // The cache lookup that queued this refresh already marked
                // these ids finished, so dropping the failure silently left
                // them unaskable — the rows kept whatever the cache held (for
                // an id never cached, nothing at all) until a loud refresh.
                log::debug!("installed addons metadata refresh failed: {error}");
                for item_id in item_ids {
                    self.metadata_finished.remove(item_id);
                    self.metadata_failed.insert(*item_id);
                }
            }
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
        if let Some(rows) = self
            .rows
            .as_deref_mut()
            .and_then(|rows| rows.get_mut(visible.clone()))
        {
            for row in rows {
                changed |= row.advance_animation(elapsed, self.play_gifs_by_default);
            }
        }
        if changed {
            // Swap advanced frames in place; a full sync_grid_items rebuild
            // (every card re-allocated + re-layout) per 16ms tick is churn.
            if let Some(rows) = self
                .rows
                .as_deref()
                .and_then(|rows| rows.get(visible.clone()))
            {
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
        let target = self.row_by_id(id)?.preview_target()?;
        self.pending_preview = Some(target.clone());
        Some(target)
    }

    pub(super) fn take_context_menu(
        &mut self,
        id: &str,
        position: iced::Point,
    ) -> Option<ContextMenuRequest> {
        let mut request = self.row_by_id(id)?.context_menu()?;
        request.position = position;
        self.pending_context_menu = Some(request.clone());
        Some(request)
    }

    pub(super) fn set_card_hovered(&mut self, id: &str, hovered: bool) -> bool {
        let Some(&index) = self.row_index.get(id) else {
            return false;
        };
        let Some(row) = self
            .rows
            .as_deref_mut()
            .and_then(|rows| rows.get_mut(index))
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

    fn apply_metadata_patches(&mut self, generation: Generation, patches: &[MetadataPatch]) {
        if generation != self.generation || patches.is_empty() {
            return;
        }

        let mut changed_indices = Vec::new();
        for patch in patches {
            for &index in self
                .workshop_index
                .get(&patch.workshop_id())
                .map_or(&[][..], Vec::as_slice)
            {
                if let Some(row) = self
                    .rows
                    .as_deref_mut()
                    .and_then(|rows| rows.get_mut(index))
                    && row.apply_metadata_patch(patch)
                {
                    changed_indices.push(index);
                }
            }
        }
        if changed_indices.is_empty() {
            return;
        }

        // A patch names a few rows; swapping just those grid items in place
        // keeps a metadata batch from re-allocating every card in the
        // library. Row ids are untouched by patches, so `row_index` and the
        // follow-up messages the callers re-derive stay valid.
        let updates = changed_indices
            .iter()
            .filter_map(|&index| {
                self.rows().get(index).map(|row| {
                    (
                        index,
                        row.to_grid_item(self.play_gifs_by_default, self.download_count_formatter),
                    )
                })
            })
            .collect();
        for message in self.grid.patch_items(updates) {
            // Visible-range echoes are re-derived by the caller (it re-runs
            // the metadata/thumbnail effects from state after this); hover
            // echoes are dropped as on every sync path — a hover retargeted
            // by rows shifting under a stationary cursor self-corrects on
            // the next cursor move. The anchor delta is the one follow-up
            // that must reach the runtime, and deltas from batches landing
            // before a drain accumulate.
            if let addon_grid::Message::ScrollAnchored(delta) = message {
                *self.pending_scroll_anchor.get_or_insert(0.0) += delta;
            }
        }
    }

    pub(super) fn take_scroll_anchor(&mut self) -> Option<f32> {
        self.pending_scroll_anchor.take()
    }

    /// Pushes every row's current thumbnail into the grid in place. A
    /// thumbnail never changes card geometry, so thumbnail-only changes
    /// (delivery, offscreen release, hover play/pause) skip the full
    /// `sync_grid_items` rebuild — re-allocating every card and re-measuring
    /// every title per scroll event is what makes large libraries lag.
    /// Pushes one row's current thumbnail into the grid in place.
    ///
    /// Installed-addon grids have no lead card, so the row index is the item
    /// index — unlike My Workshop, where item 0 is "publish new".
    fn refresh_item_thumbnail(&mut self, index: usize) {
        let Some(row) = self.rows.as_deref().and_then(|rows| rows.get(index)) else {
            return;
        };
        let thumbnail = row.card_thumbnail(self.play_gifs_by_default);
        let _ = self.grid.update_item_thumbnail(index, row.id(), thumbnail);
    }

    /// Rebuilds the id -> index map. Must follow every mutation of
    /// `loaded_rows`, which is why it hangs off the grid sync below.
    fn reindex_rows(&mut self) {
        self.row_index.clear();
        self.row_index.reserve(self.rows().len());
        if let Some(rows) = self.rows.as_deref() {
            for (index, row) in rows.iter().enumerate() {
                let _ = self.row_index.insert(row.id().to_owned(), index);
            }
        }
    }

    /// Rebuilds the grid's item list and page status from `rows`.
    ///
    /// Visible-range follow-ups are dropped because every caller re-derives
    /// metadata and thumbnail effects from state after the sync. Hover
    /// follow-ups are dropped too — a hover retargeted by rows shifting
    /// under a stationary cursor misses one GIF play/pause transition and
    /// self-corrects on the next cursor move.
    fn sync_grid_items(&mut self) {
        // Every mutation of `rows` is already followed by a sync, so the
        // reindex rides along rather than needing its own call sites.
        self.reindex_rows();
        let items = self
            .rows()
            .iter()
            .map(|row| row.to_grid_item(self.play_gifs_by_default, self.download_count_formatter))
            .collect::<Vec<_>>();
        let _ = self.grid.set_items(items);
        let _ = self
            .grid
            .set_page_status(matches!(self.load_status, LoadStatus::Loading), false);
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
    Error(UiError),
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
