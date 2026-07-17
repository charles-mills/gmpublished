use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use gmpublished_backend::error_key::keys;
use iced::widget::{canvas, image};
use iced::{Point, Size};

use crate::bridge::domain::PublishedFileId;
use crate::bridge::library::LibrarySnapshot;
use crate::bridge::size_analyzer::{
    Rect, SizeAnalyzerAddon, SizeAnalyzerError, TreemapBounds, TreemapLayout,
    analyze_installed_addons,
};
use crate::bridge::ui_error::UiError;
use crate::features::context_menu;
use crate::media::{
    size_analyzer_render::{RgbaColor, SizeAnalyzerLabelSprite, tag_color},
    thumbnail_demand::{self, DeliveryResult},
    thumbnail_worker::ThumbnailInput,
};

const ADDON_THUMBNAIL_MAX_EDGE: u32 = 256;
const ADDON_THUMBNAIL_MIN_EDGE: u32 = 16;
const SPATIAL_COLUMNS: usize = 16;
const SPATIAL_ROWS: usize = 10;
const LABEL_SCALE_BUCKET: f32 = 0.5;
const MAX_LABEL_SCALE: f32 = 3.0;
const ANALYZER_THUMBNAIL_GENERATION: u64 = 0;

/// State owned by the Size Analyzer route.
#[derive(Clone, Debug, PartialEq)]
pub struct State {
    route_visible: bool,
    load_status: LoadStatus,
    snapshot: Option<LibrarySnapshot>,
    snapshot_error: Option<String>,
    scale_factor: f32,
    pending_viewport: Option<RenderViewport>,
    last_completed_viewport: Option<RenderViewport>,
    projection_key: Option<LayoutProjectionKey>,
    layout: Option<Arc<TreemapLayout>>,
    /// Workshop ids of the current layout's leaves, projected once when the
    /// layout is installed so the demand, retain, and delivery paths never
    /// re-walk `leaf_rects()`.
    layout_workshop_ids: HashSet<PublishedFileId>,
    /// Snapshot epoch whose missing preview URLs were already handed to a
    /// resolve worker.
    preview_resolve_epoch: Option<u64>,
    preview_urls: HashMap<PublishedFileId, String>,
    thumbnail_plan: Vec<(PublishedFileId, u32)>,
    thumbnails: HashMap<PublishedFileId, ThumbnailTile>,
    /// Blurred ThumbHash stand-ins painted until the real tile decodes; kept
    /// separate so `thumbnail_demands` still requests the sharp image.
    placeholder_thumbnails: HashMap<PublishedFileId, ThumbnailTile>,
    failed_thumbnails: HashSet<PublishedFileId>,
    labels: Vec<SizeAnalyzerLabelSprite>,
    layers: TreemapLayers,
    hover: Option<HoverProbe>,
    /// Tag whose big label is omitted while the cursor hovers inside it.
    hidden_tag: Option<String>,
    pending_context_menu: Option<ContextMenuRequest>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            route_visible: false,
            load_status: LoadStatus::Idle,
            snapshot: None,
            snapshot_error: None,
            scale_factor: 1.0,
            pending_viewport: None,
            last_completed_viewport: None,
            projection_key: None,
            layout: None,
            layout_workshop_ids: HashSet::new(),
            preview_resolve_epoch: None,
            preview_urls: HashMap::new(),
            thumbnail_plan: Vec::new(),
            thumbnails: HashMap::new(),
            placeholder_thumbnails: HashMap::new(),
            failed_thumbnails: HashSet::new(),
            labels: Vec::new(),
            layers: TreemapLayers::default(),
            hover: None,
            hidden_tag: None,
            pending_context_menu: None,
        }
    }
}

impl State {
    #[cfg(test)]
    pub(crate) const fn is_route_visible(&self) -> bool {
        self.route_visible
    }

    #[cfg(test)]
    pub(crate) const fn projection_key_for_test(&self) -> Option<LayoutProjectionKey> {
        self.projection_key
    }

    pub(crate) const fn load_status(&self) -> &LoadStatus {
        &self.load_status
    }

    #[cfg(feature = "debug")]
    pub(crate) fn hide_addon(
        &mut self,
        workshop_id: Option<PublishedFileId>,
        path: Option<&Path>,
    ) -> bool {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return false;
        };
        let addons = snapshot
            .addons
            .iter()
            .filter(|addon| {
                path.is_none_or(|path| addon.path != path)
                    && workshop_id.is_none_or(|id| addon.workshop_id != Some(id))
            })
            .cloned()
            .collect::<Vec<_>>();
        if addons.len() == snapshot.addons.len() {
            return false;
        }
        let snapshot = LibrarySnapshot {
            addons: Arc::from(addons),
            epoch: snapshot.epoch.wrapping_add(1),
        };
        self.apply_snapshot(Ok(Some(snapshot)));
        true
    }

    pub(crate) fn layout(&self) -> Option<&TreemapLayout> {
        self.layout.as_deref()
    }

    #[cfg(test)]
    pub(crate) const fn thumbnail_tiles(&self) -> &HashMap<PublishedFileId, ThumbnailTile> {
        &self.thumbnails
    }

    /// The tile to paint for a leaf: the real thumbnail if decoded, otherwise
    /// its ThumbHash placeholder.
    pub(crate) fn tile_for(&self, id: PublishedFileId) -> Option<&ThumbnailTile> {
        self.thumbnails
            .get(&id)
            .or_else(|| self.placeholder_thumbnails.get(&id))
    }

    #[cfg(test)]
    pub(crate) const fn failed_thumbnail_ids(&self) -> &HashSet<PublishedFileId> {
        &self.failed_thumbnails
    }

    pub(crate) fn labels(&self) -> &[SizeAnalyzerLabelSprite] {
        &self.labels
    }

    pub(crate) fn hidden_tag(&self) -> Option<&str> {
        self.hidden_tag.as_deref()
    }

    pub(crate) const fn layers(&self) -> &TreemapLayers {
        &self.layers
    }

    pub(crate) const fn hover(&self) -> Option<&HoverProbe> {
        self.hover.as_ref()
    }

    /// Logical size of the treemap surface backing the current layout, in the
    /// same coordinate space as the hover rects. Reuses the last completed
    /// render viewport (no extra sensor), so it is only set once a layout has
    /// been drawn.
    pub(crate) fn surface_size(&self) -> Option<Size> {
        self.last_completed_viewport
            .map(RenderViewport::logical_size)
    }

    #[cfg(test)]
    pub(crate) fn preview_url_for_test(&self, workshop_id: PublishedFileId) -> Option<&str> {
        self.preview_urls.get(&workshop_id).map(String::as_str)
    }

    pub(crate) fn preview_target(&self) -> Option<PreviewTarget> {
        self.hover.as_ref().map(HoverProbe::preview_target)
    }

    pub(super) fn enter_route(&mut self) {
        self.route_visible = true;
        self.project_current();
    }

    pub(super) fn exit_route(&mut self) {
        self.route_visible = false;
        self.hover = None;
        self.pending_context_menu = None;
    }

    pub(super) fn note_viewport(&mut self, size: Size) {
        let Some(viewport) = RenderViewport::from_size(size) else {
            return;
        };
        if self.pending_viewport == Some(viewport) {
            return;
        }

        self.pending_viewport = Some(viewport);
        self.project_current();
    }

    pub(super) fn apply_snapshot(&mut self, result: Result<Option<LibrarySnapshot>, UiError>) {
        match result {
            Ok(Some(snapshot)) => {
                self.snapshot_error = None;
                // A refresh bumps the epoch on every run, including ones that
                // rediscover byte-for-byte the same library. Keeping the
                // existing snapshot (and its epoch) for content-identical
                // refreshes leaves the projection, thumbnails, and resolved
                // preview URLs untouched instead of re-projecting and
                // re-resolving for nothing.
                if self
                    .snapshot
                    .as_ref()
                    .is_some_and(|current| snapshots_content_identical(current, &snapshot))
                {
                    self.project_current();
                    return;
                }
                self.retain_snapshot_workshop_ids(&snapshot);
                self.snapshot = Some(snapshot);
            }
            Ok(None) => {
                self.snapshot = None;
                let error = UiError::new(keys::GMOD_PATH_MISSING).to_string();
                self.snapshot_error = Some(error.clone());
                self.clear_projection(LoadStatus::Error(error));
            }
            Err(error) => {
                self.snapshot = None;
                let error = error.to_string();
                self.snapshot_error = Some(error.clone());
                self.clear_projection(LoadStatus::Error(error));
            }
        }

        self.project_current();
    }

    /// Applies the window scale factor; returns true when the bucketed label
    /// raster scale changed and label bitmaps should re-rasterize.
    pub(crate) fn set_scale_factor(&mut self, scale_factor: f32) -> bool {
        let bucket = bucketed_label_scale(scale_factor);
        if bucket.to_bits() == self.scale_factor.to_bits() {
            return false;
        }

        self.scale_factor = bucket;
        true
    }

    /// Reacts to a label-scale bucket change by re-rasterizing label bitmaps
    /// for the current layout.
    pub(super) fn scale_factor_changed(&mut self) {
        if !self.route_visible {
            return;
        }
        let Some(layout) = self.layout.clone() else {
            return;
        };
        self.labels = rasterize_labels_for(&layout, self.scale_factor);
        let _invalidation = self.invalidate_layers(LayerInvalidation::LABELS);
    }

    pub(super) fn apply_preview_urls(
        &mut self,
        urls: HashMap<PublishedFileId, String>,
    ) -> LayerInvalidation {
        if urls.is_empty() {
            return LayerInvalidation::NONE;
        }

        let mut changed = false;
        for (workshop_id, preview_url) in urls {
            if !self.layout_workshop_ids.contains(&workshop_id) {
                continue;
            }
            changed |= self.preview_urls.get(&workshop_id) != Some(&preview_url);
            self.preview_urls.insert(workshop_id, preview_url);
        }

        if changed {
            self.invalidate_layers(LayerInvalidation::THUMBNAILS)
        } else {
            LayerInvalidation::NONE
        }
    }

    /// Re-derives the hovered cell from a cursor point. `sync_hidden_tag`
    /// (run once per message, see its doc comment) is what actually
    /// invalidates the labels layer when the hover changes; this method just
    /// updates `self.hover`.
    pub(super) fn update_hover_at(&mut self, point: Point) {
        if !self.route_visible || !matches!(self.load_status, LoadStatus::Ready) {
            self.clear_hover();
            return;
        }

        let Some(layout) = &self.layout else {
            self.clear_hover();
            return;
        };
        let Some(viewport) = self.last_completed_viewport else {
            self.clear_hover();
            return;
        };

        let layout_x = f64::from(point.x) * viewport.scale_x();
        let layout_y = f64::from(point.y) * viewport.scale_y();
        self.hover = layout.hit_test_addon(layout_x, layout_y).map(|hit| {
            let preview_url = hit
                .addon
                .workshop_id
                .and_then(|workshop_id| self.preview_urls.get(&workshop_id))
                .cloned();
            HoverProbe::from_hit(hit.addon, hit.tag, hit.rect, viewport, preview_url)
        });
    }

    pub(super) fn clear_hover(&mut self) {
        self.hover = None;
    }

    pub(super) fn request_context_menu(
        &mut self,
        position: iced::Point,
    ) -> Option<ContextMenuRequest> {
        let mut menu = ContextMenuRequest::from_hover(self.hover.as_ref()?);
        menu.position = position;
        self.pending_context_menu = Some(menu.clone());
        Some(menu)
    }

    pub(crate) fn thumbnail_demands(&self) -> thumbnail_demand::DemandSet {
        if !self.route_visible || !matches!(self.load_status, LoadStatus::Ready) {
            return thumbnail_demand::DemandSet::empty(thumbnail_owner());
        }

        if self.layout.is_none() {
            return thumbnail_demand::DemandSet::empty(thumbnail_owner());
        }

        let demands = self
            .thumbnail_plan
            .iter()
            .filter(|(workshop_id, _)| !self.thumbnails.contains_key(workshop_id))
            .filter(|(workshop_id, _)| !self.failed_thumbnails.contains(workshop_id))
            .filter_map(|(workshop_id, max_edge)| {
                Some(thumbnail_demand::Demand {
                    id: workshop_demand_id(*workshop_id),
                    input: ThumbnailInput::from_url(self.preview_urls.get(workshop_id)?.clone()),
                    logical_max_edge: *max_edge,
                    priority: thumbnail_demand::Priority::SizeAnalyzer,
                })
            })
            .collect();

        thumbnail_demand::DemandSet {
            owner: thumbnail_owner(),
            generation: ANALYZER_THUMBNAIL_GENERATION,
            replace: thumbnail_demand::ReplaceMode::Owner,
            demands,
        }
    }

    pub(crate) fn apply_thumbnail_delivery(
        &mut self,
        delivery: &thumbnail_demand::Delivery,
    ) -> LayerInvalidation {
        if delivery.owner != thumbnail_owner()
            || !self.route_visible
            || !matches!(self.load_status, LoadStatus::Ready)
        {
            return LayerInvalidation::NONE;
        }

        let Some(workshop_id) = parse_workshop_demand_id(&delivery.id) else {
            return LayerInvalidation::NONE;
        };
        if !self.preview_urls.contains_key(&workshop_id) {
            return LayerInvalidation::NONE;
        }

        match &delivery.result {
            DeliveryResult::Ready(ready) => {
                let metadata = ready.metadata();
                if metadata.width == 0 || metadata.height == 0 {
                    self.failed_thumbnails.insert(workshop_id);
                    return self.mark_thumbnails_dirty();
                }
                self.failed_thumbnails.remove(&workshop_id);
                self.placeholder_thumbnails.remove(&workshop_id);
                self.thumbnails.insert(
                    workshop_id,
                    ThumbnailTile {
                        handle: ready.handle().clone(),
                        width: metadata.width,
                        height: metadata.height,
                    },
                );
                self.mark_thumbnails_dirty()
            }
            DeliveryResult::Placeholder(placeholder) => {
                // Only fill an empty cell; the real tile, once present, wins.
                if placeholder.width() == 0
                    || placeholder.height() == 0
                    || self.thumbnails.contains_key(&workshop_id)
                    || self.placeholder_thumbnails.contains_key(&workshop_id)
                {
                    return LayerInvalidation::NONE;
                }
                self.placeholder_thumbnails.insert(
                    workshop_id,
                    ThumbnailTile {
                        handle: placeholder.handle().clone(),
                        width: placeholder.width(),
                        height: placeholder.height(),
                    },
                );
                self.mark_thumbnails_dirty()
            }
            DeliveryResult::Failed { .. } => {
                self.placeholder_thumbnails.remove(&workshop_id);
                self.failed_thumbnails.insert(workshop_id);
                self.mark_thumbnails_dirty()
            }
        }
    }

    /// Records a thumbnail arrival without clearing the canvas cache yet, so a
    /// burst of deliveries re-records the thumbnail layer once — on the next
    /// draw, via [`Self::flush_thumbnail_invalidation`] — instead of once per
    /// delivery.
    fn mark_thumbnails_dirty(&self) -> LayerInvalidation {
        self.layers.mark_thumbnails_dirty();
        LayerInvalidation::THUMBNAILS
    }

    /// Clears the thumbnail cache once if any delivery marked it dirty since
    /// the last draw. Called from the canvas program before the thumbnail
    /// layer re-records; returns the coalesced invalidation for tests.
    pub(crate) fn flush_thumbnail_invalidation(&self) -> LayerInvalidation {
        if self.layers.take_thumbnails_dirty() {
            self.invalidate_layers(LayerInvalidation::THUMBNAILS)
        } else {
            LayerInvalidation::NONE
        }
    }

    pub(crate) fn invalidate_ready_thumbnails(&mut self) -> LayerInvalidation {
        if self.thumbnails.is_empty() {
            return LayerInvalidation::NONE;
        }

        self.thumbnails.clear();
        self.invalidate_layers(LayerInvalidation::THUMBNAILS)
    }

    /// True when a cell's thumbnail may still arrive: a preview URL is known
    /// and its delivery has not failed. Undeliverable cells (local addons,
    /// unknown URLs, failed deliveries) show the dead placeholder instead of
    /// waiting forever.
    pub(crate) fn thumbnail_pending(&self, workshop_id: Option<PublishedFileId>) -> bool {
        workshop_id.is_some_and(|id| {
            self.preview_urls.contains_key(&id) && !self.failed_thumbnails.contains(&id)
        })
    }

    /// Re-derives which tag label the hover hides; invalidates the labels
    /// layer when it changes.
    ///
    /// Called once per message so every path that moves or clears hover
    /// (exit events, resize reflow, route exit) restores the label without
    /// any polling; same-tag cursor moves compare equal and do nothing.
    pub(super) fn sync_hidden_tag(&mut self) -> LayerInvalidation {
        let next = self.hover.as_ref().map(|hover| hover.tag.clone());
        if next == self.hidden_tag {
            return LayerInvalidation::NONE;
        }

        self.hidden_tag = next;
        self.invalidate_layers(LayerInvalidation::LABELS)
    }

    /// Routes every invalidation through `TreemapLayers::invalidate`, the
    /// single place the caches are touched, and echoes the set for callers
    /// and tests.
    fn invalidate_layers(&self, invalidation: LayerInvalidation) -> LayerInvalidation {
        self.layers.invalidate(invalidation);
        invalidation
    }

    /// Workshop ids missing preview URLs for the current layout, handed out
    /// once per snapshot epoch. Layout leaves depend only on the snapshot,
    /// so one resolve per epoch covers every viewport; ids that can never
    /// cache (dead workshop items) must not redispatch live Steam refreshes
    /// on every resize event.
    pub(crate) fn take_pending_preview_url_ids(&mut self) -> Vec<PublishedFileId> {
        if !self.route_visible || !matches!(self.load_status, LoadStatus::Ready) {
            return Vec::new();
        }
        let Some(projection) = self.projection_key else {
            return Vec::new();
        };
        if self.preview_resolve_epoch == Some(projection.snapshot_epoch) {
            return Vec::new();
        }
        self.preview_resolve_epoch = Some(projection.snapshot_epoch);

        let mut ids = self
            .layout_workshop_ids
            .iter()
            .copied()
            .filter(|id| !self.preview_urls.contains_key(id))
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    fn project_current(&mut self) {
        if !self.route_visible {
            return;
        }

        let Some(viewport) = self.pending_viewport else {
            self.load_status = LoadStatus::WaitingForViewport;
            return;
        };

        if let Some(error) = self.snapshot_error.clone() {
            self.clear_projection(LoadStatus::Error(error));
            return;
        }

        let Some(snapshot) = self.snapshot.clone() else {
            self.load_status = LoadStatus::Loading;
            return;
        };

        let projection = LayoutProjectionKey {
            snapshot_epoch: snapshot.epoch,
            dimensions: viewport.dimensions,
        };
        if self.projection_key == Some(projection) && matches!(self.load_status, LoadStatus::Ready)
        {
            return;
        }

        self.apply_layout_result(
            projection,
            viewport,
            analyze_installed_addons(&snapshot.addons, viewport.bounds()),
        );
    }

    fn apply_layout_result(
        &mut self,
        projection: LayoutProjectionKey,
        viewport: RenderViewport,
        result: Result<TreemapLayout, SizeAnalyzerError>,
    ) {
        self.hover = None;
        self.hidden_tag = None;
        self.pending_context_menu = None;

        match result {
            Ok(layout) => {
                self.last_completed_viewport = Some(viewport);
                self.projection_key = Some(projection);
                let layout = Arc::new(layout);
                let workshop_ids = layout_workshop_ids(&layout);
                self.retain_workshop_ids(&workshop_ids);
                self.layout_workshop_ids = workshop_ids;
                self.thumbnail_plan = spatial_thumbnail_plan(&layout);
                // Labels rasterize synchronously with the layout so every
                // frame of a live resize draws a coherent label set — the
                // category count is small and the raster cache absorbs
                // repeats.
                self.labels = rasterize_labels_for(&layout, self.scale_factor);
                self.layout = Some(layout);
                self.load_status = LoadStatus::Ready;
                let _invalidation = self.invalidate_layers(LayerInvalidation::ALL);
            }
            Err(SizeAnalyzerError::NoAddonsFound) => {
                self.clear_projection(LoadStatus::Empty);
            }
            Err(error) => {
                self.clear_projection(LoadStatus::Error(size_analyzer_error_key(&error)));
            }
        }
    }

    fn clear_projection(&mut self, status: LoadStatus) {
        self.last_completed_viewport = None;
        self.projection_key = None;
        self.layout = None;
        self.layout_workshop_ids.clear();
        self.thumbnail_plan.clear();
        self.preview_resolve_epoch = None;
        self.preview_urls.clear();
        self.thumbnails.clear();
        self.placeholder_thumbnails.clear();
        self.failed_thumbnails.clear();
        self.labels.clear();
        self.hover = None;
        self.hidden_tag = None;
        self.pending_context_menu = None;
        self.load_status = status;
        let _invalidation = self.invalidate_layers(LayerInvalidation::ALL);
    }

    fn retain_snapshot_workshop_ids(&mut self, snapshot: &LibrarySnapshot) {
        let ids = snapshot
            .addons
            .iter()
            .filter_map(|addon| addon.workshop_id)
            .collect::<HashSet<_>>();
        self.retain_workshop_ids(&ids);
    }

    fn retain_workshop_ids(&mut self, ids: &HashSet<PublishedFileId>) {
        self.preview_urls.retain(|id, _| ids.contains(id));
        self.thumbnails.retain(|id, _| ids.contains(id));
        self.placeholder_thumbnails.retain(|id, _| ids.contains(id));
        self.failed_thumbnails.retain(|id| ids.contains(id));
    }

    #[cfg(test)]
    pub(crate) fn last_layer_invalidation_for_test(&self) -> Option<LayerInvalidation> {
        self.layers.recorded.borrow().last().copied()
    }

    #[cfg(test)]
    pub(crate) fn layout_workshop_ids_for_test(&self) -> &HashSet<PublishedFileId> {
        &self.layout_workshop_ids
    }

    #[cfg(test)]
    pub(crate) fn thumbnail_invalidation_count_for_test(&self) -> usize {
        self.layers
            .recorded
            .borrow()
            .iter()
            .filter(|invalidation| invalidation.thumbnails)
            .count()
    }
}

/// Returns the label sprites with the hovered category's label removed.
pub fn visible_tag_labels<'a>(
    labels: &'a [SizeAnalyzerLabelSprite],
    hidden_tag: Option<&'a str>,
) -> impl Iterator<Item = &'a SizeAnalyzerLabelSprite> {
    labels
        .iter()
        .filter(move |label| hidden_tag != Some(label.text.as_str()))
}

/// Rasterizes the layout's tag labels through the shared label context.
/// Category counts are small and the (text, fitted-size, scale) cache
/// absorbs repeats, so this is sub-millisecond warm.
fn rasterize_labels_for(layout: &TreemapLayout, scale: f32) -> Vec<SizeAnalyzerLabelSprite> {
    crate::media::size_analyzer_render::with_shared_label_context(|context| {
        context.rasterize_layout_labels(layout, scale)
    })
}

fn layout_workshop_ids(layout: &TreemapLayout) -> HashSet<PublishedFileId> {
    layout
        .leaf_rects()
        .into_iter()
        .filter_map(|leaf| leaf.addon.workshop_id)
        .collect()
}

fn spatial_thumbnail_plan(layout: &TreemapLayout) -> Vec<(PublishedFileId, u32)> {
    let mut tiles = HashMap::<PublishedFileId, (Rect, u32)>::new();
    for leaf in layout.leaf_rects() {
        let Some(id) = leaf.addon.workshop_id else {
            continue;
        };
        let edge = analyzer_thumbnail_edge(leaf.rect);
        tiles
            .entry(id)
            .and_modify(|(rect, max_edge)| {
                *max_edge = (*max_edge).max(edge);
                if leaf.rect.width * leaf.rect.height > rect.width * rect.height {
                    *rect = leaf.rect;
                }
            })
            .or_insert((leaf.rect, edge));
    }

    let mut buckets = (0..SPATIAL_COLUMNS * SPATIAL_ROWS)
        .map(|_| Vec::new())
        .collect::<Vec<Vec<(PublishedFileId, Rect, u32)>>>();

    for (id, (rect, edge)) in tiles {
        let column = (((rect.x + rect.width / 2.0) / layout.bounds.width) * SPATIAL_COLUMNS as f64)
            .floor()
            .clamp(0.0, (SPATIAL_COLUMNS - 1) as f64) as usize;
        let row = (((rect.y + rect.height / 2.0) / layout.bounds.height) * SPATIAL_ROWS as f64)
            .floor()
            .clamp(0.0, (SPATIAL_ROWS - 1) as f64) as usize;
        buckets[row * SPATIAL_COLUMNS + column].push((id, rect, edge));
    }

    for bucket in &mut buckets {
        bucket.sort_by(|left, right| {
            (left.1.width * left.1.height)
                .total_cmp(&(right.1.width * right.1.height))
                .then_with(|| right.0.cmp(&left.0))
        });
    }

    let mut plan = Vec::new();
    loop {
        let mut added = false;
        for bucket in &mut buckets {
            if let Some((id, _, edge)) = bucket.pop() {
                plan.push((id, edge));
                added = true;
            }
        }
        if !added {
            return plan;
        }
    }
}

fn analyzer_thumbnail_edge(rect: Rect) -> u32 {
    (rect.width.max(rect.height).ceil() as u32)
        .clamp(ADDON_THUMBNAIL_MIN_EDGE, ADDON_THUMBNAIL_MAX_EDGE)
        .next_power_of_two()
        .min(ADDON_THUMBNAIL_MAX_EDGE)
}

/// Epoch differences alone do not count as a content change; everything
/// else does. Full addon equality is deliberately conservative — the
/// projection consumes titles, types, and tags (grouping and labels), not
/// just identity and size, so a narrower comparison would keep stale
/// labels after an in-place addon update.
fn snapshots_content_identical(a: &LibrarySnapshot, b: &LibrarySnapshot) -> bool {
    a.addons == b.addons
}

/// Renders the same two outputs as before `SizeAnalyzerError` gained a
/// `HasErrorKey` impl: `NoAddonsFound` is unreachable here (handled as
/// `LoadStatus::Empty` above), and `InvalidBounds` renders its own Display
/// with no key prefix — a generic `UiError::from(&error).to_string()` would
/// prepend `ERR_UNKNOWN:`, which the `size-analyzer-error` template has never
/// interpolated.
fn size_analyzer_error_key(error: &SizeAnalyzerError) -> String {
    match error {
        SizeAnalyzerError::NoAddonsFound => keys::NO_ADDONS_FOUND.as_str().to_owned(),
        SizeAnalyzerError::InvalidBounds { .. } => error.to_string(),
    }
}

/// A delivered thumbnail's shared texture handle plus its pixel dimensions.
///
/// The handle is the same one the grids display, so the GPU uploads each
/// thumbnail once ever; the treemap references it with zero pixel copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThumbnailTile {
    pub(crate) handle: image::Handle,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Which cached canvas layers an event invalidated.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LayerInvalidation {
    /// Tag-region and addon-cell fills must re-record.
    pub(crate) geometry: bool,
    /// Thumbnail cover-draws and dead placeholders must re-record.
    pub(crate) thumbnails: bool,
    /// Tag label bitmap draws must re-record.
    pub(crate) labels: bool,
}

impl LayerInvalidation {
    pub(crate) const NONE: Self = Self {
        geometry: false,
        thumbnails: false,
        labels: false,
    };
    pub(crate) const ALL: Self = Self {
        geometry: true,
        thumbnails: true,
        labels: true,
    };
    pub(crate) const THUMBNAILS: Self = Self {
        geometry: false,
        thumbnails: true,
        labels: false,
    };
    pub(crate) const LABELS: Self = Self {
        geometry: false,
        thumbnails: false,
        labels: true,
    };
}

/// Cached draw commands for the three treemap canvas layers.
///
/// The caches are render-side memoization, not model state: `Clone` yields
/// fresh empty caches, `PartialEq` treats layer sets as always equal, and
/// `Debug` prints a marker, so `State` keeps its derives without caches
/// participating in comparisons.
pub struct TreemapLayers {
    geometry: canvas::Cache,
    thumbnails: canvas::Cache,
    labels: canvas::Cache,
    /// Set by a thumbnail arrival, drained on the next draw, so a burst of
    /// deliveries clears and re-records the thumbnail cache once.
    thumbnails_dirty: std::cell::Cell<bool>,
    #[cfg(test)]
    recorded: std::cell::RefCell<Vec<LayerInvalidation>>,
}

impl TreemapLayers {
    pub(crate) const fn geometry(&self) -> &canvas::Cache {
        &self.geometry
    }

    pub(crate) const fn thumbnails(&self) -> &canvas::Cache {
        &self.thumbnails
    }

    pub(crate) const fn labels(&self) -> &canvas::Cache {
        &self.labels
    }

    fn mark_thumbnails_dirty(&self) {
        self.thumbnails_dirty.set(true);
    }

    fn take_thumbnails_dirty(&self) -> bool {
        self.thumbnails_dirty.replace(false)
    }

    fn invalidate(&self, invalidation: LayerInvalidation) {
        #[cfg(test)]
        self.recorded.borrow_mut().push(invalidation);
        if invalidation.geometry {
            self.geometry.clear();
        }
        if invalidation.thumbnails {
            self.thumbnails.clear();
        }
        if invalidation.labels {
            self.labels.clear();
        }
    }
}

impl Default for TreemapLayers {
    fn default() -> Self {
        Self {
            geometry: canvas::Cache::new(),
            thumbnails: canvas::Cache::new(),
            labels: canvas::Cache::new(),
            thumbnails_dirty: std::cell::Cell::new(false),
            #[cfg(test)]
            recorded: std::cell::RefCell::new(Vec::new()),
        }
    }
}

impl Clone for TreemapLayers {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for TreemapLayers {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl fmt::Debug for TreemapLayers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TreemapLayers")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadStatus {
    Idle,
    WaitingForViewport,
    Loading,
    Ready,
    Empty,
    Error(String),
}

/// Resolves the tooltip/preview title for a hovered addon. The analyzer
/// carries no live workshop metadata, so `SizeAnalyzerAddon.title` (the
/// installed GMA title) is always used when non-blank, falling back to the
/// workshop id otherwise.
fn resolve_hover_title(addon: &SizeAnalyzerAddon) -> String {
    let title = addon.title.trim();
    if !title.is_empty() {
        return title.to_owned();
    }
    addon
        .workshop_id
        .map_or_else(String::new, |id| id.to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub struct HoverProbe {
    title: String,
    tag: String,
    path: PathBuf,
    size_bytes: u64,
    workshop_id: Option<PublishedFileId>,
    preview_url: Option<String>,
    rect_x: f32,
    rect_y: f32,
    rect_width: f32,
    rect_height: f32,
    color: RgbaColor,
}

impl HoverProbe {
    fn from_hit(
        addon: &SizeAnalyzerAddon,
        tag: &str,
        rect: Rect,
        viewport: RenderViewport,
        preview_url: Option<String>,
    ) -> Self {
        Self {
            title: resolve_hover_title(addon),
            tag: tag.to_owned(),
            path: addon.path.clone(),
            size_bytes: addon.file_size_bytes,
            workshop_id: addon.workshop_id,
            preview_url,
            rect_x: (rect.x / viewport.scale_x()) as f32,
            rect_y: (rect.y / viewport.scale_y()) as f32,
            rect_width: (rect.width / viewport.scale_x()) as f32,
            rect_height: (rect.height / viewport.scale_y()) as f32,
            color: tag_color(tag),
        }
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn tag(&self) -> &str {
        &self.tag
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    pub(crate) const fn workshop_id(&self) -> Option<PublishedFileId> {
        self.workshop_id
    }

    pub(crate) fn preview_url(&self) -> Option<&str> {
        self.preview_url.as_deref()
    }

    pub(crate) const fn rect_x(&self) -> f32 {
        self.rect_x
    }

    pub(crate) const fn rect_y(&self) -> f32 {
        self.rect_y
    }

    pub(crate) const fn rect_width(&self) -> f32 {
        self.rect_width
    }

    pub(crate) const fn rect_height(&self) -> f32 {
        self.rect_height
    }

    pub(crate) const fn color(&self) -> RgbaColor {
        self.color
    }

    fn preview_target(&self) -> PreviewTarget {
        PreviewTarget {
            path: self.path.clone(),
            title: self.title.clone(),
            workshop_id: self.workshop_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewTarget {
    pub(crate) path: PathBuf,
    pub(crate) title: String,
    pub(crate) workshop_id: Option<PublishedFileId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextMenuRequest {
    pub(crate) position: iced::Point,
    target: HoverProbe,
    entries: Vec<context_menu::Entry>,
}

impl ContextMenuRequest {
    fn from_hover(hover: &HoverProbe) -> Self {
        let mut entries = vec![
            context_menu::Entry::open_addon_location(),
            context_menu::Entry::copy_path(),
        ];
        if let Some(workshop_id) = hover.workshop_id {
            entries.push(context_menu::Entry::separator());
            entries.push(context_menu::Entry::steam_workshop());
            entries.push(context_menu::Entry::copy_link());
            entries.push(context_menu::Entry::download());
            if hover.preview_url.is_some() {
                entries.push(context_menu::Entry::open_image());
                entries.push(context_menu::Entry::copy_image_link());
            }
            log::debug!("Size Analyzer context menu prepared for Workshop item {workshop_id}");
        }
        #[cfg(feature = "debug")]
        entries.extend([
            context_menu::Entry::separator(),
            context_menu::Entry::hide_addon(),
        ]);

        Self {
            position: iced::Point::ORIGIN,
            target: hover.clone(),
            entries,
        }
    }

    pub(crate) const fn target(&self) -> &HoverProbe {
        &self.target
    }

    pub(crate) fn entries(&self) -> &[context_menu::Entry] {
        &self.entries
    }
}

/// Identity of the synchronous treemap projection currently installed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayoutProjectionKey {
    snapshot_epoch: u64,
    dimensions: RenderDimensions,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderViewport {
    logical_width: f32,
    logical_height: f32,
    dimensions: RenderDimensions,
}

impl RenderViewport {
    fn from_size(size: Size) -> Option<Self> {
        Self::from_lengths(size.width, size.height)
    }

    fn from_lengths(width: f32, height: f32) -> Option<Self> {
        let dimensions = RenderDimensions::from_lengths(width, height)?;
        Some(Self {
            logical_width: width,
            logical_height: height,
            dimensions,
        })
    }

    pub(crate) const fn bounds(self) -> TreemapBounds {
        self.dimensions.bounds()
    }

    /// Logical surface size the hover rects live in (same space the tooltip
    /// is positioned within).
    const fn logical_size(self) -> Size {
        Size::new(self.logical_width, self.logical_height)
    }

    /// Rounding-correction factor (≈ 1) between logical points and the
    /// whole-unit layout space analysis ran in.
    fn scale_x(self) -> f64 {
        f64::from(self.dimensions.width) / f64::from(self.logical_width)
    }

    fn scale_y(self) -> f64 {
        f64::from(self.dimensions.height) / f64::from(self.logical_height)
    }
}

/// Treemap layout dimensions in logical points rounded to whole units.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RenderDimensions {
    width: u32,
    height: u32,
}

impl RenderDimensions {
    fn from_lengths(width: f32, height: f32) -> Option<Self> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return None;
        }

        Some(Self {
            width: rounded_dimension(width),
            height: rounded_dimension(height),
        })
    }

    const fn bounds(self) -> TreemapBounds {
        TreemapBounds::new(self.width as f64, self.height as f64)
    }
}

fn bucketed_label_scale(scale_factor: f32) -> f32 {
    if !scale_factor.is_finite() || scale_factor <= 1.0 {
        return 1.0;
    }

    ((scale_factor / LABEL_SCALE_BUCKET).ceil() * LABEL_SCALE_BUCKET).min(MAX_LABEL_SCALE)
}

fn rounded_dimension(value: f32) -> u32 {
    let rounded = value.round();
    if rounded <= 1.0 { 1 } else { rounded as u32 }
}

fn thumbnail_owner() -> thumbnail_demand::Owner {
    thumbnail_demand::Owner::SizeAnalyzer
}

fn workshop_demand_id(workshop_id: PublishedFileId) -> thumbnail_demand::DemandId {
    thumbnail_demand::DemandId::new(workshop_id.to_string())
}

fn parse_workshop_demand_id(id: &thumbnail_demand::DemandId) -> Option<PublishedFileId> {
    id.as_str()
        .parse::<u64>()
        .ok()
        .and_then(PublishedFileId::new)
}

#[cfg(test)]
mod tests;
