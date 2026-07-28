use std::{path::PathBuf, sync::Arc, time::Instant};

use iced::widget::text_editor;

use crate::bridge::domain::WorkshopDownloadSuccess;
use crate::bridge::ui_error::UiError;
use crate::features::file_preview;

use super::{
    model::{
        IgnorePatternMutationResult, IgnoredPattern, PublishIconSubmitResult, PublishSubmitContext,
        PublishSubmitResult, VerifiedContentPath, VerifiedIconPreview,
    },
    state::{AddonTag, AddonType, OpenTarget},
};
use crate::generation::Generation;

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
    WorkshopSnapshotInspected(Generation, Result<Arc<VerifiedContentPath>, UiError>),
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
    IconVerificationCompleted(Generation, Result<Arc<VerifiedIconPreview>, UiError>),
    IconRemoveRequested,
    IconUpscaleToggled(bool),
    IconAnimationTick(Instant),
    AddonTypeSelected(Option<AddonType>),
    TagSelected(usize, Option<AddonTag>),
    IgnorePatternEdited(String),
    IgnorePatternAccepted,
    IgnorePatternRemoveRequested(String),
    IgnorePatternMutationCompleted(Result<IgnorePatternMutationResult, UiError>),
    PathVerificationCompleted(Generation, Result<Arc<VerifiedContentPath>, UiError>),
    BrowserSelectHoverChanged(bool),
    BrowserScrolled {
        offset: f32,
    },
    DirectoryOpened(Arc<String>),
    PreviewEntryRequested(Arc<String>),
    FilePreview(file_preview::Message),
    UpRequested,
    TitleEdited(String),
    ChangelogActionPerformed(text_editor::Action),
    SubmitRequested,
    PublishIconRequested,
    PublishIconSubmitCompleted(Generation, Result<PublishIconSubmitResult, UiError>),
    SubmitSpinnerTick(Instant),
    SubmitContextLoaded(Result<PublishSubmitContext, UiError>),
    PublishSubmitCompleted(Generation, Result<PublishSubmitResult, UiError>),
}
