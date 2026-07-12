#[cfg(feature = "asset-studio")]
use crate::features::file_preview::PreviewRequest;

use super::model::{
    ContentPathVerificationRequest, IconVerificationRequest, IgnorePatternMutation,
    PublishIconSubmitRequestEnvelope, PublishSubmitRequestEnvelope, PublishSubmitResult,
    WorkshopContentRequest,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    ModalOpenRequested,
    ThumbnailDemandsChanged,
    ContentPickerRequested,
    IconPickerRequested,
    OpenUrlRequested(String),
    WorkshopContentRequested(WorkshopContentRequest),
    WorkshopSnapshotInspectionRequested(ContentPathVerificationRequest),
    CleanupPathRequested(std::path::PathBuf),
    PathVerificationRequested(ContentPathVerificationRequest),
    #[cfg(feature = "asset-studio")]
    EntryPreviewRequested(PreviewRequest),
    IconVerificationRequested(IconVerificationRequest),
    IgnorePatternMutationRequested(IgnorePatternMutation),
    SubmitContextRequested,
    PublishSubmitRequested(PublishSubmitRequestEnvelope),
    PublishIconSubmitRequested(PublishIconSubmitRequestEnvelope),
    PublishSuccessUrlsRequested(PublishSubmitResult),
}
