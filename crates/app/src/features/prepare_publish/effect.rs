use crate::media::preview_model::PreviewRequest;

use super::model::{
    ContentPathVerificationRequest, IconVerificationRequest, IgnorePatternMutation,
    PublishIconSubmitRequestEnvelope, PublishSubmitRequestEnvelope, PublishSubmitResult,
    WorkshopContentRequest,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    ModalOpenRequested,
    /// A message from the File Preview feature this modal deliberately embeds.
    ///
    /// The view has to wrap it — `view()` yields this feature's message type —
    /// but the *handling* belongs to `file_preview`, so it leaves as an effect
    /// rather than being intercepted at the root by an arm that had to be
    /// ordered before the general one. This is UI composition only: Prepare
    /// Publish never reaches File Preview's runner or services.
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
    PublishSubmitRequested(PublishSubmitRequestEnvelope),
    PublishIconSubmitRequested(PublishIconSubmitRequestEnvelope),
    PublishSuccessUrlsRequested(PublishSubmitResult),
    SoundRequested(crate::media::sounds::Sound),
}
