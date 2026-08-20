use std::collections::{HashMap, HashSet};
use std::time::Instant;

use iced::widget::{image, text_editor};

use gmpublished_backend::bbcode::{Document as BbCodeDocument, SpoilerId};

use crate::bridge::domain::PublishedFileId;
use crate::generation::Generation;
use crate::media::thumbnail_worker::ThumbnailInput;
use crate::media::{thumbnail_animation, thumbnail_demand};
use crate::widgets::bbcode::{self, MediaLookup, MediaView};

/// Steam's description length ceiling, shared with the backend validator so
/// the header counter and the submit guard can never disagree.
pub(super) const DESCRIPTION_MAX_BYTES: usize =
    gmpublished_backend::publishing::WORKSHOP_DESCRIPTION_MAX_BYTES;

/// The descriptions Steam items carry when nothing real was ever written —
/// the current backend default plus the one earlier builds wrote — presented
/// as an empty editor rather than literal text to overwrite. Shared with the
/// backend constants so the sets can never drift apart.
pub(super) const PLACEHOLDER_DESCRIPTIONS: [&str; 2] = [
    gmpublished_backend::publishing::WORKSHOP_DEFAULT_DESCRIPTION,
    gmpublished_backend::publishing::WORKSHOP_LEGACY_DEFAULT_DESCRIPTION,
];

const fn thumbnail_owner() -> thumbnail_demand::Owner {
    thumbnail_demand::Owner::DescriptionEditor
}

/// Fetch images at the Workshop column's display width.
const MEDIA_MAX_EDGE: u32 = bbcode::STEAM_DESCRIPTION_WIDTH as u32;

/// Source editor content with value semantics for state snapshots.
#[derive(Debug, Default)]
pub struct SourceContent(text_editor::Content);

impl SourceContent {
    fn from_text(text: &str) -> Self {
        Self(text_editor::Content::with_text(text))
    }

    pub(crate) const fn content(&self) -> &text_editor::Content {
        &self.0
    }

    pub(crate) fn text(&self) -> String {
        self.0.text()
    }

    pub(super) fn selection(&self) -> Option<String> {
        self.0.selection()
    }

    /// The caret's line and its byte offset within that line (the editor
    /// reports the cursor column as a byte index).
    pub(super) fn cursor_line(&self) -> Option<(String, usize)> {
        let position = self.0.cursor().position;
        let line = self.0.line(position.line)?;
        Some((line.text.into_owned(), position.column))
    }

    pub(super) fn perform(&mut self, action: text_editor::Action) {
        self.0.perform(action);
    }
}

impl Clone for SourceContent {
    fn clone(&self) -> Self {
        Self::from_text(&self.text())
    }
}

impl PartialEq for SourceContent {
    fn eq(&self, other: &Self) -> bool {
        self.text() == other.text()
    }
}

/// One fetched media source for the live preview.
#[derive(Debug)]
enum MediaEntry {
    Ready {
        still: image::Handle,
        width: u32,
        height: u32,
        animation: Option<thumbnail_animation::Playback>,
    },
    Failed,
}

/// Whether the live description has arrived. Editing and saving only exist
/// in [`LoadPhase::Ready`], so a failed fetch can never be "saved back" as
/// an empty description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoadPhase {
    Loading,
    Failed,
    Ready,
}

/// What a session edits: a live Workshop item, or a local draft staged into
/// a pending creation by the publish flow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionTarget {
    Existing(PublishedFileId),
    Draft,
}

#[derive(Debug)]
struct Session {
    target: SessionTarget,
    title: Option<String>,
    phase: LoadPhase,
    saving: bool,
    source: SourceContent,
    /// The text as fetched (or last saved); dirtiness is divergence from it.
    initial: String,
    document: BbCodeDocument,
    revealed_spoilers: HashSet<SpoilerId>,
    media: HashMap<String, MediaEntry>,
    confirm_discard: bool,
}

#[derive(Debug, Default)]
pub struct State {
    session: Option<Session>,
    generation: Generation,
    last_animation_tick: Option<Instant>,
    window_focused: bool,
    /// `true` after construction; `Default` is only used before the first
    /// focus event arrives, and an unfocused start corrects itself.
    focus_initialized: bool,
}

impl State {
    pub(super) fn open(
        &mut self,
        workshop_id: PublishedFileId,
        title: Option<String>,
    ) -> Generation {
        self.session = Some(Session {
            target: SessionTarget::Existing(workshop_id),
            title,
            phase: LoadPhase::Loading,
            saving: false,
            source: SourceContent::default(),
            initial: String::new(),
            document: BbCodeDocument::default(),
            revealed_spoilers: HashSet::new(),
            media: HashMap::new(),
            confirm_discard: false,
        });
        self.generation.bump()
    }

    /// Opens over a local draft: no fetch, and saving stages the text back to
    /// the caller instead of submitting to Steam.
    pub(super) fn open_draft(&mut self, title: String, initial: &str) -> Generation {
        let initial = initial.trim();
        self.session = Some(Session {
            target: SessionTarget::Draft,
            title: Some(title),
            phase: LoadPhase::Ready,
            saving: false,
            source: SourceContent::from_text(initial),
            initial: initial.to_owned(),
            document: BbCodeDocument::parse(initial),
            revealed_spoilers: HashSet::new(),
            media: HashMap::new(),
            confirm_discard: false,
        });
        self.generation.bump()
    }

    pub(crate) fn is_draft(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.target == SessionTarget::Draft)
    }

    pub(super) fn close(&mut self) {
        self.session = None;
        self.last_animation_tick = None;
        // Invalidates any in-flight fetch, save, and thumbnail deliveries.
        self.generation.bump();
    }

    pub(crate) fn is_open(&self) -> bool {
        self.session.is_some()
    }

    pub(super) const fn generation(&self) -> Generation {
        self.generation
    }

    pub(crate) fn workshop_id(&self) -> Option<PublishedFileId> {
        match self.session.as_ref()?.target {
            SessionTarget::Existing(workshop_id) => Some(workshop_id),
            SessionTarget::Draft => None,
        }
    }

    pub(crate) fn title(&self) -> Option<&str> {
        self.session.as_ref()?.title.as_deref()
    }

    pub(crate) fn loading(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.phase == LoadPhase::Loading)
    }

    pub(crate) fn load_failed(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.phase == LoadPhase::Failed)
    }

    pub(crate) fn saving(&self) -> bool {
        self.session.as_ref().is_some_and(|session| session.saving)
    }

    pub(crate) fn confirming_discard(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.confirm_discard)
    }

    pub(crate) fn source(&self) -> Option<&SourceContent> {
        self.session.as_ref().map(|session| &session.source)
    }

    /// The text a save would submit — the same trimmed view [`Self::dirty`]
    /// compares against, so the two can never disagree.
    pub(super) fn trimmed_source(&self) -> Option<String> {
        self.session
            .as_ref()
            .map(|session| trimmed_source(&session.source))
    }

    pub(crate) fn document(&self) -> Option<&BbCodeDocument> {
        self.session.as_ref().map(|session| &session.document)
    }

    pub(crate) fn revealed_spoilers(&self) -> Option<&HashSet<SpoilerId>> {
        self.session
            .as_ref()
            .map(|session| &session.revealed_spoilers)
    }

    pub(crate) fn source_is_empty(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.source.content().is_empty())
    }

    pub(crate) fn source_bytes(&self) -> usize {
        self.session
            .as_ref()
            .map_or(0, |session| trimmed_source(&session.source).len())
    }

    pub(crate) fn over_limit(&self) -> bool {
        self.source_bytes() > DESCRIPTION_MAX_BYTES
    }

    pub(crate) fn dirty(&self) -> bool {
        self.session.as_ref().is_some_and(|session| {
            session.phase == LoadPhase::Ready && trimmed_source(&session.source) != session.initial
        })
    }

    /// Saving is possible once the live description has loaded, the text
    /// actually changed, fits Steam's limit, and no save is in flight.
    pub(crate) fn can_save(&self) -> bool {
        self.dirty() && !self.saving() && !self.over_limit()
    }

    pub(super) fn set_confirm_discard(&mut self, confirm: bool) {
        if let Some(session) = self.session.as_mut() {
            session.confirm_discard = confirm;
        }
    }

    pub(super) fn set_saving(&mut self, saving: bool) {
        if let Some(session) = self.session.as_mut() {
            session.saving = saving;
        }
    }

    /// Records a successful save: the current text becomes the new baseline.
    pub(super) fn mark_saved(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.saving = false;
            session.initial = trimmed_source(&session.source);
        }
    }

    pub(super) fn apply_fetched(&mut self, title: String, description: &str) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.title = Some(title);
        session.phase = LoadPhase::Ready;
        let description = description.trim();
        let source = if PLACEHOLDER_DESCRIPTIONS.contains(&description) {
            ""
        } else {
            description
        };
        source.clone_into(&mut session.initial);
        session.source = SourceContent::from_text(source);
        session.document = BbCodeDocument::parse(source);
        session.revealed_spoilers.clear();
    }

    pub(super) fn apply_fetch_failure(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.phase = LoadPhase::Failed;
        }
    }

    pub(super) fn perform(&mut self, action: text_editor::Action) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let edited = action.is_edit();
        session.source.perform(action);
        if edited {
            self.reparse();
        }
    }

    pub(super) fn selection(&self) -> Option<String> {
        self.session.as_ref()?.source.selection()
    }

    pub(super) fn cursor_line(&self) -> Option<(String, usize)> {
        self.session.as_ref()?.source.cursor_line()
    }

    /// Re-derives the parsed document after an edit and drops media entries
    /// for sources the description no longer references.
    pub(super) fn reparse(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.document = BbCodeDocument::parse(&session.source.text());
        let referenced = bbcode::media_urls(&session.document);
        session.media.retain(|url, _| referenced.contains(url));
    }

    pub(super) fn toggle_spoiler(&mut self, id: SpoilerId) {
        if let Some(session) = self.session.as_mut()
            && !session.revealed_spoilers.remove(&id)
        {
            session.revealed_spoilers.insert(id);
        }
    }

    /// GIF playback pauses while the window is unfocused; dropping the tick
    /// timestamp keeps the pause from being replayed as one giant elapsed
    /// step when the clock subscription resumes.
    pub(crate) fn set_window_focused(&mut self, focused: bool) {
        self.window_focused = focused;
        self.focus_initialized = true;
        self.last_animation_tick = None;
    }

    /// Demands every referenced media source that has not yet delivered.
    /// Delivered and failed sources are excluded; in-flight ones stay in the
    /// set, because dropping an interest before completion silences it.
    pub(crate) fn thumbnail_demands(&self) -> thumbnail_demand::DemandSet {
        let demands = self
            .session
            .as_ref()
            .map(|session| {
                bbcode::media_urls(&session.document)
                    .into_iter()
                    .filter(|url| !session.media.contains_key(url))
                    .map(|url| thumbnail_demand::Demand {
                        id: thumbnail_demand::DemandId::row(url.as_str()),
                        input: ThumbnailInput::from_url(url),
                        logical_max_edge: MEDIA_MAX_EDGE,
                        priority: thumbnail_demand::Priority::ActiveDetail,
                        capabilities: thumbnail_demand::DemandCapabilities::SURFACE,
                    })
                    .collect()
            })
            .unwrap_or_default();
        thumbnail_demand::DemandSet {
            owner: thumbnail_owner(),
            generation: self.generation,
            replace: thumbnail_demand::ReplaceMode::Owner,
            demands,
        }
    }

    pub(crate) fn apply_thumbnail_delivery(
        &mut self,
        delivery: &thumbnail_demand::Delivery,
    ) -> bool {
        if delivery.owner != thumbnail_owner() || delivery.generation != self.generation {
            return false;
        }
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        let Some(url) = delivery.id.row_key() else {
            return false;
        };
        // A delivery can land after the edit that removed its tag; storing it
        // anyway would park an invisible entry in the map — and a Ready GIF
        // there keeps the animation subscription ticking until the next edit
        // prunes it.
        if !bbcode::media_urls(&session.document)
            .iter()
            .any(|referenced| referenced == url)
        {
            return false;
        }

        match &delivery.result {
            thumbnail_demand::DeliveryResult::Ready(ready) => {
                let metadata = ready.metadata();
                session.media.insert(
                    url.to_owned(),
                    MediaEntry::Ready {
                        still: ready.handle().clone(),
                        width: metadata.source_width,
                        height: metadata.source_height,
                        animation: thumbnail_animation::Playback::from_ready(ready),
                    },
                );
                true
            }
            // The preview shows its own loading box; ThumbHash placeholders
            // stay unused until delivered images replace them anyway.
            thumbnail_demand::DeliveryResult::Placeholder(_) => false,
            thumbnail_demand::DeliveryResult::Failed { .. } => {
                session.media.insert(url.to_owned(), MediaEntry::Failed);
                true
            }
        }
    }

    pub(crate) fn invalidate_ready_thumbnails(&mut self) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        let had_media = !session.media.is_empty();
        session.media.clear();
        had_media
    }

    /// GIFs in the preview play while the editor is open and the window is
    /// focused — the same always-on behavior as the live Workshop page.
    pub(crate) fn has_active_animation(&self) -> bool {
        (self.window_focused || !self.focus_initialized)
            && self.session.as_ref().is_some_and(|session| {
                session.media.values().any(|entry| {
                    matches!(
                        entry,
                        MediaEntry::Ready {
                            animation: Some(_),
                            ..
                        }
                    )
                })
            })
    }

    /// Advances every playing animation; reports whether a frame changed.
    pub(super) fn advance_animations(&mut self, now: Instant) -> bool {
        let Some(last_tick) = self.last_animation_tick.replace(now) else {
            return false;
        };
        let elapsed = now.saturating_duration_since(last_tick);
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        let mut changed = false;
        for entry in session.media.values_mut() {
            if let MediaEntry::Ready {
                animation: Some(animation),
                ..
            } = entry
            {
                changed |= animation.advance(elapsed);
            }
        }
        changed
    }
}

impl MediaLookup for State {
    fn media(&self, url: &str) -> MediaView<'_> {
        let Some(session) = self.session.as_ref() else {
            return MediaView::Unavailable;
        };
        match session.media.get(url) {
            // Only http(s) sources are ever demanded; anything else would
            // otherwise sit in `Loading` forever instead of degrading to
            // the link fallback.
            None if bbcode::is_http_url(url) => MediaView::Loading,
            None => MediaView::Unavailable,
            Some(MediaEntry::Failed) => MediaView::Failed,
            Some(MediaEntry::Ready {
                still,
                width,
                height,
                animation,
            }) => MediaView::Ready {
                handle: animation
                    .as_ref()
                    .map_or(still, thumbnail_animation::Playback::current_handle),
                width: *width,
                height: *height,
            },
        }
    }
}

/// The text a save would submit: surrounding whitespace never survives the
/// round trip through Steam, so it does not count as a difference either.
fn trimmed_source(source: &SourceContent) -> String {
    source.text().trim().to_owned()
}
