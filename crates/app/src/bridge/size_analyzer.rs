use std::{cmp::Ordering, collections::HashMap, path::PathBuf};

use gmpublished_backend::error_key::{ErrorKey, HasErrorKey, keys};
use thiserror::Error;

use super::domain::{InstalledAddon, PublishedFileId};

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
    pub(crate) addon_type: Option<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) file_size_bytes: u64,
}

impl SizeAnalyzerAddon {
    #[cfg(test)]
    pub(crate) fn new(
        path: impl Into<PathBuf>,
        workshop_id: Option<PublishedFileId>,
        title: impl Into<String>,
        addon_type: Option<String>,
        tags: Vec<String>,
        file_size_bytes: u64,
    ) -> Self {
        Self {
            path: path.into(),
            workshop_id,
            title: title.into(),
            addon_type,
            tags,
            file_size_bytes,
        }
    }

    pub(crate) fn from_installed(addon: &InstalledAddon) -> Self {
        let metadata = &addon.meta.header.metadata;
        Self {
            path: addon.path.clone(),
            workshop_id: addon.workshop_id,
            title: addon.meta.title().to_owned(),
            addon_type: metadata.addon_type().map(str::to_owned),
            tags: metadata.tags().cloned().unwrap_or_default(),
            file_size_bytes: addon.file_size_bytes,
        }
    }

    fn tag(&self) -> String {
        self.addon_type
            .as_deref()
            .filter(|addon_type| !addon_type.trim().is_empty())
            .or_else(|| self.tags.get(1).map(String::as_str))
            .filter(|tag| !tag.trim().is_empty())
            .unwrap_or(DEFAULT_ADDON_TAG)
            .to_lowercase()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TreemapLayout {
    pub(crate) bounds: TreemapBounds,
    pub(crate) total_size_bytes: u64,
    pub(crate) squares: Vec<TreemapSquare>,
}

impl TreemapLayout {
    pub(crate) fn leaf_rects(&self) -> Vec<TreemapLeaf<'_>> {
        let mut leaves = Vec::new();
        collect_leaf_rects(&self.squares, 0.0, 0.0, &mut leaves);
        leaves
    }

    pub(crate) fn hit_test_addon(&self, x: f64, y: f64) -> Option<TreemapHit<'_>> {
        hit_test_squares(&self.squares, 0.0, 0.0, x, y)
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
        tag: String,
        addon: SizeAnalyzerAddon,
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
    pub(crate) addon: &'a SizeAnalyzerAddon,
    pub(crate) tag: &'a str,
    pub(crate) rect: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TreemapHit<'a> {
    pub(crate) addon: &'a SizeAnalyzerAddon,
    pub(crate) tag: &'a str,
    pub(crate) rect: Rect,
}

#[derive(Debug, Error, PartialEq)]
pub enum SizeAnalyzerError {
    #[error("ERR_NO_ADDONS_FOUND")]
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

    addons.sort_by(compare_analyzer_addons);
    let total_size_bytes = addons.iter().fold(0_u64, |total, addon| {
        total.saturating_add(addon.file_size_bytes)
    });
    let squares = taggify(addons, bounds, total_size_bytes);

    Ok(TreemapLayout {
        bounds,
        total_size_bytes,
        squares,
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
        let tag = addon.tag();
        if let Some(group) = group_index
            .get(&tag)
            .and_then(|index| groups.get_mut(*index))
        {
            group.total_size_bytes = group.total_size_bytes.saturating_add(addon.file_size_bytes);
            group.sizes.push(addon.file_size_bytes as f64);
            group.addons.push(addon);
        } else {
            group_index.insert(tag.clone(), groups.len());
            groups.push(TagGroup {
                tag,
                total_size_bytes: addon.file_size_bytes,
                sizes: vec![addon.file_size_bytes as f64],
                addons: vec![addon],
            });
        }
    }

    let group_sizes = groups
        .iter()
        .map(|group| group.total_size_bytes as f64)
        .collect();

    let mut master_treemap = TreeMap::new(bounds.width, bounds.height);
    master_treemap.data = groups
        .into_iter()
        .map(|group| Some(RawTreeMapData::Tag(group)))
        .collect();
    master_treemap.process(group_sizes, total_size_bytes as f64);

    for square in &mut master_treemap.squares {
        let Some(RawTreeMapData::Tag(group)) = square.data.take() else {
            log::warn!("skipping malformed size-analyzer tag square");
            continue;
        };
        let padding = (f64::min(square.width, square.height) * 0.05).ceil();
        let child_width = (square.width.floor() - padding).max(0.0);
        let child_height = (square.height.floor() - padding).max(0.0);
        let mut treemap = TreeMap::new(child_width, child_height);
        treemap.data = group
            .addons
            .into_iter()
            .map(|addon| Some(RawTreeMapData::Addon(addon)))
            .collect();
        treemap.process(group.sizes, group.total_size_bytes as f64);

        square.data = Some(RawTreeMapData::TagRegion {
            tag: group.tag,
            total_size_bytes: group.total_size_bytes,
            children: treemap.squares,
        });
    }

    master_treemap
        .squares
        .into_iter()
        .filter_map(|square| square.into_public(None))
        .collect()
}

fn collect_leaf_rects<'a>(
    squares: &'a [TreemapSquare],
    offset_x: f64,
    offset_y: f64,
    leaves: &mut Vec<TreemapLeaf<'a>>,
) {
    for square in squares {
        match &square.data {
            TreemapSquareData::Tag { children, .. } => {
                let padding = child_padding(square.width, square.height);
                collect_leaf_rects(
                    children,
                    offset_x + square.x + padding,
                    offset_y + square.y + padding,
                    leaves,
                );
            }
            TreemapSquareData::Addon { tag, addon } => {
                leaves.push(TreemapLeaf {
                    addon,
                    tag,
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

fn hit_test_squares(
    squares: &[TreemapSquare],
    offset_x: f64,
    offset_y: f64,
    x: f64,
    y: f64,
) -> Option<TreemapHit<'_>> {
    for square in squares {
        match &square.data {
            TreemapSquareData::Tag { children, .. } => {
                let rect = Rect {
                    x: offset_x + square.x,
                    y: offset_y + square.y,
                    width: square.width,
                    height: square.height,
                };
                if rect.contains(x, y) {
                    let padding = child_padding(square.width, square.height);
                    if let Some(hit) =
                        hit_test_squares(children, rect.x + padding, rect.y + padding, x, y)
                    {
                        return Some(hit);
                    }
                }
            }
            TreemapSquareData::Addon { tag, addon } => {
                let rect = Rect {
                    x: offset_x + square.x,
                    y: offset_y + square.y,
                    width: square.width,
                    height: square.height,
                };
                if rect.contains(x, y) {
                    return Some(TreemapHit { addon, tag, rect });
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
    sizes: Vec<f64>,
    addons: Vec<SizeAnalyzerAddon>,
}

#[derive(Clone, Debug)]
enum RawTreeMapData {
    Tag(TagGroup),
    TagRegion {
        tag: String,
        total_size_bytes: u64,
        children: Vec<RawSquare>,
    },
    Addon(SizeAnalyzerAddon),
}

#[derive(Clone, Debug)]
struct RawSquare {
    data: Option<RawTreeMapData>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl RawSquare {
    fn into_public(self, inherited_tag: Option<&str>) -> Option<TreemapSquare> {
        let data = match self.data? {
            RawTreeMapData::Tag(_) => return None,
            RawTreeMapData::TagRegion {
                tag,
                total_size_bytes,
                children,
            } => {
                let children = children
                    .into_iter()
                    .filter_map(|child| child.into_public(Some(&tag)))
                    .collect();
                TreemapSquareData::Tag {
                    tag,
                    total_size_bytes,
                    children,
                }
            }
            RawTreeMapData::Addon(addon) => TreemapSquareData::Addon {
                tag: inherited_tag.unwrap_or(DEFAULT_ADDON_TAG).to_owned(),
                addon,
            },
        };

        Some(TreemapSquare {
            data,
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        })
    }
}

struct TreeMap {
    squares: Vec<RawSquare>,
    data: Vec<Option<RawTreeMapData>>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl TreeMap {
    fn new(width: f64, height: f64) -> Self {
        Self {
            data: Vec::new(),
            squares: Vec::new(),
            x: 0.0,
            y: 0.0,
            width,
            height,
        }
    }

    fn process(&mut self, data_sizes: Vec<f64>, total_size: f64) {
        if data_sizes.is_empty()
            || total_size <= 0.0
            || self.width <= 0.0
            || self.height <= 0.0
            || !total_size.is_finite()
        {
            return;
        }

        let scaled = data_sizes
            .into_iter()
            .map(|size| (size * self.height * self.width) / total_size)
            .collect::<Vec<_>>();
        self.squarify(&scaled, 0, &mut Vec::new(), self.min_width().0);
    }

    fn squarify(&mut self, squares: &[f64], next_index: usize, row: &mut Vec<f64>, width: f64) {
        let Some(next_square) = squares.get(next_index).copied() else {
            self.layout_row(row, width, self.min_width().1);
            return;
        };
        if next_index + 1 == squares.len() {
            self.layout_last_square(next_square, row, width);
            return;
        }

        let previous_worst = (!row.is_empty()).then(|| self.worst_ratio(row, width));
        row.push(next_square);
        if previous_worst.is_none_or(|worst| worst >= self.worst_ratio(row, width)) {
            self.squarify(squares, next_index + 1, row, width);
            return;
        }

        row.pop();
        self.layout_row(row, width, self.min_width().1);
        self.squarify(squares, next_index, &mut Vec::new(), self.min_width().0);
    }

    fn worst_ratio(&self, row: &[f64], width: f64) -> f64 {
        let mut sum = 0.0;
        let mut max = 0.0;
        let mut min = f64::MAX;
        for value in row {
            sum += *value;
            max = f64::max(max, *value);
            min = f64::min(min, *value);
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

    fn layout_row(&mut self, row: &mut Vec<f64>, width: f64, vertical: bool) {
        if row.is_empty() || width <= 0.0 {
            return;
        }

        let row_height = row.iter().sum::<f64>() / width;

        for value in row {
            let row_width = *value / row_height;
            let data = self.data.get_mut(self.squares.len()).and_then(Option::take);
            self.squares.push(if vertical {
                let data = RawSquare {
                    x: self.x,
                    y: self.y,
                    width: row_height,
                    height: row_width,
                    data,
                };
                self.y += row_width;
                data
            } else {
                let data = RawSquare {
                    x: self.x,
                    y: self.y,
                    width: row_width,
                    height: row_height,
                    data,
                };
                self.x += row_width;
                data
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

    fn layout_last_square(&mut self, square: f64, row: &mut Vec<f64>, width: f64) {
        let vertical = self.min_width().1;
        self.layout_row(row, width, vertical);
        let mut last = vec![square];
        self.layout_row(&mut last, width, vertical);
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
                entries: Vec::new(),
            },
        }
    }
}
