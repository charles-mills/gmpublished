use std::{ops::Range, path::PathBuf};

use iced::widget::image;

use crate::bridge::gma::is_gma_path;
use crate::bridge::tasks::BackendServices;
use crate::bridge::ui_error::UiError;
use crate::bridge::{
    domain::{
        InstalledAddon, PublishedFileId, RESULTS_PER_PAGE, WorkshopMetadata,
        workshop_url::workshop_item_url,
    },
    library::LibrarySnapshot,
};
use crate::features::context_menu;
use crate::format::DownloadCountFormatter;
use crate::media::{thumbnail_animation, thumbnail_demand, thumbnail_worker::ThumbnailInput};
use crate::widgets::{
    addon_card, addon_grid,
    grid_rows::{self, GridRow},
};

pub(super) const INSTALLED_ADDONS_PAGE_SIZE: usize = RESULTS_PER_PAGE;
const ADDON_THUMBNAIL_MAX_EDGE: u32 = 256;
const OWNER_LABEL: &str = "Installed Addons";
const THUMBNAIL_PLAY_POLICY: thumbnail_animation::PlayPolicy =
    thumbnail_animation::PlayPolicy::OnHover;

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    id: String,
    title: String,
    path: PathBuf,
    workshop_id: Option<PublishedFileId>,
    file_size_bytes: u64,
    modified_epoch_seconds: u64,
    subscription_count: u64,
    score_bucket: i32,
    score_label: String,
    preview_url: Option<String>,
    thumbnail: RowThumbnail,
    thumbnail_play_requested: bool,
}

impl Row {
    fn from_installed(addon: &InstalledAddon) -> Self {
        Self {
            id: addon.path.to_string_lossy().into_owned(),
            title: addon.display_title(),
            path: addon.path.clone(),
            workshop_id: addon.workshop_id,
            file_size_bytes: addon.file_size_bytes,
            modified_epoch_seconds: addon.modified_epoch_seconds,
            subscription_count: 0,
            score_bucket: 0,
            score_label: "0.00%".to_owned(),
            preview_url: None,
            thumbnail: RowThumbnail::Dead,
            thumbnail_play_requested: false,
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) const fn workshop_id(&self) -> Option<PublishedFileId> {
        self.workshop_id
    }

    pub(super) fn has_same_file_fingerprint(&self, other: &Self) -> bool {
        self.file_size_bytes == other.file_size_bytes
            && self.modified_epoch_seconds == other.modified_epoch_seconds
    }

    pub(crate) fn card_thumbnail(&self, play_gifs_by_default: bool) -> addon_card::Thumbnail {
        match &self.thumbnail {
            RowThumbnail::Loading => addon_card::Thumbnail::Loading,
            RowThumbnail::Dead => addon_card::Thumbnail::Dead,
            // The GPU upscales the tiny placeholder into a blur; the sharp
            // image fades in over it when it decodes.
            RowThumbnail::Placeholder(handle) => addon_card::Thumbnail::Placeholder(handle.clone()),
            RowThumbnail::Ready { still, animation } => {
                let handle = match animation {
                    Some(animation) if self.thumbnail_should_play(play_gifs_by_default) => {
                        animation.current_handle().clone()
                    }
                    _ => still.clone(),
                };
                addon_card::Thumbnail::Ready(handle)
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
        let card = addon_card::Data::addon(self.id.clone(), self.title.clone())
            .with_subscriptions(
                formatter.format_count(self.subscription_count),
                self.subscription_count,
            )
            .with_score(self.score_bucket, self.score_label.clone())
            .with_thumbnail(thumbnail);

        addon_grid::Item::new(card)
    }

    pub(crate) fn preview_target(&self) -> Option<PreviewTarget> {
        if !is_gma_path(&self.path) {
            return None;
        }

        Some(PreviewTarget {
            row_id: self.id.clone(),
            path: self.path.clone(),
            title: self.title.clone(),
            workshop_id: self.workshop_id,
            preview_url: self.preview_url.clone(),
            subscription_count: self.subscription_count,
            score_bucket: self.score_bucket,
            score_label: self.score_label.clone(),
        })
    }

    pub(crate) fn context_menu(&self) -> Option<ContextMenuRequest> {
        if !is_gma_path(&self.path) {
            return None;
        }

        let mut entries = vec![
            context_menu::Entry::extract(),
            context_menu::Entry::open_addon_location(),
            context_menu::Entry::copy_path(),
        ];

        let workshop_url = self.workshop_id.map(workshop_item_url);
        if self.workshop_id.is_some() {
            entries.extend([
                context_menu::Entry::separator(),
                context_menu::Entry::steam_workshop(),
                context_menu::Entry::copy_link(),
                context_menu::Entry::download(),
            ]);
        }

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
            context_menu::Entry::hide_addon(),
        ]);

        Some(ContextMenuRequest {
            position: iced::Point::ORIGIN,
            row_id: self.id.clone(),
            path: self.path.clone(),
            path_text: self.id.clone(),
            workshop_id: self.workshop_id,
            workshop_url,
            preview_url: self.preview_url.clone(),
            entries,
        })
    }

    pub(super) fn apply_metadata_patch(&mut self, patch: &MetadataPatch) -> bool {
        if self.workshop_id != Some(patch.workshop_id) {
            return false;
        }

        let mut changed = false;
        changed |= grid_rows::replace_if_changed(&mut self.title, patch.title.clone());
        changed |=
            grid_rows::replace_if_changed(&mut self.subscription_count, patch.subscription_count);
        changed |= grid_rows::replace_if_changed(&mut self.score_bucket, patch.score_bucket);
        changed |= grid_rows::replace_if_changed(&mut self.score_label, patch.score_label.clone());
        if self.preview_url != patch.preview_url {
            self.preview_url.clone_from(&patch.preview_url);
            self.thumbnail = if self.preview_url.is_some() {
                RowThumbnail::Loading
            } else {
                RowThumbnail::Dead
            };
            changed = true;
        }

        changed
    }

    pub(super) fn apply_thumbnail_delivery(
        &mut self,
        generation: u64,
        delivery: &thumbnail_demand::Delivery,
        current_generation: u64,
    ) -> bool {
        if generation != current_generation || delivery.id.as_str() != self.id {
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
                // Only paint over a still-loading row; a placeholder that races
                // in after the real image must not replace it.
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
        elapsed: std::time::Duration,
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
    pub(crate) fn for_test(id: &str, title: &str, workshop_id: Option<PublishedFileId>) -> Self {
        Self {
            id: id.to_owned(),
            title: title.to_owned(),
            path: PathBuf::from(id),
            workshop_id,
            file_size_bytes: 0,
            modified_epoch_seconds: 0,
            subscription_count: 0,
            score_bucket: 0,
            score_label: "0.00%".to_owned(),
            preview_url: None,
            thumbnail: RowThumbnail::Dead,
            thumbnail_play_requested: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_file_fingerprint_for_test(
        mut self,
        file_size_bytes: u64,
        modified_epoch_seconds: u64,
    ) -> Self {
        self.file_size_bytes = file_size_bytes;
        self.modified_epoch_seconds = modified_epoch_seconds;
        self
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
    pub(crate) fn thumbnail_ready_for_test(&self) -> bool {
        matches!(self.thumbnail, RowThumbnail::Ready { .. })
    }

    #[cfg(test)]
    pub(crate) fn title_for_test(&self) -> &str {
        &self.title
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
            id: thumbnail_demand::DemandId::new(self.id.clone()),
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
        self.thumbnail = if has_preview {
            RowThumbnail::Loading
        } else {
            RowThumbnail::Dead
        };
        self.thumbnail_play_requested = false;
        true
    }
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
pub struct PreviewTarget {
    pub(crate) row_id: String,
    pub(crate) path: PathBuf,
    pub(crate) title: String,
    pub(crate) workshop_id: Option<PublishedFileId>,
    pub(crate) preview_url: Option<String>,
    pub(crate) subscription_count: u64,
    pub(crate) score_bucket: i32,
    pub(crate) score_label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextMenuRequest {
    pub(crate) position: iced::Point,
    pub(crate) row_id: String,
    pub(crate) path: PathBuf,
    pub(crate) path_text: String,
    pub(crate) workshop_id: Option<PublishedFileId>,
    pub(crate) workshop_url: Option<String>,
    pub(crate) preview_url: Option<String>,
    pub(crate) entries: Vec<context_menu::Entry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataPatch {
    workshop_id: PublishedFileId,
    title: String,
    subscription_count: u64,
    score_bucket: i32,
    score_label: String,
    preview_url: Option<String>,
}

impl MetadataPatch {
    pub(super) const fn workshop_id(&self) -> PublishedFileId {
        self.workshop_id
    }

    fn from_metadata(metadata: &WorkshopMetadata) -> Option<Self> {
        let title = metadata.title.trim();
        if title.is_empty() {
            return None;
        }

        let preview_url = metadata
            .preview_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_owned);

        Some(Self {
            workshop_id: metadata.id,
            title: title.to_owned(),
            subscription_count: metadata.subscriptions,
            score_bucket: grid_rows::score_bucket(metadata.score),
            score_label: grid_rows::score_label(metadata.score),
            preview_url,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        workshop_id: PublishedFileId,
        title: &str,
        preview_url: Option<&str>,
    ) -> Self {
        Self {
            workshop_id,
            title: title.to_owned(),
            subscription_count: 12_345,
            score_bucket: 4,
            score_label: "80.00%".to_owned(),
            preview_url: preview_url.map(str::to_owned),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataResolution {
    pub(crate) patches: Vec<MetadataPatch>,
    pub(crate) stale_ids: Vec<PublishedFileId>,
}

pub fn rows_from_snapshot(snapshot: &LibrarySnapshot) -> Vec<Row> {
    snapshot.addons.iter().map(Row::from_installed).collect()
}

pub fn resolve_metadata(ctx: &BackendServices, item_ids: &[PublishedFileId]) -> MetadataResolution {
    let (metadata, stale_ids) = ctx.resolve_workshop_metadata(item_ids);
    MetadataResolution {
        patches: metadata
            .iter()
            .filter_map(MetadataPatch::from_metadata)
            .collect(),
        stale_ids,
    }
}

/// Streams metadata as each Workshop query chunk lands, handing `on_batch`
/// the patches for that chunk so visible rows hydrate after one round trip
/// rather than waiting on the slowest chunk.
pub fn refresh_metadata_streaming(
    ctx: &BackendServices,
    item_ids: &[PublishedFileId],
    mut on_batch: impl FnMut(Vec<MetadataPatch>),
) -> Result<(), UiError> {
    ctx.refresh_workshop_metadata_streaming(item_ids, |metadata| {
        let patches = metadata
            .iter()
            .filter_map(MetadataPatch::from_metadata)
            .collect::<Vec<_>>();
        if !patches.is_empty() {
            on_batch(patches);
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::DownloadCountFormat;

    #[test]
    fn placeholder_paints_a_loading_row_and_real_pixels_replace_it() {
        use crate::media::thumbnail_demand::{
            Delivery, DeliveryResult, DemandId, PlaceholderImage, ReadyThumbnail,
        };
        use crate::media::thumbnail_worker::ThumbnailMetadata;

        let workshop_id = PublishedFileId::new(42).expect("test fixture ids are always nonzero");
        let mut row = Row::for_test("/tmp/addon.gma", "Addon", Some(workshop_id));
        row.apply_metadata_patch(&MetadataPatch::for_test(
            workshop_id,
            "Addon",
            Some("https://example.test/a.jpg"),
        ));
        assert!(matches!(row.thumbnail, RowThumbnail::Loading));

        let key = ThumbnailInput::from_url("https://example.test/a.jpg").cache_key(256);
        let delivery = |result| Delivery {
            owner: thumbnail_owner(),
            generation: 0,
            id: DemandId::new("/tmp/addon.gma"),
            key: key.clone(),
            result,
        };

        assert!(row.apply_thumbnail_delivery(
            0,
            &delivery(DeliveryResult::Placeholder(PlaceholderImage::for_test(
                6, 6
            ))),
            0,
        ));
        assert!(matches!(row.thumbnail, RowThumbnail::Placeholder(_)));
        // Still demanding the sharp image while the placeholder paints.
        assert!(
            row.thumbnail_demand(thumbnail_demand::Priority::VisibleRow)
                .is_some()
        );

        let metadata = ThumbnailMetadata {
            width: 8,
            height: 8,
            source_width: 8,
            source_height: 8,
            max_edge: 256,
        };
        assert!(row.apply_thumbnail_delivery(
            0,
            &delivery(DeliveryResult::Ready(ReadyThumbnail::for_test(
                key.clone(),
                metadata,
                vec![0; 8 * 8 * 4],
            ))),
            0,
        ));
        assert!(matches!(row.thumbnail, RowThumbnail::Ready { .. }));
    }

    #[test]
    fn metadata_patch_updates_visible_card_fields_and_thumbnail_state() {
        let mut row = Row::for_test(
            "/tmp/addon.gma",
            "Local title",
            Some(PublishedFileId::new(42).expect("test fixture ids are always nonzero")),
        );
        let patch = MetadataPatch::for_test(
            PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
            "Workshop title",
            Some("https://example.test/a.jpg"),
        );

        assert!(row.apply_metadata_patch(&patch));

        assert_eq!(row.title, "Workshop title");
        assert_eq!(row.subscription_count, 12_345);
        assert_eq!(row.score_bucket, 4);
        assert!(matches!(row.thumbnail, RowThumbnail::Loading));
    }

    #[test]
    fn grid_item_formats_subscription_count_with_formatter() {
        let mut row = Row::for_test(
            "/tmp/addon.gma",
            "Local title",
            Some(PublishedFileId::new(42).expect("test fixture ids are always nonzero")),
        );
        let patch = MetadataPatch::for_test(
            PublishedFileId::new(42).expect("test fixture ids are always nonzero"),
            "Workshop title",
            Some("https://example.test/a.jpg"),
        );
        assert!(row.apply_metadata_patch(&patch));

        let formatter =
            DownloadCountFormatter::from_format_and_locale(DownloadCountFormat::Period, None);
        let item = row.to_grid_item(false, formatter);

        assert_eq!(item.card().subscriptions_label_for_test(), "12.345");
        assert_eq!(row.preview_target().unwrap().subscription_count, 12_345);
    }

    #[test]
    fn context_menu_contains_local_and_workshop_actions() {
        let mut row = Row::for_test(
            "/tmp/addon.gma",
            "Addon",
            Some(PublishedFileId::new(123).expect("test fixture ids are always nonzero")),
        );
        row.preview_url = Some("https://example.test/preview.jpg".to_owned());

        let menu = row
            .context_menu()
            .expect("gma rows should have context menus");
        let actions = menu
            .entries
            .iter()
            .filter(|entry| !entry.separator_row())
            .filter_map(context_menu::Entry::action)
            .collect::<Vec<_>>();

        let expected = vec![
            context_menu::ContextMenuAction::Extract,
            context_menu::ContextMenuAction::OpenAddonLocation,
            context_menu::ContextMenuAction::CopyPath,
            context_menu::ContextMenuAction::SteamWorkshop,
            context_menu::ContextMenuAction::CopyLink,
            context_menu::ContextMenuAction::Download,
            context_menu::ContextMenuAction::OpenImage,
            context_menu::ContextMenuAction::CopyImageLink,
        ];
        #[cfg(feature = "debug")]
        let expected = {
            let mut expected = expected;
            expected.push(context_menu::ContextMenuAction::HideAddon);
            expected
        };
        assert_eq!(actions, expected);
        assert_eq!(
            menu.workshop_url,
            Some(workshop_item_url(
                PublishedFileId::new(123).expect("test fixture ids are always nonzero")
            ))
        );
    }

    #[test]
    fn thumbnail_demands_include_only_visible_loading_rows() {
        let mut loading = Row::for_test(
            "/tmp/loading.gma",
            "Loading",
            Some(PublishedFileId::new(1).expect("test fixture ids are always nonzero")),
        );
        loading.preview_url = Some("https://example.test/loading.jpg".to_owned());
        loading.thumbnail = RowThumbnail::Loading;
        let dead = Row::for_test(
            "/tmp/dead.gma",
            "Dead",
            Some(PublishedFileId::new(2).expect("test fixture ids are always nonzero")),
        );

        let set = thumbnail_demands(&[loading, dead], 0..2, 7);

        assert_eq!(set.owner, thumbnail_owner());
        assert_eq!(set.generation, 7);
        assert_eq!(set.demands.len(), 1);
        assert_eq!(set.demands[0].id.as_str(), "/tmp/loading.gma");
    }

    #[test]
    fn animated_thumbnail_policy_respects_hover_and_user_default() {
        let mut row = Row::for_test(
            "/tmp/animated.gma",
            "Animated",
            Some(PublishedFileId::new(1).expect("test fixture ids are always nonzero")),
        )
        .with_ready_animation_for_test();

        assert!(!row.has_active_animation(false));
        assert!(row.set_thumbnail_play_requested(true));
        assert!(row.has_active_animation(false));
        assert!(row.set_thumbnail_play_requested(false));
        assert!(row.has_active_animation(true));
    }
}
