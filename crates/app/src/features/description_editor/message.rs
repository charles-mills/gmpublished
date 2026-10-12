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
    #[cfg(test)]
    OpenDraftRequested {
        title: String,
        initial: String,
    },
    OpenLocalizedRequested {
        workshop_id: PublishedFileId,
        title: Option<String>,
        language: crate::bridge::workshop_text::WorkshopLanguage,
    },
    OpenPendingTranslationsRequested {
        workshop_id: PublishedFileId,
        drafts: crate::bridge::workshop_text::LocalizedDrafts,
    },
    OpenLocalizedDraftRequested {
        language: crate::bridge::workshop_text::WorkshopLanguage,
        drafts: crate::bridge::workshop_text::LocalizedDrafts,
    },
    PanesResized {
        split: iced::widget::pane_grid::Split,
        ratio: f32,
    },
    PanesLayoutChanged(f32),
    PanesReset,
    LanguageSelected(crate::bridge::workshop_text::WorkshopLanguage),
    TitleEdited(String),
    Undo,
    Redo,
    RetryFetchRequested,
    SourceFetched {
        generation: Generation,
        result: Result<FetchedSource, UiError>,
    },
    SourceActionPerformed(text_editor::Action),
    ToolbarApplied(ToolbarAction),
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
    CloseFinished,
    AnimationTick,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchedSource {
    pub title: String,
    pub description: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SaveOutcome {
    /// Steam accepted the revision but the user has not accepted the
    /// Workshop legal agreement; the same flag the publish and icon flows
    /// answer by opening the agreement page.
    pub legal_agreement_required: bool,
}
