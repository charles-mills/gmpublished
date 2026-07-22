use std::{path::PathBuf, sync::Arc, time::Instant};

use iced::widget::text_editor;

use crate::bridge::domain::WorkshopDownloadSuccess;
use crate::bridge::ui_error::UiError;
#[cfg(feature = "asset-studio")]
use crate::features::file_preview;

use super::{
    model::{
        IgnorePatternMutationResult, IgnoredPattern, PublishIconSubmitResult, PublishSubmitContext,
        PublishSubmitResult, SelectOption, VerifiedContentPath, VerifiedIconPreview,
    },
    state::OpenTarget,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    OpenRequested {
        target: OpenTarget,
        ignored_patterns: Vec<IgnoredPattern>,
        upscale_icon_default: bool,
    },
    CloseRequested,
    WorkshopContentSubmissionCompleted(u64, Result<(), UiError>),
    WorkshopContentDownloaded(u64, WorkshopDownloadSuccess),
    WorkshopSnapshotFailed(u64, UiError),
    WorkshopSnapshotInspected(u64, Result<Arc<VerifiedContentPath>, UiError>),
    AddonPathEdited(String),
    AddonPathAccepted,
    WorkshopLinkRequested,
    AddonPathBrowseRequested,
    AddonPathBrowseCompleted(Option<PathBuf>),
    IconBrowseRequested,
    IconBrowseCompleted {
        path: Option<PathBuf>,
        temp_dir: PathBuf,
        well_rgb: [u8; 3],
    },
    IconVerificationCompleted(u64, Result<Arc<VerifiedIconPreview>, UiError>),
    IconRemoveRequested,
    IconUpscaleToggled(bool),
    IconAnimationTick(Instant),
    AddonTypeSelected(SelectOption),
    TagSelected(usize, SelectOption),
    IgnorePatternEdited(String),
    IgnorePatternAccepted,
    IgnorePatternRemoveRequested(String),
    IgnorePatternMutationCompleted(Result<IgnorePatternMutationResult, UiError>),
    PathVerificationCompleted(u64, Result<Arc<VerifiedContentPath>, UiError>),
    BrowserSelectHoverChanged(bool),
    DirectoryOpened(String),
    #[cfg(feature = "asset-studio")]
    PreviewEntryRequested(String),
    #[cfg(feature = "asset-studio")]
    FilePreview(file_preview::Message),
    UpRequested,
    TitleEdited(String),
    ChangelogActionPerformed(text_editor::Action),
    SubmitRequested,
    PublishIconRequested,
    PublishIconSubmitCompleted(u64, Result<PublishIconSubmitResult, UiError>),
    SubmitSpinnerTick(Instant),
    SubmitContextLoaded(Result<PublishSubmitContext, UiError>),
    PublishSubmitCompleted(u64, Result<PublishSubmitResult, UiError>),
}
