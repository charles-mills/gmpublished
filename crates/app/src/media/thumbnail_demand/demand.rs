//! How views describe what they want: the demand vocabulary, and the
//! viewport geometry that turns a visible range into demanded rows and
//! physical pixel sizes.

use std::{fmt, ops::Range, sync::Arc};

use crate::bridge::domain::PublishedFileId;
use crate::generation::Generation;
use crate::media::thumbnail_worker::{ThumbnailInput, ThumbnailMode};

const THUMBNAIL_SCALE_BUCKET: f32 = 0.5;
const WORKSHOP_ICON_SOURCE_MAX_EDGE: u32 = 512;
const WORKSHOP_ICON_SOURCE_MAX_SCALE: f32 = 2.0;

pub fn bucketed_thumbnail_scale(scale_factor: f32) -> f32 {
    if !scale_factor.is_finite() || scale_factor <= 1.0 {
        return 1.0;
    }

    ((scale_factor / THUMBNAIL_SCALE_BUCKET).ceil() * THUMBNAIL_SCALE_BUCKET)
        .min(WORKSHOP_ICON_SOURCE_MAX_SCALE)
}

pub fn physical_thumbnail_edge(logical_edge: u32, scale_factor: f32) -> u32 {
    let scaled = f64::from(logical_edge) * f64::from(bucketed_thumbnail_scale(scale_factor));
    scaled
        .round()
        .max(f64::from(logical_edge))
        .min(f64::from(WORKSHOP_ICON_SOURCE_MAX_EDGE)) as u32
}

pub fn prefetch_ranges(visible: Range<usize>, total: usize) -> (Range<usize>, Range<usize>) {
    if total == 0 {
        return (0..0, 0..0);
    }

    let start = visible.start.min(total);
    let end = visible.end.min(total).max(start);
    let visible_len = end.saturating_sub(start);
    if visible_len == 0 {
        return (0..0, 0..0);
    }

    let before_len = visible_len.max(4);
    let after_len = visible_len.saturating_mul(2).max(4);
    (
        start.saturating_sub(before_len)..start,
        end..end.saturating_add(after_len).min(total),
    )
}

/// Rows inside this window keep their thumbnails when a grid releases
/// off-screen handles; everything else downgrades to Loading. `None` means
/// "release nothing" (transient empty viewport, e.g. before the first
/// visible-range event — releasing everything then would flash the grid).
pub fn retained_rows(visible: Range<usize>, total: usize) -> Option<Range<usize>> {
    let visible = visible.start.min(total)..visible.end.min(total);
    if visible.is_empty() {
        return None;
    }
    let (prefetch_before, prefetch_after) = prefetch_ranges(visible, total);
    Some(prefetch_before.start..prefetch_after.end)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Owner {
    InstalledAddons,
    MyWorkshop,
    PreparePublish,
    PreviewGma,
    Search,
    SizeAnalyzer,
    WarmLibrary,
}

/// Identity of the UI entity asking for a thumbnail.
///
/// The manager treats identities opaquely, but owners do not: search rows are
/// numeric, Workshop surfaces use refined Workshop ids, and grid cards use
/// arbitrary stable row keys. Keeping those spaces distinct removes the
/// format/parse round trips that used to sit on every delivery boundary.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum DemandId {
    Row(Arc<str>),
    SearchRow(usize),
    Workshop(PublishedFileId),
    /// An owner with exactly one thumbnail interest, such as GMA preview.
    Singleton,
}

impl DemandId {
    pub fn row(value: impl Into<Arc<str>>) -> Self {
        Self::Row(value.into())
    }

    pub const fn search_row(index: usize) -> Self {
        Self::SearchRow(index)
    }

    pub const fn workshop(id: PublishedFileId) -> Self {
        Self::Workshop(id)
    }

    pub fn row_key(&self) -> Option<&str> {
        match self {
            Self::Row(key) => Some(key),
            Self::SearchRow(_) | Self::Workshop(_) | Self::Singleton => None,
        }
    }

    pub const fn search_row_index(&self) -> Option<usize> {
        match self {
            Self::SearchRow(index) => Some(*index),
            Self::Row(_) | Self::Workshop(_) | Self::Singleton => None,
        }
    }

    pub const fn workshop_id(&self) -> Option<PublishedFileId> {
        match self {
            Self::Workshop(id) => Some(*id),
            Self::Row(_) | Self::SearchRow(_) | Self::Singleton => None,
        }
    }

    pub const fn is_singleton(&self) -> bool {
        matches!(self, Self::Singleton)
    }
}

impl fmt::Display for DemandId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Row(key) => formatter.write_str(key),
            Self::SearchRow(index) => index.fmt(formatter),
            Self::Workshop(id) => id.fmt(formatter),
            Self::Singleton => formatter.write_str("singleton"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Priority {
    ActiveDetail = 0,
    VisibleRow = 1,
    SizeAnalyzer = 2,
    Prefetch = 3,
}

/// What a thumbnail interest is capable of consuming.
///
/// This is deliberately independent of [`Priority`]: cache warming is a
/// cache-only product, not merely a very-low-priority visual request. Static
/// analysis similarly declares the exact decoding and delivery behavior it
/// needs instead of teaching the scheduler about a concrete feature owner.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DemandCapabilities {
    mode: ThumbnailMode,
    delivery: DeliveryPolicy,
    placeholders: bool,
    supersedes_cache_only: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum DeliveryPolicy {
    Surface,
    CacheOnly,
}

impl DemandCapabilities {
    /// Ordinary visual demand: preserve animation and accept placeholders.
    pub const SURFACE: Self = Self {
        mode: ThumbnailMode::Animated,
        delivery: DeliveryPolicy::Surface,
        placeholders: true,
        supersedes_cache_only: false,
    };

    /// Static visual analysis: first frame only, no placeholder, and queued
    /// cache-only work for the same identity yields to it.
    pub const STATIC_ANALYSIS: Self = Self {
        mode: ThumbnailMode::Static,
        delivery: DeliveryPolicy::Surface,
        placeholders: false,
        supersedes_cache_only: true,
    };

    /// Populate the source disk cache without producing a UI delivery.
    pub const CACHE_ONLY: Self = Self {
        mode: ThumbnailMode::Animated,
        delivery: DeliveryPolicy::CacheOnly,
        placeholders: false,
        supersedes_cache_only: false,
    };

    pub(super) const fn mode(self) -> ThumbnailMode {
        self.mode
    }

    pub(super) const fn is_cache_only(self) -> bool {
        matches!(self.delivery, DeliveryPolicy::CacheOnly)
    }

    pub(super) const fn accepts_placeholders(self) -> bool {
        self.placeholders
    }

    pub(super) const fn supersedes_cache_only(self) -> bool {
        self.supersedes_cache_only
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaceMode {
    Owner,
}

#[derive(Clone, Debug)]
pub struct Demand {
    pub id: DemandId,
    pub input: ThumbnailInput,
    pub logical_max_edge: u32,
    pub priority: Priority,
    pub capabilities: DemandCapabilities,
}

impl Demand {
    #[cfg(test)]
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: DemandCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

#[derive(Clone, Debug)]
pub struct DemandSet {
    pub owner: Owner,
    pub generation: Generation,
    pub replace: ReplaceMode,
    pub demands: Vec<Demand>,
}

impl DemandSet {
    pub fn empty(owner: Owner) -> Self {
        Self {
            owner,
            generation: Generation::INITIAL,
            replace: ReplaceMode::Owner,
            demands: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_rows_covers_visible_plus_prefetch_and_guards_empty_viewports() {
        assert_eq!(retained_rows(0..0, 200), None);
        assert_eq!(retained_rows(5..5, 200), None);
        assert_eq!(retained_rows(10..20, 0), None);

        let retained = retained_rows(40..52, 200).expect("window");
        let (before, after) = prefetch_ranges(40..52, 200);
        assert_eq!(retained, before.start..after.end);
        assert!(retained.contains(&40) && retained.contains(&51));
        assert!(retained.start < 40 && retained.end > 52);

        let retained = retained_rows(0..12, 200).expect("window");
        assert_eq!(retained.start, 0);
        let retained = retained_rows(190..210, 200).expect("window");
        assert_eq!(retained.end, 200);
    }

    #[test]
    fn physical_thumbnail_edge_keeps_standard_dpi_size() {
        assert_eq!(physical_thumbnail_edge(256, 1.0), 256);
        assert_eq!(physical_thumbnail_edge(256, 0.0), 256);
        assert_eq!(physical_thumbnail_edge(256, f32::NAN), 256);
    }

    #[test]
    fn physical_thumbnail_edge_rounds_up_to_hidpi_bucket_and_source_cap() {
        assert_eq!(physical_thumbnail_edge(256, 1.25), 384);
        assert_eq!(physical_thumbnail_edge(256, 2.0), 512);
        assert_eq!(physical_thumbnail_edge(256, 9.0), 512);
    }

    #[test]
    fn prefetch_ranges_expand_middle_visible_window() {
        assert_eq!(prefetch_ranges(20..30, 100), (10..20, 30..50));
    }

    #[test]
    fn prefetch_ranges_clamp_at_start() {
        assert_eq!(prefetch_ranges(0..5, 100), (0..0, 5..15));
    }

    #[test]
    fn prefetch_ranges_clamp_at_end() {
        assert_eq!(prefetch_ranges(95..100, 100), (90..95, 100..100));
    }

    #[test]
    fn prefetch_ranges_use_minimum_window_for_tiny_lists() {
        assert_eq!(prefetch_ranges(1..2, 3), (0..1, 2..3));
    }

    #[test]
    fn prefetch_ranges_keep_empty_visible_range_empty() {
        assert_eq!(prefetch_ranges(3..3, 10), (0..0, 0..0));
        assert_eq!(prefetch_ranges(0..0, 0), (0..0, 0..0));
    }
}
