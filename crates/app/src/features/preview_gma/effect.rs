use std::path::PathBuf;

use super::model::{AuthorRequest, MetadataRequest, OpenRequest};
use crate::media::preview_model::PreviewRequest;

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    ModalOpenRequested,
    FilePreview(crate::features::file_preview::Message),
    ArchiveOpenRequested(OpenRequest),
    WorkshopMetadataRequested(MetadataRequest),
    AuthorFetchRequested(AuthorRequest),
    EditMetadataRequested {
        workshop_id: crate::bridge::domain::PublishedFileId,
        language: crate::bridge::workshop_text::WorkshopLanguage,
    },
    LocalizedTextRequested {
        request_id: crate::generation::Generation,
        workshop_id: crate::bridge::domain::PublishedFileId,
        language: crate::bridge::workshop_text::WorkshopLanguage,
    },
    DestinationSelectRequested,
    EntryPreviewRequested(PreviewRequest),
    OpenUrlRequested(String),
    CopyTextRequested(String),
    RevealPathRequested(PathBuf),
    BrowserPathChanged,
    ThumbnailDemandsChanged,
}
