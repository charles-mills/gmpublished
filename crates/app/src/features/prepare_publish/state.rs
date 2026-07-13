use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use iced::animation::Easing;
use iced::widget::text_editor;

use crate::theme::{Tokens, motion};

use crate::backend::{
    domain::{PublishedFileId, WorkshopDownloadSuccess, workshop_url},
    publish::{PublishSubmitMode, PublishSubmitPreview, PublishSubmitRequest},
    ui_error::UiError,
};
use crate::i18n::I18n;
use crate::media::{
    thumbnail_demand::{self, DeliveryResult},
    thumbnail_worker::ThumbnailInput,
};

#[cfg(feature = "asset-studio")]
use crate::backend::archive::PreviewArchiveSource;
#[cfg(feature = "asset-studio")]
use crate::features::file_preview::PreviewRequest;
use crate::util::paths::path_to_display;
use crate::widgets::file_browser::{Row as FileBrowserRow, State as FileBrowserState};

use super::model::{
    ContentPathVerificationRequest, IconVerificationRequest, IgnorePatternMutation,
    IgnorePatternMutationResult, IgnoredPattern, PublishIconSubmitRequestEnvelope,
    PublishIconSubmitResult, PublishSubmitContext, PublishSubmitRequestEnvelope,
    PublishSubmitResult, SelectOption, VerifiedContentPath, VerifiedContentPathState,
    VerifiedIconPreview, WorkshopContentRequest, default_icon_path, publish_selected_preview,
};

const DEFAULT_VALUE: &str = "";
const SEED_THUMBNAIL_MAX_EDGE: u32 = 512;

/// Steam Workshop addon-type tag. The wire value (`as_str`) is the exact
/// string the backend and workshop tags expect, not a Rust-cased rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddonType {
    ServerContent,
    Gamemode,
    Map,
    Weapon,
    Vehicle,
    Npc,
    Tool,
    Effects,
    Model,
    Entity,
}

impl AddonType {
    const ALL: [Self; 10] = [
        Self::ServerContent,
        Self::Gamemode,
        Self::Map,
        Self::Weapon,
        Self::Vehicle,
        Self::Npc,
        Self::Tool,
        Self::Effects,
        Self::Model,
        Self::Entity,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::ServerContent => "ServerContent",
            Self::Gamemode => "gamemode",
            Self::Map => "map",
            Self::Weapon => "weapon",
            Self::Vehicle => "vehicle",
            Self::Npc => "npc",
            Self::Tool => "tool",
            Self::Effects => "effects",
            Self::Model => "model",
            Self::Entity => "entity",
        }
    }

    fn from_value(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }

    fn from_workshop_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str().eq_ignore_ascii_case(tag))
    }
}

/// Steam Workshop content tag (up to three per addon).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddonTag {
    Fun,
    Roleplay,
    Scenic,
    Movie,
    Realism,
    Cartoon,
    Water,
    Comic,
    Build,
}

impl AddonTag {
    const ALL: [Self; 9] = [
        Self::Fun,
        Self::Roleplay,
        Self::Scenic,
        Self::Movie,
        Self::Realism,
        Self::Cartoon,
        Self::Water,
        Self::Comic,
        Self::Build,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Fun => "fun",
            Self::Roleplay => "roleplay",
            Self::Scenic => "scenic",
            Self::Movie => "movie",
            Self::Realism => "realism",
            Self::Cartoon => "cartoon",
            Self::Water => "water",
            Self::Comic => "comic",
            Self::Build => "build",
        }
    }

    fn from_value(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }

    fn from_workshop_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str().eq_ignore_ascii_case(tag))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenTarget {
    New,
    Update(UpdateTarget),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateTarget {
    pub(crate) workshop_id: PublishedFileId,
    pub(crate) title: String,
    pub(crate) tags: Vec<String>,
    pub(crate) preview_url: Option<String>,
    pub(crate) snapshot_request_id: u64,
    pub(crate) snapshot_destination: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkshopContentLoad {
    workshop_id: PublishedFileId,
    destination: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Mode {
    New,
    Update(UpdateTarget),
}

/// Hover presence of the file-browser empty state, with value semantics.
#[derive(Clone, Debug)]
pub struct BrowserSelectHover(motion::Presence<bool>);

impl Default for BrowserSelectHover {
    fn default() -> Self {
        Self(motion::asymmetric(
            false,
            Tokens::dark().motion.hover_in_duration(),
            Tokens::dark().motion.hover_out_duration(),
            Easing::EaseOut,
        ))
    }
}

impl PartialEq for BrowserSelectHover {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

/// Changelog editor content with value semantics for state snapshots.
#[derive(Debug, Default)]
pub struct ChangelogContent(text_editor::Content);

impl ChangelogContent {
    fn from_text(text: &str) -> Self {
        Self(text_editor::Content::with_text(text))
    }

    pub(crate) const fn content(&self) -> &text_editor::Content {
        &self.0
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn text(&self) -> String {
        self.0.text()
    }

    fn perform(&mut self, action: text_editor::Action) {
        self.0.perform(action);
    }
}

impl Clone for ChangelogContent {
    fn clone(&self) -> Self {
        Self::from_text(&self.text())
    }
}

impl PartialEq for ChangelogContent {
    fn eq(&self, other: &Self) -> bool {
        self.text() == other.text()
    }
}

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag tracks an independent async-verification or UI-focus state, not mutually exclusive"
)]
pub struct State {
    open: bool,
    mode: Mode,
    request_generation: u64,
    workshop_loads: HashMap<u64, WorkshopContentLoad>,
    active_workshop_request: Option<u64>,
    workshop_snapshot_path: Option<PathBuf>,
    pending_cleanup: Vec<PathBuf>,
    icon_generation: u64,
    submit_generation: u64,
    addon_path: String,
    verified_addon_path: Option<VerifiedContentPathState>,
    #[cfg(feature = "asset-studio")]
    preview_source: Option<Arc<PreviewArchiveSource>>,
    path_pending: bool,
    path_error: Option<UiError>,
    announce_path_success: bool,
    browser: Option<FileBrowserState>,
    browser_select_hover: BrowserSelectHover,
    thumbnail_generation: u64,
    seeded_icon_still: Option<iced::widget::image::Handle>,
    seeded_icon_backdrop: Option<iced::widget::image::Handle>,
    selected_icon: Option<VerifiedIconPreview>,
    icon_pending: bool,
    icon_error: Option<UiError>,
    can_upscale_icon: bool,
    upscale_icon: bool,
    last_icon_animation_tick: Option<Instant>,
    window_focused: bool,
    title: String,
    addon_type: Option<AddonType>,
    tags: [Option<AddonTag>; 3],
    changelog: ChangelogContent,
    ignored_patterns: Vec<IgnoredPattern>,
    ignore_pattern_input: String,
    submit_pending: bool,
    submit_started_at: Option<Instant>,
    spinner_now: Option<Instant>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
            request_generation: 0,
            workshop_loads: HashMap::new(),
            active_workshop_request: None,
            workshop_snapshot_path: None,
            pending_cleanup: Vec::new(),
            icon_generation: 0,
            submit_generation: 0,
            addon_path: String::new(),
            verified_addon_path: None,
            #[cfg(feature = "asset-studio")]
            preview_source: None,
            path_pending: false,
            path_error: None,
            announce_path_success: false,
            browser: None,
            browser_select_hover: BrowserSelectHover::default(),
            thumbnail_generation: 0,
            seeded_icon_still: None,
            seeded_icon_backdrop: None,
            selected_icon: None,
            icon_pending: false,
            icon_error: None,
            can_upscale_icon: false,
            upscale_icon: false,
            last_icon_animation_tick: None,
            window_focused: true,
            title: String::new(),
            addon_type: None,
            tags: [None, None, None],
            changelog: ChangelogContent::default(),
            ignored_patterns: Vec::new(),
            ignore_pattern_input: String::new(),
            submit_pending: false,
            submit_started_at: None,
            spinner_now: None,
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
                    ("arg0", target.title.as_str()),
                    ("arg1", target.workshop_id.to_string().as_str()),
                ],
            )),
        }
    }

    pub(crate) fn addon_path(&self) -> &str {
        &self.addon_path
    }

    pub(crate) const fn path_pending(&self) -> bool {
        self.path_pending
    }

    pub(crate) fn path_error(&self) -> Option<&UiError> {
        self.path_error.as_ref()
    }

    pub(crate) const fn announce_path_success(&self) -> bool {
        self.announce_path_success
    }

    pub(crate) const fn is_current_path_generation(&self, generation: u64) -> bool {
        self.request_generation == generation
    }

    pub(crate) fn browser_snapshot(&self) -> BrowserSnapshot {
        BrowserSnapshot::from_browser(
            self.browser.as_ref(),
            self.verified_addon_path
                .as_ref()
                .map(|verified| verified.display_path.as_str()),
        )
    }

    pub(crate) fn icon_handle(&self) -> Option<&iced::widget::image::Handle> {
        self.selected_icon.as_ref().map_or_else(
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
        self.selected_icon
            .as_ref()
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
            id: thumbnail_demand::DemandId::new(target.workshop_id.to_string()),
            input: ThumbnailInput::from_url(url),
            logical_max_edge: SEED_THUMBNAIL_MAX_EDGE,
            priority: thumbnail_demand::Priority::ActiveDetail,
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
        if delivery.id.as_str() != target.workshop_id.to_string() {
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

    #[cfg(test)]
    pub(crate) const fn icon_pending(&self) -> bool {
        self.icon_pending
    }

    pub(crate) fn icon_error(&self) -> Option<&UiError> {
        self.icon_error.as_ref()
    }

    pub(crate) fn icon_display_path(&self) -> Option<&str> {
        self.selected_icon
            .as_ref()
            .map(|selected| selected.icon.display_path.as_str())
    }

    pub(crate) fn icon_selected(&self) -> bool {
        self.selected_icon.is_some()
    }

    pub(crate) const fn can_upscale_icon(&self) -> bool {
        self.can_upscale_icon
    }

    pub(crate) const fn upscale_icon(&self) -> bool {
        self.upscale_icon
    }

    pub(crate) fn can_remove_icon(&self) -> bool {
        self.open
            && !self.update_mode()
            && (self.selected_icon.is_some() || self.icon_pending || self.icon_error.is_some())
    }

    /// GIF playback pauses on the current frame while the window is
    /// unfocused, so the clock subscription can drop to idle.
    pub(crate) fn set_window_focused(&mut self, focused: bool) -> bool {
        if self.window_focused == focused {
            return false;
        }

        self.window_focused = focused;
        self.last_icon_animation_tick = None;
        true
    }

    pub(crate) fn has_active_icon_animation(&self) -> bool {
        self.window_focused
            && self.open
            && self
                .selected_icon
                .as_ref()
                .is_some_and(|selected| selected.animation.is_some())
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn addon_type_options(&self, i18n: &I18n) -> Vec<SelectOption> {
        let mut options = Vec::with_capacity(AddonType::ALL.len() + 1);
        options.push(SelectOption::new(
            i18n.tr("prepare-publish-addon-type"),
            DEFAULT_VALUE,
        ));
        options.extend(
            AddonType::ALL
                .into_iter()
                .map(|value| SelectOption::new(addon_type_label(i18n, value), value.as_str())),
        );
        options
    }

    pub(crate) fn selected_addon_type_option(&self, i18n: &I18n) -> SelectOption {
        self.addon_type.map_or_else(
            || SelectOption::new(i18n.tr("prepare-publish-addon-type"), DEFAULT_VALUE),
            |value| SelectOption::new(addon_type_label(i18n, value), value.as_str()),
        )
    }

    pub(crate) fn tag_options(&self, current_index: usize, i18n: &I18n) -> Vec<SelectOption> {
        let mut options = Vec::with_capacity(AddonTag::ALL.len() + 1);
        options.push(SelectOption::new(
            tag_placeholder(i18n, current_index),
            DEFAULT_VALUE,
        ));
        options.extend(AddonTag::ALL.into_iter().filter_map(|tag| {
            let selected_elsewhere = self
                .tags
                .iter()
                .enumerate()
                .any(|(index, other)| index != current_index && *other == Some(tag));
            (!selected_elsewhere || self.tags[current_index] == Some(tag))
                .then(|| SelectOption::new(addon_tag_label(i18n, tag), tag.as_str()))
        }));
        options
    }

    pub(crate) fn selected_tag_option(&self, index: usize, i18n: &I18n) -> SelectOption {
        self.tags.get(index).copied().flatten().map_or_else(
            || SelectOption::new(tag_placeholder(i18n, index), DEFAULT_VALUE),
            |tag| SelectOption::new(addon_tag_label(i18n, tag), tag.as_str()),
        )
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
        self.submit_pending
    }

    pub(crate) fn can_submit(&self) -> bool {
        if !self.open
            || self.path_pending
            || self.path_error.is_some()
            || self.verified_addon_path.is_none()
            || self.icon_pending
            || self.submit_pending
            || self.addon_type.is_none()
            || self.tags.iter().all(Option::is_none)
        {
            return false;
        }

        if self.update_mode() {
            !self.changelog_trimmed().is_empty()
        } else {
            !self.title.trim().is_empty()
        }
    }

    pub(crate) fn can_publish_icon(&self) -> bool {
        self.open
            && self.update_mode()
            && !self.submit_pending
            && !self.icon_pending
            && self.selected_icon.is_some()
    }

    /// Elapsed seconds of the running submit, for the spinner.
    pub(crate) fn spinner_elapsed(&self) -> f32 {
        match (self.submit_started_at, self.spinner_now) {
            (Some(started), Some(now)) => now.saturating_duration_since(started).as_secs_f32(),
            _ => 0.0,
        }
    }

    pub(super) fn open_target(
        &mut self,
        target: OpenTarget,
        ignored_patterns: Vec<IgnoredPattern>,
        upscale_icon_default: bool,
    ) -> Option<WorkshopContentRequest> {
        // Stays monotonic across reopens so a stale thumbnail delivery from a
        // previous target can never seed the new one.
        let thumbnail_generation = self.thumbnail_generation.saturating_add(1);
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
                self.path_pending = true;
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
        request_id: u64,
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
            self.path_pending = false;
            self.path_error = Some(error);
        }
    }

    pub(super) fn apply_workshop_download(
        &mut self,
        request_id: u64,
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

    #[cfg_attr(
        not(feature = "asset-studio"),
        expect(
            clippy::needless_pass_by_ref_mut,
            reason = "no-op without asset-studio; keeps call sites cfg-free"
        )
    )]
    fn clear_preview_source(&mut self) {
        #[cfg(feature = "asset-studio")]
        {
            self.preview_source = None;
        }
    }

    #[cfg(feature = "asset-studio")]
    pub(super) fn entry_preview_request(&self, entry_path: &str) -> Option<PreviewRequest> {
        let source = self.preview_source.as_ref()?;
        let entry = source.entry(entry_path).ok()?;
        Some(PreviewRequest {
            request_id: 0,
            archive: Arc::clone(source),
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

    pub(super) fn edit_addon_path(&mut self, value: String) {
        if self.open {
            if self
                .verified_addon_path
                .as_ref()
                .is_some_and(|verified| verified.display_path == value)
            {
                self.addon_path = value;
                return;
            }

            self.detach_workshop_load();
            self.bump_request_generation();
            self.addon_path = value;
            self.verified_addon_path = None;
            self.path_pending = false;
            self.path_error = None;
            self.browser = None;
            self.clear_preview_source();
        }
    }

    pub(super) fn begin_icon_verification(
        &mut self,
        path: PathBuf,
        temp_dir: PathBuf,
        well_rgb: [u8; 3],
    ) -> Option<IconVerificationRequest> {
        if !self.open {
            return None;
        }

        let generation = self.bump_icon_generation();
        let display_path = path_to_display(&path);
        self.selected_icon = None;
        self.icon_pending = true;
        self.icon_error = None;
        self.can_upscale_icon = false;
        self.last_icon_animation_tick = None;

        Some(IconVerificationRequest {
            generation,
            display_path,
            path,
            temp_dir,
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
        self.verified_addon_path = None;
        self.path_pending = false;
        self.path_error = None;
        self.announce_path_success = false;
        self.browser = None;
        self.clear_preview_source();

        if addon_path.is_empty() {
            return None;
        }

        self.path_pending = true;
        Some(ContentPathVerificationRequest {
            generation,
            display_path: addon_path.clone(),
            path: PathBuf::from(addon_path),
        })
    }

    pub(super) fn apply_verification_result(
        &mut self,
        generation: u64,
        result: Result<Arc<VerifiedContentPath>, UiError>,
    ) -> bool {
        if !self.open || self.request_generation != generation {
            return false;
        }

        self.path_pending = false;
        match result {
            Ok(verified) => {
                self.addon_path.clone_from(&verified.display_path);
                self.verified_addon_path = Some(VerifiedContentPathState {
                    display_path: verified.display_path.clone(),
                    path: verified.path.clone(),
                    total_size: verified.total_size,
                });
                self.path_error = None;
                self.browser = Some(FileBrowserState::from_entries(
                    verified.entries.iter().cloned(),
                ));
                #[cfg(feature = "asset-studio")]
                {
                    self.preview_source = Some(Arc::clone(&verified.preview_source));
                }
            }
            Err(error) => {
                log::warn!("Prepare Publish content verification failed: {error}");
                self.verified_addon_path = None;
                self.path_error = Some(error);
                self.browser = None;
                self.clear_preview_source();
            }
        }
        true
    }

    pub(super) fn apply_snapshot_inspection_result(
        &mut self,
        generation: u64,
        result: Result<Arc<VerifiedContentPath>, UiError>,
    ) -> bool {
        if !self.open || self.request_generation != generation {
            return false;
        }

        self.path_pending = false;
        match result {
            Ok(snapshot) => {
                self.path_error = None;
                self.browser = Some(FileBrowserState::from_entries(
                    snapshot.entries.iter().cloned(),
                ));
                #[cfg(feature = "asset-studio")]
                {
                    self.preview_source = Some(Arc::clone(&snapshot.preview_source));
                }
            }
            Err(error) => {
                log::warn!("Prepare Publish Workshop snapshot inspection failed: {error}");
                self.path_error = Some(error);
                self.browser = None;
                self.clear_preview_source();
                if let Some(path) = self.workshop_snapshot_path.take() {
                    self.pending_cleanup.push(path);
                }
            }
        }
        true
    }

    pub(super) fn apply_icon_verification_result(
        &mut self,
        generation: u64,
        result: Result<Arc<VerifiedIconPreview>, UiError>,
    ) -> bool {
        if !self.open || self.icon_generation != generation {
            return false;
        }

        self.icon_pending = false;
        match result {
            Ok(verified) => {
                self.can_upscale_icon = verified.icon.can_upscale;
                self.selected_icon = Some((*verified).clone());
                self.icon_error = None;
            }
            Err(error) => {
                log::warn!("Prepare Publish icon verification failed: {error}");
                self.selected_icon = None;
                self.icon_error = Some(error);
                self.can_upscale_icon = false;
            }
        }
        self.last_icon_animation_tick = None;
        true
    }

    pub(super) fn remove_icon(&mut self) -> bool {
        if !self.open || self.update_mode() {
            return false;
        }

        let changed = self.can_remove_icon() || self.can_upscale_icon;
        self.bump_icon_generation();
        self.selected_icon = None;
        self.icon_pending = false;
        self.icon_error = None;
        self.can_upscale_icon = false;
        self.last_icon_animation_tick = None;
        changed
    }

    pub(super) fn toggle_upscale_icon(&mut self, value: bool) {
        if self.open && self.can_upscale_icon {
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
        }) = self.selected_icon.as_mut()
        else {
            return false;
        };

        animation.advance(now.saturating_duration_since(last_tick))
    }

    pub(super) fn open_directory(&mut self, path: &str) -> bool {
        self.browser
            .as_mut()
            .is_some_and(|browser| browser.open_directory(path))
    }

    pub(super) fn go_up(&mut self) -> bool {
        self.browser.as_mut().is_some_and(FileBrowserState::go_up)
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

    pub(super) fn set_addon_type(&mut self, value: &str) {
        if !self.open {
            return;
        }
        self.addon_type = AddonType::from_value(value);
    }

    pub(super) fn set_tag(&mut self, index: usize, value: &str) {
        if !self.open || index >= self.tags.len() {
            return;
        }

        let value = AddonTag::from_value(value);
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

    pub(super) fn begin_submit(
        &mut self,
        context: PublishSubmitContext,
    ) -> Option<PublishSubmitRequestEnvelope> {
        if !self.can_submit() {
            return None;
        }

        let verified = self.verified_addon_path.clone()?;
        let tags = selected_tags(&self.tags);
        if tags.is_empty() {
            return None;
        }
        let mode = self.submit_mode()?;
        let changelog = self.submit_changelog();
        let preview = self.submit_preview(&context.temp_dir);

        let generation = self.bump_submit_generation();
        self.begin_submit_pending();

        Some(PublishSubmitRequestEnvelope {
            generation,
            request: PublishSubmitRequest {
                mode,
                content_source_path: verified.path,
                title: self.title.trim().to_owned(),
                addon_type: self
                    .addon_type
                    .map_or_else(String::new, |value| value.as_str().to_owned()),
                tags,
                changelog,
                preview,
                ignore_globs: context.ignore_globs,
                total_size: verified.total_size,
                temp_dir: context.temp_dir,
            },
        })
    }

    pub(super) fn apply_submit_completion(
        &mut self,
        generation: u64,
        result: Result<PublishSubmitResult, UiError>,
    ) -> bool {
        self.apply_submit_outcome(generation, result.map(|_result| ()))
    }

    pub(super) fn begin_publish_icon(&mut self) -> Option<PublishIconSubmitRequestEnvelope> {
        if !self.can_publish_icon() {
            return None;
        }
        let Mode::Update(target) = &self.mode else {
            return None;
        };
        let workshop_id = target.workshop_id;
        let (icon_source_path, upscale) = {
            let selected = self.selected_icon.as_ref()?;
            (
                selected.icon.source_path.clone(),
                self.upscale_icon && selected.icon.can_upscale,
            )
        };

        let generation = self.bump_submit_generation();
        self.begin_submit_pending();

        Some(PublishIconSubmitRequestEnvelope {
            generation,
            icon_source_path,
            upscale,
            workshop_id,
        })
    }

    pub(super) fn apply_publish_icon_completion(
        &mut self,
        generation: u64,
        result: Result<PublishIconSubmitResult, UiError>,
    ) -> bool {
        self.apply_submit_outcome(generation, result.map(|_result| ()))
    }

    pub(super) fn tick_submit_spinner(&mut self, now: Instant) -> bool {
        if self.submit_pending {
            self.spinner_now = Some(now);
            true
        } else {
            false
        }
    }

    fn begin_submit_pending(&mut self) {
        self.submit_pending = true;
        self.submit_started_at = Some(Instant::now());
        self.spinner_now = None;
    }

    /// Failures are surfaced by the tasks overlay toast (and logged here);
    /// the modal itself only stops its spinner.
    fn apply_submit_outcome(&mut self, generation: u64, result: Result<(), UiError>) -> bool {
        if !self.open || !self.submit_pending || self.submit_generation != generation {
            return false;
        }

        self.submit_pending = false;
        self.submit_started_at = None;
        self.spinner_now = None;
        if let Err(error) = result {
            log::warn!("Prepare Publish submit failed: {error}");
        }
        true
    }

    fn submit_mode(&self) -> Option<PublishSubmitMode> {
        match &self.mode {
            Mode::New => (!self.title.trim().is_empty()).then_some(PublishSubmitMode::New),
            Mode::Update(target) => {
                (!self.changelog_trimmed().is_empty()).then_some(PublishSubmitMode::Update {
                    workshop_id: target.workshop_id,
                })
            }
        }
    }

    fn submit_changelog(&self) -> Option<String> {
        match &self.mode {
            Mode::New => None,
            Mode::Update(_) => Some(self.changelog_trimmed()),
        }
    }

    fn submit_preview(&self, temp_dir: &Path) -> Option<PublishSubmitPreview> {
        if let Some(icon) = &self.selected_icon {
            return Some(PublishSubmitPreview::Selected(publish_selected_preview(
                &icon.icon,
                self.upscale_icon,
            )));
        }

        match &self.mode {
            Mode::New => Some(PublishSubmitPreview::Default(default_icon_path(temp_dir))),
            Mode::Update(_) => None,
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

    fn bump_request_generation(&mut self) -> u64 {
        self.request_generation = self.request_generation.saturating_add(1);
        self.request_generation
    }

    fn bump_icon_generation(&mut self) -> u64 {
        self.icon_generation = self.icon_generation.saturating_add(1);
        self.icon_generation
    }

    fn bump_submit_generation(&mut self) -> u64 {
        self.submit_generation = self.submit_generation.saturating_add(1);
        self.submit_generation
    }
}

fn thumbnail_owner() -> thumbnail_demand::Owner {
    thumbnail_demand::Owner::PreparePublish
}

fn seeded_backdrop(
    still: &iced::widget::image::Handle,
    well_rgb: [u8; 3],
) -> iced::widget::image::Handle {
    if let iced::widget::image::Handle::Rgba {
        width,
        height,
        ref pixels,
        ..
    } = *still
    {
        crate::media::backdrop::bake_blurred_backdrop(width, height, pixels, well_rgb)
            .unwrap_or_else(|| still.clone())
    } else {
        still.clone()
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
