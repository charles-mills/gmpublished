use std::{sync::Arc, time::Instant};

use gmpublished_backend::bbcode::SpoilerId;
use iced::widget::pane_grid;

use crate::bridge::domain::PublishedFileId;
use crate::bridge::ui_error::UiError;
use crate::features::file_preview;

use super::model::{AuthorInfo, LoadedArchive, OpenTarget, WorkshopMetadata};
use crate::bridge::domain::SteamId;
use crate::generation::Generation;

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    OpenRequested(OpenTarget),
    /// Boxed: `LoadedArchive` carries the full file-browser state inline,
    /// which would otherwise set the size of `RootMessage`.
    ArchiveOpened(Generation, Box<Result<LoadedArchive, UiError>>),
    /// Boxed: `WorkshopMetadata` is ~216 bytes inline, which would otherwise
    /// set the size of `RootMessage`.
    WorkshopMetadataCompleted(
        Generation,
        PublishedFileId,
        Box<Result<Option<WorkshopMetadata>, UiError>>,
    ),
    AuthorFetchCompleted(Generation, SteamId, Result<AuthorInfo, UiError>),
    AuthorLinkRequested,
    BrowserScrolled {
        offset: f32,
    },
    DirectoryOpened(Arc<String>),
    ExtractArchiveRequested,
    PreviewEntryRequested(Arc<String>),
    FilePreview(file_preview::Message),
    WorkshopLinkRequested,
    DescriptionLinkRequested(String),
    DescriptionSpoilerToggled(SpoilerId),
    PanesResized {
        split: pane_grid::Split,
        ratio: f32,
    },
    PanesLayoutChanged(f32),
    PanesReset(f32),
    CopyCurrentPathRequested,
    OpenLocationRequested,
    AnimationTick(Instant),
    UpRequested,
    CloseFinished,
}
