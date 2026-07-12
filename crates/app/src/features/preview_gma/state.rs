use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use gmpublished_backend::bbcode::SpoilerId;
use iced::widget::image;

#[cfg(feature = "asset-studio")]
use crate::backend::archive::PreviewArchiveSource;
use crate::backend::gma::PreviewArchive;
use crate::backend::ui_error::UiError;
use crate::format::DownloadCountFormatter;
use crate::media::{
    thumbnail_animation,
    thumbnail_demand::{self, DeliveryResult},
    thumbnail_worker::ThumbnailInput,
};
use crate::widgets::file_browser::{Row as FileBrowserRowData, State as FileBrowserState};

#[cfg(feature = "asset-studio")]
use crate::features::file_preview::PreviewRequest;

use crate::backend::domain::PublishedFileId;

use super::details::{Details, details_for_archive, infer_workshop_id_from_path};
use super::model::{
    AuthorInfo, AuthorRequest, ExtractionIntent, ExtractionRequest, LoadedArchive, MetadataRequest,
    OpenRequest, OpenSeed, OpenTarget, WorkshopMetadata, workshop_url,
};

const PREVIEW_THUMBNAIL_MAX_EDGE: u32 = 256;
const PREVIEW_THUMBNAIL_DEMAND_ID: &str = "preview-gma";

#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag independently tracks one async fetch/UI concern, not a mode enum"
)]
#[derive(Clone, Debug, PartialEq)]
pub struct State {
    open: bool,
    loading: bool,
    error: Option<UiError>,
    title: String,
    archive_path: Option<PathBuf>,
    workshop_id: Option<PublishedFileId>,
    seed: OpenSeed,
    request_id: u64,
    archive: Option<Arc<PreviewArchive>>,
    browser: Option<FileBrowserState>,
    browser_snapshot: BrowserSnapshot,
    details: Details,
    revealed_description_spoilers: HashSet<SpoilerId>,
    workshop_metadata: Option<WorkshopMetadata>,
    workshop_metadata_requested: bool,
    author_requested: bool,
    author_fetch_failed: bool,
    download_count_formatter: DownloadCountFormatter,
    spinner_started_at: Option<Instant>,
    spinner_now: Option<Instant>,
    thumbnail_url: Option<String>,
    thumbnail: ThumbnailState,
    last_animation_tick: Option<Instant>,
    window_focused: bool,
    pending_extraction: Option<ExtractionRequest>,
    pending_initial_entry_preview: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            loading: false,
            error: None,
            title: String::new(),
            archive_path: None,
            workshop_id: None,
            seed: OpenSeed::default(),
            request_id: 0,
            archive: None,
            browser: None,
            browser_snapshot: BrowserSnapshot::default(),
            details: Details::default(),
            revealed_description_spoilers: HashSet::new(),
            workshop_metadata: None,
            workshop_metadata_requested: false,
            author_requested: false,
            author_fetch_failed: false,
            download_count_formatter: DownloadCountFormatter::default(),
            spinner_started_at: None,
            spinner_now: None,
            thumbnail_url: None,
            thumbnail: ThumbnailState::default(),
            last_animation_tick: None,
            window_focused: true,
            pending_extraction: None,
            pending_initial_entry_preview: None,
        }
    }
}

impl State {
    #[cfg(test)]
    pub(crate) const fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) const fn loading(&self) -> bool {
        self.loading
    }

    pub(crate) fn error(&self) -> Option<&UiError> {
        self.error.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    #[cfg(test)]
    pub(crate) fn archive_path(&self) -> Option<&Path> {
        self.archive_path.as_deref()
    }

    #[cfg(test)]
    pub(crate) const fn workshop_id(&self) -> Option<PublishedFileId> {
        self.workshop_id
    }

    /// The loaded archive; the extract flow reads its extracted name when
    /// opening Destination Select.
    pub(crate) const fn archive(&self) -> Option<&Arc<PreviewArchive>> {
        self.archive.as_ref()
    }

    pub(crate) const fn can_extract(&self) -> bool {
        self.open && !self.loading && self.error.is_none() && self.archive.is_some()
    }

    pub(crate) fn can_open_workshop_link(&self) -> bool {
        self.workshop_link_url().is_some()
    }

    pub(crate) fn set_download_count_formatter(
        &mut self,
        formatter: DownloadCountFormatter,
    ) -> bool {
        if self.download_count_formatter == formatter {
            return false;
        }

        self.download_count_formatter = formatter;
        if self.open && self.archive.is_some() {
            self.refresh_details();
        }
        true
    }

    pub(crate) fn can_copy_current_path(&self) -> bool {
        self.copy_current_path_text().is_some()
    }

    pub(crate) const fn has_pending_extraction(&self) -> bool {
        self.pending_extraction.is_some()
    }

    pub(crate) fn clear_pending_extraction(&mut self) {
        self.pending_extraction = None;
    }

    pub(crate) fn take_pending_archive_extraction(&mut self) -> Option<ExtractionRequest> {
        let request = self.pending_extraction.take()?;
        if !self.open || self.request_id != request.request_id {
            return None;
        }
        if matches!(request.intent, ExtractionIntent::Archive { .. }) {
            Some(request)
        } else {
            None
        }
    }

    pub(crate) const fn browser_snapshot(&self) -> &BrowserSnapshot {
        &self.browser_snapshot
    }

    pub(crate) const fn details(&self) -> &Details {
        &self.details
    }

    pub(crate) const fn revealed_description_spoilers(&self) -> &HashSet<SpoilerId> {
        &self.revealed_description_spoilers
    }

    pub(super) fn toggle_description_spoiler(&mut self, id: SpoilerId) {
        if !self.revealed_description_spoilers.remove(&id) {
            self.revealed_description_spoilers.insert(id);
        }
    }

    pub(crate) fn thumbnail_handle(&self) -> Option<&image::Handle> {
        match &self.thumbnail {
            ThumbnailState::Ready { still, animation } => animation
                .as_ref()
                .map_or(Some(still), |animation| Some(animation.current_handle())),
            ThumbnailState::None | ThumbnailState::Loading | ThumbnailState::Dead => None,
        }
    }

    pub(crate) const fn thumbnail_loading(&self) -> bool {
        matches!(self.thumbnail, ThumbnailState::Loading)
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

    pub(crate) fn has_active_animation(&self) -> bool {
        self.open
            && self.window_focused
            && matches!(
                self.thumbnail,
                ThumbnailState::Ready {
                    animation: Some(_),
                    ..
                }
            )
    }

    pub(crate) fn thumbnail_demands(&self) -> thumbnail_demand::DemandSet {
        let Some(url) = self.thumbnail_url.as_deref() else {
            return thumbnail_demand::DemandSet::empty(thumbnail_owner());
        };
        if !self.open || self.error.is_some() || !self.thumbnail_loading() {
            return thumbnail_demand::DemandSet::empty(thumbnail_owner());
        }

        thumbnail_demand::DemandSet {
            owner: thumbnail_owner(),
            generation: self.request_id,
            replace: thumbnail_demand::ReplaceMode::Owner,
            demands: vec![thumbnail_demand::Demand {
                id: thumbnail_demand::DemandId::new(PREVIEW_THUMBNAIL_DEMAND_ID),
                input: ThumbnailInput::from_url(url),
                logical_max_edge: PREVIEW_THUMBNAIL_MAX_EDGE,
                priority: thumbnail_demand::Priority::ActiveDetail,
            }],
        }
    }

    pub(super) fn begin_open(&mut self, target: OpenTarget) -> OpenRequest {
        self.request_id = self.request_id.saturating_add(1);
        self.open = true;
        self.loading = true;
        self.error = None;
        self.title = display_title(&target.path, &target.title);
        self.archive_path = Some(target.path.clone());
        self.workshop_id = target
            .workshop_id
            .or_else(|| infer_workshop_id_from_path(&target.path));
        self.seed = target.seed;
        self.archive = None;
        self.browser = None;
        self.details = Details::default();
        self.revealed_description_spoilers.clear();
        self.workshop_metadata = None;
        self.workshop_metadata_requested = self.workshop_id.is_none();
        self.author_requested = false;
        self.author_fetch_failed = false;
        self.spinner_started_at = Some(Instant::now());
        self.spinner_now = None;
        // Seed the preview from the click source so the pipeline can serve
        // the grid's cached image immediately; otherwise reserve the square
        // as pending until metadata resolves a URL (or rules one out).
        self.thumbnail_url = self.seed.preview_url.clone();
        self.thumbnail = if self.thumbnail_url.is_some() || self.workshop_id.is_some() {
            ThumbnailState::Loading
        } else {
            ThumbnailState::Dead
        };
        self.last_animation_tick = None;
        self.pending_extraction = None;
        self.pending_initial_entry_preview = target.initial_entry_preview;
        self.refresh_browser_snapshot();

        OpenRequest {
            request_id: self.request_id,
            path: target.path,
            workshop_id: self.workshop_id,
        }
    }

    pub(super) fn apply_archive_opened(
        &mut self,
        request_id: u64,
        result: Result<LoadedArchive, UiError>,
    ) -> bool {
        if !self.open || self.request_id != request_id {
            return false;
        }

        self.loading = false;
        match result {
            Ok(loaded) => {
                self.error = None;
                self.archive = Some(Arc::clone(&loaded.archive));
                self.browser = Some(loaded.browser);
                self.refresh_details();
            }
            Err(error) => {
                self.error = Some(error);
                self.archive = None;
                self.browser = None;
                self.details = Details::default();
                self.thumbnail_url = None;
                self.thumbnail = ThumbnailState::None;
                self.pending_extraction = None;
                self.pending_initial_entry_preview = None;
            }
        }
        self.refresh_browser_snapshot();
        true
    }

    #[cfg(feature = "asset-studio")]
    pub(super) fn take_initial_entry_preview_request(&mut self) -> Option<PreviewRequest> {
        let entry_path = self.pending_initial_entry_preview.take()?;
        self.entry_preview_request(&entry_path)
    }

    /// One-shot Steam profile lookup when metadata lacked a live owner.
    pub(super) fn take_author_request(&mut self) -> Option<AuthorRequest> {
        if !self.open || self.author_requested {
            return None;
        }
        let metadata = self.workshop_metadata.as_ref()?;
        if metadata.author.is_some() {
            return None;
        }
        let steamid64 = metadata.steamid64?;
        self.author_requested = true;
        Some(AuthorRequest {
            request_id: self.request_id,
            steamid64,
        })
    }

    pub(super) fn apply_author_result(
        &mut self,
        request_id: u64,
        steamid64: u64,
        result: Result<AuthorInfo, UiError>,
    ) -> bool {
        if !self.open
            || self.request_id != request_id
            || self
                .workshop_metadata
                .as_ref()
                .and_then(|metadata| metadata.steamid64)
                != Some(steamid64)
        {
            return false;
        }

        match result {
            Ok(author) => {
                if let Some(metadata) = self.workshop_metadata.as_mut() {
                    metadata.author = Some(author.name);
                    metadata.avatar = author.avatar;
                }
                self.author_fetch_failed = false;
            }
            Err(error) => {
                log::debug!("Preview GMA author lookup failed: {error}");
                self.author_fetch_failed = true;
            }
        }
        self.refresh_details();
        true
    }

    pub(crate) fn author_link_available(&self) -> bool {
        self.details
            .author
            .as_ref()
            .is_some_and(|author| author.profile_url.is_some())
    }

    pub(super) fn author_profile_url(&self) -> Option<String> {
        self.details
            .author
            .as_ref()
            .and_then(|author| author.profile_url.clone())
    }

    pub(super) fn reveal_target(&self) -> Option<PathBuf> {
        if !self.can_extract() {
            return None;
        }
        self.archive_path.clone()
    }

    pub(crate) fn spinner_visible(&self) -> bool {
        self.open && (self.loading || self.thumbnail_loading())
    }

    pub(crate) fn spinner_elapsed(&self) -> f32 {
        match (self.spinner_started_at, self.spinner_now) {
            (Some(started), Some(now)) => now.saturating_duration_since(started).as_secs_f32(),
            _ => 0.0,
        }
    }

    pub(super) fn take_workshop_metadata_request(&mut self) -> Option<MetadataRequest> {
        if !self.open
            || self.loading
            || self.error.is_some()
            || self.archive.is_none()
            || self.workshop_metadata_requested
        {
            return None;
        }

        let workshop_id = self.workshop_id?;
        self.workshop_metadata_requested = true;
        Some(MetadataRequest {
            request_id: self.request_id,
            workshop_id,
        })
    }

    pub(super) fn apply_workshop_metadata(
        &mut self,
        request_id: u64,
        workshop_id: PublishedFileId,
        result: Result<Option<WorkshopMetadata>, UiError>,
    ) -> bool {
        if !self.open || self.request_id != request_id || self.workshop_id != Some(workshop_id) {
            return false;
        }

        match result {
            Ok(Some(metadata)) if metadata.id == workshop_id => {
                metadata.title.trim().clone_into(&mut self.title);
                if metadata.preview_url != self.thumbnail_url {
                    self.thumbnail_url.clone_from(&metadata.preview_url);
                    self.thumbnail = if self.thumbnail_url.is_some() {
                        ThumbnailState::Loading
                    } else {
                        ThumbnailState::Dead
                    };
                } else if self.thumbnail_url.is_none() {
                    self.thumbnail = ThumbnailState::Dead;
                }
                self.workshop_metadata = Some(metadata);
                self.refresh_details();
            }
            Ok(_) => {
                self.workshop_metadata = None;
                self.settle_thumbnail_without_metadata();
                self.refresh_details();
            }
            Err(error) => {
                log::debug!("Preview GMA workshop metadata lookup failed: {error}");
                self.workshop_metadata = None;
                self.settle_thumbnail_without_metadata();
                self.refresh_details();
            }
        }
        self.last_animation_tick = None;
        true
    }

    pub(crate) fn apply_thumbnail_delivery(
        &mut self,
        delivery: &thumbnail_demand::Delivery,
    ) -> bool {
        if delivery.owner != thumbnail_owner()
            || delivery.generation != self.request_id
            || delivery.id.as_str() != PREVIEW_THUMBNAIL_DEMAND_ID
        {
            return false;
        }

        match &delivery.result {
            DeliveryResult::Ready(ready) => {
                self.thumbnail = ThumbnailState::Ready {
                    still: ready.handle().clone(),
                    animation: thumbnail_animation::Playback::from_ready(ready),
                };
            }
            // The full-size detail preview waits for its own sharp image rather
            // than flashing a blurred thumbnail-scale placeholder.
            DeliveryResult::Placeholder(_) => return false,
            DeliveryResult::Failed { .. } => {
                self.thumbnail = ThumbnailState::Dead;
            }
        }
        self.last_animation_tick = None;
        true
    }

    pub(crate) fn invalidate_ready_thumbnail(&mut self) -> bool {
        if !matches!(self.thumbnail, ThumbnailState::Ready { .. }) {
            return false;
        }

        self.thumbnail = if self
            .thumbnail_url
            .as_deref()
            .is_some_and(|url| !url.trim().is_empty())
        {
            ThumbnailState::Loading
        } else {
            ThumbnailState::Dead
        };
        self.last_animation_tick = None;
        true
    }

    pub(super) fn tick_animation(&mut self, now: Instant) -> bool {
        let spinning = self.spinner_visible();
        if spinning {
            self.spinner_now = Some(now);
        }
        let Some(last_tick) = self.last_animation_tick.replace(now) else {
            return spinning;
        };
        // Spinner ticks keep arriving while unfocused; the GIF stays paused.
        if !self.window_focused {
            return false;
        }
        let ThumbnailState::Ready {
            animation: Some(animation),
            ..
        } = &mut self.thumbnail
        else {
            return false;
        };

        animation.advance(now.saturating_duration_since(last_tick))
    }

    pub(super) fn close(&mut self) {
        if !self.open && !self.loading && self.archive.is_none() && self.browser.is_none() {
            return;
        }
        self.request_id = self.request_id.saturating_add(1);
        self.open = false;
        self.loading = false;
        self.error = None;
        self.title.clear();
        self.archive_path = None;
        self.workshop_id = None;
        self.archive = None;
        self.browser = None;
        self.details = Details::default();
        self.revealed_description_spoilers.clear();
        self.workshop_metadata = None;
        self.workshop_metadata_requested = true;
        self.author_requested = true;
        self.author_fetch_failed = false;
        self.spinner_started_at = None;
        self.spinner_now = None;
        self.seed = OpenSeed::default();
        self.thumbnail_url = None;
        self.thumbnail = ThumbnailState::None;
        self.last_animation_tick = None;
        self.pending_extraction = None;
        self.pending_initial_entry_preview = None;
        self.refresh_browser_snapshot();
    }

    pub(super) fn request_archive_extraction(&mut self) -> bool {
        let Some(archive) = self.ready_archive() else {
            return false;
        };
        self.pending_extraction = Some(ExtractionRequest {
            request_id: self.request_id,
            archive: Arc::clone(archive),
            intent: ExtractionIntent::Archive {
                total_bytes: archive_total_bytes(archive),
            },
        });
        true
    }

    pub(crate) fn entry_extraction_request(&self, entry_path: &str) -> Option<ExtractionRequest> {
        let archive = self.ready_archive()?;
        let entry = archive.entry(entry_path).ok()?;
        Some(ExtractionRequest {
            request_id: self.request_id,
            archive: Arc::clone(archive),
            intent: ExtractionIntent::Entry {
                path: entry_path.to_owned(),
                size_bytes: entry.size,
            },
        })
    }

    #[cfg(feature = "asset-studio")]
    pub(super) fn entry_preview_request(&self, entry_path: &str) -> Option<PreviewRequest> {
        let archive = self.ready_archive()?;
        let entry = archive.entry(entry_path).ok()?;
        Some(PreviewRequest {
            request_id: 0,
            archive: PreviewArchiveSource::from_gma(Arc::clone(archive)),
            entry_path: entry_path.to_owned(),
            display_name: entry
                .path
                .rsplit_once('/')
                .map_or(entry.path.as_str(), |(_, name)| name)
                .to_owned(),
            size_bytes: entry.size,
            crc32: entry.crc32,
            bypass_size_limits: false,
        })
    }

    pub(super) fn workshop_link_url(&self) -> Option<String> {
        if !self.can_extract() {
            return None;
        }
        self.workshop_id.map(workshop_url)
    }

    pub(super) fn copy_current_path_text(&self) -> Option<String> {
        if !self.can_extract() {
            return None;
        }
        let path = self.header_path_text();
        if path.is_empty() {
            return None;
        }
        Some(if std::path::MAIN_SEPARATOR == '/' {
            path
        } else {
            path.replace('/', std::path::MAIN_SEPARATOR_STR)
        })
    }

    pub(super) fn open_directory(&mut self, path: &str) -> bool {
        let changed = self
            .browser
            .as_mut()
            .is_some_and(|browser| browser.open_directory(path));
        if changed {
            self.refresh_browser_snapshot();
        }
        changed
    }

    pub(super) fn go_up(&mut self) -> bool {
        let changed = self.browser.as_mut().is_some_and(FileBrowserState::go_up);
        if changed {
            self.refresh_browser_snapshot();
        }
        changed
    }

    fn refresh_browser_snapshot(&mut self) {
        self.browser_snapshot =
            BrowserSnapshot::from_browser(self.browser.as_ref(), &self.archive_path_text());
    }

    fn archive_path_text(&self) -> String {
        self.archive_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub(crate) fn header_path_text(&self) -> String {
        let archive_path = self.archive_path_text();
        self.browser
            .as_ref()
            .map(|browser| browser.header_path(Some(&archive_path)))
            .unwrap_or(archive_path)
    }

    fn ready_archive(&self) -> Option<&Arc<PreviewArchive>> {
        self.can_extract()
            .then_some(self.archive.as_ref())
            .flatten()
    }

    /// Without metadata the seeded image (if any) stays; a still-pending
    /// square settles to dead.
    fn settle_thumbnail_without_metadata(&mut self) {
        if !matches!(self.thumbnail, ThumbnailState::Ready { .. }) {
            self.thumbnail = ThumbnailState::Dead;
        }
        if !matches!(self.thumbnail, ThumbnailState::Ready { .. }) {
            self.thumbnail_url = None;
        }
    }

    fn refresh_details(&mut self) {
        let Some(archive) = self.archive.as_ref() else {
            self.details = Details::default();
            return;
        };
        let details = details_for_archive(
            archive,
            &self.archive_path_text(),
            &self.title,
            self.workshop_metadata.as_ref(),
            self.author_fetch_failed,
            self.download_count_formatter,
        );
        if details.description != self.details.description {
            self.revealed_description_spoilers.clear();
        }
        self.details = details;
        // Click-time stats render on the first frame; hydration replaces them.
        if !self.details.has_stats
            && let Some(subscription_count) = self.seed.subscription_count
        {
            self.details.has_stats = true;
            self.details.subscriptions = self
                .download_count_formatter
                .format_count(subscription_count);
            self.details.score_bucket = self.seed.score_bucket.unwrap_or_default();
            self.details.score_label = self.seed.score_label.clone().unwrap_or_default();
        }
        if !self.details.title.trim().is_empty() {
            self.title = self.details.title.clone();
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
enum ThumbnailState {
    #[default]
    None,
    Loading,
    Ready {
        still: image::Handle,
        animation: Option<thumbnail_animation::Playback>,
    },
    Dead,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BrowserSnapshot {
    rows: Vec<FileBrowserRowData>,
    header_path: String,
    total_files: i32,
    shown_count: i32,
    total_size_bytes: u64,
    can_go_up: bool,
    visible: bool,
}

impl BrowserSnapshot {
    pub(crate) fn rows(&self) -> &[FileBrowserRowData] {
        &self.rows
    }

    pub(crate) const fn total_files(&self) -> i32 {
        self.total_files
    }

    pub(crate) const fn shown_count(&self) -> i32 {
        self.shown_count
    }

    pub(crate) const fn total_size_bytes(&self) -> u64 {
        self.total_size_bytes
    }

    pub(crate) const fn can_go_up(&self) -> bool {
        self.can_go_up
    }

    pub(crate) const fn visible(&self) -> bool {
        self.visible
    }

    fn from_browser(browser: Option<&FileBrowserState>, archive_path: &str) -> Self {
        let Some(browser) = browser else {
            return Self {
                header_path: archive_path.to_owned(),
                ..Self::default()
            };
        };

        Self {
            rows: browser.rows(),
            header_path: browser.header_path(Some(archive_path)),
            total_files: browser.footer_total_files(),
            shown_count: browser.footer_shown_count(),
            total_size_bytes: browser.footer_total_size_bytes(),
            can_go_up: browser.can_go_up(),
            visible: true,
        }
    }
}

fn display_title(path: &Path, title: &str) -> String {
    let title = title.trim();
    if !title.is_empty() {
        return title.to_owned();
    }

    path.file_stem()
        .or_else(|| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn thumbnail_owner() -> thumbnail_demand::Owner {
    thumbnail_demand::Owner::PreviewGma
}

fn archive_total_bytes(archive: &PreviewArchive) -> u64 {
    archive
        .entries()
        .iter()
        .fold(0_u64, |total, entry| total.saturating_add(entry.size))
}

#[cfg(test)]
mod tests;
