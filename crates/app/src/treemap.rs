//! Squarified-treemap layout and hit testing for the Size Analyzer: addon
//! sizes in, rectangles out, plus which rectangle a point lands in.

use std::{cmp::Ordering, collections::HashMap, path::PathBuf};

use gmpublished_backend::{ErrorKey, HasErrorKey, error_keys as keys};
use thiserror::Error;

use crate::bridge::domain::{InstalledAddon, PublishedFileId};

const DEFAULT_ADDON_TAG: &str = "addon";

/// Pixel bounds used for treemap layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TreemapBounds {
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl TreemapBounds {
    pub(crate) const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }

    fn validate(self) -> Result<Self, SizeAnalyzerError> {
        if self.width.is_finite()
            && self.height.is_finite()
            && self.width > 0.0
            && self.height > 0.0
        {
            Ok(self)
        } else {
            Err(SizeAnalyzerError::InvalidBounds {
                width: self.width,
                height: self.height,
            })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SizeAnalyzerAddon {
    pub(crate) path: PathBuf,
    pub(crate) workshop_id: Option<PublishedFileId>,
    pub(crate) title: String,
    group_tag: String,
    pub(crate) file_size_bytes: u64,
}

impl SizeAnalyzerAddon {
    #[cfg(test)]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "test fixture helper mirrors the owned source record API"
    )]
    pub(crate) fn new(
        path: impl Into<PathBuf>,
        workshop_id: Option<PublishedFileId>,
        title: impl Into<String>,
        addon_type: Option<String>,
        tags: Vec<String>,
        file_size_bytes: u64,
    ) -> Self {
        let group_tag = normalized_group_tag(addon_type.as_deref(), &tags);
        Self {
            path: path.into(),
            workshop_id,
            title: title.into(),
            group_tag,
            file_size_bytes,
        }
    }

    pub(crate) fn from_installed(addon: &InstalledAddon) -> Self {
        let metadata = &addon.meta.header.metadata;
        Self {
            path: addon.path.clone(),
            workshop_id: addon.workshop_id,
            title: addon.meta.title().to_owned(),
            group_tag: normalized_group_tag(
                metadata.addon_type(),
                metadata.tags().unwrap_or_default(),
            ),
            file_size_bytes: addon.file_size_bytes,
        }
    }

    fn into_tagged(self) -> (String, TreemapAddon) {
        let tag = self.group_tag;
        let addon = TreemapAddon {
            path: self.path,
            workshop_id: self.workshop_id,
            title: self.title,
            file_size_bytes: self.file_size_bytes,
        };
        (tag, addon)
    }
}

fn normalized_group_tag(addon_type: Option<&str>, tags: &[String]) -> String {
    addon_type
        .filter(|addon_type| !addon_type.trim().is_empty())
        .or_else(|| tags.get(1).map(String::as_str))
        .filter(|tag| !tag.trim().is_empty())
        .unwrap_or(DEFAULT_ADDON_TAG)
        .to_lowercase()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreemapAddon {
    pub(crate) path: PathBuf,
    pub(crate) workshop_id: Option<PublishedFileId>,
    pub(crate) title: String,
    pub(crate) file_size_bytes: u64,
}

#[cfg(test)]
impl From<SizeAnalyzerAddon> for TreemapAddon {
    fn from(addon: SizeAnalyzerAddon) -> Self {
        addon.into_tagged().1
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TreemapLayout {
    pub(crate) bounds: TreemapBounds,
    pub(crate) total_size_bytes: u64,
    pub(crate) squares: Vec<TreemapSquare>,
    leaf_count: usize,
}

impl TreemapLayout {
    pub(crate) fn leaf_rects(&self) -> Vec<TreemapLeaf<'_>> {
        let mut leaves = Vec::with_capacity(self.leaf_count);
        collect_leaf_rects(&self.squares, 0.0, 0.0, None, &mut leaves);
        leaves
    }

    pub(crate) fn hit_test_addon(&self, x: f64, y: f64) -> Option<TreemapHit<'_>> {
        hit_test_squares(&self.squares, 0.0, 0.0, None, x, y)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TreemapSquare {
    pub(crate) data: TreemapSquareData,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TreemapSquareData {
    Tag {
        tag: String,
        total_size_bytes: u64,
        children: Vec<TreemapSquare>,
    },
    Addon {
        addon: TreemapAddon,
    },
}

/// Absolute rectangle used by hit-testing and renderer overlays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl Rect {
    fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TreemapLeaf<'a> {
    pub(crate) addon: &'a TreemapAddon,
    pub(crate) tag: &'a str,
    pub(crate) rect: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TreemapHit<'a> {
    pub(crate) addon: &'a TreemapAddon,
    pub(crate) tag: &'a str,
    pub(crate) rect: Rect,
}

#[derive(Debug, Error, PartialEq)]
pub enum SizeAnalyzerError {
    #[error("no installed addons to analyze")]
    NoAddonsFound,
    #[error("invalid size-analyzer bounds {width}x{height}")]
    InvalidBounds { width: f64, height: f64 },
}

impl HasErrorKey for SizeAnalyzerError {
    fn error_key(&self) -> ErrorKey {
        match self {
            Self::NoAddonsFound => keys::NO_ADDONS_FOUND,
            Self::InvalidBounds { .. } => keys::UNKNOWN,
        }
    }

    fn error_detail(&self) -> Option<String> {
        match self {
            Self::NoAddonsFound => None,
            Self::InvalidBounds { .. } => Some(self.to_string()),
        }
    }
}

pub fn analyze_installed_addons(
    addons: &[InstalledAddon],
    bounds: TreemapBounds,
) -> Result<TreemapLayout, SizeAnalyzerError> {
    analyze_addons(addons.iter().map(SizeAnalyzerAddon::from_installed), bounds)
}

pub fn analyze_addons(
    addons: impl IntoIterator<Item = SizeAnalyzerAddon>,
    bounds: TreemapBounds,
) -> Result<TreemapLayout, SizeAnalyzerError> {
    let bounds = bounds.validate()?;
    let mut addons = addons
        .into_iter()
        .filter(|addon| addon.file_size_bytes > 0)
        .collect::<Vec<_>>();
    if addons.is_empty() {
        return Err(SizeAnalyzerError::NoAddonsFound);
    }
    let leaf_count = addons.len();

    addons.sort_by(compare_analyzer_addons);
    let total_size_bytes = addons.iter().fold(0_u64, |total, addon| {
        total.saturating_add(addon.file_size_bytes)
    });
    let squares = taggify(addons, bounds, total_size_bytes);

    Ok(TreemapLayout {
        bounds,
        total_size_bytes,
        squares,
        leaf_count,
    })
}

fn compare_analyzer_addons(a: &SizeAnalyzerAddon, b: &SizeAnalyzerAddon) -> Ordering {
    b.file_size_bytes
        .cmp(&a.file_size_bytes)
        .then_with(|| {
            a.workshop_id
                .map(PublishedFileId::get)
                .cmp(&b.workshop_id.map(PublishedFileId::get))
        })
        .then_with(|| a.path.cmp(&b.path))
        .then_with(|| a.title.cmp(&b.title))
}

fn taggify(
    addons: Vec<SizeAnalyzerAddon>,
    bounds: TreemapBounds,
    total_size_bytes: u64,
) -> Vec<TreemapSquare> {
    let mut groups = Vec::<TagGroup>::new();
    let mut group_index = HashMap::<String, usize>::new();
    for addon in addons {
        let (tag, addon) = addon.into_tagged();
        if let Some(group) = group_index
            .get(&tag)
            .and_then(|index| groups.get_mut(*index))
        {
            group.total_size_bytes = group.total_size_bytes.saturating_add(addon.file_size_bytes);
            group.addons.push(addon);
        } else {
            group_index.insert(tag.clone(), groups.len());
            groups.push(TagGroup {
                tag,
                total_size_bytes: addon.file_size_bytes,
                addons: vec![addon],
            });
        }
    }

    let group_items = groups
        .into_iter()
        .map(|group| WeightedItem::new(group.total_size_bytes as f64, group))
        .collect::<Vec<_>>();

    Squarifier::new(bounds.width, bounds.height)
        .layout(group_items, total_size_bytes as f64)
        .into_iter()
        .map(|square| {
            let group = square.data;
            let padding = (f64::min(square.width, square.height) * 0.05).ceil();
            let child_width = (square.width.floor() - padding).max(0.0);
            let child_height = (square.height.floor() - padding).max(0.0);
            let child_items = group
                .addons
                .into_iter()
                .map(|addon| WeightedItem::new(addon.file_size_bytes as f64, addon))
                .collect::<Vec<_>>();
            let children = Squarifier::new(child_width, child_height)
                .layout(child_items, group.total_size_bytes as f64)
                .into_iter()
                .map(|child| TreemapSquare {
                    data: TreemapSquareData::Addon { addon: child.data },
                    x: child.x,
                    y: child.y,
                    width: child.width,
                    height: child.height,
                })
                .collect();

            TreemapSquare {
                data: TreemapSquareData::Tag {
                    tag: group.tag,
                    total_size_bytes: group.total_size_bytes,
                    children,
                },
                x: square.x,
                y: square.y,
                width: square.width,
                height: square.height,
            }
        })
        .collect()
}

fn collect_leaf_rects<'a>(
    squares: &'a [TreemapSquare],
    offset_x: f64,
    offset_y: f64,
    inherited_tag: Option<&'a str>,
    leaves: &mut Vec<TreemapLeaf<'a>>,
) {
    for square in squares {
        match &square.data {
            TreemapSquareData::Tag { tag, children, .. } => {
                let padding = child_padding(square.width, square.height);
                collect_leaf_rects(
                    children,
                    offset_x + square.x + padding,
                    offset_y + square.y + padding,
                    Some(tag),
                    leaves,
                );
            }
            TreemapSquareData::Addon { addon } => {
                leaves.push(TreemapLeaf {
                    addon,
                    tag: inherited_tag.unwrap_or(DEFAULT_ADDON_TAG),
                    rect: Rect {
                        x: offset_x + square.x,
                        y: offset_y + square.y,
                        width: square.width,
                        height: square.height,
                    },
                });
            }
        }
    }
}

fn hit_test_squares<'a>(
    squares: &'a [TreemapSquare],
    offset_x: f64,
    offset_y: f64,
    inherited_tag: Option<&'a str>,
    x: f64,
    y: f64,
) -> Option<TreemapHit<'a>> {
    for square in squares {
        match &square.data {
            TreemapSquareData::Tag { tag, children, .. } => {
                let rect = Rect {
                    x: offset_x + square.x,
                    y: offset_y + square.y,
                    width: square.width,
                    height: square.height,
                };
                if rect.contains(x, y) {
                    let padding = child_padding(square.width, square.height);
                    if let Some(hit) = hit_test_squares(
                        children,
                        rect.x + padding,
                        rect.y + padding,
                        Some(tag),
                        x,
                        y,
                    ) {
                        return Some(hit);
                    }
                }
            }
            TreemapSquareData::Addon { addon } => {
                let rect = Rect {
                    x: offset_x + square.x,
                    y: offset_y + square.y,
                    width: square.width,
                    height: square.height,
                };
                if rect.contains(x, y) {
                    return Some(TreemapHit {
                        addon,
                        tag: inherited_tag.unwrap_or(DEFAULT_ADDON_TAG),
                        rect,
                    });
                }
            }
        }
    }

    None
}

fn child_padding(width: f64, height: f64) -> f64 {
    (f64::min(width, height) * 0.05).ceil() / 2.0
}

#[derive(Clone, Debug)]
struct TagGroup {
    tag: String,
    total_size_bytes: u64,
    addons: Vec<TreemapAddon>,
}

struct WeightedItem<T> {
    area: f64,
    data: T,
}

impl<T> WeightedItem<T> {
    const fn new(area: f64, data: T) -> Self {
        Self { area, data }
    }
}

struct LayoutSquare<T> {
    data: T,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

struct Squarifier {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Squarifier {
    fn new(width: f64, height: f64) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width,
            height,
        }
    }

    fn layout<T>(mut self, items: Vec<WeightedItem<T>>, total_size: f64) -> Vec<LayoutSquare<T>> {
        if items.is_empty()
            || total_size <= 0.0
            || self.width <= 0.0
            || self.height <= 0.0
            || !total_size.is_finite()
        {
            return Vec::new();
        }

        let mut pending = items
            .into_iter()
            .map(|item| WeightedItem {
                area: (item.area * self.height * self.width) / total_size,
                data: item.data,
            })
            .collect::<std::collections::VecDeque<_>>();
        let mut row = Vec::new();
        let mut squares = Vec::new();
        let mut row_worst = None;
        let mut width = self.min_width().0;
        while !pending.is_empty() {
            let next_area = pending.front().expect("pending is not empty").area;
            if pending.len() == 1 {
                let last = pending.pop_front().expect("pending has one item");
                let vertical = self.min_width().1;
                self.layout_row(&mut row, width, vertical, &mut squares);
                self.layout_row(&mut vec![last], width, vertical, &mut squares);
                break;
            }

            let previous_worst = row_worst;
            let next_worst = self.worst_ratio_with(&row, next_area, width);
            if previous_worst.is_none_or(|worst| worst >= next_worst) {
                row_worst = Some(next_worst);
                row.push(pending.pop_front().expect("pending is not empty"));
                continue;
            }

            self.layout_row(&mut row, width, self.min_width().1, &mut squares);
            row_worst = None;
            width = self.min_width().0;
        }
        squares
    }

    fn worst_ratio_with<T>(
        &self,
        row: &[WeightedItem<T>],
        additional_area: f64,
        width: f64,
    ) -> f64 {
        let mut sum = 0.0;
        let mut max = 0.0;
        let mut min = f64::MAX;
        for item in row {
            sum += item.area;
            max = f64::max(max, item.area);
            min = f64::min(min, item.area);
        }
        if additional_area > 0.0 {
            sum += additional_area;
            max = f64::max(max, additional_area);
            min = f64::min(min, additional_area);
        }

        let sumsum = sum.powi(2);
        let width_squared = width.powi(2);

        f64::max(
            (width_squared * max) / sumsum,
            sumsum / (width_squared * min),
        )
    }

    fn min_width(&self) -> (f64, bool) {
        if self.height.powi(2) > self.width.powi(2) {
            (self.width, false)
        } else {
            (self.height, true)
        }
    }

    fn layout_row<T>(
        &mut self,
        row: &mut Vec<WeightedItem<T>>,
        width: f64,
        vertical: bool,
        squares: &mut Vec<LayoutSquare<T>>,
    ) {
        if row.is_empty() || width <= 0.0 {
            return;
        }

        let row_height = row.iter().map(|item| item.area).sum::<f64>() / width;

        for item in row.drain(..) {
            let row_width = item.area / row_height;
            squares.push(if vertical {
                let square = LayoutSquare {
                    x: self.x,
                    y: self.y,
                    width: row_height,
                    height: row_width,
                    data: item.data,
                };
                self.y += row_width;
                square
            } else {
                let square = LayoutSquare {
                    x: self.x,
                    y: self.y,
                    width: row_width,
                    height: row_height,
                    data: item.data,
                };
                self.x += row_width;
                square
            });
        }

        if vertical {
            self.x += row_height;
            self.y -= width;
            self.width -= row_height;
        } else {
            self.x -= width;
            self.y += row_height;
            self.height -= row_height;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bridge::{
        domain::InstalledAddon,
        gma::{GmaHeader, GmaMeta, GmaMetadata},
    };

    use super::*;

    #[test]
    fn analyze_installed_addons_handles_synthetic_10k_library_under_debug_bound() {
        let addons = (0..10_000)
            .map(|index| {
                installed_addon(
                    format!("/tmp/synthetic-{index}.gma"),
                    format!("Synthetic {index}"),
                    ["map", "tool", "weapon", "servercontent"][index % 4],
                    1_000 + index as u64,
                )
            })
            .collect::<Vec<_>>();

        let layout = analyze_installed_addons(&addons, TreemapBounds::new(1920.0, 1080.0)).unwrap();

        assert_eq!(layout.leaf_rects().len(), addons.len());
    }

    fn installed_addon(path: String, title: String, addon_type: &str, size: u64) -> InstalledAddon {
        InstalledAddon {
            path: path.clone().into(),
            canonical_path: path.clone().into(),
            workshop_id: None,
            file_size_bytes: size,
            modified_epoch_seconds: 1,
            meta: GmaMeta {
                path: path.into(),
                header: GmaHeader {
                    version: 3,
                    timestamp: 0,
                    metadata: GmaMetadata::Standard {
                        title,
                        addon_type: addon_type.to_owned(),
                        tags: Vec::new(),
                        ignore: Vec::new(),
                    },
                    author: String::new(),
                    addon_version: 1,
                },
                entries: std::sync::Arc::from([]),
            },
        }
    }
}
