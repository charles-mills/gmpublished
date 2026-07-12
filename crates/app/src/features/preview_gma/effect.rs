use std::path::PathBuf;

#[cfg(not(feature = "asset-studio"))]
use super::model::ExtractionRequest;
use super::model::{AuthorRequest, MetadataRequest, OpenRequest};
#[cfg(feature = "asset-studio")]
use crate::features::file_preview::PreviewRequest;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    ModalOpenRequested,
    ArchiveOpenRequested(OpenRequest),
    WorkshopMetadataRequested(MetadataRequest),
    AuthorFetchRequested(AuthorRequest),
    DestinationSelectRequested,
    #[cfg(not(feature = "asset-studio"))]
    EntryExtractionRequested(ExtractionRequest),
    #[cfg(feature = "asset-studio")]
    EntryPreviewRequested(PreviewRequest),
    OpenUrlRequested(String),
    CopyTextRequested(String),
    RevealPathRequested(PathBuf),
    BrowserPathChanged,
    ThumbnailDemandsChanged,
}
