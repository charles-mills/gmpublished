use std::time::Instant;

use crate::backend::domain::PublishedFileId;
use crate::backend::ui_error::UiError;
#[cfg(feature = "asset-studio")]
use crate::features::file_preview;

use super::model::{AuthorInfo, LoadedArchive, OpenTarget, WorkshopMetadata};

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "asset-studio"), derive(Eq))]
pub enum Message {
    OpenRequested(OpenTarget),
    ArchiveOpened(u64, Result<LoadedArchive, UiError>),
    WorkshopMetadataCompleted(
        u64,
        PublishedFileId,
        Result<Option<WorkshopMetadata>, UiError>,
    ),
    AuthorFetchCompleted(u64, u64, Result<AuthorInfo, UiError>),
    AuthorLinkRequested,
    DirectoryOpened(String),
    ExtractArchiveRequested,
    #[cfg(not(feature = "asset-studio"))]
    ExtractEntryRequested(String),
    #[cfg(feature = "asset-studio")]
    PreviewEntryRequested(String),
    #[cfg(feature = "asset-studio")]
    FilePreview(file_preview::Message),
    WorkshopLinkRequested,
    CopyCurrentPathRequested,
    OpenLocationRequested,
    AnimationTick(Instant),
    UpRequested,
    CloseFinished,
}
