use std::path::PathBuf;

use super::model::{AuthorRequest, MetadataRequest, OpenRequest};
use crate::features::file_preview::PreviewRequest;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    ModalOpenRequested,
    ArchiveOpenRequested(OpenRequest),
    WorkshopMetadataRequested(MetadataRequest),
    AuthorFetchRequested(AuthorRequest),
    DestinationSelectRequested,
        EntryPreviewRequested(PreviewRequest),
    OpenUrlRequested(String),
    CopyTextRequested(String),
    RevealPathRequested(PathBuf),
    BrowserPathChanged,
    ThumbnailDemandsChanged,
}
