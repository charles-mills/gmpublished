use std::{
    collections::HashSet,
    ops::Range,
    path::PathBuf,
    time::{Duration, Instant},
};

use crate::bridge::tasks::TaskId;
use crate::bridge::ui_error::UiError;
use crate::bridge::{
    domain::{
        PublishedFileId, SearchFullBatch, SearchFullBatchMode, SearchFullHits, SearchFullRequest,
        SearchHit, SearchItem, SearchItemSource, SearchMode, SearchQuickBatch, SearchQuickRequest,
        SearchRequestKey, SearchSession, WorkshopMetadata, workshop_url::workshop_item_url,
    },
    tasks::BackendServices,
};
use iced::{animation::Easing, widget::image};

use crate::media::{thumbnail_demand, thumbnail_worker::ThumbnailInput};
use crate::theme::{Tokens, motion};

pub const QUICK_SEARCH_DEBOUNCE: Duration = Duration::from_millis(100);
pub const RESULT_ROW_HEIGHT: f32 = 70.0;

const VIRTUAL_ROW_OVERSCAN: usize = 4;
const SEARCH_THUMBNAIL_MAX_EDGE: u32 = 256;
const THUMBNAIL_OWNER_LABEL: &str = "Search";
const PALETTE_CLOSED_SCALE: f32 = 0.98;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowSource {
    InstalledAddons,
    InstalledAddonFile,
    MyWorkshop,
    SteamWorkshop,
}

impl RowSource {
    const fn label_key(self) -> &'static str {
        match self {
            Self::InstalledAddons => "installed-addons",
            Self::InstalledAddonFile => "search-source-file",
            Self::MyWorkshop => "my-workshop",
            Self::SteamWorkshop => "search-source-steam-workshop",
        }
    }
}

#[derive(Clone, Debug)]
pub struct State {
    input: String,
    mode: SearchMode,
    session: SearchSession,
    rows: Vec<Row>,
    expanded: bool,
    visible: bool,
    presence: motion::Presence<bool>,
    pending_quick: Option<SearchQuickRequest>,
    thumbnail_generation: u64,
    metadata_generation: u64,
    metadata_in_flight: HashSet<PublishedFileId>,
    metadata_finished: HashSet<PublishedFileId>,
    scroll_offset: f32,
}

impl Default for State {
    fn default() -> Self {
        let tokens = Tokens::dark();
        Self {
            input: String::new(),
            mode: SearchMode::Addons,
            session: SearchSession::default(),
            rows: Vec::new(),
            expanded: false,
            visible: false,
            presence: motion::asymmetric(
                false,
                tokens.motion.modal_enter_duration(),
                tokens.motion.modal_exit_duration(),
                Easing::EaseOut,
            ),
            pending_quick: None,
            thumbnail_generation: 0,
            metadata_generation: 0,
            metadata_in_flight: HashSet::new(),
            metadata_finished: HashSet::new(),
            scroll_offset: 0.0,
        }
    }
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        self.input == other.input
            && self.rows == other.rows
            && self.mode == other.mode
            && self.expanded == other.expanded
            && self.visible == other.visible
            && self.presence == other.presence
            && self.pending_quick == other.pending_quick
            && self.thumbnail_generation == other.thumbnail_generation
            && self.metadata_generation == other.metadata_generation
            && self.metadata_in_flight == other.metadata_in_flight
            && self.metadata_finished == other.metadata_finished
            && self.scroll_offset == other.scroll_offset
            && self.session.generation() == other.session.generation()
            && self.session.query() == other.session.query()
            && self.session.loading() == other.session.loading()
            && self.session.has_more() == other.session.has_more()
            && self.session.active_full_task() == other.session.active_full_task()
            && self.session.full_replace_pending() == other.session.full_replace_pending()
    }
}

impl State {
    pub(crate) fn input(&self) -> &str {
        &self.input
    }

    pub(crate) const fn mode(&self) -> SearchMode {
        self.mode
    }

    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub(crate) const fn loading(&self) -> bool {
        self.session.loading()
    }

    pub(crate) const fn has_more(&self) -> bool {
        self.session.has_more()
    }

    pub(crate) fn should_begin_full_search(&self) -> bool {
        self.expanded
            && self.query_active()
            && self.session.has_more()
            && self.session.active_full_task().is_none()
    }

    pub(crate) fn show_empty(&self) -> bool {
        self.query_active() && !self.loading() && self.rows.is_empty()
    }

    pub(crate) const fn palette_open(&self) -> bool {
        self.expanded
    }

    pub(crate) const fn palette_visible(&self) -> bool {
        self.visible
    }

    pub(crate) fn needs_motion_ticks(&self) -> bool {
        self.presence.needs_ticks()
    }

    pub(crate) fn opacity(&self, now: Instant) -> f32 {
        self.presence.interpolate(0.0, 1.0, now)
    }

    pub(crate) fn scale(&self, now: Instant) -> f32 {
        self.presence.interpolate(PALETTE_CLOSED_SCALE, 1.0, now)
    }

    pub(crate) fn dropdown_open(&self) -> bool {
        self.expanded
            && self.query_active()
            && (self.loading() || !self.rows.is_empty() || self.has_more() || self.show_empty())
    }

    pub(crate) fn query_active(&self) -> bool {
        !self.input.trim().is_empty()
    }

    pub(crate) fn edit_query(&mut self, input: String) -> QueryEditOutcome {
        if !self.expanded {
            if self.visible {
                return QueryEditOutcome {
                    quick_request: None,
                    cancel_task: None,
                };
            }
            self.expanded = true;
            self.visible = true;
            self.presence.snap(true);
        }

        self.input = input;
        // Editing keeps the palette open even when the input empties out
        // (e.g. backspacing over a typo'd first character) — an empty query
        // hides the results section, not the palette itself.
        let change = self.session.begin_query(&self.input, self.mode);
        self.replace_rows(Vec::new());
        self.pending_quick.clone_from(&change.quick_request);

        QueryEditOutcome {
            quick_request: change.quick_request,
            cancel_task: change.cancel_task,
        }
    }

    pub(crate) fn clear(&mut self) -> Option<TaskId> {
        self.input.clear();
        self.expanded = true;
        self.visible = true;
        self.replace_rows(Vec::new());
        self.pending_quick = None;
        self.session.clear().cancel_task
    }

    pub(crate) fn focus(&mut self, now: Instant) -> bool {
        if self.expanded {
            return false;
        }
        self.expanded = true;
        self.visible = true;
        self.presence.go(true, now);
        true
    }

    pub(crate) fn focus_mode(&mut self, mode: SearchMode, now: Instant) -> FocusModeOutcome {
        let mode_changed = self.mode != mode;
        let cancel_task = if mode_changed {
            self.mode = mode;
            self.input.clear();
            self.replace_rows(Vec::new());
            self.pending_quick = None;
            self.session.clear().cancel_task
        } else {
            None
        };
        let opened = self.focus(now);
        FocusModeOutcome {
            opened,
            mode_changed,
            cancel_task,
        }
    }

    /// Starts closing the palette. The clean-slate reset happens on the
    /// final animation tick so the input stays mounted through the fade.
    /// While closing, edits are ignored; this preserves the ⌘F leak guard.
    pub(crate) fn dismiss(&mut self, now: Instant) -> Option<TaskId> {
        if !self.expanded {
            return None;
        }
        let cancel_task = self.session.active_full_task();
        self.expanded = false;
        self.pending_quick = None;
        self.presence.go(false, now);
        cancel_task
    }

    /// Returns true when the close animation just settled and the palette
    /// fully reset — the caller should re-sweep thumbnail demands, since the
    /// fading rows kept theirs alive until this moment.
    pub(crate) fn tick_motion(&mut self, now: Instant) -> bool {
        if self.presence.tick(now) && !self.expanded && self.visible {
            self.reset_after_close();
            self.visible = false;
            return true;
        }
        false
    }

    fn reset_after_close(&mut self) {
        self.input.clear();
        self.replace_rows(Vec::new());
        self.pending_quick = None;
        let _clear = self.session.clear();
    }

    pub(crate) fn take_debounced_request(
        &mut self,
        request: &SearchQuickRequest,
    ) -> Option<SearchQuickRequest> {
        let current = self.pending_quick.as_ref()?;
        if current.key() != request.key()
            || !self
                .session
                .is_current(request.generation(), request.mode(), request.query())
        {
            return None;
        }

        self.pending_quick.take()
    }

    pub(crate) fn apply_quick_result(
        &mut self,
        key: &SearchRequestKey,
        result: Result<SearchQuickBatch, UiError>,
    ) -> bool {
        match result {
            Ok(batch) => {
                let Some(accepted) = self.session.accept_quick_batch(batch) else {
                    return false;
                };
                let (hits, _has_more) = accepted.into_parts();
                self.replace_rows(rows_from_hits(hits));
            }
            Err(error) => {
                if !self.session.fail_quick(key) {
                    return false;
                }
                self.replace_rows(Vec::new());
                log::warn!("quick search failed for `{}`: {error}", key.query());
            }
        }
        true
    }

    pub(crate) fn begin_full_search(&mut self, task_id: TaskId) -> Option<FullSearchStart> {
        let start = self.session.begin_full_search(task_id, self.mode)?;
        self.pending_quick = None;
        Some(FullSearchStart {
            request: start.request,
            cancel_task: start.cancel_task,
        })
    }

    pub(crate) fn apply_full_batch(&mut self, batch: SearchFullBatch) -> bool {
        let Some(accepted) = self.session.accept_full_batch(batch) else {
            return false;
        };
        let (mode, hits) = accepted.into_parts();

        match mode {
            SearchFullBatchMode::ReplaceQuickRows => {
                self.replace_rows(rows_from_full_hits(0, &hits));
            }
            SearchFullBatchMode::AppendRows => {
                let start = self.rows.len();
                self.rows.extend(rows_from_full_hits(start, &hits));
                self.thumbnail_generation = self.next_thumbnail_generation();
            }
        }
        true
    }

    pub(crate) fn finish_full_search(&mut self, request: &SearchFullRequest) -> bool {
        self.session.finish_full_search(request)
    }

    pub(crate) fn selection_for(&self, row_id: usize) -> Option<Selection> {
        self.rows.get(row_id).map(|row| {
            let mut action = row.action.clone();
            // The row's lazily-resolved thumbnail URL seeds the modal the
            // activation opens (GMA preview / Prepare Publish icon); rows are
            // constructed before URLs resolve, so it must be copied here.
            if let SelectionAction::InstalledAddon { preview_url, .. }
            | SelectionAction::MyWorkshop { preview_url, .. } = &mut action
            {
                preview_url.clone_from(&row.thumbnail_url);
            }
            Selection {
                title: row.title.clone(),
                action,
            }
        })
    }

    pub(crate) fn set_scroll_offset(&mut self, offset: f32) -> bool {
        let offset = finite_nonnegative(offset);
        if (self.scroll_offset - offset).abs() < f32::EPSILON {
            return false;
        }
        self.scroll_offset = offset;
        true
    }

    pub(crate) fn take_thumbnail_metadata_request(
        &mut self,
        viewport_height: f32,
    ) -> Option<(u64, Vec<PublishedFileId>)> {
        let ids = self.pending_thumbnail_metadata_ids(viewport_height);
        if ids.is_empty() {
            return None;
        }

        self.metadata_in_flight.extend(ids.iter().copied());
        Some((self.metadata_generation, ids))
    }

    fn pending_thumbnail_metadata_ids(&self, viewport_height: f32) -> Vec<PublishedFileId> {
        let mut ids = Vec::new();
        let (visible_range, prefetch_before, prefetch_after) =
            self.thumbnail_demand_ranges(viewport_height);
        for range in [visible_range, prefetch_before, prefetch_after] {
            for row in self.rows.get(range).unwrap_or_default() {
                if !matches!(row.thumbnail, RowThumbnail::Loading) || row.thumbnail_url.is_some() {
                    continue;
                }
                let Some(workshop_id) = row.workshop_id else {
                    continue;
                };
                if self.metadata_in_flight.contains(&workshop_id)
                    || self.metadata_finished.contains(&workshop_id)
                    || ids.contains(&workshop_id)
                {
                    continue;
                }
                ids.push(workshop_id);
            }
        }
        ids
    }

    pub(crate) fn finish_metadata_request(
        &mut self,
        generation: u64,
        item_ids: &[PublishedFileId],
        result: Result<MetadataResolution, UiError>,
    ) -> MetadataCompletion {
        if generation != self.metadata_generation {
            return MetadataCompletion::default();
        }

        for item_id in item_ids {
            self.metadata_in_flight.remove(item_id);
            self.metadata_finished.insert(*item_id);
        }

        match result {
            Ok(resolution) => MetadataCompletion {
                changed: self.apply_metadata_patches(&resolution.patches),
                stale_ids: resolution.stale_ids,
            },
            Err(error) => {
                log::warn!("search metadata lookup failed: {error}");
                MetadataCompletion {
                    changed: self.settle_unresolved_metadata(item_ids),
                    stale_ids: Vec::new(),
                }
            }
        }
    }

    pub(crate) fn apply_metadata_refresh(
        &mut self,
        generation: u64,
        item_ids: &[PublishedFileId],
        result: Result<Vec<MetadataPatch>, UiError>,
    ) -> bool {
        if generation != self.metadata_generation {
            return false;
        }

        match result {
            Ok(patches) => {
                let mut changed = self.apply_metadata_patches(&patches);
                changed |= self.settle_unresolved_metadata(item_ids);
                changed
            }
            Err(error) => {
                log::warn!("search metadata refresh failed: {error}");
                self.settle_unresolved_metadata(item_ids)
            }
        }
    }

    fn apply_metadata_patches(&mut self, patches: &[MetadataPatch]) -> bool {
        let mut changed = false;
        for row in &mut self.rows {
            for patch in patches {
                changed |= row.apply_metadata_patch(patch);
            }
        }
        changed
    }

    fn settle_unresolved_metadata(&mut self, item_ids: &[PublishedFileId]) -> bool {
        let mut changed = false;
        for row in &mut self.rows {
            if row
                .workshop_id
                .is_some_and(|workshop_id| item_ids.contains(&workshop_id))
            {
                changed |= row.settle_without_metadata();
            }
        }
        changed
    }

    pub(crate) fn thumbnail_demands(&self, viewport_height: f32) -> thumbnail_demand::DemandSet {
        // The closing palette keeps its rows on screen while fading out, so
        // their thumbnails must stay demanded until the animation settles —
        // releasing them at dismiss time blanks the images mid-fade.
        let closing_with_rows = self.visible && !self.expanded && !self.rows.is_empty();
        if !self.dropdown_open() && !closing_with_rows {
            return thumbnail_demand::DemandSet::empty(thumbnail_owner());
        }

        let (visible_range, prefetch_before, prefetch_after) =
            self.thumbnail_demand_ranges(viewport_height);
        let demands =
            self.thumbnail_demands_for_range(visible_range, thumbnail_demand::Priority::VisibleRow)
                .chain(self.thumbnail_demands_for_range(
                    prefetch_before,
                    thumbnail_demand::Priority::Prefetch,
                ))
                .chain(self.thumbnail_demands_for_range(
                    prefetch_after,
                    thumbnail_demand::Priority::Prefetch,
                ))
                .collect();

        thumbnail_demand::DemandSet {
            owner: thumbnail_owner(),
            generation: self.thumbnail_generation,
            replace: thumbnail_demand::ReplaceMode::Owner,
            demands,
        }
    }

    fn thumbnail_demand_ranges(
        &self,
        viewport_height: f32,
    ) -> (Range<usize>, Range<usize>, Range<usize>) {
        let visible_range = self.row_range(viewport_height, 0);
        let (prefetch_before, prefetch_after) =
            thumbnail_demand::prefetch_ranges(visible_range.clone(), self.rows.len());
        (visible_range, prefetch_before, prefetch_after)
    }

    fn thumbnail_demands_for_range(
        &self,
        range: Range<usize>,
        priority: thumbnail_demand::Priority,
    ) -> impl Iterator<Item = thumbnail_demand::Demand> + '_ {
        self.rows
            .get(range)
            .unwrap_or_default()
            .iter()
            .filter_map(move |row| row.thumbnail_demand(priority))
    }

    pub(crate) fn apply_thumbnail_delivery(
        &mut self,
        delivery: &thumbnail_demand::Delivery,
    ) -> bool {
        if delivery.owner != thumbnail_owner() || delivery.generation != self.thumbnail_generation {
            return false;
        }

        let mut changed = false;
        for row in &mut self.rows {
            changed |= row.apply_thumbnail_delivery(delivery);
        }
        changed
    }

    pub(crate) fn invalidate_ready_thumbnails(&mut self) -> bool {
        let mut changed = false;
        for row in &mut self.rows {
            changed |= row.invalidate_ready_thumbnail();
        }
        if changed {
            self.thumbnail_generation = self.next_thumbnail_generation();
        }
        changed
    }

    pub(crate) fn virtual_rows(&self, viewport_height: f32) -> VirtualRows {
        let range = self.row_range(viewport_height, VIRTUAL_ROW_OVERSCAN);
        let top_padding = range.start as f32 * RESULT_ROW_HEIGHT;
        let bottom_padding = self.rows.len().saturating_sub(range.end) as f32 * RESULT_ROW_HEIGHT;

        VirtualRows {
            range,
            top_padding,
            bottom_padding,
        }
    }

    fn replace_rows(&mut self, rows: Vec<Row>) {
        self.rows = rows;
        self.scroll_offset = 0.0;
        self.thumbnail_generation = self.next_thumbnail_generation();
        self.metadata_generation = self.next_metadata_generation();
        self.metadata_in_flight.clear();
        self.metadata_finished.clear();
    }

    fn next_thumbnail_generation(&self) -> u64 {
        self.thumbnail_generation.wrapping_add(1).max(1)
    }

    fn next_metadata_generation(&self) -> u64 {
        self.metadata_generation.wrapping_add(1).max(1)
    }

    fn row_range(&self, viewport_height: f32, overscan: usize) -> Range<usize> {
        if self.rows.is_empty() || viewport_height <= 0.0 {
            return 0..0;
        }

        let top = finite_nonnegative(self.scroll_offset);
        let bottom = top + finite_nonnegative(viewport_height);
        let visible_start = (top / RESULT_ROW_HEIGHT).floor() as usize;
        let visible_end = (bottom / RESULT_ROW_HEIGHT).ceil() as usize;
        let start = visible_start.saturating_sub(overscan);
        let end = visible_end.saturating_add(overscan);
        start.min(self.rows.len())..end.min(self.rows.len())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VirtualRows {
    pub(crate) range: Range<usize>,
    pub(crate) top_padding: f32,
    pub(crate) bottom_padding: f32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryEditOutcome {
    pub(crate) quick_request: Option<SearchQuickRequest>,
    pub(crate) cancel_task: Option<TaskId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FocusModeOutcome {
    pub(crate) opened: bool,
    pub(crate) mode_changed: bool,
    pub(crate) cancel_task: Option<TaskId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FullSearchStart {
    pub(crate) request: SearchFullRequest,
    pub(crate) cancel_task: Option<TaskId>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataCompletion {
    pub(crate) changed: bool,
    pub(crate) stale_ids: Vec<PublishedFileId>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataResolution {
    pub(crate) patches: Vec<MetadataPatch>,
    pub(crate) stale_ids: Vec<PublishedFileId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataPatch {
    workshop_id: PublishedFileId,
    preview_url: Option<String>,
}

impl MetadataPatch {
    fn from_metadata(metadata: &WorkshopMetadata) -> Self {
        Self {
            workshop_id: metadata.id,
            preview_url: metadata
                .preview_url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(str::to_owned),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(workshop_id: PublishedFileId, preview_url: Option<&str>) -> Self {
        Self {
            workshop_id,
            preview_url: preview_url.map(str::to_owned),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Selection {
    pub(crate) title: String,
    pub(crate) action: SelectionAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionAction {
    InstalledAddon {
        path: PathBuf,
        workshop_id: Option<PublishedFileId>,
        preview_url: Option<String>,
    },
    MyWorkshop {
        workshop_id: PublishedFileId,
        title: String,
        tags: Vec<String>,
        preview_url: Option<String>,
    },
    SteamWorkshop {
        workshop_id: PublishedFileId,
    },
    InstalledAddonFile {
        addon_path: PathBuf,
        addon_title: String,
        workshop_id: Option<PublishedFileId>,
        entry_path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    id: usize,
    title: String,
    source: RowSource,
    association: String,
    workshop_id: Option<PublishedFileId>,
    thumbnail_url: Option<String>,
    thumbnail: RowThumbnail,
    action: SelectionAction,
}

impl Row {
    pub(crate) const fn id(&self) -> usize {
        self.id
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn association(&self) -> &str {
        &self.association
    }

    pub(crate) const fn thumbnail(&self) -> &RowThumbnail {
        &self.thumbnail
    }

    pub(crate) fn source_label_key(&self) -> &'static str {
        self.source.label_key()
    }

    fn thumbnail_demand(
        &self,
        priority: thumbnail_demand::Priority,
    ) -> Option<thumbnail_demand::Demand> {
        if !matches!(self.thumbnail, RowThumbnail::Loading) {
            return None;
        }
        let preview_url = self.thumbnail_url.as_deref()?.trim();
        if preview_url.is_empty() {
            return None;
        }

        Some(thumbnail_demand::Demand {
            id: thumbnail_demand::DemandId::new(self.id.to_string()),
            input: ThumbnailInput::from_url(preview_url),
            logical_max_edge: SEARCH_THUMBNAIL_MAX_EDGE,
            priority,
        })
    }

    fn apply_thumbnail_delivery(&mut self, delivery: &thumbnail_demand::Delivery) -> bool {
        if delivery.id.as_str().parse::<usize>() != Ok(self.id) {
            return false;
        }

        self.thumbnail = match &delivery.result {
            thumbnail_demand::DeliveryResult::Ready(ready) => {
                RowThumbnail::Ready(ready.handle().clone())
            }
            // Search rows keep their spinner rather than a blurred placeholder.
            thumbnail_demand::DeliveryResult::Placeholder(_) => return false,
            thumbnail_demand::DeliveryResult::Failed { .. } => RowThumbnail::Dead,
        };
        true
    }

    fn apply_metadata_patch(&mut self, patch: &MetadataPatch) -> bool {
        if self.workshop_id != Some(patch.workshop_id) {
            return false;
        }

        if self.thumbnail_url == patch.preview_url {
            if patch.preview_url.is_none() && matches!(self.thumbnail, RowThumbnail::Loading) {
                self.thumbnail = RowThumbnail::Dead;
                return true;
            }
            return false;
        }

        self.thumbnail_url.clone_from(&patch.preview_url);
        self.thumbnail = if self.thumbnail_url.is_some() {
            RowThumbnail::Loading
        } else {
            RowThumbnail::Dead
        };
        true
    }

    fn settle_without_metadata(&mut self) -> bool {
        if self.thumbnail_url.is_some() || !matches!(self.thumbnail, RowThumbnail::Loading) {
            return false;
        }

        self.thumbnail = RowThumbnail::Dead;
        true
    }

    fn invalidate_ready_thumbnail(&mut self) -> bool {
        if !matches!(self.thumbnail, RowThumbnail::Ready(_)) {
            return false;
        }

        self.thumbnail = if self
            .thumbnail_url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
        {
            RowThumbnail::Loading
        } else {
            RowThumbnail::Dead
        };
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowThumbnail {
    Loading,
    Dead,
    Ready(image::Handle),
}

fn rows_from_hits(hits: Vec<SearchHit>) -> Vec<Row> {
    hits.into_iter()
        .enumerate()
        .map(|(index, hit)| row_from_search_item(index, &hit.item))
        .collect()
}

fn rows_from_full_hits(start: usize, hits: &SearchFullHits) -> Vec<Row> {
    let mut index = start;
    hits.map_rows(|_score, item| {
        let row = row_from_search_item(index, item);
        index += 1;
        row
    })
}

fn row_from_search_item(index: usize, item: &SearchItem) -> Row {
    let title = item.label.clone();
    match &item.source {
        SearchItemSource::InstalledAddons(path, workshop_id) => Row {
            id: index,
            title,
            source: RowSource::InstalledAddons,
            association: path.to_string_lossy().into_owned(),
            workshop_id: *workshop_id,
            thumbnail_url: None,
            thumbnail: thumbnail_for_workshop_id(*workshop_id),
            action: SelectionAction::InstalledAddon {
                path: path.clone(),
                workshop_id: *workshop_id,
                preview_url: None,
            },
        },
        SearchItemSource::InstalledAddonFile {
            addon_path,
            addon_title,
            workshop_id,
            entry_path,
            ..
        } => Row {
            id: index,
            title,
            source: RowSource::InstalledAddonFile,
            association: format!("{entry_path} - {addon_title}"),
            workshop_id: *workshop_id,
            thumbnail_url: None,
            thumbnail: thumbnail_for_workshop_id(*workshop_id),
            action: SelectionAction::InstalledAddonFile {
                addon_path: addon_path.clone(),
                addon_title: addon_title.clone(),
                workshop_id: *workshop_id,
                entry_path: entry_path.clone(),
            },
        },
        SearchItemSource::MyWorkshop(id) => Row {
            id: index,
            title: title.clone(),
            source: RowSource::MyWorkshop,
            association: workshop_item_url(*id),
            workshop_id: Some(*id),
            thumbnail_url: None,
            thumbnail: RowThumbnail::Loading,
            action: SelectionAction::MyWorkshop {
                workshop_id: *id,
                title,
                tags: my_workshop_tags_from_terms(&item.terms, *id),
                preview_url: None,
            },
        },
        SearchItemSource::WorkshopItem(id) => Row {
            id: index,
            title,
            source: RowSource::SteamWorkshop,
            association: workshop_item_url(*id),
            workshop_id: Some(*id),
            thumbnail_url: None,
            thumbnail: RowThumbnail::Loading,
            action: SelectionAction::SteamWorkshop { workshop_id: *id },
        },
    }
}

fn thumbnail_for_workshop_id(workshop_id: Option<PublishedFileId>) -> RowThumbnail {
    if workshop_id.is_some() {
        RowThumbnail::Loading
    } else {
        RowThumbnail::Dead
    }
}

fn thumbnail_owner() -> thumbnail_demand::Owner {
    thumbnail_demand::Owner::AddonGrid(THUMBNAIL_OWNER_LABEL)
}

fn finite_nonnegative(value: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

pub fn resolve_metadata(ctx: &BackendServices, item_ids: &[PublishedFileId]) -> MetadataResolution {
    let (metadata, stale_ids) = ctx.resolve_workshop_metadata(item_ids);
    MetadataResolution {
        patches: metadata.iter().map(MetadataPatch::from_metadata).collect(),
        stale_ids,
    }
}

pub fn refresh_metadata(
    ctx: &BackendServices,
    item_ids: &[PublishedFileId],
) -> Result<Vec<MetadataPatch>, UiError> {
    Ok(ctx
        .refresh_workshop_metadata(item_ids)?
        .iter()
        .map(MetadataPatch::from_metadata)
        .collect())
}

fn my_workshop_tags_from_terms(terms: &[impl AsRef<str>], id: PublishedFileId) -> Vec<String> {
    let id_term = id.to_string();
    terms
        .iter()
        .map(AsRef::as_ref)
        .filter(|term| *term != id_term.as_str())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::bridge::domain::{
        SearchFullBatch, SearchItem, SearchItemSource, SearchQuickBatch, SearchQuickCarry,
    };

    use super::*;

    #[test]
    fn editing_query_starts_debounced_quick_request() {
        let mut state = State::default();

        let outcome = state.edit_query("wire".to_owned());

        assert_eq!(
            outcome
                .quick_request
                .as_ref()
                .map(crate::bridge::domain::SearchQuickRequest::query),
            Some("wire")
        );
        assert!(state.dropdown_open());
    }

    #[test]
    fn dismiss_resets_palette_to_a_clean_slate() {
        let mut state = State::default();
        let _outcome = state.edit_query("wire".to_owned());
        let started = Instant::now();

        let _cancel = state.dismiss(started);
        assert!(!state.dropdown_open());
        assert!(!state.palette_open());
        assert!(state.palette_visible());
        assert_eq!(state.input(), "wire");

        state.tick_motion(started + Duration::from_millis(500));
        assert!(!state.palette_visible());
        assert_eq!(state.input(), "");
        assert!(state.rows().is_empty());

        assert!(state.focus(started + Duration::from_millis(600)));
        assert!(state.palette_open());
        assert!(!state.dropdown_open());
    }

    #[test]
    fn palette_entrance_starts_visibly_instead_of_snapping_open() {
        let mut state = State::default();
        let started = Instant::now();

        assert!(state.focus(started));
        assert!((0.05..0.25).contains(&state.opacity(started + Duration::from_millis(16))));
    }

    #[test]
    fn command_f_spam_cannot_accumulate_leaked_characters() {
        // Regression: iced's text_input tracks modifiers in widget state, so
        // a remounted palette input treats the closing ⌘F as a plain "f"
        // edit. Dismissal must discard it so toggling can never accumulate.
        let mut state = State::default();
        let started = Instant::now();

        for index in 0..3 {
            let now = started + Duration::from_millis(index * 600);
            assert!(state.focus(now));
            let _outcome = state.edit_query(format!("{}f", state.input()));
            let _cancel = state.dismiss(now + Duration::from_millis(1));
            let _ignored = state.edit_query(format!("{}f", state.input()));
            state.tick_motion(now + Duration::from_millis(500));
        }

        assert!(state.focus(started + Duration::from_millis(2_000)));
        assert_eq!(state.input(), "");
    }

    #[test]
    fn reopening_mid_close_cancels_pending_reset_and_keeps_input() {
        let mut state = State::default();
        let started = Instant::now();

        assert!(state.focus(started));
        let _outcome = state.edit_query("wire".to_owned());
        let _cancel = state.dismiss(started + Duration::from_millis(1));

        assert!(!state.palette_open());
        assert!(state.palette_visible());

        assert!(state.focus(started + Duration::from_millis(50)));
        state.tick_motion(started + Duration::from_millis(500));

        assert!(state.palette_open());
        assert!(state.palette_visible());
        assert_eq!(state.input(), "wire");
    }

    #[test]
    fn emptying_the_query_keeps_the_palette_open() {
        let mut state = State::default();
        assert!(state.focus(Instant::now()));

        let _outcome = state.edit_query("w".to_owned());
        let _outcome = state.edit_query(String::new());

        assert!(state.palette_open());
        assert!(!state.dropdown_open());
        assert_eq!(state.input(), "");
    }

    #[test]
    fn closing_palette_keeps_thumbnails_demanded_until_the_fade_settles() {
        let mut state = State::default();
        let started = Instant::now();
        assert!(state.focus(started));
        let request = state.edit_query("alpha".to_owned()).quick_request.unwrap();
        let batch = SearchQuickBatch::new(
            request.key().clone(),
            vec![my_workshop_hit("Alpha", 42)],
            false,
            SearchQuickCarry::default(),
        );
        assert!(state.apply_quick_result(request.key(), Ok(batch)));
        let generation = state.metadata_generation;
        assert!(state.apply_metadata_refresh(
            generation,
            &[PublishedFileId::new(42).expect("test fixture ids are always nonzero")],
            Ok(vec![MetadataPatch::for_test(
                PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
                Some("https://example.test/alpha.png")
            )]),
        ));
        assert!(!state.thumbnail_demands(600.0).demands.is_empty());

        // Rows fade out with the palette; their thumbnails stay demanded.
        let _cancel = state.dismiss(started + Duration::from_millis(1));
        assert!(!state.thumbnail_demands(600.0).demands.is_empty());

        // Fully settled: everything resets and the demands empty out.
        assert!(state.tick_motion(started + Duration::from_millis(500)));
        assert!(state.thumbnail_demands(600.0).demands.is_empty());
    }

    #[test]
    fn my_workshop_selection_carries_the_resolved_thumbnail_url() {
        let mut state = State::default();
        let request = state.edit_query("alpha".to_owned()).quick_request.unwrap();
        let batch = SearchQuickBatch::new(
            request.key().clone(),
            vec![my_workshop_hit("Alpha", 42)],
            false,
            SearchQuickCarry::default(),
        );
        assert!(state.apply_quick_result(request.key(), Ok(batch)));

        let generation = state.metadata_generation;
        assert!(state.apply_metadata_refresh(
            generation,
            &[PublishedFileId::new(42).expect("test fixture ids are always nonzero")],
            Ok(vec![MetadataPatch::for_test(
                PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
                Some("https://example.test/alpha.png")
            )]),
        ));

        let Some(SelectionAction::MyWorkshop { preview_url, .. }) =
            state.selection_for(0).map(|selection| selection.action)
        else {
            panic!("expected a My Workshop selection");
        };
        assert_eq!(
            preview_url.as_deref(),
            Some("https://example.test/alpha.png")
        );
    }

    #[test]
    fn stale_quick_result_is_ignored() {
        let mut state = State::default();
        let first = state.edit_query("first".to_owned()).quick_request.unwrap();
        let second = state.edit_query("second".to_owned()).quick_request.unwrap();

        let batch = SearchQuickBatch::new(
            first.key().clone(),
            vec![hit("First", 1)],
            false,
            SearchQuickCarry::default(),
        );

        assert!(!state.apply_quick_result(first.key(), Ok(batch)));
        assert_eq!(state.rows().len(), 0);
        assert_eq!(state.take_debounced_request(&second), Some(second));
    }

    #[test]
    fn quick_result_maps_rows_and_selection() {
        let mut state = State::default();
        let request = state.edit_query("alpha".to_owned()).quick_request.unwrap();
        let batch = SearchQuickBatch::new(
            request.key().clone(),
            vec![hit("Alpha", 42)],
            true,
            SearchQuickCarry::default(),
        );

        assert!(state.apply_quick_result(request.key(), Ok(batch)));

        assert_eq!(state.rows()[0].title(), "Alpha");
        assert!(state.has_more());
        assert_eq!(
            state.selection_for(0).map(|selection| selection.action),
            Some(SelectionAction::SteamWorkshop {
                workshop_id: PublishedFileId::new(42).expect("test fixture ids are always nonzero")
            })
        );
    }

    #[test]
    fn full_search_batches_replace_quick_rows_then_append() {
        let mut state = State::default();
        let request = state.edit_query("alpha".to_owned()).quick_request.unwrap();
        let quick = SearchQuickBatch::new(
            request.key().clone(),
            vec![hit("Quick", 1)],
            true,
            SearchQuickCarry::default(),
        );
        assert!(state.apply_quick_result(request.key(), Ok(quick)));

        let start = state
            .begin_full_search(TaskId::from_raw(88))
            .expect("full search start");
        let first = SearchFullBatch::new(
            start.request.key().clone(),
            start.request.task_id(),
            0,
            vec![hit("Full One", 2)],
        );
        assert!(state.apply_full_batch(first));
        assert_eq!(
            state.rows().iter().map(Row::title).collect::<Vec<_>>(),
            vec!["Full One"]
        );

        let second = SearchFullBatch::new(
            start.request.key().clone(),
            start.request.task_id(),
            1,
            vec![hit("Full Two", 3)],
        );
        assert!(state.apply_full_batch(second));
        assert_eq!(
            state.rows().iter().map(Row::title).collect::<Vec<_>>(),
            vec!["Full One", "Full Two"]
        );

        assert!(state.finish_full_search(&start.request));
        assert!(!state.loading());
        assert!(!state.has_more());
    }

    #[test]
    fn virtual_rows_render_visible_window_with_overscan_spacers() {
        let mut state = State::default();
        let request = state.edit_query("alpha".to_owned()).quick_request.unwrap();
        let batch = SearchQuickBatch::new(
            request.key().clone(),
            hits(20),
            false,
            SearchQuickCarry::default(),
        );
        assert!(state.apply_quick_result(request.key(), Ok(batch)));

        let _changed = state.set_scroll_offset(RESULT_ROW_HEIGHT * 10.0);
        let window = state.virtual_rows(RESULT_ROW_HEIGHT * 3.0);

        assert_eq!(window.range, 6..17);
        assert_eq!(window.top_padding, RESULT_ROW_HEIGHT * 6.0);
        assert_eq!(window.bottom_padding, RESULT_ROW_HEIGHT * 3.0);
    }

    #[test]
    fn thumbnail_metadata_and_demands_include_prefetch_window() {
        let mut state = State::default();
        let request = state.edit_query("alpha".to_owned()).quick_request.unwrap();
        let batch = SearchQuickBatch::new(
            request.key().clone(),
            hits(20),
            false,
            SearchQuickCarry::default(),
        );
        assert!(state.apply_quick_result(request.key(), Ok(batch)));

        let _changed = state.set_scroll_offset(RESULT_ROW_HEIGHT * 10.0);
        let viewport_height = RESULT_ROW_HEIGHT * 3.0;
        let (generation, ids) = state
            .take_thumbnail_metadata_request(viewport_height)
            .expect("thumbnail metadata request");

        assert_eq!(
            ids,
            [11, 12, 13, 7, 8, 9, 10, 14, 15, 16, 17, 18, 19]
                .into_iter()
                .map(|id| PublishedFileId::new(id).expect("test fixture ids are always nonzero"))
                .collect::<Vec<_>>()
        );

        let patches = ids
            .iter()
            .map(|id| {
                MetadataPatch::for_test(*id, Some(&format!("https://example.invalid/{id}.png")))
            })
            .collect();
        let completion = state.finish_metadata_request(
            generation,
            &ids,
            Ok(MetadataResolution {
                patches,
                stale_ids: Vec::new(),
            }),
        );
        assert!(completion.changed);

        let demands = state.thumbnail_demands(viewport_height);
        assert_eq!(demands.demands.len(), 13);
        assert_eq!(
            demands
                .demands
                .iter()
                .map(|demand| demand.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "10", "11", "12", "6", "7", "8", "9", "13", "14", "15", "16", "17", "18"
            ]
        );
        assert!(
            demands.demands[..3]
                .iter()
                .all(|demand| demand.priority == thumbnail_demand::Priority::VisibleRow)
        );
        assert!(
            demands.demands[3..]
                .iter()
                .all(|demand| demand.priority == thumbnail_demand::Priority::Prefetch)
        );
    }

    #[test]
    fn metadata_miss_keeps_row_loading_until_refresh_settles() {
        let mut state = State::default();
        let request = state.edit_query("alpha".to_owned()).quick_request.unwrap();
        let batch = SearchQuickBatch::new(
            request.key().clone(),
            vec![hit("Alpha", 42)],
            false,
            SearchQuickCarry::default(),
        );
        assert!(state.apply_quick_result(request.key(), Ok(batch)));

        let (generation, ids) = state
            .take_thumbnail_metadata_request(RESULT_ROW_HEIGHT)
            .expect("thumbnail metadata request");
        let completion = state.finish_metadata_request(
            generation,
            &ids,
            Ok(MetadataResolution {
                patches: Vec::new(),
                stale_ids: ids.clone(),
            }),
        );

        assert!(!completion.changed);
        assert!(matches!(state.rows()[0].thumbnail(), RowThumbnail::Loading));
        assert!(state.apply_metadata_refresh(
            generation,
            &ids,
            Ok(vec![MetadataPatch::for_test(
                PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
                Some("https://example.test/alpha.png")
            )]),
        ));
        assert_eq!(state.thumbnail_demands(RESULT_ROW_HEIGHT).demands.len(), 1);
    }

    #[test]
    fn file_search_hit_builds_archive_entry_selection() {
        let row = row_from_search_item(
            0,
            &SearchItem::new(
                SearchItemSource::InstalledAddonFile {
                    addon_path: PathBuf::from("/tmp/riverden.gma"),
                    addon_title: "Riverden".to_owned(),
                    workshop_id: Some(
                        PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
                    ),
                    entry_path: "maps/rp_riverden_v1a.bsp".to_owned(),
                    size_bytes: 42,
                    crc32: 77,
                },
                "rp_riverden_v1a.bsp",
                ["maps/rp_riverden_v1a.bsp", "Riverden"],
                0,
            ),
        );

        assert_eq!(row.title(), "rp_riverden_v1a.bsp");
        assert_eq!(row.source_label_key(), "search-source-file");
        assert!(matches!(row.thumbnail(), RowThumbnail::Loading));
        assert_eq!(
            row.action,
            SelectionAction::InstalledAddonFile {
                addon_path: PathBuf::from("/tmp/riverden.gma"),
                addon_title: "Riverden".to_owned(),
                workshop_id: Some(
                    PublishedFileId::new(123).expect("test fixture ids are always nonzero")
                ),
                entry_path: "maps/rp_riverden_v1a.bsp".to_owned(),
            }
        );
    }

    fn hits(count: u64) -> Vec<SearchHit> {
        (0..count)
            .map(|index| hit(&format!("Item {index}"), index + 1))
            .collect()
    }

    fn hit(label: &str, workshop_id: u64) -> SearchHit {
        SearchHit {
            score: 1,
            item: SearchItem::new(
                SearchItemSource::WorkshopItem(
                    PublishedFileId::new(workshop_id).expect("test fixture ids are always nonzero"),
                ),
                label,
                Vec::<String>::new(),
                0,
            ),
        }
    }

    fn my_workshop_hit(label: &str, workshop_id: u64) -> SearchHit {
        SearchHit {
            score: 1,
            item: SearchItem::new(
                SearchItemSource::MyWorkshop(
                    PublishedFileId::new(workshop_id).expect("test fixture ids are always nonzero"),
                ),
                label,
                Vec::<String>::new(),
                0,
            ),
        }
    }
}
