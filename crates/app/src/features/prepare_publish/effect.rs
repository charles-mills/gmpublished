use crate::media::preview_model::PreviewRequest;

use super::model::{
    ContentPathVerificationRequest, IconVerificationRequest, IgnorePatternMutation,
    PublishIconSubmitRequestEnvelope, PublishSubmitRequestEnvelope, PublishSubmitResult,
    WorkshopContentRequest,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    ModalOpenRequested,
    FilePreview(crate::features::file_preview::Message),
    /// The browser snapshot was rebuilt and the model's scroll offset reset;
    /// the rows scrollable widget must be snapped back to the top with it.
    BrowserScrollResetRequested,
    ThumbnailDemandsChanged,
    ContentPickerRequested,
    IconPickerRequested,
    OpenUrlRequested(String),
    WorkshopContentRequested(WorkshopContentRequest),
    WorkshopSnapshotInspectionRequested(ContentPathVerificationRequest),
    CleanupPathRequested(std::path::PathBuf),
    PathVerificationRequested(ContentPathVerificationRequest),
    EntryPreviewRequested(PreviewRequest),
    IconVerificationRequested(IconVerificationRequest),
    IgnorePatternMutationRequested(IgnorePatternMutation),
    SubmitContextRequested,
    /// Hand off to the description editor: against the live item when its
    /// id is known, as a local draft otherwise.
    DescriptionEditRequested(DescriptionEditRequest),
    PublishSubmitRequested(PublishSubmitRequestEnvelope),
    PublishIconSubmitRequested(PublishIconSubmitRequestEnvelope),
    PublishSuccessUrlsRequested(PublishSubmitResult),
    SoundRequested(crate::media::sounds::Sound),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DescriptionEditRequest {
    pub language: crate::bridge::workshop_text::WorkshopLanguage,
    pub drafts: crate::bridge::workshop_text::LocalizedDrafts,
    /// `Some` targets the live Workshop item; `None` edits a local draft
    /// staged into the pending creation.
    pub workshop_id: Option<crate::bridge::domain::PublishedFileId>,
    pub title: String,
}
