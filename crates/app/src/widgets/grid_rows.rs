use std::ops::Range;

use crate::media::thumbnail_demand;
use crate::widgets::addon_grid;

pub trait GridRow {
    fn thumbnail_demand(
        &self,
        priority: thumbnail_demand::Priority,
    ) -> Option<thumbnail_demand::Demand>;

    fn invalidate_ready_thumbnail(&mut self) -> bool;
}

pub fn thumbnail_demands<R: GridRow>(
    rows: &[R],
    visible_range: Range<usize>,
    generation: u64,
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

/// Releases Ready thumbnails outside visible+prefetch so scrolled-away rows
/// stop pinning decoded RGBA; the demand/cache path re-delivers on return.
pub fn release_offscreen_thumbnails<R: GridRow>(
    rows: &mut [R],
    visible_range: Range<usize>,
) -> bool {
    let Some(retained) = thumbnail_demand::retained_rows(visible_range, rows.len()) else {
        return false;
    };

    let mut changed = false;
    for (index, row) in rows.iter_mut().enumerate() {
        if !retained.contains(&index) {
            changed |= row.invalidate_ready_thumbnail();
        }
    }
    changed
}

pub fn invalidate_ready_thumbnails<R: GridRow>(rows: &mut [R]) -> bool {
    let mut changed = false;
    for row in rows {
        changed |= row.invalidate_ready_thumbnail();
    }
    changed
}

pub fn thumbnail_owner(label: &'static str) -> thumbnail_demand::Owner {
    thumbnail_demand::Owner::AddonGrid(label)
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
