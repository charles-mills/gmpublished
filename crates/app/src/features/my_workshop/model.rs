use std::{collections::HashMap, ops::Range, time::Duration};

use iced::widget::image;

use crate::bridge::{
    domain::{PublishedFileId, WorkshopItem, WorkshopPage, workshop_url::workshop_item_url},
    tasks::BackendServices,
    ui_error::UiError,
};
use crate::features::context_menu;
use crate::format::DownloadCountFormatter;
use crate::media::{thumbnail_animation, thumbnail_demand, thumbnail_worker::ThumbnailInput};
use crate::widgets::{
    addon_card, addon_grid,
    grid_rows::{self, GridRow},
};

pub const PUBLISH_NEW_ROW_ID: &str = "publish-new";
pub const FIRST_WORKSHOP_PAGE: u32 = 1;
pub const STATS_REFRESH_INTERVAL: Duration = Duration::from_secs(120);
pub const COUNT_ROLL_TICK_INTERVAL: Duration = Duration::from_millis(16);
pub const COUNT_ROLL_DURATION: Duration = Duration::from_millis(300);

const ADDON_THUMBNAIL_MAX_EDGE: u32 = 256;
const OWNER_LABEL: &str = "My Workshop";
const THUMBNAIL_PLAY_POLICY: thumbnail_animation::PlayPolicy =
    thumbnail_animation::PlayPolicy::OnHover;

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    workshop_id: PublishedFileId,
    title: String,
    tags: Vec<String>,
    preview_url: Option<String>,
    subscription_count: u64,
    displayed_count: u64,
    score_bucket: i32,
    score_label: String,
    thumbnail: RowThumbnail,
    thumbnail_play_requested: bool,
    dead: bool,
    count_roll: Option<CountRoll>,
}

impl Row {
    fn from_workshop_item(item: &WorkshopItem) -> Self {
        let preview_url = preview_url(item);
        let thumbnail = if item.dead || preview_url.is_none() {
            RowThumbnail::Dead
        } else {
            RowThumbnail::Loading
        };
        let workshop_id = item.id;

        Self {
            workshop_id,
            title: item.title.clone(),
            tags: item.tags.clone(),
            preview_url,
            subscription_count: item.subscriptions,
            displayed_count: item.subscriptions,
            score_bucket: grid_rows::score_bucket(item.score),
            score_label: grid_rows::score_label(item.score),
            thumbnail,
            thumbnail_play_requested: false,
            dead: item.dead,
            count_roll: None,
        }
    }

    pub(crate) fn id(&self) -> String {
        self.workshop_id.to_string()
    }

    pub(crate) const fn workshop_id(&self) -> PublishedFileId {
        self.workshop_id
    }

    pub(crate) fn card_thumbnail(&self, play_gifs_by_default: bool) -> addon_card::Thumbnail {
        match &self.thumbnail {
            RowThumbnail::Loading => addon_card::Thumbnail::Loading,
            RowThumbnail::Dead => addon_card::Thumbnail::Dead,
            // The GPU upscales the tiny placeholder into a blur until the sharp
            // image lands.
            RowThumbnail::Placeholder(handle) => addon_card::Thumbnail::Placeholder(handle.clone()),
            RowThumbnail::Ready { still, animation } => {
                addon_card::Thumbnail::Ready(animation.as_ref().map_or_else(
                    || still.clone(),
                    |animation| {
                        if self.thumbnail_should_play(play_gifs_by_default) {
                            animation.current_handle().clone()
                        } else {
                            still.clone()
                        }
                    },
                ))
            }
        }
    }

    pub(crate) fn drag_thumbnail(&self) -> Option<image::Handle> {
        match &self.thumbnail {
            RowThumbnail::Placeholder(handle) => Some(handle.clone()),
            RowThumbnail::Ready { still, .. } => Some(still.clone()),
            RowThumbnail::Loading | RowThumbnail::Dead => None,
        }
    }

    pub(crate) fn to_grid_item(
        &self,
        play_gifs_by_default: bool,
        formatter: DownloadCountFormatter,
    ) -> addon_grid::Item {
        let thumbnail = self.card_thumbnail(play_gifs_by_default);
        let roll = self.count_roll.map(|roll| addon_card::SubscriptionRoll {
            from: formatter.format_count(roll.from),
            to: formatter.format_count(roll.to),
            progress: duration_progress(roll.elapsed, COUNT_ROLL_DURATION),
            up: roll.to >= roll.from,
        });
        let card = addon_card::Data::addon(self.id(), self.title.clone())
            .with_subscriptions(
                formatter.format_count(self.displayed_count),
                self.displayed_count,
            )
            .with_subscription_roll(roll)
            .with_score(self.score_bucket, self.score_label.clone())
            .with_thumbnail(thumbnail)
            .with_enabled(!self.dead);

        addon_grid::Item::new(card)
    }

    pub(crate) fn prepare_publish_target(&self) -> Option<PreparePublishTarget> {
        (!self.dead).then(|| {
            PreparePublishTarget::Update(PreparePublishUpdateTarget {
                workshop_id: self.workshop_id,
                title: self.title.clone(),
                tags: self.tags.clone(),
                preview_url: self.preview_url.clone(),
            })
        })
    }

    pub(crate) fn context_menu(&self) -> Option<ContextMenuRequest> {
        if self.dead {
            return None;
        }

        let mut entries = vec![
            context_menu::Entry::steam_workshop(),
            context_menu::Entry::copy_link(),
            context_menu::Entry::download(),
        ];

        if self
            .preview_url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
        {
            entries.extend([
                context_menu::Entry::separator(),
                context_menu::Entry::open_image(),
                context_menu::Entry::copy_image_link(),
            ]);
        }

        #[cfg(feature = "debug")]
        entries.extend([
            context_menu::Entry::separator(),
            context_menu::Entry::adjust_subscribers(10),
            context_menu::Entry::adjust_subscribers(-10),
            context_menu::Entry::adjust_subscribers(1_000_000),
            context_menu::Entry::adjust_subscribers(-1_000_000),
            context_menu::Entry::separator(),
            context_menu::Entry::hide_addon(),
        ]);

        Some(ContextMenuRequest {
            position: iced::Point::ORIGIN,
            row_id: self.id(),
            workshop_id: self.workshop_id,
            workshop_url: workshop_item_url(self.workshop_id),
            preview_url: self.preview_url.clone(),
            entries,
        })
    }

    pub(super) fn record_actual_count(&mut self, count: u64) -> bool {
        if self.subscription_count == count {
            return false;
        }
        self.subscription_count = count;
        true
    }

    #[cfg(feature = "debug")]
    pub(super) fn adjust_subscription_count(&mut self, delta: i64) -> bool {
        let count = if delta >= 0 {
            self.subscription_count.saturating_add(delta as u64)
        } else {
            self.subscription_count.saturating_sub(delta.unsigned_abs())
        };
        self.record_actual_count(count)
    }

    pub(super) fn reconcile_displayed_count(&mut self) -> bool {
        if self.subscription_count == self.displayed_count {
            let had_roll = self.count_roll.is_some();
            self.count_roll = None;
            return had_roll;
        }

        self.count_roll = Some(CountRoll {
            from: self.displayed_count,
            to: self.subscription_count,
            elapsed: Duration::ZERO,
        });
        true
    }

    /// Returns true while a roll is active: progress moved, so the grid item
    /// must be rebuilt for the canvas to see the new roll state.
    pub(super) fn advance_count_roll(&mut self, elapsed: Duration) -> bool {
        let Some(roll) = &mut self.count_roll else {
            return false;
        };

        roll.elapsed = roll.elapsed.saturating_add(elapsed);
        if roll.elapsed >= COUNT_ROLL_DURATION {
            self.displayed_count = roll.to;
            self.count_roll = None;
        }
        true
    }

    pub(super) const fn has_active_count_roll(&self) -> bool {
        self.count_roll.is_some()
    }

    pub(super) fn apply_thumbnail_delivery(
        &mut self,
        delivery: &thumbnail_demand::Delivery,
    ) -> bool {
        if delivery.id.as_str() != self.id() {
            return false;
        }

        match &delivery.result {
            thumbnail_demand::DeliveryResult::Ready(ready) => {
                self.thumbnail = RowThumbnail::Ready {
                    still: ready.handle().clone(),
                    animation: thumbnail_animation::Playback::from_ready(ready),
                };
            }
            thumbnail_demand::DeliveryResult::Placeholder(placeholder) => {
                if matches!(self.thumbnail, RowThumbnail::Loading) {
                    self.thumbnail = RowThumbnail::Placeholder(placeholder.handle().clone());
                } else {
                    return false;
                }
            }
            thumbnail_demand::DeliveryResult::Failed { .. } => {
                self.thumbnail = RowThumbnail::Dead;
            }
        }
        true
    }

    #[cfg(test)]
    pub(super) fn has_ready_thumbnail_for_test(&self) -> bool {
        matches!(self.thumbnail, RowThumbnail::Ready { .. })
    }

    pub(super) fn has_active_animation(&self, play_gifs_by_default: bool) -> bool {
        self.thumbnail_should_play(play_gifs_by_default) && self.has_animation()
    }

    pub(super) fn has_animation(&self) -> bool {
        matches!(
            self.thumbnail,
            RowThumbnail::Ready {
                animation: Some(_),
                ..
            }
        )
    }

    pub(super) fn advance_animation(
        &mut self,
        elapsed: Duration,
        play_gifs_by_default: bool,
    ) -> bool {
        if !self.thumbnail_should_play(play_gifs_by_default) {
            return false;
        }

        let RowThumbnail::Ready {
            animation: Some(animation),
            ..
        } = &mut self.thumbnail
        else {
            return false;
        };

        animation.advance(elapsed)
    }

    pub(super) fn set_thumbnail_play_requested(&mut self, play_requested: bool) -> bool {
        grid_rows::replace_if_changed(&mut self.thumbnail_play_requested, play_requested)
    }

    fn thumbnail_should_play(&self, play_gifs_by_default: bool) -> bool {
        THUMBNAIL_PLAY_POLICY.should_play(true, self.thumbnail_play_requested, play_gifs_by_default)
    }

    #[cfg(test)]
    pub(crate) fn for_test(id: u64, title: &str, subscriptions: u64) -> Self {
        Self {
            workshop_id: PublishedFileId::new(id).expect("test fixture ids are always nonzero"),
            title: title.to_owned(),
            tags: Vec::new(),
            preview_url: Some(format!("https://example.test/{id}.jpg")),
            subscription_count: subscriptions,
            displayed_count: subscriptions,
            score_bucket: 3,
            score_label: "60.00%".to_owned(),
            thumbnail: RowThumbnail::Loading,
            thumbnail_play_requested: false,
            dead: false,
            count_roll: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_ready_animation_for_test(mut self) -> Self {
        self.thumbnail = RowThumbnail::Ready {
            still: image::Handle::from_rgba(1, 1, vec![0, 0, 0, 255]),
            animation: Some(thumbnail_animation::Playback::for_test()),
        };
        self
    }

    #[cfg(test)]
    pub(crate) const fn displayed_count(&self) -> u64 {
        self.displayed_count
    }

    #[cfg(all(test, feature = "debug"))]
    pub(crate) const fn subscription_count_for_test(&self) -> u64 {
        self.subscription_count
    }
}

impl GridRow for Row {
    fn thumbnail_demand(
        &self,
        priority: thumbnail_demand::Priority,
    ) -> Option<thumbnail_demand::Demand> {
        // A placeholder still wants its real pixels; both states demand.
        if !matches!(
            self.thumbnail,
            RowThumbnail::Loading | RowThumbnail::Placeholder(_)
        ) {
            return None;
        }
        let preview_url = self.preview_url.as_deref()?.trim();
        if preview_url.is_empty() {
            return None;
        }

        Some(thumbnail_demand::Demand {
            id: thumbnail_demand::DemandId::new(self.id()),
            input: ThumbnailInput::from_url(preview_url),
            logical_max_edge: ADDON_THUMBNAIL_MAX_EDGE,
            priority,
        })
    }

    fn invalidate_ready_thumbnail(&mut self) -> bool {
        if !matches!(self.thumbnail, RowThumbnail::Ready { .. }) {
            return false;
        }

        let has_preview = self
            .preview_url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty());
        self.thumbnail = if self.dead || !has_preview {
            RowThumbnail::Dead
        } else {
            RowThumbnail::Loading
        };
        self.thumbnail_play_requested = false;
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CountRoll {
    from: u64,
    to: u64,
    elapsed: Duration,
}

#[derive(Clone, Debug, PartialEq)]
enum RowThumbnail {
    Loading,
    /// Blurred ThumbHash stand-in shown until the real pixels decode.
    Placeholder(image::Handle),
    Dead,
    Ready {
        still: image::Handle,
        animation: Option<thumbnail_animation::Playback>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreparePublishTarget {
    New,
    Update(PreparePublishUpdateTarget),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparePublishUpdateTarget {
    pub(crate) workshop_id: PublishedFileId,
    pub(crate) title: String,
    pub(crate) tags: Vec<String>,
    pub(crate) preview_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextMenuRequest {
    pub(crate) position: iced::Point,
    pub(crate) row_id: String,
    pub(crate) workshop_id: PublishedFileId,
    pub(crate) workshop_url: String,
    pub(crate) preview_url: Option<String>,
    pub(crate) entries: Vec<context_menu::Entry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageResult {
    pub(crate) page: u32,
    pub(crate) total: u32,
    pub(crate) rows: Vec<Row>,
}

impl PageResult {
    fn from_page(page: u32, result: &WorkshopPage) -> Self {
        Self {
            page,
            total: result.total,
            rows: result.items.iter().map(Row::from_workshop_item).collect(),
        }
    }

    #[cfg(feature = "debug")]
    pub(crate) fn retain_visible(
        &mut self,
        hidden_workshop_ids: &std::collections::HashSet<PublishedFileId>,
    ) {
        self.rows
            .retain(|row| !hidden_workshop_ids.contains(&row.workshop_id()));
    }
}

pub fn browse_page(ctx: &BackendServices, page: u32) -> Result<PageResult, UiError> {
    ctx.browse_my_workshop_page(page)
        .map(|result| PageResult::from_page(page, &result))
}

pub fn refresh_subscription_counts(
    ctx: &BackendServices,
    pages: u32,
) -> Result<HashMap<PublishedFileId, u64>, UiError> {
    ctx.refresh_my_workshop_subscription_counts(pages)
}

pub fn grid_items(
    rows: &[Row],
    play_gifs_by_default: bool,
    formatter: DownloadCountFormatter,
    publish_new_title: &str,
) -> Vec<addon_grid::Item> {
    let mut items = Vec::with_capacity(rows.len().saturating_add(1));
    items.push(addon_grid::Item::new(addon_card::Data::publish_new(
        PUBLISH_NEW_ROW_ID,
        publish_new_title,
    )));
    items.extend(
        rows.iter()
            .map(|row| row.to_grid_item(play_gifs_by_default, formatter)),
    );
    items
}

pub fn thumbnail_demands(
    rows: &[Row],
    visible_range: Range<usize>,
    generation: u64,
) -> thumbnail_demand::DemandSet {
    grid_rows::thumbnail_demands(rows, visible_range, generation, thumbnail_owner())
}

/// Releases Ready thumbnails outside visible+prefetch so scrolled-away rows
/// stop pinning decoded RGBA; the demand/cache path re-delivers on return.
pub fn release_offscreen_thumbnails(
    rows: &mut [Row],
    visible_range: std::ops::Range<usize>,
) -> bool {
    grid_rows::release_offscreen_thumbnails(rows, visible_range)
}

pub fn invalidate_ready_thumbnails(rows: &mut [Row]) -> bool {
    grid_rows::invalidate_ready_thumbnails(rows)
}

pub fn empty_thumbnail_demands() -> thumbnail_demand::DemandSet {
    thumbnail_demand::DemandSet::empty(thumbnail_owner())
}

pub fn thumbnail_owner() -> thumbnail_demand::Owner {
    grid_rows::thumbnail_owner(OWNER_LABEL)
}

fn preview_url(item: &WorkshopItem) -> Option<String> {
    item.preview_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
}

fn duration_progress(elapsed: Duration, duration: Duration) -> f32 {
    let duration = duration.as_secs_f32();
    if duration <= f32::EPSILON {
        return 1.0;
    }
    (elapsed.as_secs_f32() / duration).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::DownloadCountFormat;

    #[test]
    fn grid_items_format_subscription_count_with_formatter() {
        let rows = vec![Row::for_test(42, "Addon", 12_345)];
        let formatter =
            DownloadCountFormatter::from_format_and_locale(DownloadCountFormat::Space, None);

        let items = grid_items(&rows, false, formatter, "Publish New...");

        assert_eq!(items[1].card().subscriptions_label_for_test(), "12 345");
    }

    #[test]
    fn grid_items_uses_localized_publish_new_title() {
        let items = grid_items(
            &[],
            false,
            DownloadCountFormatter::default(),
            "Publier un nouveau...",
        );

        assert_eq!(items[0].card().display_title(), "Publier un nouveau...");
    }

    #[test]
    fn row_records_count_delta_without_changing_displayed_count() {
        let mut row = Row::for_test(42, "Addon", 100);

        assert!(row.record_actual_count(125));
        assert!(row.reconcile_displayed_count());

        assert_eq!(row.displayed_count(), 100);
        assert!(row.has_active_count_roll());
    }

    #[test]
    fn count_roll_reaches_target_after_duration() {
        let mut row = Row::for_test(42, "Addon", 100);
        row.record_actual_count(150);
        row.reconcile_displayed_count();

        assert!(row.advance_count_roll(COUNT_ROLL_DURATION));

        assert_eq!(row.displayed_count(), 150);
        assert!(!row.has_active_count_roll());
    }

    #[test]
    fn grid_item_carries_roll_labels_and_progress_during_count_roll() {
        let mut row = Row::for_test(42, "Addon", 100);
        row.record_actual_count(150);
        row.reconcile_displayed_count();
        row.advance_count_roll(COUNT_ROLL_DURATION / 2);

        let item = row.to_grid_item(false, DownloadCountFormatter::default());

        let roll = item
            .card()
            .subscription_roll_for_test()
            .expect("mid-roll grid item should carry roll state");
        assert_eq!(roll.from, "100");
        assert_eq!(roll.to, "150");
        assert!(roll.up);
        assert!((roll.progress - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn count_going_back_to_displayed_cancels_the_roll() {
        let mut row = Row::for_test(42, "Addon", 100);
        row.record_actual_count(150);
        row.reconcile_displayed_count();
        assert!(row.has_active_count_roll());

        row.record_actual_count(100);
        assert!(row.reconcile_displayed_count());

        assert!(!row.has_active_count_roll());
        assert_eq!(row.displayed_count(), 100);
    }

    #[test]
    fn thumbnail_demands_include_only_visible_loading_rows() {
        let loading = Row::for_test(1, "Loading", 1);
        let mut dead = Row::for_test(2, "Dead", 2);
        dead.thumbnail = RowThumbnail::Dead;

        let set = thumbnail_demands(&[loading, dead], 0..2, 3);

        assert_eq!(set.owner, thumbnail_owner());
        assert_eq!(set.generation, 3);
        assert_eq!(set.demands.len(), 1);
        assert_eq!(set.demands[0].id.as_str(), "1");
    }

    #[test]
    fn animated_thumbnail_policy_respects_hover_and_user_default() {
        let mut row = Row::for_test(1, "Animated", 1).with_ready_animation_for_test();

        assert!(!row.has_active_animation(false));
        assert!(row.set_thumbnail_play_requested(true));
        assert!(row.has_active_animation(false));
        assert!(row.set_thumbnail_play_requested(false));
        assert!(row.has_active_animation(true));
    }
}
