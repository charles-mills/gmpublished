use crate::bridge::domain::PublishedFileId;
use crate::generation::Generation;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    ModalOpenRequested,
    ModalCloseRequested,
    /// Fetch the item's live title and description so the editor never
    /// starts from a stale cached copy.
    SourceFetchRequested(SourceRequest),
    SaveRequested(SaveRequest),
    /// A draft session finished: the text goes back to the publish flow.
    DraftStaged(String),
    OpenUrlRequested(String),
    ThumbnailDemandsChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceRequest {
    pub workshop_id: PublishedFileId,
    pub generation: Generation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaveRequest {
    pub workshop_id: PublishedFileId,
    pub description: String,
    pub generation: Generation,
}
