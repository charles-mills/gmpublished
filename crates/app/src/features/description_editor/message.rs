use iced::widget::text_editor;

use gmpublished_backend::bbcode::SpoilerId;

use super::markup::ToolbarAction;
use crate::bridge::domain::PublishedFileId;
use crate::bridge::ui_error::UiError;
use crate::generation::Generation;

#[derive(Clone, Debug)]
pub enum Message {
    /// Open for an existing Workshop item; the current description is
    /// fetched live before editing starts.
    OpenRequested {
        workshop_id: PublishedFileId,
        title: Option<String>,
    },
    /// Open over a local draft (the creation flow): nothing is fetched, and
    /// saving stages the text back to the publish modal instead of Steam.
    OpenDraftRequested {
        title: String,
        initial: String,
    },
    SourceFetched {
        generation: Generation,
        result: Result<FetchedSource, UiError>,
    },
    SourceActionPerformed(text_editor::Action),
    ToolbarApplied(ToolbarAction),
    /// Enter inside the source editor; list lines continue themselves.
    EnterPressed,
    SpoilerToggled(SpoilerId),
    LinkOpenRequested(String),
    SaveRequested,
    SaveCompleted {
        generation: Generation,
        result: Result<SaveOutcome, UiError>,
    },
    CloseRequested,
    DiscardConfirmed,
    DiscardCancelled,
    /// The modal finished its close animation; drop session state.
    CloseFinished,
    /// The animation clock fired; playback advances to `update_at`'s `now`.
    AnimationTick,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchedSource {
    pub title: String,
    pub description: String,
}

/// What a successful description revision reported back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SaveOutcome {
    /// Steam accepted the revision but the user has not accepted the
    /// Workshop legal agreement; the same flag the publish and icon flows
    /// answer by opening the agreement page.
    pub legal_agreement_required: bool,
}
