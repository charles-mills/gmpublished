use std::path::PathBuf;

use super::model::{AuthorRequest, MetadataRequest, OpenRequest};
use crate::features::file_preview::PreviewRequest;

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    ModalOpenRequested,
    /// A message from the file-preview widget this modal embeds.
    ///
    /// The view has to wrap it — `view()` yields this feature's message type —
    /// but the *handling* belongs to `file_preview`, so it leaves as an effect
    /// rather than being intercepted at the root by an arm that had to be
    /// ordered before the general one.
    FilePreview(crate::features::file_preview::Message),
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
