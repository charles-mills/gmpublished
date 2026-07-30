//! The publish form's state: what the user has chosen, what has been verified
//! against the archive on disk, and what is still missing before the Workshop
//! will accept it.
//!
//! Verification is asynchronous and supersedable — a field can be edited while
//! its previous check is still running — so every verified value carries the
//! generation it was requested at, and a result for a stale generation is
//! dropped rather than shown.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use iced::animation::Easing;
use iced::widget::text_editor;

use crate::theme::{self, motion};

use crate::bridge::tasks::WorkshopSnapshotId;
use crate::bridge::{
    domain::{PublishedFileId, WorkshopDownloadSuccess, workshop_url},
    publish::{PublishSubmitMode, PublishSubmitPreview, PublishSubmitRequest},
    ui_error::UiError,
};
use crate::i18n::{Arg, I18n};
use crate::media::{
    thumbnail_demand::{self, DeliveryResult},
    thumbnail_worker::ThumbnailInput,
};

use crate::bridge::archive::PreviewArchiveSource;
use crate::media::preview_model::PreviewRequest;
use crate::util::paths::path_to_display;
use crate::widgets::file_browser::{Row as FileBrowserRow, State as FileBrowserState};

use super::model::{
    ContentPathVerificationRequest, IconVerificationRequest, IgnorePatternMutation,
    IgnorePatternMutationResult, IgnoredPattern, PublishIconSubmitRequestEnvelope,
    PublishIconSubmitResult, PublishSubmitContext, PublishSubmitRequestEnvelope,
    PublishSubmitResult, VerifiedContentPath, VerifiedContentPathState, VerifiedIconPreview,
    WorkshopContentRequest, WorkshopSnapshotInventory, default_icon_path, publish_selected_preview,
};
use crate::generation::Generation;
use crate::spinner_clock::SpinnerClock;

mod content_selection;
mod icon_state;
mod request;
mod submission_state;
mod verification;
mod workshop_snapshot;

pub use content_selection::{
    AddonTag, AddonType, BrowserSelectHover, Mode, OpenTarget, UpdateTarget,
};
use icon_state::{seeded_backdrop, thumbnail_owner};
pub use submission_state::{Blockers, ChangelogContent, Requirement};
use verification::Verification;
use workshop_snapshot::WorkshopContentLoad;

const SEED_THUMBNAIL_MAX_EDGE: u32 = 512;

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "modal visibility, window focus, and the two one-shot UI latches are independent of each other"
)]
pub struct State {
    open: bool,
    mode: Mode,
    request_generation: Generation,
    workshop_loads: HashMap<WorkshopSnapshotId, WorkshopContentLoad>,
    active_workshop_request: Option<WorkshopSnapshotId>,
    workshop_snapshot_path: Option<PathBuf>,
    pending_cleanup: Vec<PathBuf>,
    icon_generation: Generation,
    submit_generation: Generation,
    addon_path: String,
    content_path: Verification<VerifiedContentPathState>,
    preview_source: Option<Arc<PreviewArchiveSource>>,
    announce_path_success: bool,
    browser: Option<FileBrowserState>,
    browser_snapshot: BrowserSnapshot,
    browser_scroll_offset: f32,
    browser_scroll_reset_pending: bool,
    browser_select_hover: BrowserSelectHover,
    thumbnail_generation: Generation,
    seeded_icon_still: Option<iced::widget::image::Handle>,
    seeded_icon_backdrop: Option<iced::widget::image::Handle>,
    icon: Verification<VerifiedIconPreview>,
    upscale_icon: bool,
    last_icon_animation_tick: Option<Instant>,
    window_focused: bool,
    title: String,
    addon_type: Option<AddonType>,
    tags: [Option<AddonTag>; 3],
    changelog: ChangelogContent,
    ignored_patterns: Vec<IgnoredPattern>,
    ignore_pattern_input: String,
    submit: SpinnerClock,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BrowserSnapshot {
    rows: Vec<FileBrowserRow>,
    header_path: String,
    total_files: i32,
    shown_count: i32,
    total_size_bytes: u64,
    can_go_up: bool,
    visible: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            mode: Mode::New,
            request_generation: Generation::INITIAL,
            workshop_loads: HashMap::new(),
            active_workshop_request: None,
            workshop_snapshot_path: None,
            pending_cleanup: Vec::new(),
            icon_generation: Generation::INITIAL,
            submit_generation: Generation::INITIAL,
            addon_path: String::new(),
            content_path: Verification::Empty,
            preview_source: None,
            announce_path_success: false,
            browser: None,
            browser_snapshot: BrowserSnapshot::default(),
            browser_scroll_offset: 0.0,
            browser_scroll_reset_pending: false,
            browser_select_hover: BrowserSelectHover::default(),
            thumbnail_generation: Generation::INITIAL,
            seeded_icon_still: None,
            seeded_icon_backdrop: None,
            icon: Verification::Empty,
            upscale_icon: false,
            last_icon_animation_tick: None,
            window_focused: true,
            title: String::new(),
            addon_type: None,
            tags: [None, None, None],
            changelog: ChangelogContent::default(),
            ignored_patterns: Vec::new(),
            ignore_pattern_input: String::new(),
            submit: SpinnerClock::Idle,
        }
    }
}

impl State {
    pub(crate) const fn open(&self) -> bool {
        self.open
    }

    #[cfg(test)]
    pub(crate) const fn mode(&self) -> &Mode {
        &self.mode
    }

    pub(crate) const fn update_mode(&self) -> bool {
        matches!(self.mode, Mode::Update(_))
    }

    #[cfg(test)]
    pub(crate) fn workshop_id_text(&self) -> String {
        match &self.mode {
            Mode::New => String::new(),
            Mode::Update(target) => target.workshop_id.to_string(),
        }
    }

    pub(crate) fn workshop_url(&self) -> Option<String> {
        match &self.mode {
            Mode::New => None,
            Mode::Update(target) => Some(workshop_url::workshop_item_url(target.workshop_id)),
        }
    }

    pub(crate) fn update_warning(&self, i18n: &I18n) -> Option<String> {
        match &self.mode {
            Mode::New => None,
            Mode::Update(target) => Some(i18n.trn(
                "prepare-publish-update-warning",
                &[
                    ("title", Arg::Text(target.title.as_str())),
                    ("id", Arg::Text(target.workshop_id.to_string().as_str())),
                ],
            )),
        }
    }

    pub(crate) fn addon_path(&self) -> &str {
        &self.addon_path
    }

    pub(crate) const fn path_pending(&self) -> bool {
        self.content_path.is_pending()
    }

    pub(crate) const fn path_error(&self) -> Option<&UiError> {
        self.content_path.error()
    }

    const fn verified_path(&self) -> Option<&VerifiedContentPathState> {
        match &self.content_path {
            Verification::Verified(verified) => Some(verified),
            _ => None,
        }
    }

    pub(crate) const fn announce_path_success(&self) -> bool {
        self.announce_path_success
    }

    pub(crate) fn is_current_path_generation(&self, generation: Generation) -> bool {
        self.request_generation == generation
    }

    pub(crate) const fn browser_snapshot(&self) -> &BrowserSnapshot {
        &self.browser_snapshot
    }

    /// The viewport height comes from the view's `responsive` wrapper at
    /// layout time, so initial renders and window resizes always window
    /// against the real pane height.
    pub(crate) fn browser_virtual_rows(
        &self,
        viewport_height: f32,
    ) -> crate::widgets::file_browser::VirtualRows {
        crate::widgets::file_browser::virtual_rows(
            self.browser_snapshot.rows.len(),
            self.browser_scroll_offset,
            viewport_height,
        )
    }

    pub(super) fn set_browser_scroll_offset(&mut self, offset: f32) {
        self.browser_scroll_offset = if offset.is_finite() {
            offset.max(0.0)
        } else {
            0.0
        };
    }

    /// True once per snapshot refresh: the widget's own scroll offset must be
    /// snapped back to the top to match the model's reset.
    pub(super) fn take_browser_scroll_reset(&mut self) -> bool {
        std::mem::take(&mut self.browser_scroll_reset_pending)
    }

    pub(crate) fn icon_handle(&self) -> Option<&iced::widget::image::Handle> {
        self.verified_icon().map_or_else(
            || self.seeded_icon_still.as_ref(),
            |selected| {
                selected
                    .animation
                    .as_ref()
                    .map_or(Some(&selected.still), |animation| {
                        Some(animation.current_handle())
                    })
            },
        )
    }

    /// Brighten progress of the hovered browser empty state (0 dim, 1 full).
    pub(crate) fn browser_select_hover_progress(&self, now: Instant) -> f32 {
        self.browser_select_hover.0.interpolate(0.0, 1.0, now)
    }

    #[cfg(test)]
    pub(crate) fn browser_select_hover_animating(&self, now: Instant) -> bool {
        self.open && self.browser_select_hover.0.is_animating(now)
    }

    pub(crate) fn browser_select_hover_needs_ticks(&self) -> bool {
        self.open && self.browser_select_hover.0.needs_ticks()
    }

    pub(super) fn set_browser_select_hover(&mut self, hovered: bool, now: Instant) {
        if self.open {
            self.browser_select_hover.0.go(hovered, now);
        }
    }

    pub(crate) fn tick_browser_select_hover(&mut self, now: Instant) {
        if self.open {
            self.browser_select_hover.0.tick(now);
        }
    }

    pub(crate) fn icon_backdrop_handle(&self) -> Option<&iced::widget::image::Handle> {
        self.verified_icon()
            .map(|selected| &selected.backdrop)
            .or(self.seeded_icon_backdrop.as_ref())
    }

    /// Demand for the update target's Workshop preview shown until the user
    /// picks an icon file; display-only.
    pub(crate) fn thumbnail_demands(&self) -> thumbnail_demand::DemandSet {
        let mut set = thumbnail_demand::DemandSet::empty(thumbnail_owner());
        set.generation = self.thumbnail_generation;
        if !self.open || self.seeded_icon_still.is_some() {
            return set;
        }
        let Mode::Update(target) = &self.mode else {
            return set;
        };
        let Some(url) = target.preview_url.as_deref() else {
            return set;
        };

        set.demands.push(thumbnail_demand::Demand {
            id: thumbnail_demand::DemandId::workshop(target.workshop_id),
            input: ThumbnailInput::from_url(url),
            logical_max_edge: SEED_THUMBNAIL_MAX_EDGE,
            priority: thumbnail_demand::Priority::ActiveDetail,
            capabilities: thumbnail_demand::DemandCapabilities::SURFACE,
        });
        set
    }

    /// Seeds the display-only preview from a thumbnail delivery.
    ///
    /// Failures are silent: the default icon simply stays.
    pub(crate) fn apply_thumbnail_delivery(
        &mut self,
        delivery: &thumbnail_demand::Delivery,
        well_rgb: [u8; 3],
    ) -> bool {
        if delivery.owner != thumbnail_owner()
            || delivery.generation != self.thumbnail_generation
            || !self.open
        {
            return false;
        }
        let Mode::Update(target) = &self.mode else {
            return false;
        };
        if delivery.id.workshop_id() != Some(target.workshop_id) {
            return false;
        }

        let DeliveryResult::Ready(ready) = &delivery.result else {
            return false;
        };
        let still = ready.handle().clone();
        self.seeded_icon_backdrop = Some(seeded_backdrop(&still, well_rgb));
        self.seeded_icon_still = Some(still);
        true
    }

    pub(crate) const fn icon_pending(&self) -> bool {
        self.icon.is_pending()
    }

    pub(crate) const fn icon_error(&self) -> Option<&UiError> {
        self.icon.error()
    }

    const fn verified_icon(&self) -> Option<&VerifiedIconPreview> {
        self.icon.verified()
    }

    const fn verified_icon_mut(&mut self) -> Option<&mut VerifiedIconPreview> {
        self.icon.verified_mut()
    }

    pub(crate) fn icon_display_path(&self) -> Option<&str> {
        self.verified_icon()
            .map(|selected| selected.icon.display_path.as_str())
    }

    pub(crate) const fn icon_selected(&self) -> bool {
        self.verified_icon().is_some()
    }

    pub(crate) fn can_upscale_icon(&self) -> bool {
        self.verified_icon()
            .is_some_and(|selected| selected.icon.can_upscale)
    }

    pub(crate) const fn upscale_icon(&self) -> bool {
        self.upscale_icon
    }

    pub(crate) fn can_remove_icon(&self) -> bool {
        self.open && !self.update_mode() && !self.icon.is_empty()
    }

    /// GIF playback pauses on the current frame while the window is
    /// unfocused, so the clock subscription can drop to idle.
    pub(crate) fn set_window_focused(&mut self, focused: bool) {
        if self.window_focused == focused {
            return;
        }

        self.window_focused = focused;
        self.last_icon_animation_tick = None;
    }

    pub(crate) fn has_active_icon_animation(&self) -> bool {
        self.window_focused
            && self.open
            && self
                .verified_icon()
                .is_some_and(|selected| selected.animation.is_some())
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// `None` is the row that clears the current choice.
    pub(crate) fn addon_type_options(&self, i18n: &I18n) -> Vec<(Option<AddonType>, String)> {
        let mut options = Vec::with_capacity(AddonType::ALL.len() + 1);
        // The placeholder row is there to clear a choice. With nothing chosen it
        // clears nothing and merely repeats the label already on the control's
        // face — and, matching the empty selection, renders as the highlighted
        // row. It earns its place only once there is something to undo.
        if self.addon_type.is_some() {
            options.push((None, i18n.tr("prepare-publish-addon-type")));
        }
        options.extend(
            AddonType::ALL
                .into_iter()
                .map(|value| (Some(value), addon_type_label(i18n, value))),
        );
        options
    }

    pub(crate) const fn addon_type(&self) -> Option<AddonType> {
        self.addon_type
    }

    /// What the control's face reads when the menu is closed.
    pub(crate) fn addon_type_label(&self, i18n: &I18n) -> String {
        self.addon_type.map_or_else(
            || i18n.tr("prepare-publish-addon-type"),
            |value| addon_type_label(i18n, value),
        )
    }

    /// `None` is the row that clears the current choice.
    pub(crate) fn tag_options(
        &self,
        current_index: usize,
        i18n: &I18n,
    ) -> Vec<(Option<AddonTag>, String)> {
        let mut options = Vec::with_capacity(AddonTag::ALL.len() + 1);
        // Same as the type select: nothing chosen means nothing to clear, and
        // the row would only echo the face.
        if self.tags.get(current_index).copied().flatten().is_some() {
            options.push((None, tag_placeholder(i18n, current_index)));
        }
        options.extend(AddonTag::ALL.into_iter().filter_map(|tag| {
            let selected_elsewhere = self
                .tags
                .iter()
                .enumerate()
                .any(|(index, other)| index != current_index && *other == Some(tag));
            (!selected_elsewhere || self.tags[current_index] == Some(tag))
                .then(|| (Some(tag), addon_tag_label(i18n, tag)))
        }));
        options
    }

    /// What the control's face reads when the menu is closed.
    pub(crate) fn tag_label(&self, index: usize, i18n: &I18n) -> String {
        self.tags.get(index).copied().flatten().map_or_else(
            || tag_placeholder(i18n, index),
            |tag| addon_tag_label(i18n, tag),
        )
    }

    pub(crate) fn selected_tag(&self, index: usize) -> Option<AddonTag> {
        self.tags.get(index).copied().flatten()
    }

    pub(crate) const fn changelog_content(&self) -> &text_editor::Content {
        self.changelog.content()
    }

    pub(crate) fn changelog_is_empty(&self) -> bool {
        self.changelog.is_empty()
    }

    fn changelog_trimmed(&self) -> String {
        let text = self.changelog.0.text();
        text.trim().to_owned()
    }

    pub(crate) fn ignored_patterns(&self) -> &[IgnoredPattern] {
        &self.ignored_patterns
    }

    pub(crate) fn ignore_pattern_input(&self) -> &str {
        &self.ignore_pattern_input
    }

    pub(crate) const fn submit_pending(&self) -> bool {
        self.submit.is_running()
    }

    /// Why `Publish!` is unavailable, if it is.
    ///
    /// Verification in flight suppresses the missing list rather than adding to
    /// it: until the running check reports back we cannot know what is actually
    /// absent, and naming a field mid-verify would flag the very thing the user
    /// is in the middle of supplying.
    pub(crate) fn blockers(&self) -> Blockers {
        if !self.open {
            return Blockers::default();
        }

        if self.path_pending() || self.icon_pending() || self.submit_pending() {
            return Blockers {
                pending: true,
                missing: 0,
            };
        }

        let mut missing = 0_u8;
        let mut require = |requirement: Requirement, unmet: bool| {
            if unmet {
                missing |= requirement.bit();
            }
        };
        require(Requirement::AddonPath, self.verified_path().is_none());
        require(
            Requirement::Title,
            !self.update_mode() && self.title.trim().is_empty(),
        );
        require(Requirement::AddonType, self.addon_type.is_none());
        require(Requirement::Tag, self.tags.iter().all(Option::is_none));
        require(
            Requirement::Changelog,
            self.update_mode() && self.changelog_trimmed().is_empty(),
        );

        Blockers {
            pending: false,
            missing,
        }
    }

    pub(crate) fn can_submit(&self) -> bool {
        self.open && self.blockers().is_empty()
    }

    /// The `Publish!` tooltip: the update warning, then whatever is still in
    /// the way. `None` leaves the button bare, which is the ready state of a
    /// brand-new addon.
    pub(crate) fn submit_tooltip(&self, i18n: &I18n) -> Option<String> {
        let mut blocks = Vec::new();
        if let Some(warning) = self.update_warning(i18n) {
            blocks.push(warning);
        }

        let blockers = self.blockers();
        if blockers.pending() {
            blocks.push(i18n.tr("prepare-publish-verifying-content"));
        } else {
            let missing = blockers
                .missing()
                .map(|requirement| format!("\u{2022} {}", i18n.tr(requirement.label_key())))
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                blocks.push(format!(
                    "{}\n{}",
                    i18n.tr("prepare-publish-still-needed"),
                    missing.join("\n")
                ));
            }
        }

        (!blocks.is_empty()).then(|| blocks.join("\n\n"))
    }

    pub(crate) fn can_publish_icon(&self) -> bool {
        self.open
            && self.update_mode()
            && !self.submit_pending()
            && matches!(self.icon, Verification::Verified(_))
    }

    /// Elapsed seconds of the running submit, for the spinner.
    pub(crate) fn spinner_elapsed(&self) -> f32 {
        self.submit.elapsed()
    }

    pub(super) fn open_target(
        &mut self,
        target: OpenTarget,
        ignored_patterns: Vec<IgnoredPattern>,
        upscale_icon_default: bool,
    ) -> Option<WorkshopContentRequest> {
        // Stays monotonic across reopens so a stale thumbnail delivery from a
        // previous target can never seed the new one.
        let thumbnail_generation = self.thumbnail_generation.next();
        let workshop_loads = std::mem::take(&mut self.workshop_loads);
        let mut pending_cleanup = std::mem::take(&mut self.pending_cleanup);
        if let Some(path) = self.workshop_snapshot_path.take() {
            pending_cleanup.push(path);
        }
        *self = Self::default();
        self.open = true;
        self.thumbnail_generation = thumbnail_generation;
        self.workshop_loads = workshop_loads;
        self.pending_cleanup = pending_cleanup;
        self.ignored_patterns = ignored_patterns;
        self.upscale_icon = upscale_icon_default;
        match target {
            OpenTarget::New => None,
            OpenTarget::Update(target) => {
                self.title.clone_from(&target.title);
                self.prefill_from_workshop_tags(&target.tags);
                let request = WorkshopContentRequest {
                    request_id: target.snapshot_request_id,
                    workshop_id: target.workshop_id,
                    destination: target.snapshot_destination.clone(),
                };
                self.content_path = Verification::Pending;
                self.workshop_loads.insert(
                    request.request_id,
                    WorkshopContentLoad {
                        workshop_id: request.workshop_id,
                        destination: request.destination.clone(),
                    },
                );
                self.active_workshop_request = Some(request.request_id);
                self.mode = Mode::Update(target);
                Some(request)
            }
        }
    }

    pub(super) fn close(&mut self) -> Vec<PathBuf> {
        let workshop_loads = std::mem::take(&mut self.workshop_loads);
        let mut cleanup = std::mem::take(&mut self.pending_cleanup);
        if let Some(path) = self.workshop_snapshot_path.take() {
            cleanup.push(path);
        }
        *self = Self::default();
        self.workshop_loads = workshop_loads;
        cleanup
    }

    pub(super) fn take_pending_cleanup(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.pending_cleanup)
    }

    pub(super) fn apply_workshop_submission_result(
        &mut self,
        request_id: WorkshopSnapshotId,
        result: Result<(), UiError>,
    ) {
        if result.is_ok() || !self.workshop_loads.contains_key(&request_id) {
            return;
        }
        let error = result.expect_err("checked error above");
        if let Some(load) = self.workshop_loads.remove(&request_id) {
            self.pending_cleanup.push(load.destination);
        }
        if self.active_workshop_request == Some(request_id) {
            self.active_workshop_request = None;
            self.content_path = Verification::Failed(error);
        }
    }

    pub(super) fn apply_workshop_download(
        &mut self,
        request_id: WorkshopSnapshotId,
        success: WorkshopDownloadSuccess,
    ) -> Option<ContentPathVerificationRequest> {
        let load = self.workshop_loads.remove(&request_id)?;
        let matches_load =
            load.workshop_id == success.item_id && load.destination == success.extracted_path;
        if matches_load && self.open && self.active_workshop_request == Some(request_id) {
            self.active_workshop_request = None;
            self.workshop_snapshot_path = Some(success.extracted_path.clone());
            return Some(ContentPathVerificationRequest {
                generation: self.bump_request_generation(),
                display_path: success.extracted_path.to_string_lossy().into_owned(),
                path: success.extracted_path,
            });
        }
        if success.extracted_path != load.destination {
            self.pending_cleanup.push(success.extracted_path);
        }
        self.pending_cleanup.push(load.destination);
        None
    }

    fn detach_workshop_load(&mut self) {
        self.active_workshop_request = None;
    }

    fn clear_preview_source(&mut self) {
        {
            self.preview_source = None;
        }
    }

    pub(super) fn entry_preview_request(&self, entry_path: &str) -> Option<PreviewRequest> {
        let source = self.preview_source.as_ref()?;
        let entry = source.entry(entry_path).ok()?;
        Some(PreviewRequest {
            request_id: Generation::INITIAL,
            archive: Arc::clone(source),
            entry_path: entry_path.to_owned(),
            display_name: entry
                .path
                .rsplit_once('/')
                .map_or(entry.path, |(_, name)| name)
                .to_owned(),
            size_bytes: entry.size,
            crc32: entry.crc32,
            bypass_size_limits: false,
        })
    }

    pub(super) fn edit_addon_path(&mut self, value: String) {
        if self.open {
            if self
                .verified_path()
                .is_some_and(|verified| verified.display_path == value)
            {
                self.addon_path = value;
                return;
            }

            self.detach_workshop_load();
            self.bump_request_generation();
            self.addon_path = value;
            self.content_path = Verification::Empty;
            self.browser = None;
            self.refresh_browser_snapshot();
            self.clear_preview_source();
        }
    }

    pub(super) fn begin_icon_verification(
        &mut self,
        path: PathBuf,
        well_rgb: [u8; 3],
    ) -> Option<IconVerificationRequest> {
        if !self.open {
            return None;
        }

        let generation = self.bump_icon_generation();
        let display_path = path_to_display(&path);
        self.icon = Verification::Pending;
        self.last_icon_animation_tick = None;

        Some(IconVerificationRequest {
            generation,
            display_path,
            path,
            well_rgb,
        })
    }

    pub(super) fn begin_current_path_verification(
        &mut self,
    ) -> Option<ContentPathVerificationRequest> {
        let addon_path = self.addon_path.clone();
        self.begin_content_path_verification(&addon_path)
    }

    /// Verifies the typed path; success is announced with a sound.
    pub(super) fn begin_accepted_path_verification(
        &mut self,
    ) -> Option<ContentPathVerificationRequest> {
        let request = self.begin_current_path_verification();
        self.announce_path_success = request.is_some();
        request
    }

    pub(super) fn begin_content_path_verification(
        &mut self,
        addon_path: &str,
    ) -> Option<ContentPathVerificationRequest> {
        self.detach_workshop_load();
        self.begin_content_path_verification_inner(addon_path)
    }

    fn begin_content_path_verification_inner(
        &mut self,
        addon_path: &str,
    ) -> Option<ContentPathVerificationRequest> {
        if !self.open {
            return None;
        }

        let addon_path = addon_path.trim().to_owned();
        let generation = self.bump_request_generation();
        self.addon_path.clone_from(&addon_path);
        self.content_path = Verification::Empty;
        self.announce_path_success = false;
        self.browser = None;
        self.refresh_browser_snapshot();
        self.clear_preview_source();

        if addon_path.is_empty() {
            return None;
        }

        self.content_path = Verification::Pending;
        Some(ContentPathVerificationRequest {
            generation,
            display_path: addon_path.clone(),
            path: PathBuf::from(addon_path),
        })
    }

    pub(super) fn apply_verification_result(
        &mut self,
        generation: Generation,
        result: Result<Arc<VerifiedContentPath>, UiError>,
    ) -> bool {
        if !self.open || self.request_generation != generation {
            return false;
        }

        match result {
            Ok(verified) => {
                verified.display_path().clone_into(&mut self.addon_path);
                self.content_path = Verification::Verified(VerifiedContentPathState {
                    display_path: verified.display_path().to_owned(),
                    path: verified.path().to_owned(),
                    total_size: verified.total_size(),
                });
                self.browser = Some(FileBrowserState::from_entries(
                    verified.entries().iter().cloned(),
                ));
                self.preview_source = Some(Arc::clone(verified.preview_source()));
            }
            Err(error) => {
                log::warn!("Prepare Publish content verification failed: {error}");
                self.content_path = Verification::Failed(error);
                self.browser = None;
                self.clear_preview_source();
            }
        }
        self.refresh_browser_snapshot();
        true
    }

    pub(super) fn apply_snapshot_inspection_result(
        &mut self,
        generation: Generation,
        result: Result<Arc<WorkshopSnapshotInventory>, UiError>,
    ) -> bool {
        if !self.open || self.request_generation != generation {
            return false;
        }

        match result {
            Ok(snapshot) => {
                self.content_path = Verification::Empty;
                self.browser = Some(FileBrowserState::from_entries(
                    snapshot.entries().iter().cloned(),
                ));
                self.preview_source = Some(Arc::clone(snapshot.preview_source()));
            }
            Err(error) => {
                log::warn!("Prepare Publish Workshop snapshot inspection failed: {error}");
                self.content_path = Verification::Failed(error);
                self.browser = None;
                self.clear_preview_source();
                if let Some(path) = self.workshop_snapshot_path.take() {
                    self.pending_cleanup.push(path);
                }
            }
        }
        self.refresh_browser_snapshot();
        true
    }

    pub(super) fn apply_icon_verification_result(
        &mut self,
        generation: Generation,
        result: Result<Arc<VerifiedIconPreview>, UiError>,
    ) -> bool {
        if !self.open || self.icon_generation != generation {
            return false;
        }

        self.icon = match result {
            Ok(verified) => Verification::Verified((*verified).clone()),
            Err(error) => {
                log::warn!("Prepare Publish icon verification failed: {error}");
                Verification::Failed(error)
            }
        };
        self.last_icon_animation_tick = None;
        true
    }

    pub(super) fn remove_icon(&mut self) -> bool {
        if !self.open || self.update_mode() {
            return false;
        }

        let changed = self.can_remove_icon() || self.can_upscale_icon();
        self.bump_icon_generation();
        self.icon = Verification::Empty;
        self.last_icon_animation_tick = None;
        changed
    }

    pub(super) fn toggle_upscale_icon(&mut self, value: bool) {
        if self.open && self.can_upscale_icon() {
            self.upscale_icon = value;
        }
    }

    pub(super) fn tick_icon_animation(&mut self, now: Instant) -> bool {
        let Some(last_tick) = self.last_icon_animation_tick.replace(now) else {
            return false;
        };
        let Some(VerifiedIconPreview {
            animation: Some(animation),
            ..
        }) = self.verified_icon_mut()
        else {
            return false;
        };

        animation.advance(now.saturating_duration_since(last_tick))
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
        self.browser_scroll_offset = 0.0;
        self.browser_scroll_reset_pending = true;
        self.browser_snapshot = BrowserSnapshot::from_browser(
            self.browser.as_ref(),
            self.verified_path()
                .map(|verified| verified.display_path.as_str()),
        );
    }

    pub(super) fn edit_title(&mut self, value: String) {
        if self.open && !self.update_mode() {
            self.title = value;
        }
    }

    #[cfg(test)]
    pub(super) fn edit_changelog(&mut self, value: &str) {
        if self.open && self.update_mode() {
            self.changelog = ChangelogContent::from_text(value);
        }
    }

    pub(super) fn perform_changelog_action(&mut self, action: text_editor::Action) {
        if self.open && self.update_mode() {
            self.changelog.perform(action);
        }
    }

    pub(super) fn set_addon_type(&mut self, value: Option<AddonType>) {
        if !self.open {
            return;
        }
        self.addon_type = value;
    }

    pub(super) fn set_tag(&mut self, index: usize, value: Option<AddonTag>) {
        if !self.open || index >= self.tags.len() {
            return;
        }

        let duplicate = value.is_some_and(|value| {
            self.tags
                .iter()
                .enumerate()
                .any(|(other_index, other)| other_index != index && *other == Some(value))
        });

        self.tags[index] = if duplicate { None } else { value };
    }

    pub(super) fn edit_ignore_pattern(&mut self, value: String) {
        if self.open {
            self.ignore_pattern_input = value;
        }
    }

    pub(super) fn accept_ignore_pattern(&mut self) -> Option<IgnorePatternMutation> {
        if !self.open {
            return None;
        }
        let pattern = self.ignore_pattern_input.trim().to_owned();
        if pattern.is_empty() {
            return None;
        }
        self.ignore_pattern_input.clear();
        Some(IgnorePatternMutation::Add(pattern))
    }

    pub(super) fn remove_ignore_pattern(&self, pattern: &str) -> Option<IgnorePatternMutation> {
        if self.open && !pattern.trim().is_empty() {
            Some(IgnorePatternMutation::Remove(pattern.to_owned()))
        } else {
            None
        }
    }

    pub(super) fn apply_ignore_pattern_mutation_result(
        &mut self,
        result: Result<IgnorePatternMutationResult, UiError>,
    ) -> Option<ContentPathVerificationRequest> {
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                log::warn!("Prepare Publish ignored-pattern mutation failed: {error}");
                return None;
            }
        };

        self.ignored_patterns = result.ignored_patterns;
        if let Some(error) = result.save_error {
            log::warn!("Prepare Publish ignored-pattern settings save failed: {error}");
        }
        if result.changed {
            self.begin_current_path_verification()
        } else {
            None
        }
    }

    fn prefill_from_workshop_tags(&mut self, workshop_tags: &[String]) {
        let mut chosen_tags = Vec::with_capacity(self.tags.len());
        for tag in workshop_tags {
            let tag = tag.trim();
            if tag.is_empty() || tag.eq_ignore_ascii_case("Addon") {
                continue;
            }

            if self.addon_type.is_none()
                && let Some(addon_type) = AddonType::from_workshop_tag(tag)
            {
                self.addon_type = Some(addon_type);
                continue;
            }

            if let Some(addon_tag) = AddonTag::from_workshop_tag(tag)
                && !chosen_tags.contains(&addon_tag)
            {
                chosen_tags.push(addon_tag);
            }
        }

        for (slot, tag) in self.tags.iter_mut().zip(chosen_tags) {
            *slot = Some(tag);
        }
    }

    fn bump_request_generation(&mut self) -> Generation {
        self.request_generation.bump();
        self.request_generation
    }

    fn bump_icon_generation(&mut self) -> Generation {
        self.icon_generation.bump();
        self.icon_generation
    }

    fn bump_submit_generation(&mut self) -> Generation {
        self.submit_generation.bump();
        self.submit_generation
    }
}

fn tag_placeholder(i18n: &I18n, index: usize) -> String {
    match index {
        0 => i18n.tr("prepare-publish-tag-1"),
        1 => i18n.tr("prepare-publish-tag-2"),
        _ => i18n.tr("prepare-publish-tag-3"),
    }
}

fn addon_type_label(i18n: &I18n, value: AddonType) -> String {
    i18n.tr(&format!(
        "prepare-publish-type-{}",
        value.as_str().to_ascii_lowercase()
    ))
}

fn addon_tag_label(i18n: &I18n, value: AddonTag) -> String {
    i18n.tr(&format!("prepare-publish-tag-{}", value.as_str()))
}

impl BrowserSnapshot {
    pub(crate) fn rows(&self) -> &[FileBrowserRow] {
        &self.rows
    }

    pub(crate) fn header_path(&self) -> &str {
        &self.header_path
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

    fn from_browser(browser: Option<&FileBrowserState>, display_path: Option<&str>) -> Self {
        Self {
            rows: browser.map(FileBrowserState::rows).unwrap_or_default(),
            header_path: browser
                .map(|browser| browser.header_path(display_path))
                .unwrap_or_default(),
            total_files: browser
                .map(FileBrowserState::footer_total_files)
                .unwrap_or_default(),
            shown_count: browser
                .map(FileBrowserState::footer_shown_count)
                .unwrap_or_default(),
            total_size_bytes: browser
                .map(FileBrowserState::footer_total_size_bytes)
                .unwrap_or_default(),
            can_go_up: browser.is_some_and(FileBrowserState::can_go_up),
            visible: browser.is_some(),
        }
    }
}

fn selected_tags(tags: &[Option<AddonTag>; 3]) -> Vec<String> {
    tags.iter()
        .filter_map(|tag| tag.map(|tag| tag.as_str().to_owned()))
        .collect()
}

#[cfg(test)]
mod tests;
