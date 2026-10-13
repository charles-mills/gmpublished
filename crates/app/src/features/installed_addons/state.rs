use std::collections::{HashMap, HashSet};
use std::mem;
use std::path::{Path, PathBuf};
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

use crate::widgets::grid_rows::{CardId, GridRow};

#[derive(Debug)]
pub struct State {
    route_visible: bool,
    pane: grid_rows::GridPane,
    generation: Generation,
    watch_gmod_dir: Option<PathBuf>,
    watch_degraded: bool,
    watch_retry_attempted: bool,
    watch_arm_epoch: Generation,
    /// Give the grid every row at once so its scrollbar reflects the full library.
    library: Library,
    /// workshop_id -> indices into `rows` sharing that id. Rebuilt any time
    /// `rows` is structurally replaced.
    workshop_index: HashMap<PublishedFileId, Vec<usize>>,
    metadata_in_flight: HashSet<PublishedFileId>,
    metadata_finished: HashSet<PublishedFileId>,
    /// Park transient failures until route entry or Steam reconnects. Do not mark
    /// them finished; that would prevent retries.
    metadata_failed: HashSet<PublishedFileId>,
    pending_preview: Option<PreviewTarget>,
    pending_context_menu: Option<ContextMenuRequest>,
    /// Pending relative scroll adjustment. Hidden routes update the mirror;
    /// re-entry restores Iced's offset from it.
    pending_scroll_anchor: Option<f32>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            route_visible: false,
            pane: grid_rows::GridPane::new(Settings::default().ui.play_gifs_by_default),
            generation: Generation::INITIAL,
            watch_gmod_dir: None,
            watch_degraded: false,
            watch_retry_attempted: false,
            watch_arm_epoch: Generation::INITIAL,
            library: Library::Unscanned,
            workshop_index: HashMap::new(),
            metadata_in_flight: HashSet::new(),
            metadata_finished: HashSet::new(),
            metadata_failed: HashSet::new(),
            pending_preview: None,
            pending_context_menu: None,
            pending_scroll_anchor: None,
        }
    }
}

impl State {
    pub(crate) fn apply_ui_metrics(&mut self, tokens: &crate::theme::Tokens) -> Vec<super::Effect> {
        let follow_ups = self.pane.grid_mut().set_metrics(&tokens.layout());
        super::update::apply_ui_metrics(self, follow_ups)
    }

    #[cfg(test)]
    pub(crate) const fn is_route_visible(&self) -> bool {
        self.route_visible
    }

    pub(crate) const fn library(&self) -> &Library {
        &self.library
    }

    pub(crate) fn row_count(&self) -> usize {
        self.rows().len()
    }

    fn rows(&self) -> &[Row] {
        self.library.rows()
    }

    #[cfg(feature = "debug")]
    pub(crate) fn hide_addon(
        &mut self,
        workshop_id: Option<PublishedFileId>,
        path: Option<&std::path::Path>,
    ) -> bool {
        let matches = |row: &Row| {
            path.is_some_and(|path| row.id().as_str() == path.to_string_lossy())
                || workshop_id.is_some_and(|id| row.workshop_id() == Some(id))
        };
        let Library::Scanned(rows) = &mut self.library else {
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

    pub(crate) const fn watch_arm_epoch(&self) -> Generation {
        self.watch_arm_epoch
    }

    pub(crate) fn watch_gmod_dir(&self) -> Option<&Path> {
        self.watch_gmod_dir.as_deref()
    }

    pub(crate) fn set_watch_gmod_dir(&mut self, gmod_dir: Option<PathBuf>) {
        if self.watch_gmod_dir == gmod_dir {
            return;
        }
        self.watch_gmod_dir = gmod_dir;
        self.watch_degraded = false;
        self.watch_retry_attempted = false;
    }

    pub(crate) const fn grid(&self) -> &addon_grid::State {
        self.pane.grid()
    }

    fn row_by_id(&self, id: &CardId) -> Option<&Row> {
        self.rows().get(self.pane.index_of(id)?)
    }

    pub(crate) fn workshop_id_for_card(&self, id: &CardId) -> Option<PublishedFileId> {
        self.row_by_id(id).and_then(Row::workshop_id)
    }

    pub(crate) fn drag_thumbnail_for_card(&self, id: &CardId) -> Option<image::Handle> {
        self.row_by_id(id).and_then(Row::drag_thumbnail)
    }

    pub(crate) fn thumbnail_demands(&self) -> thumbnail_demand::DemandSet {
        if !self.route_visible {
            return model::empty_thumbnail_demands();
        }

        self.pane
            .thumbnail_demands(self.rows(), self.generation, model::thumbnail_owner())
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
        let Some(row) = self.library.rows_mut().get_mut(index) else {
            return false;
        };
        if !row.apply_thumbnail_delivery(delivery.generation, delivery, self.generation) {
            return false;
        }

        self.refresh_item_thumbnail(index);
        true
    }

    pub(crate) fn invalidate_ready_thumbnails(&mut self) -> bool {
        self.pane
            .invalidate_ready_thumbnails(self.library.rows_mut())
    }

    pub(crate) fn release_offscreen_thumbnails(&mut self) -> bool {
        self.pane
            .release_offscreen_thumbnails(self.library.rows_mut())
    }

    pub(crate) fn has_active_animations(&self) -> bool {
        self.route_visible && self.pane.has_active_animations(self.rows())
    }

    pub(crate) fn needs_card_motion_ticks(&self) -> bool {
        self.route_visible && self.pane.grid().needs_visible_card_ticks()
    }

    pub(super) fn tick_visible_card_motion(&mut self, now: Instant) {
        if self.route_visible {
            for message in self.pane.grid_mut().tick_visible_card_motion(now) {
                if let addon_grid::Message::CardHoverChanged(id, hovered) = message {
                    self.set_card_hovered(&id, hovered);
                }
            }
        }
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

    pub(super) const fn grid_mut(&mut self) -> &mut addon_grid::State {
        self.pane.grid_mut()
    }

    pub(super) fn enter_route(&mut self) {
        self.route_visible = true;
        self.rearm_watch_on_route_entry();
        let _ = self.retry_failed_metadata();
        if matches!(self.library, Library::Unscanned | Library::Failed(_)) {
            self.library = Library::Scanning;
            // The grid is empty, so reconciliation cannot emit a follow-up event.
            let _follow_ups = self.pane.grid_mut().set_page_status(true, false);
        }
    }

    pub(super) fn exit_route(&mut self) {
        self.route_visible = false;
        self.pane.clear_animation_clock();
        self.pending_preview = None;
        self.pending_context_menu = None;
        self.watch_retry_attempted = false;
        // Items, scroll and viewport are unchanged; has_more_pages = false cannot widen
        // the visible range.
        let _follow_ups = self.pane.grid_mut().set_page_status(false, false);
    }

    pub(super) fn refresh_started(&mut self, reason: crate::bridge::library::LibraryRefreshReason) {
        log::info!(
            "installed addons refresh started: {reason:?}, route_visible {}, discovered {}",
            self.route_visible,
            self.row_count(),
        );
        if reason.loud() {
            self.begin_loud_refresh();
        } else if self.route_visible && !self.library.is_scanned() {
            self.library = Library::Scanning;
            // Discovery is pending and the grid is empty.
            let _follow_ups = self.pane.grid_mut().set_page_status(true, false);
        }
    }

    fn begin_loud_refresh(&mut self) {
        self.library = if self.route_visible {
            Library::Scanning
        } else {
            Library::Unscanned
        };
        self.workshop_index.clear();
        self.metadata_in_flight.clear();
        self.metadata_finished.clear();
        self.metadata_failed.clear();
        self.pane.clear_animation_clock();
        self.pending_preview = None;
        self.pending_context_menu = None;
        // The following SnapshotPushed recalculates metadata and thumbnail demand from scratch.
        let _follow_ups = self.pane.grid_mut().set_items(Vec::new());
        let _follow_ups = self
            .pane
            .grid_mut()
            .set_page_status(matches!(self.library, Library::Scanning), false);
    }

    pub(super) fn apply_snapshot(
        &mut self,
        reason: crate::bridge::library::LibraryRefreshReason,
        result: Result<Vec<Row>, UiError>,
    ) {
        self.generation.bump();

        let incoming = result.as_ref().ok().map(Vec::len);
        if reason.loud() || !self.library.is_scanned() {
            self.apply_loud_discovery(result);
        } else {
            self.apply_quiet_discovery(result);
        }
        log::info!(
            "installed addons snapshot applied: {reason:?} incoming {incoming:?} -> \
             rows {}, grid items {}, library {}, route_visible {}",
            self.row_count(),
            self.pane.grid().items_len(),
            self.library.kind(),
            self.route_visible,
        );
    }

    pub(super) fn apply_watch_armed(&mut self, degraded: bool) {
        self.watch_degraded = degraded;
        if !degraded {
            self.watch_retry_attempted = false;
        }
    }

    /// Retry failed watch roots once per route entry. Bumping the epoch rebuilds
    /// the subscription and re-arms every root.
    fn rearm_watch_on_route_entry(&mut self) {
        if self.watch_degraded && !self.watch_retry_attempted {
            self.watch_retry_attempted = true;
            self.watch_arm_epoch.bump();
        }
    }

    fn apply_loud_discovery(&mut self, result: Result<Vec<Row>, UiError>) {
        self.metadata_in_flight.clear();
        self.metadata_finished.clear();
        self.metadata_failed.clear();
        self.pending_preview = None;
        self.pending_context_menu = None;

        match result {
            Ok(rows) => {
                self.workshop_index = build_workshop_index(&rows);
                self.library = Library::Scanned(rows);
            }
            Err(error) => {
                self.workshop_index.clear();
                self.library = Library::Failed(error);
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

        let mut old_by_id = mem::replace(&mut self.library, Library::Unscanned)
            .into_rows()
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
        self.library = Library::Scanned(merged_rows);
        self.metadata_in_flight.clear();
        self.metadata_finished
            .retain(|workshop_id| unchanged_workshop_ids.contains(workshop_id));
        self.metadata_failed
            .retain(|workshop_id| unchanged_workshop_ids.contains(workshop_id));
        self.pending_preview = None;
        self.pending_context_menu = None;
        self.sync_grid_items();
    }

    pub(super) fn workshop_language_changed(&mut self) {
        self.generation.bump();
        self.metadata_in_flight.clear();
        self.metadata_finished.clear();
        self.metadata_failed.clear();
        self.pending_preview = None;
        self.pending_context_menu = None;
    }

    /// Use the same prefetch window as thumbnail demand, which needs these URLs.
    /// Visible ids come first; Steam queries them in batches of 50.
    pub(super) fn take_visible_metadata_request(
        &mut self,
    ) -> Option<(Generation, Vec<PublishedFileId>)> {
        if !self.route_visible || self.rows().is_empty() {
            return None;
        }

        let mut seen = HashSet::new();
        let mut item_ids = Vec::new();
        let visible = self.pane.visible_row_range(self.rows().len());
        let (before, after) = thumbnail_demand::prefetch_ranges(visible.clone(), self.rows().len());
        for range in [visible, after, before] {
            for row in self.rows().get(range).unwrap_or_default() {
                let Some(item_id) = row.workshop_id() else {
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
                // Park failed IDs until a retry point; do not mark them finished
                // or retry them on every update.
                log::debug!("installed addons metadata lookup failed: {error}");
                self.metadata_failed.extend(item_ids.iter().copied());
                return None;
            }
        };

        self.metadata_finished.extend(item_ids.iter().copied());
        self.apply_metadata_patches(generation, &resolution.patches);
        (!resolution.stale_ids.is_empty()).then_some((generation, resolution.stale_ids))
    }

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
                // Cache hits were marked finished before refresh. Clear that
                // flag so failed refreshes can retry.
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
            self.pane.clear_animation_clock();
            return false;
        }

        let mut rows = std::mem::take(&mut self.library);
        let changed =
            self.pane
                .tick_visible_animations(rows.rows_mut(), now, thumbnail_animation_tick());
        self.library = rows;
        changed
    }

    pub(super) fn take_preview_target(&mut self, id: &CardId) -> Option<PreviewTarget> {
        let target = self.row_by_id(id)?.preview_target()?;
        self.pending_preview = Some(target.clone());
        Some(target)
    }

    pub(super) fn take_context_menu(
        &mut self,
        id: &CardId,
        position: iced::Point,
    ) -> Option<ContextMenuRequest> {
        let mut request = self.row_by_id(id)?.context_menu()?;
        request.position = position;
        self.pending_context_menu = Some(request.clone());
        Some(request)
    }

    pub(super) fn set_card_hovered(&mut self, id: &CardId, hovered: bool) -> bool {
        let Some(index) = self.pane.index_of(id) else {
            return false;
        };
        let Some(row) = self.library.rows_mut().get_mut(index) else {
            return false;
        };
        if !row.set_thumbnail_play_requested(hovered) || !row.holds_animation() {
            return false;
        }
        let thumbnail = row.card_thumbnail(self.pane.play_gifs_by_default());
        let _ = self
            .pane
            .grid_mut()
            .update_item_thumbnail(index, row.id(), thumbnail);
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
                if let Some(row) = self.library.rows_mut().get_mut(index)
                    && row.apply_metadata_patch(patch)
                {
                    changed_indices.push(index);
                }
            }
        }
        if changed_indices.is_empty() {
            return;
        }

        // Patch only the changed cards. Row IDs and row_index remain valid.
        let updates = changed_indices
            .iter()
            .filter_map(|&index| {
                self.rows().get(index).map(|row| {
                    (
                        index,
                        row.to_grid_item(self.pane.play_gifs_by_default(), self.pane.formatter()),
                    )
                })
            })
            .collect();
        for message in self.pane.grid_mut().patch_items(updates) {
            // The caller recalculates demand and metadata. Keep the scroll delta;
            // several patches can arrive before it is drained.
            if let addon_grid::Message::ScrollAnchored(delta) = message {
                *self.pending_scroll_anchor.get_or_insert(0.0) += delta;
            }
        }
    }

    pub(super) fn take_scroll_anchor(&mut self) -> Option<f32> {
        self.pending_scroll_anchor.take()
    }

    fn refresh_item_thumbnail(&mut self, index: usize) {
        let Some(row) = self.library.rows().get(index) else {
            return;
        };
        let thumbnail = row.card_thumbnail(self.pane.play_gifs_by_default());
        let _ = self
            .pane
            .grid_mut()
            .update_item_thumbnail(index, row.id(), thumbnail);
    }

    /// Callers recalculate metadata and thumbnail demand after syncing.
    /// A shifted hover is picked up on the next cursor move.
    fn sync_grid_items(&mut self) {
        let library = std::mem::take(&mut self.library);
        let _follow_ups = self.pane.sync_items(library.rows(), []);
        self.library = library;
        let _follow_ups = self
            .pane
            .grid_mut()
            .set_page_status(matches!(self.library, Library::Scanning), false);
    }
}

const fn thumbnail_animation_tick() -> Duration {
    crate::media::thumbnail_animation::ANIMATION_TICK_INTERVAL
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Library {
    #[default]
    Unscanned,
    Scanning,
    Scanned(Vec<Row>),
    Failed(UiError),
}

impl Library {
    fn rows(&self) -> &[Row] {
        match self {
            Self::Scanned(rows) => rows,
            Self::Unscanned | Self::Scanning | Self::Failed(_) => &[],
        }
    }

    fn rows_mut(&mut self) -> &mut [Row] {
        match self {
            Self::Scanned(rows) => rows,
            Self::Unscanned | Self::Scanning | Self::Failed(_) => &mut [],
        }
    }

    const fn is_scanned(&self) -> bool {
        matches!(self, Self::Scanned(_))
    }

    fn into_rows(self) -> Vec<Row> {
        match self {
            Self::Scanned(rows) => rows,
            Self::Unscanned | Self::Scanning | Self::Failed(_) => Vec::new(),
        }
    }

    const fn kind(&self) -> &'static str {
        match self {
            Self::Unscanned => "unscanned",
            Self::Scanning => "scanning",
            Self::Scanned(_) => "scanned",
            Self::Failed(_) => "failed",
        }
    }
}

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
